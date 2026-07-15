//! `agent_send` tool — inter-agent communication within the current channel.
//!
//! Allows one agent to send a message to another agent in the same channel.
//! The message is displayed as `[From → To] message` and the target agent's
//! next turn is durably accepted for background execution.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;

use crate::agent_loop::{ConversationScope, ScheduledTurn, SurfaceContext};
use crate::config::{AgentConfig, AgentId};
use crate::llm::ToolDefinition;
use crate::runtime::turn_scheduler::{StopReason, evaluate_stop_conditions};
use crate::runtime::{AppState, channel_input, turn_scheduler::SubmitOutcome};
use crate::storage::{MessageKind, StoredMessage, call_blocking};
use crate::tools::send_message::lookup_chat_info;
use crate::tools::{Tool, ToolExecutionContext, ToolResult, parse_params, schema_object};

use super::sanitize_tool_result;

const AGENT_SEND_SYSTEM_INSTRUCTION: &str = "\
[System: This message was delivered from another agent via agent_send. \
To reply, use the agent_send tool to respond to the sender.]";

#[derive(serde::Deserialize)]
struct AgentSendParams {
    to: String,
    message: String,
}

pub(crate) struct AgentSendTool {
    agents: std::collections::HashMap<AgentId, AgentConfig>,
    db: Arc<crate::storage::Database>,
    secret_db: Option<Arc<crate::storage::Database>>,
    channels: Arc<crate::channels::adapter::ChannelRegistry>,
    /// Shared durable-turn intake. When present (production runtime), the target
    /// agent turn is durably accepted (`turn_runs` committed) **before**
    /// `delivered: true` is returned, closing the crash window where an
    /// in-memory hand-off could lose the turn. Tests run without an `AppState`
    /// and report `delivered: false` (the turn is not durably accepted).
    ///
    /// Stored in a `OnceLock` so the runtime can backfill it *after* the tool
    /// has been registered into the registry the `AppState` owns, without
    /// requiring unique ownership of that `AppState`. The `Arc` shares every
    /// runtime service (`turn_tracker`, `turn_scheduler`, `supervisor`, `db`,
    /// `config_manager`) through its `Arc` handles, so reservations and durable
    /// commits made here are visible to the live runtime.
    app_state: std::sync::OnceLock<std::sync::Arc<AppState>>,
}

/// Stable name of the `agent_send` tool, used by the registry to locate it
/// when backfilling the runtime `AppState` after registration.
pub(crate) const AGENT_SEND_NAME: &str = "agent_send";

impl AgentSendTool {
    pub(crate) fn new(
        agents: std::collections::HashMap<AgentId, AgentConfig>,
        db: Arc<crate::storage::Database>,
        secret_db: Option<Arc<crate::storage::Database>>,
        channels: Arc<crate::channels::adapter::ChannelRegistry>,
    ) -> Self {
        Self {
            agents,
            db,
            secret_db,
            channels,
            app_state: std::sync::OnceLock::new(),
        }
    }

    fn db_for(&self, scope: ConversationScope) -> &Arc<crate::storage::Database> {
        match scope {
            ConversationScope::Normal => &self.db,
            ConversationScope::Secret => self
                .secret_db
                .as_ref()
                .expect("secret db required for secret mode agent_send"),
        }
    }
}

fn agent_label<'a>(
    agents: &'a std::collections::HashMap<AgentId, AgentConfig>,
    id: &'a str,
) -> &'a str {
    agents
        .get(&AgentId::new(id))
        .and_then(|c| {
            let label = c.label.trim();
            if label.is_empty() { None } else { Some(label) }
        })
        .unwrap_or(id)
}

/// First 16 hex chars of SHA-256(text) — compact dedup-friendly fingerprint.
fn short_hash(text: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    hasher
        .finalize()
        .iter()
        .take(8)
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[async_trait]
impl Tool for AgentSendTool {
    fn name(&self) -> &str {
        "agent_send"
    }

    fn init_app_state(&self, state: std::sync::Arc<crate::runtime::AppState>) {
        let _ = self.app_state.set(state);
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "agent_send".to_string(),
            description: "Send a message to another agent in the current channel. \
                The target agent will process your message and respond in the channel. \
                Use this to collaborate with other agents by delegating tasks or asking questions."
                .to_string(),
            parameters: schema_object(
                json!({
                    "to": {
                        "type": "string",
                        "description": "Agent ID to send the message to (must be a configured agent)"
                    },
                    "message": {
                        "type": "string",
                        "description": "The message content to send to the target agent"
                    }
                }),
                &["to", "message"],
            ),
        }
    }

    async fn execute(
        &self,
        input: serde_json::Value,
        context: &ToolExecutionContext,
    ) -> ToolResult {
        let params: AgentSendParams = match parse_params(input) {
            Ok(p) => p,
            Err(e) => return e,
        };

        let target_id = params.to.trim().to_string();
        if target_id.is_empty() {
            return ToolResult::error("parameter 'to' must not be empty".to_string());
        }

        // Validate: agent must exist in config.agents
        if !self.agents.contains_key(&AgentId::new(&target_id)) {
            return ToolResult::error(format!("agent '{target_id}' not found"));
        }

        // Validate: self-send is prohibited
        if target_id == context.agent_id {
            return ToolResult::error("cannot send a message to yourself".to_string());
        }

        // Preflight: reject if target turn would exceed stop conditions.
        let target_chain_depth = context.chain_depth + 1;
        if let Some(StopReason::ChainDepthExceeded) =
            evaluate_stop_conditions(target_chain_depth, 0, &target_id, &[target_id.as_str()])
        {
            return sanitize_tool_result(
                ToolResult::success(
                    serde_json::to_string(&json!({
                        "delivered": false,
                        "to": target_id,
                        "reason": "ChainDepthExceeded"
                    }))
                    .expect("json"),
                ),
                &[],
            );
        }

        let from_label = agent_label(&self.agents, &context.agent_id).to_string();
        let to_label = agent_label(&self.agents, &target_id).to_string();
        let display_text = format!("[{from_label} → {to_label}] {}", params.message);

        // 1. Save to Channel Log
        let message_id = uuid::Uuid::new_v4().to_string();
        let chat_id = context.chat_id;
        let mut stored = StoredMessage::tool(
            context.channel_log_chat_id.unwrap_or(chat_id),
            context.agent_id.clone(),
            target_id.clone(),
            display_text.clone(),
        );
        stored.id = message_id;
        stored.message_kind = MessageKind::AgentSend;

        if let Err(error) = call_blocking(Arc::clone(self.db_for(context.scope)), move |db| {
            db.store_message_only(&stored)
        })
        .await
        {
            tracing::warn!(error = %error, "agent_send: failed to save channel log");
        }

        // 2. Display in channel
        let chat_info = lookup_chat_info(Arc::clone(self.db_for(context.scope)), chat_id).await;
        if let Ok(Some(info)) = chat_info {
            if let Some(adapter) = self.channels.get(&info.channel) {
                if let Err(error) = adapter
                    .send_text(&info.external_chat_id, &display_text)
                    .await
                {
                    tracing::warn!(error = %error, "agent_send: failed to display in channel");
                }
            }
        }

        // 3. Durable acceptance through the shared intake.
        let target_context = SurfaceContext {
            channel: context.channel.clone(),
            surface_user: "agent_send".to_string(),
            surface_thread: context.surface_thread.clone(),
            chat_type: context.chat_type.clone(),
            agent_id: target_id.clone(),
            channel_log_chat_id: context.channel_log_chat_id,
            chain_depth: target_chain_depth,
            origin_id: context.origin_id.clone(),
            trace_id: String::new(),
            scope: context.scope,

            // Stable across crash retries: the same parent Turn sending the
            // same message to the same target via the same Tool call maps to
            // one target Turn. The Tool Call ID disambiguates two separate
            // `agent_send` calls within a single parent Turn that target the
            // same agent with the same message.
            request_key: format!(
                "agent_send:{}:{}:{}:{}",
                context.turn_id,
                context.tool_call_id,
                target_id,
                short_hash(&params.message),
            ),
        };

        let target_input = format!("{AGENT_SEND_SYSTEM_INSTRUCTION}\n\n{display_text}");

        // Durable acceptance through the shared intake. The target turn is
        // committed to `turn_runs` (`accepted`) *before* `delivered: true` is
        // reported, so a crash between this point and any downstream processing
        // can never lose the turn: the TurnDispatcher resumes it from the
        // database on the next startup. This is the same reservation + durable
        // commit + spawn used by every channel, eliminating the previous
        // in-memory queue window where `delivered: true` could be returned for a
        // turn that had not yet been persisted.
        let scheduled = ScheduledTurn {
            turn_id: String::new(),
            origin_id: context.origin_id.clone(),
            context: target_context,
            input: target_input,
        };

        let delivered = match self.app_state.get() {
            Some(app_state) => {
                match channel_input::submit_scheduled_turn(app_state, scheduled).await {
                    SubmitOutcome::Rejected(reason) => {
                        tracing::warn!(reason = %reason, "agent_send: target turn rejected");
                        false
                    }
                    SubmitOutcome::Started | SubmitOutcome::Queued => true,
                }
            }
            None => {
                tracing::error!("agent_send: no AppState bound; cannot durably accept target turn");
                false
            }
        };

        sanitize_tool_result(
            ToolResult::success(
                serde_json::to_string(&json!({
                    "delivered": delivered,
                    "to": target_id
                }))
                .expect("json"),
            ),
            &[],
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_loop::deserialize_scheduled_turn;
    use crate::channels::adapter::ChannelRegistry;
    use crate::config::{AgentConfig, AgentId};
    use crate::runtime::{AppState, build_sleep_app_state_with_path};
    use crate::storage::{MessageKind, SenderKind};
    use crate::test_util::test_config;
    use crate::tools::Tool;
    use crate::tools::ToolExecutionContext;
    use serde_json::json;
    use std::collections::HashMap;
    use std::sync::Arc;

    fn test_agents() -> HashMap<AgentId, AgentConfig> {
        HashMap::from([
            (
                AgentId::new("lyre"),
                AgentConfig {
                    label: "Lyre".to_string(),
                    ..Default::default()
                },
            ),
            (
                AgentId::new("vega"),
                AgentConfig {
                    label: "Vega".to_string(),
                    ..Default::default()
                },
            ),
        ])
    }

    fn tool_with_agents(agents: HashMap<AgentId, AgentConfig>) -> AgentSendTool {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = test_config(dir.path().to_str().expect("utf8"));
        let db = Arc::new(crate::storage::Database::new(&config.db_path()).expect("db"));
        let channels = Arc::new(ChannelRegistry::new());
        AgentSendTool::new(agents, db, None, channels)
    }

    /// Builds a real `AppState` and an `AgentSendTool` bound to it via
    /// `init_app_state`, so the tool durably accepts its target turns through
    /// the shared intake (the same path production uses). The returned `TempDir`
    /// must outlive `state`.
    fn durable_tool() -> (AgentSendTool, Arc<AppState>, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut config = test_config(dir.path().to_str().expect("utf8"));
        config.agents = test_agents();
        let state =
            build_sleep_app_state_with_path(config.clone(), None).expect("build sleep state");
        // Mirrors `start_channels`, which flips intake on before serving input.
        state.supervisor.start_accepting();
        let state_arc = Arc::new(state);
        let tool = AgentSendTool::new(
            config.agents.clone(),
            Arc::clone(&state_arc.db),
            None,
            Arc::new(ChannelRegistry::new()),
        );
        tool.init_app_state(Arc::clone(&state_arc));
        (tool, state_arc, dir)
    }

    /// Returns the durably accepted target turns from `turn_runs` for `state`.
    fn accepted_turns(state: &AppState) -> Vec<ScheduledTurn> {
        state
            .db
            .scan_durable_pending_turns(100)
            .expect("scan durable turns")
            .into_iter()
            .map(|p| {
                deserialize_scheduled_turn(&p.scheduled_request_json)
                    .expect("deserialize scheduled turn")
            })
            .collect()
    }

    fn test_context_with_agent(agent_id: &str) -> ToolExecutionContext {
        ToolExecutionContext {
            chat_id: 1,
            channel: "discord".to_string(),
            surface_thread: "123".to_string(),
            chat_type: "discord".to_string(),
            agent_id: agent_id.to_string(),
            channel_log_chat_id: Some(99),
            chain_depth: 0,
            origin_id: String::new(),
            turn_id: String::new(),
            skill_env: Arc::new(std::sync::Mutex::new(HashMap::new())),
            tool_call_id: String::new(),
            scope: ConversationScope::Normal,
        }
    }

    #[test]
    fn agent_send_definition_name() {
        let tool = tool_with_agents(test_agents());
        assert_eq!(tool.name(), "agent_send");
    }

    #[test]
    fn agent_send_definition_has_to_and_message() {
        let tool = tool_with_agents(test_agents());
        let def = tool.definition();
        let params = &def.parameters;
        let props = params.get("properties").expect("properties");
        assert!(props.get("to").is_some(), "should have 'to' parameter");
        assert!(
            props.get("message").is_some(),
            "should have 'message' parameter"
        );
        let required = params
            .get("required")
            .expect("required")
            .as_array()
            .expect("array");
        assert!(required.iter().any(|r| r.as_str() == Some("to")));
        assert!(required.iter().any(|r| r.as_str() == Some("message")));
    }

    #[tokio::test]
    async fn agent_send_rejects_unknown_agent() {
        let tool = tool_with_agents(test_agents());
        let ctx = test_context_with_agent("lyre");
        let result = tool
            .execute(json!({"to": "unknown", "message": "hello"}), &ctx)
            .await;
        assert!(result.is_error);
        assert!(result.content.contains("not found"));
    }

    #[tokio::test]
    async fn agent_send_rejects_self_send() {
        let tool = tool_with_agents(test_agents());
        let ctx = test_context_with_agent("lyre");
        let result = tool
            .execute(json!({"to": "lyre", "message": "hello myself"}), &ctx)
            .await;
        assert!(result.is_error);
        assert!(result.content.contains("yourself"));
    }

    #[tokio::test]
    async fn agent_send_returns_delivered_true() {
        let (tool, state, _dir) = durable_tool();
        let ctx = test_context_with_agent("lyre");
        let result = tool
            .execute(json!({"to": "vega", "message": "hello"}), &ctx)
            .await;
        assert!(!result.is_error, "unexpected error: {}", result.content);
        let parsed: serde_json::Value = serde_json::from_str(&result.content).expect("json");
        assert_eq!(parsed["delivered"], true);
        // The turn is durably accepted, not merely queued in memory.
        let turns = accepted_turns(&state);
        assert_eq!(turns.len(), 1, "exactly one target turn should be accepted");
        assert_eq!(turns[0].context.agent_id, "vega");
    }

    #[tokio::test]
    async fn agent_send_returns_delivered_false_when_chain_depth_exceeded() {
        let (tool, state, _dir) = durable_tool();
        let mut ctx = test_context_with_agent("lyre");
        // MAX_AGENT_CHAIN_DEPTH is 4, so chain_depth=4 means target would be 5 and rejected.
        ctx.chain_depth = 4;
        let result = tool
            .execute(json!({"to": "vega", "message": "hello"}), &ctx)
            .await;
        assert!(
            !result.is_error,
            "should be success (not error), but delivered=false"
        );
        let parsed: serde_json::Value = serde_json::from_str(&result.content).expect("json");
        assert_eq!(parsed["delivered"], false);
        assert_eq!(parsed["reason"], "ChainDepthExceeded");
        // No turn should be durably accepted.
        assert!(
            accepted_turns(&state).is_empty(),
            "no turn should be accepted when chain depth exceeded"
        );
    }

    #[tokio::test]
    async fn agent_send_succeeds_at_max_chain_depth_boundary() {
        let (tool, state, _dir) = durable_tool();
        let mut ctx = test_context_with_agent("lyre");
        // chain_depth=3 means target would be 4, which is allowed by the scheduler stop evaluator.
        ctx.chain_depth = 3;
        let result = tool
            .execute(json!({"to": "vega", "message": "hello"}), &ctx)
            .await;
        assert!(!result.is_error, "unexpected error: {}", result.content);
        let parsed: serde_json::Value = serde_json::from_str(&result.content).expect("json");
        assert_eq!(parsed["delivered"], true);
        let turns = accepted_turns(&state);
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].context.chain_depth, 4);
    }

    #[tokio::test]
    async fn agent_send_sends_turn_to_target() {
        let (tool, state, _dir) = durable_tool();
        let ctx = test_context_with_agent("lyre");
        let result = tool
            .execute(json!({"to": "vega", "message": "check this"}), &ctx)
            .await;
        assert!(!result.is_error, "unexpected error: {}", result.content);
        let turns = accepted_turns(&state);
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].context.agent_id, "vega");
    }

    #[tokio::test]
    async fn agent_send_request_key_distinguishes_tool_calls() {
        // Regression: two `agent_send` calls within the same parent Turn that
        // target the same agent with the same message (distinct Tool Call IDs)
        // must map to two distinct child Turns. The Tool Call ID is part of the
        // request key so the Channel Log shows both while the target Turn is
        // deduplicated per call.
        let (tool, state, _dir) = durable_tool();
        let mut ctx_a = test_context_with_agent("lyre");
        ctx_a.turn_id = "parent-turn".to_string();
        ctx_a.tool_call_id = "call-1".to_string();
        let _ = tool
            .execute(json!({"to": "vega", "message": "same message"}), &ctx_a)
            .await;

        let mut ctx_b = test_context_with_agent("lyre");
        ctx_b.turn_id = "parent-turn".to_string();
        ctx_b.tool_call_id = "call-2".to_string();
        let _ = tool
            .execute(json!({"to": "vega", "message": "same message"}), &ctx_b)
            .await;

        let turns = accepted_turns(&state);
        assert_eq!(turns.len(), 2, "two distinct tool calls -> two child turns");
        assert_ne!(
            turns[0].context.request_key, turns[1].context.request_key,
            "distinct tool call IDs must yield distinct request keys"
        );
        assert!(turns[0].context.request_key.contains("call-1"));
        assert!(turns[1].context.request_key.contains("call-2"));
    }

    #[tokio::test]
    async fn agent_send_target_input_format() {
        let (tool, state, _dir) = durable_tool();
        let ctx = test_context_with_agent("lyre");
        let _ = tool
            .execute(json!({"to": "vega", "message": "check this"}), &ctx)
            .await;
        let turns = accepted_turns(&state);
        assert_eq!(turns.len(), 1);
        assert!(turns[0].input.starts_with("[System:"));
        assert!(turns[0].input.contains("[Lyre → Vega]"));
        assert!(turns[0].input.contains("check this"));
    }

    #[tokio::test]
    async fn agent_send_target_input_includes_system_instruction() {
        let (tool, state, _dir) = durable_tool();
        let ctx = test_context_with_agent("lyre");
        let _ = tool
            .execute(json!({"to": "vega", "message": "hello"}), &ctx)
            .await;
        let turns = accepted_turns(&state);
        assert_eq!(turns.len(), 1);
        let expected_prefix = format!("{AGENT_SEND_SYSTEM_INSTRUCTION}\n\n");
        assert!(
            turns[0].input.starts_with(&expected_prefix),
            "target input should start with system instruction followed by blank line"
        );
        assert!(
            turns[0].input[expected_prefix.len()..].starts_with("[Lyre → Vega]"),
            "display text should follow system instruction"
        );
    }

    #[tokio::test]
    async fn agent_send_target_context_uses_source_channel() {
        let (tool, state, _dir) = durable_tool();
        let ctx = test_context_with_agent("lyre");
        let _ = tool
            .execute(json!({"to": "vega", "message": "hello"}), &ctx)
            .await;
        let turns = accepted_turns(&state);
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].context.channel, "discord");
        assert_eq!(turns[0].context.channel_log_chat_id, Some(99));
    }

    #[tokio::test]
    async fn agent_send_target_context_replaces_source_agent_scope() {
        let (tool, state, _dir) = durable_tool();
        let ctx = test_context_with_agent("lyre");
        let _ = tool
            .execute(json!({"to": "vega", "message": "hello"}), &ctx)
            .await;
        let turns = accepted_turns(&state);
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].context.surface_thread, "123");
        assert_eq!(turns[0].context.session_key(), "discord:123:agent:vega");
    }

    #[tokio::test]
    async fn agent_send_saves_to_channel_log() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = test_config(dir.path().to_str().expect("utf8"));
        let db = Arc::new(crate::storage::Database::new(&config.db_path()).expect("db"));
        let channels = Arc::new(ChannelRegistry::new());

        // Create the channel_log chat so messages can be stored
        let log_chat_id = call_blocking(Arc::clone(&db), |db| {
            db.resolve_or_create_chat_id(
                "discord",
                "discord:123:multi-room-log",
                None,
                "channel_log",
                "",
            )
        })
        .await
        .expect("create log chat");

        let agents = test_agents();
        let tool = AgentSendTool::new(agents, Arc::clone(&db), None, channels);
        let ctx = ToolExecutionContext {
            chat_id: 1,
            channel: "discord".to_string(),
            surface_thread: "123".to_string(),
            chat_type: "discord".to_string(),
            agent_id: "lyre".to_string(),
            channel_log_chat_id: Some(log_chat_id),
            chain_depth: 0,
            origin_id: String::new(),
            turn_id: String::new(),
            skill_env: Arc::new(std::sync::Mutex::new(HashMap::new())),
            tool_call_id: String::new(),
            scope: ConversationScope::Normal,
        };

        let _ = tool
            .execute(json!({"to": "vega", "message": "test msg"}), &ctx)
            .await;

        let messages = call_blocking(Arc::clone(&db), move |db| {
            db.get_channel_log_messages(log_chat_id, 10)
        })
        .await
        .expect("get messages");

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].message_kind, MessageKind::AgentSend);
    }

    #[tokio::test]
    async fn agent_send_sets_sender_recipient_ids() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = test_config(dir.path().to_str().expect("utf8"));
        let db = Arc::new(crate::storage::Database::new(&config.db_path()).expect("db"));
        let channels = Arc::new(ChannelRegistry::new());

        let log_chat_id = call_blocking(Arc::clone(&db), |db| {
            db.resolve_or_create_chat_id(
                "discord",
                "discord:123:multi-room-log",
                None,
                "channel_log",
                "",
            )
        })
        .await
        .expect("create log chat");

        let tool = AgentSendTool::new(test_agents(), Arc::clone(&db), None, channels);
        let ctx = ToolExecutionContext {
            chat_id: 1,
            channel: "discord".to_string(),
            surface_thread: "123".to_string(),
            chat_type: "discord".to_string(),
            agent_id: "lyre".to_string(),
            channel_log_chat_id: Some(log_chat_id),
            chain_depth: 0,
            origin_id: String::new(),
            turn_id: String::new(),
            skill_env: Arc::new(std::sync::Mutex::new(HashMap::new())),
            tool_call_id: String::new(),
            scope: ConversationScope::Normal,
        };

        let _ = tool
            .execute(json!({"to": "vega", "message": "test"}), &ctx)
            .await;

        let messages = call_blocking(Arc::clone(&db), move |db| {
            db.get_channel_log_messages(log_chat_id, 10)
        })
        .await
        .expect("messages");

        assert_eq!(messages[0].sender_id, "lyre");
        assert_eq!(messages[0].sender_kind, SenderKind::Tool);
        assert_eq!(messages[0].recipient_agent_id.as_deref(), Some("vega"));
    }
}

#[cfg(test)]
mod integration_tests {
    use super::*;
    use crate::agent_loop::deserialize_scheduled_turn;
    use crate::channels::adapter::ChannelRegistry;
    use crate::config::{AgentConfig, AgentId};
    use crate::runtime::{AppState, build_sleep_app_state_with_path};
    use crate::storage::{MessageKind, SenderKind, call_blocking};
    use crate::test_util::test_config;
    use crate::tools::Tool;
    use serde_json::json;
    use std::collections::HashMap;
    use std::sync::Arc;

    fn multi_agent_config(state_root: &str) -> crate::config::Config {
        let mut config = test_config(state_root);
        config.agents = HashMap::from([
            (
                AgentId::new("lyre"),
                AgentConfig {
                    label: "Lyre".to_string(),
                    ..Default::default()
                },
            ),
            (
                AgentId::new("vega"),
                AgentConfig {
                    label: "Vega".to_string(),
                    ..Default::default()
                },
            ),
            (
                AgentId::new("nova"),
                AgentConfig {
                    label: "Nova".to_string(),
                    ..Default::default()
                },
            ),
        ]);
        config
    }

    fn multi_agent_tool(
        config: &crate::config::Config,
    ) -> (AgentSendTool, Arc<crate::storage::Database>) {
        let db = Arc::new(crate::storage::Database::new(&config.db_path()).expect("db"));
        let channels = Arc::new(ChannelRegistry::new());
        let tool = AgentSendTool::new(config.agents.clone(), Arc::clone(&db), None, channels);
        (tool, db)
    }

    /// Builds a real `AppState` + tool bound to it for the multi-agent config,
    /// so the target turn is durably accepted.
    fn durable_multi_tool(dir: &tempfile::TempDir) -> (AgentSendTool, Arc<AppState>) {
        let config = multi_agent_config(dir.path().to_str().expect("utf8"));
        let state =
            build_sleep_app_state_with_path(config.clone(), None).expect("build sleep state");
        state.supervisor.start_accepting();
        let state_arc = Arc::new(state);
        let tool = AgentSendTool::new(
            config.agents.clone(),
            Arc::clone(&state_arc.db),
            None,
            Arc::new(ChannelRegistry::new()),
        );
        tool.init_app_state(Arc::clone(&state_arc));
        (tool, state_arc)
    }

    fn test_context_with_agent(agent_id: &str) -> ToolExecutionContext {
        ToolExecutionContext {
            chat_id: 1,
            channel: "discord".to_string(),
            surface_thread: "123".to_string(),
            chat_type: "discord".to_string(),
            agent_id: agent_id.to_string(),
            channel_log_chat_id: None,
            chain_depth: 0,
            origin_id: String::new(),
            turn_id: String::new(),
            skill_env: Arc::new(std::sync::Mutex::new(HashMap::new())),
            tool_call_id: String::new(),
            scope: ConversationScope::Normal,
        }
    }

    async fn accepted_turns(state: &AppState) -> Vec<ScheduledTurn> {
        state
            .db
            .scan_durable_pending_turns(100)
            .expect("scan durable turns")
            .into_iter()
            .map(|p| {
                deserialize_scheduled_turn(&p.scheduled_request_json)
                    .expect("deserialize scheduled turn")
            })
            .collect()
    }

    #[tokio::test]
    async fn agent_send_in_single_agent_channel() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (tool, state) = durable_multi_tool(&dir);

        let ctx = test_context_with_agent("lyre");
        let result = tool
            .execute(json!({"to": "vega", "message": "hey vega"}), &ctx)
            .await;
        assert!(!result.is_error, "{}", result.content);

        let turns = accepted_turns(&state).await;
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].context.agent_id, "vega");
        assert_eq!(turns[0].context.channel, "discord");
    }

    #[tokio::test]
    async fn agent_send_to_non_existent_agent() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = multi_agent_config(dir.path().to_str().expect("utf8"));
        let (tool, _db) = multi_agent_tool(&config);

        let ctx = test_context_with_agent("lyre");
        let result = tool
            .execute(json!({"to": "unknown", "message": "hello?"}), &ctx)
            .await;
        assert!(result.is_error);
        assert!(result.content.contains("not found"));
    }

    #[tokio::test]
    async fn agent_send_channel_log_saved() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = multi_agent_config(dir.path().to_str().expect("utf8"));
        let (tool, db) = multi_agent_tool(&config);

        let log_chat_id = call_blocking(Arc::clone(&db), |db| {
            db.resolve_or_create_chat_id(
                "discord",
                "discord:123:multi-room-log",
                None,
                "channel_log",
                "",
            )
        })
        .await
        .expect("log chat");

        let ctx = ToolExecutionContext {
            chat_id: 1,
            channel: "discord".to_string(),
            surface_thread: "123".to_string(),
            chat_type: "discord".to_string(),
            agent_id: "lyre".to_string(),
            channel_log_chat_id: Some(log_chat_id),
            chain_depth: 0,
            origin_id: String::new(),
            turn_id: String::new(),
            skill_env: Arc::new(std::sync::Mutex::new(HashMap::new())),
            tool_call_id: String::new(),
            scope: ConversationScope::Normal,
        };

        let _ = tool
            .execute(json!({"to": "vega", "message": "check this design"}), &ctx)
            .await;

        let messages = call_blocking(Arc::clone(&db), move |db| {
            db.get_channel_log_messages(log_chat_id, 10)
        })
        .await
        .expect("messages");

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].message_kind, MessageKind::AgentSend);
        assert_eq!(messages[0].sender_id, "lyre");
        assert_eq!(messages[0].sender_kind, SenderKind::Tool);
        assert_eq!(messages[0].recipient_agent_id.as_deref(), Some("vega"));
        assert!(messages[0].content.contains("[Lyre → Vega]"));
        assert!(messages[0].content.contains("check this design"));
    }

    #[tokio::test]
    async fn agent_send_target_session_independent() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (tool, state) = durable_multi_tool(&dir);

        let ctx = test_context_with_agent("lyre");
        let _ = tool
            .execute(json!({"to": "vega", "message": "hello"}), &ctx)
            .await;

        let turns = accepted_turns(&state).await;
        assert_eq!(turns.len(), 1);
        assert_ne!(
            turns[0].context.agent_id, ctx.agent_id,
            "target session should be independent from sender"
        );
        assert_eq!(turns[0].context.surface_thread, "123");
        assert_eq!(turns[0].context.session_key(), "discord:123:agent:vega");
    }

    #[tokio::test]
    async fn agent_send_no_channel_log_when_chain_depth_exceeded() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = multi_agent_config(dir.path().to_str().expect("utf8"));
        let (tool, db) = multi_agent_tool(&config);

        let log_chat_id = call_blocking(Arc::clone(&db), |db| {
            db.resolve_or_create_chat_id(
                "discord",
                "discord:123:multi-room-log",
                None,
                "channel_log",
                "",
            )
        })
        .await
        .expect("log chat");

        let mut ctx = test_context_with_agent("lyre");
        ctx.chain_depth = 4; // exceeds limit
        let result = tool
            .execute(json!({"to": "vega", "message": "should not persist"}), &ctx)
            .await;

        assert!(!result.is_error);
        let parsed: serde_json::Value = serde_json::from_str(&result.content).expect("json");
        assert_eq!(parsed["delivered"], false);

        let messages = call_blocking(Arc::clone(&db), move |db| {
            db.get_channel_log_messages(log_chat_id, 10)
        })
        .await
        .expect("messages");

        assert!(
            messages.is_empty(),
            "no message should be persisted when chain depth exceeded"
        );
    }

    #[tokio::test]
    async fn existing_tools_not_affected() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = test_config(dir.path().to_str().expect("utf8"));
        let skills = Arc::new(crate::skills::SkillManager::from_dirs(
            config.user_skills_dir().expect("user_skills_dir"),
            config.skills_dir().expect("skills_dir"),
        ));
        let registry = crate::tools::ToolRegistry::new(&config, skills);

        assert!(registry.is_read_only("read").await);
        assert!(registry.is_read_only("grep").await);
        assert!(!registry.is_read_only("bash").await);
        assert!(!registry.is_read_only("write").await);
    }
}
