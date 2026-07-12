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
use rusqlite::params;

use crate::error::StorageError;
use crate::storage::{Database, TurnRunState};

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

const TURN_RUN_COLUMNS: &str = "turn_id, chat_id, request_key, state, current_iteration,
    input_message_id, final_message_id, config_revision,
    config_fingerprint, model_request_hash, model_attempt,
    output_published, error_kind, error_message, accepted_at,
    updated_at, finished_at, request_payload_hash";

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
    pub(crate) fn accept_or_get_turn(
        &self,
        chat_id: i64,
        request_key: &str,
        config_revision: i64,
        config_fingerprint: Option<&str>,
        request_payload_hash: &str,
    ) -> Result<AcceptOutcome, StorageError> {
        let mut conn = self.get_conn()?;
        let tx = conn.transaction()?;
        let now = chrono::Utc::now().to_rfc3339();
        let proposed_turn_id = uuid::Uuid::new_v4().to_string();
        tx.execute(
            "INSERT INTO turn_runs
                 (turn_id, chat_id, request_key, state, config_revision,
                  config_fingerprint, request_payload_hash, accepted_at, updated_at)
             VALUES (?1, ?2, ?3, 'accepted', ?4, ?5, ?6, ?7, ?7)
             ON CONFLICT(chat_id, request_key) DO NOTHING",
            params![
                &proposed_turn_id,
                chat_id,
                request_key,
                config_revision,
                config_fingerprint,
                request_payload_hash,
                &now,
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

    /// Records the committed user-input message id and advances
    /// `accepted -> input_committed`.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if the row is missing or not in the `accepted`
    /// state.
    pub(crate) fn commit_turn_input(
        &self,
        turn_id: &str,
        input_message_id: &str,
    ) -> Result<(), StorageError> {
        let mut conn = self.get_conn()?;
        let tx = conn.transaction()?;
        transition_locked(
            &tx,
            turn_id,
            TurnRunState::Accepted,
            TurnRunState::InputCommitted,
        )?;
        let now = chrono::Utc::now().to_rfc3339();
        tx.execute(
            "UPDATE turn_runs SET input_message_id = ?2, updated_at = ?3 WHERE turn_id = ?1",
            params![turn_id, input_message_id, &now],
        )?;
        tx.commit()?;
        Ok(())
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
    /// # Errors
    ///
    /// Returns [`StorageError::Conflict`] when the current state cannot start
    /// a model iteration, or [`StorageError::NotFound`] when the row is missing.
    pub(crate) fn begin_turn_model_iteration(
        &self,
        turn_id: &str,
        iteration: i64,
        model_request_hash: &str,
    ) -> Result<(), StorageError> {
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
        tx.execute(
            "UPDATE turn_runs
             SET state = 'model_pending',
                 current_iteration = ?2,
                 model_request_hash = ?3,
                 model_attempt = 1,
                 updated_at = ?4
             WHERE turn_id = ?1",
            params![turn_id, iteration, model_request_hash, &now],
        )?;
        tx.commit()?;
        Ok(())
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

    /// Recovers Turns interrupted by a process crash.
    ///
    /// State-specific recovery rules (when `config_fingerprint` matches the
    /// current config):
    ///
    /// | State             | Recovery action                                    |
    /// |-------------------|----------------------------------------------------|
    /// | `accepted`        | `failed` — input was never committed.              |
    /// | `input_committed` | preserved — a re-delivery can resume from model start. |
    /// | `model_pending`   | preserved if `output_published == 0` — safe to retry. |
    /// | `model_completed` | preserved — saved response / tool calls can continue. |
    /// | `tools_pending`   | preserved — tool call state can be inspected.      |
    /// | `tools_completed` | preserved — next iteration can proceed.            |
    ///
    /// When the persisted `config_fingerprint` differs from `current_fingerprint`,
    /// the state is always moved to `uncertain` because the Config generation
    /// cannot be verified.
    ///
    /// Returns the transitioned rows so the caller can log them. This does
    /// **not** touch `tool_calls`; [`Database::recover_running_tools`] handles
    /// the Tool ledger separately.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if the underlying SQLite writes fail.
    pub(crate) fn recover_interrupted_turns(
        &self,
        current_fingerprint: Option<&str>,
    ) -> Result<Vec<RecoveredTurnRun>, StorageError> {
        let mut conn = self.get_conn()?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;

        struct InterruptedRow {
            turn_id: String,
            chat_id: i64,
            state: String,
            config_fingerprint: Option<String>,
            output_published: bool,
            request_payload_hash: Option<String>,
            model_request_hash: Option<String>,
        }

        let interrupted: Vec<InterruptedRow> = {
            let mut stmt = tx.prepare(
                "SELECT turn_id, chat_id, state, config_fingerprint, output_published,
                        request_payload_hash, model_request_hash
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
                    config_fingerprint: row.get(3)?,
                    output_published: row.get::<_, i64>(4)? != 0,
                    request_payload_hash: row.get(5)?,
                    model_request_hash: row.get(6)?,
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

            let fingerprint_mismatch = match (&row.config_fingerprint, current_fingerprint) {
                (Some(stored), Some(current)) => stored != current,
                _ => true,
            };

            let (to, error_kind, error_message): (TurnRunState, Option<&str>, Option<String>) =
                if fingerprint_mismatch {
                    (
                        TurnRunState::Uncertain,
                        Some("config_mismatch"),
                        Some(format!(
                            "recovered on startup: config fingerprint mismatch (stored={stored:?}, current={current:?})",
                            stored = row.config_fingerprint,
                            current = current_fingerprint,
                        )),
                    )
                } else {
                    match from {
                        TurnRunState::Accepted => (
                            TurnRunState::Failed,
                            Some("interrupted"),
                            Some(format!(
                                "recovered on startup: accepted turn has no committed input (payload_hash={})",
                                row.request_payload_hash.as_deref().unwrap_or("none")
                            )),
                        ),
                        TurnRunState::ModelPending
                            if row.output_published || row.model_request_hash.is_none() =>
                        {
                            (
                                TurnRunState::Uncertain,
                                Some("interrupted"),
                                Some(format!(
                                    "recovered on startup: model_pending with output_published={} cannot prove safe retry",
                                    row.output_published
                                )),
                            )
                        }
                        _ => (from, None, None),
                    }
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::Database;

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
            .accept_or_get_turn(1, request_key, 1, Some("abc123"), "payload-hash")
            .expect("accept")
        {
            AcceptOutcome::Created(run) | AcceptOutcome::Existing(run) => run,
        }
    }

    fn state(db: &Database, turn_id: &str) -> TurnRunState {
        db.get_turn_run(turn_id).expect("get").state
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
            .accept_or_get_turn(1, "legacy:key", 1, Some("abc123"), "new-hash")
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
        db.commit_turn_input(&turn_id, "msg-1")
            .expect("commit input");

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
        db.commit_turn_input(&turn_id, "user-msg-1")
            .expect("commit input");

        // Assert
        let run = db.get_turn_run(&turn_id).expect("get");
        assert_eq!(run.state, TurnRunState::InputCommitted);
        assert_eq!(run.input_message_id.as_deref(), Some("user-msg-1"));
    }

    #[test]
    fn full_lifecycle_progresses_through_all_states() {
        // Arrange
        let (db, _dir) = test_db();
        let turn_id = accept(&db, "life").turn_id;

        // Act: accepted -> input_committed -> model_pending -> model_completed
        //      -> tools_pending -> tools_completed -> model_pending -> completed
        db.commit_turn_input(&turn_id, "in-1")
            .expect("commit input");
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
        db.commit_turn_input(&turn_id, "in").expect("commit input");

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
        db.commit_turn_input(&turn_id, "in").expect("commit input");
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
        db.commit_turn_input(&turn_id, "in").expect("commit input");

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
        db.commit_turn_input(&turn_id, "in").expect("commit input");
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
    fn recover_interrupted_marks_accepted_failed_and_preserves_safe_states() {
        // Arrange: accepted (no input), input_committed (safe to resume),
        // model_pending with output (unsafe to retry).
        let (db, _dir) = test_db();
        let accepted_id = accept(&db, "acc").turn_id;
        let input_committed_id = accept(&db, "mid").turn_id;
        let model_pending_unsafe_id = accept(&db, "unsafe").turn_id;
        let model_pending_safe_id = accept(&db, "safe").turn_id;

        db.commit_turn_input(&input_committed_id, "in")
            .expect("commit");

        db.commit_turn_input(&model_pending_unsafe_id, "in")
            .expect("commit");
        db.begin_turn_model_iteration(&model_pending_unsafe_id, 1, "h")
            .expect("begin");
        db.mark_turn_output_published(&model_pending_unsafe_id)
            .expect("mark published");

        db.commit_turn_input(&model_pending_safe_id, "in")
            .expect("commit");
        db.begin_turn_model_iteration(&model_pending_safe_id, 1, "h")
            .expect("begin");

        // Act
        let recovered = db
            .recover_interrupted_turns(Some("abc123"))
            .expect("recover");

        // Assert: accepted → failed; safe mid-flight states are preserved.
        assert_eq!(recovered.len(), 2);
        assert_eq!(state(&db, &accepted_id), TurnRunState::Failed);
        assert_eq!(
            state(&db, &input_committed_id),
            TurnRunState::InputCommitted
        );
        assert_eq!(
            state(&db, &model_pending_unsafe_id),
            TurnRunState::Uncertain
        );
        assert_eq!(
            state(&db, &model_pending_safe_id),
            TurnRunState::ModelPending
        );
    }

    #[test]
    fn recover_interrupted_leaves_terminal_states_untouched() {
        // Arrange
        let (db, _dir) = test_db();
        let completed_id = accept(&db, "done").turn_id;
        db.commit_turn_input(&completed_id, "in").expect("commit");
        db.begin_turn_model_iteration(&completed_id, 1, "h")
            .expect("begin");
        db.complete_turn_model(&completed_id)
            .expect("complete model");
        db.complete_turn(&completed_id, "final").expect("complete");

        // Act
        let recovered = db
            .recover_interrupted_turns(Some("abc123"))
            .expect("recover");

        // Assert
        assert!(recovered.is_empty(), "terminal turns are not recovered");
        assert_eq!(state(&db, &completed_id), TurnRunState::Completed);
    }

    #[test]
    fn recover_interrupted_is_idempotent() {
        // Arrange
        let (db, _dir) = test_db();
        let turn_id = accept(&db, "once").turn_id;
        db.commit_turn_input(&turn_id, "in").expect("commit");

        // Act: first recovery preserves input_committed; second sees no change.
        db.recover_interrupted_turns(Some("abc123")).expect("first");
        let second = db
            .recover_interrupted_turns(Some("abc123"))
            .expect("second");

        // Assert
        assert!(second.is_empty());
        assert_eq!(state(&db, &turn_id), TurnRunState::InputCommitted);
    }

    #[test]
    fn recover_interrupted_config_mismatch_always_uncertain() {
        // Arrange: accepted turn with a stored fingerprint.
        let (db, _dir) = test_db();
        let turn_id = accept(&db, "mismatch").turn_id;

        // Act: recovery with a different fingerprint.
        let recovered = db
            .recover_interrupted_turns(Some("different-fp"))
            .expect("recover");

        // Assert: even though the state is accepted, fingerprint mismatch
        // forces uncertain
        assert_eq!(recovered.len(), 1);
        assert_eq!(state(&db, &turn_id), TurnRunState::Uncertain);
    }
}
