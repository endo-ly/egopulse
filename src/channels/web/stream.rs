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

use crate::agent_loop::{SurfaceContext, process_turn_with_events, resolve_chat_id};
use tracing::error;

use super::sessions::parse_chat_id_from_session_key;
use super::sse::AgentEvent;
use super::{RUN_TTL_SECONDS, RunLookupError, WEB_ACTOR, WebState, web_session_key};
use crate::storage::call_blocking;

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

#[derive(Debug, Serialize)]
struct DonePayload {
    response: String,
}

#[derive(Debug, Serialize)]
struct DeltaPayload {
    delta: String,
}

#[derive(Debug, Serialize)]
struct ErrorPayload {
    error: String,
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
    /// Client-generated request id for deduplication. The same id re-delivered
    /// after a transient failure maps to the same Turn instead of a duplicate.
    pub request_id: Option<String>,
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

#[derive(Debug, Serialize)]
struct SendStreamResponse {
    ok: bool,
    run_id: String,
    session_key: String,
}

/// Starts a streaming run and returns its identifiers.
pub(super) async fn api_send_stream(
    State(state): State<WebState>,
    Json(request): Json<SendRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let started = start_stream_run(state, request, WEB_ACTOR).await?;

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
    let (mut rx, replay, done, replay_truncated, oldest_event_id) = match state
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
            let is_done = evt.event == "done" || evt.event == "error";
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
                    let is_done = evt.event == "done" || evt.event == "error";
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
    actor: &str,
) -> Result<(String, SurfaceContext), (StatusCode, String)> {
    let default_agent = state.app_state.config.default_agent.to_string();
    let surface_thread = web_session_key(raw_session_key);
    let context = SurfaceContext::new(
        "web".to_string(),
        actor.to_string(),
        surface_thread,
        "web".to_string(),
        default_agent,
    );
    let chat_id = resolve_chat_id(&state.app_state.turn_runtime(), &context)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok((format!("chat:{chat_id}"), context))
}

fn surface_context_from_chat_info(info: crate::storage::ChatInfo, actor: &str) -> SurfaceContext {
    let surface_thread = info
        .external_chat_id
        .strip_prefix(&format!("{}:", info.channel))
        .unwrap_or(&info.external_chat_id)
        .to_string();
    SurfaceContext::new(
        info.channel,
        actor.to_string(),
        surface_thread,
        info.chat_type,
        info.agent_id,
    )
}

/// Creates a run and spawns the background task that publishes its events.
pub(super) async fn start_stream_run(
    state: WebState,
    request: SendRequest,
    actor: &str,
) -> Result<StartedRun, (StatusCode, String)> {
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
            None => resolve_new_web_session(&state, raw_session_key, actor).await?,
        }
    } else {
        resolve_new_web_session(&state, raw_session_key, actor).await?
    };

    if let Some(id) = request
        .request_id
        .as_deref()
        .filter(|s| !s.trim().is_empty())
    {
        context.request_key = format!("web:{id}");
    }

    let run_id = Uuid::new_v4().to_string();
    state.run_hub.create(&run_id, actor.to_string()).await;

    match crate::slash_commands::process_slash_command(
        &state.app_state,
        &context,
        &message,
        Some(actor),
    )
    .await
    {
        crate::slash_commands::SlashCommandOutcome::Respond(response) => {
            state
                .run_hub
                .publish(
                    &run_id,
                    "done",
                    serde_json::to_string(&DonePayload { response }).unwrap_or_default(),
                )
                .await;
            state
                .run_hub
                .remove_later(run_id.clone(), RUN_TTL_SECONDS)
                .await;
            return Ok(StartedRun {
                run_id,
                session_key,
            });
        }
        crate::slash_commands::SlashCommandOutcome::Error(e) => {
            state
                .run_hub
                .publish(
                    &run_id,
                    "error",
                    serde_json::to_string(&ErrorPayload { error: e }).unwrap_or_default(),
                )
                .await;
            state
                .run_hub
                .remove_later(run_id.clone(), RUN_TTL_SECONDS)
                .await;
            return Ok(StartedRun {
                run_id,
                session_key,
            });
        }
        crate::slash_commands::SlashCommandOutcome::NotHandled => {}
    }

    let state_for_task = state.clone();
    let run_id_for_task = run_id.clone();
    let context_for_task = context;
    tokio::spawn(async move {
        state_for_task
            .run_hub
            .publish(
                &run_id_for_task,
                "status",
                serde_json::to_string(&StatusPayload {
                    message: "running".to_string(),
                })
                .unwrap_or_default(),
            )
            .await;

        let (evt_tx, mut evt_rx) = tokio::sync::mpsc::unbounded_channel::<AgentEvent>();
        let run_hub = state_for_task.run_hub.clone();
        let run_id_for_events = run_id_for_task.clone();
        let forward = tokio::spawn(async move {
            while let Some(event) = evt_rx.recv().await {
                match event {
                    AgentEvent::Iteration { iteration } => {
                        run_hub
                            .publish(
                                &run_id_for_events,
                                "status",
                                serde_json::to_string(&StatusPayload {
                                    message: format!("iteration {iteration}"),
                                })
                                .unwrap_or_default(),
                            )
                            .await;
                    }
                    AgentEvent::Delta { text } => {
                        run_hub
                            .publish(
                                &run_id_for_events,
                                "delta",
                                serde_json::to_string(&DeltaPayload { delta: text })
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
                                &run_id_for_events,
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
                                &run_id_for_events,
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
                    AgentEvent::FinalResponse { text } => {
                        run_hub
                            .publish(
                                &run_id_for_events,
                                "done",
                                serde_json::to_string(&DonePayload { response: text })
                                    .unwrap_or_default(),
                            )
                            .await;
                    }
                    AgentEvent::Error { message } => {
                        run_hub
                            .publish(
                                &run_id_for_events,
                                "error",
                                serde_json::to_string(&ErrorPayload { error: message })
                                    .unwrap_or_default(),
                            )
                            .await;
                    }
                }
            }
        });

        let evt_tx_clone = evt_tx.clone();
        let result = process_turn_with_events(
            &state_for_task.app_state.turn_runtime(),
            &context_for_task,
            &message,
            move |event| {
                let _ = evt_tx_clone.send(event);
            },
        )
        .await;

        if let Err(error) = result {
            error!(
                session = %context_for_task.surface_thread,
                error_kind = error.error_kind(),
                error = %error,
                error_debug = ?error,
                "Web: error processing message"
            );
            let _ = evt_tx.send(super::sse::AgentEvent::Error {
                message: error.user_message(),
            });
        }

        drop(evt_tx);
        let _ = forward.await;
        state_for_task
            .run_hub
            .remove_later(run_id_for_task, RUN_TTL_SECONDS)
            .await;
    });

    Ok(StartedRun {
        run_id,
        session_key,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn stream_event_format_matches() {
        let done_json = serde_json::to_string(&DonePayload {
            response: "hello".to_string(),
        })
        .unwrap();
        let done_parsed: serde_json::Value = serde_json::from_str(&done_json).unwrap();
        assert_eq!(done_parsed["response"], "hello");

        let error_json = serde_json::to_string(&ErrorPayload {
            error: "oops".to_string(),
        })
        .unwrap();
        let error_parsed: serde_json::Value = serde_json::from_str(&error_json).unwrap();
        assert_eq!(error_parsed["error"], "oops");

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
    }

    #[tokio::test]
    async fn stream_event_data_is_ws_compatible() {
        let hub = super::super::RunHub::default();
        hub.create("test-run", "test-actor".to_string()).await;

        let done_data = serde_json::to_string(&DonePayload {
            response: "final".to_string(),
        })
        .unwrap();
        hub.publish("test-run", "done", done_data).await;

        let (_rx, replay, done, _, _) = hub
            .subscribe_with_replay("test-run", None, "test-actor", false)
            .await
            .unwrap();

        assert!(done);
        assert_eq!(replay.len(), 1);
        let event = &replay[0];
        assert_eq!(event.event, "done");

        let parsed: serde_json::Value = serde_json::from_str(&event.data).unwrap();
        assert_eq!(parsed["response"], "final");

        let error_data = serde_json::to_string(&ErrorPayload {
            error: "fail".to_string(),
        })
        .unwrap();
        let parsed_error: serde_json::Value = serde_json::from_str(&error_data).unwrap();
        assert_eq!(parsed_error["error"], "fail");
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
}
