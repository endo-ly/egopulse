//! Chat API handlers with SSE streaming.

use axum::extract::State;
use axum::Json;
use axum::response::sse::{Event, Sse};
use futures_util::stream::{self, Stream};
use serde::Deserialize;
use std::convert::Infallible;

use crate::agent_loop::{process_turn, SurfaceContext};
use crate::runtime::AppState;
use crate::storage::call_blocking;

#[derive(Debug, Deserialize)]
pub struct SendRequest {
    pub session_key: Option<String>,
    pub sender_name: Option<String>,
    pub message: String,
}

/// Send a message and return SSE stream.
pub async fn send_stream(
    state: State<AppState>,
    Json(request): Json<SendRequest>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let session_key = request.session_key.unwrap_or_else(|| "main".to_string());
    let sender_name = request.sender_name.unwrap_or_else(|| "web-user".to_string());
    let message = request.message;

    // Get chat_id
    let db = state.db.clone();
    let session_key_for_resolve = session_key.clone();
    let chat_id = call_blocking(db, move |db| {
        db.resolve_or_create_chat_id("web", &session_key_for_resolve, Some(&session_key_for_resolve), "web")
    })
    .await
    .ok();

    let state_inner = state.0.clone();
    let stream = async_stream::stream! {
        yield Ok(Event::default().event("start").data(""));

        if let Some(_chat_id) = chat_id {
            let context = SurfaceContext {
                channel: "web".to_string(),
                surface_user: sender_name.clone(),
                surface_thread: session_key.clone(),
                chat_type: "web".to_string(),
            };

            // Process the turn
            match process_turn(
                &state_inner,
                &context,
                &message,
            ).await {
                Ok(response) => {
                    yield Ok(Event::default().event("done").data(&response));
                }
                Err(e) => {
                    yield Ok(Event::default().event("error").data(&e.to_string()));
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
