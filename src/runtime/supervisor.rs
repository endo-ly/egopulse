//! Runtime supervisor.
//!
//! Owns the long-lived tasks (channel listeners, workers, schedulers, MCP
//! reconnect loop) and in-flight turn tasks started by the runtime, and
//! orchestrates an ordered, deadline-bounded graceful shutdown.
//!
//! ## Ownership model
//!
//! Every task whose lifetime spans the runtime is spawned through the
//! supervisor instead of being fired-and-forgotten via `tokio::spawn`. The
//! supervisor records each task's kind, name, criticality, and terminal
//! outcome (ok / error / panic) into [`RuntimeStatus`] so an abnormal exit is
//! never silently lost.
//!
//! ## Shutdown
//!
//! [`RuntimeSupervisor::shutdown`] flips `accepting_inputs` off, cancels the
//! root [`CancellationToken`] (so cancellation-aware tasks stop gracefully),
//! then drains in-flight turns and long-lived tasks in that order, each under a
//! deadline. Anything still alive after the deadline is aborted so shutdown can
//! never hang on a single stuck task.
//!
//! New turns are not started once shutdown has begun: the intake path refuses
//! submissions and a completing turn does not start the next queued turn.

use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::time::Duration;

use futures_util::future::FutureExt;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::error::EgoPulseError;
use crate::runtime::RuntimeStatus;
use crate::runtime::metrics;

/// Default deadline for draining in-flight turns during shutdown.
const DEFAULT_TURN_DRAIN_SECS: u64 = 30;
/// Default deadline for draining long-lived tasks during shutdown.
const DEFAULT_TASK_DRAIN_SECS: u64 = 15;

/// Classification of a supervised long-lived task, used for metrics labels and
/// status reporting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TaskKind {
    Channel,
    AgentTurnWorker,
    McpReconnect,
    SleepScheduler,
    PulseScheduler,
    BackupScheduler,
}

impl TaskKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Channel => "channel",
            Self::AgentTurnWorker => "agent_turn_worker",
            Self::McpReconnect => "mcp_reconnect",
            Self::SleepScheduler => "sleep_scheduler",
            Self::PulseScheduler => "pulse_scheduler",
            Self::BackupScheduler => "backup_scheduler",
        }
    }
}

/// Whether an abnormal exit of this task should stop the runtime from
/// accepting new input.
///
/// Critical tasks are ones whose absence makes serving new turns impossible or
/// unsafe (e.g. a required channel listener, the agent turn worker). Their
/// failure is recorded as a critical task failure and surfaced to the run loop
/// so it can begin shutdown. Non-critical tasks (periodic schedulers, optional
/// reconnect loops) only log on failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Criticality {
    Critical,
    NonCritical,
}

/// Description of a long-lived task to spawn through the supervisor.
#[derive(Clone, Debug)]
pub(crate) struct TaskSpec {
    kind: TaskKind,
    name: String,
    criticality: Criticality,
}

impl TaskSpec {
    pub(crate) fn new(kind: TaskKind, name: impl Into<String>, criticality: Criticality) -> Self {
        Self {
            kind,
            name: name.into(),
            criticality,
        }
    }
}

/// Terminal outcome of a long-lived task, returned from its supervised future
/// so the run-loop monitor can detect critical failures without re-deriving
/// them from [`RuntimeStatus`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TaskResult {
    Ok,
    Err(String),
    Panic,
}

/// A completed long-lived task surfaced to the monitor loop.
#[derive(Clone, Debug)]
pub(crate) struct TaskOutcome {
    spec: TaskSpec,
    result: TaskResult,
}

impl TaskOutcome {
    pub(crate) fn name(&self) -> &str {
        &self.spec.name
    }

    pub(crate) fn result(&self) -> &TaskResult {
        &self.result
    }
}

/// Owns runtime tasks and orchestrates graceful shutdown.
pub(crate) struct RuntimeSupervisor {
    root_token: CancellationToken,
    long_lived: Mutex<JoinSet<TaskOutcome>>,
    turns: Mutex<JoinSet<()>>,
    accepting_inputs: AtomicBool,
    shutdown_started: AtomicBool,
    runtime_status: Arc<RuntimeStatus>,
    turn_drain: Duration,
    task_drain: Duration,
}

impl RuntimeSupervisor {
    /// Creates a new supervisor bound to the given [`RuntimeStatus`].
    pub(crate) fn new(runtime_status: Arc<RuntimeStatus>) -> Self {
        Self::with_drain(
            runtime_status,
            Duration::from_secs(DEFAULT_TURN_DRAIN_SECS),
            Duration::from_secs(DEFAULT_TASK_DRAIN_SECS),
        )
    }

    fn with_drain(
        runtime_status: Arc<RuntimeStatus>,
        turn_drain: Duration,
        task_drain: Duration,
    ) -> Self {
        Self {
            root_token: CancellationToken::new(),
            long_lived: Mutex::new(JoinSet::new()),
            turns: Mutex::new(JoinSet::new()),
            accepting_inputs: AtomicBool::new(true),
            shutdown_started: AtomicBool::new(false),
            runtime_status,
            turn_drain,
            task_drain,
        }
    }

    /// Returns a child cancellation token linked to the runtime root.
    ///
    /// Long-lived tasks select on this token to stop gracefully when shutdown
    /// begins.
    pub(crate) fn shutdown_token(&self) -> CancellationToken {
        self.root_token.clone()
    }

    /// Whether the runtime is currently accepting new external input.
    pub(crate) fn accepting_inputs(&self) -> bool {
        self.accepting_inputs.load(Ordering::Acquire)
    }

    /// Whether shutdown has begun.
    pub(crate) fn is_shutting_down(&self) -> bool {
        self.shutdown_started.load(Ordering::Acquire)
    }

    /// Marks the runtime as accepting external input.
    ///
    /// Called once startup recovery is complete and channels are about to start
    /// serving requests.
    pub(crate) fn start_accepting(&self) {
        self.accepting_inputs.store(true, Ordering::Release);
        self.runtime_status.set_accepting_inputs(true);
    }

    /// Spawns a long-lived task owned by the supervisor.
    ///
    /// The future is wrapped so its terminal outcome (ok / error / panic) is
    /// always recorded into [`RuntimeStatus`] and metrics, and returned to the
    /// monitor loop via [`RuntimeSupervisor::poll_long_lived`].
    pub(crate) fn spawn_long_lived<F>(&self, spec: TaskSpec, fut: F)
    where
        F: Future<Output = Result<(), EgoPulseError>> + Send + 'static,
    {
        let status = Arc::clone(&self.runtime_status);
        let mut set = self.long_lived.lock().expect("supervisor long_lived lock");
        set.spawn(async move {
            let task_result = match AssertUnwindSafe(fut).catch_unwind().await {
                Ok(Ok(())) => TaskResult::Ok,
                Ok(Err(error)) => TaskResult::Err(error.to_string()),
                Err(_) => TaskResult::Panic,
            };
            record_completion(&status, &spec, &task_result);
            TaskOutcome {
                spec,
                result: task_result,
            }
        });
        drop(set);
        self.sync_owned_count();
    }

    /// Spawns an in-flight turn task owned by the supervisor.
    ///
    /// Turn tasks are not cancellation-aware: they complete naturally and are
    /// drained within the shutdown deadline. A panicking turn is recorded as an
    /// error but does not trigger runtime shutdown (it is a per-turn bug, not a
    /// runtime-fatal condition).
    pub(crate) fn spawn_turn<F>(&self, fut: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let status = Arc::clone(&self.runtime_status);
        let mut set = self.turns.lock().expect("supervisor turns lock");
        set.spawn(async move {
            if let Err(payload) = AssertUnwindSafe(fut).catch_unwind().await {
                status.push_error(
                    "",
                    "turn_panic",
                    "",
                    "",
                    "agent turn task panicked; suppressed",
                );
                metrics::inc_runtime_task_failures("turn");
                drop(payload);
            }
        });
    }

    /// Non-blocking drain of completed long-lived tasks.
    ///
    /// Returns the first critical failure observed so the run loop can begin
    /// shutdown. Non-critical completions and normal exits are consumed
    /// silently (their outcome was already recorded inside the task).
    pub(crate) fn poll_long_lived(&self) -> Option<TaskOutcome> {
        let mut critical_failure = None;
        loop {
            let mut set = self.long_lived.lock().expect("supervisor long_lived lock");
            match set.try_join_next() {
                None => break,
                Some(Ok(outcome)) => {
                    // Any exit of a critical task during operation is unexpected
                    // (channels and the agent turn worker run until shutdown),
                    // so surface it regardless of Ok/Err/Panic.
                    if outcome.spec.criticality == Criticality::Critical
                        && critical_failure.is_none()
                    {
                        critical_failure = Some(outcome);
                    }
                }
                Some(Err(join_error)) => {
                    warn!(error = %join_error, "long-lived task join failed");
                    metrics::inc_runtime_task_failures("join_error");
                }
            }
        }
        self.sync_owned_count();
        critical_failure
    }

    /// Begins graceful shutdown.
    ///
    /// Order (per the Phase 3 shutdown protocol):
    /// 1. Stop accepting input (`accepting_inputs = false`, `shutdown_started`).
    /// 2. Drain in-flight turns within the turn deadline, then abort stragglers.
    ///    Workers and channels are still running at this point so turns can
    ///    complete their external side effects.
    /// 3. Cancel the root token so cancellation-aware tasks stop gracefully.
    /// 4. Drain long-lived tasks within the task deadline, then abort stragglers.
    ///
    /// Idempotent: a second call is a no-op.
    pub(crate) async fn shutdown(&self) {
        if self.shutdown_started.swap(true, Ordering::AcqRel) {
            return;
        }
        self.accepting_inputs.store(false, Ordering::Release);
        self.runtime_status.set_accepting_inputs(false);
        self.runtime_status.set_shutdown_started(true);
        info!("runtime supervisor: shutdown begun");

        // 5. Drain in-flight turns first, while workers/channels are still up to
        // support their completion.
        let turn_aborts = self.drain_set(&self.turns, self.turn_drain).await;
        if turn_aborts > 0 {
            warn!(
                aborted = turn_aborts,
                "runtime supervisor: aborted turns after deadline"
            );
            metrics::inc_runtime_shutdown_aborts(turn_aborts);
        }

        // 6-7. Cancel the root token so cancellation-aware tasks stop, then drain
        // long-lived tasks.
        self.root_token.cancel();
        let task_aborts = self.drain_set(&self.long_lived, self.task_drain).await;
        if task_aborts > 0 {
            warn!(
                aborted = task_aborts,
                "runtime supervisor: aborted tasks after deadline"
            );
            metrics::inc_runtime_shutdown_aborts(task_aborts);
        }
        self.sync_owned_count();
        info!("runtime supervisor: shutdown complete");
    }

    /// Drains a `JoinSet` up to `deadline`, aborting whatever remains.
    ///
    /// Takes ownership of the set via [`std::mem::take`] so the mutex is not
    /// held across awaits — new spawns during shutdown land in the fresh empty
    /// set and are not drained (acceptable: shutdown has stopped intake).
    async fn drain_set<T: 'static>(&self, slot: &Mutex<JoinSet<T>>, deadline: Duration) -> usize {
        let mut owned = {
            let mut set = slot.lock().expect("supervisor set lock");
            std::mem::take(&mut *set)
        };
        if owned.is_empty() {
            return 0;
        }
        let _ = tokio::time::timeout(deadline, async {
            while owned.join_next().await.is_some() {}
        })
        .await;
        let remaining = owned.len();
        if remaining > 0 {
            owned.abort_all();
            // Reap aborted tasks so their resources are released.
            while owned.join_next().await.is_some() {}
        }
        remaining
    }

    fn sync_owned_count(&self) {
        let count = {
            let set = self.long_lived.lock().expect("supervisor long_lived lock");
            set.len()
        };
        self.runtime_status.set_owned_task_count(count);
        metrics::set_runtime_owned_tasks(count);
    }
}

fn record_completion(status: &RuntimeStatus, spec: &TaskSpec, result: &TaskResult) {
    match result {
        TaskResult::Ok => info!(
            task = %spec.name,
            kind = spec.kind.as_str(),
            "long-lived task exited normally"
        ),
        TaskResult::Err(msg) => {
            warn!(
                task = %spec.name,
                kind = spec.kind.as_str(),
                error = %msg,
                "long-lived task failed"
            );
            status.push_error("", "task_failure", "", spec.kind.as_str(), msg);
            metrics::inc_runtime_task_failures(spec.kind.as_str());
            if spec.criticality == Criticality::Critical {
                status.record_critical_task_failure(&format!(
                    "critical task '{}' ({}) failed: {}",
                    spec.name,
                    spec.kind.as_str(),
                    msg
                ));
            }
        }
        TaskResult::Panic => {
            warn!(
                task = %spec.name,
                kind = spec.kind.as_str(),
                "long-lived task panicked"
            );
            status.push_error(
                "",
                "task_panic",
                "",
                spec.kind.as_str(),
                &format!("task '{}' panicked", spec.name),
            );
            metrics::inc_runtime_task_failures(spec.kind.as_str());
            if spec.criticality == Criticality::Critical {
                status.record_critical_task_failure(&format!(
                    "critical task '{}' ({}) panicked",
                    spec.name,
                    spec.kind.as_str(),
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use std::sync::atomic::Ordering as AtomicOrdering;

    fn make_supervisor() -> Arc<RuntimeSupervisor> {
        Arc::new(RuntimeSupervisor::new(Arc::new(RuntimeStatus::new())))
    }

    #[tokio::test]
    async fn spawn_turn_completes_and_is_drained() {
        let supervisor = make_supervisor();
        let flag = Arc::new(AtomicUsize::new(0));
        let flag_clone = Arc::clone(&flag);
        supervisor.spawn_turn(async move {
            flag_clone.store(1, AtomicOrdering::SeqCst);
        });
        // Turn runs to completion.
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(flag.load(AtomicOrdering::SeqCst), 1);
        // Shutdown drains the (already-empty) turn set without aborting.
        supervisor.shutdown().await;
        assert!(supervisor.is_shutting_down());
        assert!(!supervisor.accepting_inputs());
    }

    #[tokio::test]
    async fn spawn_turn_panic_is_recorded_not_propagated() {
        let supervisor = make_supervisor();
        supervisor.spawn_turn(async move {
            panic!("boom");
        });
        // Let the panicked task be reaped.
        tokio::time::sleep(Duration::from_millis(50)).await;
        let errors = supervisor.runtime_status.recent_errors();
        assert!(
            errors.iter().any(|e| e.error_kind == "turn_panic"),
            "turn panic should be recorded: {errors:?}"
        );
        // Supervisor did not begin shutdown just because a turn panicked.
        assert!(!supervisor.is_shutting_down());
    }

    #[tokio::test]
    async fn critical_task_failure_surfaces_via_poll() {
        let supervisor = make_supervisor();
        supervisor.spawn_long_lived(
            TaskSpec::new(TaskKind::Channel, "test-channel", Criticality::Critical),
            async move { Err(EgoPulseError::Internal("channel died".to_string())) },
        );
        // Wait for the task to finish.
        tokio::time::sleep(Duration::from_millis(50)).await;
        let outcome = supervisor.poll_long_lived();
        let outcome = outcome.expect("critical failure should be surfaced");
        assert_eq!(outcome.spec.kind, TaskKind::Channel);
        assert!(matches!(outcome.result, TaskResult::Err(_)));
        let snap = supervisor.runtime_status.snapshot();
        assert!(snap.critical_task_failure.is_some());
    }

    #[tokio::test]
    async fn critical_task_normal_exit_also_surfaces_via_poll() {
        let supervisor = make_supervisor();
        supervisor.spawn_long_lived(
            TaskSpec::new(TaskKind::Channel, "web", Criticality::Critical),
            async move { Ok(()) },
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
        let outcome = supervisor.poll_long_lived();
        let outcome = outcome.expect("critical Ok-exit should surface during operation");
        assert_eq!(outcome.result(), &TaskResult::Ok);
    }

    #[tokio::test]
    async fn non_critical_failure_does_not_surface_critical_flag() {
        let supervisor = make_supervisor();
        supervisor.spawn_long_lived(
            TaskSpec::new(
                TaskKind::BackupScheduler,
                "backup",
                Criticality::NonCritical,
            ),
            async move { Err(EgoPulseError::Internal("backup failed".to_string())) },
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
        let outcome = supervisor.poll_long_lived();
        assert!(
            outcome.is_none(),
            "non-critical failure must not surface as critical"
        );
        let snap = supervisor.runtime_status.snapshot();
        assert!(snap.critical_task_failure.is_none());
        // Still recorded as a task_failure error.
        assert!(
            supervisor
                .runtime_status
                .recent_errors()
                .iter()
                .any(|e| e.error_kind == "task_failure"),
            "non-critical failure should still be recorded"
        );
    }

    #[tokio::test]
    async fn shutdown_token_cancels_cancellation_aware_task() {
        let supervisor = make_supervisor();
        let token = supervisor.shutdown_token();
        let started = Arc::new(AtomicUsize::new(0));
        let stopped = Arc::new(AtomicUsize::new(0));
        let started_c = Arc::clone(&started);
        let stopped_c = Arc::clone(&stopped);
        supervisor.spawn_long_lived(
            TaskSpec::new(TaskKind::McpReconnect, "mcp", Criticality::NonCritical),
            async move {
                started_c.store(1, AtomicOrdering::SeqCst);
                tokio::select! {
                    _ = token.cancelled() => {}
                    _ = std::future::pending::<()>() => {}
                }
                stopped_c.store(1, AtomicOrdering::SeqCst);
                Ok(())
            },
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(started.load(AtomicOrdering::SeqCst), 1);
        supervisor.shutdown().await;
        assert_eq!(
            stopped.load(AtomicOrdering::SeqCst),
            1,
            "cancellation-aware task should stop on shutdown"
        );
    }

    #[tokio::test]
    async fn shutdown_aborts_unresponsive_task_after_deadline() {
        let supervisor = Arc::new(RuntimeSupervisor::with_drain(
            Arc::new(RuntimeStatus::new()),
            Duration::from_millis(50),
            Duration::from_millis(50),
        ));
        // A task that never completes and is not cancellation-aware.
        supervisor.spawn_long_lived(
            TaskSpec::new(TaskKind::Channel, "stuck", Criticality::Critical),
            async move {
                std::future::pending::<()>().await;
                Ok(())
            },
        );
        let s = Arc::clone(&supervisor);
        let shutdown = tokio::time::timeout(Duration::from_secs(2), async move {
            s.shutdown().await;
        })
        .await;
        assert!(shutdown.is_ok(), "shutdown must not hang on a stuck task");
        assert!(supervisor.is_shutting_down());
    }

    #[tokio::test]
    async fn shutdown_is_idempotent() {
        let supervisor = make_supervisor();
        supervisor.shutdown().await;
        // Second call returns promptly without hanging.
        let second = tokio::time::timeout(Duration::from_millis(100), supervisor.shutdown()).await;
        assert!(second.is_ok(), "second shutdown call should be a no-op");
    }

    #[tokio::test]
    async fn in_flight_turn_drains_to_completion_before_abort() {
        let supervisor = Arc::new(RuntimeSupervisor::with_drain(
            Arc::new(RuntimeStatus::new()),
            Duration::from_millis(500),
            Duration::from_millis(50),
        ));
        let completed = Arc::new(AtomicUsize::new(0));
        let completed_c = Arc::clone(&completed);
        supervisor.spawn_turn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            completed_c.store(1, AtomicOrdering::SeqCst);
        });
        let s = Arc::clone(&supervisor);
        s.shutdown().await;
        assert_eq!(
            completed.load(AtomicOrdering::SeqCst),
            1,
            "in-flight turn should complete within the deadline"
        );
    }

    #[tokio::test]
    async fn accepting_inputs_toggles_with_start_and_shutdown() {
        let supervisor = make_supervisor();
        supervisor
            .accepting_inputs
            .store(false, AtomicOrdering::Release);
        supervisor.runtime_status.set_accepting_inputs(false);
        assert!(!supervisor.accepting_inputs());
        supervisor.start_accepting();
        assert!(supervisor.accepting_inputs());
        supervisor.shutdown().await;
        assert!(!supervisor.accepting_inputs());
        assert!(supervisor.is_shutting_down());
    }
}
