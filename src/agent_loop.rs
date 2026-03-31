use crate::error::{EgoPulseError, StorageError};
use crate::llm::Message;
use crate::runtime::AppState;
use crate::storage::{SessionSnapshot, StoredMessage, call_blocking};

const MAX_HISTORY_MESSAGES: usize = 50;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SurfaceContext {
    pub channel: String,
    pub surface_user: String,
    pub surface_thread: String,
    pub chat_type: String,
}

impl SurfaceContext {
    pub fn session_key(&self) -> String {
        format!("{}:{}", self.channel, self.surface_thread)
    }
}

#[derive(Debug, Clone)]
struct LoadedSession {
    messages: Vec<Message>,
    session_updated_at: Option<String>,
}

#[derive(Debug, Clone)]
struct PersistedTurn {
    updated_at: String,
    messages: Vec<Message>,
}

pub async fn process_turn(
    state: &AppState,
    context: &SurfaceContext,
    user_input: &str,
) -> Result<String, EgoPulseError> {
    let chat_id = call_blocking(state.db.clone(), {
        let channel = context.channel.clone();
        let session_key = context.session_key();
        let surface_thread = context.surface_thread.clone();
        let chat_type = context.chat_type.clone();
        move |db| {
            db.resolve_or_create_chat_id(&channel, &session_key, Some(&surface_thread), &chat_type)
        }
    })
    .await?;

    let LoadedSession {
        mut messages,
        session_updated_at,
    } = load_messages_for_turn(state, chat_id).await?;
    let mut session_updated_at = session_updated_at;
    let user_message = Message {
        role: "user".to_string(),
        content: user_input.to_string(),
    };
    messages.push(Message {
        role: user_message.role.clone(),
        content: user_message.content.clone(),
    });

    let persisted_user_turn = persist_phase(
        state,
        StoredMessage {
            id: uuid::Uuid::new_v4().to_string(),
            chat_id,
            sender_name: context.surface_user.clone(),
            content: user_input.to_string(),
            is_from_bot: false,
            timestamp: chrono::Utc::now().to_rfc3339(),
        },
        user_message,
        &messages,
        session_updated_at,
    )
    .await?;
    messages = persisted_user_turn.messages;
    session_updated_at = Some(persisted_user_turn.updated_at);

    let response = state.llm.send_message("", messages.clone()).await?;
    let assistant_message = Message {
        role: "assistant".to_string(),
        content: response.content.clone(),
    };
    messages.push(Message {
        role: assistant_message.role.clone(),
        content: assistant_message.content.clone(),
    });

    let _persisted_assistant_turn = persist_phase(
        state,
        StoredMessage {
            id: uuid::Uuid::new_v4().to_string(),
            chat_id,
            sender_name: "egopulse".to_string(),
            content: response.content.clone(),
            is_from_bot: true,
            timestamp: chrono::Utc::now().to_rfc3339(),
        },
        assistant_message,
        &messages,
        session_updated_at,
    )
    .await?;
    Ok(response.content)
}

async fn load_messages_for_turn(
    state: &AppState,
    chat_id: i64,
) -> Result<LoadedSession, EgoPulseError> {
    let snapshot = call_blocking(state.db.clone(), move |db| {
        db.load_session_snapshot(chat_id, MAX_HISTORY_MESSAGES)
    })
    .await?;

    Ok(snapshot_to_loaded(snapshot))
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
                .map(|message| Message {
                    role: if message.is_from_bot {
                        "assistant".to_string()
                    } else {
                        "user".to_string()
                    },
                    content: message.content.clone(),
                })
                .collect::<Vec<_>>(),
        ),
        session_updated_at: snapshot.updated_at,
    }
}

async fn persist_phase(
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

    use crate::config::Config;
    use crate::error::{EgoPulseError, LlmError};
    use crate::llm::{LlmProvider, Message, MessagesResponse};
    use crate::runtime::AppState;
    use crate::storage::{Database, StoredMessage, call_blocking};

    use super::{SurfaceContext, process_turn};

    struct FakeProvider {
        response: String,
    }

    struct FailingProvider;

    #[async_trait]
    impl LlmProvider for FakeProvider {
        async fn send_message(
            &self,
            _system: &str,
            messages: Vec<Message>,
        ) -> Result<MessagesResponse, LlmError> {
            let prompt = messages
                .iter()
                .map(|message| format!("{}:{}", message.role, message.content))
                .collect::<Vec<_>>()
                .join("|");
            Ok(MessagesResponse {
                content: format!("{} [{prompt}]", self.response),
            })
        }
    }

    #[async_trait]
    impl LlmProvider for FailingProvider {
        async fn send_message(
            &self,
            _system: &str,
            _messages: Vec<Message>,
        ) -> Result<MessagesResponse, LlmError> {
            Err(LlmError::InvalidResponse("simulated failure".to_string()))
        }
    }

    fn test_config(data_dir: String) -> Config {
        Config {
            model: "gpt-4o-mini".to_string(),
            api_key: Some(SecretString::new("sk-test".to_string().into_boxed_str())),
            llm_base_url: "https://api.openai.com/v1".to_string(),
            data_dir,
            log_level: "info".to_string(),
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
        AppState {
            db: Arc::new(Database::new(&data_dir).expect("db")),
            config: test_config(data_dir),
            llm,
        }
    }

    #[test]
    fn session_key_is_channel_agnostic_but_surface_stable() {
        let context = cli_context("local-dev");
        assert_eq!(context.session_key(), "cli:local-dev");
    }

    #[tokio::test]
    async fn reuses_saved_session_before_falling_back_to_history() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = build_state_with_provider(
            dir.path().to_str().expect("utf8").to_string(),
            Box::new(FakeProvider {
                response: "ok".to_string(),
            }),
        );
        let context = cli_context("local-dev");

        let first = process_turn(&state, &context, "hello")
            .await
            .expect("first");
        let second = process_turn(&state, &context, "remember")
            .await
            .expect("second");

        assert!(first.contains("user:hello"));
        assert!(second.contains("user:hello"));
        assert!(second.contains("assistant:ok [user:hello]"));
        assert!(second.contains("user:remember"));
    }

    #[tokio::test]
    async fn falls_back_to_history_when_session_json_is_corrupted() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = build_state_with_provider(
            dir.path().to_str().expect("utf8").to_string(),
            Box::new(FakeProvider {
                response: "ok".to_string(),
            }),
        );
        let context = cli_context("recover");

        let chat_id = call_blocking(state.db.clone(), {
            let session_key = context.session_key();
            move |db| db.resolve_or_create_chat_id("cli", &session_key, Some("recover"), "cli")
        })
        .await
        .expect("chat id");

        call_blocking(state.db.clone(), move |db| {
            db.store_message(&StoredMessage {
                id: "user-1".to_string(),
                chat_id,
                sender_name: "local_user".to_string(),
                content: "hello".to_string(),
                is_from_bot: false,
                timestamp: "2024-01-01T00:00:00Z".to_string(),
            })?;
            db.store_message(&StoredMessage {
                id: "assistant-1".to_string(),
                chat_id,
                sender_name: "egopulse".to_string(),
                content: "hi".to_string(),
                is_from_bot: true,
                timestamp: "2024-01-01T00:00:01Z".to_string(),
            })?;
            db.save_session(chat_id, "{not-json")
        })
        .await
        .expect("seed");

        let response = process_turn(&state, &context, "remember")
            .await
            .expect("response");

        assert!(response.contains("user:hello"));
        assert!(response.contains("assistant:hi"));
        assert!(response.contains("user:remember"));
    }

    #[tokio::test]
    async fn keeps_user_message_in_history_when_provider_fails() {
        let dir = tempfile::tempdir().expect("tempdir");
        let failing_state = build_state_with_provider(
            dir.path().to_str().expect("utf8").to_string(),
            Box::new(FailingProvider),
        );
        let context = cli_context("failure");

        let error = process_turn(&failing_state, &context, "hello")
            .await
            .expect_err("provider failure");
        assert!(matches!(error, EgoPulseError::Llm(_)));

        let chat_id = call_blocking(failing_state.db.clone(), {
            let session_key = context.session_key();
            move |db| db.resolve_or_create_chat_id("cli", &session_key, Some("failure"), "cli")
        })
        .await
        .expect("chat id");

        let history = call_blocking(failing_state.db.clone(), move |db| {
            db.get_all_messages(chat_id)
        })
        .await
        .expect("history");
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].content, "hello");
        assert!(!history[0].is_from_bot);

        let recovered_state = build_state_with_provider(
            dir.path().to_str().expect("utf8").to_string(),
            Box::new(FakeProvider {
                response: "ok".to_string(),
            }),
        );
        let resumed = process_turn(&recovered_state, &context, "retry")
            .await
            .expect("resume after failure");
        assert!(resumed.contains("user:hello"));
        assert!(resumed.contains("user:retry"));
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

        let persisted = super::persist_phase(
            &state,
            StoredMessage {
                id: "new-user".to_string(),
                chat_id,
                sender_name: context.surface_user.clone(),
                content: "next".to_string(),
                is_from_bot: false,
                timestamp: "2024-01-01T00:00:02Z".to_string(),
            },
            Message {
                role: "user".to_string(),
                content: "next".to_string(),
            },
            &[Message {
                role: "user".to_string(),
                content: "hello".to_string(),
            }],
            Some(stale_session_updated_at),
        )
        .await
        .expect("persist turn");

        assert_eq!(persisted.messages.len(), 3);
        assert_eq!(persisted.messages[1].content, "hi");
        assert_eq!(persisted.messages[2].content, "next");
    }
}
