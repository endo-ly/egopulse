use std::path::Path;
use std::sync::{Arc, Mutex};

use rusqlite::OptionalExtension;
use rusqlite::{Connection, params};

use crate::error::StorageError;

pub struct Database {
    conn: Mutex<Connection>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredMessage {
    pub id: String,
    pub chat_id: i64,
    pub sender_name: String,
    pub content: String,
    pub is_from_bot: bool,
    pub timestamp: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredSession {
    pub messages_json: String,
    pub updated_at: String,
    pub provider: String,
    pub model: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatIdentity {
    pub chat_id: i64,
    pub channel: String,
    pub external_chat_id: String,
    pub surface_user: String,
    pub surface_thread: String,
    pub chat_title: Option<String>,
    pub chat_type: String,
    pub last_message_time: String,
}

pub async fn call_blocking<T, F>(db: Arc<Database>, f: F) -> Result<T, StorageError>
where
    T: Send + 'static,
    F: FnOnce(&Database) -> Result<T, StorageError> + Send + 'static,
{
    tokio::task::spawn_blocking(move || f(db.as_ref()))
        .await
        .map_err(|error| StorageError::TaskJoin(error.to_string()))?
}

impl Database {
    pub fn new(data_dir: &str) -> Result<Self, StorageError> {
        let db_path = Path::new(data_dir).join("egopulse.db");
        std::fs::create_dir_all(data_dir)?;

        let conn = Connection::open(db_path)?;
        conn.execute_batch("PRAGMA journal_mode=WAL;")?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS chats (
                chat_id INTEGER PRIMARY KEY,
                chat_title TEXT,
                chat_type TEXT NOT NULL DEFAULT 'cli',
                last_message_time TEXT NOT NULL,
                channel TEXT NOT NULL,
                external_chat_id TEXT NOT NULL,
                surface_user TEXT NOT NULL DEFAULT '',
                surface_thread TEXT NOT NULL DEFAULT ''
            );

            CREATE UNIQUE INDEX IF NOT EXISTS idx_chats_channel_external
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
                updated_at TEXT NOT NULL,
                provider TEXT NOT NULL DEFAULT '',
                model TEXT NOT NULL DEFAULT ''
            );",
        )?;

        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    pub fn resolve_or_create_chat_id(
        &self,
        channel: &str,
        external_chat_id: &str,
        surface_user: &str,
        surface_thread: &str,
        chat_title: Option<&str>,
        chat_type: &str,
    ) -> Result<i64, StorageError> {
        let conn = self.lock_conn()?;
        let now = chrono::Utc::now().to_rfc3339();

        if let Some(chat_id) = conn
            .query_row(
                "SELECT chat_id FROM chats WHERE channel = ?1 AND external_chat_id = ?2 LIMIT 1",
                params![channel, external_chat_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
        {
            conn.execute(
                "UPDATE chats
                 SET chat_title = COALESCE(?2, chat_title),
                     chat_type = ?3,
                     last_message_time = ?4,
                     surface_user = ?5,
                     surface_thread = ?6
                 WHERE chat_id = ?1",
                params![
                    chat_id,
                    chat_title,
                    chat_type,
                    now,
                    surface_user,
                    surface_thread
                ],
            )?;
            return Ok(chat_id);
        }

        let preferred_chat_id = external_chat_id.parse::<i64>().ok();
        if let Some(chat_id) = preferred_chat_id {
            let occupied = conn
                .query_row(
                    "SELECT 1 FROM chats WHERE chat_id = ?1 LIMIT 1",
                    params![chat_id],
                    |_| Ok(()),
                )
                .optional()?
                .is_some();
            if !occupied {
                conn.execute(
                    "INSERT INTO chats(chat_id, chat_title, chat_type, last_message_time, channel, external_chat_id, surface_user, surface_thread)
                     VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                    params![
                        chat_id,
                        chat_title,
                        chat_type,
                        now,
                        channel,
                        external_chat_id,
                        surface_user,
                        surface_thread
                    ],
                )?;
                return Ok(chat_id);
            }
        }

        conn.execute(
            "INSERT INTO chats(chat_title, chat_type, last_message_time, channel, external_chat_id, surface_user, surface_thread)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                chat_title,
                chat_type,
                now,
                channel,
                external_chat_id,
                surface_user,
                surface_thread
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub fn load_chat_identity(&self, chat_id: i64) -> Result<Option<ChatIdentity>, StorageError> {
        let conn = self.lock_conn()?;
        let result = conn.query_row(
            "SELECT chat_id, channel, external_chat_id, surface_user, surface_thread, chat_title, chat_type, last_message_time
             FROM chats
             WHERE chat_id = ?1",
            params![chat_id],
            |row| {
                Ok(ChatIdentity {
                    chat_id: row.get(0)?,
                    channel: row.get(1)?,
                    external_chat_id: row.get(2)?,
                    surface_user: row.get(3)?,
                    surface_thread: row.get(4)?,
                    chat_title: row.get(5)?,
                    chat_type: row.get(6)?,
                    last_message_time: row.get(7)?,
                })
            },
        );
        match result {
            Ok(identity) => Ok(Some(identity)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

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

    pub fn save_session(
        &self,
        chat_id: i64,
        messages_json: &str,
        provider: &str,
        model: &str,
    ) -> Result<(), StorageError> {
        let conn = self.lock_conn()?;
        let now = chrono::Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO sessions (chat_id, messages_json, updated_at, provider, model)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(chat_id) DO UPDATE SET
                messages_json = ?2,
                updated_at = ?3,
                provider = ?4,
                model = ?5",
            params![chat_id, messages_json, now, provider, model],
        )?;
        Ok(())
    }

    pub fn load_session(&self, chat_id: i64) -> Result<Option<StoredSession>, StorageError> {
        let conn = self.lock_conn()?;
        let result = conn.query_row(
            "SELECT messages_json, updated_at, provider, model
             FROM sessions
             WHERE chat_id = ?1",
            params![chat_id],
            |row| {
                Ok(StoredSession {
                    messages_json: row.get(0)?,
                    updated_at: row.get(1)?,
                    provider: row.get(2)?,
                    model: row.get(3)?,
                })
            },
        );
        match result {
            Ok(session) => Ok(Some(session)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    fn lock_conn(&self) -> Result<std::sync::MutexGuard<'_, Connection>, StorageError> {
        self.conn
            .lock()
            .map_err(|error| StorageError::InitFailed(error.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::{Database, StoredMessage};

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
        db.save_session(100, json1, "openai_compatible", "gpt-4o-mini")
            .expect("save session");

        let first = db.load_session(100).expect("load session").expect("row");
        assert_eq!(first.messages_json, json1);
        assert!(!first.updated_at.is_empty());
        assert_eq!(first.provider, "openai_compatible");
        assert_eq!(first.model, "gpt-4o-mini");

        std::thread::sleep(std::time::Duration::from_millis(10));

        let json2 = r#"[{"role":"user","content":"hello"},{"role":"assistant","content":"hi"}]"#;
        db.save_session(100, json2, "openai_compatible", "gpt-4o-mini")
            .expect("update session");

        let second = db
            .load_session(100)
            .expect("load updated session")
            .expect("row");
        assert_eq!(second.messages_json, json2);
        assert!(second.updated_at >= first.updated_at);
        assert!(db.load_session(200).expect("other chat").is_none());
    }

    #[test]
    fn resolve_or_create_chat_id_uses_surface_identity() {
        let (db, _dir) = test_db();

        let first = db
            .resolve_or_create_chat_id(
                "cli",
                "cli:local_user:local-dev",
                "local_user",
                "local-dev",
                Some("local-dev"),
                "cli",
            )
            .expect("create chat");
        let second = db
            .resolve_or_create_chat_id(
                "cli",
                "cli:local_user:local-dev",
                "local_user",
                "local-dev",
                Some("renamed"),
                "cli",
            )
            .expect("reuse chat");

        assert_eq!(first, second);
        assert!(first > 0);
        let identity = db
            .load_chat_identity(first)
            .expect("identity")
            .expect("chat exists");
        assert_eq!(identity.surface_user, "local_user");
        assert_eq!(identity.surface_thread, "local-dev");
    }
}
