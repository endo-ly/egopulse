//! Web のストリーミング送信 API を提供するモジュール。
//!
//! チャット run の開始と SSE 購読を仲介し、RunHub と agent loop を接続する。

use axum::Json;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::response::sse::{Event, KeepAlive, Sse};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use uuid::Uuid;

use crate::agent_loop::resolve_chat_id;
use crate::config::AgentId;
use crate::conversation::SurfaceContext;
use crate::error::EgoPulseError;
use crate::runtime::channel_input::{
    ObservedTurnSubmission, session_has_unfinished_turn, submit_observed_agent_turn_with_identity,
    try_stage_tool_followup_with_turn_id,
};
use crate::runtime::turn::SubmitOutcome;

use super::sessions::parse_chat_id_from_session_key;
use super::sse::AgentEvent;
use super::{RUN_TTL_SECONDS, RunLookupError, WEB_ACTOR, WebState, web_session_key};
use crate::storage::{TurnRun, TurnRunState, call_blocking};

#[derive(Debug, Serialize)]
struct StatusPayload {
    message: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ToolStartPayload {
    call_id: String,
    name: String,
    input: serde_json::Value,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ToolResultPayload {
    call_id: String,
    name: String,
    is_error: bool,
    preview: String,
    duration_ms: u128,
}

/// Terminal assistant content. `message_id` is the stable id the response
/// streamed under — the same id as the persisted final row, or a
/// run-scoped stable id for responses without a durable Turn (slash
/// commands), which never rename mid-run.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DonePayload {
    response: String,
    message_id: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DeltaPayload {
    message_id: String,
    delta: String,
}

#[derive(Debug, Serialize)]
struct ErrorPayload {
    error: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct UserInputPayload {
    /// Canonical client-issued message id, identical to the optimistic entry
    /// the client already shows: delivery is an upsert, never an adoption.
    message_id: String,
    sender_id: String,
    text: String,
    timestamp: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AssistantDiscardedPayload {
    message_id: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ReplayMetaPayload {
    replay_truncated: bool,
    oldest_event_id: Option<u64>,
    requested_last_event_id: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
/// Represents a chat message request sent from the web UI.
pub(super) struct SendRequest {
    pub session_key: Option<String>,
    pub message: String,
    /// Agent selected by the WebUI when a new session is created.
    pub agent_id: Option<String>,
    /// Client-generated canonical message id (`web:<uuid>`). The id is the
    /// optimistic entry, the Turn's `request_key`, and the persisted
    /// `messages.id` at once, so a re-delivered send maps to the same Turn
    /// instead of a duplicate.
    #[serde(rename = "messageId")]
    pub message_id: Option<String>,
}

#[derive(Debug, Deserialize)]
/// Captures SSE subscription parameters for a streaming run.
pub(super) struct StreamQuery {
    pub run_id: String,
    pub last_event_id: Option<u64>,
}

#[derive(Debug, Clone)]
/// Identifies a newly accepted streaming run.
pub(super) struct StartedRun {
    pub run_id: String,
    pub session_key: String,
}

#[derive(Debug)]
pub(super) struct ResolvedSend {
    pub message: String,
    pub session_key: String,
    pub context: SurfaceContext,
}

#[derive(Debug, Serialize)]
struct SendStreamResponse {
    ok: bool,
    run_id: String,
    session_key: String,
}

#[derive(Debug, Clone)]
pub(super) struct AcceptedWebInput {
    pub(super) started: StartedRun,
    pub(super) status: &'static str,
}

/// Accepts a Web input and returns its streaming identifiers.
pub(super) async fn api_send_stream(
    State(state): State<WebState>,
    Json(request): Json<SendRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let AcceptedWebInput { started, .. } = accept_web_input(state, request, WEB_ACTOR).await?;

    serde_json::to_value(SendStreamResponse {
        ok: true,
        run_id: started.run_id,
        session_key: started.session_key,
    })
    .map(Json)
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

/// Streams run events over SSE, including replay when available.
pub(super) async fn api_stream(
    State(state): State<WebState>,
    Query(query): Query<StreamQuery>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let (mut rx, replay, done, replay_truncated, oldest_event_id, _) = match state
        .run_hub
        .subscribe_with_replay(&query.run_id, query.last_event_id, WEB_ACTOR, false)
        .await
    {
        Ok(value) => value,
        Err(RunLookupError::NotFound) => {
            return Err((StatusCode::NOT_FOUND, "run not found".into()));
        }
        Err(RunLookupError::Forbidden) => return Err((StatusCode::FORBIDDEN, "forbidden".into())),
    };

    let stream = async_stream::stream! {
        let meta = Event::default().event("replay_meta").data(
            serde_json::to_string(&ReplayMetaPayload {
                replay_truncated,
                oldest_event_id,
                requested_last_event_id: query.last_event_id,
            })
            .unwrap_or_default(),
        );
        yield Ok::<Event, std::convert::Infallible>(meta);

        let mut finished = false;
        for evt in replay {
            let is_done = evt.terminal;
            let event = Event::default()
                .id(evt.id.to_string())
                .event(evt.event)
                .data(evt.data);
            yield Ok::<Event, std::convert::Infallible>(event);
            if is_done {
                finished = true;
                break;
            }
        }

        if finished || done {
            return;
        }

        loop {
            match rx.recv().await {
                Ok(evt) => {
                    let is_done = evt.terminal;
                    let event = Event::default()
                        .id(evt.id.to_string())
                        .event(evt.event)
                        .data(evt.data);
                    yield Ok::<Event, std::convert::Infallible>(event);
                    if is_done {
                        break;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    };

    Ok(Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(std::time::Duration::from_secs(15))
            .text("keepalive"),
    ))
}

/// Resolves (or creates) the web chat for a fresh session key and returns the
/// canonical `chat:{id}` session key together with the surface context.
///
/// New web sessions are addressed by `chat:{id}` from the moment they are sent
/// so the WebUI can adopt the persisted key immediately and reload history.
async fn resolve_new_web_session(
    state: &WebState,
    raw_session_key: &str,
    agent_id: Option<&str>,
    actor: &str,
) -> Result<(String, SurfaceContext), (StatusCode, String)> {
    let context = web_context_for_session(state, raw_session_key, agent_id, actor)?;
    let chat_id = resolve_chat_id(&state.app_state.turn_dependencies(), &context)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok((format!("chat:{chat_id}"), context))
}

fn web_context_for_session(
    state: &WebState,
    raw_session_key: &str,
    requested_agent_id: Option<&str>,
    actor: &str,
) -> Result<SurfaceContext, (StatusCode, String)> {
    let snapshot = state.app_state.config_manager.current_blocking();
    let raw_agent_id = requested_agent_id
        .filter(|agent_id| !agent_id.trim().is_empty())
        .ok_or_else(|| {
            (
                StatusCode::BAD_REQUEST,
                "agent_id is required for a new web session".to_string(),
            )
        })?;
    let agent_id = AgentId::new(raw_agent_id);
    if !snapshot.config.agents.contains_key(&agent_id) {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("unknown agent: {agent_id}"),
        ));
    }

    Ok(SurfaceContext::new(
        "web".to_string(),
        actor.to_string(),
        web_session_key(raw_session_key),
        "web".to_string(),
        agent_id.to_string(),
    ))
}

pub(super) async fn resolve_send_request(
    state: &WebState,
    request: &SendRequest,
    actor: &str,
) -> Result<ResolvedSend, (StatusCode, String)> {
    let message = request.message.trim().to_string();
    if message.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "message is required".to_string()));
    }

    let raw_session_key = request.session_key.as_deref().unwrap_or("main");
    let parsed_chat_id = parse_chat_id_from_session_key(raw_session_key);
    let (session_key, mut context) = if let Some(chat_id) = parsed_chat_id {
        let db = Arc::clone(&state.app_state.db);
        let chat_info = call_blocking(db, move |db| db.get_chat_by_id(chat_id))
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

        match chat_info {
            Some(info) => (
                format!("chat:{chat_id}"),
                surface_context_from_chat_info(info, actor),
            ),
            None => {
                resolve_new_web_session(state, raw_session_key, request.agent_id.as_deref(), actor)
                    .await?
            }
        }
    } else {
        resolve_new_web_session(state, raw_session_key, request.agent_id.as_deref(), actor).await?
    };

    context.request_key = parse_web_message_id(request.message_id.as_deref()).map_err(|()| {
        (
            StatusCode::BAD_REQUEST,
            "messageId is required in web:<uuid> format".to_string(),
        )
    })?;

    Ok(ResolvedSend {
        message,
        session_key,
        context,
    })
}

/// Validates a client-issued canonical user message id and returns it as the
/// Turn's request key. One accepted input is one message end to end, so the
/// id doubles as the idempotency key; no separate request id exists.
fn parse_web_message_id(raw: Option<&str>) -> Result<String, ()> {
    let id = raw.map(str::trim).filter(|id| !id.is_empty()).ok_or(())?;
    let uuid_part = id.strip_prefix("web:").ok_or(())?;
    Uuid::parse_str(uuid_part).map_err(|_| ())?;
    Ok(id.to_string())
}

fn web_surface_thread_from_external_chat_id(external_chat_id: &str, agent_id: &str) -> String {
    let thread = external_chat_id
        .strip_prefix("web:")
        .unwrap_or(external_chat_id);
    let agent_suffix = format!(":agent:{agent_id}");
    thread
        .strip_suffix(&agent_suffix)
        .unwrap_or(thread)
        .to_string()
}

fn surface_context_from_chat_info(info: crate::storage::ChatInfo, actor: &str) -> SurfaceContext {
    let surface_thread = if info.channel == "web" {
        web_surface_thread_from_external_chat_id(&info.external_chat_id, &info.agent_id)
    } else {
        info.external_chat_id
            .strip_prefix(&format!("{}:", info.channel))
            .unwrap_or(&info.external_chat_id)
            .to_string()
    };
    SurfaceContext::new(
        info.channel,
        actor.to_string(),
        surface_thread,
        info.chat_type,
        info.agent_id,
    )
}

pub(super) async fn publish_agent_event(state: &WebState, run_id: &str, event: AgentEvent) {
    let run_hub = &state.run_hub;
    match event {
        AgentEvent::Iteration { iteration } => {
            run_hub
                .publish(
                    run_id,
                    "status",
                    serde_json::to_string(&StatusPayload {
                        message: format!("iteration {iteration}"),
                    })
                    .unwrap_or_default(),
                )
                .await;
        }
        AgentEvent::Delta { message_id, text } => {
            run_hub
                .publish(
                    run_id,
                    "delta",
                    serde_json::to_string(&DeltaPayload {
                        message_id,
                        delta: text,
                    })
                    .unwrap_or_default(),
                )
                .await;
        }
        AgentEvent::ToolStart {
            call_id,
            name,
            input,
        } => {
            run_hub
                .publish(
                    run_id,
                    "tool_start",
                    serde_json::to_string(&ToolStartPayload {
                        call_id,
                        name,
                        input,
                    })
                    .unwrap_or_default(),
                )
                .await;
        }
        AgentEvent::ToolResult {
            call_id,
            name,
            is_error,
            preview,
            duration_ms,
        } => {
            run_hub
                .publish(
                    run_id,
                    "tool_result",
                    serde_json::to_string(&ToolResultPayload {
                        call_id,
                        name,
                        is_error,
                        preview,
                        duration_ms,
                    })
                    .unwrap_or_default(),
                )
                .await;
        }
        AgentEvent::UserInputInjected {
            message_id,
            sender_id,
            text,
            timestamp,
        } => {
            run_hub
                .publish(
                    run_id,
                    "user_input",
                    serde_json::to_string(&UserInputPayload {
                        message_id,
                        sender_id,
                        text,
                        timestamp,
                    })
                    .unwrap_or_default(),
                )
                .await;
        }
        AgentEvent::AssistantMessageDiscarded { message_id } => {
            run_hub
                .publish(
                    run_id,
                    "assistant_discarded",
                    serde_json::to_string(&AssistantDiscardedPayload { message_id })
                        .unwrap_or_default(),
                )
                .await;
        }
        AgentEvent::FinalResponse {
            turn_id: _,
            assistant_message_id,
            text,
            terminal,
        } => {
            // The final id is the stable id the response streamed under, so
            // delivery upserts by id and performs no lookup. Identity always
            // arrives decided: duplicate-delivery notices already carry their
            // origin-assigned notice id.
            run_hub
                .publish_agent_response(
                    run_id,
                    serde_json::to_string(&DonePayload {
                        response: text,
                        message_id: assistant_message_id,
                    })
                    .unwrap_or_default(),
                    terminal,
                )
                .await;
        }
        AgentEvent::Error {
            turn_id: _,
            message,
            terminal,
        } => {
            // Partial output keeps the id it streamed under; error delivery
            // resolves nothing.
            run_hub
                .publish_agent_error(
                    run_id,
                    serde_json::to_string(&ErrorPayload { error: message }).unwrap_or_default(),
                    terminal,
                )
                .await;
        }
    }
}

/// Accepts a Web input through the shared durable Turn boundary.
pub(super) async fn accept_web_input(
    state: WebState,
    request: SendRequest,
    actor: &str,
) -> Result<AcceptedWebInput, (StatusCode, String)> {
    let ResolvedSend {
        message,
        session_key,
        context,
    } = resolve_send_request(&state, &request, actor).await?;
    let chat_id = resolve_chat_id(&state.app_state.turn_dependencies(), &context)
        .await
        .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
    let _session_lock = state
        .app_state
        .turn_scheduler
        .lock_session(context.scope, chat_id)
        .await;

    if crate::slash_commands::is_slash_command(&message) {
        if session_has_unfinished_turn(&state.app_state, &context)
            .await
            .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?
        {
            return Err((
                StatusCode::TOO_MANY_REQUESTS,
                "another Turn is still running".to_string(),
            ));
        }
        return execute_web_slash_command(state, session_key, context, message, actor).await;
    }

    if let Some(turn_id) =
        try_stage_tool_followup_with_turn_id(&state.app_state, context.clone(), message.clone())
            .await
            .map_err(web_input_error)?
    {
        let run = load_turn_run(&state, context.scope, &turn_id).await?;
        let observer = (!run.state.is_terminal())
            .then(|| {
                state
                    .app_state
                    .turn_observers
                    .register_if_absent(run.request_key.clone())
            })
            .flatten();
        return accept_existing_web_run(&state, session_key, context.scope, run, observer, actor)
            .await;
    }

    let scope = context.scope;
    match submit_observed_agent_turn_with_identity(&state.app_state, context, message)
        .await
        .map_err(|reason| {
            let status = match reason {
                crate::runtime::turn::RejectReason::SessionQueueFull
                | crate::runtime::turn::RejectReason::GlobalQueueFull => {
                    StatusCode::TOO_MANY_REQUESTS
                }
                crate::runtime::turn::RejectReason::RequestConflict => StatusCode::CONFLICT,
                _ => StatusCode::INTERNAL_SERVER_ERROR,
            };
            (status, reason.message().to_string())
        })? {
        ObservedTurnSubmission::Created {
            observer,
            outcome,
            turn_id,
        } => {
            let status = if matches!(outcome, SubmitOutcome::Started) {
                "accepted"
            } else {
                "queued"
            };
            state
                .run_hub
                .create_if_absent(&turn_id, actor.to_string(), session_key.clone())
                .await;
            if let Some(observer) = observer {
                spawn_observed_run_publisher(state.clone(), observer, turn_id.clone());
            }
            Ok(AcceptedWebInput {
                started: StartedRun {
                    run_id: turn_id,
                    session_key,
                },
                status,
            })
        }
        ObservedTurnSubmission::Existing { run, observer } => {
            accept_existing_web_run(&state, session_key, scope, *run, observer, actor).await
        }
    }
}

fn web_input_error(error: EgoPulseError) -> (StatusCode, String) {
    let status = match &error {
        EgoPulseError::Storage(crate::error::StorageError::Conflict(_)) => StatusCode::CONFLICT,
        EgoPulseError::Storage(
            crate::error::StorageError::ToolFollowupSessionCapacityFull
            | crate::error::StorageError::ToolFollowupScopeCapacityFull,
        ) => StatusCode::TOO_MANY_REQUESTS,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    (status, error.to_string())
}

async fn load_turn_run(
    state: &WebState,
    scope: crate::conversation::ConversationScope,
    turn_id: &str,
) -> Result<TurnRun, (StatusCode, String)> {
    let turn_id = turn_id.to_string();
    call_blocking(state.app_state.db_for(scope), move |db| {
        db.get_turn_run(&turn_id)
    })
    .await
    .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))
}

async fn accept_existing_web_run(
    state: &WebState,
    session_key: String,
    scope: crate::conversation::ConversationScope,
    run: TurnRun,
    observer: Option<crate::runtime::turn::TurnObserver>,
    actor: &str,
) -> Result<AcceptedWebInput, (StatusCode, String)> {
    let status = if run.state.is_terminal() {
        publish_terminal_web_run(state, scope, &run, actor).await?
    } else {
        state
            .run_hub
            .create_if_absent(
                &run.turn_id,
                actor.to_string(),
                format!("chat:{}", run.chat_id),
            )
            .await;
        if let Some(observer) = observer {
            let latest = load_turn_run(state, scope, &run.turn_id).await?;
            if latest.state.is_terminal() {
                drop(observer);
                publish_terminal_web_run(state, scope, &latest, actor).await?
            } else {
                spawn_observed_run_publisher(state.clone(), observer, run.turn_id.clone());
                "queued"
            }
        } else {
            "queued"
        }
    };
    Ok(AcceptedWebInput {
        started: StartedRun {
            run_id: run.turn_id,
            session_key,
        },
        status,
    })
}

async fn publish_terminal_web_run(
    state: &WebState,
    scope: crate::conversation::ConversationScope,
    run: &TurnRun,
    actor: &str,
) -> Result<&'static str, (StatusCode, String)> {
    // The row in hand is the terminal Turn itself. The final id is the
    // stable id the response streamed under, so replaying it re-announces
    // the same message id; the input needs no echo because the client showed
    // it under its canonical id from the start.
    let (event, data, status) = match run.state {
        TurnRunState::Completed => {
            let message_id = run.final_message_id.clone().ok_or_else(|| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "completed turn has no final response".to_string(),
                )
            })?;
            let lookup_id = message_id.clone();
            let response = call_blocking(state.app_state.db_for(scope), move |db| {
                db.get_message_content(&lookup_id)
            })
            .await
            .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?
            .ok_or_else(|| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "completed turn final response is missing".to_string(),
                )
            })?;
            (
                "done",
                serde_json::to_string(&DonePayload {
                    response,
                    message_id,
                })
                .unwrap_or_default(),
                "completed",
            )
        }
        TurnRunState::Failed | TurnRunState::Cancelled | TurnRunState::Uncertain => (
            "error",
            serde_json::to_string(&ErrorPayload {
                error: run
                    .error_message
                    .clone()
                    .unwrap_or_else(|| format!("turn ended in state {}", run.state)),
            })
            .unwrap_or_default(),
            "failed",
        ),
        _ => {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                "turn is not terminal".to_string(),
            ));
        }
    };
    state
        .run_hub
        .publish_terminal_if_absent(
            &run.turn_id,
            actor.to_string(),
            format!("chat:{}", run.chat_id),
            event,
            data,
        )
        .await;
    state
        .run_hub
        .remove_later(run.turn_id.clone(), RUN_TTL_SECONDS)
        .await;
    Ok(status)
}

async fn execute_web_slash_command(
    state: WebState,
    session_key: String,
    context: SurfaceContext,
    message: String,
    actor: &str,
) -> Result<AcceptedWebInput, (StatusCode, String)> {
    let run_id = Uuid::new_v4().to_string();
    state
        .run_hub
        .create(&run_id, actor.to_string(), session_key.clone())
        .await;
    match crate::slash_commands::process_slash_command(
        &state.app_state,
        &context,
        &message,
        Some(actor),
    )
    .await
    {
        crate::slash_commands::SlashCommandOutcome::Respond(response) => {
            // Slash commands run no durable Turn, so the run-scoped stable
            // id stands in for a persisted final id. It never renames: the
            // same payload is replayed verbatim to resubscribers.
            let message_id = format!("web:slash:{run_id}");
            state
                .run_hub
                .publish(
                    &run_id,
                    "done",
                    serde_json::to_string(&DonePayload {
                        response,
                        message_id,
                    })
                    .unwrap_or_default(),
                )
                .await;
        }
        crate::slash_commands::SlashCommandOutcome::Error(error) => {
            state
                .run_hub
                .publish(
                    &run_id,
                    "error",
                    serde_json::to_string(&ErrorPayload { error }).unwrap_or_default(),
                )
                .await;
        }
        crate::slash_commands::SlashCommandOutcome::NotHandled => {}
    }
    state
        .run_hub
        .remove_later(run_id.clone(), RUN_TTL_SECONDS)
        .await;
    Ok(AcceptedWebInput {
        started: StartedRun {
            run_id,
            session_key,
        },
        status: "accepted",
    })
}

fn spawn_observed_run_publisher(
    state: WebState,
    observer: crate::runtime::turn::TurnObserver,
    run_id: String,
) {
    tokio::spawn(async move {
        let crate::runtime::turn::TurnObserver {
            mut events,
            mut completion,
        } = observer;
        loop {
            tokio::select! {
                event = events.recv() => {
                    let Some(event) = event else { break };
                    publish_agent_event(&state, &run_id, event).await;
                }
                _ = &mut completion => {
                    while let Ok(event) = events.try_recv() {
                        publish_agent_event(&state, &run_id, event).await;
                    }
                    break;
                }
            }
        }
        state.run_hub.remove_later(run_id, RUN_TTL_SECONDS).await;
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_web_state_with_agents(dir: &tempfile::TempDir) -> WebState {
        let state_root = dir.path().to_string_lossy().to_string();
        let mut config = crate::test_util::test_config(&state_root);
        config.agents.insert(
            crate::config::AgentId::new("ace"),
            crate::config::AgentConfig {
                label: "Ace".to_string(),
                ..Default::default()
            },
        );
        let app_state = crate::test_util::build_state_with_config(config, None, None, None, None);
        WebState {
            app_state: Arc::new(app_state),
            config_path: None,
            run_hub: super::super::RunHub::default(),
            active_ws_connections: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        }
    }

    #[tokio::test]
    async fn new_web_session_uses_requested_agent() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let state = test_web_state_with_agents(&dir);
        let request = SendRequest {
            session_key: Some("new-ace-session".to_string()),
            message: "hello".to_string(),
            agent_id: Some("ace".to_string()),
            message_id: Some("web:dddddddd-dddd-dddd-dddd-dddddddddddd".to_string()),
        };

        // Act
        let resolved = resolve_send_request(&state, &request, WEB_ACTOR)
            .await
            .expect("resolve request");

        // Assert
        assert_eq!(resolved.context.agent_id, "ace");
        assert_eq!(
            resolved.context.session_key(),
            "web:new-ace-session:agent:ace"
        );
        assert!(
            state
                .app_state
                .db
                .get_chat_by_channel_external_and_agent(
                    "web",
                    "web:new-ace-session:agent:ace",
                    "ace",
                )
                .expect("lookup created chat")
                .is_some()
        );
    }

    #[tokio::test]
    async fn new_web_session_requires_agent() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let state = test_web_state_with_agents(&dir);
        let request = SendRequest {
            session_key: Some("missing-agent-session".to_string()),
            message: "hello".to_string(),
            agent_id: None,
            message_id: Some("web:dddddddd-dddd-dddd-dddd-dddddddddddd".to_string()),
        };

        // Act
        let error = resolve_send_request(&state, &request, WEB_ACTOR)
            .await
            .expect_err("agent must be required");

        // Assert
        assert_eq!(error.0, StatusCode::BAD_REQUEST);
        assert!(error.1.contains("agent_id is required"));
        assert_eq!(
            state
                .app_state
                .db
                .list_sessions()
                .expect("list sessions")
                .len(),
            0
        );
    }

    #[tokio::test]
    async fn new_web_session_rejects_unknown_agent() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let state = test_web_state_with_agents(&dir);
        let request = SendRequest {
            session_key: Some("unknown-agent-session".to_string()),
            message: "hello".to_string(),
            agent_id: Some("missing".to_string()),
            message_id: Some("web:dddddddd-dddd-dddd-dddd-dddddddddddd".to_string()),
        };

        // Act
        let error = resolve_send_request(&state, &request, WEB_ACTOR)
            .await
            .expect_err("unknown agent must be rejected");

        // Assert
        assert_eq!(error.0, StatusCode::BAD_REQUEST);
        assert!(error.1.contains("unknown agent: missing"));
        assert_eq!(
            state
                .app_state
                .db
                .list_sessions()
                .expect("list sessions")
                .len(),
            0
        );
    }

    #[tokio::test]
    async fn existing_web_session_uses_persisted_agent() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let state = test_web_state_with_agents(&dir);
        let chat_id = state
            .app_state
            .db
            .resolve_or_create_chat_id("web", "web:existing-session:agent:ace", None, "web", "ace")
            .expect("create chat");
        let request = SendRequest {
            session_key: Some(format!("chat:{chat_id}")),
            message: "hello".to_string(),
            agent_id: Some("default".to_string()),
            message_id: Some("web:dddddddd-dddd-dddd-dddd-dddddddddddd".to_string()),
        };

        // Act
        let resolved = resolve_send_request(&state, &request, WEB_ACTOR)
            .await
            .expect("resolve request");

        // Assert
        assert_eq!(resolved.context.agent_id, "ace");
        assert_eq!(resolved.session_key, format!("chat:{chat_id}"));
    }

    #[test]
    fn stored_chat_context_preserves_stored_identity() {
        // Arrange
        let info = crate::storage::ChatInfo {
            chat_id: 42,
            channel: "web".to_string(),
            external_chat_id: "web:stored-thread".to_string(),
            chat_type: "dm".to_string(),
            agent_id: "non-default-agent".to_string(),
        };

        // Act
        let context = surface_context_from_chat_info(info, "web-user");

        // Assert
        assert_eq!(context.channel, "web");
        assert_eq!(context.surface_thread, "stored-thread");
        assert_eq!(context.chat_type, "dm");
        assert_eq!(context.agent_id, "non-default-agent");
    }

    #[test]
    fn agent_scoped_web_context_round_trips_to_same_session_key() {
        // Arrange
        let info = crate::storage::ChatInfo {
            chat_id: 42,
            channel: "web".to_string(),
            external_chat_id: "web:stored-thread:agent:non-default-agent".to_string(),
            chat_type: "web".to_string(),
            agent_id: "non-default-agent".to_string(),
        };

        // Act
        let context = surface_context_from_chat_info(info, "web-user");

        // Assert
        assert_eq!(context.surface_thread, "stored-thread");
        assert_eq!(
            context.session_key(),
            "web:stored-thread:agent:non-default-agent"
        );
    }

    #[test]
    fn stream_event_format_matches() {
        let done_json = serde_json::to_string(&DonePayload {
            response: "hello".to_string(),
            message_id: "turn:t1:assistant:2".to_string(),
        })
        .unwrap();
        let done_parsed: serde_json::Value = serde_json::from_str(&done_json).unwrap();
        assert_eq!(done_parsed["response"], "hello");
        assert_eq!(done_parsed["messageId"], "turn:t1:assistant:2");

        let error_json = serde_json::to_string(&ErrorPayload {
            error: "oops".to_string(),
        })
        .unwrap();
        let error_parsed: serde_json::Value = serde_json::from_str(&error_json).unwrap();
        assert_eq!(error_parsed["error"], "oops");

        let delta_json = serde_json::to_string(&DeltaPayload {
            message_id: "turn:t1:assistant:1".to_string(),
            delta: "hi".to_string(),
        })
        .unwrap();
        let delta_parsed: serde_json::Value = serde_json::from_str(&delta_json).unwrap();
        assert_eq!(delta_parsed["messageId"], "turn:t1:assistant:1");
        assert_eq!(delta_parsed["delta"], "hi");

        let status_json = serde_json::to_string(&StatusPayload {
            message: "running".to_string(),
        })
        .unwrap();
        let status_parsed: serde_json::Value = serde_json::from_str(&status_json).unwrap();
        assert_eq!(status_parsed["message"], "running");

        let tool_start_json = serde_json::to_string(&ToolStartPayload {
            call_id: "call_1".to_string(),
            name: "read".to_string(),
            input: serde_json::json!({"path": "a.txt"}),
        })
        .unwrap();
        let tool_start_parsed: serde_json::Value = serde_json::from_str(&tool_start_json).unwrap();
        assert_eq!(tool_start_parsed["callId"], "call_1");
        assert_eq!(tool_start_parsed["name"], "read");
        assert_eq!(tool_start_parsed["input"]["path"], "a.txt");
        assert!(tool_start_parsed.get("assistantMessageId").is_none());

        let tool_result_json = serde_json::to_string(&ToolResultPayload {
            call_id: "call_1".to_string(),
            name: "write".to_string(),
            is_error: false,
            preview: "done".to_string(),
            duration_ms: 123,
        })
        .unwrap();
        let tool_result_parsed: serde_json::Value =
            serde_json::from_str(&tool_result_json).unwrap();
        assert_eq!(tool_result_parsed["callId"], "call_1");
        assert_eq!(tool_result_parsed["name"], "write");
        assert_eq!(tool_result_parsed["isError"], false);
        assert_eq!(tool_result_parsed["durationMs"], 123);
        assert_eq!(tool_result_parsed["preview"], "done");

        let user_input_json = serde_json::to_string(&UserInputPayload {
            message_id: "web:11111111-1111-1111-1111-111111111111".to_string(),
            sender_id: "web-user".to_string(),
            text: "follow-up".to_string(),
            timestamp: "2026-09-12T00:00:00Z".to_string(),
        })
        .unwrap();
        let user_input_parsed: serde_json::Value = serde_json::from_str(&user_input_json).unwrap();
        assert_eq!(
            user_input_parsed["messageId"],
            "web:11111111-1111-1111-1111-111111111111"
        );
        assert!(user_input_parsed.get("requestId").is_none());

        let discarded_json = serde_json::to_string(&AssistantDiscardedPayload {
            message_id: "turn:t1:assistant:1".to_string(),
        })
        .unwrap();
        let discarded_parsed: serde_json::Value = serde_json::from_str(&discarded_json).unwrap();
        assert_eq!(discarded_parsed["messageId"], "turn:t1:assistant:1");
    }

    #[tokio::test]
    async fn stream_event_data_is_ws_compatible() {
        let hub = super::super::RunHub::default();
        hub.create("test-run", "test-actor".to_string(), "chat:1".to_string())
            .await;

        let done_data = serde_json::to_string(&DonePayload {
            response: "final".to_string(),
            message_id: "turn:t1:assistant:1".to_string(),
        })
        .unwrap();
        hub.publish("test-run", "done", done_data).await;

        let (_rx, replay, done, _, _, _) = hub
            .subscribe_with_replay("test-run", None, "test-actor", false)
            .await
            .unwrap();

        assert!(done);
        assert_eq!(replay.len(), 1);
        let event = &replay[0];
        assert_eq!(event.event, "done");

        let parsed: serde_json::Value = serde_json::from_str(&event.data).unwrap();
        assert_eq!(parsed["response"], "final");
        assert_eq!(parsed["messageId"], "turn:t1:assistant:1");

        let error_data = serde_json::to_string(&ErrorPayload {
            error: "fail".to_string(),
        })
        .unwrap();
        let parsed_error: serde_json::Value = serde_json::from_str(&error_data).unwrap();
        assert_eq!(parsed_error["error"], "fail");
    }

    #[tokio::test]
    async fn final_response_forwards_the_stable_final_id() {
        use super::AgentEvent;

        // Arrange: the final id is the stable id the response streamed
        // under, so delivery forwards it verbatim and consults no storage.
        let dir = tempfile::tempdir().expect("tempdir");
        let web_state = test_web_state_with_agents(&dir);
        let chat_id = web_state
            .app_state
            .db
            .resolve_or_create_chat_id("web", "web:ids", None, "web", "default")
            .expect("chat");
        web_state
            .run_hub
            .create("tool-run", "actor".to_string(), format!("chat:{chat_id}"))
            .await;

        // Act
        publish_agent_event(
            &web_state,
            "tool-run",
            AgentEvent::FinalResponse {
                turn_id: "turn-tool".to_string(),
                assistant_message_id: "turn:turn-tool:assistant:2".to_string(),
                text: "done".to_string(),
                terminal: true,
            },
        )
        .await;

        // Assert: the stable final id travels untouched.
        let (_rx, replay, _, _, _, _) = web_state
            .run_hub
            .subscribe_with_replay("tool-run", None, "actor", false)
            .await
            .expect("subscribe");
        let done = replay
            .iter()
            .find(|event| event.event == "done")
            .expect("done event");
        let data: serde_json::Value = serde_json::from_str(&done.data).expect("json");
        assert_eq!(data["response"], serde_json::json!("done"));
        assert_eq!(
            data["messageId"],
            serde_json::json!("turn:turn-tool:assistant:2")
        );
    }

    #[tokio::test]
    async fn staged_follow_up_done_carries_the_child_turn_final_id() {
        use super::AgentEvent;

        // Arrange: parent turn A finished, child turn B was promoted for the
        // follow-up. Both dones travel the same web run; each event carries
        // its own Turn's final id, so the parent's id can never leak into
        // the child's delivery.
        let dir = tempfile::tempdir().expect("tempdir");
        let web_state = test_web_state_with_agents(&dir);
        let chat_id = web_state
            .app_state
            .db
            .resolve_or_create_chat_id("web", "web:staged", None, "web", "default")
            .expect("chat");
        web_state
            .run_hub
            .create("shared-run", "actor".to_string(), format!("chat:{chat_id}"))
            .await;

        // Act
        publish_agent_event(
            &web_state,
            "shared-run",
            AgentEvent::FinalResponse {
                turn_id: "turn-child".to_string(),
                assistant_message_id: "turn:turn-child:assistant:1".to_string(),
                text: "continued".to_string(),
                terminal: true,
            },
        )
        .await;

        // Assert: the child's final id, never the parent's.
        let (_rx, replay, _, _, _, _) = web_state
            .run_hub
            .subscribe_with_replay("shared-run", None, "actor", false)
            .await
            .expect("subscribe");
        let done = replay
            .iter()
            .find(|event| event.event == "done")
            .expect("done event");
        let data: serde_json::Value = serde_json::from_str(&done.data).expect("json");
        assert_eq!(
            data["messageId"],
            serde_json::json!("turn:turn-child:assistant:1")
        );
    }

    #[tokio::test]
    async fn error_carries_no_message_identity() {
        use super::AgentEvent;

        // Arrange: a failed turn. Partial output keeps the id it streamed
        // under, so the error event resolves nothing and carries no ids.
        let dir = tempfile::tempdir().expect("tempdir");
        let web_state = test_web_state_with_agents(&dir);
        let chat_id = web_state
            .app_state
            .db
            .resolve_or_create_chat_id("web", "web:partial", None, "web", "default")
            .expect("chat");
        web_state
            .run_hub
            .create(
                "partial-run",
                "actor".to_string(),
                format!("chat:{chat_id}"),
            )
            .await;

        // Act
        publish_agent_event(
            &web_state,
            "partial-run",
            AgentEvent::Error {
                turn_id: "no-such-turn".to_string(),
                message: "upstream failed".to_string(),
                terminal: true,
            },
        )
        .await;

        // Assert: error text only — no adoption targets.
        let (_rx, replay, _, _, _, _) = web_state
            .run_hub
            .subscribe_with_replay("partial-run", None, "actor", false)
            .await
            .expect("subscribe");
        let error = replay
            .iter()
            .find(|event| event.event == "error")
            .expect("error event");
        let data: serde_json::Value = serde_json::from_str(&error.data).expect("json");
        assert_eq!(data["error"], serde_json::json!("upstream failed"));
        assert!(data.get("user_message_id").is_none());
        assert!(data.get("assistant_message_id").is_none());
        assert!(data.get("userMessageId").is_none());
        assert!(data.get("assistantMessageId").is_none());
    }

    #[tokio::test]
    async fn user_input_and_discard_events_carry_stable_ids() {
        use super::AgentEvent;

        // Arrange: a staged follow-up commit and a retry discard travel the
        // same run. Both name stable message ids, so the client upserts and
        // deletes by id.
        let dir = tempfile::tempdir().expect("tempdir");
        let web_state = test_web_state_with_agents(&dir);
        let chat_id = web_state
            .app_state
            .db
            .resolve_or_create_chat_id("web", "web:followup", None, "web", "default")
            .expect("chat");
        web_state
            .run_hub
            .create(
                "followup-run",
                "actor".to_string(),
                format!("chat:{chat_id}"),
            )
            .await;

        // Act
        publish_agent_event(
            &web_state,
            "followup-run",
            AgentEvent::UserInputInjected {
                message_id: "web:11111111-1111-1111-1111-111111111111".to_string(),
                sender_id: "web-user".to_string(),
                text: "and one more thing".to_string(),
                timestamp: "2026-09-12T00:00:00Z".to_string(),
            },
        )
        .await;
        publish_agent_event(
            &web_state,
            "followup-run",
            AgentEvent::AssistantMessageDiscarded {
                message_id: "turn:t1:assistant:1".to_string(),
            },
        )
        .await;

        // Assert
        let (_rx, replay, _, _, _, _) = web_state
            .run_hub
            .subscribe_with_replay("followup-run", None, "actor", false)
            .await
            .expect("subscribe");
        let input = replay
            .iter()
            .find(|event| event.event == "user_input")
            .expect("user_input event");
        let input_data: serde_json::Value = serde_json::from_str(&input.data).expect("json");
        assert_eq!(
            input_data["messageId"],
            serde_json::json!("web:11111111-1111-1111-1111-111111111111")
        );
        assert!(input_data.get("requestId").is_none());
        let discarded = replay
            .iter()
            .find(|event| event.event == "assistant_discarded")
            .expect("assistant_discarded event");
        let discarded_data: serde_json::Value =
            serde_json::from_str(&discarded.data).expect("json");
        assert_eq!(
            discarded_data["messageId"],
            serde_json::json!("turn:t1:assistant:1")
        );
    }

    #[tokio::test]
    async fn web_run_keeps_parent_error_nonterminal_for_staged_follow_up() {
        // Arrange
        let hub = super::super::RunHub::default();
        hub.create("shared-run", WEB_ACTOR.to_string(), "chat:2".to_string())
            .await;

        // Act
        hub.publish_agent_error(
            "shared-run",
            r#"{"error":"parent failed"}"#.to_string(),
            false,
        )
        .await;
        hub.publish(
            "shared-run",
            "user_input",
            r#"{"messageId":"follow-up","text":"continue"}"#.to_string(),
        )
        .await;
        hub.publish_agent_response(
            "shared-run",
            r#"{"response":"continued"}"#.to_string(),
            true,
        )
        .await;

        // Assert
        let (_rx, replay, done, _, _, _) = hub
            .subscribe_with_replay("shared-run", None, WEB_ACTOR, false)
            .await
            .expect("subscribe shared run");
        assert!(done);
        assert_eq!(
            replay
                .iter()
                .map(|event| event.event.as_str())
                .collect::<Vec<_>>(),
            vec!["error", "user_input", "done"]
        );
        assert!(!replay[0].terminal);
        assert!(replay[2].terminal);
    }

    #[tokio::test]
    async fn web_run_stays_open_until_all_follow_up_results_are_terminal() {
        // Arrange
        let hub = super::super::RunHub::default();
        hub.create(
            "multi-follow-up",
            WEB_ACTOR.to_string(),
            "chat:3".to_string(),
        )
        .await;

        // Act
        hub.publish_agent_response(
            "multi-follow-up",
            r#"{"response":"follow-up A"}"#.to_string(),
            false,
        )
        .await;
        hub.publish_agent_error(
            "multi-follow-up",
            r#"{"error":"follow-up A failed later"}"#.to_string(),
            false,
        )
        .await;
        hub.publish_agent_response(
            "multi-follow-up",
            r#"{"response":"follow-up B"}"#.to_string(),
            true,
        )
        .await;

        // Assert
        let (_rx, replay, done, _, _, _) = hub
            .subscribe_with_replay("multi-follow-up", None, WEB_ACTOR, false)
            .await
            .expect("subscribe multi-follow-up run");
        assert!(done);
        assert_eq!(replay.len(), 3);
        assert!(!replay[0].terminal);
        assert!(!replay[1].terminal);
        assert!(replay[2].terminal);
    }

    #[test]
    fn replay_meta_serializes_with_camel_case() {
        let meta = ReplayMetaPayload {
            replay_truncated: true,
            oldest_event_id: Some(5),
            requested_last_event_id: Some(3),
        };
        let json = serde_json::to_string(&meta).unwrap();
        assert!(json.contains("\"replayTruncated\":true"));
        assert!(json.contains("\"oldestEventId\":5"));
        assert!(json.contains("\"requestedLastEventId\":3"));
    }

    #[tokio::test]
    async fn web_inputs_share_fifo_scheduler_and_request_id_deduplication() {
        // Arrange: occupy one session's scheduler slot without involving a
        // WebSocket connection, so every following input must be durable and
        // queued.
        let dir = tempfile::tempdir().expect("tempdir");
        let state = test_web_state_with_agents(&dir);
        let active = resolve_send_request(
            &state,
            &SendRequest {
                session_key: Some("fifo-session".to_string()),
                message: "active".to_string(),
                agent_id: Some("default".to_string()),
                message_id: Some("web:11111111-1111-1111-1111-111111111111".to_string()),
            },
            WEB_ACTOR,
        )
        .await
        .expect("resolve active input");
        assert!(matches!(
            state
                .app_state
                .turn_scheduler
                .submit(crate::runtime::turn::ScheduledTurn {
                    turn_id: "active-turn".to_string(),
                    origin_id: "active-origin".to_string(),
                    context: active.context,
                    input: "active".to_string(),
                    config_snapshot: None,
                    received_at: None,
                    response_delivery: crate::runtime::turn::ResponseDelivery::Channel,
                }),
            crate::runtime::turn::ScheduleResult::Started(_)
        ));

        let first = accept_web_input(
            state.clone(),
            SendRequest {
                session_key: Some("fifo-session".to_string()),
                message: "first".to_string(),
                agent_id: Some("default".to_string()),
                message_id: Some("web:22222222-2222-2222-2222-222222222222".to_string()),
            },
            WEB_ACTOR,
        )
        .await
        .expect("accept first input");
        let duplicate = accept_web_input(
            state.clone(),
            SendRequest {
                session_key: Some("fifo-session".to_string()),
                message: "first".to_string(),
                agent_id: Some("default".to_string()),
                message_id: Some("web:22222222-2222-2222-2222-222222222222".to_string()),
            },
            WEB_ACTOR,
        )
        .await
        .expect("accept duplicate input");
        let second = accept_web_input(
            state.clone(),
            SendRequest {
                session_key: Some("fifo-session".to_string()),
                message: "second".to_string(),
                agent_id: Some("default".to_string()),
                message_id: Some("web:33333333-3333-3333-3333-333333333333".to_string()),
            },
            WEB_ACTOR,
        )
        .await
        .expect("accept second input");

        // Assert: duplicate delivery reuses the same durable run, while the
        // next request receives its own FIFO queue entry.
        assert_eq!(first.started.run_id, duplicate.started.run_id);
        assert_eq!(first.status, "queued");
        assert_eq!(second.status, "queued");
        assert_ne!(first.started.run_id, second.started.run_id);

        let conflict = accept_web_input(
            state.clone(),
            SendRequest {
                session_key: Some("fifo-session".to_string()),
                message: "different payload".to_string(),
                agent_id: Some("default".to_string()),
                message_id: Some("web:22222222-2222-2222-2222-222222222222".to_string()),
            },
            WEB_ACTOR,
        )
        .await
        .expect_err("request key collision must be rejected");
        assert_eq!(conflict.0, StatusCode::CONFLICT);

        let concurrent_request = SendRequest {
            session_key: Some("fifo-session".to_string()),
            message: "concurrent".to_string(),
            agent_id: Some("default".to_string()),
            message_id: Some("web:44444444-4444-4444-4444-444444444444".to_string()),
        };
        let (left, right) = tokio::join!(
            accept_web_input(state.clone(), concurrent_request.clone(), WEB_ACTOR),
            accept_web_input(state.clone(), concurrent_request, WEB_ACTOR),
        );
        let left = left.expect("accept concurrent left input");
        let right = right.expect("accept concurrent right input");

        // Concurrent re-delivery still creates one durable Turn and keeps one
        // live observer for the original request.
        assert_eq!(left.started.run_id, right.started.run_id);
        assert!(
            state
                .app_state
                .turn_observers
                .has_live_observer("web:44444444-4444-4444-4444-444444444444")
        );
        assert_eq!(state.app_state.db.count_durable_pending().unwrap(), 3);
    }

    #[tokio::test]
    async fn web_duplicate_of_completed_turn_replays_saved_response() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let state = test_web_state_with_agents(&dir);
        let resolved = resolve_send_request(
            &state,
            &SendRequest {
                session_key: Some("completed-session".to_string()),
                message: "hello".to_string(),
                agent_id: Some("default".to_string()),
                message_id: Some("web:55555555-5555-5555-5555-555555555555".to_string()),
            },
            WEB_ACTOR,
        )
        .await
        .expect("resolve completed request");
        let chat_id = resolve_chat_id(&state.app_state.turn_dependencies(), &resolved.context)
            .await
            .expect("resolve completed chat");
        let payload_hash =
            crate::runtime::turn::canonical_request_hash(&resolved.context, &resolved.message);
        let run_id = match state
            .app_state
            .db
            .accept_or_get_turn(crate::storage::AcceptTurnParams {
                chat_id,
                request_key: "web:55555555-5555-5555-5555-555555555555",
                config_revision: 1,
                config_fingerprint: Some("fingerprint"),
                request_payload_hash: &payload_hash,
                origin_id: None,
                scheduled_request_json: None,
            })
            .expect("accept completed request")
        {
            crate::storage::AcceptOutcome::Created(run) => run.turn_id,
            crate::storage::AcceptOutcome::Existing(_) => panic!("expected new turn"),
        };
        let final_message_id = "web:completed-response";
        let conn = state.app_state.db.get_conn().expect("database connection");
        conn.execute(
            "INSERT INTO messages
                 (id, chat_id, sender_id, content, sender_kind, timestamp,
                  message_kind, recipient_agent_id, seq, turn_id, parent_message_id)
             VALUES (?1, ?2, 'default', 'saved response', 'assistant', ?3,
                     'message', NULL, 0, ?4, NULL)",
            rusqlite::params![final_message_id, chat_id, "2026-09-09T00:00:00Z", &run_id],
        )
        .expect("insert final response");
        conn.execute(
            "UPDATE turn_runs SET state = 'model_completed' WHERE turn_id = ?1",
            rusqlite::params![&run_id],
        )
        .expect("mark model completed");
        state
            .app_state
            .db
            .complete_turn(&run_id, final_message_id)
            .expect("complete turn");

        // Act
        let replay = accept_web_input(
            state.clone(),
            SendRequest {
                session_key: Some("completed-session".to_string()),
                message: "hello".to_string(),
                agent_id: Some("default".to_string()),
                message_id: Some("web:55555555-5555-5555-5555-555555555555".to_string()),
            },
            WEB_ACTOR,
        )
        .await
        .expect("replay completed request");

        // Assert
        assert_eq!(replay.started.run_id, run_id);
        assert_eq!(replay.status, "completed");
        let (_rx, events, done, _, _, _) = state
            .run_hub
            .subscribe_with_replay(&run_id, None, WEB_ACTOR, false)
            .await
            .expect("subscribe replayed run");
        assert!(done);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event, "done");
        assert_eq!(
            events[0].data,
            r#"{"response":"saved response","messageId":"web:completed-response"}"#
        );
    }

    #[tokio::test]
    async fn existing_web_run_rechecks_terminal_state_after_observer_registration() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let state = test_web_state_with_agents(&dir);
        let resolved = resolve_send_request(
            &state,
            &SendRequest {
                session_key: Some("recovery-race-session".to_string()),
                message: "hello".to_string(),
                agent_id: Some("default".to_string()),
                message_id: Some("web:66666666-6666-6666-6666-666666666666".to_string()),
            },
            WEB_ACTOR,
        )
        .await
        .expect("resolve recovery request");
        let chat_id = resolve_chat_id(&state.app_state.turn_dependencies(), &resolved.context)
            .await
            .expect("resolve recovery chat");
        let payload_hash =
            crate::runtime::turn::canonical_request_hash(&resolved.context, &resolved.message);
        let run = match state
            .app_state
            .db
            .accept_or_get_turn(crate::storage::AcceptTurnParams {
                chat_id,
                request_key: "web:66666666-6666-6666-6666-666666666666",
                config_revision: 1,
                config_fingerprint: Some("fingerprint"),
                request_payload_hash: &payload_hash,
                origin_id: None,
                scheduled_request_json: None,
            })
            .expect("accept recovery request")
        {
            crate::storage::AcceptOutcome::Created(run) => run,
            crate::storage::AcceptOutcome::Existing(_) => panic!("expected new turn"),
        };
        let observer = state
            .app_state
            .turn_observers
            .register_if_absent(run.request_key.clone())
            .expect("register recovery observer");
        let final_message_id = "web:recovery-race-response";
        let conn = state.app_state.db.get_conn().expect("database connection");
        conn.execute(
            "INSERT INTO messages
                 (id, chat_id, sender_id, content, sender_kind, timestamp,
                  message_kind, recipient_agent_id, seq, turn_id, parent_message_id)
             VALUES (?1, ?2, 'default', 'saved after race', 'assistant', ?3,
                     'message', NULL, 0, ?4, NULL)",
            rusqlite::params![
                final_message_id,
                chat_id,
                "2026-09-09T00:00:00Z",
                &run.turn_id
            ],
        )
        .expect("insert recovery response");
        conn.execute(
            "UPDATE turn_runs SET state = 'model_completed' WHERE turn_id = ?1",
            rusqlite::params![&run.turn_id],
        )
        .expect("mark recovery turn model completed");
        state
            .app_state
            .db
            .complete_turn(&run.turn_id, final_message_id)
            .expect("complete recovery turn");

        // Act
        let replay = accept_existing_web_run(
            &state,
            resolved.session_key,
            resolved.context.scope,
            run,
            Some(observer),
            WEB_ACTOR,
        )
        .await
        .expect("replay terminal recovery turn");

        // Assert
        assert_eq!(replay.status, "completed");
        let (_rx, events, done, _, _, _) = state
            .run_hub
            .subscribe_with_replay(&replay.started.run_id, None, WEB_ACTOR, false)
            .await
            .expect("subscribe recovery replay");
        assert!(done);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event, "done");
        assert_eq!(
            events[0].data,
            r#"{"response":"saved after race","messageId":"web:recovery-race-response"}"#
        );
    }

    #[tokio::test]
    async fn web_tool_followups_use_parent_run_without_connection_state() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let state = test_web_state_with_agents(&dir);
        let chat_id = state
            .app_state
            .db
            .resolve_or_create_chat_id(
                "web",
                "web:tool-session:agent:default",
                None,
                "web",
                "default",
            )
            .expect("create chat");
        let turn_id = match state
            .app_state
            .db
            .accept_or_get_turn(crate::storage::AcceptTurnParams {
                chat_id,
                request_key: "tool-turn",
                config_revision: 1,
                config_fingerprint: Some("fingerprint"),
                request_payload_hash: "tool-turn-payload",
                origin_id: None,
                scheduled_request_json: None,
            })
            .expect("accept turn")
        {
            crate::storage::AcceptOutcome::Created(run) => run.turn_id,
            crate::storage::AcceptOutcome::Existing(_) => panic!("expected new turn"),
        };
        state
            .app_state
            .db
            .get_conn()
            .expect("connection")
            .execute(
                "UPDATE turn_runs SET state = 'tools_pending' WHERE turn_id = ?1",
                rusqlite::params![&turn_id],
            )
            .expect("seed tool phase");

        // Act: two independent Web entry points stage follow-ups.
        let rest = accept_web_input(
            state.clone(),
            SendRequest {
                session_key: Some("tool-session".to_string()),
                message: "from rest".to_string(),
                agent_id: Some("default".to_string()),
                message_id: Some("web:77777777-7777-7777-7777-777777777777".to_string()),
            },
            WEB_ACTOR,
        )
        .await
        .expect("accept REST follow-up");
        let websocket = accept_web_input(
            state.clone(),
            SendRequest {
                session_key: Some("tool-session".to_string()),
                message: "from websocket".to_string(),
                agent_id: Some("default".to_string()),
                message_id: Some("web:88888888-8888-8888-8888-888888888888".to_string()),
            },
            WEB_ACTOR,
        )
        .await
        .expect("accept WebSocket follow-up");

        // Assert
        assert_eq!(rest.started.run_id, turn_id);
        assert_eq!(websocket.started.run_id, turn_id);
        assert_eq!(rest.status, "queued");
        assert_eq!(websocket.status, "queued");
        let staged = state
            .app_state
            .db
            .list_staged_user_messages(&turn_id)
            .expect("staged follow-ups");
        assert_eq!(staged.len(), 2);
    }

    #[tokio::test]
    async fn web_slash_commands_use_durable_session_busy_boundary() {
        // Arrange: an accepted Turn is enough to make the session busy even
        // when the request comes from a different Web connection.
        let dir = tempfile::tempdir().expect("tempdir");
        let state = test_web_state_with_agents(&dir);
        let chat_id = state
            .app_state
            .db
            .resolve_or_create_chat_id(
                "web",
                "web:busy-session:agent:default",
                None,
                "web",
                "default",
            )
            .expect("create chat");
        state
            .app_state
            .db
            .accept_or_get_turn(crate::storage::AcceptTurnParams {
                chat_id,
                request_key: "busy-turn",
                config_revision: 1,
                config_fingerprint: Some("fingerprint"),
                request_payload_hash: "busy-turn-payload",
                origin_id: None,
                scheduled_request_json: None,
            })
            .expect("accept busy turn");

        // Act
        let busy = accept_web_input(
            state.clone(),
            SendRequest {
                session_key: Some("busy-session".to_string()),
                message: "/new".to_string(),
                agent_id: Some("default".to_string()),
                message_id: Some("web:99999999-9999-9999-9999-999999999999".to_string()),
            },
            WEB_ACTOR,
        )
        .await
        .expect_err("command must be rejected while the session is busy");
        let other_session = accept_web_input(
            state,
            SendRequest {
                session_key: Some("idle-session".to_string()),
                message: "/status".to_string(),
                agent_id: Some("default".to_string()),
                message_id: Some("web:aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa".to_string()),
            },
            WEB_ACTOR,
        )
        .await
        .expect("different session command");

        // Assert
        assert_eq!(busy.0, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(other_session.status, "accepted");
    }

    #[tokio::test]
    async fn web_slash_and_turn_admission_share_session_lock() {
        // Arrange: keep the scheduler slot occupied so the ordinary request
        // remains durably unfinished after admission.
        let dir = tempfile::tempdir().expect("tempdir");
        let state = test_web_state_with_agents(&dir);
        let resolved = resolve_send_request(
            &state,
            &SendRequest {
                session_key: Some("locked-session".to_string()),
                message: "queued".to_string(),
                agent_id: Some("default".to_string()),
                message_id: Some("web:bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb".to_string()),
            },
            WEB_ACTOR,
        )
        .await
        .expect("resolve locked session");
        let chat_id = resolve_chat_id(&state.app_state.turn_dependencies(), &resolved.context)
            .await
            .expect("resolve locked chat");
        assert!(matches!(
            state
                .app_state
                .turn_scheduler
                .submit(crate::runtime::turn::ScheduledTurn {
                    turn_id: "lock-holder".to_string(),
                    origin_id: "lock-holder-origin".to_string(),
                    context: resolved.context.clone(),
                    input: "lock holder".to_string(),
                    config_snapshot: None,
                    received_at: None,
                    response_delivery: crate::runtime::turn::ResponseDelivery::Channel,
                }),
            crate::runtime::turn::ScheduleResult::Started(_)
        ));
        let session_lock = state
            .app_state
            .turn_scheduler
            .lock_session(resolved.context.scope, chat_id)
            .await;

        // Act: Web ordinary input must wait for the same lock used by slash.
        let ordinary_state = state.clone();
        let ordinary = tokio::spawn(async move {
            accept_web_input(
                ordinary_state,
                SendRequest {
                    session_key: Some("locked-session".to_string()),
                    message: "queued".to_string(),
                    agent_id: Some("default".to_string()),
                    message_id: Some("web:bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb".to_string()),
                },
                WEB_ACTOR,
            )
            .await
        });
        tokio::task::yield_now().await;
        assert!(!ordinary.is_finished());
        drop(session_lock);
        let ordinary_result = ordinary.await.expect("ordinary admission task");
        assert_eq!(ordinary_result.expect("ordinary input").status, "queued");

        let slash = accept_web_input(
            state,
            SendRequest {
                session_key: Some("locked-session".to_string()),
                message: "/new".to_string(),
                agent_id: Some("default".to_string()),
                message_id: Some("web:cccccccc-cccc-cccc-cccc-cccccccccccc".to_string()),
            },
            WEB_ACTOR,
        )
        .await
        .expect_err("slash command must see the admitted Turn");

        // Assert
        assert_eq!(slash.0, StatusCode::TOO_MANY_REQUESTS);
    }

    #[tokio::test]
    async fn rest_send_stream_accepts_camel_case_message_id_json() {
        // Arrange: the exact JSON shape a REST client posts. Without the
        // serde rename, `messageId` would silently bind to nothing and every
        // REST send would fail intake with 400.
        let dir = tempfile::tempdir().expect("tempdir");
        let state = test_web_state_with_agents(&dir);
        let request: SendRequest = serde_json::from_str(
            r#"{"session_key":"json-session","message":"hello","agent_id":"default","messageId":"web:12345678-1234-1234-1234-123456789012"}"#,
        )
        .expect("deserialize send request");
        assert_eq!(
            request.message_id.as_deref(),
            Some("web:12345678-1234-1234-1234-123456789012")
        );

        // Act
        let accepted = accept_web_input(state.clone(), request, WEB_ACTOR)
            .await
            .expect("accept JSON send");

        // Assert: the canonical id flows verbatim into the durable Turn.
        assert_eq!(accepted.status, "accepted");
        let row_exists = call_blocking(Arc::clone(&state.app_state.db), move |db| {
            let count: i64 = db.get_conn()?.query_row(
                "SELECT COUNT(*) FROM turn_runs WHERE request_key = ?1",
                rusqlite::params!["web:12345678-1234-1234-1234-123456789012"],
                |row| row.get(0),
            )?;
            Ok::<bool, crate::error::StorageError>(count == 1)
        })
        .await
        .expect("durable row");
        assert!(row_exists, "one Turn owns the posted message id");
    }

    #[tokio::test]
    async fn web_send_requires_a_canonical_message_id() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let state = test_web_state_with_agents(&dir);
        let request = |message_id: Option<&str>| SendRequest {
            session_key: Some("validated-session".to_string()),
            message: "hello".to_string(),
            agent_id: Some("default".to_string()),
            message_id: message_id.map(str::to_string),
        };

        // Act: missing, unprefixed, and non-uuid ids are rejected; the
        // canonical id is used verbatim as the Turn's request key.
        for bad in [None, Some(""), Some("no-prefix"), Some("web:not-a-uuid")] {
            let error = resolve_send_request(&state, &request(bad), WEB_ACTOR)
                .await
                .expect_err("message id must be validated");
            assert_eq!(error.0, StatusCode::BAD_REQUEST);
            assert!(error.1.contains("messageId"), "unexpected: {}", error.1);
        }
        let resolved = resolve_send_request(
            &state,
            &request(Some("web:eeeeeeee-eeee-eeee-eeee-eeeeeeeeeeee")),
            WEB_ACTOR,
        )
        .await
        .expect("canonical id accepted");
        assert_eq!(
            resolved.context.request_key,
            "web:eeeeeeee-eeee-eeee-eeee-eeeeeeeeeeee"
        );
    }

    #[tokio::test]
    async fn web_slash_response_carries_a_stable_run_scoped_id() {
        // Arrange: a slash command runs no durable Turn, so its done names a
        // run-scoped stable id the client upserts like any assistant message.
        let dir = tempfile::tempdir().expect("tempdir");
        let state = test_web_state_with_agents(&dir);
        let accepted = accept_web_input(
            state.clone(),
            SendRequest {
                session_key: Some("slash-session".to_string()),
                message: "/status".to_string(),
                agent_id: Some("default".to_string()),
                message_id: Some("web:ffffffff-ffff-ffff-ffff-ffffffffffff".to_string()),
            },
            WEB_ACTOR,
        )
        .await
        .expect("accept slash command");
        assert_eq!(accepted.status, "accepted");

        // Act
        let (_rx, events, done, _, _, _) = state
            .run_hub
            .subscribe_with_replay(&accepted.started.run_id, None, WEB_ACTOR, false)
            .await
            .expect("subscribe slash run");

        // Assert: done names a stable id derived from the run, and replaying
        // the run re-announces the same id.
        assert!(done);
        let done_event = events
            .iter()
            .find(|event| event.event == "done")
            .expect("done event");
        let data: serde_json::Value = serde_json::from_str(&done_event.data).expect("json");
        let expected = format!("web:slash:{}", accepted.started.run_id);
        assert_eq!(data["messageId"], serde_json::json!(expected));
    }
}
