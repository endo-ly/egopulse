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

#[derive(Debug, Clone, Deserialize)]
pub(super) struct SendRequest {
    pub session_key: Option<String>,
    pub message: String,
}

#[derive(Debug, Deserialize)]
pub(super) struct StreamQuery {
    pub run_id: String,
    pub last_event_id: Option<u64>,
}

#[derive(Debug, Clone)]
pub(super) struct StartedRun {
    pub run_id: String,
    pub session_key: String,
}

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

pub(super) async fn start_stream_run(
    state: WebState,
    request: SendRequest,
    actor: &str,
) -> Result<StartedRun, (StatusCode, String)> {
    let message = request.message.trim().to_string();
    if message.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "message is required".to_string()));
    }

    let session_key = web_session_key(request.session_key.as_deref().unwrap_or("main"));
    let run_id = Uuid::new_v4().to_string();
    state.run_hub.create(&run_id, actor.to_string()).await;

    let state_for_task = state.clone();
    let run_id_for_task = run_id.clone();
    let session_key_for_task = session_key.clone();
    let actor_for_task = actor.to_string();
    tokio::spawn(async move {
        state_for_task
            .run_hub
            .publish(
                &run_id_for_task,
                "status",
                json!({"message": "running"}).to_string(),
            )
            .await;

        let context = SurfaceContext {
            channel: "web".to_string(),
            surface_user: actor_for_task.clone(),
            surface_thread: session_key_for_task.clone(),
            chat_type: "web".to_string(),
        };

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
            process_turn_with_events(&state_for_task.app_state, &context, &message, |event| {
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
