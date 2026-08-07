//! send_attachment ツール。
//!
//! エージェントが明示的にファイルをチャネルへ送信するためのツール。
//! 普段の会話はランタイムが自動送信するため、このツールはファイル配布が必要な場合に使用する。

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;

use crate::agent_loop::ConversationScope;
use crate::channels::adapter::ChannelRegistry;
use crate::llm::ToolDefinition;
use crate::storage::{Database, call_blocking};

use super::path_guard;
use super::search::resolve_workspace_path;
use super::{Tool, ToolExecutionContext, ToolResult, schema_object};

/// Tool for sending file attachments with an optional caption to the conversation channel.
///
/// Normal text responses are auto-sent by the runtime; this tool exists only for
/// cases that require explicit file attachment delivery.
pub(crate) struct SendAttachmentTool {
    workspace_dir: PathBuf,
    channels: Arc<ChannelRegistry>,
    db: Arc<Database>,
    secret_db: Option<Arc<Database>>,
}

impl SendAttachmentTool {
    pub(crate) fn new(
        workspace_dir: PathBuf,
        channels: Arc<ChannelRegistry>,
        db: Arc<Database>,
        secret_db: Option<Arc<Database>>,
    ) -> Self {
        Self {
            workspace_dir,
            channels,
            db,
            secret_db,
        }
    }

    fn db_for(&self, scope: ConversationScope) -> &Arc<Database> {
        match scope {
            ConversationScope::Normal => &self.db,
            ConversationScope::Secret => self
                .secret_db
                .as_ref()
                .expect("secret db required for secret mode send_attachment"),
        }
    }
}

#[async_trait]
impl Tool for SendAttachmentTool {
    fn name(&self) -> &str {
        "send_attachment"
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "send_attachment".to_string(),
            description: "Send a file attachment to the current conversation, optionally with a caption. Use this when you need to deliver a file to the user. For normal text responses, just write your reply — do not use this tool.".to_string(),
            parameters: schema_object(
                json!({
                    "attachment_path": {
                        "type": "string",
                        "description": "Local file path to send as an attachment (required)"
                    },
                    "caption": {
                        "type": "string",
                        "description": "Optional text to include with the attached file"
                    }
                }),
                &["attachment_path"],
            ),
        }
    }

    async fn execute(
        &self,
        input: serde_json::Value,
        context: &ToolExecutionContext,
    ) -> ToolResult {
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Params {
            #[serde(default)]
            attachment_path: Option<String>,
            #[serde(default)]
            caption: Option<String>,
        }

        let params: Params = match super::parse_params(input) {
            Ok(p) => p,
            Err(e) => return e,
        };
        let path_str = match params.attachment_path.filter(|s| !s.trim().is_empty()) {
            Some(path) => path,
            None => {
                return ToolResult::error(
                    "Missing required parameter: attachment_path".to_string(),
                );
            }
        };
        let caption = params.caption.filter(|s| !s.trim().is_empty());

        let chat_id = context.chat_id;
        let chat_info = match call_blocking(Arc::clone(self.db_for(context.scope)), move |db| {
            db.get_chat_by_id(chat_id)
        })
        .await
        {
            Ok(Some(info)) => info,
            Ok(None) => {
                return ToolResult::error(format!("no chat found for chat_id {chat_id}"));
            }
            Err(e) => return ToolResult::error(format!("failed to resolve chat info: {e}")),
        };

        let adapter = match self.channels.get(&chat_info.channel) {
            Some(a) => a,
            None => {
                return ToolResult::error(format!(
                    "no adapter for channel '{}'",
                    chat_info.channel
                ));
            }
        };

        let resolved = match resolve_workspace_path(&self.workspace_dir, &path_str) {
            Ok(p) => p,
            Err(e) => return ToolResult::error(e),
        };

        if let Err(reason) = path_guard::check_path(&resolved.to_string_lossy()) {
            return ToolResult::error(reason);
        }

        if !resolved.is_file() {
            return ToolResult::error(format!("File not found: {path_str}"));
        }

        match adapter
            .send_attachment(&chat_info.external_chat_id, &resolved, caption.as_deref())
            .await
        {
            Ok(()) => ToolResult::success("Attachment sent successfully".to_string()),
            Err(e) => ToolResult::error(format!("Failed to send attachment: {e}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn test_tool(state_root: &std::path::Path) -> SendAttachmentTool {
        let db_path = state_root.join("egopulse.db");
        SendAttachmentTool::new(
            state_root.to_path_buf(),
            Arc::new(ChannelRegistry::new()),
            Arc::new(Database::new(&db_path).expect("database")),
            None,
        )
    }

    #[test]
    fn definition_requires_attachment_path_and_exposes_caption() {
        let dir = tempfile::tempdir().expect("tempdir");
        let tool = test_tool(dir.path());

        let definition = tool.definition();

        assert_eq!(definition.name, "send_attachment");
        assert_eq!(
            definition.parameters["required"],
            json!(["attachment_path"])
        );
        assert!(
            definition.parameters["properties"]
                .get("attachment_path")
                .is_some()
        );
        assert!(definition.parameters["properties"].get("caption").is_some());
        assert!(definition.parameters["properties"].get("text").is_none());
    }

    #[tokio::test]
    async fn execute_rejects_missing_or_legacy_text_input() {
        let dir = tempfile::tempdir().expect("tempdir");
        let tool = test_tool(dir.path());
        let context = crate::test_util::test_tool_context();

        let missing_path = tool.execute(json!({}), &context).await;
        assert!(missing_path.is_error);
        assert_eq!(
            missing_path.content,
            "Missing required parameter: attachment_path"
        );

        let legacy_text = tool.execute(json!({"text": "hello"}), &context).await;
        assert!(legacy_text.is_error);
        assert!(legacy_text.content.contains("unknown field"));
    }
}
