//! スキーマ定義・マイグレーション。

use rusqlite::{Connection, OptionalExtension, params};

use crate::error::StorageError;

/// 現在のスキーマバージョン。
///
/// スキーマを変更する際はこの値をインクリメントし、
/// `run_migrations` に対応する `if version < N` ブロックを追加する。
pub(super) const SCHEMA_VERSION: i64 = 7;

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
                ON pulse_runs(chat_id);

            CREATE TABLE IF NOT EXISTS sleep_run_steps (
                sleep_run_id    TEXT NOT NULL,
                step_name       TEXT NOT NULL CHECK (step_name IN ('event_extraction', 'episodic_update', 'semantic_update', 'prospective_update')),
                status          TEXT NOT NULL CHECK (status IN ('pending', 'running', 'success', 'failed', 'skipped')),
                started_at      TEXT,
                finished_at     TEXT,
                input_tokens    INTEGER NOT NULL DEFAULT 0,
                output_tokens   INTEGER NOT NULL DEFAULT 0,
                error_message   TEXT,
                metadata_json   TEXT,
                PRIMARY KEY (sleep_run_id, step_name),
                FOREIGN KEY (sleep_run_id) REFERENCES sleep_runs(id) ON DELETE CASCADE
            );

            CREATE INDEX IF NOT EXISTS idx_sleep_run_steps_step_status
                ON sleep_run_steps(step_name, status, started_at);",
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

    if version < 5 {
        let tx = conn.unchecked_transaction()?;

        let needs_migration: bool = tx
            .query_row(
                "SELECT COUNT(*) > 0 FROM pragma_table_info('messages') WHERE name = 'is_from_bot'",
                [],
                |row| row.get(0),
            )
            .unwrap_or(false);

        if needs_migration {
            tx.execute_batch(
                "CREATE TABLE messages_v5 (
                    id TEXT NOT NULL,
                    chat_id INTEGER NOT NULL,
                    sender_id TEXT NOT NULL,
                    content TEXT NOT NULL,
                    sender_kind TEXT NOT NULL,
                    timestamp TEXT NOT NULL,
                    message_kind TEXT NOT NULL DEFAULT 'message',
                    recipient_agent_id TEXT,
                    PRIMARY KEY (id, chat_id)
                );",
            )?;

            {
                struct V4Row {
                    id: String,
                    chat_id: i64,
                    sender_name: String,
                    content: String,
                    is_from_bot: i32,
                    timestamp: String,
                    message_kind: String,
                    sender_agent_id: Option<String>,
                    recipient_agent_id: Option<String>,
                }

                let mut stmt = tx.prepare(
                    "SELECT id, chat_id, sender_name, content, is_from_bot,
                            timestamp, message_kind, sender_agent_id, recipient_agent_id
                     FROM messages",
                )?;
                let rows: Vec<V4Row> = stmt
                    .query_map([], |row| {
                        Ok(V4Row {
                            id: row.get::<_, String>(0)?,
                            chat_id: row.get::<_, i64>(1)?,
                            sender_name: row.get::<_, String>(2)?,
                            content: row.get::<_, String>(3)?,
                            is_from_bot: row.get::<_, i32>(4)?,
                            timestamp: row.get::<_, String>(5)?,
                            message_kind: row.get::<_, String>(6)?,
                            sender_agent_id: row.get::<_, Option<String>>(7)?,
                            recipient_agent_id: row.get::<_, Option<String>>(8)?,
                        })
                    })?
                    .collect::<Result<Vec<_>, _>>()?;

                for row in &rows {
                    let (sender_id, sender_kind) =
                        if row.is_from_bot != 0 && row.message_kind == "system_event" {
                            ("system".to_string(), "system")
                        } else if row.is_from_bot != 0 && row.message_kind == "agent_send" {
                            (
                                row.sender_agent_id
                                    .clone()
                                    .unwrap_or_else(|| row.sender_name.clone()),
                                "tool",
                            )
                        } else if row.is_from_bot != 0 && row.sender_agent_id.is_some() {
                            (row.sender_agent_id.clone().unwrap(), "assistant")
                        } else if row.is_from_bot != 0 {
                            (row.sender_name.clone(), "assistant")
                        } else {
                            (row.sender_name.clone(), "user")
                        };

                    tx.execute(
                        "INSERT INTO messages_v5
                            (id, chat_id, sender_id, content, sender_kind,
                             timestamp, message_kind, recipient_agent_id)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                        params![
                            row.id,
                            row.chat_id,
                            sender_id,
                            row.content,
                            sender_kind,
                            row.timestamp,
                            row.message_kind,
                            row.recipient_agent_id,
                        ],
                    )?;
                }
            }

            tx.execute_batch("DROP TABLE messages;")?;
            tx.execute_batch("ALTER TABLE messages_v5 RENAME TO messages;")?;
            tx.execute_batch(
                "CREATE INDEX IF NOT EXISTS idx_messages_chat_timestamp
                    ON messages(chat_id, timestamp);",
            )?;
        }

        set_schema_version_in_tx(
            &tx,
            5,
            "replace sender_name/is_from_bot/sender_agent_id with sender_id/sender_kind",
        )?;
        tx.commit()?;
        version = 5;
    }

    if version < 6 {
        let tx = conn.unchecked_transaction()?;
        tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS episode_rollups (
                id                   TEXT PRIMARY KEY,
                agent_id             TEXT NOT NULL,
                granularity          TEXT NOT NULL,
                period_key           TEXT NOT NULL,
                period_start         TEXT NOT NULL,
                period_end_exclusive TEXT NOT NULL,
                summary_md           TEXT NOT NULL,
                max_ripple           INTEGER NOT NULL DEFAULT 3,
                event_count          INTEGER NOT NULL DEFAULT 0,
                generated_run_id     TEXT NOT NULL,
                created_at           TEXT NOT NULL,
                updated_at           TEXT NOT NULL,
                CHECK (granularity IN ('week', 'month')),
                CHECK (max_ripple BETWEEN 1 AND 5),
                UNIQUE(agent_id, granularity, period_key)
            );

            CREATE INDEX IF NOT EXISTS idx_episode_rollups_agent_period
                ON episode_rollups(agent_id, granularity, period_start);

            CREATE INDEX IF NOT EXISTS idx_episode_rollups_agent_ripple
                ON episode_rollups(agent_id, granularity, max_ripple, period_start);",
        )?;
        set_schema_version_in_tx(&tx, 6, "add episode_rollups table")?;
        tx.commit()?;
        version = 6;
    }

    if version < 7 {
        let tx = conn.unchecked_transaction()?;
        tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS sleep_run_steps (
                sleep_run_id    TEXT NOT NULL,
                step_name       TEXT NOT NULL CHECK (step_name IN ('event_extraction', 'episodic_update', 'semantic_update', 'prospective_update')),
                status          TEXT NOT NULL CHECK (status IN ('pending', 'running', 'success', 'failed', 'skipped')),
                started_at      TEXT,
                finished_at     TEXT,
                input_tokens    INTEGER NOT NULL DEFAULT 0,
                output_tokens   INTEGER NOT NULL DEFAULT 0,
                error_message   TEXT,
                metadata_json   TEXT,
                PRIMARY KEY (sleep_run_id, step_name),
                FOREIGN KEY (sleep_run_id) REFERENCES sleep_runs(id) ON DELETE CASCADE
            );

            CREATE INDEX IF NOT EXISTS idx_sleep_run_steps_step_status
                ON sleep_run_steps(step_name, status, started_at);",
        )?;
        set_schema_version_in_tx(
            &tx,
            7,
            "add sleep_run_steps table for per-step execution log",
        )?;
        tx.commit()?;
        version = 7;
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

        let conn = db.get_conn().expect("pool");

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
            "episode_rollups",
            "sleep_run_steps",
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
        let conn = db.get_conn().expect("pool");

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
            let conn = db.get_conn().expect("pool");
            super::run_migrations(&conn).expect("re-run migrations");
        }

        let conn = db.get_conn().expect("pool");

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
            let conn = db.get_conn().expect("pool");
            super::run_migrations(&conn).expect("first run");
            super::run_migrations(&conn).expect("second run");
        }

        let conn = db.get_conn().expect("pool");
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

        let conn = _db.get_conn().expect("pool");
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
        let conn = db.get_conn().expect("pool");

        conn.execute(
            "UPDATE db_meta SET value = '2' WHERE key = 'schema_version'",
            [],
        )
        .expect("rollback version");
        drop(conn);

        {
            let conn = db.get_conn().expect("pool");
            super::run_migrations(&conn).expect("re-run migrations");
        }

        let conn = db.get_conn().expect("pool");
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
        let conn = _db.get_conn().expect("pool");

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
        let conn = _db.get_conn().expect("pool");

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

    // --- v5: messages sender_id / sender_kind ---------------------------------

    fn seed_v4_messages(conn: &rusqlite::Connection) {
        // Bot message (no agent id)
        conn.execute(
            "INSERT INTO messages (id, chat_id, sender_name, content, is_from_bot, timestamp, message_kind, sender_agent_id, recipient_agent_id)
             VALUES ('m1', 1, 'egopulse', 'bot hello', 1, '2024-01-01T00:00:00Z', 'message', NULL, NULL)",
            [],
        ).unwrap();
        // Agent message (has sender_agent_id)
        conn.execute(
            "INSERT INTO messages (id, chat_id, sender_name, content, is_from_bot, timestamp, message_kind, sender_agent_id, recipient_agent_id)
             VALUES ('m2', 1, 'lyre', 'agent reply', 1, '2024-01-01T00:00:01Z', 'message', 'lyre', NULL)",
            [],
        ).unwrap();
        // User message
        conn.execute(
            "INSERT INTO messages (id, chat_id, sender_name, content, is_from_bot, timestamp, message_kind, sender_agent_id, recipient_agent_id)
             VALUES ('m3', 1, 'alice', 'user hello', 0, '2024-01-01T00:00:02Z', 'message', NULL, NULL)",
            [],
        ).unwrap();
        // System event
        conn.execute(
            "INSERT INTO messages (id, chat_id, sender_name, content, is_from_bot, timestamp, message_kind, sender_agent_id, recipient_agent_id)
             VALUES ('m4', 1, 'system', '{\"reason\":\"TurnCountExceeded\"}', 1, '2024-01-01T00:00:03Z', 'system_event', NULL, NULL)",
            [],
        ).unwrap();
        // Agent message with recipient
        conn.execute(
            "INSERT INTO messages (id, chat_id, sender_name, content, is_from_bot, timestamp, message_kind, sender_agent_id, recipient_agent_id)
             VALUES ('m5', 1, 'lyre', 'agent send', 1, '2024-01-01T00:00:04Z', 'agent_send', 'lyre', 'bob')",
            [],
        ).unwrap();
    }

    fn run_v5_migration(db: &super::super::Database) {
        {
            let conn = db.get_conn().expect("pool");
            super::run_migrations(&conn).expect("re-run migrations");
        }
    }

    /// Creates a Database with v4 schema (old messages columns) for testing v5 migration.
    fn create_v4_db(dir: &tempfile::TempDir) -> super::super::Database {
        let db_path = dir.path().join("runtime").join("egopulse.db");
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }

        {
            let conn = rusqlite::Connection::open(&db_path).unwrap();
            conn.execute_batch("PRAGMA journal_mode=WAL;").unwrap();
            conn.busy_timeout(std::time::Duration::from_secs(5))
                .unwrap();

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
                    ON messages(chat_id, timestamp);",
            )
            .unwrap();

            conn.execute(
                "CREATE TABLE IF NOT EXISTS db_meta (
                    key TEXT PRIMARY KEY,
                    value TEXT NOT NULL
                )",
                [],
            )
            .unwrap();

            super::set_schema_version(&conn, 4, "test v4 baseline").unwrap();
        }

        super::super::Database::new_unchecked(&db_path).unwrap()
    }

    #[test]
    fn migration_v4_to_v5_adds_sender_id() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = create_v4_db(&dir);

        run_v5_migration(&db);

        let conn = db.get_conn().expect("pool");
        let has_sender_id: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM pragma_table_info('messages') WHERE name = 'sender_id'",
                [],
                |row| row.get(0),
            )
            .expect("check");
        assert!(has_sender_id, "messages should have sender_id column");
    }

    #[test]
    fn migration_v4_to_v5_removes_is_from_bot() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = create_v4_db(&dir);

        {
            let conn = db.get_conn().expect("pool");
            seed_v4_messages(&conn);
        }

        run_v5_migration(&db);

        let conn = db.get_conn().expect("pool");
        let has_is_from_bot: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM pragma_table_info('messages') WHERE name = 'is_from_bot'",
                [],
                |row| row.get(0),
            )
            .expect("check");
        assert!(
            !has_is_from_bot,
            "messages should not have is_from_bot column after migration"
        );
    }

    #[test]
    fn migration_v4_to_v5_converts_bot_message() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = create_v4_db(&dir);

        {
            let conn = db.get_conn().expect("pool");
            seed_v4_messages(&conn);
        }

        run_v5_migration(&db);

        let conn = db.get_conn().expect("pool");
        let (sender_id, sender_kind): (String, String) = conn
            .query_row(
                "SELECT sender_id, sender_kind FROM messages WHERE id = 'm1'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("row");
        assert_eq!(sender_id, "egopulse");
        assert_eq!(sender_kind, "assistant");
    }

    #[test]
    fn migration_v4_to_v5_converts_agent_message() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = create_v4_db(&dir);

        {
            let conn = db.get_conn().expect("pool");
            seed_v4_messages(&conn);
        }

        run_v5_migration(&db);

        let conn = db.get_conn().expect("pool");
        let (sender_id, sender_kind): (String, String) = conn
            .query_row(
                "SELECT sender_id, sender_kind FROM messages WHERE id = 'm2'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("row");
        assert_eq!(sender_id, "lyre");
        assert_eq!(sender_kind, "assistant");
    }

    #[test]
    fn migration_v4_to_v5_converts_user_message() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = create_v4_db(&dir);

        {
            let conn = db.get_conn().expect("pool");
            seed_v4_messages(&conn);
        }

        run_v5_migration(&db);

        let conn = db.get_conn().expect("pool");
        let (sender_id, sender_kind): (String, String) = conn
            .query_row(
                "SELECT sender_id, sender_kind FROM messages WHERE id = 'm3'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("row");
        assert_eq!(sender_id, "alice");
        assert_eq!(sender_kind, "user");
    }

    #[test]
    fn migration_v4_to_v5_converts_system_event() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = create_v4_db(&dir);

        {
            let conn = db.get_conn().expect("pool");
            seed_v4_messages(&conn);
        }

        run_v5_migration(&db);

        let conn = db.get_conn().expect("pool");
        let (sender_id, sender_kind): (String, String) = conn
            .query_row(
                "SELECT sender_id, sender_kind FROM messages WHERE id = 'm4'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("row");
        assert_eq!(sender_id, "system");
        assert_eq!(sender_kind, "system");
    }

    #[test]
    fn migration_v4_to_v5_preserves_recipient_agent_id() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = create_v4_db(&dir);

        {
            let conn = db.get_conn().expect("pool");
            seed_v4_messages(&conn);
        }

        run_v5_migration(&db);

        let conn = db.get_conn().expect("pool");
        let recipient: Option<String> = conn
            .query_row(
                "SELECT recipient_agent_id FROM messages WHERE id = 'm5'",
                [],
                |row| row.get(0),
            )
            .expect("row");
        assert_eq!(recipient.as_deref(), Some("bob"));
    }

    #[test]
    fn migration_v4_to_v5_preserves_data_count() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = create_v4_db(&dir);

        {
            let conn = db.get_conn().expect("pool");
            seed_v4_messages(&conn);
        }

        run_v5_migration(&db);

        let conn = db.get_conn().expect("pool");
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM messages", [], |row| row.get(0))
            .expect("count");
        assert_eq!(count, 5);
    }

    #[test]
    fn migration_v4_to_v5_converts_agent_send_to_tool() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = create_v4_db(&dir);

        {
            let conn = db.get_conn().expect("pool");
            seed_v4_messages(&conn);
        }

        run_v5_migration(&db);

        let conn = db.get_conn().expect("pool");
        let (sender_id, sender_kind): (String, String) = conn
            .query_row(
                "SELECT sender_id, sender_kind FROM messages WHERE id = 'm5'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("row");
        assert_eq!(sender_id, "lyre");
        assert_eq!(sender_kind, "tool");
    }

    // --- v7: sleep_run_steps ---------------------------------------------------

    /// Creates a Database with v6 schema (all tables through episode_rollups) for testing v7 migration.
    fn create_v6_db(dir: &tempfile::TempDir) -> super::super::Database {
        let db_path = dir.path().join("runtime").join("egopulse.db");
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }

        {
            let conn = rusqlite::Connection::open(&db_path).unwrap();
            conn.execute_batch("PRAGMA journal_mode=WAL;").unwrap();
            conn.busy_timeout(std::time::Duration::from_secs(5))
                .unwrap();

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
                    sender_id TEXT NOT NULL,
                    content TEXT NOT NULL,
                    sender_kind TEXT NOT NULL,
                    timestamp TEXT NOT NULL,
                    message_kind TEXT NOT NULL DEFAULT 'message',
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
                    ON pulse_runs(chat_id);

                CREATE TABLE IF NOT EXISTS episode_events (
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
                    ON episode_events(sleep_run_id);

                CREATE TABLE IF NOT EXISTS episode_rollups (
                    id                   TEXT PRIMARY KEY,
                    agent_id             TEXT NOT NULL,
                    granularity          TEXT NOT NULL,
                    period_key           TEXT NOT NULL,
                    period_start         TEXT NOT NULL,
                    period_end_exclusive TEXT NOT NULL,
                    summary_md           TEXT NOT NULL,
                    max_ripple           INTEGER NOT NULL DEFAULT 3,
                    event_count          INTEGER NOT NULL DEFAULT 0,
                    generated_run_id     TEXT NOT NULL,
                    created_at           TEXT NOT NULL,
                    updated_at           TEXT NOT NULL,
                    CHECK (granularity IN ('week', 'month')),
                    CHECK (max_ripple BETWEEN 1 AND 5),
                    UNIQUE(agent_id, granularity, period_key)
                );

                CREATE INDEX IF NOT EXISTS idx_episode_rollups_agent_period
                    ON episode_rollups(agent_id, granularity, period_start);

                CREATE INDEX IF NOT EXISTS idx_episode_rollups_agent_ripple
                    ON episode_rollups(agent_id, granularity, max_ripple, period_start);",
            )
            .unwrap();

            conn.execute(
                "CREATE TABLE IF NOT EXISTS db_meta (
                    key TEXT PRIMARY KEY,
                    value TEXT NOT NULL
                )",
                [],
            )
            .unwrap();

            super::set_schema_version(&conn, 6, "test v6 baseline").unwrap();
        }

        super::super::Database::new_unchecked(&db_path).unwrap()
    }

    fn run_v7_migration(db: &super::super::Database) {
        let conn = db.get_conn().expect("pool");
        super::run_migrations(&conn).expect("re-run migrations");
    }

    #[test]
    fn migration_v6_to_v7_creates_sleep_run_steps() {
        // Arrange: v6 DB with an existing sleep run
        let dir = tempfile::tempdir().expect("tempdir");
        let db = create_v6_db(&dir);
        {
            let conn = db.get_conn().expect("pool");
            conn.execute(
                "INSERT INTO sleep_runs (id, agent_id, status, trigger_type, started_at)
                 VALUES ('existing-run-1', 'agent-a', 'success', 'manual', '2024-01-01T00:00:00Z')",
                [],
            )
            .expect("insert existing run");
        }

        // Act: run v7 migration
        run_v7_migration(&db);

        // Assert: schema version advanced
        let conn = db.get_conn().expect("pool");
        let version: String = conn
            .query_row(
                "SELECT value FROM db_meta WHERE key = 'schema_version'",
                [],
                |row| row.get(0),
            )
            .expect("schema version");
        assert_eq!(version.parse::<i64>().unwrap(), 7);

        // Assert: sleep_run_steps table exists
        let exists: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name='sleep_run_steps'",
                [],
                |row| row.get(0),
            )
            .expect("check table");
        assert!(
            exists,
            "sleep_run_steps table should exist after v7 migration"
        );

        // Assert: all expected columns exist
        let mut stmt = conn
            .prepare("PRAGMA table_info(sleep_run_steps)")
            .expect("prepare pragma");
        let columns: Vec<String> = stmt
            .query_map([], |row| row.get::<_, String>(1))
            .expect("query")
            .map(|r| r.expect("col"))
            .collect();
        for expected in &[
            "sleep_run_id",
            "step_name",
            "status",
            "started_at",
            "finished_at",
            "input_tokens",
            "output_tokens",
            "error_message",
            "metadata_json",
        ] {
            assert!(
                columns.iter().any(|c| c == *expected),
                "missing column: {expected}"
            );
        }

        // Assert: CHECK constraint rejects invalid step_name
        let invalid_step = conn.execute(
            "INSERT INTO sleep_run_steps (sleep_run_id, step_name, status)
             VALUES ('existing-run-1', 'invalid_step', 'pending')",
            [],
        );
        assert!(invalid_step.is_err(), "should reject invalid step_name");

        // Assert: CHECK constraint rejects invalid status
        let invalid_status = conn.execute(
            "INSERT INTO sleep_run_steps (sleep_run_id, step_name, status)
             VALUES ('existing-run-1', 'event_extraction', 'invalid_status')",
            [],
        );
        assert!(invalid_status.is_err(), "should reject invalid status");

        // Assert: valid step rows can be inserted
        conn.execute(
            "INSERT INTO sleep_run_steps (sleep_run_id, step_name, status)
             VALUES ('existing-run-1', 'event_extraction', 'pending')",
            [],
        )
        .expect("insert valid step");

        // Assert: composite PK prevents duplicate (sleep_run_id, step_name)
        let duplicate = conn.execute(
            "INSERT INTO sleep_run_steps (sleep_run_id, step_name, status)
             VALUES ('existing-run-1', 'event_extraction', 'running')",
            [],
        );
        assert!(duplicate.is_err(), "should reject duplicate composite PK");

        // Assert: FK cascade — deleting sleep_run deletes its steps
        conn.execute("DELETE FROM sleep_runs WHERE id = 'existing-run-1'", [])
            .expect("delete run");
        let step_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sleep_run_steps WHERE sleep_run_id = 'existing-run-1'",
                [],
                |row| row.get(0),
            )
            .expect("count steps");
        assert_eq!(step_count, 0, "FK cascade should delete child steps");

        // Assert: search index exists
        let index_exists: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='index' AND name='idx_sleep_run_steps_step_status'",
                [],
                |row| row.get(0),
            )
            .expect("check index");
        assert!(
            index_exists,
            "idx_sleep_run_steps_step_status index should exist"
        );
    }

    #[test]
    fn fresh_database_contains_sleep_run_steps_schema() {
        // Arrange & Act: fresh DB applies all migrations including v7
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("runtime").join("egopulse.db");
        let db = super::super::Database::new(&db_path).expect("all migrations succeed");

        // Assert: schema version is current
        let conn = db.get_conn().expect("pool");
        let version: String = conn
            .query_row(
                "SELECT value FROM db_meta WHERE key = 'schema_version'",
                [],
                |row| row.get(0),
            )
            .expect("schema version");
        assert_eq!(version.parse::<i64>().unwrap(), super::SCHEMA_VERSION);

        // Assert: sleep_run_steps table exists with correct structure
        let exists: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name='sleep_run_steps'",
                [],
                |row| row.get(0),
            )
            .expect("check table");
        assert!(exists, "sleep_run_steps should exist on fresh DB");

        // Assert: can insert a valid run + step
        conn.execute(
            "INSERT INTO sleep_runs (id, agent_id, status, trigger_type, started_at)
             VALUES ('fresh-run', 'agent-a', 'running', 'manual', '2024-01-01T00:00:00Z')",
            [],
        )
        .expect("insert run");
        conn.execute(
            "INSERT INTO sleep_run_steps (sleep_run_id, step_name, status)
             VALUES ('fresh-run', 'event_extraction', 'pending')",
            [],
        )
        .expect("insert step");

        let step_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sleep_run_steps WHERE sleep_run_id = 'fresh-run'",
                [],
                |row| row.get(0),
            )
            .expect("count");
        assert_eq!(step_count, 1);
    }
}
