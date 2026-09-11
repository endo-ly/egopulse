use std::fmt;
use std::str::FromStr;

use rusqlite::{OptionalExtension, TransactionBehavior, params};

use crate::error::StorageError;

use super::{
    ChatInfo, CommittedStagedMessages, Database, MessageKind, SenderKind, SessionSnapshot,
    SessionSummary, StageToolFollowupOutcome, StoredMessage, TerminalStagedMessage, TurnRunState,
};

pub(crate) fn row_to_stored_message(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredMessage> {
    let sender_kind = parse_row_enum!(row, 4, SenderKind)?;
    let message_kind = parse_row_enum!(row, 6, MessageKind)?;

    Ok(StoredMessage {
        id: row.get(0)?,
        chat_id: row.get(1)?,
        sender_id: row.get(2)?,
        content: row.get(3)?,
        sender_kind,
        timestamp: row.get(5)?,
        message_kind,
        recipient_agent_id: row.get(7)?,
        seq: row.get(8).ok(),
        turn_id: row.get(9).ok(),
        parent_message_id: row.get(10).ok(),
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
                COALESCE((SELECT MAX(m.timestamp) FROM messages m WHERE m.chat_id = c.chat_id AND m.seq IS NOT NULL), c.last_message_time)
                    AS last_message_time,
                (
                    SELECT m.content
                    FROM messages m
                    WHERE m.chat_id = c.chat_id AND m.seq IS NOT NULL
                    ORDER BY m.seq DESC
                    LIMIT 1
                ) AS last_message_preview,
                c.agent_id
             FROM chats c
             ORDER BY last_message_time DESC, c.chat_id DESC",
        )?;
        stmt.query_map([], |row| {
            let channel: String = row.get(1)?;
            let external_chat_id: String = row.get(2)?;
            let chat_title: Option<String> = row.get(3)?;
            Ok(SessionSummary {
                chat_id: row.get(0)?,
                channel: channel.clone(),
                external_chat_id: external_chat_id.clone(),
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
                    message_kind, recipient_agent_id, seq, turn_id, parent_message_id
             FROM messages
             WHERE chat_id = ?1 AND seq IS NOT NULL
             ORDER BY seq DESC
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
                    message_kind, recipient_agent_id, seq, turn_id, parent_message_id
             FROM messages
             WHERE chat_id = ?1 AND seq IS NOT NULL
             ORDER BY seq ASC",
        )?;
        stmt.query_map(params![chat_id], row_to_stored_message)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    /// Loads the `content` column of a single message by its id.
    ///
    /// Used by the durable Turn path to return the saved final response of a
    /// `completed` Turn on re-acceptance, without re-invoking the LLM.
    pub(crate) fn get_message_content(
        &self,
        message_id: &str,
    ) -> Result<Option<String>, StorageError> {
        let conn = self.get_conn()?;
        let content = conn
            .query_row(
                "SELECT content FROM messages WHERE id = ?1",
                params![message_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        Ok(content)
    }

    /// Stages one human follow-up for the unique Tool phase of a chat.
    ///
    /// The duplicate check, active-phase cardinality check, capacity checks,
    /// and insert all share one `BEGIN IMMEDIATE` transaction. A matching
    /// existing message rows are returned as idempotent success. An already
    /// accepted Turn with the same request hash is delegated to normal Turn
    /// idempotency so recovery cannot create a second staged message after the
    /// original row has been deleted.
    pub(crate) fn stage_tool_followup(
        &self,
        chat_id: i64,
        request_key: &str,
        request_payload_hash: &str,
        sender_id: &str,
        content: &str,
        timestamp: &str,
    ) -> Result<StageToolFollowupOutcome, StorageError> {
        let mut conn = self.get_conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;

        let existing = tx
            .query_row(
                "SELECT id, chat_id, sender_id, content, sender_kind, timestamp,
                        message_kind, recipient_agent_id, seq, turn_id, parent_message_id
                 FROM messages WHERE id = ?1 AND chat_id = ?2",
                params![request_key, chat_id],
                row_to_stored_message,
            )
            .optional()?;
        if let Some(message) = existing {
            if message.sender_kind != SenderKind::User
                || message.message_kind != MessageKind::Message
                || message.sender_id != sender_id
                || message.content != content
            {
                return Err(StorageError::Conflict(format!(
                    "staged follow-up request key collision: {request_key}"
                )));
            }
            tx.commit()?;
            return Ok(StageToolFollowupOutcome::Accepted(message));
        }

        let existing_turn_hash: Option<String> = tx
            .query_row(
                "SELECT request_payload_hash FROM turn_runs
                 WHERE chat_id = ?1 AND request_key = ?2",
                params![chat_id, request_key],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(existing_hash) = existing_turn_hash {
            if existing_hash != request_payload_hash {
                return Err(StorageError::Conflict(format!(
                    "staged follow-up request key collision: {request_key}"
                )));
            }
            tx.commit()?;
            return Ok(StageToolFollowupOutcome::NoToolPhase);
        }

        let turn_ids = tx
            .prepare(
                "SELECT turn_id FROM turn_runs
                 WHERE chat_id = ?1 AND state = ?2
                 ORDER BY accepted_at ASC, turn_id ASC",
            )?
            .query_map(
                params![chat_id, TurnRunState::ToolsPending.to_string()],
                |row| row.get::<_, String>(0),
            )?
            .collect::<Result<Vec<_>, _>>()?;
        let turn_id = match turn_ids.as_slice() {
            [] => {
                tx.commit()?;
                return Ok(StageToolFollowupOutcome::NoToolPhase);
            }
            [turn_id] => turn_id.clone(),
            _ => {
                return Err(StorageError::Conflict(format!(
                    "multiple tools_pending turns for chat_id={chat_id}"
                )));
            }
        };

        let pending_session: i64 = tx.query_row(
            "SELECT COUNT(*) FROM messages
             WHERE chat_id = ?1 AND seq IS NULL
               AND sender_kind = 'user' AND message_kind = 'message'",
            params![chat_id],
            |row| row.get(0),
        )?;
        if pending_session >= super::MAX_DURABLE_PENDING_PER_SESSION {
            return Err(StorageError::ToolFollowupSessionCapacityFull);
        }
        let pending_scope: i64 = tx.query_row(
            "SELECT COUNT(*) FROM messages
             WHERE seq IS NULL AND sender_kind = 'user' AND message_kind = 'message'",
            [],
            |row| row.get(0),
        )?;
        if pending_scope >= super::MAX_DURABLE_PENDING_PER_SCOPE {
            return Err(StorageError::ToolFollowupScopeCapacityFull);
        }

        let message = StoredMessage {
            id: request_key.to_string(),
            chat_id,
            sender_id: sender_id.to_string(),
            content: content.to_string(),
            sender_kind: SenderKind::User,
            timestamp: timestamp.to_string(),
            message_kind: MessageKind::Message,
            recipient_agent_id: None,
            seq: None,
            turn_id: Some(turn_id),
            parent_message_id: None,
        };
        tx.execute(
            "INSERT INTO messages
                 (id, chat_id, sender_id, content, sender_kind, timestamp,
                  message_kind, recipient_agent_id, seq, turn_id, parent_message_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, NULL, ?9, ?10)",
            params![
                &message.id,
                message.chat_id,
                &message.sender_id,
                &message.content,
                message.sender_kind.to_string(),
                &message.timestamp,
                message.message_kind.to_string(),
                message.recipient_agent_id.as_deref(),
                message.turn_id.as_deref(),
                message.parent_message_id.as_deref(),
            ],
        )?;
        tx.commit()?;
        Ok(StageToolFollowupOutcome::Accepted(message))
    }

    /// Lists staged user messages owned by one Turn in acceptance order.
    pub(crate) fn list_staged_user_messages(
        &self,
        turn_id: &str,
    ) -> Result<Vec<StoredMessage>, StorageError> {
        let conn = self.get_conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, chat_id, sender_id, content, sender_kind, timestamp,
                    message_kind, recipient_agent_id, seq, turn_id, parent_message_id
             FROM messages
             WHERE turn_id = ?1 AND seq IS NULL
               AND sender_kind = 'user' AND message_kind = 'message'
             ORDER BY timestamp ASC, id ASC",
        )?;
        stmt.query_map(params![turn_id], row_to_stored_message)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    /// Lists staged user messages whose owning Turn is terminal and includes
    /// the durable routing payload needed to promote them into a new Turn.
    pub(crate) fn list_terminal_staged_user_messages(
        &self,
        limit: usize,
    ) -> Result<Vec<TerminalStagedMessage>, StorageError> {
        let conn = self.get_conn()?;
        let mut stmt = conn.prepare(
            "SELECT m.id, m.chat_id, m.sender_id, m.content, m.sender_kind, m.timestamp,
                    m.message_kind, m.recipient_agent_id, m.seq, m.turn_id,
                    m.parent_message_id, t.scheduled_request_json
             FROM messages m
             JOIN turn_runs t ON t.turn_id = m.turn_id
             WHERE m.seq IS NULL AND m.sender_kind = 'user'
               AND m.message_kind = 'message'
               AND t.state IN ('completed', 'failed', 'cancelled', 'uncertain')
             ORDER BY m.timestamp ASC, m.id ASC
             LIMIT ?1",
        )?;
        stmt.query_map(params![limit as i64], |row| {
            Ok(TerminalStagedMessage {
                message: row_to_stored_message(row)?,
                scheduled_request_json: row.get(11)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(Into::into)
    }

    /// Removes a staged row after its input has been durably accepted as a new
    /// normal Turn. The predicate makes retries safe after a crash between the
    /// new Turn acceptance and this cleanup.
    pub(crate) fn delete_staged_user_message_after_promotion(
        &self,
        chat_id: i64,
        message_id: &str,
    ) -> Result<(), StorageError> {
        let conn = self.get_conn()?;
        conn.execute(
            "DELETE FROM messages
             WHERE chat_id = ?1 AND id = ?2 AND seq IS NULL
               AND sender_kind = 'user' AND message_kind = 'message'",
            params![chat_id, message_id],
        )?;
        Ok(())
    }

    /// Promotes all staged user messages for a completed Tool phase into one
    /// atomic committed-history and session-snapshot update.
    pub(crate) fn commit_staged_user_messages(
        &self,
        turn_id: &str,
        session_json: &str,
        expected_revision: i64,
    ) -> Result<CommittedStagedMessages, StorageError> {
        let mut conn = self.get_conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let chat_id: i64 = tx
            .query_row(
                "SELECT chat_id FROM turn_runs
                 WHERE turn_id = ?1 AND state = ?2",
                params![turn_id, TurnRunState::ToolsCompleted.to_string()],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| {
                StorageError::Conflict(format!(
                    "staged follow-up commit requires tools_completed turn: {turn_id}"
                ))
            })?;
        let (current_revision, next_seq) =
            read_conversation_commit_cursor(&tx, chat_id, Some(expected_revision), false)?;

        let mut staged = {
            let mut stmt = tx.prepare(
                "SELECT id, chat_id, sender_id, content, sender_kind, timestamp,
                        message_kind, recipient_agent_id, seq, turn_id, parent_message_id
                 FROM messages
                 WHERE chat_id = ?1 AND turn_id = ?2 AND seq IS NULL
                   AND sender_kind = 'user' AND message_kind = 'message'
                 ORDER BY timestamp ASC, id ASC",
            )?;
            stmt.query_map(params![chat_id, turn_id], row_to_stored_message)?
                .collect::<Result<Vec<_>, _>>()?
        };
        if staged.is_empty() {
            tx.commit()?;
            return Ok(CommittedStagedMessages {
                revision: current_revision,
                messages: staged,
            });
        }

        for (offset, message) in staged.iter_mut().enumerate() {
            let seq = next_seq + offset as i64;
            let updated = tx.execute(
                "UPDATE messages SET seq = ?3
                 WHERE id = ?1 AND chat_id = ?2 AND seq IS NULL",
                params![&message.id, chat_id, seq],
            )?;
            if updated != 1 {
                return Err(StorageError::Conflict(format!(
                    "staged follow-up disappeared during commit: {}",
                    message.id
                )));
            }
            message.seq = Some(seq);
        }
        let last_seq = staged
            .last()
            .and_then(|message| message.seq)
            .unwrap_or(next_seq);
        let now = chrono::Utc::now().to_rfc3339();
        write_session_snapshot_locked(&tx, chat_id, session_json, &now, last_seq)?;
        let count = staged.len() as i64;
        advance_chat_revision_locked(&tx, chat_id, count, &now)?;
        tx.commit()?;
        Ok(CommittedStagedMessages {
            revision: current_revision + count,
            messages: staged,
        })
    }
}

// ---------------------------------------------------------------------------
// Conversation writes (messages + session snapshot)
//
// Every conversation mutation routes through these methods so that per-chat
// ordering (`seq`) and optimistic concurrency (`chats.revision`) stay
// consistent across the message row and the LLM session snapshot. Each write
// that touches both a message and the session snapshot does so inside one
// SQLite transaction; the integer `revision` on `chats` is the compare-and-swap
// anchor: a caller that loaded `revision = N` can only commit while the row is
// still at `N`, otherwise the whole transaction rolls back.
// ---------------------------------------------------------------------------

/// Result of a committed conversation change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CommitOutcome {
    /// `chats.revision` after this commit.
    pub(super) revision: i64,
    /// Per-chat `seq` assigned to the persisted message.
    pub(super) seq: i64,
}

/// Ensures a `chats` row exists for `chat_id` before any CAS (`revision`) or
/// per-chat `seq` (`next_message_seq`) bookkeeping reads from it.
///
/// A bare `chat_id` — a test seed, or a session saved before the first
/// message — must not fail the later `SELECT revision FROM chats` with
/// `QueryReturnedNoRows`. The row is created with `revision = 0` and
/// `next_message_seq = 0` so subsequent bumps start from a clean slate. In
/// production `resolve_or_create_chat_id` already created the row, so this
/// degrades to a no-op `INSERT OR IGNORE`.
fn ensure_chat_row(tx: &rusqlite::Transaction<'_>, chat_id: i64) -> Result<(), StorageError> {
    tx.execute(
        "INSERT OR IGNORE INTO chats (chat_id, last_message_time)
         VALUES (?1, ?2)",
        params![chat_id, chrono::Utc::now().to_rfc3339()],
    )?;
    Ok(())
}

/// Reads the shared conversation commit cursor and applies its revision CAS.
fn read_conversation_commit_cursor(
    tx: &rusqlite::Transaction<'_>,
    chat_id: i64,
    expected_revision: Option<i64>,
    require_new_session: bool,
) -> Result<(i64, i64), StorageError> {
    ensure_chat_row(tx, chat_id)?;
    let (current_revision, next_seq): (i64, i64) = tx.query_row(
        "SELECT revision, next_message_seq FROM chats WHERE chat_id = ?1",
        params![chat_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;

    if let Some(expected) = expected_revision {
        if expected != current_revision {
            return Err(StorageError::SessionSnapshotConflict);
        }
    } else if require_new_session {
        let exists: bool = tx
            .query_row(
                "SELECT 1 FROM sessions WHERE chat_id = ?1 LIMIT 1",
                params![chat_id],
                |_| Ok(true),
            )
            .optional()?
            .unwrap_or(false);
        if exists {
            return Err(StorageError::SessionSnapshotConflict);
        }
    }

    Ok((current_revision, next_seq))
}

fn write_session_snapshot_locked(
    tx: &rusqlite::Transaction<'_>,
    chat_id: i64,
    session_json: &str,
    updated_at: &str,
    snapshot_through_seq: i64,
) -> Result<(), StorageError> {
    tx.execute(
        "INSERT INTO sessions
             (chat_id, messages_json, updated_at, snapshot_through_seq)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(chat_id) DO UPDATE SET
            messages_json = excluded.messages_json,
            updated_at = excluded.updated_at,
            snapshot_through_seq = excluded.snapshot_through_seq",
        params![chat_id, session_json, updated_at, snapshot_through_seq],
    )?;
    Ok(())
}

fn advance_chat_revision_locked(
    tx: &rusqlite::Transaction<'_>,
    chat_id: i64,
    committed_count: i64,
    last_message_time: &str,
) -> Result<(), StorageError> {
    tx.execute(
        "UPDATE chats
         SET revision = revision + ?2,
             next_message_seq = next_message_seq + ?2,
             last_message_time = ?3
         WHERE chat_id = ?1",
        params![chat_id, committed_count, last_message_time],
    )?;
    Ok(())
}

/// Appends one message and, when `session_json` is provided, advances the LLM
/// session snapshot in the same transaction.
///
/// Issues the next per-chat integer `seq`, writes the message row with that
/// `seq` (plus `turn_id` / `parent_message_id` when supplied), upserts the
/// session snapshot with `snapshot_through_seq = seq`, and bumps
/// `chats.revision` and `chats.next_message_seq` by one.
///
/// `expected_revision` is the optimistic-concurrency token:
/// * `Some(n)` — the commit only applies while `chats.revision == n`; any
///   other value rolls the transaction back with
///   [`StorageError::SessionSnapshotConflict`].
/// * `None` — the session row must not yet exist (initial seed); if a session
///   already exists the call conflicts.
pub(super) fn commit_message_locked(
    tx: &rusqlite::Transaction<'_>,
    message: &StoredMessage,
    session_json: Option<&str>,
    expected_revision: Option<i64>,
) -> Result<CommitOutcome, StorageError> {
    let (current_revision, next_seq) = read_conversation_commit_cursor(
        tx,
        message.chat_id,
        expected_revision,
        session_json.is_some(),
    )?;

    let seq = next_seq;
    let inserted = tx.execute(
        "INSERT OR IGNORE INTO messages
             (id, chat_id, sender_id, content, sender_kind, timestamp,
              message_kind, recipient_agent_id, seq, turn_id, parent_message_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            &message.id,
            message.chat_id,
            &message.sender_id,
            &message.content,
            message.sender_kind.to_string(),
            &message.timestamp,
            message.message_kind.to_string(),
            message.recipient_agent_id.as_deref(),
            seq,
            message.turn_id.as_deref(),
            message.parent_message_id.as_deref(),
        ],
    )?;

    // Idempotent re-commit: the message row already exists (e.g. a Turn whose
    // deterministic message id is re-persisted on recovery). Verify the stored
    // row matches the incoming one — content, sender, and message kind must
    // agree. A mismatch means the same id was reused for a different message
    // and is rejected. Do not advance seq or revision — that would create a
    // gap and a spurious CAS conflict for the next writer.
    if inserted == 0 {
        let (stored_content, stored_sender, stored_kind, stored_turn, stored_parent): (
            String,
            String,
            String,
            Option<String>,
            Option<String>,
        ) = tx.query_row(
            "SELECT content, sender_id, message_kind, turn_id, parent_message_id
             FROM messages WHERE id = ?1 AND chat_id = ?2",
            params![&message.id, message.chat_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )?;
        if stored_content != message.content
            || stored_sender != message.sender_id
            || stored_kind != message.message_kind.to_string()
            || stored_turn.as_deref() != message.turn_id.as_deref()
            || stored_parent.as_deref() != message.parent_message_id.as_deref()
        {
            return Err(StorageError::Conflict(format!(
                "message id collision: {} already exists with different content",
                message.id
            )));
        }
        return Ok(CommitOutcome {
            revision: current_revision,
            seq,
        });
    }

    let now = chrono::Utc::now().to_rfc3339();

    if let Some(json) = session_json {
        write_session_snapshot_locked(tx, message.chat_id, json, &now, seq)?;
    }
    advance_chat_revision_locked(tx, message.chat_id, 1, &now)?;

    Ok(CommitOutcome {
        revision: current_revision + 1,
        seq,
    })
}

impl Database {
    /// Commits one message and, when `session_json` is provided, advances the
    /// LLM session snapshot in the same transaction. See
    /// [`commit_message_locked`] for the concurrency contract.
    fn commit_conversation_message(
        &self,
        message: &StoredMessage,
        session_json: Option<&str>,
        expected_revision: Option<i64>,
    ) -> Result<CommitOutcome, StorageError> {
        let mut conn = self.get_conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let outcome = commit_message_locked(&tx, message, session_json, expected_revision)?;
        tx.commit()?;
        Ok(outcome)
    }

    /// Advances the LLM session snapshot without appending a message.
    ///
    /// Used by compaction and by unconditional session seeds. Bumps
    /// `chats.revision` so the change is observable under the same CAS
    /// contract. `snapshot_through_seq` is left at the chat's current maximum
    /// `seq` (no new message was added).
    fn update_session_snapshot(
        &self,
        chat_id: i64,
        session_json: &str,
        expected_revision: Option<i64>,
    ) -> Result<CommitOutcome, StorageError> {
        let mut conn = self.get_conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure_chat_row(&tx, chat_id)?;

        let (current_revision, max_seq): (i64, Option<i64>) = tx.query_row(
            "SELECT c.revision,
                    (SELECT MAX(m.seq) FROM messages m WHERE m.chat_id = c.chat_id)
             FROM chats c WHERE c.chat_id = ?1",
            params![chat_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;

        if let Some(expected) = expected_revision {
            if expected != current_revision {
                tx.rollback()?;
                return Err(StorageError::SessionSnapshotConflict);
            }
        }

        let now = chrono::Utc::now().to_rfc3339();
        let through = max_seq.unwrap_or(0);
        tx.execute(
            "INSERT INTO sessions
                 (chat_id, messages_json, updated_at, snapshot_through_seq)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(chat_id) DO UPDATE SET
                messages_json = excluded.messages_json,
                updated_at = excluded.updated_at,
                snapshot_through_seq = excluded.snapshot_through_seq",
            params![chat_id, session_json, &now, through],
        )?;
        tx.execute(
            "UPDATE chats SET revision = revision + 1, last_message_time = ?2 WHERE chat_id = ?1",
            params![chat_id, &now],
        )?;

        tx.commit()?;
        Ok(CommitOutcome {
            revision: current_revision + 1,
            seq: through,
        })
    }

    /// Appends a message to a chat that has no session snapshot (multi-agent
    /// Channel Log). Issues `seq` and advances `revision` but never touches
    /// `sessions`.
    fn store_channel_log_message(&self, message: &StoredMessage) -> Result<i64, StorageError> {
        let outcome = self.commit_conversation_message(message, None, None)?;
        Ok(outcome.seq)
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
        self.update_session_snapshot(chat_id, messages_json, None)?;
        Ok(())
    }

    /// Clears session message history by setting `messages_json` to an empty
    /// JSON array.  The session row itself and `messages` / `tool_calls`
    /// records are preserved.
    ///
    /// Uses optimistic concurrency on `chats.revision`: the update only
    /// succeeds when `expected_revision` matches the current row.  Returns
    /// `Ok(true)` if the snapshot was updated, `Ok(false)` if the revision did
    /// not match (concurrent modification).
    pub(crate) fn clear_session_messages(
        &self,
        chat_id: i64,
        expected_revision: i64,
    ) -> Result<bool, StorageError> {
        match self.update_session_snapshot(chat_id, "[]", Some(expected_revision)) {
            Ok(_) => Ok(true),
            Err(StorageError::SessionSnapshotConflict) => Ok(false),
            Err(other) => Err(other),
        }
    }

    /// Updates `sessions.messages_json` to `new_messages_json` and bumps
    /// `chats.revision`. Unlike [`Database::clear_session_messages`], which
    /// wipes to `[]`, this keeps a caller-supplied payload — used by the sleep
    /// batch to retain the trailing N messages while still archiving the full
    /// conversation.
    ///
    /// Uses optimistic concurrency on `chats.revision`: the update only
    /// succeeds when `expected_revision` matches the current row. Returns
    /// `Ok(true)` if the snapshot was updated, `Ok(false)` on concurrent
    /// modification or missing row.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if the underlying SQLite update fails.
    pub(crate) fn truncate_session_messages(
        &self,
        chat_id: i64,
        expected_revision: i64,
        new_messages_json: &str,
    ) -> Result<bool, StorageError> {
        match self.update_session_snapshot(chat_id, new_messages_json, Some(expected_revision)) {
            Ok(_) => Ok(true),
            Err(StorageError::SessionSnapshotConflict) => Ok(false),
            Err(other) => Err(other),
        }
    }

    /// Persists one message and, when `messages_json` is provided, advances the
    /// LLM session snapshot in the same transaction.
    ///
    /// Uses optimistic concurrency on `chats.revision`: `Some(n)` commits only
    /// while the row is still at `n`, otherwise the whole transaction rolls
    /// back with [`StorageError::SessionSnapshotConflict`]; `None` requires the
    /// session row to not yet exist (initial seed). Returns the new
    /// `chats.revision` to be used as the CAS token for the next mutation.
    pub(crate) fn store_message_with_session(
        &self,
        message: &StoredMessage,
        messages_json: &str,
        expected_revision: Option<i64>,
    ) -> Result<i64, StorageError> {
        let outcome =
            self.commit_conversation_message(message, Some(messages_json), expected_revision)?;
        Ok(outcome.revision)
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
                "SELECT messages_json FROM sessions WHERE chat_id = ?1",
                params![chat_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?;

        let revision: i64 = tx
            .query_row(
                "SELECT revision FROM chats WHERE chat_id = ?1",
                params![chat_id],
                |row| row.get(0),
            )
            .optional()?
            .unwrap_or(0);

        let recent_messages = {
            let mut stmt = tx.prepare(
                "SELECT id, chat_id, sender_id, content, sender_kind, timestamp,
                        message_kind, recipient_agent_id, seq, turn_id, parent_message_id
                 FROM messages
                 WHERE chat_id = ?1 AND seq IS NOT NULL
                 ORDER BY seq DESC
                 LIMIT ?2",
            )?;
            let mut messages = stmt
                .query_map(params![chat_id, limit as i64], row_to_stored_message)?
                .collect::<Result<Vec<_>, _>>()?;
            messages.reverse();
            messages
        };

        tx.commit()?;

        let messages_json = session.clone();
        let session_revision = session.map(|_| revision);

        Ok(SessionSnapshot {
            messages_json,
            session_revision,
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

    /// Returns recent public Channel Log events projected for one target agent.
    ///
    /// Events already delivered to the target are omitted because the target
    /// session owns their Direct Input. The target's own assistant/tool
    /// events are omitted for the same reason: they are already in that
    /// session and would otherwise be reintroduced through the room log.
    /// Internal tool and system events are never part of the shared context.
    pub(crate) fn get_channel_log_messages_for_agent(
        &self,
        chat_id: i64,
        agent_id: &str,
        limit: usize,
    ) -> Result<Vec<StoredMessage>, StorageError> {
        let conn = self.get_conn()?;
        let mut stmt = conn.prepare_cached(
            "SELECT id, chat_id, sender_id, content, sender_kind, timestamp,
                    message_kind, recipient_agent_id, seq, turn_id, parent_message_id
             FROM messages
             WHERE chat_id = ?1
               AND seq IS NOT NULL
               AND (recipient_agent_id IS NULL OR recipient_agent_id <> ?2)
               AND NOT (sender_id = ?2 AND sender_kind IN ('assistant', 'tool'))
               AND (
                    (message_kind = 'message' AND sender_kind IN ('user', 'assistant'))
                    OR message_kind = 'agent_send'
               )
             ORDER BY seq DESC
             LIMIT ?3",
        )?;

        let mut messages = stmt
            .query_map(
                params![chat_id, agent_id, limit as i64],
                row_to_stored_message,
            )?
            .collect::<Result<Vec<_>, _>>()?;
        messages.reverse();
        Ok(messages)
    }

    /// Stores a message without touching the session snapshot.
    /// Used for Channel Log entries that have no Agent Session snapshot.
    pub(crate) fn store_message_only(&self, message: &StoredMessage) -> Result<(), StorageError> {
        self.store_channel_log_message(message).map(|_| ())
    }

    /// Persists a system event message to a Channel Log chat.
    ///
    /// `reason` is rendered via its `Display` implementation into a JSON object
    /// (`{"reason": "..."}`) so both chain-stop reasons and queue-capacity
    /// rejections can be recorded through the same path.
    pub(crate) fn store_system_event(
        &self,
        channel_log_chat_id: i64,
        reason: &impl fmt::Display,
    ) -> Result<(), StorageError> {
        let content = serde_json::json!({ "reason": reason.to_string() }).to_string();
        let mut message = StoredMessage::system(channel_log_chat_id, content);
        message.message_kind = MessageKind::SystemEvent;
        self.store_channel_log_message(&message).map(|_| ())
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
            seq: None,
            turn_id: None,
            parent_message_id: None,
        };
        self.store_channel_log_message(&message).map(|_| ())
    }
}

#[cfg(test)]
mod tests {
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
                "INSERT OR REPLACE INTO messages (id, chat_id, sender_id, content, sender_kind, timestamp, message_kind, seq)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7,
                    (SELECT COALESCE(MAX(seq), 0) + 1 FROM messages WHERE chat_id = ?2))",
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
        assert!(snapshot.session_revision.is_some());
        let first_session_revision = snapshot.session_revision.unwrap();
        assert!(first_session_revision > 0);

        std::thread::sleep(std::time::Duration::from_millis(10));

        let json2 = r#"[{"role":"user","content":"hello"},{"role":"assistant","content":"hi"}]"#;
        db.save_session(100, json2).expect("update session");

        let snapshot = db
            .load_session_snapshot(100, 10)
            .expect("load updated session");
        assert_eq!(snapshot.messages_json.as_deref(), Some(json2));
        assert!(snapshot.session_revision.unwrap() >= first_session_revision);
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
        let session_revision = snapshot.session_revision.expect("has revision");

        let cleared = db
            .clear_session_messages(chat_id, session_revision)
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
    fn clear_session_messages_returns_false_on_stale_revision() {
        let (db, _dir) = test_db();
        let chat_id = 200;

        db.save_session(chat_id, r#"[{"role":"user","content":"hello"}]"#)
            .expect("save session");

        let cleared = db
            .clear_session_messages(chat_id, 0)
            .expect("clear session messages");
        assert!(!cleared, "should not have updated the row");

        let snapshot = db.load_session_snapshot(chat_id, 10).expect("load session");
        assert!(
            snapshot.messages_json.as_deref() != Some(r#"[]"#),
            "messages_json should not be cleared"
        );
    }

    #[test]
    fn truncate_session_messages_replaces_json() {
        let (db, _dir) = test_db();
        let chat_id = 300;

        db.save_session(chat_id, r#"[{"role":"user","content":"old"}]"#)
            .expect("save session");
        store_msg(&db, "msg-1", chat_id, "hello", "2024-01-01T00:00:00Z");
        store_msg(&db, "msg-2", chat_id, "hi", "2024-01-01T00:00:01Z");

        let snapshot = db.load_session_snapshot(chat_id, 10).expect("load session");
        let session_revision = snapshot.session_revision.expect("has revision");

        let new_json = r#"[{"role":"assistant","content":"kept"}]"#;
        let truncated = db
            .truncate_session_messages(chat_id, session_revision, new_json)
            .expect("truncate session messages");
        assert!(truncated, "should have updated the row");

        let snapshot = db.load_session_snapshot(chat_id, 10).expect("load session");
        assert_eq!(
            snapshot.messages_json.as_deref(),
            Some(new_json),
            "messages_json should be replaced with the supplied payload"
        );

        let messages = db
            .get_recent_messages(chat_id, 10)
            .expect("load recent messages");
        assert_eq!(messages.len(), 2, "messages records should be preserved");
    }

    #[test]
    fn truncate_session_messages_returns_false_on_stale_revision() {
        let (db, _dir) = test_db();
        let chat_id = 400;

        db.save_session(chat_id, r#"[{"role":"user","content":"hello"}]"#)
            .expect("save session");

        let truncated = db
            .truncate_session_messages(chat_id, 0, r#"[]"#)
            .expect("truncate session messages");
        assert!(!truncated, "should not have updated the row");

        let snapshot = db.load_session_snapshot(chat_id, 10).expect("load session");
        assert!(
            snapshot.messages_json.as_deref() != Some(r#"[]"#),
            "messages_json should not be modified"
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
            seq: None,
            turn_id: None,
            parent_message_id: None,
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
                seq: None,
                turn_id: None,
                parent_message_id: None,
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
    fn stage_tool_followup_is_idempotent_and_hidden_from_committed_history() {
        // Arrange
        let (db, _dir) = test_db();
        let chat_id = db
            .resolve_or_create_chat_id("web", "web:follow-up", None, "web", "default")
            .expect("create chat");
        db.save_session(chat_id, r#"[{"role":"user","content":"base"}]"#)
            .expect("seed session");
        let turn_id = match db
            .accept_or_get_turn(super::super::AcceptTurnParams {
                chat_id,
                request_key: "turn-request",
                config_revision: 1,
                config_fingerprint: Some("fingerprint"),
                request_payload_hash: "payload",
                origin_id: None,
                scheduled_request_json: None,
            })
            .expect("accept turn")
        {
            super::super::AcceptOutcome::Created(run) => run.turn_id,
            super::super::AcceptOutcome::Existing(_) => panic!("expected new turn"),
        };
        db.get_conn()
            .expect("connection")
            .execute(
                "UPDATE turn_runs SET state = 'tools_pending' WHERE turn_id = ?1",
                rusqlite::params![&turn_id],
            )
            .expect("seed tools pending state");

        // Act
        let staged = db
            .stage_tool_followup(
                chat_id,
                "web:follow-up-request",
                "follow-up-hash",
                "web-user",
                "please also check this",
                "2026-08-28T12:00:00Z",
            )
            .expect("stage follow-up");
        let duplicate = db
            .stage_tool_followup(
                chat_id,
                "web:follow-up-request",
                "follow-up-hash",
                "web-user",
                "please also check this",
                "2026-08-28T12:01:00Z",
            )
            .expect("stage duplicate follow-up");
        let conflicting = db.stage_tool_followup(
            chat_id,
            "web:follow-up-request",
            "conflicting-hash",
            "web-user",
            "different payload",
            "2026-08-28T12:02:00Z",
        );

        // Assert
        let super::super::StageToolFollowupOutcome::Accepted(message) = staged else {
            panic!("expected staged follow-up");
        };
        assert_eq!(message.seq, None);
        assert_eq!(message.turn_id.as_deref(), Some(turn_id.as_str()));
        assert_eq!(
            duplicate,
            super::super::StageToolFollowupOutcome::Accepted(message)
        );
        assert!(matches!(conflicting, Err(StorageError::Conflict(_))));
        assert!(
            db.get_all_messages(chat_id)
                .expect("committed history")
                .is_empty(),
            "staged rows must not appear in committed history"
        );
        assert!(
            db.list_sessions()
                .expect("session list")
                .iter()
                .all(|session| session.last_message_preview.is_none()),
            "staged rows must not update session previews"
        );
        let snapshot = db.load_session_snapshot(chat_id, 10).expect("snapshot");
        assert_eq!(
            snapshot.messages_json.as_deref(),
            Some(r#"[{"role":"user","content":"base"}]"#),
            "staging must not update the session snapshot"
        );
        assert!(
            snapshot.recent_messages.is_empty(),
            "staged rows must not enter recent committed messages"
        );
        assert_eq!(
            db.count_agent_pending_sleep_messages("default")
                .expect("sleep message count"),
            0,
            "staged rows must not enter Sleep sources"
        );
    }

    #[test]
    fn commit_staged_tool_followups_assigns_fifo_seq_and_snapshot_atomically() {
        // Arrange
        let (db, _dir) = test_db();
        let chat_id = db
            .resolve_or_create_chat_id("web", "web:follow-up-commit", None, "web", "default")
            .expect("create chat");
        let turn_id = match db
            .accept_or_get_turn(super::super::AcceptTurnParams {
                chat_id,
                request_key: "turn-request",
                config_revision: 1,
                config_fingerprint: Some("fingerprint"),
                request_payload_hash: "payload",
                origin_id: None,
                scheduled_request_json: None,
            })
            .expect("accept turn")
        {
            super::super::AcceptOutcome::Created(run) => run.turn_id,
            super::super::AcceptOutcome::Existing(_) => panic!("expected new turn"),
        };
        db.get_conn()
            .expect("connection")
            .execute(
                "UPDATE turn_runs SET state = 'tools_pending' WHERE turn_id = ?1",
                rusqlite::params![&turn_id],
            )
            .expect("seed tools pending state");
        db.stage_tool_followup(
            chat_id,
            "follow-up-late",
            "follow-up-late-hash",
            "web-user",
            "late",
            "2026-08-28T12:02:00Z",
        )
        .expect("stage late follow-up");
        db.stage_tool_followup(
            chat_id,
            "follow-up-early",
            "follow-up-early-hash",
            "web-user",
            "early",
            "2026-08-28T12:01:00Z",
        )
        .expect("stage early follow-up");
        db.get_conn()
            .expect("connection")
            .execute(
                "UPDATE turn_runs SET state = 'tools_completed' WHERE turn_id = ?1",
                rusqlite::params![&turn_id],
            )
            .expect("complete tools");
        db.save_session(chat_id, "[]").expect("seed session");
        let revision = db
            .load_session_snapshot(chat_id, 10)
            .expect("load session")
            .session_revision
            .expect("session revision");

        // Act
        let committed = db
            .commit_staged_user_messages(
                &turn_id,
                r#"[{"role":"user","content":"base"},{"role":"user","content":"early"},{"role":"user","content":"late"}]"#,
                revision,
            )
            .expect("commit staged follow-ups");

        // Assert
        assert_eq!(
            committed
                .messages
                .iter()
                .map(|message| (message.content.as_str(), message.seq))
                .collect::<Vec<_>>(),
            vec![("early", Some(1)), ("late", Some(2))]
        );
        let history = db.get_all_messages(chat_id).expect("committed history");
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].content, "early");
        assert_eq!(history[1].content, "late");
        let snapshot = db.load_session_snapshot(chat_id, 10).expect("snapshot");
        assert_eq!(
            snapshot.messages_json.as_deref(),
            Some(
                r#"[{"role":"user","content":"base"},{"role":"user","content":"early"},{"role":"user","content":"late"}]"#
            )
        );
        assert_eq!(snapshot.session_revision, Some(revision + 2));
        assert!(
            db.list_staged_user_messages(&turn_id)
                .expect("staged rows")
                .is_empty()
        );
    }

    #[test]
    fn stage_tool_followup_returns_no_tool_phase_without_unique_pending_turn() {
        // Arrange
        let (db, _dir) = test_db();
        let chat_id = db
            .resolve_or_create_chat_id("web", "web:no-tool-phase", None, "web", "default")
            .expect("create chat");

        // Act
        let outcome = db
            .stage_tool_followup(
                chat_id,
                "request-without-tools",
                "request-without-tools-hash",
                "web-user",
                "ordinary message",
                "2026-08-28T12:00:00Z",
            )
            .expect("stage lookup");

        // Assert
        assert_eq!(outcome, super::super::StageToolFollowupOutcome::NoToolPhase);
        assert!(db.get_all_messages(chat_id).expect("history").is_empty());
    }

    #[test]
    fn stage_tool_followup_delegates_existing_turn_identity_to_normal_handling() {
        // Arrange
        let (db, _dir) = test_db();
        let chat_id = db
            .resolve_or_create_chat_id("web", "web:existing-turn", None, "web", "default")
            .expect("create chat");
        db.accept_or_get_turn(super::super::AcceptTurnParams {
            chat_id,
            request_key: "existing-request",
            config_revision: 1,
            config_fingerprint: Some("fingerprint"),
            request_payload_hash: "same-hash",
            origin_id: None,
            scheduled_request_json: None,
        })
        .expect("accept existing turn");

        // Act
        let duplicate = db
            .stage_tool_followup(
                chat_id,
                "existing-request",
                "same-hash",
                "web-user",
                "same input",
                "2026-08-28T12:00:00Z",
            )
            .expect("stage duplicate request");
        let conflicting = db.stage_tool_followup(
            chat_id,
            "existing-request",
            "different-hash",
            "web-user",
            "different input",
            "2026-08-28T12:01:00Z",
        );

        // Assert
        assert_eq!(
            duplicate,
            super::super::StageToolFollowupOutcome::NoToolPhase
        );
        assert!(matches!(conflicting, Err(StorageError::Conflict(_))));
        assert!(db.get_all_messages(chat_id).expect("history").is_empty());
    }

    #[test]
    fn stage_tool_followup_rejects_multiple_tools_pending_turns() {
        // Arrange
        let (db, _dir) = test_db();
        let chat_id = db
            .resolve_or_create_chat_id("web", "web:multiple-tools", None, "web", "default")
            .expect("create chat");
        for request_key in ["tool-turn-a", "tool-turn-b"] {
            let turn_id = match db
                .accept_or_get_turn(super::super::AcceptTurnParams {
                    chat_id,
                    request_key,
                    config_revision: 1,
                    config_fingerprint: Some("fingerprint"),
                    request_payload_hash: request_key,
                    origin_id: None,
                    scheduled_request_json: None,
                })
                .expect("accept turn")
            {
                super::super::AcceptOutcome::Created(run) => run.turn_id,
                super::super::AcceptOutcome::Existing(_) => panic!("expected new turn"),
            };
            db.get_conn()
                .expect("connection")
                .execute(
                    "UPDATE turn_runs SET state = 'tools_pending' WHERE turn_id = ?1",
                    rusqlite::params![turn_id],
                )
                .expect("seed tools pending state");
        }

        // Act
        let outcome = db.stage_tool_followup(
            chat_id,
            "follow-up-request",
            "follow-up-request-hash",
            "web-user",
            "follow-up",
            "2026-08-28T12:00:00Z",
        );

        // Assert
        assert!(matches!(outcome, Err(StorageError::Conflict(_))));
        assert!(db.get_all_messages(chat_id).expect("history").is_empty());
    }

    #[test]
    fn staged_commit_rolls_back_when_message_sequence_conflicts() {
        // Arrange
        let (db, _dir) = test_db();
        let chat_id = db
            .resolve_or_create_chat_id("web", "web:staged-rollback", None, "web", "default")
            .expect("create chat");
        let turn_id = match db
            .accept_or_get_turn(super::super::AcceptTurnParams {
                chat_id,
                request_key: "rollback-turn",
                config_revision: 1,
                config_fingerprint: Some("fingerprint"),
                request_payload_hash: "payload",
                origin_id: None,
                scheduled_request_json: None,
            })
            .expect("accept turn")
        {
            super::super::AcceptOutcome::Created(run) => run.turn_id,
            super::super::AcceptOutcome::Existing(_) => panic!("expected new turn"),
        };
        db.get_conn()
            .expect("connection")
            .execute(
                "UPDATE turn_runs SET state = 'tools_pending' WHERE turn_id = ?1",
                rusqlite::params![&turn_id],
            )
            .expect("seed tools pending state");
        db.stage_tool_followup(
            chat_id,
            "rollback-follow-up",
            "rollback-follow-up-hash",
            "web-user",
            "follow-up",
            "2026-08-28T12:00:00Z",
        )
        .expect("stage follow-up");
        db.get_conn()
            .expect("connection")
            .execute(
                "UPDATE turn_runs SET state = 'tools_completed' WHERE turn_id = ?1",
                rusqlite::params![&turn_id],
            )
            .expect("complete tools");
        db.save_session(chat_id, "[]").expect("seed session");
        let before = db
            .load_session_snapshot(chat_id, 10)
            .expect("snapshot before")
            .session_revision
            .expect("session revision");
        db.get_conn()
            .expect("connection")
            .execute(
                "INSERT INTO messages
                 (id, chat_id, sender_id, content, sender_kind, timestamp, message_kind, seq)
                 VALUES ('sequence-collision', ?1, 'system', 'collision', 'system',
                         '2026-08-28T11:00:00Z', 'system_event', 1)",
                rusqlite::params![chat_id],
            )
            .expect("seed sequence collision");

        // Act
        let outcome = db.commit_staged_user_messages(&turn_id, "[\"new\"]", before);

        // Assert
        assert!(
            outcome.is_err(),
            "sequence collision must fail the transaction"
        );
        assert_eq!(
            db.list_staged_user_messages(&turn_id)
                .expect("staged rows after rollback")
                .len(),
            1,
            "staged row must remain uncommitted"
        );
        let snapshot = db
            .load_session_snapshot(chat_id, 10)
            .expect("snapshot after");
        assert_eq!(snapshot.session_revision, Some(before));
        assert_eq!(snapshot.messages_json.as_deref(), Some("[]"));
        let revision: i64 = db
            .get_conn()
            .expect("connection")
            .query_row(
                "SELECT revision FROM chats WHERE chat_id = ?1",
                rusqlite::params![chat_id],
                |row| row.get(0),
            )
            .expect("chat revision");
        assert_eq!(revision, before);
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
    fn list_sessions_orders_by_latest_message_timestamp() {
        let (db, _dir) = test_db();

        // Two chats created in order; A is older at creation time.
        let chat_a = db
            .resolve_or_create_chat_id("cli", "cli:a", Some("a"), "cli", "default")
            .expect("create chat A");
        let chat_b = db
            .resolve_or_create_chat_id("cli", "cli:b", Some("b"), "cli", "default")
            .expect("create chat B");

        // Same initial message time, then A receives a newer message later.
        // A must sort above B even though B was created more recently.
        store_msg(&db, "m-a1", chat_a, "a-first", "2024-01-01T00:00:00Z");
        store_msg(&db, "m-b1", chat_b, "b-first", "2024-01-01T00:00:00Z");
        store_msg(&db, "m-a2", chat_a, "a-second", "2024-01-02T00:00:00Z");

        let sessions = db.list_sessions().expect("list sessions");
        assert_eq!(sessions.len(), 2);
        assert_eq!(
            sessions[0].chat_id, chat_a,
            "chat with the latest message must sort first"
        );
        assert_eq!(sessions[1].chat_id, chat_b);
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

    #[test]
    fn pending_sleep_messages_exclude_other_agents() {
        let (db, _dir) = test_db();

        let chat_id = db
            .resolve_or_create_chat_id("cli", "cli:msgs-a", None, "cli", "agent-a")
            .expect("create chat");

        store_msg(&db, "msg-1", chat_id, "message", "2024-01-01T00:00:00Z");

        let count = db
            .count_agent_pending_sleep_messages("agent-a")
            .expect("count");
        assert_eq!(count, 1);
    }

    #[test]
    fn get_pending_sleep_sessions_returns_empty_for_unknown_agent() {
        let (db, _dir) = test_db();

        let sessions = db
            .get_agent_sessions_with_pending_sleep_messages("nonexistent-agent", 10)
            .expect("get sessions");
        assert!(sessions.is_empty());
    }

    // --- Channel Log tests ---

    #[test]
    fn store_message_to_channel_log() {
        let (db, _dir) = test_db();

        let chat_id = db.resolve_channel_log_chat_id(100).expect("create");
        store_msg(&db, "cl-1", chat_id, "hello", "2025-01-01T00:00:00Z");

        let msgs = db.get_recent_messages(chat_id, 10).expect("messages");
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

        let msgs = db.get_recent_messages(chat_id, 3).expect("messages");
        assert_eq!(msgs.len(), 3);
        assert_eq!(msgs[0].content, "msg 2");
        assert_eq!(msgs[2].content, "msg 4");
    }

    #[test]
    fn channel_log_projection_excludes_target_delivery_and_own_events() {
        let (db, _dir) = test_db();
        let chat_id = db.resolve_channel_log_chat_id(201).expect("create");

        let mut target_input = StoredMessage::user(
            chat_id,
            "user:discord:1".to_string(),
            "target direct input".to_string(),
        );
        target_input.recipient_agent_id = Some("vega".to_string());

        let mut other_input = StoredMessage::user(
            chat_id,
            "user:discord:2".to_string(),
            "other direct input".to_string(),
        );
        other_input.recipient_agent_id = Some("lyre".to_string());

        let ambient_input = StoredMessage::user(
            chat_id,
            "user:discord:3".to_string(),
            "ambient room input".to_string(),
        );
        let own_response =
            StoredMessage::assistant(chat_id, "vega".to_string(), "vega response".to_string());
        let own_tool = StoredMessage::tool(
            chat_id,
            "vega".to_string(),
            "lyre".to_string(),
            "vega tool event".to_string(),
        );
        let other_response =
            StoredMessage::assistant(chat_id, "lyre".to_string(), "lyre response".to_string());
        let mut other_tool = StoredMessage::tool(
            chat_id,
            "lyre".to_string(),
            "shell".to_string(),
            "private tool result".to_string(),
        );
        other_tool.message_kind = MessageKind::ToolCall;
        let mut system_event = StoredMessage::system(chat_id, "private system event".to_string());
        system_event.message_kind = MessageKind::SystemEvent;
        let mut send_to_target = StoredMessage::tool(
            chat_id,
            "lyre".to_string(),
            "vega".to_string(),
            "send to vega".to_string(),
        );
        send_to_target.message_kind = MessageKind::AgentSend;
        let mut send_to_other = StoredMessage::tool(
            chat_id,
            "lyre".to_string(),
            "lyre".to_string(),
            "send to lyre".to_string(),
        );
        send_to_other.message_kind = MessageKind::AgentSend;

        for message in [
            target_input,
            other_input,
            ambient_input,
            own_response,
            own_tool,
            other_response,
            other_tool,
            system_event,
            send_to_target,
            send_to_other,
        ] {
            db.store_message_only(&message)
                .expect("store channel event");
        }

        let projected = db
            .get_channel_log_messages_for_agent(chat_id, "vega", 20)
            .expect("project channel log");
        let contents: Vec<_> = projected
            .iter()
            .map(|message| message.content.as_str())
            .collect();

        assert!(contents.contains(&"other direct input"));
        assert!(contents.contains(&"ambient room input"));
        assert!(contents.contains(&"lyre response"));
        assert!(contents.contains(&"send to lyre"));
        assert!(!contents.contains(&"target direct input"));
        assert!(!contents.contains(&"vega response"));
        assert!(!contents.contains(&"vega tool event"));
        assert!(!contents.contains(&"private tool result"));
        assert!(!contents.contains(&"private system event"));
        assert!(!contents.contains(&"send to vega"));

        let conn = db.get_conn().expect("pool");
        let session_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sessions WHERE chat_id = ?1",
                rusqlite::params![chat_id],
                |row| row.get(0),
            )
            .expect("session count");
        assert_eq!(session_count, 0, "Channel Log must not create a session");
    }

    // ---- System Event tests ----

    #[test]
    fn store_system_event_saves_to_channel_log() {
        let (db, _dir) = test_db();

        let chat_id = db.resolve_channel_log_chat_id(300).expect("create");
        db.store_system_event(
            chat_id,
            &crate::runtime::turn::StopReason::ChainDepthExceeded,
        )
        .expect("store");

        let msgs = db.get_recent_messages(chat_id, 10).expect("messages");
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].message_kind, MessageKind::SystemEvent);
    }

    #[test]
    fn store_system_event_content_is_valid_json_with_reason() {
        let (db, _dir) = test_db();

        let chat_id = db.resolve_channel_log_chat_id(301).expect("create");
        db.store_system_event(
            chat_id,
            &crate::runtime::turn::StopReason::TurnCountExceeded,
        )
        .expect("store");

        let msgs = db.get_recent_messages(chat_id, 10).expect("messages");
        let parsed: serde_json::Value = serde_json::from_str(&msgs[0].content).expect("valid json");
        assert!(parsed.get("reason").is_some());
    }

    #[test]
    fn store_system_event_sender_is_system() {
        let (db, _dir) = test_db();

        let chat_id = db.resolve_channel_log_chat_id(302).expect("create");
        db.store_system_event(chat_id, &crate::runtime::turn::StopReason::LlmFailure)
            .expect("store");

        let msgs = db.get_recent_messages(chat_id, 10).expect("messages");
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
            id: "msg-assitant".to_string(),
            chat_id,
            sender_id: "lyre".to_string(),
            content: "assitant says hi".to_string(),
            sender_kind: SenderKind::Assistant,
            timestamp: "2024-01-01T00:00:00Z".to_string(),
            message_kind: MessageKind::Message,
            recipient_agent_id: None,
            seq: None,
            turn_id: None,
            parent_message_id: None,
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
                "INSERT INTO messages (id, chat_id, sender_id, content, sender_kind, timestamp, message_kind, seq)
                 VALUES ('m1', ?1, 'user:cli:alice', 'hello', 'user', '2024-01-01T00:00:00Z', 'message', 1)",
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
        db.store_system_event(chat_id, &crate::runtime::turn::StopReason::LlmFailure)
            .expect("store");

        let msgs = db.get_all_messages(chat_id).expect("messages");
        let found = msgs.iter().find(|m| m.content.contains("llm_failure"));
        assert!(found.is_some(), "should find system event by content");
        assert_eq!(found.unwrap().sender_kind, SenderKind::System);
    }

    #[test]
    fn store_system_event_sets_system_kind() {
        let (db, _dir) = test_db();

        let chat_id = db.resolve_channel_log_chat_id(600).expect("create");
        db.store_system_event(
            chat_id,
            &crate::runtime::turn::StopReason::ChainDepthExceeded,
        )
        .expect("store");

        let msgs = db.get_recent_messages(chat_id, 10).expect("messages");
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

        let msgs = db.get_recent_messages(chat_id, 10).expect("messages");
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
    fn get_messages_after_cursor_respects_composite_upper_bound() {
        // Arrange
        let (db, _dir) = test_db();
        let chat_id = db
            .resolve_or_create_chat_id("cli", "cli:cursor-bound", None, "cli", "agent-a")
            .expect("create chat");
        let timestamp = "2025-01-01T00:00:00Z";
        store_msg(&db, "m1", chat_id, "first", timestamp);
        store_msg(&db, "m2", chat_id, "upper", timestamp);
        store_msg(&db, "m3", chat_id, "inserted later", timestamp);

        // Act
        let messages = db
            .get_messages_after_cursor(chat_id, None, (timestamp, "m2"))
            .expect("query");

        // Assert
        assert_eq!(
            messages
                .iter()
                .map(|message| message.id.as_str())
                .collect::<Vec<_>>(),
            vec!["m1", "m2"]
        );
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
}
