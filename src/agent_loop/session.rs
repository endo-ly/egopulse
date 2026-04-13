//! セッション履歴の解決・復元・永続化を担うモジュール。
//!
//! SQLite 上の chat/session snapshot と LLM 用の `Message` 表現を相互変換し、
//! 1 ターンごとの楽観的同時実行制御つき保存を提供する。

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::agent_loop::SurfaceContext;
use crate::assets::AssetStore;
use crate::error::{EgoPulseError, StorageError};
use crate::llm::{Message, MessageContent, MessageContentPart};
use crate::runtime::AppState;
use crate::storage::{SessionSnapshot, SessionSummary, StoredMessage, call_blocking};

#[derive(Debug, Clone)]
/// Holds the messages loaded for a turn together with the snapshot version.
pub(crate) struct LoadedSession {
    pub(crate) messages: Vec<Message>,
    pub(crate) session_updated_at: Option<String>,
}

#[derive(Debug, Clone)]
/// Represents the updated snapshot returned after persisting one phase.
pub(crate) struct PersistedTurn {
    pub(crate) updated_at: String,
    pub(crate) messages: Vec<Message>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct PersistedMessage {
    role: String,
    content: PersistedMessageContent,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    tool_calls: Vec<crate::llm::ToolCall>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
enum PersistedMessageContent {
    Text(String),
    Parts(Vec<PersistedMessageContentPart>),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type")]
enum PersistedMessageContentPart {
    #[serde(rename = "input_text")]
    InputText { text: String },
    #[serde(rename = "input_image_ref")]
    InputImageRef {
        image_ref: String,
        mime_type: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
}

/// Resolves or creates the internal chat ID for a conversation surface.
pub(crate) async fn resolve_chat_id(
    state: &AppState,
    context: &SurfaceContext,
) -> Result<i64, EgoPulseError> {
    call_blocking(Arc::clone(&state.db), {
        let channel = context.channel.clone();
        let session_key = context.session_key();
        let surface_thread = context.surface_thread.clone();
        let chat_type = context.chat_type.clone();
        move |db| {
            db.resolve_or_create_chat_id(&channel, &session_key, Some(&surface_thread), &chat_type)
        }
    })
    .await
    .map_err(EgoPulseError::from)
}

/// Lists all persisted sessions available in the local database.
pub async fn list_sessions(state: &AppState) -> Result<Vec<SessionSummary>, EgoPulseError> {
    call_blocking(Arc::clone(&state.db), move |db| db.list_sessions())
        .await
        .map_err(EgoPulseError::from)
}

/// Loads a session history and converts it into plain LLM messages.
pub async fn load_session_messages(
    state: &AppState,
    context: &SurfaceContext,
) -> Result<Vec<Message>, EgoPulseError> {
    let chat_id = resolve_chat_id(state, context).await?;
    let history = call_blocking(Arc::clone(&state.db), move |db| {
        db.get_all_messages(chat_id)
    })
    .await?;
    Ok(history
        .into_iter()
        .map(|message| {
            Message::text(
                if message.is_from_bot {
                    "assistant"
                } else {
                    "user"
                },
                message.content,
            )
        })
        .collect())
}

/// Loads the trimmed session snapshot used as input for the next agent turn.
pub(crate) async fn load_messages_for_turn(
    state: &AppState,
    chat_id: i64,
) -> Result<LoadedSession, EgoPulseError> {
    let max_history_messages = state.config.max_history_messages;
    let snapshot = call_blocking(Arc::clone(&state.db), move |db| {
        db.load_session_snapshot(chat_id, max_history_messages)
    })
    .await?;

    snapshot_to_loaded(snapshot, Arc::clone(&state.assets)).await
}

pub(crate) async fn persist_phase_once(
    state: &AppState,
    message: StoredMessage,
    messages: &[Message],
    session_updated_at: Option<String>,
) -> Result<PersistedTurn, EgoPulseError> {
    store_phase_snapshot(state, message, messages.to_vec(), session_updated_at)
        .await
        .map_err(EgoPulseError::Storage)
}

/// Persists one turn phase with optimistic concurrency and a single conflict retry.
pub(crate) async fn persist_phase(
    state: &AppState,
    message: StoredMessage,
    phase_message: Message,
    messages: &[Message],
    session_updated_at: Option<String>,
) -> Result<PersistedTurn, EgoPulseError> {
    let persisted = store_phase_snapshot(
        state,
        message.clone(),
        messages.to_vec(),
        session_updated_at.clone(),
    )
    .await;
    if let Some(turn) = persisted_turn_or_retry(persisted)? {
        return Ok(turn);
    }

    // 同じ session に別ターンが先に保存された場合は、最新 snapshot を読み直して
    // 今回の phase だけを末尾に積み直し、競合解消後の 1 回だけ再試行する。
    let LoadedSession {
        messages: mut refreshed_messages,
        session_updated_at: refreshed_updated_at,
    } = load_messages_for_turn(state, message.chat_id).await?;
    refreshed_messages.push(phase_message);

    store_phase_snapshot(state, message, refreshed_messages, refreshed_updated_at)
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
        messages,
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
        return None;
    }

    Some(messages)
}

fn loaded_from_recent(snapshot: &SessionSnapshot) -> LoadedSession {
    LoadedSession {
        messages: snapshot
            .recent_messages
            .iter()
            .map(|message| {
                Message::text(
                    if message.is_from_bot {
                        "assistant"
                    } else {
                        "user"
                    },
                    message.content.clone(),
                )
            })
            .collect(),
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

fn persist_messages(
    assets: &AssetStore,
    messages: &[Message],
) -> Result<Vec<PersistedMessage>, StorageError> {
    messages
        .iter()
        .map(|message| {
            Ok(PersistedMessage {
                role: message.role.clone(),
                content: persist_content(assets, &message.content)?,
                tool_calls: message.tool_calls.clone(),
                tool_call_id: message.tool_call_id.clone(),
            })
        })
        .collect()
}

fn persist_content(
    assets: &AssetStore,
    content: &MessageContent,
) -> Result<PersistedMessageContent, StorageError> {
    match content {
        MessageContent::Text(text) => Ok(PersistedMessageContent::Text(text.clone())),
        MessageContent::Parts(parts) => Ok(PersistedMessageContent::Parts(
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
) -> Result<PersistedMessageContentPart, StorageError> {
    match part {
        MessageContentPart::InputText { text } => {
            Ok(PersistedMessageContentPart::InputText { text: text.clone() })
        }
        MessageContentPart::InputImage { image_url, detail } => {
            let stored = assets.persist_image_data_url(image_url)?;
            Ok(PersistedMessageContentPart::InputImageRef {
                image_ref: stored.image_ref,
                mime_type: stored.mime_type,
                detail: detail.clone(),
            })
        }
    }
}

fn restore_snapshot_messages(
    assets: &AssetStore,
    json: &str,
) -> Result<Vec<Message>, StorageError> {
    let persisted: Vec<PersistedMessage> =
        serde_json::from_str(json).map_err(StorageError::SessionSerialize)?;
    persisted
        .into_iter()
        .map(|message| {
            Ok(Message {
                role: message.role,
                content: restore_content(assets, message.content),
                tool_calls: message.tool_calls,
                tool_call_id: message.tool_call_id,
            })
        })
        .collect()
}

fn restore_content(assets: &AssetStore, content: PersistedMessageContent) -> MessageContent {
    match content {
        PersistedMessageContent::Text(text) => MessageContent::text(text),
        PersistedMessageContent::Parts(parts) => MessageContent::parts(
            parts
                .into_iter()
                .map(|part| restore_part(assets, part))
                .collect(),
        ),
    }
}

fn restore_part(assets: &AssetStore, part: PersistedMessageContentPart) -> MessageContentPart {
    match part {
        PersistedMessageContentPart::InputText { text } => MessageContentPart::InputText { text },
        PersistedMessageContentPart::InputImageRef {
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
    let updated_at = call_blocking(Arc::clone(&state.db), move |db| {
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
    use secrecy::SecretString;

    use super::{load_messages_for_turn, persist_phase};
    use crate::agent_loop::SurfaceContext;
    use crate::assets::AssetStore;
    use crate::config::{Config, ProviderConfig};
    use crate::error::LlmError;
    use crate::llm::{LlmProvider, Message, MessageContent, MessageContentPart, MessagesResponse};
    use crate::runtime::AppState;
    use crate::skills::SkillManager;
    use crate::storage::{Database, StoredMessage, call_blocking};
    use crate::tools::ToolRegistry;

    struct FakeProvider {
        response: String,
    }

    #[async_trait]
    impl LlmProvider for FakeProvider {
        async fn send_message(
            &self,
            _system: &str,
            messages: Vec<Message>,
            _tools: Option<Vec<crate::llm::ToolDefinition>>,
        ) -> Result<MessagesResponse, LlmError> {
            let prompt = messages
                .iter()
                .map(|message| format!("{}:{}", message.role, message.content.as_text_lossy()))
                .collect::<Vec<_>>()
                .join("|");
            Ok(MessagesResponse {
                content: format!("{} [{prompt}]", self.response),
                tool_calls: Vec::new(),
            })
        }
    }

    fn test_config(data_dir: String) -> Config {
        Config {
            default_provider: "openai".to_string(),
            default_model: Some("gpt-4o-mini".to_string()),
            providers: std::collections::HashMap::from([(
                "openai".to_string(),
                ProviderConfig {
                    label: "OpenAI".to_string(),
                    base_url: "https://api.openai.com/v1".to_string(),
                    api_key: Some(SecretString::new("sk-test".to_string().into_boxed_str())),
                    default_model: "gpt-4o-mini".to_string(),
                    models: vec!["gpt-4o-mini".to_string()],
                },
            )]),
            data_dir,
            log_level: "info".to_string(),
            compaction_timeout_secs: 180,
            max_history_messages: 50,
            max_session_messages: 40,
            compact_keep_recent: 20,
            channels: std::collections::HashMap::from([(
                "web".to_string(),
                crate::config::ChannelConfig {
                    enabled: Some(true),
                    host: Some("127.0.0.1".to_string()),
                    port: Some(10961),
                    ..Default::default()
                },
            )]),
        }
    }

    fn cli_context(session: &str) -> SurfaceContext {
        SurfaceContext {
            channel: "cli".to_string(),
            surface_user: "local_user".to_string(),
            surface_thread: session.to_string(),
            chat_type: "cli".to_string(),
        }
    }

    fn build_state_with_provider(data_dir: String, llm: Box<dyn LlmProvider>) -> AppState {
        use crate::channel_adapter::ChannelRegistry;
        let config = test_config(data_dir.clone());
        let skills = Arc::new(SkillManager::from_skills_dir(
            config.skills_dir().expect("skills_dir"),
        ));
        AppState {
            db: Arc::new(Database::new(&data_dir).expect("db")),
            config: config.clone(),
            config_path: None,
            llm_override: Some(Arc::from(llm)),
            channels: Arc::new(ChannelRegistry::new()),
            skills: Arc::clone(&skills),
            tools: Arc::new(ToolRegistry::new(&config, skills)),
            assets: Arc::new(AssetStore::new(&data_dir).expect("assets")),
        }
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
            move |db| {
                db.resolve_or_create_chat_id(
                    &channel,
                    &session_key,
                    Some(&surface_thread),
                    &chat_type,
                )
            }
        })
        .await
        .expect("chat id");

        let seed_message = StoredMessage {
            id: "seed-user".to_string(),
            chat_id,
            sender_name: context.surface_user.clone(),
            content: "hello".to_string(),
            is_from_bot: false,
            timestamp: "2024-01-01T00:00:00Z".to_string(),
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
            db.load_session(chat_id)
                .map(|session| session.expect("session").1)
        })
        .await
        .expect("stale updated_at");

        let concurrent_message = StoredMessage {
            id: "seed-assistant".to_string(),
            chat_id,
            sender_name: "egopulse".to_string(),
            content: "hi".to_string(),
            is_from_bot: true,
            timestamp: "2024-01-01T00:00:01Z".to_string(),
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
            StoredMessage {
                id: "new-user".to_string(),
                chat_id,
                sender_name: context.surface_user.clone(),
                content: "next".to_string(),
                is_from_bot: false,
                timestamp: "2024-01-01T00:00:02Z".to_string(),
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
            tool_calls: Vec::new(),
            tool_call_id: Some("call_1".to_string()),
        }];

        persist_phase(
            &state,
            StoredMessage {
                id: "tool-msg".to_string(),
                chat_id,
                sender_name: "egopulse".to_string(),
                content: "Read image file [image/png]".to_string(),
                is_from_bot: true,
                timestamp: "2024-01-01T00:00:00Z".to_string(),
            },
            messages[0].clone(),
            &messages,
            None,
        )
        .await
        .expect("persist image turn");

        let (session_json, _) = call_blocking(Arc::clone(&state.db), move |db| {
            db.load_session(chat_id)
                .map(|session| session.expect("session row"))
        })
        .await
        .expect("load session row");
        assert!(!session_json.contains("data:image/png;base64"));
        assert!(session_json.contains("\"type\":\"input_image_ref\""));

        let loaded = load_messages_for_turn(&state, chat_id)
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

        let loaded = load_messages_for_turn(&state, chat_id)
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

        let loaded = load_messages_for_turn(&state, chat_id)
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
            db.load_session(chat_id)
                .map(|session| session.expect("session").1)
        })
        .await
        .expect("stale updated_at");

        let concurrent_message = StoredMessage {
            id: "concurrent-assistant".to_string(),
            chat_id,
            sender_name: "egopulse".to_string(),
            content: "concurrent".to_string(),
            is_from_bot: true,
            timestamp: "2024-01-01T00:00:52Z".to_string(),
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
            StoredMessage {
                id: "new-user-full".to_string(),
                chat_id,
                sender_name: context.surface_user.clone(),
                content: "next".to_string(),
                is_from_bot: false,
                timestamp: "2024-01-01T00:00:53Z".to_string(),
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
}
