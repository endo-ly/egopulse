//! Runtime boundary for channel-originated input.
//!
//! Channels normalize platform events first; this module translates those
//! normalized inputs into EgoPulse runtime work: `SurfaceContext`, Channel Log
//! persistence, and scheduled turn submission.

use std::sync::Arc;

use crate::agent_loop::{ConversationScope, ScheduledTurn, SurfaceContext};
use crate::runtime::AppState;
use crate::runtime::turn_scheduler::{ScheduleResult, SubmitOutcome};
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
///
/// Returns [`SubmitOutcome`] so callers can distinguish an accepted turn
/// (started or queued) from a queue-capacity rejection. Rejections are logged
/// centrally here so no turn is silently dropped.
pub(crate) fn submit_agent_turn(
    state: &AppState,
    context: SurfaceContext,
    input: String,
) -> SubmitOutcome {
    submit_scheduled_turn(
        state,
        ScheduledTurn {
            origin_id: context.origin_id.clone(),
            context,
            input,
        },
    )
}

pub(super) fn submit_scheduled_turn(state: &AppState, scheduled: ScheduledTurn) -> SubmitOutcome {
    match state.turn_scheduler.submit(scheduled) {
        ScheduleResult::Started(turn) => {
            let turn = *turn;
            let state = state.clone();
            tokio::spawn(async move {
                crate::runtime::execute_scheduled_turn(&state, turn).await;
            });
            SubmitOutcome::Started
        }
        ScheduleResult::Queued => SubmitOutcome::Queued,
        ScheduleResult::Rejected(reason) => {
            tracing::warn!(
                reason = %reason,
                "turn rejected: scheduler queue at capacity"
            );
            SubmitOutcome::Rejected(reason)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::runtime::build_sleep_app_state_with_path;

    fn build_test_state(dir: &tempfile::TempDir) -> AppState {
        let config = crate::test_util::test_config(dir.path().to_str().expect("utf8"));
        build_sleep_app_state_with_path(config, Some(dir.path().join("egopulse.config.yaml")))
            .expect("build sleep state")
    }

    #[test]
    fn channel_scope_from_secret_maps_channel_flag() {
        assert_eq!(channel_scope_from_secret(true), ConversationScope::Secret);
        assert_eq!(channel_scope_from_secret(false), ConversationScope::Normal);
    }

    #[test]
    fn build_channel_context_applies_scope_and_session_identity() {
        let context = build_channel_context(
            "discord",
            "alice",
            "123",
            "discord",
            "reviewer",
            ConversationScope::Secret,
        );

        assert_eq!(context.channel, "discord");
        assert_eq!(context.surface_user, "alice");
        assert_eq!(context.surface_thread, "123");
        assert_eq!(context.chat_type, "discord");
        assert_eq!(context.agent_id, "reviewer");
        assert_eq!(context.scope, ConversationScope::Secret);
        assert_eq!(context.session_key(), "discord:123:agent:reviewer");
    }

    #[tokio::test]
    async fn store_human_channel_log_message_persists_discord_message() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = build_test_state(&dir);

        let chat_id = store_human_channel_log_message(
            &state,
            HumanChannelLogMessage {
                key: ChannelLogKey::Discord(42),
                scope: ConversationScope::Normal,
                id: "cl-42".to_string(),
                sender_id: "user:discord:7".to_string(),
                content: "hello".to_string(),
                timestamp: "2026-06-25T00:00:00Z".to_string(),
            },
        )
        .await
        .expect("channel log chat id");

        let messages = state
            .db_for(ConversationScope::Normal)
            .get_channel_log_messages(chat_id, 10)
            .expect("messages");
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].id, "cl-42");
        assert_eq!(messages[0].sender_id, "user:discord:7");
        assert_eq!(messages[0].sender_kind, SenderKind::User);
        assert_eq!(messages[0].content, "hello");
    }
}
