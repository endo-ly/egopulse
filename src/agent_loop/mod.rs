//! 会話ターン処理とセッション解決を束ねるモジュール。
//!
//! 各チャネルから渡された surface 情報をもとに永続セッションを特定し、
//! エージェントの 1 ターン処理へ橋渡しする。

pub(crate) mod compaction;
pub(crate) mod event;
pub(crate) mod formatting;
pub(crate) mod guards;
pub(crate) mod prompt_builder;
pub(crate) mod session;
pub(crate) mod session_snapshot;
pub(crate) mod soul_agents;
pub(crate) mod tool_phase;
pub(crate) mod turn;
pub(crate) mod turn_runtime;

pub(crate) use session::{list_sessions, load_session_messages, resolve_chat_id};
pub use turn::ask_in_session;
pub(crate) use turn::{process_turn, process_turn_with_events, send_turn};
pub(crate) use turn_runtime::TurnRuntime;

use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

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

/// Computes the canonical request hash from the full surface context and input
///. The hash is independent of JSON field order or whitespace, so
/// the same logical request always produces the same digest. `origin_id`,
/// `request_key`, and `trace_id` are excluded: they are identity/routing values,
/// not part of the request content.
pub(crate) fn canonical_request_hash(context: &SurfaceContext, input: &str) -> String {
    let mut map: BTreeMap<&str, serde_json::Value> = BTreeMap::new();
    map.insert("version", serde_json::json!(1u32));
    map.insert("channel", serde_json::json!(context.channel));
    map.insert("surface_user", serde_json::json!(context.surface_user));
    map.insert("surface_thread", serde_json::json!(context.surface_thread));
    map.insert("chat_type", serde_json::json!(context.chat_type));
    map.insert("agent_id", serde_json::json!(context.agent_id));
    map.insert(
        "channel_log_chat_id",
        serde_json::json!(context.channel_log_chat_id),
    );
    map.insert("chain_depth", serde_json::json!(context.chain_depth));
    map.insert("input", serde_json::json!(input));
    // BTreeMap serializes with sorted keys, giving an order-independent digest.
    let bytes = serde_json::to_vec(&map).expect("canonical request serialization");
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    format!("{:x}", hasher.finalize())
}

/// The storage boundary a conversation belongs to.
///
/// Determines which database and archive root are used for persistence.
/// `Normal` routes to the primary `egopulse.db`; `Secret` routes to the
/// isolated `secret.db` and `secret_groups` archive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConversationScope {
    /// Default scope — persists to `egopulse.db` and `runtime/groups`.
    Normal,
    /// Secret scope — persists to `secret.db` and `runtime/secret_groups`.
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
        assert_eq!(ctx.trace_id, "");
        assert_eq!(ctx.origin_id, "");
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

    #[test]
    fn surface_context_defaults_to_normal_scope() {
        let ctx = SurfaceContext::new(
            "discord".to_string(),
            "user".to_string(),
            "thread".to_string(),
            "discord".to_string(),
            "default".to_string(),
        );
        assert_eq!(ctx.scope, ConversationScope::Normal);
    }
}
