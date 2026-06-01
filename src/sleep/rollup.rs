//! Rollup Planner — pure Rust logic that determines which week/month rollups
//! need updating for the Call 2 episodic-view system.
//!
//! This module is intentionally free of DB/LLM dependencies so that every
//! detection rule can be unit-tested with plain data.

use chrono::{DateTime, Datelike, Duration, FixedOffset, NaiveDate, Offset, TimeZone};
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
    pub existing_month_rollups: Vec<ExistingRollupInfo>,
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

/// Calendar months starting from the month of the oldest recent week, most-recent first.
pub(crate) fn recent_months_from_weeks(
    recent_weeks: &[WeekPeriod],
    count: usize,
    tz: chrono_tz::Tz,
) -> Vec<MonthPeriod> {
    if recent_weeks.is_empty() {
        return Vec::new();
    }
    let oldest = &recent_weeks[recent_weeks.len() - 1];
    let start_date = oldest.period_start.date_naive();
    let mut y = start_date.year();
    let mut m = start_date.month();

    let mut months = Vec::with_capacity(count);
    for i in 0..count {
        if i > 0 {
            m = m.saturating_sub(1);
            if m == 0 {
                m = 12;
                y -= 1;
            }
        }
        let mut mp = month_for_ym(y, m, tz);
        if i == 0 && mp.period_end_exclusive > oldest.period_start {
            mp.period_end_exclusive = oldest.period_start;
        }
        months.push(mp);
    }
    months
}

/// Complete calendar months ending before `now`, most-recent first.
///
/// Unlike `recent_months_from_weeks` which caps months to the oldest week boundary,
/// this returns full calendar months suitable for end-of-month detection.
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
    let ps: DateTime<chrono_tz::Tz> =
        tz.from_utc_datetime(&monday.and_hms_opt(0, 0, 0).expect("midnight is valid"));
    let next_monday = monday + Duration::days(7);
    let pe: DateTime<chrono_tz::Tz> =
        tz.from_utc_datetime(&next_monday.and_hms_opt(0, 0, 0).expect("midnight is valid"));
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
    let ps: DateTime<chrono_tz::Tz> =
        tz.from_utc_datetime(&first.and_hms_opt(0, 0, 0).expect("midnight is valid"));
    let (ny, nm) = if month == 12 {
        (year + 1, 1)
    } else {
        (year, month + 1)
    };
    let next_first = NaiveDate::from_ymd_opt(ny, nm, 1).expect("valid year/month");
    let pe: DateTime<chrono_tz::Tz> =
        tz.from_utc_datetime(&next_first.and_hms_opt(0, 0, 0).expect("midnight is valid"));
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

pub(crate) fn plan_rollup_updates(
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

    let existing_month_map: HashMap<&str, &ExistingRollupInfo> = input
        .existing_month_rollups
        .iter()
        .map(|r| (r.period_key.as_str(), r))
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

    // -----------------------------------------------------------------------
    // 5. Week rolling out (W-5 → its month needs update)
    // -----------------------------------------------------------------------
    let rolling_out_monday = recent
        .last()
        .map(|w| w.period_start.date_naive() - Duration::days(7));
    if let Some(ro_monday) = rolling_out_monday {
        let ro_week = week_for_date_inner(ro_monday, tz);
        let month = month_for_ym(
            ro_week.period_start.year(),
            ro_week.period_start.month(),
            tz,
        );
        let key = format!("month:{}", month.month_key);
        if !existing_month_map.contains_key(month.month_key.as_str()) && seen_keys.insert(key) {
            requests.push(make_month_request(&month, "week_rolling_out", None));
        }
    }

    // -----------------------------------------------------------------------
    // 6. Recent months from weeks — missing month rollups
    // -----------------------------------------------------------------------
    let recent_months = recent_months_from_weeks(&recent, 2, tz);
    for mp in &recent_months {
        let key = format!("month:{}", mp.month_key);
        if !existing_month_map.contains_key(mp.month_key.as_str()) && seen_keys.insert(key) {
            requests.push(make_month_request(mp, "missing_month", None));
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

    let existing_month_keys: HashSet<&str> = existing_month_rollups
        .iter()
        .map(|r| r.period_key.as_str())
        .collect();

    let complete_months = complete_months_recent(now, 2, tz);

    for mp in &complete_months {
        if now < mp.period_end_exclusive {
            continue;
        }

        if existing_month_keys.contains(mp.month_key.as_str()) {
            continue;
        }

        let has_week_rollup = existing_week_rollups
            .iter()
            .any(|wr| week_in_month(&wr.period_key, &mp.month_key, tz));

        if !has_week_rollup {
            continue;
        }

        requests.push(make_month_request(mp, "month_end", None));
    }

    requests
}

/// Check if a week (ISO week key like "2026-W14") belongs to a month (key like "2026-04").
/// A week belongs to a month if the week's Monday falls within the month period.
fn week_in_month(week_key: &str, month_key: &str, tz: chrono_tz::Tz) -> bool {
    let Some(mp) = month_period_from_key(month_key, tz) else {
        return false;
    };
    let (year, week_num) = parse_iso_week_key(week_key);
    let Some(monday) = NaiveDate::from_isoywd_opt(year, week_num, chrono::Weekday::Mon) else {
        return false;
    };
    let week_start: DateTime<FixedOffset> = to_fixed(
        tz.from_utc_datetime(&monday.and_hms_opt(0, 0, 0).expect("midnight is valid")),
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

/// Builds the Call2 system prompt from the embedded prompt template.
pub(crate) fn build_call2_system_prompt(agent_id: &str) -> String {
    include_str!("rollup_prompt.md").replace("{AGENT_NAME}", agent_id)
}

/// Builds the Call2 system prompt for week rollups from the embedded week prompt template.
pub(crate) fn build_call2_system_prompt_week(agent_id: &str) -> String {
    include_str!("rollup_week_prompt.md").replace("{AGENT_NAME}", agent_id)
}

/// Builds the Call2 system prompt for month rollups from the embedded month prompt template.
pub(crate) fn build_call2_system_prompt_month(agent_id: &str) -> String {
    include_str!("rollup_month_prompt.md").replace("{AGENT_NAME}", agent_id)
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
    use chrono::{TimeZone, Weekday};

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
        let tz_dt: DateTime<chrono_tz::Tz> = jst().from_utc_datetime(&naive);
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
    // Test 3: recent_months — identifies 2 months before recent weeks
    // -----------------------------------------------------------------------
    #[test]
    fn test_recent_months_identifies_2() {
        let now = jst_dt(2026, 5, 27, 10, 0);
        let weeks = recent_weeks(now, 4, jst());
        let months = recent_months_from_weeks(&weeks, 2, jst());

        assert_eq!(months.len(), 2);
        assert!(months[0].period_start > months[1].period_start);
        let oldest_week = &weeks[weeks.len() - 1];
        assert!(
            months[0].period_start <= oldest_week.period_start,
            "first recent month should include the oldest week's month"
        );
        assert!(months[0].month_key.starts_with("2026-04"));
        assert!(months[1].month_key.starts_with("2026-03"));
        assert!(
            months[0].period_end_exclusive <= oldest_week.period_start,
            "first recent month end should be capped to oldest week start"
        );
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
            existing_month_rollups: vec![],
            events: vec![],
        };
        let reqs = plan_rollup_updates("test-agent", now, jst(), &input);

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
            existing_month_rollups: vec![],
            events: vec![],
        };
        let reqs = plan_rollup_updates("test-agent", now, jst(), &input);

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
            existing_month_rollups: vec![],
            events: vec![PlannerEvent {
                experienced_at: exp_in_w2.to_rfc3339(),
                encoded_at: now.to_rfc3339(), // encoded now
                ripple_strength: 5,
            }],
        };
        let reqs = plan_rollup_updates("test-agent", now, jst(), &input);

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
            existing_month_rollups: vec![],
            events,
        };
        let reqs = plan_rollup_updates("test-agent", now, jst(), &input);

        let mismatch = reqs
            .iter()
            .find(|r| r.period_key == w1.week_key && r.reason == "event_count_mismatch");
        assert!(
            mismatch.is_some(),
            "should detect event_count_mismatch for W-1"
        );
    }

    // -----------------------------------------------------------------------
    // Test 8: detects week rolling out (W-5 → month update)
    // -----------------------------------------------------------------------
    #[test]
    fn test_detects_week_rolling_out() {
        let now = jst_dt(2026, 5, 27, 10, 0);
        let recent = recent_weeks(now, 4, jst());

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

        let input = RollupPlannerInput {
            existing_week_rollups: week_rollups,
            existing_month_rollups: vec![],
            events: vec![],
        };
        let reqs = plan_rollup_updates("test-agent", now, jst(), &input);

        let rolling = reqs.iter().find(|r| r.reason == "week_rolling_out");
        assert!(
            rolling.is_some(),
            "should detect month update for week rolling out"
        );
        assert_eq!(rolling.unwrap().granularity, RollupGranularity::Month);
    }

    // -----------------------------------------------------------------------
    // Test 9: detects missing month rollup
    // -----------------------------------------------------------------------
    #[test]
    fn test_detects_missing_month_rollup() {
        let now = jst_dt(2026, 5, 27, 10, 0);
        let recent = recent_weeks(now, 4, jst());

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

        let input = RollupPlannerInput {
            existing_week_rollups: week_rollups,
            existing_month_rollups: vec![],
            events: vec![],
        };
        let reqs = plan_rollup_updates("test-agent", now, jst(), &input);

        let missing_months: Vec<_> = reqs
            .iter()
            .filter(|r| r.reason == "missing_month")
            .collect();
        assert!(
            !missing_months.is_empty(),
            "should detect missing month rollups"
        );
        for m in &missing_months {
            assert_eq!(m.granularity, RollupGranularity::Month);
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
            existing_month_rollups: vec![],
            events: vec![PlannerEvent {
                experienced_at: old_ts.to_rfc3339(),
                encoded_at: old_ts.to_rfc3339(),
                ripple_strength: 5,
            }],
        };
        let reqs = plan_rollup_updates("test-agent", now, jst(), &input);

        let bg = reqs.iter().find(|r| r.reason == "background_candidate");
        assert!(
            bg.is_none(),
            "planner should not detect background_candidate after rule 7 removal"
        );
    }

    // -----------------------------------------------------------------------
    // Test 11: returns empty when no updates needed
    // -----------------------------------------------------------------------
    #[test]
    fn test_returns_empty_when_no_updates_needed() {
        let now = jst_dt(2026, 5, 27, 10, 0);
        let recent = recent_weeks(now, 4, jst());
        let recent_months = recent_months_from_weeks(&recent, 2, jst());

        // Build one event per recent week so event counts match.
        let mut events = vec![];
        for w in &recent {
            events.push(PlannerEvent {
                experienced_at: (w.period_start + Duration::days(1)).to_rfc3339(),
                encoded_at: (w.period_start + Duration::days(1)).to_rfc3339(),
                ripple_strength: 1,
            });
        }

        // Populate all rollups with matching event counts.
        let mut week_rollups: Vec<ExistingRollupInfo> = vec![];
        for w in &recent {
            week_rollups.push(ExistingRollupInfo {
                period_key: w.week_key.clone(),
                event_count: 1,
                max_ripple: 1,
                summary_md: String::new(),
            });
        }
        let mut month_rollups: Vec<ExistingRollupInfo> = vec![];
        for m in &recent_months {
            month_rollups.push(ExistingRollupInfo {
                period_key: m.month_key.clone(),
                event_count: 0,
                max_ripple: 0,
                summary_md: String::new(),
            });
        }
        // Also add the rolling-out month.
        let tz = jst();
        let ro_monday = recent.last().unwrap().period_start.date_naive() - Duration::days(7);
        let ro_week = week_for_date_inner(ro_monday, tz);
        let ro_month = month_for_ym(
            ro_week.period_start.year(),
            ro_week.period_start.month(),
            tz,
        );
        month_rollups.push(ExistingRollupInfo {
            period_key: ro_month.month_key.clone(),
            event_count: 0,
            max_ripple: 0,
            summary_md: String::new(),
        });

        let input = RollupPlannerInput {
            existing_week_rollups: week_rollups,
            existing_month_rollups: month_rollups,
            events,
        };
        let reqs = plan_rollup_updates("test-agent", now, jst(), &input);

        assert!(
            reqs.is_empty(),
            "should have no requests when everything is up to date: {reqs:?}"
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
            existing_month_rollups: vec![],
            events: vec![PlannerEvent {
                experienced_at: cur_event_ts.to_rfc3339(),
                encoded_at: cur_event_ts.to_rfc3339(),
                ripple_strength: 8,
            }],
        };
        let reqs = plan_rollup_updates("test-agent", now, jst(), &input);

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
    // Fix 1 test: week_rolling_out suppressed when month rollup exists
    // -----------------------------------------------------------------------
    #[test]
    fn test_week_rolling_out_suppressed_when_month_exists() {
        let now = jst_dt(2026, 5, 27, 10, 0);
        let recent = recent_weeks(now, 4, jst());

        let mut week_rollups: Vec<ExistingRollupInfo> = vec![];
        for w in &recent {
            week_rollups.push(ExistingRollupInfo {
                period_key: w.week_key.clone(),
                event_count: 0,
                max_ripple: 0,
                summary_md: String::new(),
            });
        }

        let tz = jst();
        let ro_monday = recent.last().unwrap().period_start.date_naive() - Duration::days(7);
        let ro_week = week_for_date_inner(ro_monday, tz);
        let ro_month = month_for_ym(
            ro_week.period_start.year(),
            ro_week.period_start.month(),
            tz,
        );

        let input = RollupPlannerInput {
            existing_week_rollups: week_rollups,
            existing_month_rollups: vec![ExistingRollupInfo {
                period_key: ro_month.month_key.clone(),
                event_count: 0,
                max_ripple: 0,
                summary_md: "existing summary".to_string(),
            }],
            events: vec![],
        };
        let reqs = plan_rollup_updates("test-agent", now, jst(), &input);

        let rolling = reqs
            .iter()
            .find(|r| r.reason == "week_rolling_out" && r.period_key == ro_month.month_key);
        assert!(
            rolling.is_none(),
            "week_rolling_out should NOT fire when month rollup already exists"
        );
    }

    // -----------------------------------------------------------------------
    // Fix 2 test: recent_months includes the month of the oldest week
    // -----------------------------------------------------------------------
    #[test]
    fn test_recent_months_includes_oldest_week_month() {
        let now = jst_dt(2026, 5, 27, 10, 0);
        let weeks = recent_weeks(now, 4, jst());
        let oldest = &weeks[weeks.len() - 1];

        let months = recent_months_from_weeks(&weeks, 2, jst());

        assert_eq!(months.len(), 2);

        let oldest_month_key = format!(
            "{}-{:02}",
            oldest.period_start.year(),
            oldest.period_start.month()
        );
        assert_eq!(
            months[0].month_key, oldest_month_key,
            "first recent month should be the month of the oldest week"
        );

        let prev_year = if oldest.period_start.month() == 1 {
            oldest.period_start.year() - 1
        } else {
            oldest.period_start.year()
        };
        let prev_month = if oldest.period_start.month() == 1 {
            12
        } else {
            oldest.period_start.month() - 1
        };
        let prev_key = format!("{}-{:02}", prev_year, prev_month);
        assert_eq!(months[1].month_key, prev_key);

        assert!(
            months[0].period_end_exclusive <= oldest.period_start,
            "first recent month end should be capped to oldest week start"
        );
    }

    #[test]
    fn test_recent_months_no_cap_when_month_ends_before_oldest_week() {
        // When the oldest week starts on the 1st of a month, the month ends
        // exactly at the week start — no cap needed (equality is fine).
        // Use a date where Monday falls on the 1st.
        let now = jst_dt(2026, 6, 4, 10, 0); // Thursday 2026-06-04
        let weeks = recent_weeks(now, 4, jst());
        let oldest = &weeks[weeks.len() - 1];
        let months = recent_months_from_weeks(&weeks, 2, jst());

        assert!(!months.is_empty());
        assert!(
            months[0].period_end_exclusive <= oldest.period_start,
            "month end should not exceed oldest week start"
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
            &[june_week_rollup.clone()],
        );

        let june_req = reqs.iter().find(|r| r.period_key == "2026-06");
        assert!(june_req.is_some(), "should trigger for June: {reqs:?}");
        assert_eq!(june_req.unwrap().reason, "month_end");

        let july_req = reqs.iter().find(|r| r.period_key == "2026-07");
        assert!(
            july_req.is_none(),
            "should NOT trigger for July (not ended): {reqs:?}"
        );

        // With existing June month rollup → June should be skipped
        let existing_june = ExistingRollupInfo {
            period_key: "2026-06".to_string(),
            event_count: 10,
            max_ripple: 4,
            summary_md: "existing".to_string(),
        };
        let reqs2 = plan_month_rollup_updates(
            "test-agent",
            now,
            tz,
            &[existing_june],
            &[june_week_rollup],
        );
        let june_req2 = reqs2.iter().find(|r| r.period_key == "2026-06");
        assert!(
            june_req2.is_none(),
            "should skip existing month rollup"
        );
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
}
