//! SQLite ベースの会話永続化レイヤー。
//!
//! チャットセッション・メッセージ履歴・ツールコール記録を単一の SQLite DB に保存する。
//! WAL モードで排他制御し、`Mutex<Connection>` でスレッド安全性を担保する。

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension, params};

use crate::error::StorageError;

/// Thread-safe SQLite database wrapper for conversation persistence.
pub struct Database {
    conn: Mutex<Connection>,
}

/// A single chat message persisted in the database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredMessage {
    pub id: String,
    pub chat_id: i64,
    pub sender_name: String,
    pub content: String,
    pub is_from_bot: bool,
    pub timestamp: String,
}

/// Metadata for listing sessions without loading full message history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionSummary {
    pub chat_id: i64,
    pub channel: String,
    pub surface_thread: String,
    pub chat_title: Option<String>,
    pub last_message_time: String,
    pub last_message_preview: Option<String>,
}

/// chat_id から引けるチャネル識別情報。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatInfo {
    pub chat_id: i64,
    pub channel: String,
    pub external_chat_id: String,
    pub chat_type: String,
}

/// Combined session snapshot: serialized messages JSON plus recent message records.
#[derive(Debug, Clone)]
pub struct SessionSnapshot {
    pub messages_json: Option<String>,
    pub updated_at: Option<String>,
    pub recent_messages: Vec<StoredMessage>,
}

/// Persisted tool call record for tracking tool execution history.
#[derive(Debug, Clone)]
pub struct ToolCall {
    pub id: String,
    pub chat_id: i64,
    pub message_id: String,
    pub tool_name: String,
    pub tool_input: String,
    pub tool_output: Option<String>,
    pub timestamp: String,
}

/// Run a blocking database operation on a tokio blocking thread.
pub async fn call_blocking<T, F>(db: Arc<Database>, f: F) -> Result<T, StorageError>
where
    T: Send + 'static,
    F: FnOnce(&Database) -> Result<T, StorageError> + Send + 'static,
{
    tokio::task::spawn_blocking(move || f(db.as_ref()))
        .await
        .map_err(|error| StorageError::TaskJoin(error.to_string()))?
}

/// 現在のスキーマバージョン。
///
/// 新しいマイグレーションを追加する際はこの値をインクリメントし、
/// `run_migrations` に対応する `if version < N` ブロックを追加する。
const SCHEMA_VERSION: i64 = 1;

impl Database {
    /// Open (or create) the database at `{state_root}/runtime/egopulse.db` and initialize schema.
    pub fn new(state_root: &str) -> Result<Self, StorageError> {
        let runtime_dir = Path::new(state_root).join("runtime");
        let db_path = runtime_dir.join("egopulse.db");
        std::fs::create_dir_all(&runtime_dir)?;

        let conn = Connection::open(db_path)?;
        conn.execute_batch("PRAGMA journal_mode=WAL;")?;
        conn.busy_timeout(Duration::from_secs(5))?;

        run_migrations(&conn)?;

        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// 現在のスキーマバージョンを返す。
    pub fn schema_version(&self) -> Result<i64, StorageError> {
        let conn = self.lock_conn()?;
        schema_version(&conn)
    }
}

// ---------------------------------------------------------------------------
// Migration infrastructure
// ---------------------------------------------------------------------------

/// `db_meta` に格納されたスキーマバージョンを読み取る。
///
/// テーブルが存在しない場合は作成し、バージョン未設定なら `0` を返す。
fn schema_version(conn: &Connection) -> Result<i64, StorageError> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS db_meta (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        )",
        [],
    )?;
    let raw: Option<String> = conn
        .query_row(
            "SELECT value FROM db_meta WHERE key = 'schema_version'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    Ok(raw.and_then(|s| s.parse::<i64>().ok()).unwrap_or(0))
}

/// スキーマバージョンを更新し、`schema_migrations` に適用履歴を記録する。
fn set_schema_version(conn: &Connection, version: i64, note: &str) -> Result<(), StorageError> {
    conn.execute(
        "INSERT INTO db_meta(key, value) VALUES('schema_version', ?1)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![version.to_string()],
    )?;
    conn.execute(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
            version INTEGER PRIMARY KEY,
            applied_at TEXT NOT NULL,
            note TEXT
        )",
        [],
    )?;
    conn.execute(
        "INSERT OR REPLACE INTO schema_migrations(version, applied_at, note)
         VALUES(?1, ?2, ?3)",
        params![version, chrono::Utc::now().to_rfc3339(), note],
    )?;
    Ok(())
}

/// 未適用のマイグレーションを逐次実行する。
///
/// 各マイグレーションは `if version < N` でガードされ、
/// 適用後に `set_schema_version` でバージョンを更新する。
/// `SCHEMA_VERSION` に到達したら完了。
fn run_migrations(conn: &Connection) -> Result<(), StorageError> {
    let mut version = schema_version(conn)?;

    if version < 1 {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS chats (
                chat_id INTEGER PRIMARY KEY,
                chat_title TEXT,
                chat_type TEXT NOT NULL DEFAULT 'private',
                last_message_time TEXT NOT NULL,
                channel TEXT,
                external_chat_id TEXT
            );

            CREATE UNIQUE INDEX IF NOT EXISTS idx_chats_channel_external_chat_id
                ON chats(channel, external_chat_id);

            CREATE TABLE IF NOT EXISTS messages (
                id TEXT NOT NULL,
                chat_id INTEGER NOT NULL,
                sender_name TEXT NOT NULL,
                content TEXT NOT NULL,
                is_from_bot INTEGER NOT NULL DEFAULT 0,
                timestamp TEXT NOT NULL,
                PRIMARY KEY (id, chat_id)
            );

            CREATE INDEX IF NOT EXISTS idx_messages_chat_timestamp
                ON messages(chat_id, timestamp);

            CREATE TABLE IF NOT EXISTS sessions (
                chat_id INTEGER PRIMARY KEY,
                messages_json TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS tool_calls (
                id TEXT PRIMARY KEY,
                chat_id INTEGER NOT NULL,
                message_id TEXT NOT NULL,
                tool_name TEXT NOT NULL,
                tool_input TEXT NOT NULL,
                tool_output TEXT,
                timestamp TEXT NOT NULL,
                FOREIGN KEY (chat_id) REFERENCES chats(chat_id)
            );

            CREATE INDEX IF NOT EXISTS idx_tool_calls_chat_id
                ON tool_calls(chat_id);

            CREATE INDEX IF NOT EXISTS idx_tool_calls_chat_message_id
                ON tool_calls(chat_id, message_id);",
        )?;
        set_schema_version(
            conn,
            1,
            "initial schema: chats, messages, sessions, tool_calls",
        )?;
        version = 1;
    }

    // --- v2 以降のマイグレーションはここに追加 ---
    // if version < 2 {
    //     conn.execute_batch("...")?;
    //     set_schema_version(conn, 2, "...")?;
    //     version = 2;
    // }

    debug_assert_eq!(version, SCHEMA_VERSION, "all migrations applied");
    Ok(())
}

impl Database {
    /// Look up the internal chat_id for a (channel, external_chat_id) pair.
    /// Returns `None` if no matching chat exists.
    pub fn resolve_chat_id(
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

    /// chat_id からチャネル・外部 ID の情報を取得する。
    pub fn get_chat_by_id(&self, chat_id: i64) -> Result<Option<ChatInfo>, StorageError> {
        let conn = self.lock_conn()?;
        match conn.query_row(
            "SELECT channel, external_chat_id, chat_type FROM chats WHERE chat_id = ?1 LIMIT 1",
            params![chat_id],
            |row| {
                Ok(ChatInfo {
                    chat_id,
                    channel: row.get(0)?,
                    external_chat_id: row.get(1)?,
                    chat_type: row.get(2)?,
                })
            },
        ) {
            Ok(info) => Ok(Some(info)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    /// Resolve or create a chat row. Updates title/type/timestamp on existing rows.
    pub fn resolve_or_create_chat_id(
        &self,
        channel: &str,
        external_chat_id: &str,
        chat_title: Option<&str>,
        chat_type: &str,
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
                         last_message_time = ?4
                     WHERE chat_id = ?1",
                    params![chat_id, chat_title, chat_type, now],
                )?;
                return Ok(chat_id);
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => {}
            Err(error) => return Err(error.into()),
        }

        conn.execute(
            "INSERT INTO chats(chat_title, chat_type, last_message_time, channel, external_chat_id)
             VALUES(?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(channel, external_chat_id) DO UPDATE SET
                chat_title = COALESCE(excluded.chat_title, chats.chat_title),
                chat_type = excluded.chat_type,
                last_message_time = excluded.last_message_time",
            params![chat_title, chat_type, now, channel, external_chat_id],
        )?;
        conn.query_row(
            "SELECT chat_id FROM chats WHERE channel = ?1 AND external_chat_id = ?2 LIMIT 1",
            params![channel, external_chat_id],
            |row| row.get::<_, i64>(0),
        )
        .map_err(Into::into)
    }

    /// Insert or replace a message record.
    pub fn store_message(&self, message: &StoredMessage) -> Result<(), StorageError> {
        let conn = self.lock_conn()?;
        conn.execute(
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
        Ok(())
    }

    /// Fetch the most recent `limit` messages for a chat, ordered oldest-first.
    pub fn get_recent_messages(
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
            .query_map(params![chat_id, limit as i64], |row| {
                Ok(StoredMessage {
                    id: row.get(0)?,
                    chat_id: row.get(1)?,
                    sender_name: row.get(2)?,
                    content: row.get(3)?,
                    is_from_bot: row.get::<_, i32>(4)? != 0,
                    timestamp: row.get(5)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        messages.reverse();
        Ok(messages)
    }

    /// Fetch all messages for a chat, ordered by timestamp ascending.
    pub fn get_all_messages(&self, chat_id: i64) -> Result<Vec<StoredMessage>, StorageError> {
        let conn = self.lock_conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, chat_id, sender_name, content, is_from_bot, timestamp
             FROM messages
             WHERE chat_id = ?1
             ORDER BY timestamp ASC",
        )?;
        stmt.query_map(params![chat_id], |row| {
            Ok(StoredMessage {
                id: row.get(0)?,
                chat_id: row.get(1)?,
                sender_name: row.get(2)?,
                content: row.get(3)?,
                is_from_bot: row.get::<_, i32>(4)? != 0,
                timestamp: row.get(5)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(Into::into)
    }

    /// List all chats with their last message preview, ordered by most recent activity.
    pub fn list_sessions(&self) -> Result<Vec<SessionSummary>, StorageError> {
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
                ) AS last_message_preview
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
            })
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(Into::into)
    }

    /// Upsert the serialized session JSON for a chat.
    pub fn save_session(&self, chat_id: i64, messages_json: &str) -> Result<(), StorageError> {
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

    /// セッションスナップショットとメッセージ履歴を削除する。
    pub fn clear_session(&self, chat_id: i64) -> Result<(), StorageError> {
        let conn = self.lock_conn()?;
        conn.execute("DELETE FROM sessions WHERE chat_id = ?1", params![chat_id])?;
        conn.execute("DELETE FROM messages WHERE chat_id = ?1", params![chat_id])?;
        Ok(())
    }

    /// Atomically store a message and update the session snapshot.
    /// Uses optimistic concurrency via `expected_updated_at`; returns
    /// `SessionSnapshotConflict` on stale writes.
    pub fn store_message_with_session(
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

    /// Load the session snapshot: serialized messages JSON plus recent message records.
    pub fn load_session_snapshot(
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
                .query_map(params![chat_id, limit as i64], |row| {
                    Ok(StoredMessage {
                        id: row.get(0)?,
                        chat_id: row.get(1)?,
                        sender_name: row.get(2)?,
                        content: row.get(3)?,
                        is_from_bot: row.get::<_, i32>(4)? != 0,
                        timestamp: row.get(5)?,
                    })
                })?
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

    /// Load the raw session JSON and `updated_at` timestamp for a chat.
    /// Returns `None` if the chat has no saved session.
    pub fn load_session(&self, chat_id: i64) -> Result<Option<(String, String)>, StorageError> {
        let conn = self.lock_conn()?;
        let result = conn.query_row(
            "SELECT messages_json, updated_at FROM sessions WHERE chat_id = ?1",
            params![chat_id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        );
        match result {
            Ok(pair) => Ok(Some(pair)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    fn lock_conn(&self) -> Result<std::sync::MutexGuard<'_, Connection>, StorageError> {
        self.conn
            .lock()
            .map_err(|error| StorageError::InitFailed(error.to_string()))
    }

    /// Store a tool call record.
    pub fn store_tool_call(&self, tool_call: &ToolCall) -> Result<(), StorageError> {
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

    /// Update the output of a tool call.
    pub fn update_tool_call_output(&self, id: &str, output: &str) -> Result<(), StorageError> {
        let conn = self.lock_conn()?;
        let rows_updated = conn.execute(
            "UPDATE tool_calls SET tool_output = ?1 WHERE id = ?2",
            params![output, id],
        )?;
        if rows_updated == 0 {
            return Err(StorageError::NotFound(format!("tool_call:{id}")));
        }
        Ok(())
    }

    /// Get all tool calls for a specific message within a chat.
    pub fn get_tool_calls_for_message(
        &self,
        chat_id: i64,
        message_id: &str,
    ) -> Result<Vec<ToolCall>, StorageError> {
        let conn = self.lock_conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, chat_id, message_id, tool_name, tool_input, tool_output, timestamp
             FROM tool_calls WHERE chat_id = ?1 AND message_id = ?2 ORDER BY timestamp",
        )?;

        let calls = stmt
            .query_map(params![chat_id, message_id], |row| {
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

    /// Get all tool calls for a specific chat.
    pub fn get_tool_calls_for_chat(&self, chat_id: i64) -> Result<Vec<ToolCall>, StorageError> {
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
}

// セッション一覧の表示名: chat_title → external_chat_id のチャネルプレフィクス除去 → そのまま
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

#[cfg(test)]
mod tests {
    use crate::error::StorageError;

    use super::{Database, StoredMessage, ToolCall};

    fn test_db() -> (Database, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = Database::new(dir.path().to_str().expect("path")).expect("db");
        (db, dir)
    }

    #[test]
    fn message_full_lifecycle() {
        let (db, _dir) = test_db();

        for index in 0..5 {
            db.store_message(&StoredMessage {
                id: format!("chat1_msg{index}"),
                chat_id: 100,
                sender_name: "alice".into(),
                content: format!("chat1 message {index}"),
                is_from_bot: false,
                timestamp: format!("2024-01-01T00:00:{index:02}Z"),
            })
            .expect("store message");
        }

        for index in 0..3 {
            db.store_message(&StoredMessage {
                id: format!("chat2_msg{index}"),
                chat_id: 200,
                sender_name: "bob".into(),
                content: format!("chat2 message {index}"),
                is_from_bot: false,
                timestamp: format!("2024-01-01T00:00:{index:02}Z"),
            })
            .expect("store message");
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

        assert!(db.load_session(100).expect("missing session").is_none());

        let json1 = r#"[{"role":"user","content":"hello"}]"#;
        db.save_session(100, json1).expect("save session");

        let (loaded, first_updated_at) = db.load_session(100).expect("load session").expect("row");
        assert_eq!(loaded, json1);
        assert!(!first_updated_at.is_empty());

        std::thread::sleep(std::time::Duration::from_millis(10));

        let json2 = r#"[{"role":"user","content":"hello"},{"role":"assistant","content":"hi"}]"#;
        db.save_session(100, json2).expect("update session");

        let (loaded_again, second_updated_at) = db
            .load_session(100)
            .expect("load updated session")
            .expect("row");
        assert_eq!(loaded_again, json2);
        assert!(second_updated_at >= first_updated_at);
        assert!(db.load_session(200).expect("other chat").is_none());
    }

    #[test]
    fn clear_session_deletes_snapshots_and_messages() {
        let (db, _dir) = test_db();
        let chat_id = 100;

        db.save_session(chat_id, r#"[{"role":"user","content":"hello"}]"#)
            .expect("save session");
        db.store_message(&StoredMessage {
            id: "msg-1".to_string(),
            chat_id,
            sender_name: "alice".to_string(),
            content: "hello".to_string(),
            is_from_bot: false,
            timestamp: "2024-01-01T00:00:00Z".to_string(),
        })
        .expect("store first message");
        db.store_message(&StoredMessage {
            id: "msg-2".to_string(),
            chat_id,
            sender_name: "assistant".to_string(),
            content: "hi".to_string(),
            is_from_bot: true,
            timestamp: "2024-01-01T00:00:01Z".to_string(),
        })
        .expect("store second message");

        db.clear_session(chat_id).expect("clear session");

        assert!(db.load_session(chat_id).expect("load session").is_none());
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
            .resolve_or_create_chat_id("cli", "cli:local-dev", Some("local-dev"), "cli")
            .expect("create chat");
        let second = db
            .resolve_or_create_chat_id("cli", "cli:local-dev", Some("renamed"), "cli")
            .expect("reuse chat");

        assert_eq!(first, second);
        assert!(first > 0);
    }

    #[test]
    fn list_sessions_prefers_logical_session_name() {
        let (db, _dir) = test_db();

        let chat_id = db
            .resolve_or_create_chat_id("cli", "cli:demo", Some("demo"), "cli")
            .expect("create chat");
        db.store_message(&StoredMessage {
            id: "msg-1".to_string(),
            chat_id,
            sender_name: "local_user".to_string(),
            content: "hello".to_string(),
            is_from_bot: false,
            timestamp: "2024-01-01T00:00:00Z".to_string(),
        })
        .expect("store message");
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
            )
            .expect("reopen chat");
        assert_eq!(reopened_chat_id, chat_id);
    }

    #[test]
    fn update_tool_call_output_fails_when_tool_call_is_missing() {
        let (db, _dir) = test_db();

        let error = db
            .update_tool_call_output("missing-tool-call", "output")
            .expect_err("missing tool call should fail");

        assert!(matches!(error, StorageError::NotFound(_)));
    }

    #[test]
    fn update_tool_call_output_updates_existing_record() {
        let (db, _dir) = test_db();
        let chat_id = db
            .resolve_or_create_chat_id("web", "web:message-1", Some("message-1"), "web")
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
        db.update_tool_call_output("tool-1", "done")
            .expect("update tool call");

        let calls = db
            .get_tool_calls_for_message(chat_id, "message-1")
            .expect("load tool calls");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].tool_output.as_deref(), Some("done"));
    }

    #[test]
    fn schema_version_is_tracked_on_init() {
        let (db, _dir) = test_db();
        let version = db.schema_version().expect("schema version");
        assert_eq!(version, 1, "新規DBはスキーマバージョン1で初期化される");
    }

    #[test]
    fn migration_history_is_recorded() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = Database::new(dir.path().to_str().expect("path")).expect("db");

        let conn = db.conn.lock().expect("lock");
        let mut stmt = conn
            .prepare("SELECT version, note FROM schema_migrations ORDER BY version")
            .expect("prepare");
        let rows: Vec<(i64, String)> = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .expect("query")
            .collect::<Result<Vec<_>, _>>()
            .expect("collect");

        assert_eq!(rows.len(), 1, "v1 マイグレーションが1件記録される");
        assert_eq!(rows[0].0, 1);
        assert!(rows[0].1.contains("initial schema"));
    }

    #[test]
    fn reopen_db_preserves_schema_version() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().to_str().expect("path").to_string();

        let first_version = {
            let db = Database::new(&path).expect("db");
            db.schema_version().expect("version")
        };

        let db = Database::new(&path).expect("reopen db");
        let second_version = db.schema_version().expect("version");

        assert_eq!(
            first_version, second_version,
            "再起動してもバージョンは変わらない"
        );
        assert_eq!(second_version, 1);
    }
}
