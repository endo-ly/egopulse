use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde_json::json;

use crate::channels::web::WebState;
use crate::channels::web::auth::constant_time_eq;
use crate::config::WebhookReceiverId;

pub(super) const MAX_WEBHOOK_PAYLOAD_BYTES: usize = 64 * 1024;

pub(crate) async fn receive_webhook(
    State(state): State<WebState>,
    Path(raw_receiver_id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let receiver_id = WebhookReceiverId::new(&raw_receiver_id);

    let Some(receiver) = state.app_state.config.webhook_receivers().get(&receiver_id) else {
        return super::error::webhook_error(
            StatusCode::NOT_FOUND,
            "webhook_receiver_not_found",
            "receiver is not configured",
        );
    };

    let Some(expected_token) = receiver.token.as_ref().map(|rv| rv.value()) else {
        return super::error::webhook_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "webhook_not_configured",
            "receiver token is not configured",
        );
    };

    let Some(token) = extract_bearer_token(&headers) else {
        return super::error::webhook_error(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "missing or malformed authorization header",
        );
    };

    if !constant_time_eq(token, expected_token) {
        return super::error::webhook_error(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "invalid receiver token",
        );
    }

    if body.len() > MAX_WEBHOOK_PAYLOAD_BYTES {
        return super::error::webhook_error(
            StatusCode::PAYLOAD_TOO_LARGE,
            "payload_too_large",
            "payload exceeds 64KB limit",
        );
    }

    let target_channel = receiver.target.channel.as_str();
    if target_channel == "voice" || state.app_state.channels.get(target_channel).is_none() {
        return super::error::webhook_error(
            StatusCode::BAD_REQUEST,
            "invalid_target",
            "target channel is not active or is voice",
        );
    }

    let agent_id = receiver
        .target
        .agent
        .as_ref()
        .unwrap_or(&state.app_state.config.default_agent);
    if !state.app_state.config.agents.contains_key(agent_id) {
        return super::error::webhook_error(
            StatusCode::BAD_REQUEST,
            "invalid_target",
            "target agent is not configured",
        );
    }

    if target_channel != "web" && receiver.target.thread.trim().is_empty() {
        return super::error::webhook_error(
            StatusCode::BAD_REQUEST,
            "invalid_target",
            "target thread is required for non-web channels",
        );
    }

    (
        StatusCode::ACCEPTED,
        axum::Json(json!({
            "ok": true,
            "receiver": receiver_id.to_string(),
            "status": "accepted",
        })),
    )
        .into_response()
}

fn extract_bearer_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|raw| raw.strip_prefix("Bearer "))
        .map(str::trim)
        .filter(|token| !token.is_empty())
}
