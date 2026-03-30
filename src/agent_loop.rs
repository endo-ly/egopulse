use serde_json::Error as JsonError;

use crate::error::EgoPulseError;
use crate::llm::Message;
use crate::runtime::AppState;
use crate::storage::{StoredMessage, call_blocking};

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
        format!(
            "{}:{}:{}",
            self.channel, self.surface_user, self.surface_thread
        )
    }
}

pub async fn process_turn(
    state: &AppState,
    context: &SurfaceContext,
    user_input: &str,
) -> Result<String, EgoPulseError> {
    let chat_id = call_blocking(state.db.clone(), {
        let channel = context.channel.clone();
        let session_key = context.session_key();
        let surface_user = context.surface_user.clone();
        let surface_thread = context.surface_thread.clone();
        let chat_type = context.chat_type.clone();
        move |db| {
            db.resolve_or_create_chat_id(
                &channel,
                &session_key,
                &surface_user,
                &surface_thread,
                Some(&surface_thread),
                &chat_type,
            )
        }
    })
    .await?;

    let mut messages = load_messages_for_turn(state, chat_id).await?;
    messages.push(Message {
        role: "user".to_string(),
        content: user_input.to_string(),
    });

    let response = state.llm.send_message("", messages.clone()).await?;
    messages.push(Message {
        role: "assistant".to_string(),
        content: response.content.clone(),
    });

    persist_turn(
        state,
        chat_id,
        context,
        user_input,
        &response.content,
        &messages,
    )
    .await?;
    Ok(response.content)
}

async fn load_messages_for_turn(
    state: &AppState,
    chat_id: i64,
) -> Result<Vec<Message>, EgoPulseError> {
    let Some(session) = call_blocking(state.db.clone(), move |db| db.load_session(chat_id)).await?
    else {
        return load_messages_from_db(state, chat_id).await;
    };

    let session_messages = deserialize_session_messages(&session.messages_json);
    match session_messages {
        Ok(messages) if !messages.is_empty() => Ok(messages),
        _ => load_messages_from_db(state, chat_id).await,
    }
}

async fn load_messages_from_db(
    state: &AppState,
    chat_id: i64,
) -> Result<Vec<Message>, EgoPulseError> {
    let history = call_blocking(state.db.clone(), move |db| {
        db.get_recent_messages(chat_id, MAX_HISTORY_MESSAGES)
    })
    .await?;
    Ok(history_to_messages(&history))
}

fn history_to_messages(history: &[StoredMessage]) -> Vec<Message> {
    history
        .iter()
        .map(|message| Message {
            role: if message.is_from_bot {
                "assistant".to_string()
            } else {
                "user".to_string()
            },
            content: message.content.clone(),
        })
        .collect()
}

fn deserialize_session_messages(json: &str) -> Result<Vec<Message>, JsonError> {
    serde_json::from_str(json)
}

async fn persist_turn(
    state: &AppState,
    chat_id: i64,
    context: &SurfaceContext,
    user_input: &str,
    assistant_output: &str,
    messages: &[Message],
) -> Result<(), EgoPulseError> {
    let user_message = StoredMessage {
        id: uuid::Uuid::new_v4().to_string(),
        chat_id,
        sender_name: context.surface_user.clone(),
        content: user_input.to_string(),
        is_from_bot: false,
        timestamp: chrono::Utc::now().to_rfc3339(),
    };
    call_blocking(state.db.clone(), move |db| db.store_message(&user_message)).await?;

    let assistant_message = StoredMessage {
        id: uuid::Uuid::new_v4().to_string(),
        chat_id,
        sender_name: "egopulse".to_string(),
        content: assistant_output.to_string(),
        is_from_bot: true,
        timestamp: chrono::Utc::now().to_rfc3339(),
    };
    call_blocking(state.db.clone(), move |db| {
        db.store_message(&assistant_message)
    })
    .await?;

    let session_json =
        serde_json::to_string(messages).map_err(crate::error::StorageError::SessionSerialize)?;
    let provider = state.config.provider_name().to_string();
    let model = state.config.model.clone();
    call_blocking(state.db.clone(), move |db| {
        db.save_session(chat_id, &session_json, &provider, &model)
    })
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use async_trait::async_trait;
    use secrecy::SecretString;

    use crate::config::Config;
    use crate::llm::{LlmProvider, Message, MessagesResponse};
    use crate::runtime::AppState;
    use crate::storage::{Database, StoredMessage, call_blocking};

    use super::{SurfaceContext, process_turn};

    struct FakeProvider {
        response: String,
    }

    #[async_trait]
    impl LlmProvider for FakeProvider {
        async fn send_message(
            &self,
            _system: &str,
            messages: Vec<Message>,
        ) -> Result<MessagesResponse, crate::error::LlmError> {
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

    fn test_state() -> (AppState, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = Database::new(dir.path().to_str().expect("path")).expect("db");
        let config = Config {
            model: "gpt-4o-mini".to_string(),
            api_key: Some(SecretString::new("sk-test".to_string().into_boxed_str())),
            llm_base_url: "https://api.openai.com/v1".to_string(),
            data_dir: dir.path().display().to_string(),
            log_level: "info".to_string(),
        };
        (
            AppState {
                config,
                db: Arc::new(db),
                llm: Box::new(FakeProvider {
                    response: "ok".to_string(),
                }),
            },
            dir,
        )
    }

    fn cli_context(session: &str) -> SurfaceContext {
        SurfaceContext {
            channel: "cli".to_string(),
            surface_user: "local_user".to_string(),
            surface_thread: session.to_string(),
            chat_type: "cli".to_string(),
        }
    }

    #[test]
    fn session_key_is_channel_agnostic_but_surface_stable() {
        let context = cli_context("local-dev");
        assert_eq!(context.session_key(), "cli:local_user:local-dev");
    }

    #[tokio::test]
    async fn reuses_saved_session_before_falling_back_to_history() {
        let (state, _dir) = test_state();
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
        let (state, _dir) = test_state();
        let context = cli_context("recover");

        let chat_id = call_blocking(state.db.clone(), {
            let session_key = context.session_key();
            move |db| {
                db.resolve_or_create_chat_id(
                    "cli",
                    &session_key,
                    "local_user",
                    "recover",
                    Some("recover"),
                    "cli",
                )
            }
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
            db.save_session(chat_id, "{not-json", "openai_compatible", "gpt-4o-mini")
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
}
