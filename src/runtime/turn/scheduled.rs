//! Durable representation of a turn submitted to the runtime scheduler.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::conversation::SurfaceContext;
use crate::error::EgoPulseError;

/// A turn submitted to the runtime scheduler for ordered execution.
///
/// Extends the durable request with origin tracking for runaway prevention.
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
    /// Timestamp captured at durable input acceptance. Recovery preserves it
    /// so the LLM direct-input metadata does not change after promotion.
    pub received_at: Option<String>,
    /// Immutable configuration selected at turn acceptance.
    ///
    /// This is populated for live turns before durable acceptance and carried
    /// through the in-memory scheduler so a queued turn does not switch to a
    /// newer configuration generation before execution.
    pub config_snapshot: Option<Arc<crate::config::manager::ConfigSnapshot>>,
    /// Runtime response destination. Client-owned output is delivered to a
    /// live transport observer when one exists and is never sent to the
    /// session's channel adapter.
    pub response_delivery: ResponseDelivery,
}

impl ScheduledTurn {
    /// Returns the stable session key for this turn's target session.
    pub(crate) fn session_key(&self) -> String {
        self.context.session_key()
    }
}

/// Selects where a scheduled turn's output is delivered.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) enum ResponseDelivery {
    /// Send the response through the session's channel adapter.
    #[default]
    Channel,
    /// Keep output owned by the submitting client instead of the session's
    /// channel. The live observer is process-local and is not durable.
    ClientOwned,
}

/// Durable encoding for a scheduled turn's response policy.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
enum PersistedResponseDelivery {
    #[serde(rename = "Channel")]
    #[default]
    Channel,
    #[serde(rename = "ClientOwned")]
    ClientOwned,
}

impl From<&ResponseDelivery> for PersistedResponseDelivery {
    fn from(delivery: &ResponseDelivery) -> Self {
        match delivery {
            ResponseDelivery::Channel => Self::Channel,
            ResponseDelivery::ClientOwned => Self::ClientOwned,
        }
    }
}

impl From<PersistedResponseDelivery> for ResponseDelivery {
    fn from(delivery: PersistedResponseDelivery) -> Self {
        match delivery {
            PersistedResponseDelivery::Channel => Self::Channel,
            PersistedResponseDelivery::ClientOwned => Self::ClientOwned,
        }
    }
}

/// Canonical (order-independent) serialization of a turn request. Fields are
/// declared in sorted-key order so the resulting digest remains stable while
/// avoiding per-call map allocations.
#[derive(Serialize)]
struct CanonicalRequest<'a> {
    agent_id: &'a str,
    chain_depth: usize,
    channel: &'a str,
    channel_log_chat_id: Option<i64>,
    chat_type: &'a str,
    input: &'a str,
    surface_thread: &'a str,
    surface_user: &'a str,
    version: u32,
}

/// Computes the canonical request hash from the full surface context and input.
/// The hash is independent of JSON field order or whitespace, so the same
/// logical request always produces the same digest. `origin_id`, `request_key`,
/// and `trace_id` are excluded: they are identity/routing values, not part of
/// the request content.
pub(crate) fn canonical_request_hash(context: &SurfaceContext, input: &str) -> String {
    let canonical = CanonicalRequest {
        agent_id: &context.agent_id,
        chain_depth: context.chain_depth,
        channel: &context.channel,
        channel_log_chat_id: context.channel_log_chat_id,
        chat_type: &context.chat_type,
        input,
        surface_thread: &context.surface_thread,
        surface_user: &context.surface_user,
        version: 1u32,
    };
    // Fields are declared in sorted-key order, giving an order-independent
    // digest without a per-call BTreeMap allocation.
    let bytes = serde_json::to_vec(&canonical).expect("canonical request serialization");
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    format!("{:x}", hasher.finalize())
}

/// Durable serialization of a [`ScheduledTurn`] for crash-safe persistence.
///
/// Stored as `turn_runs.scheduled_request_json`. On restart the turn dispatcher
/// rebuilds the [`ScheduledTurn`] from this payload so an `accepted` turn can
/// resume even if the process crashed before execution began. The `version`
/// field lets a future schema change distinguish and migrate older payloads
/// instead of silently misreading them.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedScheduledTurnV1 {
    /// Envelope version, always [`SCHEDULED_TURN_VERSION`].
    pub version: u32,
    /// Surface context identifying the agent session.
    pub context: SurfaceContext,
    /// The input text for this turn.
    pub input: String,
    /// Original durable acceptance timestamp, absent in older payloads.
    #[serde(default)]
    pub received_at: Option<String>,
    /// Response routing contract. Old payloads default to the channel route.
    #[serde(default)]
    response_delivery: PersistedResponseDelivery,
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
        received_at: turn.received_at.clone(),
        response_delivery: (&turn.response_delivery).into(),
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
        received_at: payload.received_at,
        config_snapshot: None,
        response_delivery: payload.response_delivery.into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_request_hash_matches_sorted_key_reference() {
        // Verify the canonical serialization against representative request
        // shapes so durable request hashes remain stable.
        use std::collections::BTreeMap;

        fn reference(context: &SurfaceContext, input: &str) -> String {
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
            let bytes = serde_json::to_vec(&map).expect("reference serialization");
            use sha2::{Digest, Sha256};
            let mut hasher = Sha256::new();
            hasher.update(&bytes);
            format!("{:x}", hasher.finalize())
        }

        let cases = [
            (None, "", "", "", "", "", ""),
            (
                Some(7_i64),
                "discord",
                "123",
                "discord",
                "dev",
                "alice",
                "hello",
            ),
            (
                None,
                "telegram",
                "t-1",
                "telegram",
                "lyre",
                "bob",
                "あいうえお",
            ),
            (
                Some(-3_i64),
                "web",
                "w-9",
                "web",
                "vega",
                "carol",
                "{\"k\":1}",
            ),
        ];
        for (log_id, channel, thread, chat_type, agent, user, input) in cases {
            let mut context = SurfaceContext::new(
                channel.to_string(),
                user.to_string(),
                thread.to_string(),
                chat_type.to_string(),
                agent.to_string(),
            );
            context.channel_log_chat_id = log_id;
            context.chain_depth = 2;
            assert_eq!(
                canonical_request_hash(&context, input),
                reference(&context, input),
                "digest diverged for case {channel}/{agent}/{input:?}"
            );
        }
    }

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
            config_snapshot: None,
            received_at: Some("2026-01-02T03:04:05Z".to_string()),
            response_delivery: ResponseDelivery::ClientOwned,
        };

        // Act
        let json = serialize_scheduled_turn(&turn).expect("serialize");
        let back = deserialize_scheduled_turn(&json).expect("deserialize");

        // Assert: round-trip preserves input, origin, and surface context.
        assert_eq!(back.input, "hello world");
        assert_eq!(back.origin_id, "origin-1");
        assert_eq!(back.context.channel, "discord");
        assert_eq!(back.context.agent_id, "dev");
        assert_eq!(back.response_delivery, ResponseDelivery::ClientOwned);
        assert!(!json.contains("observer_id"));
        assert!(!json.contains("Observer"));
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
}
