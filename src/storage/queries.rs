use std::str::FromStr;

use rusqlite::{OptionalExtension, params};

use crate::error::StorageError;

use super::{
    AgentSessionInfo, ChatInfo, Database, LlmUsageLogEntry, MemoryFile, MemorySnapshot,
    SessionSnapshot, SessionSummary, SleepRun, SleepRunStatus, SleepRunTrigger,
    StoredMessage, ToolCall,
};

fn row_to_stored_message(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredMessage> {
    Ok(StoredMessage {
        id: row.get(0)?,
        chat_id: row.get(1)?,
        sender_name: row.get(2)?,
        content: row.get(3)?,
        is_from_bot: row.get::<_, i32>(4)? != 0,
        timestamp: row.get(5)?,
    })
}

fn row_to_sleep_run(row: &rusqlite::Row<'_>) -> rusqlite::Result<SleepRun> {
    let status_str: String = row.get(2)?;
    let status = SleepRunStatus::from_str(&status_str).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(
            2,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, e)),
        )
    })?;

    let trigger_str: String = row.get(3)?;
    let trigger = SleepRunTrigger::from_str(&trigger_str).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(
            3,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, e)),
        )
    })?;

    Ok(SleepRun {
        id: row.get(0)?,
        agent_id: row.get(1)?,
        status,
        trigger,
        started_at: row.get(4)?,
        finished_at: row.get(5)?,
        source_chats_json: row.get(6)?,
        source_digest_md: row.get(7)?,
        input_tokens: row.get(8)?,
        output_tokens: row.get(9)?,
        total_tokens: row.get(10)?,
        error_message: row.get(11)?,
    })
}

fn row_to_memory_snapshot(row: &rusqlite::Row<'_>) -> rusqlite::Result<MemorySnapshot> {
    let file_str: String = row.get(3)?;
    let file = MemoryFile::from_str(&file_str).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(
            3,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, e)),
        )
    })?;

    Ok(MemorySnapshot {
        id: row.get(0)?,
        run_id: row.get(1)?,
        agent_id: row.get(2)?,
        file,
        content_before: row.get(4)?,
        content_after: row.get(5)?,
        created_at: row.get(6)?,
    })
}

// ---------------------------------------------------------------------------
// Sleep runs
// ---------------------------------------------------------------------------

impl Database {
    pub(crate) fn resolve_chat_id(
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

    pub(crate) fn get_chat_by_id(&self, chat_id: i64) -> Result<Option<ChatInfo>, StorageError> {
        let conn = self.lock_conn()?;
        match conn.query_row(
            "SELECT channel, external_chat_id, chat_type, agent_id FROM chats WHERE chat_id = ?1 LIMIT 1",
            params![chat_id],
            |row| {
                Ok(ChatInfo {
                    chat_id,
                    channel: row.get(0)?,
                    external_chat_id: row.get(1)?,
                    chat_type: row.get(2)?,
                    agent_id: row.get(3)?,
                })
            },
        ) {
            Ok(info) => Ok(Some(info)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    pub(crate) fn resolve_or_create_chat_id(
        &self,
        channel: &str,
        external_chat_id: &str,
        chat_title: Option<&str>,
        chat_type: &str,
        agent_id: &str,
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
                         last_message_time = ?4,
                         agent_id = COALESCE(agent_id, ?5)
                     WHERE chat_id = ?1",
                    params![chat_id, chat_title, chat_type, now, agent_id],
                )?;
                return Ok(chat_id);
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => {}
            Err(error) => return Err(error.into()),
        }

        conn.execute(
            "INSERT INTO chats(chat_title, chat_type, last_message_time, channel, external_chat_id, agent_id)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(channel, external_chat_id) DO UPDATE SET
                chat_title = COALESCE(excluded.chat_title, chats.chat_title),
                chat_type = excluded.chat_type,
                last_message_time = excluded.last_message_time,
                agent_id = COALESCE(chats.agent_id, excluded.agent_id)",
            params![chat_title, chat_type, now, channel, external_chat_id, agent_id],
        )?;
        conn.query_row(
            "SELECT chat_id FROM chats WHERE channel = ?1 AND external_chat_id = ?2 LIMIT 1",
            params![channel, external_chat_id],
            |row| row.get::<_, i64>(0),
        )
        .map_err(Into::into)
    }

    pub(crate) fn list_sessions(&self) -> Result<Vec<SessionSummary>, StorageError> {
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
                ) AS last_message_preview,
                c.agent_id
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
                agent_id: row.get(6)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(Into::into)
    }
}

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

// ---------------------------------------------------------------------------
// Messages
// ---------------------------------------------------------------------------

impl Database {
    pub(crate) fn get_recent_messages(
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
            .query_map(params![chat_id, limit as i64], row_to_stored_message)?
            .collect::<Result<Vec<_>, _>>()?;
        messages.reverse();
        Ok(messages)
    }

    pub(crate) fn get_all_messages(
        &self,
        chat_id: i64,
    ) -> Result<Vec<StoredMessage>, StorageError> {
        let conn = self.lock_conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, chat_id, sender_name, content, is_from_bot, timestamp
             FROM messages
             WHERE chat_id = ?1
             ORDER BY timestamp ASC",
        )?;
        stmt.query_map(params![chat_id], row_to_stored_message)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(Into::into)
    }
}

// ---------------------------------------------------------------------------
// Sessions
// ---------------------------------------------------------------------------

impl Database {
    pub(crate) fn save_session(
        &self,
        chat_id: i64,
        messages_json: &str,
    ) -> Result<(), StorageError> {
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

    pub(crate) fn clear_session(&self, chat_id: i64) -> Result<(), StorageError> {
        let mut conn = self.lock_conn()?;
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM sessions WHERE chat_id = ?1", params![chat_id])?;
        tx.execute("DELETE FROM messages WHERE chat_id = ?1", params![chat_id])?;
        tx.commit()?;
        Ok(())
    }

    pub(crate) fn store_message_with_session(
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

    pub(crate) fn load_session_snapshot(
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
                .query_map(params![chat_id, limit as i64], row_to_stored_message)?
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
}

// ---------------------------------------------------------------------------
// Tool calls & LLM usage
// ---------------------------------------------------------------------------

impl Database {
    pub(crate) fn store_tool_call(&self, tool_call: &ToolCall) -> Result<(), StorageError> {
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

    pub(crate) fn update_tool_call_output_for_message(
        &self,
        chat_id: i64,
        message_id: &str,
        id: &str,
        output: &str,
    ) -> Result<(), StorageError> {
        let conn = self.lock_conn()?;
        let rows_updated = conn.execute(
            "UPDATE tool_calls
             SET tool_output = ?1
             WHERE chat_id = ?2 AND message_id = ?3 AND id = ?4",
            params![output, chat_id, message_id, id],
        )?;
        if rows_updated == 0 {
            return Err(StorageError::NotFound(format!(
                "tool_call:{chat_id}:{message_id}:{id}"
            )));
        }
        Ok(())
    }

    pub(crate) fn log_llm_usage(&self, entry: &LlmUsageLogEntry<'_>) -> Result<i64, StorageError> {
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
}

#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "Phase 2 DB audit queries; exercised by unit tests below, wired into runtime in Phase 3+"
    )
)]
impl Database {
    pub(crate) fn create_sleep_run(
        &self,
        agent_id: &str,
        trigger: SleepRunTrigger,
    ) -> Result<String, StorageError> {
        let conn = self.lock_conn()?;
        let id = uuid::Uuid::new_v4().to_string();
        let status = SleepRunStatus::Running.to_string();
        let started_at = chrono::Utc::now().to_rfc3339();

        conn.execute(
            "INSERT INTO sleep_runs
                 (id, agent_id, status, trigger_type, started_at, finished_at,
                  source_chats_json, source_digest_md,
                  input_tokens, output_tokens, total_tokens, error_message)
             VALUES (?1, ?2, ?3, ?4, ?5, NULL, '[]', NULL, 0, 0, 0, NULL)",
            params![id, agent_id, status, trigger.to_string(), started_at],
        )?;
        Ok(id)
    }

    pub(crate) fn update_sleep_run_success(
        &self,
        id: &str,
        source_chats_json: &str,
        source_digest_md: Option<&str>,
        input_tokens: i64,
        output_tokens: i64,
    ) -> Result<(), StorageError> {
        let conn = self.lock_conn()?;
        let finished_at = chrono::Utc::now().to_rfc3339();
        let total_tokens = input_tokens.saturating_add(output_tokens);
        let status = SleepRunStatus::Success.to_string();
        let running = SleepRunStatus::Running.to_string();

        let changed = conn.execute(
            "UPDATE sleep_runs
             SET status = ?1, finished_at = ?2, source_chats_json = ?3,
                 source_digest_md = ?4,
                 input_tokens = ?5, output_tokens = ?6, total_tokens = ?7
             WHERE id = ?8 AND status = ?9",
            params![
                status,
                finished_at,
                source_chats_json,
                source_digest_md,
                input_tokens,
                output_tokens,
                total_tokens,
                id,
                running,
            ],
        )?;
        if changed == 0 {
            return Err(StorageError::Conflict(format!(
                "sleep_run:{id} is not running"
            )));
        }
        Ok(())
    }

    pub(crate) fn update_sleep_run_failed(
        &self,
        id: &str,
        error_message: &str,
    ) -> Result<(), StorageError> {
        let conn = self.lock_conn()?;
        let finished_at = chrono::Utc::now().to_rfc3339();
        let status = SleepRunStatus::Failed.to_string();
        let running = SleepRunStatus::Running.to_string();

        let changed = conn.execute(
            "UPDATE sleep_runs SET status = ?1, finished_at = ?2, error_message = ?3
             WHERE id = ?4 AND status = ?5",
            params![status, finished_at, error_message, id, running],
        )?;
        if changed == 0 {
            return Err(StorageError::Conflict(format!(
                "sleep_run:{id} is not running"
            )));
        }
        Ok(())
    }

    pub(crate) fn update_sleep_run_skipped(&self, id: &str) -> Result<(), StorageError> {
        let conn = self.lock_conn()?;
        let finished_at = chrono::Utc::now().to_rfc3339();
        let status = SleepRunStatus::Skipped.to_string();
        let running = SleepRunStatus::Running.to_string();

        let changed = conn.execute(
            "UPDATE sleep_runs SET status = ?1, finished_at = ?2 WHERE id = ?3 AND status = ?4",
            params![status, finished_at, id, running],
        )?;
        if changed == 0 {
            return Err(StorageError::Conflict(format!(
                "sleep_run:{id} is not running"
            )));
        }
        Ok(())
    }

    pub(crate) fn get_sleep_run(&self, id: &str) -> Result<Option<SleepRun>, StorageError> {
        let conn = self.lock_conn()?;
        conn.query_row(
            "SELECT id, agent_id, status, trigger_type, started_at, finished_at,
                    source_chats_json, source_digest_md,
                    input_tokens, output_tokens, total_tokens, error_message
             FROM sleep_runs WHERE id = ?1",
            params![id],
            row_to_sleep_run,
        )
        .optional()
        .map_err(Into::into)
    }

    pub(crate) fn list_sleep_runs(
        &self,
        agent_id: &str,
        limit: i64,
    ) -> Result<Vec<SleepRun>, StorageError> {
        let conn = self.lock_conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, agent_id, status, trigger_type, started_at, finished_at,
                    source_chats_json, source_digest_md,
                    input_tokens, output_tokens, total_tokens, error_message
             FROM sleep_runs
             WHERE agent_id = ?1
             ORDER BY started_at DESC, rowid DESC
             LIMIT ?2",
        )?;
        stmt.query_map(params![agent_id, limit], row_to_sleep_run)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    pub(crate) fn get_latest_successful_run(
        &self,
        agent_id: &str,
    ) -> Result<Option<SleepRun>, StorageError> {
        let conn = self.lock_conn()?;
        conn.query_row(
            "SELECT id, agent_id, status, trigger_type, started_at, finished_at,
                    source_chats_json, source_digest_md,
                    input_tokens, output_tokens, total_tokens, error_message
             FROM sleep_runs
             WHERE agent_id = ?1 AND status = 'success'
             ORDER BY finished_at DESC
             LIMIT 1",
            params![agent_id],
            row_to_sleep_run,
        )
        .optional()
        .map_err(Into::into)
    }

    pub(crate) fn count_agent_messages_since(
        &self,
        agent_id: &str,
        since: Option<&str>,
    ) -> Result<i64, StorageError> {
        let conn = self.lock_conn()?;
        if let Some(cutoff) = since {
            conn.query_row(
                "SELECT COUNT(*)
                 FROM messages m
                 JOIN chats c ON m.chat_id = c.chat_id
                 WHERE c.agent_id = ?1 AND m.timestamp > ?2",
                params![agent_id, cutoff],
                |row| row.get(0),
            )
            .map_err(Into::into)
        } else {
            conn.query_row(
                "SELECT COUNT(*)
                 FROM messages m
                 JOIN chats c ON m.chat_id = c.chat_id
                 WHERE c.agent_id = ?1",
                params![agent_id],
                |row| row.get(0),
            )
            .map_err(Into::into)
        }
    }

    pub(crate) fn get_agent_sessions_since(
        &self,
        agent_id: &str,
        since: Option<&str>,
        limit: usize,
    ) -> Result<Vec<AgentSessionInfo>, StorageError> {
        let conn = self.lock_conn()?;
        if let Some(cutoff) = since {
            let mut stmt = conn.prepare(
                "SELECT
                    c.chat_id,
                    c.channel,
                    c.external_chat_id,
                    s.updated_at,
                    (SELECT COUNT(*) FROM messages WHERE chat_id = c.chat_id) AS message_count,
                    LENGTH(COALESCE(s.messages_json, '')) / 3 AS estimated_tokens
                 FROM chats c
                 JOIN sessions s ON c.chat_id = s.chat_id
                 WHERE c.agent_id = ?1 AND s.updated_at > ?2
                 ORDER BY s.updated_at DESC
                 LIMIT ?3",
            )?;
            stmt.query_map(params![agent_id, cutoff, limit as i64], |row| {
                Ok(AgentSessionInfo {
                    chat_id: row.get(0)?,
                    channel: row.get(1)?,
                    external_chat_id: row.get(2)?,
                    updated_at: row.get(3)?,
                    message_count: row.get(4)?,
                    estimated_tokens: row.get(5)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()
            .map_err(Into::into)
        } else {
            let mut stmt = conn.prepare(
                "SELECT
                    c.chat_id,
                    c.channel,
                    c.external_chat_id,
                    s.updated_at,
                    (SELECT COUNT(*) FROM messages WHERE chat_id = c.chat_id) AS message_count,
                    LENGTH(COALESCE(s.messages_json, '')) / 3 AS estimated_tokens
                 FROM chats c
                 JOIN sessions s ON c.chat_id = s.chat_id
                 WHERE c.agent_id = ?1
                 ORDER BY s.updated_at DESC
                 LIMIT ?2",
            )?;
            stmt.query_map(params![agent_id, limit as i64], |row| {
                Ok(AgentSessionInfo {
                    chat_id: row.get(0)?,
                    channel: row.get(1)?,
                    external_chat_id: row.get(2)?,
                    updated_at: row.get(3)?,
                    message_count: row.get(4)?,
                    estimated_tokens: row.get(5)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()
            .map_err(Into::into)
        }
    }

    // ---------------------------------------------------------------------------
    // Memory snapshots
    // ---------------------------------------------------------------------------

    pub(crate) fn create_memory_snapshot(
        &self,
        run_id: &str,
        agent_id: &str,
        file: MemoryFile,
        content_before: &str,
        content_after: &str,
    ) -> Result<String, StorageError> {
        let conn = self.lock_conn()?;
        let id = uuid::Uuid::new_v4().to_string();
        let created_at = chrono::Utc::now().to_rfc3339();

        let changed = conn.execute(
            "INSERT INTO memory_snapshots (id, run_id, agent_id, file, content_before, content_after, created_at)
             SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7
             WHERE EXISTS (SELECT 1 FROM sleep_runs WHERE id = ?2 AND agent_id = ?3)",
            params![
                id,
                run_id,
                agent_id,
                file.to_string(),
                content_before,
                content_after,
                created_at,
            ],
        )?;
        if changed == 0 {
            return Err(StorageError::NotFound(format!("sleep_run:{run_id}")));
        }
        Ok(id)
    }

    pub(crate) fn get_snapshots_for_run(
        &self,
        run_id: &str,
    ) -> Result<Vec<MemorySnapshot>, StorageError> {
        let conn = self.lock_conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, run_id, agent_id, file, content_before, content_after, created_at
             FROM memory_snapshots
             WHERE run_id = ?1
             ORDER BY created_at ASC",
        )?;
        stmt.query_map(params![run_id], row_to_memory_snapshot)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    pub(crate) fn get_snapshots_for_agent(
        &self,
        agent_id: &str,
        limit: i64,
    ) -> Result<Vec<MemorySnapshot>, StorageError> {
        let conn = self.lock_conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, run_id, agent_id, file, content_before, content_after, created_at
             FROM memory_snapshots
             WHERE agent_id = ?1
             ORDER BY created_at DESC
             LIMIT ?2",
        )?;
        stmt.query_map(params![agent_id, limit], row_to_memory_snapshot)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    pub(crate) fn get_latest_snapshot_for_file(
        &self,
        agent_id: &str,
        file: MemoryFile,
    ) -> Result<Option<MemorySnapshot>, StorageError> {
        let conn = self.lock_conn()?;
        conn.query_row(
            "SELECT id, run_id, agent_id, file, content_before, content_after, created_at
             FROM memory_snapshots
             WHERE agent_id = ?1 AND file = ?2
             ORDER BY created_at DESC
             LIMIT 1",
            params![agent_id, file.to_string()],
            row_to_memory_snapshot,
        )
        .optional()
        .map_err(Into::into)
    }
}

#[cfg(test)]
impl Database {
    pub(crate) fn get_tool_calls_for_chat(
        &self,
        chat_id: i64,
    ) -> Result<Vec<ToolCall>, StorageError> {
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

    pub(crate) fn get_llm_usage_summary(
        &self,
        chat_id: Option<i64>,
        _since: Option<&str>,
        _request_kind: Option<&str>,
    ) -> Result<(i64, i64, i64, i64), StorageError> {
        let conn = self.lock_conn()?;
        let mut sql = String::from(
            "SELECT COUNT(*), COALESCE(SUM(input_tokens), 0), COALESCE(SUM(output_tokens), 0), COALESCE(SUM(total_tokens), 0)
             FROM llm_usage_logs WHERE 1=1",
        );
        if let Some(cid) = chat_id {
            sql.push_str(&format!(" AND chat_id = {cid}"));
        }
        let result = conn.query_row(&sql, [], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })?;
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use crate::error::StorageError;

    use super::*;

    fn test_db() -> (Database, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("runtime").join("egopulse.db");
        let db = Database::new(&db_path).expect("db");
        (db, dir)
    }

    fn store_msg(db: &Database, id: &str, chat_id: i64, content: &str, ts: &str) {
        let conn = db.conn.lock().expect("lock");
        conn.execute(
            "INSERT OR REPLACE INTO messages (id, chat_id, sender_name, content, is_from_bot, timestamp)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![id, chat_id, "alice", content, 0, ts],
        )
        .expect("store message");
    }

    #[test]
    fn message_full_lifecycle() {
        let (db, _dir) = test_db();

        for index in 0..5 {
            store_msg(
                &db,
                &format!("chat1_msg{index}"),
                100,
                &format!("chat1 message {index}"),
                &format!("2024-01-01T00:00:{index:02}Z"),
            );
        }

        for index in 0..3 {
            store_msg(
                &db,
                &format!("chat2_msg{index}"),
                200,
                &format!("chat2 message {index}"),
                &format!("2024-01-01T00:00:{index:02}Z"),
            );
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

        assert!(
            db.load_session_snapshot(100, 10)
                .expect("missing session")
                .messages_json
                .is_none()
        );

        let json1 = r#"[{"role":"user","content":"hello"}]"#;
        db.save_session(100, json1).expect("save session");

        let snapshot = db.load_session_snapshot(100, 10).expect("load session");
        assert_eq!(snapshot.messages_json.as_deref(), Some(json1));
        assert!(snapshot.updated_at.is_some());
        let first_updated_at = snapshot.updated_at.unwrap();
        assert!(!first_updated_at.is_empty());

        std::thread::sleep(std::time::Duration::from_millis(10));

        let json2 = r#"[{"role":"user","content":"hello"},{"role":"assistant","content":"hi"}]"#;
        db.save_session(100, json2).expect("update session");

        let snapshot = db
            .load_session_snapshot(100, 10)
            .expect("load updated session");
        assert_eq!(snapshot.messages_json.as_deref(), Some(json2));
        assert!(snapshot.updated_at.unwrap() >= first_updated_at);
        assert!(
            db.load_session_snapshot(200, 10)
                .expect("other chat")
                .messages_json
                .is_none()
        );
    }

    #[test]
    fn clear_session_deletes_snapshots_and_messages() {
        let (db, _dir) = test_db();
        let chat_id = 100;

        db.save_session(chat_id, r#"[{"role":"user","content":"hello"}]"#)
            .expect("save session");
        store_msg(&db, "msg-1", chat_id, "hello", "2024-01-01T00:00:00Z");
        store_msg(&db, "msg-2", chat_id, "hi", "2024-01-01T00:00:01Z");

        db.clear_session(chat_id).expect("clear session");

        assert!(
            db.load_session_snapshot(chat_id, 10)
                .expect("load session")
                .messages_json
                .is_none()
        );
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
            .resolve_or_create_chat_id("cli", "cli:local-dev", Some("local-dev"), "cli", "default")
            .expect("create chat");
        let second = db
            .resolve_or_create_chat_id("cli", "cli:local-dev", Some("renamed"), "cli", "default")
            .expect("reuse chat");

        assert_eq!(first, second);
        assert!(first > 0);
    }

    #[test]
    fn list_sessions_prefers_logical_session_name() {
        let (db, _dir) = test_db();

        let chat_id = db
            .resolve_or_create_chat_id("cli", "cli:demo", Some("demo"), "cli", "default")
            .expect("create chat");
        store_msg(&db, "msg-1", chat_id, "hello", "2024-01-01T00:00:00Z");
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
                "default",
            )
            .expect("reopen chat");
        assert_eq!(reopened_chat_id, chat_id);
    }

    #[test]
    fn update_tool_call_output_fails_when_tool_call_is_missing() {
        let (db, _dir) = test_db();
        let chat_id = db
            .resolve_or_create_chat_id("web", "web:message-1", Some("message-1"), "web", "default")
            .expect("create chat");

        let error = db
            .update_tool_call_output_for_message(
                chat_id,
                "message-1",
                "missing-tool-call",
                "output",
            )
            .expect_err("missing tool call should fail");

        assert!(matches!(error, StorageError::NotFound(_)));
    }

    #[test]
    fn update_tool_call_output_updates_existing_record() {
        let (db, _dir) = test_db();
        let chat_id = db
            .resolve_or_create_chat_id("web", "web:message-1", Some("message-1"), "web", "default")
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
        db.update_tool_call_output_for_message(chat_id, "message-1", "tool-1", "done")
            .expect("update tool call");

        let conn = db.conn.lock().expect("lock");
        let output: String = conn
            .query_row(
                "SELECT tool_output FROM tool_calls WHERE chat_id = ?1 AND message_id = ?2 AND id = ?3",
                rusqlite::params![chat_id, "message-1", "tool-1"],
                |row| row.get(0),
            )
            .expect("query");
        assert_eq!(output, "done");
    }

    #[test]
    fn tool_call_ids_are_scoped_by_message() {
        let (db, _dir) = test_db();
        let chat_id = db
            .resolve_or_create_chat_id("web", "web:message-1", Some("message-1"), "web", "default")
            .expect("create chat");
        let first = ToolCall {
            id: "tool-1".to_string(),
            chat_id,
            message_id: "message-1".to_string(),
            tool_name: "fetch".to_string(),
            tool_input: "{}".to_string(),
            tool_output: None,
            timestamp: "2024-01-01T00:00:00Z".to_string(),
        };
        let second = ToolCall {
            message_id: "message-2".to_string(),
            timestamp: "2024-01-01T00:00:01Z".to_string(),
            ..first.clone()
        };

        db.store_tool_call(&first).expect("store first tool call");
        db.store_tool_call(&second)
            .expect("store second tool call with duplicate provider id");
        db.update_tool_call_output_for_message(chat_id, "message-2", "tool-1", "done")
            .expect("update scoped output");

        let conn = db.conn.lock().expect("lock");
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM tool_calls WHERE chat_id = ?1",
                rusqlite::params![chat_id],
                |row| row.get(0),
            )
            .expect("count");
        assert_eq!(count, 2);
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
        assert!(created_at.contains('T'));
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
    fn resolve_or_create_chat_id_sets_agent_id() {
        let (db, _dir) = test_db();

        db.resolve_or_create_chat_id("cli", "cli:mybot", Some("mybot"), "cli", "mybot")
            .expect("create chat");

        let info = db
            .get_chat_by_id(
                db.resolve_or_create_chat_id("cli", "cli:mybot", Some("mybot"), "cli", "mybot")
                    .expect("chat id"),
            )
            .expect("chat info")
            .expect("chat should exist");

        assert_eq!(info.agent_id, "mybot");
    }

    #[test]
    fn resolve_or_create_chat_id_preserves_agent_id_on_update() {
        let (db, _dir) = test_db();

        let chat_id = db
            .resolve_or_create_chat_id(
                "cli",
                "cli:persist-agent",
                Some("persist-agent"),
                "cli",
                "agent_a",
            )
            .expect("create with agent_a");

        let second_id = db
            .resolve_or_create_chat_id(
                "cli",
                "cli:persist-agent",
                Some("persist-agent"),
                "cli",
                "agent_b",
            )
            .expect("reuse chat");

        assert_eq!(second_id, chat_id);

        let info = db
            .get_chat_by_id(chat_id)
            .expect("chat info")
            .expect("chat should exist");

        assert_eq!(info.agent_id, "agent_a");
    }

    #[test]
    fn get_chat_by_id_returns_agent_id() {
        let (db, _dir) = test_db();

        let chat_id = db
            .resolve_or_create_chat_id(
                "web",
                "web:agent-test",
                Some("agent-test"),
                "web",
                "custom-agent",
            )
            .expect("create chat");

        let info = db
            .get_chat_by_id(chat_id)
            .expect("get chat")
            .expect("chat should exist");

        assert_eq!(info.agent_id, "custom-agent");
    }

    #[test]
    fn list_sessions_includes_agent_id() {
        let (db, _dir) = test_db();

        db.resolve_or_create_chat_id(
            "cli",
            "cli:session-agent",
            Some("session-agent"),
            "cli",
            "list-agent",
        )
        .expect("create chat");
        store_msg(&db, "msg-1", 1, "hello", "2024-01-01T00:00:00Z");

        let sessions = db.list_sessions().expect("list sessions");
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].agent_id, "list-agent");
    }

    fn ensure_sleep_runs_table(db: &Database) {
        let conn = db.conn.lock().expect("lock");
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS sleep_runs (
                id TEXT PRIMARY KEY,
                agent_id TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'running',
                trigger_type TEXT NOT NULL,
                started_at TEXT NOT NULL,
                finished_at TEXT,
                source_chats_json TEXT NOT NULL DEFAULT '[]',
                source_digest_md TEXT,
                input_tokens INTEGER NOT NULL DEFAULT 0,
                output_tokens INTEGER NOT NULL DEFAULT 0,
                total_tokens INTEGER NOT NULL DEFAULT 0,
                error_message TEXT
            )",
        )
        .expect("create sleep_runs table");
    }

    fn create_test_sleep_run(db: &Database, agent_id: &str) -> String {
        ensure_sleep_runs_table(db);
        db.create_sleep_run(agent_id, SleepRunTrigger::Manual)
            .expect("create sleep run")
    }

    #[test]
    fn create_sleep_run_inserts_with_running_status() {
        let (db, _dir) = test_db();
        let id = create_test_sleep_run(&db, "agent-a");

        let run = db.get_sleep_run(&id).expect("get").expect("run exists");
        assert_eq!(run.status, SleepRunStatus::Running);
    }

    #[test]
    fn create_sleep_run_generates_id_and_timestamp() {
        let (db, _dir) = test_db();
        let id = create_test_sleep_run(&db, "agent-a");

        assert!(id.contains('-'), "UUID v4 should contain hyphens");

        let run = db.get_sleep_run(&id).expect("get").expect("run exists");
        assert!(
            run.started_at.contains('T'),
            "RFC3339 timestamp should contain 'T'"
        );
    }

    #[test]
    fn update_sleep_run_to_success() {
        let (db, _dir) = test_db();
        let id = create_test_sleep_run(&db, "agent-a");

        db.update_sleep_run_success(&id, r#"[1, 2, 3]"#, Some("digest-abc"), 100, 50)
            .expect("update success");

        let run = db.get_sleep_run(&id).expect("get").expect("run exists");
        assert_eq!(run.status, SleepRunStatus::Success);
        assert!(run.finished_at.is_some());
        assert_eq!(run.total_tokens, 150);
        assert_eq!(run.source_chats_json, r#"[1, 2, 3]"#);
        assert_eq!(run.source_digest_md.as_deref(), Some("digest-abc"));
    }

    #[test]
    fn update_sleep_run_to_failed() {
        let (db, _dir) = test_db();
        let id = create_test_sleep_run(&db, "agent-a");

        db.update_sleep_run_failed(&id, "LLM timeout")
            .expect("update failed");

        let run = db.get_sleep_run(&id).expect("get").expect("run exists");
        assert_eq!(run.status, SleepRunStatus::Failed);
        assert_eq!(run.error_message.as_deref(), Some("LLM timeout"));
    }

    #[test]
    fn update_sleep_run_to_skipped() {
        let (db, _dir) = test_db();
        let id = create_test_sleep_run(&db, "agent-a");

        db.update_sleep_run_skipped(&id).expect("update skipped");

        let run = db.get_sleep_run(&id).expect("get").expect("run exists");
        assert_eq!(run.status, SleepRunStatus::Skipped);
    }

    #[test]
    fn get_sleep_run_by_id() {
        let (db, _dir) = test_db();
        let id = create_test_sleep_run(&db, "agent-a");

        let run = db.get_sleep_run(&id).expect("get").expect("run exists");
        assert_eq!(run.id, id);
        assert_eq!(run.agent_id, "agent-a");
        assert_eq!(run.trigger, SleepRunTrigger::Manual);
        assert_eq!(run.source_chats_json, "[]");
        assert_eq!(run.input_tokens, 0);
        assert_eq!(run.output_tokens, 0);
        assert_eq!(run.total_tokens, 0);
    }

    #[test]
    fn get_sleep_run_returns_none_for_missing() {
        let (db, _dir) = test_db();
        ensure_sleep_runs_table(&db);

        let result = db.get_sleep_run("nonexistent-id").expect("get");
        assert!(result.is_none());
    }

    #[test]
    fn list_sleep_runs_by_agent() {
        let (db, _dir) = test_db();

        let _id_a1 = create_test_sleep_run(&db, "agent-a");
        let id_a2 = create_test_sleep_run(&db, "agent-a");
        let id_a3 = create_test_sleep_run(&db, "agent-a");
        let _id_b1 = create_test_sleep_run(&db, "agent-b");

        let runs = db.list_sleep_runs("agent-a", 2).expect("list");
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].id, id_a3);
        assert_eq!(runs[1].id, id_a2);
    }

    #[test]
    fn list_sleep_runs_empty() {
        let (db, _dir) = test_db();
        ensure_sleep_runs_table(&db);

        let runs = db.list_sleep_runs("nobody", 10).expect("list");
        assert!(runs.is_empty());
    }

    #[test]
    fn get_latest_successful_run() {
        let (db, _dir) = test_db();

        let id_1 = create_test_sleep_run(&db, "agent-a");
        db.update_sleep_run_success(&id_1, "[]", None, 10, 5)
            .expect("success 1");

        let id_2 = create_test_sleep_run(&db, "agent-a");
        db.update_sleep_run_success(&id_2, "[]", None, 20, 10)
            .expect("success 2");

        let latest = db
            .get_latest_successful_run("agent-a")
            .expect("get latest")
            .expect("should exist");
        assert_eq!(latest.id, id_2);
    }

    #[test]
    fn get_latest_successful_run_returns_none() {
        let (db, _dir) = test_db();
        let _id = create_test_sleep_run(&db, "agent-a");

        let latest = db.get_latest_successful_run("agent-a").expect("get latest");
        assert!(latest.is_none());
    }

    fn ensure_memory_snapshots_table(db: &Database) {
        let conn = db.conn.lock().expect("lock");
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS memory_snapshots (
                id TEXT PRIMARY KEY,
                run_id TEXT NOT NULL,
                agent_id TEXT NOT NULL,
                file TEXT NOT NULL,
                content_before TEXT NOT NULL,
                content_after TEXT NOT NULL,
                created_at TEXT NOT NULL
            )",
        )
        .expect("create memory_snapshots table");
    }

    fn ensure_sleep_run_exists(db: &Database, run_id: &str, agent_id: &str) {
        ensure_sleep_runs_table(db);
        let conn = db.conn.lock().expect("lock");
        conn.execute(
            "INSERT OR IGNORE INTO sleep_runs (id, agent_id, status, trigger_type, started_at)
             VALUES (?1, ?2, 'running', 'manual', ?3)",
            rusqlite::params![run_id, agent_id, chrono::Utc::now().to_rfc3339()],
        )
        .expect("create sleep run for test snapshot");
    }

    fn create_test_snapshot(
        db: &Database,
        run_id: &str,
        agent_id: &str,
        file: MemoryFile,
    ) -> String {
        ensure_memory_snapshots_table(db);
        ensure_sleep_run_exists(db, run_id, agent_id);
        db.create_memory_snapshot(
            run_id,
            agent_id,
            file,
            "before content",
            "after content",
        )
        .expect("create snapshot")
    }

    #[test]
    fn create_memory_snapshot_inserts_record() {
        let (db, _dir) = test_db();
        let id = create_test_snapshot(
            &db,
            "run-1",
            "agent-a",
            MemoryFile::Episodic,
        );

        let snapshots = db.get_snapshots_for_run("run-1").expect("get snapshots");
        assert_eq!(snapshots.len(), 1);
        assert_eq!(snapshots[0].id, id);
        assert_eq!(snapshots[0].run_id, "run-1");
        assert_eq!(snapshots[0].agent_id, "agent-a");
        assert_eq!(snapshots[0].file, MemoryFile::Episodic);
        assert_eq!(snapshots[0].content_before, "before content");
        assert_eq!(snapshots[0].content_after, "after content");
    }

    #[test]
    fn create_memory_snapshot_generates_id_and_timestamp() {
        let (db, _dir) = test_db();
        let id = create_test_snapshot(
            &db,
            "run-1",
            "agent-a",
            MemoryFile::Semantic,
        );
        assert!(id.contains('-'), "UUID v4 should contain hyphens");

        let snapshots = db.get_snapshots_for_run("run-1").expect("get snapshots");
        assert!(
            snapshots[0].created_at.contains('T'),
            "RFC3339 timestamp should contain 'T'"
        );
    }

    #[test]
    fn get_snapshots_for_run() {
        let (db, _dir) = test_db();
        create_test_snapshot(
            &db,
            "run-1",
            "agent-a",
            MemoryFile::Episodic,
        );
        std::thread::sleep(std::time::Duration::from_millis(2));
        create_test_snapshot(
            &db,
            "run-1",
            "agent-a",
            MemoryFile::Semantic,
        );
        std::thread::sleep(std::time::Duration::from_millis(2));
        create_test_snapshot(
            &db,
            "run-1",
            "agent-a",
            MemoryFile::Prospective,
        );
        create_test_snapshot(
            &db,
            "run-2",
            "agent-b",
            MemoryFile::Episodic,
        );

        let run1_snapshots = db.get_snapshots_for_run("run-1").expect("get snapshots");
        assert_eq!(run1_snapshots.len(), 3);
        assert_eq!(run1_snapshots[0].file, MemoryFile::Episodic);
        assert_eq!(run1_snapshots[1].file, MemoryFile::Semantic);
        assert_eq!(run1_snapshots[2].file, MemoryFile::Prospective);
    }

    #[test]
    fn get_snapshots_for_run_empty() {
        let (db, _dir) = test_db();
        ensure_memory_snapshots_table(&db);
        let snapshots = db
            .get_snapshots_for_run("nonexistent")
            .expect("get snapshots");
        assert!(snapshots.is_empty());
    }

    #[test]
    fn get_snapshots_for_agent() {
        let (db, _dir) = test_db();
        let id_a1 = create_test_snapshot(
            &db,
            "run-1",
            "agent-a",
            MemoryFile::Episodic,
        );
        std::thread::sleep(std::time::Duration::from_millis(2));
        let id_a2 = create_test_snapshot(
            &db,
            "run-2",
            "agent-a",
            MemoryFile::Semantic,
        );
        let _id_b = create_test_snapshot(
            &db,
            "run-3",
            "agent-b",
            MemoryFile::Episodic,
        );

        let agent_a_snapshots = db
            .get_snapshots_for_agent("agent-a", 10)
            .expect("get snapshots");
        assert_eq!(agent_a_snapshots.len(), 2);
        assert_eq!(agent_a_snapshots[0].id, id_a2);
        assert_eq!(agent_a_snapshots[1].id, id_a1);
    }

    #[test]
    fn get_snapshots_filters_by_file() {
        let (db, _dir) = test_db();
        create_test_snapshot(
            &db,
            "run-1",
            "agent-a",
            MemoryFile::Episodic,
        );
        std::thread::sleep(std::time::Duration::from_millis(2));
        create_test_snapshot(
            &db,
            "run-1",
            "agent-a",
            MemoryFile::Semantic,
        );

        let latest_episodic = db
            .get_latest_snapshot_for_file("agent-a", MemoryFile::Episodic)
            .expect("get latest")
            .expect("should exist");
        assert_eq!(latest_episodic.file, MemoryFile::Episodic);

        let latest_semantic = db
            .get_latest_snapshot_for_file("agent-a", MemoryFile::Semantic)
            .expect("get latest")
            .expect("should exist");
        assert_eq!(latest_semantic.file, MemoryFile::Semantic);
    }

    #[test]
    fn get_latest_snapshot_for_file() {
        let (db, _dir) = test_db();
        create_test_snapshot(
            &db,
            "run-1",
            "agent-a",
            MemoryFile::Episodic,
        );
        std::thread::sleep(std::time::Duration::from_millis(2));
        let id2 = create_test_snapshot(
            &db,
            "run-2",
            "agent-a",
            MemoryFile::Episodic,
        );

        let latest = db
            .get_latest_snapshot_for_file("agent-a", MemoryFile::Episodic)
            .expect("get latest")
            .expect("should exist");
        assert_eq!(latest.id, id2);
    }

    #[test]
    fn get_latest_snapshot_returns_none() {
        let (db, _dir) = test_db();
        ensure_memory_snapshots_table(&db);
        let result = db
            .get_latest_snapshot_for_file("agent-a", MemoryFile::Episodic)
            .expect("get latest");
        assert!(result.is_none());
    }

    // ---------------------------------------------------------------------------
    // Agent session enumeration tests
    // ---------------------------------------------------------------------------

    #[test]
    fn count_agent_messages_since_counts_correctly() {
        let (db, _dir) = test_db();

        let chat_id = db
            .resolve_or_create_chat_id("cli", "cli:msgs-a", None, "cli", "agent-a")
            .expect("create chat");

        store_msg(&db, "msg-1", chat_id, "old", "2024-01-01T00:00:00Z");
        store_msg(&db, "msg-2", chat_id, "old2", "2024-01-01T00:00:01Z");
        store_msg(&db, "msg-3", chat_id, "new", "2024-01-01T00:00:02Z");
        store_msg(&db, "msg-4", chat_id, "new2", "2024-01-01T00:00:03Z");

        let count = db
            .count_agent_messages_since("agent-a", Some("2024-01-01T00:00:01Z"))
            .expect("count");
        assert_eq!(count, 2, "should count only messages after the cutoff");
    }

    #[test]
    fn count_agent_messages_since_with_no_cutoff() {
        let (db, _dir) = test_db();

        let chat_id = db
            .resolve_or_create_chat_id("cli", "cli:no-cutoff", None, "cli", "agent-a")
            .expect("create chat");

        store_msg(&db, "msg-1", chat_id, "a", "2024-01-01T00:00:00Z");
        store_msg(&db, "msg-2", chat_id, "b", "2024-01-01T00:00:01Z");
        store_msg(&db, "msg-3", chat_id, "c", "2024-01-01T00:00:02Z");

        let count = db
            .count_agent_messages_since("agent-a", None)
            .expect("count");
        assert_eq!(count, 3);
    }

    #[test]
    fn count_agent_messages_since_returns_zero_for_unknown_agent() {
        let (db, _dir) = test_db();

        let count = db
            .count_agent_messages_since("nonexistent-agent", None)
            .expect("count");
        assert_eq!(count, 0);
    }

    #[test]
    fn count_agent_messages_since_excludes_other_agents() {
        let (db, _dir) = test_db();

        let chat_a = db
            .resolve_or_create_chat_id("cli", "cli:chat-a", None, "cli", "agent-a")
            .expect("create chat a");
        let chat_b = db
            .resolve_or_create_chat_id("cli", "cli:chat-b", None, "cli", "agent-b")
            .expect("create chat b");

        store_msg(&db, "msg-a1", chat_a, "hello", "2024-01-01T00:00:00Z");
        store_msg(&db, "msg-a2", chat_a, "world", "2024-01-01T00:00:01Z");
        store_msg(&db, "msg-b1", chat_b, "secret", "2024-01-01T00:00:02Z");

        let count = db
            .count_agent_messages_since("agent-a", None)
            .expect("count");
        assert_eq!(count, 2);
    }

    #[test]
    fn get_agent_sessions_since_returns_sessions() {
        let (db, _dir) = test_db();

        let chat_id = db
            .resolve_or_create_chat_id("web", "web:sess-a", Some("sess-a"), "web", "agent-a")
            .expect("create chat");

        db.save_session(chat_id, r#"{"msgs":[]}"#)
            .expect("save session");

        let sessions = db
            .get_agent_sessions_since("agent-a", None, 10)
            .expect("get sessions");
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].chat_id, chat_id);
        assert_eq!(sessions[0].channel, "web");
        assert_eq!(sessions[0].external_chat_id, "web:sess-a");
    }

    #[test]
    fn get_agent_sessions_since_ordered_by_updated_at_desc() {
        let (db, _dir) = test_db();

        let chat_1 = db
            .resolve_or_create_chat_id("cli", "cli:chat-1", None, "cli", "agent-a")
            .expect("create chat 1");
        let chat_2 = db
            .resolve_or_create_chat_id("cli", "cli:chat-2", None, "cli", "agent-a")
            .expect("create chat 2");

        // Create sessions with small delay so updated_at differs
        db.save_session(chat_1, r#"{}"#).expect("save session 1");
        std::thread::sleep(std::time::Duration::from_millis(5));
        db.save_session(chat_2, r#"{}"#).expect("save session 2");

        let sessions = db
            .get_agent_sessions_since("agent-a", None, 10)
            .expect("get sessions");
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].chat_id, chat_2, "newest first");
        assert_eq!(sessions[1].chat_id, chat_1, "oldest second");
    }

    #[test]
    fn get_agent_sessions_since_respects_limit() {
        let (db, _dir) = test_db();

        for i in 0..5 {
            let chat_id = db
                .resolve_or_create_chat_id("cli", &format!("cli:limit-{i}"), None, "cli", "agent-a")
                .expect("create chat");
            db.save_session(chat_id, r#"{}"#).expect("save session");
        }

        let sessions = db
            .get_agent_sessions_since("agent-a", None, 3)
            .expect("get sessions");
        assert_eq!(sessions.len(), 3);
    }

    #[test]
    fn get_agent_sessions_since_with_no_cutoff() {
        let (db, _dir) = test_db();

        let chat_id = db
            .resolve_or_create_chat_id("cli", "cli:nocut", None, "cli", "agent-a")
            .expect("create chat");
        db.save_session(chat_id, r#"{}"#).expect("save session");

        let sessions = db
            .get_agent_sessions_since("agent-a", None, 10)
            .expect("get sessions");
        assert_eq!(sessions.len(), 1);
    }

    #[test]
    fn get_agent_sessions_since_returns_empty_for_unknown_agent() {
        let (db, _dir) = test_db();

        let sessions = db
            .get_agent_sessions_since("nonexistent-agent", None, 10)
            .expect("get sessions");
        assert!(sessions.is_empty());
    }

    #[test]
    fn get_agent_sessions_includes_message_count() {
        let (db, _dir) = test_db();

        let chat_id = db
            .resolve_or_create_chat_id("cli", "cli:msgcount", None, "cli", "agent-a")
            .expect("create chat");

        store_msg(&db, "m1", chat_id, "msg 1", "2024-01-01T00:00:00Z");
        store_msg(&db, "m2", chat_id, "msg 2", "2024-01-01T00:00:01Z");
        store_msg(&db, "m3", chat_id, "msg 3", "2024-01-01T00:00:02Z");

        db.save_session(chat_id, r#"{}"#).expect("save session");

        let sessions = db
            .get_agent_sessions_since("agent-a", None, 10)
            .expect("get sessions");
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].message_count, 3);
    }

    #[test]
    fn get_agent_sessions_includes_estimated_tokens() {
        let (db, _dir) = test_db();

        let chat_id = db
            .resolve_or_create_chat_id("cli", "cli:tokcount", None, "cli", "agent-a")
            .expect("create chat");

        // Use a known-length session JSON: 30 chars → estimated_tokens = 30/3 = 10
        let session_json = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"; // 30 'A' chars
        assert_eq!(
            session_json.len(),
            30,
            "test fixture should be exactly 30 chars"
        );

        db.save_session(chat_id, session_json)
            .expect("save session");

        let sessions = db
            .get_agent_sessions_since("agent-a", None, 10)
            .expect("get sessions");
        assert_eq!(sessions.len(), 1);
        assert!(sessions[0].estimated_tokens > 0);
        assert_eq!(
            sessions[0].estimated_tokens,
            (session_json.len() as i64) / 3
        );
    }
}
