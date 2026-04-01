use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use serde::Deserialize;

use crate::agent_loop::{SurfaceContext, process_turn};

use super::{WebState, web_session_key};

#[derive(Debug, Deserialize)]
pub(super) struct SendRequest {
    pub session_key: Option<String>,
    pub message: String,
}

pub(super) async fn api_send_stream(
    State(state): State<WebState>,
    Json(request): Json<SendRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let message = request.message.trim();
    if message.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "message is required".to_string()));
    }

    let session_key = web_session_key(request.session_key.as_deref().unwrap_or("main"));
    let context = SurfaceContext {
        channel: "web".to_string(),
        surface_user: "web-user".to_string(),
        surface_thread: session_key.clone(),
        chat_type: "web".to_string(),
    };

    let response = process_turn(&state.app_state, &context, message)
        .await
        .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;

    Ok(Json(serde_json::json!({
        "ok": true,
        "session_key": session_key,
        "response": response,
    })))
}
