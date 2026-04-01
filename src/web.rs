use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Router, body::Body};

use crate::channel::ConversationKind;
use crate::channel_adapter::ChannelAdapter;
use crate::error::EgoPulseError;
use crate::runtime::AppState;

mod config;
mod health;
mod sessions;
pub mod sse;
mod stream;
mod ws;

include!(concat!(env!("OUT_DIR"), "/web_assets.rs"));

pub struct WebAdapter;

#[async_trait]
impl ChannelAdapter for WebAdapter {
    fn name(&self) -> &str {
        "web"
    }

    fn chat_type_routes(&self) -> Vec<(&str, ConversationKind)> {
        vec![("web", ConversationKind::Private)]
    }

    fn is_local_only(&self) -> bool {
        true
    }

    fn allows_cross_chat(&self) -> bool {
        false
    }

    async fn send_text(&self, _external_chat_id: &str, _text: &str) -> Result<(), String> {
        Ok(())
    }
}

#[derive(Clone)]
pub(crate) struct WebState {
    pub(crate) app_state: Arc<AppState>,
    pub(crate) config_path: Option<PathBuf>,
}

pub(crate) fn web_session_key(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return "main".to_string();
    }
    trimmed
        .strip_prefix("web:")
        .unwrap_or(trimmed)
        .trim()
        .to_string()
}

pub(crate) fn web_external_chat_id(session_key: &str) -> String {
    format!("web:{}", web_session_key(session_key))
}

pub(crate) fn web_asset_response(path: &str) -> Response {
    let Some(file) = WEB_ASSETS.get_file(path) else {
        return StatusCode::NOT_FOUND.into_response();
    };

    let mime = match path.rsplit('.').next() {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "application/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("png") => "image/png",
        Some("ico") => "image/x-icon",
        Some("svg") => "image/svg+xml",
        Some("json") => "application/json; charset=utf-8",
        _ => "application/octet-stream",
    };

    let mut response = Response::new(Body::from(file.contents().to_vec()));
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(mime));
    response
}

async fn index_html() -> impl IntoResponse {
    match WEB_ASSETS.get_file("index.html") {
        Some(file) => Html(String::from_utf8_lossy(file.contents()).into_owned()).into_response(),
        None => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

async fn web_asset(axum::extract::Path(path): axum::extract::Path<String>) -> impl IntoResponse {
    web_asset_response(&path)
}

pub async fn run_server(state: AppState, host: &str, port: u16) -> Result<(), EgoPulseError> {
    let addr: SocketAddr = format!("{host}:{port}").parse().map_err(|error| {
        EgoPulseError::Channel(crate::error::ChannelError::SendFailed(format!(
            "invalid address: {error}"
        )))
    })?;

    let listener = tokio::net::TcpListener::bind(addr).await.map_err(|error| {
        EgoPulseError::Channel(crate::error::ChannelError::SendFailed(format!(
            "failed to bind: {error}"
        )))
    })?;

    tracing::info!("EgoPulse server listening on http://{}", addr);

    let web_state = WebState {
        config_path: state.config_path.clone(),
        app_state: Arc::new(state),
    };

    let app = Router::new()
        .route("/", get(index_html))
        .route("/ws", get(ws::ws_handler))
        .route("/health", get(health::health))
        .route("/api/health", get(health::health))
        .route(
            "/api/config",
            get(config::api_get_config).put(config::api_put_config),
        )
        .route("/api/sessions", get(sessions::list_sessions))
        .route("/api/history", get(sessions::get_history))
        .route("/api/send_stream", post(stream::api_send_stream))
        .route("/assets/{*path}", get(web_asset))
        .route(
            "/favicon.ico",
            get(|| async { web_asset_response("favicon.ico") }),
        )
        .route(
            "/icon.png",
            get(|| async { web_asset_response("icon.png") }),
        )
        .fallback(get(index_html))
        .with_state(web_state);

    let shutdown_signal = async {
        let ctrl_c = tokio::signal::ctrl_c();

        #[cfg(unix)]
        let terminate = async {
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("Failed to install signal handler")
                .recv()
                .await;
        };

        #[cfg(not(unix))]
        let terminate = std::future::pending::<()>();

        tokio::select! {
            _ = ctrl_c => {},
            _ = terminate => {},
        }
    };

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal)
        .await
        .map_err(|error| {
            EgoPulseError::Channel(crate::error::ChannelError::SendFailed(format!(
                "server error: {error}"
            )))
        })?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn web_adapter_properties() {
        let adapter = WebAdapter;

        assert_eq!(adapter.name(), "web");
        assert!(adapter.is_local_only());
        assert!(!adapter.allows_cross_chat());
        assert_eq!(
            adapter.chat_type_routes(),
            vec![("web", ConversationKind::Private)]
        );
    }

    #[test]
    fn session_keys_are_normalized_for_web_ui() {
        assert_eq!(web_session_key("web:main"), "main");
        assert_eq!(web_session_key("main"), "main");
        assert_eq!(web_external_chat_id("main"), "web:main");
    }
}
