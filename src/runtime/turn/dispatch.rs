//! Durable turn dispatch and scheduled turn execution.

use std::sync::Arc;
use std::time::Duration;

use tokio_util::sync::CancellationToken;

use crate::agent_loop::resume_input_committed_turn;
use crate::config::manager::ConfigSnapshot;
use crate::conversation::{ConversationScope, SurfaceContext};
use crate::error::EgoPulseError;
use crate::runtime::status::RuntimeStatus;
use crate::runtime::turn::{ScheduledTurn, deserialize_scheduled_turn};
use crate::runtime::turn::{ToolProgressCoordinator, scheduler};
use crate::runtime::{AppState, Criticality, TaskKind, TaskSpec, channel_input, metrics};
use crate::storage::{
    DISPATCHER_BATCH_LIMIT, DURABLE_PAYLOAD_INVALID_ERROR_KIND, Database, TurnRunState,
    call_blocking,
};

// ---------------------------------------------------------------------------
// Durable turn dispatcher
// ---------------------------------------------------------------------------

struct PreparedScheduledTurn {
    turn: ScheduledTurn,
    session_key: String,
    origin_id: String,
}

/// Spawns the turn dispatcher: a long-lived supervisor task that periodically
/// scans both databases for durably accepted turns (request persisted but never
/// started) and re-submits them so a crash before execution is fully recovered
pub(in crate::runtime) fn spawn_turn_dispatcher(state: Arc<AppState>, shutdown: CancellationToken) {
    let supervisor = Arc::clone(&state.supervisor);
    supervisor.spawn_long_lived(
        TaskSpec::new(
            TaskKind::TurnDispatcher,
            "turn-dispatcher",
            Criticality::Critical,
        ),
        async move {
            loop {
                if shutdown.is_cancelled() {
                    break;
                }
                if let Err(error) = dispatch_durable_turns(&state).await {
                    tracing::warn!(error = %error, "turn dispatcher scan failed");
                }
                tokio::select! {
                    _ = shutdown.cancelled() => break,
                    _ = tokio::time::sleep(Duration::from_secs(5)) => {}
                }
            }
            Ok(())
        },
    );
}

/// Re-enqueues durably accepted turns found in the databases. Each is rebuilt
/// from its persisted request and re-submitted in acceptance order; the
/// scheduler deduplicates by `turn_id`, so re-scanning a turn that is already
/// running or queued is an idempotent no-op and no per-process dedup set is
/// needed. A database failure leaves the row pending so the next scan retries.
/// An invalid persisted payload is terminalized as `failed`, while turns the
/// scheduler cannot accept yet (capacity) stay `accepted`/`input_committed` in
/// the DB and are retried as capacity frees.
///
/// Each tick scans from the head (`("", "")`) with in-tick cursor pagination.
/// Because the scheduler deduplicates already-owned turns (a cheap HashMap
/// lookup that neither spawns a task nor enqueues a duplicate), re-reading the
/// busy prefix every tick is a no-op for those turns. Capacity-rejected turns
/// are *not* advanced past permanently: they stay in the DB and are revisited
/// on the next tick from the head, so no turn is ever permanently skipped.
/// Bounded by `MAX_PAGES_PER_TICK` so a huge backlog cannot monopolise a single
/// tick; remaining turns are reached on subsequent ticks.
async fn dispatch_durable_turns(state: &Arc<AppState>) -> Result<(), EgoPulseError> {
    let mut backlog_total: u64 = 0;
    let mut backlog_ok = true;
    for (scope, db) in state.scoped_databases() {
        match dispatch_durable_turns_for_scope(state, scope, db).await? {
            Some(count) => backlog_total += count,
            None => backlog_ok = false,
        }
    }
    if backlog_ok {
        metrics::set_durable_pending_turns(backlog_total as usize);
    }
    Ok(())
}

async fn dispatch_durable_turns_for_scope(
    state: &Arc<AppState>,
    scope: ConversationScope,
    db: Arc<Database>,
) -> Result<Option<u64>, EgoPulseError> {
    let backlog = match call_blocking(Arc::clone(&db), |db| db.count_durable_pending()).await {
        Ok(count) => Some(count as u64),
        Err(error) => {
            tracing::warn!(
                scope = %scope,
                error = %error,
                "durable backlog count failed; retaining previous gauge"
            );
            None
        }
    };

    const MAX_PAGES_PER_TICK: u32 = 8;
    let mut pages = 0u32;
    let mut after_at = String::new();
    let mut after_id = String::new();
    loop {
        let batch = call_blocking(Arc::clone(&db), {
            let after_at = after_at.clone();
            let after_id = after_id.clone();
            move |db| {
                db.scan_durable_pending_turns_after(&after_at, &after_id, DISPATCHER_BATCH_LIMIT)
            }
        })
        .await
        .map_err(EgoPulseError::from)?;

        let batch_len = batch.len();
        for crate::storage::DurablePendingTurn {
            turn_id,
            accepted_at,
            scheduled_request_json: json,
        } in batch
        {
            // Advance the in-tick cursor before dispatching so the next page
            // resumes after this turn regardless of the dispatch outcome.
            after_at = accepted_at;
            after_id = turn_id.clone();

            let mut turn = match deserialize_scheduled_turn(&json) {
                Ok(turn) => turn,
                Err(_) => {
                    terminalize_invalid_durable_turn(&db, scope, &turn_id).await?;
                    continue;
                }
            };
            turn.turn_id = turn_id;
            // Re-enqueue. The scheduler deduplicates by `turn_id`, so a turn
            // already running or queued is an idempotent no-op; a turn rejected
            // by capacity stays in the DB for the next tick.
            let _ = channel_input::enqueue_durable_turn(state, turn);
        }

        pages += 1;
        if (batch_len as i64) < DISPATCHER_BATCH_LIMIT || pages >= MAX_PAGES_PER_TICK {
            break;
        }
    }

    Ok(backlog)
}

async fn terminalize_invalid_durable_turn(
    db: &Arc<Database>,
    scope: ConversationScope,
    turn_id: &str,
) -> Result<(), EgoPulseError> {
    let failed_turn_id = turn_id.to_owned();
    match call_blocking(Arc::clone(db), move |db| {
        db.fail_invalid_durable_turn(&failed_turn_id)
    })
    .await
    {
        Ok(()) => {
            metrics::inc_durable_payload_invalid();
            tracing::warn!(
                scope = %scope,
                turn_id,
                error_kind = DURABLE_PAYLOAD_INVALID_ERROR_KIND,
                "terminalized durable turn with invalid payload"
            );
        }
        Err(crate::error::StorageError::Conflict(_)) => {
            tracing::debug!(
                scope = %scope,
                turn_id,
                "durable turn state changed before invalid payload terminalization"
            );
        }
        Err(error) => return Err(EgoPulseError::from(error)),
    }
    Ok(())
}

async fn retry_durable_storage_write<Attempt, WriteFuture, IsAlreadyDone>(
    state: &AppState,
    identifier: &str,
    operation: &str,
    mut attempt: Attempt,
    is_already_done: IsAlreadyDone,
) -> Result<(), EgoPulseError>
where
    Attempt: FnMut() -> WriteFuture,
    WriteFuture: std::future::Future<Output = Result<(), crate::error::StorageError>>,
    IsAlreadyDone: Fn(&crate::error::StorageError) -> bool,
{
    let mut backoff = Duration::from_millis(50);
    const MAX_BACKOFF: Duration = Duration::from_secs(5);

    loop {
        match attempt().await {
            Ok(()) => return Ok(()),
            Err(error) if is_already_done(&error) => {
                tracing::debug!(
                    operation,
                    identifier,
                    error = %error,
                    "durable write already completed; treating as success"
                );
                return Ok(());
            }
            Err(error) => {
                if state.supervisor.is_shutting_down() {
                    tracing::info!(
                        operation,
                        identifier,
                        error = %error,
                        "shutdown during durable write retry; leaving state blocked"
                    );
                    return Err(EgoPulseError::from(error));
                }
                tracing::warn!(
                    operation,
                    identifier,
                    backoff_ms = backoff.as_millis(),
                    error = %error,
                    "durable write transient failure; retrying with backoff"
                );
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(MAX_BACKOFF);
            }
        }
    }
}

/// Persists the durable cancellation of a turn, retrying across transient
/// storage failures so a momentary DB hiccup does not leave the runnable row in
/// `accepted`/`input_committed`. A row left accepted would be re-delivered by
/// the turn dispatcher after a restart, re-running a turn a stop condition had
/// already rejected.
///
/// The synchronous SQLite write is performed on a blocking thread via
/// [`call_blocking`] so it never stalls the async executor. The call sites are
/// fail-closed: a returned `Err` means the turn could not be durably cancelled,
/// and the caller must NOT mark the turn complete or start the next queued turn
/// for the same session (see [`execute_scheduled_turn`]).
///
/// Retries indefinitely with exponential backoff (capped at 5 s) so a transient
/// DB outage does not wedge the session permanently — once the DB recovers the
/// cancellation lands and the session advances. A `Conflict` (the turn already
/// moved past the cancellable point, e.g. another executor raced ahead) is
/// treated as success: the turn is past cancellation and retrying cannot help.
/// The loop aborts only on shutdown, returning `Err` so the caller can exit.
///
/// # Errors
///
/// Returns [`EgoPulseError`] only when shutdown begins mid-retry.
async fn persist_turn_cancellation(
    state: &AppState,
    scope: crate::conversation::ConversationScope,
    turn_id: &str,
    reason: &str,
    note: &str,
) -> Result<(), EgoPulseError> {
    let db = state.db_for(scope);
    let turn_id = turn_id.to_owned();
    let identifier = turn_id.clone();
    let reason = reason.to_owned();
    let note = note.to_owned();

    retry_durable_storage_write(
        state,
        &identifier,
        "cancel_turn",
        move || {
            let db = Arc::clone(&db);
            let turn_id = turn_id.clone();
            let reason = reason.clone();
            let note = note.clone();
            call_blocking(db, move |db| db.cancel_turn(&turn_id, &reason, &note))
        },
        |error| {
            matches!(
                error,
                crate::error::StorageError::NotFound(_) | crate::error::StorageError::Conflict(_)
            )
        },
    )
    .await
}

/// Durably records an origin's terminal stop reason in `turn_origins` so a
/// terminated chain survives a restart and is not silently resumed by the
/// dispatcher. Fail-closed: a failure is returned to the caller, which must NOT
/// start the next queued turn for the session — the in-memory tracker already
/// enforces the stop for this process, but the durable write protects the
/// post-restart window. Retries indefinitely with exponential backoff so a
/// transient DB outage does not wedge the session; the loop aborts only on
/// shutdown.
///
/// # Errors
///
/// Returns [`EgoPulseError`] only when shutdown begins mid-retry.
async fn persist_origin_terminal_reason(
    state: &AppState,
    scope: crate::conversation::ConversationScope,
    origin_id: &str,
    reason: scheduler::StopReason,
) -> Result<(), EgoPulseError> {
    let db = state.db_for(scope);
    let origin_id = origin_id.to_owned();
    let identifier = origin_id.clone();
    let reason = reason.to_string();

    retry_durable_storage_write(
        state,
        &identifier,
        "upsert_turn_origin",
        move || {
            let db = Arc::clone(&db);
            let origin_id = origin_id.clone();
            let reason = reason.clone();
            call_blocking(db, move |db| {
                db.upsert_turn_origin(&origin_id, Some(&reason))
            })
        },
        |_| false,
    )
    .await
}

/// Recovers durable tool and turn state on startup for every initialized scope.
///
/// Recovery is fail-closed: a failure in any database is returned so startup
/// cannot accept new turns while durable state is only partially recovered.
pub(in crate::runtime) async fn recover_durable_state(
    state: &AppState,
) -> Result<(), EgoPulseError> {
    for (scope, db) in state.scoped_databases() {
        recover_durable_state_for_db(&db, scope)?;
    }
    Ok(())
}

fn recover_durable_state_for_db(
    db: &Database,
    scope: ConversationScope,
) -> Result<(), EgoPulseError> {
    match db.recover_running_tools() {
        Ok(recovered) if !recovered.is_empty() => {
            for tool in &recovered {
                tracing::info!(
                    scope = %scope,
                    turn_id = %tool.turn_id,
                    tool_call_id = %tool.tool_call_id,
                    tool_name = %tool.tool_name,
                    from = "running",
                    to = %tool.recovered_to,
                    "recovered tool_call on startup"
                );
            }
        }
        Ok(_) => {}
        Err(error) => return Err(EgoPulseError::from(error)),
    }
    match db.recover_interrupted_turns() {
        Ok(recovered) if !recovered.is_empty() => {
            for turn in &recovered {
                tracing::info!(
                    scope = %scope,
                    turn_id = %turn.turn_id,
                    chat_id = turn.chat_id,
                    from = %turn.from,
                    to = %turn.recovered_to,
                    "recovered turn_run on startup"
                );
            }
        }
        Ok(_) => {}
        Err(error) => return Err(EgoPulseError::from(error)),
    }
    Ok(())
}

/// Rehydrates the in-memory origin tracker from durable turn state after a
/// restart so chain limits and terminal guards survive process loss.
pub(in crate::runtime) fn rehydrate_origin_tracker(state: &AppState) -> Result<(), EgoPulseError> {
    let ttl_secs = scheduler::ORIGIN_TTL.as_secs() as i64;
    for (scope, db) in state.scoped_databases() {
        rehydrate_origin_tracker_for_db(state, &db, scope, ttl_secs)?;
    }
    Ok(())
}

fn rehydrate_origin_tracker_for_db(
    state: &AppState,
    db: &Database,
    scope: ConversationScope,
    ttl_secs: i64,
) -> Result<(), EgoPulseError> {
    let origins = db.recover_origin_tracker(ttl_secs)?;
    if !origins.is_empty() {
        tracing::info!(
            scope = %scope,
            count = origins.len(),
            "rehydrated origin tracker from turn_runs"
        );
        state.turn_tracker.rehydrate_executed(&origins);
    }
    Ok(())
}

pub(crate) fn execute_scheduled_turn(
    state: &AppState,
    turn: ScheduledTurn,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
    Box::pin(async move {
        let Some(prepared) = prepare_scheduled_turn(state, turn).await else {
            return;
        };

        let current_state = match load_durable_turn_state(state, &prepared.turn).await {
            DurableTurnStateLookup::Found(state) => Some(state),
            DurableTurnStateLookup::Missing => None,
            DurableTurnStateLookup::Shutdown => return,
        };
        let config_snapshot = prepared
            .turn
            .config_snapshot
            .clone()
            .unwrap_or_else(|| state.config_manager.current_blocking());

        if !begin_scheduled_turn(state, &prepared, &config_snapshot).await {
            return;
        }

        execute_and_publish_scheduled_turn(state, &prepared, current_state, config_snapshot).await;
    })
}

// ---------------------------------------------------------------------------
// Scheduled turn lifecycle
// ---------------------------------------------------------------------------

async fn prepare_scheduled_turn(
    state: &AppState,
    mut turn: ScheduledTurn,
) -> Option<PreparedScheduledTurn> {
    turn.context.trace_id = uuid::Uuid::new_v4().to_string();
    let session_key = turn.session_key();
    let origin_id = if turn.origin_id.is_empty() {
        uuid::Uuid::new_v4().to_string()
    } else {
        turn.origin_id.clone()
    };
    state
        .runtime_status
        .touch_channel_activity(&turn.context.channel);

    if state.supervisor.is_shutting_down() {
        tracing::info!(
            agent_id = %turn.context.agent_id,
            origin_id = %origin_id,
            "shutdown in progress: not starting submitted turn"
        );
        state.turn_tracker.release(&origin_id);
        return None;
    }

    if let Some(reason) = state.turn_tracker.terminal_reason(&origin_id) {
        tracing::warn!(
            agent_id = %turn.context.agent_id,
            origin_id = %origin_id,
            reason = ?reason,
            "dropping turn: origin already has terminal stop reason"
        );

        let reason_text = reason.to_string();
        match persist_turn_cancellation(
            state,
            turn.context.scope,
            &turn.turn_id,
            &reason_text,
            "origin chain already terminated",
        )
        .await
        {
            Ok(()) => {
                state.turn_tracker.release(&origin_id);
                drain_next_queued_turn(state, &session_key).await;
            }
            Err(error) => {
                tracing::error!(
                    error = %error,
                    turn_id = %turn.turn_id,
                    "durable cancellation failed; leaving turn blocked until DB recovers"
                );
            }
        }
        return None;
    }

    Some(PreparedScheduledTurn {
        turn,
        session_key,
        origin_id,
    })
}

enum DurableTurnStateLookup {
    Found(TurnRunState),
    Missing,
    Shutdown,
}

async fn load_durable_turn_state(state: &AppState, turn: &ScheduledTurn) -> DurableTurnStateLookup {
    let mut backoff = Duration::from_millis(20);
    const MAX_TURN_DB_BACKOFF: Duration = Duration::from_secs(5);

    loop {
        match call_blocking(state.db_for(turn.context.scope), {
            let turn_id = turn.turn_id.clone();
            move |db| db.get_turn_run(&turn_id).map(|run| run.state)
        })
        .await
        {
            Ok(state) => return DurableTurnStateLookup::Found(state),
            Err(crate::error::StorageError::NotFound(_)) => {
                return DurableTurnStateLookup::Missing;
            }
            Err(error) => {
                if state.supervisor.is_shutting_down() {
                    tracing::info!(
                        error = %error,
                        turn_id = %turn.turn_id,
                        "shutdown during turn state lookup; leaving turn blocked"
                    );
                    return DurableTurnStateLookup::Shutdown;
                }
                tracing::warn!(
                    backoff_ms = backoff.as_millis(),
                    error = %error,
                    turn_id = %turn.turn_id,
                    "dispatcher: turn state lookup failed; retrying with backoff"
                );
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(MAX_TURN_DB_BACKOFF);
            }
        }
    }
}

async fn begin_scheduled_turn(
    state: &AppState,
    prepared: &PreparedScheduledTurn,
    config_snapshot: &ConfigSnapshot,
) -> bool {
    let turn = &prepared.turn;
    let valid_ids: Vec<&str> = config_snapshot
        .config
        .agents
        .keys()
        .map(|id| id.as_str())
        .collect();
    let chain_depth = turn.context.chain_depth;
    let agent_id = &turn.context.agent_id;

    match state.turn_tracker.try_begin_execution(
        &prepared.origin_id,
        chain_depth,
        agent_id,
        &valid_ids,
    ) {
        Ok(_) => true,
        Err(reason) => {
            tracing::warn!(
                agent_id = %agent_id,
                chain_depth,
                reason = ?reason,
                "scheduled turn rejected by stop condition evaluator"
            );
            if let Err(error) = persist_origin_terminal_reason(
                state,
                turn.context.scope,
                &prepared.origin_id,
                reason.clone(),
            )
            .await
            {
                tracing::error!(
                    error = %error,
                    origin_id = %prepared.origin_id,
                    turn_id = %turn.turn_id,
                    "durable terminal reason persist failed; leaving turn blocked"
                );
                return false;
            }
            if let Err(error) = persist_turn_cancellation(
                state,
                turn.context.scope,
                &turn.turn_id,
                &reason.to_string(),
                &format!("turn rejected by stop condition: {reason:?}"),
            )
            .await
            {
                tracing::error!(
                    error = %error,
                    turn_id = %turn.turn_id,
                    "durable stop cancellation failed; leaving turn blocked"
                );
                return false;
            }

            let reason_text = format!("{reason:?}");
            state.runtime_status.push_error(
                &turn.context.trace_id,
                "stop_condition",
                agent_id,
                &turn.context.channel,
                &reason_text,
            );
            metrics::inc_turn_errors_total("stop_condition", agent_id);
            if let Some(log_chat_id) = turn.context.channel_log_chat_id {
                let reason_for_event = reason.clone();
                if let Err(error) = call_blocking(state.db_for(turn.context.scope), move |db| {
                    db.store_system_event(log_chat_id, &reason_for_event)
                })
                .await
                {
                    tracing::warn!(error = %error, "failed to store system event for stop condition");
                }
            }
            drain_next_queued_turn(state, &prepared.session_key).await;
            false
        }
    }
}

async fn execute_and_publish_scheduled_turn(
    state: &AppState,
    prepared: &PreparedScheduledTurn,
    current_state: Option<TurnRunState>,
    config_snapshot: Arc<ConfigSnapshot>,
) {
    let turn = &prepared.turn;
    let origin_id = &prepared.origin_id;
    let session_key = &prepared.session_key;
    let adapter = state.channels.get(&turn.context.channel).cloned();
    let external_chat_id = turn.context.session_key();
    let _activity = match adapter.as_ref() {
        Some(adapter) => match adapter.begin_turn_activity(&external_chat_id).await {
            Ok(activity) => Some(activity),
            Err(error) => {
                tracing::warn!(
                    agent_id = %turn.context.agent_id,
                    error = %error,
                    "scheduled turn: failed to begin channel activity"
                );
                None
            }
        },
        None => None,
    };

    let started_at = chrono::Utc::now().to_rfc3339();
    let started = std::time::Instant::now();
    let runtime = state.turn_dependencies();
    let turn_result = match current_state {
        Some(TurnRunState::InputCommitted) => {
            resume_input_committed_turn(
                &runtime,
                turn.context.scope,
                &turn.turn_id,
                Arc::clone(&config_snapshot),
            )
            .await
        }
        _ => {
            execute_turn_with_progress_and_snapshot(
                state,
                &turn.context,
                &turn.input,
                config_snapshot,
            )
            .await
        }
    };
    let duration = started.elapsed().as_secs_f64();
    let is_concurrency_conflict = turn_result
        .as_ref()
        .err()
        .is_some_and(|error| matches!(error, EgoPulseError::TurnConcurrencyConflict));
    if !is_concurrency_conflict {
        record_turn_observation(
            &state.runtime_status,
            &turn.context,
            &started_at,
            duration,
            &turn_result,
        );
    }

    match turn_result {
        Ok(response) => {
            if let Some(adapter) = adapter.as_ref() {
                if let Err(error) = adapter.send_text(&external_chat_id, &response).await {
                    tracing::warn!(
                        agent_id = %turn.context.agent_id,
                        error = %error,
                        "scheduled turn: failed to send response to channel"
                    );
                    state.runtime_status.push_error(
                        &turn.context.trace_id,
                        "channel_send",
                        &turn.context.agent_id,
                        &turn.context.channel,
                        &error.to_string(),
                    );
                    metrics::inc_turn_errors_total("channel_send", &turn.context.agent_id);
                }
            }
            store_scheduled_turn_response(state, turn, &response).await;
        }
        Err(error) => {
            if is_concurrency_conflict {
                drain_next_queued_turn(state, session_key).await;
                return;
            }
            tracing::warn!(
                agent_id = %turn.context.agent_id,
                error = %error,
                "scheduled turn: process_turn failed"
            );
            state
                .turn_tracker
                .set_terminal_reason(origin_id, scheduler::StopReason::LlmFailure);
            if let Err(persist_err) = persist_origin_terminal_reason(
                state,
                turn.context.scope,
                origin_id,
                scheduler::StopReason::LlmFailure,
            )
            .await
            {
                tracing::error!(
                    error = %persist_err,
                    origin_id,
                    turn_id = %turn.turn_id,
                    "durable terminal reason persist failed; leaving turn blocked"
                );
                return;
            }
            if let Some(log_chat_id) = turn.context.channel_log_chat_id {
                let reason = scheduler::StopReason::LlmFailure;
                if let Err(db_err) = call_blocking(state.db_for(turn.context.scope), move |db| {
                    db.store_system_event(log_chat_id, &reason)
                })
                .await
                {
                    tracing::warn!(error = %db_err, "failed to store LLM failure system event");
                }
            }
            send_turn_failure_to_channel(adapter.as_deref(), &external_chat_id, &error).await;
        }
    }

    drain_next_queued_turn(state, session_key).await;
}

fn record_turn_observation(
    status: &RuntimeStatus,
    context: &SurfaceContext,
    started_at: &str,
    duration_secs: f64,
    result: &Result<String, EgoPulseError>,
) {
    status.push_turn(
        &context.trace_id,
        &context.agent_id,
        &context.channel,
        started_at,
        duration_secs,
        result.is_ok(),
    );
    if let Err(error) = result {
        status.push_error(
            &context.trace_id,
            "turn_failure",
            &context.agent_id,
            &context.channel,
            &error.to_string(),
        );
        metrics::inc_turn_errors_total("turn_failure", &context.agent_id);
    }
}

async fn store_scheduled_turn_response(state: &AppState, turn: &ScheduledTurn, response: &str) {
    if response.is_empty() {
        return;
    }
    let Some(log_chat_id) = turn.context.channel_log_chat_id else {
        return;
    };

    let db = state.db_for(turn.context.scope);
    let agent_id = turn.context.agent_id.clone();
    let response = response.to_owned();
    if let Err(error) = call_blocking(db, move |db| {
        db.store_channel_log_bot_response(log_chat_id, &agent_id, &response)
    })
    .await
    {
        tracing::warn!(error = %error, "failed to store bot response in Channel Log");
    }
}

/// Drains the next queued turn for a session after the current turn completes.
///
/// During shutdown (`accepting_inputs == false`) the next queued turn is **not**
/// started: its origin reservation is released and the chain stops, so the
/// in-flight turn task can complete and be reaped by the supervisor drain.
/// This is the single point that enforces "no new turn starts after shutdown
/// begins" for turns already buffered in the in-memory scheduler.
async fn drain_next_queued_turn(state: &AppState, session_key: &str) {
    if let Some(next) = state.turn_scheduler.on_turn_completed(session_key) {
        if state.supervisor.is_shutting_down() {
            state.turn_tracker.release(&next.origin_id);
            tracing::info!(
                origin_id = %next.origin_id,
                "shutdown in progress: not starting next queued turn"
            );
            return;
        }
        execute_scheduled_turn(state, next).await;
    }
}

/// Executes one agent turn while recording runtime activity and telemetry.
///
/// The crate-visible helper accepts the shared [`AppState`], a
/// [`crate::conversation::SurfaceContext`], and the user `input`, returning the
/// generated response as `Result<String, EgoPulseError>`. It touches channel
/// activity, records the completed turn, and records an error plus the
/// `turn_failure` metric when execution fails.
///
/// # Errors
///
/// Propagates any [`EgoPulseError`] returned by the single turn execution.
/// Such failures are also recorded through `runtime_status.push_error`.
pub(crate) async fn execute_observed_turn(
    state: &AppState,
    context: &SurfaceContext,
    input: &str,
) -> Result<String, EgoPulseError> {
    state
        .runtime_status
        .touch_channel_activity(&context.channel);
    let started_at = chrono::Utc::now().to_rfc3339();
    let started = std::time::Instant::now();
    let runtime = state.turn_dependencies();
    let result =
        crate::agent_loop::process_turn_with_events(&runtime, context, input, |_| {}).await;
    let duration = started.elapsed().as_secs_f64();
    record_turn_observation(
        &state.runtime_status,
        context,
        &started_at,
        duration,
        &result,
    );
    result
}

async fn execute_turn_with_progress_and_snapshot(
    state: &AppState,
    context: &SurfaceContext,
    input: &str,
    config_snapshot: Arc<ConfigSnapshot>,
) -> Result<String, EgoPulseError> {
    let adapter = state.channels.get(&context.channel);
    let external_chat_id = context.session_key();
    let sink = adapter
        .and_then(|adapter| adapter.tool_progress_sink())
        .filter(|_| tool_progress_enabled(&config_snapshot.config, context));

    let (evt_tx, evt_rx) =
        tokio::sync::mpsc::unbounded_channel::<crate::agent_loop::event::AgentEvent>();
    let coordinator = ToolProgressCoordinator::new(sink, external_chat_id.clone());
    let coordinator_handle = tokio::spawn(coordinator.run(evt_rx));
    // timeout 枝でタスクを確実に停止できるよう abort handle を保持する。
    let coordinator_abort = coordinator_handle.abort_handle();

    let event_sender = evt_tx.clone();
    let runtime = state.turn_dependencies();
    let result = crate::agent_loop::process_turn_with_events_and_snapshot(
        &runtime,
        context,
        input,
        move |event| {
            let _ = event_sender.send(event);
        },
        config_snapshot,
    )
    .await;

    if result
        .as_ref()
        .is_err_and(|error| error.is_codex_auth_error())
    {
        tracing::warn!("codex 401 detected, refreshing token for a later turn");
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .build()
            .unwrap_or_default();
        crate::llm::codex_auth::force_refresh_codex_token(&http).await;
    }

    // `evt_tx` を全て drop してから await する（さもないと coordinator が EOF を検出できずハングする）。
    drop(evt_tx);
    match tokio::time::timeout(Duration::from_secs(2), coordinator_handle).await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => tracing::warn!(
            error = %error,
            "tool progress coordinator task failed"
        ),
        Err(_) => {
            coordinator_abort.abort();
            tracing::warn!("tool progress coordinator did not finish within timeout; aborted");
        }
    }
    result
}

/// 当該チャネルで進捗表示が有効かを設定からルックアップする。
fn tool_progress_enabled(config: &crate::config::Config, context: &SurfaceContext) -> bool {
    let channel_config = config.channels.get(context.channel.as_str());
    match context.channel.as_str() {
        "discord" => channel_config
            .and_then(|c| c.discord_channels.as_ref())
            .and_then(|channels| {
                context
                    .surface_thread
                    .parse::<u64>()
                    .ok()
                    .and_then(|id| channels.get(&id))
            })
            .is_some_and(|c| c.tool_progress),
        "telegram" => channel_config
            .and_then(|c| c.telegram_channels.as_ref())
            .and_then(|channels| {
                context
                    .surface_thread
                    .parse::<i64>()
                    .ok()
                    .and_then(|id| channels.get(&id))
            })
            .is_some_and(|c| c.tool_progress),
        _ => false,
    }
}

async fn send_turn_failure_to_channel(
    adapter: Option<&dyn crate::channels::adapter::ChannelAdapter>,
    external_chat_id: &str,
    error: &EgoPulseError,
) {
    let Some(adapter) = adapter else { return };
    let message = format!("⚠️ {}", error.user_facing_summary());
    if let Err(send_err) = adapter.send_text(external_chat_id, &message).await {
        tracing::warn!(
            error = %send_err,
            "failed to send turn failure message to channel"
        );
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use super::*;
    use crate::agent_loop::test_support::RecordingProvider;
    use tracing_subscriber::layer::SubscriberExt;

    fn final_provider() -> RecordingProvider {
        RecordingProvider::new(
            vec![Ok(crate::llm::MessagesResponse {
                content: "ok".to_string(),
                reasoning_content: None,
                tool_calls: Vec::new(),
                usage: None,
            })],
            vec![0],
        )
    }

    #[derive(Clone)]
    struct TraceCapture {
        trace_ids: Arc<Mutex<Vec<String>>>,
    }

    impl TraceCapture {
        fn new() -> Self {
            Self {
                trace_ids: Arc::new(Mutex::new(Vec::new())),
            }
        }

        fn trace_ids(&self) -> Vec<String> {
            self.trace_ids.lock().expect("trace ids").clone()
        }
    }

    struct TraceIdVisitor {
        trace_id: Option<String>,
    }

    impl tracing::field::Visit for TraceIdVisitor {
        fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
            if field.name() == "trace_id" {
                self.trace_id = Some(format!("{value:?}"));
            }
        }
    }

    impl<S> tracing_subscriber::Layer<S> for TraceCapture
    where
        S: tracing::Subscriber,
    {
        fn on_new_span(
            &self,
            attrs: &tracing::span::Attributes<'_>,
            _id: &tracing::Id,
            _ctx: tracing_subscriber::layer::Context<'_, S>,
        ) {
            if attrs.metadata().name() != "agent_turn" {
                return;
            }
            let mut visitor = TraceIdVisitor { trace_id: None };
            attrs.record(&mut visitor);
            if let Some(trace_id) = visitor.trace_id {
                self.trace_ids.lock().expect("trace ids").push(trace_id);
            }
        }
    }

    #[test]
    fn record_turn_observation_uses_surface_trace_id_for_failures() {
        // Arrange
        let status = RuntimeStatus::new();
        let mut context = crate::test_util::cli_context("trace-id");
        context.trace_id = "trace-123".to_string();
        let result = Err(EgoPulseError::Internal("turn failed".to_string()));

        // Act
        record_turn_observation(&status, &context, "2026-08-09T00:00:00Z", 0.5, &result);

        // Assert
        let error = status.recent_errors().pop().expect("recorded error");
        assert_eq!(error.trace_id, "trace-123");
        assert_eq!(error.error_kind, "turn_failure");
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn execute_scheduled_turn_generates_trace_id() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let state = crate::test_util::build_state_with_provider(
            dir.path().to_str().expect("utf8"),
            Box::new(final_provider()),
        );
        let context = crate::test_util::cli_context("sched-trace");
        assert!(context.trace_id.is_empty());
        let capture = TraceCapture::new();
        let _guard =
            tracing::subscriber::set_default(tracing_subscriber::registry().with(capture.clone()));
        let turn = crate::runtime::turn::ScheduledTurn {
            turn_id: "turn-1".to_string(),
            context,
            input: "scheduled turn".to_string(),
            origin_id: uuid::Uuid::new_v4().to_string(),
            config_snapshot: None,
        };

        // Act
        execute_scheduled_turn(&state, turn).await;

        // Assert
        let trace_ids = capture.trace_ids();
        assert_eq!(trace_ids.len(), 1, "one agent_turn span should be created");
        assert!(!trace_ids[0].is_empty(), "trace_id must be generated");
    }

    #[test]
    fn tool_progress_enabled_reads_channel_config_flag() {
        use crate::config::{ChannelConfig, ChannelName, DiscordChannelConfig};
        use crate::conversation::SurfaceContext;
        use std::collections::HashMap;

        // Arrange: discord channel 123 has tool_progress on, 456 off
        let dir = tempfile::tempdir().expect("tempdir");
        let mut config = crate::test_util::test_config(dir.path().to_str().expect("utf8"));
        let mut discord_channels = HashMap::new();
        discord_channels.insert(
            123u64,
            DiscordChannelConfig {
                tool_progress: true,
                ..Default::default()
            },
        );
        discord_channels.insert(
            456u64,
            DiscordChannelConfig {
                tool_progress: false,
                ..Default::default()
            },
        );
        config.channels.insert(
            ChannelName::new("discord"),
            ChannelConfig {
                discord_channels: Some(discord_channels),
                ..Default::default()
            },
        );

        let ctx = |thread: &str| {
            SurfaceContext::new(
                "discord".to_string(),
                "user".to_string(),
                thread.to_string(),
                "discord".to_string(),
                "lyre".to_string(),
            )
        };

        // Act + Assert
        assert!(
            tool_progress_enabled(&config, &ctx("123")),
            "channel 123 enabled"
        );
        assert!(
            !tool_progress_enabled(&config, &ctx("456")),
            "channel 456 disabled"
        );
        assert!(
            !tool_progress_enabled(&config, &ctx("999")),
            "unknown channel disabled"
        );
        let web_ctx = SurfaceContext::new(
            "web".to_string(),
            "user".to_string(),
            "session".to_string(),
            "web".to_string(),
            "lyre".to_string(),
        );
        assert!(
            !tool_progress_enabled(&config, &web_ctx),
            "web never enabled"
        );
    }

    #[tokio::test]
    async fn execute_turn_with_progress_terminates_on_success_and_failure() {
        // A coordinator without a sink must terminate for both result paths;
        // the failure path is important because it must also close the event
        // stream.
        let success_dir = tempfile::tempdir().expect("tempdir");
        let success_state = crate::test_util::build_state_with_provider(
            success_dir.path().to_str().expect("utf8"),
            Box::new(final_provider()),
        );
        let success_context = crate::test_util::cli_context("progress-success");
        let success_result = tokio::time::timeout(
            Duration::from_secs(10),
            execute_turn_with_progress_and_snapshot(
                &success_state,
                &success_context,
                "hello",
                success_state.config_manager.current_blocking(),
            ),
        )
        .await;
        assert!(success_result.is_ok(), "success path must not hang");
        assert_eq!(success_result.unwrap().expect("turn ok"), "ok");

        let failure_dir = tempfile::tempdir().expect("tempdir");
        let failure_provider = RecordingProvider::new(
            vec![Err(crate::error::LlmError::InvalidResponse(
                "stub failure".to_string(),
            ))],
            vec![0],
        );
        let failure_state = crate::test_util::build_state_with_provider(
            failure_dir.path().to_str().expect("utf8"),
            Box::new(failure_provider),
        );
        let failure_context = crate::test_util::cli_context("progress-failure");
        let failure_result = tokio::time::timeout(
            Duration::from_secs(10),
            execute_turn_with_progress_and_snapshot(
                &failure_state,
                &failure_context,
                "hello",
                failure_state.config_manager.current_blocking(),
            ),
        )
        .await;
        assert!(failure_result.is_ok(), "failure path must not hang");
        assert!(failure_result.unwrap().is_err(), "turn should fail");
    }

    #[tokio::test]
    async fn retryable_llm_failure_executes_one_turn_and_persists_one_input() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let retry_count = crate::agent_loop::model_step::MAX_LLM_RETRIES;
        let retry_provider = RecordingProvider::new(
            (0..retry_count)
                .map(|_| {
                    Err(crate::error::LlmError::ApiError {
                        status: reqwest::StatusCode::INTERNAL_SERVER_ERROR,
                        body_preview: "test failure".to_string(),
                        retry_after_secs: None,
                    })
                })
                .collect(),
            vec![0; retry_count],
        );
        let retry_observer = retry_provider.clone();
        let state = crate::test_util::build_state_with_provider(
            dir.path().to_str().expect("utf8"),
            Box::new(retry_provider),
        );
        let context = crate::test_util::cli_context("retryable-failure");

        // Act
        let result = execute_turn_with_progress_and_snapshot(
            &state,
            &context,
            "hello",
            state.config_manager.current_blocking(),
        )
        .await;

        // Assert: the same model iteration is retried up to
        // `MAX_LLM_RETRIES` before surfacing the error, but still executes a
        // single Turn with a single persisted input.
        assert!(result.is_err(), "retryable failure must reach the caller");
        assert_eq!(
            retry_observer.seen_systems().len(),
            retry_count,
            "LLM must be retried up to MAX_LLM_RETRIES within the same iteration"
        );
        let conn = state.db.get_conn().expect("connection");
        let user_messages: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM messages WHERE sender_kind = 'user'",
                [],
                |row| row.get(0),
            )
            .expect("count user messages");
        assert_eq!(user_messages, 1, "input must be persisted once");
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn scheduled_turn_logs_route_by_conversation_scope() {
        use crate::llm::MessagesResponse;
        use crate::runtime::turn::ScheduledTurn;
        use crate::storage::call_blocking;

        // Arrange: state with secret DB + recording provider
        let dir = tempfile::tempdir().expect("tempdir");
        let provider = RecordingProvider::new(
            vec![Ok(MessagesResponse {
                content: "secret scheduled reply".to_string(),
                reasoning_content: None,
                tool_calls: Vec::new(),
                usage: None,
            })],
            vec![0],
        );
        let mut state = crate::test_util::build_state_with_provider(
            dir.path().to_str().expect("utf8"),
            Box::new(provider),
        );
        let secret_path = dir.path().join("runtime").join("secret.db");
        state.secret_db = Some(Arc::new(
            Database::new_secret(&secret_path).expect("secret db"),
        ));

        let log_chat_id: i64 = 9999;
        let mut context = crate::test_util::cli_context("scheduled-secret-routing");
        context.scope = ConversationScope::Secret;
        context.channel_log_chat_id = Some(log_chat_id);

        let turn = ScheduledTurn {
            turn_id: "turn-1".to_string(),
            context,
            input: "scheduled secret input".to_string(),
            origin_id: uuid::Uuid::new_v4().to_string(),
            config_snapshot: None,
        };

        // Act: execute the scheduled turn
        execute_scheduled_turn(&state, turn).await;

        // Assert: secret DB has the bot response
        let secret_messages = call_blocking(
            Arc::clone(state.secret_db.as_ref().expect("secret db")),
            move |db| db.get_recent_messages(log_chat_id, 10),
        )
        .await
        .expect("read secret channel log");
        let secret_has_reply = secret_messages
            .iter()
            .any(|m| m.content.contains("secret scheduled reply"));
        assert!(
            secret_has_reply,
            "secret DB should contain the bot response"
        );

        // Assert: normal DB has no entries from this turn
        let normal_messages = call_blocking(Arc::clone(&state.db), move |db| {
            db.get_recent_messages(log_chat_id, 10)
        })
        .await
        .expect("read normal channel log");
        let normal_has_reply = normal_messages
            .iter()
            .any(|m| m.content.contains("secret scheduled reply"));
        assert!(
            !normal_has_reply,
            "normal DB should not contain the secret bot response"
        );
    }

    #[tokio::test]
    async fn recover_durable_state_fails_closed_on_storage_error() {
        use crate::conversation::ConversationScope;

        let dir = tempfile::tempdir().expect("tempdir");
        let state = Arc::new(crate::test_util::build_state_with_provider(
            dir.path().to_str().expect("utf8"),
            Box::new(final_provider()),
        ));
        // Inject a permanent storage fault so every recovery read fails.
        state
            .db_for(ConversationScope::Normal)
            .fault_inject_next_get_conn(u32::MAX);
        let result = recover_durable_state(&state).await;
        assert!(
            result.is_err(),
            "startup recovery must abort (fail-closed) on storage failure"
        );
    }

    #[tokio::test]
    async fn persist_turn_cancellation_retries_until_db_recovers() {
        use crate::conversation::ConversationScope;

        let dir = tempfile::tempdir().expect("tempdir");
        let state = crate::test_util::build_state_with_provider(
            dir.path().to_str().expect("utf8"),
            Box::new(final_provider()),
        );
        // Two transient failures, then recovery. The retry loop must keep
        // trying until the DB heals, then succeed.
        state
            .db_for(ConversationScope::Normal)
            .fault_inject_next_get_conn(2);
        let result = persist_turn_cancellation(
            &state,
            ConversationScope::Normal,
            "turn-fix3",
            "turn_count_exceeded",
            "test cancellation",
        )
        .await;
        assert!(
            result.is_ok(),
            "cancellation must succeed after DB recovers (retry with backoff)"
        );
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn db_failure_during_dispatch_retries_and_preserves_turn_order() {
        use crate::conversation::ConversationScope;
        use crate::llm::MessagesResponse;
        use crate::runtime::turn::ScheduleResult;
        use crate::runtime::turn::ScheduledTurn;

        let dir = tempfile::tempdir().expect("tempdir");
        let provider = RecordingProvider::new(
            vec![
                Ok(MessagesResponse {
                    content: "reply-a".to_string(),
                    reasoning_content: None,
                    tool_calls: Vec::new(),
                    usage: None,
                }),
                Ok(MessagesResponse {
                    content: "reply-b".to_string(),
                    reasoning_content: None,
                    tool_calls: Vec::new(),
                    usage: None,
                }),
            ],
            vec![0, 0],
        );
        let state = Arc::new(crate::test_util::build_state_with_provider(
            dir.path().to_str().expect("utf8"),
            Box::new(provider.clone()),
        ));

        // Two turns for the same session: A first, B queued behind it.
        let mut ctx = crate::test_util::cli_context("fix1-session");
        ctx.origin_id = uuid::Uuid::new_v4().to_string();
        let turn_a = ScheduledTurn {
            turn_id: "A".to_string(),
            context: ctx.clone(),
            input: "a".to_string(),
            origin_id: ctx.origin_id.clone(),
            config_snapshot: None,
        };
        let turn_b = ScheduledTurn {
            turn_id: "B".to_string(),
            context: ctx.clone(),
            input: "b".to_string(),
            origin_id: ctx.origin_id.clone(),
            config_snapshot: None,
        };
        assert!(
            matches!(
                state.turn_scheduler.submit(turn_a.clone()),
                ScheduleResult::Started(_)
            ),
            "A must start immediately"
        );
        assert!(
            matches!(
                state.turn_scheduler.submit(turn_b.clone()),
                ScheduleResult::Enqueued
            ),
            "B must be queued behind A"
        );

        // Three transient failures during the state lookup, then recovery.
        // The retry loop must keep retrying until the DB heals; B must NOT
        // start while A is blocked.
        state
            .db_for(ConversationScope::Normal)
            .fault_inject_next_get_conn(3);

        execute_scheduled_turn(&state, turn_a).await;

        // After DB recovery, A executed and then B was drained and executed —
        // in order. Both turns ran (2 send_message calls), and A's input was
        // seen first.
        let seen = provider.seen_messages();
        assert_eq!(seen.len(), 2, "both A and B must execute after DB recovery");
        assert_eq!(
            state.turn_scheduler.global_queued(),
            0,
            "turn B must have been drained after A completed"
        );
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn dispatch_re_scans_capacity_rejected_turn_after_capacity_frees() {
        use crate::conversation::ConversationScope;
        use crate::runtime::channel_input;
        use crate::runtime::turn::ScheduledTurn;
        use crate::runtime::turn::SubmitOutcome;
        use crate::storage::call_blocking;

        let dir = tempfile::tempdir().expect("tempdir");
        let state = Arc::new(crate::test_util::build_state_with_provider(
            dir.path().to_str().expect("utf8"),
            Box::new(final_provider()),
        ));
        state.supervisor.start_accepting();

        let session_key = "cli:session-blk1:agent:default";
        let max_queued = crate::runtime::turn::scheduler::MAX_QUEUED_TURNS_PER_SESSION;
        // Fill the per-session queue: 1 started + max_queued queued.
        for i in 0..=max_queued {
            let mut ctx = crate::test_util::cli_context("session-blk1");
            ctx.origin_id = format!("fill-{i}");
            let turn = ScheduledTurn {
                turn_id: format!("fill-{i}"),
                context: ctx,
                input: format!("fill-{i}"),
                origin_id: format!("fill-{i}"),
                config_snapshot: None,
            };
            let _ = state.turn_scheduler.submit(turn);
        }
        assert_eq!(
            state.turn_scheduler.global_queued(),
            max_queued,
            "pre-fill queue"
        );

        // Durably accept turn A for the same (full) session: the scheduler
        // rejects it (SessionQueueFull) and it stays `accepted` in the DB.
        let mut ctx_a = crate::test_util::cli_context("session-blk1");
        ctx_a.origin_id = "origin-blk1".to_string();
        let turn_a = ScheduledTurn {
            turn_id: String::new(),
            context: ctx_a,
            input: "blk1-input".to_string(),
            origin_id: "origin-blk1".to_string(),
            config_snapshot: None,
        };
        let outcome = channel_input::submit_scheduled_turn(&state, turn_a).await;
        assert!(
            matches!(outcome, SubmitOutcome::Queued),
            "turn A must be deferred (session queue full)"
        );

        // Tick 1: dispatch scans A but the scheduler still rejects it.
        dispatch_durable_turns(&state)
            .await
            .expect("dispatch tick 1");
        let count = call_blocking(state.db_for(ConversationScope::Normal), |db| {
            db.count_durable_pending()
        })
        .await
        .expect("count");
        assert_eq!(
            count, 1,
            "turn A must still be durable-pending after tick 1"
        );
        assert_eq!(
            state.turn_scheduler.global_queued(),
            max_queued,
            "scheduler queue must be unchanged (A was rejected again)"
        );

        // Free all capacity: drain the pre-filled turns.
        for _ in 0..=max_queued {
            let _ = state.turn_scheduler.on_turn_completed(session_key);
        }

        // Tick 2: dispatch scans from the head and re-dispatches A. The session
        // is now idle so A is Started and execution proceeds.
        dispatch_durable_turns(&state)
            .await
            .expect("dispatch tick 2");
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;

        let count = call_blocking(state.db_for(ConversationScope::Normal), |db| {
            db.count_durable_pending()
        })
        .await
        .expect("count");
        assert_eq!(
            count, 0,
            "turn A must have been dispatched and executed on tick 2"
        );
    }

    #[tokio::test]
    async fn persist_origin_terminal_reason_retries_until_db_recovers() {
        use crate::conversation::ConversationScope;
        use crate::runtime::turn::StopReason;

        let dir = tempfile::tempdir().expect("tempdir");
        let state = Arc::new(crate::test_util::build_state_with_provider(
            dir.path().to_str().expect("utf8"),
            Box::new(final_provider()),
        ));
        state
            .db_for(ConversationScope::Normal)
            .fault_inject_next_get_conn(2);
        let result = persist_origin_terminal_reason(
            &state,
            ConversationScope::Normal,
            "origin-blk2",
            StopReason::LlmFailure,
        )
        .await;
        assert!(
            result.is_ok(),
            "terminal reason persist must succeed after DB recovers"
        );
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn permanent_db_failure_keeps_session_blocked() {
        use crate::conversation::ConversationScope;
        use crate::runtime::turn::ScheduleResult;
        use crate::runtime::turn::ScheduledTurn;

        let dir = tempfile::tempdir().expect("tempdir");
        let provider = crate::agent_loop::test_support::RecordingProvider::new(
            vec![Ok(crate::llm::MessagesResponse {
                content: "never".to_string(),
                reasoning_content: None,
                tool_calls: Vec::new(),
                usage: None,
            })],
            vec![0],
        );
        let state = crate::test_util::build_state_with_provider(
            dir.path().to_str().expect("utf8"),
            Box::new(provider.clone()),
        );

        let mut ctx = crate::test_util::cli_context("major-session");
        ctx.origin_id = uuid::Uuid::new_v4().to_string();
        let turn_a = ScheduledTurn {
            turn_id: "A".to_string(),
            context: ctx.clone(),
            input: "a".to_string(),
            origin_id: ctx.origin_id.clone(),
            config_snapshot: None,
        };
        let turn_b = ScheduledTurn {
            turn_id: "B".to_string(),
            context: ctx.clone(),
            input: "b".to_string(),
            origin_id: ctx.origin_id.clone(),
            config_snapshot: None,
        };
        assert!(matches!(
            state.turn_scheduler.submit(turn_a.clone()),
            ScheduleResult::Started(_)
        ));
        assert!(matches!(
            state.turn_scheduler.submit(turn_b.clone()),
            ScheduleResult::Enqueued
        ));

        // Permanent DB failure: the retry loop must keep the session blocked.
        // B must NOT be drained.
        state
            .db_for(ConversationScope::Normal)
            .fault_inject_next_get_conn(u32::MAX);

        let timed_out = tokio::time::timeout(
            std::time::Duration::from_millis(300),
            execute_scheduled_turn(&state, turn_a),
        )
        .await;
        assert!(
            timed_out.is_err(),
            "turn A must stay blocked under permanent DB failure"
        );
        assert!(
            provider.seen_messages().is_empty(),
            "no turn must execute under permanent DB failure"
        );
        assert_eq!(
            state.turn_scheduler.global_queued(),
            1,
            "turn B must remain queued (session blocked)"
        );
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn dispatch_invalid_durable_payloads_terminalizes_without_execution() {
        use crate::llm::MessagesResponse;
        use crate::storage::{AcceptOutcome, AcceptTurnParams};

        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let provider = RecordingProvider::new(
            vec![Ok(MessagesResponse {
                content: "must not run".to_string(),
                reasoning_content: None,
                tool_calls: Vec::new(),
                usage: None,
            })],
            vec![0],
        );
        let state = Arc::new(crate::test_util::build_state_with_provider(
            dir.path().to_str().expect("utf8"),
            Box::new(provider.clone()),
        ));
        let chat_id = call_blocking(Arc::clone(&state.db), |db| {
            db.resolve_or_create_chat_id("cli", "invalid-payloads", None, "cli", "default")
        })
        .await
        .expect("create chat");
        let rows = call_blocking(Arc::clone(&state.db), move |db| {
            let mut turn_ids = Vec::new();
            for (request_key, payload, origin_id) in [
                ("malformed", "{malformed", Some("invalid-origin-malformed")),
                (
                    "unsupported",
                    r#"{"version":999}"#,
                    Some("invalid-origin-version"),
                ),
            ] {
                let run = match db.accept_or_get_turn(AcceptTurnParams {
                    chat_id,
                    request_key,
                    config_revision: 1,
                    config_fingerprint: Some("fp"),
                    request_payload_hash: "hash",
                    origin_id,
                    scheduled_request_json: Some(payload),
                })? {
                    AcceptOutcome::Created(run) => run,
                    AcceptOutcome::Existing(_) => panic!("expected created"),
                };
                turn_ids.push(run.turn_id);
            }
            Ok(turn_ids)
        })
        .await
        .expect("accept invalid turns");

        // Act
        dispatch_durable_turns(&state)
            .await
            .expect("dispatch invalid turns");

        // Assert
        for turn_id in rows {
            let run = call_blocking(Arc::clone(&state.db), move |db| db.get_turn_run(&turn_id))
                .await
                .expect("get failed turn");
            assert_eq!(run.state, TurnRunState::Failed);
            assert_eq!(
                run.error_kind.as_deref(),
                Some(DURABLE_PAYLOAD_INVALID_ERROR_KIND)
            );
            assert_eq!(
                run.error_message.as_deref(),
                Some("scheduled durable turn payload is invalid")
            );
            assert!(
                !run.error_message
                    .as_deref()
                    .unwrap_or_default()
                    .contains("malformed")
            );
        }
        let pending = call_blocking(Arc::clone(&state.db), |db| db.count_durable_pending())
            .await
            .expect("count pending");
        assert_eq!(pending, 0);
        assert!(provider.seen_messages().is_empty());
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn dispatch_retries_invalid_payload_after_storage_read_failure() {
        use crate::storage::{AcceptOutcome, AcceptTurnParams};

        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let state = Arc::new(crate::test_util::build_state_with_provider(
            dir.path().to_str().expect("utf8"),
            Box::new(final_provider()),
        ));
        let turn_id = call_blocking(Arc::clone(&state.db), |db| {
            let chat_id = db.resolve_or_create_chat_id(
                "cli",
                "invalid-payload-retry",
                None,
                "cli",
                "default",
            )?;
            match db.accept_or_get_turn(AcceptTurnParams {
                chat_id,
                request_key: "invalid-retry",
                config_revision: 1,
                config_fingerprint: Some("fp"),
                request_payload_hash: "hash",
                origin_id: None,
                scheduled_request_json: Some("not-json"),
            })? {
                AcceptOutcome::Created(run) => Ok(run.turn_id),
                AcceptOutcome::Existing(_) => panic!("expected created"),
            }
        })
        .await
        .expect("accept invalid turn");
        // The dispatcher tolerates a backlog-gauge read failure and continues
        // scanning, so fail both the gauge read and the pending-row scan.
        state.db.fault_inject_next_get_conn(2);

        // Act: the first tick cannot read the database; the next tick retries
        // the still-pending row and can terminalize it.
        let first = dispatch_durable_turns(&state).await;
        assert!(first.is_err(), "storage read failure must reach dispatcher");
        let pending_after_failure = call_blocking(Arc::clone(&state.db), {
            let turn_id = turn_id.clone();
            move |db| db.get_turn_run(&turn_id).map(|run| run.state)
        })
        .await
        .expect("get pending turn");
        assert_eq!(pending_after_failure, TurnRunState::Accepted);
        dispatch_durable_turns(&state)
            .await
            .expect("retry invalid turn");

        // Assert
        let failed = call_blocking(Arc::clone(&state.db), move |db| db.get_turn_run(&turn_id))
            .await
            .expect("get failed turn");
        assert_eq!(failed.state, TurnRunState::Failed);
        assert_eq!(
            failed.error_kind.as_deref(),
            Some(DURABLE_PAYLOAD_INVALID_ERROR_KIND)
        );
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn resume_input_committed_turn_restarts_model_loop() {
        use crate::agent_loop::resume_input_committed_turn;
        use crate::llm::MessagesResponse;
        use crate::runtime::turn::ScheduledTurn;
        use crate::runtime::turn::serialize_scheduled_turn;
        use crate::storage::{AcceptOutcome, StoredMessage, TurnRunState};

        let dir = tempfile::tempdir().expect("tempdir");
        let provider = RecordingProvider::new(
            vec![Ok(MessagesResponse {
                content: "resumed reply".to_string(),
                reasoning_content: None,
                tool_calls: Vec::new(),
                usage: None,
            })],
            vec![0],
        );
        let state = crate::test_util::build_state_with_provider(
            dir.path().to_str().expect("utf8"),
            Box::new(provider),
        );
        let runtime = state.turn_dependencies();

        let mut context = crate::test_util::cli_context("resume-input-committed");
        context.scope = ConversationScope::Normal;
        let chat_id = crate::agent_loop::resolve_chat_id(&runtime, &context)
            .await
            .expect("resolve chat");

        let input = "resume input".to_string();
        let scheduled = ScheduledTurn {
            turn_id: String::new(),
            context: context.clone(),
            input: input.clone(),
            origin_id: uuid::Uuid::new_v4().to_string(),
            config_snapshot: None,
        };
        let scheduled_json = serialize_scheduled_turn(&scheduled).expect("serialize");

        let accepted = call_blocking(state.db_for(ConversationScope::Normal), {
            let scheduled_json = scheduled_json.clone();
            move |db| {
                db.accept_or_get_turn(crate::storage::AcceptTurnParams {
                    chat_id,
                    request_key: "resume-k",
                    config_revision: 1,
                    config_fingerprint: None,
                    request_payload_hash: "h",
                    origin_id: None,
                    scheduled_request_json: Some(&scheduled_json),
                })
            }
        })
        .await
        .expect("accept");
        let turn_id = match accepted {
            AcceptOutcome::Created(run) => run.turn_id,
            _ => panic!("expected created"),
        };

        // Drive the turn to input_committed with the deterministic input message
        // id the resume path validates (`turn:{id}:input`).
        let mut msg = StoredMessage::user(chat_id, "sender".to_string(), input.clone());
        msg.id = format!("turn:{turn_id}:input");
        msg.turn_id = Some(turn_id.clone());
        call_blocking(state.db_for(ConversationScope::Normal), {
            let msg = msg.clone();
            let turn_id = turn_id.clone();
            move |db| db.commit_turn_input_with_conversation(&msg, "[]", None, &turn_id, 0, None)
        })
        .await
        .expect("commit input");

        assert_eq!(
            call_blocking(state.db_for(ConversationScope::Normal), {
                let turn_id = turn_id.clone();
                move |db| db.get_turn_run(&turn_id).map(|r| r.state)
            })
            .await
            .expect("get state"),
            TurnRunState::InputCommitted
        );

        // Act: resume the input_committed turn.
        let result = resume_input_committed_turn(
            &runtime,
            ConversationScope::Normal,
            &turn_id,
            state.config_manager.current_blocking(),
        )
        .await;
        assert!(result.is_ok(), "resume should succeed: {result:?}");

        // Assert: the model loop ran to completion and the turn is terminal.
        assert_eq!(
            call_blocking(state.db_for(ConversationScope::Normal), {
                let turn_id = turn_id.clone();
                move |db| db.get_turn_run(&turn_id).map(|r| r.state)
            })
            .await
            .expect("get final state"),
            TurnRunState::Completed
        );
    }
}
