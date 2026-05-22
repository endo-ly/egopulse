//! Web-layer health and telemetry endpoints.

use std::collections::BTreeMap;

use axum::Json;
use axum::extract::State;

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

/// JSON telemetry endpoint combining Prometheus counters with recent turns and errors.
pub(super) async fn telemetry_handler(state: State<WebState>) -> Json<serde_json::Value> {
    let prom_text = metrics::metrics_output();
    let parsed_metrics = parse_prometheus_to_json(&prom_text);

    let recent_turns = state.app_state.runtime_status.recent_turns();
    let recent_errors = state.app_state.runtime_status.recent_errors();

    Json(serde_json::json!({
        "metrics": parsed_metrics,
        "recent_turns": recent_turns,
        "recent_errors": recent_errors,
    }))
}

fn parse_prometheus_to_json(prom_text: &str) -> BTreeMap<String, Vec<serde_json::Value>> {
    let mut result: BTreeMap<String, Vec<serde_json::Value>> = BTreeMap::new();

    for line in prom_text.lines() {
        if line.starts_with('#') || line.is_empty() {
            continue;
        }

        let (metric_name, labels, value) = match parse_prometheus_line(line) {
            Some(parsed) => parsed,
            None => continue,
        };

        result
            .entry(metric_name.to_string())
            .or_default()
            .push(serde_json::json!({
                "labels": labels,
                "value": value,
            }));
    }

    result
}

fn parse_prometheus_line(
    line: &str,
) -> Option<(&str, serde_json::Map<String, serde_json::Value>, f64)> {
    let space_pos = line.rfind(' ')?;
    let value_str = &line[space_pos + 1..];
    let value: f64 = value_str.parse().ok()?;

    let (metric_name, labels) = if let Some(bp) = line.find('{') {
        let name = &line[..bp];
        let labels_end = line.rfind('}')?;
        let labels_str = &line[bp + 1..labels_end];
        (name, parse_labels(labels_str)?)
    } else {
        let name = &line[..space_pos];
        (name, serde_json::Map::new())
    };

    Some((metric_name, labels, value))
}

fn parse_labels(input: &str) -> Option<serde_json::Map<String, serde_json::Value>> {
    let input = input.trim();
    let input = input.strip_suffix('}').unwrap_or(input);
    let mut map = serde_json::Map::new();

    for pair in split_label_pairs(input) {
        let (key, value) = split_key_value(&pair)?;
        map.insert(
            key.to_string(),
            serde_json::Value::String(value.to_string()),
        );
    }

    Some(map)
}

fn split_label_pairs(input: &str) -> Vec<String> {
    let mut pairs = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;

    for ch in input.chars() {
        if ch == '"' {
            in_quotes = !in_quotes;
            current.push(ch);
        } else if ch == ',' && !in_quotes {
            if !current.trim().is_empty() {
                pairs.push(current.trim().to_string());
            }
            current.clear();
        } else {
            current.push(ch);
        }
    }
    if !current.trim().is_empty() {
        pairs.push(current.trim().to_string());
    }
    pairs
}

fn split_key_value(pair: &str) -> Option<(&str, &str)> {
    let eq_pos = pair.find('=')?;
    let key = pair[..eq_pos].trim();
    let raw_value = pair[eq_pos + 1..].trim();
    let value = raw_value
        .strip_prefix('"')
        .and_then(|v| v.strip_suffix('"'))
        .unwrap_or(raw_value);
    Some((key, value))
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
    async fn telemetry_returns_json_with_metrics_and_recent_data() {
        crate::runtime::metrics::init_metrics();
        crate::runtime::metrics::inc_turns_total("telemetry-check", "test");

        let rs = Arc::new(RuntimeStatus::new());
        rs.update_channel("web", ChannelState::Running);
        rs.push_turn("t1", "agent", "web", "2025-01-01T00:00:00Z", 1.5, true);
        rs.push_error("e1", "timeout", "agent", "web", "timed out");

        let ws = test_web_state_from_status(rs);
        let state = State(ws);

        let Json(value) = telemetry_handler(state).await;

        assert!(value["metrics"].is_object(), "metrics should be an object");
        assert!(
            value["metrics"]
                .as_object()
                .expect("metrics map")
                .contains_key("egopulse_turns_total"),
            "metrics should contain egopulse_turns_total"
        );

        let turns = value["recent_turns"]
            .as_array()
            .expect("recent_turns array");
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0]["trace_id"], "t1");

        let errors = value["recent_errors"]
            .as_array()
            .expect("recent_errors array");
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0]["trace_id"], "e1");
    }

    #[tokio::test]
    async fn telemetry_metrics_have_egopulse_prefix() {
        crate::runtime::metrics::init_metrics();
        crate::runtime::metrics::inc_turns_total("test", "web");
        crate::runtime::metrics::inc_tool_calls_total("shell", "ok");

        let ws = test_web_state();
        let state = State(ws);

        let Json(value) = telemetry_handler(state).await;

        let metrics = value["metrics"].as_object().expect("metrics map");
        for key in metrics.keys() {
            assert!(
                key.starts_with("egopulse_"),
                "metric key must have egopulse_ prefix: {key}"
            );
        }
    }
}
