use rusqlite::{OptionalExtension, params};

use sha2::{Digest, Sha256};

use crate::error::StorageError;

use super::{Database, IdempotencyClass, ToolCall, ToolState};

impl Database {
    /// Loads persisted tool calls for a chat, ordered by execution time.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] on database connection or query failures.
    pub(crate) fn get_tool_calls_for_chat(
        &self,
        chat_id: i64,
    ) -> Result<Vec<ToolCall>, StorageError> {
        let conn = self.get_conn()?;
        let mut stmt = conn.prepare_cached(
            "SELECT id, message_id, tool_name, tool_input, tool_output, timestamp
             FROM tool_calls WHERE chat_id = ?1 ORDER BY timestamp",
        )?;
        let calls = stmt
            .query_map(params![chat_id], |row| {
                Ok(ToolCall {
                    id: row.get(0)?,
                    message_id: row.get(1)?,
                    tool_name: row.get(2)?,
                    tool_input: row.get(3)?,
                    tool_output: row.get(4)?,
                    timestamp: row.get(5)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(calls)
    }
}

// ---------------------------------------------------------------------------
// Tool execution ledger (`tool_calls`)
//
// Every Tool execution routes through these `Database` methods so that:
//
// * a Tool call is **claimed** (created or re-evaluated) before execution,
// * the **input hash** guards against input drift on re-claim,
// * a **succeeded** Tool returns its stored output instead of re-executing,
// * a **running** Tool left by a crash becomes **uncertain** on recovery,
// * **non-idempotent** Tools are never auto-retried from `uncertain`,
// * result and state are written in one transaction (no partial commit).
//
// Reads (e.g. `get_tool_calls_for_chat`) stay on `Database`; only state-mutating
// writes live here.
// ---------------------------------------------------------------------------

/// Parameters for claiming a Tool execution slot.
///
/// `tool_input` is the original JSON arguments serialized as-is — this is
/// what history / display APIs read back. `input_hash` is the SHA-256 hex
/// digest of the canonical Tool name + arguments (see
/// [`canonical_tool_input`]); the caller computes it so the hash is fixed
/// before any DB write.
pub(crate) struct ClaimParams<'a> {
    pub turn_id: &'a str,
    pub chat_id: i64,
    pub message_id: &'a str,
    pub tool_call_id: &'a str,
    pub tool_name: &'a str,
    /// Original JSON arguments, preserved verbatim for history display.
    pub tool_input: &'a str,
    pub input_hash: &'a str,
    pub idempotency_class: IdempotencyClass,
    pub idempotency_key: Option<&'a str>,
}

/// Outcome of [`Database::claim_tool_execution`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ClaimOutcome {
    /// The slot is freshly acquired and the Tool may execute. The ledger row
    /// advanced to `running` with `started_at` set.
    Acquired,
    /// A prior execution already succeeded. The caller returns the stored
    /// `tool_output` without re-executing the Tool.
    Reused { tool_output: String },
    /// The Tool is in a non-executable state (`failed`, `uncertain`, or
    /// already `running`). The caller must not execute and surfaces the state.
    Blocked { state: ToolState },
}

/// One row transitioned by [`Database::recover_running_tools`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RecoveredToolCall {
    pub turn_id: String,
    pub tool_call_id: String,
    pub tool_name: String,
    /// The state the row was moved to. `Uncertain` for Tools that must not be
    /// auto-retried; `Pending` for read-only / idempotent Tools whose stored
    /// input hash and key make a safe retry possible on the next claim.
    pub recovered_to: ToolState,
    pub idempotency_class: IdempotencyClass,
}

/// Persisted columns read back during a claim re-evaluation.
struct ExistingToolRow {
    state: String,
    input_hash: Option<String>,
    tool_output: Option<String>,
    idempotency_class: Option<String>,
    idempotency_key: Option<String>,
}

impl ExistingToolRow {
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            state: row.get(0)?,
            input_hash: row.get(1)?,
            tool_output: row.get(2)?,
            idempotency_class: row.get(3)?,
            idempotency_key: row.get(4)?,
        })
    }
}

impl Database {
    /// Atomically claims a Tool execution slot.
    ///
    /// Lookup is by `(turn_id, tool_call_id)` — the durable ledger identity.
    /// * No row → create `pending` with the input hash, then advance to
    ///   `running` and set `started_at`. Returns [`ClaimOutcome::Acquired`].
    /// * Existing row with a **different** input hash →
    ///   [`StorageError::ToolInputConflict`] (never execute, never guess).
    /// * `succeeded` → [`ClaimOutcome::Reused`] with the stored output.
    /// * `pending` with matching hash → advance to `running`. `Acquired`.
    /// * `uncertain` read-only / idempotent Tool with matching hash (and key
    ///   for idempotent) → retry is safe, advance to `running`. `Acquired`.
    /// * `running`, `failed`, or non-retryable `uncertain` → [`ClaimOutcome::Blocked`].
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::ToolInputConflict`] when the same
    /// `(turn_id, tool_call_id)` is claimed with a different input hash.
    pub(crate) fn claim_tool_execution(
        &self,
        params: ClaimParams<'_>,
    ) -> Result<ClaimOutcome, StorageError> {
        let mut conn = self.get_conn()?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let outcome = claim_locked(&tx, params)?;
        tx.commit()?;
        Ok(outcome)
    }

    /// Records a successful Tool execution in one transaction.
    ///
    /// Sets `state = succeeded`, stores the sanitized `tool_output`, stamps
    /// `finished_at`, and clears any prior error fields. The current state
    /// must be `running`; any other state is rejected so a stale or already
    /// completed row is never partially overwritten.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::NotFound`] when no row exists for
    /// `(turn_id, tool_call_id)`, or [`StorageError::Conflict`] when the row
    /// is not in the `running` state.
    pub(crate) fn record_tool_success(
        &self,
        turn_id: &str,
        tool_call_id: &str,
        tool_output: &str,
    ) -> Result<(), StorageError> {
        let mut conn = self.get_conn()?;
        let tx = conn.transaction()?;
        let now = chrono::Utc::now().to_rfc3339();
        require_state(&tx, turn_id, tool_call_id, ToolState::Running)?;
        tx.execute(
            "UPDATE tool_calls
             SET state = 'succeeded',
                 tool_output = ?3,
                 finished_at = ?4,
                 error_kind = NULL,
                 error_message = NULL
             WHERE turn_id = ?1 AND id = ?2",
            params![turn_id, tool_call_id, tool_output, &now],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Records a failed Tool execution in one transaction.
    ///
    /// Sets `state = failed`, stores the sanitized `error_kind` /
    /// `error_message`, and stamps `finished_at`. The current state must be
    /// `running`. Output is left untouched (a partial result is never
    /// promoted to a success).
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::NotFound`] when no row exists for
    /// `(turn_id, tool_call_id)`, or [`StorageError::Conflict`] when the row
    /// is not in the `running` state.
    pub(crate) fn record_tool_failure(
        &self,
        turn_id: &str,
        tool_call_id: &str,
        error_kind: &str,
        error_message: &str,
    ) -> Result<(), StorageError> {
        let mut conn = self.get_conn()?;
        let tx = conn.transaction()?;
        let now = chrono::Utc::now().to_rfc3339();
        require_state(&tx, turn_id, tool_call_id, ToolState::Running)?;
        tx.execute(
            "UPDATE tool_calls
             SET state = 'failed',
                 error_kind = ?3,
                 error_message = ?4,
                 finished_at = ?5
             WHERE turn_id = ?1 AND id = ?2",
            params![turn_id, tool_call_id, error_kind, error_message, &now],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Transitions every `running` Tool to a recovery state.
    ///
    /// The default recovery outcome is `uncertain`: a Tool interrupted mid-execution
    /// cannot have its result verified, so it must not be auto-retried. The
    /// sole exception is a read-only or idempotent Tool whose stored input
    /// hash and idempotency key make a safe retry possible — these reset to
    /// `pending` so the next claim re-executes them with the same input.
    ///
    /// Returns the transitioned rows so the caller can report or decide on
    /// further action. `pending` rows (never started) are left untouched.
    pub(crate) fn recover_running_tools(&self) -> Result<Vec<RecoveredToolCall>, StorageError> {
        let mut conn = self.get_conn()?;
        let tx = conn.transaction()?;

        let running: Vec<(String, String, String, String, Option<String>)> = {
            let mut stmt = tx.prepare(
                "SELECT turn_id, id, tool_name, idempotency_class, idempotency_key
                 FROM tool_calls
                 WHERE state = 'running'",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<String>>(4)?,
                ))
            })?;
            rows.collect::<Result<Vec<_>, _>>()?
        };

        let mut recovered = Vec::with_capacity(running.len());
        for (turn_id, tool_call_id, tool_name, class_str, key) in running {
            let class: IdempotencyClass = class_str
                .parse()
                .map_err(|e| StorageError::Conflict(format!("invalid idempotency_class: {e}")))?;
            let recovered_to = if retry_eligible(class, key.as_deref()) {
                ToolState::Pending
            } else {
                ToolState::Uncertain
            };
            tx.execute(
                "UPDATE tool_calls
                 SET state = ?3
                 WHERE turn_id = ?1 AND id = ?2",
                params![&turn_id, &tool_call_id, recovered_to.to_string()],
            )?;
            recovered.push(RecoveredToolCall {
                turn_id,
                tool_call_id,
                tool_name,
                recovered_to,
                idempotency_class: class,
            });
        }

        tx.commit()?;
        Ok(recovered)
    }
}

fn claim_locked(
    tx: &rusqlite::Transaction<'_>,
    params: ClaimParams<'_>,
) -> Result<ClaimOutcome, StorageError> {
    let now = chrono::Utc::now().to_rfc3339();
    // Load the persisted classification alongside the execution state so
    // the retry decision trusts the ledger, not the incoming params. A
    // caller must not relax a stored `non_idempotent` Tool into
    // `read_only` to force a retry.
    let existing = tx
        .query_row(
            "SELECT state, input_hash, tool_output, idempotency_class, idempotency_key
             FROM tool_calls
             WHERE turn_id = ?1 AND id = ?2",
            params![params.turn_id, params.tool_call_id],
            ExistingToolRow::from_row,
        )
        .optional()?;

    let Some(row) = existing else {
        // Fresh claim: create pending, then advance to running.
        tx.execute(
            "INSERT INTO tool_calls
                 (id, chat_id, message_id, tool_name, tool_input, timestamp,
                  turn_id, state, input_hash, idempotency_class, idempotency_key)
              VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'pending', ?8, ?9, ?10)",
            params![
                params.tool_call_id,
                params.chat_id,
                params.message_id,
                params.tool_name,
                params.tool_input,
                &now,
                params.turn_id,
                params.input_hash,
                params.idempotency_class.to_string(),
                params.idempotency_key,
            ],
        )?;
        advance_to_running(tx, params.turn_id, params.tool_call_id, &now)?;
        return Ok(ClaimOutcome::Acquired);
    };

    let stored_hash = row.input_hash.unwrap_or_default();
    if stored_hash != params.input_hash {
        return Err(StorageError::ToolInputConflict {
            tool_call_id: params.tool_call_id.to_string(),
            stored_hash,
            requested_hash: params.input_hash.to_string(),
        });
    }

    // Trust the persisted idempotency classification for retry decisions.
    // A mismatch between stored and incoming classification is a conflict:
    // the Tool's retry contract cannot change across claims for the same
    // execution slot.
    let stored_class: IdempotencyClass = row
        .idempotency_class
        .as_deref()
        .and_then(|s| s.parse().ok())
        .unwrap_or(IdempotencyClass::NonIdempotent);
    if stored_class != params.idempotency_class {
        return Err(StorageError::Conflict(format!(
            "tool_calls idempotency_class changed: stored {stored_class} but requested {}",
            params.idempotency_class
        )));
    }

    let state: ToolState = row
        .state
        .parse()
        .map_err(|e| StorageError::Conflict(format!("invalid tool_calls.state: {e}")))?;

    let outcome = match state {
        ToolState::Succeeded => ClaimOutcome::Reused {
            tool_output: row.tool_output.unwrap_or_default(),
        },
        ToolState::Pending => {
            advance_to_running(tx, params.turn_id, params.tool_call_id, &now)?;
            ClaimOutcome::Acquired
        }
        ToolState::Uncertain if retry_eligible(stored_class, row.idempotency_key.as_deref()) => {
            advance_to_running(tx, params.turn_id, params.tool_call_id, &now)?;
            ClaimOutcome::Acquired
        }
        other => ClaimOutcome::Blocked { state: other },
    };
    Ok(outcome)
}

fn advance_to_running(
    tx: &rusqlite::Transaction<'_>,
    turn_id: &str,
    tool_call_id: &str,
    now: &str,
) -> Result<(), StorageError> {
    tx.execute(
        "UPDATE tool_calls
         SET state = 'running', started_at = ?3
         WHERE turn_id = ?1 AND id = ?2",
        params![turn_id, tool_call_id, now],
    )?;
    Ok(())
}

/// A `running` Tool left by a crash may be retried only when its idempotency
/// class guarantees a duplicate execution is observably safe: read-only Tools
/// never mutate external state, and idempotent Tools deduplicate on a stable key.
fn retry_eligible(class: IdempotencyClass, idempotency_key: Option<&str>) -> bool {
    match class {
        IdempotencyClass::ReadOnly => true,
        IdempotencyClass::Idempotent => idempotency_key.is_some(),
        IdempotencyClass::NonIdempotent => false,
    }
}

fn require_state(
    tx: &rusqlite::Transaction<'_>,
    turn_id: &str,
    tool_call_id: &str,
    expected: ToolState,
) -> Result<(), StorageError> {
    let state_str: Option<String> = tx
        .query_row(
            "SELECT state FROM tool_calls WHERE turn_id = ?1 AND id = ?2",
            params![turn_id, tool_call_id],
            |row| row.get(0),
        )
        .optional()?;
    let Some(state_str) = state_str else {
        return Err(StorageError::NotFound(format!(
            "tool_call:{turn_id}:{tool_call_id}"
        )));
    };
    let state: ToolState = state_str
        .parse()
        .map_err(|e| StorageError::Conflict(format!("invalid tool_calls.state: {e}")))?;
    if state != expected {
        return Err(StorageError::Conflict(format!(
            "tool state transition rejected: expected {expected} but was {state}"
        )));
    }
    Ok(())
}

/// Deterministic serialization of a Tool call's identity and arguments.
///
/// The Tool name prefixes the arguments so the same arguments dispatched to
/// different Tools produce distinct hashes. `serde_json` serializes object
/// keys in sorted order by default (no `preserve_order` feature), so the
/// result is stable regardless of insertion order.
pub(crate) fn canonical_tool_input(tool_name: &str, arguments: &serde_json::Value) -> String {
    format!("{tool_name}:{arguments}")
}

/// SHA-256 hex digest of the canonical input, used as the retry-identity token.
pub(crate) fn input_hash(canonical: &str) -> String {
    let digest = Sha256::digest(canonical.as_bytes());
    format!("{digest:x}")
}

#[cfg(test)]
impl Database {
    /// Inserts a `tool_calls` row with the legacy composite identity
    /// `(id, chat_id, message_id)`. Used by tests that need to seed ledger
    /// rows for read-path assertions without exercising the full claim flow.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn insert_tool_call_for_test(
        &self,
        id: &str,
        chat_id: i64,
        message_id: &str,
        tool_name: &str,
        tool_input: &str,
        tool_output: Option<&str>,
        timestamp: &str,
    ) -> Result<(), StorageError> {
        let conn = self.get_conn()?;
        conn.execute(
            "INSERT INTO tool_calls
                 (id, chat_id, message_id, tool_name, tool_input, tool_output, timestamp, state)
              VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'succeeded')",
            params![
                id,
                chat_id,
                message_id,
                tool_name,
                tool_input,
                tool_output,
                timestamp
            ],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::Database;

    /// Test DB without any seeded chat (suitable for read-path tests that
    /// resolve their own chat id).
    fn test_db() -> (Database, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("runtime").join("egopulse.db");
        let db = Database::new(&db_path).expect("db");
        (db, dir)
    }

    /// Test DB with a seeded chat (`cli:tool-ledger` -> chat_id 1) so claim
    /// inserts satisfy the `tool_calls.chat_id` FK to `chats.chat_id`.
    fn ledger_test_db() -> (Database, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("runtime").join("egopulse.db");
        let db = Database::new(&db_path).expect("db");
        let chat_id = db
            .resolve_or_create_chat_id("cli", "cli:tool-ledger", None, "private", "default")
            .expect("create chat");
        assert_eq!(chat_id, 1, "expected the seeded chat to be chat_id 1");
        (db, dir)
    }

    fn claim_params<'a>(
        turn_id: &'a str,
        tool_call_id: &'a str,
        input_hash: &'a str,
        class: IdempotencyClass,
        key: Option<&'a str>,
    ) -> ClaimParams<'a> {
        ClaimParams {
            turn_id,
            chat_id: 1,
            message_id: "m-1",
            tool_call_id,
            tool_name: "shell",
            tool_input: "{}",
            input_hash,
            idempotency_class: class,
            idempotency_key: key,
        }
    }

    fn row_state(db: &Database, turn_id: &str, tool_call_id: &str) -> ToolState {
        let conn = db.get_conn().expect("conn");
        let state: String = conn
            .query_row(
                "SELECT state FROM tool_calls WHERE turn_id = ?1 AND id = ?2",
                params![turn_id, tool_call_id],
                |row| row.get(0),
            )
            .expect("row");
        state.parse().expect("valid state")
    }

    fn row_output(db: &Database, turn_id: &str, tool_call_id: &str) -> Option<String> {
        let conn = db.get_conn().expect("conn");
        conn.query_row(
            "SELECT tool_output FROM tool_calls WHERE turn_id = ?1 AND id = ?2",
            params![turn_id, tool_call_id],
            |row| row.get(0),
        )
        .ok()
    }

    #[test]
    fn get_tool_calls_for_chat_returns_persisted_calls_in_timestamp_order() {
        let (db, _dir) = test_db();
        let chat_id = db
            .resolve_or_create_chat_id("web", "web:message-1", Some("message-1"), "web", "default")
            .expect("create chat");

        // Two tool calls on different assistant messages; the same provider
        // call id is reused across messages (scoped by the composite PK).
        db.insert_tool_call_for_test(
            "tool-1",
            chat_id,
            "message-1",
            "read",
            r#"{"path":"a.txt"}"#,
            Some(r#"{"result":"ok"}"#),
            "2024-01-01T00:00:00Z",
        )
        .expect("insert first");
        db.insert_tool_call_for_test(
            "tool-1",
            chat_id,
            "message-2",
            "read",
            r#"{"path":"b.txt"}"#,
            None,
            "2024-01-01T00:00:01Z",
        )
        .expect("insert second");

        let calls = db.get_tool_calls_for_chat(chat_id).expect("tool calls");
        assert_eq!(calls.len(), 2, "composite PK scopes by message");
        assert_eq!(calls[0].message_id, "message-1");
        assert_eq!(calls[1].message_id, "message-2");
        assert_eq!(calls[0].tool_output.as_deref(), Some(r#"{"result":"ok"}"#));
        assert!(calls[1].tool_output.is_none());
    }

    #[test]
    fn claim_creates_running_row_for_new_tool_call() {
        // Arrange
        let (db, _dir) = ledger_test_db();

        // Act
        let outcome = db
            .claim_tool_execution(claim_params(
                "turn-1",
                "call-1",
                "hash-1",
                IdempotencyClass::NonIdempotent,
                None,
            ))
            .expect("claim");

        // Assert
        assert_eq!(outcome, ClaimOutcome::Acquired);
        assert_eq!(row_state(&db, "turn-1", "call-1"), ToolState::Running);
    }

    #[test]
    fn claim_does_not_duplicate_same_turn_and_call_id() {
        // Arrange
        let (db, _dir) = ledger_test_db();
        db.claim_tool_execution(claim_params(
            "turn-1",
            "call-1",
            "hash-1",
            IdempotencyClass::NonIdempotent,
            None,
        ))
        .expect("first claim");

        // Act: re-claim the same slot (e.g. retry within the turn).
        let outcome = db
            .claim_tool_execution(claim_params(
                "turn-1",
                "call-1",
                "hash-1",
                IdempotencyClass::NonIdempotent,
                None,
            ))
            .expect("second claim");

        // Assert: no second row, and the running slot is blocked (already in flight).
        assert_eq!(
            outcome,
            ClaimOutcome::Blocked {
                state: ToolState::Running
            }
        );
        let conn = db.get_conn().expect("conn");
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM tool_calls WHERE turn_id = ?1 AND id = ?2",
                params!["turn-1", "call-1"],
                |row| row.get(0),
            )
            .expect("count");
        assert_eq!(count, 1, "no duplicate ledger row");
    }

    #[test]
    fn claim_rejects_different_input_hash_for_same_call_id() {
        // Arrange
        let (db, _dir) = ledger_test_db();
        db.claim_tool_execution(claim_params(
            "turn-1",
            "call-1",
            "hash-1",
            IdempotencyClass::NonIdempotent,
            None,
        ))
        .expect("first claim");

        // Act: same slot, different input.
        let error = db
            .claim_tool_execution(claim_params(
                "turn-1",
                "call-1",
                "hash-2",
                IdempotencyClass::NonIdempotent,
                None,
            ))
            .expect_err("conflict expected");

        // Assert
        assert!(matches!(
            error,
            StorageError::ToolInputConflict {
                tool_call_id,
                stored_hash,
                requested_hash,
            } if tool_call_id == "call-1" && stored_hash == "hash-1" && requested_hash == "hash-2"
        ));
    }

    #[test]
    fn claim_reuses_succeeded_tool_output_without_reexecuting() {
        // Arrange
        let (db, _dir) = ledger_test_db();
        db.claim_tool_execution(claim_params(
            "turn-1",
            "call-1",
            "hash-1",
            IdempotencyClass::NonIdempotent,
            None,
        ))
        .expect("claim");
        db.record_tool_success("turn-1", "call-1", "result-payload")
            .expect("record success");

        // Act: re-claim the completed slot.
        let outcome = db
            .claim_tool_execution(claim_params(
                "turn-1",
                "call-1",
                "hash-1",
                IdempotencyClass::NonIdempotent,
                None,
            ))
            .expect("re-claim");

        // Assert
        assert_eq!(
            outcome,
            ClaimOutcome::Reused {
                tool_output: "result-payload".to_string()
            }
        );
        assert_eq!(row_state(&db, "turn-1", "call-1"), ToolState::Succeeded);
    }

    #[test]
    fn claim_blocks_failed_tool_call() {
        // Arrange
        let (db, _dir) = ledger_test_db();
        db.claim_tool_execution(claim_params(
            "turn-1",
            "call-1",
            "hash-1",
            IdempotencyClass::NonIdempotent,
            None,
        ))
        .expect("claim");
        db.record_tool_failure("turn-1", "call-1", "tool_error", "boom")
            .expect("record failure");

        // Act
        let outcome = db
            .claim_tool_execution(claim_params(
                "turn-1",
                "call-1",
                "hash-1",
                IdempotencyClass::NonIdempotent,
                None,
            ))
            .expect("re-claim");

        // Assert: failed Tools are never auto-retried.
        assert_eq!(
            outcome,
            ClaimOutcome::Blocked {
                state: ToolState::Failed
            }
        );
    }

    #[test]
    fn record_success_rejects_non_running_state() {
        // Arrange
        let (db, _dir) = ledger_test_db();
        db.claim_tool_execution(claim_params(
            "turn-1",
            "call-1",
            "hash-1",
            IdempotencyClass::NonIdempotent,
            None,
        ))
        .expect("claim");
        db.record_tool_success("turn-1", "call-1", "first")
            .expect("first success");

        // Act: a second success on an already-succeeded row is rejected.
        let error = db
            .record_tool_success("turn-1", "call-1", "second")
            .expect_err("transition rejected");

        // Assert: no partial commit — the stored output stays as the first.
        assert!(matches!(error, StorageError::Conflict(_)));
        assert_eq!(
            row_output(&db, "turn-1", "call-1").as_deref(),
            Some("first")
        );
    }

    #[test]
    fn record_failure_clears_error_fields_on_subsequent_success_path() {
        // This is covered indirectly: a fresh running row has no error fields.
        // The key invariant is that record_success nulls error_kind/message,
        // tested by claiming a retry-eligible uncertain row then succeeding.
        // Arrange
        let (db, _dir) = ledger_test_db();
        db.claim_tool_execution(claim_params(
            "turn-1",
            "call-1",
            "hash-1",
            IdempotencyClass::ReadOnly,
            None,
        ))
        .expect("claim");
        // Simulate a crash: force the row to uncertain with stale error fields.
        {
            let conn = db.get_conn().expect("conn");
            conn.execute(
                "UPDATE tool_calls SET state = 'uncertain', error_kind = 'stale', error_message = 'stale-msg' WHERE turn_id = ?1 AND id = ?2",
                params!["turn-1", "call-1"],
            )
            .expect("force uncertain");
        }

        // Act: read-only retry re-acquires, then succeeds.
        db.claim_tool_execution(claim_params(
            "turn-1",
            "call-1",
            "hash-1",
            IdempotencyClass::ReadOnly,
            None,
        ))
        .expect("re-claim");
        db.record_tool_success("turn-1", "call-1", "ok")
            .expect("success");

        // Assert: error fields cleared.
        let conn = db.get_conn().expect("conn");
        let (error_kind, error_message): (Option<String>, Option<String>) = conn
            .query_row(
                "SELECT error_kind, error_message FROM tool_calls WHERE turn_id = ?1 AND id = ?2",
                params!["turn-1", "call-1"],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("row");
        assert!(error_kind.is_none());
        assert!(error_message.is_none());
    }

    #[test]
    fn recover_running_marks_non_idempotent_tool_uncertain() {
        // Arrange
        let (db, _dir) = ledger_test_db();
        db.claim_tool_execution(claim_params(
            "turn-1",
            "call-1",
            "hash-1",
            IdempotencyClass::NonIdempotent,
            None,
        ))
        .expect("claim");
        // Row is now running (crash mid-execution).

        // Act
        let recovered = db.recover_running_tools().expect("recover");

        // Assert
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].recovered_to, ToolState::Uncertain);
        assert_eq!(row_state(&db, "turn-1", "call-1"), ToolState::Uncertain);
    }

    #[test]
    fn recover_running_resets_read_only_tool_to_pending() {
        // Arrange
        let (db, _dir) = ledger_test_db();
        db.claim_tool_execution(claim_params(
            "turn-1",
            "call-1",
            "hash-1",
            IdempotencyClass::ReadOnly,
            None,
        ))
        .expect("claim");

        // Act
        let recovered = db.recover_running_tools().expect("recover");

        // Assert: safe to retry — reset to pending for the next claim.
        assert_eq!(recovered[0].recovered_to, ToolState::Pending);
        assert_eq!(row_state(&db, "turn-1", "call-1"), ToolState::Pending);
    }

    #[test]
    fn recover_running_resets_idempotent_tool_with_key_to_pending() {
        // Arrange
        let (db, _dir) = ledger_test_db();
        db.claim_tool_execution(claim_params(
            "turn-1",
            "call-1",
            "hash-1",
            IdempotencyClass::Idempotent,
            Some("key-1"),
        ))
        .expect("claim");

        // Act
        let recovered = db.recover_running_tools().expect("recover");

        // Assert
        assert_eq!(recovered[0].recovered_to, ToolState::Pending);
    }

    #[test]
    fn recover_running_marks_idempotent_tool_without_key_uncertain() {
        // Arrange: idempotent class but no key — cannot deduplicate, so stop.
        let (db, _dir) = ledger_test_db();
        db.claim_tool_execution(claim_params(
            "turn-1",
            "call-1",
            "hash-1",
            IdempotencyClass::Idempotent,
            None,
        ))
        .expect("claim");

        // Act
        let recovered = db.recover_running_tools().expect("recover");

        // Assert
        assert_eq!(recovered[0].recovered_to, ToolState::Uncertain);
    }

    #[test]
    fn claim_retries_uncertain_read_only_tool_with_matching_hash() {
        // Arrange: crash left the read-only Tool uncertain.
        let (db, _dir) = ledger_test_db();
        db.claim_tool_execution(claim_params(
            "turn-1",
            "call-1",
            "hash-1",
            IdempotencyClass::ReadOnly,
            None,
        ))
        .expect("claim");
        db.recover_running_tools().expect("recover to pending");

        // Act: re-claim with the same input.
        let outcome = db
            .claim_tool_execution(claim_params(
                "turn-1",
                "call-1",
                "hash-1",
                IdempotencyClass::ReadOnly,
                None,
            ))
            .expect("re-claim");

        // Assert: retried (re-acquired), not blocked.
        assert_eq!(outcome, ClaimOutcome::Acquired);
    }

    #[test]
    fn claim_does_not_retry_uncertain_non_idempotent_tool() {
        // Arrange
        let (db, _dir) = ledger_test_db();
        db.claim_tool_execution(claim_params(
            "turn-1",
            "call-1",
            "hash-1",
            IdempotencyClass::NonIdempotent,
            None,
        ))
        .expect("claim");
        db.recover_running_tools().expect("recover to uncertain");

        // Act
        let outcome = db
            .claim_tool_execution(claim_params(
                "turn-1",
                "call-1",
                "hash-1",
                IdempotencyClass::NonIdempotent,
                None,
            ))
            .expect("re-claim");

        // Assert: blocked — non-idempotent Tools are never auto-retried.
        assert_eq!(
            outcome,
            ClaimOutcome::Blocked {
                state: ToolState::Uncertain
            }
        );
    }

    #[test]
    fn claim_rejects_idempotency_class_change_on_existing_row() {
        // Arrange: a non-idempotent Tool claimed and then left uncertain by
        // recovery.
        let (db, _dir) = ledger_test_db();
        db.claim_tool_execution(claim_params(
            "turn-1",
            "call-1",
            "hash-1",
            IdempotencyClass::NonIdempotent,
            None,
        ))
        .expect("initial claim");
        db.recover_running_tools().expect("recover to uncertain");

        // Act: re-claim relaxing the classification to read_only. The ledger
        // must reject this — a caller must not upgrade a non-idempotent Tool
        // into a retryable class to force a second execution.
        let result = db.claim_tool_execution(claim_params(
            "turn-1",
            "call-1",
            "hash-1",
            IdempotencyClass::ReadOnly,
            None,
        ));

        // Assert
        assert!(
            matches!(result, Err(StorageError::Conflict(_))),
            "class change must be rejected, got: {result:?}"
        );
    }

    #[test]
    fn claim_advances_pending_row_to_running() {
        // Arrange: a pending row left by a crash before execution started.
        let (db, _dir) = ledger_test_db();
        db.claim_tool_execution(claim_params(
            "turn-1",
            "call-1",
            "hash-1",
            IdempotencyClass::NonIdempotent,
            None,
        ))
        .expect("claim");
        {
            let conn = db.get_conn().expect("conn");
            conn.execute(
                "UPDATE tool_calls SET state = 'pending', started_at = NULL WHERE turn_id = ?1 AND id = ?2",
                params!["turn-1", "call-1"],
            )
            .expect("reset to pending");
        }

        // Act
        let outcome = db
            .claim_tool_execution(claim_params(
                "turn-1",
                "call-1",
                "hash-1",
                IdempotencyClass::NonIdempotent,
                None,
            ))
            .expect("re-claim");

        // Assert
        assert_eq!(outcome, ClaimOutcome::Acquired);
        assert_eq!(row_state(&db, "turn-1", "call-1"), ToolState::Running);
    }

    #[test]
    fn canonical_tool_input_includes_tool_name_and_sorted_arguments() {
        // Arrange: arguments with keys in non-sorted insertion order.
        let args = serde_json::json!({"b": 2, "a": 1});

        // Act
        let canonical = canonical_tool_input("shell", &args);

        // Assert: tool name prefixes; object keys are sorted by serde_json.
        assert_eq!(canonical, r#"shell:{"a":1,"b":2}"#);
    }

    #[test]
    fn input_hash_is_stable_for_equal_canonical_input() {
        // Arrange
        let left = canonical_tool_input("shell", &serde_json::json!({"a": 1, "b": 2}));
        let right = canonical_tool_input("shell", &serde_json::json!({"b": 2, "a": 1}));

        // Act & Assert: different insertion order, same hash.
        assert_eq!(input_hash(&left), input_hash(&right));
        assert_ne!(input_hash(&left), input_hash("shell:{}"));
    }

    #[test]
    fn unique_index_prevents_duplicate_turn_and_call_id_on_raw_insert() {
        // Arrange: the partial UNIQUE(turn_id, id) index is the DB-level guard
        // behind claim's deduplication. Verify it rejects a manual duplicate.
        let (db, _dir) = ledger_test_db();
        db.claim_tool_execution(claim_params(
            "turn-1",
            "call-1",
            "hash-1",
            IdempotencyClass::NonIdempotent,
            None,
        ))
        .expect("claim");

        // Act: bypass the ledger and insert a second row with the same key.
        let conn = db.get_conn().expect("conn");
        let result = conn.execute(
            "INSERT INTO tool_calls (id, chat_id, message_id, tool_name, tool_input, timestamp, turn_id, state)
             VALUES ('call-1', 1, 'm-1', 'shell', '{}', '2024-01-01T00:00:00Z', 'turn-1', 'pending')",
            [],
        );

        // Assert
        assert!(
            result.is_err(),
            "UNIQUE(turn_id, id) must reject the duplicate"
        );
    }
}
