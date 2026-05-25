//! スキーマ定義・マイグレーション。

use rusqlite::{Connection, OptionalExtension, params};

use crate::error::StorageError;

/// 現在のスキーマバージョン。
///
/// スキーマを変更する際はこの値をインクリメントし、
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

/// Transaction 内でスキーマバージョンを更新する。
fn set_schema_version_in_tx(
    tx: &rusqlite::Transaction,
    version: i64,
    note: &str,
) -> Result<(), StorageError> {
    tx.execute(
        "INSERT INTO db_meta(key, value) VALUES('schema_version', ?1)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![version.to_string()],
    )?;
    tx.execute(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
            version INTEGER PRIMARY KEY,
            applied_at TEXT NOT NULL,
            note TEXT
        )",
        [],
    )?;
    tx.execute(
        "INSERT OR REPLACE INTO schema_migrations(version, applied_at, note)
         VALUES(?1, ?2, ?3)",
        params![version, chrono::Utc::now().to_rfc3339(), note],
    )?;
    Ok(())
}

/// 未適用のマイグレーションを逐次実行する。
pub(super) fn run_migrations(conn: &Connection) -> Result<(), StorageError> {
    let mut version = schema_version(conn)?;

    if version < SCHEMA_VERSION {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS chats (
                chat_id INTEGER PRIMARY KEY,
                chat_title TEXT,
                chat_type TEXT NOT NULL DEFAULT 'private',
                last_message_time TEXT NOT NULL,
                channel TEXT,
                external_chat_id TEXT,
                agent_id TEXT NOT NULL DEFAULT 'default'
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
                message_kind TEXT NOT NULL DEFAULT 'message',
                sender_agent_id TEXT,
                recipient_agent_id TEXT,
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

            CREATE INDEX IF NOT EXISTS idx_tool_calls_chat_id
                ON tool_calls(chat_id);

            CREATE INDEX IF NOT EXISTS idx_tool_calls_chat_message_id
                ON tool_calls(chat_id, message_id);

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

            CREATE INDEX IF NOT EXISTS idx_llm_usage_chat_created
                ON llm_usage_logs(chat_id, created_at);

            CREATE INDEX IF NOT EXISTS idx_llm_usage_created
                ON llm_usage_logs(created_at);

            CREATE TABLE IF NOT EXISTS sleep_runs (
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

            CREATE INDEX IF NOT EXISTS idx_sleep_runs_agent_started
                ON sleep_runs(agent_id, started_at);

            CREATE INDEX IF NOT EXISTS idx_sleep_runs_agent_status
                ON sleep_runs(agent_id, status);

            CREATE TABLE IF NOT EXISTS memory_snapshots (
                id              TEXT PRIMARY KEY,
                run_id          TEXT NOT NULL,
                agent_id        TEXT NOT NULL,
                file            TEXT NOT NULL,
                content_before  TEXT NOT NULL,
                content_after   TEXT NOT NULL,
                created_at      TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_memory_snapshots_run_id
                ON memory_snapshots(run_id);

            CREATE INDEX IF NOT EXISTS idx_memory_snapshots_agent_created
                ON memory_snapshots(agent_id, created_at);

            CREATE TABLE IF NOT EXISTS pulse_runs (
                id            TEXT PRIMARY KEY,
                agent_id      TEXT NOT NULL,
                intention_id  TEXT NOT NULL,
                due_key       TEXT NOT NULL,
                chat_id       INTEGER,
                message_id    TEXT,
                status        TEXT NOT NULL,
                started_at    TEXT NOT NULL,
                finished_at   TEXT,
                output_kind   TEXT,
                output_text   TEXT,
                error_message TEXT
            );

            CREATE UNIQUE INDEX IF NOT EXISTS idx_pulse_runs_due
                ON pulse_runs(agent_id, intention_id, due_key);

            CREATE INDEX IF NOT EXISTS idx_pulse_runs_agent_started
                ON pulse_runs(agent_id, started_at);

            CREATE INDEX IF NOT EXISTS idx_pulse_runs_chat_id
                ON pulse_runs(chat_id);",
        )?;
        set_schema_version(
            conn,
            1,
            "full schema: chats, messages, sessions, tool_calls, llm_usage_logs, sleep_runs, memory_snapshots, pulse_runs",
        )?;
        version = 1;
    }

    if version < 2 {
        conn.execute_batch(
            "UPDATE chats
             SET external_chat_id = external_chat_id || ':agent:default'
             WHERE external_chat_id NOT LIKE '%:agent:%'
               AND channel NOT IN ('discord')
               AND chat_type != 'channel_log'",
        )?;
        set_schema_version(
            conn,
            2,
            "append :agent:default to session keys without :agent: segment",
        )?;
        version = 2;
    }

    if version < 3 {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS episode_events (
                id               TEXT PRIMARY KEY,
                agent_id         TEXT NOT NULL,
                experienced_at   TEXT NOT NULL,
                encoded_at       TEXT NOT NULL,
                kind             TEXT NOT NULL,
                title            TEXT NOT NULL,
                body_md          TEXT NOT NULL,
                ripple_strength  INTEGER NOT NULL DEFAULT 3,
                certainty        TEXT NOT NULL DEFAULT 'stated',
                sleep_run_id     TEXT NOT NULL,
                source_refs_json TEXT,
                created_at       TEXT NOT NULL,
                updated_at       TEXT NOT NULL,
                CHECK (kind IN (
                    'self', 'relationship', 'world', 'feat',
                    'anomaly', 'decision', 'insight', 'rhythm'
                )),
                CHECK (ripple_strength BETWEEN 1 AND 5),
                CHECK (certainty IN ('stated', 'derived', 'tentative'))
            );

            CREATE INDEX IF NOT EXISTS idx_episode_events_agent_experienced
                ON episode_events(agent_id, experienced_at);

            CREATE INDEX IF NOT EXISTS idx_episode_events_agent_kind_experienced
                ON episode_events(agent_id, kind, experienced_at);

            CREATE INDEX IF NOT EXISTS idx_episode_events_agent_ripple_experienced
                ON episode_events(agent_id, ripple_strength, experienced_at);

            CREATE INDEX IF NOT EXISTS idx_episode_events_sleep_run
                ON episode_events(sleep_run_id);",
        )?;
        set_schema_version(conn, 3, "add episode_events table")?;
        version = 3;
    }

    if version < 4 {
        let tx = conn.unchecked_transaction()?;
        // NOTE: we must NOT issue UPDATEs on the old table before recreating it,
        // because the existing CHECK constraint only allows
        // ('observed','inferred','uncertain').  Mapping happens in the SELECT below.
        tx.execute_batch(
            "CREATE TABLE episode_events_v4 (
                 id               TEXT PRIMARY KEY,
                 agent_id         TEXT NOT NULL,
                 experienced_at   TEXT NOT NULL,
                 encoded_at       TEXT NOT NULL,
                 kind             TEXT NOT NULL,
                 title            TEXT NOT NULL,
                 body_md          TEXT NOT NULL,
                 ripple_strength  INTEGER NOT NULL DEFAULT 3,
                 certainty        TEXT NOT NULL DEFAULT 'stated',
                 sleep_run_id     TEXT NOT NULL,
                 source_refs_json TEXT,
                 created_at       TEXT NOT NULL,
                 updated_at       TEXT NOT NULL,
                 CHECK (kind IN (
                     'self', 'relationship', 'world', 'feat',
                     'anomaly', 'decision', 'insight', 'rhythm'
                 )),
                 CHECK (ripple_strength BETWEEN 1 AND 5),
                 CHECK (certainty IN ('stated', 'derived', 'tentative'))
             );

             INSERT INTO episode_events_v4
                 SELECT
                     id,
                     agent_id,
                     experienced_at,
                     encoded_at,
                     kind,
                     title,
                     body_md,
                     ripple_strength,
                     CASE certainty
                         WHEN 'observed'  THEN 'stated'
                         WHEN 'inferred'  THEN 'derived'
                         WHEN 'uncertain' THEN 'tentative'
                         ELSE certainty
                     END,
                     sleep_run_id,
                     source_refs_json,
                     created_at,
                     updated_at
                 FROM episode_events;

             DROP TABLE episode_events;
             ALTER TABLE episode_events_v4 RENAME TO episode_events;

             CREATE INDEX IF NOT EXISTS idx_episode_events_agent_experienced
                 ON episode_events(agent_id, experienced_at);
             CREATE INDEX IF NOT EXISTS idx_episode_events_agent_kind_experienced
                 ON episode_events(agent_id, kind, experienced_at);
             CREATE INDEX IF NOT EXISTS idx_episode_events_agent_ripple_experienced
                 ON episode_events(agent_id, ripple_strength, experienced_at);
             CREATE INDEX IF NOT EXISTS idx_episode_events_sleep_run
                 ON episode_events(sleep_run_id);",
        )?;
        set_schema_version_in_tx(
            &tx,
            4,
            "rename certainty values: observed→stated, inferred→derived, uncertain→tentative",
        )?;
        tx.commit()?;
        version = 4;
    }

    debug_assert_eq!(version, SCHEMA_VERSION, "all migrations applied");
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn fresh_db_applies_all_migrations() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("runtime").join("egopulse.db");
        let db = super::super::Database::new(&db_path).expect("all migrations succeed");

        let conn = db.conn.lock().expect("lock");

        let expected_tables = [
            "chats",
            "messages",
            "sessions",
            "tool_calls",
            "llm_usage_logs",
            "sleep_runs",
            "memory_snapshots",
            "pulse_runs",
            "episode_events",
        ];
        for name in &expected_tables {
            let exists: bool = conn
                .query_row(
                    "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name = ?1",
                    [name],
                    |row| row.get(0),
                )
                .expect("check table");
            assert!(exists, "expected table {name}");
        }

        let version: String = conn
            .query_row(
                "SELECT value FROM db_meta WHERE key = 'schema_version'",
                [],
                |row| row.get(0),
            )
            .expect("schema version");
        assert_eq!(version.parse::<i64>().unwrap(), super::SCHEMA_VERSION);
    }

    #[test]
    fn migration_v2_appends_agent_default_to_session_keys() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("runtime").join("egopulse.db");
        let db = super::super::Database::new(&db_path).expect("migrations");
        let conn = db.conn.lock().expect("lock");

        // Insert old-format session keys
        conn.execute(
            "INSERT INTO chats (chat_title, chat_type, last_message_time, channel, external_chat_id, agent_id)
             VALUES ('tg-chat', 'group', '2025-01-01T00:00:00Z', 'telegram', 'telegram:-100123', 'default')",
            [],
        ).expect("insert telegram");
        conn.execute(
            "INSERT INTO chats (chat_title, chat_type, last_message_time, channel, external_chat_id, agent_id)
             VALUES ('cli-chat', 'private', '2025-01-01T00:00:00Z', 'cli', 'cli:mysession', 'default')",
            [],
        ).expect("insert cli");
        conn.execute(
            "INSERT INTO chats (chat_title, chat_type, last_message_time, channel, external_chat_id, agent_id)
             VALUES ('tui-chat', 'private', '2025-01-01T00:00:00Z', 'tui', 'tui:local-abc', 'default')",
            [],
        ).expect("insert tui");
        conn.execute(
            "INSERT INTO chats (chat_title, chat_type, last_message_time, channel, external_chat_id, agent_id)
             VALUES ('web-chat', 'private', '2025-01-01T00:00:00Z', 'web', 'web:s1', 'default')",
            [],
        ).expect("insert web");
        // Discord already has :agent: — should be untouched
        conn.execute(
            "INSERT INTO chats (chat_title, chat_type, last_message_time, channel, external_chat_id, agent_id)
             VALUES ('dc-chat', 'guild', '2025-01-01T00:00:00Z', 'discord', 'discord:123:agent:alice', 'alice')",
            [],
        ).expect("insert discord");

        // Roll back schema version to 1 so v2 migration runs
        conn.execute(
            "UPDATE db_meta SET value = '1' WHERE key = 'schema_version'",
            [],
        )
        .expect("rollback version");

        drop(conn);

        // Re-run migrations (v2 will apply)
        {
            let conn = db.conn.lock().expect("lock");
            super::run_migrations(&conn).expect("re-run migrations");
        }

        let conn = db.conn.lock().expect("lock");

        // Telegram renamed
        let key = conn
            .query_row(
                "SELECT external_chat_id FROM chats WHERE channel = 'telegram'",
                [],
                |row| row.get::<_, String>(0),
            )
            .expect("telegram key");
        assert_eq!(key, "telegram:-100123:agent:default");

        // CLI renamed
        let key = conn
            .query_row(
                "SELECT external_chat_id FROM chats WHERE channel = 'cli'",
                [],
                |row| row.get::<_, String>(0),
            )
            .expect("cli key");
        assert_eq!(key, "cli:mysession:agent:default");

        // TUI renamed
        let key = conn
            .query_row(
                "SELECT external_chat_id FROM chats WHERE channel = 'tui'",
                [],
                |row| row.get::<_, String>(0),
            )
            .expect("tui key");
        assert_eq!(key, "tui:local-abc:agent:default");

        // Web renamed
        let key = conn
            .query_row(
                "SELECT external_chat_id FROM chats WHERE channel = 'web'",
                [],
                |row| row.get::<_, String>(0),
            )
            .expect("web key");
        assert_eq!(key, "web:s1:agent:default");

        // Discord untouched
        let key = conn
            .query_row(
                "SELECT external_chat_id FROM chats WHERE channel = 'discord'",
                [],
                |row| row.get::<_, String>(0),
            )
            .expect("discord key");
        assert_eq!(key, "discord:123:agent:alice");
    }

    #[test]
    fn migration_v2_is_idempotent() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("runtime").join("egopulse.db");
        let db = super::super::Database::new(&db_path).expect("migrations");

        // Run migration twice
        {
            let conn = db.conn.lock().expect("lock");
            super::run_migrations(&conn).expect("first run");
            super::run_migrations(&conn).expect("second run");
        }

        let conn = db.conn.lock().expect("lock");
        let version: String = conn
            .query_row(
                "SELECT value FROM db_meta WHERE key = 'schema_version'",
                [],
                |row| row.get(0),
            )
            .expect("version");
        assert_eq!(version.parse::<i64>().unwrap(), super::SCHEMA_VERSION);
    }

    // --- v3: episode_events ---------------------------------------------------

    #[test]
    fn fresh_db_includes_episode_events() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("runtime").join("egopulse.db");
        let _db = super::super::Database::new(&db_path).expect("migrations");

        let conn = _db.conn.lock().expect("lock");
        let exists: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name='episode_events'",
                [],
                |row| row.get(0),
            )
            .expect("check table");
        assert!(exists, "episode_events table should exist on fresh DB");
    }

    #[test]
    fn migration_from_v2_to_v3_adds_episode_events() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("runtime").join("egopulse.db");
        let db = super::super::Database::new(&db_path).expect("migrations");
        let conn = db.conn.lock().expect("lock");

        conn.execute(
            "UPDATE db_meta SET value = '2' WHERE key = 'schema_version'",
            [],
        )
        .expect("rollback version");
        drop(conn);

        {
            let conn = db.conn.lock().expect("lock");
            super::run_migrations(&conn).expect("re-run migrations");
        }

        let conn = db.conn.lock().expect("lock");
        let exists: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name='episode_events'",
                [],
                |row| row.get(0),
            )
            .expect("check table");
        assert!(
            exists,
            "episode_events should be created by v2→v3 migration"
        );
    }

    #[test]
    fn episode_events_all_columns_exist() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("runtime").join("egopulse.db");
        let _db = super::super::Database::new(&db_path).expect("migrations");
        let conn = _db.conn.lock().expect("lock");

        let expected_columns = [
            "id",
            "agent_id",
            "experienced_at",
            "encoded_at",
            "kind",
            "title",
            "body_md",
            "ripple_strength",
            "certainty",
            "sleep_run_id",
            "source_refs_json",
            "created_at",
            "updated_at",
        ];

        let mut stmt = conn
            .prepare("PRAGMA table_info(episode_events)")
            .expect("prepare pragma");
        let columns: Vec<String> = stmt
            .query_map([], |row| row.get::<_, String>(1))
            .expect("query")
            .map(|r| r.expect("col"))
            .collect();

        for name in &expected_columns {
            assert!(columns.iter().any(|c| c == *name), "missing column: {name}");
        }
    }

    #[test]
    fn episode_events_indexes_exist() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("runtime").join("egopulse.db");
        let _db = super::super::Database::new(&db_path).expect("migrations");
        let conn = _db.conn.lock().expect("lock");

        let expected_indexes = [
            "idx_episode_events_agent_experienced",
            "idx_episode_events_agent_kind_experienced",
            "idx_episode_events_agent_ripple_experienced",
            "idx_episode_events_sleep_run",
        ];

        let mut stmt = conn
            .prepare(
                "SELECT name FROM sqlite_master WHERE type='index' AND name LIKE 'idx_episode_events%'",
            )
            .expect("prepare");
        let indexes: Vec<String> = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .expect("query")
            .map(|r| r.expect("idx"))
            .collect();

        for name in &expected_indexes {
            assert!(indexes.iter().any(|i| i == *name), "missing index: {name}");
        }
    }
}
