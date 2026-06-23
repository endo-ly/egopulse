//! Pulse Activation runner — executes the Pulse Capsule through the LLM with tool support.

use std::sync::Arc;

use crate::agent_loop::ConversationScope;
use crate::agent_loop::SurfaceContext;
use crate::agent_loop::prompt_builder::build_system_prompt;
use crate::agent_loop::tool_phase::{
    MAX_TOOL_ITERATIONS, ToolExecutionHooks, ToolPhaseRequest, ToolPhaseResponse,
    build_tool_result_phase, send_tool_phase_request,
};
use crate::error::EgoPulseError;
use crate::llm::Message;
use crate::pulse::capsule::HomeSurface;
use crate::pulse::capsule::PulseCapsule;
use crate::runtime::AppState;
use crate::storage::{PulseOutputKind, ToolCall as StoredToolCall};
use crate::tools::ToolExecutionContext;
use tracing::warn;

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
        scope: ConversationScope::Normal,
    };

    let system_prompt = build_system_prompt(state, &context);
    let tool_defs = state.tools.definitions_async().await;
    let mut messages = Arc::new(vec![Message::text("user", &capsule.prompt)]);
    let mut tool_phases = Vec::new();

    for iteration in 1..=MAX_TOOL_ITERATIONS {
        let phase_response = send_tool_phase_request(ToolPhaseRequest {
            state,
            llm: channel_llm.as_ref(),
            system_prompt: &system_prompt,
            messages: Arc::clone(&messages),
            tools: Some(Arc::clone(&tool_defs)),
            chat_id,
            caller_channel: &context.channel,
            request_kind: "pulse",
            usage_log_failure: "pulse llm usage logging failed",
            log_scope: "pulse",
            send_failure_log: "pulse LLM send_message failed",
            iteration,
            scope: ConversationScope::Normal,
        })
        .await?;

        let assistant_phase = match phase_response {
            ToolPhaseResponse::Final(response) => {
                let output_text = response.content.trim().to_string();
                let output_kind = classify_output(&output_text);
                return Ok(ActivationResult {
                    output_text,
                    output_kind,
                    tool_phases,
                });
            }
            ToolPhaseResponse::MalformedToolCalls(response) => {
                let output_text = response.content.trim().to_string();
                let output_kind = classify_output(&output_text);
                return Ok(ActivationResult {
                    output_text,
                    output_kind,
                    tool_phases,
                });
            }
            ToolPhaseResponse::ToolCalls(assistant_phase) => assistant_phase,
        };
        let assistant_message_id = format!("pulse-assistant-{}", uuid::Uuid::new_v4());

        let tool_outcomes = crate::agent_loop::tool_phase::execute_tool_calls(
            state,
            &tool_context,
            assistant_phase.tool_calls.clone(),
            ToolExecutionHooks::none(),
        )
        .await?;
        let stored_tool_calls = tool_outcomes
            .iter()
            .map(|outcome| StoredToolCall {
                id: outcome.tool_call.id.clone(),
                chat_id,
                message_id: assistant_message_id.clone(),
                tool_name: outcome.tool_call.name.clone(),
                tool_input: outcome.tool_call.arguments.to_string(),
                tool_output: Some(outcome.payload.clone()),
                timestamp: outcome.timestamp.clone(),
            })
            .collect::<Vec<_>>();
        let tool_result_phase = build_tool_result_phase(tool_outcomes);

        {
            let messages_mut = Arc::make_mut(&mut messages);
            messages_mut.push(assistant_phase.assistant_message.clone());
            messages_mut.extend(tool_result_phase.tool_messages.clone());
        }
        tool_phases.push(ToolPhase {
            assistant_message_id,
            assistant_message: assistant_phase.assistant_message,
            assistant_preview: assistant_phase.assistant_preview,
            tool_messages: tool_result_phase.tool_messages,
            tool_result_preview: tool_result_phase.tool_result_preview,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pulse::capsule::HomeSurface;
    use crate::pulse::capsule::build_capsule;
    use crate::pulse::definition::{TemporalIntention, TemporalSchedule};

    fn test_intention() -> TemporalIntention {
        TemporalIntention {
            id: "morning_review".to_string(),
            enabled: true,
            schedule: TemporalSchedule::Daily {
                at: "09:00".to_string(),
            },
            attention: "Check today's schedule.".to_string(),
            delivery: None,
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

    #[tokio::test]
    async fn activation_logs_llm_usage_with_pulse_request_kind() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state_root = dir.path().to_str().expect("utf8").to_string();

        let provider = crate::agent_loop::turn::RecordingProvider::new(
            vec![Ok(crate::llm::MessagesResponse {
                content: "PULSE_OK".to_string(),
                reasoning_content: None,
                tool_calls: vec![],
                usage: Some(crate::llm::LlmUsage {
                    input_tokens: 50,
                    output_tokens: 25,
                }),
            })],
            vec![0],
        );

        let config = crate::test_util::test_config(&state_root);
        let state = crate::test_util::build_state_with_config(
            config,
            Some(std::sync::Arc::new(provider)),
            None,
            None,
            None,
        );

        let surface = HomeSurface {
            chat_id: 1,
            channel: "cli".to_string(),
            external_chat_id: "test-pulse".to_string(),
            chat_type: "cli".to_string(),
        };

        let capsule = build_capsule(
            "default",
            &test_intention(),
            "",
            &[],
            &surface,
            "2026-05-10T09:00:00+09:00",
        );

        let result = run_activation(&state, "default", &capsule, &surface)
            .await
            .expect("activation");

        assert_eq!(result.output_kind, PulseOutputKind::Silent);

        for _ in 0..20 {
            let row: Option<(String, i64, i64)> = {
                let conn = state.db.get_conn().expect("pool");
                conn.query_row(
                    "SELECT request_kind, input_tokens, output_tokens FROM llm_usage_logs",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .ok()
            };

            if let Some((kind, input, output)) = row {
                assert_eq!(kind, "pulse");
                assert_eq!(input, 50);
                assert_eq!(output, 25);
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        panic!("pulse llm usage log was not written within the polling timeout");
    }
}
