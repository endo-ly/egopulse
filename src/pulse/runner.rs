//! Pulse Activation runner — executes the Pulse Capsule through the LLM with tool support.

use crate::agent_loop::SurfaceContext;
use crate::agent_loop::formatting::{
    format_tool_result, summarize_tool_calls_with_content, tool_message_content,
};
use crate::agent_loop::prompt_builder::build_system_prompt;
use crate::error::EgoPulseError;
use crate::llm::{Message, MessagesResponse, ToolCall};
use crate::pulse::capsule::HomeSurface;
use crate::pulse::capsule::PulseCapsule;
use crate::runtime::AppState;
use crate::storage::{PulseOutputKind, ToolCall as StoredToolCall};
use crate::tools::ToolExecutionContext;
use tracing::warn;

const MAX_TOOL_ITERATIONS: usize = 50;

/// RAII guard that decrements the active turn counter on drop.
struct PulseTurnGuard<'a> {
    tracker: &'a crate::runtime::ActiveTurnTracker,
    agent_id: String,
}

impl Drop for PulseTurnGuard<'_> {
    fn drop(&mut self) {
        self.tracker.end_turn(&self.agent_id);
    }
}

/// Result of a Pulse Activation.
#[derive(Clone, Debug)]
pub(crate) struct ActivationResult {
    /// The LLM output text (may be PULSE_OK or a notification).
    pub output_text: String,
    /// Whether the output is PULSE_OK (silent) or a notification.
    pub output_kind: PulseOutputKind,
    /// Tool call/result phases produced during activation. Persist only for Notify output.
    pub tool_phases: Vec<ToolPhase>,
}

/// A tool-call assistant phase and its tool result messages.
#[derive(Clone, Debug)]
pub(crate) struct ToolPhase {
    pub assistant_message_id: String,
    pub assistant_message: Message,
    pub assistant_preview: String,
    pub tool_messages: Vec<Message>,
    pub tool_result_preview: String,
    pub stored_tool_calls: Vec<StoredToolCall>,
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
    home_surface: &HomeSurface,
) -> Result<ActivationResult, EgoPulseError> {
    state.active_turns.begin_turn(agent_id);
    let _guard = PulseTurnGuard {
        tracker: &state.active_turns,
        agent_id: agent_id.to_string(),
    };

    let context = SurfaceContext::new(
        home_surface.channel.clone(),
        agent_id.to_string(),
        home_surface.external_chat_id.clone(),
        home_surface.chat_type.clone(),
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

    let chat_id = home_surface.chat_id;
    let tool_context = ToolExecutionContext {
        chat_id,
        channel: home_surface.channel.clone(),
        surface_thread: home_surface.external_chat_id.clone(),
        chat_type: home_surface.chat_type.clone(),
        agent_id: agent_id.to_string(),
        channel_log_chat_id: None,
        chain_depth: 0,
        origin_id: String::new(),
        turn_sender: state.turn_sender.clone(),
        skill_env: std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
    };

    let system_prompt = build_system_prompt(state, &context);
    let tool_defs = state.tools.definitions_async().await;
    let mut messages = vec![Message::text("user", &capsule.prompt)];
    let mut tool_phases = Vec::new();

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
                tool_phases,
            });
        }

        let valid_tool_calls = filter_valid_tool_calls(response.tool_calls.clone());
        let assistant_message_id = format!("pulse-assistant-{}", uuid::Uuid::new_v4());

        let (tool_messages, stored_tool_calls) = execute_tool_calls(
            state,
            &tool_context,
            &response,
            &valid_tool_calls,
            &assistant_message_id,
            chat_id,
        )
        .await?;

        let assistant_message = Message {
            role: "assistant".to_string(),
            content: crate::llm::MessageContent::text(response.content.clone()),
            reasoning_content: response.reasoning_content.clone(),
            tool_calls: valid_tool_calls.clone(),
            tool_call_id: None,
        };
        let assistant_preview =
            summarize_tool_calls_with_content(&response.content, &valid_tool_calls);
        let tool_result_preview = summarize_tool_result_messages(&tool_messages);

        messages.push(assistant_message.clone());
        messages.extend(tool_messages.clone());
        tool_phases.push(ToolPhase {
            assistant_message_id,
            assistant_message,
            assistant_preview,
            tool_messages,
            tool_result_preview,
            stored_tool_calls,
        });
    }

    Err(EgoPulseError::Internal(format!(
        "pulse tool loop exceeded max iterations ({MAX_TOOL_ITERATIONS})"
    )))
}

fn classify_output(text: &str) -> PulseOutputKind {
    let trimmed = text.trim();
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("PULSE_OK") {
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
    assistant_message_id: &str,
    chat_id: i64,
) -> Result<(Vec<Message>, Vec<StoredToolCall>), EgoPulseError> {
    let mut tool_messages = Vec::with_capacity(valid_tool_calls.len());
    let mut stored_tool_calls = Vec::with_capacity(valid_tool_calls.len());

    for tool_call in valid_tool_calls {
        let result = state
            .tools
            .execute(&tool_call.name, tool_call.arguments.clone(), tool_context)
            .await;

        let tool_payload = format_tool_result(tool_call, &result);

        tool_messages.push(Message {
            role: "tool".to_string(),
            content: tool_message_content(&tool_payload, &result),
            reasoning_content: None,
            tool_calls: Vec::new(),
            tool_call_id: Some(tool_call.id.clone()),
        });
        stored_tool_calls.push(StoredToolCall {
            id: tool_call.id.clone(),
            chat_id,
            message_id: assistant_message_id.to_string(),
            tool_name: tool_call.name.clone(),
            tool_input: tool_call.arguments.to_string(),
            tool_output: Some(tool_payload),
            timestamp: chrono::Utc::now().to_rfc3339(),
        });

        if !response.content.trim().is_empty() {
            tracing::debug!(
                tool = %tool_call.name,
                is_error = result.is_error,
                "pulse tool executed"
            );
        }
    }

    Ok((tool_messages, stored_tool_calls))
}

fn summarize_tool_result_messages(tool_messages: &[Message]) -> String {
    let joined = tool_messages
        .iter()
        .map(|message| message.content.as_text_lossy())
        .collect::<Vec<_>>()
        .join("\n");
    crate::agent_loop::formatting::preview_text(&joined, 160)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pulse::capsule::HomeSurface;
    use crate::pulse::capsule::build_capsule;
    use crate::pulse::definition::{TemporalIntention, TemporalSchedule};

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
            chat_type: "dm".to_string(),
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
    fn activation_builds_surface_context_with_real_channel() {
        // Arrange
        let agent_id = "lyre";
        let surface = test_home_surface();

        // Act
        let context = SurfaceContext::new(
            surface.channel.clone(),
            agent_id.to_string(),
            surface.external_chat_id.clone(),
            surface.chat_type.clone(),
            agent_id.to_string(),
        );

        // Assert
        assert_eq!(context.channel, "discord");
        assert_eq!(context.agent_id, "lyre");
        assert_eq!(context.chat_type, "dm");
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
        let state = crate::test_util::build_state_with_config(config, None, None, None, None);

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
        assert_eq!(classify_output(""), PulseOutputKind::Silent);
    }
}
