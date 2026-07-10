use std::str::FromStr;

use rusqlite::{OptionalExtension, params};

use crate::error::StorageError;

use super::{
    ChatInfo, Database, MessageKind, SenderKind, SessionSnapshot, SessionSummary, StoredMessage,
};

pub(crate) fn row_to_stored_message(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredMessage> {
    let sender_kind = parse_row_enum!(row, 4, SenderKind)?;
    let message_kind = parse_row_enum!(row, 6, MessageKind)?;

    Ok(StoredMessage {
        id: row.get(0)?,
        chat_id: row.get(1)?,
        sender_id: row.get(2)?,
        content: row.get(3)?,
        sender_kind,
        timestamp: row.get(5)?,
        message_kind,
        recipient_agent_id: row.get(7)?,
    })
}

impl Database {
    pub(crate) fn resolve_chat_id(
        &self,
        channel: &str,
        external_chat_id: &str,
    ) -> Result<Option<i64>, StorageError> {
        let conn = self.get_conn()?;
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
        let conn = self.get_conn()?;
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

    pub(crate) fn get_chat_by_channel_external_and_agent(
        &self,
        channel: &str,
        external_chat_id: &str,
        agent_id: &str,
    ) -> Result<Option<ChatInfo>, StorageError> {
        let conn = self.get_conn()?;
        match conn.query_row(
            "SELECT chat_id, chat_type, agent_id FROM chats WHERE channel = ?1 AND external_chat_id = ?2 AND agent_id = ?3 LIMIT 1",
            params![channel, external_chat_id, agent_id],
            |row| {
                Ok(ChatInfo {
                    chat_id: row.get(0)?,
                    channel: channel.to_string(),
                    external_chat_id: external_chat_id.to_string(),
                    chat_type: row.get(1)?,
                    agent_id: row.get(2)?,
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
        let conn = self.get_conn()?;
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
        let conn = self.get_conn()?;
        let mut stmt = conn.prepare_cached(
            "SELECT
                c.chat_id,
                c.channel,
                c.external_chat_id,
                c.chat_title,
                COALESCE((SELECT MAX(m.timestamp) FROM messages m WHERE m.chat_id = c.chat_id), c.last_message_time)
                    AS last_message_time,
                (
                    SELECT m.content
                    FROM messages m
                    WHERE m.chat_id = c.chat_id
                    ORDER BY m.timestamp DESC, m.id DESC
                    LIMIT 1
                ) AS last_message_preview,
                c.agent_id
             FROM chats c
             ORDER BY last_message_time DESC, c.chat_id DESC",
        )?;
        stmt.query_map([], |row| {
            let channel: String = row.get(1)?;
            let external_chat_id: String = row.get(2)?;
            let chat_title: Option<String> = row.get(3)?;
            Ok(SessionSummary {
                chat_id: row.get(0)?,
                channel: channel.clone(),
                external_chat_id: external_chat_id.clone(),
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
        let conn = self.get_conn()?;
        let mut stmt = conn.prepare_cached(
            "SELECT id, chat_id, sender_id, content, sender_kind, timestamp,
                    message_kind, recipient_agent_id
             FROM messages
             WHERE chat_id = ?1
             ORDER BY timestamp DESC, id DESC
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
        let conn = self.get_conn()?;
        let mut stmt = conn.prepare_cached(
            "SELECT id, chat_id, sender_id, content, sender_kind, timestamp,
                    message_kind, recipient_agent_id
             FROM messages
             WHERE chat_id = ?1
             ORDER BY timestamp ASC, id ASC",
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
        let conn = self.get_conn()?;
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

    /// Clears session message history by setting `messages_json` to an empty
    /// JSON array.  The session row itself and `messages` / `tool_calls`
    /// records are preserved.
    ///
    /// Uses optimistic concurrency: the update only succeeds when
    /// `expected_updated_at` matches the current row.  Returns `Ok(true)` if
    /// the row was updated, `Ok(false)` if the row was not found or the
    /// timestamp did not match (concurrent modification).
    pub(crate) fn clear_session_messages(
        &self,
        chat_id: i64,
        expected_updated_at: &str,
    ) -> Result<bool, StorageError> {
        let conn = self.get_conn()?;
        let now = chrono::Utc::now().to_rfc3339();
        let rows = conn.execute(
            "UPDATE sessions SET messages_json = '[]', updated_at = ?1 \
             WHERE chat_id = ?2 AND updated_at = ?3",
            params![now, chat_id, expected_updated_at],
        )?;
        Ok(rows > 0)
    }

    /// Updates `sessions.messages_json` to `new_messages_json` and bumps
    /// `updated_at`. Unlike [`Database::clear_session_messages`], which wipes
    /// to `[]`, this keeps a caller-supplied payload — used by the sleep
    /// batch to retain the trailing N messages while still archiving the full
    /// conversation.
    ///
    /// Uses optimistic concurrency: the update only succeeds when
    /// `expected_updated_at` matches the current row. Returns `Ok(true)` if
    /// the row was updated, `Ok(false)` on concurrent modification or missing
    /// row.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if the underlying SQLite update fails.
    pub(crate) fn truncate_session_messages(
        &self,
        chat_id: i64,
        expected_updated_at: &str,
        new_messages_json: &str,
    ) -> Result<bool, StorageError> {
        let conn = self.get_conn()?;
        let now = chrono::Utc::now().to_rfc3339();
        let rows = conn.execute(
            "UPDATE sessions SET messages_json = ?1, updated_at = ?2 \
             WHERE chat_id = ?3 AND updated_at = ?4",
            params![new_messages_json, now, chat_id, expected_updated_at],
        )?;
        Ok(rows > 0)
    }

    pub(crate) fn store_message_with_session(
        &self,
        message: &StoredMessage,
        messages_json: &str,
        expected_updated_at: Option<&str>,
    ) -> Result<String, StorageError> {
        let mut conn = self.get_conn()?;
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT OR REPLACE INTO messages (id, chat_id, sender_id, content, sender_kind, timestamp, message_kind, recipient_agent_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                message.id,
                message.chat_id,
                message.sender_id,
                message.content,
                message.sender_kind.to_string(),
                message.timestamp,
                message.message_kind.to_string(),
                message.recipient_agent_id.as_deref(),
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
        let mut conn = self.get_conn()?;
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
                "SELECT id, chat_id, sender_id, content, sender_kind, timestamp,
                        message_kind, recipient_agent_id
                 FROM messages
                 WHERE chat_id = ?1
                 ORDER BY timestamp DESC, id DESC
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
// Channel Log (multi-agent room shared log)
// ---------------------------------------------------------------------------

impl Database {
    /// Resolves or creates the Channel Log chat for a Discord multi-agent room.
    ///
    /// The Channel Log uses `channel = "discord"`,
    /// `external_chat_id = "discord:{channel_id}:multi-room-log"`,
    /// `chat_type = "channel_log"`, `agent_id = ""`.
    /// It has **no session row** — only messages.
    pub(crate) fn resolve_channel_log_chat_id(&self, channel_id: u64) -> Result<i64, StorageError> {
        let external_id = format!("discord:{channel_id}:multi-room-log");
        self.resolve_or_create_chat_id("discord", &external_id, None, "channel_log", "")
    }

    /// Resolves or creates a Channel Log chat for Telegram multi-agent rooms.
    /// Same concept as [`resolve_channel_log_chat_id`] but keyed by Telegram `i64` chat ID.
    pub(crate) fn resolve_telegram_channel_log_chat_id(
        &self,
        chat_id: i64,
    ) -> Result<i64, StorageError> {
        let external_id = format!("telegram:{chat_id}:multi-room-log");
        self.resolve_or_create_chat_id("telegram", &external_id, None, "channel_log", "")
    }

    /// Returns the most recent messages from a Channel Log, ordered oldest-first.
    pub(crate) fn get_channel_log_messages(
        &self,
        chat_id: i64,
        limit: usize,
    ) -> Result<Vec<StoredMessage>, StorageError> {
        self.get_recent_messages(chat_id, limit)
    }

    /// Stores a message without touching the session snapshot.
    /// Used for Channel Log entries (agent_send, system events) that have no session.
    pub(crate) fn store_message_only(&self, message: &StoredMessage) -> Result<(), StorageError> {
        let conn = self.get_conn()?;
        conn.execute(
            "INSERT OR REPLACE INTO messages (id, chat_id, sender_id, content, sender_kind, timestamp, message_kind, recipient_agent_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                message.id,
                message.chat_id,
                message.sender_id,
                message.content,
                message.sender_kind.to_string(),
                message.timestamp,
                message.message_kind.to_string(),
                message.recipient_agent_id.as_deref(),
            ],
        )?;
        Ok(())
    }

    /// Persists a stop-reason system event to the Channel Log.
    ///
    /// Content format: `{"reason": "StopReasonVariant"}`.
    /// Sender: `sender_id = "system"`, `sender_kind = System`.
    pub(crate) fn store_system_event(
        &self,
        channel_log_chat_id: i64,
        reason: &crate::runtime::turn_scheduler::StopReason,
    ) -> Result<(), StorageError> {
        let content = serde_json::json!({
            "reason": format!("{reason:?}")
        })
        .to_string();

        let mut message = StoredMessage::system(channel_log_chat_id, content);
        message.message_kind = MessageKind::SystemEvent;

        self.store_message_only(&message)
    }

    /// Persists a bot response to the Channel Log.
    ///
    /// Sender is the agent ID, `sender_kind = Assistant`, `MessageKind::Message`.
    pub(crate) fn store_channel_log_bot_response(
        &self,
        channel_log_chat_id: i64,
        agent_id: &str,
        response: &str,
    ) -> Result<(), StorageError> {
        let message = StoredMessage {
            id: format!("cl-bot-{}", uuid::Uuid::new_v4()),
            chat_id: channel_log_chat_id,
            sender_id: agent_id.to_string(),
            content: response.to_string(),
            sender_kind: SenderKind::Assistant,
            timestamp: chrono::Utc::now().to_rfc3339(),
            message_kind: MessageKind::Message,
            recipient_agent_id: None,
        };
        self.store_message_only(&message)
    }
}

// ---------------------------------------------------------------------------
// Tool calls & LLM usage
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn test_db() -> (Database, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("runtime").join("egopulse.db");
        let db = Database::new(&db_path).expect("db");
        (db, dir)
    }

    fn store_msg(db: &Database, id: &str, chat_id: i64, content: &str, ts: &str) {
        let conn = db.get_conn().expect("pool");
        conn.execute(
                "INSERT OR REPLACE INTO messages (id, chat_id, sender_id, content, sender_kind, timestamp, message_kind)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                rusqlite::params![id, chat_id, "alice", content, "user", ts, "message"],
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
    fn clear_session_messages_empties_json_only() {
        let (db, _dir) = test_db();
        let chat_id = 100;

        db.save_session(chat_id, r#"[{"role":"user","content":"hello"}]"#)
            .expect("save session");
        store_msg(&db, "msg-1", chat_id, "hello", "2024-01-01T00:00:00Z");
        store_msg(&db, "msg-2", chat_id, "hi", "2024-01-01T00:00:01Z");

        let snapshot = db.load_session_snapshot(chat_id, 10).expect("load session");
        let updated_at = snapshot.updated_at.as_deref().expect("has updated_at");

        let cleared = db
            .clear_session_messages(chat_id, updated_at)
            .expect("clear session messages");
        assert!(cleared, "should have updated the row");

        let snapshot = db.load_session_snapshot(chat_id, 10).expect("load session");
        assert_eq!(
            snapshot.messages_json.as_deref(),
            Some(r#"[]"#),
            "messages_json should be empty array"
        );

        let messages = db
            .get_recent_messages(chat_id, 10)
            .expect("load recent messages");
        assert_eq!(messages.len(), 2, "messages records should be preserved");
    }

    #[test]
    fn clear_session_messages_returns_false_on_stale_timestamp() {
        let (db, _dir) = test_db();
        let chat_id = 200;

        db.save_session(chat_id, r#"[{"role":"user","content":"hello"}]"#)
            .expect("save session");

        let cleared = db
            .clear_session_messages(chat_id, "stale-timestamp")
            .expect("clear session messages");
        assert!(!cleared, "should not have updated the row");

        let snapshot = db.load_session_snapshot(chat_id, 10).expect("load session");
        assert!(
            snapshot.messages_json.as_deref() != Some(r#"[]"#),
            "messages_json should not be cleared"
        );
    }

    #[test]
    fn truncate_session_messages_replaces_json() {
        let (db, _dir) = test_db();
        let chat_id = 300;

        db.save_session(chat_id, r#"[{"role":"user","content":"old"}]"#)
            .expect("save session");
        store_msg(&db, "msg-1", chat_id, "hello", "2024-01-01T00:00:00Z");
        store_msg(&db, "msg-2", chat_id, "hi", "2024-01-01T00:00:01Z");

        let snapshot = db.load_session_snapshot(chat_id, 10).expect("load session");
        let updated_at = snapshot.updated_at.as_deref().expect("has updated_at");

        let new_json = r#"[{"role":"assistant","content":"kept"}]"#;
        let truncated = db
            .truncate_session_messages(chat_id, updated_at, new_json)
            .expect("truncate session messages");
        assert!(truncated, "should have updated the row");

        let snapshot = db.load_session_snapshot(chat_id, 10).expect("load session");
        assert_eq!(
            snapshot.messages_json.as_deref(),
            Some(new_json),
            "messages_json should be replaced with the supplied payload"
        );

        let messages = db
            .get_recent_messages(chat_id, 10)
            .expect("load recent messages");
        assert_eq!(messages.len(), 2, "messages records should be preserved");
    }

    #[test]
    fn truncate_session_messages_returns_false_on_stale_timestamp() {
        let (db, _dir) = test_db();
        let chat_id = 400;

        db.save_session(chat_id, r#"[{"role":"user","content":"hello"}]"#)
            .expect("save session");

        let truncated = db
            .truncate_session_messages(chat_id, "stale-timestamp", r#"[]"#)
            .expect("truncate session messages");
        assert!(!truncated, "should not have updated the row");

        let snapshot = db.load_session_snapshot(chat_id, 10).expect("load session");
        assert!(
            snapshot.messages_json.as_deref() != Some(r#"[]"#),
            "messages_json should not be modified"
        );
    }

    #[test]
    fn store_message_with_session_rejects_duplicate_initial_snapshot() {
        let (db, _dir) = test_db();
        let message = StoredMessage {
            id: "msg-1".to_string(),
            chat_id: 100,
            sender_id: "user:cli:default".to_string(),
            content: "hello".to_string(),
            sender_kind: SenderKind::User,
            timestamp: "2024-01-01T00:00:00Z".to_string(),
            message_kind: MessageKind::Message,
            recipient_agent_id: None,
        };

        db.store_message_with_session(&message, r#"[{"role":"user","content":"hello"}]"#, None)
            .expect("insert session");

        let conflict = db.store_message_with_session(
            &StoredMessage {
                id: "msg-2".to_string(),
                chat_id: 100,
                sender_id: "user:cli:default".to_string(),
                content: "hello again".to_string(),
                sender_kind: SenderKind::User,
                timestamp: "2024-01-01T00:00:01Z".to_string(),
                message_kind: MessageKind::Message,
                recipient_agent_id: None,
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
    fn list_sessions_orders_by_latest_message_timestamp() {
        let (db, _dir) = test_db();

        // Two chats created in order; A is older at creation time.
        let chat_a = db
            .resolve_or_create_chat_id("cli", "cli:a", Some("a"), "cli", "default")
            .expect("create chat A");
        let chat_b = db
            .resolve_or_create_chat_id("cli", "cli:b", Some("b"), "cli", "default")
            .expect("create chat B");

        // Same initial message time, then A receives a newer message later.
        // A must sort above B even though B was created more recently.
        store_msg(&db, "m-a1", chat_a, "a-first", "2024-01-01T00:00:00Z");
        store_msg(&db, "m-b1", chat_b, "b-first", "2024-01-01T00:00:00Z");
        store_msg(&db, "m-a2", chat_a, "a-second", "2024-01-02T00:00:00Z");

        let sessions = db.list_sessions().expect("list sessions");
        assert_eq!(sessions.len(), 2);
        assert_eq!(
            sessions[0].chat_id, chat_a,
            "chat with the latest message must sort first"
        );
        assert_eq!(sessions[1].chat_id, chat_b);
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

    #[test]
    fn pending_sleep_messages_exclude_other_agents() {
        let (db, _dir) = test_db();

        let chat_id = db
            .resolve_or_create_chat_id("cli", "cli:msgs-a", None, "cli", "agent-a")
            .expect("create chat");

        store_msg(&db, "msg-1", chat_id, "message", "2024-01-01T00:00:00Z");

        let count = db
            .count_agent_pending_sleep_messages("agent-a")
            .expect("count");
        assert_eq!(count, 1);
    }

    #[test]
    fn get_pending_sleep_sessions_returns_empty_for_unknown_agent() {
        let (db, _dir) = test_db();

        let sessions = db
            .get_agent_sessions_with_pending_sleep_messages("nonexistent-agent", 10)
            .expect("get sessions");
        assert!(sessions.is_empty());
    }

    // --- Channel Log tests ---

    #[test]
    fn store_message_to_channel_log() {
        let (db, _dir) = test_db();

        let chat_id = db.resolve_channel_log_chat_id(100).expect("create");
        store_msg(&db, "cl-1", chat_id, "hello", "2025-01-01T00:00:00Z");

        let msgs = db.get_channel_log_messages(chat_id, 10).expect("messages");
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "hello");
    }

    #[test]
    fn get_recent_channel_log_messages() {
        let (db, _dir) = test_db();

        let chat_id = db.resolve_channel_log_chat_id(200).expect("create");
        for i in 0..5 {
            store_msg(
                &db,
                &format!("cl-{i}"),
                chat_id,
                &format!("msg {i}"),
                &format!("2025-01-01T00:00:{i:02}Z"),
            );
        }

        let msgs = db.get_channel_log_messages(chat_id, 3).expect("messages");
        assert_eq!(msgs.len(), 3);
        assert_eq!(msgs[0].content, "msg 2");
        assert_eq!(msgs[2].content, "msg 4");
    }

    // ---- System Event tests ----

    #[test]
    fn store_system_event_saves_to_channel_log() {
        let (db, _dir) = test_db();

        let chat_id = db.resolve_channel_log_chat_id(300).expect("create");
        db.store_system_event(
            chat_id,
            &crate::runtime::turn_scheduler::StopReason::ChainDepthExceeded,
        )
        .expect("store");

        let msgs = db.get_channel_log_messages(chat_id, 10).expect("messages");
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].message_kind, MessageKind::SystemEvent);
    }

    #[test]
    fn store_system_event_content_is_valid_json_with_reason() {
        let (db, _dir) = test_db();

        let chat_id = db.resolve_channel_log_chat_id(301).expect("create");
        db.store_system_event(
            chat_id,
            &crate::runtime::turn_scheduler::StopReason::TurnCountExceeded,
        )
        .expect("store");

        let msgs = db.get_channel_log_messages(chat_id, 10).expect("messages");
        let parsed: serde_json::Value = serde_json::from_str(&msgs[0].content).expect("valid json");
        assert!(parsed.get("reason").is_some());
    }

    #[test]
    fn store_system_event_sender_is_system() {
        let (db, _dir) = test_db();

        let chat_id = db.resolve_channel_log_chat_id(302).expect("create");
        db.store_system_event(
            chat_id,
            &crate::runtime::turn_scheduler::StopReason::LlmFailure,
        )
        .expect("store");

        let msgs = db.get_channel_log_messages(chat_id, 10).expect("messages");
        assert_eq!(msgs[0].sender_id, "system");
        assert_eq!(msgs[0].sender_kind, SenderKind::System);
    }

    #[test]
    fn store_message_with_sender_kind() {
        let (db, _dir) = test_db();

        let chat_id = db
            .resolve_or_create_chat_id("cli", "cli:sender-kind", None, "cli", "default")
            .expect("create chat");
        let message = StoredMessage {
            id: "msg-assistant".to_string(),
            chat_id,
            sender_id: "lyre".to_string(),
            content: "assistant says hi".to_string(),
            sender_kind: SenderKind::Assistant,
            timestamp: "2024-01-01T00:00:00Z".to_string(),
            message_kind: MessageKind::Message,
            recipient_agent_id: None,
        };

        db.store_message_with_session(&message, r#"[]"#, None)
            .expect("store");

        let msgs = db.get_recent_messages(chat_id, 10).expect("messages");
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].sender_id, "lyre");
        assert_eq!(msgs[0].sender_kind, SenderKind::Assistant);
    }

    #[test]
    fn store_message_user_kind() {
        let (db, _dir) = test_db();

        let chat_id = db
            .resolve_or_create_chat_id("cli", "cli:user-kind", None, "cli", "default")
            .expect("create chat");
        let message =
            StoredMessage::user(chat_id, "user:discord:123".to_string(), "hello".to_string());

        db.store_message_with_session(&message, r#"[]"#, None)
            .expect("store");

        let msgs = db.get_recent_messages(chat_id, 10).expect("messages");
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].sender_kind, SenderKind::User);
        assert_eq!(msgs[0].sender_id, "user:discord:123");
    }

    #[test]
    fn store_message_system_kind() {
        let (db, _dir) = test_db();

        let chat_id = db
            .resolve_or_create_chat_id("cli", "cli:sys-kind", None, "cli", "default")
            .expect("create chat");
        let message = StoredMessage::system(chat_id, "boot complete".to_string());

        db.store_message_with_session(&message, r#"[]"#, None)
            .expect("store");

        let msgs = db.get_recent_messages(chat_id, 10).expect("messages");
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].sender_kind, SenderKind::System);
        assert_eq!(msgs[0].sender_id, "system");
    }

    #[test]
    fn store_message_tool_kind() {
        let (db, _dir) = test_db();

        let chat_id = db
            .resolve_or_create_chat_id("cli", "cli:tool-kind", None, "cli", "default")
            .expect("create chat");
        let message = StoredMessage::tool(
            chat_id,
            "tool:web_fetch".to_string(),
            "lyre".to_string(),
            "fetched https://example.com".to_string(),
        );

        db.store_message_with_session(&message, r#"[]"#, None)
            .expect("store");

        let msgs = db.get_recent_messages(chat_id, 10).expect("messages");
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].sender_kind, SenderKind::Tool);
        assert_eq!(msgs[0].sender_id, "tool:web_fetch");
        assert_eq!(msgs[0].recipient_agent_id.as_deref(), Some("lyre"));
    }

    #[test]
    fn get_recent_messages_returns_sender_id() {
        let (db, _dir) = test_db();

        let chat_id = db
            .resolve_or_create_chat_id("web", "web:sender-id", None, "web", "default")
            .expect("create chat");

        let conn = db.get_conn().expect("pool");
        conn.execute(
                "INSERT INTO messages (id, chat_id, sender_id, content, sender_kind, timestamp, message_kind)
                 VALUES ('m1', ?1, 'user:cli:alice', 'hello', 'user', '2024-01-01T00:00:00Z', 'message')",
                rusqlite::params![chat_id],
            )
            .expect("insert");
        drop(conn);

        let msgs = db.get_recent_messages(chat_id, 10).expect("messages");
        assert_eq!(msgs[0].sender_id, "user:cli:alice");
        assert_eq!(msgs[0].sender_kind, SenderKind::User);
    }

    #[test]
    fn find_message_by_content_finds_system_event() {
        let (db, _dir) = test_db();

        let chat_id = db.resolve_channel_log_chat_id(500).expect("create");
        db.store_system_event(
            chat_id,
            &crate::runtime::turn_scheduler::StopReason::LlmFailure,
        )
        .expect("store");

        let msgs = db.get_all_messages(chat_id).expect("messages");
        let found = msgs.iter().find(|m| m.content.contains("LlmFailure"));
        assert!(found.is_some(), "should find system event by content");
        assert_eq!(found.unwrap().sender_kind, SenderKind::System);
    }

    #[test]
    fn store_system_event_sets_system_kind() {
        let (db, _dir) = test_db();

        let chat_id = db.resolve_channel_log_chat_id(600).expect("create");
        db.store_system_event(
            chat_id,
            &crate::runtime::turn_scheduler::StopReason::ChainDepthExceeded,
        )
        .expect("store");

        let msgs = db.get_channel_log_messages(chat_id, 10).expect("messages");
        assert_eq!(msgs[0].sender_id, "system");
        assert_eq!(msgs[0].sender_kind, SenderKind::System);
        assert_eq!(msgs[0].message_kind, MessageKind::SystemEvent);
    }

    #[test]
    fn store_agent_response_sets_assistant_kind() {
        let (db, _dir) = test_db();

        let chat_id = db.resolve_channel_log_chat_id(700).expect("create");
        db.store_channel_log_bot_response(chat_id, "lyre", "Hello from agent")
            .expect("store");

        let msgs = db.get_channel_log_messages(chat_id, 10).expect("messages");
        assert_eq!(msgs[0].sender_id, "lyre");
        assert_eq!(msgs[0].sender_kind, SenderKind::Assistant);
        assert_eq!(msgs[0].content, "Hello from agent");
    }

    #[test]
    fn roundtrip_recipient_agent_id() {
        let (db, _dir) = test_db();

        let chat_id = db
            .resolve_or_create_chat_id("cli", "cli:recipient", None, "cli", "default")
            .expect("create chat");
        let message = StoredMessage::tool(
            chat_id,
            "tool:read".to_string(),
            "bob".to_string(),
            "file contents".to_string(),
        );

        db.store_message_with_session(&message, r#"[]"#, None)
            .expect("store");

        let msgs = db.get_recent_messages(chat_id, 10).expect("messages");
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].recipient_agent_id.as_deref(), Some("bob"));
        assert_eq!(msgs[0].sender_kind, SenderKind::Tool);
    }

    #[test]
    fn get_messages_between_returns_all_without_cutoff() {
        let (db, _dir) = test_db();
        let chat_id = db
            .resolve_or_create_chat_id("cli", "cli:between-1", None, "cli", "agent-a")
            .expect("create chat");
        store_msg(&db, "m1", chat_id, "first", "2025-01-01T00:00:00Z");
        store_msg(&db, "m2", chat_id, "second", "2025-01-02T00:00:00Z");
        store_msg(&db, "m3", chat_id, "third", "2025-01-03T00:00:00Z");

        let msgs = db.get_messages_between(chat_id, None, None).expect("query");
        assert_eq!(msgs.len(), 3);
        assert_eq!(msgs[0].content, "first");
        assert_eq!(msgs[2].content, "third");
    }

    #[test]
    fn get_messages_between_filters_by_from() {
        let (db, _dir) = test_db();
        let chat_id = db
            .resolve_or_create_chat_id("cli", "cli:between-2", None, "cli", "agent-a")
            .expect("create chat");
        store_msg(&db, "m1", chat_id, "old", "2025-01-01T00:00:00Z");
        store_msg(&db, "m2", chat_id, "mid", "2025-01-02T00:00:00Z");
        store_msg(&db, "m3", chat_id, "new", "2025-01-03T00:00:00Z");

        let msgs = db
            .get_messages_between(chat_id, Some("2025-01-02T00:00:00Z"), None)
            .expect("query");
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].content, "mid");
        assert_eq!(msgs[1].content, "new");
    }

    #[test]
    fn get_messages_between_filters_by_to() {
        let (db, _dir) = test_db();
        let chat_id = db
            .resolve_or_create_chat_id("cli", "cli:between-3", None, "cli", "agent-a")
            .expect("create chat");
        store_msg(&db, "m1", chat_id, "old", "2025-01-01T00:00:00Z");
        store_msg(&db, "m2", chat_id, "mid", "2025-01-02T00:00:00Z");
        store_msg(&db, "m3", chat_id, "new", "2025-01-03T00:00:00Z");

        let msgs = db
            .get_messages_between(chat_id, None, Some("2025-01-02T00:00:00Z"))
            .expect("query");
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "old");
    }

    #[test]
    fn get_messages_between_filters_by_range() {
        let (db, _dir) = test_db();
        let chat_id = db
            .resolve_or_create_chat_id("cli", "cli:between-4", None, "cli", "agent-a")
            .expect("create chat");
        store_msg(&db, "m1", chat_id, "old", "2025-01-01T00:00:00Z");
        store_msg(&db, "m2", chat_id, "mid", "2025-01-02T00:00:00Z");
        store_msg(&db, "m3", chat_id, "new", "2025-01-03T00:00:00Z");

        let msgs = db
            .get_messages_between(
                chat_id,
                Some("2025-01-02T00:00:00Z"),
                Some("2025-01-03T00:00:00Z"),
            )
            .expect("query");
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "mid");
    }

    #[test]
    fn get_messages_between_returns_empty_for_wrong_chat() {
        let (db, _dir) = test_db();
        let _chat_id = db
            .resolve_or_create_chat_id("cli", "cli:between-5", None, "cli", "agent-a")
            .expect("create chat");

        let msgs = db.get_messages_between(999, None, None).expect("query");
        assert!(msgs.is_empty());
    }

    #[test]
    fn get_messages_after_cursor_respects_composite_upper_bound() {
        // Arrange
        let (db, _dir) = test_db();
        let chat_id = db
            .resolve_or_create_chat_id("cli", "cli:cursor-bound", None, "cli", "agent-a")
            .expect("create chat");
        let timestamp = "2025-01-01T00:00:00Z";
        store_msg(&db, "m1", chat_id, "first", timestamp);
        store_msg(&db, "m2", chat_id, "upper", timestamp);
        store_msg(&db, "m3", chat_id, "inserted later", timestamp);

        // Act
        let messages = db
            .get_messages_after_cursor(chat_id, None, (timestamp, "m2"))
            .expect("query");

        // Assert
        assert_eq!(
            messages
                .iter()
                .map(|message| message.id.as_str())
                .collect::<Vec<_>>(),
            vec!["m1", "m2"]
        );
    }

    #[test]
    fn get_agent_chats_with_messages_between_returns_chats_with_messages() {
        let (db, _dir) = test_db();
        let chat_id = db
            .resolve_or_create_chat_id("cli", "cli:chats-1", None, "cli", "agent-a")
            .expect("create chat");
        store_msg(&db, "m1", chat_id, "hello", "2025-01-01T00:00:00Z");

        let chats = db
            .get_agent_chats_with_messages_between("agent-a", None, None)
            .expect("query");
        assert_eq!(chats.len(), 1);
        assert_eq!(chats[0].0, chat_id);
    }

    #[test]
    fn get_agent_chats_with_messages_between_excludes_channel_log() {
        let (db, _dir) = test_db();
        let log_id = db.resolve_channel_log_chat_id(42).expect("create log");
        let conn = db.get_conn().expect("pool");
        conn.execute(
                "INSERT OR REPLACE INTO messages (id, chat_id, sender_id, content, sender_kind, timestamp, message_kind)
                 VALUES ('cl-1', ?1, 'system', 'event', 'system', '2025-01-01T00:00:00Z', 'system_event')",
                rusqlite::params![log_id],
            )
            .expect("store msg");
        drop(conn);

        let chats = db
            .get_agent_chats_with_messages_between("", None, None)
            .expect("query");
        assert!(chats.is_empty(), "channel_log should be excluded");
    }

    #[test]
    fn get_agent_chats_with_messages_between_filters_by_time_range() {
        let (db, _dir) = test_db();
        let chat_id = db
            .resolve_or_create_chat_id("cli", "cli:chats-2", None, "cli", "agent-a")
            .expect("create chat");
        store_msg(&db, "old", chat_id, "old", "2025-01-01T00:00:00Z");
        store_msg(&db, "new", chat_id, "new", "2025-06-01T00:00:00Z");

        let chats = db
            .get_agent_chats_with_messages_between("agent-a", Some("2025-03-01T00:00:00Z"), None)
            .expect("query");
        assert_eq!(
            chats.len(),
            1,
            "should find chat with messages after cutoff"
        );
    }
}
