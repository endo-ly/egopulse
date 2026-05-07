//! Sleep batch scheduler — schedule calculation and execution control.

use chrono::{DateTime, Duration, LocalResult, NaiveTime, TimeZone, Utc};
use chrono_tz::Tz;

use crate::config::SleepBatchConfig;

/// Returns the next scheduled run as a UTC instant, or `None` if the scheduler
/// is disabled or the configuration is incomplete.
///
/// The result is a **pure function** of `(schedule, timezone, now)` with no
/// side-effects, making it straightforward to unit-test without tokio.
pub(crate) fn next_scheduled_run(
    config: &SleepBatchConfig,
    now: DateTime<Utc>,
) -> Option<DateTime<Utc>> {
    if !config.scheduler_enabled() {
        return None;
    }
    let schedule = config.schedule.as_ref()?;
    let timezone = config.timezone.as_ref()?;
    let tz: Tz = timezone.parse().ok()?;
    let time = parse_schedule_time(schedule)?;
    next_run_for_time(tz, time, now)
}

fn parse_schedule_time(schedule: &str) -> Option<NaiveTime> {
    let (hour, minute) = parse_hhmm(schedule)?;
    NaiveTime::from_hms_opt(hour, minute, 0)
}

fn parse_hhmm(schedule: &str) -> Option<(u32, u32)> {
    let (h, m) = schedule.split_once(':')?;
    let hour: u32 = h.parse().ok()?;
    let minute: u32 = m.parse().ok()?;
    if hour > 23 || minute > 59 {
        return None;
    }
    Some((hour, minute))
}

fn next_run_for_time(
    tz: Tz,
    time: NaiveTime,
    now: DateTime<Utc>,
) -> Option<DateTime<Utc>> {
    let local_now = now.with_timezone(&tz);
    let today_date = local_now.date_naive();

    if let Some(instant) = try_date(tz, today_date, time, &local_now) {
        return Some(instant);
    }

    let tomorrow_date = today_date + Duration::days(1);
    try_date(tz, tomorrow_date, time, &local_now)
}

fn try_date(
    tz: Tz,
    date: chrono::NaiveDate,
    time: NaiveTime,
    local_now: &DateTime<Tz>,
) -> Option<DateTime<Utc>> {
    let naive = date.and_time(time);
    match tz.from_local_datetime(&naive) {
        LocalResult::Single(dt) => {
            if dt > *local_now {
                Some(dt.with_timezone(&Utc))
            } else {
                None
            }
        }
        LocalResult::Ambiguous(earliest, _latest) => {
            if earliest > *local_now {
                Some(earliest.with_timezone(&Utc))
            } else {
                None
            }
        }
        LocalResult::None => resolve_gap(tz, naive, local_now),
    }
}

fn resolve_gap(
    tz: Tz,
    start: chrono::NaiveDateTime,
    local_now: &DateTime<Tz>,
) -> Option<DateTime<Utc>> {
    let mut candidate = start;
    for _ in 0..120 {
        candidate = candidate + Duration::minutes(1);
        if let LocalResult::Single(dt) = tz.from_local_datetime(&candidate) {
            if dt > *local_now {
                return Some(dt.with_timezone(&Utc));
            }
            return None;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enabled_config(schedule: &str, timezone: &str) -> SleepBatchConfig {
        SleepBatchConfig {
            enabled: true,
            schedule: Some(schedule.to_string()),
            timezone: Some(timezone.to_string()),
            ..Default::default()
        }
    }

    #[test]
    fn next_run_returns_today_when_time_is_future() {
        let config = enabled_config("14:00", "Asia/Tokyo");
        // 2026-01-15 05:00 UTC = 2026-01-15 14:00 JST
        // Set now to 13:00 JST = 04:00 UTC → target is still in the future
        let now = "2026-01-15T04:00:00Z".parse::<DateTime<Utc>>().unwrap();
        let result = next_scheduled_run(&config, now).unwrap();
        // Expected: 2026-01-15 14:00 JST = 2026-01-15 05:00 UTC
        assert_eq!(result, "2026-01-15T05:00:00Z".parse::<DateTime<Utc>>().unwrap());
    }

    #[test]
    fn next_run_returns_tomorrow_when_time_has_passed() {
        let config = enabled_config("14:00", "Asia/Tokyo");
        // Set now to 15:00 JST = 06:00 UTC → target already passed today
        let now = "2026-01-15T06:00:00Z".parse::<DateTime<Utc>>().unwrap();
        let result = next_scheduled_run(&config, now).unwrap();
        // Expected: 2026-01-16 14:00 JST = 2026-01-16 05:00 UTC
        assert_eq!(result, "2026-01-16T05:00:00Z".parse::<DateTime<Utc>>().unwrap());
    }

    #[test]
    fn next_run_uses_configured_iana_timezone() {
        let config = enabled_config("09:00", "America/New_York");
        // 2026-01-15 04:00 UTC = 2026-01-14 23:00 EST → 09:00 EST is tomorrow
        let now = "2026-01-15T04:00:00Z".parse::<DateTime<Utc>>().unwrap();
        let result = next_scheduled_run(&config, now).unwrap();
        // Expected: 2026-01-15 09:00 EST = 2026-01-15 14:00 UTC
        assert_eq!(result, "2026-01-15T14:00:00Z".parse::<DateTime<Utc>>().unwrap());
    }

    #[test]
    fn next_run_handles_utc_timezone() {
        let config = enabled_config("04:00", "UTC");
        let now = "2026-01-15T03:00:00Z".parse::<DateTime<Utc>>().unwrap();
        let result = next_scheduled_run(&config, now).unwrap();
        assert_eq!(result, "2026-01-15T04:00:00Z".parse::<DateTime<Utc>>().unwrap());
    }

    #[test]
    fn next_run_moves_dst_gap_to_first_valid_time() {
        // America/New_York: DST starts 2026-03-08 at 02:00 EST → clocks jump to 03:00 EDT.
        // Local time 02:30 does not exist. Should move to 03:00 EDT.
        let config = enabled_config("02:30", "America/New_York");
        // 2026-03-08 01:00 EST = 06:00 UTC (before the gap)
        let now = "2026-03-08T06:00:00Z".parse::<DateTime<Utc>>().unwrap();
        let result = next_scheduled_run(&config, now).unwrap();
        // Expected: 2026-03-08 03:00 EDT = 07:00 UTC
        assert_eq!(result, "2026-03-08T07:00:00Z".parse::<DateTime<Utc>>().unwrap());
    }

    #[test]
    fn next_run_uses_earliest_instant_for_dst_fold() {
        // America/New_York: DST ends 2026-11-01 at 02:00 EDT → clocks fall back to 01:00 EST.
        // Local time 01:30 exists twice. Use earliest (EDT) = 05:30 UTC.
        let config = enabled_config("01:30", "America/New_York");
        // 2026-11-01 00:00 EDT = 04:00 UTC (before the fold)
        let now = "2026-11-01T04:00:00Z".parse::<DateTime<Utc>>().unwrap();
        let result = next_scheduled_run(&config, now).unwrap();
        // Expected: 2026-11-01 01:30 EDT = 05:30 UTC (earliest instant)
        assert_eq!(result, "2026-11-01T05:30:00Z".parse::<DateTime<Utc>>().unwrap());
    }

    #[test]
    fn next_run_rejects_invalid_local_time() {
        let config = enabled_config("99:00", "Asia/Tokyo");
        let now = "2026-01-15T00:00:00Z".parse::<DateTime<Utc>>().unwrap();
        assert!(next_scheduled_run(&config, now).is_none());
    }

    #[test]
    fn scheduler_config_disabled_has_no_next_run() {
        let config = SleepBatchConfig {
            enabled: false,
            ..Default::default()
        };
        let now = "2026-01-15T00:00:00Z".parse::<DateTime<Utc>>().unwrap();
        assert!(next_scheduled_run(&config, now).is_none());
    }
}
