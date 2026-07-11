//! `agent_send` tool — inter-agent communication within the current channel.
//!
//! Allows one agent to send a message to another agent in the same channel.
//! The message is displayed as `[From → To] message` and the target agent's
//! next turn is queued for background execution.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;

use crate::agent_loop::{ConversationScope, PendingAgentTurn, SurfaceContext};
use crate::config::{AgentConfig, AgentId};
use crate::llm::ToolDefinition;
use crate::runtime::turn_scheduler::{StopReason, evaluate_stop_conditions};
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
}

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

#[async_trait]
impl Tool for AgentSendTool {
    fn name(&self) -> &str {
        "agent_send"
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

        // 3. Queue target agent turn
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

            request_key: String::new(),
        };

        let target_input = format!("{AGENT_SEND_SYSTEM_INSTRUCTION}\n\n{display_text}");

        let turn = PendingAgentTurn {
            context: target_context,
            input: target_input,
            origin_id: context.origin_id.clone(),
        };

        if let Err(error) = context.turn_sender.send(turn).await {
            tracing::warn!(error = %error, "agent_send: failed to queue target turn");
            return ToolResult::error(format!(
                "failed to queue turn for agent '{target_id}': {error}"
            ));
        }

        sanitize_tool_result(
            ToolResult::success(
                serde_json::to_string(&json!({
                    "delivered": true,
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
    use crate::config::{AgentConfig, AgentId};
    use crate::storage::{MessageKind, SenderKind};
    use crate::test_util::test_config;
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
        let channels = Arc::new(crate::channels::adapter::ChannelRegistry::new());
        AgentSendTool::new(agents, db, None, channels)
    }

    fn test_context_with_agent(
        agent_id: &str,
        turn_sender: tokio::sync::mpsc::Sender<PendingAgentTurn>,
    ) -> ToolExecutionContext {
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
            turn_sender,
            skill_env: std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
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
        let (tx, _rx) = tokio::sync::mpsc::channel(16);
        let ctx = test_context_with_agent("lyre", tx);
        let result = tool
            .execute(json!({"to": "unknown", "message": "hello"}), &ctx)
            .await;
        assert!(result.is_error);
        assert!(result.content.contains("not found"));
    }

    #[tokio::test]
    async fn agent_send_rejects_self_send() {
        let tool = tool_with_agents(test_agents());
        let (tx, _rx) = tokio::sync::mpsc::channel(16);
        let ctx = test_context_with_agent("lyre", tx);
        let result = tool
            .execute(json!({"to": "lyre", "message": "hello myself"}), &ctx)
            .await;
        assert!(result.is_error);
        assert!(result.content.contains("yourself"));
    }

    #[tokio::test]
    async fn agent_send_returns_delivered_true() {
        let tool = tool_with_agents(test_agents());
        let (tx, _rx) = tokio::sync::mpsc::channel(16);
        let ctx = test_context_with_agent("lyre", tx);
        let result = tool
            .execute(json!({"to": "vega", "message": "hello"}), &ctx)
            .await;
        assert!(!result.is_error, "unexpected error: {}", result.content);
        let parsed: serde_json::Value = serde_json::from_str(&result.content).expect("json");
        assert_eq!(parsed["delivered"], true);
        assert_eq!(parsed["to"], "vega");
    }

    #[tokio::test]
    async fn agent_send_returns_delivered_false_when_chain_depth_exceeded() {
        let tool = tool_with_agents(test_agents());
        let (tx, mut rx) = tokio::sync::mpsc::channel(16);
        let mut ctx = test_context_with_agent("lyre", tx);
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
        assert_eq!(parsed["to"], "vega");
        assert_eq!(parsed["reason"], "ChainDepthExceeded");
        // No turn should be queued
        assert!(
            rx.try_recv().is_err(),
            "no turn should be queued when chain depth exceeded"
        );
    }

    #[tokio::test]
    async fn agent_send_succeeds_at_max_chain_depth_boundary() {
        let tool = tool_with_agents(test_agents());
        let (tx, mut rx) = tokio::sync::mpsc::channel(16);
        let mut ctx = test_context_with_agent("lyre", tx);
        // chain_depth=3 means target would be 4, which is allowed by the scheduler stop evaluator.
        ctx.chain_depth = 3;
        let result = tool
            .execute(json!({"to": "vega", "message": "hello"}), &ctx)
            .await;
        assert!(!result.is_error, "unexpected error: {}", result.content);
        let parsed: serde_json::Value = serde_json::from_str(&result.content).expect("json");
        assert_eq!(parsed["delivered"], true);
        // Turn should be queued with chain_depth = 4
        let turn = rx.try_recv().expect("turn should be queued at boundary");
        assert_eq!(turn.context.chain_depth, 4);
    }

    #[tokio::test]
    async fn agent_send_sends_turn_to_target() {
        let tool = tool_with_agents(test_agents());
        let (tx, mut rx) = tokio::sync::mpsc::channel(16);
        let ctx = test_context_with_agent("lyre", tx);
        let result = tool
            .execute(json!({"to": "vega", "message": "check this"}), &ctx)
            .await;
        assert!(!result.is_error, "unexpected error: {}", result.content);

        let turn = rx.try_recv().expect("should have queued a turn");
        assert_eq!(turn.context.agent_id, "vega");
    }

    #[tokio::test]
    async fn agent_send_target_input_format() {
        let tool = tool_with_agents(test_agents());
        let (tx, mut rx) = tokio::sync::mpsc::channel(16);
        let ctx = test_context_with_agent("lyre", tx);
        let _ = tool
            .execute(json!({"to": "vega", "message": "check this"}), &ctx)
            .await;

        let turn = rx.try_recv().expect("turn");
        assert!(turn.input.starts_with("[System:"));
        assert!(turn.input.contains("[Lyre → Vega]"));
        assert!(turn.input.contains("check this"));
    }

    #[tokio::test]
    async fn agent_send_target_input_includes_system_instruction() {
        let tool = tool_with_agents(test_agents());
        let (tx, mut rx) = tokio::sync::mpsc::channel(16);
        let ctx = test_context_with_agent("lyre", tx);
        let _ = tool
            .execute(json!({"to": "vega", "message": "hello"}), &ctx)
            .await;

        let turn = rx.try_recv().expect("turn");
        let expected_prefix = format!("{AGENT_SEND_SYSTEM_INSTRUCTION}\n\n");
        assert!(
            turn.input.starts_with(&expected_prefix),
            "target input should start with system instruction followed by blank line"
        );
        assert!(
            turn.input[expected_prefix.len()..].starts_with("[Lyre → Vega]"),
            "display text should follow system instruction"
        );
    }

    #[tokio::test]
    async fn agent_send_target_context_uses_source_channel() {
        let tool = tool_with_agents(test_agents());
        let (tx, mut rx) = tokio::sync::mpsc::channel(16);
        let ctx = test_context_with_agent("lyre", tx);
        let _ = tool
            .execute(json!({"to": "vega", "message": "hello"}), &ctx)
            .await;

        let turn = rx.try_recv().expect("turn");
        assert_eq!(turn.context.channel, "discord");
        assert_eq!(turn.context.channel_log_chat_id, Some(99));
    }

    #[tokio::test]
    async fn agent_send_target_context_replaces_source_agent_scope() {
        let tool = tool_with_agents(test_agents());
        let (tx, mut rx) = tokio::sync::mpsc::channel(16);
        let ctx = test_context_with_agent("lyre", tx);
        let _ = tool
            .execute(json!({"to": "vega", "message": "hello"}), &ctx)
            .await;

        let turn = rx.try_recv().expect("turn");
        assert_eq!(turn.context.surface_thread, "123");
        assert_eq!(turn.context.session_key(), "discord:123:agent:vega");
    }

    #[tokio::test]
    async fn agent_send_saves_to_channel_log() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = test_config(dir.path().to_str().expect("utf8"));
        let db = Arc::new(crate::storage::Database::new(&config.db_path()).expect("db"));
        let channels = Arc::new(crate::channels::adapter::ChannelRegistry::new());

        // Create the channel_log_chat so messages can be stored
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
        let (tx, _rx) = tokio::sync::mpsc::channel(16);
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
            turn_sender: tx,
            skill_env: std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
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
        let channels = Arc::new(crate::channels::adapter::ChannelRegistry::new());

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
        let (tx, _rx) = tokio::sync::mpsc::channel(16);
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
            turn_sender: tx,
            skill_env: std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
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
    use crate::config::{AgentConfig, AgentId};
    use crate::storage::{MessageKind, SenderKind, call_blocking};
    use crate::test_util::test_config;
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
        let channels = Arc::new(crate::channels::adapter::ChannelRegistry::new());
        let tool = AgentSendTool::new(config.agents.clone(), Arc::clone(&db), None, channels);
        (tool, db)
    }

    #[tokio::test]
    async fn agent_send_in_single_agent_channel() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = multi_agent_config(dir.path().to_str().expect("utf8"));
        let (tool, _db) = multi_agent_tool(&config);

        let (tx, mut rx) = tokio::sync::mpsc::channel(16);
        let ctx = ToolExecutionContext {
            chat_id: 1,
            channel: "discord".to_string(),
            surface_thread: "123".to_string(),
            chat_type: "discord".to_string(),
            agent_id: "lyre".to_string(),
            channel_log_chat_id: None,
            chain_depth: 0,
            origin_id: String::new(),
            turn_id: String::new(),
            turn_sender: tx,
            skill_env: std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            scope: ConversationScope::Normal,
        };

        let result = tool
            .execute(json!({"to": "vega", "message": "hey vega"}), &ctx)
            .await;
        assert!(!result.is_error, "{}", result.content);

        let turn = rx.try_recv().expect("turn queued");
        assert_eq!(turn.context.agent_id, "vega");
        assert_eq!(turn.context.channel, "discord");
    }

    #[tokio::test]
    async fn agent_send_to_non_existent_agent() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = multi_agent_config(dir.path().to_str().expect("utf8"));
        let (tool, _db) = multi_agent_tool(&config);

        let (tx, _) = tokio::sync::mpsc::channel(16);
        let ctx = ToolExecutionContext {
            chat_id: 1,
            channel: "discord".to_string(),
            surface_thread: "123".to_string(),
            chat_type: "discord".to_string(),
            agent_id: "lyre".to_string(),
            channel_log_chat_id: None,
            chain_depth: 0,
            origin_id: String::new(),
            turn_id: String::new(),
            turn_sender: tx,
            skill_env: std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            scope: ConversationScope::Normal,
        };

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

        let (tx, _rx) = tokio::sync::mpsc::channel(16);
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
            turn_sender: tx,
            skill_env: std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
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
        let config = multi_agent_config(dir.path().to_str().expect("utf8"));
        let (tool, _db) = multi_agent_tool(&config);

        let (tx, mut rx) = tokio::sync::mpsc::channel(16);
        let ctx = ToolExecutionContext {
            chat_id: 1,
            channel: "discord".to_string(),
            surface_thread: "123".to_string(),
            chat_type: "discord".to_string(),
            agent_id: "lyre".to_string(),
            channel_log_chat_id: None,
            chain_depth: 0,
            origin_id: String::new(),
            turn_id: String::new(),
            turn_sender: tx,
            skill_env: std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            scope: ConversationScope::Normal,
        };

        let _ = tool
            .execute(json!({"to": "vega", "message": "hello"}), &ctx)
            .await;

        let turn = rx.try_recv().expect("turn");
        assert_ne!(
            turn.context.agent_id, ctx.agent_id,
            "target session should be independent from sender"
        );
        assert_eq!(turn.context.surface_thread, "123");
        assert_eq!(turn.context.session_key(), "discord:123:agent:vega");
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

        let (tx, _rx) = tokio::sync::mpsc::channel(16);
        let ctx = ToolExecutionContext {
            chat_id: 1,
            channel: "discord".to_string(),
            surface_thread: "123".to_string(),
            chat_type: "discord".to_string(),
            agent_id: "lyre".to_string(),
            channel_log_chat_id: Some(log_chat_id),
            chain_depth: 4, // exceeds limit
            origin_id: String::new(),
            turn_id: String::new(),
            turn_sender: tx,
            skill_env: std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            scope: ConversationScope::Normal,
        };

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
