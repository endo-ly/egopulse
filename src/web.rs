use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use axum::extract::OriginalUri;
use axum::http::{HeaderValue, StatusCode, Uri, header};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Router, body::Body};
use tokio::sync::{Mutex, broadcast};

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

pub(crate) const WEB_ACTOR: &str = "web-user";
pub(crate) const RUN_HISTORY_LIMIT: usize = 512;
pub(crate) const RUN_TTL_SECONDS: u64 = 300;

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
    pub(crate) run_hub: RunHub,
}

#[derive(Clone, Debug)]
pub(crate) struct RunEvent {
    pub(crate) id: u64,
    pub(crate) event: String,
    pub(crate) data: String,
}

#[derive(Clone, Default)]
pub(crate) struct RunHub {
    channels: Arc<Mutex<HashMap<String, RunChannel>>>,
}

#[derive(Clone)]
struct RunChannel {
    sender: broadcast::Sender<RunEvent>,
    history: VecDeque<RunEvent>,
    next_id: u64,
    done: bool,
    owner_actor: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RunLookupError {
    NotFound,
    Forbidden,
}

impl RunHub {
    pub(crate) async fn create(&self, run_id: &str, owner_actor: String) {
        let (tx, _) = broadcast::channel(512);
        let mut guard = self.channels.lock().await;
        guard.insert(
            run_id.to_string(),
            RunChannel {
                sender: tx,
                history: VecDeque::new(),
                next_id: 1,
                done: false,
                owner_actor,
            },
        );
    }

    pub(crate) async fn publish(
        &self,
        run_id: &str,
        event: &str,
        data: String,
        history_limit: usize,
    ) {
        let mut guard = self.channels.lock().await;
        let Some(channel) = guard.get_mut(run_id) else {
            return;
        };

        let evt = RunEvent {
            id: channel.next_id,
            event: event.to_string(),
            data,
        };
        channel.next_id = channel.next_id.saturating_add(1);
        if channel.history.len() >= history_limit {
            let _ = channel.history.pop_front();
        }
        channel.history.push_back(evt.clone());
        if evt.event == "done" || evt.event == "error" {
            channel.done = true;
        }
        let _ = channel.sender.send(evt);
    }

    pub(crate) async fn subscribe_with_replay(
        &self,
        run_id: &str,
        last_event_id: Option<u64>,
        requester_actor: &str,
        is_admin: bool,
    ) -> Result<
        (
            broadcast::Receiver<RunEvent>,
            Vec<RunEvent>,
            bool,
            bool,
            Option<u64>,
        ),
        RunLookupError,
    > {
        let guard = self.channels.lock().await;
        let Some(channel) = guard.get(run_id) else {
            return Err(RunLookupError::NotFound);
        };
        if !is_admin && channel.owner_actor != requester_actor {
            return Err(RunLookupError::Forbidden);
        }
        let oldest_event_id = channel.history.front().map(|event| event.id);
        let replay_truncated = matches!(
            (last_event_id, oldest_event_id),
            (Some(last), Some(oldest)) if last.saturating_add(1) < oldest
        );
        let replay = channel
            .history
            .iter()
            .filter(|event| last_event_id.is_none_or(|id| event.id > id))
            .cloned()
            .collect::<Vec<_>>();
        Ok((
            channel.sender.subscribe(),
            replay,
            channel.done,
            replay_truncated,
            oldest_event_id,
        ))
    }

    pub(crate) async fn remove_later(&self, run_id: String, after_seconds: u64) {
        let channels = self.channels.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(after_seconds)).await;
            let mut guard = channels.lock().await;
            guard.remove(&run_id);
        });
    }
}

pub(crate) fn web_session_key(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return "main".to_string();
    }
    let stripped = trimmed
        .strip_prefix("web:")
        .unwrap_or(trimmed)
        .trim();
    if stripped.is_empty() {
        return "main".to_string();
    }
    stripped.to_string()
}

pub(crate) fn web_external_chat_id(session_key: &str) -> String {
    format!("web:{}", web_session_key(session_key))
}

pub(crate) fn web_asset_response(path: &str) -> Response {
    let normalized = path.trim_start_matches('/');
    let candidates = [
        normalized.to_string(),
        format!("assets/{normalized}"),
        normalized
            .strip_prefix("assets/")
            .unwrap_or(normalized)
            .to_string(),
    ];

    let file = candidates
        .iter()
        .find_map(|candidate| WEB_ASSETS.get_file(candidate));
    let Some(file) = file else {
        return StatusCode::NOT_FOUND.into_response();
    };

    let mime = match normalized.rsplit('.').next() {
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

async fn index_or_asset(OriginalUri(uri): OriginalUri) -> impl IntoResponse {
    asset_or_index(&uri)
}

fn asset_or_index(uri: &Uri) -> Response {
    match uri.path() {
        "/favicon.ico" => web_asset_response("favicon.ico"),
        "/icon.png" => web_asset_response("icon.png"),
        path if path.starts_with("/assets/") => web_asset_response(path),
        _ => match WEB_ASSETS.get_file("index.html") {
            Some(file) => {
                Html(String::from_utf8_lossy(file.contents()).into_owned()).into_response()
            }
            None => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        },
    }
}

pub async fn run_server(state: AppState, host: &str, port: u16) -> Result<(), EgoPulseError> {
    let mut addrs = tokio::net::lookup_host((host, port))
        .await
        .map_err(|error| {
            EgoPulseError::Channel(crate::error::ChannelError::SendFailed(format!(
                "failed to resolve address: {error}"
            )))
        })?;
    let addr = addrs.next().ok_or_else(|| {
        EgoPulseError::Channel(crate::error::ChannelError::SendFailed(format!(
            "no addresses resolved for {host}:{port}"
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
        run_hub: RunHub::default(),
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
        .route("/api/stream", get(stream::api_stream))
        .fallback(get(index_or_asset))
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
