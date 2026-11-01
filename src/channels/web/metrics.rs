//! Runtime-wide usage and reliability aggregates for the WebUI Metrics screen.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use axum::Json;
use axum::extract::{Query, State};
use chrono::{DateTime, Datelike, Duration, NaiveDateTime, TimeZone, Timelike, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};

use crate::storage::{
    AgentActivity, FailureRecord, FailureSource, PulseIntentionUsage, SleepRunCount,
    SleepUsageMinute, ToolCallCount, TurnFact, UsageMinute, call_blocking,
};

use super::agents::resolved_agent_id;
use super::{ApiResult, WebState, bad_request, internal_error, not_found};

/// Failures listed under Reliability.
const RECENT_FAILURE_LIMIT: usize = 20;
/// Request kind under which Sleep tokens are reported.
const SLEEP_KIND: &str = "sleep";

#[derive(Deserialize)]
pub(super) struct MetricsQuery {
    range: Option<String>,
    agent: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RangeKey {
    Day,
    Week,
    Month,
}

impl RangeKey {
    fn parse(value: Option<&str>) -> Option<Self> {
        match value.unwrap_or("7d") {
            "24h" => Some(Self::Day),
            "7d" => Some(Self::Week),
            "30d" => Some(Self::Month),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Day => "24h",
            Self::Week => "7d",
            Self::Month => "30d",
        }
    }
}

/// The local-time buckets a range is drawn with and the UTC window they span.
#[derive(Debug)]
struct Buckets {
    starts: Vec<DateTime<Utc>>,
    end: DateTime<Utc>,
    hourly: bool,
}

impl Buckets {
    /// Hourly buckets for `24h`, local days otherwise, ending at the bucket
    /// that contains `now`.
    fn new(range: RangeKey, now: DateTime<Utc>, tz: Tz) -> Self {
        let local_now = now.with_timezone(&tz);
        let (count, hourly) = match range {
            RangeKey::Day => (24, true),
            RangeKey::Week => (7, false),
            RangeKey::Month => (30, false),
        };
        let mut starts = Vec::with_capacity(count);
        for back in (0..count as i64).rev() {
            let start = if hourly {
                let hour = local_now - Duration::hours(back);
                tz.with_ymd_and_hms(hour.year(), hour.month(), hour.day(), hour.hour(), 0, 0)
                    .earliest()
            } else {
                let day = local_now.date_naive() - Duration::days(back);
                day.and_hms_opt(0, 0, 0)
                    .and_then(|midnight| tz.from_local_datetime(&midnight).earliest())
            };
            if let Some(start) = start {
                starts.push(start.with_timezone(&Utc));
            }
        }
        starts.dedup();
        Self {
            starts,
            end: now,
            hourly,
        }
    }

    fn start(&self) -> DateTime<Utc> {
        self.starts.first().copied().unwrap_or(self.end)
    }

    /// The bucket containing a UTC `YYYY-MM-DDTHH:MM` minute.
    fn index_of(&self, minute: &str) -> Option<usize> {
        let at = NaiveDateTime::parse_from_str(minute, "%Y-%m-%dT%H:%M")
            .ok()?
            .and_utc();
        if at < self.start() || at >= self.end {
            return None;
        }
        Some(
            self.starts
                .partition_point(|start| *start <= at)
                .saturating_sub(1),
        )
    }
}

fn rfc3339(at: DateTime<Utc>) -> String {
    at.to_rfc3339()
}

/// Everything the Metrics queries read from the database for one window.
struct WindowData {
    usage: Vec<UsageMinute>,
    sleep_usage: Vec<SleepUsageMinute>,
    pulse: Vec<PulseIntentionUsage>,
    turns: Vec<TurnFact>,
    tools: Vec<ToolCallCount>,
    sleep_runs: Vec<SleepRunCount>,
}

struct MetricsData {
    current: WindowData,
    previous_usage: Vec<UsageMinute>,
    previous_sleep_usage: Vec<SleepUsageMinute>,
    previous_turns: Vec<TurnFact>,
    failures: Vec<FailureRecord>,
    activity: Vec<AgentActivity>,
}

#[derive(Serialize, Default, Clone, Copy)]
struct UsageTotals {
    tokens: i64,
    input_tokens: i64,
    output_tokens: i64,
    cached_input_tokens: i64,
    cache_reported_input_tokens: i64,
    llm_calls: i64,
}

#[derive(Serialize)]
struct SeriesBucket {
    start: String,
    by_agent: BTreeMap<String, i64>,
    by_kind: BTreeMap<String, i64>,
}

#[derive(Serialize)]
struct KindUsage {
    kind: String,
    tokens: i64,
    llm_calls: i64,
}

#[derive(Serialize)]
struct ModelUsage {
    provider: String,
    model: String,
    llm_calls: i64,
    input_tokens: i64,
    output_tokens: i64,
    cached_input_tokens: i64,
    cache_reported_input_tokens: i64,
}

#[derive(Serialize)]
struct IntentionUsage {
    agent_id: String,
    intention_id: String,
    runs: i64,
    failed_runs: i64,
    input_tokens: i64,
    output_tokens: i64,
}

#[derive(Serialize, Default)]
struct TurnSummary {
    total: i64,
    previous_total: i64,
    completed: i64,
    failed: i64,
    cancelled: i64,
    uncertain: i64,
    running: i64,
    duration_p50_ms: Option<i64>,
    duration_p95_ms: Option<i64>,
}

#[derive(Serialize)]
struct KindCount {
    kind: String,
    count: i64,
}

#[derive(Serialize)]
struct ToolSummary {
    tool: String,
    calls: i64,
    failed: i64,
}

#[derive(Serialize)]
struct Failure {
    at: String,
    source: &'static str,
    agent_id: String,
    kind: String,
    summary: Option<String>,
    session_key: Option<String>,
    run_id: Option<String>,
}

#[derive(Serialize)]
struct AgentRow {
    agent_id: String,
    label: String,
    turns: i64,
    failed_turns: i64,
    tokens: i64,
    last_active_at: Option<String>,
    pulse_runs: i64,
    pulse_failed: i64,
    last_sleep_status: Option<String>,
}

/// Runtime-wide usage and reliability over a range.
///
/// Query parameters:
/// - `range` (optional, default `7d`): `24h`, `7d`, or `30d`
/// - `agent` (optional): narrows Usage and Reliability to one agent; the
///   agent table always lists every agent
///
/// Buckets follow `Config.timezone` (hours for `24h`, days otherwise). Only
/// the normal conversation scope is counted.
///
/// # Errors
///
/// Returns `400` for an unknown range, `404` for an unknown agent, and `500` on
/// database errors.
pub(super) async fn get_metrics(
    State(state): State<WebState>,
    Query(query): Query<MetricsQuery>,
) -> ApiResult {
    let range =
        RangeKey::parse(query.range.as_deref()).ok_or_else(|| bad_request("invalid_range"))?;
    let agent_filter = match query.agent.as_deref().filter(|id| !id.is_empty()) {
        Some(id) => Some(
            resolved_agent_id(&state, id)
                .ok_or_else(|| not_found("agent_not_found"))?
                .to_string(),
        ),
        None => None,
    };

    let snapshot = state.app_state.config_manager.current_blocking();
    let tz: Tz = snapshot.config.timezone.parse().unwrap_or(Tz::UTC);
    let buckets = Buckets::new(range, Utc::now(), tz);
    let start = rfc3339(buckets.start());
    let end = rfc3339(buckets.end);
    let previous_start = rfc3339(buckets.start() - (buckets.end - buckets.start()));

    let db = Arc::clone(&state.app_state.db);
    let failure_agent = agent_filter.clone();
    let data = call_blocking(db, move |db| {
        Ok(MetricsData {
            current: WindowData {
                usage: db.usage_by_minute(&start, &end)?,
                sleep_usage: db.sleep_usage_by_minute(&start, &end)?,
                pulse: db.pulse_intention_usage(&start, &end)?,
                turns: db.turn_facts(&start, &end)?,
                tools: db.tool_call_counts(&start, &end)?,
                sleep_runs: db.sleep_run_counts(&start, &end)?,
            },
            previous_usage: db.usage_by_minute(&previous_start, &start)?,
            previous_sleep_usage: db.sleep_usage_by_minute(&previous_start, &start)?,
            previous_turns: db.turn_facts(&previous_start, &start)?,
            failures: db.recent_failures(
                &start,
                &end,
                failure_agent.as_deref(),
                RECENT_FAILURE_LIMIT,
            )?,
            activity: db.agent_activity()?,
        })
    })
    .await
    .map_err(|error| {
        tracing::warn!(%error, "failed to aggregate metrics");
        internal_error()
    })?;

    let mut agent_labels: Vec<(String, String)> = snapshot
        .config
        .agents
        .iter()
        .map(|(id, agent)| (id.to_string(), agent.label.clone()))
        .collect();
    agent_labels.sort();

    let keep = |agent_id: &str| agent_filter.as_deref().is_none_or(|id| id == agent_id);

    Ok(Json(serde_json::json!({
        "ok": true,
        "range": {
            "key": range.as_str(),
            "start": rfc3339(buckets.start()),
            "end": rfc3339(buckets.end),
            "bucket": if buckets.hourly { "hour" } else { "day" },
            "timezone": tz.name(),
        },
        "agent": agent_filter,
        "usage": usage_payload(&buckets, &data, &keep),
        "reliability": reliability_payload(&data, &keep),
        "agents": agent_rows(&agent_labels, &data),
    })))
}

fn add_usage(totals: &mut UsageTotals, row: &UsageMinute) {
    totals.tokens += row.input_tokens + row.output_tokens;
    totals.input_tokens += row.input_tokens;
    totals.output_tokens += row.output_tokens;
    totals.cached_input_tokens += row.cached_input_tokens;
    totals.cache_reported_input_tokens += row.cache_reported_input_tokens;
    totals.llm_calls += row.calls;
}

fn total_tokens(
    usage: &[UsageMinute],
    sleep: &[SleepUsageMinute],
    keep: &impl Fn(&str) -> bool,
) -> i64 {
    usage
        .iter()
        .filter(|row| keep(&row.agent_id))
        .map(|row| row.input_tokens + row.output_tokens)
        .chain(
            sleep
                .iter()
                .filter(|row| keep(&row.agent_id))
                .map(|row| row.input_tokens + row.output_tokens),
        )
        .sum()
}

fn usage_payload(
    buckets: &Buckets,
    data: &MetricsData,
    keep: &impl Fn(&str) -> bool,
) -> serde_json::Value {
    let current = &data.current;
    let mut totals = UsageTotals::default();
    let mut series: Vec<SeriesBucket> = buckets
        .starts
        .iter()
        .map(|start| SeriesBucket {
            start: rfc3339(*start),
            by_agent: BTreeMap::new(),
            by_kind: BTreeMap::new(),
        })
        .collect();
    let mut kinds: HashMap<String, (i64, i64)> = HashMap::new();
    let mut models: HashMap<(String, String), ModelUsage> = HashMap::new();

    for row in current.usage.iter().filter(|row| keep(&row.agent_id)) {
        add_usage(&mut totals, row);
        let tokens = row.input_tokens + row.output_tokens;
        if let Some(bucket) = buckets
            .index_of(&row.minute)
            .and_then(|i| series.get_mut(i))
        {
            *bucket.by_agent.entry(row.agent_id.clone()).or_default() += tokens;
            *bucket.by_kind.entry(row.request_kind.clone()).or_default() += tokens;
        }
        let kind = kinds.entry(row.request_kind.clone()).or_default();
        kind.0 += tokens;
        kind.1 += row.calls;
        let model = models
            .entry((row.provider.clone(), row.model.clone()))
            .or_insert_with(|| ModelUsage {
                provider: row.provider.clone(),
                model: row.model.clone(),
                llm_calls: 0,
                input_tokens: 0,
                output_tokens: 0,
                cached_input_tokens: 0,
                cache_reported_input_tokens: 0,
            });
        model.llm_calls += row.calls;
        model.input_tokens += row.input_tokens;
        model.output_tokens += row.output_tokens;
        model.cached_input_tokens += row.cached_input_tokens;
        model.cache_reported_input_tokens += row.cache_reported_input_tokens;
    }

    for row in current.sleep_usage.iter().filter(|row| keep(&row.agent_id)) {
        let tokens = row.input_tokens + row.output_tokens;
        totals.tokens += tokens;
        totals.input_tokens += row.input_tokens;
        totals.output_tokens += row.output_tokens;
        if let Some(bucket) = buckets
            .index_of(&row.minute)
            .and_then(|i| series.get_mut(i))
        {
            *bucket.by_agent.entry(row.agent_id.clone()).or_default() += tokens;
            *bucket.by_kind.entry(SLEEP_KIND.to_string()).or_default() += tokens;
        }
        kinds.entry(SLEEP_KIND.to_string()).or_default().0 += tokens;
    }
    let sleep_runs: i64 = current
        .sleep_runs
        .iter()
        .filter(|row| keep(&row.agent_id))
        .map(|row| row.count)
        .sum();
    if let Some(sleep) = kinds.get_mut(SLEEP_KIND) {
        sleep.1 = sleep_runs;
    }

    let mut by_kind: Vec<KindUsage> = kinds
        .into_iter()
        .map(|(kind, (tokens, llm_calls))| KindUsage {
            kind,
            tokens,
            llm_calls,
        })
        .collect();
    by_kind.sort_by(|a, b| b.tokens.cmp(&a.tokens).then_with(|| a.kind.cmp(&b.kind)));
    let mut by_model: Vec<ModelUsage> = models.into_values().collect();
    by_model.sort_by(|a, b| {
        (b.input_tokens + b.output_tokens)
            .cmp(&(a.input_tokens + a.output_tokens))
            .then_with(|| a.model.cmp(&b.model))
    });

    let mut unattributed = (0, 0);
    let mut intentions: Vec<IntentionUsage> = Vec::new();
    for row in current.pulse.iter().filter(|row| keep(&row.agent_id)) {
        match &row.intention_id {
            Some(intention_id) => intentions.push(IntentionUsage {
                agent_id: row.agent_id.clone(),
                intention_id: intention_id.clone(),
                runs: row.runs,
                failed_runs: row.failed_runs,
                input_tokens: row.input_tokens,
                output_tokens: row.output_tokens,
            }),
            None => {
                unattributed.0 += row.input_tokens;
                unattributed.1 += row.output_tokens;
            }
        }
    }
    intentions.sort_by(|a, b| {
        (b.input_tokens + b.output_tokens)
            .cmp(&(a.input_tokens + a.output_tokens))
            .then_with(|| b.runs.cmp(&a.runs))
    });

    serde_json::json!({
        "totals": totals,
        "previous_tokens": total_tokens(&data.previous_usage, &data.previous_sleep_usage, keep),
        "series": series,
        "by_kind": by_kind,
        "by_model": by_model,
        "pulse_intentions": intentions,
        "pulse_unattributed": { "input_tokens": unattributed.0, "output_tokens": unattributed.1 },
    })
}

/// The nearest-rank percentile of sorted values.
fn percentile(sorted: &[i64], p: f64) -> Option<i64> {
    if sorted.is_empty() {
        return None;
    }
    let rank = ((p * sorted.len() as f64).ceil() as usize).clamp(1, sorted.len());
    Some(sorted[rank - 1])
}

fn turn_summary(
    turns: &[TurnFact],
    previous: &[TurnFact],
    keep: &impl Fn(&str) -> bool,
) -> TurnSummary {
    let mut summary = TurnSummary::default();
    let mut durations = Vec::new();
    for turn in turns.iter().filter(|turn| keep(&turn.agent_id)) {
        summary.total += 1;
        match turn.state.as_str() {
            "completed" => summary.completed += 1,
            "failed" => summary.failed += 1,
            "cancelled" => summary.cancelled += 1,
            "uncertain" => summary.uncertain += 1,
            _ => summary.running += 1,
        }
        if turn.state == "completed"
            && let Some(ms) = turn.duration_ms
        {
            durations.push(ms);
        }
    }
    durations.sort_unstable();
    summary.duration_p50_ms = percentile(&durations, 0.5);
    summary.duration_p95_ms = percentile(&durations, 0.95);
    summary.previous_total = previous.iter().filter(|turn| keep(&turn.agent_id)).count() as i64;
    summary
}

fn sorted_counts(counts: HashMap<String, i64>) -> Vec<KindCount> {
    let mut counts: Vec<KindCount> = counts
        .into_iter()
        .map(|(kind, count)| KindCount { kind, count })
        .collect();
    counts.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.kind.cmp(&b.kind)));
    counts
}

fn reliability_payload(data: &MetricsData, keep: &impl Fn(&str) -> bool) -> serde_json::Value {
    let current = &data.current;
    let turns = turn_summary(&current.turns, &data.previous_turns, keep);

    let mut turn_kinds: HashMap<String, i64> = HashMap::new();
    for turn in current.turns.iter().filter(|turn| keep(&turn.agent_id)) {
        if matches!(turn.state.as_str(), "failed" | "uncertain") {
            let kind = turn
                .error_kind
                .clone()
                .unwrap_or_else(|| turn.state.clone());
            *turn_kinds.entry(kind).or_default() += 1;
        }
    }

    let mut tools: HashMap<String, (i64, i64)> = HashMap::new();
    let mut tool_kinds: HashMap<String, i64> = HashMap::new();
    for row in current.tools.iter().filter(|row| keep(&row.agent_id)) {
        let tool = tools.entry(row.tool_name.clone()).or_default();
        tool.0 += row.count;
        if row.state != "succeeded" {
            tool.1 += row.count;
            let kind = row.error_kind.clone().unwrap_or_else(|| row.state.clone());
            *tool_kinds.entry(kind).or_default() += row.count;
        }
    }
    let mut tools: Vec<ToolSummary> = tools
        .into_iter()
        .map(|(tool, (calls, failed))| ToolSummary {
            tool,
            calls,
            failed,
        })
        .collect();
    tools.sort_by(|a, b| {
        b.failed
            .cmp(&a.failed)
            .then_with(|| b.calls.cmp(&a.calls))
            .then_with(|| a.tool.cmp(&b.tool))
    });

    let (mut pulse_total, mut pulse_failed) = (0, 0);
    for row in current.pulse.iter().filter(|row| keep(&row.agent_id)) {
        pulse_total += row.runs;
        pulse_failed += row.failed_runs;
    }
    let (mut sleep_total, mut sleep_failed, mut sleep_partial) = (0, 0, 0);
    for row in current.sleep_runs.iter().filter(|row| keep(&row.agent_id)) {
        sleep_total += row.count;
        match row.status.as_str() {
            "failed" => sleep_failed += row.count,
            "partial_failure" => sleep_partial += row.count,
            _ => {}
        }
    }

    let failures: Vec<Failure> = data
        .failures
        .iter()
        .map(|failure| Failure {
            at: failure.at.clone(),
            source: match failure.source {
                FailureSource::Turn => "turn",
                FailureSource::Pulse => "pulse",
                FailureSource::Sleep => "sleep",
            },
            agent_id: failure.agent_id.clone(),
            kind: failure.kind.clone(),
            summary: failure.summary.clone(),
            session_key: failure.chat_id.map(|id| format!("chat:{id}")),
            run_id: failure.run_id.clone(),
        })
        .collect();

    serde_json::json!({
        "turns": turns,
        "turn_failure_kinds": sorted_counts(turn_kinds),
        "tools": tools,
        "tool_failure_kinds": sorted_counts(tool_kinds),
        "runs": {
            "pulse": { "total": pulse_total, "failed": pulse_failed },
            "sleep": { "total": sleep_total, "failed": sleep_failed, "partial_failure": sleep_partial },
        },
        "recent_failures": failures,
    })
}

fn agent_rows(labels: &[(String, String)], data: &MetricsData) -> Vec<AgentRow> {
    let current = &data.current;
    labels
        .iter()
        .map(|(agent_id, label)| {
            let is_agent = |id: &str| id == agent_id;
            let turns: Vec<&TurnFact> = current
                .turns
                .iter()
                .filter(|t| is_agent(&t.agent_id))
                .collect();
            let activity = data.activity.iter().find(|a| is_agent(&a.agent_id));
            let pulse = current.pulse.iter().filter(|row| is_agent(&row.agent_id));
            let (pulse_runs, pulse_failed) = pulse.fold((0, 0), |acc, row| {
                (acc.0 + row.runs, acc.1 + row.failed_runs)
            });
            AgentRow {
                agent_id: agent_id.clone(),
                label: label.clone(),
                turns: turns.len() as i64,
                failed_turns: turns
                    .iter()
                    .filter(|t| matches!(t.state.as_str(), "failed" | "uncertain"))
                    .count() as i64,
                tokens: total_tokens(&current.usage, &current.sleep_usage, &is_agent),
                last_active_at: activity.and_then(|a| a.last_turn_at.clone()),
                pulse_runs,
                pulse_failed,
                last_sleep_status: activity.and_then(|a| a.last_sleep_status.clone()),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utc(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s)
            .expect("rfc3339")
            .with_timezone(&Utc)
    }

    #[test]
    fn daily_buckets_start_at_local_midnight() {
        let tz: Tz = "Asia/Kolkata".parse().expect("tz");
        let buckets = Buckets::new(RangeKey::Week, utc("2026-10-10T12:00:00Z"), tz);

        assert_eq!(buckets.starts.len(), 7);
        assert_eq!(buckets.starts[6], utc("2026-10-09T18:30:00Z"));
        assert_eq!(buckets.starts[0], utc("2026-10-03T18:30:00Z"));
        assert_eq!(buckets.index_of("2026-10-09T18:29"), Some(5));
        assert_eq!(buckets.index_of("2026-10-09T18:30"), Some(6));
        assert_eq!(buckets.index_of("2026-10-03T18:29"), None);
    }

    #[test]
    fn hourly_buckets_cover_the_last_24_hours() {
        let buckets = Buckets::new(RangeKey::Day, utc("2026-10-10T12:34:00Z"), Tz::UTC);

        assert_eq!(buckets.starts.len(), 24);
        assert_eq!(buckets.starts[23], utc("2026-10-10T12:00:00Z"));
        assert_eq!(buckets.index_of("2026-10-10T12:34"), None);
        assert_eq!(buckets.index_of("2026-10-10T12:33"), Some(23));
    }

    #[test]
    fn percentile_uses_nearest_rank() {
        assert_eq!(percentile(&[], 0.5), None);
        assert_eq!(percentile(&[10, 20, 30, 40], 0.5), Some(20));
        assert_eq!(percentile(&[10, 20, 30, 40], 0.95), Some(40));
    }
}
