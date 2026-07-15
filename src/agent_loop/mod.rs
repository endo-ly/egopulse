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
pub(crate) use turn::{
    process_turn, process_turn_with_events, resume_input_committed_turn, send_turn,
};
pub(crate) use turn_runtime::TurnRuntime;

use crate::error::EgoPulseError;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

/// A turn submitted to the [`crate::runtime::turn_scheduler::TurnScheduler`] for ordered execution.
///
/// Extends [`PendingAgentTurn`] with origin tracking for runaway prevention.
#[derive(Debug, Clone)]
pub(crate) struct ScheduledTurn {
    /// Stable Turn ID, also the primary key in `turn_runs`.
    pub turn_id: String,
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

/// Durable serialization of a [`ScheduledTurn`] for crash-safe persistence.
///
/// Stored as `turn_runs.scheduled_request_json`. On restart the turn dispatcher
/// rebuilds the `ScheduledTurn` from this payload so an `accepted` turn can
/// resume even if the process crashed before execution began. The `version`
/// field lets a future schema change distinguish and migrate older payloads
/// instead of silently misreading them.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct PersistedScheduledTurnV1 {
    /// Envelope version, always [`SCHEDULED_TURN_VERSION`].
    pub version: u32,
    /// Surface context identifying the agent session.
    pub context: SurfaceContext,
    /// The input text for this turn.
    pub input: String,
}

/// Current durable scheduled-turn payload version.
pub(crate) const SCHEDULED_TURN_VERSION: u32 = 1;

/// Serializes a [`ScheduledTurn`] for durable persistence.
///
/// # Errors
///
/// Returns [`EgoPulseError::Internal`] when JSON serialization fails.
pub(crate) fn serialize_scheduled_turn(turn: &ScheduledTurn) -> Result<String, EgoPulseError> {
    let payload = PersistedScheduledTurnV1 {
        version: SCHEDULED_TURN_VERSION,
        context: turn.context.clone(),
        input: turn.input.clone(),
    };
    serde_json::to_string(&payload)
        .map_err(|e| EgoPulseError::Internal(format!("serialize scheduled turn: {e}")))
}

/// Rebuilds a [`ScheduledTurn`] from its durable persisted payload.
///
/// # Errors
///
/// Returns [`EgoPulseError::Internal`] when JSON deserialization fails, the
/// payload is malformed, or its version is not [`SCHEDULED_TURN_VERSION`].
pub(crate) fn deserialize_scheduled_turn(json: &str) -> Result<ScheduledTurn, EgoPulseError> {
    let value: serde_json::Value = serde_json::from_str(json)
        .map_err(|e| EgoPulseError::Internal(format!("deserialize scheduled turn: {e}")))?;
    let version = value
        .get("version")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| {
            EgoPulseError::Internal("scheduled turn payload missing version field".to_string())
        })? as u32;
    if version != SCHEDULED_TURN_VERSION {
        return Err(EgoPulseError::Internal(format!(
            "unsupported scheduled turn version {version} (supported {SCHEDULED_TURN_VERSION})"
        )));
    }
    let payload: PersistedScheduledTurnV1 = serde_json::from_value(value)
        .map_err(|e| EgoPulseError::Internal(format!("deserialize scheduled turn: {e}")))?;
    Ok(ScheduledTurn {
        turn_id: String::new(),
        context: payload.context.clone(),
        input: payload.input,
        origin_id: payload.context.origin_id.clone(),
    })
}

/// The storage boundary a conversation belongs to.
///
/// Determines which database and archive root are used for persistence.
/// `Normal` routes to the primary `egopulse.db`; `Secret` routes to the
/// isolated `secret.db` and `secret_groups` archive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
    fn scheduled_turn_serializes_and_deserializes() {
        // Arrange
        let mut context = SurfaceContext::new(
            "discord".to_string(),
            "alice".to_string(),
            "123".to_string(),
            "discord".to_string(),
            "dev".to_string(),
        );
        context.origin_id = "origin-1".to_string();
        let turn = ScheduledTurn {
            turn_id: "turn-1".to_string(),
            context,
            input: "hello world".to_string(),
            origin_id: "origin-1".to_string(),
        };

        // Act
        let json = serialize_scheduled_turn(&turn).expect("serialize");
        let back = deserialize_scheduled_turn(&json).expect("deserialize");

        // Assert: round-trip preserves input, origin, and surface context.
        assert_eq!(back.input, "hello world");
        assert_eq!(back.origin_id, "origin-1");
        assert_eq!(back.context.channel, "discord");
        assert_eq!(back.context.agent_id, "dev");
    }

    #[test]
    fn deserialize_scheduled_turn_rejects_unknown_version() {
        // A future-version payload must be rejected, not silently misread.
        let future = serde_json::json!({
            "version": 999,
            "context": {
                "channel": "discord",
                "surface_user": "u",
                "surface_thread": "t",
                "chat_type": "discord",
                "agent_id": "a",
                "channel_log_chat_id": null,
                "chain_depth": 0,
                "origin_id": "",
                "trace_id": "",
                "scope": "normal",
                "request_key": ""
            },
            "input": "x"
        })
        .to_string();
        let err = deserialize_scheduled_turn(&future).expect_err("should reject");
        assert!(err.to_string().contains("version"));
    }

    #[test]
    fn deserialize_scheduled_turn_rejects_missing_version() {
        // A payload without a version field is malformed.
        let no_version = serde_json::json!({
            "context": {
                "channel": "discord",
                "surface_user": "u",
                "surface_thread": "t",
                "chat_type": "discord",
                "agent_id": "a",
                "channel_log_chat_id": null,
                "chain_depth": 0,
                "origin_id": "",
                "trace_id": "",
                "scope": "normal",
                "request_key": ""
            },
            "input": "x"
        })
        .to_string();
        let err = deserialize_scheduled_turn(&no_version).expect_err("should reject");
        assert!(err.to_string().contains("version"));
    }

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
