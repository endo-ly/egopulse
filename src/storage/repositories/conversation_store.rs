//! `ConversationStore`: the single authoritative boundary for all
//! `messages` and `sessions` persistence.
//!
//! Every conversation mutation routes through this store so that per-chat
//! ordering (`seq`) and optimistic concurrency (`chats.revision`) stay
//! consistent across the message row and the LLM session snapshot.
//!
//! Every method that touches both a message and a session snapshot does so
//! inside one SQLite transaction. The integer `revision` on `chats` is the
//! compare-and-swap anchor: a caller that loaded `revision = N` can only
//! commit while the row is still at `N`; otherwise the whole transaction
//! rolls back and the caller retries or reports a conflict.

use std::fmt;

use rusqlite::OptionalExtension;
use rusqlite::params;

use crate::error::StorageError;
use crate::storage::{Database, MessageKind, SenderKind, StoredMessage};

/// Result of a committed conversation change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CommitOutcome {
    /// Audit timestamp written to the session row.
    pub updated_at: String,
    /// `chats.revision` after this commit.
    pub revision: i64,
    /// Per-chat `seq` assigned to the persisted message.
    pub seq: i64,
}

/// Aggregates all `messages` / `sessions` writes and the integer
/// `seq` / `revision` bookkeeping that keeps them coherent.
///
/// Borrows the owning [`Database`] for its lifetime; construct one via
/// [`Database::conversation_store`].
pub(crate) struct ConversationStore<'a> {
    db: &'a Database,
}

impl<'a> ConversationStore<'a> {
    pub(crate) fn new(db: &'a Database) -> Self {
        Self { db }
    }

    // --- single-transaction message + session commit -----------------------

    /// Appends one message and, when `session_json` is provided, advances
    /// the LLM session snapshot in the same transaction.
    ///
    /// Issues the next per-chat integer `seq`, writes the message row with
    /// that `seq` (plus `turn_id` / `parent_message_id` when supplied),
    /// upserts the session snapshot with `snapshot_through_seq = seq`, and
    /// bumps `chats.revision` and `chats.next_message_seq` by one.
    ///
    /// `expected_revision` is the optimistic-concurrency token:
    /// * `Some(n)` — the commit only applies while `chats.revision == n`;
    ///   any other value rolls the transaction back with
    ///   [`StorageError::SessionSnapshotConflict`].
    /// * `None` — the session row must not yet exist (initial seed); if a
    ///   session already exists the call conflicts.
    ///   Ensures a `chats` row exists for `chat_id` before any CAS (`revision`)
    ///   or per-chat `seq` (`next_message_seq`) bookkeeping reads from it.
    ///
    /// A bare `chat_id` — a test seed, or a session saved before the first
    /// message — must not fail the later `SELECT revision FROM chats` with
    /// `QueryReturnedNoRows`. The row is created with `revision = 0` and
    /// `next_message_seq = 0` so subsequent bumps start from a clean slate.
    /// In production `resolve_or_create_chat_id` already created the row, so
    /// this degrades to a no-op `INSERT OR IGNORE`.
    fn ensure_chat_row(tx: &rusqlite::Transaction<'_>, chat_id: i64) -> Result<(), StorageError> {
        tx.execute(
            "INSERT OR IGNORE INTO chats (chat_id, last_message_time)
             VALUES (?1, ?2)",
            params![chat_id, chrono::Utc::now().to_rfc3339()],
        )?;
        Ok(())
    }

    fn commit_message_locked(
        tx: &rusqlite::Transaction<'_>,
        message: &StoredMessage,
        session_json: Option<&str>,
        expected_revision: Option<i64>,
    ) -> Result<CommitOutcome, StorageError> {
        Self::ensure_chat_row(tx, message.chat_id)?;
        let (current_revision, next_seq): (i64, i64) = tx.query_row(
            "SELECT revision, next_message_seq FROM chats WHERE chat_id = ?1",
            params![message.chat_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;

        if let Some(expected) = expected_revision {
            if expected != current_revision {
                return Err(StorageError::SessionSnapshotConflict);
            }
        } else if session_json.is_some() {
            // Initial seed: refuse to clobber an existing session snapshot.
            let exists: bool = tx
                .query_row(
                    "SELECT 1 FROM sessions WHERE chat_id = ?1 LIMIT 1",
                    params![message.chat_id],
                    |_| Ok(true),
                )
                .optional()?
                .unwrap_or(false);
            if exists {
                return Err(StorageError::SessionSnapshotConflict);
            }
        }

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

        // Idempotent re-commit: the message row already exists (e.g. a Turn
        // whose deterministic message id is re-persisted on recovery). Do not
        // advance seq or revision — that would create a gap and a spurious
        // CAS conflict for the next writer.
        if inserted == 0 {
            return Ok(CommitOutcome {
                updated_at: chrono::Utc::now().to_rfc3339(),
                revision: current_revision,
                seq,
            });
        }

        let now = chrono::Utc::now().to_rfc3339();

        if let Some(json) = session_json {
            tx.execute(
                "INSERT INTO sessions
                     (chat_id, messages_json, updated_at, snapshot_through_seq)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(chat_id) DO UPDATE SET
                    messages_json = excluded.messages_json,
                    updated_at = excluded.updated_at,
                    snapshot_through_seq = excluded.snapshot_through_seq",
                params![message.chat_id, json, &now, seq],
            )?;
        }

        tx.execute(
            "UPDATE chats
             SET revision = revision + 1,
                 next_message_seq = next_message_seq + 1,
                 last_message_time = ?2
             WHERE chat_id = ?1",
            params![message.chat_id, &now],
        )?;

        Ok(CommitOutcome {
            updated_at: now,
            revision: current_revision + 1,
            seq,
        })
    }

    fn commit_message(
        &self,
        message: &StoredMessage,
        session_json: Option<&str>,
        expected_revision: Option<i64>,
    ) -> Result<CommitOutcome, StorageError> {
        let mut conn = self.db.get_conn()?;
        let tx = conn.transaction()?;
        let outcome = Self::commit_message_locked(&tx, message, session_json, expected_revision)?;
        tx.commit()?;
        Ok(outcome)
    }

    /// Commits a message that already carries its own identity (used by
    /// callers that pre-build the [`StoredMessage`]).
    pub(crate) fn commit_message_struct(
        &self,
        message: &StoredMessage,
        session_json: Option<&str>,
        expected_revision: Option<i64>,
    ) -> Result<CommitOutcome, StorageError> {
        self.commit_message(message, session_json, expected_revision)
    }

    // --- session snapshot update (no new message) ------------------------

    /// Advances the LLM session snapshot without appending a message.
    ///
    /// Used by compaction and by unconditional session seeds. Bumps
    /// `chats.revision` so the change is observable under the same CAS
    /// contract. `snapshot_through_seq` is left at the chat's current
    /// maximum `seq` (no new message was added).
    pub(crate) fn update_session_snapshot(
        &self,
        chat_id: i64,
        session_json: &str,
        expected_revision: Option<i64>,
    ) -> Result<CommitOutcome, StorageError> {
        let mut conn = self.db.get_conn()?;
        let tx = conn.transaction()?;
        Self::ensure_chat_row(&tx, chat_id)?;

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
            updated_at: now,
            revision: current_revision + 1,
            seq: through,
        })
    }

    /// Clears the session snapshot to `[]` for `chat_id`, under a
    /// `revision` CAS. Returns `false` on revision mismatch.
    pub(crate) fn clear_session_snapshot(
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

    /// Replaces the session snapshot with `new_messages_json` for `chat_id`,
    /// under a `revision` CAS. Returns `false` on revision mismatch.
    pub(crate) fn truncate_session_snapshot(
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

    // --- channel log (sessionless) writes ---------------------------------

    /// Appends a message to a chat that has no session snapshot
    /// (multi-agent Channel Log). Issues `seq` and advances `revision`
    /// but never touches `sessions`.
    pub(crate) fn store_channel_log_message(
        &self,
        message: &StoredMessage,
    ) -> Result<i64, StorageError> {
        let outcome = self.commit_message(message, None, None)?;
        Ok(outcome.seq)
    }

    /// Persists a system event to a Channel Log chat.
    pub(crate) fn store_system_event(
        &self,
        channel_log_chat_id: i64,
        reason: &impl fmt::Display,
    ) -> Result<i64, StorageError> {
        let content = serde_json::json!({ "reason": reason.to_string() }).to_string();
        let mut message = StoredMessage::system(channel_log_chat_id, content);
        message.message_kind = MessageKind::SystemEvent;
        self.store_channel_log_message(&message)
    }

    /// Persists a bot response to a Channel Log chat.
    pub(crate) fn store_channel_log_bot_response(
        &self,
        channel_log_chat_id: i64,
        agent_id: &str,
        response: &str,
    ) -> Result<i64, StorageError> {
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
        self.store_channel_log_message(&message)
    }
}
