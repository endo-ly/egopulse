use async_trait::async_trait;
use axum::Json;
use axum::body::Body;
use axum::extract::State;
use axum::extract::rejection::JsonRejection;
use axum::http::{HeaderMap, Request, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use crate::agent_loop::SurfaceContext;
use crate::channels::adapter::{ChannelAdapter, ConversationKind};
use crate::runtime::AppState;

use super::web::WebState;

/// Channel adapter for inbound Voice API turns.
///
/// `VoiceAdapter` registers voice conversations with the channel registry. It
/// does not provide outbound delivery; responses are returned synchronously by
/// the Voice API handler.
pub(crate) struct VoiceAdapter;

#[async_trait]
impl ChannelAdapter for VoiceAdapter {
    /// Returns the stable channel name used for voice sessions.
    fn name(&self) -> &str {
        "voice"
    }

    /// Registers `voice` conversations as private, agent-scoped sessions.
    fn chat_type_routes(&self) -> Vec<(&str, ConversationKind)> {
        vec![("voice", ConversationKind::Private)]
    }

    /// Rejects outbound delivery because voice replies use the HTTP response.
    ///
    /// This method intentionally always returns
    /// `Err("outbound voice delivery is not supported")`.
    async fn send_text(&self, _external_chat_id: &str, _text: &str) -> Result<(), String> {
        Err("outbound voice delivery is not supported".to_string())
    }
}

/// JSON request accepted by `POST /api/voice/turn`.
#[derive(Debug, Deserialize)]
pub(crate) struct VoiceTurnRequest {
    /// Required STT result. Whitespace-only text is rejected.
    text: String,
    /// Optional voice surface; defaults to `channels.voice.default_surface`.
    /// Must be non-empty and must not contain `:`.
    surface: Option<String>,
    /// Optional session within the surface; defaults to
    /// `channels.voice.default_session`. Must not be empty or contain `:`.
    session_key: Option<String>,
    /// Optional stable speaker identifier; defaults to `voice-user`.
    /// Must not be empty or contain `:`.
    user_id: Option<String>,
    /// Optional trigger or transcription source; defaults to `unknown`.
    source: Option<String>,
    /// Optional target agent; defaults to the configured default agent.
    /// Must not be empty or contain `:`.
    agent_id: Option<String>,
}

/// Successful JSON response returned by `POST /api/voice/turn`.
#[derive(Debug, Serialize)]
pub(crate) struct VoiceTurnResponse {
    /// Always `true` for this success response type.
    ok: bool,
    /// Agent-generated reply. It may be empty when the runtime returns silence.
    response: String,
    /// Normalized session key used for persistence.
    session_key: String,
    /// Normalized voice surface used for persistence.
    surface: String,
    /// Stable `{surface}:{session_key}` conversation identifier.
    surface_thread: String,
    /// Agent that processed the turn.
    agent_id: String,
    /// UUID identifying this turn in logs and runtime telemetry.
    trace_id: String,
}

/// Authenticates Voice API requests using `Authorization: Bearer <token>`.
///
/// The middleware reads configuration from `State<WebState>`, inspects the
/// supplied `HeaderMap`, and forwards the original `Request<Body>` through
/// `Next::run` after a constant-time token comparison. It returns `404 Not
/// Found` when Voice authentication is disabled and `401 Unauthorized` when
/// the header is missing or invalid.
pub(crate) async fn require_voice_auth(
    State(state): State<WebState>,
    headers: HeaderMap,
    request: Request<Body>,
    next: Next,
) -> Response {
    let Some(expected) = state.app_state.config.voice_auth_token() else {
        return error(
            StatusCode::NOT_FOUND,
            "voice_channel_disabled",
            "voice channel is disabled",
        );
    };
    let candidate = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "));
    let authorized =
        candidate.is_some_and(|token| super::web::auth::constant_time_eq(token.trim(), expected));
    if !authorized {
        tracing::warn!("voice auth failed: invalid or missing token");
        return error(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "invalid voice auth token",
        );
    }
    next.run(request).await
}

/// Handles the asynchronous Voice API entry point.
///
/// `state` supplies the shared `WebState`; `request` is a JSON
/// [`VoiceTurnRequest`]. JSON extraction failures return `400 Bad Request` with
/// the `invalid_params` code and `JsonRejection::body_text()`. Valid input is
/// delegated to `process_request`, returning its JSON success response or
/// forwarding its error response unchanged. Processing records conversation
/// history and runtime telemetry through the agent runtime.
pub(crate) async fn turn(
    State(state): State<WebState>,
    request: Result<Json<VoiceTurnRequest>, JsonRejection>,
) -> Response {
    let Json(request) = match request {
        Ok(request) => request,
        Err(rejection) => {
            return error(
                StatusCode::BAD_REQUEST,
                "invalid_params",
                &rejection.body_text(),
            );
        }
    };
    match process_request(&state.app_state, request).await {
        Ok(response) => Json(response).into_response(),
        Err(response) => response,
    }
}

/// Validates and executes one normalized voice turn.
///
/// Surface and session defaults come from configuration, while `:` is rejected
/// in identity components because it separates the `{surface}:{session_key}`
/// thread identity. The configured surface allowlist is enforced before a
/// traced [`SurfaceContext`] is passed to
/// [`crate::runtime::execute_observed_turn`].
async fn process_request(
    state: &AppState,
    request: VoiceTurnRequest,
) -> Result<VoiceTurnResponse, Response> {
    let text = request.text.trim();
    if text.is_empty() {
        return Err(error(
            StatusCode::BAD_REQUEST,
            "invalid_params",
            "text is required",
        ));
    }
    let surface = normalized_component(
        request.surface.as_deref(),
        state.config.voice_default_surface(),
        "surface",
    )
    .map_err(validation_error_response)?;
    let session_key = normalized_component(
        request.session_key.as_deref(),
        state.config.voice_default_session(),
        "session_key",
    )
    .map_err(validation_error_response)?;
    let allowed = state.config.voice_allowed_surfaces();
    if !allowed.is_empty() && !allowed.iter().any(|candidate| candidate == &surface) {
        return Err(error(
            StatusCode::FORBIDDEN,
            "surface_not_allowed",
            "surface is not allowed",
        ));
    }
    let user_id = normalized_component(request.user_id.as_deref(), "voice-user", "user_id")
        .map_err(validation_error_response)?;
    let agent_id = normalized_component(
        request.agent_id.as_deref(),
        state.config.default_agent.as_str(),
        "agent_id",
    )
    .map_err(validation_error_response)?;
    let source = request.source.as_deref().unwrap_or("unknown").trim();
    let surface_thread = format!("{surface}:{session_key}");
    let trace_id = Uuid::new_v4().to_string();
    let mut context = SurfaceContext::new(
        "voice".to_string(),
        user_id,
        surface_thread.clone(),
        "voice".to_string(),
        agent_id.clone(),
    );
    context.trace_id = trace_id.clone();
    tracing::info!(
        channel = "voice",
        surface,
        session_key,
        surface_thread,
        source,
        agent_id,
        trace_id,
        "processing voice turn"
    );
    let response = crate::runtime::execute_observed_turn(state, &context, text)
        .await
        .map_err(|err| {
            tracing::error!(error = %err, trace_id, "voice turn failed");
            error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "turn_failed",
                "voice turn failed",
            )
        })?;

    Ok(VoiceTurnResponse {
        ok: true,
        response,
        session_key,
        surface,
        surface_thread,
        agent_id,
        trace_id,
    })
}

fn normalized_component(
    value: Option<&str>,
    fallback: &str,
    field: &str,
) -> Result<String, String> {
    let value = value.unwrap_or(fallback).trim();
    if value.is_empty() || value.contains(':') {
        return Err(format!(
            "{field} must be non-empty and must not contain ':'"
        ));
    }
    Ok(value.to_string())
}

fn validation_error_response(message: String) -> Response {
    error(StatusCode::BAD_REQUEST, "invalid_params", &message)
}

fn error(status: StatusCode, code: &str, message: &str) -> Response {
    (
        status,
        Json(json!({"ok": false, "error": code, "message": message})),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn voice_adapter_registers_private_voice_route() {
        let adapter = VoiceAdapter;
        assert_eq!(adapter.name(), "voice");
        assert_eq!(
            adapter.chat_type_routes(),
            vec![("voice", ConversationKind::Private)]
        );
    }

    #[test]
    fn voice_identity_rejects_delimiter() {
        assert!(normalized_component(Some("desk:mic"), "voice", "surface").is_err());
        assert_eq!(
            normalized_component(Some(" stackchan "), "voice", "surface").unwrap(),
            "stackchan"
        );
    }
}
