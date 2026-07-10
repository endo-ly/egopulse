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
    /// The origin tracker reached [`MAX_TRACKED_ORIGINS`] capacity for a new
    /// origin. The turn is refused at acceptance so it is never silently
    /// dropped after a `202 Accepted` was already returned.
    OriginTrackerFull,
    /// The origin already recorded a terminal stop reason (its chain
    /// terminated earlier). Refused at acceptance rather than accepted and
    /// dropped later.
    ChainTerminated,
}

impl RejectReason {
    /// Machine-readable code for the rejection, suitable for error responses
    /// and metric labels.
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::SessionQueueFull => "session_queue_full",
            Self::GlobalQueueFull => "global_queue_full",
            Self::OriginTrackerFull => "tracker_full",
            Self::ChainTerminated => "chain_terminated",
        }
    }

    /// User-facing message describing why the turn was rejected.
    ///
    /// Used in API/webhook responses so the reason is not masked behind a
    /// generic "queue at capacity" string when the rejection is actually a
    /// tracker-capacity or chain-terminated condition.
    pub(crate) fn message(&self) -> &'static str {
        match self {
            Self::SessionQueueFull => "session turn queue is at capacity",
            Self::GlobalQueueFull => "global turn queue is at capacity",
            Self::OriginTrackerFull => "origin turn tracker is at capacity",
            Self::ChainTerminated => "turn chain already terminated",
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
    /// Number of turns for this origin that have actually started executing.
    /// This is what the per-chain turn limit is measured against, and it only
    /// advances when a turn begins running — never at acceptance.
    executed_turn_count: usize,
    /// Number of turns reserved at acceptance but not yet executed. Capacity is
    /// reserved up front so a rejected turn is refused before `202 Accepted`,
    /// but reservations must not inflate [`executed_turn_count`].
    pending_reservations: usize,
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

    /// Reserves tracker capacity for `origin_id` at turn acceptance.
    ///
    /// Performs the acceptance-time checks atomically in a single lock so a
    /// rejected turn is refused before any `202 Accepted` is returned to the
    /// caller:
    ///
    /// 1. If the origin already has a terminal stop reason, returns
    ///    `Err(ChainTerminated)` — the chain already ended.
    /// 2. Otherwise, for a brand-new origin, returns `Err(OriginTrackerFull)`
    ///    when [`MAX_TRACKED_ORIGINS`] capacity is reached. Existing origins are
    ///    always reservable.
    ///
    /// On success the origin's pending reservation count is incremented. The
    /// reservation is independent of the executed turn count (see
    /// [`TurnTracker::try_begin_execution`]): a turn is only counted as executed
    /// once it actually starts running, so fan-out or burst submissions never
    /// inflate the per-chain turn limit that execution is measured against.
    ///
    /// Call [`TurnTracker::release`] if the turn is later refused downstream
    /// (e.g. by the scheduler queue) so the reservation does not leak capacity.
    ///
    /// # Errors
    ///
    /// Returns the [`RejectReason`] when the origin must be refused.
    pub(crate) fn reserve(&self, origin_id: &str) -> Result<(), RejectReason> {
        let mut origins = self.origins.lock().expect("turn_tracker lock");
        self.prune_stale_locked(&mut origins);
        let now = self.clock.now();
        if let Some(state) = origins.get_mut(origin_id) {
            if state.terminal_reason.is_some() {
                return Err(RejectReason::ChainTerminated);
            }
            state.pending_reservations += 1;
            state.last_touched = now;
            return Ok(());
        }
        if origins.len() >= MAX_TRACKED_ORIGINS {
            return Err(RejectReason::OriginTrackerFull);
        }
        origins.insert(
            origin_id.to_string(),
            OriginState {
                executed_turn_count: 0,
                pending_reservations: 1,
                terminal_reason: None,
                last_touched: now,
            },
        );
        Ok(())
    }

    /// Releases a reservation made by [`TurnTracker::reserve`].
    ///
    /// Called when a turn that reserved capacity is refused downstream (e.g. by
    /// the scheduler queue). Decrements the origin's pending reservation count
    /// and, for an origin that never executed and has no terminal reason,
    /// removes the entry so it does not occupy tracker capacity.
    pub(crate) fn release(&self, origin_id: &str) {
        let mut origins = self.origins.lock().expect("turn_tracker lock");
        if let Some(state) = origins.get_mut(origin_id) {
            state.pending_reservations = state.pending_reservations.saturating_sub(1);
            if state.pending_reservations == 0
                && state.executed_turn_count == 0
                && state.terminal_reason.is_none()
            {
                origins.remove(origin_id);
            }
        }
    }

    /// Atomically gates a turn on every stop condition and commits it to
    /// execution, all under a single tracker lock.
    ///
    /// This is the only place a pending reservation is converted into an
    /// executed turn. Performing the per-chain turn-count check and the
    /// executed-count increment inside the same lock guarantees that two
    /// concurrent turns for the same origin (which may run on separate
    /// scheduler sessions, since the session key includes the agent id) cannot
    /// both pass the limit — the second sees the first's increment.
    ///
    /// Steps, all under one lock:
    /// 1. If the origin already has a terminal stop reason, refuse and release
    ///    the reservation.
    /// 2. Evaluate every stop condition (chain depth, per-chain turn count,
    ///    agent validity) against the prospective executed count (`current + 1`).
    /// 3. On failure, record the terminal reason and release the reservation.
    /// 4. On success, decrement `pending_reservations` and increment
    ///    `executed_turn_count`, returning the new count.
    ///
    /// Returns `Ok(executed_turn_count)` when the turn may proceed, or
    /// `Err(reason)` when it must be rejected. On either path the reservation is
    /// consumed here, so the caller must not call [`TurnTracker::release`].
    ///
    /// # Panics
    ///
    /// Panics if the internal tracker lock is poisoned.
    pub(crate) fn try_begin_execution(
        &self,
        origin_id: &str,
        chain_depth: usize,
        agent_id: &str,
        valid_agent_ids: &[&str],
    ) -> Result<usize, StopReason> {
        let mut origins = self.origins.lock().expect("turn_tracker lock");
        self.prune_stale_locked(&mut origins);
        let now = self.clock.now();
        let state = origins.entry(origin_id.to_string()).or_insert(OriginState {
            executed_turn_count: 0,
            pending_reservations: 0,
            terminal_reason: None,
            last_touched: now,
        });
        state.last_touched = now;

        // A chain that already terminated refuses further turns. Release this
        // turn's reservation without incrementing the executed count.
        if let Some(existing) = state.terminal_reason.clone() {
            state.pending_reservations = state.pending_reservations.saturating_sub(1);
            return Err(existing);
        }

        // Evaluate every stop condition against the prospective executed count,
        // atomically with the commit below so the check and the increment
        // cannot be split by a concurrent turn.
        let prospective = state.executed_turn_count + 1;
        if let Some(reason) =
            evaluate_stop_conditions(chain_depth, prospective, agent_id, valid_agent_ids)
        {
            state.terminal_reason = Some(reason.clone());
            state.pending_reservations = state.pending_reservations.saturating_sub(1);
            return Err(reason);
        }

        state.pending_reservations = state.pending_reservations.saturating_sub(1);
        state.executed_turn_count = prospective;
        Ok(state.executed_turn_count)
    }

    /// Returns the number of turns that have actually begun execution for
    /// `origin_id`, or `0` if the origin is not tracked. Test-only assertion
    /// helper; production reads happen inside [`try_begin_execution`].
    #[cfg(test)]
    fn executed_count(&self, origin_id: &str) -> usize {
        let origins = self.origins.lock().expect("turn_tracker lock");
        origins
            .get(origin_id)
            .map(|state| state.executed_turn_count)
            .unwrap_or(0)
    }

    /// Records the reason a turn chain stopped for the given origin.
    ///
    /// The reason is stored on the origin's state so later turns can read it via
    /// [`TurnTracker::terminal_reason`]. If the origin is not yet tracked, an
    /// entry is created with this reason and a zero turn count. Stale origins
    /// are pruned before the write so expired chains do not resurface.
    ///
    /// # Panics
    ///
    /// Panics if the internal tracker lock is poisoned.
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
                executed_turn_count: 0,
                pending_reservations: 0,
                terminal_reason: Some(reason),
                last_touched: now,
            });
    }

    /// Returns the terminal reason previously recorded for an origin, if any.
    ///
    /// `None` means the origin has no recorded terminal reason: the chain is
    /// still active or never stopped. Callers combine this with the origin's
    /// turn count to decide whether a new turn may start. Stale origins are
    /// pruned before the read.
    ///
    /// # Panics
    ///
    /// Panics if the internal tracker lock is poisoned.
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
            if stale && (state.executed_turn_count > 0 || state.terminal_reason.is_some()) {
                tracing::debug!(
                    origin_id = %origin_id,
                    executed_turn_count = state.executed_turn_count,
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
    /// async work runs while the lock is held. Rejection metrics are recorded
    /// by the caller ([`submit_scheduled_turn`](crate::runtime::channel_input)),
    /// not here, so each rejected turn is counted exactly once.
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
            return ScheduleResult::Rejected(RejectReason::SessionQueueFull);
        }
        if inner.global_queued >= MAX_GLOBAL_QUEUED_TURNS {
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
    pub(crate) fn global_queued(&self) -> usize {
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
        tracker.reserve("orig-1").unwrap();
        assert_eq!(
            tracker.try_begin_execution("orig-1", 0, "agent_a", &["agent_a"]),
            Ok(1)
        );
        tracker.reserve("orig-1").unwrap();
        assert_eq!(
            tracker.try_begin_execution("orig-1", 0, "agent_a", &["agent_a"]),
            Ok(2)
        );
        tracker.reserve("orig-1").unwrap();
        assert_eq!(
            tracker.try_begin_execution("orig-1", 0, "agent_a", &["agent_a"]),
            Ok(3)
        );
    }

    #[test]
    fn turn_tracker_different_origins_independent() {
        let tracker = TurnTracker::new();
        tracker.reserve("orig-1").unwrap();
        assert_eq!(
            tracker.try_begin_execution("orig-1", 0, "agent_a", &["agent_a"]),
            Ok(1)
        );
        tracker.reserve("orig-1").unwrap();
        assert_eq!(
            tracker.try_begin_execution("orig-1", 0, "agent_a", &["agent_a"]),
            Ok(2)
        );
        tracker.reserve("orig-2").unwrap();
        assert_eq!(
            tracker.try_begin_execution("orig-2", 0, "agent_a", &["agent_a"]),
            Ok(1)
        );
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
        // A reserve after a terminal reason is refused.
        assert_eq!(
            tracker.reserve("orig-1"),
            Err(RejectReason::ChainTerminated)
        );
        // try_begin_execution also refuses a terminated chain without executing.
        tracker.reserve("orig-2").unwrap();
        tracker.set_terminal_reason("orig-2", StopReason::TurnCountExceeded);
        assert_eq!(
            tracker.try_begin_execution("orig-2", 0, "agent_a", &["agent_a"]),
            Err(StopReason::TurnCountExceeded)
        );
    }

    #[test]
    fn turn_tracker_prunes_stale_origins_after_ttl() {
        let clock = MockClock::new();
        let tracker = TurnTracker::new_with_clock(Arc::clone(&clock) as Arc<dyn Clock>);

        assert_eq!(tracker.reserve("stale"), Ok(()));
        assert_eq!(
            tracker.try_begin_execution("stale", 0, "agent_a", &["agent_a"]),
            Ok(1)
        );
        assert_eq!(tracker.tracked_len(), 1);

        // Just under TTL: still retained.
        clock.advance(ORIGIN_TTL - Duration::from_secs(1));
        assert_eq!(tracker.reserve("fresh"), Ok(()));
        assert_eq!(
            tracker.try_begin_execution("fresh", 0, "agent_a", &["agent_a"]),
            Ok(1)
        );
        assert_eq!(tracker.tracked_len(), 2);

        // Past TTL for "stale": pruned on the next operation. "fresh" is
        // retained and its count keeps climbing.
        clock.advance(Duration::from_secs(2));
        assert_eq!(tracker.reserve("fresh"), Ok(()));
        assert_eq!(
            tracker.try_begin_execution("fresh", 0, "agent_a", &["agent_a"]),
            Ok(2)
        );
        assert_eq!(tracker.tracked_len(), 1);
        // "stale" was evicted, so re-reserving starts fresh.
        assert_eq!(tracker.reserve("stale"), Ok(()));
    }

    #[test]
    fn turn_tracker_rejects_new_origin_at_capacity_but_keeps_existing() {
        let clock = MockClock::new();
        let tracker = TurnTracker::new_with_clock(Arc::clone(&clock) as Arc<dyn Clock>);

        // Fill to capacity with distinct origins.
        for i in 0..MAX_TRACKED_ORIGINS {
            assert_eq!(
                tracker.reserve(&format!("orig-{i}")),
                Ok(()),
                "origin {i} should be tracked"
            );
        }
        assert_eq!(tracker.tracked_len(), MAX_TRACKED_ORIGINS);

        // A brand-new origin is rejected at capacity.
        assert_eq!(
            tracker.reserve("orig-new"),
            Err(RejectReason::OriginTrackerFull)
        );

        // An existing origin is still reservable at capacity.
        assert_eq!(tracker.reserve("orig-0"), Ok(()));
        assert_eq!(tracker.tracked_len(), MAX_TRACKED_ORIGINS);
    }

    #[test]
    fn reserve_does_not_inflate_executed_turn_count() {
        let tracker = TurnTracker::new();
        // 13 turns accepted (reserved) for the same origin/session before any
        // executes. The reservation must NOT count toward the executed-turn
        // limit that execution is measured against.
        for _ in 0..13 {
            tracker.reserve("orig-1").unwrap();
        }
        // Reserving 13 times never advanced the executed count.
        assert_eq!(tracker.executed_count("orig-1"), 0);
        // The first 12 turns begin executing without tripping the limit.
        for executed in 1..=12 {
            assert_eq!(
                tracker.try_begin_execution("orig-1", 0, "agent_a", &["agent_a"]),
                Ok(executed)
            );
        }
        // The 13th is rejected before it executes, and the executed count stays
        // at 12 — the hard limit is never exceeded.
        assert_eq!(
            tracker.try_begin_execution("orig-1", 0, "agent_a", &["agent_a"]),
            Err(StopReason::TurnCountExceeded)
        );
        assert_eq!(tracker.executed_count("orig-1"), 12);
    }

    #[test]
    fn concurrent_try_begin_execution_never_exceeds_turn_limit() {
        let tracker = Arc::new(TurnTracker::new());
        // Reserve and execute 11 turns so the next is the 12th (last allowed).
        for _ in 0..11 {
            tracker.reserve("orig-1").unwrap();
            tracker
                .try_begin_execution("orig-1", 0, "agent_a", &["agent_a"])
                .unwrap();
        }
        // Two more turns reserved for the same origin; both contend for the
        // single remaining slot (the 12th).
        tracker.reserve("orig-1").unwrap();
        tracker.reserve("orig-1").unwrap();

        let barrier = Arc::new(std::sync::Barrier::new(2));
        let mut handles = Vec::new();
        for _ in 0..2 {
            let tracker = Arc::clone(&tracker);
            let barrier = Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                barrier.wait();
                tracker.try_begin_execution("orig-1", 0, "agent_a", &["agent_a"])
            }));
        }
        let h1 = handles.remove(0);
        let h2 = handles.remove(0);
        let r1 = h1.join().expect("turn thread 1");
        let r2 = h2.join().expect("turn thread 2");

        // Exactly one turn wins the 12th slot; the other is rejected.
        let (winner, loser) = if r1.is_ok() { (r1, r2) } else { (r2, r1) };
        assert_eq!(winner, Ok(12));
        assert_eq!(loser, Err(StopReason::TurnCountExceeded));

        // The hard limit is never exceeded: executed count is 12, not 13.
        assert_eq!(tracker.executed_count("orig-1"), 12);
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
