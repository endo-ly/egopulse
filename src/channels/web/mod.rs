//! Web UI サーバーと共有状態を提供するモジュール。
//!
//! HTTP API、静的アセット配信、SSE / WebSocket 用の実行状態を束ねる。

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::time::Duration;

use async_trait::async_trait;
use axum::extract::OriginalUri;
use axum::http::{HeaderValue, Method, StatusCode, Uri, header};
use axum::middleware;
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Router, body::Body};
use tokio::sync::{Mutex, broadcast};

use crate::channels::adapter::ChannelAdapter;
use crate::channels::adapter::ConversationKind;
use crate::error::EgoPulseError;
use crate::runtime::AppState;

mod agents;
pub(crate) mod auth;
mod config;
pub(crate) mod health;
mod sessions;
mod sleep;
pub(crate) mod sse;
mod stream;
mod ws;

include!(concat!(env!("OUT_DIR"), "/web_assets.rs"));

/// Identifies messages initiated from the web surface.
pub(crate) const WEB_ACTOR: &str = "web-user";
/// Caps the number of events retained per streaming run.
pub(crate) const RUN_HISTORY_LIMIT: usize = 512;
/// Defines how long completed runs remain replayable.
pub(crate) const RUN_TTL_SECONDS: u64 = 300;

/// Adapts the local web surface to the shared channel interface.
pub(crate) struct WebAdapter;

#[async_trait]
impl ChannelAdapter for WebAdapter {
    fn name(&self) -> &str {
        "web"
    }

    fn chat_type_routes(&self) -> Vec<(&str, ConversationKind)> {
        vec![("web", ConversationKind::Private)]
    }

    async fn send_text(&self, _external_chat_id: &str, _text: &str) -> Result<(), String> {
        Ok(())
    }
}

#[derive(Clone)]
/// Holds shared state for the web server and its live transports.
pub(crate) struct WebState {
    pub(crate) app_state: Arc<AppState>,
    pub(crate) config_path: Option<PathBuf>,
    pub(crate) run_hub: RunHub,
    pub(crate) active_ws_connections: Arc<AtomicUsize>,
}

#[derive(Clone, Debug)]
/// Represents one replayable SSE event emitted for a run.
pub(crate) struct RunEvent {
    pub(crate) id: u64,
    pub(crate) event: String,
    pub(crate) data: String,
}

#[derive(Clone, Default)]
/// Tracks active streaming runs and their replay buffers.
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
/// Describes why a run subscription lookup failed.
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

    pub(crate) async fn publish(&self, run_id: &str, event: &str, data: String) {
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
        if channel.history.len() >= RUN_HISTORY_LIMIT {
            let _ = channel.history.pop_front();
        }
        channel.history.push_back(evt.clone());
        if evt.event == "done" || evt.event == "error" {
            channel.done = true;
        }
        let _ = channel.sender.send(evt);
    }

    /// Subscribes to a run and returns any replayable events after `last_event_id`.
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

/// Normalizes a raw web session identifier into its storage key.
pub(crate) fn web_session_key(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return "main".to_string();
    }
    let stripped = trimmed.strip_prefix("web:").unwrap_or(trimmed).trim();
    if stripped.is_empty() {
        return "main".to_string();
    }
    stripped.to_string()
}

/// Formats the external chat identifier used for persisted web sessions.
pub(crate) fn web_external_chat_id(session_key: &str) -> String {
    format!("web:{}", web_session_key(session_key))
}

/// Serves an embedded static asset response for the requested path.
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

async fn index_or_asset(OriginalUri(uri): OriginalUri, method: Method) -> impl IntoResponse {
    if method != Method::GET || uri.path().starts_with("/api/") {
        return StatusCode::NOT_FOUND.into_response();
    }
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

/// Starts the web server and mounts HTTP, SSE, and WebSocket routes.
pub(crate) async fn run_server(
    state: AppState,
    host: &str,
    port: u16,
) -> Result<(), EgoPulseError> {
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
        active_ws_connections: Arc::new(AtomicUsize::new(0)),
    };
    let app = build_router(web_state);

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

fn build_router(web_state: WebState) -> Router {
    let api_routes = Router::new()
        .route(
            "/api/config",
            get(config::api_get_config).put(config::api_put_config),
        )
        .route("/api/sessions", get(sessions::list_sessions))
        .route("/api/history", get(sessions::get_history))
        .route("/api/send_stream", post(stream::api_send_stream))
        .route("/api/stream", get(stream::api_stream))
        .route("/api/agents", get(agents::list_agents))
        .route("/api/sleep/runs", get(sleep::list_sleep_runs))
        .route("/api/sleep/runs/{run_id}", get(sleep::get_sleep_run_detail))
        .route_layer(middleware::from_fn_with_state(
            web_state.clone(),
            auth::require_http_auth,
        ));

    let voice_routes = web_state.app_state.config.voice_enabled().then(|| {
        Router::new()
            .route("/api/voice/turn", post(crate::channels::voice::turn))
            .route_layer(middleware::from_fn_with_state(
                web_state.clone(),
                crate::channels::voice::require_voice_auth,
            ))
    });

    let webhook_routes = Router::new().route(
        "/api/webhooks/{receiver_id}",
        post(crate::webhooks::handler::receive_webhook),
    );

    let mut app = Router::new()
        .route("/", get(index_html))
        .route("/ws", get(ws::ws_handler))
        .route("/health", get(health::health))
        .route("/telemetry", get(health::telemetry_handler))
        .merge(api_routes);
    if let Some(routes) = voice_routes {
        app = app.merge(routes);
    }
    app = app.merge(webhook_routes);
    app.fallback(index_or_asset).with_state(web_state)
}

#[cfg(test)]
mod tests {
    use axum::body::{Body, to_bytes};
    use axum::http::Request;
    use tower::ServiceExt;

    use super::*;
    use crate::agent_loop::turn::FakeProvider;
    use crate::channels::adapter::ChannelRegistry;
    use crate::config::secret_ref::ResolvedValue;
    use crate::config::{
        AgentId, ChannelConfig, ChannelName, WebhookReceiverConfig, WebhookReceiverId,
        WebhookTargetConfig, WebhooksConfig,
    };
    use crate::llm::MessagesResponse;
    use crate::test_util::{build_state_with_config, test_config};

    struct NoopChannelAdapter(&'static str);

    #[async_trait::async_trait]
    impl ChannelAdapter for NoopChannelAdapter {
        fn name(&self) -> &str {
            self.0
        }

        fn chat_type_routes(&self) -> Vec<(&str, ConversationKind)> {
            Vec::new()
        }

        async fn send_text(&self, _: &str, _: &str) -> Result<(), String> {
            Ok(())
        }
    }

    #[test]
    fn web_adapter_properties() {
        let adapter = WebAdapter;

        assert_eq!(adapter.name(), "web");
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

    #[test]
    fn session_key_edge_cases() {
        assert_eq!(web_session_key(""), "main");
        assert_eq!(web_session_key("   "), "main");
        assert_eq!(web_session_key("web:"), "main");
        assert_eq!(web_session_key("web:   "), "main");
        assert_eq!(web_session_key("  web:foo  "), "foo");
        assert_eq!(web_session_key("web:web:nested"), "web:nested");
    }

    fn voice_test_router(dir: &tempfile::TempDir) -> (Router, Arc<AppState>) {
        let mut config = test_config(dir.path().to_str().expect("state root"));
        config
            .channels
            .get_mut("web")
            .expect("web config")
            .auth_token = Some(crate::config::secret_ref::ResolvedValue::Literal(
            "web-secret".to_string(),
        ));
        config.channels.insert(
            ChannelName::new("voice"),
            ChannelConfig {
                enabled: Some(true),
                auth_token: Some(crate::config::secret_ref::ResolvedValue::Literal(
                    "voice-secret".to_string(),
                )),
                default_surface: Some("stackchan".to_string()),
                default_session: Some("main".to_string()),
                allowed_surfaces: Some(vec!["stackchan".to_string()]),
                ..Default::default()
            },
        );
        let provider = FakeProvider {
            responses: std::sync::Mutex::new(vec![MessagesResponse {
                content: "音声応答です".to_string(),
                reasoning_content: None,
                tool_calls: vec![],
                usage: None,
            }]),
        };
        let app_state = Arc::new(build_state_with_config(
            config,
            Some(Arc::new(provider)),
            None,
            None,
            None,
        ));
        let router = build_router(WebState {
            config_path: None,
            app_state: Arc::clone(&app_state),
            run_hub: RunHub::default(),
            active_ws_connections: Arc::new(AtomicUsize::new(0)),
        });
        (router, app_state)
    }

    fn web_only_test_router(dir: &tempfile::TempDir) -> Router {
        let config = test_config(dir.path().to_str().expect("state root"));
        let state = build_state_with_config(config, None, None, None, None);
        build_router(WebState {
            config_path: None,
            app_state: Arc::new(state),
            run_hub: RunHub::default(),
            active_ws_connections: Arc::new(AtomicUsize::new(0)),
        })
    }

    #[tokio::test]
    async fn voice_route_requires_voice_token() {
        let dir = tempfile::tempdir().expect("tempdir");
        let response = voice_test_router(&dir)
            .0
            .oneshot(
                Request::post("/api/voice/turn")
                    .header(header::AUTHORIZATION, "Bearer wrong-token")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"text":"こんにちは"}"#))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn disabled_voice_route_returns_not_found() {
        let dir = tempfile::tempdir().expect("tempdir");
        let response = web_only_test_router(&dir)
            .oneshot(
                Request::post("/api/voice/turn")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"text":"こんにちは"}"#))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn voice_route_rejects_web_token() {
        let dir = tempfile::tempdir().expect("tempdir");
        let response = voice_test_router(&dir)
            .0
            .oneshot(
                Request::post("/api/voice/turn")
                    .header(header::AUTHORIZATION, "Bearer web-secret")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"text":"こんにちは"}"#))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn voice_route_validates_text_and_surface() {
        let dir = tempfile::tempdir().expect("tempdir");
        let blank = voice_test_router(&dir)
            .0
            .oneshot(
                Request::post("/api/voice/turn")
                    .header(header::AUTHORIZATION, "Bearer voice-secret")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"text":"  "}"#))
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(blank.status(), StatusCode::BAD_REQUEST);

        let disallowed = voice_test_router(&dir)
            .0
            .oneshot(
                Request::post("/api/voice/turn")
                    .header(header::AUTHORIZATION, "Bearer voice-secret")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"text":"こんにちは","surface":"phone"}"#))
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(disallowed.status(), StatusCode::FORBIDDEN);

        let malformed = voice_test_router(&dir)
            .0
            .oneshot(
                Request::post("/api/voice/turn")
                    .header(header::AUTHORIZATION, "Bearer voice-secret")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from("{"))
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(malformed.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn voice_route_processes_and_persists_turn() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (app, app_state) = voice_test_router(&dir);
        let response = app
            .oneshot(
                Request::post("/api/voice/turn")
                    .header(header::AUTHORIZATION, "Bearer voice-secret")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        r#"{"text":"こんにちは","surface":"stackchan","session_key":"main","source":"stackchan-wake"}"#,
                    ))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        let body: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
        assert_eq!(body["response"], "音声応答です");
        assert_eq!(body["surface_thread"], "stackchan:main");
        assert!(body["trace_id"].as_str().is_some_and(|id| !id.is_empty()));

        let config = test_config(dir.path().to_str().expect("state root"));
        let db = crate::storage::Database::new(&config.db_path()).expect("db");
        let session = db
            .list_sessions()
            .expect("list sessions")
            .into_iter()
            .find(|session| {
                session.channel == "voice"
                    && session.surface_thread == "stackchan:main"
                    && session.agent_id == "default"
            })
            .expect("voice session");
        let messages = db.get_all_messages(session.chat_id).expect("load messages");
        assert_eq!(messages.len(), 2);

        let turns = app_state.runtime_status.recent_turns();
        let turn = turns.last().expect("voice turn status");
        assert_eq!(turn.channel, "voice");
        assert_eq!(turn.agent_id, "default");
        assert!(turn.ok);
    }

    fn webhook_test_router_with_receivers(
        dir: &tempfile::TempDir,
        receivers: HashMap<WebhookReceiverId, WebhookReceiverConfig>,
    ) -> Router {
        let mut config = test_config(dir.path().to_str().expect("state root"));
        config
            .channels
            .get_mut("web")
            .expect("web config")
            .auth_token = Some(ResolvedValue::Literal("web-secret".to_string()));
        config.webhooks = WebhooksConfig { receivers };
        let mut registry = ChannelRegistry::new();
        registry.register(Arc::new(WebAdapter));
        registry.register(Arc::new(NoopChannelAdapter("discord")));
        registry.register(Arc::new(NoopChannelAdapter("telegram")));
        let state = build_state_with_config(config, None, None, None, Some(Arc::new(registry)));
        build_router(WebState {
            config_path: None,
            app_state: Arc::new(state),
            run_hub: RunHub::default(),
            active_ws_connections: Arc::new(AtomicUsize::new(0)),
        })
    }

    fn default_webhook_receivers() -> HashMap<WebhookReceiverId, WebhookReceiverConfig> {
        HashMap::from([
            (
                WebhookReceiverId::new("egograph"),
                WebhookReceiverConfig {
                    token: Some(ResolvedValue::Literal("egograph-secret".to_string())),
                    file_token: None,
                    target: WebhookTargetConfig {
                        channel: ChannelName::new("discord"),
                        thread: "123".to_string(),
                        agent: Some(AgentId::new("default")),
                    },
                },
            ),
            (
                WebhookReceiverId::new("github"),
                WebhookReceiverConfig {
                    token: Some(ResolvedValue::Literal("github-secret".to_string())),
                    file_token: None,
                    target: WebhookTargetConfig {
                        channel: ChannelName::new("telegram"),
                        thread: "-100".to_string(),
                        agent: Some(AgentId::new("default")),
                    },
                },
            ),
        ])
    }

    fn webhook_test_router(dir: &tempfile::TempDir) -> Router {
        webhook_test_router_with_receivers(dir, default_webhook_receivers())
    }

    #[tokio::test]
    async fn webhook_route_accepts_only_matching_receiver_token() {
        let dir = tempfile::tempdir().expect("tempdir");
        let app = webhook_test_router(&dir);

        let ok = app
            .clone()
            .oneshot(
                Request::post("/api/webhooks/egograph")
                    .header(header::AUTHORIZATION, "Bearer egograph-secret")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from("{}"))
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(ok.status(), StatusCode::ACCEPTED);

        let other_receiver_token = app
            .clone()
            .oneshot(
                Request::post("/api/webhooks/egograph")
                    .header(header::AUTHORIZATION, "Bearer github-secret")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from("{}"))
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(other_receiver_token.status(), StatusCode::UNAUTHORIZED);

        let web_token = app
            .clone()
            .oneshot(
                Request::post("/api/webhooks/egograph")
                    .header(header::AUTHORIZATION, "Bearer web-secret")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from("{}"))
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(web_token.status(), StatusCode::UNAUTHORIZED);

        let no_token = app
            .oneshot(
                Request::post("/api/webhooks/egograph")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from("{}"))
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(no_token.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn webhook_route_rejects_unknown_receiver() {
        let dir = tempfile::tempdir().expect("tempdir");
        let app = webhook_test_router(&dir);

        let response = app
            .oneshot(
                Request::post("/api/webhooks/unknown")
                    .header(header::AUTHORIZATION, "Bearer anything")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from("{}"))
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn webhook_route_rejects_payload_over_fixed_limit() {
        let dir = tempfile::tempdir().expect("tempdir");
        let app = webhook_test_router(&dir);

        let oversized = "x".repeat(64 * 1024 + 1);
        let response = app
            .oneshot(
                Request::post("/api/webhooks/egograph")
                    .header(header::AUTHORIZATION, "Bearer egograph-secret")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(oversized))
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[tokio::test]
    async fn webhook_route_rejects_unregistered_or_voice_target_channel() {
        let dir = tempfile::tempdir().expect("tempdir");
        let receivers = HashMap::from([
            (
                WebhookReceiverId::new("bad-channel"),
                WebhookReceiverConfig {
                    token: Some(ResolvedValue::Literal("secret".to_string())),
                    file_token: None,
                    target: WebhookTargetConfig {
                        channel: ChannelName::new("missing"),
                        thread: "123".to_string(),
                        agent: Some(AgentId::new("default")),
                    },
                },
            ),
            (
                WebhookReceiverId::new("voice-target"),
                WebhookReceiverConfig {
                    token: Some(ResolvedValue::Literal("secret".to_string())),
                    file_token: None,
                    target: WebhookTargetConfig {
                        channel: ChannelName::new("voice"),
                        thread: "123".to_string(),
                        agent: Some(AgentId::new("default")),
                    },
                },
            ),
        ]);
        let app = webhook_test_router_with_receivers(&dir, receivers);

        let missing = app
            .clone()
            .oneshot(
                Request::post("/api/webhooks/bad-channel")
                    .header(header::AUTHORIZATION, "Bearer secret")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from("{}"))
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(missing.status(), StatusCode::BAD_REQUEST);

        let voice = app
            .oneshot(
                Request::post("/api/webhooks/voice-target")
                    .header(header::AUTHORIZATION, "Bearer secret")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from("{}"))
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(voice.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn webhook_route_rejects_missing_agent_and_blank_thread() {
        let dir = tempfile::tempdir().expect("tempdir");
        let receivers = HashMap::from([
            (
                WebhookReceiverId::new("bad-agent"),
                WebhookReceiverConfig {
                    token: Some(ResolvedValue::Literal("secret".to_string())),
                    file_token: None,
                    target: WebhookTargetConfig {
                        channel: ChannelName::new("discord"),
                        thread: "123".to_string(),
                        agent: Some(AgentId::new("nonexistent")),
                    },
                },
            ),
            (
                WebhookReceiverId::new("blank-thread"),
                WebhookReceiverConfig {
                    token: Some(ResolvedValue::Literal("secret".to_string())),
                    file_token: None,
                    target: WebhookTargetConfig {
                        channel: ChannelName::new("discord"),
                        thread: "   ".to_string(),
                        agent: Some(AgentId::new("default")),
                    },
                },
            ),
        ]);
        let app = webhook_test_router_with_receivers(&dir, receivers);

        let bad_agent = app
            .clone()
            .oneshot(
                Request::post("/api/webhooks/bad-agent")
                    .header(header::AUTHORIZATION, "Bearer secret")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from("{}"))
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(bad_agent.status(), StatusCode::BAD_REQUEST);

        let blank_thread = app
            .oneshot(
                Request::post("/api/webhooks/blank-thread")
                    .header(header::AUTHORIZATION, "Bearer secret")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from("{}"))
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(blank_thread.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn webhook_route_accepts_and_enqueues_turn_without_waiting_for_completion() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut config = test_config(dir.path().to_str().expect("state root"));
        config
            .channels
            .get_mut("web")
            .expect("web config")
            .auth_token = Some(ResolvedValue::Literal("web-secret".to_string()));
        config.webhooks = WebhooksConfig {
            receivers: HashMap::from([(
                WebhookReceiverId::new("egograph"),
                WebhookReceiverConfig {
                    token: Some(ResolvedValue::Literal("egograph-secret".to_string())),
                    file_token: None,
                    target: WebhookTargetConfig {
                        channel: ChannelName::new("discord"),
                        thread: "123456789".to_string(),
                        agent: Some(AgentId::new("default")),
                    },
                },
            )]),
        };
        let mut registry = ChannelRegistry::new();
        registry.register(Arc::new(WebAdapter));
        registry.register(Arc::new(NoopChannelAdapter("discord")));
        let provider = FakeProvider {
            responses: std::sync::Mutex::new(vec![MessagesResponse {
                content: "Pipeline failure investigated.".to_string(),
                reasoning_content: None,
                tool_calls: vec![],
                usage: None,
            }]),
        };
        let app_state = Arc::new(build_state_with_config(
            config,
            Some(Arc::new(provider)),
            None,
            None,
            Some(Arc::new(registry)),
        ));
        let app = build_router(WebState {
            config_path: None,
            app_state: Arc::clone(&app_state),
            run_hub: RunHub::default(),
            active_ws_connections: Arc::new(AtomicUsize::new(0)),
        });

        let payload = serde_json::json!({
            "source": "urn:egograph:pipelines",
            "type": "egograph.pipelines.workflow_failed",
            "data": {
                "workflow_id": "test_workflow",
                "run_id": "test_run",
                "error_message": "test error"
            }
        });
        let response = app
            .oneshot(
                Request::post("/api/webhooks/egograph")
                    .header(header::AUTHORIZATION, "Bearer egograph-secret")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(payload.to_string()))
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::ACCEPTED);

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let turns = app_state.runtime_status.recent_turns();
            if turns.iter().any(|t| t.channel == "discord" && t.ok) {
                break;
            }
            if std::time::Instant::now() > deadline {
                panic!("webhook turn was not enqueued within 5 seconds");
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    }

    #[tokio::test]
    async fn webhook_route_is_not_protected_by_web_api_auth_middleware() {
        let dir = tempfile::tempdir().expect("tempdir");
        let app = webhook_test_router(&dir);

        let response = app
            .oneshot(
                Request::post("/api/webhooks/egograph")
                    .header(header::AUTHORIZATION, "Bearer egograph-secret")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from("{}"))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_ne!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "webhook route must not be gated by web API auth middleware"
        );
    }
}
