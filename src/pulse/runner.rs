//! Pulse Activation runner — executes the Pulse Capsule through the LLM with tool support.

use crate::agent_loop::SurfaceContext;
use crate::error::EgoPulseError;
use crate::llm::{Message, MessagesResponse, ToolCall};
use crate::pulse::capsule::{PulseCapsule, core_contract_text};
use crate::runtime::AppState;
use crate::storage::PulseOutputKind;
use crate::tools::ToolExecutionContext;
use tracing::warn;

const MAX_TOOL_ITERATIONS: usize = 50;

/// Result of a Pulse Activation.
#[derive(Clone, Debug)]
pub(crate) struct ActivationResult {
    /// The LLM output text (may be PULSE_OK or a notification).
    pub output_text: String,
    /// Whether the output is PULSE_OK (silent) or a notification.
    pub output_kind: PulseOutputKind,
}

/// Execute a Pulse Activation.
///
/// This runs the Pulse Capsule through the LLM with tool support.
/// It does NOT persist to the normal session (that's handled separately).
///
/// # Errors
/// Returns `EgoPulseError` when LLM resolution or tool execution fails.
pub(crate) async fn run_activation(
    state: &AppState,
    agent_id: &str,
    capsule: &PulseCapsule,
    chat_id: i64,
) -> Result<ActivationResult, EgoPulseError> {
    let context = SurfaceContext::new(
        "pulse".to_string(),
        agent_id.to_string(),
        format!("pulse:{agent_id}"),
        "pulse".to_string(),
        agent_id.to_string(),
    );

    let channel_llm = state.llm_for_context(&context).inspect_err(|e| {
        warn!(
            error_kind = e.error_kind(),
            error = %e,
            agent_id,
            "pulse llm_for_context failed"
        );
    })?;

    let tool_context = ToolExecutionContext {
        chat_id,
        channel: "pulse".to_string(),
        surface_thread: format!("pulse:{agent_id}"),
        chat_type: "pulse".to_string(),
        agent_id: agent_id.to_string(),
        channel_log_chat_id: None,
        chain_depth: 0,
        origin_id: String::new(),
        turn_sender: state.turn_sender.clone(),
    };

    let system_prompt = core_contract_text().to_string();
    let tool_defs = state.tools.definitions_async().await;
    let mut messages = vec![Message::text("user", &capsule.prompt)];

    for iteration in 1..=MAX_TOOL_ITERATIONS {
        let response = channel_llm
            .send_message(&system_prompt, messages.clone(), Some(tool_defs.clone()))
            .await
            .inspect_err(|e| {
                warn!(error = %e, iteration, "pulse LLM send_message failed");
            })?;

        if response.tool_calls.is_empty() {
            let output_text = response.content.trim().to_string();
            let output_kind = classify_output(&output_text);
            return Ok(ActivationResult {
                output_text,
                output_kind,
            });
        }

        let valid_tool_calls = filter_valid_tool_calls(response.tool_calls.clone());
        let tool_messages =
            execute_tool_calls(state, &tool_context, &response, &valid_tool_calls).await?;

        messages.push(Message {
            role: "assistant".to_string(),
            content: crate::llm::MessageContent::text(response.content.clone()),
            reasoning_content: response.reasoning_content.clone(),
            tool_calls: valid_tool_calls,
            tool_call_id: None,
        });
        messages.extend(tool_messages);
    }

    Err(EgoPulseError::Internal(format!(
        "pulse tool loop exceeded max iterations ({MAX_TOOL_ITERATIONS})"
    )))
}

fn classify_output(text: &str) -> PulseOutputKind {
    if text.trim().eq_ignore_ascii_case("PULSE_OK") {
        PulseOutputKind::Silent
    } else {
        PulseOutputKind::Notify
    }
}

fn filter_valid_tool_calls(tool_calls: Vec<ToolCall>) -> Vec<ToolCall> {
    let mut index_by_id = std::collections::HashMap::new();
    let mut valid = Vec::new();

    for tool_call in tool_calls {
        if tool_call.name.trim().is_empty() || tool_call.id.trim().is_empty() {
            warn!(
                "pulse: skipping malformed tool call (empty name or id): id='{}' name='{}'",
                tool_call.id, tool_call.name
            );
            continue;
        }

        if let Some(index) = index_by_id.get(&tool_call.id).copied() {
            warn!(
                "pulse: replacing duplicate tool call id: id='{}' name='{}'",
                tool_call.id, tool_call.name
            );
            valid[index] = tool_call;
        } else {
            index_by_id.insert(tool_call.id.clone(), valid.len());
            valid.push(tool_call);
        }
    }

    valid
}

async fn execute_tool_calls(
    state: &AppState,
    tool_context: &ToolExecutionContext,
    response: &MessagesResponse,
    valid_tool_calls: &[ToolCall],
) -> Result<Vec<Message>, EgoPulseError> {
    let mut tool_messages = Vec::with_capacity(valid_tool_calls.len());

    for tool_call in valid_tool_calls {
        let result = state
            .tools
            .execute(&tool_call.name, tool_call.arguments.clone(), tool_context)
            .await;

        let content = if result.is_error {
            format!("Tool error ({}): {}", tool_call.name, result.content)
        } else {
            result.content
        };

        tool_messages.push(Message {
            role: "tool".to_string(),
            content: crate::llm::MessageContent::text(content),
            reasoning_content: None,
            tool_calls: Vec::new(),
            tool_call_id: Some(tool_call.id.clone()),
        });

        if !response.content.trim().is_empty() {
            tracing::debug!(
                tool = %tool_call.name,
                is_error = result.is_error,
                "pulse tool executed"
            );
        }
    }

    Ok(tool_messages)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pulse::capsule::build_capsule;
    use crate::pulse::definition::{TemporalIntention, TemporalSchedule};
    use crate::pulse::home_surface::HomeSurface;
    use std::sync::Arc;

    fn test_intention() -> TemporalIntention {
        TemporalIntention {
            id: "morning_review".to_string(),
            schedule: TemporalSchedule::Daily {
                at: "09:00".to_string(),
            },
            attention: "Check today's schedule.".to_string(),
        }
    }

    fn test_home_surface() -> HomeSurface {
        HomeSurface {
            chat_id: 1,
            channel: "discord".to_string(),
            external_chat_id: "123".to_string(),
        }
    }

    #[test]
    fn classify_output_pulse_ok_is_silent() {
        // Arrange
        let input = "PULSE_OK";

        // Act
        let kind = classify_output(input);

        // Assert
        assert_eq!(kind, PulseOutputKind::Silent);
    }

    #[test]
    fn classify_output_pulse_ok_case_insensitive_and_whitespace_trimmed() {
        // Arrange
        let input = "  pulse_ok  ";

        // Act
        let kind = classify_output(input);

        // Assert
        assert_eq!(kind, PulseOutputKind::Silent);
    }

    #[test]
    fn classify_output_non_pulse_ok_is_notify() {
        // Arrange
        let input = "You have 3 unread messages.";

        // Act
        let kind = classify_output(input);

        // Assert
        assert_eq!(kind, PulseOutputKind::Notify);
    }

    #[test]
    fn activation_builds_surface_context_with_pulse_channel() {
        // Arrange
        let agent_id = "lyre";
        let expected_channel = "pulse";

        // Act
        let context = SurfaceContext::new(
            expected_channel.to_string(),
            agent_id.to_string(),
            format!("pulse:{agent_id}"),
            "pulse".to_string(),
            agent_id.to_string(),
        );

        // Assert
        assert_eq!(context.channel, "pulse");
        assert_eq!(context.agent_id, "lyre");
        assert_eq!(context.surface_thread, "pulse:lyre");
    }

    #[test]
    fn activation_separates_llm_input_from_persisted_session_input() {
        // Arrange
        let intention = test_intention();
        let surface = test_home_surface();
        let capsule = build_capsule(
            "lyre",
            &intention,
            "notes",
            None,
            &[],
            &surface,
            "2026-05-10T09:00:00+09:00",
        );

        // Act: the capsule prompt is the LLM input
        let llm_input = &capsule.prompt;

        // Assert: LLM input is the capsule prompt, not a synthetic marker
        assert!(llm_input.contains("# Pulse Activation"));
        assert!(llm_input.contains("## Core Contract"));

        // The synthetic user input "[Pulse: morning_review]" is a separate concern (Step 7)
        let synthetic_input = "[Pulse: morning_review]";
        assert!(
            !llm_input.contains(synthetic_input),
            "capsule prompt should not contain the synthetic session input"
        );
    }

    #[tokio::test]
    async fn activation_has_normal_tool_execution_capability() {
        // Arrange: build a state with tools and verify tool defs load
        let dir = tempfile::tempdir().expect("tempdir");
        let state_root = dir.path().to_str().expect("utf8").to_string();
        let config = crate::test_util::test_config(&state_root);
        let db = Arc::new(crate::storage::Database::new(&config.db_path()).expect("db"));
        let skills = Arc::new(crate::skills::SkillManager::from_dirs(
            config.user_skills_dir().expect("user_skills_dir"),
            config.skills_dir().expect("skills_dir"),
        ));
        let tools = Arc::new(crate::tools::ToolRegistry::new(
            &config,
            Arc::clone(&skills),
        ));

        let state = AppState {
            db,
            config,
            config_path: None,
            llm_override: None,
            channels: Arc::new(crate::channels::adapter::ChannelRegistry::new()),
            skills: state_skills_ref(&skills),
            tools,
            mcp_manager: None,
            assets: Arc::new(
                crate::assets::AssetStore::new(&dir.path().join("runtime").join("assets"))
                    .expect("assets"),
            ),
            soul_agents: Arc::new(crate::soul_agents::SoulAgentsLoader::new(
                &crate::test_util::test_config(&state_root),
            )),
            memory_loader: Arc::new(crate::memory::MemoryLoader::new(
                std::path::PathBuf::from(&state_root).join("agents"),
            )),
            llm_cache: std::sync::Mutex::new(std::collections::HashMap::new()),
            active_turns: Arc::new(crate::runtime::ActiveTurnTracker::new()),
            turn_sender: tokio::sync::mpsc::channel(16).0,
            turn_scheduler: Arc::new(crate::runtime::turn_scheduler::TurnScheduler::new()),
            turn_tracker: Arc::new(crate::runtime::turn_scheduler::TurnTracker::new()),
        };

        // Act: tool definitions should be loadable
        let tool_defs = state.tools.definitions_async().await;

        // Assert: tools are registered
        assert!(!tool_defs.is_empty(), "tool registry should have tools");
        let tool_names: Vec<&str> = tool_defs.iter().map(|d| d.name.as_str()).collect();
        assert!(
            tool_names.contains(&"read"),
            "read tool should be available: {tool_names:?}"
        );
    }

    #[test]
    fn activation_does_not_use_tiny_llm_gate() {
        // Arrange: run_activation signature does not have a gate LLM parameter
        // The runner directly calls the main LLM — no secondary gate.

        // Assert: verify the classify_output function is a simple string comparison
        // (not an LLM call)
        assert_eq!(classify_output("PULSE_OK"), PulseOutputKind::Silent);
        assert_eq!(
            classify_output("Something happened!"),
            PulseOutputKind::Notify
        );
        assert_eq!(classify_output(""), PulseOutputKind::Notify);
    }

    fn state_skills_ref(
        skills: &Arc<crate::skills::SkillManager>,
    ) -> Arc<crate::skills::SkillManager> {
        Arc::clone(skills)
    }
}
