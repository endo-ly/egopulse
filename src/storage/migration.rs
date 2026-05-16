//! スキーマ定義・マイグレーション。

use rusqlite::{Connection, OptionalExtension, params};

use crate::error::StorageError;

/// 現在のスキーマバージョン。
///
/// スキーマを変更する際はこの値をインクリメントし、
/// `run_migrations` に対応する `if version < N` ブロックを追加する。
pub(super) const SCHEMA_VERSION: i64 = 1;

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
            SCHEMA_VERSION,
            "full schema: chats, messages, sessions, tool_calls, llm_usage_logs, sleep_runs, memory_snapshots, pulse_runs",
        )?;
        version = SCHEMA_VERSION;
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
}
