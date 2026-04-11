//! Web のストリーミング送信 API を提供するモジュール。
//!
//! チャット run の開始と SSE 購読を仲介し、RunHub と agent loop を接続する。

use axum::Json;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::response::sse::{Event, KeepAlive, Sse};
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use crate::agent_loop::{SurfaceContext, process_turn_with_events};

use super::sse::AgentEvent;
use super::{RUN_TTL_SECONDS, RunLookupError, WEB_ACTOR, WebState, web_session_key};
use super::sessions::parse_chat_id_from_session_key;
use crate::storage::call_blocking;

#[derive(Debug, Clone, Deserialize)]
/// Represents a chat message request sent from the web UI.
pub(super) struct SendRequest {
    pub session_key: Option<String>,
    pub message: String,
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

/// Starts a streaming run and returns its identifiers.
pub(super) async fn api_send_stream(
    State(state): State<WebState>,
    Json(request): Json<SendRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let started = start_stream_run(state, request, WEB_ACTOR).await?;

    Ok(Json(json!({
        "ok": true,
        "run_id": started.run_id,
        "session_key": started.session_key,
    })))
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
            json!({
                "replay_truncated": replay_truncated,
                "oldest_event_id": oldest_event_id,
                "requested_last_event_id": query.last_event_id,
            })
            .to_string()
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

    let (session_key, context) = if let Some(chat_id) = parsed_chat_id {
        let db = state.app_state.db.clone();
        let chat_info = call_blocking(db, move |db| db.get_chat_by_id(chat_id))
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

        match chat_info {
            Some(info) => {
                let surface_thread = info
                    .external_chat_id
                    .strip_prefix(&format!("{}:", info.channel))
                    .unwrap_or(&info.external_chat_id)
                    .to_string();
                (
                    format!("chat:{}", chat_id),
                    SurfaceContext {
                        channel: info.channel,
                        surface_user: actor.to_string(),
                        surface_thread,
                        chat_type: info.chat_type,
                    },
                )
            }
            None => {
                let key = web_session_key(raw_session_key);
                (
                    key.clone(),
                    SurfaceContext {
                        channel: "web".to_string(),
                        surface_user: actor.to_string(),
                        surface_thread: key,
                        chat_type: "web".to_string(),
                    },
                )
            }
        }
    } else {
        let key = web_session_key(raw_session_key);
        (
            key.clone(),
            SurfaceContext {
                channel: "web".to_string(),
                surface_user: actor.to_string(),
                surface_thread: key,
                chat_type: "web".to_string(),
            },
        )
    };

    let run_id = Uuid::new_v4().to_string();
    state.run_hub.create(&run_id, actor.to_string()).await;

    let state_for_task = state.clone();
    let run_id_for_task = run_id.clone();
    let context_for_task = context;
    tokio::spawn(async move {
        state_for_task
            .run_hub
            .publish(
                &run_id_for_task,
                "status",
                json!({"message": "running"}).to_string(),
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
                                json!({"message": format!("iteration {iteration}")}).to_string(),
                            )
                            .await;
                    }
                    AgentEvent::ToolStart { name, .. } => {
                        run_hub
                            .publish(
                                &run_id_for_events,
                                "tool_start",
                                json!({"name": name}).to_string(),
                            )
                            .await;
                    }
                    AgentEvent::ToolResult {
                        name,
                        is_error,
                        duration_ms,
                        ..
                    } => {
                        run_hub
                            .publish(
                                &run_id_for_events,
                                "tool_result",
                                json!({
                                    "name": name,
                                    "is_error": is_error,
                                    "duration_ms": duration_ms,
                                })
                                .to_string(),
                            )
                            .await;
                    }
                    AgentEvent::TextDelta { delta } => {
                        run_hub
                            .publish(
                                &run_id_for_events,
                                "delta",
                                json!({"delta": delta}).to_string(),
                            )
                            .await;
                    }
                    AgentEvent::FinalResponse { text } => {
                        run_hub
                            .publish(
                                &run_id_for_events,
                                "done",
                                json!({"response": text}).to_string(),
                            )
                            .await;
                    }
                    AgentEvent::Error { message } => {
                        run_hub
                            .publish(
                                &run_id_for_events,
                                "error",
                                json!({"error": message}).to_string(),
                            )
                            .await;
                    }
                }
            }
        });

        let result =
            process_turn_with_events(&state_for_task.app_state, &context_for_task, &message, |event| {
                let _ = evt_tx.send(event);
            })
            .await;

        if let Err(error) = result {
            let _ = evt_tx.send(super::sse::AgentEvent::Error {
                message: error.to_string(),
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
