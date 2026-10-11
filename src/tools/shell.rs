//! シェル実行ツール — bash。

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;
use tokio::process::Command;
use tokio::time::{Duration, timeout};

use crate::config::secret_ref::read_dotenv;
use crate::llm::ToolDefinition;
use crate::skills::{SkillManager, resolve_required_env};

use super::text::{format_size, truncate_tail};
use super::{
    DEFAULT_BASH_TIMEOUT_SECS, DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES, Tool, ToolExecutionContext,
    ToolResult, command_guard, kill_process_group, path_guard, redact_known_secret_patterns,
    schema_object,
};

/// Executes bash commands in the workspace with configurable timeout and output capture.
pub(crate) struct BashTool {
    pub(super) workspace_dir: PathBuf,
    /// SkillManager used to dynamically compute the union of all installed
    /// skills' `required_env` keys on each execution. This keeps auto-hydration
    /// consistent with dynamic skill discovery (skills added at runtime are
    /// picked up without restart).
    skill_manager: Arc<SkillManager>,
    /// Path to the runtime dotenv file used for fallback resolution.
    dotenv_path: PathBuf,
}

impl BashTool {
    pub(crate) fn new(
        workspace_dir: PathBuf,
        skill_manager: Arc<SkillManager>,
        dotenv_path: PathBuf,
    ) -> Self {
        Self {
            workspace_dir,
            skill_manager,
            dotenv_path,
        }
    }

    /// Resolve allowlisted skill keys fresh from process env → dotenv.
    ///
    /// The allowlist is computed dynamically from `SkillManager` so that
    /// skills added at runtime are reflected immediately. The injected map
    /// is exactly the fresh resolution — no stale values from previous
    /// `activate_skill` calls are carried over.
    ///
    /// The map is always written back to `context.skill_env` (even if empty)
    /// so that post-execution redaction covers exactly what was injected.
    fn resolve_env_for_execution(&self, context: &ToolExecutionContext) -> HashMap<String, String> {
        let keys = self.skill_manager.all_required_env_keys();
        let resolved = if keys.is_empty() {
            HashMap::new()
        } else {
            let dotenv = read_dotenv(&self.dotenv_path);
            resolve_required_env(&keys, &dotenv)
        };

        *context.skill_env.lock().expect("skill env lock") = resolved.clone();

        resolved
    }

    fn temp_dir(&self) -> PathBuf {
        self.workspace_dir.join(".tmp").join("bash")
    }

    /// Spawns `bash -lc <command>` with stdout and stderr both written to `output_path`.
    ///
    /// The command is passed verbatim instead of being wrapped in shell syntax,
    /// so trailing heredoc delimiters and comments keep their meaning and
    /// bash's own parse errors are captured in the output file.
    fn spawn_bash_command(
        &self,
        command: &str,
        output_path: &Path,
        skill_env: &HashMap<String, String>,
    ) -> Result<tokio::process::Child, String> {
        let output = fs::File::create(output_path).map_err(|error| {
            format!(
                "Failed to create bash output file {}: {error}",
                output_path.display()
            )
        })?;
        let stderr = output
            .try_clone()
            .map_err(|error| format!("Failed to prepare bash output file: {error}"))?;
        let mut cmd = Command::new("bash");
        cmd.arg("-lc")
            .arg(command)
            .current_dir(&self.workspace_dir)
            .stdout(Stdio::from(output))
            .stderr(Stdio::from(stderr))
            .process_group(0)
            .kill_on_drop(true)
            .env_clear()
            .envs(bash_child_env(std::env::vars(), skill_env));
        cmd.spawn()
            .map_err(|error| format!("Failed to execute bash command: {error}"))
    }
}

/// bash 子プロセスへ引き継ぐ親プロセスの環境変数。それ以外は渡さない。
const BASH_INHERITED_ENV_KEYS: &[&str] = &["PATH", "HOME", "LANG", "TERM"];

/// bash 子プロセスに渡す環境変数を構築する。
///
/// 親プロセスの環境変数をすべて継承すると `SSH_AUTH_SOCK` 等の認証手段が
/// LLM 生成コマンドに漏れるため、最小限の鍵だけを明示的に渡し、
/// skill が宣言した `required_env` を上書きとして加える。
fn bash_child_env(
    parent_vars: impl Iterator<Item = (String, String)>,
    skill_env: &HashMap<String, String>,
) -> HashMap<String, String> {
    let mut env: HashMap<String, String> = parent_vars
        .filter(|(key, _)| BASH_INHERITED_ENV_KEYS.contains(&key.as_str()))
        .collect();
    for (key, value) in skill_env {
        env.insert(key.clone(), value.clone());
    }
    env
}

#[async_trait]
impl Tool for BashTool {
    fn name(&self) -> &str {
        "bash"
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "bash".to_string(),
            description: "Execute a bash command in the current working directory. Returns stdout and stderr. Output is truncated to last 2000 lines or 100KB (whichever is hit first). If truncated, full output is saved to a temp file. Optionally provide a timeout in seconds.".to_string(),
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
        context: &ToolExecutionContext,
    ) -> ToolResult {
        #[derive(serde::Deserialize)]
        struct Params {
            command: String,
            #[serde(default)]
            timeout: Option<u64>,
        }

        let params: Params = match super::parse_params(input) {
            Ok(p) => p,
            Err(e) => return e,
        };
        let command = &params.command;
        if let Err(reason) = command_guard::check_command(command) {
            return ToolResult::error_with_kind(super::ToolErrorKind::GuardDenied, reason);
        }
        if let Err(reason) = path_guard::check_command_paths(command) {
            return ToolResult::error_with_kind(super::ToolErrorKind::GuardDenied, reason);
        }
        let timeout_secs = params.timeout.unwrap_or(DEFAULT_BASH_TIMEOUT_SECS);
        let temp_dir = self.temp_dir();
        if let Err(error) = tokio::fs::create_dir_all(&temp_dir).await {
            return ToolResult::error(format!(
                "Failed to prepare bash temp directory {}: {error}",
                temp_dir.display()
            ));
        }
        let temp_path = temp_dir.join(format!("egopulse-bash-{}.log", uuid::Uuid::new_v4()));

        let skill_env = self.resolve_env_for_execution(context);
        let mut child = match self.spawn_bash_command(command, &temp_path, &skill_env) {
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
                return bash_error_result(output, &temp_path, Some(timeout_secs), None)
                    .with_error_kind(super::ToolErrorKind::Timeout);
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
                return ToolResult::error_with_details(text, details)
                    .with_error_kind(super::ToolErrorKind::ExitNonZero);
            }
            return ToolResult::error_with_kind(super::ToolErrorKind::ExitNonZero, text);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn child_env(parent: &[(&str, &str)], skill_env: &[(&str, &str)]) -> HashMap<String, String> {
        let parent_vars = parent
            .iter()
            .map(|(key, value)| (key.to_string(), value.to_string()));
        let skill: HashMap<String, String> = skill_env
            .iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect();
        bash_child_env(parent_vars, &skill)
    }

    #[test]
    fn bash_child_env_passes_only_minimal_keys_and_skill_env() {
        // Arrange
        let parent = [
            ("PATH", "/usr/bin"),
            ("HOME", "/home/agent"),
            ("LANG", "C.UTF-8"),
            ("TERM", "xterm-256color"),
            ("SSH_AUTH_SOCK", "/tmp/agent.sock"),
            ("DBUS_SESSION_BUS_ADDRESS", "unix:path=/tmp/bus"),
            ("EGOPULSE_S2_CANARY", "leak-me"),
        ];

        // Act
        let env = child_env(&parent, &[("SKILL_TOKEN", "skill-value")]);

        // Assert
        for key in ["PATH", "HOME", "LANG", "TERM", "SKILL_TOKEN"] {
            assert!(env.contains_key(key), "should keep {key}");
        }
        for key in [
            "SSH_AUTH_SOCK",
            "DBUS_SESSION_BUS_ADDRESS",
            "EGOPULSE_S2_CANARY",
        ] {
            assert!(!env.contains_key(key), "should drop {key}");
        }
    }

    #[test]
    fn bash_child_env_skips_missing_optional_keys() {
        // Arrange
        let parent = [("PATH", "/usr/bin")];

        // Act
        let env = child_env(&parent, &[]);

        // Assert
        assert_eq!(env.len(), 1);
        assert_eq!(env.get("PATH").map(String::as_str), Some("/usr/bin"));
    }

    #[test]
    fn bash_child_env_prefers_skill_env_over_inherited_key() {
        // Arrange
        let parent = [("PATH", "/usr/bin"), ("HOME", "/real-home")];

        // Act
        let env = child_env(&parent, &[("HOME", "/skill-home")]);

        // Assert
        assert_eq!(env.get("HOME").map(String::as_str), Some("/skill-home"));
    }
}
