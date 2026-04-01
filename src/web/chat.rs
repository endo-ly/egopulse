//! Chat API handlers with SSE streaming.

use std::convert::Infallible;

use axum::Json;
use axum::extract::State;
use axum::response::sse::{Event, Sse};
use futures_util::stream::{self, Stream};
use serde::Deserialize;
use tokio::sync::mpsc::{self, UnboundedSender};
use tokio::task::{JoinError, JoinHandle};

use crate::agent_loop::{SurfaceContext, process_turn_with_events};
use crate::runtime::AppState;
use crate::storage::call_blocking;
use crate::web::sse::{AgentEvent, PublicAgentEvent};

#[derive(Debug, Deserialize)]
pub struct SendRequest {
    pub session_key: Option<String>,
    pub sender_name: Option<String>,
    pub message: String,
}

struct AbortOnDrop<T> {
    handle: Option<JoinHandle<T>>,
}

impl<T> AbortOnDrop<T> {
    fn new(handle: JoinHandle<T>) -> Self {
        Self {
            handle: Some(handle),
        }
    }

    async fn wait(mut self) -> Result<T, JoinError> {
        self.handle
            .take()
            .expect("join handle should be present")
            .await
    }
}

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        if let Some(handle) = &self.handle {
            handle.abort();
        }
    }
}

/// Convert AgentEvent to SSE Event.
fn event_to_sse(event: AgentEvent) -> Event {
    let public_event = PublicAgentEvent::from(event);
    let json = serde_json::to_string(&public_event).unwrap_or_default();
    match &public_event {
        PublicAgentEvent::Iteration { .. } => Event::default().event("iteration").data(&json),
        PublicAgentEvent::ToolStart { .. } => Event::default().event("tool_start").data(&json),
        PublicAgentEvent::ToolResult { .. } => Event::default().event("tool_result").data(&json),
        PublicAgentEvent::TextDelta { .. } => Event::default().event("text_delta").data(&json),
        PublicAgentEvent::FinalResponse { .. } => {
            Event::default().event("final_response").data(&json)
        }
        PublicAgentEvent::Error { .. } => Event::default().event("error").data(&json),
    }
}

fn forward_agent_event(tx: &UnboundedSender<AgentEvent>, event: AgentEvent) {
    if tx.send(event).is_err() {
        tracing::debug!("SSE receiver dropped; stop forwarding agent events");
    }
}

/// Send a message and return SSE stream with events.
pub async fn send_stream(
    state: State<AppState>,
    Json(request): Json<SendRequest>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let session_key = request.session_key.unwrap_or_else(|| "main".to_string());
    let sender_name = request
        .sender_name
        .unwrap_or_else(|| "web-user".to_string());
    let message = request.message;

    // Get chat_id
    let db = state.db.clone();
    let session_key_for_resolve = session_key.clone();
    let chat_id = match call_blocking(db, move |db| {
        db.resolve_or_create_chat_id(
            "web",
            &session_key_for_resolve,
            Some(&session_key_for_resolve),
            "web",
        )
    })
    .await
    {
        Ok(chat_id) => Some(chat_id),
        Err(error) => {
            tracing::warn!(session_key = %session_key, error = %error, "Failed to resolve web session");
            None
        }
    };

    let state_inner = state.0.clone();
    let (tx, mut rx) = mpsc::unbounded_channel::<AgentEvent>();

    let stream = async_stream::stream! {
        // Send initial start event
        yield Ok(Event::default().event("start").data(""));

        if let Some(_chat_id) = chat_id {
            let context = SurfaceContext {
                channel: "web".to_string(),
                surface_user: sender_name.clone(),
                surface_thread: session_key.clone(),
                chat_type: "web".to_string(),
            };

            // Clone state for the async task
            let state_for_task = state_inner.clone();
            let tx_for_task = tx.clone();
            drop(tx);

            let handle = AbortOnDrop::new(tokio::spawn(async move {
                process_turn_with_events(
                    &state_for_task,
                    &context,
                    &message,
                    move |event: AgentEvent| {
                        forward_agent_event(&tx_for_task, event);
                    },
                )
                .await
            }));

            // Forward events from channel to SSE stream
            while let Some(event) = rx.recv().await {
                yield Ok(event_to_sse(event));
            }

            // Wait for processing to complete
            match handle.wait().await {
                Ok(Ok(_)) => {},
                Ok(Err(e)) => {
                    yield Ok(Event::default().event("error").data(e.to_string()));
                }
                Err(e) => {
                    yield Ok(Event::default().event("error").data(e.to_string()));
                }
            }
        } else {
            yield Ok(Event::default().event("error").data("Failed to resolve session"));
        }
    };

    Sse::new(stream)
}

/// SSE stream endpoint (placeholder for reconnection support).
pub async fn stream() -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let stream = stream::empty();
    Sse::new(stream)
}
