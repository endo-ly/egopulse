//! LLM エージェント向けファイル操作・シェルツール群。
//!
//! ワークスペース内で動作する read / write / edit / bash / grep / find / ls の
//! 7 種のファイル操作ツールと、スキル遅延読み込み用の activate_skill を提供する。
//! 各ツールは出力を行数・バイト数で切り詰め、LLM のコンテキストウィンドウに収まるよう制御する。

mod command_guard;
mod files;
mod mcp_adapter;
mod path_guard;
mod sanitizer;
mod search;
mod shell;
mod text;

#[allow(unused_imports)] // re-export for future use from other modules
pub(crate) use command_guard::*;
pub(crate) use files::*;
#[allow(unused_imports)] // re-export for future use from other modules
pub(crate) use mcp_adapter::*;
#[allow(unused_imports)] // re-export for future use from other modules
pub(crate) use path_guard::*;
pub(crate) use sanitizer::*;
pub(crate) use search::*;
pub(crate) use shell::*;
pub(crate) use text::*;

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;

use crate::config::Config;
use crate::llm::ToolDefinition;
use crate::skills::{LoadedSkill, SkillManager};

const DEFAULT_MAX_LINES: usize = 2000;
const DEFAULT_MAX_BYTES: usize = 50 * 1024;
const DEFAULT_FIND_LIMIT: usize = 1000;
const DEFAULT_GREP_LIMIT: usize = 100;
const DEFAULT_LS_LIMIT: usize = 500;
const GREP_MAX_LINE_LENGTH: usize = 500;
const DEFAULT_BASH_TIMEOUT_SECS: u64 = 30;
const DEFAULT_GREP_TIMEOUT_SECS: u64 = 30;

/// Contextual metadata passed to every tool execution (chat identity, channel, thread).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolExecutionContext {
    pub chat_id: i64,
    pub channel: String,
    pub surface_thread: String,
    pub chat_type: String,
}

/// Uniform result type returned by all tool implementations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolResult {
    pub content: String,
    pub is_error: bool,
    pub details: Option<serde_json::Value>,
    pub llm_content: crate::llm::MessageContent,
}

impl ToolResult {
    /// Create a successful result with plain text content.
    pub fn success(content: String) -> Self {
        Self {
            llm_content: crate::llm::MessageContent::text(content.clone()),
            content,
            is_error: false,
            details: None,
        }
    }

    /// Create a successful result with structured details (e.g. truncation metadata).
    pub fn success_with_details(content: String, details: serde_json::Value) -> Self {
        Self {
            llm_content: crate::llm::MessageContent::text(content.clone()),
            content,
            is_error: false,
            details: Some(details),
        }
    }

    /// Create a successful result with separate LLM-facing multimodal content (e.g. images).
    pub fn success_with_llm_content(
        content: String,
        llm_content: crate::llm::MessageContent,
    ) -> Self {
        Self {
            content,
            is_error: false,
            details: None,
            llm_content,
        }
    }

    /// Create an error result with plain text content.
    pub fn error(content: String) -> Self {
        Self {
            llm_content: crate::llm::MessageContent::text(content.clone()),
            content,
            is_error: true,
            details: None,
        }
    }

    /// Create an error result with structured details.
    pub fn error_with_details(content: String, details: serde_json::Value) -> Self {
        Self {
            llm_content: crate::llm::MessageContent::text(content.clone()),
            content,
            is_error: true,
            details: Some(details),
        }
    }
}

/// Trait implemented by every tool available to the LLM agent.
#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn definition(&self) -> ToolDefinition;
    async fn execute(&self, input: serde_json::Value, context: &ToolExecutionContext)
    -> ToolResult;
}

/// Owns all tool instances and dispatches execution by tool name.
pub struct ToolRegistry {
    tools: Vec<Box<dyn Tool>>,
    config_secrets: Vec<(String, String)>,
}

impl ToolRegistry {
    /// Instantiate all built-in tools scoped to the configured workspace.
    pub fn new(config: &Config, skill_manager: Arc<SkillManager>) -> Self {
        let workspace_dir = match config.workspace_dir() {
            Ok(dir) => dir,
            Err(error) => {
                tracing::warn!("failed to resolve workspace dir: {error}");
                return Self {
                    tools: vec![Box::new(ActivateSkillTool::new(skill_manager))],
                    config_secrets: collect_config_secrets(config),
                };
            }
        };
        if let Err(error) = std::fs::create_dir_all(&workspace_dir) {
            tracing::warn!(
                workspace_dir = %workspace_dir.display(),
                "failed to create workspace dir: {error}"
            );
        }

        Self {
            tools: vec![
                Box::new(ReadTool::new(workspace_dir.clone())),
                Box::new(BashTool::new(workspace_dir.clone())),
                Box::new(EditTool::new(workspace_dir.clone())),
                Box::new(WriteTool::new(workspace_dir.clone())),
                Box::new(GrepTool::new(workspace_dir.clone())),
                Box::new(FindTool::new(workspace_dir.clone())),
                Box::new(LsTool::new(workspace_dir)),
                Box::new(ActivateSkillTool::new(skill_manager)),
            ],
            config_secrets: collect_config_secrets(config),
        }
    }

    pub fn register_tool(&mut self, tool: Box<dyn Tool>) {
        self.tools.push(tool);
    }

    /// Collect tool definitions asynchronously.
    pub async fn definitions_async(&self) -> Vec<ToolDefinition> {
        self.tools.iter().map(|tool| tool.definition()).collect()
    }

    /// Find and execute a tool by name. Returns an error result for unknown tools.
    pub async fn execute(
        &self,
        name: &str,
        input: serde_json::Value,
        context: &ToolExecutionContext,
    ) -> ToolResult {
        for tool in &self.tools {
            if tool.name() == name {
                let result = tool.execute(input, context).await;
                return sanitize_tool_result(result, &self.config_secrets);
            }
        }
        sanitize_tool_result(
            ToolResult::error(format!("Unknown tool: {name}")),
            &self.config_secrets,
        )
    }
}

/// Loads a skill's full instructions on demand by name.
struct ActivateSkillTool {
    skill_manager: Arc<SkillManager>,
}

impl ActivateSkillTool {
    fn new(skill_manager: Arc<SkillManager>) -> Self {
        Self { skill_manager }
    }
}

#[async_trait]
impl Tool for ActivateSkillTool {
    fn name(&self) -> &str {
        "activate_skill"
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "activate_skill".to_string(),
            description: "Load the full instructions for a discovered skill. Use this when a skill from the available skills catalog matches the task.".to_string(),
            parameters: schema_object(
                json!({
                    "skill_name": {
                        "type": "string",
                        "description": "The skill name to load"
                    }
                }),
                &["skill_name"],
            ),
        }
    }

    async fn execute(
        &self,
        input: serde_json::Value,
        _context: &ToolExecutionContext,
    ) -> ToolResult {
        let Some(skill_name) = input.get("skill_name").and_then(|value| value.as_str()) else {
            return ToolResult::error("Missing required parameter: skill_name".to_string());
        };

        match self.skill_manager.load_skill_checked(skill_name) {
            Ok(LoadedSkill {
                metadata,
                instructions,
            }) => ToolResult::success(format!(
                "# Skill: {}\n\nDescription: {}\nSkill directory: {}\n\n## Instructions\n\n{}",
                metadata.name,
                metadata.description,
                metadata.dir_path.display(),
                instructions
            )),
            Err(error) => ToolResult::error(error),
        }
    }
}

fn truncation_json(truncation: &TruncationResult) -> serde_json::Value {
    json!({
        "truncated": truncation.truncated,
        "truncatedBy": truncation.truncated_by,
        "totalLines": truncation.total_lines,
        "totalBytes": truncation.total_bytes,
        "outputLines": truncation.output_lines,
        "outputBytes": truncation.output_bytes,
        "lastLinePartial": truncation.last_line_partial,
        "firstLineExceedsLimit": truncation.first_line_exceeds_limit,
        "maxLines": truncation.max_lines,
        "maxBytes": truncation.max_bytes
    })
}

fn schema_object(properties: serde_json::Value, required: &[&str]) -> serde_json::Value {
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
    })
}

#[cfg(test)]
mod tests {
    use super::{Tool, ToolExecutionContext, ToolRegistry, ToolResult};
    use crate::config::{ChannelConfig, Config, ProviderConfig};
    use crate::llm::{MessageContent, MessageContentPart, ToolDefinition};
    use crate::skills::SkillManager;
    use crate::test_env::EnvVarGuard;

    use serde_json::json;
    use serial_test::serial;
    use std::sync::Arc;

    struct StaticTool {
        name: &'static str,
        result: ToolResult,
    }

    #[async_trait::async_trait]
    impl Tool for StaticTool {
        fn name(&self) -> &str {
            self.name
        }

        fn definition(&self) -> ToolDefinition {
            ToolDefinition {
                name: self.name.to_string(),
                description: "test tool".to_string(),
                parameters: json!({"type": "object"}),
            }
        }

        async fn execute(
            &self,
            _input: serde_json::Value,
            _context: &ToolExecutionContext,
        ) -> ToolResult {
            self.result.clone()
        }
    }

    fn test_config(state_root: &str) -> Config {
        Config {
            default_provider: "local".to_string(),
            default_model: Some("gpt-4o-mini".to_string()),
            providers: std::collections::HashMap::from([(
                "local".to_string(),
                ProviderConfig {
                    label: "Local".to_string(),
                    base_url: "http://127.0.0.1:1234/v1".to_string(),
                    api_key: None,
                    default_model: "gpt-4o-mini".to_string(),
                    models: vec!["gpt-4o-mini".to_string()],
                },
            )]),
            state_root: state_root.to_string(),
            log_level: "info".to_string(),
            compaction_timeout_secs: 180,
            max_history_messages: 50,
            max_session_messages: 40,
            compact_keep_recent: 20,
            channels: std::collections::HashMap::from([(
                "web".to_string(),
                ChannelConfig {
                    enabled: Some(true),
                    ..Default::default()
                },
            )]),
        }
    }

    fn test_context() -> ToolExecutionContext {
        ToolExecutionContext {
            chat_id: 1,
            channel: "cli".to_string(),
            surface_thread: "demo".to_string(),
            chat_type: "cli".to_string(),
        }
    }

    fn test_registry(config: &Config) -> ToolRegistry {
        ToolRegistry::new(
            config,
            Arc::new(SkillManager::from_skills_dir(
                config.skills_dir().expect("skills_dir"),
            )),
        )
    }

    #[tokio::test]
    #[serial]
    async fn read_respects_workspace() {
        let dir = tempfile::tempdir().expect("tempdir");
        let _home = EnvVarGuard::set("HOME", dir.path());
        let config = test_config(dir.path().to_str().expect("utf8"));
        let workspace = config.workspace_dir().expect("workspace_dir");
        std::fs::create_dir_all(&workspace).expect("workspace");
        std::fs::write(workspace.join("notes.txt"), "hello\nworld").expect("write file");
        let registry = test_registry(&config);

        let result = registry
            .execute("read", json!({"path": "notes.txt"}), &test_context())
            .await;
        assert!(!result.is_error);
        assert!(result.content.contains("hello"));
    }

    #[tokio::test]
    #[serial]
    async fn read_reports_supported_images() {
        let dir = tempfile::tempdir().expect("tempdir");
        let _home = EnvVarGuard::set("HOME", dir.path());
        let config = test_config(dir.path().to_str().expect("utf8"));
        let workspace = config.workspace_dir().expect("workspace_dir");
        std::fs::create_dir_all(&workspace).expect("workspace");
        std::fs::write(
            workspace.join("pixel.png"),
            [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A],
        )
        .expect("png");
        let registry = test_registry(&config);

        let result = registry
            .execute("read", json!({"path": "pixel.png"}), &test_context())
            .await;
        assert!(!result.is_error);
        assert!(result.content.contains("Read image file [image/png]"));
        match result.llm_content {
            crate::llm::MessageContent::Parts(parts) => {
                assert_eq!(parts.len(), 2);
                assert!(matches!(
                    &parts[1],
                    crate::llm::MessageContentPart::InputImage { .. }
                ));
            }
            other => panic!("expected multimodal llm_content, got {other:?}"),
        }
    }

    #[tokio::test]
    #[serial]
    async fn write_creates_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let _home = EnvVarGuard::set("HOME", dir.path());
        let config = test_config(dir.path().to_str().expect("utf8"));
        let registry = test_registry(&config);

        std::fs::create_dir_all(config.workspace_dir().expect("workspace_dir").join("src"))
            .expect("create src dir");

        let result = registry
            .execute(
                "write",
                json!({"path": "src/demo.txt", "content": "hello world"}),
                &test_context(),
            )
            .await;
        assert!(!result.is_error);
        assert!(result.content.contains("Successfully wrote 11 bytes"));
        assert_eq!(
            std::fs::read_to_string(
                config
                    .workspace_dir()
                    .expect("workspace_dir")
                    .join("src/demo.txt")
            )
            .expect("read"),
            "hello world"
        );
    }

    #[tokio::test]
    #[serial]
    async fn edit_replaces_exact_match_and_returns_diff() {
        let dir = tempfile::tempdir().expect("tempdir");
        let _home = EnvVarGuard::set("HOME", dir.path());
        let config = test_config(dir.path().to_str().expect("utf8"));
        let workspace = config.workspace_dir().expect("workspace_dir");
        std::fs::create_dir_all(&workspace).expect("workspace");
        std::fs::write(workspace.join("notes.txt"), "alpha\nbeta\ngamma\n").expect("write");
        let registry = test_registry(&config);

        let result = registry
            .execute(
                "edit",
                json!({
                    "path": "notes.txt",
                    "edits": [{"oldText": "beta", "newText": "delta"}]
                }),
                &test_context(),
            )
            .await;
        assert!(!result.is_error, "{}", result.content);
        let content = std::fs::read_to_string(workspace.join("notes.txt")).expect("read");
        assert!(content.contains("delta"));
        assert_eq!(
            result
                .details
                .as_ref()
                .and_then(|details| details.get("firstChangedLine"))
                .and_then(|value| value.as_u64()),
            Some(2)
        );
        assert!(
            result
                .details
                .as_ref()
                .and_then(|details| details.get("diff"))
                .and_then(|value| value.as_str())
                .unwrap_or_default()
                .contains("-beta")
        );
    }

    #[tokio::test]
    #[serial]
    async fn ls_lists_directory_entries() {
        let dir = tempfile::tempdir().expect("tempdir");
        let _home = EnvVarGuard::set("HOME", dir.path());
        let config = test_config(dir.path().to_str().expect("utf8"));
        let workspace = config.workspace_dir().expect("workspace_dir");
        std::fs::create_dir_all(workspace.join("nested")).expect("nested");
        std::fs::write(workspace.join("a.txt"), "a").expect("a");
        std::fs::write(workspace.join(".hidden"), "b").expect("hidden");
        let registry = test_registry(&config);

        let result = registry.execute("ls", json!({}), &test_context()).await;
        assert!(!result.is_error);
        assert!(result.content.contains(".hidden"));
        assert!(result.content.contains("nested/"));
    }

    #[tokio::test]
    #[serial]
    async fn find_discovers_matching_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        let _home = EnvVarGuard::set("HOME", dir.path());
        let config = test_config(dir.path().to_str().expect("utf8"));
        let workspace = config.workspace_dir().expect("workspace_dir");
        std::fs::create_dir_all(workspace.join("src")).expect("src");
        std::fs::write(workspace.join("src/lib.rs"), "pub fn demo() {}").expect("lib");
        let registry = test_registry(&config);

        let result = registry
            .execute("find", json!({"pattern": "*.rs"}), &test_context())
            .await;
        assert!(!result.is_error, "{}", result.content);
        assert!(result.content.contains("src/lib.rs"));
    }

    #[tokio::test]
    #[serial]
    async fn grep_finds_matching_lines() {
        let dir = tempfile::tempdir().expect("tempdir");
        let _home = EnvVarGuard::set("HOME", dir.path());
        let config = test_config(dir.path().to_str().expect("utf8"));
        let workspace = config.workspace_dir().expect("workspace_dir");
        std::fs::create_dir_all(workspace.join("src")).expect("src");
        std::fs::write(workspace.join("src/lib.rs"), "pub fn demo() {}\n").expect("lib");
        let registry = test_registry(&config);

        let result = registry
            .execute("grep", json!({"pattern": "demo"}), &test_context())
            .await;
        assert!(!result.is_error, "{}", result.content);
        assert!(result.content.contains("src/lib.rs:1:pub fn demo() {}"));
    }

    #[tokio::test]
    #[serial]
    async fn bash_runs_in_workspace() {
        let dir = tempfile::tempdir().expect("tempdir");
        let _home = EnvVarGuard::set("HOME", dir.path());
        let config = test_config(dir.path().to_str().expect("utf8"));
        let workspace = config.workspace_dir().expect("workspace_dir");
        std::fs::create_dir_all(&workspace).expect("workspace");
        std::fs::write(workspace.join("notes.txt"), "hello").expect("notes");
        let registry = test_registry(&config);

        let result = registry
            .execute(
                "bash",
                json!({"command": "printf 'ok\\n'; cat notes.txt"}),
                &test_context(),
            )
            .await;
        assert!(!result.is_error, "{}", result.content);
        assert!(result.content.contains("ok"));
        assert!(result.content.contains("hello"));
        let bash_temp_dir = workspace.join(".tmp").join("bash");
        assert!(bash_temp_dir.is_dir());
        assert_eq!(
            std::fs::read_dir(&bash_temp_dir)
                .expect("bash temp dir entries")
                .count(),
            0
        );
    }

    #[tokio::test]
    #[serial]
    async fn activate_skill_loads_skill_instructions() {
        let dir = tempfile::tempdir().expect("tempdir");
        let _home = EnvVarGuard::set("HOME", dir.path());
        let config = test_config(dir.path().to_str().expect("utf8"));
        let skills_dir = config.skills_dir().expect("skills_dir");
        std::fs::create_dir_all(skills_dir.join("pdf")).expect("skill dir");
        std::fs::write(
            skills_dir.join("pdf").join("SKILL.md"),
            "---\nname: pdf\ndescription: PDF helper\n---\nUse the PDF flow.\n",
        )
        .expect("write skill");
        let registry = test_registry(&config);

        let result = registry
            .execute(
                "activate_skill",
                json!({"skill_name": "pdf"}),
                &test_context(),
            )
            .await;
        assert!(!result.is_error);
        assert!(result.content.contains("# Skill: pdf"));
        assert!(result.content.contains("Use the PDF flow."));
    }

    #[tokio::test]
    #[serial]
    async fn redacts_error_result_fields_before_returning() {
        let dir = tempfile::tempdir().expect("tempdir");
        let _home = EnvVarGuard::set("HOME", dir.path());
        let mut config = test_config(dir.path().to_str().expect("utf8"));
        config.channels.insert(
            "discord".to_string(),
            ChannelConfig {
                file_bot_token: Some("sk-secret-token-123".to_string()),
                ..Default::default()
            },
        );
        let mut registry = test_registry(&config);
        registry.register_tool(Box::new(StaticTool {
            name: "leaky_error",
            result: ToolResult::error_with_details(
                "error exposes sk-secret-token-123".to_string(),
                json!({"trace":"sk-secret-token-123"}),
            ),
        }));

        let result = registry
            .execute("leaky_error", json!({}), &test_context())
            .await;

        assert!(result.is_error);
        assert!(!result.content.contains("sk-secret-token-123"));
        assert!(result.content.contains("[REDACTED:"));
        assert!(
            result
                .details
                .as_ref()
                .and_then(|d| d.get("trace"))
                .and_then(|v| v.as_str())
                .is_some_and(|text| !text.contains("sk-secret-token-123"))
        );
        match &result.llm_content {
            MessageContent::Text(text) => assert!(!text.contains("sk-secret-token-123")),
            other => panic!("expected text llm content, got {other:?}"),
        }
    }

    #[tokio::test]
    #[serial]
    async fn redacts_short_configured_secret_values() {
        let dir = tempfile::tempdir().expect("tempdir");
        let _home = EnvVarGuard::set("HOME", dir.path());
        let mut config = test_config(dir.path().to_str().expect("utf8"));
        config.channels.insert(
            "discord".to_string(),
            ChannelConfig {
                auth_token: Some("xyz".to_string()),
                ..Default::default()
            },
        );
        let mut registry = test_registry(&config);
        registry.register_tool(Box::new(StaticTool {
            name: "leaky_short_secret",
            result: ToolResult::success("short secret xyz leaked".to_string()),
        }));

        let result = registry
            .execute("leaky_short_secret", json!({}), &test_context())
            .await;

        assert!(!result.is_error);
        assert!(!result.content.contains(" xyz "));
        assert!(
            result
                .content
                .contains("[REDACTED:channel.discord.auth_token]")
        );
    }

    #[tokio::test]
    #[serial]
    async fn ignores_empty_configured_secret_values() {
        let dir = tempfile::tempdir().expect("tempdir");
        let _home = EnvVarGuard::set("HOME", dir.path());
        let mut config = test_config(dir.path().to_str().expect("utf8"));
        config.channels.insert(
            "discord".to_string(),
            ChannelConfig {
                auth_token: Some(String::new()),
                ..Default::default()
            },
        );
        let mut registry = test_registry(&config);
        registry.register_tool(Box::new(StaticTool {
            name: "empty_secret",
            result: ToolResult::success("hello".to_string()),
        }));

        let result = registry
            .execute("empty_secret", json!({}), &test_context())
            .await;
        assert!(!result.is_error);
        assert_eq!(result.content, "hello");
    }

    #[tokio::test]
    #[serial]
    async fn redacts_multimodal_llm_content_before_returning() {
        let dir = tempfile::tempdir().expect("tempdir");
        let _home = EnvVarGuard::set("HOME", dir.path());
        let mut config = test_config(dir.path().to_str().expect("utf8"));
        config.channels.insert(
            "discord".to_string(),
            ChannelConfig {
                file_auth_token: Some("sk-multimodal-secret".to_string()),
                ..Default::default()
            },
        );
        let mut registry = test_registry(&config);
        registry.register_tool(Box::new(StaticTool {
            name: "leaky_multimodal",
            result: ToolResult::success_with_llm_content(
                "ok".to_string(),
                MessageContent::parts(vec![
                    MessageContentPart::InputText {
                        text: "payload sk-multimodal-secret".to_string(),
                    },
                    MessageContentPart::InputImage {
                        image_url: "https://example.com/image?token=sk-multimodal-secret"
                            .to_string(),
                        detail: Some("sk-multimodal-secret".to_string()),
                    },
                ]),
            ),
        }));

        let result = registry
            .execute("leaky_multimodal", json!({}), &test_context())
            .await;

        assert!(!result.is_error);
        match &result.llm_content {
            MessageContent::Parts(parts) => {
                assert_eq!(parts.len(), 2);
                match &parts[0] {
                    MessageContentPart::InputText { text } => {
                        assert!(!text.contains("sk-multimodal-secret"));
                    }
                    _ => panic!("expected InputText"),
                }
                match &parts[1] {
                    MessageContentPart::InputImage { image_url, detail } => {
                        assert!(!image_url.contains("sk-multimodal-secret"));
                        assert!(
                            detail
                                .as_deref()
                                .is_some_and(|value| !value.contains("sk-multimodal-secret"))
                        );
                    }
                    _ => panic!("expected InputImage"),
                }
            }
            other => panic!("expected parts llm content, got {other:?}"),
        }
    }

    #[tokio::test]
    #[serial]
    async fn find_and_ls_filter_sensitive_entries() {
        let dir = tempfile::tempdir().expect("tempdir");
        let _home = EnvVarGuard::set("HOME", dir.path());
        let config = test_config(dir.path().to_str().expect("utf8"));
        let workspace = config.workspace_dir().expect("workspace_dir");
        std::fs::create_dir_all(workspace.join("src")).expect("src");
        std::fs::create_dir_all(workspace.join(".ssh")).expect(".ssh");
        std::fs::write(workspace.join("src/lib.rs"), "pub fn demo() {}").expect("lib");
        std::fs::write(workspace.join(".env"), "SECRET=1").expect(".env");
        std::fs::write(workspace.join(".ssh/id_rsa"), "private").expect("id_rsa");
        let registry = test_registry(&config);

        let ls_result = registry.execute("ls", json!({}), &test_context()).await;
        assert!(!ls_result.is_error, "{}", ls_result.content);
        assert!(ls_result.content.contains("src/"));
        assert!(!ls_result.content.contains(".env"));
        assert!(!ls_result.content.contains(".ssh/"));

        let find_result = registry
            .execute("find", json!({"pattern": "*"}), &test_context())
            .await;
        assert!(!find_result.is_error, "{}", find_result.content);
        assert!(find_result.content.contains("src/lib.rs"));
        assert!(!find_result.content.contains(".env"));
        assert!(!find_result.content.contains(".ssh/id_rsa"));
    }
}
