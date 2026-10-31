//! When an intention fires next, judged exactly as the scheduler judges it.

use std::sync::Arc;

use chrono::{DateTime, Utc};

use super::definition::{
    TemporalIntention, TemporalSchedule, check_due, estimate_next_day_at, estimate_next_due,
    generate_due_key,
};
use crate::error::StorageError;
use crate::storage::Database;

/// The next firing of an enabled intention.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum NextDue {
    /// The time has passed and the current period is unconsumed; the next
    /// scan fires it.
    DueNow,
    /// The next firing is at this instant.
    At(DateTime<Utc>),
    /// The current period already ran and the calendar offers no later
    /// firing within it.
    AttemptedThisPeriod,
    /// A `once` intention that already ran; it never retries.
    AttemptedOnce,
    /// No future firing exists (e.g. a `once` whose date has passed).
    NoUpcoming,
}

/// Resolves when `intention` fires next by combining the calendar estimate
/// ([`estimate_next_due`]) with the run history the scheduler gate enforces
/// ([`Database::has_pulse_due_run`]). Sharing this judgment keeps every view
/// from promising a firing the scheduler will suppress, or missing one it
/// will perform.
///
/// # Errors
///
/// Returns [`StorageError`] when run-history lookups fail.
pub(crate) async fn resolve_next_due(
    db: &Arc<Database>,
    agent_id: &str,
    intention: &TemporalIntention,
    timezone: &str,
    now: DateTime<Utc>,
) -> Result<NextDue, StorageError> {
    let last_success_at = if matches!(intention.schedule, TemporalSchedule::Interval { .. }) {
        let agent_id = agent_id.to_string();
        let intention_id = intention.id.clone();
        crate::storage::call_blocking(Arc::clone(db), move |db| {
            db.get_last_success_started_at(&agent_id, &intention_id)
        })
        .await?
    } else {
        None
    };

    let current_key = generate_due_key(agent_id, intention, now, timezone);
    let consumed = {
        let agent_id = agent_id.to_string();
        let intention_id = intention.id.clone();
        let key = current_key.clone();
        crate::storage::call_blocking(Arc::clone(db), move |db| {
            db.has_pulse_due_run(&agent_id, &intention_id, &key)
        })
        .await?
    };

    if check_due(agent_id, intention, now, timezone, last_success_at).due && !consumed {
        return Ok(NextDue::DueNow);
    }
    if consumed && matches!(intention.schedule, TemporalSchedule::Once { .. }) {
        return Ok(NextDue::AttemptedOnce);
    }

    Ok(
        match estimate_next_due(intention, now, timezone, last_success_at) {
            None => NextDue::NoUpcoming,
            Some(candidate) => {
                let candidate_key = generate_due_key(agent_id, intention, candidate, timezone);
                if consumed && candidate_key == current_key {
                    match &intention.schedule {
                        // A daily, weekly or once period is spent by its run. An
                        // interval window stays open until a run succeeds and the
                        // key advances with the local date, so the scheduler fires
                        // it again tomorrow instead of never within this period.
                        TemporalSchedule::Interval { at, .. } => {
                            estimate_next_day_at(at, now, timezone)
                                .map_or(NextDue::AttemptedThisPeriod, NextDue::At)
                        }
                        _ => NextDue::AttemptedThisPeriod,
                    }
                } else if candidate <= now {
                    NextDue::DueNow
                } else {
                    NextDue::At(candidate)
                }
            }
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::test_config;

    fn utc(year: i32, month: u32, day: u32, hour: u32) -> DateTime<Utc> {
        chrono::TimeZone::with_ymd_and_hms(&Utc, year, month, day, hour, 0, 0)
            .single()
            .expect("instant")
    }

    fn database() -> (Arc<Database>, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = test_config(dir.path().to_str().expect("utf8"));
        let db = Arc::new(Database::new(&config.db_path()).expect("db"));
        (db, dir)
    }

    fn intention(schedule: TemporalSchedule) -> TemporalIntention {
        TemporalIntention {
            id: "check".to_string(),
            enabled: true,
            schedule,
            attention: String::new(),
            delivery: None,
        }
    }

    fn interval(days: u32, at: &str) -> TemporalIntention {
        intention(TemporalSchedule::Interval {
            interval_days: days,
            at: at.to_string(),
        })
    }

    async fn resolve(
        db: &Arc<Database>,
        intention: &TemporalIntention,
        now: DateTime<Utc>,
    ) -> NextDue {
        resolve_next_due(db, "lyre", intention, "UTC", now)
            .await
            .expect("resolve")
    }

    #[tokio::test]
    async fn interval_that_already_ran_today_fires_again_tomorrow() {
        // Arrange: an open window that already had its run today.
        let (db, _dir) = database();
        let intention = interval(1, "09:00");
        let now = utc(2026, 5, 10, 10);
        let key = generate_due_key("lyre", &intention, now, "UTC");
        db.try_create_pulse_run("run-1", "lyre", "check", &key, "x")
            .expect("run");

        // Act
        let next = resolve(&db, &intention, now).await;

        // Assert: the scheduler retries once the local date advances, not never.
        assert_eq!(next, NextDue::At(utc(2026, 5, 11, 9)));
    }

    #[tokio::test]
    async fn daily_that_already_ran_today_has_no_firing_left_today() {
        // Arrange: today's slot is spent before its time arrives.
        let (db, _dir) = database();
        let intention = intention(TemporalSchedule::Daily {
            at: "09:00".to_string(),
        });
        let now = utc(2026, 5, 10, 8);
        let key = generate_due_key("lyre", &intention, now, "UTC");
        db.try_create_pulse_run("run-1", "lyre", "check", &key, "x")
            .expect("run");

        // Act
        let next = resolve(&db, &intention, now).await;

        // Assert: a daily period is spent by its attempt.
        assert_eq!(next, NextDue::AttemptedThisPeriod);
    }
}
