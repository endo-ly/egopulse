//! シェル実行ツール — bash。

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use async_trait::async_trait;
use serde_json::json;
use tokio::process::Command;
use tokio::time::{Duration, timeout};

use crate::llm::ToolDefinition;

use super::text::{format_size, shell_quote, truncate_tail};
use super::{
    DEFAULT_BASH_TIMEOUT_SECS, DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES, Tool, ToolExecutionContext,
    ToolResult, command_guard, path_guard, redact_known_secret_patterns, schema_object,
};

/// Executes bash commands in the workspace with configurable timeout and output capture.
pub(crate) struct BashTool {
    pub(super) workspace_dir: PathBuf,
}

impl BashTool {
    pub fn new(workspace_dir: PathBuf) -> Self {
        Self { workspace_dir }
    }

    fn temp_dir(&self) -> PathBuf {
        self.workspace_dir.join(".tmp").join("bash")
    }

    fn spawn_bash_command(&self, wrapped_command: &str) -> Result<tokio::process::Child, String> {
        let mut cmd = Command::new("bash");
        cmd.arg("-lc")
            .arg(wrapped_command)
            .current_dir(&self.workspace_dir)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .kill_on_drop(true);
        cmd.spawn()
            .map_err(|error| format!("Failed to execute bash command: {error}"))
    }
}

#[async_trait]
impl Tool for BashTool {
    fn name(&self) -> &str {
        "bash"
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "bash".to_string(),
            description: "Execute a bash command in the current working directory. Returns stdout and stderr. Output is truncated to last 2000 lines or 50KB (whichever is hit first). If truncated, full output is saved to a temp file. Optionally provide a timeout in seconds.".to_string(),
            parameters: schema_object(
                json!({
                    "command": {
                        "type": "string",
                        "description": "Bash command to execute"
                    },
                    "timeout": {
                        "type": "integer",
                        "description": "Timeout in seconds (optional, default: 30)"
                    }
                }),
                &["command"],
            ),
        }
    }

    async fn execute(
        &self,
        input: serde_json::Value,
        _context: &ToolExecutionContext,
    ) -> ToolResult {
        let Some(command) = input.get("command").and_then(|value| value.as_str()) else {
            return ToolResult::error("Missing required parameter: command".to_string());
        };
        if let Err(reason) = command_guard::check_command(command) {
            return ToolResult::error(reason);
        }
        if let Err(reason) = path_guard::check_command_paths(command) {
            return ToolResult::error(reason);
        }
        let timeout_secs = input
            .get("timeout")
            .and_then(|value| value.as_u64())
            .unwrap_or(DEFAULT_BASH_TIMEOUT_SECS);
        let temp_dir = self.temp_dir();
        if let Err(error) = tokio::fs::create_dir_all(&temp_dir).await {
            return ToolResult::error(format!(
                "Failed to prepare bash temp directory {}: {error}",
                temp_dir.display()
            ));
        }
        let temp_path = temp_dir.join(format!("egopulse-bash-{}.log", uuid::Uuid::new_v4()));
        let quoted_temp = shell_quote(&temp_path.to_string_lossy());
        let wrapped_command = format!("({command}) > {quoted_temp} 2>&1");

        let mut child = match self.spawn_bash_command(&wrapped_command) {
            Ok(child) => child,
            Err(error) => return ToolResult::error(error),
        };

        let status = match timeout(Duration::from_secs(timeout_secs), child.wait()).await {
            Ok(Ok(status)) => Ok(status),
            Ok(Err(error)) => Err(format!("Failed to execute bash command: {error}")),
            Err(_) => {
                kill_process_group(&mut child);
                let _ = child.wait().await;
                let output = read_temp_output(&temp_path);
                return bash_error_result(output, &temp_path, Some(timeout_secs), None);
            }
        };

        let status = match status {
            Ok(status) => status,
            Err(error) => return ToolResult::error(error),
        };

        let output = read_temp_output(&temp_path);
        let output = redact_known_secret_patterns(&output);
        let (mut text, details) = render_bash_output(&output, &temp_path);

        if !status.success() {
            if let Some(code) = status.code() {
                text.push_str(&format!("\n\nCommand exited with code {code}"));
            }
            if let Some(details) = details {
                return ToolResult::error_with_details(text, details);
            }
            return ToolResult::error(text);
        }

        if let Some(details) = details {
            ToolResult::success_with_details(text, details)
        } else {
            ToolResult::success(text)
        }
    }
}

pub(crate) fn bash_error_result(
    output: String,
    temp_path: &Path,
    timeout_secs: Option<u64>,
    aborted: Option<bool>,
) -> ToolResult {
    let truncation = truncate_tail(&output, DEFAULT_MAX_LINES, DEFAULT_MAX_BYTES);
    let details = json!({
        "truncation": if truncation.truncated { Some(super::truncation_json(&truncation)) } else { None::<serde_json::Value> },
        "fullOutputPath": if truncation.truncated { Some(temp_path.to_string_lossy().to_string()) } else { None::<String> }
    });
    let mut text = if truncation.content.is_empty() {
        "(no output)".to_string()
    } else {
        truncation.content.clone()
    };
    if let Some(true) = aborted {
        text.push_str("\n\nCommand aborted");
    } else if let Some(timeout_secs) = timeout_secs {
        text.push_str(&format!(
            "\n\nCommand timed out after {timeout_secs} seconds"
        ));
    }
    if truncation.truncated {
        ToolResult::error_with_details(text, details)
    } else {
        let _ = fs::remove_file(temp_path);
        ToolResult::error(text)
    }
}

fn kill_process_group(child: &mut tokio::process::Child) {
    if let Some(pid) = child.id() {
        // 負の PID でプロセスグループ全体に SIGKILL を送信
        let ret = unsafe { libc::kill(-(pid as i32), libc::SIGKILL) };
        if ret != 0 {
            let _ = child.start_kill();
        }
    } else {
        let _ = child.start_kill();
    }
}

pub(crate) fn read_temp_output(path: &Path) -> String {
    fs::read(path)
        .map(|bytes| String::from_utf8_lossy(&bytes).replace("\r\n", "\n"))
        .unwrap_or_default()
}

fn render_bash_output(output: &str, temp_path: &Path) -> (String, Option<serde_json::Value>) {
    let truncation = truncate_tail(output, DEFAULT_MAX_LINES, DEFAULT_MAX_BYTES);
    let details = truncation.truncated.then(|| {
        json!({
            "truncation": super::truncation_json(&truncation),
            "fullOutputPath": temp_path.to_string_lossy()
        })
    });
    let mut text = if truncation.content.is_empty() {
        "(no output)".to_string()
    } else {
        truncation.content.clone()
    };

    if !truncation.truncated {
        let _ = fs::remove_file(temp_path);
        return (text, details);
    }

    append_truncation_summary(&mut text, &truncation, output, temp_path);
    (text, details)
}

fn append_truncation_summary(
    text: &mut String,
    truncation: &super::text::TruncationResult,
    output: &str,
    temp_path: &Path,
) {
    let start_line = truncation
        .total_lines
        .saturating_sub(truncation.output_lines)
        + 1;
    let end_line = truncation.total_lines;
    if truncation.last_line_partial {
        let last_line_size = format_size(output.split('\n').next_back().unwrap_or_default().len());
        text.push_str(&format!(
            "\n\n[Showing last {} of line {end_line} (line is {last_line_size}). Full output: {}]",
            format_size(truncation.output_bytes),
            temp_path.to_string_lossy()
        ));
        return;
    }

    if truncation.truncated_by == Some("lines") {
        text.push_str(&format!(
            "\n\n[Showing lines {start_line}-{end_line} of {}. Full output: {}]",
            truncation.total_lines,
            temp_path.to_string_lossy()
        ));
        return;
    }

    text.push_str(&format!(
        "\n\n[Showing lines {start_line}-{end_line} of {} ({} limit). Full output: {}]",
        truncation.total_lines,
        format_size(DEFAULT_MAX_BYTES),
        temp_path.to_string_lossy()
    ));
}
