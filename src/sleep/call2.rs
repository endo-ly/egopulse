//! Rollup Planner — pure Rust logic that determines which week/month rollups
//! need updating for the Call 2 episodic-view system.
//!
//! This module is intentionally free of DB/LLM dependencies so that every
//! detection rule can be unit-tested with plain data.

use chrono::{DateTime, Datelike, Duration, FixedOffset, NaiveDate, TimeZone};
use std::collections::{HashMap, HashSet};

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

pub(crate) fn current_week(now: DateTime<FixedOffset>) -> WeekPeriod {
    let tz = *now.offset();
    let monday = monday_of(now.date_naive());
    week_for_date_inner(monday, tz)
}

/// Closed weeks before current, most-recent first (W-1, W-2, ...).
pub(crate) fn recent_weeks(now: DateTime<FixedOffset>, count: usize) -> Vec<WeekPeriod> {
    let cur = current_week(now);
    let tz = *cur.period_start.offset();
    let mut weeks = Vec::with_capacity(count);
    let mut monday = cur.period_start.date_naive();
    for _ in 0..count {
        monday -= Duration::days(7);
        weeks.push(week_for_date_inner(monday, tz));
    }
    weeks
}

/// Calendar months *before* the oldest recent week, most-recent first.
pub(crate) fn recent_months_from_weeks(
    recent_weeks: &[WeekPeriod],
    count: usize,
) -> Vec<MonthPeriod> {
    if recent_weeks.is_empty() {
        return Vec::new();
    }
    let oldest = &recent_weeks[recent_weeks.len() - 1];
    let tz = *oldest.period_start.offset();
    // The month of the oldest week is excluded — we want months *before* it.
    let start_date = oldest.period_start.date_naive();
    let first_month = start_date.month();
    let first_year = start_date.year();

    let mut months = Vec::with_capacity(count);
    let mut y = first_year;
    let mut m = first_month;

    for _ in 0..count {
        // Step back one month.
        m = m.saturating_sub(1);
        if m == 0 {
            m = 12;
            y -= 1;
        }
        months.push(month_for_ym(y, m, tz));
    }
    months
}

fn week_for_date(date: NaiveDate, tz: FixedOffset) -> WeekPeriod {
    let monday = monday_of(date);
    week_for_date_inner(monday, tz)
}

fn week_for_date_inner(monday: NaiveDate, tz: FixedOffset) -> WeekPeriod {
    let iso = monday.iso_week();
    let week_key = format!("{}-W{:02}", iso.year(), iso.week());
    let period_start =
        tz.from_utc_datetime(&monday.and_hms_opt(0, 0, 0).expect("midnight is valid"));
    let period_end_exclusive = period_start + Duration::days(7);
    WeekPeriod {
        week_key,
        period_start,
        period_end_exclusive,
    }
}

fn month_for_ym(year: i32, month: u32, tz: FixedOffset) -> MonthPeriod {
    let month_key = format!("{year}-{month:02}");
    let first = NaiveDate::from_ymd_opt(year, month, 1).expect("valid year/month");
    let period_start =
        tz.from_utc_datetime(&first.and_hms_opt(0, 0, 0).expect("midnight is valid"));
    let (ny, nm) = if month == 12 {
        (year + 1, 1)
    } else {
        (year, month + 1)
    };
    let next_first = NaiveDate::from_ymd_opt(ny, nm, 1).expect("valid year/month");
    let period_end_exclusive =
        tz.from_utc_datetime(&next_first.and_hms_opt(0, 0, 0).expect("midnight is valid"));
    MonthPeriod {
        month_key,
        period_start,
        period_end_exclusive,
    }
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
    input: &RollupPlannerInput,
) -> Vec<RollupRequest> {
    let mut requests: Vec<RollupRequest> = Vec::new();
    let mut seen_keys: HashSet<String> = HashSet::new();

    let cur_week = current_week(now);
    let recent = recent_weeks(now, 4);

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
    let tz = *cur_week.period_start.offset();
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
    let recent_months = recent_months_from_weeks(&recent, 2);
    for mp in &recent_months {
        let key = format!("month:{}", mp.month_key);
        if !existing_month_map.contains_key(mp.month_key.as_str()) && seen_keys.insert(key) {
            requests.push(make_month_request(mp, "missing_month", None));
        }
    }

    // -----------------------------------------------------------------------
    // 7. Background candidates: old months with high ripple but no rollup
    // -----------------------------------------------------------------------
    for ev in &input.events {
        let Some(exp) = parse_rfc3339(&ev.experienced_at) else {
            continue;
        };
        if ev.ripple_strength < 4 {
            continue;
        }
        // Only consider events in months that are NOT in recent weeks or current week.
        if exp >= cur_week.period_start {
            continue;
        }
        let in_recent = recent
            .iter()
            .any(|w| exp >= w.period_start && exp < w.period_end_exclusive);
        if in_recent {
            continue;
        }
        let month = month_for_ym(exp.year(), exp.month(), tz);
        let key = format!("month:{}", month.month_key);
        if !existing_month_map.contains_key(month.month_key.as_str()) && seen_keys.insert(key) {
            requests.push(make_month_request(&month, "background_candidate", None));
        }
    }

    requests
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
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Weekday};

    /// JST (UTC+9) — the timezone used throughout these tests.
    fn jst() -> FixedOffset {
        FixedOffset::east_opt(9 * 3600).unwrap()
    }

    /// Helper: create a `DateTime` in JST from `(year, month, day, hour, min)`.
    fn jst_dt(y: i32, m: u32, d: u32, hh: u32, mm: u32) -> DateTime<FixedOffset> {
        let naive = chrono::NaiveDate::from_ymd_opt(y, m, d)
            .unwrap()
            .and_hms_opt(hh, mm, 0)
            .unwrap();
        jst().from_utc_datetime(&naive)
    }

    /// Helper: RFC 3339 string for a JST datetime.
    fn jst_rfc3339(y: i32, m: u32, d: u32, hh: u32, mm: u32) -> String {
        jst_dt(y, m, d, hh, mm).to_rfc3339()
    }

    // -----------------------------------------------------------------------
    // Test 1: current_week — Monday start
    // -----------------------------------------------------------------------
    #[test]
    fn test_current_week_monday_start() {
        // Wednesday 2026-05-27 10:00 JST
        let now = jst_dt(2026, 5, 27, 10, 0);
        let cw = current_week(now);

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
        let weeks = recent_weeks(now, 4);

        assert_eq!(weeks.len(), 4);
        // Most recent first.
        assert_eq!(
            weeks[0].period_end_exclusive,
            current_week(now).period_start
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
        let weeks = recent_weeks(now, 4);
        let months = recent_months_from_weeks(&weeks, 2);

        assert_eq!(months.len(), 2);
        // Most recent first.
        assert!(months[0].period_start > months[1].period_start);
        // Months are before the oldest week.
        let oldest_week = &weeks[weeks.len() - 1];
        assert!(months[0].period_end_exclusive <= oldest_week.period_start);
    }

    // -----------------------------------------------------------------------
    // Test 4: detects new closed week (W-1 has no rollup)
    // -----------------------------------------------------------------------
    #[test]
    fn test_detects_new_closed_week() {
        let now = jst_dt(2026, 5, 27, 10, 0);
        let recent = recent_weeks(now, 4);

        // No week rollups at all → W-1 should be detected as closed_week.
        let input = RollupPlannerInput {
            existing_week_rollups: vec![],
            existing_month_rollups: vec![],
            events: vec![],
        };
        let reqs = plan_rollup_updates("test-agent", now, &input);

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
        let recent = recent_weeks(now, 4);

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
        let reqs = plan_rollup_updates("test-agent", now, &input);

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
        let recent = recent_weeks(now, 4);
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
        let reqs = plan_rollup_updates("test-agent", now, &input);

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
        let recent = recent_weeks(now, 4);
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
        let reqs = plan_rollup_updates("test-agent", now, &input);

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
        let recent = recent_weeks(now, 4);

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
        let reqs = plan_rollup_updates("test-agent", now, &input);

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
        let recent = recent_weeks(now, 4);

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
        let reqs = plan_rollup_updates("test-agent", now, &input);

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
    // Test 10: detects background candidates
    // -----------------------------------------------------------------------
    #[test]
    fn test_detects_background_candidates() {
        let now = jst_dt(2026, 5, 27, 10, 0);
        let recent = recent_weeks(now, 4);

        // All recent weeks have rollups.
        let mut week_rollups: Vec<ExistingRollupInfo> = vec![];
        for w in &recent {
            week_rollups.push(ExistingRollupInfo {
                period_key: w.week_key.clone(),
                event_count: 1,
                max_ripple: 1,
                summary_md: String::new(),
            });
        }

        // Event in a much older month with high ripple and no rollup.
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
        let reqs = plan_rollup_updates("test-agent", now, &input);

        let bg = reqs.iter().find(|r| r.reason == "background_candidate");
        assert!(
            bg.is_some(),
            "should detect background candidate for old month with high ripple"
        );
        assert_eq!(bg.unwrap().granularity, RollupGranularity::Month);
        assert!(bg.unwrap().period_key.starts_with("2026-01"));
    }

    // -----------------------------------------------------------------------
    // Test 11: returns empty when no updates needed
    // -----------------------------------------------------------------------
    #[test]
    fn test_returns_empty_when_no_updates_needed() {
        let now = jst_dt(2026, 5, 27, 10, 0);
        let recent = recent_weeks(now, 4);
        let recent_months = recent_months_from_weeks(&recent, 2);

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
        let reqs = plan_rollup_updates("test-agent", now, &input);

        assert!(
            reqs.is_empty(),
            "should return empty when everything is up to date, got: {reqs:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Test 12: excludes current week events from month rollup
    // -----------------------------------------------------------------------
    #[test]
    fn test_excludes_current_week_events_from_month_rollup() {
        let now = jst_dt(2026, 5, 27, 10, 0);
        let cur = current_week(now);
        let recent = recent_weeks(now, 4);

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
        let reqs = plan_rollup_updates("test-agent", now, &input);

        // Should NOT produce a background_candidate for current week's month.
        let bg = reqs.iter().find(|r| {
            r.reason == "background_candidate" && r.granularity == RollupGranularity::Month
        });
        assert!(
            bg.is_none(),
            "current week events should NOT trigger background_candidate month rollup"
        );
    }
}
