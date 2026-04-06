use crate::agent_loop::SurfaceContext;
use crate::agent_loop::session::{load_messages_for_turn, persist_phase, resolve_chat_id};
use crate::error::{ChannelError, EgoPulseError};
use crate::llm::Message;
use crate::runtime::{AppState, build_app_state};
use crate::storage::StoredMessage;
use crate::web::sse::AgentEvent;

pub async fn ask_in_session(
    config: crate::config::Config,
    session: &str,
    prompt: &str,
) -> Result<String, EgoPulseError> {
    let state = build_app_state(config)?;
    let context = SurfaceContext {
        channel: "cli".to_string(),
        surface_user: "local_user".to_string(),
        surface_thread: session.to_string(),
        chat_type: "cli".to_string(),
    };

    tokio::select! {
        response = process_turn(&state, &context, prompt) => response,
        _ = tokio::signal::ctrl_c() => Err(EgoPulseError::ShutdownRequested),
    }
}

pub async fn send_turn(
    state: &AppState,
    context: &SurfaceContext,
    prompt: &str,
) -> Result<String, EgoPulseError> {
    tokio::select! {
        response = process_turn(state, context, prompt) => response,
        _ = tokio::signal::ctrl_c() => Err(EgoPulseError::ShutdownRequested),
    }
}

pub async fn process_turn(
    state: &AppState,
    context: &SurfaceContext,
    user_input: &str,
) -> Result<String, EgoPulseError> {
    let chat_id = resolve_chat_id(state, context).await?;

    let crate::agent_loop::session::LoadedSession {
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

pub async fn process_turn_with_events<F>(
    state: &AppState,
    context: &SurfaceContext,
    user_input: &str,
    on_event: F,
) -> Result<String, EgoPulseError>
where
    F: Fn(AgentEvent) + Send + Sync,
{
    on_event(AgentEvent::Iteration { iteration: 1 });

    let chat_id = resolve_chat_id(state, context).await?;

    let crate::agent_loop::session::LoadedSession {
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

    let start = std::time::Instant::now();
    let llm = state.llm.clone();
    let messages_for_llm = messages.clone();

    let (text_tx, mut text_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let response_handle = tokio::spawn(async move {
        llm.send_message_stream("", messages_for_llm, Some(&text_tx))
            .await
    });

    let mut streamed_text = String::new();
    while let Some(chunk) = text_rx.recv().await {
        if !chunk.is_empty() {
            streamed_text.push_str(&chunk);
            on_event(AgentEvent::TextDelta { delta: chunk });
        }
    }

    let response = response_handle.await.map_err(|error| {
        EgoPulseError::Channel(ChannelError::SendFailed(format!(
            "stream task join failed: {error}"
        )))
    })??;

    let duration_ms = start.elapsed().as_millis();

    let final_content = if streamed_text.is_empty() {
        response.content.clone()
    } else {
        streamed_text
    };

    let assistant_message = Message {
        role: "assistant".to_string(),
        content: final_content.clone(),
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
            content: final_content.clone(),
            is_from_bot: true,
            timestamp: chrono::Utc::now().to_rfc3339(),
        },
        assistant_message,
        &messages,
        session_updated_at,
    )
    .await?;

    on_event(AgentEvent::FinalResponse {
        text: final_content.clone(),
    });

    tracing::debug!(
        channel = %context.channel,
        chat_id = chat_id,
        duration_ms = duration_ms,
        "Turn completed"
    );

    Ok(final_content)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use async_trait::async_trait;
    use secrecy::SecretString;

    use crate::agent_loop::{SurfaceContext, process_turn};
    use crate::config::Config;
    use crate::error::{EgoPulseError, LlmError};
    use crate::llm::{LlmProvider, Message, MessagesResponse};
    use crate::runtime::AppState;
    use crate::storage::{Database, StoredMessage, call_blocking};

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
        AppState {
            db: Arc::new(Database::new(&data_dir).expect("db")),
            config: test_config(data_dir),
            config_path: None,
            llm: Arc::from(llm),
            channels: Arc::new(ChannelRegistry::new()),
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
}
