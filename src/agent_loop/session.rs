//! セッション履歴の解決・復元・永続化を担うモジュール。
//!
//! SQLite 上の chat/session snapshot と LLM 用の `Message` 表現を相互変換し、
//! 1 ターンごとの楽観的同時実行制御つき保存を提供する。

use std::sync::Arc;

use crate::agent_loop::SurfaceContext;
use crate::assets::AssetStore;
use crate::error::{EgoPulseError, StorageError};
use crate::llm::{Message, MessageContent, MessageContentPart};
use crate::runtime::AppState;
use crate::storage::{SenderKind, SessionSnapshot, SessionSummary, StoredMessage, call_blocking};

#[derive(Debug, Clone)]
/// Holds the messages loaded for a turn together with the snapshot version.
pub(crate) struct LoadedSession {
    pub(crate) messages: Arc<Vec<Message>>,
    pub(crate) session_updated_at: Option<String>,
}

#[derive(Debug, Clone)]
/// Represents the updated snapshot returned after persisting one phase.
pub(crate) struct PersistedTurn {
    pub(crate) updated_at: String,
    pub(crate) messages: Vec<Message>,
}

/// Resolves or creates the internal chat ID for a conversation surface.
pub(crate) async fn resolve_chat_id(
    state: &AppState,
    context: &SurfaceContext,
) -> Result<i64, EgoPulseError> {
    call_blocking(Arc::clone(state.db_for(context.is_secret)), {
        let channel = context.channel.clone();
        let session_key = context.session_key();
        let surface_thread = context.surface_thread.clone();
        let chat_type = context.chat_type.clone();
        let agent_id = context.agent_id.clone();
        move |db| {
            db.resolve_or_create_chat_id(
                &channel,
                &session_key,
                Some(&surface_thread),
                &chat_type,
                &agent_id,
            )
        }
    })
    .await
    .map_err(EgoPulseError::from)
}

/// Lists all persisted sessions available in the local database.
pub(crate) async fn list_sessions(state: &AppState) -> Result<Vec<SessionSummary>, EgoPulseError> {
    call_blocking(Arc::clone(&state.db), move |db| db.list_sessions())
        .await
        .map_err(EgoPulseError::from)
}

/// Loads a session history and converts it into plain LLM messages.
pub(crate) async fn load_session_messages(
    state: &AppState,
    context: &SurfaceContext,
) -> Result<Vec<Message>, EgoPulseError> {
    let chat_id = resolve_chat_id(state, context).await?;
    let history = call_blocking(Arc::clone(state.db_for(context.is_secret)), move |db| {
        db.get_all_messages(chat_id)
    })
    .await?;
    Ok(history
        .into_iter()
        .map(|message| {
            let role = match message.sender_kind {
                SenderKind::Assistant | SenderKind::Tool => "assistant",
                SenderKind::User => "user",
                SenderKind::System => "system",
            };
            Message::text(role, message.content)
        })
        .collect())
}

/// Loads the trimmed session snapshot used as input for the next agent turn.
pub(crate) async fn load_messages_for_turn(
    state: &AppState,
    is_secret: bool,
    chat_id: i64,
) -> Result<LoadedSession, EgoPulseError> {
    let max_history_messages = state.config.max_history_messages;
    let snapshot = call_blocking(Arc::clone(state.db_for(is_secret)), move |db| {
        db.load_session_snapshot(chat_id, max_history_messages)
    })
    .await?;

    snapshot_to_loaded(snapshot, Arc::clone(&state.assets)).await
}

pub(crate) async fn persist_phase_once(
    state: &AppState,
    is_secret: bool,
    message: StoredMessage,
    messages: &[Message],
    session_updated_at: Option<String>,
) -> Result<PersistedTurn, EgoPulseError> {
    store_phase_snapshot(
        state,
        is_secret,
        message,
        messages.to_vec(),
        session_updated_at,
    )
    .await
    .map_err(EgoPulseError::Storage)
}

/// Persists one turn phase with optimistic concurrency and a single conflict retry.
pub(crate) async fn persist_phase(
    state: &AppState,
    is_secret: bool,
    message: StoredMessage,
    phase_message: Message,
    messages: &[Message],
    session_updated_at: Option<String>,
) -> Result<PersistedTurn, EgoPulseError> {
    persist_phase_messages(
        state,
        is_secret,
        message,
        vec![phase_message],
        messages,
        session_updated_at,
    )
    .await
}

pub(crate) async fn persist_phase_messages(
    state: &AppState,
    is_secret: bool,
    message: StoredMessage,
    phase_messages: Vec<Message>,
    messages: &[Message],
    session_updated_at: Option<String>,
) -> Result<PersistedTurn, EgoPulseError> {
    let persisted = store_phase_snapshot(
        state,
        is_secret,
        message.clone(),
        messages.to_vec(),
        session_updated_at.clone(),
    )
    .await;
    if let Some(turn) = persisted_turn_or_retry(persisted)? {
        return Ok(turn);
    }

    // 同じ session に別ターンが先に保存された場合は、最新 snapshot を読み直して
    // 今回の phase 群だけを末尾に積み直し、競合解消後の 1 回だけ再試行する。
    let LoadedSession {
        messages: refreshed_messages,
        session_updated_at: refreshed_updated_at,
    } = load_messages_for_turn(state, is_secret, message.chat_id).await?;
    let mut all_messages = Arc::try_unwrap(refreshed_messages).unwrap_or_else(|arc| (*arc).clone());
    all_messages.extend(phase_messages);

    store_phase_snapshot(
        state,
        is_secret,
        message,
        all_messages,
        refreshed_updated_at,
    )
    .await
    .map_err(EgoPulseError::Storage)
}

fn persisted_turn_or_retry(
    persisted: Result<PersistedTurn, StorageError>,
) -> Result<Option<PersistedTurn>, EgoPulseError> {
    match persisted {
        Ok(turn) => Ok(Some(turn)),
        Err(StorageError::SessionSnapshotConflict) => Ok(None),
        Err(error) => Err(EgoPulseError::Storage(error)),
    }
}

async fn snapshot_to_loaded(
    snapshot: SessionSnapshot,
    assets: Arc<AssetStore>,
) -> Result<LoadedSession, EgoPulseError> {
    let Some(json) = snapshot.messages_json.as_ref() else {
        return Ok(loaded_from_recent(&snapshot));
    };

    let restored = tokio::task::spawn_blocking({
        let assets = Arc::clone(&assets);
        let json = json.clone();
        move || restore_snapshot_messages(&assets, &json)
    })
    .await
    .map_err(|error| EgoPulseError::Storage(StorageError::TaskJoin(error.to_string())))?;

    let Some(messages) = restored_messages_or_recent(restored) else {
        return Ok(loaded_from_recent(&snapshot));
    };

    Ok(LoadedSession {
        messages: Arc::new(repair_orphan_tool_outputs(messages)),
        session_updated_at: snapshot.updated_at,
    })
}

fn restored_messages_or_recent(
    restored: Result<Vec<Message>, StorageError>,
) -> Option<Vec<Message>> {
    let Ok(messages) = restored else {
        return None;
    };
    if messages.is_empty() {
        return Some(messages);
    }

    Some(messages)
}

fn repair_orphan_tool_outputs(messages: Vec<Message>) -> Vec<Message> {
    let mut repaired = Vec::with_capacity(messages.len());
    let mut iter = messages.into_iter().peekable();

    while let Some(message) = iter.next() {
        if message.role == "assistant" && !message.tool_calls.is_empty() {
            let expected_ids = message
                .tool_calls
                .iter()
                .map(|tool_call| tool_call.id.clone())
                .collect::<Vec<_>>();
            repaired.push(message);

            let mut seen_ids = std::collections::HashSet::new();
            while iter
                .peek()
                .is_some_and(|candidate| candidate.role == "tool")
            {
                let tool_message = iter.next().expect("peeked tool message exists");
                if let Some(id) = &tool_message.tool_call_id {
                    seen_ids.insert(id.clone());
                }
                repaired.push(tool_message);
            }

            for missing_id in expected_ids.into_iter().filter(|id| !seen_ids.contains(id)) {
                repaired.push(orphan_tool_output_message(missing_id));
            }
        } else {
            repaired.push(message);
        }
    }

    repaired
}

fn orphan_tool_output_message(tool_call_id: String) -> Message {
    Message {
        role: "tool".to_string(),
        content: MessageContent::text(
            r#"{"status":"error","error":"tool output was missing from the restored session snapshot"}"#,
        ),
        reasoning_content: None,
        tool_calls: Vec::new(),
        tool_call_id: Some(tool_call_id),
    }
}

fn loaded_from_recent(snapshot: &SessionSnapshot) -> LoadedSession {
    LoadedSession {
        messages: Arc::new(
            snapshot
                .recent_messages
                .iter()
                .map(|message| {
                    let role = match message.sender_kind {
                        SenderKind::Assistant | SenderKind::Tool => "assistant",
                        SenderKind::User => "user",
                        SenderKind::System => "system",
                    };
                    Message::text(role, message.content.clone())
                })
                .collect(),
        ),
        session_updated_at: snapshot.updated_at.clone(),
    }
}

async fn serialize_snapshot(
    assets: Arc<AssetStore>,
    messages: Vec<Message>,
) -> Result<String, EgoPulseError> {
    tokio::task::spawn_blocking(move || {
        let persisted = persist_messages(&assets, &messages)?;
        serde_json::to_string(&persisted).map_err(StorageError::SessionSerialize)
    })
    .await
    .map_err(|error| EgoPulseError::Storage(StorageError::TaskJoin(error.to_string())))?
    .map_err(EgoPulseError::Storage)
}

/// Convert `InputImage` parts to `InputImageRef` for disk serialization.
fn persist_messages(
    assets: &AssetStore,
    messages: &[Message],
) -> Result<Vec<Message>, StorageError> {
    messages
        .iter()
        .map(|message| {
            Ok(Message {
                role: message.role.clone(),
                content: persist_content(assets, &message.content)?,
                reasoning_content: message.reasoning_content.clone(),
                tool_calls: message.tool_calls.clone(),
                tool_call_id: message.tool_call_id.clone(),
            })
        })
        .collect()
}

fn persist_content(
    assets: &AssetStore,
    content: &MessageContent,
) -> Result<MessageContent, StorageError> {
    match content {
        MessageContent::Text(text) => Ok(MessageContent::Text(text.clone())),
        MessageContent::Parts(parts) => Ok(MessageContent::Parts(
            parts
                .iter()
                .map(|part| persist_part(assets, part))
                .collect::<Result<Vec<_>, _>>()?,
        )),
    }
}

fn persist_part(
    assets: &AssetStore,
    part: &MessageContentPart,
) -> Result<MessageContentPart, StorageError> {
    match part {
        MessageContentPart::InputText { text } => {
            Ok(MessageContentPart::InputText { text: text.clone() })
        }
        MessageContentPart::InputImage { image_url, detail } => {
            let stored = assets.persist_image_data_url(image_url)?;
            Ok(MessageContentPart::InputImageRef {
                image_ref: stored.image_ref,
                mime_type: stored.mime_type,
                detail: detail.clone(),
            })
        }
        MessageContentPart::InputImageRef { .. } => Ok(part.clone()),
    }
}

/// Deserialize snapshot JSON as `Vec<Message>` and hydrate `InputImageRef` → `InputImage`.
///
/// For text-only sessions (the common case), the JSON string is scanned for
/// `"input_image_ref"` before any per-message work. When absent the hydration
/// pass is skipped entirely, eliminating the second iteration.
fn restore_snapshot_messages(
    assets: &AssetStore,
    json: &str,
) -> Result<Vec<Message>, StorageError> {
    let messages: Vec<Message> =
        serde_json::from_str(json).map_err(StorageError::SessionSerialize)?;

    // Fast path: no image references in the serialized form → nothing to hydrate.
    if !json.contains("\"input_image_ref\"") {
        return Ok(messages);
    }

    // Selective hydration: only transform messages that actually contain refs.
    Ok(messages
        .into_iter()
        .map(|message| {
            if message_contains_image_ref(&message) {
                hydrate_message(assets, message)
            } else {
                message
            }
        })
        .collect())
}

fn message_contains_image_ref(message: &Message) -> bool {
    match &message.content {
        MessageContent::Text(_) => false,
        MessageContent::Parts(parts) => parts
            .iter()
            .any(|part| matches!(part, MessageContentPart::InputImageRef { .. })),
    }
}

fn hydrate_message(assets: &AssetStore, message: Message) -> Message {
    Message {
        content: hydrate_content(assets, message.content),
        ..message
    }
}

fn hydrate_content(assets: &AssetStore, content: MessageContent) -> MessageContent {
    match content {
        MessageContent::Text(text) => MessageContent::Text(text),
        MessageContent::Parts(parts) => MessageContent::Parts(
            parts
                .into_iter()
                .map(|part| hydrate_part(assets, part))
                .collect(),
        ),
    }
}

fn hydrate_part(assets: &AssetStore, part: MessageContentPart) -> MessageContentPart {
    match part {
        MessageContentPart::InputText { text } => MessageContentPart::InputText { text },
        MessageContentPart::InputImage { .. } => part,
        MessageContentPart::InputImageRef {
            image_ref,
            mime_type,
            detail,
        } => assets
            .load_image_data_url(&image_ref, &mime_type)
            .map(|image_url| MessageContentPart::InputImage { image_url, detail })
            .unwrap_or_else(|error| missing_image_text_part(&image_ref, error)),
    }
}

fn missing_image_text_part(image_ref: &str, error: StorageError) -> MessageContentPart {
    let reason = match error {
        StorageError::NotFound(_) => format!("missing image_ref {image_ref}"),
        other => other.to_string(),
    };
    MessageContentPart::InputText {
        text: format!("Previously attached image could not be restored: {reason}"),
    }
}

async fn store_phase_snapshot(
    state: &AppState,
    is_secret: bool,
    message: StoredMessage,
    snapshot_messages: Vec<Message>,
    session_updated_at: Option<String>,
) -> Result<PersistedTurn, StorageError> {
    let session_json = serialize_snapshot(Arc::clone(&state.assets), snapshot_messages.clone())
        .await
        .map_err(|error| match error {
            EgoPulseError::Storage(storage) => storage,
            other => StorageError::TaskJoin(other.to_string()),
        })?;
    let updated_at = call_blocking(Arc::clone(state.db_for(is_secret)), move |db| {
        db.store_message_with_session(&message, &session_json, session_updated_at.as_deref())
    })
    .await?;
    Ok(PersistedTurn {
        updated_at,
        messages: snapshot_messages,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use async_trait::async_trait;

    use super::{load_messages_for_turn, persist_phase};
    use crate::agent_loop::SurfaceContext;
    use crate::assets::AssetStore;
    use crate::config::Config;
    use crate::error::LlmError;
    use crate::llm::{
        LlmProvider, Message, MessageContent, MessageContentPart, MessagesResponse, ToolCall,
    };
    use crate::runtime::AppState;
    use crate::storage::{MessageKind, SenderKind, StoredMessage, call_blocking};

    struct FakeProvider {
        response: String,
    }

    #[async_trait]
    impl LlmProvider for FakeProvider {
        fn provider_name(&self) -> &str {
            "test"
        }

        fn model_name(&self) -> &str {
            "test-model"
        }

        async fn send_message(
            &self,
            _system: &str,
            messages: Arc<Vec<Message>>,
            _tools: Option<std::sync::Arc<Vec<crate::llm::ToolDefinition>>>,
        ) -> Result<MessagesResponse, LlmError> {
            let prompt = messages
                .iter()
                .map(|message| format!("{}:{}", message.role, message.content.as_text_lossy()))
                .collect::<Vec<_>>()
                .join("|");
            Ok(MessagesResponse {
                content: format!("{} [{prompt}]", self.response),
                reasoning_content: None,
                tool_calls: Vec::new(),
                usage: None,
            })
        }
    }

    fn test_config(state_root: String) -> Config {
        crate::test_util::test_config(&state_root)
    }

    fn cli_context(session: &str) -> SurfaceContext {
        crate::test_util::cli_context(session)
    }

    fn build_state_with_provider(state_root: String, llm: Box<dyn LlmProvider>) -> AppState {
        crate::test_util::build_state_with_provider(&state_root, llm)
    }

    #[tokio::test]
    async fn persist_phase_returns_refreshed_snapshot_after_conflict() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = build_state_with_provider(
            dir.path().to_str().expect("utf8").to_string(),
            Box::new(FakeProvider {
                response: "ok".to_string(),
            }),
        );
        let context = cli_context("conflict");

        let chat_id = call_blocking(Arc::clone(&state.db), {
            let channel = context.channel.clone();
            let session_key = context.session_key();
            let surface_thread = context.surface_thread.clone();
            let chat_type = context.chat_type.clone();
            let agent_id = context.agent_id.clone();
            move |db| {
                db.resolve_or_create_chat_id(
                    &channel,
                    &session_key,
                    Some(&surface_thread),
                    &chat_type,
                    &agent_id,
                )
            }
        })
        .await
        .expect("chat id");

        let seed_message = StoredMessage {
            id: "seed-user".to_string(),
            chat_id,
            sender_id: context.surface_user.clone(),
            content: "hello".to_string(),
            sender_kind: SenderKind::User,
            timestamp: "2024-01-01T00:00:00Z".to_string(),
            message_kind: MessageKind::Message,
            recipient_agent_id: None,
        };
        call_blocking(Arc::clone(&state.db), {
            let message = seed_message.clone();
            move |db| {
                db.store_message_with_session(
                    &message,
                    r#"[{"role":"user","content":"hello"}]"#,
                    None,
                )
                .map(|_| ())
            }
        })
        .await
        .expect("seed session");

        let stale_session_updated_at = call_blocking(Arc::clone(&state.db), move |db| {
            db.load_session_snapshot(chat_id, 1)
                .map(|snapshot| snapshot.updated_at.expect("session updated_at"))
        })
        .await
        .expect("stale updated_at");

        let concurrent_message = StoredMessage {
            id: "seed-assistant".to_string(),
            chat_id,
            sender_id: "egopulse".to_string(),
            content: "hi".to_string(),
            sender_kind: SenderKind::Assistant,
            timestamp: "2024-01-01T00:00:01Z".to_string(),
            message_kind: MessageKind::Message,
            recipient_agent_id: None,
        };
        call_blocking(Arc::clone(&state.db), {
            let message = concurrent_message.clone();
            let expected_updated_at = stale_session_updated_at.clone();
            move |db| {
                db.store_message_with_session(
                    &message,
                    r#"[{"role":"user","content":"hello"},{"role":"assistant","content":"hi"}]"#,
                    Some(&expected_updated_at),
                )
                .map(|_| ())
            }
        })
        .await
        .expect("advance session");

        let persisted = persist_phase(
            &state,
            false,
            StoredMessage {
                id: "new-user".to_string(),
                chat_id,
                sender_id: context.surface_user.clone(),
                content: "next".to_string(),
                sender_kind: SenderKind::User,
                timestamp: "2024-01-01T00:00:02Z".to_string(),
                message_kind: MessageKind::Message,
                recipient_agent_id: None,
            },
            Message::text("user", "next"),
            &[Message::text("user", "hello")],
            Some(stale_session_updated_at),
        )
        .await
        .expect("persist turn");

        assert_eq!(persisted.messages.len(), 3);
        assert_eq!(persisted.messages[1].content.as_text_lossy(), "hi");
        assert_eq!(persisted.messages[2].content.as_text_lossy(), "next");
    }

    #[tokio::test]
    async fn persists_image_refs_and_rehydrates_them_for_next_turn() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = build_state_with_provider(
            dir.path().to_str().expect("utf8").to_string(),
            Box::new(FakeProvider {
                response: "ok".to_string(),
            }),
        );
        let context = cli_context("images");
        let chat_id = super::resolve_chat_id(&state, &context)
            .await
            .expect("chat id");
        let data_url = "data:image/png;base64,AAAA";

        let messages = vec![Message {
            role: "tool".to_string(),
            content: MessageContent::parts(vec![
                MessageContentPart::InputText {
                    text: "Read image file [image/png]".to_string(),
                },
                MessageContentPart::InputImage {
                    image_url: data_url.to_string(),
                    detail: Some("auto".to_string()),
                },
            ]),
            reasoning_content: None,
            tool_calls: Vec::new(),
            tool_call_id: Some("call_1".to_string()),
        }];

        persist_phase(
            &state,
            false,
            StoredMessage {
                id: "tool-msg".to_string(),
                chat_id,
                sender_id: "egopulse".to_string(),
                content: "Read image file [image/png]".to_string(),
                sender_kind: SenderKind::Assistant,
                timestamp: "2024-01-01T00:00:00Z".to_string(),
                message_kind: MessageKind::Message,
                recipient_agent_id: None,
            },
            messages[0].clone(),
            &messages,
            None,
        )
        .await
        .expect("persist image turn");

        let (session_json, _) = call_blocking(Arc::clone(&state.db), move |db| {
            db.load_session_snapshot(chat_id, 10).map(|snapshot| {
                (
                    snapshot.messages_json.expect("session json"),
                    snapshot.updated_at.expect("session updated_at"),
                )
            })
        })
        .await
        .expect("load session row");
        assert!(!session_json.contains("data:image/png;base64"));
        assert!(session_json.contains("\"type\":\"input_image_ref\""));

        let loaded = load_messages_for_turn(&state, false, chat_id)
            .await
            .expect("load messages");
        match &loaded.messages[0].content {
            MessageContent::Parts(parts) => {
                assert!(matches!(
                    parts[1],
                    MessageContentPart::InputImage { ref image_url, .. } if image_url == data_url
                ));
            }
            other => panic!("expected multimodal content, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn missing_image_refs_turn_into_explicit_text_notes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = build_state_with_provider(
            dir.path().to_str().expect("utf8").to_string(),
            Box::new(FakeProvider {
                response: "ok".to_string(),
            }),
        );
        let context = cli_context("missing-image");
        let chat_id = super::resolve_chat_id(&state, &context)
            .await
            .expect("chat id");

        call_blocking(Arc::clone(&state.db), move |db| {
            db.save_session(
                chat_id,
                r#"[{"role":"tool","content":[{"type":"input_text","text":"Read image file [image/png]"},{"type":"input_image_ref","image_ref":"missing-ref","mime_type":"image/png","detail":"auto"}],"tool_call_id":"call_1"}]"#,
            )
        })
        .await
        .expect("save snapshot");

        let loaded = load_messages_for_turn(&state, false, chat_id)
            .await
            .expect("load messages");
        match &loaded.messages[0].content {
            MessageContent::Parts(parts) => {
                assert!(matches!(
                    parts[1],
                    MessageContentPart::InputText { ref text }
                    if text.contains("missing image_ref missing-ref")
                ));
            }
            other => panic!("expected restored parts, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn load_messages_for_turn_repairs_orphan_tool_outputs() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = build_state_with_provider(
            dir.path().to_str().expect("utf8").to_string(),
            Box::new(FakeProvider {
                response: "ok".to_string(),
            }),
        );
        let context = cli_context("orphan-tool-output");
        let chat_id = super::resolve_chat_id(&state, &context)
            .await
            .expect("chat id");
        let snapshot = vec![
            Message::text("user", "please inspect"),
            Message {
                role: "assistant".to_string(),
                content: MessageContent::text("I will inspect."),
                reasoning_content: None,
                tool_calls: vec![ToolCall {
                    id: "call-missing".to_string(),
                    name: "read".to_string(),
                    arguments: serde_json::json!({"path": "Cargo.toml"}),
                }],
                tool_call_id: None,
            },
            Message::text("user", "what happened?"),
        ];
        let snapshot_json = serde_json::to_string(&snapshot).expect("snapshot json");

        call_blocking(Arc::clone(&state.db), move |db| {
            db.save_session(chat_id, &snapshot_json)
        })
        .await
        .expect("save snapshot");

        let loaded = load_messages_for_turn(&state, false, chat_id)
            .await
            .expect("load messages");

        assert_eq!(loaded.messages.len(), 4);
        assert_eq!(loaded.messages[2].role, "tool");
        assert_eq!(
            loaded.messages[2].tool_call_id.as_deref(),
            Some("call-missing")
        );
        assert!(
            loaded.messages[2]
                .content
                .as_text_lossy()
                .contains("tool output was missing")
        );
        assert_eq!(loaded.messages[3].content.as_text_lossy(), "what happened?");
    }

    #[tokio::test]
    async fn load_messages_for_turn_restores_full_snapshot_without_fixed_trim() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = build_state_with_provider(
            dir.path().to_str().expect("utf8").to_string(),
            Box::new(FakeProvider {
                response: "ok".to_string(),
            }),
        );
        let context = cli_context("full-snapshot");
        let chat_id = super::resolve_chat_id(&state, &context)
            .await
            .expect("chat id");
        let snapshot = (0..55)
            .map(|index| {
                Message::text(
                    if index % 2 == 0 { "user" } else { "assistant" },
                    format!("message-{index}"),
                )
            })
            .collect::<Vec<_>>();
        let snapshot_json = serde_json::to_string(&snapshot).expect("snapshot json");

        call_blocking(Arc::clone(&state.db), move |db| {
            db.save_session(chat_id, &snapshot_json)
        })
        .await
        .expect("save snapshot");

        let loaded = load_messages_for_turn(&state, false, chat_id)
            .await
            .expect("load messages");
        assert_eq!(loaded.messages.len(), 55);
        assert_eq!(loaded.messages[0].content.as_text_lossy(), "message-0");
        assert_eq!(loaded.messages[54].content.as_text_lossy(), "message-54");
    }

    #[tokio::test]
    async fn persist_phase_conflict_retry_keeps_full_refreshed_snapshot() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = build_state_with_provider(
            dir.path().to_str().expect("utf8").to_string(),
            Box::new(FakeProvider {
                response: "ok".to_string(),
            }),
        );
        let context = cli_context("conflict-full");
        let chat_id = super::resolve_chat_id(&state, &context)
            .await
            .expect("chat id");
        let seed_messages = (0..51)
            .map(|index| {
                Message::text(
                    if index % 2 == 0 { "user" } else { "assistant" },
                    format!("seed-{index}"),
                )
            })
            .collect::<Vec<_>>();
        let seed_json = serde_json::to_string(&seed_messages).expect("seed json");

        call_blocking(Arc::clone(&state.db), {
            let seed_json = seed_json.clone();
            move |db| db.save_session(chat_id, &seed_json)
        })
        .await
        .expect("save seed snapshot");

        let stale_session_updated_at = call_blocking(Arc::clone(&state.db), move |db| {
            db.load_session_snapshot(chat_id, 1)
                .map(|snapshot| snapshot.updated_at.expect("session updated_at"))
        })
        .await
        .expect("stale updated_at");

        let concurrent_message = StoredMessage {
            id: "concurrent-assistant".to_string(),
            chat_id,
            sender_id: "egopulse".to_string(),
            content: "concurrent".to_string(),
            sender_kind: SenderKind::Assistant,
            timestamp: "2024-01-01T00:00:52Z".to_string(),
            message_kind: MessageKind::Message,
            recipient_agent_id: None,
        };
        let mut latest_messages = seed_messages.clone();
        latest_messages.push(Message::text("assistant", "concurrent"));
        let latest_json = serde_json::to_string(&latest_messages).expect("latest json");

        call_blocking(Arc::clone(&state.db), {
            let message = concurrent_message.clone();
            let latest_json = latest_json.clone();
            let expected_updated_at = stale_session_updated_at.clone();
            move |db| {
                db.store_message_with_session(&message, &latest_json, Some(&expected_updated_at))
                    .map(|_| ())
            }
        })
        .await
        .expect("advance session");

        let mut stale_messages = seed_messages.clone();
        stale_messages.push(Message::text("user", "next"));
        let persisted = persist_phase(
            &state,
            false,
            StoredMessage {
                id: "new-user-full".to_string(),
                chat_id,
                sender_id: context.surface_user.clone(),
                content: "next".to_string(),
                sender_kind: SenderKind::User,
                timestamp: "2024-01-01T00:00:53Z".to_string(),
                message_kind: MessageKind::Message,
                recipient_agent_id: None,
            },
            Message::text("user", "next"),
            &stale_messages,
            Some(stale_session_updated_at),
        )
        .await
        .expect("persist turn");

        assert_eq!(persisted.messages.len(), 53);
        assert_eq!(persisted.messages[0].content.as_text_lossy(), "seed-0");
        assert_eq!(persisted.messages[51].content.as_text_lossy(), "concurrent");
        assert_eq!(persisted.messages[52].content.as_text_lossy(), "next");
    }

    #[test]
    fn surface_context_defaults_to_default_agent() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = test_config(dir.path().to_str().expect("utf8").to_string());

        let context = SurfaceContext {
            channel: "cli".to_string(),
            surface_user: "local_user".to_string(),
            surface_thread: "s1".to_string(),
            chat_type: "cli".to_string(),
            agent_id: config.default_agent.to_string(),
            channel_log_chat_id: None,
            chain_depth: 0,
            origin_id: String::new(),
            trace_id: String::new(),
            is_secret: false,
        };

        assert_eq!(context.agent_id, "default");
    }

    #[tokio::test]
    async fn same_discord_thread_different_agents_create_different_chats() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = build_state_with_provider(
            dir.path().to_str().expect("utf8").to_string(),
            Box::new(FakeProvider {
                response: "ok".to_string(),
            }),
        );

        let ctx_a = SurfaceContext {
            channel: "discord".to_string(),
            surface_user: "user".to_string(),
            surface_thread: "ch999:bot:bot1".to_string(),
            chat_type: "discord".to_string(),
            agent_id: "agent_a".to_string(),
            channel_log_chat_id: None,
            chain_depth: 0,
            origin_id: String::new(),
            trace_id: String::new(),
            is_secret: false,
        };
        let ctx_b = SurfaceContext {
            channel: "discord".to_string(),
            surface_user: "user".to_string(),
            surface_thread: "ch999:bot:bot1".to_string(),
            chat_type: "discord".to_string(),
            agent_id: "agent_b".to_string(),
            channel_log_chat_id: None,
            chain_depth: 0,
            origin_id: String::new(),
            trace_id: String::new(),
            is_secret: false,
        };

        let chat_a = super::resolve_chat_id(&state, &ctx_a)
            .await
            .expect("chat_a");
        let chat_b = super::resolve_chat_id(&state, &ctx_b)
            .await
            .expect("chat_b");

        assert_ne!(
            chat_a, chat_b,
            "different agents must produce different chat ids"
        );
    }

    #[tokio::test]
    async fn same_discord_thread_same_agent_reuses_chat() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = build_state_with_provider(
            dir.path().to_str().expect("utf8").to_string(),
            Box::new(FakeProvider {
                response: "ok".to_string(),
            }),
        );

        let ctx = SurfaceContext {
            channel: "discord".to_string(),
            surface_user: "user".to_string(),
            surface_thread: "ch555:bot:bot1".to_string(),
            chat_type: "discord".to_string(),
            agent_id: "agent_a".to_string(),
            channel_log_chat_id: None,
            chain_depth: 0,
            origin_id: String::new(),
            trace_id: String::new(),
            is_secret: false,
        };

        let chat_first = super::resolve_chat_id(&state, &ctx).await.expect("first");
        let chat_second = super::resolve_chat_id(&state, &ctx).await.expect("second");

        assert_eq!(chat_first, chat_second, "same agent must reuse chat id");
    }

    #[test]
    fn web_and_telegram_keep_existing_identity_with_default_agent() {
        let web_ctx = SurfaceContext {
            channel: "web".to_string(),
            surface_user: "user".to_string(),
            surface_thread: "s1".to_string(),
            chat_type: "web".to_string(),
            agent_id: "default".to_string(),
            channel_log_chat_id: None,
            chain_depth: 0,
            origin_id: String::new(),
            trace_id: String::new(),
            is_secret: false,
        };
        let telegram_ctx = SurfaceContext {
            channel: "telegram".to_string(),
            surface_user: "user".to_string(),
            surface_thread: "s2".to_string(),
            chat_type: "telegram".to_string(),
            agent_id: "default".to_string(),
            channel_log_chat_id: None,
            chain_depth: 0,
            origin_id: String::new(),
            trace_id: String::new(),
            is_secret: false,
        };

        assert_eq!(web_ctx.session_key(), "web:s1:agent:default");
        assert_eq!(telegram_ctx.session_key(), "telegram:s2:agent:default");
    }

    #[test]
    fn restored_messages_or_recent_empty_is_some() {
        let result = super::restored_messages_or_recent(Ok(vec![]));
        assert_eq!(result, Some(vec![]));

        let msg = Message {
            role: "user".into(),
            content: MessageContent::Text("hello".into()),
            reasoning_content: None,
            tool_calls: vec![],
            tool_call_id: None,
        };
        let result = super::restored_messages_or_recent(Ok(vec![msg.clone()]));
        assert_eq!(result, Some(vec![msg]));

        let err: Result<Vec<Message>, crate::error::StorageError> =
            Err(crate::error::StorageError::Io(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "test",
            )));
        let result = super::restored_messages_or_recent(err);
        assert_eq!(result, None);
    }

    #[tokio::test]
    async fn web_chat_id_reentry_preserves_existing_external_chat_id() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = build_state_with_provider(
            dir.path().to_str().expect("utf8").to_string(),
            Box::new(FakeProvider {
                response: "ok".to_string(),
            }),
        );

        let ctx = SurfaceContext {
            channel: "web".to_string(),
            surface_user: "user".to_string(),
            surface_thread: "chat:abc123".to_string(),
            chat_type: "web".to_string(),
            agent_id: "default".to_string(),
            channel_log_chat_id: None,
            chain_depth: 0,
            origin_id: String::new(),
            trace_id: String::new(),
            is_secret: false,
        };

        let first = super::resolve_chat_id(&state, &ctx).await.expect("first");
        let second = super::resolve_chat_id(&state, &ctx).await.expect("second");

        assert_eq!(first, second, "reentry must preserve existing chat id");
    }

    // -- Step 4: SenderKind role mapping tests ----------------------------------

    #[test]
    fn load_session_maps_assistant_to_assistant() {
        let message = StoredMessage::assistant(1, "lyre".to_string(), "hello".to_string());
        assert_eq!(message.sender_kind, SenderKind::Assistant);
        let role = match message.sender_kind {
            SenderKind::Assistant | SenderKind::Tool => "assistant",
            SenderKind::User => "user",
            SenderKind::System => "system",
        };
        assert_eq!(role, "assistant");
    }

    #[test]
    fn load_session_maps_user_to_user() {
        let message = StoredMessage::user(1, "user:cli:default".to_string(), "hi".to_string());
        assert_eq!(message.sender_kind, SenderKind::User);
        let role = match message.sender_kind {
            SenderKind::Assistant | SenderKind::Tool => "assistant",
            SenderKind::User => "user",
            SenderKind::System => "system",
        };
        assert_eq!(role, "user");
    }

    #[test]
    fn load_session_maps_system_to_system() {
        let message = StoredMessage::system(1, "boot complete".to_string());
        assert_eq!(message.sender_kind, SenderKind::System);
        let role = match message.sender_kind {
            SenderKind::Assistant | SenderKind::Tool => "assistant",
            SenderKind::User => "user",
            SenderKind::System => "system",
        };
        assert_eq!(role, "system");
    }

    #[test]
    fn load_session_maps_tool_to_assistant() {
        let message = StoredMessage::tool(
            1,
            "lyre".to_string(),
            "vega".to_string(),
            "hello".to_string(),
        );
        assert_eq!(message.sender_kind, SenderKind::Tool);
        let role = match message.sender_kind {
            SenderKind::Assistant | SenderKind::Tool => "assistant",
            SenderKind::User => "user",
            SenderKind::System => "system",
        };
        assert_eq!(role, "assistant");
    }

    #[tokio::test]
    async fn loaded_from_recent_preserves_sender_kind() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = build_state_with_provider(
            dir.path().to_str().expect("utf8").to_string(),
            Box::new(FakeProvider {
                response: "ok".to_string(),
            }),
        );
        let context = cli_context("sender-kind");
        let chat_id = super::resolve_chat_id(&state, &context)
            .await
            .expect("chat id");

        call_blocking(Arc::clone(&state.db), {
            move |db| {
                db.store_message_only(&StoredMessage {
                    id: "msg-user".to_string(),
                    chat_id,
                    sender_id: "user:cli:default".to_string(),
                    content: "hello".to_string(),
                    sender_kind: SenderKind::User,
                    timestamp: "2024-01-01T00:00:00Z".to_string(),
                    message_kind: MessageKind::Message,
                    recipient_agent_id: None,
                })
            }
        })
        .await
        .expect("store user message");

        call_blocking(Arc::clone(&state.db), {
            move |db| {
                db.store_message_only(&StoredMessage {
                    id: "msg-assistant".to_string(),
                    chat_id,
                    sender_id: "lyre".to_string(),
                    content: "response".to_string(),
                    sender_kind: SenderKind::Assistant,
                    timestamp: "2024-01-01T00:00:01Z".to_string(),
                    message_kind: MessageKind::Message,
                    recipient_agent_id: None,
                })
            }
        })
        .await
        .expect("store assistant message");

        let loaded = load_messages_for_turn(&state, false, chat_id)
            .await
            .expect("load messages");

        assert_eq!(loaded.messages.len(), 2);
        assert_eq!(loaded.messages[0].role, "user");
        assert_eq!(loaded.messages[0].content.as_text_lossy(), "hello");
        assert_eq!(loaded.messages[1].role, "assistant");
        assert_eq!(loaded.messages[1].content.as_text_lossy(), "response");
    }

    // -- Restore snapshot hydration optimization tests -------------------------

    #[test]
    fn text_only_snapshot_skips_hydration() {
        let dir = tempfile::tempdir().expect("tempdir");
        let assets = AssetStore::new(dir.path()).expect("store");
        let json = serde_json::to_string(&vec![
            Message::text("user", "hello"),
            Message::text("assistant", "world"),
        ])
        .expect("json");

        let messages = super::restore_snapshot_messages(&assets, &json).expect("restore");
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, "user");
        assert_eq!(messages[0].content.as_text_lossy(), "hello");
        assert_eq!(messages[1].role, "assistant");
        assert_eq!(messages[1].content.as_text_lossy(), "world");
    }

    #[test]
    fn image_snapshot_hydrates_refs() {
        let dir = tempfile::tempdir().expect("tempdir");
        let assets = AssetStore::new(dir.path()).expect("store");
        let data_url = "data:image/png;base64,AAAA";
        let stored = assets.persist_image_data_url(data_url).expect("persist");

        let snapshot = vec![Message {
            role: "tool".to_string(),
            content: MessageContent::parts(vec![
                MessageContentPart::InputText {
                    text: "screenshot".to_string(),
                },
                MessageContentPart::InputImageRef {
                    image_ref: stored.image_ref.clone(),
                    mime_type: stored.mime_type.clone(),
                    detail: Some("auto".to_string()),
                },
            ]),
            reasoning_content: None,
            tool_calls: Vec::new(),
            tool_call_id: Some("call_1".to_string()),
        }];
        let json = serde_json::to_string(&snapshot).expect("json");

        let messages = super::restore_snapshot_messages(&assets, &json).expect("restore");
        assert_eq!(messages.len(), 1);
        match &messages[0].content {
            MessageContent::Parts(parts) => {
                assert!(matches!(
                    &parts[1],
                    MessageContentPart::InputImage { image_url, detail }
                    if image_url == data_url && detail.as_deref() == Some("auto")
                ));
            }
            other => panic!("expected parts, got {other:?}"),
        }
    }

    #[test]
    fn identical_output_to_two_pass() {
        let dir = tempfile::tempdir().expect("tempdir");
        let assets = AssetStore::new(dir.path()).expect("store");
        let data_url = "data:image/png;base64,iVBORw==";
        let stored = assets.persist_image_data_url(data_url).expect("persist");

        let snapshot = vec![
            Message::text("user", "look at this"),
            Message {
                role: "tool".to_string(),
                content: MessageContent::parts(vec![
                    MessageContentPart::InputText {
                        text: "file read".to_string(),
                    },
                    MessageContentPart::InputImageRef {
                        image_ref: stored.image_ref.clone(),
                        mime_type: stored.mime_type.clone(),
                        detail: None,
                    },
                ]),
                reasoning_content: None,
                tool_calls: Vec::new(),
                tool_call_id: Some("call_img".to_string()),
            },
            Message::text("assistant", "I see the image"),
        ];
        let json = serde_json::to_string(&snapshot).expect("json");

        let single_pass = super::restore_snapshot_messages(&assets, &json).expect("single");

        // Simulate old two-pass: deserialize then hydrate every message unconditionally.
        let raw: Vec<Message> = serde_json::from_str(&json).expect("deserialize");
        let two_pass: Vec<Message> = raw
            .into_iter()
            .map(|message| {
                let content = match message.content {
                    MessageContent::Text(text) => MessageContent::Text(text),
                    MessageContent::Parts(parts) => MessageContent::Parts(
                        parts
                            .into_iter()
                            .map(|part| match part {
                                MessageContentPart::InputText { text } => {
                                    MessageContentPart::InputText { text }
                                }
                                MessageContentPart::InputImage { image_url, detail } => {
                                    MessageContentPart::InputImage { image_url, detail }
                                }
                                MessageContentPart::InputImageRef {
                                    image_ref,
                                    mime_type,
                                    detail,
                                } => assets
                                    .load_image_data_url(&image_ref, &mime_type)
                                    .map(|image_url| MessageContentPart::InputImage {
                                        image_url,
                                        detail,
                                    })
                                    .unwrap_or_else(|error| {
                                        MessageContentPart::InputText {
                                            text: format!(
                                                "Previously attached image could not be restored: {error}"
                                            ),
                                        }
                                    }),
                            })
                            .collect(),
                    ),
                };
                Message {
                    content,
                    ..message
                }
            })
            .collect();

        assert_eq!(single_pass, two_pass);
    }

    #[test]
    fn large_session_load_performance() {
        let dir = tempfile::tempdir().expect("tempdir");
        let assets = AssetStore::new(dir.path()).expect("store");
        let messages: Vec<Message> = (0..1000)
            .map(|index| {
                Message::text(
                    if index % 2 == 0 { "user" } else { "assistant" },
                    format!("message-{index}"),
                )
            })
            .collect();
        let json = serde_json::to_string(&messages).expect("json");

        let start = std::time::Instant::now();
        let restored = super::restore_snapshot_messages(&assets, &json).expect("restore");
        let _elapsed = start.elapsed();

        assert_eq!(restored.len(), 1000);
    }

    /// Verifies that `persist_phase` borrows `&[Message]` for serialization,
    /// so the caller retains ownership of the `Arc<Vec<Message>>`.
    #[tokio::test]
    async fn persist_phase_borrows_messages() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = build_state_with_provider(
            dir.path().to_str().expect("utf8").to_string(),
            Box::new(FakeProvider {
                response: "ok".to_string(),
            }),
        );
        let context = cli_context("borrow-test");
        let chat_id = super::resolve_chat_id(&state, &context)
            .await
            .expect("chat id");

        let messages: Arc<Vec<Message>> = Arc::new(vec![
            Message::text("user", "hello"),
            Message::text("assistant", "hi"),
        ]);

        let _persisted = persist_phase(
            &state,
            false,
            StoredMessage {
                id: "borrow-msg".to_string(),
                chat_id,
                sender_id: "user".to_string(),
                content: "test".to_string(),
                sender_kind: SenderKind::User,
                timestamp: "2024-01-01T00:00:00Z".to_string(),
                message_kind: MessageKind::Message,
                recipient_agent_id: None,
            },
            Message::text("user", "test"),
            &messages,
            None,
        )
        .await
        .expect("persist");

        assert_eq!(Arc::strong_count(&messages), 1);
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].content.as_text_lossy(), "hello");
    }
}
