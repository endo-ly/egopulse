//! スキーマ定義・マイグレーション。

use rusqlite::{Connection, OptionalExtension, params};

use crate::error::StorageError;

/// 現在のスキーマバージョン。
///
/// スキーマを変更する際はこの値をインクリメントし、
/// `run_migrations` に対応する `if version < N` ブロックを追加する。
pub(super) const SCHEMA_VERSION: i64 = 13;

/// `db_meta` に格納されたスキーマバージョンを読み取る。
///
/// テーブルが存在しない場合またはバージョン未設定なら `0` を返す。この読み取りは
/// forward-version guard より前にDDL/DMLを実行しない。
fn schema_version(conn: &Connection) -> Result<i64, StorageError> {
    let has_db_meta: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'db_meta')",
        [],
        |row| row.get(0),
    )?;
    if !has_db_meta {
        return Ok(0);
    }
    let raw: Option<String> = conn
        .query_row(
            "SELECT value FROM db_meta WHERE key = 'schema_version'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    Ok(raw.and_then(|s| s.parse::<i64>().ok()).unwrap_or(0))
}

/// Creates the schema metadata table after the forward-version guard has run.
fn ensure_schema_meta(conn: &Connection) -> Result<(), StorageError> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS db_meta (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        )",
        [],
    )?;
    Ok(())
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

/// Adds a column to `table` only when it is not already present.
///
/// Migrations must be safe to re-run after a version rollback (the test
/// suite does this), so every `ALTER TABLE ADD COLUMN` is guarded by a
/// `pragma_table_info` existence check.
fn add_column_if_missing(
    conn: &Connection,
    table: &str,
    column: &str,
    type_def: &str,
) -> Result<(), StorageError> {
    let exists: bool = {
        let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
        let names: Vec<String> = stmt
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<Result<Vec<_>, _>>()?;
        names.iter().any(|name| name == column)
    };
    if !exists {
        conn.execute_batch(&format!(
            "ALTER TABLE {table} ADD COLUMN {column} {type_def}"
        ))?
    }
    Ok(())
}

/// 未適用のマイグレーションを逐次実行する。
pub(super) fn run_migrations(conn: &Connection) -> Result<(), StorageError> {
    let mut version = schema_version(conn)?;

    if version > SCHEMA_VERSION {
        return Err(StorageError::UnsupportedSchemaVersion {
            database: "normal",
            found: version,
            supported: SCHEMA_VERSION,
        });
    }
    ensure_schema_meta(conn)?;

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
                created_at      TEXT NOT NULL,
                UNIQUE (run_id, file),
                FOREIGN KEY (run_id) REFERENCES sleep_runs(id) ON DELETE CASCADE,
                CHECK (file IN ('episodic', 'semantic', 'prospective'))
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

    if version < 8 {
        let tx = conn.unchecked_transaction()?;
        tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS sleep_step_checkpoints (
                agent_id     TEXT NOT NULL,
                step_name    TEXT NOT NULL,
                source_kind  TEXT NOT NULL,
                source_id    TEXT NOT NULL,
                cursor_at    TEXT NOT NULL,
                cursor_id    TEXT NOT NULL,
                updated_at   TEXT NOT NULL,
                PRIMARY KEY (agent_id, step_name, source_kind, source_id),
                CHECK (step_name IN ('event_extraction', 'semantic_update', 'prospective_update')),
                CHECK (source_kind IN ('messages', 'episode_events')),
                CHECK (
                    (step_name IN ('event_extraction', 'prospective_update') AND source_kind = 'messages')
                    OR (step_name = 'semantic_update' AND source_kind = 'episode_events')
                )
            );",
        )?;
        set_schema_version_in_tx(
            &tx,
            8,
            "add sleep_step_checkpoints for per-step cursor tracking",
        )?;
        tx.commit()?;
        version = 8;
    }

    if version < 9 {
        let tx = conn.unchecked_transaction()?;
        tx.execute_batch(
            "CREATE TABLE memory_snapshots_v9 (
                id              TEXT PRIMARY KEY,
                run_id          TEXT NOT NULL,
                agent_id        TEXT NOT NULL,
                file            TEXT NOT NULL,
                content_before  TEXT NOT NULL,
                content_after   TEXT NOT NULL,
                created_at      TEXT NOT NULL,
                UNIQUE (run_id, file),
                FOREIGN KEY (run_id) REFERENCES sleep_runs(id) ON DELETE CASCADE,
                CHECK (file IN ('episodic', 'semantic', 'prospective'))
            );

            INSERT INTO memory_snapshots_v9
                SELECT
                    ms.id,
                    ms.run_id,
                    ms.agent_id,
                    ms.file,
                    ms.content_before,
                    ms.content_after,
                    ms.created_at
                FROM memory_snapshots ms
                JOIN sleep_runs sr ON sr.id = ms.run_id
                WHERE ms.file IN ('episodic', 'semantic', 'prospective')
                  AND NOT EXISTS (
                      SELECT 1
                      FROM memory_snapshots newer
                      WHERE newer.run_id = ms.run_id
                        AND newer.file = ms.file
                        AND (
                            newer.created_at > ms.created_at
                            OR (
                                newer.created_at = ms.created_at
                                AND newer.id > ms.id
                            )
                        )
                  );

            DROP TABLE memory_snapshots;
            ALTER TABLE memory_snapshots_v9 RENAME TO memory_snapshots;

            CREATE INDEX IF NOT EXISTS idx_memory_snapshots_run_id
                ON memory_snapshots(run_id);
            CREATE INDEX IF NOT EXISTS idx_memory_snapshots_agent_created
                ON memory_snapshots(agent_id, created_at);",
        )?;
        set_schema_version_in_tx(
            &tx,
            9,
            "enforce memory_snapshot uniqueness, FK, and file CHECK",
        )?;
        tx.commit()?;
        version = 9;
    }

    if version < 10 {
        let tx = conn.unchecked_transaction()?;
        // Idempotent: rollback tests re-run migrations on an already-current
        // schema, so only add the columns when they are still missing.
        let needs_columns = {
            let mut stmt = tx.prepare("PRAGMA table_info(llm_usage_logs)")?;
            let names: Vec<String> = stmt
                .query_map([], |row| row.get::<_, String>(1))?
                .collect::<Result<Vec<_>, _>>()?;
            !names.iter().any(|name| name == "estimated_tokens")
        };
        if needs_columns {
            // Existing rows keep DEFAULT 0 and are excluded from calibration
            // rebuild by the `estimated_tokens > 0` filter, preserving
            // usage/cost history.
            tx.execute_batch(
                "ALTER TABLE llm_usage_logs ADD COLUMN estimated_tokens INTEGER NOT NULL DEFAULT 0;
                 ALTER TABLE llm_usage_logs ADD COLUMN has_tools INTEGER NOT NULL DEFAULT 0;",
            )?;
        }
        set_schema_version_in_tx(
            &tx,
            10,
            "add estimated_tokens and has_tools to llm_usage_logs for calibration rebuild",
        )?;
        tx.commit()?;
        version = 10;
    }

    if version < 11 {
        conn.execute_batch("CREATE INDEX IF NOT EXISTS idx_chats_agent_id ON chats(agent_id)")?;
        set_schema_version(
            conn,
            11,
            "add index on chats(agent_id) for sleep pending-message queries",
        )?;
        version = 11;
    }

    if version < 12 {
        let tx = conn.unchecked_transaction()?;

        // --- chats: integer revision CAS + per-chat message sequence ------
        add_column_if_missing(&tx, "chats", "revision", "INTEGER NOT NULL DEFAULT 0")?;
        add_column_if_missing(
            &tx,
            "chats",
            "next_message_seq",
            "INTEGER NOT NULL DEFAULT 1",
        )?;

        // --- messages: causal seq, turn ownership, parent linkage -----------
        add_column_if_missing(&tx, "messages", "seq", "INTEGER")?;
        add_column_if_missing(&tx, "messages", "turn_id", "TEXT")?;
        add_column_if_missing(&tx, "messages", "parent_message_id", "TEXT")?;

        // --- sessions: how far the snapshot covers -------------------------
        add_column_if_missing(
            &tx,
            "sessions",
            "snapshot_through_seq",
            "INTEGER NOT NULL DEFAULT 0",
        )?;

        // --- tool_calls: execution ledger columns --------------------------
        add_column_if_missing(&tx, "tool_calls", "turn_id", "TEXT")?;
        add_column_if_missing(
            &tx,
            "tool_calls",
            "state",
            "TEXT NOT NULL DEFAULT 'pending'",
        )?;
        add_column_if_missing(&tx, "tool_calls", "input_hash", "TEXT")?;
        add_column_if_missing(&tx, "tool_calls", "idempotency_class", "TEXT")?;
        add_column_if_missing(&tx, "tool_calls", "idempotency_key", "TEXT")?;
        add_column_if_missing(&tx, "tool_calls", "started_at", "TEXT")?;
        add_column_if_missing(&tx, "tool_calls", "finished_at", "TEXT")?;
        add_column_if_missing(&tx, "tool_calls", "error_kind", "TEXT")?;
        add_column_if_missing(&tx, "tool_calls", "error_message", "TEXT")?;

        // --- turn_runs: durable Turn lifecycle -----------------------------
        tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS turn_runs (
                turn_id TEXT PRIMARY KEY,
                chat_id INTEGER NOT NULL,
                request_key TEXT NOT NULL,
                state TEXT NOT NULL CHECK (state IN (
                    'accepted',
                    'input_committed',
                    'model_pending',
                    'model_completed',
                    'tools_pending',
                    'tools_completed',
                    'completed',
                    'failed',
                    'cancelled',
                    'uncertain'
                )),
                current_iteration INTEGER NOT NULL DEFAULT 0,
                input_message_id TEXT,
                final_message_id TEXT,
                config_revision INTEGER NOT NULL DEFAULT 0,
                config_fingerprint TEXT,
                model_request_hash TEXT,
                model_attempt INTEGER NOT NULL DEFAULT 0,
                output_published INTEGER NOT NULL DEFAULT 0,
                error_kind TEXT,
                error_message TEXT,
                accepted_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                finished_at TEXT,
                request_payload_hash TEXT,
                UNIQUE(chat_id, request_key)
            );

            CREATE INDEX IF NOT EXISTS idx_turn_runs_chat ON turn_runs(chat_id);
            CREATE INDEX IF NOT EXISTS idx_turn_runs_state ON turn_runs(state);",
        )?;

        // --- backfill messages.seq per chat in stable (timestamp, id) order.
        // Only rows without an assigned seq are touched, so the statement is
        // safe to re-run after a version rollback.
        tx.execute_batch(
            "UPDATE messages
             SET seq = numbered.new_seq
             FROM (
                 SELECT rowid AS source_rowid,
                        ROW_NUMBER() OVER (
                            PARTITION BY chat_id
                            ORDER BY timestamp ASC, id ASC
                        ) AS new_seq
                 FROM messages
             ) AS numbered
             WHERE messages.rowid = numbered.source_rowid
               AND messages.seq IS NULL",
        )?;

        // Causal order uniqueness for assigned seqs. NULL seqs (unassigned
        // legacy messages) are excluded so they never collide.
        tx.execute_batch(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_messages_chat_seq
                ON messages(chat_id, seq)
                WHERE seq IS NOT NULL",
        )?;

        // --- backfill chats.revision / next_message_seq from messages.
        // revision starts at the message count (each legacy message is one
        // conversation change); next_message_seq continues after the last seq.
        tx.execute_batch(
            "UPDATE chats
             SET next_message_seq = COALESCE(
                     (SELECT MAX(seq) + 1 FROM messages WHERE messages.chat_id = chats.chat_id),
                     1
                 ),
                 revision = COALESCE(
                     (SELECT COUNT(*) FROM messages WHERE messages.chat_id = chats.chat_id),
                     0
                 )",
        )?;

        // --- backfill sessions.snapshot_through_seq from the chat's max seq.
        tx.execute_batch(
            "UPDATE sessions
             SET snapshot_through_seq = COALESCE(
                 (SELECT MAX(seq) FROM messages WHERE messages.chat_id = sessions.chat_id),
                 0
             )",
        )?;

        // --- backfill tool_calls.state. Existing rows with output are
        // treated as succeeded; rows without output cannot have their result
        // verified, so they become uncertain rather than auto-retried.
        tx.execute_batch(
            "UPDATE tool_calls
             SET state = CASE
                 WHEN tool_output IS NOT NULL THEN 'succeeded'
                 ELSE 'uncertain'
             END
             WHERE state = 'pending'",
        )?;

        // Causal identity for the Tool execution ledger: a Tool call is unique
        // within its Turn so `claim` can detect a re-claim of the same
        // (turn_id, tool_call_id) and reuse or block on the stored state.
        // Legacy rows with NULL turn_id are excluded (NULL ≠ NULL in SQLite).
        tx.execute_batch(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_tool_calls_turn_id
                ON tool_calls(turn_id, id)
                WHERE turn_id IS NOT NULL",
        )?;

        set_schema_version_in_tx(
            &tx,
            12,
            "add turn_runs; extend chats/messages/sessions/tool_calls with durable turn + integer seq/revision state",
        )?;
        tx.commit()?;
        version = 12;
    }

    if version < 13 {
        let tx = conn.unchecked_transaction()?;

        add_column_if_missing(&tx, "turn_runs", "request_payload_hash", "TEXT")?;

        set_schema_version_in_tx(
            &tx,
            13,
            "add turn_runs.request_payload_hash for ingress idempotency verification",
        )?;
        tx.commit()?;
        version = 13;
    }

    debug_assert_eq!(version, SCHEMA_VERSION, "all migrations applied");
    Ok(())
}

/// Secret DB のスキーマバージョン。
///
/// `egopulse.db` とは独立して管理する。
pub(super) const SECRET_SCHEMA_VERSION: i64 = 4;

/// Secret DB のマイグレーションを実行する。
///
/// `egopulse.db` の6テーブル（chats, messages, sessions, llm_usage_logs, db_meta, schema_migrations）
/// のみを作成する。`tool_calls`, `sleep_runs`, `pulse_runs` 等は含まない。
pub(super) fn run_secret_migrations(conn: &Connection) -> Result<(), StorageError> {
    let mut version = schema_version(conn)?;

    if version > SECRET_SCHEMA_VERSION {
        return Err(StorageError::UnsupportedSchemaVersion {
            database: "secret",
            found: version,
            supported: SECRET_SCHEMA_VERSION,
        });
    }
    ensure_schema_meta(conn)?;

    if version < 1 {
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
                ON llm_usage_logs(created_at);",
        )?;
        set_schema_version(
            conn,
            1,
            "initial secret schema: chats, messages, sessions, llm_usage_logs",
        )?;
        version = 1;
    }

    if version < 2 {
        // Idempotent: rollback tests re-run migrations on an already-current
        // schema, so only add the columns when they are still missing.
        let needs_columns = {
            let mut stmt = conn.prepare("PRAGMA table_info(llm_usage_logs)")?;
            let names: Vec<String> = stmt
                .query_map([], |row| row.get::<_, String>(1))?
                .collect::<Result<Vec<_>, _>>()?;
            !names.iter().any(|name| name == "estimated_tokens")
        };
        if needs_columns {
            // Existing rows keep DEFAULT 0 and are excluded from calibration
            // rebuild by the `estimated_tokens > 0` filter, preserving
            // usage/cost history.
            conn.execute_batch(
                "ALTER TABLE llm_usage_logs ADD COLUMN estimated_tokens INTEGER NOT NULL DEFAULT 0;
                 ALTER TABLE llm_usage_logs ADD COLUMN has_tools INTEGER NOT NULL DEFAULT 0;",
            )?;
        }
        set_schema_version(
            conn,
            2,
            "add estimated_tokens and has_tools to llm_usage_logs for calibration rebuild",
        )?;
        version = 2;
    }

    if version < 3 {
        let tx = conn.unchecked_transaction()?;

        // Mirror the conversation + turn extensions on the secret DB.
        // tool_calls is absent from the secret DB (secret mode skips tool
        // persistence), so only the chat/message/session columns and turn_runs
        // are added here.
        add_column_if_missing(&tx, "chats", "revision", "INTEGER NOT NULL DEFAULT 0")?;
        add_column_if_missing(
            &tx,
            "chats",
            "next_message_seq",
            "INTEGER NOT NULL DEFAULT 1",
        )?;
        add_column_if_missing(&tx, "messages", "seq", "INTEGER")?;
        add_column_if_missing(&tx, "messages", "turn_id", "TEXT")?;
        add_column_if_missing(&tx, "messages", "parent_message_id", "TEXT")?;
        add_column_if_missing(
            &tx,
            "sessions",
            "snapshot_through_seq",
            "INTEGER NOT NULL DEFAULT 0",
        )?;

        tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS turn_runs (
                turn_id TEXT PRIMARY KEY,
                chat_id INTEGER NOT NULL,
                request_key TEXT NOT NULL,
                state TEXT NOT NULL CHECK (state IN (
                    'accepted',
                    'input_committed',
                    'model_pending',
                    'model_completed',
                    'tools_pending',
                    'tools_completed',
                    'completed',
                    'failed',
                    'cancelled',
                    'uncertain'
                )),
                current_iteration INTEGER NOT NULL DEFAULT 0,
                input_message_id TEXT,
                final_message_id TEXT,
                config_revision INTEGER NOT NULL DEFAULT 0,
                config_fingerprint TEXT,
                model_request_hash TEXT,
                model_attempt INTEGER NOT NULL DEFAULT 0,
                output_published INTEGER NOT NULL DEFAULT 0,
                error_kind TEXT,
                error_message TEXT,
                accepted_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                finished_at TEXT,
                request_payload_hash TEXT,
                UNIQUE(chat_id, request_key)
            );

            CREATE INDEX IF NOT EXISTS idx_turn_runs_chat ON turn_runs(chat_id);
            CREATE INDEX IF NOT EXISTS idx_turn_runs_state ON turn_runs(state);",
        )?;

        tx.execute_batch(
            "UPDATE messages
             SET seq = numbered.new_seq
             FROM (
                 SELECT rowid AS source_rowid,
                        ROW_NUMBER() OVER (
                            PARTITION BY chat_id
                            ORDER BY timestamp ASC, id ASC
                        ) AS new_seq
                 FROM messages
             ) AS numbered
             WHERE messages.rowid = numbered.source_rowid
               AND messages.seq IS NULL",
        )?;
        tx.execute_batch(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_messages_chat_seq
                ON messages(chat_id, seq)
                WHERE seq IS NOT NULL",
        )?;
        tx.execute_batch(
            "UPDATE chats
             SET next_message_seq = COALESCE(
                     (SELECT MAX(seq) + 1 FROM messages WHERE messages.chat_id = chats.chat_id),
                     1
                 ),
                 revision = COALESCE(
                     (SELECT COUNT(*) FROM messages WHERE messages.chat_id = chats.chat_id),
                     0
                 )",
        )?;
        tx.execute_batch(
            "UPDATE sessions
             SET snapshot_through_seq = COALESCE(
                 (SELECT MAX(seq) FROM messages WHERE messages.chat_id = sessions.chat_id),
                 0
             )",
        )?;

        set_schema_version_in_tx(
            &tx,
            3,
            "add turn_runs; extend chats/messages/sessions with durable turn + integer seq/revision state",
        )?;
        tx.commit()?;
        version = 3;
    }

    if version < 4 {
        let tx = conn.unchecked_transaction()?;

        add_column_if_missing(&tx, "turn_runs", "request_payload_hash", "TEXT")?;

        // Tool execution ledger. Secret conversations now own a private
        // `tool_calls` table so claim-before-execute, result reuse, and
        // non-idempotent Tool dedup apply uniformly to both scopes. Secret
        // Tool input/output stays inside the secret DB and never reaches the
        // normal DB.
        tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS tool_calls (
                id TEXT NOT NULL,
                chat_id INTEGER NOT NULL,
                message_id TEXT NOT NULL,
                tool_name TEXT NOT NULL,
                tool_input TEXT NOT NULL,
                tool_output TEXT,
                timestamp TEXT NOT NULL,
                turn_id TEXT,
                state TEXT NOT NULL DEFAULT 'pending',
                input_hash TEXT,
                idempotency_class TEXT,
                idempotency_key TEXT,
                started_at TEXT,
                finished_at TEXT,
                error_kind TEXT,
                error_message TEXT,
                PRIMARY KEY (id, chat_id, message_id),
                FOREIGN KEY (chat_id) REFERENCES chats(chat_id)
            );

            CREATE INDEX IF NOT EXISTS idx_tool_calls_chat_id
                ON tool_calls(chat_id);

            CREATE INDEX IF NOT EXISTS idx_tool_calls_chat_message_id
                ON tool_calls(chat_id, message_id);

            CREATE UNIQUE INDEX IF NOT EXISTS idx_tool_calls_turn_id
                ON tool_calls(turn_id, id)
                WHERE turn_id IS NOT NULL",
        )?;

        set_schema_version_in_tx(
            &tx,
            4,
            "add turn_runs.request_payload_hash and tool_calls ledger",
        )?;
        tx.commit()?;
        version = 4;
    }

    debug_assert_eq!(version, SECRET_SCHEMA_VERSION);
    Ok(())
}

#[cfg(test)]
mod tests {
    fn set_future_schema_version(conn: &rusqlite::Connection, version: i64) {
        conn.execute(
            "UPDATE db_meta SET value = ?1 WHERE key = 'schema_version'",
            [version.to_string()],
        )
        .expect("set future schema version");
    }

    fn schema_version(conn: &rusqlite::Connection) -> i64 {
        conn.query_row(
            "SELECT value FROM db_meta WHERE key = 'schema_version'",
            [],
            |row| row.get::<_, String>(0),
        )
        .expect("schema version")
        .parse()
        .expect("schema version is an integer")
    }

    #[test]
    fn normal_future_schema_is_rejected_without_writes() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("runtime").join("egopulse.db");
        let db = super::super::Database::new(&db_path).expect("current db");
        let conn = db.get_conn().expect("connection");
        let future_version = super::SCHEMA_VERSION + 1;
        set_future_schema_version(&conn, future_version);
        conn.execute(
            "INSERT INTO chats (chat_id, chat_type, last_message_time, agent_id)
             VALUES (9001, 'test', '2026-01-01T00:00:00Z', 'agent')",
            [],
        )
        .expect("insert preserved data");

        // Act
        let error = super::run_migrations(&conn).expect_err("future schema must fail");

        // Assert
        assert!(matches!(
            error,
            crate::error::StorageError::UnsupportedSchemaVersion {
                database: "normal",
                found,
                supported: super::SCHEMA_VERSION,
            } if found == future_version
        ));
        assert_eq!(schema_version(&conn), future_version);
        let chats: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM chats WHERE chat_id = 9001",
                [],
                |row| row.get(0),
            )
            .expect("count preserved data");
        assert_eq!(chats, 1);
    }

    #[test]
    fn secret_future_schema_is_rejected_without_writes() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("runtime").join("secret.db");
        let db = super::super::Database::new_secret(&db_path).expect("current secret db");
        let conn = db.get_conn().expect("connection");
        let future_version = super::SECRET_SCHEMA_VERSION + 1;
        set_future_schema_version(&conn, future_version);
        conn.execute(
            "INSERT INTO chats (chat_id, chat_type, last_message_time, agent_id)
             VALUES (9001, 'test', '2026-01-01T00:00:00Z', 'agent')",
            [],
        )
        .expect("insert preserved data");

        // Act
        let error = super::run_secret_migrations(&conn).expect_err("future schema must fail");

        // Assert
        assert!(matches!(
            error,
            crate::error::StorageError::UnsupportedSchemaVersion {
                database: "secret",
                found,
                supported: super::SECRET_SCHEMA_VERSION,
            } if found == future_version
        ));
        assert_eq!(schema_version(&conn), future_version);
        let chats: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM chats WHERE chat_id = 9001",
                [],
                |row| row.get(0),
            )
            .expect("count preserved data");
        assert_eq!(chats, 1);
    }

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
            "sleep_step_checkpoints",
            "turn_runs",
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

        // Assert: schema version advanced past v7
        let conn = db.get_conn().expect("pool");
        let version: String = conn
            .query_row(
                "SELECT value FROM db_meta WHERE key = 'schema_version'",
                [],
                |row| row.get(0),
            )
            .expect("schema version");
        assert!(
            version.parse::<i64>().unwrap() >= 7,
            "schema version should be at least 7"
        );

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

    // --- v8: sleep_step_checkpoints -------------------------------------------

    fn create_v7_db(dir: &tempfile::TempDir) -> super::super::Database {
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
                    id TEXT NOT NULL, chat_id INTEGER NOT NULL,
                    sender_id TEXT NOT NULL, content TEXT NOT NULL,
                    sender_kind TEXT NOT NULL, timestamp TEXT NOT NULL,
                    message_kind TEXT NOT NULL DEFAULT 'message',
                    recipient_agent_id TEXT,
                    PRIMARY KEY (id, chat_id)
                );
                CREATE INDEX IF NOT EXISTS idx_messages_chat_timestamp ON messages(chat_id, timestamp);
                CREATE TABLE IF NOT EXISTS sessions (
                    chat_id INTEGER PRIMARY KEY, messages_json TEXT NOT NULL, updated_at TEXT NOT NULL
                );
                CREATE TABLE IF NOT EXISTS tool_calls (
                    id TEXT NOT NULL, chat_id INTEGER NOT NULL, message_id TEXT NOT NULL,
                    tool_name TEXT NOT NULL, tool_input TEXT NOT NULL, tool_output TEXT,
                    timestamp TEXT NOT NULL,
                    PRIMARY KEY (id, chat_id, message_id),
                    FOREIGN KEY (chat_id) REFERENCES chats(chat_id)
                );
                CREATE INDEX IF NOT EXISTS idx_tool_calls_chat_id ON tool_calls(chat_id);
                CREATE INDEX IF NOT EXISTS idx_tool_calls_chat_message_id ON tool_calls(chat_id, message_id);
                CREATE TABLE IF NOT EXISTS llm_usage_logs (
                    id INTEGER PRIMARY KEY AUTOINCREMENT, chat_id INTEGER NOT NULL,
                    caller_channel TEXT NOT NULL, provider TEXT NOT NULL, model TEXT NOT NULL,
                    input_tokens INTEGER NOT NULL, output_tokens INTEGER NOT NULL,
                    total_tokens INTEGER NOT NULL, request_kind TEXT NOT NULL DEFAULT 'agent_loop',
                    created_at TEXT NOT NULL
                );
                CREATE INDEX IF NOT EXISTS idx_llm_usage_chat_created ON llm_usage_logs(chat_id, created_at);
                CREATE INDEX IF NOT EXISTS idx_llm_usage_created ON llm_usage_logs(created_at);
                CREATE TABLE IF NOT EXISTS sleep_runs (
                    id TEXT PRIMARY KEY, agent_id TEXT NOT NULL, status TEXT NOT NULL,
                    trigger_type TEXT NOT NULL, started_at TEXT NOT NULL, finished_at TEXT,
                    source_chats_json TEXT NOT NULL DEFAULT '[]', source_digest_md TEXT,
                    input_tokens INTEGER NOT NULL DEFAULT 0, output_tokens INTEGER NOT NULL DEFAULT 0,
                    total_tokens INTEGER NOT NULL DEFAULT 0, error_message TEXT
                );
                CREATE INDEX IF NOT EXISTS idx_sleep_runs_agent_started ON sleep_runs(agent_id, started_at);
                CREATE INDEX IF NOT EXISTS idx_sleep_runs_agent_status ON sleep_runs(agent_id, status);
                CREATE TABLE IF NOT EXISTS memory_snapshots (
                    id TEXT PRIMARY KEY, run_id TEXT NOT NULL, agent_id TEXT NOT NULL,
                    file TEXT NOT NULL, content_before TEXT NOT NULL, content_after TEXT NOT NULL,
                    created_at TEXT NOT NULL
                );
                CREATE INDEX IF NOT EXISTS idx_memory_snapshots_run_id ON memory_snapshots(run_id);
                CREATE INDEX IF NOT EXISTS idx_memory_snapshots_agent_created ON memory_snapshots(agent_id, created_at);
                CREATE TABLE IF NOT EXISTS pulse_runs (
                    id TEXT PRIMARY KEY, agent_id TEXT NOT NULL, intention_id TEXT NOT NULL,
                    due_key TEXT NOT NULL, chat_id INTEGER, message_id TEXT, status TEXT NOT NULL,
                    started_at TEXT NOT NULL, finished_at TEXT, output_kind TEXT, output_text TEXT,
                    error_message TEXT
                );
                CREATE UNIQUE INDEX IF NOT EXISTS idx_pulse_runs_due ON pulse_runs(agent_id, intention_id, due_key);
                CREATE INDEX IF NOT EXISTS idx_pulse_runs_agent_started ON pulse_runs(agent_id, started_at);
                CREATE INDEX IF NOT EXISTS idx_pulse_runs_chat_id ON pulse_runs(chat_id);
                CREATE TABLE IF NOT EXISTS episode_events (
                    id TEXT PRIMARY KEY, agent_id TEXT NOT NULL, experienced_at TEXT NOT NULL,
                    encoded_at TEXT NOT NULL, kind TEXT NOT NULL, title TEXT NOT NULL,
                    body_md TEXT NOT NULL, ripple_strength INTEGER NOT NULL DEFAULT 3,
                    certainty TEXT NOT NULL DEFAULT 'stated', sleep_run_id TEXT NOT NULL,
                    source_refs_json TEXT, created_at TEXT NOT NULL, updated_at TEXT NOT NULL,
                    CHECK (kind IN ('self', 'relationship', 'world', 'feat', 'anomaly', 'decision', 'insight', 'rhythm')),
                    CHECK (ripple_strength BETWEEN 1 AND 5),
                    CHECK (certainty IN ('stated', 'derived', 'tentative'))
                );
                CREATE INDEX IF NOT EXISTS idx_episode_events_agent_experienced ON episode_events(agent_id, experienced_at);
                CREATE INDEX IF NOT EXISTS idx_episode_events_agent_kind_experienced ON episode_events(agent_id, kind, experienced_at);
                CREATE INDEX IF NOT EXISTS idx_episode_events_agent_ripple_experienced ON episode_events(agent_id, ripple_strength, experienced_at);
                CREATE INDEX IF NOT EXISTS idx_episode_events_sleep_run ON episode_events(sleep_run_id);
                CREATE TABLE IF NOT EXISTS episode_rollups (
                    id TEXT PRIMARY KEY, agent_id TEXT NOT NULL, granularity TEXT NOT NULL,
                    period_key TEXT NOT NULL, period_start TEXT NOT NULL, period_end_exclusive TEXT NOT NULL,
                    summary_md TEXT NOT NULL, max_ripple INTEGER NOT NULL DEFAULT 3,
                    event_count INTEGER NOT NULL DEFAULT 0, generated_run_id TEXT NOT NULL,
                    created_at TEXT NOT NULL, updated_at TEXT NOT NULL,
                    CHECK (granularity IN ('week', 'month')),
                    CHECK (max_ripple BETWEEN 1 AND 5),
                    UNIQUE(agent_id, granularity, period_key)
                );
                CREATE INDEX IF NOT EXISTS idx_episode_rollups_agent_period ON episode_rollups(agent_id, granularity, period_start);
                CREATE INDEX IF NOT EXISTS idx_episode_rollups_agent_ripple ON episode_rollups(agent_id, granularity, max_ripple, period_start);
                CREATE TABLE IF NOT EXISTS sleep_run_steps (
                    sleep_run_id TEXT NOT NULL,
                    step_name TEXT NOT NULL CHECK (step_name IN ('event_extraction', 'episodic_update', 'semantic_update', 'prospective_update')),
                    status TEXT NOT NULL CHECK (status IN ('pending', 'running', 'success', 'failed', 'skipped')),
                    started_at TEXT, finished_at TEXT,
                    input_tokens INTEGER NOT NULL DEFAULT 0, output_tokens INTEGER NOT NULL DEFAULT 0,
                    error_message TEXT, metadata_json TEXT,
                    PRIMARY KEY (sleep_run_id, step_name),
                    FOREIGN KEY (sleep_run_id) REFERENCES sleep_runs(id) ON DELETE CASCADE
                );
            CREATE INDEX IF NOT EXISTS idx_sleep_run_steps_step_status
                ON sleep_run_steps(step_name, status, started_at);",
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

            super::set_schema_version(&conn, 7, "test v7 baseline").unwrap();
        }

        super::super::Database::new_unchecked(&db_path).unwrap()
    }

    #[test]
    fn migration_v7_to_v8_creates_validated_sleep_checkpoints() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let db = create_v7_db(&dir);

        // Act
        let conn = db.get_conn().expect("pool");
        super::run_migrations(&conn).expect("re-run migrations");

        // Assert: schema version advanced past v8
        let version: String = conn
            .query_row(
                "SELECT value FROM db_meta WHERE key = 'schema_version'",
                [],
                |row| row.get(0),
            )
            .expect("schema version");
        assert!(
            version.parse::<i64>().unwrap() >= 8,
            "schema version should be at least 8"
        );

        // Assert: table exists
        let exists: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name='sleep_step_checkpoints'",
                [],
                |row| row.get(0),
            )
            .expect("check table");
        assert!(exists, "sleep_step_checkpoints should exist");

        // Assert: valid rows can be inserted
        conn.execute(
            "INSERT INTO sleep_step_checkpoints
                (agent_id, step_name, source_kind, source_id, cursor_at, cursor_id, updated_at)
             VALUES ('agent-a', 'event_extraction', 'messages', 'chat-1', '2024-01-01T00:00:00Z', 'msg-1', '2024-01-01T00:00:00Z')",
            [],
        ).expect("insert valid checkpoint");

        // Assert: episodic_update is rejected (no checkpoint for derived step)
        let rejected = conn.execute(
            "INSERT INTO sleep_step_checkpoints
                (agent_id, step_name, source_kind, source_id, cursor_at, cursor_id, updated_at)
             VALUES ('agent-a', 'episodic_update', 'messages', 'chat-1', '2024-01-01T00:00:00Z', 'msg-1', '2024-01-01T00:00:00Z')",
            [],
        );
        assert!(
            rejected.is_err(),
            "should reject episodic_update checkpoint"
        );

        // Assert: semantic_update + messages is rejected (wrong source_kind)
        let rejected = conn.execute(
            "INSERT INTO sleep_step_checkpoints
                (agent_id, step_name, source_kind, source_id, cursor_at, cursor_id, updated_at)
             VALUES ('agent-a', 'semantic_update', 'messages', 'chat-1', '2024-01-01T00:00:00Z', 'msg-1', '2024-01-01T00:00:00Z')",
            [],
        );
        assert!(
            rejected.is_err(),
            "should reject semantic_update + messages"
        );
    }

    #[test]
    fn sleep_checkpoint_schema_rejects_invalid_step_source_pairs() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let db = create_v7_db(&dir);
        let conn = db.get_conn().expect("pool");
        super::run_migrations(&conn).expect("migrations");

        // Assert: prospective_update + episode_events is rejected
        let rejected = conn.execute(
            "INSERT INTO sleep_step_checkpoints
                (agent_id, step_name, source_kind, source_id, cursor_at, cursor_id, updated_at)
             VALUES ('agent-a', 'prospective_update', 'episode_events', 'agent-a', '2024-01-01T00:00:00Z', 'evt-1', '2024-01-01T00:00:00Z')",
            [],
        );
        assert!(
            rejected.is_err(),
            "should reject prospective_update + episode_events"
        );

        // Assert: event_extraction + episode_events is rejected
        let rejected = conn.execute(
            "INSERT INTO sleep_step_checkpoints
                (agent_id, step_name, source_kind, source_id, cursor_at, cursor_id, updated_at)
             VALUES ('agent-a', 'event_extraction', 'episode_events', 'agent-a', '2024-01-01T00:00:00Z', 'evt-1', '2024-01-01T00:00:00Z')",
            [],
        );
        assert!(
            rejected.is_err(),
            "should reject event_extraction + episode_events"
        );

        // Assert: semantic_update + episode_events is valid
        conn.execute(
            "INSERT INTO sleep_step_checkpoints
                (agent_id, step_name, source_kind, source_id, cursor_at, cursor_id, updated_at)
             VALUES ('agent-a', 'semantic_update', 'episode_events', 'agent-a', '2024-01-01T00:00:00Z', 'evt-1', '2024-01-01T00:00:00Z')",
            [],
        )
        .expect("semantic_update + episode_events should be valid");
    }

    // --- v9: memory_snapshots constraints -------------------------------------

    fn create_v8_db(dir: &tempfile::TempDir) -> super::super::Database {
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
                    chat_id INTEGER PRIMARY KEY, chat_title TEXT,
                    chat_type TEXT NOT NULL DEFAULT 'private', last_message_time TEXT NOT NULL,
                    channel TEXT, external_chat_id TEXT, agent_id TEXT NOT NULL DEFAULT 'default'
                );
                CREATE UNIQUE INDEX IF NOT EXISTS idx_chats_channel_external_chat_id ON chats(channel, external_chat_id);
                CREATE TABLE IF NOT EXISTS messages (
                    id TEXT NOT NULL, chat_id INTEGER NOT NULL, sender_id TEXT NOT NULL,
                    content TEXT NOT NULL, sender_kind TEXT NOT NULL, timestamp TEXT NOT NULL,
                    message_kind TEXT NOT NULL DEFAULT 'message', recipient_agent_id TEXT,
                    PRIMARY KEY (id, chat_id)
                );
                CREATE INDEX IF NOT EXISTS idx_messages_chat_timestamp ON messages(chat_id, timestamp);
                CREATE TABLE IF NOT EXISTS sessions (
                    chat_id INTEGER PRIMARY KEY, messages_json TEXT NOT NULL, updated_at TEXT NOT NULL
                );
                CREATE TABLE IF NOT EXISTS tool_calls (
                    id TEXT NOT NULL, chat_id INTEGER NOT NULL, message_id TEXT NOT NULL,
                    tool_name TEXT NOT NULL, tool_input TEXT NOT NULL, tool_output TEXT,
                    timestamp TEXT NOT NULL, PRIMARY KEY (id, chat_id, message_id),
                    FOREIGN KEY (chat_id) REFERENCES chats(chat_id)
                );
                CREATE INDEX IF NOT EXISTS idx_tool_calls_chat_id ON tool_calls(chat_id);
                CREATE INDEX IF NOT EXISTS idx_tool_calls_chat_message_id ON tool_calls(chat_id, message_id);
                CREATE TABLE IF NOT EXISTS llm_usage_logs (
                    id INTEGER PRIMARY KEY AUTOINCREMENT, chat_id INTEGER NOT NULL,
                    caller_channel TEXT NOT NULL, provider TEXT NOT NULL, model TEXT NOT NULL,
                    input_tokens INTEGER NOT NULL, output_tokens INTEGER NOT NULL,
                    total_tokens INTEGER NOT NULL, request_kind TEXT NOT NULL DEFAULT 'agent_loop',
                    created_at TEXT NOT NULL
                );
                CREATE INDEX IF NOT EXISTS idx_llm_usage_chat_created ON llm_usage_logs(chat_id, created_at);
                CREATE INDEX IF NOT EXISTS idx_llm_usage_created ON llm_usage_logs(created_at);
                CREATE TABLE IF NOT EXISTS sleep_runs (
                    id TEXT PRIMARY KEY, agent_id TEXT NOT NULL, status TEXT NOT NULL,
                    trigger_type TEXT NOT NULL, started_at TEXT NOT NULL, finished_at TEXT,
                    source_chats_json TEXT NOT NULL DEFAULT '[]', source_digest_md TEXT,
                    input_tokens INTEGER NOT NULL DEFAULT 0, output_tokens INTEGER NOT NULL DEFAULT 0,
                    total_tokens INTEGER NOT NULL DEFAULT 0, error_message TEXT
                );
                CREATE INDEX IF NOT EXISTS idx_sleep_runs_agent_started ON sleep_runs(agent_id, started_at);
                CREATE INDEX IF NOT EXISTS idx_sleep_runs_agent_status ON sleep_runs(agent_id, status);
                CREATE TABLE IF NOT EXISTS memory_snapshots (
                    id TEXT PRIMARY KEY, run_id TEXT NOT NULL, agent_id TEXT NOT NULL,
                    file TEXT NOT NULL, content_before TEXT NOT NULL, content_after TEXT NOT NULL,
                    created_at TEXT NOT NULL
                );
                CREATE INDEX IF NOT EXISTS idx_memory_snapshots_run_id ON memory_snapshots(run_id);
                CREATE INDEX IF NOT EXISTS idx_memory_snapshots_agent_created ON memory_snapshots(agent_id, created_at);
                CREATE TABLE IF NOT EXISTS pulse_runs (
                    id TEXT PRIMARY KEY, agent_id TEXT NOT NULL, intention_id TEXT NOT NULL,
                    due_key TEXT NOT NULL, chat_id INTEGER, message_id TEXT, status TEXT NOT NULL,
                    started_at TEXT NOT NULL, finished_at TEXT, output_kind TEXT, output_text TEXT,
                    error_message TEXT
                );
                CREATE UNIQUE INDEX IF NOT EXISTS idx_pulse_runs_due ON pulse_runs(agent_id, intention_id, due_key);
                CREATE INDEX IF NOT EXISTS idx_pulse_runs_agent_started ON pulse_runs(agent_id, started_at);
                CREATE INDEX IF NOT EXISTS idx_pulse_runs_chat_id ON pulse_runs(chat_id);
                CREATE TABLE IF NOT EXISTS episode_events (
                    id TEXT PRIMARY KEY, agent_id TEXT NOT NULL, experienced_at TEXT NOT NULL,
                    encoded_at TEXT NOT NULL, kind TEXT NOT NULL, title TEXT NOT NULL,
                    body_md TEXT NOT NULL, ripple_strength INTEGER NOT NULL DEFAULT 3,
                    certainty TEXT NOT NULL DEFAULT 'stated', sleep_run_id TEXT NOT NULL,
                    source_refs_json TEXT, created_at TEXT NOT NULL, updated_at TEXT NOT NULL,
                    CHECK (kind IN ('self', 'relationship', 'world', 'feat', 'anomaly', 'decision', 'insight', 'rhythm')),
                    CHECK (ripple_strength BETWEEN 1 AND 5),
                    CHECK (certainty IN ('stated', 'derived', 'tentative'))
                );
                CREATE INDEX IF NOT EXISTS idx_episode_events_agent_experienced ON episode_events(agent_id, experienced_at);
                CREATE INDEX IF NOT EXISTS idx_episode_events_agent_kind_experienced ON episode_events(agent_id, kind, experienced_at);
                CREATE INDEX IF NOT EXISTS idx_episode_events_agent_ripple_experienced ON episode_events(agent_id, ripple_strength, experienced_at);
                CREATE INDEX IF NOT EXISTS idx_episode_events_sleep_run ON episode_events(sleep_run_id);
                CREATE TABLE IF NOT EXISTS episode_rollups (
                    id TEXT PRIMARY KEY, agent_id TEXT NOT NULL, granularity TEXT NOT NULL,
                    period_key TEXT NOT NULL, period_start TEXT NOT NULL, period_end_exclusive TEXT NOT NULL,
                    summary_md TEXT NOT NULL, max_ripple INTEGER NOT NULL DEFAULT 3,
                    event_count INTEGER NOT NULL DEFAULT 0, generated_run_id TEXT NOT NULL,
                    created_at TEXT NOT NULL, updated_at TEXT NOT NULL,
                    CHECK (granularity IN ('week', 'month')), CHECK (max_ripple BETWEEN 1 AND 5),
                    UNIQUE(agent_id, granularity, period_key)
                );
                CREATE INDEX IF NOT EXISTS idx_episode_rollups_agent_period ON episode_rollups(agent_id, granularity, period_start);
                CREATE INDEX IF NOT EXISTS idx_episode_rollups_agent_ripple ON episode_rollups(agent_id, granularity, max_ripple, period_start);
                CREATE TABLE IF NOT EXISTS sleep_run_steps (
                    sleep_run_id TEXT NOT NULL,
                    step_name TEXT NOT NULL CHECK (step_name IN ('event_extraction', 'episodic_update', 'semantic_update', 'prospective_update')),
                    status TEXT NOT NULL CHECK (status IN ('pending', 'running', 'success', 'failed', 'skipped')),
                    started_at TEXT, finished_at TEXT,
                    input_tokens INTEGER NOT NULL DEFAULT 0, output_tokens INTEGER NOT NULL DEFAULT 0,
                    error_message TEXT, metadata_json TEXT,
                    PRIMARY KEY (sleep_run_id, step_name),
                    FOREIGN KEY (sleep_run_id) REFERENCES sleep_runs(id) ON DELETE CASCADE
                );
                CREATE INDEX IF NOT EXISTS idx_sleep_run_steps_step_status ON sleep_run_steps(step_name, status, started_at);
                CREATE TABLE IF NOT EXISTS sleep_step_checkpoints (
                    agent_id TEXT NOT NULL, step_name TEXT NOT NULL, source_kind TEXT NOT NULL,
                    source_id TEXT NOT NULL, cursor_at TEXT NOT NULL, cursor_id TEXT NOT NULL,
                    updated_at TEXT NOT NULL,
                    PRIMARY KEY (agent_id, step_name, source_kind, source_id),
                    CHECK (step_name IN ('event_extraction', 'semantic_update', 'prospective_update')),
                    CHECK (source_kind IN ('messages', 'episode_events')),
                    CHECK (
                        (step_name IN ('event_extraction', 'prospective_update') AND source_kind = 'messages')
                        OR (step_name = 'semantic_update' AND source_kind = 'episode_events')
                    )
                );",
            )
            .unwrap();

            conn.execute(
                "CREATE TABLE IF NOT EXISTS db_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL)",
                [],
            )
            .unwrap();

            super::set_schema_version(&conn, 8, "test v8 baseline").unwrap();
        }

        super::super::Database::new_unchecked(&db_path).unwrap()
    }

    #[test]
    fn migration_v8_to_v9_rebuilds_memory_snapshots_with_constraints() {
        // Arrange: v8 DB with existing snapshots
        let dir = tempfile::tempdir().expect("tempdir");
        let db = create_v8_db(&dir);
        {
            let conn = db.get_conn().expect("pool");
            conn.execute(
                "INSERT INTO sleep_runs (id, agent_id, status, trigger_type, started_at)
                 VALUES ('run-1', 'agent-a', 'success', 'manual', '2024-01-01T00:00:00Z')",
                [],
            )
            .expect("insert run");
            conn.execute(
                "INSERT INTO memory_snapshots (id, run_id, agent_id, file, content_before, content_after, created_at)
                 VALUES ('snap-1', 'run-1', 'agent-a', 'episodic', 'before', 'after', '2024-01-01T00:00:00Z')",
                [],
            )
            .expect("insert snapshot");
        }

        // Act: run v9 migration
        let conn = db.get_conn().expect("pool");
        super::run_migrations(&conn).expect("migrations");

        // Assert: schema version advanced
        let version: String = conn
            .query_row(
                "SELECT value FROM db_meta WHERE key = 'schema_version'",
                [],
                |row| row.get(0),
            )
            .expect("version");
        assert_eq!(version.parse::<i64>().unwrap(), super::SCHEMA_VERSION);

        // Assert: existing snapshot preserved
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM memory_snapshots", [], |row| {
                row.get(0)
            })
            .expect("count");
        assert_eq!(count, 1, "existing snapshot should be preserved");

        // Assert: UNIQUE constraint rejects duplicate (run_id, file)
        let duplicate = conn.execute(
            "INSERT INTO memory_snapshots (id, run_id, agent_id, file, content_before, content_after, created_at)
             VALUES ('snap-2', 'run-1', 'agent-a', 'episodic', 'b2', 'a2', '2024-01-01T00:00:01Z')",
            [],
        );
        assert!(duplicate.is_err(), "should reject duplicate run_id+file");

        // Assert: CHECK constraint rejects invalid file
        let invalid_file = conn.execute(
            "INSERT INTO memory_snapshots (id, run_id, agent_id, file, content_before, content_after, created_at)
             VALUES ('snap-3', 'run-1', 'agent-a', 'invalid_file', 'b', 'a', '2024-01-01T00:00:00Z')",
            [],
        );
        assert!(invalid_file.is_err(), "should reject invalid file value");

        // Assert: FK constraint rejects non-existent run
        let invalid_run = conn.execute(
            "INSERT INTO memory_snapshots (id, run_id, agent_id, file, content_before, content_after, created_at)
             VALUES ('snap-4', 'nonexistent-run', 'agent-a', 'semantic', 'b', 'a', '2024-01-01T00:00:00Z')",
            [],
        );
        assert!(invalid_run.is_err(), "should reject non-existent run_id");
    }

    // --- v12: durable turn + integer seq/revision -----------------------------

    fn rollback_schema(db: &super::super::Database, to_version: i64, label: &str) {
        let conn = db.get_conn().expect("conn");
        conn.execute(
            "UPDATE db_meta SET value = ?1 WHERE key = 'schema_version'",
            [to_version.to_string()],
        )
        .unwrap_or_else(|e| panic!("rollback {label}: {e}"));
        super::run_migrations(&conn).expect("re-run migrations");
    }

    #[test]
    fn migration_v12_backfills_message_seq_in_timestamp_id_order() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("runtime").join("egopulse.db");
        let db = super::super::Database::new(&db_path).expect("db");
        let chat = db
            .resolve_or_create_chat_id("cli", "cli:v12-seq", None, "private", "default")
            .expect("chat");
        {
            let conn = db.get_conn().expect("conn");
            // Insert out of id/timestamp order to confirm stable (timestamp, id) sorting.
            conn.execute(
                "INSERT INTO messages (id, chat_id, sender_id, content, sender_kind, timestamp, message_kind)
                 VALUES ('m-b', ?1, 'a', 'second', 'assistant', '2024-01-01T00:00:01Z', 'message')",
                rusqlite::params![chat],
            )
            .expect("insert b");
            conn.execute(
                "INSERT INTO messages (id, chat_id, sender_id, content, sender_kind, timestamp, message_kind)
                 VALUES ('m-a', ?1, 'a', 'first', 'user', '2024-01-01T00:00:00Z', 'message')",
                rusqlite::params![chat],
            )
            .expect("insert a");
            conn.execute(
                "INSERT INTO messages (id, chat_id, sender_id, content, sender_kind, timestamp, message_kind)
                 VALUES ('m-c', ?1, 'a', 'third', 'assistant', '2024-01-01T00:00:02Z', 'message')",
                rusqlite::params![chat],
            )
            .expect("insert c");
        }

        // Act
        rollback_schema(&db, 11, "v12");

        // Assert
        let conn = db.get_conn().expect("conn");
        let rows: Vec<(String, i64)> = conn
            .prepare("SELECT id, seq FROM messages WHERE chat_id = ?1 ORDER BY seq")
            .expect("prepare")
            .query_map(rusqlite::params![chat], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .expect("query")
            .collect::<Result<Vec<_>, _>>()
            .expect("collect");
        assert_eq!(
            rows,
            vec![
                ("m-a".to_string(), 1),
                ("m-b".to_string(), 2),
                ("m-c".to_string(), 3)
            ],
            "seq must follow (timestamp, id) order"
        );
    }

    #[test]
    fn migration_v12_assigns_deterministic_seq_for_same_timestamp() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("runtime").join("egopulse.db");
        let db = super::super::Database::new(&db_path).expect("db");
        let chat = db
            .resolve_or_create_chat_id("cli", "cli:v12-same-ts", None, "private", "default")
            .expect("chat");
        let timestamp = "2024-01-01T00:00:00Z";
        {
            let conn = db.get_conn().expect("conn");
            for (id, content) in [("z-id", "z"), ("a-id", "a"), ("m-id", "m")] {
                conn.execute(
                    "INSERT INTO messages (id, chat_id, sender_id, content, sender_kind, timestamp, message_kind)
                     VALUES (?1, ?2, 'a', ?3, 'user', ?4, 'message')",
                    rusqlite::params![id, chat, content, timestamp],
                )
                .expect("insert");
            }
        }

        // Act
        rollback_schema(&db, 11, "v12");

        // Assert: identical timestamps resolve by id ascending.
        let conn = db.get_conn().expect("conn");
        let rows: Vec<(String, i64)> = conn
            .prepare("SELECT id, seq FROM messages WHERE chat_id = ?1 ORDER BY seq")
            .expect("prepare")
            .query_map(rusqlite::params![chat], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .expect("query")
            .collect::<Result<Vec<_>, _>>()
            .expect("collect");
        assert_eq!(
            rows,
            vec![
                ("a-id".to_string(), 1),
                ("m-id".to_string(), 2),
                ("z-id".to_string(), 3)
            ],
        );
    }

    #[test]
    fn migration_v12_backfills_chats_revision_and_next_message_seq() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("runtime").join("egopulse.db");
        let db = super::super::Database::new(&db_path).expect("db");
        let chat_with_msgs = db
            .resolve_or_create_chat_id("cli", "cli:v12-chats", None, "private", "default")
            .expect("chat");
        let empty_chat = db
            .resolve_or_create_chat_id("cli", "cli:v12-empty", None, "private", "default")
            .expect("empty chat");
        {
            let conn = db.get_conn().expect("conn");
            for i in 0..3 {
                conn.execute(
                    "INSERT INTO messages (id, chat_id, sender_id, content, sender_kind, timestamp, message_kind)
                     VALUES (?1, ?2, 'a', ?3, 'user', ?4, 'message')",
                    rusqlite::params![
                        format!("m-{i}"),
                        chat_with_msgs,
                        format!("c{i}"),
                        format!("2024-01-01T00:00:0{i}Z")
                    ],
                )
                .expect("insert");
            }
        }

        // Act
        rollback_schema(&db, 11, "v12");

        // Assert
        let conn = db.get_conn().expect("conn");
        let (revision, next_seq): (i64, i64) = conn
            .query_row(
                "SELECT revision, next_message_seq FROM chats WHERE chat_id = ?1",
                rusqlite::params![chat_with_msgs],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("row");
        assert_eq!(revision, 3, "revision = message count");
        assert_eq!(next_seq, 4, "next_message_seq = max seq + 1");

        let (revision, next_seq): (i64, i64) = conn
            .query_row(
                "SELECT revision, next_message_seq FROM chats WHERE chat_id = ?1",
                rusqlite::params![empty_chat],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("row");
        assert_eq!(revision, 0, "empty chat has 0 changes");
        assert_eq!(next_seq, 1, "empty chat starts at seq 1");
    }

    #[test]
    fn migration_v12_backfills_sessions_snapshot_through_seq() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("runtime").join("egopulse.db");
        let db = super::super::Database::new(&db_path).expect("db");
        let chat = db
            .resolve_or_create_chat_id("cli", "cli:v12-sess", None, "private", "default")
            .expect("chat");
        let llm_context = r#"[{"role":"user","content":"hi"}]"#;
        db.save_session(chat, llm_context).expect("session");
        {
            let conn = db.get_conn().expect("conn");
            conn.execute(
                "INSERT INTO messages (id, chat_id, sender_id, content, sender_kind, timestamp, message_kind)
                 VALUES ('m-1', ?1, 'a', 'hi', 'user', '2024-01-01T00:00:00Z', 'message')",
                rusqlite::params![chat],
            )
            .expect("insert");
        }

        // Act
        rollback_schema(&db, 11, "v12");

        // Assert
        let conn = db.get_conn().expect("conn");
        let (snapshot_through, json): (i64, String) = conn
            .query_row(
                "SELECT snapshot_through_seq, messages_json FROM sessions WHERE chat_id = ?1",
                rusqlite::params![chat],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("row");
        assert_eq!(snapshot_through, 1, "snapshot covers the legacy message");
        assert_eq!(json, llm_context, "LLM context must be preserved verbatim");
    }

    #[test]
    fn migration_v12_backfills_tool_calls_state_from_output_presence() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("runtime").join("egopulse.db");
        let db = super::super::Database::new(&db_path).expect("db");
        let chat = db
            .resolve_or_create_chat_id("cli", "cli:v12-tools", None, "private", "default")
            .expect("chat");
        {
            let conn = db.get_conn().expect("conn");
            conn.execute(
                "INSERT INTO tool_calls (id, chat_id, message_id, tool_name, tool_input, tool_output, timestamp)
                 VALUES ('tc-done', ?1, 'm-1', 'shell', '{}', '{\"ok\":true}', '2024-01-01T00:00:00Z')",
                rusqlite::params![chat],
            )
            .expect("insert done");
            conn.execute(
                "INSERT INTO tool_calls (id, chat_id, message_id, tool_name, tool_input, tool_output, timestamp)
                 VALUES ('tc-pending', ?1, 'm-2', 'shell', '{}', NULL, '2024-01-01T00:00:01Z')",
                rusqlite::params![chat],
            )
            .expect("insert pending");
        }

        // Act
        rollback_schema(&db, 11, "v12");

        // Assert
        let conn = db.get_conn().expect("conn");
        let done: String = conn
            .query_row(
                "SELECT state FROM tool_calls WHERE id = 'tc-done'",
                [],
                |row| row.get(0),
            )
            .expect("row");
        let pending: String = conn
            .query_row(
                "SELECT state FROM tool_calls WHERE id = 'tc-pending'",
                [],
                |row| row.get(0),
            )
            .expect("row");
        assert_eq!(done, "succeeded", "output present => succeeded");
        assert_eq!(
            pending, "uncertain",
            "no output => uncertain, never auto-retried"
        );
    }

    #[test]
    fn migration_v12_is_idempotent_on_rerun() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("runtime").join("egopulse.db");
        let db = super::super::Database::new(&db_path).expect("db");
        let chat = db
            .resolve_or_create_chat_id("cli", "cli:v12-idem", None, "private", "default")
            .expect("chat");
        {
            let conn = db.get_conn().expect("conn");
            conn.execute(
                "INSERT INTO messages (id, chat_id, sender_id, content, sender_kind, timestamp, message_kind)
                 VALUES ('m-1', ?1, 'a', 'only', 'user', '2024-01-01T00:00:00Z', 'message')",
                rusqlite::params![chat],
            )
            .expect("insert");
        }

        // Act: roll back and re-run the v12 block twice.
        rollback_schema(&db, 11, "v12 first");
        rollback_schema(&db, 11, "v12 second");

        // Assert: no duplicated seq, no duplicate turn_runs, schema version is current.
        let conn = db.get_conn().expect("conn");
        let seqs: Vec<i64> = conn
            .prepare("SELECT seq FROM messages WHERE chat_id = ?1 ORDER BY seq")
            .expect("prepare")
            .query_map(rusqlite::params![chat], |row| row.get(0))
            .expect("query")
            .collect::<Result<Vec<_>, _>>()
            .expect("collect");
        assert_eq!(seqs, vec![1], "no duplicate events");
        let turn_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM turn_runs", [], |row| row.get(0))
            .expect("count");
        assert_eq!(turn_count, 0, "backfill creates no turns");
        let version: String = conn
            .query_row(
                "SELECT value FROM db_meta WHERE key = 'schema_version'",
                [],
                |row| row.get(0),
            )
            .expect("version");
        assert_eq!(version.parse::<i64>().unwrap(), super::SCHEMA_VERSION);
    }

    #[test]
    fn migration_v12_preserves_web_history_order_and_content() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("runtime").join("egopulse.db");
        let db = super::super::Database::new(&db_path).expect("db");
        let chat = db
            .resolve_or_create_chat_id("cli", "cli:v12-preserve", None, "private", "default")
            .expect("chat");
        {
            let conn = db.get_conn().expect("conn");
            for (id, content, ts) in [
                ("m-1", "hello", "2024-01-01T00:00:00Z"),
                ("m-2", "world", "2024-01-01T00:00:01Z"),
            ] {
                conn.execute(
                    "INSERT INTO messages (id, chat_id, sender_id, content, sender_kind, timestamp, message_kind)
                     VALUES (?1, ?2, 'a', ?3, 'user', ?4, 'message')",
                    rusqlite::params![id, chat, content, ts],
                )
                .expect("insert");
            }
        }
        let before = db.get_all_messages(chat).expect("messages before");

        // Act
        rollback_schema(&db, 11, "v12");

        // Assert: messages table content/order unchanged (only seq added).
        let after = db.get_all_messages(chat).expect("messages after");
        assert_eq!(
            before
                .iter()
                .map(|m| (&m.id, &m.content))
                .collect::<Vec<_>>(),
            after
                .iter()
                .map(|m| (&m.id, &m.content))
                .collect::<Vec<_>>(),
            "web history must be unchanged by migration"
        );
    }

    #[test]
    fn secret_migration_v3_backfills_conversation_extensions() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("runtime").join("secret.db");
        let db = super::super::Database::new_secret(&db_path).expect("secret db");
        let chat = db
            .resolve_or_create_chat_id("discord", "discord:9:agent:x", None, "private", "x")
            .expect("chat");
        {
            let conn = db.get_conn().expect("conn");
            conn.execute(
                "INSERT INTO messages (id, chat_id, sender_id, content, sender_kind, timestamp, message_kind)
                 VALUES ('s-1', ?1, 'u', 'secret hello', 'user', '2024-01-01T00:00:00Z', 'message')",
                rusqlite::params![chat],
            )
            .expect("insert");
            conn.execute(
                "UPDATE db_meta SET value = '2' WHERE key = 'schema_version'",
                [],
            )
            .expect("rollback to v2");
            super::run_secret_migrations(&conn).expect("re-run secret migrations");
        }

        // Assert
        let conn = db.get_conn().expect("conn");
        let seq: i64 = conn
            .query_row("SELECT seq FROM messages WHERE id = 's-1'", [], |row| {
                row.get(0)
            })
            .expect("seq");
        assert_eq!(seq, 1);
        let (revision, next_seq): (i64, i64) = conn
            .query_row(
                "SELECT revision, next_message_seq FROM chats WHERE chat_id = ?1",
                rusqlite::params![chat],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("chats");
        assert_eq!(revision, 1);
        assert_eq!(next_seq, 2);
        // turn_runs exists on secret DB too so secret conversations get the same lifecycle.
        let turn_table: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='turn_runs'",
                [],
                |row| row.get(0),
            )
            .expect("turn_runs presence");
        assert_eq!(turn_table, 1);
    }
}
