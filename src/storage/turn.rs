//! Durable Turn lifecycle persistence (`turn_runs`) and the Turn state machine.
//!
//! Every Turn's lifecycle is persisted into the `turn_runs` table so that:
//!
//! * the same `chat_id + request_key` is accepted exactly once (idempotent
//!   ingress),
//! * the Turn's current state is queryable from the DB,
//! * state transitions are validated against a central rule
//!   ([`crate::storage::TurnRunState::can_transition`]) — no free-string
//!   updates,
//! * a `completed` Turn returns its saved final result without re-invoking
//!   the LLM,
//! * interrupted Turns are recovered to a safe stop (`uncertain`/`failed`)
//!   on startup rather than auto-resumed.
//!
//! Config revision / fingerprint are stored as given by the caller. A `NULL`
//! `config_fingerprint` means the Config identity was not captured at
//! acceptance time; recovery treats it as "cannot verify config integrity"
//! and conservatively stops.

use rusqlite::OptionalExtension;
use rusqlite::TransactionBehavior;
use rusqlite::params;

use crate::error::StorageError;
use crate::storage::{
    Database, DurablePendingTurn, MAX_DURABLE_PENDING_PER_SCOPE, MAX_DURABLE_PENDING_PER_SESSION,
    StoredMessage, TurnRunState,
};

/// One row of `turn_runs`, read back for lifecycle decisions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TurnRun {
    pub turn_id: String,
    pub chat_id: i64,
    pub request_key: String,
    pub state: TurnRunState,
    pub current_iteration: i64,
    pub input_message_id: Option<String>,
    pub final_message_id: Option<String>,
    pub config_revision: i64,
    pub config_fingerprint: Option<String>,
    pub model_request_hash: Option<String>,
    pub model_attempt: i64,
    pub output_published: bool,
    pub error_kind: Option<String>,
    pub error_message: Option<String>,
    pub accepted_at: String,
    pub updated_at: String,
    pub finished_at: Option<String>,
    pub request_payload_hash: Option<String>,
    /// Serialized accepted request ([`crate::agent_loop::PersistedScheduledTurnV1`]),
    /// present once the turn has been durably accepted. Lets a restarted runtime
    /// rebuild the `SurfaceContext` without re-delivering the platform event.
    pub scheduled_request_json: Option<String>,
    /// Origin chain id tracking every turn caused by one human input
    /// (`agent_send` cascade). Equals `turn_id` for a root turn.
    pub origin_id: Option<String>,
}

/// Outcome of [`Database::accept_or_get_turn`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AcceptOutcome {
    /// A fresh `accepted` Turn was created and may proceed.
    Created(TurnRun),
    /// A Turn for the same `chat_id + request_key` already existed. The caller
    /// inspects its state to decide whether to reuse, resume, or refuse.
    Existing(TurnRun),
}

/// One Turn recovered by [`Database::recover_interrupted_turns`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RecoveredTurnRun {
    pub turn_id: String,
    pub chat_id: i64,
    pub from: TurnRunState,
    pub recovered_to: TurnRunState,
}

/// Per-origin execution count rehydrated from `turn_runs` after a restart.
/// The in-memory [`crate::runtime::turn_scheduler::TurnTracker`] is
/// rebuilt from these so a chain that already consumed turns before a crash
/// keeps its per-chain turn limit instead of resetting to zero. `terminal_reason`
/// (when present) is the durable terminal stop reason restored from
/// `turn_origins`, so a terminated chain is not silently resumed after a restart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RecoveredOrigin {
    pub origin_id: String,
    pub executed_turn_count: usize,
    /// Durable terminal stop reason restored from `turn_origins`, if the chain
    /// terminated before the crash. Stored as the canonical `Display` string and
    /// parsed back by the tracker on rehydration.
    pub terminal_reason: Option<String>,
}

const TURN_RUN_COLUMNS: &str = "turn_id, chat_id, request_key, state, current_iteration,
    input_message_id, final_message_id, config_revision,
    config_fingerprint, model_request_hash, model_attempt,
    output_published, error_kind, error_message, accepted_at,
    updated_at, finished_at, request_payload_hash,
    scheduled_request_json, origin_id";

/// Loads and parses the current state of a Turn inside a transaction.
/// Returns [`StorageError::NotFound`] when the row is missing, or
/// [`StorageError::Conflict`] when the persisted state string is invalid.
fn read_state_locked(
    tx: &rusqlite::Transaction<'_>,
    turn_id: &str,
) -> Result<TurnRunState, StorageError> {
    let state_str: String = tx
        .query_row(
            "SELECT state FROM turn_runs WHERE turn_id = ?1",
            params![turn_id],
            |row| row.get(0),
        )
        .optional()?
        .ok_or_else(|| StorageError::NotFound(format!("turn_run:{turn_id}")))?;
    state_str
        .parse()
        .map_err(|e| StorageError::Conflict(format!("invalid turn_runs.state: {e}")))
}

fn transition_locked(
    tx: &rusqlite::Transaction<'_>,
    turn_id: &str,
    from: TurnRunState,
    to: TurnRunState,
) -> Result<(), StorageError> {
    let current = read_state_locked(tx, turn_id)?;
    if current != from {
        return Err(StorageError::Conflict(format!(
            "turn state transition rejected: expected {from} but was {current}"
        )));
    }
    if !TurnRunState::can_transition(from, to) {
        return Err(StorageError::Conflict(format!(
            "turn state transition rejected: {from} -> {to}"
        )));
    }
    let now = chrono::Utc::now().to_rfc3339();
    tx.execute(
        "UPDATE turn_runs SET state = ?2, updated_at = ?3 WHERE turn_id = ?1",
        params![turn_id, to.to_string(), &now],
    )?;
    Ok(())
}

fn require_state_locked(
    tx: &rusqlite::Transaction<'_>,
    turn_id: &str,
    expected: TurnRunState,
) -> Result<(), StorageError> {
    let state = read_state_locked(tx, turn_id)?;
    if state != expected {
        return Err(StorageError::Conflict(format!(
            "turn state transition rejected: expected {expected} but was {state}"
        )));
    }
    Ok(())
}

fn read_turn_run(
    handle: &rusqlite::Connection,
    sql: &str,
    params: impl rusqlite::Params,
) -> Result<Option<TurnRun>, StorageError> {
    let row = handle
        .query_row(sql, params, |row| {
            let state_str: String = row.get(3)?;
            let state: TurnRunState = state_str.parse().map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    3,
                    rusqlite::types::Type::Text,
                    Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, e)),
                )
            })?;
            Ok(TurnRun {
                turn_id: row.get(0)?,
                chat_id: row.get(1)?,
                request_key: row.get(2)?,
                state,
                current_iteration: row.get(4)?,
                input_message_id: row.get(5)?,
                final_message_id: row.get(6)?,
                config_revision: row.get(7)?,
                config_fingerprint: row.get(8)?,
                model_request_hash: row.get(9)?,
                model_attempt: row.get(10)?,
                output_published: row.get::<_, i64>(11)? != 0,
                error_kind: row.get(12)?,
                error_message: row.get(13)?,
                accepted_at: row.get(14)?,
                updated_at: row.get(15)?,
                finished_at: row.get(16)?,
                request_payload_hash: row.get(17)?,
                scheduled_request_json: row.get(18)?,
                origin_id: row.get(19)?,
            })
        })
        .optional()?;
    Ok(row)
}

impl Database {
    /// Creates a new `accepted` Turn for `(chat_id, request_key)`, or returns
    /// the existing row if the same key was already accepted.
    ///
    /// The `UNIQUE(chat_id, request_key)` constraint is the idempotency guard:
    /// a re-delivered platform message (same request_key) maps to the same
    /// Turn instead of spawning a duplicate. The caller branches on
    /// [`AcceptOutcome`] — `Created` proceeds with the normal flow, `Existing`
    /// reuses / resumes / refuses based on the persisted state.
    ///
    /// `config_revision` / `config_fingerprint` are fixed at acceptance so a
    /// later recovery can detect a Config generation mismatch. A `0` /
    /// `None` pair is accepted when the Config identity is not yet known at
    /// acceptance time.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if the underlying SQLite write fails.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn accept_or_get_turn(
        &self,
        chat_id: i64,
        request_key: &str,
        config_revision: i64,
        config_fingerprint: Option<&str>,
        request_payload_hash: &str,
        origin_id: Option<&str>,
        scheduled_request_json: Option<&str>,
    ) -> Result<AcceptOutcome, StorageError> {
        let mut conn = self.get_conn()?;
        // BEGIN IMMEDIATE acquires the write lock up front so the capacity
        // count and the INSERT are observed atomically by other acceptors:
        // no concurrent accept can pass the same limit and overshoot.
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = chrono::Utc::now().to_rfc3339();
        let proposed_turn_id = uuid::Uuid::new_v4().to_string();
        // A root turn (no incoming origin) uses its own turn_id as the origin so
        // every turn in the chain shares one identity.
        let persisted_origin_id = origin_id
            .filter(|s| !s.is_empty())
            .unwrap_or(&proposed_turn_id);

        // Durable-pending capacity gate (per-session then runtime-wide),
        // evaluated inside the same transaction that will perform the INSERT.
        // A rejection here surfaces as a 429 to the caller and leaves no row
        // behind. Existing re-deliveries (the ON CONFLICT DO NOTHING path below)
        // are not "new" pending load, so they must bypass the cap: only count
        // when the request_key is not already present for this chat.
        let already_present: i64 = tx.query_row(
            "SELECT COUNT(*) FROM turn_runs WHERE chat_id = ?1 AND request_key = ?2",
            params![chat_id, request_key],
            |row| row.get(0),
        )?;
        if already_present == 0 {
            let session_pending: i64 = tx.query_row(
                "SELECT COUNT(*) FROM turn_runs
                 WHERE chat_id = ?1 AND state IN ('accepted', 'input_committed')",
                params![chat_id],
                |row| row.get(0),
            )?;
            if session_pending >= MAX_DURABLE_PENDING_PER_SESSION {
                return Err(StorageError::TurnSessionQueueFull);
            }
            let global_pending: i64 = tx.query_row(
                "SELECT COUNT(*) FROM turn_runs
                 WHERE state IN ('accepted', 'input_committed')",
                [],
                |row| row.get(0),
            )?;
            if global_pending >= MAX_DURABLE_PENDING_PER_SCOPE {
                return Err(StorageError::TurnGlobalQueueFull);
            }
        }

        tx.execute(
            "INSERT INTO turn_runs
                 (turn_id, chat_id, request_key, state, config_revision,
                  config_fingerprint, request_payload_hash, accepted_at, updated_at,
                  scheduled_request_json, origin_id)
              VALUES (?1, ?2, ?3, 'accepted', ?4, ?5, ?6, ?7, ?7, ?8, ?9)
              ON CONFLICT(chat_id, request_key) DO NOTHING",
            params![
                &proposed_turn_id,
                chat_id,
                request_key,
                config_revision,
                config_fingerprint,
                request_payload_hash,
                &now,
                scheduled_request_json,
                persisted_origin_id,
            ],
        )?;

        let run = read_turn_run(
            &tx,
            &format!(
                "SELECT {TURN_RUN_COLUMNS}
                 FROM turn_runs
                 WHERE chat_id = ?1 AND request_key = ?2"
            ),
            params![chat_id, request_key],
        )?
        .ok_or_else(|| {
            StorageError::Conflict(format!(
                "turn_runs row vanished after upsert for chat_id={chat_id} request_key={request_key}"
            ))
        })?;

        tx.commit()?;

        // A re-delivery under the same request_key must carry the same input
        // payload. A hash mismatch means the same key was reused for different
        // content — reject rather than silently returning the unrelated Turn.
        // A NULL `request_payload_hash` is legacy data (captured before payload
        // hashing existed); accept it as `Existing` so older rows stay usable.
        let outcome = if run.turn_id == proposed_turn_id {
            AcceptOutcome::Created(run)
        } else if let Some(existing_hash) = run.request_payload_hash.as_deref() {
            if existing_hash != request_payload_hash {
                return Err(StorageError::Conflict(format!(
                    "turn_runs request_payload_hash mismatch for chat_id={chat_id} request_key={request_key}"
                )));
            }
            AcceptOutcome::Existing(run)
        } else {
            AcceptOutcome::Existing(run)
        };
        Ok(outcome)
    }

    /// Loads a Turn by its `turn_id`.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::NotFound`] when no row exists.
    pub(crate) fn get_turn_run(&self, turn_id: &str) -> Result<TurnRun, StorageError> {
        let conn = self.get_conn()?;
        read_turn_run(
            &conn,
            &format!(
                "SELECT {TURN_RUN_COLUMNS}
                 FROM turn_runs
                 WHERE turn_id = ?1"
            ),
            params![turn_id],
        )?
        .ok_or_else(|| StorageError::NotFound(format!("turn_run:{turn_id}")))
    }

    /// Returns the IDs of durably pending Turns whose full request is
    /// persisted in `scheduled_request_json` but never started execution.
    ///
    /// Covers both `accepted` (never started) and `input_committed`
    /// (model loop not yet started) turns. A single query ordered by
    /// `accepted_at` (then `turn_id`) preserves the original acceptance order
    /// across both states, so a restart cannot reverse the turn order of a
    /// session (an `input_committed` turn accepted before an `accepted` turn
    /// is still dispatched first). The dispatcher re-submits freely; the
    /// scheduler deduplicates by `turn_id`, so no grace window is needed.
    ///
    /// Only returns rows whose `(accepted_at, turn_id)` is strictly greater than
    /// the supplied cursor. This lets the dispatcher advance past turns it has
    /// already considered (including busy turns still owned by the in-memory
    /// scheduler) instead of re-reading the same fixed prefix on every 5s tick.
    /// Without it, the head of a large pending set can starve later sessions'
    /// turns (the scheduler dedups already-known turns, but the scan keeps
    /// re-reading them). Rows are ordered by `(accepted_at, turn_id)` so the
    /// cursor preserves acceptance order and never skips a turn permanently: a
    /// turn the scheduler already knows about was processed on a prior tick, and
    /// a genuinely new turn has a later `accepted_at` (after the cursor).
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if the underlying SQLite read fails.
    pub(crate) fn scan_durable_pending_turns_after(
        &self,
        after_accepted_at: &str,
        after_turn_id: &str,
        limit: i64,
    ) -> Result<Vec<DurablePendingTurn>, StorageError> {
        let conn = self.get_conn()?;
        let mut stmt = conn.prepare(
            "SELECT turn_id, accepted_at, scheduled_request_json
              FROM turn_runs
              WHERE (accepted_at, turn_id) > (?1, ?2)
                AND state IN ('accepted', 'input_committed')
                AND scheduled_request_json IS NOT NULL
              ORDER BY accepted_at ASC, turn_id ASC
              LIMIT ?3",
        )?;
        let rows = stmt
            .query_map(params![after_accepted_at, after_turn_id, limit], |row| {
                Ok(DurablePendingTurn {
                    turn_id: row.get(0)?,
                    accepted_at: row.get(1)?,
                    scheduled_request_json: row.get(2)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Total durable-pending turn count (`accepted`/`input_committed`), for
    /// backlog observability. Uses `idx_turn_runs_state`, so it stays cheap
    /// regardless of table size. Unlike [`Self::scan_durable_pending_turns_after`]
    /// this is not batch-limited, so it reflects the true backlog even when it
    /// exceeds the dispatcher batch.
    pub(crate) fn count_durable_pending(&self) -> Result<i64, StorageError> {
        let conn = self.get_conn()?;
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM turn_runs
             WHERE state IN ('accepted', 'input_committed')",
            [],
            |row| row.get(0),
        )?;
        Ok(count)
    }

    /// Validates `from -> to` against the central transition rule and, if
    /// allowed, advances the row. The expected `from` makes the transition
    /// optimistic: a mismatch (concurrent writer or unexpected state) is
    /// rejected rather than silently overwritten.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::NotFound`] when no row exists, or
    /// [`StorageError::Conflict`] when the current state is not `from` or the
    /// transition is not permitted by [`TurnRunState::can_transition`].
    fn transition_turn_state(
        &self,
        turn_id: &str,
        from: TurnRunState,
        to: TurnRunState,
    ) -> Result<(), StorageError> {
        let mut conn = self.get_conn()?;
        let tx = conn.transaction()?;
        transition_locked(&tx, turn_id, from, to)?;
        tx.commit()?;
        Ok(())
    }

    /// Commits the user-input message, advances the session snapshot, and
    /// transitions `turn_runs` from `accepted` to `input_committed` in a single
    /// transaction.
    ///
    /// The user message insert, the `sessions.messages_json` update, the
    /// `chats.revision` / `next_message_seq` bump, the `turn_runs.state`
    /// transition, and the `turn_runs.input_message_id` stamp share one SQLite
    /// transaction so a crash between saving the conversation and recording the
    /// Turn state cannot leave the two out of sync. `message.id` is recorded as
    /// `turn_runs.input_message_id`.
    ///
    /// `expected_revision` is the optimistic-concurrency token for the
    /// conversation: `Some(n)` commits only while `chats.revision == n`, while
    /// `None` requires the session row to not yet exist (initial seed). A
    /// mismatch rolls the whole transaction back with
    /// [`StorageError::SessionSnapshotConflict`].
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if the conversation commit fails, or
    /// [`StorageError::Conflict`] when the Turn is missing or not in the
    /// `accepted` state.
    pub(crate) fn commit_turn_input_with_conversation(
        &self,
        message: &StoredMessage,
        session_json: &str,
        expected_revision: Option<i64>,
        turn_id: &str,
    ) -> Result<i64, StorageError> {
        let mut conn = self.get_conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let outcome = super::chat::commit_message_locked(
            &tx,
            message,
            Some(session_json),
            expected_revision,
        )?;
        transition_locked(
            &tx,
            turn_id,
            TurnRunState::Accepted,
            TurnRunState::InputCommitted,
        )?;
        let now = chrono::Utc::now().to_rfc3339();
        tx.execute(
            "UPDATE turn_runs SET input_message_id = ?2, updated_at = ?3 WHERE turn_id = ?1",
            params![turn_id, &message.id, &now],
        )?;
        tx.commit()?;
        Ok(outcome.revision)
    }

    /// Marks that any external output (delta, narration, Tool Call, assistant
    /// message) has been published for this Turn. Idempotent: setting it again
    /// is a no-op. Once set, a later failure must route the Turn to `uncertain`
    /// rather than `failed`, because the published output cannot be unwound.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::NotFound`] when no row exists.
    pub(crate) fn mark_turn_output_published(&self, turn_id: &str) -> Result<(), StorageError> {
        let conn = self.get_conn()?;
        let now = chrono::Utc::now().to_rfc3339();
        let rows = conn.execute(
            "UPDATE turn_runs SET output_published = 1, updated_at = ?2 WHERE turn_id = ?1",
            params![turn_id, &now],
        )?;
        if rows == 0 {
            return Err(StorageError::NotFound(format!("turn_run:{turn_id}")));
        }
        Ok(())
    }

    /// Begins a new model iteration: advances to `model_pending`, stamps the
    /// iteration number and the fixed `model_request_hash`, and resets
    /// `model_attempt` to `1`. The request hash lets a later retry or recovery
    /// prove the same payload is being re-sent.
    ///
    /// Allowed from: `input_committed`, `model_completed`, `tools_completed`.
    ///
    /// This is a true optimistic CAS: the `UPDATE` is conditional on the exact
    /// state read first, so two executors that both observe `input_committed`
    /// cannot both move the turn to `model_pending`. This is the execution-right
    /// boundary — the executor that commits `input_committed ->
    /// model_pending` first owns the turn; a concurrent duplicate (e.g. a
    /// recovered `input_committed` turn re-dispatched by the turn dispatcher)
    /// observes `Ok(false)` and exits without producing output.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::Conflict`] when the current state cannot start
    /// a model iteration, or [`StorageError::NotFound`] when the row is missing.
    pub(crate) fn begin_turn_model_iteration(
        &self,
        turn_id: &str,
        iteration: i64,
        model_request_hash: &str,
    ) -> Result<bool, StorageError> {
        let mut conn = self.get_conn()?;
        let tx = conn.transaction()?;
        let current = read_state_locked(&tx, turn_id)?;
        if !matches!(
            current,
            TurnRunState::InputCommitted
                | TurnRunState::ModelCompleted
                | TurnRunState::ToolsCompleted
        ) {
            return Err(StorageError::Conflict(format!(
                "begin_model_iteration rejected from state {current}"
            )));
        }
        let now = chrono::Utc::now().to_rfc3339();
        let affected = tx.execute(
            "UPDATE turn_runs
             SET state = 'model_pending',
                 current_iteration = ?2,
                 model_request_hash = ?3,
                 model_attempt = 1,
                 updated_at = ?4
             WHERE turn_id = ?1 AND state = ?5",
            params![
                turn_id,
                iteration,
                model_request_hash,
                &now,
                current.to_string()
            ],
        )?;
        if affected == 0 {
            // Another executor already transitioned this turn away from
            // `current`. It owns the turn now; this duplicate must not run.
            return Ok(false);
        }
        tx.commit()?;
        Ok(true)
    }

    /// Increments `model_attempt` for an in-place retry of the current
    /// iteration (same `model_request_hash`). The state stays `model_pending`.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::Conflict`] when the row is not `model_pending`,
    /// or [`StorageError::NotFound`] when the row is missing.
    pub(crate) fn increment_turn_model_attempt(&self, turn_id: &str) -> Result<(), StorageError> {
        let mut conn = self.get_conn()?;
        let tx = conn.transaction()?;
        require_state_locked(&tx, turn_id, TurnRunState::ModelPending)?;
        let now = chrono::Utc::now().to_rfc3339();
        tx.execute(
            "UPDATE turn_runs
             SET model_attempt = model_attempt + 1,
                 updated_at = ?2
             WHERE turn_id = ?1",
            params![turn_id, &now],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Advances `model_pending -> model_completed` (the LLM returned a
    /// response).
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::Conflict`] when the row is not `model_pending`.
    pub(crate) fn complete_turn_model(&self, turn_id: &str) -> Result<(), StorageError> {
        self.transition_turn_state(
            turn_id,
            TurnRunState::ModelPending,
            TurnRunState::ModelCompleted,
        )
    }

    /// Advances `model_completed -> tools_pending` (the response carried Tool
    /// Calls that will now execute).
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::Conflict`] when the row is not `model_completed`.
    pub(crate) fn begin_turn_tools(&self, turn_id: &str) -> Result<(), StorageError> {
        self.transition_turn_state(
            turn_id,
            TurnRunState::ModelCompleted,
            TurnRunState::ToolsPending,
        )
    }

    /// Advances `tools_pending -> tools_completed`.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::Conflict`] when the row is not `tools_pending`.
    pub(crate) fn complete_turn_tools(&self, turn_id: &str) -> Result<(), StorageError> {
        self.transition_turn_state(
            turn_id,
            TurnRunState::ToolsPending,
            TurnRunState::ToolsCompleted,
        )
    }

    /// Finalizes the Turn as `completed`, records the final assistant message
    /// id, and stamps `finished_at`. Allowed from `model_completed` (final
    /// response, no tools) and `tools_completed` (finalize after tools).
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::Conflict`] when the current state cannot
    /// finalize.
    pub(crate) fn complete_turn(
        &self,
        turn_id: &str,
        final_message_id: &str,
    ) -> Result<(), StorageError> {
        let mut conn = self.get_conn()?;
        let tx = conn.transaction()?;
        let current = read_state_locked(&tx, turn_id)?;
        if !matches!(
            current,
            TurnRunState::ModelCompleted | TurnRunState::ToolsCompleted
        ) {
            return Err(StorageError::Conflict(format!(
                "complete rejected from state {current}"
            )));
        }
        let now = chrono::Utc::now().to_rfc3339();
        tx.execute(
            "UPDATE turn_runs
             SET state = 'completed',
                 final_message_id = ?2,
                 finished_at = ?3,
                 updated_at = ?3,
                 error_kind = NULL,
                 error_message = NULL
             WHERE turn_id = ?1",
            params![turn_id, final_message_id, &now],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Terminates the Turn as `failed` or `uncertain` with a sanitized error
    /// classification. `uncertain` is used when external output may already
    /// have been published (partial delta / Tool Call) and a retry cannot be
    /// proven safe; `failed` is used for clean failures before any output.
    ///
    /// The caller chooses the target state; this method validates that the
    /// current state is non-terminal and records `error_kind` /
    /// `error_message` / `finished_at`.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::Conflict`] when the row is already terminal, or
    /// when `to` is not a failure state.
    pub(crate) fn fail_turn(
        &self,
        turn_id: &str,
        to: TurnRunState,
        error_kind: &str,
        error_message: &str,
    ) -> Result<(), StorageError> {
        if !matches!(to, TurnRunState::Failed | TurnRunState::Uncertain) {
            return Err(StorageError::Conflict(format!(
                "fail target must be failed or uncertain, got {to}"
            )));
        }
        let mut conn = self.get_conn()?;
        let tx = conn.transaction()?;
        let current = read_state_locked(&tx, turn_id)?;
        if current.is_terminal() {
            return Err(StorageError::Conflict(format!(
                "fail rejected from terminal state {current}"
            )));
        }
        let now = chrono::Utc::now().to_rfc3339();
        tx.execute(
            "UPDATE turn_runs
             SET state = ?2,
                 error_kind = ?3,
                 error_message = ?4,
                 finished_at = ?5,
                 updated_at = ?5
             WHERE turn_id = ?1",
            params![turn_id, to.to_string(), error_kind, error_message, &now],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Terminates a durably accepted turn that will never execute, recording why.
    ///
    /// Used when the turn dispatcher drops a turn because its origin already has a
    /// terminal stop reason, or [`crate::runtime::turn_scheduler::TurnTracker::try_begin_execution`]
    /// refuses it (chain depth, turn count, missing agent). Moving the turn to
    /// `cancelled` (with `error_kind` / `error_message`) prevents the dispatcher
    /// from re-scanning and re-dispatching it on every 5s loop — otherwise a
    /// discarded turn stays `accepted` forever and is retried indefinitely.
    ///
    /// Idempotent: a turn that is already terminal is left untouched (the
    /// cancellation is treated as already applied).
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::Conflict`] when the current state cannot be
    /// cancelled (e.g. the turn is already mid-model-loop).
    pub(crate) fn cancel_turn(
        &self,
        turn_id: &str,
        error_kind: &str,
        error_message: &str,
    ) -> Result<(), StorageError> {
        let mut conn = self.get_conn()?;
        let tx = conn.transaction()?;
        let current = read_state_locked(&tx, turn_id)?;
        if current.is_terminal() {
            return Ok(());
        }
        if !TurnRunState::can_transition(current, TurnRunState::Cancelled) {
            return Err(StorageError::Conflict(format!(
                "cancel rejected from state {current}"
            )));
        }
        let now = chrono::Utc::now().to_rfc3339();
        tx.execute(
            "UPDATE turn_runs
             SET state = 'cancelled',
                 error_kind = ?2,
                 error_message = ?3,
                 finished_at = ?4,
                 updated_at = ?4
             WHERE turn_id = ?1",
            params![turn_id, error_kind, error_message, &now],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Recovers Turns interrupted by a process crash.
    ///
    /// Crashed turns are moved to a terminal state so they are never silently
    /// resumed by a re-delivered request. A re-sent request whose turn is
    /// already `InProgress` (still owned by the original executor that crashed)
    /// would otherwise be accepted as `TurnAcceptance::InProgress` by a fresh
    /// executor and produce an empty response, leaving the turn stuck forever.
    ///
    /// State-specific recovery rules:
    ///
    /// | State             | Recovery action                                          |
    /// |-------------------|----------------------------------------------------------|
    /// | `accepted`        | `failed` unless durably persisted (then left for the    |
    /// |                   | dispatcher) — input never committed, retry is safe.      |
    /// | `input_committed` | `failed` unless durably persisted (then left for the    |
    /// |                   | dispatcher to resume) — no model output yet.             |
    /// | `model_pending`   | `uncertain` — output may have been published externally. |
    /// | `model_completed` | `uncertain` — output may have been published externally. |
    /// | `tools_pending`   | `uncertain` — tool side effects may have run.            |
    /// | `tools_completed` | `uncertain` — tool side effects may have run.            |
    ///
    /// Returns the transitioned rows so the caller can log them. This does
    /// **not** touch `tool_calls`; [`Database::recover_running_tools`] handles
    /// the Tool ledger separately.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if the underlying SQLite writes fail.
    pub(crate) fn recover_interrupted_turns(&self) -> Result<Vec<RecoveredTurnRun>, StorageError> {
        let mut conn = self.get_conn()?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;

        struct InterruptedRow {
            turn_id: String,
            chat_id: i64,
            state: String,
            request_payload_hash: Option<String>,
            scheduled_request_json: Option<String>,
        }

        let interrupted: Vec<InterruptedRow> = {
            let mut stmt = tx.prepare(
                "SELECT turn_id, chat_id, state, request_payload_hash,
                        scheduled_request_json
                 FROM turn_runs
                 WHERE state IN (
                     'accepted', 'input_committed', 'model_pending',
                     'model_completed', 'tools_pending', 'tools_completed'
                 )",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok(InterruptedRow {
                    turn_id: row.get(0)?,
                    chat_id: row.get(1)?,
                    state: row.get(2)?,
                    request_payload_hash: row.get(3)?,
                    scheduled_request_json: row.get(4)?,
                })
            })?;
            rows.collect::<Result<Vec<_>, _>>()?
        };

        let mut recovered = Vec::with_capacity(interrupted.len());
        for row in interrupted {
            let from: TurnRunState = row
                .state
                .parse()
                .map_err(|e| StorageError::Conflict(format!("invalid turn_runs.state: {e}")))?;

            // A durably accepted or input_committed turn (its full request is
            // persisted) is left for the turn dispatcher to resume after startup
            // instead of being failed here: the request can be rebuilt and the
            // turn re-executed (accepted) or its model loop restarted
            // (input_committed) safely. Legacy turns without
            // a persisted request (direct-execution paths) still fall through to
            // the fail-stop branch below.
            if row.scheduled_request_json.is_some()
                && matches!(from, TurnRunState::Accepted | TurnRunState::InputCommitted)
            {
                continue;
            }

            // Fail-stop recovery: every non-terminal turn is terminated on
            // startup. `accepted` / `input_committed` never published any
            // model output, so failing them is safe and lets the user retry.
            // The remaining states may have emitted output (or tool side
            // effects) that the process failed to durably record before
            // crashing, so they go to `uncertain` to avoid re-sending.
            let (to, error_kind, error_message): (TurnRunState, Option<&str>, Option<String>) =
                match from {
                    TurnRunState::Accepted => (
                        TurnRunState::Failed,
                        Some("interrupted"),
                        Some(format!(
                            "recovered on startup: accepted turn never started (payload_hash={})",
                            row.request_payload_hash.as_deref().unwrap_or("none")
                        )),
                    ),
                    TurnRunState::InputCommitted => (
                        TurnRunState::Failed,
                        Some("interrupted"),
                        Some(
                            "recovered on startup: input committed but model never ran".to_string(),
                        ),
                    ),
                    TurnRunState::ModelPending
                    | TurnRunState::ModelCompleted
                    | TurnRunState::ToolsPending
                    | TurnRunState::ToolsCompleted => (
                        TurnRunState::Uncertain,
                        Some("interrupted"),
                        Some(format!(
                            "recovered on startup: {from} may have published output externally; requires manual review"
                        )),
                    ),
                    _ => (from, None, None),
                };

            if to != from {
                let now = chrono::Utc::now().to_rfc3339();
                tx.execute(
                    "UPDATE turn_runs
                     SET state = ?2,
                         error_kind = ?3,
                         error_message = ?4,
                         finished_at = ?5,
                         updated_at = ?6
                     WHERE turn_id = ?1",
                    params![
                        &row.turn_id,
                        to.to_string(),
                        error_kind,
                        error_message.as_deref(),
                        if error_kind.is_some() {
                            Some(&now)
                        } else {
                            None
                        },
                        &now,
                    ],
                )?;
                recovered.push(RecoveredTurnRun {
                    turn_id: row.turn_id,
                    chat_id: row.chat_id,
                    from,
                    recovered_to: to,
                });
            }
        }

        tx.commit()?;
        Ok(recovered)
    }

    /// Aggregates per-origin execution counts from `turn_runs` so the in-memory
    /// `TurnTracker` can be rehydrated after a restart.
    ///
    /// Only turns whose `accepted_at` is within `ttl_secs` of now are considered
    /// (stale chains have been pruned from the live tracker and must not
    /// resurface). Every turn that has left `accepted` / `input_committed`
    /// counts toward its origin, because those are exactly the states the turn
    /// dispatcher re-executes live after startup; counting them here would
    /// double-count. Once a turn has begun execution it has consumed a slot via
    /// [`crate::runtime::turn_scheduler::TurnTracker::try_begin_execution`],
    /// regardless of how it later terminated.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if the underlying SQLite read fails.
    pub(crate) fn recover_origin_tracker(
        &self,
        ttl_secs: i64,
    ) -> Result<Vec<RecoveredOrigin>, StorageError> {
        let conn = self.get_conn()?;
        let mut stmt = conn.prepare(
            "SELECT t.origin_id, COUNT(*),
                    (SELECT o.terminal_reason FROM turn_origins o
                     WHERE o.origin_id = t.origin_id)
              FROM turn_runs t
              WHERE t.origin_id IS NOT NULL
                AND t.state NOT IN ('accepted', 'input_committed')
                AND datetime(t.accepted_at) > datetime('now', ?1)
              GROUP BY t.origin_id",
        )?;
        let rows = stmt.query_map(params![format!("{} seconds", -ttl_secs)], |row| {
            Ok(RecoveredOrigin {
                origin_id: row.get(0)?,
                executed_turn_count: row.get::<_, i64>(1)? as usize,
                terminal_reason: row.get::<_, Option<String>>(2)?,
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Persists (insert-or-update) the durable terminal stop reason for an
    /// origin. The `executed_turn_count` column is left unchanged (it is derived
    /// from `turn_runs` at startup); only `terminal_reason` is written. A `None`
    /// reason clears it. Called whenever the in-memory
    /// [`crate::runtime::turn_scheduler::TurnTracker`] records a terminal reason,
    /// so a terminated chain is durably remembered across restarts.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if the underlying SQLite write fails.
    pub(crate) fn upsert_turn_origin(
        &self,
        origin_id: &str,
        terminal_reason: Option<&str>,
    ) -> Result<(), StorageError> {
        let conn = self.get_conn()?;
        let now = chrono::Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO turn_origins (origin_id, executed_turn_count, terminal_reason, updated_at)
             VALUES (?1, 0, ?2, ?3)
             ON CONFLICT(origin_id) DO UPDATE SET
                terminal_reason = excluded.terminal_reason,
                updated_at = excluded.updated_at",
            params![origin_id, terminal_reason, &now],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{Database, StoredMessage};

    fn test_db() -> (Database, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("runtime").join("egopulse.db");
        let db = Database::new(&db_path).expect("db");
        // turn_runs.chat_id has no FK, but mirror the production setup by
        // creating a chat so the row is realistic.
        let chat_id = db
            .resolve_or_create_chat_id("cli", "cli:turn-run", None, "cli", "default")
            .expect("create chat");
        assert_eq!(chat_id, 1, "expected the seeded chat to be chat_id 1");
        (db, dir)
    }

    fn accept(db: &Database, request_key: &str) -> TurnRun {
        match db
            .accept_or_get_turn(
                1,
                request_key,
                1,
                Some("abc123"),
                "payload-hash",
                None,
                None,
            )
            .expect("accept")
        {
            AcceptOutcome::Created(run) | AcceptOutcome::Existing(run) => run,
        }
    }

    fn state(db: &Database, turn_id: &str) -> TurnRunState {
        db.get_turn_run(turn_id).expect("get").state
    }

    /// Drives the production `commit_turn_input_with_conversation` path so the
    /// state-machine exercises the same atomic commit the runtime uses, and
    /// tracks the per-chat session revision across sequential commits.
    fn commit_input(db: &Database, turn_id: &str, session_revision: &mut Option<i64>) -> String {
        let mut msg = StoredMessage::user(1, "sender".to_string(), format!("input-{turn_id}"));
        msg.turn_id = Some(turn_id.to_string());
        let rev = db
            .commit_turn_input_with_conversation(&msg, "[]", *session_revision, turn_id)
            .expect("commit input");
        *session_revision = Some(rev);
        msg.id
    }

    #[test]
    fn accept_creates_new_turn_in_accepted_state() {
        // Arrange
        let (db, _dir) = test_db();

        // Act
        let run = accept(&db, "discord:42:100");

        // Assert
        assert_eq!(run.state, TurnRunState::Accepted);
        assert_eq!(run.chat_id, 1);
        assert_eq!(run.request_key, "discord:42:100");
        assert_eq!(run.config_revision, 1);
        assert_eq!(run.config_fingerprint.as_deref(), Some("abc123"));
        assert_eq!(run.model_attempt, 0);
        assert!(!run.output_published);
    }

    #[test]
    fn commit_turn_input_with_conversation_is_atomic_on_conflict() {
        // Arrange: a pre-existing session row at revision 0, plus an accepted
        // Turn. A mismatched `expected_revision` must roll back both the
        // message insert and the `accepted → input_committed` transition.
        let (db, _dir) = test_db();
        let turn_id = accept(&db, "atomic").turn_id;
        {
            let conn = db.get_conn().expect("conn");
            conn.execute(
                "INSERT INTO sessions (chat_id, messages_json, updated_at, snapshot_through_seq)
                 VALUES (1, '[]', 't', 0)",
                [],
            )
            .expect("seed session");
            conn.execute("UPDATE chats SET revision = 5 WHERE chat_id = 1", [])
                .expect("bump revision");
        }
        let mut msg = StoredMessage::user(1, "sender".to_string(), "hello".to_string());
        msg.turn_id = Some(turn_id.clone());

        // Act: expected_revision mismatches the real revision (5).
        let error = db
            .commit_turn_input_with_conversation(&msg, "[]", Some(0), &turn_id)
            .expect_err("conflict expected");
        assert!(matches!(error, StorageError::SessionSnapshotConflict));

        // Assert: neither the conversation nor the Turn state changed.
        assert_eq!(state(&db, &turn_id), TurnRunState::Accepted);
        let message_count: i64 = db
            .get_conn()
            .expect("conn")
            .query_row(
                "SELECT COUNT(*) FROM messages WHERE chat_id = 1",
                [],
                |row| row.get(0),
            )
            .expect("count");
        assert_eq!(
            message_count, 0,
            "message insert must roll back on conflict"
        );
    }

    #[test]
    fn accept_returns_existing_turn_for_same_request_key() {
        // Arrange
        let (db, _dir) = test_db();
        let first = accept(&db, "telegram:7:99");

        // Act
        let second = accept(&db, "telegram:7:99");

        // Assert: same Turn, no duplicate row.
        assert_eq!(first.turn_id, second.turn_id);
        let count: i64 = db
            .get_conn()
            .expect("conn")
            .query_row(
                "SELECT COUNT(*) FROM turn_runs WHERE chat_id = 1 AND request_key = 'telegram:7:99'",
                [],
                |row| row.get(0),
            )
            .expect("count");
        assert_eq!(count, 1);
    }

    #[test]
    fn accept_distinguishes_different_request_keys() {
        // Arrange
        let (db, _dir) = test_db();

        // Act
        let a = accept(&db, "discord:1:1");
        let b = accept(&db, "discord:1:2");

        // Assert
        assert_ne!(a.turn_id, b.turn_id);
    }

    #[test]
    fn accept_treats_null_request_payload_hash_as_legacy_existing() {
        // Arrange: a pre-existing turn_runs row with a NULL request_payload_hash
        // (legacy data captured before payload hashing existed).
        let (db, _dir) = test_db();
        db.get_conn()
            .expect("conn")
            .execute(
                "INSERT INTO turn_runs
                     (turn_id, chat_id, request_key, state, config_revision, accepted_at, updated_at)
                 VALUES (?1, 1, 'legacy:key', 'completed', 1, 't', 't')",
                params![&"legacy-turn"],
            )
            .expect("seed legacy row");

        // Act: re-accept the same request_key with any payload hash.
        let outcome = db
            .accept_or_get_turn(1, "legacy:key", 1, Some("abc123"), "new-hash", None, None)
            .expect("accept");

        // Assert: a NULL stored hash is legacy data, so it is accepted as
        // Existing rather than rejected as a payload mismatch.
        assert!(
            matches!(outcome, AcceptOutcome::Existing(_)),
            "legacy NULL request_payload_hash must be AcceptedOutcome::Existing"
        );
    }

    #[test]
    fn transition_rejects_invalid_state_change() {
        // Arrange
        let (db, _dir) = test_db();
        let turn_id = accept(&db, "k").turn_id;

        // Act: accepted -> tools_pending is not a permitted transition.
        let error = db
            .transition_turn_state(&turn_id, TurnRunState::Accepted, TurnRunState::ToolsPending)
            .expect_err("invalid transition");

        // Assert
        assert!(matches!(error, StorageError::Conflict(_)));
        assert_eq!(state(&db, &turn_id), TurnRunState::Accepted);
    }

    #[test]
    fn transition_rejects_when_current_state_differs_from_expected() {
        // Arrange
        let (db, _dir) = test_db();
        let turn_id = accept(&db, "k").turn_id;
        let mut rev = None;
        commit_input(&db, &turn_id, &mut rev);

        // Act: the row is now input_committed, not accepted.
        let error = db
            .transition_turn_state(&turn_id, TurnRunState::Accepted, TurnRunState::ModelPending)
            .expect_err("stale expected state");

        // Assert
        assert!(matches!(error, StorageError::Conflict(_)));
    }

    #[test]
    fn commit_input_advances_to_input_committed_and_stores_message_id() {
        // Arrange
        let (db, _dir) = test_db();
        let turn_id = accept(&db, "k").turn_id;

        // Act
        let mut rev = None;
        let msg_id = commit_input(&db, &turn_id, &mut rev);

        // Assert
        let run = db.get_turn_run(&turn_id).expect("get");
        assert_eq!(run.state, TurnRunState::InputCommitted);
        assert_eq!(run.input_message_id.as_deref(), Some(msg_id.as_str()));
    }

    #[test]
    fn full_lifecycle_progresses_through_all_states() {
        // Arrange
        let (db, _dir) = test_db();
        let turn_id = accept(&db, "life").turn_id;

        // Act: accepted -> input_committed -> model_pending -> model_completed
        //      -> tools_pending -> tools_completed -> model_pending -> completed
        let mut rev = None;
        commit_input(&db, &turn_id, &mut rev);
        db.begin_turn_model_iteration(&turn_id, 1, "hash-1")
            .expect("begin iter 1");
        db.complete_turn_model(&turn_id).expect("complete model");
        db.begin_turn_tools(&turn_id).expect("begin tools");
        db.complete_turn_tools(&turn_id).expect("complete tools");
        db.begin_turn_model_iteration(&turn_id, 2, "hash-2")
            .expect("begin iter 2");
        db.complete_turn_model(&turn_id).expect("complete model 2");
        db.complete_turn(&turn_id, "final-1").expect("complete");

        // Assert
        let run = db.get_turn_run(&turn_id).expect("get");
        assert_eq!(run.state, TurnRunState::Completed);
        assert_eq!(run.final_message_id.as_deref(), Some("final-1"));
        assert!(run.finished_at.is_some());
        assert!(run.error_kind.is_none());
    }

    #[test]
    fn begin_model_iteration_stamps_hash_and_resets_attempt() {
        // Arrange
        let (db, _dir) = test_db();
        let turn_id = accept(&db, "iter").turn_id;
        let mut rev = None;
        commit_input(&db, &turn_id, &mut rev);

        // Act
        db.begin_turn_model_iteration(&turn_id, 1, "hash-A")
            .expect("begin");

        // Assert
        let run = db.get_turn_run(&turn_id).expect("get");
        assert_eq!(run.state, TurnRunState::ModelPending);
        assert_eq!(run.current_iteration, 1);
        assert_eq!(run.model_request_hash.as_deref(), Some("hash-A"));
        assert_eq!(run.model_attempt, 1);
    }

    #[test]
    fn increment_model_attempt_increments_without_changing_state() {
        // Arrange
        let (db, _dir) = test_db();
        let turn_id = accept(&db, "retry").turn_id;
        let mut rev = None;
        commit_input(&db, &turn_id, &mut rev);
        db.begin_turn_model_iteration(&turn_id, 1, "hash")
            .expect("begin iter");

        // Act
        db.increment_turn_model_attempt(&turn_id)
            .expect("increment");
        db.increment_turn_model_attempt(&turn_id)
            .expect("increment");

        // Assert
        let run = db.get_turn_run(&turn_id).expect("get");
        assert_eq!(run.state, TurnRunState::ModelPending);
        assert_eq!(run.model_attempt, 3);
    }

    #[test]
    fn increment_model_attempt_rejected_outside_model_pending() {
        // Arrange
        let (db, _dir) = test_db();
        let turn_id = accept(&db, "no-retry").turn_id;

        // Act
        let error = db
            .increment_turn_model_attempt(&turn_id)
            .expect_err("rejected");

        // Assert
        assert!(matches!(error, StorageError::Conflict(_)));
    }

    #[test]
    fn mark_output_published_is_idempotent() {
        // Arrange
        let (db, _dir) = test_db();
        let turn_id = accept(&db, "pub").turn_id;

        // Act
        db.mark_turn_output_published(&turn_id).expect("first");
        db.mark_turn_output_published(&turn_id).expect("second");

        // Assert
        assert!(db.get_turn_run(&turn_id).expect("get").output_published);
    }

    #[test]
    fn fail_records_error_classification_and_finished_at() {
        // Arrange
        let (db, _dir) = test_db();
        let turn_id = accept(&db, "fail").turn_id;
        let mut rev = None;
        commit_input(&db, &turn_id, &mut rev);

        // Act
        db.fail_turn(&turn_id, TurnRunState::Failed, "llm_error", "boom")
            .expect("fail");

        // Assert
        let run = db.get_turn_run(&turn_id).expect("get");
        assert_eq!(run.state, TurnRunState::Failed);
        assert_eq!(run.error_kind.as_deref(), Some("llm_error"));
        assert_eq!(run.error_message.as_deref(), Some("boom"));
        assert!(run.finished_at.is_some());
    }

    #[test]
    fn fail_uncertain_for_published_output() {
        // Arrange
        let (db, _dir) = test_db();
        let turn_id = accept(&db, "unc").turn_id;
        let mut rev = None;
        commit_input(&db, &turn_id, &mut rev);
        db.begin_turn_model_iteration(&turn_id, 1, "h")
            .expect("begin");
        db.mark_turn_output_published(&turn_id).expect("published");

        // Act: output was published -> uncertain, not failed.
        db.fail_turn(
            &turn_id,
            TurnRunState::Uncertain,
            "partial_output",
            "delta emitted",
        )
        .expect("fail");

        // Assert
        assert_eq!(state(&db, &turn_id), TurnRunState::Uncertain);
    }

    #[test]
    fn fail_rejected_from_terminal_state() {
        // Arrange
        let (db, _dir) = test_db();
        let turn_id = accept(&db, "term").turn_id;
        db.fail_turn(&turn_id, TurnRunState::Failed, "x", "y")
            .expect("first fail");

        // Act
        let error = db
            .fail_turn(&turn_id, TurnRunState::Failed, "x", "y")
            .expect_err("terminal");

        // Assert
        assert!(matches!(error, StorageError::Conflict(_)));
    }

    #[test]
    fn fail_rejects_non_failure_target() {
        // Arrange
        let (db, _dir) = test_db();
        let turn_id = accept(&db, "bad-target").turn_id;

        // Act
        let error = db
            .fail_turn(&turn_id, TurnRunState::Completed, "x", "y")
            .expect_err("non-failure target");

        // Assert
        assert!(matches!(error, StorageError::Conflict(_)));
    }

    #[test]
    fn recover_interrupted_fail_stops_all_non_terminal_turns() {
        // Arrange: every non-terminal state is represented.
        let (db, _dir) = test_db();
        let accepted_id = accept(&db, "acc").turn_id;
        let input_committed_id = accept(&db, "mid").turn_id;
        let model_pending_unsafe_id = accept(&db, "unsafe").turn_id;
        let model_pending_safe_id = accept(&db, "safe").turn_id;
        let model_completed_id = accept(&db, "mcomp").turn_id;
        let tools_pending_id = accept(&db, "tpend").turn_id;
        let tools_completed_id = accept(&db, "tcomp").turn_id;

        // All committed Turns share one chat, so the session revision is
        // threaded through the sequential commits.
        let mut rev = None;
        commit_input(&db, &input_committed_id, &mut rev);

        commit_input(&db, &model_pending_unsafe_id, &mut rev);
        db.begin_turn_model_iteration(&model_pending_unsafe_id, 1, "h")
            .expect("begin");
        db.mark_turn_output_published(&model_pending_unsafe_id)
            .expect("mark published");

        commit_input(&db, &model_pending_safe_id, &mut rev);
        db.begin_turn_model_iteration(&model_pending_safe_id, 1, "h")
            .expect("begin");

        commit_input(&db, &model_completed_id, &mut rev);
        db.begin_turn_model_iteration(&model_completed_id, 1, "h")
            .expect("begin");
        db.complete_turn_model(&model_completed_id)
            .expect("complete model");

        commit_input(&db, &tools_pending_id, &mut rev);
        db.begin_turn_model_iteration(&tools_pending_id, 1, "h")
            .expect("begin");
        db.complete_turn_model(&tools_pending_id)
            .expect("complete model");
        db.begin_turn_tools(&tools_pending_id).expect("begin tools");

        commit_input(&db, &tools_completed_id, &mut rev);
        db.begin_turn_model_iteration(&tools_completed_id, 1, "h")
            .expect("begin");
        db.complete_turn_model(&tools_completed_id)
            .expect("complete model");
        db.begin_turn_tools(&tools_completed_id)
            .expect("begin tools");
        db.complete_turn_tools(&tools_completed_id)
            .expect("complete tools");

        // Act
        let recovered = db.recover_interrupted_turns().expect("recover");

        // Assert: accepted / input_committed fail (safe to retry); every
        // state that may have published output goes to uncertain.
        assert_eq!(recovered.len(), 7);
        assert_eq!(state(&db, &accepted_id), TurnRunState::Failed);
        assert_eq!(state(&db, &input_committed_id), TurnRunState::Failed);
        assert_eq!(
            state(&db, &model_pending_unsafe_id),
            TurnRunState::Uncertain
        );
        assert_eq!(state(&db, &model_pending_safe_id), TurnRunState::Uncertain);
        assert_eq!(state(&db, &model_completed_id), TurnRunState::Uncertain);
        assert_eq!(state(&db, &tools_pending_id), TurnRunState::Uncertain);
        assert_eq!(state(&db, &tools_completed_id), TurnRunState::Uncertain);
    }

    #[test]
    fn cancel_turn_moves_accepted_to_cancelled_with_reason() {
        // Arrange
        let (db, _dir) = test_db();
        let turn_id = accept(&db, "cancel-me").turn_id;

        // Act
        db.cancel_turn(&turn_id, "chain_depth_exceeded", "origin chain terminated")
            .expect("cancel");

        // Assert
        let run = db.get_turn_run(&turn_id).expect("get");
        assert_eq!(run.state, TurnRunState::Cancelled);
        assert_eq!(run.error_kind.as_deref(), Some("chain_depth_exceeded"));
        assert_eq!(
            run.error_message.as_deref(),
            Some("origin chain terminated")
        );
        assert!(run.finished_at.is_some());
    }

    #[test]
    fn cancel_turn_is_idempotent_on_terminal_state() {
        // Arrange
        let (db, _dir) = test_db();
        let turn_id = accept(&db, "cancel-idem").turn_id;
        db.cancel_turn(&turn_id, "turn_count_exceeded", "stop")
            .expect("first cancel");

        // Act: a second cancel must not error.
        db.cancel_turn(&turn_id, "turn_count_exceeded", "stop")
            .expect("second cancel");

        // Assert
        assert_eq!(state(&db, &turn_id), TurnRunState::Cancelled);
    }

    #[test]
    fn cancel_turn_rejected_from_non_cancellable_state() {
        // Arrange
        let (db, _dir) = test_db();
        let turn_id = accept(&db, "no-cancel").turn_id;
        let mut rev = None;
        commit_input(&db, &turn_id, &mut rev);
        db.begin_turn_model_iteration(&turn_id, 1, "h")
            .expect("begin");

        // Act
        let error = db
            .cancel_turn(&turn_id, "x", "y")
            .expect_err("cannot cancel mid-loop");

        // Assert
        assert!(matches!(error, StorageError::Conflict(_)));
        assert_eq!(state(&db, &turn_id), TurnRunState::ModelPending);
    }

    #[test]
    fn recover_interrupted_leaves_terminal_states_untouched() {
        // Arrange
        let (db, _dir) = test_db();
        let completed_id = accept(&db, "done").turn_id;
        let mut rev = None;
        commit_input(&db, &completed_id, &mut rev);
        db.begin_turn_model_iteration(&completed_id, 1, "h")
            .expect("begin");
        db.complete_turn_model(&completed_id)
            .expect("complete model");
        db.complete_turn(&completed_id, "final").expect("complete");

        // Act
        let recovered = db.recover_interrupted_turns().expect("recover");

        // Assert
        assert!(recovered.is_empty(), "terminal turns are not recovered");
        assert_eq!(state(&db, &completed_id), TurnRunState::Completed);
    }

    #[test]
    fn recover_interrupted_is_idempotent() {
        // Arrange
        let (db, _dir) = test_db();
        let turn_id = accept(&db, "once").turn_id;
        let mut rev = None;
        commit_input(&db, &turn_id, &mut rev);

        // Act: first recovery fail-stops input_committed; second sees no change.
        db.recover_interrupted_turns().expect("first");
        let second = db.recover_interrupted_turns().expect("second");

        // Assert
        assert!(second.is_empty());
        assert_eq!(state(&db, &turn_id), TurnRunState::Failed);
    }

    #[test]
    fn recover_interrupted_fails_accepted_regardless_of_fingerprint() {
        // Arrange: accepted turn with a stored fingerprint.
        let (db, _dir) = test_db();
        let turn_id = accept(&db, "mismatch").turn_id;

        // Act: recovery with a different fingerprint.
        let recovered = db.recover_interrupted_turns().expect("recover");

        // Assert: fail-stop ignores the fingerprint; an accepted turn that
        // never started is always failed so the user can safely retry.
        assert_eq!(recovered.len(), 1);
        assert_eq!(state(&db, &turn_id), TurnRunState::Failed);
    }

    #[test]
    fn scan_durable_accepted_excludes_legacy_and_recent() {
        // Arrange: one durable-accepted turn (request persisted) and one legacy
        // accepted turn (no persisted request).
        let (db, _dir) = test_db();
        let durable = match db
            .accept_or_get_turn(1, "durable:k", 1, Some("fp"), "h", None, Some(r#"{"x":1}"#))
            .expect("accept durable")
        {
            AcceptOutcome::Created(run) => run,
            _ => panic!("expected created"),
        };
        db.accept_or_get_turn(1, "legacy:k", 1, Some("fp"), "h", None, None)
            .expect("accept legacy");

        // Age the durable row past the dispatcher's grace window so the scan
        // would pick it up.
        db.get_conn()
            .expect("conn")
            .execute(
                "UPDATE turn_runs SET accepted_at = '2000-01-01T00:00:00Z' WHERE turn_id = ?1",
                params![&durable.turn_id],
            )
            .expect("age durable");

        // Act
        let ids = db
            .scan_durable_pending_turns_after("", "", 100)
            .expect("scan")
            .into_iter()
            .map(|p| p.turn_id)
            .collect::<Vec<_>>();

        // Assert: only the durable turn is returned; the legacy one is excluded.
        assert_eq!(ids, vec![durable.turn_id]);
    }

    #[test]
    fn recover_leaves_durable_accepted_for_dispatcher() {
        // Arrange: a durably accepted turn (request persisted) older than the
        // grace window.
        let (db, _dir) = test_db();
        let durable = match db
            .accept_or_get_turn(1, "dur:k", 1, Some("fp"), "h", None, Some("{}"))
            .expect("accept durable")
        {
            AcceptOutcome::Created(run) => run,
            _ => panic!("expected created"),
        };
        db.get_conn()
            .expect("conn")
            .execute(
                "UPDATE turn_runs SET accepted_at = '2000-01-01T00:00:00Z' WHERE turn_id = ?1",
                params![&durable.turn_id],
            )
            .expect("age durable");

        // Act
        db.recover_interrupted_turns().expect("recover");

        // Assert: the durable accepted turn is left for the dispatcher to resume,
        // not failed like a legacy accepted turn.
        assert_eq!(
            db.get_turn_run(&durable.turn_id).expect("get").state,
            TurnRunState::Accepted
        );
    }

    #[test]
    fn scan_durable_pending_unifies_accepted_and_input_committed() {
        // Arrange: a durable accepted turn, a durable input_committed turn,
        // and a legacy input_committed turn with no persisted request.
        let (db, _dir) = test_db();
        let mut rev = None;
        let accepted = match db
            .accept_or_get_turn(1, "acc:k", 1, Some("fp"), "h", None, Some(r#"{"x":1}"#))
            .expect("accept acc")
        {
            AcceptOutcome::Created(run) => run,
            _ => panic!("expected created"),
        };
        let committed = match db
            .accept_or_get_turn(1, "com:k", 1, Some("fp"), "h", None, Some(r#"{"y":2}"#))
            .expect("accept com")
        {
            AcceptOutcome::Created(run) => run,
            _ => panic!("expected created"),
        };
        commit_input(&db, &committed.turn_id, &mut rev);
        let legacy = match db
            .accept_or_get_turn(1, "legacy:k", 1, Some("fp"), "h", None, None)
            .expect("accept legacy")
        {
            AcceptOutcome::Created(run) => run,
            _ => panic!("expected created"),
        };
        commit_input(&db, &legacy.turn_id, &mut rev);

        // Act
        let ids = db
            .scan_durable_pending_turns_after("", "", 100)
            .expect("scan")
            .into_iter()
            .map(|p| p.turn_id)
            .collect::<Vec<_>>();

        // Assert: both durable turns are returned;
        // the legacy turn (no request) is excluded.
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&accepted.turn_id));
        assert!(ids.contains(&committed.turn_id));
    }

    #[test]
    fn scan_durable_pending_preserves_accepted_at_order_across_states() {
        // Arrange: same session with A=input_committed (accepted first) and
        // B=accepted (accepted later). A must dispatch before B so restart
        // cannot reverse the turn order.
        let (db, _dir) = test_db();
        let mut rev = None;

        let a = match db
            .accept_or_get_turn(1, "a:k", 1, Some("fp"), "h", None, Some("{}"))
            .expect("accept a")
        {
            AcceptOutcome::Created(run) => run,
            _ => panic!("expected created"),
        };
        commit_input(&db, &a.turn_id, &mut rev);

        let b = match db
            .accept_or_get_turn(1, "b:k", 1, Some("fp"), "h", None, Some("{}"))
            .expect("accept b")
        {
            AcceptOutcome::Created(run) => run,
            _ => panic!("expected created"),
        };

        // Act
        let ids = db
            .scan_durable_pending_turns_after("", "", 100)
            .expect("scan")
            .into_iter()
            .map(|p| p.turn_id)
            .collect::<Vec<_>>();

        // Assert: A appears before B regardless of state difference.
        let pos_a = ids.iter().position(|id| id == &a.turn_id);
        let pos_b = ids.iter().position(|id| id == &b.turn_id);
        assert!(
            pos_a < pos_b,
            "input_committed turn A must dispatch before accepted turn B; got {:?}",
            ids
        );
    }

    #[test]
    fn accept_rejects_when_per_session_durable_pending_full() {
        let (db, _dir) = test_db();
        // Fill the session up to the per-session durable limit.
        for i in 0..MAX_DURABLE_PENDING_PER_SESSION {
            db.accept_or_get_turn(
                1,
                &format!("sess:k{i}"),
                1,
                Some("fp"),
                "h",
                None,
                Some("{}"),
            )
            .expect("accept within limit");
        }
        // The next distinct request for the same session is rejected.
        let err = db
            .accept_or_get_turn(1, "sess:overflow", 1, Some("fp"), "h", None, Some("{}"))
            .expect_err("expected session queue full");
        assert!(matches!(err, StorageError::TurnSessionQueueFull));
        // A different session is still accepted (the cap is per-session).
        db.accept_or_get_turn(2, "other:k", 1, Some("fp"), "h", None, Some("{}"))
            .expect("accept on a different session");
    }

    #[test]
    fn accept_capacity_check_skips_existing_request_key() {
        // A re-delivery of an already-accepted request_key must NOT be counted
        // as new pending load, so it must bypass the capacity gate (otherwise
        // retrying a turn in a full session would 429 instead of dedup).
        let (db, _dir) = test_db();
        for i in 0..MAX_DURABLE_PENDING_PER_SESSION {
            db.accept_or_get_turn(
                1,
                &format!("sess:k{i}"),
                1,
                Some("fp"),
                "h",
                None,
                Some("{}"),
            )
            .expect("accept within limit");
        }
        // Re-deliver the first request_key: should return Existing, not 429.
        let outcome = db
            .accept_or_get_turn(1, "sess:k0", 1, Some("fp"), "h", None, Some("{}"))
            .expect("redelivery bypasses cap");
        assert!(matches!(outcome, AcceptOutcome::Existing(_)));
    }

    #[test]
    fn accept_rejects_when_global_durable_pending_full() {
        let (db, _dir) = test_db();
        // Fill the runtime-wide limit by spreading one turn per session.
        for i in 0..MAX_DURABLE_PENDING_PER_SCOPE {
            db.accept_or_get_turn(
                // chat_id must be unique per row; offset by 1 to avoid the
                // seeded chat_id=1 collisions.
                i + 100,
                &format!("g:k{i}"),
                1,
                Some("fp"),
                "h",
                None,
                Some("{}"),
            )
            .expect("accept within global limit");
        }
        // A brand-new session's first turn is rejected globally.
        let err = db
            .accept_or_get_turn(
                MAX_DURABLE_PENDING_PER_SCOPE + 1000,
                "g:overflow",
                1,
                Some("fp"),
                "h",
                None,
                Some("{}"),
            )
            .expect_err("expected global queue full");
        assert!(matches!(err, StorageError::TurnGlobalQueueFull));
    }

    // --- Fix 4: the dispatcher scan must reach turns beyond the batch prefix ---

    #[test]
    fn scan_durable_pending_turns_after_reaches_past_batch_prefix() {
        let (db, _dir) = test_db();
        // Insert 300 durable-pending turns (well above the 256-batch limit).
        for i in 0..300i64 {
            db.accept_or_get_turn(
                i + 10_000,
                &format!("p:k{i}"),
                1,
                Some("fp"),
                "h",
                None,
                Some("{}"),
            )
            .expect("accept");
        }

        // A single uncursored scan is capped at the batch limit and cannot see
        // the 257th+ turn — the starvation the cursor pagination fixes.
        let single = db
            .scan_durable_pending_turns_after("", "", 256)
            .expect("single scan");
        assert_eq!(single.len(), 256, "uncursored scan sees only the prefix");

        // Cursor pagination: first batch, then continue from its last row.
        let first = db
            .scan_durable_pending_turns_after("", "", 256)
            .expect("first page");
        assert_eq!(first.len(), 256);
        let last = first.last().expect("last of first page");
        let second = db
            .scan_durable_pending_turns_after(&last.accepted_at, &last.turn_id, 256)
            .expect("second page");
        assert_eq!(second.len(), 44, "second page reaches the 257th+ turns");

        // No turn is revisited: the union of both pages covers all 300 exactly.
        let mut ids: Vec<&str> = first
            .iter()
            .chain(second.iter())
            .map(|t| t.turn_id.as_str())
            .collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(
            ids.len(),
            300,
            "cursor pagination covers every pending turn"
        );
    }

    #[test]
    fn recover_leaves_durable_input_committed_for_dispatcher() {
        // Arrange: a durably input_committed turn (request persisted) older than
        // the grace window.
        let (db, _dir) = test_db();
        let mut rev = None;
        let durable = match db
            .accept_or_get_turn(1, "dur:k", 1, Some("fp"), "h", None, Some("{}"))
            .expect("accept durable")
        {
            AcceptOutcome::Created(run) => run,
            _ => panic!("expected created"),
        };
        commit_input(&db, &durable.turn_id, &mut rev);
        db.get_conn()
            .expect("conn")
            .execute(
                "UPDATE turn_runs SET accepted_at = '2000-01-01T00:00:00Z' WHERE turn_id = ?1",
                params![&durable.turn_id],
            )
            .expect("age durable");

        // Act
        db.recover_interrupted_turns().expect("recover");

        // Assert: the durable input_committed turn is left for the dispatcher to
        // resume (model loop restart), not failed like a legacy one.
        assert_eq!(
            db.get_turn_run(&durable.turn_id).expect("get").state,
            TurnRunState::InputCommitted
        );
    }

    #[test]
    fn begin_turn_model_iteration_advances_from_input_committed() {
        // Arrange
        let (db, _dir) = test_db();
        let turn_id = accept(&db, "begin").turn_id;
        let mut rev = None;
        commit_input(&db, &turn_id, &mut rev);

        // Act
        let advanced = db
            .begin_turn_model_iteration(&turn_id, 1, "hash-A")
            .expect("begin");

        // Assert: the model iteration started exactly once.
        assert!(advanced);
        assert_eq!(state(&db, &turn_id), TurnRunState::ModelPending);
    }

    #[test]
    fn begin_turn_model_iteration_rejects_state_outside_allowed_set() {
        // Arrange: an accepted turn has not yet committed input, so it is not in
        // the set of states from which a model iteration may begin.
        let (db, _dir) = test_db();
        let turn_id = accept(&db, "begin-rejected").turn_id;

        // Act
        let error = db
            .begin_turn_model_iteration(&turn_id, 1, "hash-A")
            .expect_err("must reject");

        // Assert: only a terminal-conflict signals an unexpected precondition.
        assert!(matches!(error, StorageError::Conflict(_)));
    }

    #[test]
    fn begin_turn_model_iteration_from_model_completed_resets_and_advances() {
        // Arrange: a turn that already completed a model phase.
        let (db, _dir) = test_db();
        let turn_id = accept(&db, "begin-reset").turn_id;
        let mut rev = None;
        commit_input(&db, &turn_id, &mut rev);
        db.begin_turn_model_iteration(&turn_id, 1, "hash-A")
            .expect("begin");
        db.complete_turn_model(&turn_id).expect("complete");

        // Act: a second model iteration (e.g. after tool execution) is allowed.
        let advanced = db
            .begin_turn_model_iteration(&turn_id, 2, "hash-B")
            .expect("begin again");

        // Assert: the new attempt started and re-stamped its hash.
        assert!(advanced);
        let run = db.get_turn_run(&turn_id).expect("get");
        assert_eq!(run.state, TurnRunState::ModelPending);
        assert_eq!(run.model_attempt, 1);
        assert_eq!(run.model_request_hash.as_deref(), Some("hash-B"));
    }

    #[test]
    fn recover_origin_tracker_counts_executed_turns_within_ttl() {
        // Arrange: ORIG1 has one accepted (excluded), one model_pending and one
        // failed (both counted), plus a stale completed turn outside the TTL
        // window (excluded). ORIG2 has a single completed turn (counted).
        let (db, _dir) = test_db();
        let now = chrono::Utc::now().to_rfc3339();
        let o1a = accept(&db, "o1a").turn_id;
        let o1b = accept(&db, "o1b").turn_id;
        let o1c = accept(&db, "o1c").turn_id;
        let o2a = accept(&db, "o2a").turn_id;
        let stale = accept(&db, "stale").turn_id;
        let conn = db.get_conn().expect("conn");
        conn.execute(
            "UPDATE turn_runs SET origin_id = 'ORIG1', state = 'accepted', accepted_at = ?1 WHERE turn_id = ?2",
            params![now, &o1a],
        )
        .expect("set o1a");
        conn.execute(
            "UPDATE turn_runs SET origin_id = 'ORIG1', state = 'model_pending', accepted_at = ?1 WHERE turn_id = ?2",
            params![now, &o1b],
        )
        .expect("set o1b");
        conn.execute(
            "UPDATE turn_runs SET origin_id = 'ORIG1', state = 'failed', accepted_at = ?1 WHERE turn_id = ?2",
            params![now, &o1c],
        )
        .expect("set o1c");
        conn.execute(
            "UPDATE turn_runs SET origin_id = 'ORIG2', state = 'completed', accepted_at = ?1 WHERE turn_id = ?2",
            params![now, &o2a],
        )
        .expect("set o2a");
        conn.execute(
            "UPDATE turn_runs SET origin_id = 'ORIG1', state = 'completed', accepted_at = '2000-01-01T00:00:00Z' WHERE turn_id = ?1",
            params![&stale],
        )
        .expect("set stale");

        // Act
        let recovered = db.recover_origin_tracker(3600).expect("recover origins");

        // Assert: accepted and stale turns are excluded; per-origin counts hold.
        assert_eq!(recovered.len(), 2);
        let o1 = recovered
            .iter()
            .find(|r| r.origin_id == "ORIG1")
            .expect("ORIG1");
        assert_eq!(o1.executed_turn_count, 2);
        let o2 = recovered
            .iter()
            .find(|r| r.origin_id == "ORIG2")
            .expect("ORIG2");
        assert_eq!(o2.executed_turn_count, 1);
    }

    #[test]
    fn recover_origin_tracker_excludes_turns_outside_ttl() {
        // Arrange: a single completed turn dated far in the past.
        let (db, _dir) = test_db();
        let turn_id = accept(&db, "old").turn_id;
        db.get_conn()
            .expect("conn")
            .execute(
                "UPDATE turn_runs SET origin_id = 'ORIG1', state = 'completed', accepted_at = '2000-01-01T00:00:00Z' WHERE turn_id = ?1",
                params![&turn_id],
            )
            .expect("age");

        // Act: a negative TTL narrows the window so the past turn is excluded.
        let recovered = db.recover_origin_tracker(-1).expect("recover origins");

        // Assert
        assert!(recovered.is_empty());
    }
}
