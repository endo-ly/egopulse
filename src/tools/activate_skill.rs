//! activate_skill ツール。
//!
//! スキルの完全な手順を名前でオンデマンド読み込みする。

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;

use crate::config::secret_ref::read_dotenv;
use crate::llm::ToolDefinition;
use crate::skills::{LoadedSkill, SkillManager, resolve_required_env};

use super::{Tool, ToolExecutionContext, ToolResult, parse_params, schema_object};

/// Loads a skill's full instructions on demand by name.
pub(crate) struct ActivateSkillTool {
    skill_manager: Arc<SkillManager>,
    dotenv_path: PathBuf,
}

impl ActivateSkillTool {
    pub(crate) fn new(skill_manager: Arc<SkillManager>, dotenv_path: PathBuf) -> Self {
        Self {
            skill_manager,
            dotenv_path,
        }
    }
}

#[async_trait]
impl Tool for ActivateSkillTool {
    fn name(&self) -> &str {
        "activate_skill"
    }

    fn is_read_only(&self) -> bool {
        true
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
        context: &ToolExecutionContext,
    ) -> ToolResult {
        #[derive(serde::Deserialize)]
        struct Params {
            skill_name: String,
        }

        let params: Params = match parse_params(input) {
            Ok(p) => p,
            Err(e) => return e,
        };

        match self.skill_manager.load_skill_checked(&params.skill_name) {
            Ok(LoadedSkill {
                metadata,
                instructions,
            }) => {
                if !metadata.required_env.is_empty() {
                    let dotenv = read_dotenv(&self.dotenv_path);
                    let resolved = resolve_required_env(&metadata.required_env, &dotenv);
                    let resolved_keys: Vec<&str> = resolved.keys().map(|k| k.as_str()).collect();
                    let missing: Vec<&String> = metadata
                        .required_env
                        .iter()
                        .filter(|k| !resolved.contains_key(k.as_str()))
                        .collect();

                    let mut env_section = String::from("\n\n## Environment Variables\n");
                    for key in &resolved_keys {
                        env_section.push_str(&format!("- {key} ✓\n"));
                    }
                    for key in &missing {
                        env_section.push_str(&format!("- {key} ✗ (not found)\n"));
                    }

                    if !resolved.is_empty() {
                        tracing::info!(skill = %metadata.name, keys = ?resolved_keys, "resolved skill env vars");
                    }
                    if !missing.is_empty() {
                        tracing::warn!(skill = %metadata.name, missing = ?missing, "some required_env keys not found");
                    }

                    *context.skill_env.lock().expect("skill env lock") = resolved;

                    ToolResult::success(format!(
                        "# Skill: {}\n\nDescription: {}\nSkill directory: {}\n{}\n\n## Instructions\n\n{}",
                        metadata.name,
                        metadata.description,
                        metadata.dir_path.display(),
                        env_section.trim_end(),
                        instructions
                    ))
                } else {
                    context.skill_env.lock().expect("skill env lock").clear();
                    ToolResult::success(format!(
                        "# Skill: {}\n\nDescription: {}\nSkill directory: {}\n\n## Instructions\n\n{}",
                        metadata.name,
                        metadata.description,
                        metadata.dir_path.display(),
                        instructions
                    ))
                }
            }
            Err(error) => ToolResult::error(error),
        }
    }
}
