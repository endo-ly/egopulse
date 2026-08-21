//! send_attachment ツール。
//!
//! エージェントが明示的にファイルをチャネルへ送信するためのツール。
//! 普段の会話はランタイムが自動送信するため、このツールはファイル配布が必要な場合に使用する。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;
use tokio::io::AsyncReadExt;

use crate::channels::adapter::{ChannelRegistry, PreparedAttachment};
use crate::conversation::ConversationScope;
use crate::llm::ToolDefinition;
use crate::storage::{Database, call_blocking};

use super::path_guard;
use super::search::resolve_workspace_path;
use super::{Tool, ToolExecutionContext, ToolResult, schema_object};

async fn prepare_attachment(
    workspace_dir: &Path,
    requested_path: &str,
) -> Result<PreparedAttachment, String> {
    let workspace_root = tokio::fs::canonicalize(workspace_dir)
        .await
        .map_err(|e| format!("Failed to resolve workspace: {e}"))?;
    let candidate = resolve_workspace_path(workspace_dir, requested_path)?;
    path_guard::check_path(&candidate.to_string_lossy())?;
    let resolved = match tokio::fs::canonicalize(&candidate).await {
        Ok(path) => path,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(format!("File not found: {requested_path}"));
        }
        Err(error) => return Err(format!("Failed to resolve attachment: {error}")),
    };

    if !resolved.starts_with(&workspace_root) {
        return Err(format!(
            "Attachment path must stay within workspace: {requested_path}"
        ));
    }
    path_guard::check_path(&resolved.to_string_lossy())?;

    let mut file = match tokio::fs::File::open(&resolved).await {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(format!("File not found: {requested_path}"));
        }
        Err(error) => return Err(format!("Failed to read attachment: {error}")),
    };
    if !file
        .metadata()
        .await
        .map_err(|e| format!("Failed to read attachment: {e}"))?
        .is_file()
    {
        return Err(format!("File not found: {requested_path}"));
    }

    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .await
        .map_err(|e| format!("Failed to read attachment: {e}"))?;

    Ok(PreparedAttachment::new(resolved, bytes))
}

/// Tool for sending file attachments with optional text to the conversation channel.
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
    /// Creates an attachment tool bound to the workspace, channel registry,
    /// and normal and secret-scope databases.
    ///
    /// Relative attachment paths are resolved from `workspace_dir`, files are
    /// delivered through `channels`, and `secret_db` is used for secret-scope
    /// conversations when available.
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

    fn db_for(&self, scope: ConversationScope) -> Arc<Database> {
        match scope {
            ConversationScope::Normal => Arc::clone(&self.db),
            ConversationScope::Secret => Arc::clone(
                self.secret_db
                    .as_ref()
                    .expect("secret db required for secret mode send_attachment"),
            ),
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
            description: "Send a file attachment to the current conversation, optionally with text. Use this when you need to deliver a file to the user. For normal text responses, just write your reply — do not use this tool.".to_string(),
            parameters: schema_object(
                json!({
                    "attachment_path": {
                        "type": "string",
                        "description": "Local file path to send as an attachment (required)"
                    },
                    "text": {
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
            text: Option<String>,
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
        let text = params.text.filter(|s| !s.trim().is_empty());

        let chat_id = context.chat_id;
        let chat_info = match call_blocking(self.db_for(context.scope), move |db| {
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

        let attachment = match prepare_attachment(&self.workspace_dir, &path_str).await {
            Ok(attachment) => attachment,
            Err(error) => return ToolResult::error(error),
        };

        match adapter
            .send_attachment(&chat_info.external_chat_id, text.as_deref(), &attachment)
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
    use async_trait::async_trait;
    use serde_json::json;
    use std::sync::Mutex;

    use crate::channels::adapter::{ChannelAdapter, ConversationKind};

    #[derive(Clone)]
    struct ReceivedAttachment {
        text: Option<String>,
        path: PathBuf,
    }

    struct RecordingAdapter {
        received: Arc<Mutex<Option<ReceivedAttachment>>>,
    }

    #[async_trait]
    impl ChannelAdapter for RecordingAdapter {
        fn name(&self) -> &str {
            "cli"
        }

        fn chat_type_routes(&self) -> Vec<(&str, ConversationKind)> {
            vec![("cli", ConversationKind::Private)]
        }

        async fn send_text(&self, _: &str, _: &str) -> Result<(), String> {
            Ok(())
        }

        async fn send_attachment(
            &self,
            _: &str,
            text: Option<&str>,
            attachment: &PreparedAttachment,
        ) -> Result<(), String> {
            *self.received.lock().expect("recording adapter lock") = Some(ReceivedAttachment {
                text: text.map(str::to_string),
                path: attachment.path().to_path_buf(),
            });
            Ok(())
        }
    }

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
    fn definition_requires_attachment_path_and_exposes_text() {
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
        assert!(definition.parameters["properties"].get("text").is_some());
        assert!(definition.parameters["properties"].get("caption").is_none());
    }

    #[tokio::test]
    async fn execute_requires_attachment_path_even_with_text() {
        let dir = tempfile::tempdir().expect("tempdir");
        let tool = test_tool(dir.path());
        let context = crate::test_util::test_tool_context();

        let missing_path = tool.execute(json!({}), &context).await;
        assert!(missing_path.is_error);
        assert_eq!(
            missing_path.content,
            "Missing required parameter: attachment_path"
        );

        let text_without_path = tool.execute(json!({"text": "hello"}), &context).await;
        assert!(text_without_path.is_error);
        assert_eq!(
            text_without_path.content,
            "Missing required parameter: attachment_path"
        );

        let caption_input = tool
            .execute(
                json!({"attachment_path": "file.txt", "caption": "hello"}),
                &context,
            )
            .await;
        assert!(caption_input.is_error);
        assert!(caption_input.content.contains("unknown field"));
    }

    #[tokio::test]
    async fn execute_sends_text_and_prepared_attachment() {
        let dir = tempfile::tempdir().expect("tempdir");
        let attachment_path = dir.path().join("report.txt");
        std::fs::write(&attachment_path, "report").expect("attachment");

        let received = Arc::new(Mutex::new(None));
        let adapter = Arc::new(RecordingAdapter {
            received: Arc::clone(&received),
        });
        let mut channels = ChannelRegistry::new();
        channels.register(adapter);

        let db = Arc::new(Database::new(&dir.path().join("egopulse.db")).expect("database"));
        let chat_id = db
            .resolve_or_create_chat_id("cli", "cli:attachment", None, "cli", "default")
            .expect("chat id");
        let tool = SendAttachmentTool::new(
            dir.path().to_path_buf(),
            Arc::new(channels),
            Arc::clone(&db),
            None,
        );
        let mut context = crate::test_util::test_tool_context();
        context.chat_id = chat_id;

        let result = tool
            .execute(
                json!({"attachment_path": "report.txt", "text": "see report"}),
                &context,
            )
            .await;

        assert!(!result.is_error, "{result:?}");
        let observed = received
            .lock()
            .expect("recording adapter lock")
            .clone()
            .expect("attachment delivery");
        assert_eq!(observed.text.as_deref(), Some("see report"));
        assert_eq!(
            observed.path,
            std::fs::canonicalize(attachment_path).expect("canonical path")
        );
    }

    #[tokio::test]
    async fn prepare_attachment_rejects_workspace_escape() {
        let dir = tempfile::tempdir().expect("tempdir");
        let outside_dir = tempfile::tempdir().expect("outside tempdir");
        let outside_path = outside_dir.path().join("outside.txt");
        std::fs::write(&outside_path, "outside").expect("outside attachment");

        let result =
            prepare_attachment(dir.path(), outside_path.to_str().expect("utf8 path")).await;

        let error = match result {
            Ok(_) => panic!("workspace escape must be rejected"),
            Err(error) => error,
        };
        assert!(error.contains("within workspace"));
    }
}
