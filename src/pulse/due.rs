//! Temporal due resolver for Pulse intentions.
//!
//! Determines when a [`TemporalIntention`] is "due" and generates a unique
//! `due_key` for deduplication. Supports daily, weekly, and once schedules
//! with IANA timezone-aware evaluation and DST-safe datetime construction.

use chrono::{DateTime, Datelike, LocalResult, NaiveTime, TimeZone, Utc, Weekday};
use chrono_tz::Tz;

use super::definition::{TemporalIntention, TemporalSchedule};

/// Result of a due check for a single intention.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DueCheck {
    /// Whether the intention is currently due.
    pub due: bool,
    /// Unique key identifying the current evaluation period for deduplication.
    pub due_key: String,
}

/// Check if a temporal intention is due at the given time and produce its deduplication key.
///
/// `agent_id` identifies the agent for due_key generation.
/// `now` is the current UTC time.
/// `timezone` is the IANA timezone for daily/weekly evaluation (e.g. "Asia/Tokyo").
pub(crate) fn check_due(
    agent_id: &str,
    intention: &TemporalIntention,
    now: DateTime<Utc>,
    timezone: &str,
) -> DueCheck {
    let due_key = generate_due_key(agent_id, intention, now, timezone);
    let due = match &intention.schedule {
        TemporalSchedule::Daily { at } => is_daily_due(at, now, timezone),
        TemporalSchedule::Weekly { day, at } => is_weekly_due(day, at, now, timezone),
        TemporalSchedule::Once { at } => is_once_due(at, now),
    };
    DueCheck { due, due_key }
}

/// Generate the deduplication key for a given intention at the current evaluation time.
///
/// Format per schedule kind:
/// - daily:  `{agent_id}:{intention_id}:{YYYY-MM-DD}` (local date)
/// - weekly: `{agent_id}:{intention_id}:{YYYY-WNN}`   (ISO week)
/// - once:   `{agent_id}:{intention_id}:{RFC3339_at}`  (exact scheduled instant)
pub(crate) fn generate_due_key(
    agent_id: &str,
    intention: &TemporalIntention,
    now: DateTime<Utc>,
    timezone: &str,
) -> String {
    let tz: Tz = timezone.parse().unwrap_or(Tz::UTC);
    match &intention.schedule {
        TemporalSchedule::Daily { .. } => {
            let local_now = now.with_timezone(&tz);
            format!(
                "{agent_id}:{}:{}",
                intention.id,
                local_now.format("%Y-%m-%d")
            )
        }
        TemporalSchedule::Weekly { .. } => {
            let local_now = now.with_timezone(&tz);
            let iso = local_now.iso_week();
            format!(
                "{agent_id}:{}:{}-W{:02}",
                intention.id,
                iso.year(),
                iso.week()
            )
        }
        TemporalSchedule::Once { at } => {
            format!("{agent_id}:{}:{at}", intention.id)
        }
    }
}

/// Evaluate daily schedule: parse `HH:MM`, construct today's local time in the
/// configured timezone, and check if `now` has passed it.
///
/// DST handling:
/// - **Gap** (non-existent local time, e.g. spring forward): treated as not due (skip).
/// - **Fold** (ambiguous local time, e.g. fall back): uses the earlier occurrence.
fn is_daily_due(at: &str, now: DateTime<Utc>, timezone: &str) -> bool {
    let time = match parse_hhmm(at) {
        Some(t) => t,
        None => {
            tracing::warn!("invalid time format in daily schedule: {at}");
            return false;
        }
    };
    let tz: Tz = match timezone.parse() {
        Ok(tz) => tz,
        Err(e) => {
            tracing::warn!("invalid timezone \"{timezone}\": {e}");
            return false;
        }
    };

    let local_now = now.with_timezone(&tz);
    let naive_dt = local_now.date_naive().and_time(time);

    let target = match tz.from_local_datetime(&naive_dt) {
        LocalResult::None => {
            tracing::debug!("skipping daily intention: local time {at} falls in DST gap");
            return false;
        }
        LocalResult::Single(dt) => dt,
        LocalResult::Ambiguous(earliest, _latest) => earliest,
    };

    now >= target.with_timezone(&Utc)
}

/// Evaluate weekly schedule: first check if the current local weekday matches
/// the configured `day`, then delegate to the daily time check.
fn is_weekly_due(day: &str, at: &str, now: DateTime<Utc>, timezone: &str) -> bool {
    let target_weekday = match parse_weekday(day) {
        Some(w) => w,
        None => {
            tracing::warn!("invalid weekday in weekly schedule: {day}");
            return false;
        }
    };
    let tz: Tz = match timezone.parse() {
        Ok(tz) => tz,
        Err(e) => {
            tracing::warn!("invalid timezone \"{timezone}\": {e}");
            return false;
        }
    };

    let local_now = now.with_timezone(&tz);
    if local_now.weekday() != target_weekday {
        return false;
    }

    is_daily_due(at, now, timezone)
}

/// Evaluate once schedule: parse the RFC3339 `at` instant and check if `now`
/// has reached it.
fn is_once_due(at: &str, now: DateTime<Utc>) -> bool {
    match DateTime::parse_from_rfc3339(at) {
        Ok(scheduled) => now >= scheduled.to_utc(),
        Err(e) => {
            tracing::warn!("invalid RFC3339 in once schedule: {at}: {e}");
            false
        }
    }
}

/// Parse a `HH:MM` time string into a [`NaiveTime`].
fn parse_hhmm(at: &str) -> Option<NaiveTime> {
    NaiveTime::parse_from_str(at, "%H:%M").ok()
}

/// Map a lowercase weekday abbreviation to [`Weekday`].
fn parse_weekday(day: &str) -> Option<Weekday> {
    match day {
        "mon" => Some(Weekday::Mon),
        "tue" => Some(Weekday::Tue),
        "wed" => Some(Weekday::Wed),
        "thu" => Some(Weekday::Thu),
        "fri" => Some(Weekday::Fri),
        "sat" => Some(Weekday::Sat),
        "sun" => Some(Weekday::Sun),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- Helpers ---

    fn make_daily(at: &str) -> TemporalIntention {
        TemporalIntention {
            id: "test_intention".to_string(),
            schedule: TemporalSchedule::Daily { at: at.to_string() },
            attention: String::new(),
        }
    }

    fn make_weekly(day: &str, at: &str) -> TemporalIntention {
        TemporalIntention {
            id: "test_intention".to_string(),
            schedule: TemporalSchedule::Weekly {
                day: day.to_string(),
                at: at.to_string(),
            },
            attention: String::new(),
        }
    }

    fn make_once(at: &str) -> TemporalIntention {
        TemporalIntention {
            id: "test_intention".to_string(),
            schedule: TemporalSchedule::Once { at: at.to_string() },
            attention: String::new(),
        }
    }

    fn utc(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, mo, d, h, mi, s).unwrap()
    }

    // --- 1. daily_due_after_local_time ---

    #[test]
    fn daily_due_after_local_time() {
        // Arrange: intention at 09:00 JST (UTC+9) = 00:00 UTC
        let intention = make_daily("09:00");
        let now = utc(2026, 5, 10, 0, 1, 0); // 09:01 JST

        // Act
        let result = check_due("lyre", &intention, now, "Asia/Tokyo");

        // Assert
        assert!(result.due);
    }

    // --- 2. daily_not_due_before_local_time ---

    #[test]
    fn daily_not_due_before_local_time() {
        // Arrange: intention at 09:00 JST = 00:00 UTC
        //          now is 08:59 JST on May 10 = 23:59 UTC on May 9
        let intention = make_daily("09:00");
        let now = utc(2026, 5, 9, 23, 59, 0);

        // Act
        let result = check_due("lyre", &intention, now, "Asia/Tokyo");

        // Assert
        assert!(!result.due);
    }

    // --- 3. weekly_due_only_on_matching_day ---

    #[test]
    fn weekly_due_only_on_matching_day() {
        // Arrange: intention for Sunday 21:00 JST
        let intention = make_weekly("sun", "21:00");

        // Act & Assert: Sunday May 10 2026 at 21:01 JST = 12:01 UTC -> due
        let sunday = utc(2026, 5, 10, 12, 1, 0);
        assert!(
            check_due("lyre", &intention, sunday, "Asia/Tokyo").due,
            "should be due on Sunday after 21:00 JST"
        );

        // Act & Assert: Saturday May 9 2026 at 21:01 JST = 12:01 UTC -> not due
        let saturday = utc(2026, 5, 9, 12, 1, 0);
        assert!(
            !check_due("lyre", &intention, saturday, "Asia/Tokyo").due,
            "should not be due on Saturday"
        );
    }

    // --- 4. once_due_after_rfc3339_at ---

    #[test]
    fn once_due_after_rfc3339_at() {
        // Arrange: intention at 2026-05-12T18:00:00+09:00 = 09:00 UTC
        let intention = make_once("2026-05-12T18:00:00+09:00");

        // Act & Assert: now after scheduled instant -> due
        let after = utc(2026, 5, 12, 9, 30, 0);
        assert!(check_due("lyre", &intention, after, "Asia/Tokyo").due);

        // Act & Assert: now before scheduled instant -> not due
        let before = utc(2026, 5, 12, 8, 30, 0);
        assert!(!check_due("lyre", &intention, before, "Asia/Tokyo").due);
    }

    // --- 5. due_key_daily_uses_local_date ---

    #[test]
    fn due_key_daily_uses_local_date() {
        // Arrange: 2026-05-10 00:00 UTC = 2026-05-10 09:00 JST
        let intention = TemporalIntention {
            id: "morning_review".to_string(),
            schedule: TemporalSchedule::Daily {
                at: "09:00".to_string(),
            },
            attention: String::new(),
        };
        let now = utc(2026, 5, 10, 0, 0, 0);

        // Act
        let key = generate_due_key("lyre", &intention, now, "Asia/Tokyo");

        // Assert
        assert_eq!(key, "lyre:morning_review:2026-05-10");
    }

    // --- 6. due_key_weekly_uses_iso_week ---

    #[test]
    fn due_key_weekly_uses_iso_week() {
        // Arrange: May 10 2026 is Sunday of ISO week 19
        //          12:00 UTC = 21:00 JST
        let intention = TemporalIntention {
            id: "weekly_reflection".to_string(),
            schedule: TemporalSchedule::Weekly {
                day: "sun".to_string(),
                at: "21:00".to_string(),
            },
            attention: String::new(),
        };
        let now = utc(2026, 5, 10, 12, 0, 0);

        // Act
        let key = generate_due_key("kitara", &intention, now, "Asia/Tokyo");

        // Assert
        assert_eq!(key, "kitara:weekly_reflection:2026-W19");
    }

    // --- 7. due_key_once_uses_once_instant ---

    #[test]
    fn due_key_once_uses_once_instant() {
        // Arrange
        let intention = TemporalIntention {
            id: "event_check".to_string(),
            schedule: TemporalSchedule::Once {
                at: "2026-05-12T18:00:00+09:00".to_string(),
            },
            attention: String::new(),
        };
        let now = utc(2026, 5, 12, 9, 30, 0);

        // Act
        let key = generate_due_key("lyre", &intention, now, "Asia/Tokyo");

        // Assert
        assert_eq!(key, "lyre:event_check:2026-05-12T18:00:00+09:00");
    }

    // --- 8. due_resolver_handles_dst_gap_and_fold ---

    #[test]
    fn due_resolver_handles_dst_gap_and_fold() {
        // America/New_York DST transitions in 2026:
        //   Spring forward: 2026-03-08 02:00 EST -> 03:00 EDT
        //     (gap: 02:00-03:00 local does not exist)
        //   Fall back: 2026-11-01 02:00 EDT -> 01:00 EST
        //     (fold: 01:00-02:00 local occurs twice)

        // --- Spring-forward gap: 02:30 local does not exist ---
        let gap_intention = make_daily("02:30");
        // 07:30 UTC on 2026-03-08 = 03:30 EDT (well past the gap)
        let now_after_gap = utc(2026, 3, 8, 7, 30, 0);
        let result = check_due("test", &gap_intention, now_after_gap, "America/New_York");

        assert!(
            !result.due,
            "intention at 02:30 should not be due during DST gap"
        );
        assert!(
            result.due_key.contains("2026-03-08"),
            "due_key should still contain the local date even during gap"
        );

        // --- Fall-back fold: 01:30 local occurs twice ---
        // Earlier occurrence: 01:30 EDT = 05:30 UTC
        // Later   occurrence: 01:30 EST = 06:30 UTC
        let fold_intention = make_daily("01:30");

        // At 05:31 UTC = 01:31 EDT -> earlier occurrence has passed -> due
        let now_after_earlier = utc(2026, 11, 1, 5, 31, 0);
        let result = check_due(
            "test",
            &fold_intention,
            now_after_earlier,
            "America/New_York",
        );
        assert!(
            result.due,
            "intention at 01:30 should be due after earlier occurrence in fold"
        );

        // At 05:29 UTC = 01:29 EDT -> earlier occurrence hasn't arrived yet -> not due
        let now_before_earlier = utc(2026, 11, 1, 5, 29, 0);
        let result = check_due(
            "test",
            &fold_intention,
            now_before_earlier,
            "America/New_York",
        );
        assert!(
            !result.due,
            "intention at 01:30 should not be due before earlier occurrence"
        );
    }
}
