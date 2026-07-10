//! Per-session turn scheduler with concurrency control and runaway prevention.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::agent_loop::ScheduledTurn;
use crate::runtime::metrics;

/// In-flight turn tracker used by the sleep scheduler to defer scheduled
/// batches while an agent is actively processing a conversation turn.
#[derive(Debug, Default)]
pub(crate) struct ActiveTurnTracker {
    turns: Mutex<HashMap<String, u32>>,
}

impl ActiveTurnTracker {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn begin_turn(&self, agent_id: &str) {
        let mut turns = self.turns.lock().expect("active_turns lock");
        *turns.entry(agent_id.to_string()).or_insert(0) += 1;
        metrics::set_active_turns_gauge(turns.values().map(|&c| c as usize).sum());
    }

    /// Removes the entry when the count reaches zero so `is_active` stays O(1).
    pub(crate) fn end_turn(&self, agent_id: &str) {
        let mut turns = self.turns.lock().expect("active_turns lock");
        if let Some(count) = turns.get_mut(agent_id) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                turns.remove(agent_id);
            }
        }
        metrics::set_active_turns_gauge(turns.values().map(|&c| c as usize).sum());
    }

    pub(crate) fn is_active(&self, agent_id: &str) -> bool {
        let turns = self.turns.lock().expect("active_turns lock");
        turns.get(agent_id).is_some_and(|&c| c > 0)
    }

    /// Returns the total number of currently in-flight turns across all agents.
    pub(crate) fn total_active(&self) -> usize {
        let turns = self.turns.lock().expect("active_turns lock");
        turns.values().map(|&c| c as usize).sum()
    }
}

/// Maximum chain depth for `agent_send` cascading (A→B→C…).
pub(crate) const MAX_AGENT_CHAIN_DEPTH: usize = 4;

/// Maximum turns allowed per human-originated input chain.
pub(crate) const MAX_AGENT_TURNS_PER_INPUT: usize = 12;

/// Maximum turns queued for a single session before new turns are rejected.
///
/// Bounds memory growth from a single hot session under burst load (e.g.
/// webhook storms or channel floods) while preserving in-session ordering for
/// the accepted window.
pub(crate) const MAX_QUEUED_TURNS_PER_SESSION: usize = 32;

/// Maximum turns queued across the whole runtime before new turns are rejected.
///
/// Bounds total scheduler memory across all sessions during sustained
/// overload. Phase 3 will replace the in-memory queue with a durable one; until
/// then this is an explicit finite capacity, not unbounded delay.
pub(crate) const MAX_GLOBAL_QUEUED_TURNS: usize = 512;

/// Maximum distinct origin IDs tracked by [`TurnTracker`] before new origins
/// are rejected. Each active human input chain has its own origin; this bounds
/// tracker memory during prolonged high cardinality.
pub(crate) const MAX_TRACKED_ORIGINS: usize = 4096;

/// How long a completed or terminal origin is retained before TTL eviction.
///
/// Lets the chain guard reject late follow-up turns for the same origin while
/// keeping the tracker bounded. Active chains refresh `last_touched` on every
/// operation and are never evicted while progressing.
pub(crate) const ORIGIN_TTL: Duration = Duration::from_secs(24 * 60 * 60);

/// Reasons a scheduled turn may be rejected or stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum StopReason {
    ChainDepthExceeded,
    TurnCountExceeded,
    AgentNotFound,
    LlmFailure,
}

impl std::fmt::Display for StopReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ChainDepthExceeded => write!(f, "chain_depth_exceeded"),
            Self::TurnCountExceeded => write!(f, "turn_count_exceeded"),
            Self::AgentNotFound => write!(f, "agent_not_found"),
            Self::LlmFailure => write!(f, "llm_failure"),
        }
    }
}

/// Reason a turn was rejected by the scheduler queue capacity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RejectReason {
    /// The per-session queue reached [`MAX_QUEUED_TURNS_PER_SESSION`].
    SessionQueueFull,
    /// The runtime-wide queue reached [`MAX_GLOBAL_QUEUED_TURNS`].
    GlobalQueueFull,
}

impl RejectReason {
    /// Machine-readable code for the rejection, suitable for error responses
    /// and metric labels.
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::SessionQueueFull => "session_queue_full",
            Self::GlobalQueueFull => "global_queue_full",
        }
    }
}

impl std::fmt::Display for RejectReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Internal result of [`TurnScheduler::submit`].
///
/// `Started` hands the turn back so the caller can begin execution; `Queued`
/// means the turn was buffered behind an in-progress turn; `Rejected` means
/// the turn was refused and must not be executed.
pub(crate) enum ScheduleResult {
    Started(Box<ScheduledTurn>),
    Queued,
    Rejected(RejectReason),
}

/// Caller-facing outcome of submitting a turn.
///
/// Mirrors [`ScheduleResult`] but without the turn payload, since execution is
/// spawned inside the submit path. Lets callers distinguish an accepted turn
/// (immediately started or queued) from a rejected one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SubmitOutcome {
    Started,
    Queued,
    Rejected(RejectReason),
}

/// Clock abstraction used by [`TurnTracker`] for TTL pruning.
///
/// Production uses [`SystemClock`]; tests inject a controllable clock so TTL
/// behaviour can be exercised without wall-clock sleeps.
trait Clock: Send + Sync {
    fn now(&self) -> Instant;
}

struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

/// Per-origin turn counter, terminal stop reason, and last-touched time for
/// runaway prevention. Each origin is one human-input chain.
pub(crate) struct TurnTracker {
    origins: Mutex<HashMap<String, OriginState>>,
    clock: Arc<dyn Clock>,
}

struct OriginState {
    turn_count: usize,
    terminal_reason: Option<StopReason>,
    last_touched: Instant,
}

impl TurnTracker {
    pub(crate) fn new() -> Self {
        Self {
            origins: Mutex::new(HashMap::new()),
            clock: Arc::new(SystemClock),
        }
    }

    /// Increments the turn count for `origin_id`.
    ///
    /// Returns `Some(new_count)` when tracked. Returns `None` when the tracker
    /// is at [`MAX_TRACKED_ORIGINS`] capacity (after TTL pruning) and
    /// `origin_id` is new — the caller must reject the turn rather than execute
    /// it untracked. Existing origins are always updatable, even at capacity.
    pub(crate) fn increment(&self, origin_id: &str) -> Option<usize> {
        let mut origins = self.origins.lock().expect("turn_tracker lock");
        self.prune_stale_locked(&mut origins);
        let now = self.clock.now();
        if let Some(state) = origins.get_mut(origin_id) {
            state.turn_count += 1;
            state.last_touched = now;
            Some(state.turn_count)
        } else if origins.len() >= MAX_TRACKED_ORIGINS {
            None
        } else {
            origins.insert(
                origin_id.to_string(),
                OriginState {
                    turn_count: 1,
                    terminal_reason: None,
                    last_touched: now,
                },
            );
            Some(1)
        }
    }

    pub(crate) fn set_terminal_reason(&self, origin_id: &str, reason: StopReason) {
        let mut origins = self.origins.lock().expect("turn_tracker lock");
        self.prune_stale_locked(&mut origins);
        let now = self.clock.now();
        origins
            .entry(origin_id.to_string())
            .and_modify(|s| {
                s.terminal_reason = Some(reason.clone());
                s.last_touched = now;
            })
            .or_insert(OriginState {
                turn_count: 0,
                terminal_reason: Some(reason),
                last_touched: now,
            });
    }

    pub(crate) fn terminal_reason(&self, origin_id: &str) -> Option<StopReason> {
        let mut origins = self.origins.lock().expect("turn_tracker lock");
        self.prune_stale_locked(&mut origins);
        origins
            .get(origin_id)
            .and_then(|s| s.terminal_reason.clone())
    }

    /// Removes origins whose `last_touched` is older than [`ORIGIN_TTL`].
    ///
    /// Active chains refresh `last_touched` on every operation and are never
    /// evicted while progressing. Removed entries with a terminal reason or
    /// non-zero count are logged so operators can distinguish a genuine stale
    /// eviction from a still-active chain.
    fn prune_stale_locked(&self, origins: &mut HashMap<String, OriginState>) {
        let now = self.clock.now();
        origins.retain(|origin_id, state| {
            let stale = now
                .checked_duration_since(state.last_touched)
                .is_some_and(|elapsed| elapsed > ORIGIN_TTL);
            if stale && (state.turn_count > 0 || state.terminal_reason.is_some()) {
                tracing::debug!(
                    origin_id = %origin_id,
                    turn_count = state.turn_count,
                    terminal_reason = ?state.terminal_reason,
                    "pruning stale origin from turn tracker (ttl elapsed)"
                );
            }
            !stale
        });
    }

    #[cfg(test)]
    fn new_with_clock(clock: Arc<dyn Clock>) -> Self {
        Self {
            origins: Mutex::new(HashMap::new()),
            clock,
        }
    }

    #[cfg(test)]
    fn tracked_len(&self) -> usize {
        let origins = self.origins.lock().expect("turn_tracker lock");
        origins.len()
    }
}

impl Default for TurnTracker {
    fn default() -> Self {
        Self::new()
    }
}

/// Slot state for a single agent session within the scheduler.
struct TurnSlot {
    busy: bool,
    queue: VecDeque<ScheduledTurn>,
}

struct SchedulerInner {
    slots: HashMap<String, TurnSlot>,
    /// Total queued turns across all sessions (excludes the in-progress turn).
    global_queued: usize,
}

/// Per-session busy flag and input queue for ordered turn execution with
/// bounded capacity.
///
/// When a turn is submitted for a session that is already processing a turn,
/// the new turn is enqueued up to [`MAX_QUEUED_TURNS_PER_SESSION`] per session
/// and [`MAX_GLOBAL_QUEUED_TURNS`] across the runtime. Beyond those limits the
/// turn is rejected so overload surfaces as an explicit refusal instead of
/// unbounded memory growth. After the current turn completes, the caller
/// invokes [`TurnScheduler::on_turn_completed`] to drain the next queued turn.
pub(crate) struct TurnScheduler {
    inner: Mutex<SchedulerInner>,
}

impl TurnScheduler {
    pub(crate) fn new() -> Self {
        Self {
            inner: Mutex::new(SchedulerInner {
                slots: HashMap::new(),
                global_queued: 0,
            }),
        }
    }

    /// Submits a turn for execution.
    ///
    /// - [`ScheduleResult::Started`] when the slot was idle — the caller should
    ///   begin executing the returned turn immediately.
    /// - [`ScheduleResult::Queued`] when the slot is busy and capacity allowed
    ///   buffering the turn for later execution.
    /// - [`ScheduleResult::Rejected`] when per-session or global capacity is
    ///   full — the turn is refused and must not be executed.
    ///
    /// Per-session and global limits are checked under a single lock and no
    /// async work runs while the lock is held.
    pub(crate) fn submit(&self, turn: ScheduledTurn) -> ScheduleResult {
        let mut inner = self.inner.lock().expect("turn_scheduler lock");
        let session_key = turn.session_key();

        // Idle (no slot or slot not busy): start immediately.
        let is_idle = inner.slots.get(&session_key).is_none_or(|s| !s.busy);
        if is_idle {
            let slot = inner.slots.entry(session_key).or_insert_with(|| TurnSlot {
                busy: false,
                queue: VecDeque::new(),
            });
            slot.busy = true;
            // Started turns are not counted in global_queued (only queued are).
            return ScheduleResult::Started(Box::new(turn));
        }

        // Busy: check capacity under the same lock. Each check is a short-lived
        // borrow so they never overlap with the push below; the lock is held
        // continuously, so the decision is race-free.
        let session_full = inner
            .slots
            .get(&session_key)
            .is_some_and(|s| s.queue.len() >= MAX_QUEUED_TURNS_PER_SESSION);
        if session_full {
            metrics::inc_turn_queue_rejections("session_full");
            return ScheduleResult::Rejected(RejectReason::SessionQueueFull);
        }
        if inner.global_queued >= MAX_GLOBAL_QUEUED_TURNS {
            metrics::inc_turn_queue_rejections("global_full");
            return ScheduleResult::Rejected(RejectReason::GlobalQueueFull);
        }

        inner
            .slots
            .get_mut(&session_key)
            .expect("busy slot exists")
            .queue
            .push_back(turn);
        inner.global_queued += 1;
        metrics::set_turn_queue_depth(inner.global_queued);
        ScheduleResult::Queued
    }

    /// Called after a turn completes. Returns the next queued turn for this
    /// session, or `None` if the queue is empty (slot becomes idle).
    ///
    /// Always decrements the global queue count when a queued turn is drained.
    pub(crate) fn on_turn_completed(&self, session_key: &str) -> Option<ScheduledTurn> {
        let mut inner = self.inner.lock().expect("turn_scheduler lock");
        if let Some(slot) = inner.slots.get_mut(session_key) {
            if let Some(next) = slot.queue.pop_front() {
                inner.global_queued = inner.global_queued.saturating_sub(1);
                metrics::set_turn_queue_depth(inner.global_queued);
                return Some(next);
            }
            inner.slots.remove(session_key);
        }
        None
    }

    /// Returns `true` if the given session currently has a turn in progress.
    #[cfg(test)]
    fn is_busy(&self, session_key: &str) -> bool {
        let inner = self.inner.lock().expect("turn_scheduler lock");
        inner.slots.get(session_key).is_some_and(|s| s.busy)
    }

    /// Returns the number of queued turns for the given session.
    #[cfg(test)]
    fn queue_len(&self, session_key: &str) -> usize {
        let inner = self.inner.lock().expect("turn_scheduler lock");
        inner.slots.get(session_key).map_or(0, |s| s.queue.len())
    }

    /// Returns the total number of queued turns across all sessions.
    #[cfg(test)]
    fn global_queued(&self) -> usize {
        let inner = self.inner.lock().expect("turn_scheduler lock");
        inner.global_queued
    }
}

impl Default for TurnScheduler {
    fn default() -> Self {
        Self::new()
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
    if turn_count > MAX_AGENT_TURNS_PER_INPUT {
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
            "ch1".to_string(),
            "discord".to_string(),
            agent_id.to_string(),
        )
    }

    fn test_turn(agent_id: &str, origin_id: &str) -> ScheduledTurn {
        ScheduledTurn {
            context: test_context(agent_id),
            input: "hello".to_string(),
            origin_id: origin_id.to_string(),
        }
    }

    /// Submits one Started turn plus `queued` additional turns for a session,
    /// returning the Started turn count behaviour. Used to fill queues.
    fn fill_session(scheduler: &TurnScheduler, agent: &str, queued: usize) {
        scheduler.submit(test_turn(agent, "started"));
        for i in 0..queued {
            scheduler.submit(test_turn(agent, &format!("queued-{i}")));
        }
    }

    /// A controllable clock for TTL tests.
    struct MockClock {
        now: Mutex<Instant>,
    }

    impl MockClock {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                now: Mutex::new(Instant::now()),
            })
        }

        fn advance(&self, duration: Duration) {
            *self.now.lock().expect("mock clock") += duration;
        }
    }

    impl Clock for MockClock {
        fn now(&self) -> Instant {
            *self.now.lock().expect("mock clock")
        }
    }

    // ---- ScheduledTurn tests ----

    #[test]
    fn scheduled_turn_from_surface_context() {
        let turn = test_turn("agent_a", "orig-1");
        assert_eq!(turn.context.agent_id, "agent_a");
        assert_eq!(turn.input, "hello");
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
    fn turn_scheduler_idle_session_first_turn_is_started() {
        let scheduler = TurnScheduler::new();
        let result = scheduler.submit(test_turn("agent_a", "orig-1"));

        assert!(matches!(result, ScheduleResult::Started(_)));
        let key = "discord:ch1:agent:agent_a";
        assert!(scheduler.is_busy(key));
        assert_eq!(scheduler.queue_len(key), 0);
        assert_eq!(scheduler.global_queued(), 0);
    }

    #[test]
    fn turn_scheduler_busy_session_queues_until_per_session_limit() {
        let scheduler = TurnScheduler::new();
        let key = "discord:ch1:agent:agent_a";

        // First turn starts; the next 32 queue up to the per-session limit.
        fill_session(&scheduler, "agent_a", MAX_QUEUED_TURNS_PER_SESSION);

        assert!(scheduler.is_busy(key));
        assert_eq!(scheduler.queue_len(key), MAX_QUEUED_TURNS_PER_SESSION);
        assert_eq!(scheduler.global_queued(), MAX_QUEUED_TURNS_PER_SESSION);
    }

    #[test]
    fn turn_scheduler_rejects_when_per_session_queue_full() {
        let scheduler = TurnScheduler::new();
        fill_session(&scheduler, "agent_a", MAX_QUEUED_TURNS_PER_SESSION);

        let result = scheduler.submit(test_turn("agent_a", "overflow"));

        assert!(matches!(
            result,
            ScheduleResult::Rejected(RejectReason::SessionQueueFull)
        ));
        // Rejected turn is not enqueued.
        assert_eq!(
            scheduler.queue_len("discord:ch1:agent:agent_a"),
            MAX_QUEUED_TURNS_PER_SESSION
        );
        assert_eq!(scheduler.global_queued(), MAX_QUEUED_TURNS_PER_SESSION);
    }

    #[test]
    fn turn_scheduler_rejects_when_global_queue_full() {
        let scheduler = TurnScheduler::new();

        // Fill MAX_GLOBAL_QUEUED_TURNS across sessions, each below its
        // per-session limit (MAX_QUEUED_TURNS_PER_SESSION).
        let sessions_needed = MAX_GLOBAL_QUEUED_TURNS / MAX_QUEUED_TURNS_PER_SESSION;
        for i in 0..sessions_needed {
            fill_session(
                &scheduler,
                &format!("agent_{i}"),
                MAX_QUEUED_TURNS_PER_SESSION,
            );
        }
        assert_eq!(scheduler.global_queued(), MAX_GLOBAL_QUEUED_TURNS);

        // A new session's second turn must be rejected on the global limit.
        scheduler.submit(test_turn("overflow_agent", "started"));
        let result = scheduler.submit(test_turn("overflow_agent", "queued"));
        assert!(matches!(
            result,
            ScheduleResult::Rejected(RejectReason::GlobalQueueFull)
        ));
        assert_eq!(scheduler.global_queued(), MAX_GLOBAL_QUEUED_TURNS);
    }

    #[test]
    fn turn_scheduler_dequeue_decrements_global_count() {
        let scheduler = TurnScheduler::new();
        let key = "discord:ch1:agent:agent_a";

        fill_session(&scheduler, "agent_a", 3);
        assert_eq!(scheduler.global_queued(), 3);

        let next = scheduler.on_turn_completed(key);
        assert!(next.is_some());
        assert_eq!(scheduler.global_queued(), 2);

        let next = scheduler.on_turn_completed(key);
        assert!(next.is_some());
        assert_eq!(scheduler.global_queued(), 1);
    }

    #[test]
    fn turn_scheduler_drain_empty_removes_slot() {
        let scheduler = TurnScheduler::new();
        let key = "discord:ch1:agent:agent_a";

        scheduler.submit(test_turn("agent_a", "orig-1"));
        let next = scheduler.on_turn_completed(key);

        assert!(next.is_none());
        assert!(!scheduler.is_busy(key));
        assert_eq!(scheduler.global_queued(), 0);
    }

    #[test]
    fn turn_scheduler_drain_after_completion_keeps_slot_busy() {
        let scheduler = TurnScheduler::new();
        let key = "discord:ch1:agent:agent_a";

        scheduler.submit(test_turn("agent_a", "orig-1"));
        scheduler.submit(test_turn("agent_a", "orig-2"));

        let next = scheduler.on_turn_completed(key);
        assert!(next.is_some());
        assert_eq!(next.unwrap().origin_id, "orig-2");
        assert!(scheduler.is_busy(key));
    }

    #[test]
    fn turn_scheduler_rejected_turn_is_not_enqueued() {
        let scheduler = TurnScheduler::new();
        let key = "discord:ch1:agent:agent_a";

        fill_session(&scheduler, "agent_a", MAX_QUEUED_TURNS_PER_SESSION);
        let before = scheduler.queue_len(key);

        let _ = scheduler.submit(test_turn("agent_a", "rejected-overflow"));

        assert_eq!(scheduler.queue_len(key), before);
    }

    #[test]
    fn turn_scheduler_different_sessions_independent() {
        let scheduler = TurnScheduler::new();
        let key_a = "discord:ch1:agent:agent_a";
        let key_b = "discord:ch1:agent:agent_b";

        let result_a = scheduler.submit(test_turn("agent_a", "orig-1"));
        let result_b = scheduler.submit(test_turn("agent_b", "orig-1"));

        assert!(matches!(result_a, ScheduleResult::Started(_)));
        assert!(matches!(result_b, ScheduleResult::Started(_)));
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
        let result = evaluate_stop_conditions(0, 13, "agent_a", &["agent_a"]);
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
        assert_eq!(tracker.increment("orig-1"), Some(1));
        assert_eq!(tracker.increment("orig-1"), Some(2));
        assert_eq!(tracker.increment("orig-1"), Some(3));
    }

    #[test]
    fn turn_tracker_different_origins_independent() {
        let tracker = TurnTracker::new();
        assert_eq!(tracker.increment("orig-1"), Some(1));
        assert_eq!(tracker.increment("orig-1"), Some(2));
        assert_eq!(tracker.increment("orig-2"), Some(1));
    }

    #[test]
    fn turn_tracker_terminal_reason_blocks_future_turns() {
        let tracker = TurnTracker::new();
        assert!(tracker.terminal_reason("orig-1").is_none());
        tracker.set_terminal_reason("orig-1", StopReason::ChainDepthExceeded);
        assert_eq!(
            tracker.terminal_reason("orig-1"),
            Some(StopReason::ChainDepthExceeded)
        );
    }

    #[test]
    fn turn_tracker_prunes_stale_origins_after_ttl() {
        let clock = MockClock::new();
        let tracker = TurnTracker::new_with_clock(Arc::clone(&clock) as Arc<dyn Clock>);

        assert_eq!(tracker.increment("stale"), Some(1));
        assert_eq!(tracker.tracked_len(), 1);

        // Just under TTL: still retained.
        clock.advance(ORIGIN_TTL - Duration::from_secs(1));
        assert_eq!(tracker.increment("fresh"), Some(1));
        assert_eq!(tracker.tracked_len(), 2);

        // Past TTL for "stale": pruned on the next operation. "fresh" is
        // retained and its count keeps climbing.
        clock.advance(Duration::from_secs(2));
        assert_eq!(tracker.increment("fresh"), Some(2));
        assert_eq!(tracker.tracked_len(), 1);
        // "stale" was evicted, so re-incrementing starts fresh at 1.
        assert_eq!(tracker.increment("stale"), Some(1));
    }

    #[test]
    fn turn_tracker_rejects_new_origin_at_capacity_but_keeps_existing() {
        let clock = MockClock::new();
        let tracker = TurnTracker::new_with_clock(Arc::clone(&clock) as Arc<dyn Clock>);

        // Fill to capacity with distinct origins.
        for i in 0..MAX_TRACKED_ORIGINS {
            assert_eq!(
                tracker.increment(&format!("orig-{i}")),
                Some(1),
                "origin {i} should be tracked"
            );
        }
        assert_eq!(tracker.tracked_len(), MAX_TRACKED_ORIGINS);

        // A brand-new origin is rejected.
        assert_eq!(tracker.increment("orig-new"), None);

        // An existing origin is still updatable at capacity.
        assert_eq!(tracker.increment("orig-0"), Some(2));
        assert_eq!(tracker.tracked_len(), MAX_TRACKED_ORIGINS);
    }

    // -----------------------------------------------------------------------
    // ActiveTurnTracker tests (migrated from runtime/mod.rs)
    // -----------------------------------------------------------------------

    #[test]
    fn active_turn_tracker_marks_agent_running_during_turn() {
        let tracker = ActiveTurnTracker::new();
        tracker.begin_turn("agent-a");
        assert!(tracker.is_active("agent-a"));
    }

    #[test]
    fn active_turn_tracker_clears_agent_after_success() {
        let tracker = ActiveTurnTracker::new();
        tracker.begin_turn("agent-a");
        tracker.end_turn("agent-a");
        assert!(!tracker.is_active("agent-a"));
    }

    #[test]
    fn active_turn_tracker_clears_agent_after_error() {
        let tracker = ActiveTurnTracker::new();
        tracker.begin_turn("agent-a");
        // Simulate error path: end_turn is called regardless
        tracker.end_turn("agent-a");
        assert!(!tracker.is_active("agent-a"));
    }

    #[test]
    fn active_turn_tracker_counts_parallel_turns_per_agent() {
        let tracker = ActiveTurnTracker::new();
        tracker.begin_turn("agent-a");
        tracker.begin_turn("agent-a");
        assert!(tracker.is_active("agent-a"));

        tracker.end_turn("agent-a");
        assert!(
            tracker.is_active("agent-a"),
            "still active after one turn ends"
        );

        tracker.end_turn("agent-a");
        assert!(
            !tracker.is_active("agent-a"),
            "inactive after all turns end"
        );
    }

    #[test]
    fn active_turn_tracker_is_agent_scoped() {
        let tracker = ActiveTurnTracker::new();
        tracker.begin_turn("agent-a");
        assert!(!tracker.is_active("agent-b"), "other agent unaffected");
        tracker.end_turn("agent-a");
        assert!(!tracker.is_active("agent-a"));
    }
}
