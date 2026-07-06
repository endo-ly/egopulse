use rusqlite::params;

use crate::error::StorageError;

use super::{ChatInfo, Database, PulseOutputKind, PulseRunStatus};

/// Parses a `pulse_runs` row into [`super::PulseRun`].
///
/// Only used by [`Database::get_pulse_run`] which is itself gated with
/// `#[cfg(test)]` — see that method for the rationale.
#[cfg(test)]
fn row_to_pulse_run(row: &rusqlite::Row<'_>) -> rusqlite::Result<super::PulseRun> {
    use super::PulseRun;
    use std::str::FromStr;

    let status = parse_row_enum!(row, 6, PulseRunStatus)?;
    let output_kind: Option<PulseOutputKind> = row
        .get::<_, Option<String>>(9)?
        .map(|s| PulseOutputKind::from_str(&s))
        .transpose()
        .map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(
                9,
                rusqlite::types::Type::Text,
                Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, e)),
            )
        })?;

    Ok(PulseRun {
        id: row.get(0)?,
        agent_id: row.get(1)?,
        intention_id: row.get(2)?,
        due_key: row.get(3)?,
        chat_id: row.get(4)?,
        message_id: row.get(5)?,
        status,
        started_at: row.get(7)?,
        finished_at: row.get(8)?,
        output_kind,
        output_text: row.get(10)?,
        error_message: row.get(11)?,
    })
}

// ---------------------------------------------------------------------------
// Pulse runs
// ---------------------------------------------------------------------------

impl Database {
    /// Inserts a new `pulse_run` row with status "running".
    ///
    /// Returns `Err(StorageError::Conflict)` when the `(agent_id,
    /// intention_id, due_key)` unique index is already satisfied.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::Conflict`] when the `(agent_id, intention_id, due_key)` unique index is already satisfied. Returns other [`StorageError`] variants on database connection or execution failures.
    pub(crate) fn try_create_pulse_run(
        &self,
        id: &str,
        agent_id: &str,
        intention_id: &str,
        due_key: &str,
    ) -> Result<(), StorageError> {
        let conn = self.get_conn()?;
        let status = PulseRunStatus::Running.to_string();
        let started_at = chrono::Utc::now().to_rfc3339();

        match conn.execute(
            "INSERT INTO pulse_runs
                 (id, agent_id, intention_id, due_key, chat_id, message_id,
                  status, started_at, finished_at, output_kind, output_text, error_message)
             VALUES (?1, ?2, ?3, ?4, NULL, NULL, ?5, ?6, NULL, NULL, NULL, NULL)",
            params![id, agent_id, intention_id, due_key, status, started_at],
        ) {
            Ok(_) => Ok(()),
            Err(rusqlite::Error::SqliteFailure(err, Some(msg)))
                if err.code == rusqlite::ErrorCode::ConstraintViolation =>
            {
                Err(StorageError::Conflict(format!(
                    "pulse_run due_key conflict: agent={agent_id} intention={intention_id} due={due_key}: {msg}"
                )))
            }
            Err(error) => Err(error.into()),
        }
    }

    /// Marks a pulse run as successful and records the output details.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::Conflict`] if the pulse run is not in the `Running` state.
    /// Returns other [`StorageError`] variants on database connection or execution failures.
    pub(crate) fn update_pulse_run_success(
        &self,
        id: &str,
        chat_id: Option<i64>,
        message_id: Option<&str>,
        output_kind: PulseOutputKind,
        output_text: &str,
    ) -> Result<(), StorageError> {
        let conn = self.get_conn()?;
        let finished_at = chrono::Utc::now().to_rfc3339();
        let status = PulseRunStatus::Success.to_string();
        let running = PulseRunStatus::Running.to_string();

        let changed = conn.execute(
            "UPDATE pulse_runs
             SET status = ?1, finished_at = ?2, chat_id = ?3, message_id = ?4,
                 output_kind = ?5, output_text = ?6
             WHERE id = ?7 AND status = ?8",
            params![
                status,
                finished_at,
                chat_id,
                message_id,
                output_kind.to_string(),
                output_text,
                id,
                running,
            ],
        )?;
        if changed == 0 {
            return Err(StorageError::Conflict(format!(
                "pulse_run:{id} is not running"
            )));
        }
        Ok(())
    }

    /// Marks a pulse run as failed with an error message.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::Conflict`] if the pulse run is not in the `Running` state.
    /// Returns other [`StorageError`] variants on database connection or execution failures.
    pub(crate) fn update_pulse_run_failed(
        &self,
        id: &str,
        error_message: &str,
    ) -> Result<(), StorageError> {
        let conn = self.get_conn()?;
        let finished_at = chrono::Utc::now().to_rfc3339();
        let status = PulseRunStatus::Failed.to_string();
        let running = PulseRunStatus::Running.to_string();

        let changed = conn.execute(
            "UPDATE pulse_runs SET status = ?1, finished_at = ?2, error_message = ?3
             WHERE id = ?4 AND status = ?5",
            params![status, finished_at, error_message, id, running],
        )?;
        if changed == 0 {
            return Err(StorageError::Conflict(format!(
                "pulse_run:{id} is not running"
            )));
        }
        Ok(())
    }

    /// Marks a pulse run as skipped with a reason.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::Conflict`] if the pulse run is not in the `Running` state.
    /// Returns other [`StorageError`] variants on database connection or execution failures.
    pub(crate) fn update_pulse_run_skipped(
        &self,
        id: &str,
        reason: &str,
    ) -> Result<(), StorageError> {
        let conn = self.get_conn()?;
        let finished_at = chrono::Utc::now().to_rfc3339();
        let status = PulseRunStatus::Skipped.to_string();
        let running = PulseRunStatus::Running.to_string();

        let changed = conn.execute(
            "UPDATE pulse_runs SET status = ?1, finished_at = ?2, error_message = ?3
             WHERE id = ?4 AND status = ?5",
            params![status, finished_at, reason, id, running],
        )?;
        if changed == 0 {
            return Err(StorageError::Conflict(format!(
                "pulse_run:{id} is not running"
            )));
        }
        Ok(())
    }

    /// Returns the `started_at` timestamp of the most recent `success` pulse
    /// run for `(agent_id, intention_id)`, or `None` when no successful run
    /// exists.
    ///
    /// Used by the Pulse scheduler to evaluate `interval` schedules relative
    /// to the last successful activation (see `docs/pulse.md` §4).
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] on database connection or query execution
    /// failures.
    pub(crate) fn get_last_success_started_at(
        &self,
        agent_id: &str,
        intention_id: &str,
    ) -> Result<Option<chrono::DateTime<chrono::Utc>>, StorageError> {
        use rusqlite::OptionalExtension;

        let conn = self.get_conn()?;
        let raw: Option<String> = conn
            .query_row(
                "SELECT started_at FROM pulse_runs
                 WHERE agent_id = ?1 AND intention_id = ?2 AND status = ?3
                 ORDER BY started_at DESC
                 LIMIT 1",
                params![agent_id, intention_id, PulseRunStatus::Success.to_string()],
                |row| row.get(0),
            )
            .optional()?;
        Ok(raw
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(&s).ok())
            .map(|dt| dt.with_timezone(&chrono::Utc)))
    }

    /// Returns `true` when any record exists for `(agent_id, intention_id,
    /// due_key)`, regardless of status.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] on database connection or query execution failures.
    pub(crate) fn has_pulse_due_run(
        &self,
        agent_id: &str,
        intention_id: &str,
        due_key: &str,
    ) -> Result<bool, StorageError> {
        let conn = self.get_conn()?;
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM pulse_runs
             WHERE agent_id = ?1 AND intention_id = ?2 AND due_key = ?3",
            params![agent_id, intention_id, due_key],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    /// Marks every `running` pulse run as `failed` with
    /// `error_message = "orphaned: process restarted"`. Called once at
    /// runtime startup to clean up rows left behind by a previous crash.
    ///
    /// Returns the number of rows reaped.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] on database connection or execution failures.
    pub(crate) fn reap_orphaned_pulse_runs(&self) -> Result<usize, StorageError> {
        let conn = self.get_conn()?;
        let finished_at = chrono::Utc::now().to_rfc3339();
        let status = PulseRunStatus::Failed.to_string();
        let running = PulseRunStatus::Running.to_string();
        let error_message = "orphaned: process restarted";

        let changed = conn.execute(
            "UPDATE pulse_runs
             SET status = ?1, finished_at = ?2, error_message = ?3
             WHERE status = ?4",
            params![status, finished_at, error_message, running],
        )?;
        Ok(changed)
    }

    /// Returns a single pulse run by its primary key.
    ///
    /// Only called from integration tests (pulse/output.rs) which verify
    /// that pulse output handling correctly persisted the run status.
    /// No runtime callers yet — gated with #[cfg(test)] to avoid dead_code
    /// warnings in production builds.  Remove the gate when a runtime caller
    /// is introduced.
    #[cfg(test)]
    pub(crate) fn get_pulse_run(&self, id: &str) -> Result<Option<super::PulseRun>, StorageError> {
        use rusqlite::OptionalExtension;

        let conn = self.get_conn()?;
        conn.query_row(
            "SELECT id, agent_id, intention_id, due_key, chat_id, message_id,
                    status, started_at, finished_at, output_kind, output_text, error_message
             FROM pulse_runs WHERE id = ?1",
            params![id],
            row_to_pulse_run,
        )
        .optional()
        .map_err(Into::into)
    }

    /// Returns agent chats matching the given channels, ordered by `last_message_time` descending.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] on database connection or query execution failures.
    pub(crate) fn get_agent_chats_by_recent(
        &self,
        agent_id: &str,
        channels: &[&str],
    ) -> Result<Vec<ChatInfo>, StorageError> {
        let conn = self.get_conn()?;

        let placeholders: Vec<&str> = channels.iter().map(|_| "?").collect();
        let sql = format!(
            "SELECT chat_id, channel, external_chat_id, chat_type, agent_id
             FROM chats
             WHERE agent_id = ? AND channel IN ({})
             ORDER BY last_message_time DESC",
            placeholders.join(", ")
        );

        let mut stmt = conn.prepare_cached(&sql)?;
        let params: Vec<&dyn rusqlite::types::ToSql> = {
            let mut p: Vec<&dyn rusqlite::types::ToSql> = Vec::with_capacity(1 + channels.len());
            p.push(&agent_id);
            for ch in channels {
                p.push(ch);
            }
            p
        };
        stmt.query_map(params.as_slice(), |row| {
            Ok(ChatInfo {
                chat_id: row.get(0)?,
                channel: row.get(1)?,
                external_chat_id: row.get(2)?,
                chat_type: row.get(3)?,
                agent_id: row.get(4)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(Into::into)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_db() -> (Database, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("runtime").join("egopulse.db");
        let db = Database::new(&db_path).expect("db");
        (db, dir)
    }

    fn create_test_pulse_run(
        db: &Database,
        id: &str,
        agent_id: &str,
        intention_id: &str,
        due_key: &str,
    ) {
        db.try_create_pulse_run(id, agent_id, intention_id, due_key)
            .expect("create pulse run");
    }

    /// Inserts a pulse run row with an explicit `started_at` and terminal
    /// `status`, bypassing the `Running` lifecycle. Used to verify
    /// `get_last_success_started_at` ordering deterministically.
    fn insert_terminal_pulse_run(
        db: &Database,
        id: &str,
        agent_id: &str,
        intention_id: &str,
        due_key: &str,
        started_at: &str,
        status: PulseRunStatus,
    ) {
        let conn = db.get_conn().expect("conn");
        conn.execute(
            "INSERT INTO pulse_runs
                 (id, agent_id, intention_id, due_key, chat_id, message_id,
                  status, started_at, finished_at, output_kind, output_text, error_message)
             VALUES (?1, ?2, ?3, ?4, NULL, NULL, ?5, ?6, ?6, NULL, NULL, NULL)",
            params![
                id,
                agent_id,
                intention_id,
                due_key,
                status.to_string(),
                started_at
            ],
        )
        .expect("insert terminal pulse run");
    }

    #[test]
    fn pulse_run_try_create_enforces_due_key_unique() {
        let (db, _dir) = test_db();

        // Arrange: create first run with a given due_key
        create_test_pulse_run(&db, "run-1", "agent-a", "int-1", "2025-01-01");

        // Act: attempt to create a second run with the same (agent_id, intention_id, due_key)
        let result = db.try_create_pulse_run("run-2", "agent-a", "int-1", "2025-01-01");

        // Assert: second insert should fail with Conflict
        assert!(matches!(result, Err(StorageError::Conflict(_))));
    }

    #[test]
    fn pulse_run_update_success_notify_records_message() {
        let (db, _dir) = test_db();

        // Arrange
        create_test_pulse_run(&db, "run-1", "agent-a", "int-1", "2025-01-01");

        // Act: update to success with Notify output kind
        db.update_pulse_run_success(
            "run-1",
            Some(42),
            Some("msg-abc"),
            PulseOutputKind::Notify,
            "notification text",
        )
        .expect("update success");

        // Assert: read back and verify all fields
        let run = db.get_pulse_run("run-1").expect("get").expect("exists");
        assert_eq!(run.status, PulseRunStatus::Success);
        assert_eq!(run.chat_id, Some(42));
        assert_eq!(run.message_id.as_deref(), Some("msg-abc"));
        assert_eq!(run.output_kind, Some(PulseOutputKind::Notify));
        assert_eq!(run.output_text.as_deref(), Some("notification text"));
        assert!(run.finished_at.is_some());
    }

    #[test]
    fn pulse_run_update_success_silent_records_pulse_ok() {
        let (db, _dir) = test_db();

        // Arrange
        create_test_pulse_run(&db, "run-1", "agent-a", "int-1", "2025-01-01");

        // Act
        db.update_pulse_run_success("run-1", None, None, PulseOutputKind::Silent, "PULSE_OK")
            .expect("update success");

        // Assert
        let run = db.get_pulse_run("run-1").expect("get").expect("exists");
        assert_eq!(run.status, PulseRunStatus::Success);
        assert_eq!(run.output_kind, Some(PulseOutputKind::Silent));
        assert_eq!(run.output_text.as_deref(), Some("PULSE_OK"));
        assert!(run.chat_id.is_none());
        assert!(run.message_id.is_none());
    }

    #[test]
    fn pulse_run_update_failed_records_error() {
        let (db, _dir) = test_db();

        // Arrange
        create_test_pulse_run(&db, "run-1", "agent-a", "int-1", "2025-01-01");

        // Act
        db.update_pulse_run_failed("run-1", "LLM timeout")
            .expect("update failed");

        // Assert
        let run = db.get_pulse_run("run-1").expect("get").expect("exists");
        assert_eq!(run.status, PulseRunStatus::Failed);
        assert_eq!(run.error_message.as_deref(), Some("LLM timeout"));
        assert!(run.finished_at.is_some());
    }

    #[test]
    fn pulse_run_update_skipped_records_reason() {
        let (db, _dir) = test_db();

        // Arrange
        create_test_pulse_run(&db, "run-1", "agent-a", "int-1", "2025-01-01");

        // Act
        db.update_pulse_run_skipped("run-1", "no new messages")
            .expect("update skipped");

        // Assert
        let run = db.get_pulse_run("run-1").expect("get").expect("exists");
        assert_eq!(run.status, PulseRunStatus::Skipped);
        assert_eq!(run.error_message.as_deref(), Some("no new messages"));
        assert!(run.finished_at.is_some());
    }

    #[test]
    fn pulse_run_has_due_run_detects_terminal_or_running_record() {
        let (db, _dir) = test_db();

        // Arrange & Act & Assert: no record initially
        assert!(
            !db.has_pulse_due_run("agent-a", "int-1", "2025-01-01")
                .expect("has")
        );

        // Create a running run
        create_test_pulse_run(&db, "run-1", "agent-a", "int-1", "2025-01-01");
        assert!(
            db.has_pulse_due_run("agent-a", "int-1", "2025-01-01")
                .expect("has running")
        );

        // Update to success — still returns true
        db.update_pulse_run_success("run-1", None, None, PulseOutputKind::Silent, "PULSE_OK")
            .expect("success");
        assert!(
            db.has_pulse_due_run("agent-a", "int-1", "2025-01-01")
                .expect("has success")
        );

        // Different due_key should not match
        assert!(
            !db.has_pulse_due_run("agent-a", "int-1", "2025-01-02")
                .expect("has other")
        );
    }

    #[test]
    fn pulse_run_failed_consumes_due_key_without_retry() {
        let (db, _dir) = test_db();

        // Arrange: create and fail a run
        create_test_pulse_run(&db, "run-1", "agent-a", "int-1", "2025-01-01");
        db.update_pulse_run_failed("run-1", "error")
            .expect("failed");

        // Act & Assert: has_pulse_due_run still returns true (due_key consumed)
        assert!(
            db.has_pulse_due_run("agent-a", "int-1", "2025-01-01")
                .expect("has")
        );

        // Attempting to create a second run with same due_key should conflict
        let result = db.try_create_pulse_run("run-2", "agent-a", "int-1", "2025-01-01");
        assert!(matches!(result, Err(StorageError::Conflict(_))));
    }

    #[test]
    fn reap_orphaned_pulse_runs_marks_running_as_failed() {
        let (db, _dir) = test_db();

        // Arrange: 2 running + 1 success
        create_test_pulse_run(&db, "run-1", "agent-a", "int-1", "2025-01-01");
        create_test_pulse_run(&db, "run-2", "agent-b", "int-2", "2025-01-02");
        create_test_pulse_run(&db, "run-3", "agent-c", "int-3", "2025-01-03");
        db.update_pulse_run_success("run-3", None, None, PulseOutputKind::Silent, "ok")
            .expect("success");

        // Act
        let reaped = db.reap_orphaned_pulse_runs().expect("reap");

        // Assert
        assert_eq!(reaped, 2, "should reap exactly the 2 running rows");

        let r1 = db.get_pulse_run("run-1").expect("get").expect("exists");
        assert_eq!(r1.status, PulseRunStatus::Failed);
        assert!(r1.finished_at.is_some());
        assert!(
            r1.error_message
                .as_deref()
                .is_some_and(|m| m.contains("orphaned")),
            "error_message should mention orphaned, got {:?}",
            r1.error_message
        );

        let r2 = db.get_pulse_run("run-2").expect("get").expect("exists");
        assert_eq!(r2.status, PulseRunStatus::Failed);
        assert!(r2.finished_at.is_some());
        assert!(
            r2.error_message
                .as_deref()
                .is_some_and(|m| m.contains("orphaned"))
        );

        let r3 = db.get_pulse_run("run-3").expect("get").expect("exists");
        assert_eq!(
            r3.status,
            PulseRunStatus::Success,
            "success row should be untouched"
        );
    }

    #[test]
    fn reap_orphaned_pulse_runs_preserves_terminal_status() {
        let (db, _dir) = test_db();

        // Arrange: one each of success/failed/skipped, no running
        create_test_pulse_run(&db, "run-s", "agent-a", "int-x", "2025-01-01");
        db.update_pulse_run_success("run-s", None, None, PulseOutputKind::Silent, "ok")
            .expect("success");

        create_test_pulse_run(&db, "run-f", "agent-a", "int-x", "2025-01-02");
        db.update_pulse_run_failed("run-f", "err").expect("failed");

        create_test_pulse_run(&db, "run-k", "agent-a", "int-x", "2025-01-03");
        db.update_pulse_run_skipped("run-k", "skip")
            .expect("skipped");

        // Act
        let reaped = db.reap_orphaned_pulse_runs().expect("reap");

        // Assert
        assert_eq!(reaped, 0, "no running rows should be reaped");

        let s = db.get_pulse_run("run-s").expect("get").expect("exists");
        assert_eq!(s.status, PulseRunStatus::Success);
        assert!(s.error_message.is_none());

        let f = db.get_pulse_run("run-f").expect("get").expect("exists");
        assert_eq!(f.status, PulseRunStatus::Failed);
        assert_eq!(f.error_message.as_deref(), Some("err"));

        let k = db.get_pulse_run("run-k").expect("get").expect("exists");
        assert_eq!(k.status, PulseRunStatus::Skipped);
        assert_eq!(k.error_message.as_deref(), Some("skip"));
    }

    #[test]
    fn reap_orphaned_pulse_runs_returns_zero_on_empty() {
        let (db, _dir) = test_db();

        // Act
        let reaped = db.reap_orphaned_pulse_runs().expect("reap");

        // Assert
        assert_eq!(reaped, 0, "empty DB should return 0");
    }

    // -----------------------------------------------------------------------
    // get_last_success_started_at
    // -----------------------------------------------------------------------

    #[test]
    fn get_last_success_started_at_returns_none_when_no_runs() {
        let (db, _dir) = test_db();

        let result = db
            .get_last_success_started_at("agent-a", "int-1")
            .expect("query");

        assert!(result.is_none());
    }

    #[test]
    fn get_last_success_started_at_ignores_non_success_rows() {
        let (db, _dir) = test_db();

        // Arrange: only running / failed / skipped rows exist
        create_test_pulse_run(&db, "run-r", "agent-a", "int-1", "2025-01-01");
        insert_terminal_pulse_run(
            &db,
            "run-f",
            "agent-a",
            "int-1",
            "2025-01-02",
            "2025-01-02T00:00:00Z",
            PulseRunStatus::Failed,
        );
        insert_terminal_pulse_run(
            &db,
            "run-k",
            "agent-a",
            "int-1",
            "2025-01-03",
            "2025-01-03T00:00:00Z",
            PulseRunStatus::Skipped,
        );

        let result = db
            .get_last_success_started_at("agent-a", "int-1")
            .expect("query");

        assert!(result.is_none(), "non-success rows must not be considered");
    }

    #[test]
    fn get_last_success_started_at_returns_latest_success_started_at() {
        let (db, _dir) = test_db();

        // Arrange: two success runs, the later one must win
        insert_terminal_pulse_run(
            &db,
            "run-old",
            "agent-a",
            "int-1",
            "2025-01-01",
            "2025-01-01T09:00:00Z",
            PulseRunStatus::Success,
        );
        insert_terminal_pulse_run(
            &db,
            "run-new",
            "agent-a",
            "int-1",
            "2025-01-05",
            "2025-01-05T09:00:00Z",
            PulseRunStatus::Success,
        );
        // A newer failed run must NOT shadow the older success
        insert_terminal_pulse_run(
            &db,
            "run-failed",
            "agent-a",
            "int-1",
            "2025-01-06",
            "2025-01-06T09:00:00Z",
            PulseRunStatus::Failed,
        );

        let result = db
            .get_last_success_started_at("agent-a", "int-1")
            .expect("query");

        let expected = chrono::DateTime::parse_from_rfc3339("2025-01-05T09:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        assert_eq!(result, Some(expected));
    }

    #[test]
    fn get_last_success_started_at_scopes_to_agent_and_intention() {
        let (db, _dir) = test_db();

        insert_terminal_pulse_run(
            &db,
            "run-a",
            "agent-a",
            "int-1",
            "2025-01-01",
            "2025-01-01T09:00:00Z",
            PulseRunStatus::Success,
        );
        insert_terminal_pulse_run(
            &db,
            "run-b",
            "agent-b",
            "int-1",
            "2025-01-02",
            "2025-01-02T09:00:00Z",
            PulseRunStatus::Success,
        );
        insert_terminal_pulse_run(
            &db,
            "run-c",
            "agent-a",
            "int-2",
            "2025-01-03",
            "2025-01-03T09:00:00Z",
            PulseRunStatus::Success,
        );

        assert_eq!(
            db.get_last_success_started_at("agent-a", "int-1")
                .expect("query"),
            Some(
                chrono::DateTime::parse_from_rfc3339("2025-01-01T09:00:00Z")
                    .unwrap()
                    .with_timezone(&chrono::Utc)
            )
        );
        assert_eq!(
            db.get_last_success_started_at("agent-b", "int-1")
                .expect("query"),
            Some(
                chrono::DateTime::parse_from_rfc3339("2025-01-02T09:00:00Z")
                    .unwrap()
                    .with_timezone(&chrono::Utc)
            )
        );
        assert!(
            db.get_last_success_started_at("agent-a", "int-9")
                .expect("query")
                .is_none()
        );
    }

    // ---------------------------------------------------------------------------
    // Episode event tests
    // ---------------------------------------------------------------------------
}
