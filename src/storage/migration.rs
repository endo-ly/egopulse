//! スキーマ定義・マイグレーション。

use rusqlite::{Connection, OptionalExtension, params};

use crate::error::StorageError;

/// 現在のスキーマバージョン。
///
/// 新しいマイグレーションを追加する際はこの値をインクリメントし、
/// `run_migrations` に対応する `if version < N` ブロックを追加する。
pub(super) const SCHEMA_VERSION: i64 = 4;

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

pub(super) fn set_schema_version_in_tx(
    tx: &rusqlite::Transaction<'_>,
    version: i64,
    note: &str,
) -> Result<(), StorageError> {
    set_schema_version(tx, version, note)
}

/// 未適用のマイグレーションを逐次実行する。
///
/// 各マイグレーションは `if version < N` でガードされ、
/// 適用後に `set_schema_version` でバージョンを更新する。
/// `SCHEMA_VERSION` に到達したら完了。
pub(super) fn run_migrations(conn: &Connection) -> Result<(), StorageError> {
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

    if version < 3 {
        let tx = conn.unchecked_transaction()?;
        tx.execute_batch(
            "DROP INDEX IF EXISTS idx_tool_calls_chat_id;
            DROP INDEX IF EXISTS idx_tool_calls_chat_message_id;

            CREATE TABLE IF NOT EXISTS tool_calls_v3 (
                id TEXT NOT NULL,
                chat_id INTEGER NOT NULL,
                message_id TEXT NOT NULL,
                tool_name TEXT NOT NULL,
                tool_input TEXT NOT NULL,
                tool_output TEXT,
                timestamp TEXT NOT NULL,
                PRIMARY KEY (id, chat_id, message_id),
                FOREIGN KEY (chat_id) REFERENCES chats(chat_id)
            );

            INSERT OR IGNORE INTO tool_calls_v3
                (id, chat_id, message_id, tool_name, tool_input, tool_output, timestamp)
            SELECT
                id,
                COALESCE(chat_id, 0),
                COALESCE(message_id, ''),
                COALESCE(tool_name, ''),
                COALESCE(tool_input, ''),
                tool_output,
                COALESCE(timestamp, '')
            FROM tool_calls;

            DROP TABLE tool_calls;
            ALTER TABLE tool_calls_v3 RENAME TO tool_calls;

            CREATE INDEX IF NOT EXISTS idx_tool_calls_chat_id
                ON tool_calls(chat_id);

            CREATE INDEX IF NOT EXISTS idx_tool_calls_chat_message_id
                ON tool_calls(chat_id, message_id);",
        )?;
        set_schema_version_in_tx(
            &tx,
            3,
            "scope tool call uniqueness to chat and assistant message",
        )?;
        tx.commit()?;
        version = 3;
    }

    if version < 4 {
        let tx = conn.unchecked_transaction()?;
        tx.execute_batch("ALTER TABLE chats ADD COLUMN agent_id TEXT NOT NULL DEFAULT 'default';")?;
        set_schema_version_in_tx(&tx, 4, "add NOT NULL agent_id to chats (default: default)")?;
        tx.commit()?;
        version = 4;
    }

    debug_assert_eq!(version, SCHEMA_VERSION, "all migrations applied");
    Ok(())
}

#[cfg(test)]
mod tests {
    use rusqlite::Connection;

    fn test_db() -> super::super::Database {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("runtime").join("egopulse.db");
        super::super::Database::new(&db_path).expect("db")
    }

    #[test]
    fn migration_history_is_recorded() {
        let db = test_db();

        let conn = db.conn.lock().expect("lock");
        let mut stmt = conn
            .prepare("SELECT version, note FROM schema_migrations ORDER BY version")
            .expect("prepare");
        let rows: Vec<(i64, String)> = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .expect("query")
            .collect::<Result<Vec<_>, _>>()
            .expect("collect");

        assert_eq!(rows.len(), 4);
        assert_eq!(rows[0].0, 1);
        assert!(rows[0].1.contains("initial schema"));
        assert_eq!(rows[1].0, 2);
        assert!(rows[1].1.contains("llm_usage_logs"));
        assert_eq!(rows[2].0, 3);
        assert!(rows[2].1.contains("tool call"));
        assert_eq!(rows[3].0, 4);
        assert!(rows[3].1.contains("agent_id"));
    }

    #[test]
    fn migration_v2_creates_llm_usage_logs_table() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("runtime").join("egopulse.db");
        let db = super::super::Database::new(&db_path).expect("db");

        let conn = db.conn.lock().expect("lock");
        let table_exists: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name='llm_usage_logs'",
                [],
                |row| row.get(0),
            )
            .expect("check table");

        assert!(table_exists);

        let index_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name LIKE 'idx_llm_usage_%'",
                [],
                |row| row.get(0),
            )
            .expect("check indexes");

        assert_eq!(index_count, 2);
    }

    #[test]
    fn migration_v4_adds_agent_id_to_chats() {
        let db = test_db();

        let conn = db.conn.lock().expect("lock");
        let has_agent_id: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM pragma_table_info('chats') WHERE name = 'agent_id'",
                [],
                |row| row.get(0),
            )
            .expect("check column");
        assert!(has_agent_id);
    }

    #[test]
    fn migration_v4_agent_id_is_not_null() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("runtime").join("egopulse.db");
        std::fs::create_dir_all(db_path.parent().expect("parent")).expect("create dir");

        // Create a v3 DB with a chats row (no agent_id column)
        {
            let conn = Connection::open(&db_path).expect("open");
            conn.execute_batch("PRAGMA journal_mode=WAL;").expect("wal");
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS db_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
                 CREATE TABLE IF NOT EXISTS schema_migrations (version INTEGER PRIMARY KEY, applied_at TEXT NOT NULL, note TEXT);
                 INSERT OR REPLACE INTO db_meta (key, value) VALUES ('schema_version', '3');
                 INSERT OR REPLACE INTO schema_migrations (version, applied_at, note) VALUES (3, '2025-01-01T00:00:00Z', 'test v3');
                 CREATE TABLE IF NOT EXISTS chats (
                     chat_id INTEGER PRIMARY KEY,
                     chat_title TEXT,
                     chat_type TEXT NOT NULL DEFAULT 'private',
                     last_message_time TEXT NOT NULL,
                     channel TEXT,
                     external_chat_id TEXT
                 );
                 INSERT INTO chats (chat_id, chat_title, chat_type, last_message_time)
                 VALUES (1, 'test chat', 'private', '2025-01-01T00:00:00Z');",
            )
            .expect("create v3 schema");
        }

        // Open with Database::new() which runs all migrations
        let db = super::super::Database::new(&db_path).expect("reopen");
        let conn = db.conn.lock().expect("lock");

        let agent_id: String = conn
            .query_row("SELECT agent_id FROM chats WHERE chat_id = 1", [], |row| {
                row.get(0)
            })
            .expect("query agent_id");
        assert_eq!(agent_id, "default");
    }

    #[test]
    fn migration_v4_history_is_recorded() {
        let db = test_db();

        let conn = db.conn.lock().expect("lock");
        let has_v4: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM schema_migrations WHERE version = 4",
                [],
                |row| row.get(0),
            )
            .expect("check v4 record");
        assert!(has_v4);
    }

    #[test]
    fn migration_v4_from_v3_db() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("runtime").join("egopulse.db");
        std::fs::create_dir_all(db_path.parent().expect("parent")).expect("create dir");

        // Create a full v3 DB with known data
        {
            let conn = Connection::open(&db_path).expect("open");
            conn.execute_batch("PRAGMA journal_mode=WAL;").expect("wal");
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS db_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
                 CREATE TABLE IF NOT EXISTS schema_migrations (version INTEGER PRIMARY KEY, applied_at TEXT NOT NULL, note TEXT);
                 INSERT OR REPLACE INTO db_meta (key, value) VALUES ('schema_version', '3');
                 INSERT OR REPLACE INTO schema_migrations (version, applied_at, note) VALUES (3, '2025-01-01T00:00:00Z', 'test v3');
                 CREATE TABLE IF NOT EXISTS chats (
                     chat_id INTEGER PRIMARY KEY,
                     chat_title TEXT,
                     chat_type TEXT NOT NULL DEFAULT 'private',
                     last_message_time TEXT NOT NULL,
                     channel TEXT,
                     external_chat_id TEXT
                 );
                 CREATE TABLE IF NOT EXISTS messages (
                     id TEXT NOT NULL,
                     chat_id INTEGER NOT NULL,
                     sender_name TEXT NOT NULL,
                     content TEXT NOT NULL,
                     is_from_bot INTEGER NOT NULL DEFAULT 0,
                     timestamp TEXT NOT NULL,
                     PRIMARY KEY (id, chat_id)
                 );
                 CREATE TABLE IF NOT EXISTS sessions (
                     chat_id INTEGER PRIMARY KEY,
                     messages_json TEXT NOT NULL,
                     updated_at TEXT NOT NULL
                 );
                 CREATE TABLE IF NOT EXISTS tool_calls (
                     id TEXT NOT NULL,
                     chat_id INTEGER NOT NULL,
                     message_id TEXT NOT NULL,
                     tool_name TEXT NOT NULL,
                     tool_input TEXT NOT NULL,
                     tool_output TEXT,
                     timestamp TEXT NOT NULL,
                     PRIMARY KEY (id, chat_id, message_id),
                     FOREIGN KEY (chat_id) REFERENCES chats(chat_id)
                 );
                 INSERT INTO chats (chat_id, chat_title, chat_type, last_message_time)
                 VALUES (42, 'v3 chat', 'group', '2025-06-15T12:00:00Z');",
            )
            .expect("create v3 schema");
        }

        // Open with Database::new() which runs all migrations including v4
        let db = super::super::Database::new(&db_path).expect("reopen");
        let conn = db.conn.lock().expect("lock");

        let agent_id: String = conn
            .query_row("SELECT agent_id FROM chats WHERE chat_id = 42", [], |row| {
                row.get(0)
            })
            .expect("query agent_id");
        assert_eq!(agent_id, "default");
    }

    #[test]
    fn migration_v2_applied_on_existing_db() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("runtime").join("egopulse.db");
        std::fs::create_dir_all(db_path.parent().expect("parent")).expect("create dir");

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
                 CREATE TABLE IF NOT EXISTS tool_calls (
                    id TEXT PRIMARY KEY,
                    chat_id INTEGER NOT NULL,
                    message_id TEXT NOT NULL,
                    tool_name TEXT NOT NULL,
                    tool_input TEXT NOT NULL,
                    tool_output TEXT,
                    timestamp TEXT NOT NULL
                 );",
            )
            .expect("create v1 schema");
        }

        let db = super::super::Database::new(&db_path).expect("reopen");

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
}
