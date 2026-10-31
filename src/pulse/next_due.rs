//! When an intention fires next, judged exactly as the scheduler judges it.

use std::sync::Arc;

use chrono::{DateTime, Utc};

use super::definition::{
    TemporalIntention, TemporalSchedule, check_due, estimate_next_due, generate_due_key,
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
                    NextDue::AttemptedThisPeriod
                } else if candidate <= now {
                    NextDue::DueNow
                } else {
                    NextDue::At(candidate)
                }
            }
        },
    )
}
