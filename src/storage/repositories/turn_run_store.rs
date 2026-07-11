//! `TurnRepository`: the single authoritative boundary for `turn_runs`
//! persistence and the Turn state machine.
//!
//! Phase 2 Package 4 (Work Package 3 — Durable Turn State and Safe Retry)
//! persists every Turn's lifecycle into the `turn_runs` table so that:
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
//! Config revision / fingerprint are stored as given by the caller. Package 4
//! stores `revision = 0` and `fingerprint = NULL` (the immutable ConfigManager
//! snapshot is Package 5); recovery treats a `NULL` fingerprint as "cannot
//! verify config integrity" and conservatively stops.
//!
//! The repository borrows the owning [`Database`] for its lifetime; construct
//! one via [`Database::turn_run_store`].

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
}

/// Outcome of [`TurnRepository::accept_or_get`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AcceptOutcome {
    /// A fresh `accepted` Turn was created and may proceed.
    Created(TurnRun),
    /// A Turn for the same `chat_id + request_key` already existed. The caller
    /// inspects its state to decide whether to reuse, resume, or refuse.
    Existing(TurnRun),
}

/// One Turn recovered by [`TurnRepository::recover_interrupted`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RecoveredTurnRun {
    pub turn_id: String,
    pub chat_id: i64,
    pub from: TurnRunState,
    pub recovered_to: TurnRunState,
}

/// Aggregates all `turn_runs` writes: idempotent acceptance, validated state
/// transitions, model-iteration bookkeeping, terminal recording, and crash
/// recovery. Reads that support those writes live here too; general Turn
/// queries (e.g. listing) stay on [`Database`].
pub(crate) struct TurnRepository<'a> {
    db: &'a Database,
}

impl<'a> TurnRepository<'a> {
    pub(crate) fn new(db: &'a Database) -> Self {
        Self { db }
    }

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
    /// later recovery can detect a Config generation mismatch. Package 4
    /// supplies `0` / `None`; Package 5 populates the real snapshot identity.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if the underlying SQLite write fails.
    pub(crate) fn accept_or_get(
        &self,
        chat_id: i64,
        request_key: &str,
        config_revision: i64,
        config_fingerprint: Option<&str>,
    ) -> Result<AcceptOutcome, StorageError> {
        let mut conn = self.db.get_conn()?;
        let tx = conn.transaction()?;
        let now = chrono::Utc::now().to_rfc3339();
        let proposed_turn_id = uuid::Uuid::new_v4().to_string();
        tx.execute(
            "INSERT INTO turn_runs
                 (turn_id, chat_id, request_key, state, config_revision,
                  config_fingerprint, accepted_at, updated_at)
             VALUES (?1, ?2, ?3, 'accepted', ?4, ?5, ?6, ?6)
             ON CONFLICT(chat_id, request_key) DO NOTHING",
            params![
                &proposed_turn_id,
                chat_id,
                request_key,
                config_revision,
                config_fingerprint,
                &now,
            ],
        )?;

        let run = Self::read_row(
            &tx,
            "SELECT turn_id, chat_id, request_key, state, current_iteration,
                    input_message_id, final_message_id, config_revision,
                    config_fingerprint, model_request_hash, model_attempt,
                    output_published, error_kind, error_message, accepted_at,
                    updated_at, finished_at
             FROM turn_runs
             WHERE chat_id = ?1 AND request_key = ?2",
            params![chat_id, request_key],
        )?
        .ok_or_else(|| {
            StorageError::Conflict(format!(
                "turn_runs row vanished after upsert for chat_id={chat_id} request_key={request_key}"
            ))
        })?;

        tx.commit()?;

        let outcome = if run.turn_id == proposed_turn_id {
            AcceptOutcome::Created(run)
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
    pub(crate) fn get(&self, turn_id: &str) -> Result<TurnRun, StorageError> {
        let conn = self.db.get_conn()?;
        Self::read_row(
            &conn,
            "SELECT turn_id, chat_id, request_key, state, current_iteration,
                    input_message_id, final_message_id, config_revision,
                    config_fingerprint, model_request_hash, model_attempt,
                    output_published, error_kind, error_message, accepted_at,
                    updated_at, finished_at
             FROM turn_runs
             WHERE turn_id = ?1",
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
    pub(crate) fn transition(
        &self,
        turn_id: &str,
        from: TurnRunState,
        to: TurnRunState,
    ) -> Result<(), StorageError> {
        let mut conn = self.db.get_conn()?;
        let tx = conn.transaction()?;
        Self::transition_locked(&tx, turn_id, from, to)?;
        tx.commit()?;
        Ok(())
    }

    fn transition_locked(
        tx: &rusqlite::Transaction<'_>,
        turn_id: &str,
        from: TurnRunState,
        to: TurnRunState,
    ) -> Result<(), StorageError> {
        let current_str: String = tx
            .query_row(
                "SELECT state FROM turn_runs WHERE turn_id = ?1",
                params![turn_id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| StorageError::NotFound(format!("turn_run:{turn_id}")))?;
        let current: TurnRunState = current_str
            .parse()
            .map_err(|e| StorageError::Conflict(format!("invalid turn_runs.state: {e}")))?;
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

    /// Records the committed user-input message id and advances
    /// `accepted -> input_committed`.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if the row is missing or not in the `accepted`
    /// state.
    pub(crate) fn commit_input(
        &self,
        turn_id: &str,
        input_message_id: &str,
    ) -> Result<(), StorageError> {
        let mut conn = self.db.get_conn()?;
        let tx = conn.transaction()?;
        Self::transition_locked(
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
    pub(crate) fn mark_output_published(&self, turn_id: &str) -> Result<(), StorageError> {
        let conn = self.db.get_conn()?;
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
    pub(crate) fn begin_model_iteration(
        &self,
        turn_id: &str,
        iteration: i64,
        model_request_hash: &str,
    ) -> Result<(), StorageError> {
        let mut conn = self.db.get_conn()?;
        let tx = conn.transaction()?;
        let current_str: String = tx
            .query_row(
                "SELECT state FROM turn_runs WHERE turn_id = ?1",
                params![turn_id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| StorageError::NotFound(format!("turn_run:{turn_id}")))?;
        let current: TurnRunState = current_str
            .parse()
            .map_err(|e| StorageError::Conflict(format!("invalid turn_runs.state: {e}")))?;
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
    pub(crate) fn increment_model_attempt(&self, turn_id: &str) -> Result<(), StorageError> {
        let mut conn = self.db.get_conn()?;
        let tx = conn.transaction()?;
        Self::require_state_locked(&tx, turn_id, TurnRunState::ModelPending)?;
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
    pub(crate) fn complete_model(&self, turn_id: &str) -> Result<(), StorageError> {
        self.transition(
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
    pub(crate) fn begin_tools(&self, turn_id: &str) -> Result<(), StorageError> {
        self.transition(
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
    pub(crate) fn complete_tools(&self, turn_id: &str) -> Result<(), StorageError> {
        self.transition(
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
    pub(crate) fn complete(
        &self,
        turn_id: &str,
        final_message_id: &str,
    ) -> Result<(), StorageError> {
        let mut conn = self.db.get_conn()?;
        let tx = conn.transaction()?;
        let current_str: String = tx
            .query_row(
                "SELECT state FROM turn_runs WHERE turn_id = ?1",
                params![turn_id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| StorageError::NotFound(format!("turn_run:{turn_id}")))?;
        let current: TurnRunState = current_str
            .parse()
            .map_err(|e| StorageError::Conflict(format!("invalid turn_runs.state: {e}")))?;
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
    pub(crate) fn fail(
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
        let mut conn = self.db.get_conn()?;
        let tx = conn.transaction()?;
        let current_str: String = tx
            .query_row(
                "SELECT state FROM turn_runs WHERE turn_id = ?1",
                params![turn_id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| StorageError::NotFound(format!("turn_run:{turn_id}")))?;
        let current: TurnRunState = current_str
            .parse()
            .map_err(|e| StorageError::Conflict(format!("invalid turn_runs.state: {e}")))?;
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
    /// * `accepted` (input never committed) -> `failed`: the original input is
    ///   gone with the dead process, so the Turn cannot be retried in place.
    /// * `input_committed` / `model_pending` / `model_completed` /
    ///   `tools_pending` / `tools_completed` -> `uncertain`: a mid-flight Turn
    ///   cannot prove no output was published, and without an immutable Config
    ///   snapshot (Package 5) the Config generation cannot be verified either.
    ///   Safe stop takes priority over speculative resume (Plan §2.4).
    /// * Terminal states are left untouched.
    ///
    /// Returns the transitioned rows so the caller can log them. This does
    /// **not** touch `tool_calls`; [`crate::storage::ToolExecutionRepository::recover_running`]
    /// handles the Tool ledger separately.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if the underlying SQLite writes fail.
    pub(crate) fn recover_interrupted(&self) -> Result<Vec<RecoveredTurnRun>, StorageError> {
        let mut conn = self.db.get_conn()?;
        let tx = conn.transaction()?;

        let interrupted: Vec<(String, i64, String)> = {
            let mut stmt = tx.prepare(
                "SELECT turn_id, chat_id, state
                 FROM turn_runs
                 WHERE state IN (
                     'accepted', 'input_committed', 'model_pending',
                     'model_completed', 'tools_pending', 'tools_completed'
                 )",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?;
            rows.collect::<Result<Vec<_>, _>>()?
        };

        let mut recovered = Vec::with_capacity(interrupted.len());
        for (turn_id, chat_id, state_str) in interrupted {
            let from: TurnRunState = state_str
                .parse()
                .map_err(|e| StorageError::Conflict(format!("invalid turn_runs.state: {e}")))?;
            let to = match from {
                TurnRunState::Accepted => TurnRunState::Failed,
                _ => TurnRunState::Uncertain,
            };
            let now = chrono::Utc::now().to_rfc3339();
            tx.execute(
                "UPDATE turn_runs
                 SET state = ?2,
                     error_kind = ?3,
                     error_message = ?4,
                     finished_at = ?5,
                     updated_at = ?5
                 WHERE turn_id = ?1",
                params![
                    &turn_id,
                    to.to_string(),
                    "interrupted",
                    "recovered on startup: process restart left the turn non-terminal",
                    &now,
                ],
            )?;
            recovered.push(RecoveredTurnRun {
                turn_id,
                chat_id,
                from,
                recovered_to: to,
            });
        }

        tx.commit()?;
        Ok(recovered)
    }

    fn require_state_locked(
        tx: &rusqlite::Transaction<'_>,
        turn_id: &str,
        expected: TurnRunState,
    ) -> Result<(), StorageError> {
        let state_str: Option<String> = tx
            .query_row(
                "SELECT state FROM turn_runs WHERE turn_id = ?1",
                params![turn_id],
                |row| row.get(0),
            )
            .optional()?;
        let Some(state_str) = state_str else {
            return Err(StorageError::NotFound(format!("turn_run:{turn_id}")));
        };
        let state: TurnRunState = state_str
            .parse()
            .map_err(|e| StorageError::Conflict(format!("invalid turn_runs.state: {e}")))?;
        if state != expected {
            return Err(StorageError::Conflict(format!(
                "turn state transition rejected: expected {expected} but was {state}"
            )));
        }
        Ok(())
    }

    fn read_row(
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
                })
            })
            .optional()?;
        Ok(row)
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
            .turn_run_store()
            .accept_or_get(1, request_key, 0, None)
            .expect("accept")
        {
            AcceptOutcome::Created(run) | AcceptOutcome::Existing(run) => run,
        }
    }

    fn state(db: &Database, turn_id: &str) -> TurnRunState {
        db.turn_run_store().get(turn_id).expect("get").state
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
        assert_eq!(run.config_revision, 0);
        assert!(run.config_fingerprint.is_none());
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
    fn transition_rejects_invalid_state_change() {
        // Arrange
        let (db, _dir) = test_db();
        let turn_id = accept(&db, "k").turn_id;

        // Act: accepted -> tools_pending is not a permitted transition.
        let error = db
            .turn_run_store()
            .transition(&turn_id, TurnRunState::Accepted, TurnRunState::ToolsPending)
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
        db.turn_run_store()
            .commit_input(&turn_id, "msg-1")
            .expect("commit input");

        // Act: the row is now input_committed, not accepted.
        let error = db
            .turn_run_store()
            .transition(&turn_id, TurnRunState::Accepted, TurnRunState::ModelPending)
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
        db.turn_run_store()
            .commit_input(&turn_id, "user-msg-1")
            .expect("commit input");

        // Assert
        let run = db.turn_run_store().get(&turn_id).expect("get");
        assert_eq!(run.state, TurnRunState::InputCommitted);
        assert_eq!(run.input_message_id.as_deref(), Some("user-msg-1"));
    }

    #[test]
    fn full_lifecycle_progresses_through_all_states() {
        // Arrange
        let (db, _dir) = test_db();
        let repo = db.turn_run_store();
        let turn_id = accept(&db, "life").turn_id;

        // Act: accepted -> input_committed -> model_pending -> model_completed
        //      -> tools_pending -> tools_completed -> model_pending -> completed
        repo.commit_input(&turn_id, "in-1").expect("commit input");
        repo.begin_model_iteration(&turn_id, 1, "hash-1")
            .expect("begin iter 1");
        repo.complete_model(&turn_id).expect("complete model");
        repo.begin_tools(&turn_id).expect("begin tools");
        repo.complete_tools(&turn_id).expect("complete tools");
        repo.begin_model_iteration(&turn_id, 2, "hash-2")
            .expect("begin iter 2");
        repo.complete_model(&turn_id).expect("complete model 2");
        repo.complete(&turn_id, "final-1").expect("complete");

        // Assert
        let run = repo.get(&turn_id).expect("get");
        assert_eq!(run.state, TurnRunState::Completed);
        assert_eq!(run.final_message_id.as_deref(), Some("final-1"));
        assert!(run.finished_at.is_some());
        assert!(run.error_kind.is_none());
    }

    #[test]
    fn begin_model_iteration_stamps_hash_and_resets_attempt() {
        // Arrange
        let (db, _dir) = test_db();
        let repo = db.turn_run_store();
        let turn_id = accept(&db, "iter").turn_id;
        repo.commit_input(&turn_id, "in").expect("commit input");

        // Act
        repo.begin_model_iteration(&turn_id, 1, "hash-A")
            .expect("begin");

        // Assert
        let run = repo.get(&turn_id).expect("get");
        assert_eq!(run.state, TurnRunState::ModelPending);
        assert_eq!(run.current_iteration, 1);
        assert_eq!(run.model_request_hash.as_deref(), Some("hash-A"));
        assert_eq!(run.model_attempt, 1);
    }

    #[test]
    fn increment_model_attempt_increments_without_changing_state() {
        // Arrange
        let (db, _dir) = test_db();
        let repo = db.turn_run_store();
        let turn_id = accept(&db, "retry").turn_id;
        repo.commit_input(&turn_id, "in").expect("commit input");
        repo.begin_model_iteration(&turn_id, 1, "hash")
            .expect("begin iter");

        // Act
        repo.increment_model_attempt(&turn_id).expect("increment");
        repo.increment_model_attempt(&turn_id).expect("increment");

        // Assert
        let run = repo.get(&turn_id).expect("get");
        assert_eq!(run.state, TurnRunState::ModelPending);
        assert_eq!(run.model_attempt, 3);
    }

    #[test]
    fn increment_model_attempt_rejected_outside_model_pending() {
        // Arrange
        let (db, _dir) = test_db();
        let repo = db.turn_run_store();
        let turn_id = accept(&db, "no-retry").turn_id;

        // Act
        let error = repo
            .increment_model_attempt(&turn_id)
            .expect_err("rejected");

        // Assert
        assert!(matches!(error, StorageError::Conflict(_)));
    }

    #[test]
    fn mark_output_published_is_idempotent() {
        // Arrange
        let (db, _dir) = test_db();
        let repo = db.turn_run_store();
        let turn_id = accept(&db, "pub").turn_id;

        // Act
        repo.mark_output_published(&turn_id).expect("first");
        repo.mark_output_published(&turn_id).expect("second");

        // Assert
        assert!(repo.get(&turn_id).expect("get").output_published);
    }

    #[test]
    fn fail_records_error_classification_and_finished_at() {
        // Arrange
        let (db, _dir) = test_db();
        let repo = db.turn_run_store();
        let turn_id = accept(&db, "fail").turn_id;
        repo.commit_input(&turn_id, "in").expect("commit input");

        // Act
        repo.fail(&turn_id, TurnRunState::Failed, "llm_error", "boom")
            .expect("fail");

        // Assert
        let run = repo.get(&turn_id).expect("get");
        assert_eq!(run.state, TurnRunState::Failed);
        assert_eq!(run.error_kind.as_deref(), Some("llm_error"));
        assert_eq!(run.error_message.as_deref(), Some("boom"));
        assert!(run.finished_at.is_some());
    }

    #[test]
    fn fail_uncertain_for_published_output() {
        // Arrange
        let (db, _dir) = test_db();
        let repo = db.turn_run_store();
        let turn_id = accept(&db, "unc").turn_id;
        repo.commit_input(&turn_id, "in").expect("commit input");
        repo.begin_model_iteration(&turn_id, 1, "h").expect("begin");
        repo.mark_output_published(&turn_id).expect("published");

        // Act: output was published -> uncertain, not failed.
        repo.fail(
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
        let repo = db.turn_run_store();
        let turn_id = accept(&db, "term").turn_id;
        repo.fail(&turn_id, TurnRunState::Failed, "x", "y")
            .expect("first fail");

        // Act
        let error = repo
            .fail(&turn_id, TurnRunState::Failed, "x", "y")
            .expect_err("terminal");

        // Assert
        assert!(matches!(error, StorageError::Conflict(_)));
    }

    #[test]
    fn fail_rejects_non_failure_target() {
        // Arrange
        let (db, _dir) = test_db();
        let repo = db.turn_run_store();
        let turn_id = accept(&db, "bad-target").turn_id;

        // Act
        let error = repo
            .fail(&turn_id, TurnRunState::Completed, "x", "y")
            .expect_err("non-failure target");

        // Assert
        assert!(matches!(error, StorageError::Conflict(_)));
    }

    #[test]
    fn recover_interrupted_marks_accepted_failed_and_mid_flight_uncertain() {
        // Arrange: one accepted (input not committed), one mid-flight.
        let (db, _dir) = test_db();
        let repo = db.turn_run_store();
        let accepted_id = accept(&db, "acc").turn_id;
        let mid_flight_id = accept(&db, "mid").turn_id;
        repo.commit_input(&mid_flight_id, "in").expect("commit");
        repo.begin_model_iteration(&mid_flight_id, 1, "h")
            .expect("begin");

        // Act
        let recovered = repo.recover_interrupted().expect("recover");

        // Assert
        assert_eq!(recovered.len(), 2);
        assert_eq!(state(&db, &accepted_id), TurnRunState::Failed);
        assert_eq!(state(&db, &mid_flight_id), TurnRunState::Uncertain);
    }

    #[test]
    fn recover_interrupted_leaves_terminal_states_untouched() {
        // Arrange
        let (db, _dir) = test_db();
        let repo = db.turn_run_store();
        let completed_id = accept(&db, "done").turn_id;
        repo.commit_input(&completed_id, "in").expect("commit");
        repo.begin_model_iteration(&completed_id, 1, "h")
            .expect("begin");
        repo.complete_model(&completed_id).expect("complete model");
        repo.complete(&completed_id, "final").expect("complete");

        // Act
        let recovered = repo.recover_interrupted().expect("recover");

        // Assert
        assert!(recovered.is_empty(), "terminal turns are not recovered");
        assert_eq!(state(&db, &completed_id), TurnRunState::Completed);
    }

    #[test]
    fn recover_interrupted_is_idempotent() {
        // Arrange
        let (db, _dir) = test_db();
        let repo = db.turn_run_store();
        let turn_id = accept(&db, "once").turn_id;
        repo.commit_input(&turn_id, "in").expect("commit");

        // Act
        repo.recover_interrupted().expect("first");
        let second = repo.recover_interrupted().expect("second");

        // Assert: the now-uncertain turn is terminal and not recovered again.
        assert!(second.is_empty());
        assert_eq!(state(&db, &turn_id), TurnRunState::Uncertain);
    }
}
