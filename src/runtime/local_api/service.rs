//! Runtime-owned handlers for local API operations.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, WriteHalf};
use tokio::net::UnixStream;
use tokio::sync::{mpsc::unbounded_channel, oneshot};

use crate::agent_loop;
use crate::agent_loop::event::AgentEvent;
use crate::agent_loop::message_format::{is_tool_error_message, tool_result_body};
use crate::conversation::SurfaceContext;
use crate::error::EgoPulseError;
use crate::runtime::AppState;
use crate::runtime::channel_input::ToolFollowupOutcome;
use crate::slash_commands::SlashCommandOutcome;
use crate::storage::{SessionSummary as StoredSessionSummary, call_blocking};

use super::PROTOCOL_VERSION;
use super::protocol::{
    CommandOutcome, FollowupOutcome, Request, RequestEnvelope, Response, ResponseEnvelope,
    SessionReference, SessionSummary, SessionView, TranscriptEntry, TurnEvent,
};

pub(crate) async fn handle_connection(
    stream: UnixStream,
    state: Arc<AppState>,
) -> Result<(), EgoPulseError> {
    let (read_half, mut write_half) = tokio::io::split(stream);
    let mut lines = BufReader::new(read_half).lines();
    let Some(line) = lines
        .next_line()
        .await
        .map_err(|error| EgoPulseError::RuntimeLocalApi(error.to_string()))?
    else {
        return Ok(());
    };

    let envelope: RequestEnvelope = match serde_json::from_str(&line) {
        Ok(envelope) => envelope,
        Err(error) => {
            write_response(
                &mut write_half,
                Response::Error {
                    message: format!("invalid request: {error}"),
                },
            )
            .await?;
            return Ok(());
        }
    };

    if envelope.protocol_version != PROTOCOL_VERSION {
        write_response(
            &mut write_half,
            Response::ProtocolMismatch {
                expected: PROTOCOL_VERSION,
                actual: envelope.protocol_version,
            },
        )
        .await?;
        return Ok(());
    }

    match envelope.request {
        Request::RuntimeInfo => {
            write_response(
                &mut write_half,
                Response::RuntimeInfo {
                    protocol_version: PROTOCOL_VERSION,
                    egopulse_version: env!("CARGO_PKG_VERSION").to_string(),
                },
            )
            .await?;
        }
        Request::ListSessions => match list_sessions(&state).await {
            Ok(sessions) => {
                write_response(&mut write_half, Response::Sessions { sessions }).await?
            }
            Err(error) => write_error(&mut write_half, error).await?,
        },
        Request::OpenSession { session } => match open_session(&state, session).await {
            Ok(session) => write_response(&mut write_half, Response::Session { session }).await?,
            Err(error) => write_error(&mut write_half, error).await?,
        },
        Request::ExecuteCommand {
            session,
            text,
            sender_id,
        } => match execute_command(&state, session, &text, sender_id.as_deref()).await {
            Ok((outcome, provider, model)) => {
                write_response(
                    &mut write_half,
                    Response::CommandFinished {
                        outcome,
                        effective_provider: provider,
                        effective_model: model,
                    },
                )
                .await?;
            }
            Err(error) => write_error(&mut write_half, error).await?,
        },
        Request::StageFollowup { session, input } => {
            match stage_followup(&state, session, input).await {
                Ok(outcome) => {
                    write_response(&mut write_half, Response::FollowupFinished { outcome }).await?
                }
                Err(error) => write_error(&mut write_half, error).await?,
            }
        }
        Request::ExecuteTurn { session, prompt } => {
            execute_turn(&state, &mut write_half, session, prompt).await?;
        }
    }
    Ok(())
}

async fn list_sessions(state: &AppState) -> Result<Vec<SessionSummary>, EgoPulseError> {
    let sessions = call_blocking(Arc::clone(&state.db), |db| db.list_sessions()).await?;
    Ok(sessions.into_iter().map(session_summary).collect())
}

async fn open_session(
    state: &AppState,
    reference: SessionReference,
) -> Result<SessionView, EgoPulseError> {
    let resolved = resolve_session(state, reference).await?;
    let messages = match &resolved.reference {
        SessionReference::Existing { .. } => {
            agent_loop::load_transcript_history(&state.turn_dependencies(), &resolved.context)
                .await?
        }
        SessionReference::Named { .. } => Vec::new(),
    };
    Ok(SessionView {
        reference: resolved.reference,
        channel: resolved.context.channel,
        surface_thread: resolved.display_surface_thread,
        chat_type: resolved.context.chat_type,
        agent_id: resolved.context.agent_id,
        effective_provider: resolved.provider,
        effective_model: resolved.model,
        transcript: messages_to_entries(&messages),
    })
}

async fn execute_command(
    state: &AppState,
    reference: SessionReference,
    text: &str,
    sender_id: Option<&str>,
) -> Result<(CommandOutcome, String, String), EgoPulseError> {
    ensure_runtime_accepting(state)?;
    let resolved = resolve_session(state, reference).await?;
    let outcome =
        crate::slash_commands::process_slash_command(state, &resolved.context, text, sender_id)
            .await;
    let outcome = match outcome {
        SlashCommandOutcome::Respond(text) => CommandOutcome::Respond { text },
        SlashCommandOutcome::Error(message) => CommandOutcome::Error { message },
        SlashCommandOutcome::NotHandled => CommandOutcome::NotHandled,
    };
    let (provider, model) = effective_model(state, &resolved.context)?;
    Ok((outcome, provider, model))
}

async fn stage_followup(
    state: &Arc<AppState>,
    reference: SessionReference,
    input: String,
) -> Result<FollowupOutcome, EgoPulseError> {
    let resolved = resolve_session(state, reference).await?;
    let mut context = resolved.context;
    context.request_key = format!("tui:{}", uuid::Uuid::new_v4());
    match crate::runtime::try_stage_tool_followup(state, context, input).await? {
        ToolFollowupOutcome::Accepted => Ok(FollowupOutcome::Accepted),
        ToolFollowupOutcome::NoToolPhase => Ok(FollowupOutcome::NoToolPhase),
    }
}

async fn execute_turn(
    state: &AppState,
    writer: &mut WriteHalf<UnixStream>,
    reference: SessionReference,
    prompt: String,
) -> Result<(), EgoPulseError> {
    ensure_runtime_accepting(state)?;
    let resolved = resolve_session(state, reference).await?;
    let (events_tx, mut events_rx) = unbounded_channel();
    let (completion_tx, mut completion_rx) = oneshot::channel();
    let dependencies = state.turn_dependencies();
    let context = resolved.context;
    state.supervisor.spawn_turn(async move {
        let result =
            agent_loop::process_turn_with_events(&dependencies, &context, &prompt, move |event| {
                let _ = events_tx.send(event);
            })
            .await;
        let _ = completion_tx.send(result);
    });

    loop {
        tokio::select! {
            Some(event) = events_rx.recv() => {
                write_response(writer, Response::TurnEvent { event: map_agent_event(event) }).await?;
            }
            result = &mut completion_rx => {
                while let Ok(event) = events_rx.try_recv() {
                    write_response(writer, Response::TurnEvent { event: map_agent_event(event) }).await?;
                }
                match result {
                    Ok(Ok(response)) => write_response(writer, Response::TurnFinished { response }).await?,
                    Ok(Err(error)) => write_response(writer, Response::Error { message: error.user_message() }).await?,
                    Err(_) => write_response(writer, Response::Error { message: "runtime turn ended unexpectedly".to_string() }).await?,
                }
                return Ok(());
            }
        }
    }
}

struct ResolvedSession {
    reference: SessionReference,
    context: SurfaceContext,
    display_surface_thread: String,
    provider: String,
    model: String,
}

async fn resolve_session(
    state: &AppState,
    reference: SessionReference,
) -> Result<ResolvedSession, EgoPulseError> {
    let (context, display_surface_thread) = match &reference {
        SessionReference::Existing { chat_id } => {
            let sessions = call_blocking(Arc::clone(&state.db), |db| db.list_sessions()).await?;
            let summary = sessions
                .into_iter()
                .find(|summary| summary.chat_id == *chat_id)
                .ok_or_else(|| EgoPulseError::Internal("session chat was not found".to_string()))?;
            let chat_id = *chat_id;
            let chat = call_blocking(Arc::clone(&state.db), move |db| db.get_chat_by_id(chat_id))
                .await?
                .ok_or_else(|| EgoPulseError::Internal("session chat was not found".to_string()))?;
            let agent_id = if chat.agent_id.is_empty() {
                if summary.agent_id.is_empty() {
                    state.current_config().default_agent.to_string()
                } else {
                    summary.agent_id.clone()
                }
            } else {
                chat.agent_id.clone()
            };
            let context_thread = context_thread_from_chat(&chat, &agent_id);
            (
                SurfaceContext::new(
                    chat.channel,
                    "local_user".to_string(),
                    context_thread,
                    chat.chat_type,
                    agent_id,
                ),
                summary.surface_thread,
            )
        }
        SessionReference::Named { name } => (
            SurfaceContext::new(
                "tui".to_string(),
                "local_user".to_string(),
                name.clone(),
                "tui".to_string(),
                state.current_config().default_agent.to_string(),
            ),
            name.clone(),
        ),
    };
    let (provider, model) = effective_model(state, &context)?;
    Ok(ResolvedSession {
        reference,
        context,
        display_surface_thread,
        provider,
        model,
    })
}

fn context_thread_from_chat(chat: &crate::storage::ChatInfo, agent_id: &str) -> String {
    let without_channel = chat
        .external_chat_id
        .strip_prefix(&format!("{}:", chat.channel))
        .unwrap_or(&chat.external_chat_id);
    without_channel
        .strip_suffix(&format!(":agent:{agent_id}"))
        .unwrap_or(without_channel)
        .to_string()
}

fn effective_model(
    state: &AppState,
    context: &SurfaceContext,
) -> Result<(String, String), EgoPulseError> {
    let config = state.current_config();
    let resolved = match config.resolve_llm_for_agent_channel(
        &crate::config::AgentId::new(&context.agent_id),
        &context.channel,
    ) {
        Ok(resolved) => resolved,
        Err(_) => config.resolve_global_llm(),
    };
    Ok((resolved.provider, resolved.model))
}

fn ensure_runtime_accepting(state: &AppState) -> Result<(), EgoPulseError> {
    if state.supervisor.accepting_inputs() {
        Ok(())
    } else {
        Err(EgoPulseError::ShutdownRequested)
    }
}

fn session_summary(summary: StoredSessionSummary) -> SessionSummary {
    SessionSummary {
        chat_id: summary.chat_id,
        channel: summary.channel,
        surface_thread: summary.surface_thread,
        chat_title: summary.chat_title,
        last_message_time: summary.last_message_time,
        last_message_preview: summary.last_message_preview,
        agent_id: summary.agent_id,
    }
}

fn messages_to_entries(messages: &[crate::llm::Message]) -> Vec<TranscriptEntry> {
    let mut entries = Vec::new();
    let mut tools = HashMap::new();
    for message in messages {
        let text = message.content.as_text_lossy();
        match message.role.as_str() {
            "user" => entries.push(TranscriptEntry::User { text }),
            "assistant" => {
                if !text.trim().is_empty() {
                    entries.push(TranscriptEntry::Assistant { text });
                }
                for call in &message.tool_calls {
                    tools.insert(call.id.clone(), (call.name.clone(), call.arguments.clone()));
                    entries.push(TranscriptEntry::ToolStarted {
                        call_id: call.id.clone(),
                        name: call.name.clone(),
                        input: call.arguments.clone(),
                    });
                }
            }
            "tool" => {
                let call_id = message.tool_call_id.clone().unwrap_or_default();
                let (name, _) = tools
                    .get(&call_id)
                    .cloned()
                    .unwrap_or_else(|| ("tool".to_string(), serde_json::Value::Null));
                entries.push(TranscriptEntry::ToolFinished {
                    call_id,
                    name,
                    is_error: is_tool_error_message(message),
                    preview: crate::channels::utils::text::truncate_by_chars(
                        &tool_result_body(&text),
                        120,
                    ),
                    duration_ms: None,
                });
            }
            "system" => entries.push(TranscriptEntry::System { text }),
            _ => entries.push(TranscriptEntry::Assistant { text }),
        }
    }
    entries
}

fn map_agent_event(event: AgentEvent) -> TurnEvent {
    match event {
        AgentEvent::Iteration { iteration } => TurnEvent::Iteration { iteration },
        AgentEvent::Delta { text } => TurnEvent::Delta { text },
        AgentEvent::ToolStart {
            name,
            input,
            call_id,
        } => TurnEvent::ToolStart {
            name,
            input,
            call_id,
        },
        AgentEvent::ToolResult {
            name,
            is_error,
            preview,
            duration_ms,
            call_id,
        } => TurnEvent::ToolResult {
            name,
            is_error,
            preview,
            duration_ms,
            call_id,
        },
        AgentEvent::UserInputInjected {
            message_id,
            sender_id,
            text,
            timestamp,
        } => TurnEvent::UserInputInjected {
            message_id,
            sender_id,
            text,
            timestamp,
        },
        AgentEvent::FinalResponse { text } => TurnEvent::FinalResponse { text },
        AgentEvent::Error { message } => TurnEvent::Error { message },
    }
}

async fn write_response(
    writer: &mut WriteHalf<UnixStream>,
    response: Response,
) -> Result<(), EgoPulseError> {
    let envelope = ResponseEnvelope {
        protocol_version: PROTOCOL_VERSION,
        response,
    };
    let mut bytes = serde_json::to_vec(&envelope)
        .map_err(|error| EgoPulseError::RuntimeLocalApi(error.to_string()))?;
    bytes.push(b'\n');
    writer
        .write_all(&bytes)
        .await
        .map_err(|error| EgoPulseError::RuntimeLocalApi(error.to_string()))?;
    writer
        .flush()
        .await
        .map_err(|error| EgoPulseError::RuntimeLocalApi(error.to_string()))
}

async fn write_error(
    writer: &mut WriteHalf<UnixStream>,
    error: EgoPulseError,
) -> Result<(), EgoPulseError> {
    write_response(
        writer,
        Response::Error {
            message: error.user_message(),
        },
    )
    .await
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use super::{handle_connection, map_agent_event, messages_to_entries};
    use crate::agent_loop::event::AgentEvent;
    use crate::config::{AgentConfig, AgentId, AgentProfileConfig};
    use crate::llm::{Message, MessageContent, ToolCall};
    use crate::runtime::local_api::LocalRuntimeClient;
    use crate::runtime::local_api::protocol::{
        CommandOutcome, Request, RequestEnvelope, Response, ResponseEnvelope, SessionReference,
        TranscriptEntry, TurnEvent,
    };
    use crate::runtime::local_api::{PROTOCOL_VERSION, server};
    use crate::storage::{MessageKind, SenderKind, StoredMessage};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixStream;

    fn runtime_state(dir: &tempfile::TempDir) -> Arc<crate::runtime::AppState> {
        let mut config = crate::test_util::test_config(dir.path().to_str().expect("utf8"));
        config.channels.clear();
        config.agents.insert(
            AgentId::new("special"),
            AgentConfig {
                profiles: HashMap::from([(
                    "discord".to_string(),
                    AgentProfileConfig {
                        model: Some("discord-model".to_string()),
                        ..Default::default()
                    },
                )]),
                ..Default::default()
            },
        );
        Arc::new(crate::test_util::build_state_with_config(
            config, None, None, None, None,
        ))
    }

    async fn start_runtime(
        dir: &tempfile::TempDir,
    ) -> (Arc<crate::runtime::AppState>, LocalRuntimeClient) {
        let state = runtime_state(dir);
        server::start(&state).expect("start local API");
        let path = super::super::socket_path(std::path::Path::new(&state.config.state_root));
        let client = LocalRuntimeClient::connect(path)
            .await
            .expect("connect local API");
        (state, client)
    }

    #[test]
    fn maps_internal_agent_event_to_local_event() {
        let event = map_agent_event(AgentEvent::Delta {
            text: "hello".to_string(),
        });

        assert_eq!(
            event,
            TurnEvent::Delta {
                text: "hello".to_string()
            }
        );
    }

    #[tokio::test]
    async fn runtime_api_lists_and_opens_cross_channel_session() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let (state, client) = start_runtime(&dir).await;
        let chat_id = state
            .db
            .resolve_or_create_chat_id(
                "discord",
                "discord:42:agent:special",
                Some("work"),
                "dm",
                "special",
            )
            .expect("create chat");
        let message = StoredMessage {
            id: "message-1".to_string(),
            chat_id,
            sender_id: "user".to_string(),
            content: "hello".to_string(),
            sender_kind: SenderKind::User,
            timestamp: "2026-08-29T00:00:00Z".to_string(),
            message_kind: MessageKind::Message,
            recipient_agent_id: None,
            seq: None,
            turn_id: None,
            parent_message_id: None,
        };
        state
            .db
            .store_message_with_session(&message, r#"[{"role":"user","content":"hello"}]"#, None)
            .expect("store message");

        // Act
        let sessions = client.list_sessions().await.expect("list sessions");
        let loaded = client
            .open_session(SessionReference::Existing { chat_id })
            .await
            .expect("open session");

        // Assert
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].channel, "discord");
        assert_eq!(sessions[0].agent_id, "special");
        assert_eq!(loaded.channel, "discord");
        assert_eq!(loaded.agent_id, "special");
        assert_eq!(loaded.effective_provider, "openai");
        assert_eq!(loaded.effective_model, "discord-model");
        assert_eq!(
            loaded.transcript,
            vec![TranscriptEntry::User {
                text: "hello".to_string()
            }]
        );

        state.supervisor.shutdown().await;
    }

    #[tokio::test]
    async fn runtime_api_opens_new_named_session_without_creating_it() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let (state, client) = start_runtime(&dir).await;

        // Act
        let loaded = client
            .open_session(SessionReference::Named {
                name: "new-session".to_string(),
            })
            .await
            .expect("open named session");

        // Assert
        assert_eq!(loaded.channel, "tui");
        assert_eq!(loaded.surface_thread, "new-session");
        assert!(loaded.transcript.is_empty());
        assert!(
            client
                .list_sessions()
                .await
                .expect("list sessions")
                .is_empty()
        );

        state.supervisor.shutdown().await;
    }

    #[tokio::test]
    async fn runtime_api_executes_shared_commands() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let (state, client) = start_runtime(&dir).await;
        let session = SessionReference::Named {
            name: "command-session".to_string(),
        };

        // Act
        let model = client
            .execute_command(session.clone(), "/model".to_string())
            .await
            .expect("execute model command");
        let new_session = client
            .execute_command(session, "/new".to_string())
            .await
            .expect("execute new command");

        // Assert
        assert!(matches!(model.outcome, CommandOutcome::Respond { .. }));
        assert_eq!(model.effective_provider, "openai");
        assert_eq!(model.effective_model, "gpt-4o-mini");
        assert!(
            matches!(new_session.outcome, CommandOutcome::Respond { ref text } if text == "Session cleared.")
        );

        state.supervisor.shutdown().await;
    }

    #[tokio::test]
    async fn client_disconnect_does_not_stop_runtime() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let (state, client) = start_runtime(&dir).await;
        let path = super::super::socket_path(std::path::Path::new(&state.config.state_root));

        // Act
        drop(client);
        let reconnected = LocalRuntimeClient::connect(path).await;

        // Assert
        assert!(reconnected.is_ok());
        state.supervisor.shutdown().await;
    }

    #[tokio::test]
    async fn runtime_api_streams_turn_events() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let config = crate::test_util::test_config(dir.path().to_str().expect("utf8"));
        let provider: Arc<dyn crate::llm::LlmProvider> =
            Arc::new(crate::agent_loop::test_support::DeltaEmittingProvider {
                chunks: vec!["one".to_string(), " two".to_string()],
                final_response: "one two".to_string(),
            });
        let state = Arc::new(crate::test_util::build_state_with_config(
            config,
            Some(provider),
            None,
            None,
            None,
        ));
        server::start(&state).expect("start local API");
        let path = super::super::socket_path(std::path::Path::new(&state.config.state_root));
        let client = LocalRuntimeClient::connect(path)
            .await
            .expect("connect local API");
        let events = Arc::new(std::sync::Mutex::new(Vec::new()));
        let received_events = Arc::clone(&events);

        // Act
        let response = client
            .execute_turn(
                SessionReference::Named {
                    name: "turn-session".to_string(),
                },
                "hello".to_string(),
                move |event| received_events.lock().expect("events").push(event),
            )
            .await
            .expect("execute turn");

        // Assert
        assert_eq!(response, "one two");
        assert!(
            events
                .lock()
                .expect("events")
                .iter()
                .any(|event| matches!(event, TurnEvent::Delta { text } if text == "one"))
        );
        state.supervisor.shutdown().await;
    }

    #[tokio::test]
    async fn malformed_request_returns_error_response() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let state = runtime_state(&dir);
        let (client_stream, server_stream) = UnixStream::pair().expect("unix pair");
        let server_task = tokio::spawn(handle_connection(server_stream, state));
        let (read_half, mut write_half) = tokio::io::split(client_stream);
        let mut lines = BufReader::new(read_half).lines();

        // Act
        write_half
            .write_all(b"not-json\n")
            .await
            .expect("write request");
        let line = lines
            .next_line()
            .await
            .expect("read response")
            .expect("response line");
        let response: ResponseEnvelope = serde_json::from_str(&line).expect("decode response");
        server_task
            .await
            .expect("join server")
            .expect("handle request");

        // Assert
        assert_eq!(response.protocol_version, PROTOCOL_VERSION);
        assert!(matches!(response.response, Response::Error { .. }));
    }

    #[test]
    fn protocol_envelope_sets_current_version() {
        // Arrange / Act
        let envelope = RequestEnvelope::new(Request::RuntimeInfo);

        // Assert
        assert_eq!(envelope.protocol_version, PROTOCOL_VERSION);
    }

    #[test]
    fn transcript_conversion_preserves_tool_lifecycle() {
        // Arrange
        let messages = vec![
            Message::text("user", "inspect"),
            Message {
                role: "assistant".to_string(),
                content: MessageContent::text(""),
                reasoning_content: None,
                tool_calls: vec![ToolCall {
                    id: "call-1".to_string(),
                    name: "shell".to_string(),
                    arguments: serde_json::json!({"command": "pwd"}),
                }],
                tool_call_id: None,
            },
            Message {
                role: "tool".to_string(),
                content: MessageContent::text(
                    r#"{"tool":"shell","status":"success","result":"/tmp"}"#,
                ),
                reasoning_content: None,
                tool_calls: Vec::new(),
                tool_call_id: Some("call-1".to_string()),
            },
        ];

        // Act
        let entries = messages_to_entries(&messages);

        // Assert
        assert!(matches!(entries[0], TranscriptEntry::User { ref text } if text == "inspect"));
        assert!(matches!(
            entries[1],
            TranscriptEntry::ToolStarted { ref call_id, ref name, .. }
                if call_id == "call-1" && name == "shell"
        ));
        assert!(matches!(
            entries[2],
            TranscriptEntry::ToolFinished { ref call_id, ref name, is_error: false, ref preview, .. }
                if call_id == "call-1" && name == "shell" && preview == "/tmp"
        ));
    }
}
