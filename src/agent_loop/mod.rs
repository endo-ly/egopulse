//! 会話ターン処理とセッション解決を束ねるモジュール。
//!
//! 各チャネルから渡された surface 情報をもとに永続セッションを特定し、
//! エージェントの 1 ターン処理へ橋渡しする。

pub(crate) mod compaction;
mod formatting;
pub(crate) mod guards;
pub(crate) mod prompt_builder;
pub(crate) mod session;
pub(crate) mod turn;

pub(crate) use session::{list_sessions, load_session_messages};
pub use turn::ask_in_session;
pub(crate) use turn::{process_turn, process_turn_with_events, send_turn};

#[derive(Debug, Clone, PartialEq, Eq)]
/// Identifies the external conversation surface mapped to a persisted session.
pub(crate) struct SurfaceContext {
    pub channel: String,
    pub surface_user: String,
    pub surface_thread: String,
    pub chat_type: String,
    pub agent_id: String,
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
        }
    }

    /// Returns the stable session key in `channel:surface_thread` format.
    pub(crate) fn session_key(&self) -> String {
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
        // Arrange
        let ctx = SurfaceContext::new(
            "discord".to_string(),
            "bob".to_string(),
            "123:bot:main:agent:dev".to_string(),
            "discord".to_string(),
            "dev".to_string(),
        );

        // Act
        let key = ctx.session_key();

        // Assert
        assert_eq!(key, "discord:123:bot:main:agent:dev");
    }
}
