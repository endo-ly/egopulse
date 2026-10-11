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

    async fn execute_with_env(
        &self,
        input: serde_json::Value,
        skill_env: &HashMap<String, String>,
    ) -> ToolResult {
        #[derive(serde::Deserialize)]
        struct Params {
            command: String,
            #[serde(default)]
            timeout: Option<u64>,
        }

        let params: Params = match super::parse_params(input) {
            Ok(params) => params,
            Err(error) => return error,
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

        let mut child = match self.spawn_bash_command(command, &temp_path, skill_env) {
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

    fn temp_dir(&self) -> PathBuf {
        self.workspace_dir.join(".tmp").join("bash")
    }

    /// Spawns a non-login, non-interactive Bash without startup files.
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
        cmd.arg("--noprofile")
            .arg("--norc")
            .arg("-c")
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
const BASH_STARTUP_ENV_KEYS: &[&str] = &["BASH_ENV", "ENV", "SHELLOPTS", "BASHOPTS"];

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
        if BASH_STARTUP_ENV_KEYS.contains(&key.as_str()) {
            continue;
        }
        env.insert(key.clone(), value.clone());
    }
    env
}

#[async_trait]
impl Tool for BashTool {
    fn name(&self) -> &str {
        "bash"
    }

    fn prepare_execution_context(&self, context: &ToolExecutionContext) {
        self.resolve_env_for_execution(context);
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
        let skill_env = self.resolve_env_for_execution(context);
        self.execute_with_env(input, &skill_env).await
    }

    async fn execute_prepared(
        &self,
        input: serde_json::Value,
        context: &ToolExecutionContext,
    ) -> ToolResult {
        let skill_env = context.skill_env.lock().expect("skill env lock").clone();
        self.execute_with_env(input, &skill_env).await
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
    use crate::test_env::EnvVarGuard;
    use serial_test::serial;

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
        let env = child_env(
            &parent,
            &[
                ("SKILL_TOKEN", "skill-value"),
                ("BASH_ENV", "/tmp/untrusted-env"),
                ("ENV", "/tmp/untrusted-env"),
                ("SHELLOPTS", "xtrace"),
                ("BASHOPTS", "extdebug"),
            ],
        );

        // Assert
        for key in ["PATH", "HOME", "LANG", "TERM", "SKILL_TOKEN"] {
            assert!(env.contains_key(key), "should keep {key}");
        }
        for key in [
            "SSH_AUTH_SOCK",
            "DBUS_SESSION_BUS_ADDRESS",
            "EGOPULSE_S2_CANARY",
            "BASH_ENV",
            "ENV",
            "SHELLOPTS",
            "BASHOPTS",
        ] {
            assert!(!env.contains_key(key), "should drop {key}");
        }
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

    #[tokio::test]
    #[serial]
    async fn direct_bash_execution_resolves_required_env() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let workspace = dir.path().join("workspace");
        let skills_dir = dir.path().join("skills");
        let skill_dir = skills_dir.join("direct-env");
        std::fs::create_dir_all(&workspace).expect("workspace");
        std::fs::create_dir_all(&skill_dir).expect("skill directory");
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: direct-env\ndescription: Direct execution\nrequired_env:\n  - EGOPULSE_DIRECT_BASH_ENV\n---\nBody\n",
        )
        .expect("skill metadata");
        let dotenv_path = dir.path().join(".env");
        std::fs::write(&dotenv_path, "EGOPULSE_DIRECT_BASH_ENV=resolved-value\n").expect("dotenv");
        let skills = Arc::new(crate::skills::SkillManager::from_dirs(
            skills_dir,
            dir.path().join("builtin-skills"),
        ));
        let tool = BashTool::new(workspace, skills, dotenv_path);
        let context = crate::test_util::test_tool_context();

        // Act
        let result = tool
            .execute(
                serde_json::json!({"command": "test \"$EGOPULSE_DIRECT_BASH_ENV\" = resolved-value && printf resolved"}),
                &context,
            )
            .await;

        // Assert
        assert!(!result.is_error, "{}", result.content);
        assert!(result.content.contains("resolved"));
        assert_eq!(
            context
                .skill_env
                .lock()
                .expect("skill env")
                .get("EGOPULSE_DIRECT_BASH_ENV")
                .map(String::as_str),
            Some("resolved-value")
        );
    }

    #[tokio::test]
    #[serial]
    async fn bash_process_uses_only_the_sanitized_environment_without_startup_files() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let workspace = dir.path().join("workspace");
        let home = dir.path().join("home");
        std::fs::create_dir_all(&workspace).expect("workspace");
        std::fs::create_dir_all(&home).expect("home");
        let marker_script = dir.path().join("startup.sh");
        std::fs::write(&marker_script, "touch \"$HOME/startup-ran\"\n").expect("script");
        for startup_file in [".bash_profile", ".bashrc"] {
            std::fs::write(home.join(startup_file), "touch \"$HOME/profile-ran\"\n")
                .expect("startup file");
        }
        let _env_guard = EnvVarGuard::set("HOME", &home)
            .also_set("EGOPULSE_S2_CANARY", "parent-only")
            .also_set("BASH_ENV", &marker_script)
            .also_set("ENV", &marker_script)
            .also_set("SHELLOPTS", "xtrace")
            .also_set("BASHOPTS", "extdebug");
        let skill_env = HashMap::from([
            ("SKILL_TOKEN".to_string(), "required-dummy".to_string()),
            (
                "BASH_ENV".to_string(),
                marker_script.to_string_lossy().into_owned(),
            ),
            (
                "ENV".to_string(),
                marker_script.to_string_lossy().into_owned(),
            ),
            ("SHELLOPTS".to_string(), "xtrace".to_string()),
            ("BASHOPTS".to_string(), "extdebug".to_string()),
        ]);
        let skills = Arc::new(crate::skills::SkillManager::from_dirs(
            dir.path().join("skills"),
            dir.path().join("builtin-skills"),
        ));
        let tool = BashTool::new(workspace.clone(), skills, dir.path().join("dotenv"));
        let output_path = dir.path().join("bash-output.txt");
        let command = concat!(
            "printf 'canary=%s skill=%s BASH_ENV=%s ENV=%s\\n' ",
            "\"${EGOPULSE_S2_CANARY-missing}\" \"$SKILL_TOKEN\" ",
            "\"${BASH_ENV-unset}\" \"${ENV-unset}\"; ",
            "printf 'SHELLOPTS=%s BASHOPTS=%s\\n' \"$SHELLOPTS\" \"$BASHOPTS\"; ",
            "printf 'cwd=%s\\n' \"$PWD\"; printf 'stderr-marker\\n' >&2"
        );

        // Act
        let mut child = tool
            .spawn_bash_command(command, &output_path, &skill_env)
            .expect("spawn bash");
        let status = child.wait().await.expect("wait for bash");
        let output = std::fs::read_to_string(&output_path).expect("output");

        // Assert
        assert!(status.success(), "{output}");
        assert!(output.contains("canary=missing"), "{output}");
        assert!(output.contains("skill=required-dummy"), "{output}");
        assert!(output.contains("BASH_ENV=unset ENV=unset"), "{output}");
        assert!(!output.contains("xtrace"), "{output}");
        assert!(!output.contains("extdebug"), "{output}");
        assert!(
            output.contains(&format!("cwd={}", workspace.display())),
            "{output}"
        );
        assert!(
            output.contains("stderr-marker"),
            "stderr is captured with stdout"
        );
        assert!(!home.join("startup-ran").exists());
        assert!(!home.join("profile-ran").exists());
    }

    #[tokio::test]
    async fn bash_timeout_kills_the_process_group() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let workspace = dir.path().join("workspace");
        std::fs::create_dir_all(&workspace).expect("workspace");
        let skills = Arc::new(crate::skills::SkillManager::from_dirs(
            dir.path().join("skills"),
            dir.path().join("builtin-skills"),
        ));
        let tool = BashTool::new(workspace, skills, dir.path().join("dotenv"));
        let marker = dir.path().join("orphan-process-ran");
        let command = format!("( sleep 2; touch '{}' ) & wait", marker.display());
        let context = crate::test_util::test_tool_context();

        // Act
        let result = tool
            .execute(
                serde_json::json!({"command": command, "timeout": 1}),
                &context,
            )
            .await;
        tokio::time::sleep(Duration::from_millis(2200)).await;

        // Assert
        assert!(result.is_error);
        assert_eq!(
            result.error_kind,
            Some(super::super::ToolErrorKind::Timeout)
        );
        assert!(result.content.contains("timed out after 1 seconds"));
        assert!(
            !marker.exists(),
            "a background process in the timed-out group must not continue"
        );
    }
}
