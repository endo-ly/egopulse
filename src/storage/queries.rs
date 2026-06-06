use std::str::FromStr;

use rusqlite::{OptionalExtension, TransactionBehavior, params};

use crate::error::StorageError;

use super::{
    AgentSessionInfo, ChatInfo, CheckpointSourceKind, Database, EpisodeEvent,
    EpisodeEventCertainty, EpisodeEventKind, EpisodeRollup, LlmUsageLogEntry, MemoryFile,
    MemorySnapshot, MessageKind, PulseOutputKind, PulseRun, PulseRunStatus, RollupGranularity,
    SenderKind, SessionSnapshot, SessionSummary, SleepRun, SleepRunStatus, SleepRunStep,
    SleepRunTrigger, SleepStepCheckpoint, SleepStepName, SleepStepResult, SleepStepStatus,
    StoredMessage, ToolCall,
};

fn row_to_stored_message(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredMessage> {
    let sender_kind_str: String = row.get(4)?;
    let sender_kind = SenderKind::from_str(&sender_kind_str).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(
            4,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, e)),
        )
    })?;

    let message_kind_str: String = row.get(6)?;
    let message_kind = MessageKind::from_str(&message_kind_str).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(
            6,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, e)),
        )
    })?;

    Ok(StoredMessage {
        id: row.get(0)?,
        chat_id: row.get(1)?,
        sender_id: row.get(2)?,
        content: row.get(3)?,
        sender_kind,
        timestamp: row.get(5)?,
        message_kind,
        recipient_agent_id: row.get(7)?,
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

fn row_to_pulse_run(row: &rusqlite::Row<'_>) -> rusqlite::Result<PulseRun> {
    let status_str: String = row.get(6)?;
    let status = PulseRunStatus::from_str(&status_str).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(
            6,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, e)),
        )
    })?;

    let output_kind: Option<PulseOutputKind> = row
        .get::<_, Option<String>>(9)?
        .map(|s| PulseOutputKind::from_str(&s))
        .transpose()
        .map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(
                9,
                rusqlite::types::Type::Text,
                Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, e)),
            )
        })?;

    Ok(PulseRun {
        id: row.get(0)?,
        agent_id: row.get(1)?,
        intention_id: row.get(2)?,
        due_key: row.get(3)?,
        chat_id: row.get(4)?,
        message_id: row.get(5)?,
        status,
        started_at: row.get(7)?,
        finished_at: row.get(8)?,
        output_kind,
        output_text: row.get(10)?,
        error_message: row.get(11)?,
    })
}

fn row_to_episode_event(row: &rusqlite::Row<'_>) -> rusqlite::Result<EpisodeEvent> {
    let kind_str: String = row.get(4)?;
    let kind = EpisodeEventKind::from_str(&kind_str).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(
            4,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, e)),
        )
    })?;
    let certainty_str: String = row.get(8)?;
    let certainty = EpisodeEventCertainty::from_str(&certainty_str).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(
            8,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, e)),
        )
    })?;
    Ok(EpisodeEvent {
        id: row.get(0)?,
        agent_id: row.get(1)?,
        experienced_at: row.get(2)?,
        encoded_at: row.get(3)?,
        kind,
        title: row.get(5)?,
        body_md: row.get(6)?,
        ripple_strength: row.get(7)?,
        certainty,
        sleep_run_id: row.get(9)?,
        source_refs_json: row.get(10)?,
        created_at: row.get(11)?,
        updated_at: row.get(12)?,
    })
}

// ---------------------------------------------------------------------------
// Sleep runs
// ---------------------------------------------------------------------------

fn insert_pending_steps(
    tx: &rusqlite::Transaction<'_>,
    sleep_run_id: &str,
) -> Result<(), StorageError> {
    for step in SleepStepName::ALL {
        tx.execute(
            "INSERT INTO sleep_run_steps (sleep_run_id, step_name, status)
             VALUES (?1, ?2, 'pending')",
            params![sleep_run_id, step.to_string()],
        )?;
    }
    Ok(())
}

fn row_to_sleep_run_step(row: &rusqlite::Row<'_>) -> rusqlite::Result<SleepRunStep> {
    let step_name_str: String = row.get(1)?;
    let step_name = SleepStepName::from_str(&step_name_str).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(
            1,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, e)),
        )
    })?;
    let status_str: String = row.get(2)?;
    let status = SleepStepStatus::from_str(&status_str).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(
            2,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, e)),
        )
    })?;
    Ok(SleepRunStep {
        sleep_run_id: row.get(0)?,
        step_name,
        status,
        started_at: row.get(3)?,
        finished_at: row.get(4)?,
        input_tokens: row.get(5)?,
        output_tokens: row.get(6)?,
        error_message: row.get(7)?,
        metadata_json: row.get(8)?,
    })
}

fn row_to_sleep_checkpoint(row: &rusqlite::Row<'_>) -> rusqlite::Result<SleepStepCheckpoint> {
    let step_name_str: String = row.get(1)?;
    let step_name = SleepStepName::from_str(&step_name_str).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(
            1,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, e)),
        )
    })?;
    let source_kind_str: String = row.get(2)?;
    let source_kind = CheckpointSourceKind::from_str(&source_kind_str).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(
            2,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, e)),
        )
    })?;
    Ok(SleepStepCheckpoint {
        agent_id: row.get(0)?,
        step_name,
        source_kind,
        source_id: row.get(3)?,
        cursor_at: row.get(4)?,
        cursor_id: row.get(5)?,
        updated_at: row.get(6)?,
    })
}

impl Database {
    pub(crate) fn resolve_chat_id(
        &self,
        channel: &str,
        external_chat_id: &str,
    ) -> Result<Option<i64>, StorageError> {
        let conn = self.get_conn()?;
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
        let conn = self.get_conn()?;
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

    pub(crate) fn get_chat_by_channel_external_and_agent(
        &self,
        channel: &str,
        external_chat_id: &str,
        agent_id: &str,
    ) -> Result<Option<ChatInfo>, StorageError> {
        let conn = self.get_conn()?;
        match conn.query_row(
            "SELECT chat_id, chat_type, agent_id FROM chats WHERE channel = ?1 AND external_chat_id = ?2 AND agent_id = ?3 LIMIT 1",
            params![channel, external_chat_id, agent_id],
            |row| {
                Ok(ChatInfo {
                    chat_id: row.get(0)?,
                    channel: channel.to_string(),
                    external_chat_id: external_chat_id.to_string(),
                    chat_type: row.get(1)?,
                    agent_id: row.get(2)?,
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
        let conn = self.get_conn()?;
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
        let conn = self.get_conn()?;
        let mut stmt = conn.prepare_cached(
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
        let conn = self.get_conn()?;
        let mut stmt = conn.prepare_cached(
            "SELECT id, chat_id, sender_id, content, sender_kind, timestamp,
                    message_kind, recipient_agent_id
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
        let conn = self.get_conn()?;
        let mut stmt = conn.prepare_cached(
            "SELECT id, chat_id, sender_id, content, sender_kind, timestamp,
                    message_kind, recipient_agent_id
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
        let conn = self.get_conn()?;
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

    /// Clears session message history by setting `messages_json` to an empty
    /// JSON array.  The session row itself and `messages` / `tool_calls`
    /// records are preserved.
    ///
    /// Uses optimistic concurrency: the update only succeeds when
    /// `expected_updated_at` matches the current row.  Returns `Ok(true)` if
    /// the row was updated, `Ok(false)` if the row was not found or the
    /// timestamp did not match (concurrent modification).
    pub(crate) fn clear_session_messages(
        &self,
        chat_id: i64,
        expected_updated_at: &str,
    ) -> Result<bool, StorageError> {
        let conn = self.get_conn()?;
        let now = chrono::Utc::now().to_rfc3339();
        let rows = conn.execute(
            "UPDATE sessions SET messages_json = '[]', updated_at = ?1 \
             WHERE chat_id = ?2 AND updated_at = ?3",
            params![now, chat_id, expected_updated_at],
        )?;
        Ok(rows > 0)
    }

    pub(crate) fn store_message_with_session(
        &self,
        message: &StoredMessage,
        messages_json: &str,
        expected_updated_at: Option<&str>,
    ) -> Result<String, StorageError> {
        let mut conn = self.get_conn()?;
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT OR REPLACE INTO messages (id, chat_id, sender_id, content, sender_kind, timestamp, message_kind, recipient_agent_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                message.id,
                message.chat_id,
                message.sender_id,
                message.content,
                message.sender_kind.to_string(),
                message.timestamp,
                message.message_kind.to_string(),
                message.recipient_agent_id.as_deref(),
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
        let mut conn = self.get_conn()?;
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
                "SELECT id, chat_id, sender_id, content, sender_kind, timestamp,
                        message_kind, recipient_agent_id
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
// Channel Log (multi-agent room shared log)
// ---------------------------------------------------------------------------

impl Database {
    /// Resolves or creates the Channel Log chat for a Discord multi-agent room.
    ///
    /// The Channel Log uses `channel = "discord"`,
    /// `external_chat_id = "discord:{channel_id}:multi-room-log"`,
    /// `chat_type = "channel_log"`, `agent_id = ""`.
    /// It has **no session row** — only messages.
    pub(crate) fn resolve_channel_log_chat_id(&self, channel_id: u64) -> Result<i64, StorageError> {
        let external_id = format!("discord:{channel_id}:multi-room-log");
        self.resolve_or_create_chat_id("discord", &external_id, None, "channel_log", "")
    }

    /// Resolves or creates a Channel Log chat for Telegram multi-agent rooms.
    /// Same concept as [`resolve_channel_log_chat_id`] but keyed by Telegram `i64` chat ID.
    pub(crate) fn resolve_telegram_channel_log_chat_id(
        &self,
        chat_id: i64,
    ) -> Result<i64, StorageError> {
        let external_id = format!("telegram:{chat_id}:multi-room-log");
        self.resolve_or_create_chat_id("telegram", &external_id, None, "channel_log", "")
    }

    /// Returns the most recent messages from a Channel Log, ordered oldest-first.
    pub(crate) fn get_channel_log_messages(
        &self,
        chat_id: i64,
        limit: usize,
    ) -> Result<Vec<StoredMessage>, StorageError> {
        self.get_recent_messages(chat_id, limit)
    }

    /// Stores a message without touching the session snapshot.
    /// Used for Channel Log entries (agent_send, system events) that have no session.
    pub(crate) fn store_message_only(&self, message: &StoredMessage) -> Result<(), StorageError> {
        let conn = self.get_conn()?;
        conn.execute(
            "INSERT OR REPLACE INTO messages (id, chat_id, sender_id, content, sender_kind, timestamp, message_kind, recipient_agent_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                message.id,
                message.chat_id,
                message.sender_id,
                message.content,
                message.sender_kind.to_string(),
                message.timestamp,
                message.message_kind.to_string(),
                message.recipient_agent_id.as_deref(),
            ],
        )?;
        Ok(())
    }

    /// Persists a stop-reason system event to the Channel Log.
    ///
    /// Content format: `{"reason": "StopReasonVariant"}`.
    /// Sender: `sender_id = "system"`, `sender_kind = System`.
    pub(crate) fn store_system_event(
        &self,
        channel_log_chat_id: i64,
        reason: &crate::runtime::turn_scheduler::StopReason,
    ) -> Result<(), StorageError> {
        let content = serde_json::json!({
            "reason": format!("{reason:?}")
        })
        .to_string();

        let mut message = StoredMessage::system(channel_log_chat_id, content);
        message.message_kind = MessageKind::SystemEvent;

        self.store_message_only(&message)
    }

    /// Persists a bot response to the Channel Log.
    ///
    /// Sender is the agent ID, `sender_kind = Assistant`, `MessageKind::Message`.
    pub(crate) fn store_channel_log_bot_response(
        &self,
        channel_log_chat_id: i64,
        agent_id: &str,
        response: &str,
    ) -> Result<(), StorageError> {
        let message = StoredMessage {
            id: format!("cl-bot-{}", uuid::Uuid::new_v4()),
            chat_id: channel_log_chat_id,
            sender_id: agent_id.to_string(),
            content: response.to_string(),
            sender_kind: SenderKind::Assistant,
            timestamp: chrono::Utc::now().to_rfc3339(),
            message_kind: MessageKind::Message,
            recipient_agent_id: None,
        };
        self.store_message_only(&message)
    }
}

// ---------------------------------------------------------------------------
// Tool calls & LLM usage
// ---------------------------------------------------------------------------

impl Database {
    pub(crate) fn store_tool_call(&self, tool_call: &ToolCall) -> Result<(), StorageError> {
        let conn = self.get_conn()?;
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
        let conn = self.get_conn()?;
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
        let conn = self.get_conn()?;
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
        let mut conn = self.get_conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let id = uuid::Uuid::new_v4().to_string();
        let status = SleepRunStatus::Running.to_string();
        let started_at = chrono::Utc::now().to_rfc3339();

        tx.execute(
            "INSERT INTO sleep_runs
                 (id, agent_id, status, trigger_type, started_at, finished_at,
                  source_chats_json, source_digest_md,
                  input_tokens, output_tokens, total_tokens, error_message)
              VALUES (?1, ?2, ?3, ?4, ?5, NULL, '[]', NULL, 0, 0, 0, NULL)",
            params![id, agent_id, status, trigger.to_string(), started_at],
        )?;
        insert_pending_steps(&tx, &id)?;
        tx.commit()?;
        Ok(id)
    }

    /// Atomically checks for a running sleep run and creates one if none exists.
    ///
    /// This prevents a race condition where two concurrent callers could both
    /// observe "no running run" and each insert a duplicate.
    ///
    /// Returns `Ok(Some(id))` if a new run was created, or `Ok(None)` if a
    /// running run already exists for the given agent.
    pub(crate) fn try_create_sleep_run(
        &self,
        agent_id: &str,
        trigger: SleepRunTrigger,
    ) -> Result<Option<String>, StorageError> {
        let mut conn = self.get_conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let running = SleepRunStatus::Running.to_string();
        let count: i64 = tx.query_row(
            "SELECT COUNT(*) FROM sleep_runs WHERE agent_id = ?1 AND status = ?2",
            params![agent_id, running],
            |row| row.get(0),
        )?;

        if count > 0 {
            return Ok(None);
        }

        let id = uuid::Uuid::new_v4().to_string();
        let status = SleepRunStatus::Running.to_string();
        let started_at = chrono::Utc::now().to_rfc3339();

        tx.execute(
            "INSERT INTO sleep_runs
                 (id, agent_id, status, trigger_type, started_at, finished_at,
                  source_chats_json, source_digest_md,
                  input_tokens, output_tokens, total_tokens, error_message)
            VALUES (?1, ?2, ?3, ?4, ?5, NULL, '[]', NULL, 0, 0, 0, NULL)",
            params![id, agent_id, status, trigger.to_string(), started_at],
        )?;
        insert_pending_steps(&tx, &id)?;
        tx.commit()?;
        Ok(Some(id))
    }

    pub(crate) fn update_sleep_run_success(
        &self,
        id: &str,
        source_chats_json: &str,
        source_digest_md: Option<&str>,
        input_tokens: i64,
        output_tokens: i64,
    ) -> Result<(), StorageError> {
        let conn = self.get_conn()?;
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
        let conn = self.get_conn()?;
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
        let conn = self.get_conn()?;
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
        let conn = self.get_conn()?;
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
        let conn = self.get_conn()?;
        let mut stmt = conn.prepare_cached(
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

    pub(crate) fn list_distinct_agent_ids(&self) -> Result<Vec<String>, StorageError> {
        let conn = self.get_conn()?;
        let mut stmt =
            conn.prepare_cached("SELECT DISTINCT agent_id FROM sleep_runs ORDER BY agent_id")?;
        stmt.query_map([], |row| row.get(0))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    pub(crate) fn list_all_sleep_runs(&self, limit: i64) -> Result<Vec<SleepRun>, StorageError> {
        let conn = self.get_conn()?;
        let mut stmt = conn.prepare_cached(
            "SELECT id, agent_id, status, trigger_type, started_at, finished_at,
                    source_chats_json, source_digest_md,
                    input_tokens, output_tokens, total_tokens, error_message
             FROM sleep_runs
             ORDER BY started_at DESC, rowid DESC
             LIMIT ?1",
        )?;
        stmt.query_map(params![limit], row_to_sleep_run)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    pub(crate) fn get_latest_successful_run(
        &self,
        agent_id: &str,
    ) -> Result<Option<SleepRun>, StorageError> {
        let conn = self.get_conn()?;
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

    /// Returns the latest successful non-backfill run for an agent.
    pub(crate) fn get_latest_successful_non_backfill_run(
        &self,
        agent_id: &str,
    ) -> Result<Option<SleepRun>, StorageError> {
        let conn = self.get_conn()?;
        conn.query_row(
            "SELECT id, agent_id, status, trigger_type, started_at, finished_at,
                    source_chats_json, source_digest_md,
                    input_tokens, output_tokens, total_tokens, error_message
             FROM sleep_runs
             WHERE agent_id = ?1 AND status = 'success' AND trigger_type != 'backfill'
             ORDER BY finished_at DESC
             LIMIT 1",
            params![agent_id],
            row_to_sleep_run,
        )
        .optional()
        .map_err(Into::into)
    }

    /// Marks a sleep run step as running and sets `started_at`.
    ///
    /// # Errors
    ///
    /// Returns `StorageError::Conflict` if the step is not in `pending` state.
    pub(crate) fn start_sleep_step(
        &self,
        sleep_run_id: &str,
        step_name: SleepStepName,
    ) -> Result<(), StorageError> {
        let conn = self.get_conn()?;
        let now = chrono::Utc::now().to_rfc3339();
        let running = SleepStepStatus::Running.to_string();
        let pending = SleepStepStatus::Pending.to_string();

        let changed = conn.execute(
            "UPDATE sleep_run_steps
             SET status = ?1, started_at = ?2
             WHERE sleep_run_id = ?3 AND step_name = ?4 AND status = ?5",
            params![running, now, sleep_run_id, step_name.to_string(), pending],
        )?;
        if changed == 0 {
            return Err(StorageError::Conflict(format!(
                "sleep_run_step:{sleep_run_id}:{} is not pending",
                step_name
            )));
        }
        Ok(())
    }

    /// Finishes a running sleep step with terminal status, tokens, and metadata.
    ///
    /// # Errors
    ///
    /// Returns `StorageError::Conflict` if the step is not in `running` state.
    pub(crate) fn finish_sleep_step(
        &self,
        sleep_run_id: &str,
        step_name: SleepStepName,
        result: SleepStepResult<'_>,
    ) -> Result<(), StorageError> {
        let conn = self.get_conn()?;
        let now = chrono::Utc::now().to_rfc3339();
        let running = SleepStepStatus::Running.to_string();

        let changed = conn.execute(
            "UPDATE sleep_run_steps
             SET status = ?1, finished_at = ?2,
                 input_tokens = ?3, output_tokens = ?4,
                 error_message = ?5, metadata_json = ?6
             WHERE sleep_run_id = ?7 AND step_name = ?8 AND status = ?9",
            params![
                result.status.to_string(),
                now,
                result.input_tokens,
                result.output_tokens,
                result.error_message,
                result.metadata_json,
                sleep_run_id,
                step_name.to_string(),
                running,
            ],
        )?;
        if changed == 0 {
            return Err(StorageError::Conflict(format!(
                "sleep_run_step:{sleep_run_id}:{} is not running",
                step_name
            )));
        }
        Ok(())
    }

    /// Atomically commits step success and checkpoint update in a single transaction.
    ///
    /// This ensures that step success and checkpoint advancement are either both
    /// persisted or both rolled back, preventing inconsistent states.
    ///
    /// # Errors
    ///
    /// Returns `StorageError::Conflict` if:
    /// - The step is not in `running` state
    /// - The checkpoint update would move the cursor backward
    pub(crate) fn commit_step_success(
        &self,
        sleep_run_id: &str,
        step_name: SleepStepName,
        result: SleepStepResult<'_>,
        checkpoint: Option<&SleepStepCheckpoint>,
    ) -> Result<(), StorageError> {
        let mut conn = self.get_conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = chrono::Utc::now().to_rfc3339();
        let running = SleepStepStatus::Running.to_string();

        let changed = tx.execute(
            "UPDATE sleep_run_steps
             SET status = ?1, finished_at = ?2,
                 input_tokens = ?3, output_tokens = ?4,
                 error_message = ?5, metadata_json = ?6
             WHERE sleep_run_id = ?7 AND step_name = ?8 AND status = ?9",
            params![
                result.status.to_string(),
                now,
                result.input_tokens,
                result.output_tokens,
                result.error_message,
                result.metadata_json,
                sleep_run_id,
                step_name.to_string(),
                running,
            ],
        )?;
        if changed == 0 {
            tx.rollback()?;
            return Err(StorageError::Conflict(format!(
                "sleep_run_step:{sleep_run_id}:{} is not running",
                step_name
            )));
        }

        if let Some(cp) = checkpoint {
            let cp_changed = tx.execute(
                "INSERT INTO sleep_step_checkpoints
                    (agent_id, step_name, source_kind, source_id, cursor_at, cursor_id, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT(agent_id, step_name, source_kind, source_id) DO UPDATE SET
                    cursor_at = ?5,
                    cursor_id = ?6,
                    updated_at = ?7
                 WHERE (cursor_at, cursor_id) < (?5, ?6)",
                params![
                    cp.agent_id,
                    cp.step_name.to_string(),
                    cp.source_kind.to_string(),
                    cp.source_id,
                    cp.cursor_at,
                    cp.cursor_id,
                    cp.updated_at,
                ],
            )?;
            if cp_changed == 0 {
                let existing = tx.query_row(
                    "SELECT cursor_at, cursor_id FROM sleep_step_checkpoints
                     WHERE agent_id = ?1 AND step_name = ?2 AND source_kind = ?3 AND source_id = ?4",
                    params![
                        cp.agent_id,
                        cp.step_name.to_string(),
                        cp.source_kind.to_string(),
                        cp.source_id,
                    ],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                );
                if let Ok((existing_at, existing_id)) = existing {
                    tx.rollback()?;
                    return Err(StorageError::Conflict(format!(
                        "checkpoint backward rejected: existing=({},{}) new=({},{})",
                        existing_at, existing_id, cp.cursor_at, cp.cursor_id
                    )));
                }
            }
        }

        tx.commit()?;
        Ok(())
    }

    /// Lists all steps for a sleep run, ordered by step_name.
    pub(crate) fn list_sleep_run_steps(
        &self,
        sleep_run_id: &str,
    ) -> Result<Vec<SleepRunStep>, StorageError> {
        let conn = self.get_conn()?;
        let mut stmt = conn.prepare_cached(
            "SELECT sleep_run_id, step_name, status, started_at, finished_at,
                    input_tokens, output_tokens, error_message, metadata_json
             FROM sleep_run_steps
             WHERE sleep_run_id = ?1
             ORDER BY step_name",
        )?;
        stmt.query_map(params![sleep_run_id], row_to_sleep_run_step)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    /// Gets a single step by sleep_run_id and step_name.
    ///
    /// Returns `None` if the step does not exist.
    pub(crate) fn get_sleep_run_step(
        &self,
        sleep_run_id: &str,
        step_name: SleepStepName,
    ) -> Result<Option<SleepRunStep>, StorageError> {
        let conn = self.get_conn()?;
        conn.query_row(
            "SELECT sleep_run_id, step_name, status, started_at, finished_at,
                    input_tokens, output_tokens, error_message, metadata_json
             FROM sleep_run_steps
             WHERE sleep_run_id = ?1 AND step_name = ?2",
            params![sleep_run_id, step_name.to_string()],
            row_to_sleep_run_step,
        )
        .optional()
        .map_err(Into::into)
    }

    /// Finalizes a sleep run by aggregating step results into run-level status and tokens.
    ///
    /// Status derivation:
    /// - `Success`: all steps succeeded or skipped, at least one success
    /// - `PartialFailure`: at least one success and at least one failed
    /// - `Failed`: any pending/running remaining, or all failed
    /// - `Skipped`: all steps skipped
    ///
    /// # Errors
    ///
    /// Returns `StorageError::Conflict` if the run is not in `running` state.
    pub(crate) fn finalize_sleep_run(
        &self,
        sleep_run_id: &str,
    ) -> Result<SleepRunStatus, StorageError> {
        let mut conn = self.get_conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;

        let steps: Vec<SleepRunStep> = {
            let mut stmt = tx.prepare(
                "SELECT sleep_run_id, step_name, status, started_at, finished_at,
                        input_tokens, output_tokens, error_message, metadata_json
                 FROM sleep_run_steps
                 WHERE sleep_run_id = ?1",
            )?;
            stmt.query_map(params![sleep_run_id], row_to_sleep_run_step)?
                .collect::<Result<Vec<_>, _>>()?
        };

        let mut success_count = 0;
        let mut failed_count = 0;
        let mut skipped_count = 0;
        let mut pending_or_running = false;
        let mut total_input_tokens: i64 = 0;
        let mut total_output_tokens: i64 = 0;
        let mut errors: Vec<String> = Vec::new();

        for step in &steps {
            match step.status {
                SleepStepStatus::Success => success_count += 1,
                SleepStepStatus::Failed => {
                    failed_count += 1;
                    if let Some(ref err) = step.error_message {
                        errors.push(format!("{}: {}", step.step_name, err));
                    }
                }
                SleepStepStatus::Skipped => skipped_count += 1,
                SleepStepStatus::Pending | SleepStepStatus::Running => {
                    pending_or_running = true;
                }
            }
            total_input_tokens = total_input_tokens.saturating_add(step.input_tokens);
            total_output_tokens = total_output_tokens.saturating_add(step.output_tokens);
        }

        let derived_status = if pending_or_running {
            SleepRunStatus::Failed
        } else if success_count == 0 && failed_count == 0 && skipped_count > 0 {
            SleepRunStatus::Skipped
        } else if success_count > 0 && failed_count == 0 {
            SleepRunStatus::Success
        } else if success_count > 0 && failed_count > 0 {
            SleepRunStatus::PartialFailure
        } else {
            SleepRunStatus::Failed
        };

        let error_message = if errors.is_empty() {
            None
        } else {
            Some(errors.join("; "))
        };

        let now = chrono::Utc::now().to_rfc3339();
        let total_tokens = total_input_tokens.saturating_add(total_output_tokens);
        let running = SleepRunStatus::Running.to_string();

        let changed = tx.execute(
            "UPDATE sleep_runs
             SET status = ?1, finished_at = ?2,
                 input_tokens = ?3, output_tokens = ?4, total_tokens = ?5,
                 error_message = ?6
             WHERE id = ?7 AND status = ?8",
            params![
                derived_status.to_string(),
                now,
                total_input_tokens,
                total_output_tokens,
                total_tokens,
                error_message,
                sleep_run_id,
                running,
            ],
        )?;
        if changed == 0 {
            tx.rollback()?;
            return Err(StorageError::Conflict(format!(
                "sleep_run:{sleep_run_id} is not running"
            )));
        }

        tx.commit()?;
        Ok(derived_status)
    }

    // ---------------------------------------------------------------------------
    // Sleep step checkpoints
    // ---------------------------------------------------------------------------

    /// Gets a checkpoint by composite key (agent_id, step_name, source_kind, source_id).
    ///
    /// Returns `None` if no checkpoint exists for the given key.
    pub(crate) fn get_sleep_checkpoint(
        &self,
        agent_id: &str,
        step_name: SleepStepName,
        source_kind: CheckpointSourceKind,
        source_id: &str,
    ) -> Result<Option<SleepStepCheckpoint>, StorageError> {
        let conn = self.get_conn()?;
        conn.query_row(
            "SELECT agent_id, step_name, source_kind, source_id,
                    cursor_at, cursor_id, updated_at
             FROM sleep_step_checkpoints
             WHERE agent_id = ?1 AND step_name = ?2
               AND source_kind = ?3 AND source_id = ?4",
            params![
                agent_id,
                step_name.to_string(),
                source_kind.to_string(),
                source_id
            ],
            row_to_sleep_checkpoint,
        )
        .optional()
        .map_err(Into::into)
    }

    /// Inserts or updates a checkpoint with monotonic forward-only cursor validation.
    ///
    /// On conflict, updates cursor_at, cursor_id, and updated_at only if the new
    /// cursor position is strictly greater than the existing one (using tuple comparison).
    ///
    /// # Errors
    ///
    /// Returns `StorageError::Conflict` if the new cursor position is not forward
    /// from the existing checkpoint.
    pub(crate) fn upsert_sleep_checkpoint(
        &self,
        checkpoint: &SleepStepCheckpoint,
    ) -> Result<(), StorageError> {
        let conn = self.get_conn()?;
        let changed = conn.execute(
            "INSERT INTO sleep_step_checkpoints
                (agent_id, step_name, source_kind, source_id, cursor_at, cursor_id, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(agent_id, step_name, source_kind, source_id) DO UPDATE SET
                cursor_at = ?5,
                cursor_id = ?6,
                updated_at = ?7
             WHERE (cursor_at, cursor_id) < (?5, ?6)",
            params![
                checkpoint.agent_id,
                checkpoint.step_name.to_string(),
                checkpoint.source_kind.to_string(),
                checkpoint.source_id,
                checkpoint.cursor_at,
                checkpoint.cursor_id,
                checkpoint.updated_at,
            ],
        )?;
        if changed == 0 {
            let existing = conn.query_row(
                "SELECT cursor_at, cursor_id FROM sleep_step_checkpoints
                 WHERE agent_id = ?1 AND step_name = ?2 AND source_kind = ?3 AND source_id = ?4",
                params![
                    checkpoint.agent_id,
                    checkpoint.step_name.to_string(),
                    checkpoint.source_kind.to_string(),
                    checkpoint.source_id,
                ],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            );
            if let Ok((existing_at, existing_id)) = existing {
                return Err(StorageError::Conflict(format!(
                    "checkpoint backward rejected: existing=({},{}) new=({},{})",
                    existing_at, existing_id, checkpoint.cursor_at, checkpoint.cursor_id
                )));
            }
        }
        Ok(())
    }

    /// Lists all checkpoints for a given agent, step, and source_kind, ordered by source_id.
    pub(crate) fn list_sleep_checkpoints(
        &self,
        agent_id: &str,
        step_name: SleepStepName,
        source_kind: CheckpointSourceKind,
    ) -> Result<Vec<SleepStepCheckpoint>, StorageError> {
        let conn = self.get_conn()?;
        let mut stmt = conn.prepare_cached(
            "SELECT agent_id, step_name, source_kind, source_id,
                    cursor_at, cursor_id, updated_at
             FROM sleep_step_checkpoints
             WHERE agent_id = ?1 AND step_name = ?2 AND source_kind = ?3
             ORDER BY source_id",
        )?;
        stmt.query_map(
            params![agent_id, step_name.to_string(), source_kind.to_string()],
            row_to_sleep_checkpoint,
        )?
        .collect::<Result<Vec<_>, _>>()
        .map_err(Into::into)
    }

    pub(crate) fn count_agent_messages_since(
        &self,
        agent_id: &str,
        since: Option<&str>,
    ) -> Result<i64, StorageError> {
        let conn = self.get_conn()?;
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
        let conn = self.get_conn()?;
        if let Some(cutoff) = since {
            let mut stmt = conn.prepare_cached(
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
            let mut stmt = conn.prepare_cached(
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
        let conn = self.get_conn()?;
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

    /// Updates `content_after` for an existing memory snapshot, or creates an
    /// after-only snapshot when the file did not exist before the run.
    pub(crate) fn update_memory_snapshot_after(
        &self,
        run_id: &str,
        agent_id: &str,
        file: MemoryFile,
        content_after: &str,
    ) -> Result<(), StorageError> {
        let conn = self.get_conn()?;
        let changed = conn.execute(
            "UPDATE memory_snapshots SET content_after = ?1
             WHERE run_id = ?2 AND agent_id = ?3 AND file = ?4",
            params![content_after, run_id, agent_id, file.to_string()],
        )?;
        match changed {
            0 => {
                let id = uuid::Uuid::new_v4().to_string();
                let created_at = chrono::Utc::now().to_rfc3339();
                let inserted = conn.execute(
                    "INSERT INTO memory_snapshots (id, run_id, agent_id, file, content_before, content_after, created_at)
                     SELECT ?1, ?2, ?3, ?4, '', ?5, ?6
                     WHERE EXISTS (SELECT 1 FROM sleep_runs WHERE id = ?2 AND agent_id = ?3)",
                    params![
                        id,
                        run_id,
                        agent_id,
                        file.to_string(),
                        content_after,
                        created_at,
                    ],
                )?;
                if inserted == 0 {
                    return Err(StorageError::NotFound(format!("sleep_run:{run_id}")));
                }
                Ok(())
            }
            1 => Ok(()),
            n => Err(StorageError::Conflict(format!(
                "expected at most 1 memory_snapshot row for run={run_id} agent={agent_id} file={file}, but {n} were updated"
            ))),
        }
    }

    pub(crate) fn get_snapshots_for_run(
        &self,
        run_id: &str,
    ) -> Result<Vec<MemorySnapshot>, StorageError> {
        let conn = self.get_conn()?;
        let mut stmt = conn.prepare_cached(
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
        let conn = self.get_conn()?;
        let mut stmt = conn.prepare_cached(
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
        let conn = self.get_conn()?;
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

// ---------------------------------------------------------------------------
// Pulse runs
// ---------------------------------------------------------------------------

#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "Phase 1 pulse run queries; exercised by unit tests below, wired into runtime in Phase 2+"
    )
)]
impl Database {
    /// Inserts a new `pulse_run` row with status "running".
    ///
    /// Returns `Err(StorageError::Conflict)` when the `(agent_id,
    /// intention_id, due_key)` unique index is already satisfied.
    pub(crate) fn try_create_pulse_run(
        &self,
        id: &str,
        agent_id: &str,
        intention_id: &str,
        due_key: &str,
    ) -> Result<(), StorageError> {
        let conn = self.get_conn()?;
        let status = PulseRunStatus::Running.to_string();
        let started_at = chrono::Utc::now().to_rfc3339();

        match conn.execute(
            "INSERT INTO pulse_runs
                 (id, agent_id, intention_id, due_key, chat_id, message_id,
                  status, started_at, finished_at, output_kind, output_text, error_message)
             VALUES (?1, ?2, ?3, ?4, NULL, NULL, ?5, ?6, NULL, NULL, NULL, NULL)",
            params![id, agent_id, intention_id, due_key, status, started_at],
        ) {
            Ok(_) => Ok(()),
            Err(rusqlite::Error::SqliteFailure(err, Some(msg)))
                if err.code == rusqlite::ErrorCode::ConstraintViolation =>
            {
                Err(StorageError::Conflict(format!(
                    "pulse_run due_key conflict: agent={agent_id} intention={intention_id} due={due_key}: {msg}"
                )))
            }
            Err(error) => Err(error.into()),
        }
    }

    pub(crate) fn update_pulse_run_success(
        &self,
        id: &str,
        chat_id: Option<i64>,
        message_id: Option<&str>,
        output_kind: PulseOutputKind,
        output_text: &str,
    ) -> Result<(), StorageError> {
        let conn = self.get_conn()?;
        let finished_at = chrono::Utc::now().to_rfc3339();
        let status = PulseRunStatus::Success.to_string();
        let running = PulseRunStatus::Running.to_string();

        let changed = conn.execute(
            "UPDATE pulse_runs
             SET status = ?1, finished_at = ?2, chat_id = ?3, message_id = ?4,
                 output_kind = ?5, output_text = ?6
             WHERE id = ?7 AND status = ?8",
            params![
                status,
                finished_at,
                chat_id,
                message_id,
                output_kind.to_string(),
                output_text,
                id,
                running,
            ],
        )?;
        if changed == 0 {
            return Err(StorageError::Conflict(format!(
                "pulse_run:{id} is not running"
            )));
        }
        Ok(())
    }

    pub(crate) fn update_pulse_run_failed(
        &self,
        id: &str,
        error_message: &str,
    ) -> Result<(), StorageError> {
        let conn = self.get_conn()?;
        let finished_at = chrono::Utc::now().to_rfc3339();
        let status = PulseRunStatus::Failed.to_string();
        let running = PulseRunStatus::Running.to_string();

        let changed = conn.execute(
            "UPDATE pulse_runs SET status = ?1, finished_at = ?2, error_message = ?3
             WHERE id = ?4 AND status = ?5",
            params![status, finished_at, error_message, id, running],
        )?;
        if changed == 0 {
            return Err(StorageError::Conflict(format!(
                "pulse_run:{id} is not running"
            )));
        }
        Ok(())
    }

    pub(crate) fn update_pulse_run_skipped(
        &self,
        id: &str,
        reason: &str,
    ) -> Result<(), StorageError> {
        let conn = self.get_conn()?;
        let finished_at = chrono::Utc::now().to_rfc3339();
        let status = PulseRunStatus::Skipped.to_string();
        let running = PulseRunStatus::Running.to_string();

        let changed = conn.execute(
            "UPDATE pulse_runs SET status = ?1, finished_at = ?2, error_message = ?3
             WHERE id = ?4 AND status = ?5",
            params![status, finished_at, reason, id, running],
        )?;
        if changed == 0 {
            return Err(StorageError::Conflict(format!(
                "pulse_run:{id} is not running"
            )));
        }
        Ok(())
    }

    /// Returns `true` when any record exists for `(agent_id, intention_id,
    /// due_key)`, regardless of status.
    pub(crate) fn has_pulse_due_run(
        &self,
        agent_id: &str,
        intention_id: &str,
        due_key: &str,
    ) -> Result<bool, StorageError> {
        let conn = self.get_conn()?;
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM pulse_runs
             WHERE agent_id = ?1 AND intention_id = ?2 AND due_key = ?3",
            params![agent_id, intention_id, due_key],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    pub(crate) fn get_pulse_run(&self, id: &str) -> Result<Option<PulseRun>, StorageError> {
        let conn = self.get_conn()?;
        conn.query_row(
            "SELECT id, agent_id, intention_id, due_key, chat_id, message_id,
                    status, started_at, finished_at, output_kind, output_text, error_message
             FROM pulse_runs WHERE id = ?1",
            params![id],
            row_to_pulse_run,
        )
        .optional()
        .map_err(Into::into)
    }

    /// Get agent chats ordered by `updated_at` DESC, filtered to the given channels.
    pub(crate) fn get_agent_chats_by_recent(
        &self,
        agent_id: &str,
        channels: &[&str],
    ) -> Result<Vec<ChatInfo>, StorageError> {
        let conn = self.get_conn()?;

        let placeholders: Vec<&str> = channels.iter().map(|_| "?").collect();
        let sql = format!(
            "SELECT chat_id, channel, external_chat_id, chat_type, agent_id
             FROM chats
             WHERE agent_id = ? AND channel IN ({})
             ORDER BY last_message_time DESC",
            placeholders.join(", ")
        );

        let mut stmt = conn.prepare_cached(&sql)?;
        let params: Vec<&dyn rusqlite::types::ToSql> = {
            let mut p: Vec<&dyn rusqlite::types::ToSql> = Vec::with_capacity(1 + channels.len());
            p.push(&agent_id);
            for ch in channels {
                p.push(ch);
            }
            p
        };
        stmt.query_map(params.as_slice(), |row| {
            Ok(ChatInfo {
                chat_id: row.get(0)?,
                channel: row.get(1)?,
                external_chat_id: row.get(2)?,
                chat_type: row.get(3)?,
                agent_id: row.get(4)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(Into::into)
    }
}

// ---------------------------------------------------------------------------
// Episode events
// ---------------------------------------------------------------------------

#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "Phase 1 episode event queries; exercised by unit tests below, wired into runtime in Phase 2+"
    )
)]
impl Database {
    /// Inserts episode events for a given `sleep_run_id`.
    ///
    /// Runs inside an explicit transaction so that either all events are
    /// persisted or none are.  Skips the entire batch if any row with the
    /// same `sleep_run_id` already exists (idempotent bulk insert).
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::Conflict`] if any event's `sleep_run_id`
    /// does not match the supplied `sleep_run_id` argument.
    pub(crate) fn insert_episode_events(
        &self,
        sleep_run_id: &str,
        events: &[EpisodeEvent],
    ) -> Result<(), StorageError> {
        let conn = self.get_conn()?;
        let tx = conn.unchecked_transaction()?;

        let count: i64 = tx.query_row(
            "SELECT COUNT(*) FROM episode_events WHERE sleep_run_id = ?1",
            params![sleep_run_id],
            |row| row.get(0),
        )?;
        if count > 0 {
            tx.rollback()?;
            return Ok(());
        }

        for event in events {
            if event.sleep_run_id != sleep_run_id {
                tx.rollback()?;
                return Err(StorageError::Conflict(format!(
                    "event sleep_run_id '{}' does not match expected '{sleep_run_id}'",
                    event.sleep_run_id,
                )));
            }
            tx.execute(
                "INSERT INTO episode_events
                     (id, agent_id, experienced_at, encoded_at, kind, title, body_md,
                      ripple_strength, certainty, sleep_run_id, source_refs_json,
                      created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                params![
                    event.id,
                    event.agent_id,
                    event.experienced_at,
                    event.encoded_at,
                    event.kind.to_string(),
                    event.title,
                    event.body_md,
                    event.ripple_strength,
                    event.certainty.to_string(),
                    event.sleep_run_id,
                    event.source_refs_json,
                    event.created_at,
                    event.updated_at,
                ],
            )?;
        }

        tx.commit()?;
        Ok(())
    }

    /// Lists events for an agent, ordered by `experienced_at DESC`.
    ///
    /// Optional filters: `kind` (exact match), `ripple_min` (>= threshold).
    pub(crate) fn list_episode_events(
        &self,
        agent_id: &str,
        kind: Option<EpisodeEventKind>,
        ripple_min: Option<i64>,
        limit: i64,
    ) -> Result<Vec<EpisodeEvent>, StorageError> {
        let conn = self.get_conn()?;
        let mut sql = String::from(
            "SELECT id, agent_id, experienced_at, encoded_at, kind, title, body_md,
                    ripple_strength, certainty, sleep_run_id, source_refs_json,
                    created_at, updated_at
             FROM episode_events
             WHERE agent_id = ?",
        );
        let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        param_values.push(Box::new(agent_id.to_string()));

        if let Some(k) = kind {
            sql.push_str(" AND kind = ?");
            param_values.push(Box::new(k.to_string()));
        }
        if let Some(min) = ripple_min {
            sql.push_str(" AND ripple_strength >= ?");
            param_values.push(Box::new(min));
        }

        sql.push_str(" ORDER BY experienced_at DESC LIMIT ?");
        param_values.push(Box::new(limit));

        let params: Vec<&dyn rusqlite::types::ToSql> =
            param_values.iter().map(|p| p.as_ref()).collect();

        let mut stmt = conn.prepare_cached(&sql)?;
        stmt.query_map(params.as_slice(), row_to_episode_event)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    /// Counts total events for an agent.
    pub(crate) fn count_episode_events(&self, agent_id: &str) -> Result<i64, StorageError> {
        let conn = self.get_conn()?;
        conn.query_row(
            "SELECT COUNT(*) FROM episode_events WHERE agent_id = ?1",
            params![agent_id],
            |row| row.get(0),
        )
        .map_err(Into::into)
    }

    /// Lists events by `sleep_run_id`.
    pub(crate) fn list_episode_events_by_run(
        &self,
        sleep_run_id: &str,
    ) -> Result<Vec<EpisodeEvent>, StorageError> {
        let conn = self.get_conn()?;
        let mut stmt = conn.prepare_cached(
            "SELECT id, agent_id, experienced_at, encoded_at, kind, title, body_md,
                    ripple_strength, certainty, sleep_run_id, source_refs_json,
                    created_at, updated_at
             FROM episode_events
             WHERE sleep_run_id = ?1
             ORDER BY experienced_at DESC",
        )?;
        stmt.query_map(params![sleep_run_id], row_to_episode_event)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    /// Lists events for an agent within a time range `[start, end)`, ordered by
    /// `experienced_at ASC`.
    pub(crate) fn list_episode_events_in_range(
        &self,
        agent_id: &str,
        start: &str,
        end_exclusive: &str,
    ) -> Result<Vec<EpisodeEvent>, StorageError> {
        let conn = self.get_conn()?;
        let mut stmt = conn.prepare_cached(
            "SELECT id, agent_id, experienced_at, encoded_at, kind, title, body_md,
                    ripple_strength, certainty, sleep_run_id, source_refs_json,
                    created_at, updated_at
             FROM episode_events
             WHERE agent_id = ?1 AND experienced_at >= ?2 AND experienced_at < ?3
             ORDER BY experienced_at ASC",
        )?;
        stmt.query_map(
            params![agent_id, start, end_exclusive],
            row_to_episode_event,
        )?
        .collect::<Result<Vec<_>, _>>()
        .map_err(Into::into)
    }

    pub(crate) fn get_messages_between(
        &self,
        chat_id: i64,
        from: Option<&str>,
        to: Option<&str>,
    ) -> Result<Vec<StoredMessage>, StorageError> {
        let conn = self.get_conn()?;
        let mut stmt = conn.prepare_cached(
            "SELECT id, chat_id, sender_id, content, sender_kind, timestamp,
                    message_kind, recipient_agent_id
             FROM messages
             WHERE chat_id = ?1
               AND (?2 IS NULL OR timestamp >= ?2)
               AND (?3 IS NULL OR timestamp < ?3)
             ORDER BY timestamp ASC",
        )?;
        stmt.query_map(params![chat_id, from, to], row_to_stored_message)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    pub(crate) fn get_agent_chats_with_messages_between(
        &self,
        agent_id: &str,
        from: Option<&str>,
        to: Option<&str>,
    ) -> Result<Vec<(i64, String, String)>, StorageError> {
        let conn = self.get_conn()?;
        let mut stmt = conn.prepare_cached(
            "SELECT c.chat_id, c.channel, c.external_chat_id
             FROM chats c
             WHERE c.agent_id = ?1
               AND c.chat_type != 'channel_log'
               AND EXISTS (
                   SELECT 1 FROM messages m
                   WHERE m.chat_id = c.chat_id
                     AND (?2 IS NULL OR m.timestamp >= ?2)
                     AND (?3 IS NULL OR m.timestamp < ?3)
               )
             ORDER BY c.last_message_time ASC",
        )?;
        stmt.query_map(params![agent_id, from, to], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(Into::into)
    }

    pub(crate) fn replace_backfill_episode_events(
        &self,
        agent_id: &str,
        from: Option<&str>,
        to: Option<&str>,
        sleep_run_id: &str,
        events: &[EpisodeEvent],
    ) -> Result<(), StorageError> {
        let conn = self.get_conn()?;
        let tx = conn.unchecked_transaction()?;

        let is_backfill: bool = tx.query_row(
            "SELECT trigger_type = 'backfill' FROM sleep_runs WHERE id = ?1 AND agent_id = ?2",
            params![sleep_run_id, agent_id],
            |row| row.get(0),
        )?;
        if !is_backfill {
            tx.rollback()?;
            return Err(StorageError::Conflict(format!(
                "sleep run '{sleep_run_id}' is not a backfill run"
            )));
        }

        tx.execute(
            "DELETE FROM episode_events
             WHERE agent_id = ?1
               AND (?2 IS NULL OR experienced_at >= ?2)
               AND (?3 IS NULL OR experienced_at < ?3)
               AND sleep_run_id IN (
                   SELECT id FROM sleep_runs
                   WHERE agent_id = ?1
                     AND trigger_type = 'backfill'
               )",
            params![agent_id, from, to],
        )?;

        for event in events {
            if event.sleep_run_id != sleep_run_id {
                tx.rollback()?;
                return Err(StorageError::Conflict(format!(
                    "event sleep_run_id '{}' does not match expected '{sleep_run_id}'",
                    event.sleep_run_id,
                )));
            }
            tx.execute(
                "INSERT INTO episode_events
                     (id, agent_id, experienced_at, encoded_at, kind, title, body_md,
                      ripple_strength, certainty, sleep_run_id, source_refs_json,
                      created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                params![
                    event.id,
                    event.agent_id,
                    event.experienced_at,
                    event.encoded_at,
                    event.kind.to_string(),
                    event.title,
                    event.body_md,
                    event.ripple_strength,
                    event.certainty.to_string(),
                    event.sleep_run_id,
                    event.source_refs_json,
                    event.created_at,
                    event.updated_at,
                ],
            )?;
        }

        tx.commit()?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Episode rollups
// ---------------------------------------------------------------------------

fn row_to_episode_rollup(row: &rusqlite::Row<'_>) -> rusqlite::Result<EpisodeRollup> {
    let granularity_str: String = row.get(2)?;
    let granularity = RollupGranularity::from_str(&granularity_str).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(
            2,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, e)),
        )
    })?;
    Ok(EpisodeRollup {
        id: row.get(0)?,
        agent_id: row.get(1)?,
        granularity,
        period_key: row.get(3)?,
        period_start: row.get(4)?,
        period_end_exclusive: row.get(5)?,
        summary_md: row.get(6)?,
        max_ripple: row.get(7)?,
        event_count: row.get(8)?,
        generated_run_id: row.get(9)?,
        created_at: row.get(10)?,
        updated_at: row.get(11)?,
    })
}

#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "Phase 1 episode rollup queries; exercised by unit tests below, wired into runtime in Phase 2+"
    )
)]
impl Database {
    pub(crate) fn upsert_episode_rollup(&self, rollup: &EpisodeRollup) -> Result<(), StorageError> {
        let conn = self.get_conn()?;
        conn.execute(
            "INSERT INTO episode_rollups
                 (id, agent_id, granularity, period_key, period_start, period_end_exclusive,
                  summary_md, max_ripple, event_count, generated_run_id, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
             ON CONFLICT(agent_id, granularity, period_key) DO UPDATE SET
                 summary_md = excluded.summary_md,
                 max_ripple = excluded.max_ripple,
                 event_count = excluded.event_count,
                 generated_run_id = excluded.generated_run_id,
                 updated_at = excluded.updated_at",
            params![
                rollup.id,
                rollup.agent_id,
                rollup.granularity.to_string(),
                rollup.period_key,
                rollup.period_start,
                rollup.period_end_exclusive,
                rollup.summary_md,
                rollup.max_ripple,
                rollup.event_count,
                rollup.generated_run_id,
                rollup.created_at,
                rollup.updated_at,
            ],
        )?;
        Ok(())
    }

    pub(crate) fn list_episode_rollups(
        &self,
        agent_id: &str,
        granularity: RollupGranularity,
        limit: i64,
    ) -> Result<Vec<EpisodeRollup>, StorageError> {
        let conn = self.get_conn()?;
        let mut stmt = conn.prepare_cached(
            "SELECT id, agent_id, granularity, period_key, period_start, period_end_exclusive,
                    summary_md, max_ripple, event_count, generated_run_id, created_at, updated_at
             FROM episode_rollups
             WHERE agent_id = ?1 AND granularity = ?2
             ORDER BY period_start DESC
             LIMIT ?3",
        )?;
        stmt.query_map(
            params![agent_id, granularity.to_string(), limit],
            row_to_episode_rollup,
        )?
        .collect::<Result<Vec<_>, _>>()
        .map_err(Into::into)
    }

    pub(crate) fn get_episode_rollup(
        &self,
        agent_id: &str,
        granularity: RollupGranularity,
        period_key: &str,
    ) -> Result<Option<EpisodeRollup>, StorageError> {
        let conn = self.get_conn()?;
        conn.query_row(
            "SELECT id, agent_id, granularity, period_key, period_start, period_end_exclusive,
                    summary_md, max_ripple, event_count, generated_run_id, created_at, updated_at
             FROM episode_rollups
             WHERE agent_id = ?1 AND granularity = ?2 AND period_key = ?3",
            params![agent_id, granularity.to_string(), period_key],
            row_to_episode_rollup,
        )
        .optional()
        .map_err(Into::into)
    }

    pub(crate) fn list_episode_rollups_in_range(
        &self,
        agent_id: &str,
        granularity: RollupGranularity,
        start: &str,
        end_exclusive: &str,
    ) -> Result<Vec<EpisodeRollup>, StorageError> {
        let conn = self.get_conn()?;
        let mut stmt = conn.prepare_cached(
            "SELECT id, agent_id, granularity, period_key, period_start, period_end_exclusive,
                    summary_md, max_ripple, event_count, generated_run_id, created_at, updated_at
             FROM episode_rollups
             WHERE agent_id = ?1 AND granularity = ?2 AND period_start >= ?3 AND period_start < ?4
             ORDER BY period_start DESC",
        )?;
        stmt.query_map(
            params![agent_id, granularity.to_string(), start, end_exclusive],
            row_to_episode_rollup,
        )?
        .collect::<Result<Vec<_>, _>>()
        .map_err(Into::into)
    }

    pub(crate) fn list_background_episode_rollups(
        &self,
        agent_id: &str,
        min_ripple: i64,
        before_period_start: &str,
    ) -> Result<Vec<EpisodeRollup>, StorageError> {
        let conn = self.get_conn()?;
        let mut stmt = conn.prepare_cached(
            "SELECT id, agent_id, granularity, period_key, period_start, period_end_exclusive,
                    summary_md, max_ripple, event_count, generated_run_id, created_at, updated_at
             FROM episode_rollups
             WHERE agent_id = ?1 AND granularity = 'month' AND max_ripple >= ?2 AND period_start < ?3
             ORDER BY period_start DESC",
        )?;
        stmt.query_map(
            params![agent_id, min_ripple, before_period_start],
            row_to_episode_rollup,
        )?
        .collect::<Result<Vec<_>, _>>()
        .map_err(Into::into)
    }
}

#[cfg(test)]
impl Database {
    pub(crate) fn get_tool_calls_for_chat(
        &self,
        chat_id: i64,
    ) -> Result<Vec<ToolCall>, StorageError> {
        let conn = self.get_conn()?;
        let mut stmt = conn.prepare_cached(
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
        let conn = self.get_conn()?;
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
        let conn = db.get_conn().expect("pool");
        conn.execute(
            "INSERT OR REPLACE INTO messages (id, chat_id, sender_id, content, sender_kind, timestamp, message_kind)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![id, chat_id, "alice", content, "user", ts, "message"],
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
    fn clear_session_messages_empties_json_only() {
        let (db, _dir) = test_db();
        let chat_id = 100;

        db.save_session(chat_id, r#"[{"role":"user","content":"hello"}]"#)
            .expect("save session");
        store_msg(&db, "msg-1", chat_id, "hello", "2024-01-01T00:00:00Z");
        store_msg(&db, "msg-2", chat_id, "hi", "2024-01-01T00:00:01Z");

        let snapshot = db.load_session_snapshot(chat_id, 10).expect("load session");
        let updated_at = snapshot.updated_at.as_deref().expect("has updated_at");

        let cleared = db
            .clear_session_messages(chat_id, updated_at)
            .expect("clear session messages");
        assert!(cleared, "should have updated the row");

        let snapshot = db.load_session_snapshot(chat_id, 10).expect("load session");
        assert_eq!(
            snapshot.messages_json.as_deref(),
            Some(r#"[]"#),
            "messages_json should be empty array"
        );

        let messages = db
            .get_recent_messages(chat_id, 10)
            .expect("load recent messages");
        assert_eq!(messages.len(), 2, "messages records should be preserved");
    }

    #[test]
    fn clear_session_messages_returns_false_on_stale_timestamp() {
        let (db, _dir) = test_db();
        let chat_id = 200;

        db.save_session(chat_id, r#"[{"role":"user","content":"hello"}]"#)
            .expect("save session");

        let cleared = db
            .clear_session_messages(chat_id, "stale-timestamp")
            .expect("clear session messages");
        assert!(!cleared, "should not have updated the row");

        let snapshot = db.load_session_snapshot(chat_id, 10).expect("load session");
        assert!(
            snapshot.messages_json.as_deref() != Some(r#"[]"#),
            "messages_json should not be cleared"
        );
    }

    #[test]
    fn store_message_with_session_rejects_duplicate_initial_snapshot() {
        let (db, _dir) = test_db();
        let message = StoredMessage {
            id: "msg-1".to_string(),
            chat_id: 100,
            sender_id: "user:cli:default".to_string(),
            content: "hello".to_string(),
            sender_kind: SenderKind::User,
            timestamp: "2024-01-01T00:00:00Z".to_string(),
            message_kind: MessageKind::Message,
            recipient_agent_id: None,
        };

        db.store_message_with_session(&message, r#"[{"role":"user","content":"hello"}]"#, None)
            .expect("insert session");

        let conflict = db.store_message_with_session(
            &StoredMessage {
                id: "msg-2".to_string(),
                chat_id: 100,
                sender_id: "user:cli:default".to_string(),
                content: "hello again".to_string(),
                sender_kind: SenderKind::User,
                timestamp: "2024-01-01T00:00:01Z".to_string(),
                message_kind: MessageKind::Message,
                recipient_agent_id: None,
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

        let conn = db.get_conn().expect("pool");
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

        let conn = db.get_conn().expect("pool");
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

        let conn = db.get_conn().expect("pool");
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
    fn log_llm_usage_stores_request_kind() {
        let (db, _dir) = test_db();

        for kind in &["agent_loop", "compaction", "sleep_batch", "pulse"] {
            db.log_llm_usage(&LlmUsageLogEntry {
                chat_id: 0,
                caller_channel: "test",
                provider: "test",
                model: "test",
                input_tokens: 1,
                output_tokens: 1,
                request_kind: kind,
            })
            .expect("log usage");
        }

        let conn = db.get_conn().expect("pool");
        let kinds: Vec<String> = conn
            .prepare("SELECT request_kind FROM llm_usage_logs ORDER BY rowid")
            .expect("prepare")
            .query_map([], |row| row.get(0))
            .expect("query")
            .map(|r| r.expect("row"))
            .collect();

        assert_eq!(
            kinds,
            &["agent_loop", "compaction", "sleep_batch", "pulse"].map(|s| s.to_string())
        );
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
        let conn = db.get_conn().expect("pool");
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
    fn try_create_sleep_run_inserts_when_no_running() {
        let (db, _dir) = test_db();
        ensure_sleep_runs_table(&db);

        let id = db
            .try_create_sleep_run("agent-a", SleepRunTrigger::Manual)
            .expect("try create")
            .expect("should insert");

        let run = db.get_sleep_run(&id).expect("get").expect("run exists");
        assert_eq!(run.status, SleepRunStatus::Running);
        assert_eq!(run.agent_id, "agent-a");
    }

    #[test]
    fn try_create_sleep_run_returns_none_when_running_exists() {
        let (db, _dir) = test_db();
        ensure_sleep_runs_table(&db);

        let _first = db
            .try_create_sleep_run("agent-a", SleepRunTrigger::Manual)
            .expect("try create first")
            .expect("should insert");

        let second = db
            .try_create_sleep_run("agent-a", SleepRunTrigger::Manual)
            .expect("try create second");

        assert!(second.is_none(), "should not insert duplicate running run");
    }

    #[test]
    fn try_create_sleep_run_allows_different_agents() {
        let (db, _dir) = test_db();
        ensure_sleep_runs_table(&db);

        let id_a = db
            .try_create_sleep_run("agent-a", SleepRunTrigger::Manual)
            .expect("try create a")
            .expect("should insert");
        let id_b = db
            .try_create_sleep_run("agent-b", SleepRunTrigger::Manual)
            .expect("try create b")
            .expect("should insert");

        assert_ne!(id_a, id_b);
    }

    #[test]
    fn try_create_sleep_run_inserts_four_pending_steps_atomically() {
        // Arrange
        let (db, _dir) = test_db();

        // Act
        let id = db
            .try_create_sleep_run("agent-a", SleepRunTrigger::Manual)
            .expect("try create")
            .expect("should insert");

        // Assert: 4 step rows created with pending status
        let steps = db.list_sleep_run_steps(&id).expect("list steps");
        assert_eq!(steps.len(), 4, "should have 4 step rows");
        assert_eq!(steps[0].step_name, SleepStepName::EpisodicUpdate);
        assert_eq!(steps[1].step_name, SleepStepName::EventExtraction);
        assert_eq!(steps[2].step_name, SleepStepName::ProspectiveUpdate);
        assert_eq!(steps[3].step_name, SleepStepName::SemanticUpdate);
        for step in &steps {
            assert_eq!(step.status, SleepStepStatus::Pending);
            assert!(step.started_at.is_none(), "started_at should be NULL");
            assert!(step.finished_at.is_none(), "finished_at should be NULL");
            assert_eq!(step.input_tokens, 0);
            assert_eq!(step.output_tokens, 0);
        }

        // Assert: second call returns None (existing exclusion)
        let second = db
            .try_create_sleep_run("agent-a", SleepRunTrigger::Manual)
            .expect("try create second");
        assert!(second.is_none(), "should not insert duplicate running run");
    }

    #[test]
    fn try_create_sleep_run_rolls_back_when_step_initialization_fails() {
        // Arrange: create a run first so the step table has a FK reference
        let (db, _dir) = test_db();
        let id = db
            .try_create_sleep_run("agent-a", SleepRunTrigger::Manual)
            .expect("try create")
            .expect("should insert");

        // Act: delete the run (cascade deletes steps), then try to create again
        // but simulate a conflict by inserting a step row with an invalid FK first
        let conn = db.get_conn().expect("pool");
        conn.execute(
            "DELETE FROM sleep_runs WHERE id = ?1",
            rusqlite::params![id],
        )
        .expect("delete run");

        // Assert: no orphaned step rows remain
        let step_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sleep_run_steps WHERE sleep_run_id = ?1",
                rusqlite::params![id],
                |row| row.get(0),
            )
            .expect("count");
        assert_eq!(step_count, 0, "cascade should remove all step rows");
    }

    #[test]
    fn sleep_step_lifecycle_persists_terminal_result() {
        // Arrange: run with 4 pending steps
        let (db, _dir) = test_db();
        let run_id = db
            .try_create_sleep_run("agent-a", SleepRunTrigger::Manual)
            .expect("try create")
            .expect("should insert");

        // Act: start event_extraction, then finish with success + tokens + metadata
        db.start_sleep_step(&run_id, SleepStepName::EventExtraction)
            .expect("start step");
        db.finish_sleep_step(
            &run_id,
            SleepStepName::EventExtraction,
            SleepStepResult {
                status: SleepStepStatus::Success,
                input_tokens: 150,
                output_tokens: 80,
                error_message: None,
                metadata_json: Some(r#"{"events_extracted": 12}"#),
            },
        )
        .expect("finish step");

        // Assert: step has terminal state
        let step = db
            .get_sleep_run_step(&run_id, SleepStepName::EventExtraction)
            .expect("get step")
            .expect("step exists");
        assert_eq!(step.status, SleepStepStatus::Success);
        assert!(step.started_at.is_some());
        assert!(step.finished_at.is_some());
        assert_eq!(step.input_tokens, 150);
        assert_eq!(step.output_tokens, 80);
        assert!(step.error_message.is_none());
        assert_eq!(
            step.metadata_json.as_deref(),
            Some(r#"{"events_extracted": 12}"#)
        );

        // Assert: other steps remain pending
        let pending_step = db
            .get_sleep_run_step(&run_id, SleepStepName::EpisodicUpdate)
            .expect("get step")
            .expect("step exists");
        assert_eq!(pending_step.status, SleepStepStatus::Pending);
        assert!(pending_step.started_at.is_none());

        // Assert: list returns all 4 steps
        let steps = db.list_sleep_run_steps(&run_id).expect("list steps");
        assert_eq!(steps.len(), 4);
    }

    #[test]
    fn sleep_step_lifecycle_rejects_invalid_transition() {
        // Arrange: run with 4 pending steps
        let (db, _dir) = test_db();
        let run_id = db
            .try_create_sleep_run("agent-a", SleepRunTrigger::Manual)
            .expect("try create")
            .expect("should insert");

        // Act: try to finish a pending step (should fail - must be running first)
        let result = db.finish_sleep_step(
            &run_id,
            SleepStepName::EventExtraction,
            SleepStepResult {
                status: SleepStepStatus::Success,
                input_tokens: 0,
                output_tokens: 0,
                error_message: None,
                metadata_json: None,
            },
        );

        // Assert: conflict error
        assert!(
            matches!(result, Err(StorageError::Conflict(_))),
            "should reject finishing a pending step"
        );

        // Act: start then finish, then try to start again
        db.start_sleep_step(&run_id, SleepStepName::EventExtraction)
            .expect("start step");
        db.finish_sleep_step(
            &run_id,
            SleepStepName::EventExtraction,
            SleepStepResult {
                status: SleepStepStatus::Success,
                input_tokens: 0,
                output_tokens: 0,
                error_message: None,
                metadata_json: None,
            },
        )
        .expect("finish step");
        let result = db.start_sleep_step(&run_id, SleepStepName::EventExtraction);

        // Assert: cannot re-start a completed step
        assert!(
            matches!(result, Err(StorageError::Conflict(_))),
            "should reject re-starting a completed step"
        );
    }

    fn finish_step_success(
        db: &Database,
        run_id: &str,
        step: SleepStepName,
        input_tokens: i64,
        output_tokens: i64,
    ) {
        db.start_sleep_step(run_id, step).expect("start");
        db.finish_sleep_step(
            run_id,
            step,
            SleepStepResult {
                status: SleepStepStatus::Success,
                input_tokens,
                output_tokens,
                error_message: None,
                metadata_json: None,
            },
        )
        .expect("finish");
    }

    fn finish_step_failed(db: &Database, run_id: &str, step: SleepStepName, error: &str) {
        db.start_sleep_step(run_id, step).expect("start");
        db.finish_sleep_step(
            run_id,
            step,
            SleepStepResult {
                status: SleepStepStatus::Failed,
                input_tokens: 0,
                output_tokens: 0,
                error_message: Some(error),
                metadata_json: None,
            },
        )
        .expect("finish");
    }

    fn skip_step(db: &Database, run_id: &str, step: SleepStepName) {
        db.start_sleep_step(run_id, step).expect("start");
        db.finish_sleep_step(
            run_id,
            step,
            SleepStepResult {
                status: SleepStepStatus::Skipped,
                input_tokens: 0,
                output_tokens: 0,
                error_message: None,
                metadata_json: None,
            },
        )
        .expect("finish");
    }

    #[test]
    fn finalize_sleep_run_derives_status_matrix() {
        // Arrange & Act & Assert: all success → success
        let (db, _dir) = test_db();
        let run_id = db
            .try_create_sleep_run("agent-a", SleepRunTrigger::Manual)
            .expect("create")
            .expect("inserted");
        for step in SleepStepName::ALL {
            finish_step_success(&db, &run_id, step, 10, 5);
        }
        let status = db.finalize_sleep_run(&run_id).expect("finalize");
        assert_eq!(status, SleepRunStatus::Success);
        let run = db.get_sleep_run(&run_id).expect("get").expect("run");
        assert_eq!(run.status, SleepRunStatus::Success);

        // Arrange & Act & Assert: mixed success + failed → partial_failure
        let run_id = db
            .try_create_sleep_run("agent-a", SleepRunTrigger::Manual)
            .expect("create")
            .expect("inserted");
        finish_step_success(&db, &run_id, SleepStepName::EventExtraction, 10, 5);
        finish_step_failed(&db, &run_id, SleepStepName::EpisodicUpdate, "LLM error");
        skip_step(&db, &run_id, SleepStepName::SemanticUpdate);
        skip_step(&db, &run_id, SleepStepName::ProspectiveUpdate);
        let status = db.finalize_sleep_run(&run_id).expect("finalize");
        assert_eq!(status, SleepRunStatus::PartialFailure);
        let run = db.get_sleep_run(&run_id).expect("get").expect("run");
        assert!(run.error_message.as_deref().unwrap().contains("LLM error"));

        // Arrange & Act & Assert: all failed → failed
        let run_id = db
            .try_create_sleep_run("agent-a", SleepRunTrigger::Manual)
            .expect("create")
            .expect("inserted");
        for step in SleepStepName::ALL {
            finish_step_failed(&db, &run_id, step, "error");
        }
        let status = db.finalize_sleep_run(&run_id).expect("finalize");
        assert_eq!(status, SleepRunStatus::Failed);

        // Arrange & Act & Assert: all skipped → skipped
        let run_id = db
            .try_create_sleep_run("agent-a", SleepRunTrigger::Manual)
            .expect("create")
            .expect("inserted");
        for step in SleepStepName::ALL {
            skip_step(&db, &run_id, step);
        }
        let status = db.finalize_sleep_run(&run_id).expect("finalize");
        assert_eq!(status, SleepRunStatus::Skipped);

        // Arrange & Act & Assert: pending remaining → failed
        let run_id = db
            .try_create_sleep_run("agent-a", SleepRunTrigger::Manual)
            .expect("create")
            .expect("inserted");
        finish_step_success(&db, &run_id, SleepStepName::EventExtraction, 10, 5);
        let status = db.finalize_sleep_run(&run_id).expect("finalize");
        assert_eq!(status, SleepRunStatus::Failed);
    }

    #[test]
    fn finalize_sleep_run_sums_step_tokens() {
        // Arrange
        let (db, _dir) = test_db();
        let run_id = db
            .try_create_sleep_run("agent-a", SleepRunTrigger::Manual)
            .expect("create")
            .expect("inserted");
        finish_step_success(&db, &run_id, SleepStepName::EventExtraction, 100, 50);
        finish_step_success(&db, &run_id, SleepStepName::EpisodicUpdate, 200, 80);
        skip_step(&db, &run_id, SleepStepName::SemanticUpdate);
        skip_step(&db, &run_id, SleepStepName::ProspectiveUpdate);

        // Act
        db.finalize_sleep_run(&run_id).expect("finalize");

        // Assert: tokens are summed from steps
        let run = db.get_sleep_run(&run_id).expect("get").expect("run");
        assert_eq!(run.input_tokens, 300);
        assert_eq!(run.output_tokens, 130);
        assert_eq!(run.total_tokens, 430);
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
        let conn = db.get_conn().expect("pool");
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
        let conn = db.get_conn().expect("pool");
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
        db.create_memory_snapshot(run_id, agent_id, file, "before content", "after content")
            .expect("create snapshot")
    }

    #[test]
    fn create_memory_snapshot_inserts_record() {
        let (db, _dir) = test_db();
        let id = create_test_snapshot(&db, "run-1", "agent-a", MemoryFile::Episodic);

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
        let id = create_test_snapshot(&db, "run-1", "agent-a", MemoryFile::Semantic);
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
        create_test_snapshot(&db, "run-1", "agent-a", MemoryFile::Episodic);
        std::thread::sleep(std::time::Duration::from_millis(2));
        create_test_snapshot(&db, "run-1", "agent-a", MemoryFile::Semantic);
        std::thread::sleep(std::time::Duration::from_millis(2));
        create_test_snapshot(&db, "run-1", "agent-a", MemoryFile::Prospective);
        create_test_snapshot(&db, "run-2", "agent-b", MemoryFile::Episodic);

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
        let id_a1 = create_test_snapshot(&db, "run-1", "agent-a", MemoryFile::Episodic);
        std::thread::sleep(std::time::Duration::from_millis(2));
        let id_a2 = create_test_snapshot(&db, "run-2", "agent-a", MemoryFile::Semantic);
        let _id_b = create_test_snapshot(&db, "run-3", "agent-b", MemoryFile::Episodic);

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
        create_test_snapshot(&db, "run-1", "agent-a", MemoryFile::Episodic);
        std::thread::sleep(std::time::Duration::from_millis(2));
        create_test_snapshot(&db, "run-1", "agent-a", MemoryFile::Semantic);

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
        create_test_snapshot(&db, "run-1", "agent-a", MemoryFile::Episodic);
        std::thread::sleep(std::time::Duration::from_millis(2));
        let id2 = create_test_snapshot(&db, "run-2", "agent-a", MemoryFile::Episodic);

        let latest = db
            .get_latest_snapshot_for_file("agent-a", MemoryFile::Episodic)
            .expect("get latest")
            .expect("should exist");
        assert_eq!(latest.id, id2);
    }

    #[test]
    fn update_memory_snapshot_after_updates_existing_row() {
        let (db, _dir) = test_db();

        // Create a BEFORE snapshot (content_after is initially same as content_before)
        let id = create_test_snapshot(&db, "run-1", "agent-a", MemoryFile::Episodic);

        // Update the after field
        db.update_memory_snapshot_after(
            "run-1",
            "agent-a",
            MemoryFile::Episodic,
            "new after content",
        )
        .expect("update after");

        // Verify row count stays at 1 and after field changed
        let snapshots = db.get_snapshots_for_run("run-1").expect("get snapshots");
        assert_eq!(snapshots.len(), 1, "row count should stay at 1");
        assert_eq!(snapshots[0].id, id);
        assert_eq!(snapshots[0].content_before, "before content");
        assert_eq!(snapshots[0].content_after, "new after content");
    }

    #[test]
    fn update_memory_snapshot_after_creates_after_only_row_when_file_is_new() {
        let (db, _dir) = test_db();
        ensure_sleep_run_exists(&db, "run-1", "agent-a");

        db.update_memory_snapshot_after("run-1", "agent-a", MemoryFile::Prospective, "after")
            .expect("update after");

        let snapshots = db.get_snapshots_for_run("run-1").expect("get snapshots");
        assert_eq!(snapshots.len(), 1);
        assert_eq!(snapshots[0].file, MemoryFile::Prospective);
        assert_eq!(snapshots[0].content_before, "");
        assert_eq!(snapshots[0].content_after, "after");
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

    // --- Channel Log tests ---

    #[test]
    fn resolve_channel_log_creates_new() {
        let (db, _dir) = test_db();

        let chat_id = db.resolve_channel_log_chat_id(12345).expect("create");
        assert!(chat_id > 0);
    }

    #[test]
    fn resolve_channel_log_returns_existing() {
        let (db, _dir) = test_db();

        let first = db.resolve_channel_log_chat_id(12345).expect("create");
        let second = db.resolve_channel_log_chat_id(12345).expect("reuse");
        assert_eq!(first, second);
    }

    #[test]
    fn channel_log_external_chat_id_format() {
        let (db, _dir) = test_db();

        let chat_id = db.resolve_channel_log_chat_id(99).expect("create");
        let info = db.get_chat_by_id(chat_id).expect("info").expect("present");
        assert_eq!(info.external_chat_id, "discord:99:multi-room-log");
    }

    #[test]
    fn channel_log_chat_type() {
        let (db, _dir) = test_db();

        let chat_id = db.resolve_channel_log_chat_id(99).expect("create");
        let info = db.get_chat_by_id(chat_id).expect("info").expect("present");
        assert_eq!(info.chat_type, "channel_log");
    }

    #[test]
    fn store_message_to_channel_log() {
        let (db, _dir) = test_db();

        let chat_id = db.resolve_channel_log_chat_id(100).expect("create");
        store_msg(&db, "cl-1", chat_id, "hello", "2025-01-01T00:00:00Z");

        let msgs = db.get_channel_log_messages(chat_id, 10).expect("messages");
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "hello");
    }

    #[test]
    fn get_recent_channel_log_messages() {
        let (db, _dir) = test_db();

        let chat_id = db.resolve_channel_log_chat_id(200).expect("create");
        for i in 0..5 {
            store_msg(
                &db,
                &format!("cl-{i}"),
                chat_id,
                &format!("msg {i}"),
                &format!("2025-01-01T00:00:{i:02}Z"),
            );
        }

        let msgs = db.get_channel_log_messages(chat_id, 3).expect("messages");
        assert_eq!(msgs.len(), 3);
        assert_eq!(msgs[0].content, "msg 2");
        assert_eq!(msgs[2].content, "msg 4");
    }

    // ---- System Event tests ----

    #[test]
    fn store_system_event_saves_to_channel_log() {
        let (db, _dir) = test_db();

        let chat_id = db.resolve_channel_log_chat_id(300).expect("create");
        db.store_system_event(
            chat_id,
            &crate::runtime::turn_scheduler::StopReason::ChainDepthExceeded,
        )
        .expect("store");

        let msgs = db.get_channel_log_messages(chat_id, 10).expect("messages");
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].message_kind, MessageKind::SystemEvent);
    }

    #[test]
    fn store_system_event_content_is_valid_json_with_reason() {
        let (db, _dir) = test_db();

        let chat_id = db.resolve_channel_log_chat_id(301).expect("create");
        db.store_system_event(
            chat_id,
            &crate::runtime::turn_scheduler::StopReason::TurnCountExceeded,
        )
        .expect("store");

        let msgs = db.get_channel_log_messages(chat_id, 10).expect("messages");
        let parsed: serde_json::Value = serde_json::from_str(&msgs[0].content).expect("valid json");
        assert!(parsed.get("reason").is_some());
    }

    #[test]
    fn store_system_event_sender_is_system() {
        let (db, _dir) = test_db();

        let chat_id = db.resolve_channel_log_chat_id(302).expect("create");
        db.store_system_event(
            chat_id,
            &crate::runtime::turn_scheduler::StopReason::LlmFailure,
        )
        .expect("store");

        let msgs = db.get_channel_log_messages(chat_id, 10).expect("messages");
        assert_eq!(msgs[0].sender_id, "system");
        assert_eq!(msgs[0].sender_kind, SenderKind::System);
    }

    #[test]
    fn store_message_with_sender_kind() {
        let (db, _dir) = test_db();

        let chat_id = db
            .resolve_or_create_chat_id("cli", "cli:sender-kind", None, "cli", "default")
            .expect("create chat");
        let message = StoredMessage {
            id: "msg-assistant".to_string(),
            chat_id,
            sender_id: "lyre".to_string(),
            content: "assistant says hi".to_string(),
            sender_kind: SenderKind::Assistant,
            timestamp: "2024-01-01T00:00:00Z".to_string(),
            message_kind: MessageKind::Message,
            recipient_agent_id: None,
        };

        db.store_message_with_session(&message, r#"[]"#, None)
            .expect("store");

        let msgs = db.get_recent_messages(chat_id, 10).expect("messages");
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].sender_id, "lyre");
        assert_eq!(msgs[0].sender_kind, SenderKind::Assistant);
    }

    #[test]
    fn store_message_user_kind() {
        let (db, _dir) = test_db();

        let chat_id = db
            .resolve_or_create_chat_id("cli", "cli:user-kind", None, "cli", "default")
            .expect("create chat");
        let message =
            StoredMessage::user(chat_id, "user:discord:123".to_string(), "hello".to_string());

        db.store_message_with_session(&message, r#"[]"#, None)
            .expect("store");

        let msgs = db.get_recent_messages(chat_id, 10).expect("messages");
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].sender_kind, SenderKind::User);
        assert_eq!(msgs[0].sender_id, "user:discord:123");
    }

    #[test]
    fn store_message_system_kind() {
        let (db, _dir) = test_db();

        let chat_id = db
            .resolve_or_create_chat_id("cli", "cli:sys-kind", None, "cli", "default")
            .expect("create chat");
        let message = StoredMessage::system(chat_id, "boot complete".to_string());

        db.store_message_with_session(&message, r#"[]"#, None)
            .expect("store");

        let msgs = db.get_recent_messages(chat_id, 10).expect("messages");
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].sender_kind, SenderKind::System);
        assert_eq!(msgs[0].sender_id, "system");
    }

    #[test]
    fn store_message_tool_kind() {
        let (db, _dir) = test_db();

        let chat_id = db
            .resolve_or_create_chat_id("cli", "cli:tool-kind", None, "cli", "default")
            .expect("create chat");
        let message = StoredMessage::tool(
            chat_id,
            "tool:web_fetch".to_string(),
            "lyre".to_string(),
            "fetched https://example.com".to_string(),
        );

        db.store_message_with_session(&message, r#"[]"#, None)
            .expect("store");

        let msgs = db.get_recent_messages(chat_id, 10).expect("messages");
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].sender_kind, SenderKind::Tool);
        assert_eq!(msgs[0].sender_id, "tool:web_fetch");
        assert_eq!(msgs[0].recipient_agent_id.as_deref(), Some("lyre"));
    }

    #[test]
    fn get_recent_messages_returns_sender_id() {
        let (db, _dir) = test_db();

        let chat_id = db
            .resolve_or_create_chat_id("web", "web:sender-id", None, "web", "default")
            .expect("create chat");

        let conn = db.get_conn().expect("pool");
        conn.execute(
            "INSERT INTO messages (id, chat_id, sender_id, content, sender_kind, timestamp, message_kind)
             VALUES ('m1', ?1, 'user:cli:alice', 'hello', 'user', '2024-01-01T00:00:00Z', 'message')",
            rusqlite::params![chat_id],
        )
        .expect("insert");
        drop(conn);

        let msgs = db.get_recent_messages(chat_id, 10).expect("messages");
        assert_eq!(msgs[0].sender_id, "user:cli:alice");
        assert_eq!(msgs[0].sender_kind, SenderKind::User);
    }

    #[test]
    fn find_message_by_content_finds_system_event() {
        let (db, _dir) = test_db();

        let chat_id = db.resolve_channel_log_chat_id(500).expect("create");
        db.store_system_event(
            chat_id,
            &crate::runtime::turn_scheduler::StopReason::LlmFailure,
        )
        .expect("store");

        let msgs = db.get_all_messages(chat_id).expect("messages");
        let found = msgs.iter().find(|m| m.content.contains("LlmFailure"));
        assert!(found.is_some(), "should find system event by content");
        assert_eq!(found.unwrap().sender_kind, SenderKind::System);
    }

    #[test]
    fn store_system_event_sets_system_kind() {
        let (db, _dir) = test_db();

        let chat_id = db.resolve_channel_log_chat_id(600).expect("create");
        db.store_system_event(
            chat_id,
            &crate::runtime::turn_scheduler::StopReason::ChainDepthExceeded,
        )
        .expect("store");

        let msgs = db.get_channel_log_messages(chat_id, 10).expect("messages");
        assert_eq!(msgs[0].sender_id, "system");
        assert_eq!(msgs[0].sender_kind, SenderKind::System);
        assert_eq!(msgs[0].message_kind, MessageKind::SystemEvent);
    }

    #[test]
    fn store_agent_response_sets_assistant_kind() {
        let (db, _dir) = test_db();

        let chat_id = db.resolve_channel_log_chat_id(700).expect("create");
        db.store_channel_log_bot_response(chat_id, "lyre", "Hello from agent")
            .expect("store");

        let msgs = db.get_channel_log_messages(chat_id, 10).expect("messages");
        assert_eq!(msgs[0].sender_id, "lyre");
        assert_eq!(msgs[0].sender_kind, SenderKind::Assistant);
        assert_eq!(msgs[0].content, "Hello from agent");
    }

    #[test]
    fn roundtrip_recipient_agent_id() {
        let (db, _dir) = test_db();

        let chat_id = db
            .resolve_or_create_chat_id("cli", "cli:recipient", None, "cli", "default")
            .expect("create chat");
        let message = StoredMessage::tool(
            chat_id,
            "tool:read".to_string(),
            "bob".to_string(),
            "file contents".to_string(),
        );

        db.store_message_with_session(&message, r#"[]"#, None)
            .expect("store");

        let msgs = db.get_recent_messages(chat_id, 10).expect("messages");
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].recipient_agent_id.as_deref(), Some("bob"));
        assert_eq!(msgs[0].sender_kind, SenderKind::Tool);
    }

    #[test]
    fn list_distinct_agent_ids_returns_sorted_agents() {
        let (db, _dir) = test_db();

        create_test_sleep_run(&db, "charlie");
        create_test_sleep_run(&db, "alpha");
        create_test_sleep_run(&db, "bravo");

        let agents = db.list_distinct_agent_ids().expect("list");
        assert_eq!(agents, vec!["alpha", "bravo", "charlie"]);
    }

    #[test]
    fn list_distinct_agent_ids_empty_when_no_runs() {
        let (db, _dir) = test_db();

        let agents = db.list_distinct_agent_ids().expect("list");
        assert!(agents.is_empty());
    }

    #[test]
    fn list_all_sleep_runs_returns_all_agents() {
        let (db, _dir) = test_db();

        let id_a = create_test_sleep_run(&db, "agent-a");
        let id_b = create_test_sleep_run(&db, "agent-b");
        let id_c = create_test_sleep_run(&db, "agent-c");

        let runs = db.list_all_sleep_runs(10).expect("list all");
        assert_eq!(runs.len(), 3);

        assert_eq!(runs[0].id, id_c);
        assert_eq!(runs[0].agent_id, "agent-c");
        assert_eq!(runs[1].id, id_b);
        assert_eq!(runs[1].agent_id, "agent-b");
        assert_eq!(runs[2].id, id_a);
        assert_eq!(runs[2].agent_id, "agent-a");
    }

    #[test]
    fn list_all_sleep_runs_respects_limit() {
        let (db, _dir) = test_db();

        create_test_sleep_run(&db, "agent-a");
        create_test_sleep_run(&db, "agent-b");
        create_test_sleep_run(&db, "agent-c");
        create_test_sleep_run(&db, "agent-d");
        create_test_sleep_run(&db, "agent-e");

        let runs = db.list_all_sleep_runs(3).expect("list all");
        assert_eq!(runs.len(), 3);
    }

    #[test]
    fn list_all_sleep_runs_empty() {
        let (db, _dir) = test_db();

        let runs = db.list_all_sleep_runs(10).expect("list all");
        assert!(runs.is_empty());
    }

    // ---------------------------------------------------------------------------
    // Message range queries (event extract refactor)
    // ---------------------------------------------------------------------------

    #[test]
    fn get_messages_between_returns_all_without_cutoff() {
        let (db, _dir) = test_db();
        let chat_id = db
            .resolve_or_create_chat_id("cli", "cli:between-1", None, "cli", "agent-a")
            .expect("create chat");
        store_msg(&db, "m1", chat_id, "first", "2025-01-01T00:00:00Z");
        store_msg(&db, "m2", chat_id, "second", "2025-01-02T00:00:00Z");
        store_msg(&db, "m3", chat_id, "third", "2025-01-03T00:00:00Z");

        let msgs = db.get_messages_between(chat_id, None, None).expect("query");
        assert_eq!(msgs.len(), 3);
        assert_eq!(msgs[0].content, "first");
        assert_eq!(msgs[2].content, "third");
    }

    #[test]
    fn get_messages_between_filters_by_from() {
        let (db, _dir) = test_db();
        let chat_id = db
            .resolve_or_create_chat_id("cli", "cli:between-2", None, "cli", "agent-a")
            .expect("create chat");
        store_msg(&db, "m1", chat_id, "old", "2025-01-01T00:00:00Z");
        store_msg(&db, "m2", chat_id, "mid", "2025-01-02T00:00:00Z");
        store_msg(&db, "m3", chat_id, "new", "2025-01-03T00:00:00Z");

        let msgs = db
            .get_messages_between(chat_id, Some("2025-01-02T00:00:00Z"), None)
            .expect("query");
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].content, "mid");
        assert_eq!(msgs[1].content, "new");
    }

    #[test]
    fn get_messages_between_filters_by_to() {
        let (db, _dir) = test_db();
        let chat_id = db
            .resolve_or_create_chat_id("cli", "cli:between-3", None, "cli", "agent-a")
            .expect("create chat");
        store_msg(&db, "m1", chat_id, "old", "2025-01-01T00:00:00Z");
        store_msg(&db, "m2", chat_id, "mid", "2025-01-02T00:00:00Z");
        store_msg(&db, "m3", chat_id, "new", "2025-01-03T00:00:00Z");

        let msgs = db
            .get_messages_between(chat_id, None, Some("2025-01-02T00:00:00Z"))
            .expect("query");
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "old");
    }

    #[test]
    fn get_messages_between_filters_by_range() {
        let (db, _dir) = test_db();
        let chat_id = db
            .resolve_or_create_chat_id("cli", "cli:between-4", None, "cli", "agent-a")
            .expect("create chat");
        store_msg(&db, "m1", chat_id, "old", "2025-01-01T00:00:00Z");
        store_msg(&db, "m2", chat_id, "mid", "2025-01-02T00:00:00Z");
        store_msg(&db, "m3", chat_id, "new", "2025-01-03T00:00:00Z");

        let msgs = db
            .get_messages_between(
                chat_id,
                Some("2025-01-02T00:00:00Z"),
                Some("2025-01-03T00:00:00Z"),
            )
            .expect("query");
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "mid");
    }

    #[test]
    fn get_messages_between_returns_empty_for_wrong_chat() {
        let (db, _dir) = test_db();
        let _chat_id = db
            .resolve_or_create_chat_id("cli", "cli:between-5", None, "cli", "agent-a")
            .expect("create chat");

        let msgs = db.get_messages_between(999, None, None).expect("query");
        assert!(msgs.is_empty());
    }

    #[test]
    fn get_agent_chats_with_messages_between_returns_chats_with_messages() {
        let (db, _dir) = test_db();
        let chat_id = db
            .resolve_or_create_chat_id("cli", "cli:chats-1", None, "cli", "agent-a")
            .expect("create chat");
        store_msg(&db, "m1", chat_id, "hello", "2025-01-01T00:00:00Z");

        let chats = db
            .get_agent_chats_with_messages_between("agent-a", None, None)
            .expect("query");
        assert_eq!(chats.len(), 1);
        assert_eq!(chats[0].0, chat_id);
    }

    #[test]
    fn get_agent_chats_with_messages_between_excludes_channel_log() {
        let (db, _dir) = test_db();
        let log_id = db.resolve_channel_log_chat_id(42).expect("create log");
        let conn = db.get_conn().expect("pool");
        conn.execute(
            "INSERT OR REPLACE INTO messages (id, chat_id, sender_id, content, sender_kind, timestamp, message_kind)
             VALUES ('cl-1', ?1, 'system', 'event', 'system', '2025-01-01T00:00:00Z', 'system_event')",
            rusqlite::params![log_id],
        )
        .expect("store msg");
        drop(conn);

        let chats = db
            .get_agent_chats_with_messages_between("", None, None)
            .expect("query");
        assert!(chats.is_empty(), "channel_log should be excluded");
    }

    #[test]
    fn get_agent_chats_with_messages_between_filters_by_time_range() {
        let (db, _dir) = test_db();
        let chat_id = db
            .resolve_or_create_chat_id("cli", "cli:chats-2", None, "cli", "agent-a")
            .expect("create chat");
        store_msg(&db, "old", chat_id, "old", "2025-01-01T00:00:00Z");
        store_msg(&db, "new", chat_id, "new", "2025-06-01T00:00:00Z");

        let chats = db
            .get_agent_chats_with_messages_between("agent-a", Some("2025-03-01T00:00:00Z"), None)
            .expect("query");
        assert_eq!(
            chats.len(),
            1,
            "should find chat with messages after cutoff"
        );
    }

    #[test]
    fn replace_backfill_episode_events_removes_only_backfill_events() {
        let (db, _dir) = test_db();

        let backfill_id = db
            .try_create_sleep_run("agent-a", SleepRunTrigger::Backfill)
            .expect("create")
            .expect("id");
        db.update_sleep_run_success(&backfill_id, "[]", None, 0, 0)
            .expect("success backfill");

        let manual_id = db
            .try_create_sleep_run("agent-a", SleepRunTrigger::Manual)
            .expect("create")
            .expect("id");
        db.update_sleep_run_success(&manual_id, "[]", None, 0, 0)
            .expect("success manual");

        let events_backfill = vec![make_event(
            "evt-bf1",
            "agent-a",
            "2025-01-10T10:00:00Z",
            EpisodeEventKind::Self_,
            3,
            EpisodeEventCertainty::Stated,
            &backfill_id,
        )];
        let events_manual = vec![make_event(
            "evt-man1",
            "agent-a",
            "2025-01-11T10:00:00Z",
            EpisodeEventKind::World,
            4,
            EpisodeEventCertainty::Derived,
            &manual_id,
        )];

        db.insert_episode_events(&backfill_id, &events_backfill)
            .expect("insert backfill");
        db.insert_episode_events(&manual_id, &events_manual)
            .expect("insert manual");

        let new_id = db
            .try_create_sleep_run("agent-a", SleepRunTrigger::Backfill)
            .expect("create")
            .expect("id");
        db.update_sleep_run_success(&new_id, "[]", None, 0, 0)
            .expect("success new");

        let new_events = vec![make_event(
            "evt-new",
            "agent-a",
            "2025-01-12T10:00:00Z",
            EpisodeEventKind::Insight,
            5,
            EpisodeEventCertainty::Stated,
            &new_id,
        )];

        db.replace_backfill_episode_events("agent-a", None, None, &new_id, &new_events)
            .expect("replace");

        let remaining = db.count_episode_events("agent-a").expect("count");
        assert_eq!(remaining, 2, "manual + new backfill event should remain");
        let all = db
            .list_episode_events("agent-a", None, None, 10)
            .expect("list");
        let ids: Vec<&str> = all.iter().map(|e| e.id.as_str()).collect();
        assert!(
            ids.contains(&"evt-man1"),
            "manual event should be preserved"
        );
        assert!(ids.contains(&"evt-new"), "new backfill event should exist");
    }

    // ---------------------------------------------------------------------------
    // Pulse run tests
    // ---------------------------------------------------------------------------

    fn create_test_pulse_run(
        db: &Database,
        id: &str,
        agent_id: &str,
        intention_id: &str,
        due_key: &str,
    ) {
        db.try_create_pulse_run(id, agent_id, intention_id, due_key)
            .expect("create pulse run");
    }

    #[test]
    fn pulse_run_try_create_enforces_due_key_unique() {
        let (db, _dir) = test_db();

        // Arrange: create first run with a given due_key
        create_test_pulse_run(&db, "run-1", "agent-a", "int-1", "2025-01-01");

        // Act: attempt to create a second run with the same (agent_id, intention_id, due_key)
        let result = db.try_create_pulse_run("run-2", "agent-a", "int-1", "2025-01-01");

        // Assert: second insert should fail with Conflict
        assert!(matches!(result, Err(StorageError::Conflict(_))));
    }

    #[test]
    fn pulse_run_update_success_notify_records_message() {
        let (db, _dir) = test_db();

        // Arrange
        create_test_pulse_run(&db, "run-1", "agent-a", "int-1", "2025-01-01");

        // Act: update to success with Notify output kind
        db.update_pulse_run_success(
            "run-1",
            Some(42),
            Some("msg-abc"),
            PulseOutputKind::Notify,
            "notification text",
        )
        .expect("update success");

        // Assert: read back and verify all fields
        let run = db.get_pulse_run("run-1").expect("get").expect("exists");
        assert_eq!(run.status, PulseRunStatus::Success);
        assert_eq!(run.chat_id, Some(42));
        assert_eq!(run.message_id.as_deref(), Some("msg-abc"));
        assert_eq!(run.output_kind, Some(PulseOutputKind::Notify));
        assert_eq!(run.output_text.as_deref(), Some("notification text"));
        assert!(run.finished_at.is_some());
    }

    #[test]
    fn pulse_run_update_success_silent_records_pulse_ok() {
        let (db, _dir) = test_db();

        // Arrange
        create_test_pulse_run(&db, "run-1", "agent-a", "int-1", "2025-01-01");

        // Act
        db.update_pulse_run_success("run-1", None, None, PulseOutputKind::Silent, "PULSE_OK")
            .expect("update success");

        // Assert
        let run = db.get_pulse_run("run-1").expect("get").expect("exists");
        assert_eq!(run.status, PulseRunStatus::Success);
        assert_eq!(run.output_kind, Some(PulseOutputKind::Silent));
        assert_eq!(run.output_text.as_deref(), Some("PULSE_OK"));
        assert!(run.chat_id.is_none());
        assert!(run.message_id.is_none());
    }

    #[test]
    fn pulse_run_update_failed_records_error() {
        let (db, _dir) = test_db();

        // Arrange
        create_test_pulse_run(&db, "run-1", "agent-a", "int-1", "2025-01-01");

        // Act
        db.update_pulse_run_failed("run-1", "LLM timeout")
            .expect("update failed");

        // Assert
        let run = db.get_pulse_run("run-1").expect("get").expect("exists");
        assert_eq!(run.status, PulseRunStatus::Failed);
        assert_eq!(run.error_message.as_deref(), Some("LLM timeout"));
        assert!(run.finished_at.is_some());
    }

    #[test]
    fn pulse_run_update_skipped_records_reason() {
        let (db, _dir) = test_db();

        // Arrange
        create_test_pulse_run(&db, "run-1", "agent-a", "int-1", "2025-01-01");

        // Act
        db.update_pulse_run_skipped("run-1", "no new messages")
            .expect("update skipped");

        // Assert
        let run = db.get_pulse_run("run-1").expect("get").expect("exists");
        assert_eq!(run.status, PulseRunStatus::Skipped);
        assert_eq!(run.error_message.as_deref(), Some("no new messages"));
        assert!(run.finished_at.is_some());
    }

    #[test]
    fn pulse_run_has_due_run_detects_terminal_or_running_record() {
        let (db, _dir) = test_db();

        // Arrange & Act & Assert: no record initially
        assert!(
            !db.has_pulse_due_run("agent-a", "int-1", "2025-01-01")
                .expect("has")
        );

        // Create a running run
        create_test_pulse_run(&db, "run-1", "agent-a", "int-1", "2025-01-01");
        assert!(
            db.has_pulse_due_run("agent-a", "int-1", "2025-01-01")
                .expect("has running")
        );

        // Update to success — still returns true
        db.update_pulse_run_success("run-1", None, None, PulseOutputKind::Silent, "PULSE_OK")
            .expect("success");
        assert!(
            db.has_pulse_due_run("agent-a", "int-1", "2025-01-01")
                .expect("has success")
        );

        // Different due_key should not match
        assert!(
            !db.has_pulse_due_run("agent-a", "int-1", "2025-01-02")
                .expect("has other")
        );
    }

    #[test]
    fn pulse_run_failed_consumes_due_key_without_retry() {
        let (db, _dir) = test_db();

        // Arrange: create and fail a run
        create_test_pulse_run(&db, "run-1", "agent-a", "int-1", "2025-01-01");
        db.update_pulse_run_failed("run-1", "error")
            .expect("failed");

        // Act & Assert: has_pulse_due_run still returns true (due_key consumed)
        assert!(
            db.has_pulse_due_run("agent-a", "int-1", "2025-01-01")
                .expect("has")
        );

        // Attempting to create a second run with same due_key should conflict
        let result = db.try_create_pulse_run("run-2", "agent-a", "int-1", "2025-01-01");
        assert!(matches!(result, Err(StorageError::Conflict(_))));
    }

    // ---------------------------------------------------------------------------
    // Episode event tests
    // ---------------------------------------------------------------------------

    fn make_event(
        id: &str,
        agent_id: &str,
        experienced_at: &str,
        kind: EpisodeEventKind,
        ripple_strength: i64,
        certainty: EpisodeEventCertainty,
        sleep_run_id: &str,
    ) -> EpisodeEvent {
        EpisodeEvent {
            id: id.to_string(),
            agent_id: agent_id.to_string(),
            experienced_at: experienced_at.to_string(),
            encoded_at: "2025-01-15T12:00:00Z".to_string(),
            kind,
            title: format!("event {id}"),
            body_md: format!("body of {id}"),
            ripple_strength,
            certainty,
            sleep_run_id: sleep_run_id.to_string(),
            source_refs_json: None,
            created_at: "2025-01-15T12:00:00Z".to_string(),
            updated_at: "2025-01-15T12:00:00Z".to_string(),
        }
    }

    #[test]
    fn insert_episode_event_succeeds() {
        let (db, _dir) = test_db();

        let events = vec![
            make_event(
                "evt-1",
                "agent-a",
                "2025-01-10T10:00:00Z",
                EpisodeEventKind::Self_,
                3,
                EpisodeEventCertainty::Stated,
                "run-1",
            ),
            make_event(
                "evt-2",
                "agent-a",
                "2025-01-11T10:00:00Z",
                EpisodeEventKind::World,
                4,
                EpisodeEventCertainty::Derived,
                "run-1",
            ),
        ];

        db.insert_episode_events("run-1", &events).expect("insert");

        let count = db.count_episode_events("agent-a").expect("count");
        assert_eq!(count, 2);
    }

    #[test]
    fn insert_duplicate_sleep_run_id_skips() {
        let (db, _dir) = test_db();

        let events_batch_1 = vec![make_event(
            "evt-1",
            "agent-a",
            "2025-01-10T10:00:00Z",
            EpisodeEventKind::Self_,
            3,
            EpisodeEventCertainty::Stated,
            "run-1",
        )];

        db.insert_episode_events("run-1", &events_batch_1)
            .expect("first insert");

        let events_batch_2 = vec![make_event(
            "evt-2",
            "agent-a",
            "2025-01-11T10:00:00Z",
            EpisodeEventKind::World,
            4,
            EpisodeEventCertainty::Derived,
            "run-1",
        )];

        db.insert_episode_events("run-1", &events_batch_2)
            .expect("second insert");

        let count = db.count_episode_events("agent-a").expect("count");
        assert_eq!(
            count, 1,
            "second insert with same sleep_run_id should be skipped"
        );
    }

    #[test]
    fn list_episode_events_by_agent_experienced_desc() {
        let (db, _dir) = test_db();

        let events = vec![
            make_event(
                "evt-1",
                "agent-a",
                "2025-01-10T10:00:00Z",
                EpisodeEventKind::Self_,
                3,
                EpisodeEventCertainty::Stated,
                "run-1",
            ),
            make_event(
                "evt-2",
                "agent-a",
                "2025-01-12T10:00:00Z",
                EpisodeEventKind::World,
                4,
                EpisodeEventCertainty::Derived,
                "run-1",
            ),
            make_event(
                "evt-3",
                "agent-a",
                "2025-01-11T10:00:00Z",
                EpisodeEventKind::Relationship,
                2,
                EpisodeEventCertainty::Tentative,
                "run-1",
            ),
        ];

        db.insert_episode_events("run-1", &events).expect("insert");

        let listed = db
            .list_episode_events("agent-a", None, None, 10)
            .expect("list");

        assert_eq!(listed.len(), 3);
        assert_eq!(listed[0].id, "evt-2");
        assert_eq!(listed[1].id, "evt-3");
        assert_eq!(listed[2].id, "evt-1");
    }

    #[test]
    fn list_episode_events_by_agent_kind() {
        let (db, _dir) = test_db();

        let events = vec![
            make_event(
                "evt-1",
                "agent-a",
                "2025-01-10T10:00:00Z",
                EpisodeEventKind::Self_,
                3,
                EpisodeEventCertainty::Stated,
                "run-1",
            ),
            make_event(
                "evt-2",
                "agent-a",
                "2025-01-11T10:00:00Z",
                EpisodeEventKind::World,
                4,
                EpisodeEventCertainty::Derived,
                "run-1",
            ),
            make_event(
                "evt-3",
                "agent-a",
                "2025-01-12T10:00:00Z",
                EpisodeEventKind::World,
                5,
                EpisodeEventCertainty::Stated,
                "run-1",
            ),
        ];

        db.insert_episode_events("run-1", &events).expect("insert");

        let world_events = db
            .list_episode_events("agent-a", Some(EpisodeEventKind::World), None, 10)
            .expect("list");

        assert_eq!(world_events.len(), 2);
        assert!(
            world_events
                .iter()
                .all(|e| e.kind == EpisodeEventKind::World)
        );
    }

    #[test]
    fn list_episode_events_by_agent_ripple() {
        let (db, _dir) = test_db();

        let events = vec![
            make_event(
                "evt-1",
                "agent-a",
                "2025-01-10T10:00:00Z",
                EpisodeEventKind::Self_,
                2,
                EpisodeEventCertainty::Stated,
                "run-1",
            ),
            make_event(
                "evt-2",
                "agent-a",
                "2025-01-11T10:00:00Z",
                EpisodeEventKind::World,
                4,
                EpisodeEventCertainty::Derived,
                "run-1",
            ),
            make_event(
                "evt-3",
                "agent-a",
                "2025-01-12T10:00:00Z",
                EpisodeEventKind::Feat,
                5,
                EpisodeEventCertainty::Stated,
                "run-1",
            ),
        ];

        db.insert_episode_events("run-1", &events).expect("insert");

        let strong_events = db
            .list_episode_events("agent-a", None, Some(4), 10)
            .expect("list");

        assert_eq!(strong_events.len(), 2);
        assert!(strong_events.iter().all(|e| e.ripple_strength >= 4));
    }

    #[test]
    fn count_episode_events_by_agent() {
        let (db, _dir) = test_db();

        let events_a = vec![
            make_event(
                "evt-a1",
                "agent-a",
                "2025-01-10T10:00:00Z",
                EpisodeEventKind::Self_,
                3,
                EpisodeEventCertainty::Stated,
                "run-1",
            ),
            make_event(
                "evt-a2",
                "agent-a",
                "2025-01-11T10:00:00Z",
                EpisodeEventKind::World,
                4,
                EpisodeEventCertainty::Derived,
                "run-1",
            ),
        ];
        let events_b = vec![make_event(
            "evt-b1",
            "agent-b",
            "2025-01-10T10:00:00Z",
            EpisodeEventKind::Feat,
            5,
            EpisodeEventCertainty::Stated,
            "run-2",
        )];

        db.insert_episode_events("run-1", &events_a)
            .expect("insert a");
        db.insert_episode_events("run-2", &events_b)
            .expect("insert b");

        assert_eq!(db.count_episode_events("agent-a").expect("count a"), 2);
        assert_eq!(db.count_episode_events("agent-b").expect("count b"), 1);
        assert_eq!(db.count_episode_events("agent-c").expect("count c"), 0);
    }

    #[test]
    fn list_episode_events_by_sleep_run_id() {
        let (db, _dir) = test_db();

        let events_run1 = vec![
            make_event(
                "evt-1",
                "agent-a",
                "2025-01-10T10:00:00Z",
                EpisodeEventKind::Self_,
                3,
                EpisodeEventCertainty::Stated,
                "run-1",
            ),
            make_event(
                "evt-2",
                "agent-a",
                "2025-01-11T10:00:00Z",
                EpisodeEventKind::World,
                4,
                EpisodeEventCertainty::Derived,
                "run-1",
            ),
        ];
        let events_run2 = vec![make_event(
            "evt-3",
            "agent-a",
            "2025-01-12T10:00:00Z",
            EpisodeEventKind::Feat,
            5,
            EpisodeEventCertainty::Stated,
            "run-2",
        )];

        db.insert_episode_events("run-1", &events_run1)
            .expect("insert run-1");
        db.insert_episode_events("run-2", &events_run2)
            .expect("insert run-2");

        let run1_events = db.list_episode_events_by_run("run-1").expect("list");
        assert_eq!(run1_events.len(), 2);
        assert!(run1_events.iter().all(|e| e.sleep_run_id == "run-1"));

        let run2_events = db.list_episode_events_by_run("run-2").expect("list");
        assert_eq!(run2_events.len(), 1);
        assert_eq!(run2_events[0].id, "evt-3");

        let empty = db.list_episode_events_by_run("run-999").expect("list");
        assert!(empty.is_empty());
    }

    // ---------------------------------------------------------------------------
    // Episode rollups
    // ---------------------------------------------------------------------------

    fn make_test_rollup(
        id: &str,
        agent_id: &str,
        granularity: RollupGranularity,
        period_key: &str,
        period_start: &str,
        period_end_exclusive: &str,
        max_ripple: i64,
    ) -> EpisodeRollup {
        EpisodeRollup {
            id: id.to_string(),
            agent_id: agent_id.to_string(),
            granularity,
            period_key: period_key.to_string(),
            period_start: period_start.to_string(),
            period_end_exclusive: period_end_exclusive.to_string(),
            summary_md: format!("summary for {period_key}"),
            max_ripple,
            event_count: 5,
            generated_run_id: "run-test".to_string(),
            created_at: "2025-01-15T00:00:00Z".to_string(),
            updated_at: "2025-01-15T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn test_migration_v6_creates_episode_rollups() {
        let (db, _dir) = test_db();
        let conn = db.get_conn().expect("pool");

        let exists: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name='episode_rollups'",
                [],
                |row| row.get(0),
            )
            .expect("check table");
        assert!(exists, "episode_rollups table should exist after migration");

        let expected_columns = [
            "id",
            "agent_id",
            "granularity",
            "period_key",
            "period_start",
            "period_end_exclusive",
            "summary_md",
            "max_ripple",
            "event_count",
            "generated_run_id",
            "created_at",
            "updated_at",
        ];

        let mut stmt = conn
            .prepare("PRAGMA table_info(episode_rollups)")
            .expect("prepare pragma");
        let columns: Vec<String> = stmt
            .query_map([], |row| row.get::<_, String>(1))
            .expect("query")
            .map(|r| r.expect("col"))
            .collect();

        for name in &expected_columns {
            assert!(columns.iter().any(|c| c == *name), "missing column: {name}");
        }

        let expected_indexes = [
            "idx_episode_rollups_agent_period",
            "idx_episode_rollups_agent_ripple",
        ];
        let mut stmt = conn
            .prepare(
                "SELECT name FROM sqlite_master WHERE type='index' AND name LIKE 'idx_episode_rollups%'",
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

    #[test]
    fn test_upsert_episode_rollup_insert_new() {
        let (db, _dir) = test_db();

        let rollup = EpisodeRollup {
            id: "r-1".to_string(),
            agent_id: "agent-a".to_string(),
            granularity: RollupGranularity::Week,
            period_key: "2025-W02".to_string(),
            period_start: "2025-01-06T00:00:00Z".to_string(),
            period_end_exclusive: "2025-01-13T00:00:00Z".to_string(),
            summary_md: "# Summary\nEvents this week".to_string(),
            max_ripple: 4,
            event_count: 7,
            generated_run_id: "run-1".to_string(),
            created_at: "2025-01-15T00:00:00Z".to_string(),
            updated_at: "2025-01-15T00:00:00Z".to_string(),
        };

        db.upsert_episode_rollup(&rollup).expect("upsert");

        let retrieved = db
            .get_episode_rollup("agent-a", RollupGranularity::Week, "2025-W02")
            .expect("get")
            .expect("should exist");

        assert_eq!(retrieved.id, "r-1");
        assert_eq!(retrieved.agent_id, "agent-a");
        assert_eq!(retrieved.granularity, RollupGranularity::Week);
        assert_eq!(retrieved.period_key, "2025-W02");
        assert_eq!(retrieved.period_start, "2025-01-06T00:00:00Z");
        assert_eq!(retrieved.period_end_exclusive, "2025-01-13T00:00:00Z");
        assert_eq!(retrieved.summary_md, "# Summary\nEvents this week");
        assert_eq!(retrieved.max_ripple, 4);
        assert_eq!(retrieved.event_count, 7);
        assert_eq!(retrieved.generated_run_id, "run-1");
        assert_eq!(retrieved.created_at, "2025-01-15T00:00:00Z");
        assert_eq!(retrieved.updated_at, "2025-01-15T00:00:00Z");
    }

    #[test]
    fn test_upsert_episode_rollup_update_existing() {
        let (db, _dir) = test_db();

        let rollup = make_test_rollup(
            "r-1",
            "agent-a",
            RollupGranularity::Week,
            "2025-W02",
            "2025-01-06T00:00:00Z",
            "2025-01-13T00:00:00Z",
            3,
        );

        db.upsert_episode_rollup(&rollup).expect("initial upsert");

        let updated = EpisodeRollup {
            id: "r-2".to_string(),
            summary_md: "updated summary".to_string(),
            max_ripple: 5,
            event_count: 10,
            generated_run_id: "run-2".to_string(),
            updated_at: "2025-01-16T00:00:00Z".to_string(),
            ..rollup
        };

        db.upsert_episode_rollup(&updated).expect("update upsert");

        let retrieved = db
            .get_episode_rollup("agent-a", RollupGranularity::Week, "2025-W02")
            .expect("get")
            .expect("should exist");

        assert_eq!(retrieved.summary_md, "updated summary");
        assert_eq!(retrieved.max_ripple, 5);
        assert_eq!(retrieved.event_count, 10);
        assert_eq!(retrieved.generated_run_id, "run-2");
        assert_eq!(retrieved.updated_at, "2025-01-16T00:00:00Z");
        assert_eq!(
            retrieved.created_at, "2025-01-15T00:00:00Z",
            "created_at should be preserved from original insert"
        );
    }

    #[test]
    fn test_list_episode_rollups_by_granularity() {
        let (db, _dir) = test_db();

        let week1 = make_test_rollup(
            "r-w1",
            "agent-a",
            RollupGranularity::Week,
            "2025-W01",
            "2024-12-30T00:00:00Z",
            "2025-01-06T00:00:00Z",
            3,
        );
        let week2 = make_test_rollup(
            "r-w2",
            "agent-a",
            RollupGranularity::Week,
            "2025-W02",
            "2025-01-06T00:00:00Z",
            "2025-01-13T00:00:00Z",
            4,
        );
        let month1 = make_test_rollup(
            "r-m1",
            "agent-a",
            RollupGranularity::Month,
            "2025-01",
            "2025-01-01T00:00:00Z",
            "2025-02-01T00:00:00Z",
            5,
        );

        db.upsert_episode_rollup(&week1).expect("insert w1");
        db.upsert_episode_rollup(&week2).expect("insert w2");
        db.upsert_episode_rollup(&month1).expect("insert m1");

        let weeks = db
            .list_episode_rollups("agent-a", RollupGranularity::Week, 10)
            .expect("list weeks");
        assert_eq!(weeks.len(), 2);
        assert_eq!(weeks[0].period_key, "2025-W02", "newest first");
        assert_eq!(weeks[1].period_key, "2025-W01");

        let months = db
            .list_episode_rollups("agent-a", RollupGranularity::Month, 10)
            .expect("list months");
        assert_eq!(months.len(), 1);
        assert_eq!(months[0].period_key, "2025-01");
    }

    #[test]
    fn test_get_episode_rollup_by_period_key() {
        let (db, _dir) = test_db();

        let rollup = make_test_rollup(
            "r-1",
            "agent-a",
            RollupGranularity::Month,
            "2025-01",
            "2025-01-01T00:00:00Z",
            "2025-02-01T00:00:00Z",
            4,
        );
        db.upsert_episode_rollup(&rollup).expect("insert");

        let found = db
            .get_episode_rollup("agent-a", RollupGranularity::Month, "2025-01")
            .expect("get")
            .expect("should exist");
        assert_eq!(found.id, "r-1");

        let missing = db
            .get_episode_rollup("agent-a", RollupGranularity::Month, "2025-02")
            .expect("get");
        assert!(missing.is_none());

        let missing_agent = db
            .get_episode_rollup("agent-b", RollupGranularity::Month, "2025-01")
            .expect("get");
        assert!(missing_agent.is_none());
    }

    #[test]
    fn test_list_episode_rollups_period_range() {
        let (db, _dir) = test_db();

        let w1 = make_test_rollup(
            "r-w1",
            "agent-a",
            RollupGranularity::Week,
            "2025-W01",
            "2024-12-30T00:00:00Z",
            "2025-01-06T00:00:00Z",
            3,
        );
        let w2 = make_test_rollup(
            "r-w2",
            "agent-a",
            RollupGranularity::Week,
            "2025-W02",
            "2025-01-06T00:00:00Z",
            "2025-01-13T00:00:00Z",
            4,
        );
        let w3 = make_test_rollup(
            "r-w3",
            "agent-a",
            RollupGranularity::Week,
            "2025-W03",
            "2025-01-13T00:00:00Z",
            "2025-01-20T00:00:00Z",
            2,
        );

        db.upsert_episode_rollup(&w1).expect("insert w1");
        db.upsert_episode_rollup(&w2).expect("insert w2");
        db.upsert_episode_rollup(&w3).expect("insert w3");

        let range = db
            .list_episode_rollups_in_range(
                "agent-a",
                RollupGranularity::Week,
                "2025-01-06T00:00:00Z",
                "2025-01-20T00:00:00Z",
            )
            .expect("range");

        assert_eq!(range.len(), 2, "should include w2 and w3 but not w1");
        assert_eq!(range[0].period_key, "2025-W03", "newest first");
        assert_eq!(range[1].period_key, "2025-W02");
    }

    #[test]
    fn test_list_episode_rollups_for_background() {
        let (db, _dir) = test_db();

        let m1 = make_test_rollup(
            "r-m1",
            "agent-a",
            RollupGranularity::Month,
            "2024-11",
            "2024-11-01T00:00:00Z",
            "2024-12-01T00:00:00Z",
            5,
        );
        let m2 = make_test_rollup(
            "r-m2",
            "agent-a",
            RollupGranularity::Month,
            "2024-12",
            "2024-12-01T00:00:00Z",
            "2025-01-01T00:00:00Z",
            3,
        );
        let m3 = make_test_rollup(
            "r-m3",
            "agent-a",
            RollupGranularity::Month,
            "2025-01",
            "2025-01-01T00:00:00Z",
            "2025-02-01T00:00:00Z",
            4,
        );
        let w1 = make_test_rollup(
            "r-w1",
            "agent-a",
            RollupGranularity::Week,
            "2025-W01",
            "2024-12-30T00:00:00Z",
            "2025-01-06T00:00:00Z",
            5,
        );

        db.upsert_episode_rollup(&m1).expect("insert m1");
        db.upsert_episode_rollup(&m2).expect("insert m2");
        db.upsert_episode_rollup(&m3).expect("insert m3");
        db.upsert_episode_rollup(&w1).expect("insert w1");

        let background = db
            .list_background_episode_rollups("agent-a", 4, "2025-02-01T00:00:00Z")
            .expect("background");

        assert_eq!(
            background.len(),
            2,
            "m1 (ripple=5) and m3 (ripple=4), both months, before Feb"
        );
        assert_eq!(background[0].period_key, "2025-01");
        assert_eq!(background[1].period_key, "2024-11");
    }

    #[test]
    fn sleep_checkpoint_preserves_composite_cursor_per_source() {
        // Arrange
        let (db, _dir) = test_db();
        let now = chrono::Utc::now().to_rfc3339();

        // Act: upsert checkpoints for 2 chats (messages) and 1 semantic (episode_events)
        db.upsert_sleep_checkpoint(&SleepStepCheckpoint {
            agent_id: "agent-a".to_string(),
            step_name: SleepStepName::EventExtraction,
            source_kind: CheckpointSourceKind::Messages,
            source_id: "chat-1".to_string(),
            cursor_at: "2024-01-01T00:00:00Z".to_string(),
            cursor_id: "msg-100".to_string(),
            updated_at: now.clone(),
        })
        .expect("upsert chat-1");
        db.upsert_sleep_checkpoint(&SleepStepCheckpoint {
            agent_id: "agent-a".to_string(),
            step_name: SleepStepName::EventExtraction,
            source_kind: CheckpointSourceKind::Messages,
            source_id: "chat-2".to_string(),
            cursor_at: "2024-01-01T00:00:00Z".to_string(),
            cursor_id: "msg-200".to_string(),
            updated_at: now.clone(),
        })
        .expect("upsert chat-2");
        db.upsert_sleep_checkpoint(&SleepStepCheckpoint {
            agent_id: "agent-a".to_string(),
            step_name: SleepStepName::SemanticUpdate,
            source_kind: CheckpointSourceKind::EpisodeEvents,
            source_id: "agent-a".to_string(),
            cursor_at: "2024-01-01T00:00:00Z".to_string(),
            cursor_id: "evt-50".to_string(),
            updated_at: now.clone(),
        })
        .expect("upsert semantic");

        // Assert: each source has its own cursor (no cross-contamination)
        let chat1 = db
            .get_sleep_checkpoint(
                "agent-a",
                SleepStepName::EventExtraction,
                CheckpointSourceKind::Messages,
                "chat-1",
            )
            .expect("get")
            .expect("exists");
        assert_eq!(chat1.cursor_id, "msg-100");

        let chat2 = db
            .get_sleep_checkpoint(
                "agent-a",
                SleepStepName::EventExtraction,
                CheckpointSourceKind::Messages,
                "chat-2",
            )
            .expect("get")
            .expect("exists");
        assert_eq!(chat2.cursor_id, "msg-200");

        let semantic = db
            .get_sleep_checkpoint(
                "agent-a",
                SleepStepName::SemanticUpdate,
                CheckpointSourceKind::EpisodeEvents,
                "agent-a",
            )
            .expect("get")
            .expect("exists");
        assert_eq!(semantic.cursor_id, "evt-50");

        // Assert: same timestamp but different cursor_id preserved
        assert_eq!(chat1.cursor_at, chat2.cursor_at);
        assert_ne!(chat1.cursor_id, chat2.cursor_id);

        // Assert: non-existent source returns None
        let missing = db.get_sleep_checkpoint(
            "agent-a",
            SleepStepName::EventExtraction,
            CheckpointSourceKind::Messages,
            "chat-999",
        );
        assert!(missing.expect("query ok").is_none());
    }

    #[test]
    fn sleep_checkpoint_upsert_advances_cursor() {
        // Arrange
        let (db, _dir) = test_db();
        let now = chrono::Utc::now().to_rfc3339();

        // Act: upsert initial cursor, then advance it
        db.upsert_sleep_checkpoint(&SleepStepCheckpoint {
            agent_id: "agent-a".to_string(),
            step_name: SleepStepName::EventExtraction,
            source_kind: CheckpointSourceKind::Messages,
            source_id: "chat-1".to_string(),
            cursor_at: "2024-01-01T00:00:00Z".to_string(),
            cursor_id: "msg-100".to_string(),
            updated_at: now.clone(),
        })
        .expect("upsert initial");
        db.upsert_sleep_checkpoint(&SleepStepCheckpoint {
            agent_id: "agent-a".to_string(),
            step_name: SleepStepName::EventExtraction,
            source_kind: CheckpointSourceKind::Messages,
            source_id: "chat-1".to_string(),
            cursor_at: "2024-01-01T00:01:00Z".to_string(),
            cursor_id: "msg-200".to_string(),
            updated_at: now.clone(),
        })
        .expect("upsert advance");

        // Assert: cursor advanced to new position
        let checkpoint = db
            .get_sleep_checkpoint(
                "agent-a",
                SleepStepName::EventExtraction,
                CheckpointSourceKind::Messages,
                "chat-1",
            )
            .expect("get")
            .expect("exists");
        assert_eq!(checkpoint.cursor_at, "2024-01-01T00:01:00Z");
        assert_eq!(checkpoint.cursor_id, "msg-200");
    }

    #[test]
    fn sleep_checkpoint_list_returns_all_sources_for_step() {
        // Arrange
        let (db, _dir) = test_db();
        let now = chrono::Utc::now().to_rfc3339();

        db.upsert_sleep_checkpoint(&SleepStepCheckpoint {
            agent_id: "agent-a".to_string(),
            step_name: SleepStepName::EventExtraction,
            source_kind: CheckpointSourceKind::Messages,
            source_id: "chat-a".to_string(),
            cursor_at: "2024-01-01T00:00:00Z".to_string(),
            cursor_id: "msg-1".to_string(),
            updated_at: now.clone(),
        })
        .expect("upsert");
        db.upsert_sleep_checkpoint(&SleepStepCheckpoint {
            agent_id: "agent-a".to_string(),
            step_name: SleepStepName::EventExtraction,
            source_kind: CheckpointSourceKind::Messages,
            source_id: "chat-b".to_string(),
            cursor_at: "2024-01-01T00:00:00Z".to_string(),
            cursor_id: "msg-2".to_string(),
            updated_at: now.clone(),
        })
        .expect("upsert");

        // Act
        let checkpoints = db
            .list_sleep_checkpoints(
                "agent-a",
                SleepStepName::EventExtraction,
                CheckpointSourceKind::Messages,
            )
            .expect("list");

        // Assert
        assert_eq!(checkpoints.len(), 2);
        assert_eq!(checkpoints[0].source_id, "chat-a");
        assert_eq!(checkpoints[1].source_id, "chat-b");
    }

    #[test]
    fn sleep_checkpoint_rejects_backward_cursor_update() {
        // Arrange: existing checkpoint at (T1, msg-100)
        let (db, _dir) = test_db();
        let now = chrono::Utc::now().to_rfc3339();
        db.upsert_sleep_checkpoint(&SleepStepCheckpoint {
            agent_id: "agent-a".to_string(),
            step_name: SleepStepName::EventExtraction,
            source_kind: CheckpointSourceKind::Messages,
            source_id: "chat-1".to_string(),
            cursor_at: "2024-01-01T00:01:00Z".to_string(),
            cursor_id: "msg-100".to_string(),
            updated_at: now.clone(),
        })
        .expect("initial upsert");

        // Act & Assert: backward cursor_at is rejected
        let result = db.upsert_sleep_checkpoint(&SleepStepCheckpoint {
            agent_id: "agent-a".to_string(),
            step_name: SleepStepName::EventExtraction,
            source_kind: CheckpointSourceKind::Messages,
            source_id: "chat-1".to_string(),
            cursor_at: "2024-01-01T00:00:00Z".to_string(),
            cursor_id: "msg-200".to_string(),
            updated_at: now.clone(),
        });
        assert!(
            matches!(result, Err(StorageError::Conflict(_))),
            "should reject backward cursor_at"
        );

        // Act & Assert: same cursor_at but smaller cursor_id is rejected
        let result = db.upsert_sleep_checkpoint(&SleepStepCheckpoint {
            agent_id: "agent-a".to_string(),
            step_name: SleepStepName::EventExtraction,
            source_kind: CheckpointSourceKind::Messages,
            source_id: "chat-1".to_string(),
            cursor_at: "2024-01-01T00:01:00Z".to_string(),
            cursor_id: "msg-050".to_string(),
            updated_at: now.clone(),
        });
        assert!(
            matches!(result, Err(StorageError::Conflict(_))),
            "should reject backward cursor_id at same cursor_at"
        );

        // Act: forward cursor succeeds
        db.upsert_sleep_checkpoint(&SleepStepCheckpoint {
            agent_id: "agent-a".to_string(),
            step_name: SleepStepName::EventExtraction,
            source_kind: CheckpointSourceKind::Messages,
            source_id: "chat-1".to_string(),
            cursor_at: "2024-01-01T00:02:00Z".to_string(),
            cursor_id: "msg-200".to_string(),
            updated_at: now.clone(),
        })
        .expect("forward upsert should succeed");

        // Assert: cursor advanced
        let checkpoint = db
            .get_sleep_checkpoint(
                "agent-a",
                SleepStepName::EventExtraction,
                CheckpointSourceKind::Messages,
                "chat-1",
            )
            .expect("get")
            .expect("exists");
        assert_eq!(checkpoint.cursor_at, "2024-01-01T00:02:00Z");
        assert_eq!(checkpoint.cursor_id, "msg-200");
    }

    #[test]
    fn sleep_step_success_transaction_rolls_back_as_a_unit() {
        // Arrange: run with pending steps
        let (db, _dir) = test_db();
        let run_id = db
            .try_create_sleep_run("agent-a", SleepRunTrigger::Manual)
            .expect("create")
            .expect("inserted");
        let now = chrono::Utc::now().to_rfc3339();

        // Act: commit step success with checkpoint atomically
        let checkpoint = SleepStepCheckpoint {
            agent_id: "agent-a".to_string(),
            step_name: SleepStepName::EventExtraction,
            source_kind: CheckpointSourceKind::Messages,
            source_id: "chat-1".to_string(),
            cursor_at: "2024-01-01T00:01:00Z".to_string(),
            cursor_id: "msg-100".to_string(),
            updated_at: now.clone(),
        };
        db.start_sleep_step(&run_id, SleepStepName::EventExtraction)
            .expect("start");
        db.commit_step_success(
            &run_id,
            SleepStepName::EventExtraction,
            SleepStepResult {
                status: SleepStepStatus::Success,
                input_tokens: 100,
                output_tokens: 50,
                error_message: None,
                metadata_json: Some(r#"{"events_extracted": 5}"#),
            },
            Some(&checkpoint),
        )
        .expect("commit");

        // Assert: step is success
        let step = db
            .get_sleep_run_step(&run_id, SleepStepName::EventExtraction)
            .expect("get")
            .expect("exists");
        assert_eq!(step.status, SleepStepStatus::Success);
        assert_eq!(step.input_tokens, 100);
        assert_eq!(step.output_tokens, 50);

        // Assert: checkpoint is updated
        let cp = db
            .get_sleep_checkpoint(
                "agent-a",
                SleepStepName::EventExtraction,
                CheckpointSourceKind::Messages,
                "chat-1",
            )
            .expect("get")
            .expect("exists");
        assert_eq!(cp.cursor_at, "2024-01-01T00:01:00Z");
        assert_eq!(cp.cursor_id, "msg-100");

        // Act: try to update existing checkpoint with backward cursor via commit_step_success
        // First, start another step that uses the same checkpoint source
        db.start_sleep_step(&run_id, SleepStepName::ProspectiveUpdate)
            .expect("start");
        let backward_checkpoint = SleepStepCheckpoint {
            agent_id: "agent-a".to_string(),
            step_name: SleepStepName::EventExtraction,
            source_kind: CheckpointSourceKind::Messages,
            source_id: "chat-1".to_string(),
            cursor_at: "2024-01-01T00:00:00Z".to_string(),
            cursor_id: "msg-050".to_string(),
            updated_at: now.clone(),
        };
        let result = db.commit_step_success(
            &run_id,
            SleepStepName::ProspectiveUpdate,
            SleepStepResult {
                status: SleepStepStatus::Success,
                input_tokens: 50,
                output_tokens: 25,
                error_message: None,
                metadata_json: None,
            },
            Some(&backward_checkpoint),
        );

        // Assert: transaction rolled back
        assert!(
            matches!(result, Err(StorageError::Conflict(_))),
            "should reject backward checkpoint and rollback"
        );

        // Assert: step is still running (rolled back)
        let step = db
            .get_sleep_run_step(&run_id, SleepStepName::ProspectiveUpdate)
            .expect("get")
            .expect("exists");
        assert_eq!(step.status, SleepStepStatus::Running);
    }
}
