//! Web-layer health and Prometheus metrics endpoints.

use axum::Json;
use axum::extract::State;
use axum::http::{HeaderValue, header};
use axum::response::Response;

use crate::runtime::metrics;
use crate::runtime::runtime_status::ChannelState;

use super::WebState;

/// Health probe — returns DB, channels, MCP, active turns, and recent errors.
pub(super) async fn health(state: State<WebState>) -> Json<serde_json::Value> {
    let snapshot = state.app_state.runtime_status.snapshot();
    let active_turns = state.app_state.active_turns.total_active();

    let has_running_channel = snapshot
        .channels
        .values()
        .any(|ch| matches!(ch.state, ChannelState::Running));
    let ok = snapshot.db_healthy && has_running_channel;

    let mcp = build_mcp_status(&state).await;

    Json(serde_json::json!({
        "ok": ok,
        "version": snapshot.version,
        "uptime_secs": uptime_from_snapshot(&snapshot.started_at),
        "pid": snapshot.pid,
        "db": { "ok": snapshot.db_healthy },
        "channels": snapshot.channels,
        "mcp": mcp,
        "active_turns": active_turns,
        "recent_errors_count": snapshot.recent_errors.len(),
    }))
}

/// Prometheus text exposition endpoint.
pub(super) async fn metrics_handler() -> Response {
    let body = metrics::metrics_output();
    let mut response = Response::new(body.into());
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; version=0.0.4; charset=utf-8"),
    );
    response
}

fn uptime_from_snapshot(started_at: &str) -> u64 {
    chrono::DateTime::parse_from_rfc3339(started_at)
        .map(|dt| {
            let now = chrono::Utc::now();
            (now - dt.to_utc()).num_seconds().max(0) as u64
        })
        .unwrap_or(0)
}

async fn build_mcp_status(state: &WebState) -> serde_json::Value {
    let Some(mcp_mgr) = &state.app_state.mcp_manager else {
        return serde_json::json!(null);
    };

    let mcp_guard = mcp_mgr.read().await;
    let mcp_snap = mcp_guard.status_snapshot();

    let servers: Vec<serde_json::Value> = mcp_snap
        .connected
        .iter()
        .map(|s| {
            serde_json::json!({
                "name": s.name,
                "connected": true,
            })
        })
        .chain(mcp_snap.failed.iter().map(|s| {
            serde_json::json!({
                "name": s.name,
                "connected": false,
                "error": s.error,
            })
        }))
        .collect();

    serde_json::json!({
        "healthy": mcp_snap.connected.len(),
        "failed": mcp_snap.failed.len(),
        "servers": servers,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::runtime::RuntimeStatus;
    use crate::runtime::runtime_status::ChannelState;
    use crate::test_util;

    fn test_web_state() -> WebState {
        let state_root = tempfile::tempdir().expect("tempdir").keep();
        let state = test_util::build_state_with_config(
            test_util::test_config(state_root.to_str().expect("utf8")),
            None,
            None,
            None,
            None,
        );
        WebState {
            app_state: Arc::new(state),
            config_path: None,
            run_hub: super::super::RunHub::default(),
            active_ws_connections: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        }
    }

    fn test_web_state_from_status(rs: Arc<RuntimeStatus>) -> WebState {
        let state_root = tempfile::tempdir().expect("tempdir").keep();
        let mut state = test_util::build_state_with_config(
            test_util::test_config(state_root.to_str().expect("utf8")),
            None,
            None,
            None,
            None,
        );
        state.runtime_status = rs;
        WebState {
            app_state: Arc::new(state),
            config_path: None,
            run_hub: super::super::RunHub::default(),
            active_ws_connections: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        }
    }

    #[tokio::test]
    async fn health_returns_full_status() {
        let rs = Arc::new(RuntimeStatus::new());
        rs.update_channel("web", ChannelState::Running);

        let ws = test_web_state_from_status(rs);
        let state = State(ws);

        let Json(value) = health(state).await;

        assert_eq!(value["ok"], true);
        assert!(value["version"].is_string());
        assert_ne!(value["version"].as_str().unwrap_or(""), "");
        assert!(value["uptime_secs"].is_number());
        assert!(value["pid"].is_number());
        assert!(value["db"].is_object());
        assert!(value["channels"].is_object());
        assert!(value["active_turns"].is_number());
        assert!(value["recent_errors_count"].is_number());
    }

    #[tokio::test]
    async fn health_ok_when_all_healthy() {
        let rs = Arc::new(RuntimeStatus::new());
        rs.update_channel("web", ChannelState::Running);

        let ws = test_web_state_from_status(rs);
        let state = State(ws);

        let Json(value) = health(state).await;

        assert_eq!(value["ok"], true);
        assert_eq!(value["db"]["ok"], true);
    }

    #[tokio::test]
    async fn health_not_ok_when_db_unhealthy() {
        let rs = Arc::new(RuntimeStatus::new());
        rs.update_channel("web", ChannelState::Running);
        rs.set_db_healthy(false);

        let ws = test_web_state_from_status(rs);
        let state = State(ws);

        let Json(value) = health(state).await;

        assert_eq!(value["ok"], false);
        assert_eq!(value["db"]["ok"], false);
    }

    #[tokio::test]
    async fn health_not_ok_when_all_channels_failed() {
        let rs = Arc::new(RuntimeStatus::new());
        rs.update_channel_error("web", "connection refused");

        let ws = test_web_state_from_status(rs);
        let state = State(ws);

        let Json(value) = health(state).await;

        assert_eq!(value["ok"], false);
    }

    #[tokio::test]
    async fn health_includes_mcp_status() {
        let ws = test_web_state();
        let state = State(ws);

        let Json(value) = health(state).await;

        assert!(value["mcp"].is_null());
    }

    #[tokio::test]
    async fn metrics_returns_prometheus_text() {
        crate::runtime::metrics::init_metrics();
        crate::runtime::metrics::inc_turns_total("health-check", "test");

        let response = metrics_handler().await;
        let ct = response
            .headers()
            .get(header::CONTENT_TYPE)
            .expect("content-type header")
            .to_str()
            .expect("utf8");
        assert!(
            ct.contains("text/plain"),
            "content-type should be text/plain: {ct}"
        );

        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .expect("body");
        let text = String::from_utf8(body.to_vec()).expect("utf8 body");
        assert!(
            text.contains("# HELP"),
            "should contain Prometheus HELP lines: {text}"
        );
        assert!(
            text.contains("# TYPE"),
            "should contain Prometheus TYPE lines: {text}"
        );
    }

    #[tokio::test]
    async fn metrics_contains_egopulse_prefix() {
        crate::runtime::metrics::init_metrics();
        crate::runtime::metrics::inc_turns_total("test", "web");
        crate::runtime::metrics::inc_tool_calls_total("shell", "ok");

        let response = metrics_handler().await;
        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .expect("body");
        let text = String::from_utf8(body.to_vec()).expect("utf8 body");

        for line in text.lines() {
            if line.starts_with('#') || line.is_empty() {
                continue;
            }
            let metric_name = line.split('{').next().unwrap_or(line);
            assert!(
                metric_name.starts_with("egopulse_"),
                "metric must have egopulse_ prefix: {line}"
            );
        }
    }
}
