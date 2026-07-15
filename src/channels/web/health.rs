//! Web-layer health and telemetry endpoints.

use std::collections::BTreeMap;

use axum::Json;
use axum::extract::State;
use serde::{Deserialize, Serialize};

use crate::runtime::metrics;
use crate::runtime::runtime_status::{AuditError, ChannelHealth, ChannelState, TurnRecord};

use super::WebState;

// ---------------------------------------------------------------------------
// Response structs
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize)]
pub(crate) struct HealthResponse {
    ok: bool,
    version: String,
    uptime_secs: u64,
    pid: u32,
    db: DbHealth,
    accepting_inputs: bool,
    shutdown_started: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    critical_task_failure: Option<String>,
    owned_task_count: usize,
    channels: std::collections::HashMap<String, ChannelHealth>,
    mcp: McpStatus,
    active_turns: usize,
    recent_errors_count: usize,
    instance_lock: InstanceLockStatus,
}

#[derive(Serialize, Deserialize)]
pub(crate) struct DbHealth {
    ok: bool,
}

#[derive(Serialize, Deserialize)]
pub(crate) struct McpStatus {
    healthy: usize,
    failed: usize,
    servers: Vec<McpServer>,
}

#[derive(Serialize, Deserialize)]
pub(crate) struct McpServer {
    name: String,
    connected: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub(crate) struct TelemetryResponse {
    metrics: BTreeMap<String, Vec<MetricEntry>>,
    recent_turns: Vec<TurnRecord>,
    recent_errors: Vec<AuditError>,
}

#[derive(Serialize, Deserialize)]
pub(crate) struct MetricEntry {
    labels: BTreeMap<String, String>,
    value: f64,
}

/// Runtime instance lock state exposed by the health endpoint. `held` is true
/// when this process owns the exclusive advisory lock for its state root;
/// `lock_file` is the path of the lock file backing that ownership.
#[derive(Serialize, Deserialize)]
pub(crate) struct InstanceLockStatus {
    held: bool,
    lock_file: String,
}

#[derive(Serialize, Deserialize)]
pub(crate) struct GatewayStatusResponse {
    #[serde(flatten)]
    pub health: HealthResponse,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub telemetry: Option<TelemetryResponse>,
}

// ---------------------------------------------------------------------------
// Endpoints
// ---------------------------------------------------------------------------

/// Health probe — returns DB, channels, MCP, active turns, and recent errors.
pub(super) async fn health(state: State<WebState>) -> Json<HealthResponse> {
    let snapshot = state.app_state.runtime_status.snapshot();
    let active_turns = state.app_state.active_turns.total_active();

    let has_running_channel = snapshot
        .channels
        .values()
        .any(|ch| matches!(ch.state, ChannelState::Running));
    // `ok` requires a healthy DB, at least one running channel, and that the
    // runtime is still accepting input (not shutting down and no critical task
    // failure).
    let ok = snapshot.db_healthy
        && has_running_channel
        && snapshot.accepting_inputs
        && !snapshot.shutdown_started
        && snapshot.critical_task_failure.is_none();

    Json(HealthResponse {
        ok,
        version: snapshot.version,
        uptime_secs: uptime_from_snapshot(&snapshot.started_at),
        pid: snapshot.pid,
        db: DbHealth {
            ok: snapshot.db_healthy,
        },
        accepting_inputs: snapshot.accepting_inputs,
        shutdown_started: snapshot.shutdown_started,
        critical_task_failure: snapshot.critical_task_failure,
        owned_task_count: snapshot.owned_task_count,
        channels: snapshot.channels,
        mcp: build_mcp_status(&state).await,
        active_turns,
        recent_errors_count: snapshot.recent_errors.len(),
        instance_lock: InstanceLockStatus {
            held: state.app_state.supervisor.instance_lock_held(),
            lock_file: state
                .app_state
                .supervisor
                .instance_lock_path()
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
        },
    })
}

/// JSON telemetry endpoint combining Prometheus counters with recent turns and errors.
pub(super) async fn telemetry_handler(state: State<WebState>) -> Json<TelemetryResponse> {
    let prom_text = metrics::metrics_output();
    let parsed_metrics = parse_prometheus_to_json(&prom_text);

    let recent_turns = state.app_state.runtime_status.recent_turns();
    let recent_errors = state.app_state.runtime_status.recent_errors();

    Json(TelemetryResponse {
        metrics: parsed_metrics,
        recent_turns,
        recent_errors,
    })
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn parse_prometheus_to_json(prom_text: &str) -> BTreeMap<String, Vec<MetricEntry>> {
    let mut result: BTreeMap<String, Vec<MetricEntry>> = BTreeMap::new();

    for line in prom_text.lines() {
        if line.starts_with('#') || line.is_empty() {
            continue;
        }

        let (metric_name, labels_raw, value) = match parse_prometheus_line(line) {
            Some(parsed) => parsed,
            None => continue,
        };

        let labels = labels_raw.into_iter().collect::<BTreeMap<_, _>>();

        result
            .entry(metric_name.to_string())
            .or_default()
            .push(MetricEntry { labels, value });
    }

    result
}

type Labels = Vec<(String, String)>;

fn parse_prometheus_line(line: &str) -> Option<(&str, Labels, f64)> {
    let space_pos = line.rfind(' ')?;
    let value_str = &line[space_pos + 1..];
    let value: f64 = value_str.parse().ok()?;

    let (metric_name, labels) = if let Some(bp) = line.find('{') {
        let name = &line[..bp];
        let labels_end = line.rfind('}')?;
        let labels_str = &line[bp + 1..labels_end];
        (name, parse_label_pairs(labels_str)?)
    } else {
        let name = &line[..space_pos];
        (name, Vec::new())
    };

    Some((metric_name, labels, value))
}

fn parse_label_pairs(input: &str) -> Option<Vec<(String, String)>> {
    let input = input.trim();
    let input = input.strip_suffix('}').unwrap_or(input);
    let mut pairs = Vec::new();

    for pair in split_label_pairs(input) {
        let (key, value) = split_key_value(&pair)?;
        pairs.push((key.to_string(), value.to_string()));
    }

    Some(pairs)
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

async fn build_mcp_status(state: &WebState) -> McpStatus {
    let Some(mcp_mgr) = &state.app_state.mcp_manager else {
        return McpStatus {
            healthy: 0,
            failed: 0,
            servers: Vec::new(),
        };
    };

    let mcp_guard = mcp_mgr.read().await;
    let mcp_snap = mcp_guard.status_snapshot();

    let servers: Vec<McpServer> = mcp_snap
        .connected
        .iter()
        .map(|s| McpServer {
            name: s.name.clone(),
            connected: true,
            error: None,
        })
        .chain(mcp_snap.failed.iter().map(|s| McpServer {
            name: s.name.clone(),
            connected: false,
            error: Some(s.error.clone()),
        }))
        .collect();

    McpStatus {
        healthy: mcp_snap.connected.len(),
        failed: mcp_snap.failed.len(),
        servers,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::runtime::runtime_status::ChannelState;

    fn test_state() -> WebState {
        let dir = tempfile::TempDir::new().unwrap();
        let state_root = dir.path().to_string_lossy().to_string();
        let app_state = crate::test_util::build_state_with_config(
            crate::test_util::test_config(&state_root),
            None,
            None,
            None,
            None,
        );
        WebState {
            app_state: Arc::new(app_state),
            config_path: None,
            run_hub: crate::channels::web::RunHub::default(),
            active_ws_connections: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        }
    }

    // -----------------------------------------------------------------------
    // health endpoint
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn health_returns_full_status() {
        let state = test_state();
        state
            .app_state
            .runtime_status
            .update_channel("web", ChannelState::Running);

        let Json(resp) = health(State(state)).await;

        assert!(resp.ok);
        assert_eq!(resp.version, env!("CARGO_PKG_VERSION"));
        assert!(resp.pid > 0);
        assert_eq!(resp.active_turns, 0);
        assert_eq!(resp.recent_errors_count, 0);
        assert!(resp.db.ok);

        let web = resp.channels.get("web").expect("web channel missing");
        assert!(matches!(web.state, ChannelState::Running));
    }

    #[tokio::test]
    async fn health_ok_when_all_healthy() {
        let state = test_state();
        state
            .app_state
            .runtime_status
            .update_channel("web", ChannelState::Running);

        let Json(resp) = health(State(state)).await;

        assert!(resp.ok);
    }

    #[tokio::test]
    async fn health_not_ok_when_db_unhealthy() {
        let state = test_state();
        state
            .app_state
            .runtime_status
            .update_channel("discord", ChannelState::Running);
        state.app_state.runtime_status.set_db_healthy(false);

        let Json(resp) = health(State(state)).await;

        assert!(!resp.ok);
    }

    #[tokio::test]
    async fn health_not_ok_when_all_channels_failed() {
        let state = test_state();
        state
            .app_state
            .runtime_status
            .update_channel("web", ChannelState::Failed);
        state
            .app_state
            .runtime_status
            .update_channel("discord", ChannelState::Failed);

        let Json(resp) = health(State(state)).await;

        assert!(!resp.ok);
    }

    #[tokio::test]
    async fn health_includes_mcp_status() {
        let state = test_state();
        state
            .app_state
            .runtime_status
            .update_channel("web", ChannelState::Running);

        let Json(resp) = health(State(state)).await;
        let json = serde_json::to_value(&resp).unwrap();
        assert!(json.get("mcp").is_some());
    }

    #[tokio::test]
    async fn health_reports_shutdown_and_critical_failure_fields() {
        let state = test_state();
        state
            .app_state
            .runtime_status
            .update_channel("web", ChannelState::Running);

        // Healthy default: accepting input, not shutting down, no critical failure.
        let Json(resp) = health(State(state.clone())).await;
        assert!(resp.accepting_inputs);
        assert!(!resp.shutdown_started);
        assert!(resp.critical_task_failure.is_none());
        assert_eq!(resp.owned_task_count, 0);
        assert!(resp.ok);

        // Simulate shutdown: accepting_inputs flipped off.
        state.app_state.runtime_status.set_accepting_inputs(false);
        state.app_state.runtime_status.set_shutdown_started(true);
        let Json(resp) = health(State(state.clone())).await;
        assert!(!resp.accepting_inputs);
        assert!(resp.shutdown_started);
        assert!(!resp.ok, "shutdown must make health not ok");

        // A critical task failure also flips ok to false even before shutdown.
        state
            .app_state
            .runtime_status
            .record_critical_task_failure("web died");
        let Json(resp) = health(State(state)).await;
        assert_eq!(resp.critical_task_failure.as_deref(), Some("web died"));
        assert!(!resp.ok);
    }

    // -----------------------------------------------------------------------
    // parse_prometheus_to_json
    // -----------------------------------------------------------------------

    #[test]
    fn parse_prometheus_with_labels() {
        let input = r#"egopulse_turns_total{agent="alice",channel="discord"} 42
# comment
egopulse_errors_total{kind="llm"} 3
"#;
        let parsed = parse_prometheus_to_json(input);

        let turns = parsed
            .get("egopulse_turns_total")
            .expect("turns_total missing");
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].value, 42.0);
        assert_eq!(turns[0].labels.get("agent"), Some(&"alice".to_string()));
        assert_eq!(turns[0].labels.get("channel"), Some(&"discord".to_string()));

        let errors = parsed
            .get("egopulse_errors_total")
            .expect("errors_total missing");
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].value, 3.0);
    }

    #[test]
    fn parse_prometheus_ignores_comments_and_empty() {
        let input = "# HELP foo\n\negopulse_active_turns 7\n";
        let parsed = parse_prometheus_to_json(input);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed["egopulse_active_turns"][0].value, 7.0);
        assert!(parsed["egopulse_active_turns"][0].labels.is_empty());
    }

    #[test]
    fn parse_prometheus_sums_multiple_entries() {
        let input = r#"metric_a 10
metric_a 5
"#;
        let parsed = parse_prometheus_to_json(input);
        assert_eq!(parsed["metric_a"].len(), 2);
        assert_eq!(parsed["metric_a"][0].value, 10.0);
        assert_eq!(parsed["metric_a"][1].value, 5.0);
    }

    #[test]
    fn parse_prometheus_value_with_decimal() {
        let input = "metric 1.5\n";
        let parsed = parse_prometheus_to_json(input);
        assert_eq!(parsed["metric"][0].value, 1.5);
    }

    // -----------------------------------------------------------------------
    // parse_labels
    // -----------------------------------------------------------------------

    #[test]
    fn parse_labels_simple() {
        assert_eq!(
            parse_label_pairs("a=\"1\",b=\"2\""),
            Some(vec![
                ("a".to_string(), "1".to_string()),
                ("b".to_string(), "2".to_string())
            ])
        );
    }

    #[test]
    fn parse_labels_empty() {
        assert_eq!(parse_label_pairs(""), Some(vec![]));
    }

    #[test]
    fn parse_labels_spaces() {
        assert_eq!(
            parse_label_pairs("a = \"1\" , b = \"2\""),
            Some(vec![
                ("a".to_string(), "1".to_string()),
                ("b".to_string(), "2".to_string())
            ])
        );
    }

    #[test]
    fn parse_labels_with_trailing_comma() {
        assert_eq!(
            parse_label_pairs("a=\"1\",b=\"2\","),
            Some(vec![
                ("a".to_string(), "1".to_string()),
                ("b".to_string(), "2".to_string())
            ])
        );
    }

    #[test]
    fn parse_labels_closed_brace() {
        assert_eq!(
            parse_label_pairs("a=\"1\",b=\"2\"}"),
            Some(vec![
                ("a".to_string(), "1".to_string()),
                ("b".to_string(), "2".to_string())
            ])
        );
    }

    // -----------------------------------------------------------------------
    // split_label_pairs
    // -----------------------------------------------------------------------

    #[test]
    fn split_label_pairs_no_quotes() {
        assert_eq!(
            split_label_pairs("a=1,b=2"),
            vec!["a=1".to_string(), "b=2".to_string()]
        );
    }

    #[test]
    fn split_label_pairs_with_quoted_value() {
        assert_eq!(
            split_label_pairs(r#"a="1,2",b="3""#),
            vec![r#"a="1,2""#.to_string(), r#"b="3""#.to_string()]
        );
    }

    #[test]
    fn split_label_pairs_trailing_comma() {
        let got = split_label_pairs("a=\"1\",b=\"2\",");
        assert_eq!(got, vec!["a=\"1\"".to_string(), "b=\"2\"".to_string()]);
    }

    #[test]
    fn split_label_pairs_empty() {
        assert_eq!(split_label_pairs(""), Vec::<String>::new());
    }

    // -----------------------------------------------------------------------
    // uptime_from_snapshot
    // -----------------------------------------------------------------------

    #[test]
    fn uptime_from_snapshot_valid() {
        let started = "2020-01-01T00:00:00Z";
        let secs = uptime_from_snapshot(started);
        assert!(secs > 0);
    }

    #[test]
    fn uptime_from_snapshot_invalid() {
        assert_eq!(uptime_from_snapshot("invalid"), 0);
    }
}
