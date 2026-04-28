//! send_message ツール。
//!
//! エージェントが明示的にテキストやファイル添付をチャネルに送信するためのツール。
//! 普段の会話はランタイムが自動送信するため、このツールはファイル送信が必要な場合に使用する。

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;

use crate::channel_adapter::ChannelRegistry;
use crate::error::StorageError;
use crate::llm::ToolDefinition;
use crate::storage::{Database, call_blocking};

use super::path_guard;
use super::search::resolve_workspace_path;
use super::{Tool, ToolExecutionContext, ToolResult, schema_object};

/// Tool for sending messages with optional file attachments to the conversation channel.
///
/// Normal text responses are auto-sent by the runtime; this tool exists for cases
/// that require explicit file attachment delivery.
pub(crate) struct SendMessageTool {
    workspace_dir: PathBuf,
    channels: Arc<ChannelRegistry>,
    db: Arc<Database>,
}

impl SendMessageTool {
    pub fn new(
        workspace_dir: PathBuf,
        channels: Arc<ChannelRegistry>,
        db: Arc<Database>,
    ) -> Self {
        Self {
            workspace_dir,
            channels,
            db,
        }
    }
}

#[async_trait]
impl Tool for SendMessageTool {
    fn name(&self) -> &str {
        "send_message"
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "send_message".to_string(),
            description: "Send a message with optional file attachment to the current conversation. Use this when you need to send a file to the user. For normal text responses, just write your reply — do not use this tool.".to_string(),
            parameters: schema_object(
                json!({
                    "text": {
                        "type": "string",
                        "description": "Message text to send (optional if attachment_path is provided)"
                    },
                    "attachment_path": {
                        "type": "string",
                        "description": "Local file path to send as attachment (optional)"
                    },
                    "caption": {
                        "type": "string",
                        "description": "Caption for the attached file (optional, used only with attachment_path)"
                    }
                }),
                &[],
            ),
        }
    }

    async fn execute(
        &self,
        input: serde_json::Value,
        context: &ToolExecutionContext,
    ) -> ToolResult {
        let text = input.get("text").and_then(|v| v.as_str());
        let attachment_path = input.get("attachment_path").and_then(|v| v.as_str());
        let caption = input.get("caption").and_then(|v| v.as_str());

        if text.is_none() && attachment_path.is_none() {
            return ToolResult::error(
                "At least one of 'text' or 'attachment_path' must be provided".to_string(),
            );
        }

        let chat_info = match lookup_chat_info(Arc::clone(&self.db), context.chat_id).await {
            Ok(Some(info)) => info,
            Ok(None) => {
                return ToolResult::error(format!(
                    "no chat found for chat_id {}",
                    context.chat_id
                ))
            }
            Err(e) => return ToolResult::error(format!("failed to resolve chat info: {e}")),
        };

        let adapter = match self.channels.get(&chat_info.channel) {
            Some(a) => a,
            None => {
                return ToolResult::error(format!(
                    "no adapter for channel '{}'",
                    chat_info.channel
                ))
            }
        };

        if let Some(path_str) = attachment_path {
            if let Err(reason) = path_guard::check_path(path_str) {
                return ToolResult::error(reason);
            }

            let resolved = match resolve_workspace_path(&self.workspace_dir, path_str) {
                Ok(p) => p,
                Err(e) => return ToolResult::error(e),
            };

            if !resolved.exists() {
                return ToolResult::error(format!("File not found: {path_str}"));
            }

            match adapter
                .send_attachment(&chat_info.external_chat_id, text, &resolved, caption)
                .await
            {
                Ok(()) => ToolResult::success("Message sent successfully".to_string()),
                Err(e) => ToolResult::error(format!("Failed to send message: {e}")),
            }
        } else if let Some(text_content) = text {
            match adapter
                .send_text(&chat_info.external_chat_id, text_content)
                .await
            {
                Ok(()) => ToolResult::success("Message sent successfully".to_string()),
                Err(e) => ToolResult::error(format!("Failed to send message: {e}")),
            }
        } else {
            ToolResult::error("No content to send".to_string())
        }
    }
}

/// Look up [`ChatInfo`] by `chat_id` via the blocking thread pool.
async fn lookup_chat_info(
    db: Arc<Database>,
    chat_id: i64,
) -> Result<Option<crate::storage::ChatInfo>, StorageError> {
    call_blocking(db, move |db| db.get_chat_by_id(chat_id)).await
}
