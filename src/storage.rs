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

/// LLM使用量ログの記録用データ。
pub struct LlmUsageLogEntry<'a> {
    pub chat_id: i64,
    pub caller_channel: &'a str,
    pub provider: &'a str,
    pub model: &'a str,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub request_kind: &'a str,
}

/// LLM使用量の集計サマリ。
#[derive(Debug, PartialEq)]
pub struct LlmUsageSummary {
    pub requests: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub total_tokens: i64,
    pub last_request_at: Option<String>,
}

/// モデル別のLLM使用量サマリ。
#[derive(Debug, PartialEq)]
pub struct LlmModelUsageSummary {
    pub model: String,
    pub requests: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub total_tokens: i64,
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
const SCHEMA_VERSION: i64 = 2;

impl Database {
    /// Open (or create) the database at `db_path` and initialize schema.
    pub fn new(db_path: &Path) -> Result<Self, StorageError> {
        let legacy_db = db_path
            .parent()
            .and_then(|runtime| runtime.parent())
            .map(|root| root.join("data").join("egopulse.db"))
            .unwrap_or_else(|| Path::new("data").join("egopulse.db"));
        if legacy_db.exists() && !db_path.exists() {
            return Err(StorageError::InitFailed(format!(
                "legacy_db_pending_migration: found {}, but {} does not exist. \
                 run 'mv {} {}' to migrate.",
                legacy_db.display(),
                db_path.display(),
                legacy_db.display(),
                db_path.display(),
            )));
        }

        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

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

    if version < 2 {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS llm_usage_logs (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                chat_id INTEGER NOT NULL,
                caller_channel TEXT NOT NULL,
                provider TEXT NOT NULL,
                model TEXT NOT NULL,
                input_tokens INTEGER NOT NULL,
                output_tokens INTEGER NOT NULL,
                total_tokens INTEGER NOT NULL,
                request_kind TEXT NOT NULL DEFAULT 'agent_loop',
                created_at TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_llm_usage_chat_created
                ON llm_usage_logs(chat_id, created_at);

            CREATE INDEX IF NOT EXISTS idx_llm_usage_created
                ON llm_usage_logs(created_at);",
        )?;
        set_schema_version(conn, 2, "add llm_usage_logs table for LLM usage tracking")?;
        version = 2;
    }

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

    /// LLM使用量ログを記録し、挿入された行IDを返す。
    pub fn log_llm_usage(&self, entry: &LlmUsageLogEntry<'_>) -> Result<i64, StorageError> {
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

    /// LLM使用量の集計サマリを取得する。
    ///
    /// `chat_id`, `since`, `request_kind` でフィルタリング可能。
    pub fn get_llm_usage_summary(
        &self,
        chat_id: Option<i64>,
        since: Option<&str>,
        request_kind: Option<&str>,
    ) -> Result<LlmUsageSummary, StorageError> {
        let conn = self.lock_conn()?;

        let mut sql = String::from(
            "SELECT COUNT(*) as requests,
                    COALESCE(SUM(input_tokens), 0) as input_tokens,
                    COALESCE(SUM(output_tokens), 0) as output_tokens,
                    COALESCE(SUM(total_tokens), 0) as total_tokens,
                    MAX(created_at) as last_request_at
             FROM llm_usage_logs WHERE 1=1",
        );
        let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

        if let Some(cid) = chat_id {
            sql.push_str(" AND chat_id = ?");
            param_values.push(Box::new(cid));
        }
        if let Some(s) = since {
            let normalized = chrono::DateTime::parse_from_rfc3339(s)
                .map(|dt| dt.with_timezone(&chrono::Utc).to_rfc3339())
                .unwrap_or_else(|_| s.to_string());
            sql.push_str(" AND created_at >= ?");
            param_values.push(Box::new(normalized));
        }
        if let Some(kind) = request_kind {
            sql.push_str(" AND request_kind = ?");
            param_values.push(Box::new(kind.to_string()));
        }

        let params_refs: Vec<&dyn rusqlite::types::ToSql> =
            param_values.iter().map(|p| p.as_ref()).collect();

        let result = conn.query_row(&sql, params_refs.as_slice(), |row| {
            Ok(LlmUsageSummary {
                requests: row.get(0)?,
                input_tokens: row.get(1)?,
                output_tokens: row.get(2)?,
                total_tokens: row.get(3)?,
                last_request_at: row.get(4)?,
            })
        });

        match result {
            Ok(summary) => Ok(summary),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(LlmUsageSummary {
                requests: 0,
                input_tokens: 0,
                output_tokens: 0,
                total_tokens: 0,
                last_request_at: None,
            }),
            Err(e) => Err(e.into()),
        }
    }

    /// モデル別のLLM使用量サマリを取得する。
    ///
    /// `total_tokens` の降順で返す。`chat_id`, `since`, `request_kind` でフィルタリング可能。
    pub fn get_llm_usage_by_model(
        &self,
        chat_id: Option<i64>,
        since: Option<&str>,
        request_kind: Option<&str>,
    ) -> Result<Vec<LlmModelUsageSummary>, StorageError> {
        let conn = self.lock_conn()?;

        let mut sql = String::from(
            "SELECT model,
                    COUNT(*) as requests,
                    COALESCE(SUM(input_tokens), 0) as input_tokens,
                    COALESCE(SUM(output_tokens), 0) as output_tokens,
                    COALESCE(SUM(total_tokens), 0) as total_tokens
             FROM llm_usage_logs WHERE 1=1",
        );
        let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

        if let Some(cid) = chat_id {
            sql.push_str(" AND chat_id = ?");
            param_values.push(Box::new(cid));
        }
        if let Some(s) = since {
            let normalized = chrono::DateTime::parse_from_rfc3339(s)
                .map(|dt| dt.with_timezone(&chrono::Utc).to_rfc3339())
                .unwrap_or_else(|_| s.to_string());
            sql.push_str(" AND created_at >= ?");
            param_values.push(Box::new(normalized));
        }
        if let Some(kind) = request_kind {
            sql.push_str(" AND request_kind = ?");
            param_values.push(Box::new(kind.to_string()));
        }

        sql.push_str(" GROUP BY model ORDER BY total_tokens DESC");

        let params_refs: Vec<&dyn rusqlite::types::ToSql> =
            param_values.iter().map(|p| p.as_ref()).collect();

        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt
            .query_map(params_refs.as_slice(), |row| {
                Ok(LlmModelUsageSummary {
                    model: row.get(0)?,
                    requests: row.get(1)?,
                    input_tokens: row.get(2)?,
                    output_tokens: row.get(3)?,
                    total_tokens: row.get(4)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(rows)
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

    use super::{Database, LlmUsageLogEntry, StoredMessage, ToolCall};
    use rusqlite::Connection;

    fn test_db() -> (Database, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("runtime").join("egopulse.db");
        let db = Database::new(&db_path).expect("db");
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
        assert_eq!(version, 2, "新規DBはスキーマバージョン2で初期化される");
    }

    #[test]
    fn migration_history_is_recorded() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = Database::new(&dir.path().join("runtime").join("egopulse.db")).expect("db");

        let conn = db.conn.lock().expect("lock");
        let mut stmt = conn
            .prepare("SELECT version, note FROM schema_migrations ORDER BY version")
            .expect("prepare");
        let rows: Vec<(i64, String)> = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .expect("query")
            .collect::<Result<Vec<_>, _>>()
            .expect("collect");

        assert_eq!(rows.len(), 2, "v1・v2 マイグレーションが2件記録される");
        assert_eq!(rows[0].0, 1);
        assert!(rows[0].1.contains("initial schema"));
        assert_eq!(rows[1].0, 2);
        assert!(rows[1].1.contains("llm_usage_logs"));
    }

    #[test]
    fn reopen_db_preserves_schema_version() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("runtime").join("egopulse.db");

        let first_version = {
            let db = Database::new(&db_path).expect("db");
            db.schema_version().expect("version")
        };

        let db = Database::new(&db_path).expect("reopen db");
        let second_version = db.schema_version().expect("version");

        assert_eq!(
            first_version, second_version,
            "再起動してもバージョンは変わらない"
        );
        assert_eq!(second_version, 2);
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
        assert!(created_at.contains('T'), "RFC3339形式であること");
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
    fn get_llm_usage_summary_returns_zeros_when_empty() {
        let (db, _dir) = test_db();

        let summary = db.get_llm_usage_summary(None, None, None).expect("summary");

        assert_eq!(summary.requests, 0);
        assert_eq!(summary.input_tokens, 0);
        assert_eq!(summary.output_tokens, 0);
        assert_eq!(summary.total_tokens, 0);
        assert!(summary.last_request_at.is_none());
    }

    #[test]
    fn get_llm_usage_summary_aggregates_all() {
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
        .expect("log 1");
        db.log_llm_usage(&LlmUsageLogEntry {
            chat_id: 100,
            caller_channel: "tui",
            provider: "openai",
            model: "gpt-4",
            input_tokens: 200,
            output_tokens: 100,
            request_kind: "agent_loop",
        })
        .expect("log 2");
        db.log_llm_usage(&LlmUsageLogEntry {
            chat_id: 200,
            caller_channel: "web",
            provider: "openai",
            model: "gpt-4",
            input_tokens: 300,
            output_tokens: 150,
            request_kind: "agent_loop",
        })
        .expect("log 3");

        let summary = db.get_llm_usage_summary(None, None, None).expect("summary");

        assert_eq!(summary.requests, 3);
        assert_eq!(summary.input_tokens, 600);
        assert_eq!(summary.output_tokens, 300);
        assert_eq!(summary.total_tokens, 900);
        assert!(summary.last_request_at.is_some());
    }

    #[test]
    fn get_llm_usage_summary_filters_by_chat_id() {
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
        .expect("log 1");
        db.log_llm_usage(&LlmUsageLogEntry {
            chat_id: 200,
            caller_channel: "web",
            provider: "openai",
            model: "gpt-4",
            input_tokens: 200,
            output_tokens: 100,
            request_kind: "agent_loop",
        })
        .expect("log 2");

        let summary = db
            .get_llm_usage_summary(Some(100), None, None)
            .expect("summary");

        assert_eq!(summary.requests, 1);
        assert_eq!(summary.input_tokens, 100);
        assert_eq!(summary.output_tokens, 50);
    }

    #[test]
    fn get_llm_usage_summary_filters_by_since() {
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
        .expect("log 1");
        std::thread::sleep(std::time::Duration::from_millis(10));
        let cutoff = chrono::Utc::now().to_rfc3339();
        std::thread::sleep(std::time::Duration::from_millis(10));
        db.log_llm_usage(&LlmUsageLogEntry {
            chat_id: 100,
            caller_channel: "tui",
            provider: "openai",
            model: "gpt-4",
            input_tokens: 200,
            output_tokens: 100,
            request_kind: "agent_loop",
        })
        .expect("log 2");

        let summary = db
            .get_llm_usage_summary(None, Some(&cutoff), None)
            .expect("summary");

        assert_eq!(summary.requests, 1);
        assert_eq!(summary.input_tokens, 200);
    }

    #[test]
    fn get_llm_usage_summary_filters_by_chat_id_and_since() {
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
        .expect("log 1");
        std::thread::sleep(std::time::Duration::from_millis(10));
        let cutoff = chrono::Utc::now().to_rfc3339();
        std::thread::sleep(std::time::Duration::from_millis(10));
        db.log_llm_usage(&LlmUsageLogEntry {
            chat_id: 200,
            caller_channel: "web",
            provider: "openai",
            model: "gpt-4",
            input_tokens: 200,
            output_tokens: 100,
            request_kind: "agent_loop",
        })
        .expect("log 2");
        db.log_llm_usage(&LlmUsageLogEntry {
            chat_id: 100,
            caller_channel: "tui",
            provider: "openai",
            model: "gpt-4",
            input_tokens: 300,
            output_tokens: 150,
            request_kind: "agent_loop",
        })
        .expect("log 3");

        let summary = db
            .get_llm_usage_summary(Some(100), Some(&cutoff), None)
            .expect("summary");

        assert_eq!(summary.requests, 1);
        assert_eq!(summary.input_tokens, 300);
    }

    #[test]
    fn get_llm_usage_by_model_groups_correctly() {
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
        .expect("log 1");
        db.log_llm_usage(&LlmUsageLogEntry {
            chat_id: 100,
            caller_channel: "tui",
            provider: "openai",
            model: "gpt-4",
            input_tokens: 200,
            output_tokens: 100,
            request_kind: "agent_loop",
        })
        .expect("log 2");
        db.log_llm_usage(&LlmUsageLogEntry {
            chat_id: 100,
            caller_channel: "tui",
            provider: "openai",
            model: "claude-3",
            input_tokens: 300,
            output_tokens: 150,
            request_kind: "agent_loop",
        })
        .expect("log 3");

        let models = db.get_llm_usage_by_model(None, None, None).expect("models");

        assert_eq!(models.len(), 2);
        let gpt4 = models.iter().find(|m| m.model == "gpt-4").expect("gpt-4");
        assert_eq!(gpt4.requests, 2);
        assert_eq!(gpt4.input_tokens, 300);
        assert_eq!(gpt4.output_tokens, 150);

        let claude = models
            .iter()
            .find(|m| m.model == "claude-3")
            .expect("claude-3");
        assert_eq!(claude.requests, 1);
        assert_eq!(claude.input_tokens, 300);
    }

    #[test]
    fn get_llm_usage_by_model_orders_by_total_tokens_desc() {
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
        .expect("log 1");
        db.log_llm_usage(&LlmUsageLogEntry {
            chat_id: 100,
            caller_channel: "tui",
            provider: "openai",
            model: "claude-3",
            input_tokens: 300,
            output_tokens: 150,
            request_kind: "agent_loop",
        })
        .expect("log 2");

        let models = db.get_llm_usage_by_model(None, None, None).expect("models");

        assert_eq!(models.len(), 2);
        assert!(
            models[0].total_tokens >= models[1].total_tokens,
            "total_tokens降順であること"
        );
        assert_eq!(models[0].model, "claude-3");
    }

    #[test]
    fn migration_v2_creates_llm_usage_logs_table() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("runtime").join("egopulse.db");
        let db = Database::new(&db_path).expect("db");

        let conn = db.conn.lock().expect("lock");
        let table_exists: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name='llm_usage_logs'",
                [],
                |row| row.get(0),
            )
            .expect("check table");

        assert!(table_exists, "llm_usage_logsテーブルが存在すること");

        let index_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name LIKE 'idx_llm_usage_%'",
                [],
                |row| row.get(0),
            )
            .expect("check indexes");

        assert_eq!(index_count, 2, "2つのインデックスが作成されること");
    }

    #[test]
    fn migration_v2_applied_on_existing_db() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("runtime").join("egopulse.db");
        std::fs::create_dir_all(db_path.parent().expect("parent")).expect("create dir");

        // 生のConnectionでv1スキーマを手動構築（db_meta + schema_migrations + v1テーブル）
        {
            let conn = Connection::open(&db_path).expect("open");
            conn.execute_batch("PRAGMA journal_mode=WAL;").expect("wal");
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS db_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
                 CREATE TABLE IF NOT EXISTS schema_migrations (version INTEGER PRIMARY KEY, applied_at TEXT NOT NULL, note TEXT);
                 INSERT OR REPLACE INTO db_meta (key, value) VALUES ('schema_version', '1');
                 INSERT OR REPLACE INTO schema_migrations (version, applied_at, note) VALUES (1, '2025-01-01T00:00:00Z', 'test v1');
                 CREATE TABLE IF NOT EXISTS chats (chat_id INTEGER PRIMARY KEY);
                 CREATE TABLE IF NOT EXISTS messages (id TEXT NOT NULL, chat_id INTEGER NOT NULL, PRIMARY KEY (id, chat_id));
                 CREATE TABLE IF NOT EXISTS sessions (chat_id INTEGER PRIMARY KEY, messages_json TEXT NOT NULL, updated_at TEXT NOT NULL);
                 CREATE TABLE IF NOT EXISTS tool_calls (id TEXT PRIMARY KEY);",
            )
            .expect("create v1 schema");
        }

        let db = Database::new(&db_path).expect("reopen");
        let version = db.schema_version().expect("version");
        assert_eq!(version, 2);

        let conn = db.conn.lock().expect("lock");
        let table_exists: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name='llm_usage_logs'",
                [],
                |row| row.get(0),
            )
            .expect("check table");
        assert!(table_exists);
    }

    #[test]
    fn schema_version_increments_to_2() {
        let (db, _dir) = test_db();
        let version = db.schema_version().expect("version");
        assert_eq!(
            version, 2,
            "スキーマバージョンが2にインクリメントされていること"
        );
    }
}
