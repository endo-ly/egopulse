//! Runtime boundary for channel-originated input.
//!
//! Channels normalize platform events first; this module translates those
//! normalized inputs into EgoPulse runtime work: `SurfaceContext`, Channel Log
//! persistence, and scheduled turn submission.

use std::sync::Arc;
use std::sync::{OnceLock, Weak};

use crate::agent_loop::session::resolve_chat_id;
use crate::agent_loop::{
    ConversationScope, ScheduledTurn, SurfaceContext, canonical_request_hash,
    serialize_scheduled_turn,
};
use crate::error::EgoPulseError;
use crate::runtime::AppState;
use crate::runtime::metrics;
use crate::runtime::turn_scheduler::{RejectReason, ScheduleResult, SubmitOutcome};
use crate::storage::{AcceptOutcome, AcceptTurnParams};
use crate::storage::{MessageKind, SenderKind, StoredMessage, call_blocking};

/// Narrow capability for durably accepting and scheduling a target turn.
///
/// `AgentSendTool` (and only it) needs to push a target agent turn through the
/// same durable intake channels use. Rather than handing the whole [`AppState`]
/// to a tool — which would form an `AppState -> ToolRegistry -> AgentSendTool
/// -> AppState` strong cycle and force a fragile `Arc::get_mut` two-stage
/// construction — the tool holds an `Arc<TurnIntake>`. The intake is a thin
/// deferred handle: it is created up front (so the tool can be registered into
/// the registry before the registry is wrapped in `Arc`), and bound to the live
/// runtime once, after [`AppState`] is built.
///
/// The single `Weak<AppState>` is the minimal back-reference needed because
/// turn execution (`execute_scheduled_turn`) still runs inside the runtime that
/// owns this intake; in production the runtime outlives every tool call, so the
/// upgrade always succeeds. If it ever cannot (runtime already dropped), the
/// turn is reported as rejected so the tool surfaces `delivered: false`.
pub(crate) struct TurnIntake {
    runtime: OnceLock<Weak<AppState>>,
}

impl TurnIntake {
    pub(crate) fn new() -> Self {
        Self {
            runtime: OnceLock::new(),
        }
    }

    /// Binds the intake to the live runtime. Called exactly once, after
    /// [`AppState`] is constructed. Panics if called more than once, since a
    /// second bind would be a construction-order invariant violation rather
    /// than a recoverable condition.
    pub(crate) fn bind(&self, state: &Arc<AppState>) {
        self.runtime
            .set(Arc::downgrade(state))
            .expect("TurnIntake must be bound exactly once");
    }

    /// Durably accepts and schedules `turn` through the shared intake. Returns
    /// [`SubmitOutcome::Rejected`] only if the runtime is not (or no longer)
    /// bound; otherwise it mirrors [`submit_scheduled_turn`].
    pub(crate) async fn submit(&self, turn: ScheduledTurn) -> SubmitOutcome {
        match self.runtime.get().and_then(Weak::upgrade) {
            Some(state) => submit_scheduled_turn(&state, turn).await,
            None => SubmitOutcome::Rejected(RejectReason::Internal),
        }
    }
}

/// Platform-specific key used to resolve a multi-agent Channel Log chat.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChannelLogKey {
    Discord(u64),
    Telegram(i64),
}

/// Human-originated message to persist in a multi-agent Channel Log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HumanChannelLogMessage {
    pub(crate) key: ChannelLogKey,
    pub(crate) scope: ConversationScope,
    pub(crate) id: String,
    pub(crate) sender_id: String,
    pub(crate) content: String,
    pub(crate) timestamp: String,
}

/// Returns the storage scope represented by a channel's `secret` flag.
pub(crate) fn channel_scope_from_secret(secret: bool) -> ConversationScope {
    if secret {
        ConversationScope::Secret
    } else {
        ConversationScope::Normal
    }
}

/// Builds a surface context from channel-normalized input metadata.
pub(crate) fn build_channel_context(
    channel: &str,
    surface_user: &str,
    surface_thread: &str,
    chat_type: &str,
    agent_id: &str,
    scope: ConversationScope,
) -> SurfaceContext {
    let mut context = SurfaceContext::new(
        channel.to_string(),
        surface_user.to_string(),
        surface_thread.to_string(),
        chat_type.to_string(),
        agent_id.to_string(),
    );
    context.scope = scope;
    context
}

/// Resolves a Channel Log chat and stores one human-originated message.
pub(crate) async fn store_human_channel_log_message(
    state: &AppState,
    message: HumanChannelLogMessage,
) -> Option<i64> {
    let db = Arc::clone(state.db_for(message.scope));
    let key = message.key;
    match call_blocking(Arc::clone(&db), move |db| match key {
        ChannelLogKey::Discord(channel_id) => db.resolve_channel_log_chat_id(channel_id),
        ChannelLogKey::Telegram(chat_id) => db.resolve_telegram_channel_log_chat_id(chat_id),
    })
    .await
    {
        Ok(chat_id) => {
            let stored = StoredMessage {
                id: message.id,
                chat_id,
                sender_id: message.sender_id,
                content: message.content,
                sender_kind: SenderKind::User,
                timestamp: message.timestamp,
                message_kind: MessageKind::Message,
                recipient_agent_id: None,
                seq: None,
                turn_id: None,
                parent_message_id: None,
            };
            if let Err(error) = call_blocking(db, move |db| db.store_message_only(&stored)).await {
                tracing::warn!(
                    key = ?message.key,
                    error = %error,
                    "failed to store human message in Channel Log"
                );
            }
            Some(chat_id)
        }
        Err(error) => {
            tracing::warn!(
                key = ?message.key,
                error = %error,
                "failed to resolve Channel Log chat_id"
            );
            None
        }
    }
}

/// Submits an agent turn and starts execution immediately when the session is idle.
///
/// Returns [`SubmitOutcome`] so callers can distinguish an accepted turn
/// (started or queued) from a queue-capacity rejection. Rejections are logged
/// centrally here so no turn is silently dropped.
pub(crate) async fn submit_agent_turn(
    state: &AppState,
    context: SurfaceContext,
    input: String,
) -> SubmitOutcome {
    submit_scheduled_turn(
        state,
        ScheduledTurn {
            turn_id: uuid::Uuid::new_v4().to_string(),
            origin_id: context.origin_id.clone(),
            context,
            input,
        },
    )
    .await
}

/// Submits an agent turn and starts execution immediately when the session is idle.
///
/// The turn is durably accepted (its full request persisted to `turn_runs` as
/// `accepted`) **before** it is handed to the in-memory scheduler, so a crash
/// during shutdown can never lose an accepted turn: the turn dispatcher resumes
/// it from the database on the next startup.
///
/// Acceptance contract: once the DB commit succeeds the turn *is* accepted and
/// the caller observes success. Every rejection (shutdown, terminated chain,
/// origin tracker capacity) is decided **before** the commit, so a rejected
/// turn never leaves a runnable row that the dispatcher would later execute.
/// In-memory scheduler capacity after the commit is a wait-time problem, not a
/// rejection: the turn stays `accepted` and the dispatcher retries as capacity
/// frees.
pub(crate) async fn submit_scheduled_turn(
    state: &AppState,
    mut scheduled: ScheduledTurn,
) -> SubmitOutcome {
    // An empty request_key would collide on UNIQUE(chat_id, request_key) and
    // make every keyless turn on the same chat look like a duplicate. Assign a
    // stable key before the request is persisted so recovery reuses the same
    // one.
    if scheduled.context.request_key.is_empty() {
        scheduled.context.request_key = uuid::Uuid::new_v4().to_string();
    }
    // Generate the origin id at the acceptance boundary so live execution and
    // post-crash recovery share one identity (the DB row stores the same id).
    if scheduled.origin_id.is_empty() {
        scheduled.origin_id = uuid::Uuid::new_v4().to_string();
        scheduled.context.origin_id = scheduled.origin_id.clone();
    }
    let origin_id = scheduled.origin_id.clone();

    // Refuse new input the moment shutdown begins, before any commit.
    if !state.supervisor.accepting_inputs() {
        tracing::info!("turn rejected: runtime not accepting inputs (shutdown)");
        metrics::inc_turn_queue_rejections("shutdown");
        return SubmitOutcome::Rejected(RejectReason::Shutdown);
    }

    // Acceptance-time origin checks BEFORE the commit so a rejected turn leaves
    // no runnable row: a terminated chain or a full origin tracker.
    if let Err(reason) = state.turn_tracker.reserve(&origin_id) {
        tracing::warn!(reason = %reason, "turn rejected at acceptance: origin tracker");
        metrics::inc_turn_queue_rejections(reason.as_str());
        return SubmitOutcome::Rejected(reason);
    }

    // Durably accept the turn. On failure release the reservation so tracker
    // capacity is not leaked, and reject so the caller retries. Capacity
    // rejections (per-session / global durable-pending limits) are surfaced as
    // their own reason codes so callers can return the correct 429; they were
    // decided inside the same transaction as the INSERT, so no runnable row is
    // left behind.
    let accepted = match durably_accept_turn(state, &scheduled).await {
        Ok(outcome) => outcome,
        Err(error) => {
            state.turn_tracker.release(&origin_id);
            let reason = match &error {
                crate::error::EgoPulseError::Storage(
                    crate::error::StorageError::TurnSessionQueueFull,
                ) => {
                    tracing::info!(
                        chat_id = ?scheduled.context,
                        "turn rejected: per-session durable queue full"
                    );
                    metrics::inc_turn_queue_rejections("session_queue_full");
                    RejectReason::SessionQueueFull
                }
                crate::error::EgoPulseError::Storage(
                    crate::error::StorageError::TurnGlobalQueueFull,
                ) => {
                    tracing::info!("turn rejected: global durable queue full");
                    metrics::inc_turn_queue_rejections("global_queue_full");
                    RejectReason::GlobalQueueFull
                }
                _ => {
                    tracing::warn!(error = %error, "durable accept failed; rejecting turn");
                    metrics::inc_turn_queue_rejections(RejectReason::Internal.as_str());
                    RejectReason::Internal
                }
            };
            return SubmitOutcome::Rejected(reason);
        }
    };

    let run = match accepted {
        crate::storage::AcceptOutcome::Created(run) => run,
        // Same request already accepted elsewhere: do not start a second
        // execution. Release this reservation; the existing owner holds its own.
        crate::storage::AcceptOutcome::Existing(_) => {
            state.turn_tracker.release(&origin_id);
            return SubmitOutcome::Queued;
        }
    };
    // Stamp the authoritative ids from the DB row (#5: the DB is the source of
    // truth, not the tentative ScheduledTurn id).
    scheduled.turn_id = run.turn_id.clone();
    if let Some(canonical_origin) = run.origin_id.as_deref() {
        scheduled.origin_id = canonical_origin.to_string();
        scheduled.context.origin_id = canonical_origin.to_string();
    }

    // Post-commit: the turn is durably accepted. In-memory scheduler capacity
    // is a wait, not a rejection — defer to the dispatcher if it cannot run
    // now. A reservation is held until execution converts it.
    schedule_and_spawn(state, scheduled)
}

/// Re-enqueues an already-durably-accepted turn (used by the turn dispatcher
/// to resume or retry turns). Performs no acceptance work and no origin
/// reservation: live turns reserved at intake, and recovered origins are
/// rehydrated. The scheduler deduplicates by `turn_id`, so repeat dispatch is
/// an idempotent no-op; capacity overflow defers to the next scan.
pub(super) fn enqueue_durable_turn(state: &AppState, scheduled: ScheduledTurn) -> SubmitOutcome {
    schedule_and_spawn(state, scheduled)
}

/// Hands the turn to the in-memory scheduler, spawning execution immediately
/// when the session is idle. A scheduler rejection (capacity) is converted to
/// `Queued` rather than surfaced: the turn is already durably accepted, so it
/// must never be reported as rejected to the caller. The dispatcher retries as
/// capacity frees.
fn schedule_and_spawn(state: &AppState, scheduled: ScheduledTurn) -> SubmitOutcome {
    // Shutdown gate: never start a new turn task once shutdown has begun. The
    // durable turn stays in `turn_runs` and is resumed on the next startup, so
    // the supervisor only owns the in-flight turns it is draining (the
    // dispatcher, an input producer, is still running at this point and would
    // otherwise spawn into the fresh post-drain JoinSet).
    if state.supervisor.is_shutting_down() {
        return SubmitOutcome::Queued;
    }
    match state.turn_scheduler.submit(scheduled) {
        ScheduleResult::Started(turn) => {
            let turn = *turn;
            let state = state.clone();
            let supervisor = Arc::clone(&state.supervisor);
            supervisor.spawn_turn(async move {
                crate::runtime::execute_scheduled_turn(&state, turn).await;
            });
            SubmitOutcome::Started
        }
        // Enqueued or already-owned by the scheduler: the turn waits in
        // `turn_runs` for the dispatcher / completion drain.
        ScheduleResult::Enqueued | ScheduleResult::AlreadyOwned => SubmitOutcome::Queued,
        ScheduleResult::DeferredCapacity => {
            tracing::debug!("turn deferred: in-memory scheduler at capacity; dispatcher retries");
            SubmitOutcome::Queued
        }
    }
}

/// Persists the full request to `turn_runs` as `accepted` before scheduling.
///
/// Uses the same `(chat_id, request_key)` identity and canonical payload hash
/// the executor will use, so the executor's later `accept_or_get_turn` call
/// finds the same row (idempotent `Existing`) instead of creating a duplicate.
/// The fully serialized [`ScheduledTurn`] is stored in `scheduled_request_json`
/// so the turn dispatcher can rebuild and resume it after a crash.
async fn durably_accept_turn(
    state: &AppState,
    scheduled: &ScheduledTurn,
) -> Result<AcceptOutcome, EgoPulseError> {
    let scope = scheduled.context.scope;
    let chat_id = resolve_chat_id(&state.turn_runtime(), &scheduled.context).await?;
    let request_hash = canonical_request_hash(&scheduled.context, &scheduled.input);
    let scheduled_json = serialize_scheduled_turn(scheduled)?;
    let snapshot = state.config_manager.current_blocking();
    let request_key = scheduled.context.request_key.clone();
    let origin_id = if scheduled.origin_id.is_empty() {
        None
    } else {
        Some(scheduled.origin_id.clone())
    };
    let revision = snapshot.revision as i64;
    let fingerprint = snapshot.fingerprint.clone();
    call_blocking(Arc::clone(state.db_for(scope)), move |db| {
        db.accept_or_get_turn(AcceptTurnParams {
            chat_id,
            request_key: &request_key,
            config_revision: revision,
            config_fingerprint: Some(&fingerprint),
            request_payload_hash: &request_hash,
            origin_id: origin_id.as_deref(),
            scheduled_request_json: Some(&scheduled_json),
        })
    })
    .await
    .map_err(EgoPulseError::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::runtime::build_sleep_app_state_with_path;

    fn build_test_state(dir: &tempfile::TempDir) -> AppState {
        let config = crate::test_util::test_config(dir.path().to_str().expect("utf8"));
        build_sleep_app_state_with_path(config, Some(dir.path().join("egopulse.config.yaml")))
            .expect("build sleep state")
    }

    #[test]
    fn channel_scope_from_secret_maps_channel_flag() {
        assert_eq!(channel_scope_from_secret(true), ConversationScope::Secret);
        assert_eq!(channel_scope_from_secret(false), ConversationScope::Normal);
    }

    #[test]
    fn build_channel_context_applies_scope_and_session_identity() {
        let context = build_channel_context(
            "discord",
            "alice",
            "123",
            "discord",
            "reviewer",
            ConversationScope::Secret,
        );

        assert_eq!(context.channel, "discord");
        assert_eq!(context.surface_user, "alice");
        assert_eq!(context.surface_thread, "123");
        assert_eq!(context.chat_type, "discord");
        assert_eq!(context.agent_id, "reviewer");
        assert_eq!(context.scope, ConversationScope::Secret);
        assert_eq!(context.session_key(), "discord:123:agent:reviewer");
    }

    #[tokio::test]
    async fn store_human_channel_log_message_persists_discord_message() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = build_test_state(&dir);

        let chat_id = store_human_channel_log_message(
            &state,
            HumanChannelLogMessage {
                key: ChannelLogKey::Discord(42),
                scope: ConversationScope::Normal,
                id: "cl-42".to_string(),
                sender_id: "user:discord:7".to_string(),
                content: "hello".to_string(),
                timestamp: "2026-06-25T00:00:00Z".to_string(),
            },
        )
        .await
        .expect("channel log chat id");

        let messages = state
            .db_for(ConversationScope::Normal)
            .get_channel_log_messages(chat_id, 10)
            .expect("messages");
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].id, "cl-42");
        assert_eq!(messages[0].sender_id, "user:discord:7");
        assert_eq!(messages[0].sender_kind, SenderKind::User);
        assert_eq!(messages[0].content, "hello");
    }
}
