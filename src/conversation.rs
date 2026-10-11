//! Shared conversation identity and surface context.

use serde::{Deserialize, Serialize};

/// The storage boundary a conversation belongs to.
///
/// Determines which database and archive root are used for persistence.
/// `Normal` routes to the primary `egopulse.db`; `Secret` routes to the
/// isolated `secret.db` and `secret_groups` archive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub(crate) enum ConversationScope {
    /// Default scope — persists to `egopulse.db` and `runtime/groups`.
    Normal,
    /// Secret scope — persists to `secret.db` and `secret_groups` archive.
    Secret,
}

impl std::fmt::Display for ConversationScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Normal => write!(f, "normal"),
            Self::Secret => write!(f, "secret"),
        }
    }
}

/// Identifies the external conversation surface mapped to a persisted session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SurfaceContext {
    pub channel: String,
    pub surface_user: String,
    pub surface_thread: String,
    pub chat_type: String,
    pub agent_id: String,
    /// For multi-agent rooms: the Channel Log chat ID used for Shared Room Context injection.
    /// `None` for single-agent channels and DMs.
    pub channel_log_chat_id: Option<i64>,
    /// Current `agent_send` chain depth (0 for user-initiated turns).
    pub chain_depth: usize,
    /// Origin ID: UUID tracking all turns caused by a single human input.
    /// Empty string when origin tracking is not applicable (e.g. non-Discord channels).
    pub origin_id: String,
    /// Trace ID for observability: UUID correlating all log events within a single turn.
    /// Generated in `execute_scheduled_turn` and propagated through the turn lifecycle.
    pub trace_id: String,
    /// Storage scope for this conversation surface.
    pub scope: ConversationScope,
    /// Stable ingress identity used for idempotent Turn acceptance
    /// (`turn_runs.request_key`). Deduplicates re-delivered platform messages:
    /// the same `chat_id + request_key` maps to the same Turn instead of a
    /// duplicate. Each ingress derives it from a stable platform identifier
    /// (e.g. Discord `channel_id:message_id`); an empty value falls back to a
    /// fresh UUID at acceptance so distinct inputs never collide.
    pub request_key: String,
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
            trace_id: String::new(),
            scope: ConversationScope::Normal,
            request_key: String::new(),
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

    /// Recovers the raw `surface_thread` from a stored session key.
    ///
    /// This is the inverse of [`Self::session_key`]: it strips the
    /// `{channel}:` prefix and the `:agent:{agent_id}` suffix so that a context
    /// rebuilt from a persisted `external_chat_id` maps back to the same
    /// session instead of nesting the key. Keys without the prefix or suffix
    /// are returned with only the parts that are present removed.
    pub(crate) fn surface_thread_from_session_key(
        channel: &str,
        session_key: &str,
        agent_id: &str,
    ) -> String {
        let thread = session_key
            .strip_prefix(channel)
            .and_then(|rest| rest.strip_prefix(':'))
            .unwrap_or(session_key);
        if agent_id.is_empty() {
            return thread.to_string();
        }
        thread
            .strip_suffix(&format!(":agent:{agent_id}"))
            .unwrap_or(thread)
            .to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn surface_thread_from_session_key_round_trips_agent_scoped_keys() {
        // Arrange
        let original = SurfaceContext::new(
            "discord".to_string(),
            "user".to_string(),
            "999".to_string(),
            "discord".to_string(),
            "vega".to_string(),
        );
        let stored_key = original.session_key();

        // Act
        let thread =
            SurfaceContext::surface_thread_from_session_key("discord", &stored_key, "vega");

        // Assert
        assert_eq!(thread, "999");
    }

    #[test]
    fn surface_thread_from_session_key_keeps_keys_without_prefix_or_suffix() {
        // Arrange
        let legacy_key = "1489661143982149763";

        // Act
        let thread = SurfaceContext::surface_thread_from_session_key("discord", legacy_key, "lyre");

        // Assert
        assert_eq!(thread, legacy_key);
    }

    #[test]
    fn surface_thread_from_session_key_handles_keys_without_agent() {
        // Arrange
        let stored_key = "cli:mysession";

        // Act
        let thread = SurfaceContext::surface_thread_from_session_key("cli", stored_key, "");

        // Assert
        assert_eq!(thread, "mysession");
    }
}
