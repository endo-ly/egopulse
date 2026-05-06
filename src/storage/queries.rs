use rusqlite::{OptionalExtension, params};

use crate::error::StorageError;

use super::{
    ChatInfo, Database, LlmUsageLogEntry, SessionSnapshot, SessionSummary, StoredMessage, ToolCall,
};

fn row_to_stored_message(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredMessage> {
    Ok(StoredMessage {
        id: row.get(0)?,
        chat_id: row.get(1)?,
        sender_name: row.get(2)?,
        content: row.get(3)?,
        is_from_bot: row.get::<_, i32>(4)? != 0,
        timestamp: row.get(5)?,
    })
}

// ---------------------------------------------------------------------------
// Chats
// ---------------------------------------------------------------------------

impl Database {
    pub(crate) fn resolve_chat_id(
        &self,
        channel: &str,
        external_chat_id: &str,
    ) -> Result<Option<i64>, StorageError> {
        let conn = self.lock_conn()?;
        match conn.query_row(
            "SELECT chat_id FROM chats WHERE channel = ?1 AND external_chat_id = ?2 LIMIT 1",
            params![channel, external_chat_id],
            |row| row.get::<_, i64>(0),
        ) {
            Ok(chat_id) => Ok(Some(chat_id)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    pub(crate) fn get_chat_by_id(&self, chat_id: i64) -> Result<Option<ChatInfo>, StorageError> {
        let conn = self.lock_conn()?;
        match conn.query_row(
            "SELECT channel, external_chat_id, chat_type, agent_id FROM chats WHERE chat_id = ?1 LIMIT 1",
            params![chat_id],
            |row| {
                Ok(ChatInfo {
                    chat_id,
                    channel: row.get(0)?,
                    external_chat_id: row.get(1)?,
                    chat_type: row.get(2)?,
                    agent_id: row.get(3)?,
                })
            },
        ) {
            Ok(info) => Ok(Some(info)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    pub(crate) fn resolve_or_create_chat_id(
        &self,
        channel: &str,
        external_chat_id: &str,
        chat_title: Option<&str>,
        chat_type: &str,
        agent_id: &str,
    ) -> Result<i64, StorageError> {
        let conn = self.lock_conn()?;
        let now = chrono::Utc::now().to_rfc3339();

        match conn.query_row(
            "SELECT chat_id FROM chats WHERE channel = ?1 AND external_chat_id = ?2 LIMIT 1",
            params![channel, external_chat_id],
            |row| row.get::<_, i64>(0),
        ) {
            Ok(chat_id) => {
                conn.execute(
                    "UPDATE chats
                     SET chat_title = COALESCE(?2, chat_title),
                         chat_type = ?3,
                         last_message_time = ?4,
                         agent_id = COALESCE(agent_id, ?5)
                     WHERE chat_id = ?1",
                    params![chat_id, chat_title, chat_type, now, agent_id],
                )?;
                return Ok(chat_id);
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => {}
            Err(error) => return Err(error.into()),
        }

        conn.execute(
            "INSERT INTO chats(chat_title, chat_type, last_message_time, channel, external_chat_id, agent_id)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(channel, external_chat_id) DO UPDATE SET
                chat_title = COALESCE(excluded.chat_title, chats.chat_title),
                chat_type = excluded.chat_type,
                last_message_time = excluded.last_message_time,
                agent_id = COALESCE(chats.agent_id, excluded.agent_id)",
            params![chat_title, chat_type, now, channel, external_chat_id, agent_id],
        )?;
        conn.query_row(
            "SELECT chat_id FROM chats WHERE channel = ?1 AND external_chat_id = ?2 LIMIT 1",
            params![channel, external_chat_id],
            |row| row.get::<_, i64>(0),
        )
        .map_err(Into::into)
    }

    pub(crate) fn list_sessions(&self) -> Result<Vec<SessionSummary>, StorageError> {
        let conn = self.lock_conn()?;
        let mut stmt = conn.prepare(
            "SELECT
                c.chat_id,
                c.channel,
                c.external_chat_id,
                c.chat_title,
                c.last_message_time,
                (
                    SELECT m.content
                    FROM messages m
                    WHERE m.chat_id = c.chat_id
                    ORDER BY m.timestamp DESC
                    LIMIT 1
                ) AS last_message_preview,
                c.agent_id
             FROM chats c
             ORDER BY c.last_message_time DESC, c.chat_id DESC",
        )?;
        stmt.query_map([], |row| {
            let channel: String = row.get(1)?;
            let external_chat_id: String = row.get(2)?;
            let chat_title: Option<String> = row.get(3)?;
            Ok(SessionSummary {
                chat_id: row.get(0)?,
                channel: channel.clone(),
                surface_thread: logical_session_thread(
                    &channel,
                    &external_chat_id,
                    chat_title.as_deref(),
                ),
                chat_title,
                last_message_time: row.get(4)?,
                last_message_preview: row.get(5)?,
                agent_id: row.get(6)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(Into::into)
    }
}

fn logical_session_thread(
    channel: &str,
    external_chat_id: &str,
    chat_title: Option<&str>,
) -> String {
    if let Some(title) = chat_title.map(str::trim).filter(|value| !value.is_empty()) {
        return title.to_string();
    }

    let prefix = format!("{channel}:");
    if let Some(stripped) = external_chat_id.strip_prefix(&prefix) {
        let trimmed = stripped.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }

    external_chat_id.to_string()
}

// ---------------------------------------------------------------------------
// Messages
// ---------------------------------------------------------------------------

impl Database {
    pub(crate) fn get_recent_messages(
        &self,
        chat_id: i64,
        limit: usize,
    ) -> Result<Vec<StoredMessage>, StorageError> {
        let conn = self.lock_conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, chat_id, sender_name, content, is_from_bot, timestamp
             FROM messages
             WHERE chat_id = ?1
             ORDER BY timestamp DESC
             LIMIT ?2",
        )?;

        let mut messages = stmt
            .query_map(params![chat_id, limit as i64], row_to_stored_message)?
            .collect::<Result<Vec<_>, _>>()?;
        messages.reverse();
        Ok(messages)
    }

    pub(crate) fn get_all_messages(
        &self,
        chat_id: i64,
    ) -> Result<Vec<StoredMessage>, StorageError> {
        let conn = self.lock_conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, chat_id, sender_name, content, is_from_bot, timestamp
             FROM messages
             WHERE chat_id = ?1
             ORDER BY timestamp ASC",
        )?;
        stmt.query_map(params![chat_id], row_to_stored_message)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(Into::into)
    }
}

// ---------------------------------------------------------------------------
// Sessions
// ---------------------------------------------------------------------------

impl Database {
    pub(crate) fn save_session(
        &self,
        chat_id: i64,
        messages_json: &str,
    ) -> Result<(), StorageError> {
        let conn = self.lock_conn()?;
        let now = chrono::Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO sessions (chat_id, messages_json, updated_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(chat_id) DO UPDATE SET
                messages_json = ?2,
                updated_at = ?3",
            params![chat_id, messages_json, now],
        )?;
        Ok(())
    }

    pub(crate) fn clear_session(&self, chat_id: i64) -> Result<(), StorageError> {
        let mut conn = self.lock_conn()?;
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM sessions WHERE chat_id = ?1", params![chat_id])?;
        tx.execute("DELETE FROM messages WHERE chat_id = ?1", params![chat_id])?;
        tx.commit()?;
        Ok(())
    }

    pub(crate) fn store_message_with_session(
        &self,
        message: &StoredMessage,
        messages_json: &str,
        expected_updated_at: Option<&str>,
    ) -> Result<String, StorageError> {
        let mut conn = self.lock_conn()?;
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT OR REPLACE INTO messages (id, chat_id, sender_name, content, is_from_bot, timestamp)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                message.id,
                message.chat_id,
                message.sender_name,
                message.content,
                message.is_from_bot as i32,
                message.timestamp,
            ],
        )?;
        let now = chrono::Utc::now().to_rfc3339();
        if let Some(expected_updated_at) = expected_updated_at {
            let updated = tx.execute(
                "UPDATE sessions
                 SET messages_json = ?2,
                     updated_at = ?3
                 WHERE chat_id = ?1
                   AND updated_at = ?4",
                params![message.chat_id, messages_json, now, expected_updated_at],
            )?;
            if updated == 0 {
                tx.rollback()?;
                return Err(StorageError::SessionSnapshotConflict);
            }
        } else {
            let inserted = tx.execute(
                "INSERT INTO sessions (chat_id, messages_json, updated_at)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(chat_id) DO NOTHING",
                params![message.chat_id, messages_json, now],
            )?;
            if inserted == 0 {
                tx.rollback()?;
                return Err(StorageError::SessionSnapshotConflict);
            }
        }
        tx.commit()?;
        Ok(now)
    }

    pub(crate) fn load_session_snapshot(
        &self,
        chat_id: i64,
        limit: usize,
    ) -> Result<SessionSnapshot, StorageError> {
        let mut conn = self.lock_conn()?;
        let tx = conn.transaction()?;

        let session = tx
            .query_row(
                "SELECT messages_json, updated_at FROM sessions WHERE chat_id = ?1",
                params![chat_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?;

        let recent_messages = {
            let mut stmt = tx.prepare(
                "SELECT id, chat_id, sender_name, content, is_from_bot, timestamp
                 FROM messages
                 WHERE chat_id = ?1
                 ORDER BY timestamp DESC
                 LIMIT ?2",
            )?;
            let mut messages = stmt
                .query_map(params![chat_id, limit as i64], row_to_stored_message)?
                .collect::<Result<Vec<_>, _>>()?;
            messages.reverse();
            messages
        };

        tx.commit()?;

        let (messages_json, updated_at) = session
            .map(|(messages_json, updated_at)| (Some(messages_json), Some(updated_at)))
            .unwrap_or((None, None));

        Ok(SessionSnapshot {
            messages_json,
            updated_at,
            recent_messages,
        })
    }
}

// ---------------------------------------------------------------------------
// Tool calls & LLM usage
// ---------------------------------------------------------------------------

impl Database {
    pub(crate) fn store_tool_call(&self, tool_call: &ToolCall) -> Result<(), StorageError> {
        let conn = self.lock_conn()?;
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

    pub(crate) fn update_tool_call_output_for_message(
        &self,
        chat_id: i64,
        message_id: &str,
        id: &str,
        output: &str,
    ) -> Result<(), StorageError> {
        let conn = self.lock_conn()?;
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

    pub(crate) fn log_llm_usage(&self, entry: &LlmUsageLogEntry<'_>) -> Result<i64, StorageError> {
        let conn = self.lock_conn()?;
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
}

#[cfg(test)]
impl Database {
    pub(crate) fn get_tool_calls_for_chat(
        &self,
        chat_id: i64,
    ) -> Result<Vec<ToolCall>, StorageError> {
        let conn = self.lock_conn()?;
        let mut stmt = conn.prepare(
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

    pub(crate) fn get_llm_usage_summary(
        &self,
        chat_id: Option<i64>,
        _since: Option<&str>,
        _request_kind: Option<&str>,
    ) -> Result<(i64, i64, i64, i64), StorageError> {
        let conn = self.lock_conn()?;
        let mut sql = String::from(
            "SELECT COUNT(*), COALESCE(SUM(input_tokens), 0), COALESCE(SUM(output_tokens), 0), COALESCE(SUM(total_tokens), 0)
             FROM llm_usage_logs WHERE 1=1",
        );
        if let Some(cid) = chat_id {
            sql.push_str(&format!(" AND chat_id = {cid}"));
        }
        let result = conn.query_row(&sql, [], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })?;
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use crate::error::StorageError;

    use super::*;

    fn test_db() -> (Database, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("runtime").join("egopulse.db");
        let db = Database::new(&db_path).expect("db");
        (db, dir)
    }

    fn store_msg(db: &Database, id: &str, chat_id: i64, content: &str, ts: &str) {
        let conn = db.conn.lock().expect("lock");
        conn.execute(
            "INSERT OR REPLACE INTO messages (id, chat_id, sender_name, content, is_from_bot, timestamp)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![id, chat_id, "alice", content, 0, ts],
        )
        .expect("store message");
    }

    #[test]
    fn message_full_lifecycle() {
        let (db, _dir) = test_db();

        for index in 0..5 {
            store_msg(
                &db,
                &format!("chat1_msg{index}"),
                100,
                &format!("chat1 message {index}"),
                &format!("2024-01-01T00:00:{index:02}Z"),
            );
        }

        for index in 0..3 {
            store_msg(
                &db,
                &format!("chat2_msg{index}"),
                200,
                &format!("chat2 message {index}"),
                &format!("2024-01-01T00:00:{index:02}Z"),
            );
        }

        let chat1_messages = db.get_all_messages(100).expect("chat1 messages");
        assert_eq!(chat1_messages.len(), 5);
        assert_eq!(chat1_messages[0].content, "chat1 message 0");
        assert_eq!(chat1_messages[4].content, "chat1 message 4");

        let chat2_messages = db.get_all_messages(200).expect("chat2 messages");
        assert_eq!(chat2_messages.len(), 3);

        let recent = db.get_recent_messages(100, 2).expect("recent messages");
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0].content, "chat1 message 3");
        assert_eq!(recent[1].content, "chat1 message 4");

        assert!(db.get_all_messages(999).expect("empty chat").is_empty());
    }

    #[test]
    fn session_lifecycle() {
        let (db, _dir) = test_db();

        assert!(
            db.load_session_snapshot(100, 10)
                .expect("missing session")
                .messages_json
                .is_none()
        );

        let json1 = r#"[{"role":"user","content":"hello"}]"#;
        db.save_session(100, json1).expect("save session");

        let snapshot = db.load_session_snapshot(100, 10).expect("load session");
        assert_eq!(snapshot.messages_json.as_deref(), Some(json1));
        assert!(snapshot.updated_at.is_some());
        let first_updated_at = snapshot.updated_at.unwrap();
        assert!(!first_updated_at.is_empty());

        std::thread::sleep(std::time::Duration::from_millis(10));

        let json2 = r#"[{"role":"user","content":"hello"},{"role":"assistant","content":"hi"}]"#;
        db.save_session(100, json2).expect("update session");

        let snapshot = db
            .load_session_snapshot(100, 10)
            .expect("load updated session");
        assert_eq!(snapshot.messages_json.as_deref(), Some(json2));
        assert!(snapshot.updated_at.unwrap() >= first_updated_at);
        assert!(
            db.load_session_snapshot(200, 10)
                .expect("other chat")
                .messages_json
                .is_none()
        );
    }

    #[test]
    fn clear_session_deletes_snapshots_and_messages() {
        let (db, _dir) = test_db();
        let chat_id = 100;

        db.save_session(chat_id, r#"[{"role":"user","content":"hello"}]"#)
            .expect("save session");
        store_msg(&db, "msg-1", chat_id, "hello", "2024-01-01T00:00:00Z");
        store_msg(&db, "msg-2", chat_id, "hi", "2024-01-01T00:00:01Z");

        db.clear_session(chat_id).expect("clear session");

        assert!(
            db.load_session_snapshot(chat_id, 10)
                .expect("load session")
                .messages_json
                .is_none()
        );
        assert!(
            db.get_recent_messages(chat_id, 10)
                .expect("load recent messages")
                .is_empty()
        );
    }

    #[test]
    fn clear_session_idempotent_on_empty_chat() {
        let (db, _dir) = test_db();

        db.clear_session(999).expect("clear missing session");
    }

    #[test]
    fn store_message_with_session_rejects_duplicate_initial_snapshot() {
        let (db, _dir) = test_db();
        let message = StoredMessage {
            id: "msg-1".to_string(),
            chat_id: 100,
            sender_name: "alice".to_string(),
            content: "hello".to_string(),
            is_from_bot: false,
            timestamp: "2024-01-01T00:00:00Z".to_string(),
        };

        db.store_message_with_session(&message, r#"[{"role":"user","content":"hello"}]"#, None)
            .expect("insert session");

        let conflict = db.store_message_with_session(
            &StoredMessage {
                id: "msg-2".to_string(),
                chat_id: 100,
                sender_name: "alice".to_string(),
                content: "hello again".to_string(),
                is_from_bot: false,
                timestamp: "2024-01-01T00:00:01Z".to_string(),
            },
            r#"[{"role":"user","content":"hello again"}]"#,
            None,
        );

        assert!(matches!(
            conflict,
            Err(StorageError::SessionSnapshotConflict)
        ));
    }

    #[test]
    fn resolve_or_create_chat_id_uses_surface_identity() {
        let (db, _dir) = test_db();

        let first = db
            .resolve_or_create_chat_id("cli", "cli:local-dev", Some("local-dev"), "cli", "default")
            .expect("create chat");
        let second = db
            .resolve_or_create_chat_id("cli", "cli:local-dev", Some("renamed"), "cli", "default")
            .expect("reuse chat");

        assert_eq!(first, second);
        assert!(first > 0);
    }

    #[test]
    fn list_sessions_prefers_logical_session_name() {
        let (db, _dir) = test_db();

        let chat_id = db
            .resolve_or_create_chat_id("cli", "cli:demo", Some("demo"), "cli", "default")
            .expect("create chat");
        store_msg(&db, "msg-1", chat_id, "hello", "2024-01-01T00:00:00Z");
        db.save_session(chat_id, r#"[{"role":"user","content":"hello"}]"#)
            .expect("save session");

        let sessions = db.list_sessions().expect("list sessions");
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].channel, "cli");
        assert_eq!(sessions[0].surface_thread, "demo");
        assert_eq!(sessions[0].chat_title.as_deref(), Some("demo"));

        let reopened_chat_id = db
            .resolve_or_create_chat_id(
                "cli",
                &format!("cli:{}", sessions[0].surface_thread),
                sessions[0].chat_title.as_deref(),
                "cli",
                "default",
            )
            .expect("reopen chat");
        assert_eq!(reopened_chat_id, chat_id);
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

        let conn = db.conn.lock().expect("lock");
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

        let conn = db.conn.lock().expect("lock");
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

        let conn = db.conn.lock().expect("lock");
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
    fn log_llm_usage_returns_row_id() {
        let (db, _dir) = test_db();

        let row_id = db
            .log_llm_usage(&LlmUsageLogEntry {
                chat_id: 100,
                caller_channel: "tui",
                provider: "openai",
                model: "gpt-4",
                input_tokens: 100,
                output_tokens: 50,
                request_kind: "agent_loop",
            })
            .expect("log usage");

        assert!(row_id > 0);
    }

    #[test]
    fn resolve_or_create_chat_id_sets_agent_id() {
        let (db, _dir) = test_db();

        db.resolve_or_create_chat_id("cli", "cli:mybot", Some("mybot"), "cli", "mybot")
            .expect("create chat");

        let info = db
            .get_chat_by_id(
                db.resolve_or_create_chat_id("cli", "cli:mybot", Some("mybot"), "cli", "mybot")
                    .expect("chat id"),
            )
            .expect("chat info")
            .expect("chat should exist");

        assert_eq!(info.agent_id, "mybot");
    }

    #[test]
    fn resolve_or_create_chat_id_preserves_agent_id_on_update() {
        let (db, _dir) = test_db();

        let chat_id = db
            .resolve_or_create_chat_id(
                "cli",
                "cli:persist-agent",
                Some("persist-agent"),
                "cli",
                "agent_a",
            )
            .expect("create with agent_a");

        let second_id = db
            .resolve_or_create_chat_id(
                "cli",
                "cli:persist-agent",
                Some("persist-agent"),
                "cli",
                "agent_b",
            )
            .expect("reuse chat");

        assert_eq!(second_id, chat_id);

        let info = db
            .get_chat_by_id(chat_id)
            .expect("chat info")
            .expect("chat should exist");

        assert_eq!(info.agent_id, "agent_a");
    }

    #[test]
    fn get_chat_by_id_returns_agent_id() {
        let (db, _dir) = test_db();

        let chat_id = db
            .resolve_or_create_chat_id(
                "web",
                "web:agent-test",
                Some("agent-test"),
                "web",
                "custom-agent",
            )
            .expect("create chat");

        let info = db
            .get_chat_by_id(chat_id)
            .expect("get chat")
            .expect("chat should exist");

        assert_eq!(info.agent_id, "custom-agent");
    }

    #[test]
    fn list_sessions_includes_agent_id() {
        let (db, _dir) = test_db();

        db.resolve_or_create_chat_id(
            "cli",
            "cli:session-agent",
            Some("session-agent"),
            "cli",
            "list-agent",
        )
        .expect("create chat");
        store_msg(&db, "msg-1", 1, "hello", "2024-01-01T00:00:00Z");

        let sessions = db.list_sessions().expect("list sessions");
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].agent_id, "list-agent");
    }
}
