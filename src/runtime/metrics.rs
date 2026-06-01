//! Prometheus metrics initialization and helper functions.
//!
//! Exposes a `OnceLock`-based singleton recorder. Call [`init_metrics`] once
//! at startup; subsequent calls are no-ops. Use [`metrics_output`] to render
//! the current Prometheus text exposition.

use std::sync::OnceLock;

use metrics::{counter, describe_counter, describe_gauge, gauge};
use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};

static HANDLE: OnceLock<PrometheusHandle> = OnceLock::new();

/// Initializes the global Prometheus metrics recorder.
///
/// Returns a `&'static PrometheusHandle` for rendering. Safe to call more than
/// once — only the first invocation installs the recorder.
///
/// # Panics
///
/// Panics if the recorder cannot be built (should never happen outside of OOM).
pub(crate) fn init_metrics() -> &'static PrometheusHandle {
    HANDLE.get_or_init(|| {
        let handle = PrometheusBuilder::new()
            .install_recorder()
            .expect("prometheus recorder");

        describe_counter!(
            "egopulse_turns_total",
            "Total number of agent turns executed"
        );
        describe_counter!("egopulse_turn_errors_total", "Total number of turn errors");
        describe_counter!("egopulse_llm_tokens_total", "Total LLM tokens used");
        describe_counter!("egopulse_tool_calls_total", "Total tool calls executed");
        describe_gauge!("egopulse_active_turns", "Number of currently active turns");

        handle
    })
}

/// Renders the current Prometheus text exposition.
///
/// Idempotent — initializes the recorder if not yet installed.
pub(crate) fn metrics_output() -> String {
    init_metrics().render()
}

pub(crate) fn inc_turns_total(agent: &str, channel: &str) {
    counter!("egopulse_turns_total", "agent" => agent.to_owned(), "channel" => channel.to_owned())
        .increment(1);
}

pub(crate) fn inc_turn_errors_total(kind: &str, agent: &str) {
    counter!("egopulse_turn_errors_total", "kind" => kind.to_owned(), "agent" => agent.to_owned())
        .increment(1);
}

/// `direction` should be `"input"` or `"output"`.
pub(crate) fn inc_llm_tokens_total(direction: &str, provider: &str, amount: i64) {
    if amount <= 0 {
        return;
    }
    counter!("egopulse_llm_tokens_total", "direction" => direction.to_owned(), "provider" => provider.to_owned())
        .increment(amount as u64);
}

pub(crate) fn set_active_turns_gauge(count: usize) {
    gauge!("egopulse_active_turns").set(count as f64);
}

pub(crate) fn inc_tool_calls_total(tool: &str, status: &str) {
    counter!("egopulse_tool_calls_total", "tool" => tool.to_owned(), "status" => status.to_owned())
        .increment(1);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metrics_output_contains_registered_descriptions() {
        // init_metrics is idempotent — safe to call in multiple tests.
        init_metrics();

        // Exercise counters so at least one data line appears.
        inc_turns_total("test-agent", "test-channel");
        inc_turn_errors_total("test-kind", "test-agent");
        inc_llm_tokens_total("input", "openai", 42);
        inc_tool_calls_total("shell", "ok");
        set_active_turns_gauge(3);

        let output = metrics_output();

        assert!(
            output.contains("# HELP egopulse_turns_total"),
            "should contain HELP for turns_total: {output}"
        );
        assert!(
            output.contains("# TYPE egopulse_turns_total"),
            "should contain TYPE for turns_total: {output}"
        );
        assert!(
            output.contains("egopulse_turn_errors_total"),
            "should contain turn_errors_total: {output}"
        );
        assert!(
            output.contains("egopulse_llm_tokens_total"),
            "should contain llm_tokens_total: {output}"
        );
        assert!(
            output.contains("egopulse_tool_calls_total"),
            "should contain tool_calls_total: {output}"
        );
        assert!(
            output.contains("egopulse_active_turns"),
            "should contain active_turns: {output}"
        );
    }

    #[test]
    fn all_metric_names_have_egopulse_prefix() {
        init_metrics();

        inc_turns_total("a", "b");
        inc_turn_errors_total("c", "d");
        inc_llm_tokens_total("input", "openai", 1);
        inc_tool_calls_total("shell", "ok");
        set_active_turns_gauge(0);

        let output = metrics_output();
        for line in output.lines() {
            if line.starts_with('#') || line.is_empty() {
                continue;
            }
            // Every metric data line should start with "egopulse_"
            let metric_name = line.split('{').next().unwrap_or(line);
            assert!(
                metric_name.starts_with("egopulse_"),
                "metric line must start with egopulse_ prefix: {line}"
            );
        }
    }
}
