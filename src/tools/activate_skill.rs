//! activate_skill ツール。
//!
//! スキルの完全な手順を名前でオンデマンド読み込みする。

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;

use crate::llm::ToolDefinition;
use crate::skills::{LoadedSkill, SkillManager};

use super::{Tool, ToolExecutionContext, ToolResult, parse_params, schema_object};

/// Loads a skill's full instructions on demand by name.
pub(crate) struct ActivateSkillTool {
    skill_manager: Arc<SkillManager>,
}

impl ActivateSkillTool {
    pub(crate) fn new(skill_manager: Arc<SkillManager>) -> Self {
        Self { skill_manager }
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
        _context: &ToolExecutionContext,
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
