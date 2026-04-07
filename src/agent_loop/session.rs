use crate::agent_loop::SurfaceContext;
use crate::error::{EgoPulseError, StorageError};
use crate::llm::Message;
use crate::runtime::AppState;
use crate::storage::{SessionSnapshot, SessionSummary, StoredMessage, call_blocking};

const MAX_HISTORY_MESSAGES: usize = 50;

#[derive(Debug, Clone)]
pub(crate) struct LoadedSession {
    pub(crate) messages: Vec<Message>,
    pub(crate) session_updated_at: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct PersistedTurn {
    pub(crate) updated_at: String,
    pub(crate) messages: Vec<Message>,
}

pub(crate) async fn resolve_chat_id(
    state: &AppState,
    context: &SurfaceContext,
) -> Result<i64, EgoPulseError> {
    call_blocking(state.db.clone(), {
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

pub async fn list_sessions(state: &AppState) -> Result<Vec<SessionSummary>, EgoPulseError> {
    call_blocking(state.db.clone(), move |db| db.list_sessions())
        .await
        .map_err(EgoPulseError::from)
}

pub async fn load_session_messages(
    state: &AppState,
    context: &SurfaceContext,
) -> Result<Vec<Message>, EgoPulseError> {
    let chat_id = resolve_chat_id(state, context).await?;
    let history = call_blocking(state.db.clone(), move |db| db.get_all_messages(chat_id)).await?;
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

pub(crate) async fn load_messages_for_turn(
    state: &AppState,
    chat_id: i64,
) -> Result<LoadedSession, EgoPulseError> {
    let snapshot = call_blocking(state.db.clone(), move |db| {
        db.load_session_snapshot(chat_id, MAX_HISTORY_MESSAGES)
    })
    .await?;

    Ok(snapshot_to_loaded(snapshot))
}

pub(crate) async fn persist_phase(
    state: &AppState,
    message: StoredMessage,
    phase_message: Message,
    messages: &[Message],
    session_updated_at: Option<String>,
) -> Result<PersistedTurn, EgoPulseError> {
    let mut retry_snapshot = trim_history(messages);
    let mut retry_session_updated_at = session_updated_at;

    for attempt in 0..2 {
        let session_json =
            serde_json::to_string(&retry_snapshot).map_err(StorageError::SessionSerialize)?;
        let result = call_blocking(state.db.clone(), {
            let session_updated_at = retry_session_updated_at.clone();
            let session_json = session_json.clone();
            let message = message.clone();
            move |db| {
                db.store_message_with_session(
                    &message,
                    &session_json,
                    session_updated_at.as_deref(),
                )
            }
        })
        .await;

        match result {
            Ok(updated_at) => {
                return Ok(PersistedTurn {
                    updated_at,
                    messages: retry_snapshot.clone(),
                });
            }
            Err(StorageError::SessionSnapshotConflict) if attempt == 0 => {
                let LoadedSession {
                    messages,
                    session_updated_at,
                } = load_messages_for_turn(state, message.chat_id).await?;
                let mut refreshed_messages = messages;
                refreshed_messages.push(phase_message.clone());
                retry_snapshot = trim_history(&refreshed_messages);
                retry_session_updated_at = session_updated_at;
            }
            Err(error) => return Err(EgoPulseError::Storage(error)),
        }
    }

    Err(EgoPulseError::Storage(
        StorageError::SessionSnapshotConflict,
    ))
}

fn snapshot_to_loaded(snapshot: SessionSnapshot) -> LoadedSession {
    if let Some(json) = snapshot.messages_json
        && let Ok(messages) = serde_json::from_str::<Vec<Message>>(&json)
        && !messages.is_empty()
    {
        return LoadedSession {
            messages: trim_history(&messages),
            session_updated_at: snapshot.updated_at,
        };
    }

    LoadedSession {
        messages: trim_history(
            &snapshot
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
                .collect::<Vec<_>>(),
        ),
        session_updated_at: snapshot.updated_at,
    }
}

fn trim_history(messages: &[Message]) -> Vec<Message> {
    if messages.len() <= MAX_HISTORY_MESSAGES {
        return messages.to_vec();
    }

    messages[messages.len() - MAX_HISTORY_MESSAGES..].to_vec()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use async_trait::async_trait;
    use secrecy::SecretString;

    use super::persist_phase;
    use crate::agent_loop::SurfaceContext;
    use crate::config::Config;
    use crate::error::LlmError;
    use crate::llm::{LlmProvider, Message, MessagesResponse};
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
            model: "gpt-4o-mini".to_string(),
            api_key: Some(SecretString::new("sk-test".to_string().into_boxed_str())),
            llm_base_url: "https://api.openai.com/v1".to_string(),
            data_dir,
            log_level: "info".to_string(),
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
        let skills = Arc::new(SkillManager::from_skills_dir(config.skills_dir()));
        AppState {
            db: Arc::new(Database::new(&data_dir).expect("db")),
            config: config.clone(),
            config_path: None,
            llm: Arc::from(llm),
            channels: Arc::new(ChannelRegistry::new()),
            skills: Arc::clone(&skills),
            tools: Arc::new(ToolRegistry::new(&config, skills)),
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

        let chat_id = call_blocking(state.db.clone(), {
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
        call_blocking(state.db.clone(), {
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

        let stale_session_updated_at = call_blocking(state.db.clone(), move |db| {
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
        call_blocking(state.db.clone(), {
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
}
