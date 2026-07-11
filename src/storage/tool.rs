use rusqlite::params;

use crate::error::StorageError;
use crate::llm::calibration::CalibrationObservation;

use super::{Database, LlmUsageLogEntry, ToolCall};

impl Database {
    /// Logs an LLM usage entry and returns the inserted row's rowid.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] on database connection or execution failures.
    pub(crate) fn log_llm_usage(&self, entry: &LlmUsageLogEntry<'_>) -> Result<i64, StorageError> {
        let conn = self.get_conn()?;
        let total_tokens = entry.input_tokens.saturating_add(entry.output_tokens);
        let created_at = chrono::Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO llm_usage_logs
                (chat_id, caller_channel, provider, model, input_tokens, output_tokens, total_tokens, request_kind, estimated_tokens, has_tools, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                entry.chat_id,
                entry.caller_channel,
                entry.provider,
                entry.model,
                entry.input_tokens,
                entry.output_tokens,
                total_tokens,
                entry.request_kind,
                entry.estimated_tokens,
                entry.has_tools,
                created_at,
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

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

    /// Loads recent observations for calibration factor rebuild.
    ///
    /// Returns [`CalibrationObservation`]s ordered oldest-first within each
    /// key, limited to the most recent `limit_per_key` rows per key. Rows with
    /// non-positive `estimated_tokens` or `input_tokens` are excluded so the
    /// caller can replay a clean history through the calibrator.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] on database connection or query failures.
    pub(crate) fn load_calibration_observations(
        &self,
        limit_per_key: usize,
    ) -> Result<Vec<CalibrationObservation>, StorageError> {
        let conn = self.get_conn()?;
        let mut stmt = conn.prepare_cached(
            "WITH ranked AS (
                 SELECT provider, model, request_kind, has_tools,
                        estimated_tokens, input_tokens, created_at,
                        ROW_NUMBER() OVER (
                            PARTITION BY provider, model, request_kind, has_tools
                            ORDER BY created_at DESC
                        ) AS rn
                 FROM llm_usage_logs
                 WHERE estimated_tokens > 0 AND input_tokens > 0
             )
             SELECT provider, model, request_kind, has_tools,
                    estimated_tokens, input_tokens, created_at
             FROM ranked
             WHERE rn <= ?1
             ORDER BY created_at DESC",
        )?;
        let mut observations: Vec<CalibrationObservation> = stmt
            .query_map(params![limit_per_key as i64], |row| {
                Ok(CalibrationObservation {
                    provider: row.get(0)?,
                    model: row.get(1)?,
                    request_kind: row.get(2)?,
                    has_tools: row.get::<_, i64>(3)? != 0,
                    estimated_tokens: row.get::<_, i64>(4)? as usize,
                    input_tokens: row.get(5)?,
                    created_at: row.get(6)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        // SQL returns newest-first; reverse to oldest-first so the calibrator
        // can replay each key's history chronologically. `created_at` is kept
        // so callers can merge observations from multiple databases in true
        // chronological order.
        observations.reverse();
        Ok(observations)
    }
}

#[cfg(test)]
impl Database {
    pub(crate) fn get_llm_usage_summary(
        &self,
        chat_id: Option<i64>,
    ) -> Result<(i64, i64, i64, i64), StorageError> {
        let conn = self.get_conn()?;
        let mut sql = String::from(
            "SELECT COUNT(*), COALESCE(SUM(input_tokens), 0), COALESCE(SUM(output_tokens), 0), COALESCE(SUM(total_tokens), 0)
             FROM llm_usage_logs WHERE 1=1",
        );
        let mut params: Vec<&dyn rusqlite::types::ToSql> = Vec::new();
        if let Some(ref cid) = chat_id {
            sql.push_str(" AND chat_id = ?");
            params.push(cid as &dyn rusqlite::types::ToSql);
        }
        let result = conn.query_row(&sql, params.as_slice(), |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })?;
        Ok(result)
    }

    /// Inserts a `tool_calls` row with the legacy composite identity
    /// `(id, chat_id, message_id)`. Used by tests that need to seed ledger
    /// rows for read-path assertions without exercising the full claim flow.
    #[cfg(test)]
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

    fn test_db() -> (Database, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("runtime").join("egopulse.db");
        let db = Database::new(&db_path).expect("db");
        (db, dir)
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
    fn log_llm_usage_inserts_record() {
        let (db, _dir) = test_db();

        db.log_llm_usage(&LlmUsageLogEntry {
            chat_id: 100,
            caller_channel: "tui",
            provider: "openai",
            model: "gpt-4",
            input_tokens: 100,
            output_tokens: 50,
            request_kind: "agent_loop",
            estimated_tokens: 0,
            has_tools: false,
        })
        .expect("log usage");

        let conn = db.get_conn().expect("pool");
        let (total_tokens, created_at): (i64, String) = conn
            .query_row(
                "SELECT total_tokens, created_at FROM llm_usage_logs WHERE chat_id = 100",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("row");

        assert_eq!(total_tokens, 150);
        assert!(created_at.contains('T'));
    }

    #[test]
    fn log_llm_usage_stores_request_kind() {
        let (db, _dir) = test_db();

        for kind in &["agent_loop", "compaction", "sleep_batch", "pulse"] {
            db.log_llm_usage(&LlmUsageLogEntry {
                chat_id: 0,
                caller_channel: "test",
                provider: "test",
                model: "test",
                input_tokens: 1,
                output_tokens: 1,
                request_kind: kind,
                estimated_tokens: 0,
                has_tools: false,
            })
            .expect("log usage");
        }

        let conn = db.get_conn().expect("pool");
        let kinds: Vec<String> = conn
            .prepare("SELECT request_kind FROM llm_usage_logs ORDER BY rowid")
            .expect("prepare")
            .query_map([], |row| row.get(0))
            .expect("query")
            .map(|r| r.expect("row"))
            .collect();

        assert_eq!(
            kinds,
            &["agent_loop", "compaction", "sleep_batch", "pulse"].map(|s| s.to_string())
        );
    }

    fn log_observation(db: &Database, input_tokens: i64, estimated: i64, has_tools: bool) {
        db.log_llm_usage(&LlmUsageLogEntry {
            chat_id: 1,
            caller_channel: "test",
            provider: "p",
            model: "m",
            input_tokens,
            output_tokens: 0,
            request_kind: "agent_loop",
            estimated_tokens: estimated,
            has_tools,
        })
        .expect("log usage");
        // created_at has sub-ms resolution, but a small sleep guarantees a
        // strictly increasing ordering for the deterministic assertions below.
        std::thread::sleep(std::time::Duration::from_millis(2));
    }

    #[test]
    fn load_calibration_observations_returns_persisted_observations_oldest_first() {
        // Arrange: two observations for the same key, written in order
        let (db, _dir) = test_db();
        log_observation(&db, 200, 100, true);
        log_observation(&db, 300, 100, true);

        // Act
        let observations = db.load_calibration_observations(30).expect("load");

        // Assert: oldest-first so the calibrator can replay chronologically
        assert_eq!(observations.len(), 2);
        assert_eq!(observations[0].input_tokens, 200);
        assert_eq!(observations[1].input_tokens, 300);
        assert_eq!(observations[0].estimated_tokens, 100);
        assert!(observations[0].has_tools);
        assert_eq!(observations[0].provider, "p");
        assert_eq!(observations[0].request_kind, "agent_loop");
    }

    #[test]
    fn load_calibration_observations_limits_to_most_recent_per_key() {
        // Arrange: three observations for one key
        let (db, _dir) = test_db();
        log_observation(&db, 100, 50, false);
        log_observation(&db, 200, 50, false);
        log_observation(&db, 300, 50, false);

        // Act: cap at the two most recent
        let observations = db.load_calibration_observations(2).expect("load");

        // Assert: only the two newest (200, 300), oldest-first
        assert_eq!(observations.len(), 2);
        assert_eq!(observations[0].input_tokens, 200);
        assert_eq!(observations[1].input_tokens, 300);
    }

    #[test]
    fn load_calibration_observations_excludes_rows_with_zero_estimate() {
        // Arrange: one valid observation and one with a zero estimate
        let (db, _dir) = test_db();
        log_observation(&db, 200, 100, true);
        log_observation(&db, 999, 0, false);

        // Act
        let observations = db.load_calibration_observations(30).expect("load");

        // Assert: only the row with a positive estimate is returned
        assert_eq!(observations.len(), 1);
        assert_eq!(observations[0].input_tokens, 200);
    }

    #[test]
    fn load_calibration_observations_keeps_keys_separate() {
        // Arrange: observations for two distinct keys
        let (db, _dir) = test_db();
        db.log_llm_usage(&LlmUsageLogEntry {
            chat_id: 1,
            caller_channel: "test",
            provider: "p",
            model: "m",
            input_tokens: 200,
            output_tokens: 0,
            request_kind: "agent_loop",
            estimated_tokens: 100,
            has_tools: true,
        })
        .expect("log agent_loop");
        db.log_llm_usage(&LlmUsageLogEntry {
            chat_id: 1,
            caller_channel: "test",
            provider: "p",
            model: "m",
            input_tokens: 150,
            output_tokens: 0,
            request_kind: "compaction",
            estimated_tokens: 100,
            has_tools: false,
        })
        .expect("log compaction");

        // Act
        let observations = db.load_calibration_observations(30).expect("load");

        // Assert: both keys appear, each with its own shape
        assert_eq!(observations.len(), 2);
        assert!(
            observations.iter().any(|o| {
                o.request_kind == "agent_loop" && o.has_tools && o.input_tokens == 200
            })
        );
        assert!(
            observations.iter().any(|o| {
                o.request_kind == "compaction" && !o.has_tools && o.input_tokens == 150
            })
        );
    }
}
