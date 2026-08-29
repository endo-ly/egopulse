//! Agent Loop policy and model/tool iteration state.

use std::sync::Arc;

use tracing::warn;

use crate::agent_loop::TurnDependencies;
use crate::agent_loop::compaction::{PromptContext, maybe_compact_messages};
use crate::agent_loop::event::{AgentEvent, EventEmitter};
use crate::agent_loop::message_format::strip_thinking;
use crate::agent_loop::model_step::{ModelRunner, ModelStep, ModelStepRequest};
use crate::agent_loop::response_guard::{is_declarative_only_reply, runtime_guard_messages};
use crate::agent_loop::tool_execution::{
    ExecutedToolCall, MAX_TOOL_RESULT_TEXT_CHARS, ToolExecutionHooks, ToolExecutor,
    build_tool_result_phase,
};
use crate::agent_loop::turn::PreparedTurn;
use crate::agent_loop::turn::lifecycle::TurnLifecycle;
use crate::agent_loop::turn::persistence::TurnPersistence;
use crate::channels::utils::text::truncate_by_chars;
use crate::conversation::SurfaceContext;
use crate::error::EgoPulseError;
use crate::llm::{Message, ToolCall};

/// Maximum number of model/tool iterations allowed for one activation.
pub(crate) const MAX_TOOL_ITERATIONS: usize = 50;

/// The iteration at which the model is warned that the hard loop limit is near.
pub(crate) const FINAL_RESPONSE_WARNING_ITERATION: usize = MAX_TOOL_ITERATIONS - 2;

/// Runtime guard appended to the iteration-48 request to warn the model that
/// the tool loop is nearing its hard limit and begin final-response preparation.
pub(crate) const FINAL_RESPONSE_WARNING_GUARD: &str = "[runtime_guard]: The tool loop is near its hard limit. Two model iterations remain after this one. Do not start broad new work; prepare the best concise answer for the user now and state any uncertainty. If this is a Pulse activation and you have used tools, summarize the result instead of returning PULSE_OK.";

/// Runtime guard appended to requests in the final-response window to
/// prioritize a concise final response over starting new broad tool work.
pub(crate) const FINAL_RESPONSE_GUARD: &str = "[runtime_guard]: The tool loop is at its final response window. Provide the best concise answer to the user now. Do not start broad new work; state what was completed and any uncertainty. If this is a Pulse activation and you have used tools, summarize the result instead of returning PULSE_OK.";

/// Mutable conversation state owned by one Agent Loop.
pub(crate) struct LoopState {
    pub(crate) messages: Arc<Vec<Message>>,
    pub(crate) session_revision: Option<i64>,
    retry_messages: Option<Arc<Vec<Message>>>,
    declarative_retry_attempted: bool,
}

impl LoopState {
    pub(crate) fn new(messages: Arc<Vec<Message>>, session_revision: Option<i64>) -> Self {
        Self {
            messages,
            session_revision,
            retry_messages: None,
            declarative_retry_attempted: false,
        }
    }

    fn request_messages(&mut self) -> Arc<Vec<Message>> {
        self.retry_messages
            .take()
            .unwrap_or_else(|| Arc::clone(&self.messages))
    }

    fn reset_retry_guards_after_tool_phase(&mut self) {
        self.declarative_retry_attempted = false;
    }
}

/// Result returned when the Agent Loop has produced a final response.
pub(crate) struct AgentLoopResult {
    pub(crate) final_content: String,
    pub(crate) reasoning_content: Option<String>,
    pub(crate) messages: Arc<Vec<Message>>,
    pub(crate) session_revision: Option<i64>,
}

/// Executes the LLM/tool loop for one durable Turn.
pub(crate) struct AgentLoop<'a> {
    state: &'a TurnDependencies,
    context: &'a SurfaceContext,
    prepared: &'a PreparedTurn,
    prompt_ctx: PromptContext<'a>,
    channel_context_msg: Option<Message>,
    on_event: EventEmitter,
    lifecycle: TurnLifecycle<'a>,
    persistence: TurnPersistence<'a>,
}

impl<'a> AgentLoop<'a> {
    /// Creates an Agent Loop with dependencies fixed for one Turn.
    pub(super) fn new(
        state: &'a TurnDependencies,
        context: &'a SurfaceContext,
        prepared: &'a PreparedTurn,
        prompt_ctx: PromptContext<'a>,
        channel_context_msg: Option<Message>,
        on_event: EventEmitter,
    ) -> Self {
        let lifecycle =
            TurnLifecycle::new(state, context.scope, &prepared.turn_id, &context.origin_id);
        let persistence = TurnPersistence::new(state, context, prepared.chat_id, &prepared.turn_id);
        Self {
            state,
            context,
            prepared,
            prompt_ctx,
            channel_context_msg,
            on_event,
            lifecycle,
            persistence,
        }
    }

    /// Runs model steps, tool execution, and loop-level compaction.
    pub(crate) async fn run(
        &self,
        messages: Arc<Vec<Message>>,
        session_revision: Option<i64>,
        start_iteration: usize,
    ) -> Result<AgentLoopResult, EgoPulseError> {
        let mut loop_state = LoopState::new(messages, session_revision);

        for iteration in start_iteration..=MAX_TOOL_ITERATIONS {
            self.on_event.emit(AgentEvent::Iteration { iteration });
            let request_messages = request_messages_for_iteration(
                &mut loop_state,
                iteration,
                &self.channel_context_msg,
            );
            let event_emitter = self.on_event.clone();
            let on_delta = move |text: String| {
                event_emitter.emit(AgentEvent::Delta { text });
            };
            let model_runner = ModelRunner::new(ModelStepRequest {
                state: self.state,
                llm: self.prepared.channel_llm.as_ref(),
                system_prompt: &self.prepared.system_prompt,
                messages: Arc::clone(&request_messages),
                tools: Some(Arc::clone(&self.prepared.tool_defs)),
                chat_id: self.prepared.chat_id,
                caller_channel: &self.context.channel,
                request_kind: "agent_loop",
                usage_log_failure: "llm usage logging failed",
                log_scope: "agent_loop",
                send_failure_log: "LLM send_message failed",
                iteration,
                scope: self.context.scope,
                on_delta: &on_delta,
            });
            let phase_response = match model_runner.run_with_retry(&self.prepared.turn_id).await {
                Ok(response) => response,
                Err(error) => {
                    let (error, output_published) = error.into_parts();
                    if output_published {
                        self.lifecycle.mark_output_published().await;
                    }
                    return Err(error);
                }
            };
            drop(request_messages);

            match phase_response {
                ModelStep::Final(response) => {
                    self.lifecycle.complete_model().await?;
                    match evaluate_end_turn(
                        &response.content,
                        response.reasoning_content.as_deref(),
                        &mut loop_state.declarative_retry_attempted,
                        &loop_state.messages,
                    )? {
                        LoopAction::Retry(messages) => loop_state.retry_messages = Some(messages),
                        LoopAction::Done {
                            final_content,
                            reasoning_content,
                        } => {
                            return Ok(AgentLoopResult {
                                final_content,
                                reasoning_content,
                                messages: loop_state.messages,
                                session_revision: loop_state.session_revision,
                            });
                        }
                    }
                    continue;
                }
                ModelStep::MalformedToolCalls(response) => {
                    self.lifecycle.complete_model().await?;
                    match evaluate_malformed_response(
                        &response.content,
                        response.reasoning_content.as_deref(),
                        &mut loop_state.declarative_retry_attempted,
                        &loop_state.messages,
                    )? {
                        LoopAction::Retry(messages) => loop_state.retry_messages = Some(messages),
                        LoopAction::Done {
                            final_content,
                            reasoning_content,
                        } => {
                            return Ok(AgentLoopResult {
                                final_content,
                                reasoning_content,
                                messages: loop_state.messages,
                                session_revision: loop_state.session_revision,
                            });
                        }
                    }
                    continue;
                }
                ModelStep::ToolCalls(assistant_phase) => {
                    self.execute_tool_phase(&mut loop_state, assistant_phase)
                        .await?;
                }
            }

            loop_state.reset_retry_guards_after_tool_phase();
            match maybe_compact_messages(
                self.state,
                self.context,
                self.prepared.chat_id,
                &loop_state.messages,
                &self.prepared.channel_llm,
                &self.prompt_ctx,
                &self.prepared.config_snapshot.config,
            )
            .await
            {
                Ok(compacted) => loop_state.messages = Arc::new(compacted),
                Err(error) => warn!(
                    iteration,
                    error = %error,
                    "message compaction failed; continuing with uncompacted messages"
                ),
            }
        }

        Err(EgoPulseError::Internal(format!(
            "tool loop exceeded max iterations ({MAX_TOOL_ITERATIONS})"
        )))
    }

    async fn execute_tool_phase(
        &self,
        loop_state: &mut LoopState,
        assistant_phase: crate::agent_loop::model_step::AssistantToolPhase,
    ) -> Result<(), EgoPulseError> {
        self.lifecycle.complete_model().await?;
        self.lifecycle.begin_tools().await?;
        self.lifecycle.mark_output_published().await;

        let assistant_message_id = uuid::Uuid::new_v4().to_string();
        let messages = std::mem::replace(&mut loop_state.messages, Arc::new(Vec::new()));
        let messages = Arc::try_unwrap(messages).unwrap_or_else(|messages| (*messages).clone());
        let persisted = self
            .persistence
            .persist_tool_call(
                &assistant_message_id,
                &assistant_phase,
                messages,
                loop_state.session_revision,
            )
            .await?;
        let tool_outcomes = self
            .execute_tools(&assistant_message_id, assistant_phase.tool_calls)
            .await?;
        let persisted = self
            .persistence
            .persist_tool_results(
                &assistant_message_id,
                persisted.messages,
                build_tool_result_phase(tool_outcomes),
                Some(persisted.revision),
            )
            .await?;
        loop_state.messages = Arc::new(persisted.messages);
        loop_state.session_revision = Some(persisted.revision);
        self.lifecycle.complete_tools().await?;
        let persisted = self
            .persistence
            .commit_staged_user_messages(
                Arc::clone(&loop_state.messages),
                loop_state.session_revision,
                &self.prepared.config_snapshot,
                &self.on_event,
            )
            .await?;
        loop_state.messages = Arc::new(persisted.messages);
        loop_state.session_revision = Some(persisted.revision);
        Ok(())
    }

    async fn execute_tools(
        &self,
        assistant_message_id: &str,
        tool_calls: Vec<ToolCall>,
    ) -> Result<Vec<ExecutedToolCall>, EgoPulseError> {
        let start_emitter = self.on_event.clone();
        let result_emitter = self.on_event.clone();
        let hooks = ToolExecutionHooks {
            on_start: Some(Arc::new(move |tool_call: &ToolCall| {
                start_emitter.emit(AgentEvent::ToolStart {
                    call_id: tool_call.id.clone(),
                    name: tool_call.name.clone(),
                    input: tool_call.arguments.clone(),
                });
            })),
            on_result: Some(Arc::new(move |outcome: &ExecutedToolCall| {
                result_emitter.emit(AgentEvent::ToolResult {
                    call_id: outcome.tool_call.id.clone(),
                    name: outcome.tool_call.name.clone(),
                    is_error: outcome.result.is_error,
                    preview: truncate_by_chars(&outcome.payload, MAX_TOOL_RESULT_TEXT_CHARS),
                    duration_ms: outcome.duration_ms,
                });
            })),
        };

        ToolExecutor::new(self.state, &self.prepared.tool_context, hooks)
            .execute(assistant_message_id, tool_calls)
            .await
    }
}

/// Adds channel context and the iteration-specific guard to a request-only copy.
fn request_messages_for_iteration(
    loop_state: &mut LoopState,
    iteration: usize,
    channel_context_msg: &Option<Message>,
) -> Arc<Vec<Message>> {
    let mut request_messages = loop_state.request_messages();
    if iteration == 1 {
        if let Some(ctx_msg) = channel_context_msg {
            let mut messages =
                Arc::try_unwrap(request_messages).unwrap_or_else(|messages| (*messages).clone());
            messages.insert(0, ctx_msg.clone());
            request_messages = Arc::new(messages);
        }
    }

    messages_for_iteration(&request_messages, iteration)
}

/// Adds the iteration-specific runtime guard to a request-only copy of `messages`.
pub(crate) fn messages_for_iteration(
    messages: &Arc<Vec<Message>>,
    iteration: usize,
) -> Arc<Vec<Message>> {
    let guard_messages = match iteration {
        FINAL_RESPONSE_WARNING_ITERATION => Some(FINAL_RESPONSE_WARNING_GUARD),
        iteration if iteration > FINAL_RESPONSE_WARNING_ITERATION => Some(FINAL_RESPONSE_GUARD),
        _ => None,
    };
    let Some(guard) = guard_messages else {
        return Arc::clone(messages);
    };

    let mut request_messages = (**messages).clone();
    request_messages.push(Message::text("user", guard));
    Arc::new(request_messages)
}

enum LoopAction {
    Retry(Arc<Vec<Message>>),
    Done {
        final_content: String,
        reasoning_content: Option<String>,
    },
}

fn evaluate_end_turn(
    raw_content: &str,
    reasoning_content: Option<&str>,
    declarative_retry_attempted: &mut bool,
    messages: &[Message],
) -> Result<LoopAction, EgoPulseError> {
    let visible_text = strip_thinking(raw_content.trim());
    let has_displayable_output = !visible_text.trim().is_empty();

    if has_displayable_output
        && !*declarative_retry_attempted
        && is_declarative_only_reply(&visible_text)
    {
        *declarative_retry_attempted = true;
        warn!("declarative-only reply detected; injecting corrective prompt and retrying once");
        return Ok(LoopAction::Retry(Arc::new(runtime_guard_messages(
            messages,
            raw_content,
            reasoning_content,
            "[runtime_guard]: Your previous reply only declared what you would do without actually executing any tools. If the user's request requires tool calls, execute them NOW instead of just describing what you plan to do. Then provide the result.",
        ))));
    }

    if !has_displayable_output {
        return Err(EgoPulseError::Llm(crate::error::LlmError::InvalidResponse(
            "assistant content was empty after retry".to_string(),
        )));
    }

    Ok(LoopAction::Done {
        final_content: visible_text.trim().to_string(),
        reasoning_content: reasoning_content.map(ToString::to_string),
    })
}

fn evaluate_malformed_response(
    raw_content: &str,
    reasoning_content: Option<&str>,
    declarative_retry_attempted: &mut bool,
    messages: &[Message],
) -> Result<LoopAction, EgoPulseError> {
    let visible_text = strip_thinking(raw_content.trim());

    if visible_text.trim().is_empty() {
        return Err(EgoPulseError::Llm(crate::error::LlmError::InvalidResponse(
            "all tool calls were malformed (empty names)".to_string(),
        )));
    }

    if !*declarative_retry_attempted && is_declarative_only_reply(&visible_text) {
        *declarative_retry_attempted = true;
        warn!("all tool calls were malformed and reply was declarative-only; retrying once");
        return Ok(LoopAction::Retry(Arc::new(runtime_guard_messages(
            messages,
            raw_content,
            reasoning_content,
            "[runtime_guard]: Your previous reply attempted tool use but did not produce a valid executable tool call. If tools are required, call them now and then provide the result.",
        ))));
    }

    Ok(LoopAction::Done {
        final_content: visible_text.trim().to_string(),
        reasoning_content: reasoning_content.map(ToString::to_string),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_loop::process_turn;
    use crate::agent_loop::test_support::{
        RecordingProvider, build_state_with_provider, cli_context,
    };
    use crate::conversation::ConversationScope;
    use crate::llm::{MessagesResponse, ToolCall};
    use crate::storage::call_blocking;
    use crate::tools::{Tool, ToolExecutionContext, ToolResult};
    use serial_test::serial;
    use std::sync::Arc;

    struct BlockingTool {
        started: Arc<std::sync::atomic::AtomicUsize>,
        started_notify: Arc<tokio::sync::Notify>,
        release: Arc<tokio::sync::Notify>,
    }

    #[async_trait::async_trait]
    impl Tool for BlockingTool {
        fn name(&self) -> &str {
            "wait_test"
        }

        fn definition(&self) -> crate::llm::ToolDefinition {
            crate::llm::ToolDefinition {
                name: "wait_test".to_string(),
                description: "test-only blocking tool".to_string(),
                parameters: serde_json::json!({"type": "object"}),
            }
        }

        async fn execute(
            &self,
            input: serde_json::Value,
            _context: &ToolExecutionContext,
        ) -> ToolResult {
            self.started
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.started_notify.notify_waiters();
            self.release.notified().await;
            ToolResult::success(format!("released: {input}"))
        }
    }

    #[tokio::test]
    #[serial]
    async fn late_tool_loop_requests_final_response_at_hard_cap() {
        // Arrange: keep the model in the Tool phase until the warning boundary,
        // then return a final response after the shared runtime guards.
        let dir = tempfile::tempdir().expect("tempdir");
        let mut responses = Vec::with_capacity(FINAL_RESPONSE_WARNING_ITERATION + 2);
        for iteration in 1..=(FINAL_RESPONSE_WARNING_ITERATION + 1) {
            responses.push(Ok(MessagesResponse {
                content: format!("Checking result {iteration}"),
                reasoning_content: None,
                tool_calls: vec![ToolCall {
                    id: format!("cap-ls-{iteration}"),
                    name: "ls".to_string(),
                    arguments: serde_json::json!({}),
                }],
                usage: None,
            }));
        }
        responses.push(Ok(MessagesResponse {
            content: "The available results are complete.".to_string(),
            reasoning_content: None,
            tool_calls: Vec::new(),
            usage: None,
        }));
        let provider =
            RecordingProvider::new(responses, vec![0; FINAL_RESPONSE_WARNING_ITERATION + 2]);
        let state = build_state_with_provider(
            dir.path().to_str().expect("utf8").to_string(),
            Box::new(provider.clone()),
        );
        let context = cli_context("late-tool-loop");

        // Act
        let reply = process_turn(
            &state.turn_dependencies(),
            &context,
            "inspect the workspace",
        )
        .await
        .expect("late tool loop should finalize");

        // Assert: the final response is returned at the hard-cap boundary.
        assert_eq!(reply, "The available results are complete.");

        let seen_messages = provider.seen_messages();
        let warning_message = seen_messages[FINAL_RESPONSE_WARNING_ITERATION - 1]
            .last()
            .expect("warning guard message");
        assert!(
            warning_message
                .content
                .as_text_lossy()
                .contains(FINAL_RESPONSE_WARNING_GUARD)
        );
        let final_guard_message = seen_messages[FINAL_RESPONSE_WARNING_ITERATION + 1]
            .last()
            .expect("final guard message");
        assert!(
            final_guard_message
                .content
                .as_text_lossy()
                .contains(FINAL_RESPONSE_GUARD)
        );

        let chat_id = call_blocking(Arc::clone(&state.db), move |db| {
            db.resolve_or_create_chat_id(
                "cli",
                "cli:late-tool-loop:agent:default",
                Some("late-tool-loop"),
                "cli",
                "default",
            )
        })
        .await
        .expect("chat id");
        let loaded = crate::agent_loop::session::load_messages_for_turn(
            &state.turn_dependencies(),
            ConversationScope::Normal,
            chat_id,
        )
        .await
        .expect("session");
        assert!(loaded.messages.iter().all(|message| {
            let text = message.content.as_text_lossy();
            !text.contains(FINAL_RESPONSE_WARNING_GUARD) && !text.contains(FINAL_RESPONSE_GUARD)
        }));
    }

    #[tokio::test]
    #[serial]
    async fn tool_phase_followups_commit_after_all_tool_results_in_fifo_order() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let provider = RecordingProvider::new(
            vec![
                Ok(MessagesResponse {
                    content: "running checks".to_string(),
                    reasoning_content: None,
                    tool_calls: vec![
                        ToolCall {
                            id: "wait-a".to_string(),
                            name: "wait_test".to_string(),
                            arguments: serde_json::json!({"name": "a"}),
                        },
                        ToolCall {
                            id: "wait-b".to_string(),
                            name: "wait_test".to_string(),
                            arguments: serde_json::json!({"name": "b"}),
                        },
                    ],
                    usage: None,
                }),
                Ok(MessagesResponse {
                    content: "done with follow-ups".to_string(),
                    reasoning_content: None,
                    tool_calls: Vec::new(),
                    usage: None,
                }),
            ],
            vec![0, 0],
        );
        let started = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let started_notify = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let mut state = build_state_with_provider(
            dir.path().to_str().expect("utf8").to_string(),
            Box::new(provider.clone()),
        );
        Arc::get_mut(&mut state.tools)
            .expect("test state owns tool registry")
            .register_tool(Box::new(BlockingTool {
                started: Arc::clone(&started),
                started_notify: Arc::clone(&started_notify),
                release: Arc::clone(&release),
            }));
        let state = Arc::new(state);
        let context = cli_context("follow-up-integration");
        let events = Arc::new(std::sync::Mutex::new(Vec::<AgentEvent>::new()));
        let event_log = Arc::clone(&events);
        let runtime = state.turn_dependencies();
        let turn_context = context.clone();
        let turn = tokio::spawn(async move {
            crate::agent_loop::process_turn_with_events(
                &runtime,
                &turn_context,
                "inspect everything",
                move |event| event_log.lock().expect("events").push(event),
            )
            .await
        });

        // Act: wait until the durable Turn has entered its Tool phase, then
        // stage two messages from different human senders before releasing the
        // two Tool calls.
        started_notify.notified().await;
        let mut follow_up_b = context.clone();
        follow_up_b.surface_user = "user-b".to_string();
        follow_up_b.request_key = "cli:follow-up-b".to_string();
        let mut follow_up_c = context.clone();
        follow_up_c.surface_user = "user-c".to_string();
        follow_up_c.request_key = "cli:follow-up-c".to_string();
        assert_eq!(
            crate::runtime::try_stage_tool_followup(
                &state,
                follow_up_b,
                "follow-up B".to_string(),
            )
            .await
            .expect("stage B"),
            crate::runtime::ToolFollowupOutcome::Accepted
        );
        assert_eq!(
            crate::runtime::try_stage_tool_followup(
                &state,
                follow_up_c,
                "follow-up C".to_string(),
            )
            .await
            .expect("stage C"),
            crate::runtime::ToolFollowupOutcome::Accepted
        );
        release.notify_waiters();
        while started.load(std::sync::atomic::Ordering::SeqCst) < 2 {
            started_notify.notified().await;
        }
        release.notify_waiters();
        let reply = turn.await.expect("turn task").expect("turn result");

        // Assert
        assert_eq!(reply, "done with follow-ups");
        let (last_tool_result, first_injected, injected) = {
            let event_log = events.lock().expect("events");
            let last_tool_result = event_log
                .iter()
                .enumerate()
                .filter_map(|(index, event)| {
                    matches!(event, AgentEvent::ToolResult { .. }).then_some(index)
                })
                .max()
                .expect("Tool results emitted");
            let first_injected = event_log
                .iter()
                .enumerate()
                .find_map(|(index, event)| {
                    matches!(event, AgentEvent::UserInputInjected { .. }).then_some(index)
                })
                .expect("follow-up event emitted");
            let injected: Vec<(String, String)> = event_log
                .iter()
                .filter_map(|event| match event {
                    AgentEvent::UserInputInjected {
                        sender_id, text, ..
                    } => Some((sender_id.clone(), text.clone())),
                    _ => None,
                })
                .collect();
            (last_tool_result, first_injected, injected)
        };
        assert!(last_tool_result < first_injected);
        assert_eq!(
            injected,
            vec![
                ("user-b".to_string(), "follow-up B".to_string()),
                ("user-c".to_string(), "follow-up C".to_string()),
            ]
        );

        let chat_id = call_blocking(Arc::clone(&state.db), |db| {
            db.resolve_or_create_chat_id(
                "cli",
                "cli:follow-up-integration:agent:default",
                Some("follow-up-integration"),
                "cli",
                "default",
            )
        })
        .await
        .expect("chat id");
        let history = call_blocking(Arc::clone(&state.db), move |db| {
            db.get_all_messages(chat_id)
        })
        .await
        .expect("history");
        let followups: Vec<_> = history
            .iter()
            .filter(|message| message.content.starts_with("follow-up"))
            .collect();
        assert_eq!(followups.len(), 2);
        assert_eq!(followups[0].content, "follow-up B");
        assert_eq!(followups[0].sender_id, "user-b");
        assert_eq!(followups[1].content, "follow-up C");
        assert_eq!(followups[1].sender_id, "user-c");
        assert!(followups.iter().all(|message| message.seq.is_some()));
        assert!(
            followups.iter().all(|message| {
                chrono::DateTime::parse_from_rfc3339(&message.timestamp).is_ok()
            })
        );

        let snapshot = call_blocking(Arc::clone(&state.db), move |db| {
            db.load_session_snapshot(chat_id, 50)
        })
        .await
        .expect("session snapshot");
        let snapshot_json = snapshot.messages_json.expect("snapshot exists");
        assert!(snapshot_json.contains("follow-up B"));
        assert!(snapshot_json.contains("follow-up C"));
        assert!(
            call_blocking(Arc::clone(&state.db), move |db| {
                let conn = db.get_conn()?;
                let turn_id: String = conn.query_row(
                    "SELECT turn_id FROM turn_runs WHERE chat_id = ?1",
                    rusqlite::params![chat_id],
                    |row| row.get(0),
                )?;
                Ok::<bool, crate::error::StorageError>(
                    db.list_staged_user_messages(&turn_id)?.is_empty(),
                )
            })
            .await
            .expect("staged rows")
        );
    }

    #[test]
    fn messages_for_iteration_adds_warning_guards_only_in_final_window() {
        // Arrange
        let messages = Arc::new(vec![Message::text("user", "inspect")]);

        // Act
        let before_warning =
            messages_for_iteration(&messages, FINAL_RESPONSE_WARNING_ITERATION - 1);
        let warning = messages_for_iteration(&messages, FINAL_RESPONSE_WARNING_ITERATION);
        let final_window = messages_for_iteration(&messages, MAX_TOOL_ITERATIONS);

        // Assert
        assert_eq!(before_warning.len(), 1);
        assert!(
            warning
                .last()
                .expect("warning message")
                .content
                .as_text_lossy()
                .contains(FINAL_RESPONSE_WARNING_GUARD)
        );
        assert!(
            final_window
                .last()
                .expect("final guard message")
                .content
                .as_text_lossy()
                .contains(FINAL_RESPONSE_GUARD)
        );
        assert_eq!(
            messages.len(),
            1,
            "request guards must not mutate loop state"
        );
    }

    #[test]
    fn evaluate_end_turn_retries_declarative_output_once() {
        // Arrange
        let messages = vec![Message::text("user", "do the work")];
        let mut retry_attempted = false;

        // Act
        let action = evaluate_end_turn(
            "I will inspect the files.",
            None,
            &mut retry_attempted,
            &messages,
        )
        .expect("declarative response should be recoverable");

        // Assert
        assert!(retry_attempted);
        match action {
            LoopAction::Retry(retry_messages) => {
                assert!(retry_messages.len() > messages.len());
            }
            LoopAction::Done { .. } => {
                panic!("declarative response should inject a corrective retry")
            }
        }
    }
}
