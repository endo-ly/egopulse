//! Per-session turn scheduler with concurrency control and runaway prevention.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

use crate::agent_loop::ScheduledTurn;

/// Maximum chain depth for `agent_send` cascading (A→B→C…).
pub(crate) const MAX_AGENT_CHAIN_DEPTH: usize = 4;

/// Maximum turns allowed per human-originated input chain.
pub(crate) const MAX_AGENT_TURNS_PER_INPUT: usize = 12;

/// Reasons a scheduled turn may be rejected or stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum StopReason {
    ChainDepthExceeded,
    TurnCountExceeded,
    AgentNotFound,
    LlmFailure,
    #[allow(dead_code)]
    SessionUnprocessable,
}

/// Per-origin turn counter for runaway prevention.
#[derive(Debug, Default)]
pub(crate) struct TurnTracker {
    counts: Mutex<HashMap<String, usize>>,
}

impl TurnTracker {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn increment(&self, origin_id: &str) {
        let mut counts = self.counts.lock().expect("turn_tracker lock");
        *counts.entry(origin_id.to_string()).or_insert(0) += 1;
    }

    pub(crate) fn count(&self, origin_id: &str) -> usize {
        let counts = self.counts.lock().expect("turn_tracker lock");
        counts.get(origin_id).copied().unwrap_or(0)
    }
}

/// Slot state for a single agent session within the scheduler.
struct TurnSlot {
    busy: bool,
    queue: VecDeque<ScheduledTurn>,
}

/// Per-session busy flag and input queue for ordered turn execution.
///
/// When a turn is submitted for a session that is already processing a turn,
/// the new turn is enqueued. After the current turn completes, the caller
/// invokes [`TurnScheduler::on_turn_completed`] to drain the next queued turn.
pub(crate) struct TurnScheduler {
    slots: Mutex<HashMap<String, TurnSlot>>,
}

impl TurnScheduler {
    pub(crate) fn new() -> Self {
        Self {
            slots: Mutex::new(HashMap::new()),
        }
    }

    /// Submits a turn for execution.
    ///
    /// Returns `Ok(Some(turn))` when the slot is idle — the caller should
    /// begin executing the returned turn immediately. Returns `Ok(None)` when
    /// the slot is busy — the turn has been enqueued for later execution.
    pub(crate) fn submit(&self, turn: ScheduledTurn) -> Option<ScheduledTurn> {
        let mut slots = self.slots.lock().expect("turn_scheduler lock");
        let slot = slots.entry(turn.session_key()).or_insert_with(|| TurnSlot {
            busy: false,
            queue: VecDeque::new(),
        });

        if slot.busy {
            slot.queue.push_back(turn);
            None
        } else {
            slot.busy = true;
            // Drop lock before returning to avoid holding it during execution.
            Some(turn)
        }
    }

    /// Called after a turn completes. Returns the next queued turn for this
    /// session, or `None` if the queue is empty (slot becomes idle).
    pub(crate) fn on_turn_completed(&self, session_key: &str) -> Option<ScheduledTurn> {
        let mut slots = self.slots.lock().expect("turn_scheduler lock");
        if let Some(slot) = slots.get_mut(session_key) {
            if let Some(next) = slot.queue.pop_front() {
                return Some(next);
            }
            slot.busy = false;
        }
        None
    }

    /// Returns `true` if the given session currently has a turn in progress.
    #[cfg(test)]
    fn is_busy(&self, session_key: &str) -> bool {
        let slots = self.slots.lock().expect("turn_scheduler lock");
        slots.get(session_key).is_some_and(|s| s.busy)
    }

    /// Returns the number of queued turns for the given session.
    #[cfg(test)]
    fn queue_len(&self, session_key: &str) -> usize {
        let slots = self.slots.lock().expect("turn_scheduler lock");
        slots.get(session_key).map_or(0, |s| s.queue.len())
    }
}

/// Evaluates all stop conditions for a scheduled turn.
///
/// Returns `Some(StopReason)` if the turn should be rejected, or `None` if
/// execution may proceed.
pub(crate) fn evaluate_stop_conditions(
    chain_depth: usize,
    turn_count: usize,
    agent_id: &str,
    valid_agent_ids: &[&str],
) -> Option<StopReason> {
    if chain_depth > MAX_AGENT_CHAIN_DEPTH {
        return Some(StopReason::ChainDepthExceeded);
    }
    if turn_count >= MAX_AGENT_TURNS_PER_INPUT {
        return Some(StopReason::TurnCountExceeded);
    }
    if !valid_agent_ids.contains(&agent_id) {
        return Some(StopReason::AgentNotFound);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_loop::SurfaceContext;

    fn test_context(agent_id: &str) -> SurfaceContext {
        SurfaceContext::new(
            "discord".to_string(),
            "user".to_string(),
            format!("ch1:agent:{agent_id}"),
            "discord".to_string(),
            agent_id.to_string(),
        )
    }

    fn test_turn(agent_id: &str, origin_id: &str) -> ScheduledTurn {
        ScheduledTurn {
            context: test_context(agent_id),
            input: "hello".to_string(),
            external_chat_id: "ext123".to_string(),
            origin_id: origin_id.to_string(),
        }
    }

    // ---- ScheduledTurn tests ----

    #[test]
    fn scheduled_turn_from_surface_context() {
        let turn = test_turn("agent_a", "orig-1");
        assert_eq!(turn.context.agent_id, "agent_a");
        assert_eq!(turn.input, "hello");
        assert_eq!(turn.external_chat_id, "ext123");
        assert_eq!(turn.origin_id, "orig-1");
        assert_eq!(turn.context.chain_depth, 0);
    }

    #[test]
    fn scheduled_turn_session_key_matches_surface() {
        let turn = test_turn("agent_a", "orig-1");
        assert_eq!(turn.session_key(), turn.context.session_key());
    }

    // ---- TurnScheduler tests ----

    #[test]
    fn turn_scheduler_new_is_empty() {
        let scheduler = TurnScheduler::new();
        assert!(!scheduler.is_busy("discord:ch1:agent:a"));
    }

    #[test]
    fn turn_scheduler_submit_first_turn_sets_busy() {
        let scheduler = TurnScheduler::new();
        let turn = test_turn("agent_a", "orig-1");
        let result = scheduler.submit(turn);

        assert!(result.is_some());
        let key = "discord:ch1:agent:agent_a";
        assert!(scheduler.is_busy(key));
        assert_eq!(scheduler.queue_len(key), 0);
    }

    #[test]
    fn turn_scheduler_submit_second_turn_enqueues() {
        let scheduler = TurnScheduler::new();
        let key = "discord:ch1:agent:agent_a";

        let turn1 = test_turn("agent_a", "orig-1");
        scheduler.submit(turn1);

        let turn2 = test_turn("agent_a", "orig-1");
        let result = scheduler.submit(turn2);

        assert!(result.is_none());
        assert!(scheduler.is_busy(key));
        assert_eq!(scheduler.queue_len(key), 1);
    }

    #[test]
    fn turn_scheduler_drain_after_completion() {
        let scheduler = TurnScheduler::new();
        let key = "discord:ch1:agent:agent_a";

        let turn1 = test_turn("agent_a", "orig-1");
        scheduler.submit(turn1);

        let turn2 = test_turn("agent_a", "orig-2");
        scheduler.submit(turn2);

        let next = scheduler.on_turn_completed(key);
        assert!(next.is_some());
        assert_eq!(next.unwrap().origin_id, "orig-2");
        assert!(scheduler.is_busy(key));
    }

    #[test]
    fn turn_scheduler_drain_empty_clears_busy() {
        let scheduler = TurnScheduler::new();
        let key = "discord:ch1:agent:agent_a";

        let turn = test_turn("agent_a", "orig-1");
        scheduler.submit(turn);

        let next = scheduler.on_turn_completed(key);
        assert!(next.is_none());
        assert!(!scheduler.is_busy(key));
    }

    #[test]
    fn turn_scheduler_different_sessions_independent() {
        let scheduler = TurnScheduler::new();
        let key_a = "discord:ch1:agent:agent_a";
        let key_b = "discord:ch1:agent:agent_b";

        let turn_a = test_turn("agent_a", "orig-1");
        let turn_b = test_turn("agent_b", "orig-1");

        let result_a = scheduler.submit(turn_a);
        let result_b = scheduler.submit(turn_b);

        assert!(result_a.is_some());
        assert!(result_b.is_some());
        assert!(scheduler.is_busy(key_a));
        assert!(scheduler.is_busy(key_b));
    }

    // ---- Stop Condition tests ----

    #[test]
    fn stop_condition_chain_depth_exceeded() {
        let result = evaluate_stop_conditions(5, 0, "agent_a", &["agent_a"]);
        assert_eq!(result, Some(StopReason::ChainDepthExceeded));
    }

    #[test]
    fn stop_condition_turn_count_exceeded() {
        let result = evaluate_stop_conditions(0, 12, "agent_a", &["agent_a"]);
        assert_eq!(result, Some(StopReason::TurnCountExceeded));
    }

    #[test]
    fn stop_condition_agent_not_found() {
        let result = evaluate_stop_conditions(0, 0, "unknown", &["agent_a"]);
        assert_eq!(result, Some(StopReason::AgentNotFound));
    }

    #[test]
    fn stop_condition_none_when_all_ok() {
        let result = evaluate_stop_conditions(1, 5, "agent_a", &["agent_a"]);
        assert_eq!(result, None);
    }

    // ---- TurnTracker tests ----

    #[test]
    fn turn_tracker_increments_per_origin() {
        let tracker = TurnTracker::new();
        tracker.increment("orig-1");
        tracker.increment("orig-1");
        tracker.increment("orig-1");
        assert_eq!(tracker.count("orig-1"), 3);
    }

    #[test]
    fn turn_tracker_different_origins_independent() {
        let tracker = TurnTracker::new();
        tracker.increment("orig-1");
        tracker.increment("orig-1");
        tracker.increment("orig-2");
        assert_eq!(tracker.count("orig-1"), 2);
        assert_eq!(tracker.count("orig-2"), 1);
    }

    #[test]
    fn turn_tracker_count_returns_zero_for_unknown() {
        let tracker = TurnTracker::new();
        assert_eq!(tracker.count("nonexistent"), 0);
    }
}
