//! Rollup Planner — pure Rust logic that determines which week/month rollups
//! need updating for the Call 2 episodic-view system.
//!
//! This module is intentionally free of DB/LLM dependencies so that every
//! detection rule can be unit-tested with plain data.

use chrono::{DateTime, Datelike, Duration, FixedOffset, NaiveDate, Offset};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use tracing::warn;

fn to_fixed(dt: DateTime<chrono_tz::Tz>) -> DateTime<FixedOffset> {
    dt.with_timezone(&dt.offset().fix())
}

use crate::storage::RollupGranularity;

// ---------------------------------------------------------------------------
// Data structures
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WeekPeriod {
    pub week_key: String,
    pub period_start: DateTime<FixedOffset>,
    pub period_end_exclusive: DateTime<FixedOffset>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MonthPeriod {
    pub month_key: String,
    pub period_start: DateTime<FixedOffset>,
    pub period_end_exclusive: DateTime<FixedOffset>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RollupRequest {
    pub granularity: RollupGranularity,
    pub period_key: String,
    pub period_start: String,
    pub period_end_exclusive: String,
    pub reason: String,
    pub previous_summary_md: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExistingRollupInfo {
    pub period_key: String,
    pub event_count: i64,
    pub max_ripple: i64,
    pub summary_md: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PlannerEvent {
    pub experienced_at: String,
    pub encoded_at: String,
    pub ripple_strength: i64,
}

pub(crate) struct RollupPlannerInput {
    pub existing_week_rollups: Vec<ExistingRollupInfo>,
    pub events: Vec<PlannerEvent>,
}

// ---------------------------------------------------------------------------
// Helper functions
// ---------------------------------------------------------------------------

pub(crate) fn current_week(now: DateTime<FixedOffset>, tz: chrono_tz::Tz) -> WeekPeriod {
    let monday = monday_of(now.date_naive());
    week_for_date_inner(monday, tz)
}

/// Closed weeks before current, most-recent first (W-1, W-2, ...).
pub(crate) fn recent_weeks(
    now: DateTime<FixedOffset>,
    count: usize,
    tz: chrono_tz::Tz,
) -> Vec<WeekPeriod> {
    let cur = current_week(now, tz);
    let mut weeks = Vec::with_capacity(count);
    let mut monday = cur.period_start.date_naive();
    for _ in 0..count {
        monday -= Duration::days(7);
        weeks.push(week_for_date_inner(monday, tz));
    }
    weeks
}

/// Complete calendar months ending before `now`, most-recent first.
///
/// Returns full calendar months suitable for end-of-month detection.
pub(crate) fn complete_months_recent(
    now: DateTime<FixedOffset>,
    count: usize,
    tz: chrono_tz::Tz,
) -> Vec<MonthPeriod> {
    let (prev_y, prev_m) = if now.month() == 1 {
        (now.year() - 1, 12)
    } else {
        (now.year(), now.month() - 1)
    };
    let mut months = Vec::with_capacity(count);
    let mut y = prev_y;
    let mut m = prev_m;
    for _ in 0..count {
        months.push(month_for_ym(y, m, tz));
        m = m.saturating_sub(1);
        if m == 0 {
            m = 12;
            y -= 1;
        }
    }
    months
}

fn week_for_date_inner(monday: NaiveDate, tz: chrono_tz::Tz) -> WeekPeriod {
    let ps: DateTime<chrono_tz::Tz> = monday
        .and_hms_opt(0, 0, 0)
        .expect("midnight is valid")
        .and_local_timezone(tz)
        .unwrap();
    let next_monday = monday + Duration::days(7);
    let pe: DateTime<chrono_tz::Tz> = next_monday
        .and_hms_opt(0, 0, 0)
        .expect("midnight is valid")
        .and_local_timezone(tz)
        .unwrap();
    WeekPeriod {
        week_key: format!(
            "{}-W{:02}",
            monday.iso_week().year(),
            monday.iso_week().week()
        ),
        period_start: to_fixed(ps),
        period_end_exclusive: to_fixed(pe),
    }
}

fn month_for_ym(year: i32, month: u32, tz: chrono_tz::Tz) -> MonthPeriod {
    let month_key = format!("{year}-{month:02}");
    let first = NaiveDate::from_ymd_opt(year, month, 1).expect("valid year/month");
    let ps: DateTime<chrono_tz::Tz> = first
        .and_hms_opt(0, 0, 0)
        .expect("midnight is valid")
        .and_local_timezone(tz)
        .unwrap();
    let (ny, nm) = if month == 12 {
        (year + 1, 1)
    } else {
        (year, month + 1)
    };
    let next_first = NaiveDate::from_ymd_opt(ny, nm, 1).expect("valid year/month");
    let pe: DateTime<chrono_tz::Tz> = next_first
        .and_hms_opt(0, 0, 0)
        .expect("midnight is valid")
        .and_local_timezone(tz)
        .unwrap();
    MonthPeriod {
        month_key,
        period_start: to_fixed(ps),
        period_end_exclusive: to_fixed(pe),
    }
}

pub(crate) fn month_period_from_key(key: &str, tz: chrono_tz::Tz) -> Option<MonthPeriod> {
    let (year, month) = parse_ym_key(key)?;
    Some(month_for_ym(year, month, tz))
}

fn parse_ym_key(key: &str) -> Option<(i32, u32)> {
    let (year_str, month_str) = key.split_once('-')?;
    let year: i32 = year_str.parse().ok()?;
    let month: u32 = month_str.parse().ok()?;
    if !(1..=12).contains(&month) {
        return None;
    }
    Some((year, month))
}

fn monday_of(date: NaiveDate) -> NaiveDate {
    let wd = date.weekday().num_days_from_monday();
    date - Duration::days(i64::from(wd))
}

fn parse_rfc3339(s: &str) -> Option<DateTime<FixedOffset>> {
    DateTime::parse_from_rfc3339(s).ok()
}

fn count_events_in_period(
    events: &[PlannerEvent],
    start: &DateTime<FixedOffset>,
    end: &DateTime<FixedOffset>,
) -> usize {
    events
        .iter()
        .filter(|e| {
            let Some(ts) = parse_rfc3339(&e.experienced_at) else {
                return false;
            };
            ts >= *start && ts < *end
        })
        .count()
}

// ---------------------------------------------------------------------------
// Main planner
// ---------------------------------------------------------------------------

pub(crate) fn plan_week_rollup_updates(
    _agent_id: &str,
    now: DateTime<FixedOffset>,
    tz: chrono_tz::Tz,
    input: &RollupPlannerInput,
) -> Vec<RollupRequest> {
    let mut requests: Vec<RollupRequest> = Vec::new();
    let mut seen_keys: HashSet<String> = HashSet::new();

    let cur_week = current_week(now, tz);
    let recent = recent_weeks(now, 4, tz);

    let existing_week_keys: HashSet<&str> = input
        .existing_week_rollups
        .iter()
        .map(|r| r.period_key.as_str())
        .collect();

    // -----------------------------------------------------------------------
    // 1. New closed week (W-1)
    // -----------------------------------------------------------------------
    if let Some(w1) = recent.first() {
        if !existing_week_keys.contains(w1.week_key.as_str()) {
            let key = format!("week:{}", w1.week_key);
            if seen_keys.insert(key) {
                requests.push(make_week_request(w1, "closed_week", None));
            }
        }
    }

    // -----------------------------------------------------------------------
    // 2. Missing week rollup (any of recent 4)
    // -----------------------------------------------------------------------
    for wk in &recent {
        if !existing_week_keys.contains(wk.week_key.as_str()) {
            let key = format!("week:{}", wk.week_key);
            if seen_keys.insert(key) {
                requests.push(make_week_request(wk, "missing_week", None));
            }
        }
    }

    // -----------------------------------------------------------------------
    // 3. Delayed events: recent encoded_at but experienced_at in a closed week
    // -----------------------------------------------------------------------
    let recent_threshold = now - Duration::days(2);
    let delayed_events: Vec<&PlannerEvent> = input
        .events
        .iter()
        .filter(|e| {
            let Some(enc) = parse_rfc3339(&e.encoded_at) else {
                return false;
            };
            enc >= recent_threshold
        })
        .collect();

    for ev in &delayed_events {
        let Some(exp) = parse_rfc3339(&ev.experienced_at) else {
            continue;
        };
        // Only care about events experienced in a closed week.
        if exp >= cur_week.period_start {
            continue;
        }
        for wk in &recent {
            if exp >= wk.period_start
                && exp < wk.period_end_exclusive
                && existing_week_keys.contains(wk.week_key.as_str())
            {
                let key = format!("week:{}", wk.week_key);
                if seen_keys.insert(key) {
                    let prev = input
                        .existing_week_rollups
                        .iter()
                        .find(|r| r.period_key == wk.week_key)
                        .map(|r| r.summary_md.clone());
                    requests.push(make_week_request(wk, "delayed_events", prev));
                }
            }
        }
    }

    // -----------------------------------------------------------------------
    // 4. Event count mismatch
    // -----------------------------------------------------------------------
    for rollup in &input.existing_week_rollups {
        let Some(wk) = recent
            .iter()
            .chain(std::iter::once(&cur_week))
            .find(|w| w.week_key == rollup.period_key)
        else {
            continue;
        };
        let actual =
            count_events_in_period(&input.events, &wk.period_start, &wk.period_end_exclusive);
        if i64::try_from(actual).unwrap_or(i64::MAX) != rollup.event_count {
            let key = format!("week:{}", wk.week_key);
            if seen_keys.insert(key) {
                requests.push(make_week_request(
                    wk,
                    "event_count_mismatch",
                    Some(rollup.summary_md.clone()),
                ));
            }
        }
    }

    requests
}

/// Plan month rollup updates based on the new trigger:
/// month has ended AND at least one week rollup exists for that month AND no existing month rollup.
pub(crate) fn plan_month_rollup_updates(
    _agent_id: &str,
    now: DateTime<FixedOffset>,
    tz: chrono_tz::Tz,
    existing_month_rollups: &[ExistingRollupInfo],
    existing_week_rollups: &[ExistingRollupInfo],
) -> Vec<RollupRequest> {
    let mut requests = Vec::new();

    let existing_month_map: HashMap<&str, &ExistingRollupInfo> = existing_month_rollups
        .iter()
        .map(|r| (r.period_key.as_str(), r))
        .collect();

    let complete_months = complete_months_recent(now, 2, tz);

    for mp in &complete_months {
        if now < mp.period_end_exclusive {
            continue;
        }

        let month_weeks: Vec<&ExistingRollupInfo> = existing_week_rollups
            .iter()
            .filter(|wr| week_in_month(&wr.period_key, &mp.month_key, tz))
            .collect();

        if month_weeks.is_empty() {
            continue;
        }

        if let Some(existing) = existing_month_map.get(mp.month_key.as_str()) {
            let (computed_max, computed_count) = compute_month_rollup_stats(&month_weeks);
            if computed_count == existing.event_count && computed_max == existing.max_ripple {
                continue;
            }
            requests.push(make_month_request(
                mp,
                "month_stale",
                Some(existing.summary_md.clone()),
            ));
        } else {
            requests.push(make_month_request(mp, "month_end", None));
        }
    }

    requests
}

/// Check if a week (ISO week key like "2026-W14") belongs to a month (key like "2026-04").
/// A week belongs to a month if the week's Monday falls within the month period.
pub(crate) fn week_in_month(week_key: &str, month_key: &str, tz: chrono_tz::Tz) -> bool {
    let Some(mp) = month_period_from_key(month_key, tz) else {
        return false;
    };
    let (year, week_num) = parse_iso_week_key(week_key);
    let Some(monday) = NaiveDate::from_isoywd_opt(year, week_num, chrono::Weekday::Mon) else {
        return false;
    };
    let week_start: DateTime<FixedOffset> = to_fixed(
        monday
            .and_hms_opt(0, 0, 0)
            .expect("midnight is valid")
            .and_local_timezone(tz)
            .unwrap(),
    );
    week_start >= mp.period_start && week_start < mp.period_end_exclusive
}

fn parse_iso_week_key(key: &str) -> (i32, u32) {
    let parts: Vec<&str> = key.split('-').collect();
    if parts.len() != 2 {
        return (0, 0);
    }
    let year: i32 = parts[0].parse().unwrap_or(0);
    let week_str = parts[1].strip_prefix('W').unwrap_or(parts[1]);
    let week_num: u32 = week_str.parse().unwrap_or(0);
    (year, week_num)
}

// ---------------------------------------------------------------------------
// Request builders
// ---------------------------------------------------------------------------

fn make_week_request(
    wp: &WeekPeriod,
    reason: &str,
    previous_summary_md: Option<String>,
) -> RollupRequest {
    RollupRequest {
        granularity: RollupGranularity::Week,
        period_key: wp.week_key.clone(),
        period_start: wp.period_start.to_rfc3339(),
        period_end_exclusive: wp.period_end_exclusive.to_rfc3339(),
        reason: reason.to_string(),
        previous_summary_md,
    }
}

fn make_month_request(
    mp: &MonthPeriod,
    reason: &str,
    previous_summary_md: Option<String>,
) -> RollupRequest {
    RollupRequest {
        granularity: RollupGranularity::Month,
        period_key: mp.month_key.clone(),
        period_start: mp.period_start.to_rfc3339(),
        period_end_exclusive: mp.period_end_exclusive.to_rfc3339(),
        reason: reason.to_string(),
        previous_summary_md,
    }
}

// ---------------------------------------------------------------------------
// Call2 LLM Integration — Input Builder, Output Parser, Validation, Security
// ---------------------------------------------------------------------------

/// Maximum allowed length for `summary_md` in characters.
const SUMMARY_MD_MAX_LEN: usize = 10_000;

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Errors that can occur during Call2 LLM output parsing and validation.
#[derive(Debug, thiserror::Error)]
pub(crate) enum Call2Error {
    #[error("json parse failed: {0}")]
    JsonParse(String),
    #[error("validation failed: {0}")]
    Validation(String),
}

// ---------------------------------------------------------------------------
// Call2 Input / Output types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub(crate) struct Call2RollupRequest {
    pub granularity: String,
    pub period_key: String,
    pub period_start: String,
    pub period_end_exclusive: String,
    pub reason: String,
    pub previous_summary_md: Option<String>,
    pub events: Vec<Call2Event>,
    pub week_rollups: Vec<Call2WeekRollupSummary>,
    pub previous_month_summary_md: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct Call2Event {
    pub id: String,
    pub experienced_at: String,
    pub kind: String,
    pub title: String,
    pub body_md: String,
    pub ripple_strength: i64,
    pub certainty: String,
}

/// A week rollup summary used as input for month rollup generation.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct Call2WeekRollupSummary {
    pub period_key: String,
    pub summary_md: String,
    pub max_ripple: i64,
    pub event_count: i64,
}

/// Call2 LLM output JSON structure.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Call2Output {
    pub rollups: Vec<Call2RollupOutput>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct Call2RollupOutput {
    pub granularity: String,
    pub period_key: String,
    pub summary_md: String,
    pub max_ripple: i64,
    pub event_count: i64,
}

// ---------------------------------------------------------------------------
// Input builder
// ---------------------------------------------------------------------------

/// Builds the Call2 input from rollup requests and their associated events.
///
/// The caller populates `events_map` from DB queries keyed by `period_key`.
pub(crate) fn build_call2_input(
    rollup_requests: &[RollupRequest],
    events_map: &HashMap<String, Vec<Call2Event>>,
) -> Vec<Call2RollupRequest> {
    rollup_requests
        .iter()
        .map(|req| {
            let events = events_map.get(&req.period_key).cloned().unwrap_or_default();
            Call2RollupRequest {
                granularity: granularity_to_str(req.granularity),
                period_key: req.period_key.clone(),
                period_start: req.period_start.clone(),
                period_end_exclusive: req.period_end_exclusive.clone(),
                reason: req.reason.clone(),
                previous_summary_md: req.previous_summary_md.clone(),
                events,
                week_rollups: Vec::new(),
                previous_month_summary_md: None,
            }
        })
        .collect()
}

fn granularity_to_str(g: RollupGranularity) -> String {
    match g {
        RollupGranularity::Week => "week".to_string(),
        RollupGranularity::Month => "month".to_string(),
    }
}

/// Builds the Call2 input for month rollup requests using week rollup summaries.
///
/// Unlike `build_call2_input` which uses raw events, this uses week rollup
/// summaries as input for month-level summarization.
pub(crate) fn build_call2_input_month(
    rollup_requests: &[RollupRequest],
    week_rollups_map: &HashMap<String, Vec<Call2WeekRollupSummary>>,
    previous_month_map: &HashMap<String, String>,
) -> Vec<Call2RollupRequest> {
    rollup_requests
        .iter()
        .map(|req| {
            let week_rollups = week_rollups_map
                .get(&req.period_key)
                .cloned()
                .unwrap_or_default();
            let previous_month_summary_md = previous_month_map.get(&req.period_key).cloned();
            Call2RollupRequest {
                granularity: granularity_to_str(req.granularity),
                period_key: req.period_key.clone(),
                period_start: req.period_start.clone(),
                period_end_exclusive: req.period_end_exclusive.clone(),
                reason: req.reason.clone(),
                previous_summary_md: req.previous_summary_md.clone(),
                events: Vec::new(),
                week_rollups,
                previous_month_summary_md,
            }
        })
        .collect()
}

/// Computes month rollup statistics (max_ripple, event_count) by aggregating
/// the statistics of the week rollups belonging to that month.
///
/// Uses the maximum `max_ripple` and the sum of `event_count` from all
/// provided week rollups.
pub(crate) fn compute_month_rollup_stats(week_rollups: &[&ExistingRollupInfo]) -> (i64, i64) {
    let max_ripple = week_rollups
        .iter()
        .map(|wr| wr.max_ripple)
        .max()
        .unwrap_or(3);
    let event_count = week_rollups.iter().map(|wr| wr.event_count).sum();
    (max_ripple, event_count)
}

pub(crate) fn compute_rollup_stats(events: Option<&Vec<Call2Event>>) -> (i64, i64) {
    let slice = events.map(|v| v.as_slice()).unwrap_or(&[]);
    let max_ripple = slice.iter().map(|e| e.ripple_strength).max().unwrap_or(3);
    let event_count = i64::try_from(slice.len()).unwrap_or(0);
    (max_ripple, event_count)
}

// ---------------------------------------------------------------------------
// Output parser + validator
// ---------------------------------------------------------------------------

/// Parse and validate Call2 LLM output.
///
/// Returns valid rollups. Rollups with empty `summary_md` are skipped
/// with a warning. All other validation failures produce an error.
///
/// # Errors
///
/// Returns [`Call2Error::JsonParse`] when the input is not valid JSON or
/// the structure does not match [`Call2Output`].
/// Returns [`Call2Error::Validation`] when a rollup field violates constraints.
pub(crate) fn parse_call2_output(
    json_str: &str,
    valid_period_keys: &HashSet<String>,
) -> Result<Vec<Call2RollupOutput>, Call2Error> {
    let output: Call2Output = serde_json::from_str(json_str)
        .map_err(|e| Call2Error::JsonParse(format!("failed to parse Call2 output JSON: {e}")))?;

    let mut valid_rollups = Vec::with_capacity(output.rollups.len());

    for rollup in output.rollups {
        validate_granularity(&rollup.granularity)?;
        validate_period_key(&rollup.period_key, valid_period_keys)?;
        if skip_empty_summary(&rollup) {
            continue;
        }
        validate_summary_length(&rollup.summary_md)?;
        validate_no_event_ids(&rollup.summary_md)?;
        validate_max_ripple(rollup.max_ripple)?;
        validate_event_count(rollup.event_count)?;
        valid_rollups.push(rollup);
    }

    Ok(valid_rollups)
}

fn validate_granularity(g: &str) -> Result<(), Call2Error> {
    if g != "week" && g != "month" {
        return Err(Call2Error::Validation(format!(
            "invalid granularity: {g:?} (expected \"week\" or \"month\")"
        )));
    }
    Ok(())
}

fn validate_period_key(key: &str, valid: &HashSet<String>) -> Result<(), Call2Error> {
    if !valid.contains(key) {
        return Err(Call2Error::Validation(format!(
            "unknown period_key: {key:?}"
        )));
    }
    Ok(())
}

/// Returns `true` if the rollup should be silently skipped.
fn skip_empty_summary(rollup: &Call2RollupOutput) -> bool {
    if rollup.summary_md.trim().is_empty() {
        warn!(period_key = %rollup.period_key, "skipping rollup with empty summary_md");
        true
    } else {
        false
    }
}

fn validate_summary_length(summary: &str) -> Result<(), Call2Error> {
    if summary.len() > SUMMARY_MD_MAX_LEN {
        return Err(Call2Error::Validation(format!(
            "summary_md too long: {} chars (max {SUMMARY_MD_MAX_LEN})",
            summary.len()
        )));
    }
    Ok(())
}

fn validate_no_event_ids(summary: &str) -> Result<(), Call2Error> {
    if summary.contains("evt_") {
        return Err(Call2Error::Validation(
            "summary_md must not contain event ID references (evt_ prefix)".to_string(),
        ));
    }
    Ok(())
}

fn validate_max_ripple(v: i64) -> Result<(), Call2Error> {
    if !(1..=5).contains(&v) {
        return Err(Call2Error::Validation(format!(
            "max_ripple out of range: {v} (expected 1-5)"
        )));
    }
    Ok(())
}

fn validate_event_count(v: i64) -> Result<(), Call2Error> {
    if v < 0 {
        return Err(Call2Error::Validation(format!("negative event_count: {v}")));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Security redaction
// ---------------------------------------------------------------------------

/// Redacts potential secrets from text.
///
/// Detects and replaces common secret patterns:
/// - OpenAI-style API keys (`sk-...`)
/// - Bearer tokens (`Bearer ...`)
/// - Key-value secrets (`api_key=...`, `token=...`, `password=...`)
pub(crate) fn redact_secrets(text: &str) -> String {
    let mut result = text.to_string();
    result = redact_prefixed_values(&result, "sk-", is_secret_value_char);
    result = redact_prefixed_values(&result, "Bearer ", |c| !c.is_whitespace());
    for key in ["api_key", "token", "password"] {
        for sep in ["=", ":"] {
            let pattern = format!("{key}{sep}");
            result = redact_prefixed_values(&result, &pattern, |c| {
                !c.is_whitespace() && c != ',' && c != '"' && c != '\'' && c != ')' && c != ']'
            });
        }
    }
    result
}

/// Returns `true` for characters that may appear in a secret value.
fn is_secret_value_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '-' || c == '.'
}

/// Replaces the value portion after each occurrence of `prefix` with `[REDACTED]`.
fn redact_prefixed_values(
    text: &str,
    prefix: &str,
    is_value_char: impl Fn(char) -> bool,
) -> String {
    let mut result = String::with_capacity(text.len());
    let mut remaining = text;

    while let Some(pos) = remaining.find(prefix) {
        result.push_str(&remaining[..pos]);
        result.push_str(prefix);
        let after_prefix = &remaining[pos + prefix.len()..];
        let value_end = after_prefix
            .char_indices()
            .find(|(_, c)| !is_value_char(*c))
            .map(|(i, _)| i)
            .unwrap_or(after_prefix.len());
        if value_end > 0 {
            result.push_str("[REDACTED]");
        }
        remaining = &after_prefix[value_end..];
    }
    result.push_str(remaining);
    result
}

// ---------------------------------------------------------------------------
// Prompt builders
// ---------------------------------------------------------------------------

/// Builds the Call2 system prompt for week rollups from the embedded week prompt template.
pub(crate) fn build_call2_system_prompt_week(agent_id: &str) -> String {
    include_str!("prompts/rollup_week_prompt.md").replace("{AGENT_NAME}", agent_id)
}

/// Builds the Call2 system prompt for month rollups from the embedded month prompt template.
pub(crate) fn build_call2_system_prompt_month(agent_id: &str) -> String {
    include_str!("prompts/rollup_month_prompt.md").replace("{AGENT_NAME}", agent_id)
}

/// Builds the Call2 user prompt with the input JSON.
pub(crate) fn build_call2_user_prompt(input_json: &str) -> String {
    format!(
        "以下の Call2 入力 JSON に基づいて、必要な episode_rollups を生成してください。\n\n\
         重要:\n\
         - 出力は JSON のみです。\n\
         - rollups 以外のトップレベルキーを出してはいけません。\n\
         - summary_md は Markdown bullet のみです。\n\
         - episodic.md 全文は生成しないでください。\n\n\
         入力 JSON:\n{input_json}"
    )
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Weekday;

    /// JST (UTC+9) — the timezone used throughout these tests.
    fn jst() -> chrono_tz::Tz {
        chrono_tz::Asia::Tokyo
    }

    /// Helper: create a `DateTime<FixedOffset>` in JST from `(year, month, day, hour, min)`.
    fn jst_dt(y: i32, m: u32, d: u32, hh: u32, mm: u32) -> DateTime<FixedOffset> {
        let naive = chrono::NaiveDate::from_ymd_opt(y, m, d)
            .unwrap()
            .and_hms_opt(hh, mm, 0)
            .unwrap();
        let tz_dt: DateTime<chrono_tz::Tz> = naive.and_local_timezone(jst()).unwrap();
        to_fixed(tz_dt)
    }

    // -----------------------------------------------------------------------
    // Test 1: current_week — Monday start
    // -----------------------------------------------------------------------
    #[test]
    fn test_current_week_monday_start() {
        // Wednesday 2026-05-27 10:00 JST
        let now = jst_dt(2026, 5, 27, 10, 0);
        let cw = current_week(now, jst());

        assert_eq!(cw.period_start.weekday(), Weekday::Mon);
        assert_eq!(cw.period_start.day(), 25); // Monday 2026-05-25
        assert_eq!(cw.period_end_exclusive.weekday(), Weekday::Mon);
        assert_eq!(cw.period_end_exclusive.day(), 1); // 2026-06-01
        assert!(cw.week_key.starts_with("2026-W"));
    }

    #[test]
    fn test_current_week_starts_at_local_midnight() {
        let now = jst_dt(2026, 5, 27, 10, 0);
        let cw = current_week(now, jst());

        assert_eq!(
            cw.period_start,
            jst_dt(2026, 5, 25, 0, 0),
            "current week should start at Monday 00:00 local time"
        );
        assert_eq!(
            cw.period_end_exclusive,
            jst_dt(2026, 6, 1, 0, 0),
            "current week should end at next Monday 00:00 local time"
        );
    }

    #[test]
    fn test_monday_morning_event_in_current_week() {
        let now = jst_dt(2026, 5, 27, 10, 0);
        let cw = current_week(now, jst());

        let monday_morning = jst_dt(2026, 5, 25, 8, 0);
        assert!(
            monday_morning >= cw.period_start,
            "Monday 08:00 should be >= week start"
        );
        assert!(
            monday_morning < cw.period_end_exclusive,
            "Monday 08:00 should be < week end"
        );
    }

    #[test]
    fn test_month_for_ym_starts_at_local_midnight() {
        let mp = month_for_ym(2026, 5, jst());

        assert_eq!(
            mp.period_start,
            jst_dt(2026, 5, 1, 0, 0),
            "May 2026 should start at 2026-05-01T00:00:00+09:00"
        );
        assert_eq!(
            mp.period_end_exclusive,
            jst_dt(2026, 6, 1, 0, 0),
            "May 2026 should end at 2026-06-01T00:00:00+09:00"
        );
    }

    // -----------------------------------------------------------------------
    // Test 2: recent_weeks — identifies 4 closed weeks
    // -----------------------------------------------------------------------
    #[test]
    fn test_recent_weeks_identifies_4_closed() {
        let now = jst_dt(2026, 5, 27, 10, 0);
        let weeks = recent_weeks(now, 4, jst());

        assert_eq!(weeks.len(), 4);
        // Most recent first.
        assert_eq!(
            weeks[0].period_end_exclusive,
            current_week(now, jst()).period_start
        );
        // Each is 7 days long.
        for w in &weeks {
            let dur = w.period_end_exclusive - w.period_start;
            assert_eq!(dur, Duration::days(7));
        }
        // Strictly decreasing.
        for pair in weeks.windows(2) {
            assert!(pair[0].period_start > pair[1].period_start);
        }
    }

    // -----------------------------------------------------------------------
    // Test 4: detects new closed week (W-1 has no rollup)
    // -----------------------------------------------------------------------
    #[test]
    fn test_detects_new_closed_week() {
        let now = jst_dt(2026, 5, 27, 10, 0);
        let recent = recent_weeks(now, 4, jst());

        // No week rollups at all → W-1 should be detected as closed_week.
        let input = RollupPlannerInput {
            existing_week_rollups: vec![],

            events: vec![],
        };
        let reqs = plan_week_rollup_updates("test-agent", now, jst(), &input);

        let w1_key = &recent[0].week_key;
        let closed = reqs
            .iter()
            .find(|r| r.period_key == *w1_key && r.reason == "closed_week");
        assert!(
            closed.is_some(),
            "should detect W-1 ({w1_key}) as closed_week"
        );
    }

    // -----------------------------------------------------------------------
    // Test 5: detects missing week rollup (W-2 has no rollup)
    // -----------------------------------------------------------------------
    #[test]
    fn test_detects_missing_week_rollup() {
        let now = jst_dt(2026, 5, 27, 10, 0);
        let recent = recent_weeks(now, 4, jst());

        // Only W-1 has a rollup; W-2 is missing.
        let input = RollupPlannerInput {
            existing_week_rollups: vec![ExistingRollupInfo {
                period_key: recent[0].week_key.clone(),
                event_count: 0,
                max_ripple: 0,
                summary_md: String::new(),
            }],

            events: vec![],
        };
        let reqs = plan_week_rollup_updates("test-agent", now, jst(), &input);

        let w2_key = &recent[1].week_key;
        let missing = reqs
            .iter()
            .find(|r| r.period_key == *w2_key && r.reason == "missing_week");
        assert!(
            missing.is_some(),
            "should detect W-2 ({w2_key}) as missing_week"
        );
    }

    // -----------------------------------------------------------------------
    // Test 6: detects delayed events in closed week
    // -----------------------------------------------------------------------
    #[test]
    fn test_detects_delayed_events_in_closed_week() {
        let now = jst_dt(2026, 5, 27, 10, 0);
        let recent = recent_weeks(now, 4, jst());
        let w2 = &recent[1];

        // Event experienced in W-2 but encoded recently (delayed).
        let exp_in_w2 = w2.period_start + Duration::days(2);
        let input = RollupPlannerInput {
            existing_week_rollups: vec![ExistingRollupInfo {
                period_key: w2.week_key.clone(),
                event_count: 1,
                max_ripple: 3,
                summary_md: "old summary".to_string(),
            }],

            events: vec![PlannerEvent {
                experienced_at: exp_in_w2.to_rfc3339(),
                encoded_at: now.to_rfc3339(), // encoded now
                ripple_strength: 5,
            }],
        };
        let reqs = plan_week_rollup_updates("test-agent", now, jst(), &input);

        let delayed = reqs
            .iter()
            .find(|r| r.period_key == w2.week_key && r.reason == "delayed_events");
        assert!(delayed.is_some(), "should detect delayed events in W-2");
        assert_eq!(
            delayed.unwrap().previous_summary_md.as_deref(),
            Some("old summary")
        );
    }

    // -----------------------------------------------------------------------
    // Test 7: detects event count mismatch
    // -----------------------------------------------------------------------
    #[test]
    fn test_detects_event_count_mismatch() {
        let now = jst_dt(2026, 5, 27, 10, 0);
        let recent = recent_weeks(now, 4, jst());
        let w1 = &recent[0];

        // Rollup says 5 events, but we provide 7 events in W-1.
        let mut events = vec![];
        for i in 0..7 {
            let ts = w1.period_start + Duration::days(i % 5) + Duration::hours(i);
            events.push(PlannerEvent {
                experienced_at: ts.to_rfc3339(),
                encoded_at: ts.to_rfc3339(),
                ripple_strength: 1,
            });
        }

        let input = RollupPlannerInput {
            existing_week_rollups: vec![ExistingRollupInfo {
                period_key: w1.week_key.clone(),
                event_count: 5,
                max_ripple: 3,
                summary_md: String::new(),
            }],

            events,
        };
        let reqs = plan_week_rollup_updates("test-agent", now, jst(), &input);

        let mismatch = reqs
            .iter()
            .find(|r| r.period_key == w1.week_key && r.reason == "event_count_mismatch");
        assert!(
            mismatch.is_some(),
            "should detect event_count_mismatch for W-1"
        );
    }

    // -----------------------------------------------------------------------
    // Test 8: month planner detects completed month with week rollups
    // -----------------------------------------------------------------------
    #[test]
    fn test_detects_week_rolling_out() {
        let now = jst_dt(2026, 5, 27, 10, 0);
        let recent = recent_weeks(now, 4, jst());
        let tz = jst();

        // All recent weeks have rollups so closed_week/missing_week don't fire.
        let mut week_rollups: Vec<ExistingRollupInfo> = vec![];
        for w in &recent {
            week_rollups.push(ExistingRollupInfo {
                period_key: w.week_key.clone(),
                event_count: 0,
                max_ripple: 0,
                summary_md: String::new(),
            });
        }

        let reqs = plan_month_rollup_updates("test-agent", now, tz, &[], &week_rollups);

        // April should be detected because W-18 (Apr 27) belongs to April
        let april = reqs.iter().find(|r| r.period_key == "2026-04");
        assert!(
            april.is_some(),
            "should detect month update for April: {reqs:?}"
        );
        assert_eq!(april.unwrap().granularity, RollupGranularity::Month);
        assert_eq!(april.unwrap().reason, "month_end");
    }

    // -----------------------------------------------------------------------
    // Test 9: month planner detects missing month rollups
    // -----------------------------------------------------------------------
    #[test]
    fn test_detects_missing_month_rollup() {
        let now = jst_dt(2026, 5, 27, 10, 0);
        let recent = recent_weeks(now, 4, jst());
        let tz = jst();

        // All weeks present, but months are missing.
        let mut week_rollups: Vec<ExistingRollupInfo> = vec![];
        for w in &recent {
            week_rollups.push(ExistingRollupInfo {
                period_key: w.week_key.clone(),
                event_count: 0,
                max_ripple: 0,
                summary_md: String::new(),
            });
        }

        let reqs = plan_month_rollup_updates("test-agent", now, tz, &[], &week_rollups);

        assert!(
            !reqs.is_empty(),
            "should detect missing month rollups: {reqs:?}"
        );
        for m in &reqs {
            assert_eq!(m.granularity, RollupGranularity::Month);
            assert_eq!(m.reason, "month_end");
        }
    }

    // -----------------------------------------------------------------------
    // Test 10: planner no longer detects background candidates (moved to batch.rs)
    // -----------------------------------------------------------------------
    #[test]
    fn test_planner_does_not_detect_background_candidates() {
        let now = jst_dt(2026, 5, 27, 10, 0);
        let recent = recent_weeks(now, 4, jst());

        let mut week_rollups: Vec<ExistingRollupInfo> = vec![];
        for w in &recent {
            week_rollups.push(ExistingRollupInfo {
                period_key: w.week_key.clone(),
                event_count: 1,
                max_ripple: 1,
                summary_md: String::new(),
            });
        }

        let old_ts = jst_dt(2026, 1, 15, 12, 0);

        let input = RollupPlannerInput {
            existing_week_rollups: week_rollups,

            events: vec![PlannerEvent {
                experienced_at: old_ts.to_rfc3339(),
                encoded_at: old_ts.to_rfc3339(),
                ripple_strength: 5,
            }],
        };
        let reqs = plan_week_rollup_updates("test-agent", now, jst(), &input);

        let bg = reqs.iter().find(|r| r.reason == "background_candidate");
        assert!(
            bg.is_none(),
            "planner should not detect background_candidate after rule 7 removal"
        );
    }

    // -----------------------------------------------------------------------
    // Test 11: returns empty when no updates needed (both planners)
    // -----------------------------------------------------------------------
    #[test]
    fn test_returns_empty_when_no_updates_needed() {
        let now = jst_dt(2026, 5, 27, 10, 0);
        let recent = recent_weeks(now, 4, jst());
        let tz = jst();

        let mut events = vec![];
        for w in &recent {
            events.push(PlannerEvent {
                experienced_at: (w.period_start + Duration::days(1)).to_rfc3339(),
                encoded_at: (w.period_start + Duration::days(1)).to_rfc3339(),
                ripple_strength: 1,
            });
        }

        let mut week_rollups: Vec<ExistingRollupInfo> = vec![];
        for w in &recent {
            week_rollups.push(ExistingRollupInfo {
                period_key: w.week_key.clone(),
                event_count: 1,
                max_ripple: 1,
                summary_md: String::new(),
            });
        }

        let input = RollupPlannerInput {
            existing_week_rollups: week_rollups.clone(),

            events,
        };
        let week_reqs = plan_week_rollup_updates("test-agent", now, tz, &input);
        assert!(
            week_reqs.is_empty(),
            "week planner should have no requests when all weeks are up to date: {week_reqs:?}"
        );

        let complete_months = complete_months_recent(now, 2, tz);
        let month_rollups: Vec<ExistingRollupInfo> = complete_months
            .iter()
            .map(|m| {
                let month_weeks: Vec<&ExistingRollupInfo> = week_rollups
                    .iter()
                    .filter(|wr| week_in_month(&wr.period_key, &m.month_key, tz))
                    .collect();
                let (max_ripple, event_count) = compute_month_rollup_stats(&month_weeks);
                ExistingRollupInfo {
                    period_key: m.month_key.clone(),
                    event_count,
                    max_ripple,
                    summary_md: String::new(),
                }
            })
            .collect();
        let month_reqs =
            plan_month_rollup_updates("test-agent", now, tz, &month_rollups, &week_rollups);
        assert!(
            month_reqs.is_empty(),
            "month planner should have no requests when months exist: {month_reqs:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Test 12: excludes current week events from month rollup
    // -----------------------------------------------------------------------
    #[test]
    fn test_excludes_current_week_events_from_month_rollup() {
        let now = jst_dt(2026, 5, 27, 10, 0);
        let cur = current_week(now, jst());
        let recent = recent_weeks(now, 4, jst());

        // All recent weeks have rollups with matching counts.
        let mut week_rollups: Vec<ExistingRollupInfo> = vec![];
        for w in &recent {
            week_rollups.push(ExistingRollupInfo {
                period_key: w.week_key.clone(),
                event_count: 0,
                max_ripple: 0,
                summary_md: String::new(),
            });
        }

        // High-ripple event in the *current* week (not closed yet).
        let cur_event_ts = cur.period_start + Duration::days(1);

        let input = RollupPlannerInput {
            existing_week_rollups: week_rollups,

            events: vec![PlannerEvent {
                experienced_at: cur_event_ts.to_rfc3339(),
                encoded_at: cur_event_ts.to_rfc3339(),
                ripple_strength: 8,
            }],
        };
        let reqs = plan_week_rollup_updates("test-agent", now, jst(), &input);

        // Should NOT produce a background_candidate for current week's month.
        let bg = reqs.iter().find(|r| {
            r.reason == "background_candidate" && r.granularity == RollupGranularity::Month
        });
        assert!(
            bg.is_none(),
            "current week events should NOT trigger background_candidate month rollup"
        );
    }

    // -----------------------------------------------------------------------
    // Call2 LLM Integration Tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_build_call2_input_json_structure() {
        let req = RollupRequest {
            granularity: RollupGranularity::Week,
            period_key: "2026-W21".to_string(),
            period_start: "2026-05-18T00:00:00+09:00".to_string(),
            period_end_exclusive: "2026-05-25T00:00:00+09:00".to_string(),
            reason: "closed_week".to_string(),
            previous_summary_md: None,
        };
        let input = build_call2_input(&[req], &HashMap::new());
        let wrapped = serde_json::json!({"rollup_requests": input});
        let json = serde_json::to_string(&wrapped).expect("serialize");
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("parse back");
        assert!(
            parsed.get("rollup_requests").is_some(),
            "should have 'rollup_requests' key"
        );
        assert!(parsed["rollup_requests"].is_array());
        assert_eq!(parsed["rollup_requests"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn test_build_call2_input_includes_previous_summary() {
        let req = RollupRequest {
            granularity: RollupGranularity::Week,
            period_key: "2026-W21".to_string(),
            period_start: "2026-05-18T00:00:00+09:00".to_string(),
            period_end_exclusive: "2026-05-25T00:00:00+09:00".to_string(),
            reason: "delayed_events".to_string(),
            previous_summary_md: Some("old summary".to_string()),
        };
        let input = build_call2_input(&[req], &HashMap::new());
        assert_eq!(input[0].previous_summary_md.as_deref(), Some("old summary"));
    }

    #[test]
    fn test_build_call2_input_includes_events() {
        let req = RollupRequest {
            granularity: RollupGranularity::Week,
            period_key: "2026-W21".to_string(),
            period_start: "2026-05-18T00:00:00+09:00".to_string(),
            period_end_exclusive: "2026-05-25T00:00:00+09:00".to_string(),
            reason: "closed_week".to_string(),
            previous_summary_md: None,
        };
        let events = vec![Call2Event {
            id: "evt-001".to_string(),
            experienced_at: "2026-05-20T14:00:00+09:00".to_string(),
            kind: "decision".to_string(),
            title: "Test event".to_string(),
            body_md: "Body text".to_string(),
            ripple_strength: 3,
            certainty: "stated".to_string(),
        }];
        let mut events_map = HashMap::new();
        events_map.insert("2026-W21".to_string(), events);
        let input = build_call2_input(&[req], &events_map);
        assert_eq!(input[0].events.len(), 1);
        assert_eq!(input[0].events[0].id, "evt-001");
    }

    #[test]
    fn test_parse_call2_output_valid_json() {
        let json = r#"{"rollups":[{"granularity":"week","period_key":"2026-W21","summary_md":"- Summary","max_ripple":3,"event_count":5}]}"#;
        let mut valid = HashSet::new();
        valid.insert("2026-W21".to_string());
        let result = parse_call2_output(json, &valid).expect("should parse");
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].period_key, "2026-W21");
        assert_eq!(result[0].max_ripple, 3);
    }

    #[test]
    fn test_parse_call2_output_missing_rollups_key() {
        let json = r#"{"events":[]}"#;
        let result = parse_call2_output(json, &HashSet::new());
        assert!(
            result.is_err(),
            "should error when 'rollups' key is missing"
        );
        match result.unwrap_err() {
            Call2Error::JsonParse(msg) => assert!(
                msg.contains("unknown field") || msg.contains("missing field"),
                "unexpected error: {msg}"
            ),
            other => panic!("expected JsonParse, got: {other}"),
        }
    }

    #[test]
    fn test_parse_call2_output_invalid_granularity() {
        let json = r#"{"rollups":[{"granularity":"quarter","period_key":"2026-W21","summary_md":"- Summary","max_ripple":3,"event_count":5}]}"#;
        let mut valid = HashSet::new();
        valid.insert("2026-W21".to_string());
        let result = parse_call2_output(json, &valid);
        assert!(result.is_err());
        match result.unwrap_err() {
            Call2Error::Validation(msg) => assert!(msg.contains("granularity"), "{msg}"),
            other => panic!("expected Validation, got: {other}"),
        }
    }

    #[test]
    fn test_parse_call2_output_unknown_period_key() {
        let json = r#"{"rollups":[{"granularity":"week","period_key":"2026-W99","summary_md":"- Summary","max_ripple":3,"event_count":5}]}"#;
        let mut valid = HashSet::new();
        valid.insert("2026-W21".to_string());
        let result = parse_call2_output(json, &valid);
        assert!(result.is_err());
        match result.unwrap_err() {
            Call2Error::Validation(msg) => assert!(msg.contains("period_key"), "{msg}"),
            other => panic!("expected Validation, got: {other}"),
        }
    }

    #[test]
    fn test_validate_summary_md_empty() {
        let json = r#"{"rollups":[{"granularity":"week","period_key":"2026-W21","summary_md":"","max_ripple":3,"event_count":5}]}"#;
        let mut valid = HashSet::new();
        valid.insert("2026-W21".to_string());
        let result = parse_call2_output(json, &valid).expect("should succeed");
        assert!(
            result.is_empty(),
            "empty summary_md should be filtered out, not error"
        );
    }

    #[test]
    fn test_validate_summary_md_too_long() {
        let long_summary = "x".repeat(10_001);
        let json = format!(
            r#"{{"rollups":[{{"granularity":"week","period_key":"2026-W21","summary_md":"{long_summary}","max_ripple":3,"event_count":5}}]}}"#
        );
        let mut valid = HashSet::new();
        valid.insert("2026-W21".to_string());
        let result = parse_call2_output(&json, &valid);
        assert!(result.is_err());
        match result.unwrap_err() {
            Call2Error::Validation(msg) => assert!(msg.contains("too long"), "{msg}"),
            other => panic!("expected Validation, got: {other}"),
        }
    }

    #[test]
    fn test_validate_no_event_ids_in_output() {
        let json = r#"{"rollups":[{"granularity":"week","period_key":"2026-W21","summary_md":"- Something evt_abc123 happened","max_ripple":3,"event_count":5}]}"#;
        let mut valid = HashSet::new();
        valid.insert("2026-W21".to_string());
        let result = parse_call2_output(json, &valid);
        assert!(result.is_err());
        match result.unwrap_err() {
            Call2Error::Validation(msg) => assert!(msg.contains("evt_"), "{msg}"),
            other => panic!("expected Validation, got: {other}"),
        }
    }

    #[test]
    fn test_validate_max_ripple_range() {
        for invalid_ripple in [0, 6, -1, 10] {
            let json = format!(
                r#"{{"rollups":[{{"granularity":"week","period_key":"2026-W21","summary_md":"- S","max_ripple":{invalid_ripple},"event_count":1}}]}}"#
            );
            let mut valid = HashSet::new();
            valid.insert("2026-W21".to_string());
            let result = parse_call2_output(&json, &valid);
            assert!(
                result.is_err(),
                "max_ripple={invalid_ripple} should be invalid"
            );
            match result.unwrap_err() {
                Call2Error::Validation(msg) => assert!(msg.contains("max_ripple"), "{msg}"),
                other => panic!("expected Validation, got: {other}"),
            }
        }
    }

    #[test]
    fn test_validate_event_count_non_negative() {
        let json = r#"{"rollups":[{"granularity":"week","period_key":"2026-W21","summary_md":"- S","max_ripple":3,"event_count":-1}]}"#;
        let mut valid = HashSet::new();
        valid.insert("2026-W21".to_string());
        let result = parse_call2_output(json, &valid);
        assert!(result.is_err());
        match result.unwrap_err() {
            Call2Error::Validation(msg) => assert!(msg.contains("event_count"), "{msg}"),
            other => panic!("expected Validation, got: {other}"),
        }
    }

    #[test]
    fn test_call2_retry_on_json_parse_failure() {
        let bad_json = "this is not json";
        let result = parse_call2_output(bad_json, &HashSet::new());
        assert!(result.is_err());
        match result.unwrap_err() {
            Call2Error::JsonParse(_) => {}
            other => panic!("expected JsonParse, got: {other}"),
        }
    }

    #[test]
    fn test_call2_retry_on_missing_field() {
        let json = r#"{"rollups":[{"granularity":"week","period_key":"2026-W21","summary_md":"- S","event_count":1}]}"#;
        let result = parse_call2_output(json, &HashSet::new());
        assert!(result.is_err());
        match result.unwrap_err() {
            Call2Error::JsonParse(msg) => assert!(msg.contains("missing field"), "{msg}"),
            other => panic!("expected JsonParse, got: {other}"),
        }
    }

    #[test]
    fn test_call2_fallback_on_retry_exhaustion() {
        let bad_json = "not valid json at all";
        let result = parse_call2_output(bad_json, &HashSet::new());
        assert!(
            result.is_err(),
            "parse should fail, triggering retry/fallback in batch.rs"
        );
    }

    #[test]
    fn test_security_redaction_in_input() {
        let body = "User said: my key is sk-abc123def456ghi789";
        let redacted = redact_secrets(body);
        assert!(
            !redacted.contains("abc123def456ghi789"),
            "secret should be redacted"
        );
        assert!(
            redacted.contains("sk-[REDACTED]"),
            "should show sk- prefix with REDACTED"
        );
    }

    #[test]
    fn test_security_redaction_in_output() {
        let body = "The header was Bearer token123xyz";
        let redacted = redact_secrets(body);
        assert!(
            !redacted.contains("token123xyz"),
            "bearer token should be redacted"
        );
        assert!(
            redacted.contains("Bearer [REDACTED]"),
            "should show Bearer with REDACTED"
        );
    }

    // -----------------------------------------------------------------------
    // Fix 1 test: month planner skips month when rollup already exists
    // -----------------------------------------------------------------------
    #[test]
    fn test_week_rolling_out_suppressed_when_month_exists() {
        let now = jst_dt(2026, 5, 27, 10, 0);
        let recent = recent_weeks(now, 4, jst());
        let tz = jst();

        let mut week_rollups: Vec<ExistingRollupInfo> = vec![];
        for w in &recent {
            week_rollups.push(ExistingRollupInfo {
                period_key: w.week_key.clone(),
                event_count: 0,
                max_ripple: 0,
                summary_md: String::new(),
            });
        }

        let ro_monday = recent.last().unwrap().period_start.date_naive() - Duration::days(7);
        let ro_week = week_for_date_inner(ro_monday, tz);
        let ro_month = month_for_ym(
            ro_week.period_start.year(),
            ro_week.period_start.month(),
            tz,
        );

        let existing_month = ExistingRollupInfo {
            period_key: ro_month.month_key.clone(),
            event_count: 0,
            max_ripple: 0,
            summary_md: "existing summary".to_string(),
        };

        let reqs =
            plan_month_rollup_updates("test-agent", now, tz, &[existing_month], &week_rollups);

        let rolling = reqs.iter().find(|r| r.period_key == ro_month.month_key);
        assert!(
            rolling.is_none(),
            "month should NOT be requested when rollup already exists"
        );
    }

    #[test]
    fn test_month_period_from_key_valid() {
        let tz = jst();
        let mp = month_period_from_key("2026-03", tz).expect("should parse");
        assert_eq!(mp.month_key, "2026-03");
        assert_eq!(mp.period_start.day(), 1);
        assert_eq!(mp.period_start.month(), 3);
        assert_eq!(mp.period_end_exclusive.month(), 4);
    }

    #[test]
    fn test_month_period_from_key_december_wraps() {
        let tz = jst();
        let mp = month_period_from_key("2025-12", tz).expect("should parse");
        assert_eq!(mp.month_key, "2025-12");
        assert_eq!(mp.period_end_exclusive.year(), 2026);
        assert_eq!(mp.period_end_exclusive.month(), 1);
    }

    #[test]
    fn test_month_period_from_key_invalid_month() {
        let tz = jst();
        assert!(month_period_from_key("2026-13", tz).is_none());
        assert!(month_period_from_key("2026-00", tz).is_none());
    }

    #[test]
    fn test_month_period_from_key_invalid_format() {
        let tz = jst();
        assert!(month_period_from_key("invalid", tz).is_none());
        assert!(month_period_from_key("2026", tz).is_none());
        assert!(month_period_from_key("", tz).is_none());
    }

    #[test]
    fn test_complete_months_recent_returns_full_months() {
        let tz = jst();
        let now = jst_dt(2026, 7, 15, 10, 0);

        let months = complete_months_recent(now, 2, tz);

        assert_eq!(months.len(), 2);
        assert_eq!(months[0].month_key, "2026-06");
        assert_eq!(months[1].month_key, "2026-05");
        assert_eq!(months[0].period_start.day(), 1);
        assert_eq!(months[0].period_end_exclusive.month(), 7);
        assert_eq!(months[1].period_start.day(), 1);
        assert_eq!(months[1].period_end_exclusive.month(), 6);
    }

    #[test]
    fn test_month_trigger_conditions() {
        let tz = jst();
        let now = jst_dt(2026, 7, 15, 10, 0);

        let june_week_rollup = ExistingRollupInfo {
            period_key: "2026-W25".to_string(),
            event_count: 5,
            max_ripple: 3,
            summary_md: "test summary".to_string(),
        };

        // No existing month rollups → June should trigger
        let reqs = plan_month_rollup_updates(
            "test-agent",
            now,
            tz,
            &[],
            std::slice::from_ref(&june_week_rollup),
        );

        let june_req = reqs.iter().find(|r| r.period_key == "2026-06");
        assert!(june_req.is_some(), "should trigger for June: {reqs:?}");
        assert_eq!(june_req.unwrap().reason, "month_end");

        let july_req = reqs.iter().find(|r| r.period_key == "2026-07");
        assert!(
            july_req.is_none(),
            "should NOT trigger for July (not ended): {reqs:?}"
        );

        // With existing June month rollup with matching stats → June should be skipped
        let existing_june = ExistingRollupInfo {
            period_key: "2026-06".to_string(),
            event_count: 5,
            max_ripple: 3,
            summary_md: "existing".to_string(),
        };
        let reqs2 =
            plan_month_rollup_updates("test-agent", now, tz, &[existing_june], &[june_week_rollup]);
        let june_req2 = reqs2.iter().find(|r| r.period_key == "2026-06");
        assert!(june_req2.is_none(), "should skip existing month rollup");
    }

    // -----------------------------------------------------------------------
    // Split prompt template tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_system_prompt_reads_split_templates() {
        // Arrange
        let agent_id = "test-agent";

        // Act
        let week_prompt = build_call2_system_prompt_week(agent_id);
        let month_prompt = build_call2_system_prompt_month(agent_id);

        // Assert — both should start with the role description
        assert!(
            week_prompt.starts_with("あなたは test-agent の海馬です"),
            "week prompt should start with agent name: {}",
            &week_prompt[..week_prompt.len().min(100)]
        );
        assert!(
            month_prompt.starts_with("あなたは test-agent の海馬です"),
            "month prompt should start with agent name: {}",
            &month_prompt[..month_prompt.len().min(100)]
        );

        // Assert — week prompt should contain week-specific content
        assert!(
            week_prompt.contains("週要約"),
            "week prompt should contain 週要約 section"
        );
        assert!(
            week_prompt.contains("独立bullet") || week_prompt.contains("独立 bullet"),
            "week prompt should contain 独立 bullet instruction"
        );

        // Assert — month prompt should contain month-specific content
        assert!(
            month_prompt.contains("月要約"),
            "month prompt should contain 月要約 section"
        );
        assert!(
            month_prompt.contains("week_rollups"),
            "month prompt should mention week_rollups in input schema"
        );
        assert!(
            month_prompt.contains("previous_month_summary_md"),
            "month prompt should mention previous_month_summary_md"
        );
    }

    #[test]
    fn test_build_month_input_with_week_rollups_and_stats() {
        // Arrange
        let month_req = RollupRequest {
            granularity: RollupGranularity::Month,
            period_key: "2026-07".to_string(),
            period_start: "2026-07-01T00:00:00+09:00".to_string(),
            period_end_exclusive: "2026-08-01T00:00:00+09:00".to_string(),
            reason: "month_end".to_string(),
            previous_summary_md: None,
        };

        let week_rollup_w27 = Call2WeekRollupSummary {
            period_key: "2026-W27".to_string(),
            summary_md: "W27 summary".to_string(),
            max_ripple: 3,
            event_count: 5,
        };
        let week_rollup_w28 = Call2WeekRollupSummary {
            period_key: "2026-W28".to_string(),
            summary_md: "W28 summary".to_string(),
            max_ripple: 4,
            event_count: 8,
        };

        let mut week_rollups_map = HashMap::new();
        week_rollups_map.insert(
            "2026-07".to_string(),
            vec![week_rollup_w27, week_rollup_w28],
        );

        let mut previous_month_map = HashMap::new();
        previous_month_map.insert("2026-07".to_string(), "Previous June summary".to_string());

        // Act — Input builder
        let input = build_call2_input_month(&[month_req], &week_rollups_map, &previous_month_map);

        // Assert — Input builder
        assert_eq!(input.len(), 1);
        let req = &input[0];
        assert_eq!(req.granularity, "month");
        assert_eq!(req.period_key, "2026-07");
        assert_eq!(req.week_rollups.len(), 2);
        assert_eq!(req.week_rollups[0].period_key, "2026-W27");
        assert_eq!(req.week_rollups[1].period_key, "2026-W28");
        assert_eq!(
            req.previous_month_summary_md.as_deref(),
            Some("Previous June summary")
        );
        assert!(
            req.events.is_empty(),
            "month input should not have raw events"
        );

        // Act — Stats computation
        let existing_w27 = ExistingRollupInfo {
            period_key: "2026-W27".to_string(),
            event_count: 5,
            max_ripple: 3,
            summary_md: String::new(),
        };
        let existing_w28 = ExistingRollupInfo {
            period_key: "2026-W28".to_string(),
            event_count: 8,
            max_ripple: 4,
            summary_md: String::new(),
        };
        let (max_ripple, event_count) = compute_month_rollup_stats(&[&existing_w27, &existing_w28]);

        // Assert — Stats
        assert_eq!(max_ripple, 4, "should be max of week max_ripples");
        assert_eq!(event_count, 13, "should be sum of week event_counts");
    }

    #[test]
    fn test_batch_executes_week_then_month_calls() {
        let tz = jst();
        let now = jst_dt(2026, 7, 15, 10, 0);
        let recent = recent_weeks(now, 4, tz);

        let w1 = &recent[0];

        let week_rollup_for_w2 = ExistingRollupInfo {
            period_key: recent[1].week_key.clone(),
            event_count: 5,
            max_ripple: 3,
            summary_md: "W-2 summary".to_string(),
        };
        let week_rollup_for_w3 = ExistingRollupInfo {
            period_key: recent[2].week_key.clone(),
            event_count: 3,
            max_ripple: 2,
            summary_md: "W-3 summary".to_string(),
        };
        let week_rollup_for_w4 = ExistingRollupInfo {
            period_key: recent[3].week_key.clone(),
            event_count: 2,
            max_ripple: 4,
            summary_md: "W-4 summary".to_string(),
        };

        let input = RollupPlannerInput {
            existing_week_rollups: vec![
                week_rollup_for_w2.clone(),
                week_rollup_for_w3.clone(),
                week_rollup_for_w4.clone(),
            ],

            events: vec![],
        };

        let week_reqs = plan_week_rollup_updates("test-agent", now, tz, &input);
        for req in &week_reqs {
            assert_eq!(
                req.granularity,
                RollupGranularity::Week,
                "week planner should only return week requests: {:?}",
                req
            );
        }
        assert!(
            week_reqs.iter().any(|r| r.period_key == w1.week_key),
            "W-1 should be requested by week planner: {week_reqs:?}"
        );

        let month_reqs = plan_month_rollup_updates(
            "test-agent",
            now,
            tz,
            &[],
            &[week_rollup_for_w2, week_rollup_for_w3, week_rollup_for_w4],
        );
        for req in &month_reqs {
            assert_eq!(
                req.granularity,
                RollupGranularity::Month,
                "month planner should only return month requests: {:?}",
                req
            );
        }

        let june = month_reqs.iter().find(|r| r.period_key == "2026-06");
        assert!(
            june.is_some(),
            "June should be triggered by month planner: {month_reqs:?}"
        );
    }

    #[test]
    fn compute_rollup_stats_from_actual_events() {
        let events = vec![
            Call2Event {
                id: "e1".to_string(),
                experienced_at: "2026-05-20T10:00:00+09:00".to_string(),
                kind: "decision".to_string(),
                title: "t1".to_string(),
                body_md: "b1".to_string(),
                ripple_strength: 3,
                certainty: "stated".to_string(),
            },
            Call2Event {
                id: "e2".to_string(),
                experienced_at: "2026-05-21T10:00:00+09:00".to_string(),
                kind: "insight".to_string(),
                title: "t2".to_string(),
                body_md: "b2".to_string(),
                ripple_strength: 5,
                certainty: "derived".to_string(),
            },
        ];
        let (max_ripple, event_count) = compute_rollup_stats(Some(&events));
        assert_eq!(max_ripple, 5);
        assert_eq!(event_count, 2);
    }

    #[test]
    fn compute_rollup_stats_defaults_when_empty() {
        let (max_ripple, event_count) = compute_rollup_stats(None);
        assert_eq!(max_ripple, 3);
        assert_eq!(event_count, 0);
    }

    #[test]
    fn compute_rollup_stats_single_event() {
        let events = vec![Call2Event {
            id: "e1".to_string(),
            experienced_at: "2026-05-20T10:00:00+09:00".to_string(),
            kind: "feat".to_string(),
            title: "t".to_string(),
            body_md: "b".to_string(),
            ripple_strength: 4,
            certainty: "stated".to_string(),
        }];
        let (max_ripple, event_count) = compute_rollup_stats(Some(&events));
        assert_eq!(max_ripple, 4);
        assert_eq!(event_count, 1);
    }
}
