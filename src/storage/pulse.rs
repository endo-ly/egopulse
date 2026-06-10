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

    // ---------------------------------------------------------------------------
    // Episode event tests
    // ---------------------------------------------------------------------------
}
