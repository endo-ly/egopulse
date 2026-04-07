use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use serde_json::json;

use crate::config::Config;
use crate::llm::ToolDefinition;
use crate::skills::{LoadedSkill, SkillManager};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolExecutionContext {
    pub chat_id: i64,
    pub channel: String,
    pub surface_thread: String,
    pub chat_type: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolResult {
    pub content: String,
    pub is_error: bool,
}

impl ToolResult {
    pub fn success(content: String) -> Self {
        Self {
            content,
            is_error: false,
        }
    }

    pub fn error(content: String) -> Self {
        Self {
            content,
            is_error: true,
        }
    }
}

#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn definition(&self) -> ToolDefinition;
    async fn execute(&self, input: serde_json::Value, context: &ToolExecutionContext)
    -> ToolResult;
}

pub struct ToolRegistry {
    tools: Vec<Box<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new(config: &Config, skill_manager: Arc<SkillManager>) -> Self {
        let workspace_dir = config.workspace_dir();
        if let Err(error) = std::fs::create_dir_all(&workspace_dir) {
            tracing::warn!(
                workspace_dir = %workspace_dir.display(),
                "failed to create workspace dir: {error}"
            );
        }

        Self {
            tools: vec![
                Box::new(PingTool),
                Box::new(TimeTool),
                Box::new(RuntimeStatusTool::new(config)),
                Box::new(ReadFileTool::new(workspace_dir)),
                Box::new(ActivateSkillTool::new(skill_manager)),
            ],
        }
    }

    pub fn definitions(&self) -> Vec<ToolDefinition> {
        self.tools.iter().map(|tool| tool.definition()).collect()
    }

    pub async fn execute(
        &self,
        name: &str,
        input: serde_json::Value,
        context: &ToolExecutionContext,
    ) -> ToolResult {
        for tool in &self.tools {
            if tool.name() == name {
                return tool.execute(input, context).await;
            }
        }
        ToolResult::error(format!("Unknown tool: {name}"))
    }
}

struct PingTool;

#[async_trait]
impl Tool for PingTool {
    fn name(&self) -> &str {
        "ping"
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "ping".to_string(),
            description: "Check that the tool runtime is alive.".to_string(),
            parameters: schema_object(json!({}), &[]),
        }
    }

    async fn execute(
        &self,
        _input: serde_json::Value,
        _context: &ToolExecutionContext,
    ) -> ToolResult {
        ToolResult::success("pong".to_string())
    }
}

struct TimeTool;

#[async_trait]
impl Tool for TimeTool {
    fn name(&self) -> &str {
        "time"
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "time".to_string(),
            description: "Return the current UTC time in RFC3339 format.".to_string(),
            parameters: schema_object(json!({}), &[]),
        }
    }

    async fn execute(
        &self,
        _input: serde_json::Value,
        _context: &ToolExecutionContext,
    ) -> ToolResult {
        ToolResult::success(Utc::now().to_rfc3339())
    }
}

struct RuntimeStatusTool {
    model: String,
    data_dir: String,
    workspace_dir: PathBuf,
    skills_dir: PathBuf,
}

impl RuntimeStatusTool {
    fn new(config: &Config) -> Self {
        Self {
            model: config.model.clone(),
            data_dir: config.data_dir.clone(),
            workspace_dir: config.workspace_dir(),
            skills_dir: config.skills_dir(),
        }
    }
}

#[async_trait]
impl Tool for RuntimeStatusTool {
    fn name(&self) -> &str {
        "runtime_status"
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "runtime_status".to_string(),
            description: "Return the current EgoPulse runtime context and directories.".to_string(),
            parameters: schema_object(json!({}), &[]),
        }
    }

    async fn execute(
        &self,
        _input: serde_json::Value,
        context: &ToolExecutionContext,
    ) -> ToolResult {
        ToolResult::success(
            serde_json::to_string_pretty(&json!({
                "model": self.model,
                "data_dir": self.data_dir,
                "workspace_dir": self.workspace_dir,
                "skills_dir": self.skills_dir,
                "channel": context.channel,
                "chat_type": context.chat_type,
                "surface_thread": context.surface_thread,
                "chat_id": context.chat_id,
            }))
            .expect("runtime status json"),
        )
    }
}

struct ReadFileTool {
    workspace_dir: PathBuf,
}

impl ReadFileTool {
    fn new(workspace_dir: PathBuf) -> Self {
        Self { workspace_dir }
    }
}

#[async_trait]
impl Tool for ReadFileTool {
    fn name(&self) -> &str {
        "read_file"
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "read_file".to_string(),
            description: "Read a file from the runtime workspace and return content with line numbers. Prefer paths relative to the workspace."
                .to_string(),
            parameters: schema_object(
                json!({
                    "path": {
                        "type": "string",
                        "description": "Path to the file, usually relative to the workspace"
                    },
                    "offset": {
                        "type": "integer",
                        "description": "1-based line offset"
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Maximum number of lines to read"
                    }
                }),
                &["path"],
            ),
        }
    }

    async fn execute(
        &self,
        input: serde_json::Value,
        _context: &ToolExecutionContext,
    ) -> ToolResult {
        let Some(path) = input.get("path").and_then(|value| value.as_str()) else {
            return ToolResult::error("Missing 'path' parameter".to_string());
        };
        let resolved = match resolve_workspace_path(&self.workspace_dir, path) {
            Ok(path) => path,
            Err(error) => return ToolResult::error(error),
        };

        let content = match tokio::fs::read_to_string(&resolved).await {
            Ok(content) => content,
            Err(error) => return ToolResult::error(format!("Failed to read file: {error}")),
        };

        let lines = content.lines().collect::<Vec<_>>();
        let offset = input
            .get("offset")
            .and_then(|value| value.as_u64())
            .map(|value| value.saturating_sub(1) as usize)
            .unwrap_or(0);
        let limit = input
            .get("limit")
            .and_then(|value| value.as_u64())
            .map(|value| value as usize)
            .unwrap_or(2000);
        let end = (offset + limit).min(lines.len());
        let selected = lines[offset..end]
            .iter()
            .enumerate()
            .map(|(index, line)| format!("{:>6}\t{}", offset + index + 1, line))
            .collect::<Vec<_>>()
            .join("\n");

        ToolResult::success(selected)
    }
}

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
            description: "Load the full instructions for a discovered skill. Use this when a skill from the available skills catalog matches the task."
                .to_string(),
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

fn resolve_workspace_path(workspace_dir: &Path, requested_path: &str) -> Result<PathBuf, String> {
    let requested = PathBuf::from(requested_path);
    let candidate = if requested.is_absolute() {
        requested
    } else {
        workspace_dir.join(requested)
    };

    let mut normalized = PathBuf::new();
    for component in candidate.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::RootDir | Component::Prefix(_) | Component::Normal(_) => {
                normalized.push(component.as_os_str())
            }
        }
    }

    if !normalized.starts_with(workspace_dir) {
        return Err(format!(
            "Refusing to access path outside workspace: {}",
            normalized.display()
        ));
    }

    Ok(normalized)
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
    use super::{ToolExecutionContext, ToolRegistry};
    use crate::config::{ChannelConfig, Config};
    use crate::skills::SkillManager;

    use serde_json::json;
    use serial_test::serial;
    use std::sync::Arc;

    struct HomeGuard {
        original: Option<std::ffi::OsString>,
    }

    impl HomeGuard {
        fn set(path: &std::path::Path) -> Self {
            let original = std::env::var_os("HOME");
            unsafe {
                std::env::set_var("HOME", path);
            }
            Self { original }
        }
    }

    impl Drop for HomeGuard {
        fn drop(&mut self) {
            match &self.original {
                Some(value) => unsafe {
                    std::env::set_var("HOME", value);
                },
                None => unsafe {
                    std::env::remove_var("HOME");
                },
            }
        }
    }

    fn test_config(data_dir: &str) -> Config {
        Config {
            model: "gpt-4o-mini".to_string(),
            api_key: None,
            llm_base_url: "http://127.0.0.1:1234/v1".to_string(),
            data_dir: data_dir.to_string(),
            log_level: "info".to_string(),
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

    #[tokio::test]
    #[serial]
    async fn read_file_respects_workspace() {
        let dir = tempfile::tempdir().expect("tempdir");
        let _home = HomeGuard::set(dir.path());
        let config = test_config(dir.path().to_str().expect("utf8"));
        let workspace = config.workspace_dir();
        std::fs::create_dir_all(&workspace).expect("workspace");
        std::fs::write(workspace.join("notes.txt"), "hello\nworld").expect("write file");
        let registry = ToolRegistry::new(
            &config,
            Arc::new(SkillManager::from_skills_dir(config.skills_dir())),
        );

        let result = registry
            .execute("read_file", json!({"path": "notes.txt"}), &test_context())
            .await;
        assert!(!result.is_error);
        assert!(result.content.contains("hello"));
    }

    #[tokio::test]
    #[serial]
    async fn activate_skill_loads_skill_instructions() {
        let dir = tempfile::tempdir().expect("tempdir");
        let _home = HomeGuard::set(dir.path());
        let config = test_config(dir.path().to_str().expect("utf8"));
        let skills_dir = config.skills_dir();
        std::fs::create_dir_all(skills_dir.join("pdf")).expect("skill dir");
        std::fs::write(
            skills_dir.join("pdf").join("SKILL.md"),
            "---\nname: pdf\ndescription: PDF helper\n---\nUse the PDF flow.\n",
        )
        .expect("write skill");
        let registry = ToolRegistry::new(
            &config,
            Arc::new(SkillManager::from_skills_dir(config.skills_dir())),
        );

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
}
