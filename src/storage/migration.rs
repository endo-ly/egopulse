//! スキーマ定義・マイグレーション。

use rusqlite::{Connection, OptionalExtension, params};

use crate::error::StorageError;

/// 現在のスキーマバージョン。
///
/// 新しいマイグレーションを追加する際はこの値をインクリメントし、
/// `run_migrations` に対応する `if version < N` ブロックを追加する。
pub(super) const SCHEMA_VERSION: i64 = 8;

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

fn strip_bot_segment(external_chat_id: &str) -> Option<String> {
    let bot_start = external_chat_id.find(":bot:")?;
    let after_bot = &external_chat_id[bot_start + ":bot:".len()..];
    let agent_start = after_bot.find(":agent:")?;
    let before = &external_chat_id[..bot_start];
    let after = &after_bot[agent_start..];
    Some(format!("{before}{after}"))
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
        tx.execute_batch("ALTER TABLE chats ADD COLUMN agent_id TEXT NOT NULL DEFAULT 'lyre';")?;
        set_schema_version_in_tx(&tx, 4, "add NOT NULL agent_id to chats (default: lyre)")?;
        tx.commit()?;
        version = 4;
    }

    if version < 5 {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS sleep_runs (
                id                  TEXT PRIMARY KEY,
                agent_id            TEXT NOT NULL,
                status              TEXT NOT NULL,
                trigger_type        TEXT NOT NULL,
                started_at          TEXT NOT NULL,
                finished_at         TEXT,
                source_chats_json   TEXT NOT NULL DEFAULT '[]',
                source_digest_md    TEXT,
                phases_json         TEXT NOT NULL DEFAULT '[]',
                summary_md          TEXT,
                input_tokens        INTEGER NOT NULL DEFAULT 0,
                output_tokens       INTEGER NOT NULL DEFAULT 0,
                total_tokens        INTEGER NOT NULL DEFAULT 0,
                error_message       TEXT
            );

            CREATE INDEX IF NOT EXISTS idx_sleep_runs_agent_started
                ON sleep_runs(agent_id, started_at);

            CREATE INDEX IF NOT EXISTS idx_sleep_runs_agent_status
                ON sleep_runs(agent_id, status);

            CREATE TABLE IF NOT EXISTS memory_snapshots (
                id              TEXT PRIMARY KEY,
                run_id          TEXT NOT NULL,
                agent_id        TEXT NOT NULL,
                phase           TEXT NOT NULL,
                file            TEXT NOT NULL,
                content_before  TEXT NOT NULL,
                content_after   TEXT NOT NULL,
                created_at      TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_memory_snapshots_run_id
                ON memory_snapshots(run_id);

            CREATE INDEX IF NOT EXISTS idx_memory_snapshots_agent_created
                ON memory_snapshots(agent_id, created_at);",
        )?;
        set_schema_version(
            conn,
            5,
            "add sleep_runs and memory_snapshots tables for long-term memory audit",
        )?;
        version = 5;
    }

    if version < 6 {
        conn.execute_batch(
            "DROP TABLE IF EXISTS memory_snapshots;
             DROP TABLE IF EXISTS sleep_runs;

             CREATE TABLE sleep_runs (
                 id                  TEXT PRIMARY KEY,
                 agent_id            TEXT NOT NULL,
                 status              TEXT NOT NULL,
                 trigger_type        TEXT NOT NULL,
                 started_at          TEXT NOT NULL,
                 finished_at         TEXT,
                 source_chats_json   TEXT NOT NULL DEFAULT '[]',
                 source_digest_md    TEXT,
                 input_tokens        INTEGER NOT NULL DEFAULT 0,
                 output_tokens       INTEGER NOT NULL DEFAULT 0,
                 total_tokens        INTEGER NOT NULL DEFAULT 0,
                 error_message       TEXT
             );

             CREATE INDEX idx_sleep_runs_agent_started
                 ON sleep_runs(agent_id, started_at);

             CREATE INDEX idx_sleep_runs_agent_status
                 ON sleep_runs(agent_id, status);

             CREATE TABLE memory_snapshots (
                 id              TEXT PRIMARY KEY,
                 run_id          TEXT NOT NULL,
                 agent_id        TEXT NOT NULL,
                 file            TEXT NOT NULL,
                 content_before  TEXT NOT NULL,
                 content_after   TEXT NOT NULL,
                 created_at      TEXT NOT NULL
             );

             CREATE INDEX idx_memory_snapshots_run_id
                 ON memory_snapshots(run_id);

             CREATE INDEX idx_memory_snapshots_agent_created
                 ON memory_snapshots(agent_id, created_at);",
        )?;
        set_schema_version(
            conn,
            6,
            "simplify sleep batch audit schema: remove phases_json, summary_md, phase",
        )?;
        version = 6;
    }

    if version < 7 {
        let tx = conn.unchecked_transaction()?;
        tx.execute_batch(
            "ALTER TABLE messages ADD COLUMN message_kind TEXT NOT NULL DEFAULT 'message';
             ALTER TABLE messages ADD COLUMN sender_agent_id TEXT;
             ALTER TABLE messages ADD COLUMN recipient_agent_id TEXT;",
        )?;
        set_schema_version_in_tx(
            &tx,
            7,
            "add message_kind, sender_agent_id, recipient_agent_id to messages",
        )?;
        tx.commit()?;
        version = 7;
    }

    if version < 8 {
        let tx = conn.unchecked_transaction()?;
        // ":bot:<bot_id>" → strip (e.g. "discord:123:bot:main:agent:lyre" → "discord:123:agent:lyre")
        {
            let mut stmt = tx.prepare(
                "SELECT rowid, external_chat_id FROM chats
                 WHERE channel = 'discord'
                   AND external_chat_id LIKE '%:bot:%:agent:%'",
            )?;
            let rows: Vec<(i64, String)> = stmt
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
                .collect::<Result<Vec<_>, _>>()?;
            drop(stmt);

            for (rowid, old_id) in &rows {
                if let Some(new_id) = strip_bot_segment(old_id) {
                    tx.execute(
                        "UPDATE chats SET external_chat_id = ?1 WHERE rowid = ?2",
                        params![new_id, rowid],
                    )?;
                }
            }
        }
        set_schema_version_in_tx(
            &tx,
            8,
            "remove bot_id from Discord session external_chat_id",
        )?;
        tx.commit()?;
        version = 8;
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

        assert_eq!(rows.len(), 7, "v1 through v7");
        assert_eq!(rows[0].0, 1);
        assert!(rows[0].1.contains("initial schema"));
        assert_eq!(rows[1].0, 2);
        assert!(rows[1].1.contains("llm_usage_logs"));
        assert_eq!(rows[2].0, 3);
        assert!(rows[2].1.contains("tool call"));
        assert_eq!(rows[3].0, 4);
        assert!(rows[3].1.contains("agent_id"));
        assert_eq!(rows[4].0, 5);
        assert!(rows[4].1.contains("sleep_runs"));
        assert_eq!(rows[5].0, 6);
        assert!(rows[5].1.contains("simplify"));
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
                     PRIMARY KEY (id, chat_id, message_id)
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
        assert_eq!(agent_id, "lyre");
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
        assert_eq!(agent_id, "lyre");
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

    #[test]
    fn migration_v5_creates_sleep_runs_table() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("runtime").join("egopulse.db");
        let db = super::super::Database::new(&db_path).expect("db");

        let conn = db.conn.lock().expect("lock");
        let table_exists: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name='sleep_runs'",
                [],
                |row| row.get(0),
            )
            .expect("check table");

        assert!(table_exists);
    }

    #[test]
    fn migration_v5_creates_memory_snapshots_table() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("runtime").join("egopulse.db");
        let db = super::super::Database::new(&db_path).expect("db");

        let conn = db.conn.lock().expect("lock");
        let table_exists: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name='memory_snapshots'",
                [],
                |row| row.get(0),
            )
            .expect("check table");

        assert!(table_exists);
    }

    #[test]
    fn migration_v5_creates_four_indexes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("runtime").join("egopulse.db");
        let db = super::super::Database::new(&db_path).expect("db");

        let conn = db.conn.lock().expect("lock");

        let expected_indexes = [
            "idx_sleep_runs_agent_started",
            "idx_sleep_runs_agent_status",
            "idx_memory_snapshots_run_id",
            "idx_memory_snapshots_agent_created",
        ];

        for index_name in &expected_indexes {
            let exists: bool = conn
                .query_row(
                    "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='index' AND name = ?1",
                    [index_name],
                    |row| row.get(0),
                )
                .expect("check index");
            assert!(exists, "expected index {index_name} to exist");
        }
    }

    #[test]
    fn migration_v5_history_is_recorded() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("runtime").join("egopulse.db");
        let db = super::super::Database::new(&db_path).expect("db");

        let conn = db.conn.lock().expect("lock");
        let has_v5: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM schema_migrations WHERE version = 5",
                [],
                |row| row.get(0),
            )
            .expect("check v5 record");
        assert!(has_v5);
    }

    #[test]
    fn migration_v5_from_v4_db() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("runtime").join("egopulse.db");
        std::fs::create_dir_all(db_path.parent().expect("parent")).expect("create dir");

        // Create a full v4 DB with known data
        {
            let conn = Connection::open(&db_path).expect("open");
            conn.execute_batch("PRAGMA journal_mode=WAL;").expect("wal");
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS db_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
                 CREATE TABLE IF NOT EXISTS schema_migrations (version INTEGER PRIMARY KEY, applied_at TEXT NOT NULL, note TEXT);
                 INSERT OR REPLACE INTO db_meta (key, value) VALUES ('schema_version', '4');
                 INSERT OR REPLACE INTO schema_migrations (version, applied_at, note) VALUES (4, '2025-01-01T00:00:00Z', 'test v4');
                 CREATE TABLE IF NOT EXISTS chats (
                     chat_id INTEGER PRIMARY KEY,
                     chat_title TEXT,
                     chat_type TEXT NOT NULL DEFAULT 'private',
                     last_message_time TEXT NOT NULL,
                     channel TEXT,
                     external_chat_id TEXT,
                     agent_id TEXT NOT NULL DEFAULT 'lyre'
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
                 CREATE TABLE IF NOT EXISTS llm_usage_logs (
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
                 INSERT INTO chats (chat_id, chat_title, chat_type, last_message_time)
                 VALUES (42, 'v4 chat', 'group', '2025-06-15T12:00:00Z');",
            )
            .expect("create v4 schema");
        }

        // Open with Database::new() which runs migrations including v5
        let db = super::super::Database::new(&db_path).expect("reopen");
        let conn = db.conn.lock().expect("lock");

        // Verify sleep_runs table exists
        let sleep_runs_exists: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name='sleep_runs'",
                [],
                |row| row.get(0),
            )
            .expect("check sleep_runs table");
        assert!(sleep_runs_exists);

        // Verify memory_snapshots table exists
        let memory_snapshots_exists: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name='memory_snapshots'",
                [],
                |row| row.get(0),
            )
            .expect("check memory_snapshots table");
        assert!(memory_snapshots_exists);

        // Verify schema_migrations has version 5 record
        let has_v5: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM schema_migrations WHERE version = 5",
                [],
                |row| row.get(0),
            )
            .expect("check v5 record");
        assert!(has_v5);
    }

    #[test]
    fn migration_v5_from_fresh_db() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("runtime").join("egopulse.db");
        let db = super::super::Database::new(&db_path).expect("db");

        let conn = db.conn.lock().expect("lock");

        // Verify all tables from v1-v6 exist
        let expected_tables = [
            "chats",
            "messages",
            "sessions",
            "tool_calls",
            "llm_usage_logs",
            "sleep_runs",
            "memory_snapshots",
        ];

        for table_name in &expected_tables {
            let exists: bool = conn
                .query_row(
                    "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name = ?1",
                    [table_name],
                    |row| row.get(0),
                )
                .expect("check table");
            assert!(exists, "expected table {table_name} to exist");
        }
    }

    #[test]
    fn migration_sleep_runs_has_no_phases_json() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("runtime").join("egopulse.db");
        let db = super::super::Database::new(&db_path).expect("db");

        let conn = db.conn.lock().expect("lock");
        let has_column: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM pragma_table_info('sleep_runs') WHERE name = 'phases_json'",
                [],
                |row| row.get(0),
            )
            .expect("check column");
        assert!(!has_column, "sleep_runs should not have phases_json column");
    }

    #[test]
    fn migration_sleep_runs_has_no_summary_md() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("runtime").join("egopulse.db");
        let db = super::super::Database::new(&db_path).expect("db");

        let conn = db.conn.lock().expect("lock");
        let has_column: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM pragma_table_info('sleep_runs') WHERE name = 'summary_md'",
                [],
                |row| row.get(0),
            )
            .expect("check column");
        assert!(!has_column, "sleep_runs should not have summary_md column");
    }

    #[test]
    fn migration_memory_snapshots_has_no_phase() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("runtime").join("egopulse.db");
        let db = super::super::Database::new(&db_path).expect("db");

        let conn = db.conn.lock().expect("lock");
        let has_column: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM pragma_table_info('memory_snapshots') WHERE name = 'phase'",
                [],
                |row| row.get(0),
            )
            .expect("check column");
        assert!(!has_column, "memory_snapshots should not have phase column");
    }

    #[test]
    fn migration_v6_history_is_recorded() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("runtime").join("egopulse.db");
        let db = super::super::Database::new(&db_path).expect("db");

        let conn = db.conn.lock().expect("lock");
        let has_v6: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM schema_migrations WHERE version = 6",
                [],
                |row| row.get(0),
            )
            .expect("check v6 record");
        assert!(has_v6);
    }

    #[test]
    fn migration_v7_adds_columns() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("runtime").join("egopulse.db");
        let db = super::super::Database::new(&db_path).expect("db");

        let conn = db.conn.lock().expect("lock");

        let has_message_kind: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM pragma_table_info('messages') WHERE name = 'message_kind'",
                [],
                |row| row.get(0),
            )
            .expect("check message_kind");
        assert!(has_message_kind);

        let has_sender_agent_id: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM pragma_table_info('messages') WHERE name = 'sender_agent_id'",
                [],
                |row| row.get(0),
            )
            .expect("check sender_agent_id");
        assert!(has_sender_agent_id);

        let has_recipient_agent_id: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM pragma_table_info('messages') WHERE name = 'recipient_agent_id'",
                [],
                |row| row.get(0),
            )
            .expect("check recipient_agent_id");
        assert!(has_recipient_agent_id);
    }

    #[test]
    fn migration_v7_default_values() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("runtime").join("egopulse.db");
        std::fs::create_dir_all(db_path.parent().expect("parent")).expect("create dir");

        // Create a v6 DB with an existing message
        {
            let conn = Connection::open(&db_path).expect("open");
            conn.execute_batch("PRAGMA journal_mode=WAL;").expect("wal");
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS db_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
                 CREATE TABLE IF NOT EXISTS schema_migrations (version INTEGER PRIMARY KEY, applied_at TEXT NOT NULL, note TEXT);
                 INSERT OR REPLACE INTO db_meta (key, value) VALUES ('schema_version', '6');
                 INSERT OR REPLACE INTO schema_migrations (version, applied_at, note) VALUES (6, '2025-01-01T00:00:00Z', 'test v6');
                 CREATE TABLE IF NOT EXISTS chats (
                     chat_id INTEGER PRIMARY KEY,
                     chat_title TEXT,
                     chat_type TEXT NOT NULL DEFAULT 'private',
                     last_message_time TEXT NOT NULL,
                     channel TEXT,
                     external_chat_id TEXT,
                     agent_id TEXT NOT NULL DEFAULT 'lyre'
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
                     PRIMARY KEY (id, chat_id, message_id)
                 );
                 CREATE TABLE IF NOT EXISTS llm_usage_logs (
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
                 CREATE TABLE IF NOT EXISTS sleep_runs (
                     id TEXT PRIMARY KEY,
                     agent_id TEXT NOT NULL,
                     status TEXT NOT NULL,
                     trigger_type TEXT NOT NULL,
                     started_at TEXT NOT NULL,
                     finished_at TEXT,
                     source_chats_json TEXT NOT NULL DEFAULT '[]',
                     source_digest_md TEXT,
                     input_tokens INTEGER NOT NULL DEFAULT 0,
                     output_tokens INTEGER NOT NULL DEFAULT 0,
                     total_tokens INTEGER NOT NULL DEFAULT 0,
                     error_message TEXT
                 );
                 CREATE TABLE IF NOT EXISTS memory_snapshots (
                     id TEXT PRIMARY KEY,
                     run_id TEXT NOT NULL,
                     agent_id TEXT NOT NULL,
                     file TEXT NOT NULL,
                     content_before TEXT NOT NULL,
                     content_after TEXT NOT NULL,
                     created_at TEXT NOT NULL
                 );
                 INSERT INTO messages (id, chat_id, sender_name, content, is_from_bot, timestamp)
                 VALUES ('msg-1', 1, 'alice', 'hello', 0, '2024-01-01T00:00:00Z');",
            )
            .expect("create v6 schema");
        }

        let db = super::super::Database::new(&db_path).expect("reopen");
        let conn = db.conn.lock().expect("lock");

        let message_kind: String = conn
            .query_row(
                "SELECT message_kind FROM messages WHERE id = 'msg-1'",
                [],
                |row| row.get(0),
            )
            .expect("query message_kind");
        assert_eq!(message_kind, "message");

        let sender_agent_id: Option<String> = conn
            .query_row(
                "SELECT sender_agent_id FROM messages WHERE id = 'msg-1'",
                [],
                |row| row.get(0),
            )
            .expect("query sender_agent_id");
        assert!(sender_agent_id.is_none());

        let recipient_agent_id: Option<String> = conn
            .query_row(
                "SELECT recipient_agent_id FROM messages WHERE id = 'msg-1'",
                [],
                |row| row.get(0),
            )
            .expect("query recipient_agent_id");
        assert!(recipient_agent_id.is_none());
    }

    #[test]
    fn migration_v7_from_v6_db() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("runtime").join("egopulse.db");
        std::fs::create_dir_all(db_path.parent().expect("parent")).expect("create dir");

        // Create a minimal v6 DB
        {
            let conn = Connection::open(&db_path).expect("open");
            conn.execute_batch("PRAGMA journal_mode=WAL;").expect("wal");
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS db_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
                 CREATE TABLE IF NOT EXISTS schema_migrations (version INTEGER PRIMARY KEY, applied_at TEXT NOT NULL, note TEXT);
                 INSERT OR REPLACE INTO db_meta (key, value) VALUES ('schema_version', '6');
                 INSERT OR REPLACE INTO schema_migrations (version, applied_at, note) VALUES (6, '2025-01-01T00:00:00Z', 'test v6');
                 CREATE TABLE IF NOT EXISTS chats (
                     chat_id INTEGER PRIMARY KEY,
                     chat_title TEXT,
                     chat_type TEXT NOT NULL DEFAULT 'private',
                     last_message_time TEXT NOT NULL,
                     channel TEXT,
                     external_chat_id TEXT,
                     agent_id TEXT NOT NULL DEFAULT 'lyre'
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
                     PRIMARY KEY (id, chat_id, message_id)
                 );
                 CREATE TABLE IF NOT EXISTS llm_usage_logs (
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
                 CREATE TABLE IF NOT EXISTS sleep_runs (
                     id TEXT PRIMARY KEY,
                     agent_id TEXT NOT NULL,
                     status TEXT NOT NULL,
                     trigger_type TEXT NOT NULL,
                     started_at TEXT NOT NULL,
                     finished_at TEXT,
                     source_chats_json TEXT NOT NULL DEFAULT '[]',
                     source_digest_md TEXT,
                     input_tokens INTEGER NOT NULL DEFAULT 0,
                     output_tokens INTEGER NOT NULL DEFAULT 0,
                     total_tokens INTEGER NOT NULL DEFAULT 0,
                     error_message TEXT
                 );
                 CREATE TABLE IF NOT EXISTS memory_snapshots (
                     id TEXT PRIMARY KEY,
                     run_id TEXT NOT NULL,
                     agent_id TEXT NOT NULL,
                     file TEXT NOT NULL,
                     content_before TEXT NOT NULL,
                     content_after TEXT NOT NULL,
                     created_at TEXT NOT NULL
                 );",
            )
            .expect("create v6 schema");
        }

        let db = super::super::Database::new(&db_path).expect("migrate");
        let conn = db.conn.lock().expect("lock");

        let has_v7: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM schema_migrations WHERE version = 7",
                [],
                |row| row.get(0),
            )
            .expect("check v7 record");
        assert!(has_v7);

        let has_message_kind: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM pragma_table_info('messages') WHERE name = 'message_kind'",
                [],
                |row| row.get(0),
            )
            .expect("check column");
        assert!(has_message_kind);
    }

    fn create_v7_db(db_path: &std::path::Path) {
        std::fs::create_dir_all(db_path.parent().expect("parent")).expect("create dir");
        let conn = Connection::open(db_path).expect("open");
        conn.execute_batch("PRAGMA journal_mode=WAL;").expect("wal");
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS db_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS schema_migrations (version INTEGER PRIMARY KEY, applied_at TEXT NOT NULL, note TEXT);
             INSERT OR REPLACE INTO db_meta (key, value) VALUES ('schema_version', '7');
             INSERT OR REPLACE INTO schema_migrations (version, applied_at, note) VALUES (7, '2025-01-01T00:00:00Z', 'test v7');
             CREATE TABLE IF NOT EXISTS chats (
                 chat_id INTEGER PRIMARY KEY,
                 chat_title TEXT,
                 chat_type TEXT NOT NULL DEFAULT 'private',
                 last_message_time TEXT NOT NULL,
                 channel TEXT,
                 external_chat_id TEXT,
                 agent_id TEXT NOT NULL DEFAULT 'lyre'
             );
             CREATE TABLE IF NOT EXISTS messages (
                 id TEXT NOT NULL,
                 chat_id INTEGER NOT NULL,
                 sender_name TEXT NOT NULL,
                 content TEXT NOT NULL,
                 is_from_bot INTEGER NOT NULL DEFAULT 0,
                 timestamp TEXT NOT NULL,
                 message_kind TEXT NOT NULL DEFAULT 'message',
                 sender_agent_id TEXT,
                 recipient_agent_id TEXT,
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
                 PRIMARY KEY (id, chat_id, message_id)
             );
             CREATE TABLE IF NOT EXISTS llm_usage_logs (
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
             CREATE TABLE IF NOT EXISTS sleep_runs (
                 id TEXT PRIMARY KEY,
                 agent_id TEXT NOT NULL,
                 status TEXT NOT NULL,
                 trigger_type TEXT NOT NULL,
                 started_at TEXT NOT NULL,
                 finished_at TEXT,
                 source_chats_json TEXT NOT NULL DEFAULT '[]',
                 source_digest_md TEXT,
                 input_tokens INTEGER NOT NULL DEFAULT 0,
                 output_tokens INTEGER NOT NULL DEFAULT 0,
                 total_tokens INTEGER NOT NULL DEFAULT 0,
                 error_message TEXT
             );
             CREATE TABLE IF NOT EXISTS memory_snapshots (
                 id TEXT PRIMARY KEY,
                 run_id TEXT NOT NULL,
                 agent_id TEXT NOT NULL,
                 file TEXT NOT NULL,
                 content_before TEXT NOT NULL,
                 content_after TEXT NOT NULL,
                 created_at TEXT NOT NULL
             );",
        )
        .expect("create v7 schema");
    }

    #[test]
    fn migration_v8_removes_bot_id_from_external_chat_id() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("runtime").join("egopulse.db");
        create_v7_db(&db_path);

        // Insert old-format Discord rows
        {
            let conn = Connection::open(&db_path).expect("open");
            conn.execute(
                "INSERT INTO chats (chat_id, chat_title, chat_type, last_message_time, channel, external_chat_id, agent_id)
                 VALUES (100, 'test', 'discord', '2025-01-01T00:00:00Z', 'discord', 'discord:123:bot:main:agent:lyre', 'lyre')",
                [],
            )
            .expect("insert old discord");
            conn.execute(
                "INSERT INTO chats (chat_id, chat_title, chat_type, last_message_time, channel, external_chat_id, agent_id)
                 VALUES (101, 'test2', 'discord', '2025-01-01T00:00:00Z', 'discord', 'discord:456:bot:bot_a:agent:vega', 'vega')",
                [],
            )
            .expect("insert old discord 2");
        }

        let db = super::super::Database::new(&db_path).expect("migrate to v8");
        let conn = db.conn.lock().expect("lock");

        let id1: String = conn
            .query_row(
                "SELECT external_chat_id FROM chats WHERE chat_id = 100",
                [],
                |row| row.get(0),
            )
            .expect("query");
        assert_eq!(id1, "discord:123:agent:lyre");

        let id2: String = conn
            .query_row(
                "SELECT external_chat_id FROM chats WHERE chat_id = 101",
                [],
                |row| row.get(0),
            )
            .expect("query");
        assert_eq!(id2, "discord:456:agent:vega");
    }

    #[test]
    fn migration_v8_preserves_non_discord_chats() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("runtime").join("egopulse.db");
        create_v7_db(&db_path);

        {
            let conn = Connection::open(&db_path).expect("open");
            conn.execute(
                "INSERT INTO chats (chat_id, chat_title, chat_type, last_message_time, channel, external_chat_id, agent_id)
                 VALUES (200, 'web chat', 'web', '2025-01-01T00:00:00Z', 'web', 'web:message-1', 'default')",
                [],
            )
            .expect("insert web");
            conn.execute(
                "INSERT INTO chats (chat_id, chat_title, chat_type, last_message_time, channel, external_chat_id, agent_id)
                 VALUES (201, 'cli chat', 'cli', '2025-01-01T00:00:00Z', 'cli', 'cli:local-dev', 'default')",
                [],
            )
            .expect("insert cli");
        }

        let db = super::super::Database::new(&db_path).expect("migrate to v8");
        let conn = db.conn.lock().expect("lock");

        let web_id: String = conn
            .query_row(
                "SELECT external_chat_id FROM chats WHERE chat_id = 200",
                [],
                |row| row.get(0),
            )
            .expect("query");
        assert_eq!(web_id, "web:message-1");

        let cli_id: String = conn
            .query_row(
                "SELECT external_chat_id FROM chats WHERE chat_id = 201",
                [],
                |row| row.get(0),
            )
            .expect("query");
        assert_eq!(cli_id, "cli:local-dev");
    }

    #[test]
    fn migration_v8_handles_no_bot_id_format() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("runtime").join("egopulse.db");
        create_v7_db(&db_path);

        {
            let conn = Connection::open(&db_path).expect("open");
            conn.execute(
                "INSERT INTO chats (chat_id, chat_title, chat_type, last_message_time, channel, external_chat_id, agent_id)
                 VALUES (300, 'already new', 'discord', '2025-01-01T00:00:00Z', 'discord', 'discord:789:agent:lyre', 'lyre')",
                [],
            )
            .expect("insert new format");
        }

        let db = super::super::Database::new(&db_path).expect("migrate to v8");
        let conn = db.conn.lock().expect("lock");

        let id: String = conn
            .query_row(
                "SELECT external_chat_id FROM chats WHERE chat_id = 300",
                [],
                |row| row.get(0),
            )
            .expect("query");
        assert_eq!(id, "discord:789:agent:lyre");
    }

    #[test]
    fn migration_v8_history_is_recorded() {
        let db = test_db();
        let conn = db.conn.lock().expect("lock");
        let has_v8: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM schema_migrations WHERE version = 8",
                [],
                |row| row.get(0),
            )
            .expect("check v8 record");
        assert!(has_v8);
    }

    #[test]
    fn migration_history_count_includes_v8() {
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

        assert_eq!(rows.len(), 8);
        assert_eq!(rows[7].0, 8);
        assert!(rows[7].1.contains("bot_id"));
    }
}
