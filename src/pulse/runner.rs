//! Pulse Activation runner — executes the Pulse Capsule through the LLM with tool support.

use std::sync::Arc;

use crate::agent_loop::loop_runner::{
    MAX_TOOL_ITERATIONS, append_guard_denial_wrap, apply_guard_denial_state, messages_for_iteration,
};
use crate::agent_loop::model_step::{ModelRunner, ModelStep, ModelStepRequest, ignore_delta};
use crate::agent_loop::tool_execution::{
    GuardDenialTracker, ToolExecutionHooks, ToolExecutor, build_tool_result_phase,
};
use crate::conversation::ConversationScope;
use crate::conversation::SurfaceContext;
use crate::error::EgoPulseError;
use crate::llm::Message;
use crate::pulse::capsule::HomeSurface;
use crate::pulse::capsule::PulseCapsule;
use crate::runtime::AppState;
use crate::storage::PulseOutputKind;
use crate::tools::ToolExecutionContext;
use tracing::warn;

/// Longest backoff a Pulse activation waits before retrying an LLM call.
///
/// Pulse scans agents sequentially, so a provider quota wait (a long
/// `Retry-After`) fails the activation instead of stalling every later Pulse.
const PULSE_MAX_LLM_RETRY_BACKOFF: std::time::Duration = std::time::Duration::from_secs(60);
const GUARD_DENIAL_FALLBACK_NOTICE: &str = "Pulse stopped after repeated security-guard denials. No further tools were run; review the blocked action and decide how to proceed.";

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
}

/// Builds the surface context for a Pulse activation on the agent's home surface.
///
/// The home surface stores the full session key as `external_chat_id`, so the
/// raw thread is recovered from it; otherwise tools that derive session keys
/// from the context (e.g. `agent_send`) would nest the key.
fn pulse_surface_context(home_surface: &HomeSurface, agent_id: &str) -> SurfaceContext {
    SurfaceContext::new(
        home_surface.channel.clone(),
        agent_id.to_string(),
        SurfaceContext::surface_thread_from_session_key(
            &home_surface.channel,
            &home_surface.external_chat_id,
            agent_id,
        ),
        home_surface.chat_type.clone(),
        agent_id.to_string(),
    )
}

/// Execute a Pulse Activation.
///
/// This runs the Pulse Capsule through the LLM with tool support.
/// It does NOT persist to the normal session (that's handled separately).
/// The supplied configuration snapshot is used consistently for LLM
/// resolution, system-prompt construction, and tool execution.
///
/// # Errors
/// Returns [`EgoPulseError`] when LLM resolution, LLM calls, or tool execution
/// fails. Usage-recording failures are logged and do not fail the activation.
pub(crate) async fn run_activation_with_snapshot(
    state: &AppState,
    agent_id: &str,
    capsule: &PulseCapsule,
    home_surface: &HomeSurface,
    config_snapshot: Arc<crate::config::manager::ConfigSnapshot>,
) -> Result<ActivationResult, EgoPulseError> {
    state.active_turns.begin_turn(agent_id);
    let _guard = PulseTurnGuard {
        tracker: &state.active_turns,
        agent_id: agent_id.to_string(),
    };

    let mut context = pulse_surface_context(home_surface, agent_id);
    let channel_log_key = match context.channel.as_str() {
        "discord" => context
            .surface_thread
            .parse::<u64>()
            .ok()
            .filter(|id| {
                config_snapshot
                    .config
                    .discord_channels()
                    .get(id)
                    .is_some_and(|channel| channel.multi_agent)
            })
            .map(crate::runtime::ChannelLogKey::Discord),
        "telegram" => context
            .surface_thread
            .parse::<i64>()
            .ok()
            .filter(|id| {
                config_snapshot
                    .config
                    .telegram_channels()
                    .get(id)
                    .is_some_and(|channel| channel.multi_agent)
            })
            .map(crate::runtime::ChannelLogKey::Telegram),
        _ => None,
    };
    if let Some(key) = channel_log_key {
        context.channel_log_chat_id = Some(
            crate::storage::call_blocking(state.db_for(context.scope), move |db| match key {
                crate::runtime::ChannelLogKey::Discord(channel_id) => {
                    db.resolve_channel_log_chat_id(channel_id)
                }
                crate::runtime::ChannelLogKey::Telegram(chat_id) => {
                    db.resolve_telegram_channel_log_chat_id(chat_id)
                }
            })
            .await?,
        );
    }

    let turn_dependencies = state.turn_dependencies();
    let channel_llm = turn_dependencies
        .llm_for_context_with_snapshot(&context, &config_snapshot)
        .inspect_err(|e| {
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
        surface_thread: context.surface_thread.clone(),
        chat_type: home_surface.chat_type.clone(),
        agent_id: agent_id.to_string(),
        channel_log_chat_id: context.channel_log_chat_id,
        chain_depth: 0,
        origin_id: String::new(),
        turn_id: uuid::Uuid::new_v4().to_string(),
        skill_env: std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        scope: ConversationScope::Normal,
        config_snapshot: Some(Arc::clone(&config_snapshot)),
        tool_call_id: String::new(),
    };

    let system_prompt = crate::agent_loop::prompt::build_system_prompt_with_config(
        &turn_dependencies,
        &context,
        &config_snapshot.config,
    );
    let tool_defs = state.tools.definitions_async().await;
    let mut messages = Arc::new(vec![Message::text("user", &capsule.prompt)]);
    let mut tool_phases = Vec::new();
    let guard_tracker = Arc::new(GuardDenialTracker::new());

    for iteration in 1..=MAX_TOOL_ITERATIONS {
        let request_messages =
            append_guard_denial_wrap(messages_for_iteration(&messages, iteration), &guard_tracker);
        let model_runner = ModelRunner::new(ModelStepRequest {
            state: &turn_dependencies,
            llm: channel_llm.as_ref(),
            system_prompt: &system_prompt,
            messages: Arc::clone(&request_messages),
            tools: Some(apply_guard_denial_state(&tool_defs, &guard_tracker)),
            chat_id,
            caller_channel: &context.channel,
            request_kind: "pulse",
            usage_log_failure: "pulse llm usage logging failed",
            log_scope: "pulse",
            send_failure_log: "pulse LLM send_message failed",
            iteration,
            scope: ConversationScope::Normal,
            on_delta: &ignore_delta,
        });
        let phase_response = match model_runner
            .run_with_transient_retry(PULSE_MAX_LLM_RETRY_BACKOFF)
            .await
        {
            Ok(response) => response,
            Err(error) if guard_tracker.limit_reached() => {
                let (error, _) = error.into_parts();
                warn!(error = %error, "pulse could not generate a report after guard denial limit");
                return Ok(guard_denial_fallback(tool_phases));
            }
            Err(error) => return Err(error.into_parts().0),
        };

        let assistant_phase = match phase_response {
            ModelStep::Final(response) => {
                let output_text = response.content.trim().to_string();
                return Ok(activation_result(
                    output_text,
                    tool_phases,
                    guard_tracker.limit_reached(),
                ));
            }
            ModelStep::MalformedToolCalls(response) => {
                let output_text = response.content.trim().to_string();
                return Ok(activation_result(
                    output_text,
                    tool_phases,
                    guard_tracker.limit_reached(),
                ));
            }
            ModelStep::ToolCalls(assistant_phase) => assistant_phase,
        };
        let assistant_message_id = format!("pulse-assistant-{}", uuid::Uuid::new_v4());

        let tool_outcomes = ToolExecutor::new(
            &turn_dependencies,
            &tool_context,
            ToolExecutionHooks::none(),
        )
        .with_guard_denial_tracker(Arc::clone(&guard_tracker))
        .execute(&assistant_message_id, assistant_phase.tool_calls.clone())
        .await?;
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
        });
    }

    if guard_tracker.limit_reached() {
        return Ok(guard_denial_fallback(tool_phases));
    }

    Err(EgoPulseError::Internal(format!(
        "pulse tool loop exceeded max iterations ({MAX_TOOL_ITERATIONS})"
    )))
}

fn activation_result(
    output_text: String,
    tool_phases: Vec<ToolPhase>,
    guard_limit_reached: bool,
) -> ActivationResult {
    if guard_limit_reached && classify_output(&output_text) == PulseOutputKind::Silent {
        return guard_denial_fallback(tool_phases);
    }
    ActivationResult {
        output_kind: classify_output(&output_text),
        output_text,
        tool_phases,
    }
}

fn guard_denial_fallback(tool_phases: Vec<ToolPhase>) -> ActivationResult {
    ActivationResult {
        output_text: GUARD_DENIAL_FALLBACK_NOTICE.to_string(),
        output_kind: PulseOutputKind::Notify,
        tool_phases,
    }
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
    use crate::llm::ToolDefinition;
    use crate::pulse::capsule::HomeSurface;
    use crate::pulse::capsule::build_capsule;
    use crate::pulse::definition::{TemporalIntention, TemporalSchedule};
    use crate::tools::{Tool, ToolExecutionContext, ToolResult};
    use serial_test::serial;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct SideEffectProbeTool {
        calls: Arc<AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl Tool for SideEffectProbeTool {
        fn name(&self) -> &str {
            "write_probe"
        }

        fn definition(&self) -> ToolDefinition {
            ToolDefinition {
                name: self.name().to_string(),
                description: "Test-only side-effecting Tool".to_string(),
                parameters: serde_json::json!({"type": "object"}),
            }
        }

        async fn execute(
            &self,
            _input: serde_json::Value,
            _context: &ToolExecutionContext,
        ) -> ToolResult {
            self.calls.fetch_add(1, Ordering::SeqCst);
            ToolResult::success("write completed".to_string())
        }
    }

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
    fn pulse_surface_context_maps_back_to_home_session_key() {
        // Arrange
        let home_surface = HomeSurface {
            chat_id: 25,
            channel: "discord".to_string(),
            external_chat_id: "discord:999:agent:vega".to_string(),
            chat_type: "discord".to_string(),
        };

        // Act
        let context = pulse_surface_context(&home_surface, "vega");

        // Assert
        assert_eq!(context.surface_thread, "999");
        assert_eq!(context.session_key(), home_surface.external_chat_id);
    }

    #[test]
    fn classify_output_covers_silent_and_notify_results() {
        let cases = [
            ("PULSE_OK", PulseOutputKind::Silent),
            ("  pulse_ok  ", PulseOutputKind::Silent),
            ("You have 3 unread messages.", PulseOutputKind::Notify),
        ];

        for (input, expected) in cases {
            assert_eq!(classify_output(input), expected, "input: {input:?}");
        }
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
    #[serial]
    async fn guard_denial_limit_blocks_side_effect_tools_and_notifies_on_report_failure() {
        // Arrange: three denied bash calls cross the limit; a subsequent
        // side-effecting Tool request is blocked and a failed report becomes
        // a fixed Notify result.
        let dir = tempfile::tempdir().expect("tempdir");
        let write_calls = Arc::new(AtomicUsize::new(0));
        let denied_call = |id: &str| crate::llm::ToolCall {
            id: id.to_string(),
            name: "bash".to_string(),
            arguments: serde_json::json!({"command": "cat .env"}),
            invalid_arguments: None,
        };
        let tool_response = |id: &str, content: &str| {
            Ok(crate::llm::MessagesResponse {
                content: content.to_string(),
                reasoning_content: None,
                tool_calls: vec![denied_call(id)],
                usage: None,
            })
        };
        let responses = vec![
            tool_response("pulse-guard-1", "Trying."),
            tool_response("pulse-guard-2", "Retrying."),
            tool_response("pulse-guard-3", "Again."),
            Ok(crate::llm::MessagesResponse {
                content: "Once more.".to_string(),
                reasoning_content: None,
                tool_calls: vec![crate::llm::ToolCall {
                    id: "pulse-guard-blocked".to_string(),
                    name: "write_probe".to_string(),
                    arguments: serde_json::json!({}),
                    invalid_arguments: None,
                }],
                usage: None,
            }),
            Err(crate::error::LlmError::RequestConstructionFailed(
                "report generation failed".to_string(),
            )),
        ];
        let provider =
            crate::agent_loop::test_support::RecordingProvider::new(responses, vec![0; 5]);
        let observer = provider.clone();
        let config = crate::test_util::test_config(dir.path().to_str().expect("utf8"));
        let mut state = crate::test_util::build_state_with_config(
            config,
            Some(std::sync::Arc::new(provider)),
            None,
            None,
            None,
        );
        Arc::get_mut(&mut state.tools)
            .expect("tool registry is uniquely owned")
            .register_tool(Box::new(SideEffectProbeTool {
                calls: Arc::clone(&write_calls),
            }));
        let chat_id = crate::storage::call_blocking(Arc::clone(&state.db), move |db| {
            db.resolve_or_create_chat_id("cli", "cli:guard-pulse:agent:default", "cli", "default")
        })
        .await
        .expect("chat id");
        let surface = HomeSurface {
            chat_id,
            channel: "cli".to_string(),
            external_chat_id: "guard-pulse".to_string(),
            chat_type: "cli".to_string(),
        };
        let capsule = build_capsule(
            "default",
            &test_intention(),
            &[],
            &surface,
            "2026-05-10T09:00:00+09:00",
        );

        // Act
        let result = run_activation_with_snapshot(
            &state,
            "default",
            &capsule,
            &surface,
            state.config_manager.current_blocking(),
        )
        .await
        .expect("pulse wraps up after the denial limit");

        // Assert: the wrap-up guard rode along with the fourth request and the
        // failed LLM report is converted to a visible fixed notification.
        assert_eq!(result.output_text, GUARD_DENIAL_FALLBACK_NOTICE);
        assert_eq!(result.output_kind, PulseOutputKind::Notify);
        let seen_messages = observer.seen_messages();
        assert_eq!(seen_messages.len(), 5);
        assert!(
            observer.seen_tools()[3].as_ref().is_some_and(Vec::is_empty),
            "subsequent model requests must not expose Tool definitions"
        );
        let fourth_request = seen_messages[3].last().expect("wrap guard message");
        assert!(
            fourth_request
                .content
                .as_text_lossy()
                .contains(crate::agent_loop::loop_runner::GUARD_DENIAL_WRAP_GUARD)
        );

        // Assert: the blocked side-effecting Tool did not run and its tool
        // result reports the stop condition.
        let blocked_result = seen_messages[4]
            .iter()
            .find(|message| message.tool_call_id.as_deref() == Some("pulse-guard-blocked"))
            .expect("blocked call must receive a tool result");
        assert!(
            blocked_result
                .content
                .as_text_lossy()
                .contains("All Tool execution is stopped for this run")
        );
        assert_eq!(write_calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn pulse_guard_limit_never_silently_accepts_pulse_ok() {
        let result = activation_result("PULSE_OK".to_string(), Vec::new(), true);

        assert_eq!(result.output_kind, PulseOutputKind::Notify);
        assert_eq!(result.output_text, GUARD_DENIAL_FALLBACK_NOTICE);
    }

    #[tokio::test]
    #[serial]
    async fn late_tool_loop_requests_final_response_at_hard_cap() {
        // Arrange: keep the activation in the Tool phase until the shared
        // warning boundary, then return a response after the shared guards.
        let dir = tempfile::tempdir().expect("tempdir");
        let mut responses = Vec::with_capacity(
            crate::agent_loop::loop_runner::FINAL_RESPONSE_WARNING_ITERATION + 2,
        );
        for iteration in 1..=(crate::agent_loop::loop_runner::FINAL_RESPONSE_WARNING_ITERATION + 1)
        {
            responses.push(Ok(crate::llm::MessagesResponse {
                content: format!("Checking result {iteration}"),
                reasoning_content: None,
                tool_calls: vec![crate::llm::ToolCall {
                    id: format!("pulse-ls-{iteration}"),
                    name: "ls".to_string(),
                    arguments: serde_json::json!({}),
                    invalid_arguments: None,
                }],
                usage: None,
            }));
        }
        responses.push(Ok(crate::llm::MessagesResponse {
            content: "The Pulse result is ready.".to_string(),
            reasoning_content: None,
            tool_calls: Vec::new(),
            usage: None,
        }));
        let provider = crate::agent_loop::test_support::RecordingProvider::new(
            responses,
            vec![0; crate::agent_loop::loop_runner::FINAL_RESPONSE_WARNING_ITERATION + 2],
        );
        let observer = provider.clone();
        let config = crate::test_util::test_config(dir.path().to_str().expect("utf8"));
        let state = crate::test_util::build_state_with_config(
            config,
            Some(std::sync::Arc::new(provider)),
            None,
            None,
            None,
        );
        let chat_id = crate::storage::call_blocking(Arc::clone(&state.db), move |db| {
            db.resolve_or_create_chat_id("cli", "cli:late-pulse:agent:default", "cli", "default")
        })
        .await
        .expect("chat id");
        let surface = HomeSurface {
            chat_id,
            channel: "cli".to_string(),
            external_chat_id: "late-pulse".to_string(),
            chat_type: "cli".to_string(),
        };
        let capsule = build_capsule(
            "default",
            &test_intention(),
            &[],
            &surface,
            "2026-05-10T09:00:00+09:00",
        );

        // Act
        let result = run_activation_with_snapshot(
            &state,
            "default",
            &capsule,
            &surface,
            state.config_manager.current_blocking(),
        )
        .await
        .expect("late Pulse tool loop should finalize");

        // Assert: Pulse uses the same final-response guards as a normal Turn.
        assert_eq!(result.output_text, "The Pulse result is ready.");
        assert_eq!(result.output_kind, PulseOutputKind::Notify);
        let seen_messages = observer.seen_messages();
        let warning_message = seen_messages
            [crate::agent_loop::loop_runner::FINAL_RESPONSE_WARNING_ITERATION - 1]
            .last()
            .expect("warning guard message");
        assert!(
            warning_message
                .content
                .as_text_lossy()
                .contains(crate::agent_loop::loop_runner::FINAL_RESPONSE_WARNING_GUARD)
        );
        let final_guard_message = seen_messages
            [crate::agent_loop::loop_runner::FINAL_RESPONSE_WARNING_ITERATION + 1]
            .last()
            .expect("final guard message");
        assert!(
            final_guard_message
                .content
                .as_text_lossy()
                .contains(crate::agent_loop::loop_runner::FINAL_RESPONSE_GUARD)
        );
    }

    #[tokio::test]
    async fn activation_logs_llm_usage_with_pulse_request_kind() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state_root = dir.path().to_str().expect("utf8").to_string();

        let provider = crate::agent_loop::test_support::RecordingProvider::new(
            vec![Ok(crate::llm::MessagesResponse {
                content: "PULSE_OK".to_string(),
                reasoning_content: None,
                tool_calls: vec![],
                usage: Some(crate::llm::LlmUsage {
                    input_tokens: 50,
                    output_tokens: 25,
                    cached_input_tokens: None,
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
            &[],
            &surface,
            "2026-05-10T09:00:00+09:00",
        );

        let result = run_activation_with_snapshot(
            &state,
            "default",
            &capsule,
            &surface,
            state.config_manager.current_blocking(),
        )
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

    #[tokio::test]
    #[serial]
    async fn activation_reuses_shared_empty_response_recovery() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let state_root = dir.path().to_str().expect("utf8").to_string();
        let provider = crate::agent_loop::test_support::RecordingProvider::new(
            vec![
                Err(crate::error::LlmError::InvalidResponse(
                    "assistant content was empty (output_items=1)".to_string(),
                )),
                Ok(crate::llm::MessagesResponse {
                    content: "PULSE_OK".to_string(),
                    reasoning_content: None,
                    tool_calls: vec![],
                    usage: None,
                }),
            ],
            vec![0, 0],
        );
        let observer = provider.clone();
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
            external_chat_id: "test-pulse-empty-recovery".to_string(),
            chat_type: "cli".to_string(),
        };
        let capsule = build_capsule(
            "default",
            &test_intention(),
            &[],
            &surface,
            "2026-05-10T09:00:00+09:00",
        );

        // Act
        let result = run_activation_with_snapshot(
            &state,
            "default",
            &capsule,
            &surface,
            state.config_manager.current_blocking(),
        )
        .await
        .expect("activation should recover");

        // Assert
        assert_eq!(result.output_kind, PulseOutputKind::Silent);
        let seen_messages = observer.seen_messages();
        assert_eq!(seen_messages.len(), 2);
        let retry_message = seen_messages[1].last().expect("runtime guard message");
        assert_eq!(retry_message.role, "user");
        assert!(
            crate::agent_loop::message_format::message_to_text(retry_message)
                .contains("no user-visible text")
        );
    }

    fn pulse_ok_response() -> Result<crate::llm::MessagesResponse, crate::error::LlmError> {
        Ok(crate::llm::MessagesResponse {
            content: "PULSE_OK".to_string(),
            reasoning_content: None,
            tool_calls: vec![],
            usage: None,
        })
    }

    #[tokio::test]
    #[serial]
    async fn activation_agent_send_uses_shared_log_and_target_session() {
        use crate::config::{
            AgentConfig, AgentId, ChannelConfig, ChannelName, DiscordChannelConfig,
        };
        use crate::runtime::turn::{ResponseDelivery, ScheduleResult, ScheduledTurn};

        for (multi_agent, existing_log) in [(true, true), (true, false), (false, false)] {
            // Arrange: a real tool, durable intake and DB, with the recipient
            // queued until the activation has completed its model loop.
            let dir = tempfile::tempdir().expect("tempdir");
            let mut config = crate::test_util::test_config(dir.path().to_str().unwrap());
            config
                .agents
                .insert(AgentId::new("vega"), AgentConfig::default());
            config.channels.insert(
                ChannelName::new("discord"),
                ChannelConfig {
                    enabled: Some(true),
                    discord_channels: Some(std::collections::HashMap::from([(
                        123,
                        DiscordChannelConfig {
                            multi_agent,
                            agents: vec![AgentId::new("default"), AgentId::new("vega")],
                            ..Default::default()
                        },
                    )])),
                    ..Default::default()
                },
            );
            let mut send_response = pulse_ok_response().unwrap();
            send_response.tool_calls.push(crate::llm::ToolCall {
                id: "pulse-send".to_string(),
                name: "agent_send".to_string(),
                arguments: serde_json::json!({"to":"vega", "message":"pulse delivery"}),
                invalid_arguments: None,
            });
            let provider = crate::agent_loop::test_support::RecordingProvider::new(
                vec![Ok(send_response), pulse_ok_response(), pulse_ok_response()],
                vec![0; 3],
            );
            let mut state = crate::test_util::build_state_with_config(
                config,
                Some(Arc::new(provider)),
                None,
                None,
                None,
            );
            let intake = Arc::new(crate::runtime::TurnIntake::new());
            let tool = crate::tools::AgentSendTool::new_with_manager(
                Arc::clone(&state.db),
                None,
                Arc::clone(&state.channels),
                Arc::clone(&intake),
                Arc::clone(&state.config_manager),
            );
            Arc::get_mut(&mut state.tools)
                .unwrap()
                .register_tool(Box::new(tool));
            let state = Arc::new(state);
            intake.bind(&state);
            state.supervisor.start_accepting();
            let sender_id = state
                .db
                .resolve_or_create_chat_id(
                    "discord",
                    "discord:123:agent:default",
                    "discord",
                    "default",
                )
                .unwrap();
            let preexisting_log_id =
                existing_log.then(|| state.db.resolve_channel_log_chat_id(123).unwrap());
            let surface = crate::pulse::capsule::resolve_home_surface(
                &state.db,
                "default",
                &["discord"],
                Some(&crate::pulse::definition::DeliverySpec {
                    channel: "discord".to_string(),
                    external_chat_id: "123".to_string(),
                }),
            )
            .await
            .unwrap()
            .unwrap();
            assert_eq!(surface.chat_id, sender_id);
            let target_context = SurfaceContext::new(
                "discord".to_string(),
                "scheduler".to_string(),
                "123".to_string(),
                "discord".to_string(),
                "vega".to_string(),
            );
            assert!(matches!(
                state.turn_scheduler.submit(ScheduledTurn {
                    turn_id: "blocker".to_string(),
                    origin_id: "blocker".to_string(),
                    context: target_context,
                    input: "blocker".to_string(),
                    config_snapshot: Some(state.config_manager.current_blocking()),
                    received_at: None,
                    response_delivery: ResponseDelivery::Channel,
                }),
                ScheduleResult::Started(_)
            ));
            let capsule = build_capsule(
                "default",
                &test_intention(),
                &[],
                &surface,
                "2026-05-10T09:00:00+09:00",
            );

            // Act
            run_activation_with_snapshot(
                &state,
                "default",
                &capsule,
                &surface,
                state.config_manager.current_blocking(),
            )
            .await
            .unwrap();
            let target = state
                .turn_scheduler
                .on_turn_completed("discord:123:agent:vega")
                .expect("queued recipient turn");
            let shared_log = state
                .db
                .get_chat_by_channel_external_and_agent("discord", "discord:123:multi-room-log", "")
                .unwrap();
            assert_eq!(shared_log.is_some(), multi_agent);
            let expected_log = shared_log.as_ref().map(|chat| chat.chat_id);
            if let Some(preexisting_log_id) = preexisting_log_id {
                assert_eq!(expected_log, Some(preexisting_log_id));
            }

            assert_eq!(target.context.channel_log_chat_id, expected_log);
            assert_eq!(target.context.session_key(), "discord:123:agent:vega");
            let (stored_chat, count): (i64, i64) = state
                .db
                .get_conn()
                .unwrap()
                .query_row(
                    "SELECT chat_id, COUNT(*) FROM messages WHERE message_kind = 'agent_send'",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap();
            assert_eq!(count, 1);
            assert_eq!(stored_chat, expected_log.unwrap_or(sender_id));
            crate::runtime::execute_scheduled_turn(&state, target).await;
            let target_chat = state
                .db
                .get_chat_by_channel_external_and_agent("discord", "discord:123:agent:vega", "vega")
                .unwrap()
                .unwrap();
            assert!(
                state
                    .db
                    .load_session_snapshot(target_chat.chat_id, 30)
                    .unwrap()
                    .messages_json
                    .is_some()
            );
            let chat_count: i64 = state
                .db
                .get_conn()
                .unwrap()
                .query_row("SELECT COUNT(*) FROM chats", [], |row| row.get(0))
                .unwrap();
            assert_eq!(
                chat_count,
                2 + i64::from(multi_agent),
                "only the sender, recipient, and configured shared log exist"
            );
            state.supervisor.shutdown().await;
        }
    }

    fn api_error(status: u16, retry_after_secs: Option<u64>) -> crate::error::LlmError {
        crate::error::LlmError::ApiError {
            status: reqwest::StatusCode::from_u16(status).expect("status"),
            body_preview: "provider error".to_string(),
            retry_after_secs,
        }
    }

    async fn activate_with_responses(
        state_root: &str,
        responses: Vec<Result<crate::llm::MessagesResponse, crate::error::LlmError>>,
    ) -> (
        Result<ActivationResult, EgoPulseError>,
        crate::agent_loop::test_support::RecordingProvider,
    ) {
        let delays = vec![0; responses.len()];
        let provider = crate::agent_loop::test_support::RecordingProvider::new(responses, delays)
            .with_streaming_delta("partial, unpublished text");
        let observer = provider.clone();
        let state = crate::test_util::build_state_with_config(
            crate::test_util::test_config(state_root),
            Some(std::sync::Arc::new(provider)),
            None,
            None,
            None,
        );
        let surface = HomeSurface {
            chat_id: 1,
            channel: "cli".to_string(),
            external_chat_id: "test-pulse-retry".to_string(),
            chat_type: "cli".to_string(),
        };
        let capsule = build_capsule(
            "default",
            &test_intention(),
            &[],
            &surface,
            "2026-05-10T09:00:00+09:00",
        );
        let result = run_activation_with_snapshot(
            &state,
            "default",
            &capsule,
            &surface,
            state.config_manager.current_blocking(),
        )
        .await;
        (result, observer)
    }

    #[tokio::test]
    #[serial]
    async fn activation_retries_transient_llm_error() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let responses = vec![Err(api_error(503, None)), pulse_ok_response()];

        // Act
        let (result, observer) =
            activate_with_responses(dir.path().to_str().expect("utf8"), responses).await;

        // Assert
        let result = result.expect("activation should recover from a transient error");
        assert_eq!(result.output_kind, PulseOutputKind::Silent);
        assert_eq!(observer.seen_messages().len(), 2);
    }

    #[tokio::test]
    #[serial]
    async fn activation_fails_fast_when_retry_after_exceeds_backoff_limit() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let responses = vec![Err(api_error(429, Some(3_600))), pulse_ok_response()];

        // Act
        let (result, observer) = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            activate_with_responses(dir.path().to_str().expect("utf8"), responses),
        )
        .await
        .expect("long Retry-After must fail without waiting");

        // Assert
        assert!(result.is_err(), "quota wait must not be retried");
        assert_eq!(observer.seen_messages().len(), 1);
    }

    #[tokio::test]
    #[serial]
    async fn activation_recovers_empty_response_after_delta_and_guard_failure() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let mut empty = pulse_ok_response().unwrap();
        empty.content = "<thinking>hidden</thinking>".to_string();
        let responses = vec![Ok(empty), Err(api_error(503, Some(0))), pulse_ok_response()];

        // Act
        let (result, observer) =
            activate_with_responses(dir.path().to_str().expect("utf8"), responses).await;

        // Assert
        assert_eq!(
            result.expect("empty response should recover").output_kind,
            PulseOutputKind::Silent
        );
        let requests = observer.seen_messages();
        assert_eq!(requests.len(), 3);
        assert_eq!(
            requests[1], requests[2],
            "transport retry must preserve the guarded request"
        );
        assert!(
            requests[1]
                .last()
                .unwrap()
                .content
                .as_text_lossy()
                .contains("no user-visible text")
        );
    }
}
