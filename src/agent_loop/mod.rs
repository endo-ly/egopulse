//! 会話ターン処理とセッション解決を束ねるモジュール。
//!
//! 各チャネルから渡された surface 情報をもとに永続セッションを特定し、
//! エージェントの 1 ターン処理へ橋渡しする。

pub(crate) mod compaction;
pub(crate) mod formatting;
pub(crate) mod guards;
pub(crate) mod prompt_builder;
pub(crate) mod session;
pub(crate) mod soul_agents;
pub(crate) mod turn;

pub(crate) use session::{list_sessions, load_session_messages};
pub use turn::ask_in_session;
pub(crate) use turn::{process_turn, process_turn_with_events, send_turn};

/// A pending turn to be executed for a target agent, enqueued by `agent_send`.
#[derive(Debug, Clone)]
pub(crate) struct PendingAgentTurn {
    /// The surface context for the target agent (same channel, target agent_id).
    pub context: SurfaceContext,
    /// The input text in `[From → To] message` format.
    pub input: String,
    /// Origin ID: UUID tracking all turns caused by a single human input.
    /// Propagated from the originating human message through agent_send chains.
    pub origin_id: String,
}

/// A turn submitted to the [`crate::runtime::turn_scheduler::TurnScheduler`] for ordered execution.
///
/// Extends [`PendingAgentTurn`] with origin tracking for runaway prevention.
#[derive(Debug, Clone)]
pub(crate) struct ScheduledTurn {
    /// Surface context identifying the agent session.
    pub context: SurfaceContext,
    /// The input text for this turn.
    pub input: String,
    /// Origin ID: UUID tracking all turns caused by a single human input.
    pub origin_id: String,
}

impl ScheduledTurn {
    /// Returns the stable session key for this turn's target session.
    pub(crate) fn session_key(&self) -> String {
        self.context.session_key()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Identifies the external conversation surface mapped to a persisted session.
pub(crate) struct SurfaceContext {
    pub channel: String,
    pub surface_user: String,
    pub surface_thread: String,
    pub chat_type: String,
    pub agent_id: String,
    /// For multi-agent rooms: the Channel Log chat ID used for Channel Context injection.
    /// `None` for single-agent channels and DMs.
    pub channel_log_chat_id: Option<i64>,
    /// Current `agent_send` chain depth (0 for user-initiated turns).
    pub chain_depth: usize,
    /// Origin ID: UUID tracking all turns caused by a single human input.
    /// Empty string when origin tracking is not applicable (e.g. non-Discord channels).
    pub origin_id: String,
}

impl SurfaceContext {
    /// Creates a new `SurfaceContext` identifying the external conversation surface.
    pub(crate) fn new(
        channel: String,
        surface_user: String,
        surface_thread: String,
        chat_type: String,
        agent_id: String,
    ) -> Self {
        Self {
            channel,
            surface_user,
            surface_thread,
            chat_type,
            agent_id,
            channel_log_chat_id: None,
            chain_depth: 0,
            origin_id: String::new(),
        }
    }

    /// Returns the stable session key for the current surface and agent.
    pub(crate) fn session_key(&self) -> String {
        if !self.agent_id.is_empty() {
            return format!(
                "{}:{}:agent:{}",
                self.channel, self.surface_thread, self.agent_id
            );
        }
        format!("{}:{}", self.channel, self.surface_thread)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_assigns_all_fields() {
        // Arrange
        let channel = "cli".to_string();
        let surface_user = "alice".to_string();
        let surface_thread = "session-1".to_string();
        let chat_type = "cli".to_string();
        let agent_id = "developer".to_string();

        // Act
        let ctx = SurfaceContext::new(
            channel.clone(),
            surface_user.clone(),
            surface_thread.clone(),
            chat_type.clone(),
            agent_id.clone(),
        );

        // Assert
        assert_eq!(ctx.channel, channel);
        assert_eq!(ctx.surface_user, surface_user);
        assert_eq!(ctx.surface_thread, surface_thread);
        assert_eq!(ctx.chat_type, chat_type);
        assert_eq!(ctx.agent_id, agent_id);
    }

    #[test]
    fn session_key_format_is_channel_colon_thread() {
        let ctx = SurfaceContext::new(
            "discord".to_string(),
            "bob".to_string(),
            "123".to_string(),
            "discord".to_string(),
            "dev".to_string(),
        );

        assert_eq!(ctx.surface_thread, "123");
        assert_eq!(ctx.session_key(), "discord:123:agent:dev");
    }

    #[test]
    fn session_key_includes_agent_for_all_channels() {
        let ctx = SurfaceContext::new(
            "web".to_string(),
            "dev".to_string(),
            "session-1".to_string(),
            "web".to_string(),
            "vega".to_string(),
        );

        assert_eq!(ctx.session_key(), "web:session-1:agent:vega");
    }

    #[test]
    fn session_key_without_agent_uses_simple_format() {
        let ctx = SurfaceContext::new(
            "cli".to_string(),
            "user".to_string(),
            "mysession".to_string(),
            "cli".to_string(),
            "".to_string(),
        );

        assert_eq!(ctx.session_key(), "cli:mysession");
    }
}
