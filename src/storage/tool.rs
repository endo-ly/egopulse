use rusqlite::params;

use crate::error::StorageError;

use super::{Database, LlmUsageLogEntry, ToolCall};

impl Database {
    /// Persists a [`ToolCall`] record.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] on database connection or execution failures.
    pub(crate) fn store_tool_call(&self, tool_call: &ToolCall) -> Result<(), StorageError> {
        let conn = self.get_conn()?;
        conn.execute(
            "INSERT INTO tool_calls (id, chat_id, message_id, tool_name, tool_input, tool_output, timestamp)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                tool_call.id,
                tool_call.chat_id,
                tool_call.message_id,
                tool_call.tool_name,
                tool_call.tool_input,
                tool_call.tool_output,
                tool_call.timestamp,
            ],
        )?;
        Ok(())
    }

    /// Updates the output field of a tool call identified by `(chat_id, message_id, id)`.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::NotFound`] when no matching row exists.
    /// Returns other [`StorageError`] variants on database connection or execution failures.
    pub(crate) fn update_tool_call_output_for_message(
        &self,
        chat_id: i64,
        message_id: &str,
        id: &str,
        output: &str,
    ) -> Result<(), StorageError> {
        let conn = self.get_conn()?;
        let rows_updated = conn.execute(
            "UPDATE tool_calls
             SET tool_output = ?1
             WHERE chat_id = ?2 AND message_id = ?3 AND id = ?4",
            params![output, chat_id, message_id, id],
        )?;
        if rows_updated == 0 {
            return Err(StorageError::NotFound(format!(
                "tool_call:{chat_id}:{message_id}:{id}"
            )));
        }
        Ok(())
    }

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
                (chat_id, caller_channel, provider, model, input_tokens, output_tokens, total_tokens, request_kind, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                entry.chat_id,
                entry.caller_channel,
                entry.provider,
                entry.model,
                entry.input_tokens,
                entry.output_tokens,
                total_tokens,
                entry.request_kind,
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
            "SELECT id, chat_id, message_id, tool_name, tool_input, tool_output, timestamp
             FROM tool_calls WHERE chat_id = ?1 ORDER BY timestamp",
        )?;
        let calls = stmt
            .query_map(params![chat_id], |row| {
                Ok(ToolCall {
                    id: row.get(0)?,
                    chat_id: row.get(1)?,
                    message_id: row.get(2)?,
                    tool_name: row.get(3)?,
                    tool_input: row.get(4)?,
                    tool_output: row.get(5)?,
                    timestamp: row.get(6)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(calls)
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
    fn update_tool_call_output_fails_when_tool_call_is_missing() {
        let (db, _dir) = test_db();
        let chat_id = db
            .resolve_or_create_chat_id("web", "web:message-1", Some("message-1"), "web", "default")
            .expect("create chat");

        let error = db
            .update_tool_call_output_for_message(
                chat_id,
                "message-1",
                "missing-tool-call",
                "output",
            )
            .expect_err("missing tool call should fail");

        assert!(matches!(error, StorageError::NotFound(_)));
    }

    #[test]
    fn update_tool_call_output_updates_existing_record() {
        let (db, _dir) = test_db();
        let chat_id = db
            .resolve_or_create_chat_id("web", "web:message-1", Some("message-1"), "web", "default")
            .expect("create chat");
        let tool_call = ToolCall {
            id: "tool-1".to_string(),
            chat_id,
            message_id: "message-1".to_string(),
            tool_name: "fetch".to_string(),
            tool_input: "{}".to_string(),
            tool_output: None,
            timestamp: "2024-01-01T00:00:00Z".to_string(),
        };

        db.store_tool_call(&tool_call).expect("store tool call");
        db.update_tool_call_output_for_message(chat_id, "message-1", "tool-1", "done")
            .expect("update tool call");

        let conn = db.get_conn().expect("pool");
        let output: String = conn
            .query_row(
                "SELECT tool_output FROM tool_calls WHERE chat_id = ?1 AND message_id = ?2 AND id = ?3",
                rusqlite::params![chat_id, "message-1", "tool-1"],
                |row| row.get(0),
            )
            .expect("query");
        assert_eq!(output, "done");
    }

    #[test]
    fn tool_call_ids_are_scoped_by_message() {
        let (db, _dir) = test_db();
        let chat_id = db
            .resolve_or_create_chat_id("web", "web:message-1", Some("message-1"), "web", "default")
            .expect("create chat");
        let first = ToolCall {
            id: "tool-1".to_string(),
            chat_id,
            message_id: "message-1".to_string(),
            tool_name: "fetch".to_string(),
            tool_input: "{}".to_string(),
            tool_output: None,
            timestamp: "2024-01-01T00:00:00Z".to_string(),
        };
        let second = ToolCall {
            message_id: "message-2".to_string(),
            timestamp: "2024-01-01T00:00:01Z".to_string(),
            ..first.clone()
        };

        db.store_tool_call(&first).expect("store first tool call");
        db.store_tool_call(&second)
            .expect("store second tool call with duplicate provider id");
        db.update_tool_call_output_for_message(chat_id, "message-2", "tool-1", "done")
            .expect("update scoped output");

        let conn = db.get_conn().expect("pool");
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM tool_calls WHERE chat_id = ?1",
                rusqlite::params![chat_id],
                |row| row.get(0),
            )
            .expect("count");
        assert_eq!(count, 2);
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
}
