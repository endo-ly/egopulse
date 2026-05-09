//! スキーマ定義・マイグレーション。

use std::collections::HashMap;

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

/// Concatenate two JSON arrays: `[a1, a2] + [b1] → [a1, a2, b1]`.
///
/// Falls back to empty arrays on parse failure so the merge never panics.
fn concat_json_arrays(first: &str, second: &str) -> String {
    let mut arr: Vec<serde_json::Value> = serde_json::from_str(first).unwrap_or_default();
    let second_arr: Vec<serde_json::Value> = serde_json::from_str(second).unwrap_or_default();
    arr.extend(second_arr);
    serde_json::to_string(&arr).unwrap_or_default()
}

/// Merge the `sessions` row from `loser` into `winner`.
///
/// - If only the loser has a session, it is moved to the winner.
/// - If both have sessions, `messages_json` arrays are concatenated.
/// - If only the winner has a session, nothing changes.
fn merge_sessions(
    tx: &rusqlite::Transaction<'_>,
    winner: i64,
    loser: i64,
) -> Result<(), StorageError> {
    let loser_exists: bool = tx.query_row(
        "SELECT COUNT(*) > 0 FROM sessions WHERE chat_id = ?1",
        params![loser],
        |row| row.get(0),
    )?;

    if !loser_exists {
        return Ok(());
    }

    let winner_exists: bool = tx.query_row(
        "SELECT COUNT(*) > 0 FROM sessions WHERE chat_id = ?1",
        params![winner],
        |row| row.get(0),
    )?;

    if !winner_exists {
        tx.execute(
            "UPDATE sessions SET chat_id = ?1 WHERE chat_id = ?2",
            params![winner, loser],
        )?;
        return Ok(());
    }
    let loser_json: String = tx.query_row(
        "SELECT messages_json FROM sessions WHERE chat_id = ?1",
        params![loser],
        |row| row.get(0),
    )?;
    let winner_json: String = tx.query_row(
        "SELECT messages_json FROM sessions WHERE chat_id = ?1",
        params![winner],
        |row| row.get(0),
    )?;
    let loser_updated: String = tx.query_row(
        "SELECT updated_at FROM sessions WHERE chat_id = ?1",
        params![loser],
        |row| row.get(0),
    )?;
    let winner_updated: String = tx.query_row(
        "SELECT updated_at FROM sessions WHERE chat_id = ?1",
        params![winner],
        |row| row.get(0),
    )?;

    let merged_json = concat_json_arrays(&winner_json, &loser_json);
    let updated_at = std::cmp::max(winner_updated, loser_updated);

    tx.execute(
        "UPDATE sessions SET messages_json = ?1, updated_at = ?2 WHERE chat_id = ?3",
        params![merged_json, updated_at, winner],
    )?;
    tx.execute("DELETE FROM sessions WHERE chat_id = ?1", params![loser])?;

    Ok(())
}

/// Merge all data from `loser` chat into `winner` chat, then delete the loser row.
///
/// Data is reassigned in FK-dependency order:
/// 1. `tool_calls` (FK → chats) — conflicting PK rows from loser are dropped
/// 2. `messages` — conflicting PK rows from loser are dropped
/// 3. `llm_usage_logs` — auto-increment PK, no conflicts possible
/// 4. `sessions` — `messages_json` arrays are concatenated
///
/// PK conflicts arise when the same `(id, chat_id)` would collide after
/// reassignment.  Conflicting rows are true duplicates (same Discord snowflake
/// ID seen by two bot instances), so dropping the loser's copy preserves all
/// unique data.
fn merge_chat_into_winner(
    tx: &rusqlite::Transaction<'_>,
    winner: i64,
    loser: i64,
) -> Result<(), StorageError> {
    tx.execute(
        "DELETE FROM tool_calls WHERE chat_id = ?1
         AND (id, message_id) IN (
             SELECT id, message_id FROM tool_calls WHERE chat_id = ?2
         )",
        params![loser, winner],
    )?;
    tx.execute(
        "UPDATE tool_calls SET chat_id = ?1 WHERE chat_id = ?2",
        params![winner, loser],
    )?;

    tx.execute(
        "DELETE FROM messages WHERE chat_id = ?1
         AND id IN (SELECT id FROM messages WHERE chat_id = ?2)",
        params![loser, winner],
    )?;
    tx.execute(
        "UPDATE messages SET chat_id = ?1 WHERE chat_id = ?2",
        params![winner, loser],
    )?;

    tx.execute(
        "UPDATE llm_usage_logs SET chat_id = ?1 WHERE chat_id = ?2",
        params![winner, loser],
    )?;

    merge_sessions(tx, winner, loser)?;

    tx.execute("DELETE FROM chats WHERE chat_id = ?1", params![loser])?;

    Ok(())
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
        // When multiple rows map to the same stripped ID, merge them into one.
        {
            let has_external_id: bool = tx
                .query_row(
                    "SELECT COUNT(*) > 0 FROM pragma_table_info('chats') WHERE name = 'external_chat_id'",
                    [],
                    |row| row.get(0),
                )
                .unwrap_or(false);

            if has_external_id {
                let mut stmt = tx.prepare(
                    "SELECT chat_id, external_chat_id FROM chats
                     WHERE channel = 'discord'
                       AND external_chat_id LIKE '%:bot:%:agent:%'",
                )?;
                let rows: Vec<(i64, String)> = stmt
                    .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
                    .collect::<Result<Vec<_>, _>>()?;
                drop(stmt);

                let mut groups: HashMap<String, Vec<i64>> = HashMap::new();
                for (chat_id, old_id) in &rows {
                    if let Some(new_id) = strip_bot_segment(old_id) {
                        groups.entry(new_id).or_default().push(*chat_id);
                    }
                }

                for (new_id, mut chat_ids) in groups {
                    let existing: Option<i64> = tx
                        .query_row(
                            "SELECT chat_id FROM chats
                             WHERE channel = 'discord' AND external_chat_id = ?1
                             LIMIT 1",
                            params![new_id],
                            |row| row.get(0),
                        )
                        .ok();

                    if let Some(existing_id) = existing {
                        chat_ids.push(existing_id);
                    }

                    if chat_ids.is_empty() {
                        continue;
                    }

                    let winner = existing
                        .unwrap_or_else(|| *chat_ids.iter().max().expect("group is non-empty"));

                    for loser in chat_ids {
                        if loser != winner {
                            merge_chat_into_winner(&tx, winner, loser)?;
                        }
                    }

                    tx.execute(
                        "UPDATE chats SET external_chat_id = ?1 WHERE chat_id = ?2",
                        params![new_id, winner],
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
