//! Runtime ingress helpers for channel-originated turns.
//!
//! Channels normalize platform events first; this module translates those
//! normalized inputs into EgoPulse runtime work: `SurfaceContext`, Channel Log
//! persistence, and scheduled turn submission.

use std::sync::Arc;

use crate::agent_loop::{ConversationScope, ScheduledTurn, SurfaceContext};
use crate::runtime::AppState;
use crate::storage::{MessageKind, SenderKind, StoredMessage, call_blocking};

/// Platform-specific key used to resolve a multi-agent Channel Log chat.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChannelLogKey {
    Discord(u64),
    Telegram(i64),
}

/// Human-originated message to persist in a multi-agent Channel Log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HumanChannelLogMessage {
    pub(crate) key: ChannelLogKey,
    pub(crate) scope: ConversationScope,
    pub(crate) id: String,
    pub(crate) sender_id: String,
    pub(crate) content: String,
    pub(crate) timestamp: String,
}

/// Returns the storage scope represented by a channel's `secret` flag.
pub(crate) fn channel_scope_from_secret(secret: bool) -> ConversationScope {
    if secret {
        ConversationScope::Secret
    } else {
        ConversationScope::Normal
    }
}

/// Builds a surface context from channel-normalized input metadata.
pub(crate) fn build_channel_context(
    channel: &str,
    surface_user: &str,
    surface_thread: &str,
    chat_type: &str,
    agent_id: &str,
    scope: ConversationScope,
) -> SurfaceContext {
    let mut context = SurfaceContext::new(
        channel.to_string(),
        surface_user.to_string(),
        surface_thread.to_string(),
        chat_type.to_string(),
        agent_id.to_string(),
    );
    context.scope = scope;
    context
}

/// Resolves a Channel Log chat and stores one human-originated message.
pub(crate) async fn store_human_channel_log_message(
    state: &AppState,
    message: HumanChannelLogMessage,
) -> Option<i64> {
    let db = Arc::clone(state.db_for(message.scope));
    let key = message.key;
    match call_blocking(Arc::clone(&db), move |db| match key {
        ChannelLogKey::Discord(channel_id) => db.resolve_channel_log_chat_id(channel_id),
        ChannelLogKey::Telegram(chat_id) => db.resolve_telegram_channel_log_chat_id(chat_id),
    })
    .await
    {
        Ok(chat_id) => {
            let stored = StoredMessage {
                id: message.id,
                chat_id,
                sender_id: message.sender_id,
                content: message.content,
                sender_kind: SenderKind::User,
                timestamp: message.timestamp,
                message_kind: MessageKind::Message,
                recipient_agent_id: None,
            };
            if let Err(error) = call_blocking(db, move |db| db.store_message_only(&stored)).await {
                tracing::warn!(
                    key = ?message.key,
                    error = %error,
                    "failed to store human message in Channel Log"
                );
            }
            Some(chat_id)
        }
        Err(error) => {
            tracing::warn!(
                key = ?message.key,
                error = %error,
                "failed to resolve Channel Log chat_id"
            );
            None
        }
    }
}

/// Submits an agent turn and starts execution immediately when the session is idle.
pub(crate) fn submit_agent_turn(state: &AppState, context: SurfaceContext, input: String) {
    submit_scheduled_turn(
        state,
        ScheduledTurn {
            origin_id: context.origin_id.clone(),
            context,
            input,
        },
    );
}

pub(super) fn submit_scheduled_turn(state: &AppState, scheduled: ScheduledTurn) {
    if let Some(turn) = state.turn_scheduler.submit(scheduled) {
        let state = state.clone();
        tokio::spawn(async move {
            crate::runtime::execute_scheduled_turn(&state, turn).await;
        });
    }
}
