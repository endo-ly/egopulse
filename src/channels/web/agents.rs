//! Agent listing and avatar API endpoints for the WebUI.

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderName, StatusCode, header};
use serde::Serialize;
use std::collections::HashMap;

use super::WebState;
use crate::config::AgentId;

type AvatarImageResponse = ([(HeaderName, String); 3], Vec<u8>);

#[derive(Debug, Serialize)]
pub(super) struct AgentInfo {
    id: String,
    label: String,
    is_default: bool,
    avatar_url: Option<String>,
}

/// Hard upload cap. The client resizes to a 256x256 WebP before upload, so
/// legitimate payloads stay far below this; the cap only guards abuse.
const MAX_AVATAR_BYTES: usize = 1024 * 1024;

const ALLOWED_AVATAR_CONTENT_TYPES: [&str; 3] = ["image/png", "image/jpeg", "image/webp"];

pub(super) async fn list_agents(
    State(state): State<WebState>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let snapshot = state.app_state.config_manager.current_blocking();
    let config = &snapshot.config;
    let default_agent = &config.default_agent;

    let avatar_versions: HashMap<String, String> = state
        .app_state
        .db
        .agent_avatar_versions()
        .map_err(|error| {
            tracing::warn!(%error, "failed to load agent avatar versions");
            internal_error()
        })?
        .into_iter()
        .collect();

    let mut agents: Vec<AgentInfo> = config
        .agents
        .iter()
        .map(|(id, agent_config)| AgentInfo {
            id: id.to_string(),
            label: agent_config.label.clone(),
            is_default: id == default_agent,
            avatar_url: avatar_versions
                .get(id.as_str())
                .map(|version| avatar_url(id.as_str(), version)),
        })
        .collect();
    agents.sort_by(|a, b| a.id.cmp(&b.id));

    Ok(Json(serde_json::json!({"ok": true, "agents": agents})))
}

/// Stores the avatar image uploaded from the user's device.
///
/// Returns `404` for unknown agents, `415` for unsupported content types,
/// `413` for oversized bodies, and `400` for empty bodies.
pub(super) async fn put_agent_avatar(
    State(state): State<WebState>,
    Path(agent_id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let agent_id = match resolved_agent_id(&state, &agent_id) {
        Some(id) => id,
        None => return Err(not_found()),
    };

    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(|value| {
            value
                .split(';')
                .next()
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase()
        })
        .unwrap_or_default();
    if !ALLOWED_AVATAR_CONTENT_TYPES.contains(&content_type.as_str()) {
        return Err((
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            Json(serde_json::json!({"ok": false, "error": "unsupported_media_type"})),
        ));
    }
    if body.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"ok": false, "error": "empty_body"})),
        ));
    }
    if body.len() > MAX_AVATAR_BYTES {
        return Err((
            StatusCode::PAYLOAD_TOO_LARGE,
            Json(serde_json::json!({"ok": false, "error": "payload_too_large"})),
        ));
    }

    let db = state.app_state.db.clone();
    let id = agent_id.as_str().to_string();
    let stored_type = content_type.clone();
    let version =
        tokio::task::spawn_blocking(move || db.upsert_agent_avatar(&id, &stored_type, &body))
            .await
            .map_err(|error| {
                tracing::warn!(%error, "avatar upload task panicked");
                internal_error()
            })?
            .map_err(|error| {
                tracing::warn!(%error, agent_id = %agent_id, "failed to store agent avatar");
                internal_error()
            })?;

    Ok(Json(serde_json::json!({
        "ok": true,
        "avatar_url": avatar_url(agent_id.as_str(), &version),
    })))
}

/// Serves the stored avatar image bytes.
///
/// Returns `404` when the agent is unknown or has no avatar set.
pub(super) async fn get_agent_avatar(
    State(state): State<WebState>,
    Path(agent_id): Path<String>,
) -> Result<AvatarImageResponse, (StatusCode, Json<serde_json::Value>)> {
    let agent_id = match resolved_agent_id(&state, &agent_id) {
        Some(id) => id,
        None => return Err(not_found()),
    };

    let db = state.app_state.db.clone();
    let id = agent_id.as_str().to_string();
    let avatar = tokio::task::spawn_blocking(move || db.get_agent_avatar(&id))
        .await
        .map_err(|error| {
            tracing::warn!(%error, "avatar fetch task panicked");
            internal_error()
        })?
        .map_err(|error| {
            tracing::warn!(%error, agent_id = %agent_id, "failed to load agent avatar");
            internal_error()
        })?
        .ok_or(not_found())?;

    Ok((
        [
            (header::CONTENT_TYPE, avatar.content_type),
            (header::ETAG, format!("\"{}\"", avatar.updated_at)),
            (header::CACHE_CONTROL, "private, max-age=3600".to_string()),
        ],
        avatar.image,
    ))
}

/// Removes the agent avatar so the UI falls back to the letter avatar.
///
/// Returns `404` for unknown agents.
pub(super) async fn delete_agent_avatar(
    State(state): State<WebState>,
    Path(agent_id): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let agent_id = match resolved_agent_id(&state, &agent_id) {
        Some(id) => id,
        None => return Err(not_found()),
    };

    let db = state.app_state.db.clone();
    let id = agent_id.as_str().to_string();
    tokio::task::spawn_blocking(move || db.delete_agent_avatar(&id))
        .await
        .map_err(|error| {
            tracing::warn!(%error, "avatar delete task panicked");
            internal_error()
        })?
        .map_err(|error| {
            tracing::warn!(%error, agent_id = %agent_id, "failed to delete agent avatar");
            internal_error()
        })?;

    Ok(Json(serde_json::json!({"ok": true})))
}

/// Normalizes `agent_id` and resolves it against the configured agents.
/// Unknown (or unsafe, e.g. path traversal) ids yield `None`.
fn resolved_agent_id(state: &WebState, agent_id: &str) -> Option<AgentId> {
    let agent_id = AgentId::new(agent_id);
    let snapshot = state.app_state.config_manager.current_blocking();
    snapshot
        .config
        .agents
        .contains_key(&agent_id)
        .then_some(agent_id)
}

fn avatar_url(agent_id: &str, version: &str) -> String {
    format!(
        "/api/agents/{agent_id}/avatar?v={}",
        percent_encode_rfc3339(version)
    )
}

/// Percent-encodes the two characters of an RFC 3339 timestamp that are
/// unsafe in a query component (`:` and `+`).
fn percent_encode_rfc3339(version: &str) -> String {
    let mut encoded = String::with_capacity(version.len() + 8);
    for byte in version.bytes() {
        match byte {
            b':' => encoded.push_str("%3A"),
            b'+' => encoded.push_str("%2B"),
            _ => encoded.push(byte as char),
        }
    }
    encoded
}

fn not_found() -> (StatusCode, Json<serde_json::Value>) {
    (
        StatusCode::NOT_FOUND,
        Json(serde_json::json!({"ok": false, "error": "agent_not_found"})),
    )
}

fn internal_error() -> (StatusCode, Json<serde_json::Value>) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(serde_json::json!({"ok": false, "error": "internal_error"})),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::State as AxumState;

    use crate::channels::web::{RunHub, WebState};
    use crate::error::LlmError;
    use crate::llm::{LlmProvider, Message, MessagesResponse};
    use crate::test_util::build_state_with_provider;
    use std::sync::Arc;

    struct DummyLlm;

    #[async_trait::async_trait]
    impl LlmProvider for DummyLlm {
        fn provider_name(&self) -> &str {
            "dummy"
        }

        fn model_name(&self) -> &str {
            "dummy"
        }

        async fn send_message(
            &self,
            _system: &str,
            _messages: Arc<Vec<Message>>,
            _tools: Option<Arc<Vec<crate::llm::ToolDefinition>>>,
        ) -> Result<MessagesResponse, LlmError> {
            panic!("handler tests should not call LLM")
        }

        async fn send_message_streaming(
            &self,
            system: &str,
            messages: Arc<Vec<Message>>,
            tools: Option<Arc<Vec<crate::llm::ToolDefinition>>>,
            on_delta: &(dyn Fn(String) + Send + Sync),
        ) -> Result<MessagesResponse, LlmError> {
            let _ = on_delta;
            self.send_message(system, messages, tools).await
        }
    }

    fn test_web_state(dir: &tempfile::TempDir) -> WebState {
        let state_root = dir.path().to_string_lossy().to_string();
        let app_state = build_state_with_provider(&state_root, Box::new(DummyLlm));
        WebState {
            app_state: Arc::new(app_state),
            config_path: None,
            run_hub: RunHub::default(),
            active_ws_connections: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        }
    }

    fn png_headers() -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(header::CONTENT_TYPE, "image/png".parse().expect("header"));
        headers
    }

    #[tokio::test]
    async fn api_agents_returns_configured_agents() {
        let dir = tempfile::tempdir().expect("tempdir");
        let web_state = test_web_state(&dir);

        let result = list_agents(AxumState(web_state)).await.expect("ok");
        let body = result.0;
        assert_eq!(body["ok"], serde_json::json!(true));

        let agents = body["agents"].as_array().expect("agents array");
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0]["id"], "default");
        assert_eq!(agents[0]["label"], "Default Agent");
        assert_eq!(agents[0]["is_default"], true);
        assert_eq!(agents[0]["avatar_url"], serde_json::Value::Null);
    }

    #[tokio::test]
    async fn put_then_get_avatar_roundtrips_image_and_url() {
        let dir = tempfile::tempdir().expect("tempdir");
        let web_state = test_web_state(&dir);

        let put = put_agent_avatar(
            AxumState(web_state.clone()),
            Path("default".to_string()),
            png_headers(),
            Bytes::from_static(b"fake-png-bytes"),
        )
        .await
        .expect("put ok");
        assert_eq!(put.0["ok"], serde_json::json!(true));
        let url = put.0["avatar_url"].as_str().expect("avatar_url");
        assert!(url.starts_with("/api/agents/default/avatar?v="));

        let (headers, body) =
            get_agent_avatar(AxumState(web_state.clone()), Path("DEFAULT".to_string()))
                .await
                .expect("get ok");
        assert_eq!(headers[0].0, header::CONTENT_TYPE);
        assert_eq!(headers[0].1, "image/png");
        assert_eq!(headers[1].0, header::ETAG);
        assert_eq!(body, b"fake-png-bytes");

        let result = list_agents(AxumState(web_state)).await.expect("ok");
        let agents = result.0["agents"].as_array().expect("agents array");
        assert_eq!(
            agents[0]["avatar_url"].as_str().expect("avatar url"),
            url,
            "list avatar_url must match the upload response"
        );
    }

    #[tokio::test]
    async fn put_avatar_rejects_unknown_agent() {
        let dir = tempfile::tempdir().expect("tempdir");
        let web_state = test_web_state(&dir);

        let error = put_agent_avatar(
            AxumState(web_state),
            Path("../escape".to_string()),
            png_headers(),
            Bytes::from_static(b"x"),
        )
        .await
        .expect_err("unknown agent must fail");
        assert_eq!(error.0, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn put_avatar_rejects_unsupported_content_type() {
        let dir = tempfile::tempdir().expect("tempdir");
        let web_state = test_web_state(&dir);

        let mut headers = HeaderMap::new();
        headers.insert(header::CONTENT_TYPE, "image/gif".parse().expect("header"));
        let error = put_agent_avatar(
            AxumState(web_state),
            Path("default".to_string()),
            headers,
            Bytes::from_static(b"x"),
        )
        .await
        .expect_err("unsupported type must fail");
        assert_eq!(error.0, StatusCode::UNSUPPORTED_MEDIA_TYPE);
    }

    #[tokio::test]
    async fn put_avatar_rejects_oversized_body() {
        let dir = tempfile::tempdir().expect("tempdir");
        let web_state = test_web_state(&dir);

        let error = put_agent_avatar(
            AxumState(web_state),
            Path("default".to_string()),
            png_headers(),
            Bytes::from(vec![0u8; MAX_AVATAR_BYTES + 1]),
        )
        .await
        .expect_err("oversized body must fail");
        assert_eq!(error.0, StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[tokio::test]
    async fn put_avatar_rejects_empty_body() {
        let dir = tempfile::tempdir().expect("tempdir");
        let web_state = test_web_state(&dir);

        let error = put_agent_avatar(
            AxumState(web_state),
            Path("default".to_string()),
            png_headers(),
            Bytes::new(),
        )
        .await
        .expect_err("empty body must fail");
        assert_eq!(error.0, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn get_avatar_returns_404_when_unset() {
        let dir = tempfile::tempdir().expect("tempdir");
        let web_state = test_web_state(&dir);

        let error = get_agent_avatar(AxumState(web_state), Path("default".to_string()))
            .await
            .expect_err("unset avatar must 404");
        assert_eq!(error.0, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn delete_avatar_removes_image_and_allows_reupload() {
        let dir = tempfile::tempdir().expect("tempdir");
        let web_state = test_web_state(&dir);

        let _ = put_agent_avatar(
            AxumState(web_state.clone()),
            Path("default".to_string()),
            png_headers(),
            Bytes::from_static(b"fake-png-bytes"),
        )
        .await
        .expect("put ok");

        let deleted =
            delete_agent_avatar(AxumState(web_state.clone()), Path("default".to_string()))
                .await
                .expect("delete ok");
        assert_eq!(deleted.0["ok"], serde_json::json!(true));

        let error = get_agent_avatar(AxumState(web_state.clone()), Path("default".to_string()))
            .await
            .expect_err("avatar must be gone");
        assert_eq!(error.0, StatusCode::NOT_FOUND);

        // Re-upload after delete works (row was fully removed, not tombstoned).
        let _ = put_agent_avatar(
            AxumState(web_state.clone()),
            Path("default".to_string()),
            png_headers(),
            Bytes::from_static(b"second"),
        )
        .await
        .expect("re-put ok");

        let error = delete_agent_avatar(AxumState(web_state), Path("unknown".to_string()))
            .await
            .expect_err("unknown agent must fail");
        assert_eq!(error.0, StatusCode::NOT_FOUND);
    }
}
