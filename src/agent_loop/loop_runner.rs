//! Agent Loop policy and model/tool iteration state.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tracing::warn;

use crate::agent_loop::TurnDependencies;
use crate::agent_loop::compaction::{
    CompactionOutcome, CompactionReason, CompactionRequest, MeasuredUsage, PromptContext,
    RequestMeasurement, compact, compact_if_needed, estimate_request_overhead, model_key,
    request_shape_hash,
};
use crate::agent_loop::event::{AgentEvent, EventEmitter};
use crate::agent_loop::message_format::strip_thinking;
use crate::agent_loop::model_step::{
    ModelRunner, ModelStep, ModelStepRequest, build_assistant_tool_phase,
};
use crate::agent_loop::response_guard::{is_declarative_only_reply, runtime_guard_suffix};
use crate::agent_loop::session_snapshot::{SessionContext, checkpoint_message};
use crate::agent_loop::tool_execution::{
    ExecutedToolCall, GuardDenialTracker, MAX_TOOL_RESULT_TEXT_CHARS, ToolExecutionHooks,
    ToolExecutor, build_tool_result_phase,
};
use crate::agent_loop::turn::PreparedTurn;
use crate::agent_loop::turn::lifecycle::TurnLifecycle;
use crate::agent_loop::turn::persistence::TurnPersistence;
use crate::channels::utils::text::truncate_by_chars;
use crate::conversation::SurfaceContext;
use crate::error::{EgoPulseError, LlmError};
use crate::llm::{Message, ToolCall, ToolDefinition, TruncationReason};

/// Maximum number of model/tool iterations allowed for one activation.
pub(crate) const MAX_TOOL_ITERATIONS: usize = 50;

/// Reason recorded on a Tool Call that the output limit cut before completion.
const TRUNCATED_TOOL_CALL_REASON: &str =
    "the output limit truncated the tool arguments before they were complete";

/// The iteration at which the model is warned that the hard loop limit is near.
pub(crate) const FINAL_RESPONSE_WARNING_ITERATION: usize = MAX_TOOL_ITERATIONS - 2;

/// Runtime guard appended to the iteration-48 request to warn the model that
/// the tool loop is nearing its hard limit and begin final-response preparation.
pub(crate) const FINAL_RESPONSE_WARNING_GUARD: &str = "[runtime_guard]: The tool loop is near its hard limit. Two model iterations remain after this one. Do not start broad new work; prepare the best concise answer for the user now and state any uncertainty. If this is a Pulse activation and you have used tools, summarize the result instead of returning PULSE_OK.";

/// Runtime guard appended to requests in the final-response window to
/// prioritize a concise final response over starting new broad tool work.
pub(crate) const FINAL_RESPONSE_GUARD: &str = "[runtime_guard]: The tool loop is at its final response window. Provide the best concise answer to the user now. Do not start broad new work; state what was completed and any uncertainty. If this is a Pulse activation and you have used tools, summarize the result instead of returning PULSE_OK.";

/// Runtime guard appended to requests after the security-guard denial limit is
/// reached. Tells the model to stop working around the denials and report.
pub(crate) const GUARD_DENIAL_WRAP_GUARD: &str = "[runtime_guard]: The security-guard denial limit has been reached for this run. Treat the denials as final and stop attempting workarounds or further tool work. Wrap up now: report to the user what you were trying to do, what was blocked, and any results obtained, then ask how they want to proceed. If this is a Pulse activation, summarize the outcome instead of returning PULSE_OK.";

/// Mutable conversation state owned by one Agent Loop.
pub(crate) struct LoopState {
    pub(crate) context: SessionContext,
    pub(crate) session_revision: Option<i64>,
    /// Corrections appended to the next request only, such as the discarded
    /// declarative reply and its runtime guard. They are never persisted, and
    /// because they live outside the conversation they survive compaction of
    /// the conversation itself.
    retry_suffix: Option<Vec<Message>>,
    declarative_retry_attempted: bool,
    /// Set when a successful response reported a prompt larger than the
    /// context window. The next model request compacts even if the estimated
    /// projection would fit.
    force_compaction: bool,
}

impl LoopState {
    pub(crate) fn new(context: SessionContext, session_revision: Option<i64>) -> Self {
        Self {
            context,
            session_revision,
            retry_suffix: None,
            declarative_retry_attempted: false,
            force_compaction: false,
        }
    }

    fn clear_retry_state(&mut self) {
        self.retry_suffix = None;
        self.declarative_retry_attempted = false;
    }
}

/// Result returned when the Agent Loop has produced a final response.
/// `final_message_id` is the stable id the response streamed under; the
/// caller persists the final row under exactly this id.
pub(crate) struct AgentLoopResult {
    pub(crate) final_content: String,
    pub(crate) reasoning_content: Option<String>,
    pub(crate) final_message_id: String,
    pub(crate) context: SessionContext,
    pub(crate) session_revision: Option<i64>,
    /// Why the response ended before the model chose to stop.
    pub(crate) finish_reason: Option<TruncationReason>,
    /// Usage of the request that produced the final response.
    pub(crate) usage: Option<MeasuredUsage>,
}

/// Inputs of one truncation recovery decision.
struct TruncationRecovery<'a> {
    usage: Option<&'a crate::llm::LlmUsage>,
    recovery_attempted: bool,
}

/// The final answer fields shared by normal completion and truncation.
struct FinalizedAnswer {
    message_id: String,
    content: String,
    reasoning_content: Option<String>,
    finish_reason: Option<TruncationReason>,
    usage: Option<crate::llm::LlmUsage>,
}

/// Executes the LLM/tool loop for one durable Turn.
pub(crate) struct AgentLoop<'a> {
    state: &'a TurnDependencies,
    context: &'a SurfaceContext,
    prepared: &'a PreparedTurn,
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
            channel_context_msg,
            on_event,
            lifecycle,
            persistence,
        }
    }

    /// Runs model steps, tool execution, and loop-level compaction.
    pub(crate) async fn run(
        &self,
        context: SessionContext,
        session_revision: Option<i64>,
        start_iteration: usize,
    ) -> Result<AgentLoopResult, EgoPulseError> {
        let mut loop_state = LoopState::new(context, session_revision);
        let guard_tracker = Arc::new(GuardDenialTracker::new());
        let context_window = self
            .prepared
            .config_snapshot
            .config
            .resolve_model_metadata(
                &crate::config::ProviderId::new(self.prepared.channel_llm.provider_name()),
                self.prepared.channel_llm.model_name(),
            )
            .context_window;
        let mut iteration = start_iteration;
        // A truncated tool phase is converted into failure results once; a
        // second one ends the turn instead of feeding another unusable call
        // back to the model.
        let mut truncated_tool_phase_continued = false;

        'turns: while iteration <= MAX_TOOL_ITERATIONS {
            self.on_event.emit(AgentEvent::Iteration { iteration });
            // The assistant message id is fixed before the model call, so
            // the streamed deltas, a tool preview, and the final row all
            // share one identity from the start.
            let message_id =
                crate::agent_loop::turn::assistant_message_id(&self.prepared.turn_id, iteration);

            // Tools and request-only messages are fixed for the whole
            // iteration, so the compaction decision, the request itself, and
            // the usage anchor all describe exactly the same request.
            let tools = apply_guard_denial_state(&self.prepared.tool_defs, &guard_tracker);
            let tools_json = if tools.is_empty() {
                None
            } else {
                serde_json::to_string(tools.as_ref()).ok()
            };
            let prompt = PromptContext {
                system_prompt: &self.prepared.system_prompt,
                tools_json: tools_json.as_deref(),
                has_tools: !tools.is_empty(),
            };
            let scope = self.request_scope(&loop_state, iteration, &guard_tracker);
            self.compact_before_model(&mut loop_state, &scope, &prompt, context_window)
                .await?;

            let model_key = model_key(self.prepared.channel_llm.as_ref());
            let mut overflow_recovery_attempted = false;
            let output_published = Arc::new(AtomicBool::new(false));
            let (phase_response, measurement) = loop {
                let shape_hash = request_shape_hash(
                    &self.prepared.system_prompt,
                    tools_json.as_deref(),
                    &model_key,
                );
                let request_messages = request_messages(&loop_state.context, &scope);
                let event_emitter = self.on_event.clone();
                let emitted_id = message_id.clone();
                output_published.store(false, Ordering::SeqCst);
                let published_flag = Arc::clone(&output_published);
                let on_delta = move |text: String| {
                    published_flag.store(true, Ordering::SeqCst);
                    event_emitter.emit(AgentEvent::Delta {
                        message_id: emitted_id.clone(),
                        text,
                    });
                };
                let model_runner = ModelRunner::new(ModelStepRequest {
                    state: self.state,
                    llm: self.prepared.channel_llm.as_ref(),
                    system_prompt: &self.prepared.system_prompt,
                    messages: Arc::clone(&request_messages),
                    tools: Some(Arc::clone(&tools)),
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
                let attempt = if overflow_recovery_attempted {
                    model_runner
                        .retry_after_compaction(&self.prepared.turn_id)
                        .await
                } else {
                    model_runner.run_with_retry(&self.prepared.turn_id).await
                };
                drop(request_messages);

                match attempt {
                    Ok(outcome) => {
                        // The usage describes the request that was actually
                        // sent: any message an internal recovery appended to it
                        // belongs to the measured overhead, not to the
                        // conversation.
                        let mut request_only = scope.request_only();
                        request_only.extend(outcome.recovery_suffix.iter());
                        let request_overhead_tokens =
                            estimate_request_overhead(&prompt, &loop_state.context, &request_only);
                        let measurement = RequestMeasurement {
                            model_key: model_key.clone(),
                            request_shape_hash: shape_hash.clone(),
                            request_overhead_tokens,
                        };
                        if let ModelStep::Truncated { response, reason } = &outcome.step {
                            let published = output_published.load(Ordering::SeqCst);
                            if published {
                                // The partial output cannot be withdrawn from a
                                // crash-recovery perspective; record that it was
                                // visible before deciding what to do with it.
                                self.lifecycle.mark_output_published().await;
                            }
                            let recovered = self
                                .recover_truncated_response(
                                    &mut loop_state,
                                    &scope,
                                    &prompt,
                                    TruncationRecovery {
                                        usage: response.usage.as_ref(),
                                        recovery_attempted: overflow_recovery_attempted,
                                    },
                                )
                                .await?;
                            if recovered {
                                overflow_recovery_attempted = true;
                                if published {
                                    // The retry regenerates the answer under
                                    // the same message id.
                                    self.on_event.emit(AgentEvent::AssistantMessageDiscarded {
                                        message_id: message_id.clone(),
                                    });
                                }
                                continue;
                            }

                            let partial_text =
                                strip_thinking(response.content.trim()).trim().to_string();
                            if response.tool_calls.is_empty() || truncated_tool_phase_continued {
                                // Keep the generated text even when recovery is
                                // impossible, and complete the Turn with the
                                // truncation recorded on the message. A second
                                // unusable tool call is not fed back either.
                                if partial_text.is_empty() {
                                    let detail = if response.tool_calls.is_empty() {
                                        "model output was truncated before any text was generated"
                                    } else {
                                        "model output was truncated inside a tool call"
                                    };
                                    return Err(EgoPulseError::Llm(LlmError::IncompleteResponse(
                                        detail.to_string(),
                                    )));
                                }
                                self.lifecycle.complete_model().await?;
                                let silent_overflow =
                                    response.usage.as_ref().is_some_and(|usage| {
                                        usage.input_tokens > context_window as i64
                                    });
                                return self
                                    .finalize_response(
                                        loop_state,
                                        &scope,
                                        &prompt,
                                        silent_overflow,
                                        FinalizedAnswer {
                                            message_id,
                                            content: partial_text,
                                            reasoning_content: response.reasoning_content.clone(),
                                            finish_reason: Some(*reason),
                                            usage: response.usage.clone(),
                                        },
                                        measurement,
                                    )
                                    .await;
                            }

                            // The call was cut inside its arguments. The normal
                            // Tool executor reports the invalid arguments as a
                            // failure without running the Tool, so the model
                            // receives the call/result pair on the next step.
                            truncated_tool_phase_continued = true;
                            let mut tool_calls = response.tool_calls.clone();
                            for tool_call in &mut tool_calls {
                                tool_call.invalid_arguments =
                                    Some(TRUNCATED_TOOL_CALL_REASON.to_string());
                            }
                            let mut phase = build_assistant_tool_phase(
                                response.content.clone(),
                                response.reasoning_content.clone(),
                                tool_calls,
                                response.usage.clone(),
                            );
                            phase.assistant_message.finish_reason = Some(*reason);
                            self.execute_tool_phase(
                                &mut loop_state,
                                phase,
                                &message_id,
                                &guard_tracker,
                                &measurement,
                            )
                            .await?;
                            loop_state.force_compaction = response
                                .usage
                                .as_ref()
                                .is_some_and(|usage| usage.input_tokens > context_window as i64);
                            loop_state.clear_retry_state();
                            iteration += 1;
                            continue 'turns;
                        }
                        break (outcome.step, measurement);
                    }
                    Err(error) => {
                        let (error, output_published) = error.into_parts();
                        if output_published {
                            self.lifecycle.mark_output_published().await;
                        }
                        // A context-overflow rejection is the only failure that
                        // may be retried with a smaller request. Published
                        // output cannot be taken back, and a second overflow
                        // after compaction must surface instead of looping.
                        let recovered = if !overflow_recovery_attempted
                            && !output_published
                            && (error.is_context_overflow() || error.is_output_capacity_exceeded())
                        {
                            self.compact_overflow(&mut loop_state, &scope, &prompt)
                                .await?
                        } else {
                            false
                        };
                        if recovered {
                            overflow_recovery_attempted = true;
                            warn!(
                                iteration,
                                "context overflow or output capacity recovery: compacted and retrying the model step once"
                            );
                            continue;
                        }
                        return Err(error);
                    }
                }
            };

            let usage = phase_response.usage().cloned();
            // `input_tokens` already includes cached prompt tokens for
            // OpenAI-compatible providers, so only the reported prompt size is
            // compared with the window.
            let silent_overflow = usage
                .as_ref()
                .is_some_and(|usage| usage.input_tokens > context_window as i64);
            match phase_response {
                step @ (ModelStep::Final(_) | ModelStep::MalformedToolCalls(_)) => {
                    self.lifecycle.complete_model().await?;
                    let action = match step {
                        ModelStep::Final(response) => evaluate_end_turn(
                            &response.content,
                            response.reasoning_content.as_deref(),
                            &mut loop_state.declarative_retry_attempted,
                        )?,
                        ModelStep::MalformedToolCalls(response) => evaluate_malformed_response(
                            &response.content,
                            response.reasoning_content.as_deref(),
                            &mut loop_state.declarative_retry_attempted,
                        )?,
                        _ => unreachable!(),
                    };
                    match action {
                        LoopAction::Retry(suffix) => {
                            if output_published.load(Ordering::SeqCst) {
                                self.on_event.emit(AgentEvent::AssistantMessageDiscarded {
                                    message_id: message_id.clone(),
                                });
                            }
                            loop_state.retry_suffix = Some(suffix);
                        }
                        LoopAction::Done {
                            final_content,
                            reasoning_content,
                        } => {
                            return self
                                .finalize_response(
                                    loop_state,
                                    &scope,
                                    &prompt,
                                    silent_overflow,
                                    FinalizedAnswer {
                                        message_id,
                                        content: final_content,
                                        reasoning_content,
                                        finish_reason: None,
                                        usage: usage.clone(),
                                    },
                                    measurement,
                                )
                                .await;
                        }
                    }
                    iteration += 1;
                    continue;
                }
                ModelStep::Truncated { .. } => {
                    // Truncation is decided in the model-step loop above;
                    // reaching this point would mean executing a partial answer.
                    return Err(EgoPulseError::Llm(
                        crate::error::LlmError::IncompleteResponse(
                            "model output was truncated".to_string(),
                        ),
                    ));
                }
                ModelStep::ToolCalls(assistant_phase) => {
                    self.execute_tool_phase(
                        &mut loop_state,
                        assistant_phase,
                        &message_id,
                        &guard_tracker,
                        &measurement,
                    )
                    .await?;
                    loop_state.force_compaction = silent_overflow;
                }
            }

            loop_state.clear_retry_state();
            iteration += 1;
        }

        Err(EgoPulseError::Internal(format!(
            "tool loop exceeded max iterations ({MAX_TOOL_ITERATIONS})"
        )))
    }

    /// Builds the messages that belong to the upcoming request but not to the
    /// persisted conversation.
    fn request_scope(
        &self,
        loop_state: &LoopState,
        iteration: usize,
        guard_tracker: &GuardDenialTracker,
    ) -> RequestScope {
        let mut suffix = Vec::new();
        if let Some(retry) = &loop_state.retry_suffix {
            suffix.extend(retry.iter().cloned());
        }
        if let Some(guard) = iteration_guard(iteration) {
            suffix.push(Message::text("user", guard));
        }
        if guard_tracker.limit_reached() {
            suffix.push(Message::text("user", GUARD_DENIAL_WRAP_GUARD));
        }
        RequestScope {
            prefix: if iteration == 1 {
                self.channel_context_msg.clone()
            } else {
                None
            },
            suffix,
        }
    }

    /// Whether automatic compaction and recovery are allowed.
    ///
    /// Every automatic path asks this one question; a per-path check would
    /// let one recovery route ignore the setting.
    fn automatic_compaction_enabled(&self) -> bool {
        self.prepared.config_snapshot.config.compaction.enabled
    }

    /// Builds the request-scoped context shared by the compaction entry points.
    fn compaction_request<'s>(
        &'s self,
        scope: &'s RequestScope,
        prompt: PromptContext<'s>,
    ) -> CompactionRequest<'s>
    where
        'a: 's,
    {
        CompactionRequest {
            state: self.state,
            context: self.context,
            chat_id: self.prepared.chat_id,
            llm: &self.prepared.channel_llm,
            prompt,
            config: &self.prepared.config_snapshot.config,
            request_only_messages: scope.request_only(),
        }
    }

    /// Compacts the loop context before a model request when it is too large.
    ///
    /// A summarization failure must not end the turn: the request may still
    /// fit, and a real overflow is recovered by [`Self::compact_overflow`].
    /// A compaction that succeeded is persisted before the request is built,
    /// so the checkpoint is never only in memory.
    async fn compact_before_model(
        &self,
        loop_state: &mut LoopState,
        scope: &RequestScope,
        prompt: &PromptContext<'_>,
        context_window: usize,
    ) -> Result<(), EgoPulseError> {
        let request = self.compaction_request(scope, *prompt);
        // A successful response whose measured prompt already exceeded the
        // window compacts even when the estimate would fit, matching the
        // provider's own accounting instead of waiting for a rejection.
        let forced = std::mem::take(&mut loop_state.force_compaction)
            && self.automatic_compaction_enabled()
            && context_window > 0;
        let outcome = if forced {
            compact(
                &request,
                &loop_state.context,
                CompactionReason::Overflow,
                None,
            )
            .await
        } else {
            compact_if_needed(&request, &loop_state.context, None).await
        };
        match outcome {
            Ok(CompactionOutcome::Compacted(context)) => {
                self.commit_compacted_context(loop_state, context).await
            }
            Ok(CompactionOutcome::Unchanged) => Ok(()),
            Err(error) => {
                warn!(
                    turn_id = %self.prepared.turn_id,
                    error = %error,
                    "context compaction failed; continuing with the uncompacted context"
                );
                Ok(())
            }
        }
    }

    /// Compacts the context after a successful response that already exceeded
    /// the window.
    ///
    /// The answer is final, so it is never regenerated: only the stored
    /// context is reduced for the next request. A summarization or storage
    /// failure is logged and the original context is kept; the answer stands.
    ///
    /// Returns `true` when the context was replaced. The usage measurement of
    /// the response then describes the previous conversation and must not
    /// become the next anchor.
    async fn compact_after_response(
        &self,
        loop_state: &mut LoopState,
        scope: &RequestScope,
        prompt: &PromptContext<'_>,
        silent_overflow: bool,
    ) -> bool {
        if !silent_overflow || !self.automatic_compaction_enabled() {
            return false;
        }
        match compact(
            &self.compaction_request(scope, *prompt),
            &loop_state.context,
            CompactionReason::Overflow,
            None,
        )
        .await
        {
            Ok(CompactionOutcome::Compacted(context)) => {
                if let Err(error) = self.commit_compacted_context(loop_state, context).await {
                    warn!(
                        turn_id = %self.prepared.turn_id,
                        error = %error,
                        "compacted context could not be saved; returning the response with the original context"
                    );
                    return false;
                }
                true
            }
            Ok(CompactionOutcome::Unchanged) => false,
            Err(error) => {
                warn!(
                    turn_id = %self.prepared.turn_id,
                    error = %error,
                    "silent context overflow compaction failed; returning the response"
                );
                false
            }
        }
    }

    /// Completes the Turn with a final answer and its measured usage.
    ///
    /// The answer is already final, so a silent overflow only compacts the
    /// stored context for the next request. Usage is dropped when compaction
    /// replaced the measured conversation.
    async fn finalize_response(
        &self,
        mut loop_state: LoopState,
        scope: &RequestScope,
        prompt: &PromptContext<'_>,
        silent_overflow: bool,
        answer: FinalizedAnswer,
        measurement: RequestMeasurement,
    ) -> Result<AgentLoopResult, EgoPulseError> {
        let FinalizedAnswer {
            message_id,
            content,
            reasoning_content,
            finish_reason,
            usage,
        } = answer;
        let compacted = self
            .compact_after_response(&mut loop_state, scope, prompt, silent_overflow)
            .await;
        Ok(AgentLoopResult {
            final_content: content,
            reasoning_content,
            final_message_id: message_id,
            context: loop_state.context,
            session_revision: loop_state.session_revision,
            finish_reason,
            usage: (!compacted)
                .then(|| {
                    usage.map(|usage| MeasuredUsage {
                        usage,
                        request: measurement,
                    })
                })
                .flatten(),
        })
    }

    /// Compacts the context after the provider reported a context overflow.
    ///
    /// Returns `true` when the compacted context was saved and the model step
    /// may be sent again. The same model iteration is retried; the Turn itself
    /// is not restarted.
    async fn compact_overflow(
        &self,
        loop_state: &mut LoopState,
        scope: &RequestScope,
        prompt: &PromptContext<'_>,
    ) -> Result<bool, EgoPulseError> {
        // `enabled=false` disables every automatic recovery path. Manual
        // `/compact` keeps working because it does not pass through here.
        if !self.automatic_compaction_enabled() {
            return Ok(false);
        }
        let outcome = compact(
            &self.compaction_request(scope, *prompt),
            &loop_state.context,
            CompactionReason::Overflow,
            None,
        )
        .await;
        match outcome {
            Err(error) => {
                warn!(error = %error, "context overflow compaction failed");
                Ok(false)
            }
            Ok(CompactionOutcome::Unchanged) => Ok(false),
            Ok(CompactionOutcome::Compacted(context)) => {
                self.commit_compacted_context(loop_state, context).await?;
                Ok(true)
            }
        }
    }

    /// Attempts the single compaction-and-retry allowed for a truncated
    /// generation.
    ///
    /// A `length` stop is only recoverable when the provider reported usage and
    /// the generation ended before the intended output limit. Reaching the
    /// intended limit is not evidence that compacting will produce a complete
    /// answer, so those cases surface the error. A tool-call response is never
    /// executed: the partial call is discarded with the response.
    ///
    /// Returns `true` when the caller must resend the same model iteration.
    async fn recover_truncated_response(
        &self,
        loop_state: &mut LoopState,
        scope: &RequestScope,
        prompt: &PromptContext<'_>,
        recovery: TruncationRecovery<'_>,
    ) -> Result<bool, EgoPulseError> {
        let enabled = self.automatic_compaction_enabled();
        let TruncationRecovery {
            usage,
            recovery_attempted,
        } = recovery;
        let Some(usage) = usage else {
            return Ok(false);
        };
        let intended_max_output = self
            .prepared
            .config_snapshot
            .config
            .resolve_model_metadata(
                &crate::config::ProviderId::new(self.prepared.channel_llm.provider_name()),
                self.prepared.channel_llm.model_name(),
            )
            .max_output_tokens;
        let early_stop =
            intended_max_output > 0 && usage.output_tokens < intended_max_output as i64;
        if !enabled || recovery_attempted || !early_stop {
            return Ok(false);
        }

        let recovered = self.compact_overflow(loop_state, scope, prompt).await?;
        if recovered {
            warn!(
                turn_id = %self.prepared.turn_id,
                output_tokens = usage.output_tokens,
                intended_max_output_tokens = intended_max_output,
                "truncated generation recovery: compacted and retrying the model step once"
            );
        }
        Ok(recovered)
    }

    /// Persists a compacted context as the session snapshot.
    ///
    /// The model request must not be built from a context that is not durable:
    /// a crash would otherwise lose the checkpoint and re-summarize the same
    /// history. A concurrent writer keeps its own snapshot and this Turn ends
    /// as a concurrency conflict instead of overwriting it.
    async fn commit_compacted_context(
        &self,
        loop_state: &mut LoopState,
        context: SessionContext,
    ) -> Result<(), EgoPulseError> {
        let revision = crate::agent_loop::session::persist_compacted_context(
            self.state,
            self.context.scope,
            self.prepared.chat_id,
            context.clone(),
            loop_state.session_revision,
        )
        .await?;
        let Some(revision) = revision else {
            return Err(EgoPulseError::TurnConcurrencyConflict);
        };
        loop_state.context = context;
        loop_state.session_revision = Some(revision);
        Ok(())
    }

    async fn execute_tool_phase(
        &self,
        loop_state: &mut LoopState,
        assistant_phase: crate::agent_loop::model_step::AssistantToolPhase,
        assistant_message_id: &str,
        guard_tracker: &Arc<GuardDenialTracker>,
        measurement: &RequestMeasurement,
    ) -> Result<(), EgoPulseError> {
        self.lifecycle.complete_model().await?;
        self.lifecycle.begin_tools().await?;
        self.lifecycle.mark_output_published().await;

        let persisted = self
            .persistence
            .persist_tool_call(
                assistant_message_id,
                &assistant_phase,
                loop_state.context.clone(),
                measurement,
                loop_state.session_revision,
            )
            .await?;
        let tool_outcomes = self
            .execute_tools(
                assistant_message_id,
                assistant_phase.tool_calls,
                guard_tracker,
            )
            .await?;
        let persisted = self
            .persistence
            .persist_tool_results(
                assistant_message_id,
                persisted.context,
                build_tool_result_phase(tool_outcomes),
                Some(persisted.revision),
            )
            .await?;
        loop_state.context = persisted.context;
        loop_state.session_revision = Some(persisted.revision);
        self.lifecycle.complete_tools().await?;
        let persisted = self
            .persistence
            .commit_staged_user_messages(
                loop_state.context.clone(),
                loop_state.session_revision,
                &self.prepared.config_snapshot,
                &self.on_event,
            )
            .await?;
        loop_state.context = persisted.context;
        loop_state.session_revision = Some(persisted.revision);
        Ok(())
    }

    async fn execute_tools(
        &self,
        assistant_message_id: &str,
        tool_calls: Vec<ToolCall>,
        guard_tracker: &Arc<GuardDenialTracker>,
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
                    attachments: outcome.result.attachments.clone(),
                });
            })),
        };

        ToolExecutor::new(self.state, &self.prepared.tool_context, hooks)
            .with_guard_denial_tracker(Arc::clone(guard_tracker))
            .execute(assistant_message_id, tool_calls)
            .await
    }
}

/// Messages that belong to one model request but not to the conversation.
///
/// They are fixed for every attempt of an iteration, so compaction decisions
/// and the request itself always describe the same context.
struct RequestScope {
    /// Channel context prepended before the checkpoint.
    prefix: Option<Message>,
    /// Retry corrections and runtime guards appended after the conversation.
    suffix: Vec<Message>,
}

impl RequestScope {
    /// Returns every request-only message, in request order.
    fn request_only(&self) -> Vec<&Message> {
        self.prefix.iter().chain(self.suffix.iter()).collect()
    }
}

/// Builds the request messages from the conversation and its request scope.
fn request_messages(context: &SessionContext, scope: &RequestScope) -> Arc<Vec<Message>> {
    let mut messages = Vec::with_capacity(
        context.messages.len() + scope.prefix.iter().count() + scope.suffix.len() + 1,
    );
    messages.extend(scope.prefix.iter().cloned());
    if let Some(checkpoint) = &context.checkpoint {
        messages.push(checkpoint_message(checkpoint));
    }
    messages.extend(context.messages.iter().cloned());
    messages.extend(scope.suffix.iter().cloned());
    Arc::new(messages)
}

/// Applies the same post-denial stop state to Turn and Pulse model requests.
pub(crate) fn apply_guard_denial_state(
    tool_defs: &Arc<Vec<ToolDefinition>>,
    tracker: &GuardDenialTracker,
) -> Arc<Vec<ToolDefinition>> {
    if tracker.limit_reached() {
        Arc::new(Vec::new())
    } else {
        Arc::clone(tool_defs)
    }
}

pub(crate) fn append_guard_denial_wrap(
    messages: Arc<Vec<Message>>,
    tracker: &GuardDenialTracker,
) -> Arc<Vec<Message>> {
    if !tracker.limit_reached() {
        return messages;
    }
    let mut messages = (*messages).clone();
    messages.push(Message::text("user", GUARD_DENIAL_WRAP_GUARD));
    Arc::new(messages)
}

/// Returns the runtime guard that applies to one iteration, if any.
fn iteration_guard(iteration: usize) -> Option<&'static str> {
    match iteration {
        FINAL_RESPONSE_WARNING_ITERATION => Some(FINAL_RESPONSE_WARNING_GUARD),
        iteration if iteration > FINAL_RESPONSE_WARNING_ITERATION => Some(FINAL_RESPONSE_GUARD),
        _ => None,
    }
}

/// Adds the iteration-specific runtime guard to a request-only copy of `messages`.
pub(crate) fn messages_for_iteration(
    messages: &Arc<Vec<Message>>,
    iteration: usize,
) -> Arc<Vec<Message>> {
    let Some(guard) = iteration_guard(iteration) else {
        return Arc::clone(messages);
    };
    let mut request_messages = (**messages).clone();
    request_messages.push(Message::text("user", guard));
    Arc::new(request_messages)
}

enum LoopAction {
    /// The messages to append to the next request, not the whole history.
    Retry(Vec<Message>),
    Done {
        final_content: String,
        reasoning_content: Option<String>,
    },
}

fn evaluate_end_turn(
    raw_content: &str,
    reasoning_content: Option<&str>,
    declarative_retry_attempted: &mut bool,
) -> Result<LoopAction, EgoPulseError> {
    let visible_text = strip_thinking(raw_content.trim());
    let has_displayable_output = !visible_text.trim().is_empty();

    if has_displayable_output
        && !*declarative_retry_attempted
        && is_declarative_only_reply(&visible_text)
    {
        *declarative_retry_attempted = true;
        warn!("declarative-only reply detected; injecting corrective prompt and retrying once");
        return Ok(LoopAction::Retry(runtime_guard_suffix(
            raw_content,
            reasoning_content,
            "[runtime_guard]: Your previous reply only declared what you would do without actually executing any tools. If the user's request requires tool calls, execute them NOW instead of just describing what you plan to do. Then provide the result.",
        )));
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
        return Ok(LoopAction::Retry(runtime_guard_suffix(
            raw_content,
            reasoning_content,
            "[runtime_guard]: Your previous reply attempted tool use but did not produce a valid executable tool call. If tools are required, call them now and then provide the result.",
        )));
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
    use crate::agent_loop::process_turn_with_events;
    use crate::agent_loop::test_support::{
        RecordingProvider, build_state, build_state_with_compaction_window,
        build_state_with_provider, cli_context,
    };
    use crate::conversation::ConversationScope;
    use crate::llm::{MessagesResponse, ToolCall};
    use crate::storage::call_blocking;
    use crate::tools::{Tool, ToolExecutionContext, ToolResult};
    use serial_test::serial;
    use std::sync::Arc;

    /// Provider that streams scripted chunks before returning each response,
    /// so tests observe the exact deltas the loop publishes per iteration.
    struct ScriptedStreamingProvider {
        script: std::sync::Mutex<Vec<(Vec<String>, MessagesResponse)>>,
    }

    impl ScriptedStreamingProvider {
        fn new(script: Vec<(Vec<String>, MessagesResponse)>) -> Self {
            Self {
                script: std::sync::Mutex::new(script),
            }
        }
    }

    #[async_trait::async_trait]
    impl crate::llm::LlmProvider for ScriptedStreamingProvider {
        async fn send_message(
            &self,
            _system: &str,
            _messages: Arc<Vec<crate::llm::Message>>,
            _tools: Option<Arc<Vec<crate::llm::ToolDefinition>>>,
        ) -> Result<MessagesResponse, crate::error::LlmError> {
            let (chunks, response) = self.script.lock().expect("script").remove(0);
            debug_assert!(chunks.is_empty(), "scripted chunks need streaming");
            Ok(response)
        }

        async fn send_message_streaming(
            &self,
            _system: &str,
            _messages: Arc<Vec<crate::llm::Message>>,
            _tools: Option<Arc<Vec<crate::llm::ToolDefinition>>>,
            on_delta: &(dyn Fn(String) + Send + Sync),
        ) -> Result<MessagesResponse, crate::error::LlmError> {
            let (chunks, response) = self.script.lock().expect("script").remove(0);
            for chunk in chunks {
                on_delta(chunk);
            }
            Ok(response)
        }

        fn provider_name(&self) -> &str {
            "test"
        }

        fn model_name(&self) -> &str {
            "test-model"
        }
    }

    fn final_response(content: &str) -> MessagesResponse {
        MessagesResponse {
            content: content.to_string(),
            reasoning_content: None,
            tool_calls: Vec::new(),
            usage: None,
        }
    }

    fn streamed_final(chunks: &[&str]) -> (Vec<String>, MessagesResponse) {
        let content = chunks.concat();
        (
            chunks.iter().map(ToString::to_string).collect(),
            final_response(&content),
        )
    }

    /// Reads the persisted session when the model request is built, so the
    /// test can prove the checkpoint was durable before the request.
    #[derive(Clone)]
    struct CheckpointObservingProvider {
        probe: Arc<std::sync::Mutex<Option<SessionProbe>>>,
        persisted_before_request: Arc<std::sync::atomic::AtomicBool>,
    }

    struct SessionProbe {
        db: Arc<crate::storage::Database>,
        chat_id: i64,
    }

    impl CheckpointObservingProvider {
        fn new() -> Self {
            Self {
                probe: Arc::new(std::sync::Mutex::new(None)),
                persisted_before_request: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            }
        }

        fn watch(&self, db: Arc<crate::storage::Database>, chat_id: i64) {
            *self.probe.lock().expect("probe") = Some(SessionProbe { db, chat_id });
        }
    }

    #[async_trait::async_trait]
    impl crate::llm::LlmProvider for CheckpointObservingProvider {
        async fn send_message(
            &self,
            _system: &str,
            _messages: Arc<Vec<crate::llm::Message>>,
            _tools: Option<Arc<Vec<crate::llm::ToolDefinition>>>,
        ) -> Result<MessagesResponse, crate::error::LlmError> {
            Ok(MessagesResponse {
                content: "history summary".to_string(),
                reasoning_content: None,
                tool_calls: Vec::new(),
                usage: None,
            })
        }

        async fn send_message_streaming(
            &self,
            _system: &str,
            _messages: Arc<Vec<crate::llm::Message>>,
            _tools: Option<Arc<Vec<crate::llm::ToolDefinition>>>,
            _on_delta: &(dyn Fn(String) + Send + Sync),
        ) -> Result<MessagesResponse, crate::error::LlmError> {
            let persisted = {
                let probe = self.probe.lock().expect("probe");
                let probe = probe.as_ref().expect("probe is configured");
                let snapshot = probe
                    .db
                    .load_session_snapshot(probe.chat_id, 50)
                    .expect("snapshot");
                snapshot
                    .messages_json
                    .as_deref()
                    .is_some_and(|json| json.contains("history summary"))
            };
            self.persisted_before_request
                .store(persisted, std::sync::atomic::Ordering::SeqCst);
            Ok(final_response("answer after compaction"))
        }

        fn provider_name(&self) -> &str {
            "test"
        }

        fn model_name(&self) -> &str {
            "test-model"
        }
    }

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
    async fn context_overflow_compacts_and_retries_the_model_step_once() {
        // Arrange: the first request is sent before the threshold is reached;
        // the provider then rejects it as oversized.
        let dir = tempfile::tempdir().expect("tempdir");
        let provider = RecordingProvider::new(
            vec![
                Err(crate::error::LlmError::ContextOverflow {
                    status: reqwest::StatusCode::BAD_REQUEST,
                    body_preview: "maximum context length is 2000 tokens".to_string(),
                }),
                Ok(MessagesResponse {
                    content: "history summary".to_string(),
                    reasoning_content: None,
                    tool_calls: Vec::new(),
                    usage: Some(crate::llm::LlmUsage {
                        input_tokens: 100,
                        output_tokens: 10,
                        cached_input_tokens: None,
                    }),
                }),
                Ok(final_response("recovered answer")),
            ],
            vec![0, 0, 0],
        );
        // The window is wide enough that the pre-request threshold does not
        // fire: the provider rejects the request instead, which is the path
        // under test.
        let state =
            build_state_with_compaction_window(dir.path(), 60_000, Box::new(provider.clone()));
        let context = cli_context("overflow-recovery");
        let runtime = state.turn_dependencies();
        let chat_id = crate::agent_loop::session::resolve_chat_id(&runtime, &context)
            .await
            .expect("chat id");
        crate::agent_loop::session::save_test_session(
            &state.assets,
            &state.db,
            chat_id,
            vec![
                Message::text("user", "old question"),
                Message::text("assistant", "old answer"),
            ],
        );

        // Act
        let reply = process_turn(&runtime, &context, "fresh question")
            .await
            .expect("overflow recovery should finalize");

        // Assert: the summary request ran, then the model step was re-issued
        // with the checkpoint in place.
        assert_eq!(reply, "recovered answer");
        let systems = provider.seen_systems();
        assert_eq!(systems.len(), 3, "summary request + two model requests");
        assert!(
            systems[1].contains("context summarization assistant"),
            "the second call must be the compaction summary request: {}",
            systems[1]
        );
        let retried = &provider.seen_messages()[2];
        assert!(
            retried[0]
                .content
                .as_text_lossy()
                .starts_with("[CONTEXT CHECKPOINT — REFERENCE ONLY]"),
            "the retried request must start with the checkpoint"
        );
        assert!(
            retried[0]
                .content
                .as_text_lossy()
                .contains("history summary")
        );
    }

    #[tokio::test]
    #[serial]
    async fn a_successful_response_above_the_window_is_compacted_before_the_next_request() {
        // Arrange: the provider accepted a request larger than the window and
        // reported the usage. The response is a normal answer, so it must not
        // be generated again; the oversized context is compacted before the
        // next turn sends anything.
        let dir = tempfile::tempdir().expect("tempdir");
        let provider = RecordingProvider::new(
            vec![
                Ok(MessagesResponse {
                    content: "first answer".to_string(),
                    reasoning_content: None,
                    tool_calls: Vec::new(),
                    usage: Some(crate::llm::LlmUsage {
                        input_tokens: 5_000,
                        output_tokens: 10,
                        cached_input_tokens: None,
                    }),
                }),
                Ok(MessagesResponse {
                    content: "history summary".to_string(),
                    reasoning_content: None,
                    tool_calls: Vec::new(),
                    usage: None,
                }),
                Ok(final_response("second answer")),
            ],
            vec![0, 0, 0],
        );
        let state =
            build_state_with_compaction_window(dir.path(), 2_000, Box::new(provider.clone()));
        let context = cli_context("silent-overflow");
        let runtime = state.turn_dependencies();

        // Act
        let first = process_turn(&runtime, &context, "first question")
            .await
            .expect("first turn");
        let second = process_turn(&runtime, &context, "second question")
            .await
            .expect("second turn");

        // Assert: the oversized response stands, and the next request is
        // preceded by exactly one compaction summary.
        assert_eq!(first, "first answer");
        assert_eq!(second, "second answer");
        let systems = provider.seen_systems();
        assert_eq!(systems.len(), 3, "first answer, summary, second answer");
        assert!(
            systems[1].contains("context summarization assistant"),
            "the second turn must compact before sending: {}",
            systems[1]
        );
        let second_request = &provider.seen_messages()[2];
        assert!(
            second_request[0]
                .content
                .as_text_lossy()
                .starts_with("[CONTEXT CHECKPOINT — REFERENCE ONLY]"),
            "the second request must carry the checkpoint"
        );
    }

    #[tokio::test]
    #[serial]
    async fn a_tool_phase_above_the_window_forces_compaction_before_the_next_request() {
        // Arrange: the tool-phase response reports a prompt above the window,
        // so the next iteration must compact even though the estimate fits.
        let dir = tempfile::tempdir().expect("tempdir");
        let provider = RecordingProvider::new(
            vec![
                Ok(MessagesResponse {
                    content: "Checking the workspace".to_string(),
                    reasoning_content: None,
                    tool_calls: vec![ToolCall {
                        id: "forced-1".to_string(),
                        name: "ls".to_string(),
                        arguments: serde_json::json!({}),
                        invalid_arguments: None,
                    }],
                    usage: Some(crate::llm::LlmUsage {
                        input_tokens: 5_000,
                        output_tokens: 10,
                        cached_input_tokens: None,
                    }),
                }),
                Ok(MessagesResponse {
                    content: "history summary".to_string(),
                    reasoning_content: None,
                    tool_calls: Vec::new(),
                    usage: None,
                }),
                Ok(final_response("final answer")),
            ],
            vec![0, 0, 0],
        );
        let state =
            build_state_with_compaction_window(dir.path(), 2_000, Box::new(provider.clone()));
        let context = cli_context("forced-compaction");

        // Act
        let reply = process_turn(
            &state.turn_dependencies(),
            &context,
            "inspect the workspace",
        )
        .await
        .expect("turn should finalize");

        // Assert: one summary, then the next model request with the checkpoint.
        assert_eq!(reply, "final answer");
        let systems = provider.seen_systems();
        assert_eq!(systems.len(), 3, "model, summary, model");
        assert!(
            systems[1].contains("context summarization assistant"),
            "the second call must be the compaction summary: {}",
            systems[1]
        );
        let second_request = &provider.seen_messages()[2];
        assert!(
            second_request[0]
                .content
                .as_text_lossy()
                .starts_with("[CONTEXT CHECKPOINT — REFERENCE ONLY]"),
            "the request after the forced compaction must carry the checkpoint"
        );
    }

    #[tokio::test]
    #[serial]
    async fn overflow_during_a_declarative_retry_keeps_the_corrective_messages() {
        // Arrange: the model only declares what it will do, the corrective
        // retry overflows, and the compaction must not drop the correction.
        let dir = tempfile::tempdir().expect("tempdir");
        let provider = RecordingProvider::new(
            vec![
                Ok(final_response("I will inspect the files.")),
                Err(crate::error::LlmError::ContextOverflow {
                    status: reqwest::StatusCode::BAD_REQUEST,
                    body_preview: "maximum context length is 2000 tokens".to_string(),
                }),
                Ok(MessagesResponse {
                    content: "history summary".to_string(),
                    reasoning_content: None,
                    tool_calls: Vec::new(),
                    usage: None,
                }),
                Ok(final_response("recovered answer")),
            ],
            vec![0, 0, 0, 0],
        );
        let state =
            build_state_with_compaction_window(dir.path(), 60_000, Box::new(provider.clone()));
        let context = cli_context("overflow-retry-guard");
        let runtime = state.turn_dependencies();
        let chat_id = crate::agent_loop::session::resolve_chat_id(&runtime, &context)
            .await
            .expect("chat id");
        crate::agent_loop::session::save_test_session(
            &state.assets,
            &state.db,
            chat_id,
            vec![
                Message::text("user", "old question"),
                Message::text("assistant", "old answer"),
            ],
        );

        // Act
        let reply = process_turn(&runtime, &context, "fresh question")
            .await
            .expect("overflow recovery should finalize");

        // Assert: the retried request still ends with the discarded reply and
        // its corrective guard, on top of the compacted conversation.
        assert_eq!(reply, "recovered answer");
        let requests = provider.seen_messages();
        assert_eq!(requests.len(), 4, "retry + summary + retried request");
        let retried = &requests[3];
        assert!(
            retried[0]
                .content
                .as_text_lossy()
                .starts_with("[CONTEXT CHECKPOINT — REFERENCE ONLY]"),
            "the retried request must start with the checkpoint"
        );
        let suffix = &retried[retried.len() - 2..];
        assert_eq!(suffix[0].role, "assistant");
        assert_eq!(
            suffix[0].content.as_text_lossy(),
            "I will inspect the files."
        );
        assert!(suffix[1].content.as_text_lossy().contains("runtime_guard"));
    }

    #[tokio::test]
    #[serial]
    async fn a_second_context_overflow_after_compaction_surfaces_the_error() {
        // Arrange: every model request overflows, so recovery must stop after
        // one attempt instead of looping.
        let dir = tempfile::tempdir().expect("tempdir");
        let overflow = || {
            Err(crate::error::LlmError::ContextOverflow {
                status: reqwest::StatusCode::BAD_REQUEST,
                body_preview: "maximum context length exceeded".to_string(),
            })
        };
        let provider = RecordingProvider::new(
            vec![
                overflow(),
                Ok(MessagesResponse {
                    content: "history summary".to_string(),
                    reasoning_content: None,
                    tool_calls: Vec::new(),
                    usage: None,
                }),
                overflow(),
            ],
            vec![0, 0, 0],
        );
        let state =
            build_state_with_compaction_window(dir.path(), 60_000, Box::new(provider.clone()));
        let context = cli_context("overflow-twice");
        let runtime = state.turn_dependencies();
        let chat_id = crate::agent_loop::session::resolve_chat_id(&runtime, &context)
            .await
            .expect("chat id");
        crate::agent_loop::session::save_test_session(
            &state.assets,
            &state.db,
            chat_id,
            vec![
                Message::text("user", "old question"),
                Message::text("assistant", "old answer"),
            ],
        );

        // Act
        let result = process_turn(&runtime, &context, "fresh question").await;

        // Assert: only one compaction summary call, then the overflow error.
        assert!(result.is_err());
        assert_eq!(provider.seen_systems().len(), 3);
    }

    #[tokio::test]
    #[serial]
    async fn truncation_recovery_discards_published_partial_output() {
        // Arrange: deltas are published before the provider reports the limit.
        let dir = tempfile::tempdir().expect("tempdir");
        let provider = TruncatedScriptProvider::new(vec![
            TruncatedScriptStep::Truncated {
                content: "partial answer",
                output_tokens: 100,
                tool_calls: Vec::new(),
                stream_delta: "partial ",
            },
            TruncatedScriptStep::Complete("complete answer"),
        ]);
        let state =
            build_state_with_compaction_window(dir.path(), 60_000, Box::new(provider.clone()));
        let context = cli_context("truncation-discard");
        let chat_id = seed_short_session(&state, &context).await;
        let events = Arc::new(std::sync::Mutex::new(Vec::<AgentEvent>::new()));
        let event_log = Arc::clone(&events);

        // Act
        let reply = process_turn_with_events(
            &state.turn_dependencies(),
            &context,
            "fresh question",
            move |event| event_log.lock().expect("events").push(event),
        )
        .await
        .expect("turn should finalize after recovery");

        // Assert: the retry is a real model request preceded by the
        // checkpoint, and the partial streamed text is discarded before it.
        assert_eq!(reply, "complete answer");
        assert_eq!(provider.model_requests(), 2);
        assert_eq!(provider.summary_requests(), 1);
        let requests = provider.seen_messages();
        assert!(
            requests[1][0]
                .content
                .as_text_lossy()
                .starts_with("[CONTEXT CHECKPOINT — REFERENCE ONLY]"),
            "the retried request must start with the checkpoint"
        );
        let (deltas, discarded, final_id) = collect_loop_events(&events);
        assert_eq!(discarded, vec![deltas[0].0.clone()]);
        assert_eq!(final_id, deltas[0].0, "the retry reuses the iteration id");
        let history = call_blocking(Arc::clone(&state.db), move |db| {
            db.get_all_messages(chat_id)
        })
        .await
        .expect("messages");
        assert!(
            history
                .iter()
                .all(|message| !message.content.contains("partial answer")),
            "truncated text must not be persisted"
        );
    }

    #[tokio::test]
    #[serial]
    async fn failed_summary_preserves_truncated_answer_without_checkpoint_or_discard() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let provider = TruncatedScriptProvider::new(vec![TruncatedScriptStep::Truncated {
            content: "partial answer",
            output_tokens: 100,
            tool_calls: Vec::new(),
            stream_delta: "partial ",
        }]);
        provider
            .inner
            .fail_summary
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let state =
            build_state_with_compaction_window(dir.path(), 60_000, Box::new(provider.clone()));
        let context = cli_context("truncation-summary-failure");
        let chat_id = seed_short_session(&state, &context).await;
        let events = Arc::new(std::sync::Mutex::new(Vec::<AgentEvent>::new()));
        let event_log = Arc::clone(&events);

        // Act
        let reply = process_turn_with_events(
            &state.turn_dependencies(),
            &context,
            "fresh question",
            move |event| {
                event_log.lock().expect("events").push(event);
            },
        )
        .await
        .expect("partial answer survives");

        // Assert
        assert_eq!(reply, "partial answer");
        assert_eq!(provider.model_requests(), 1);
        assert_eq!(provider.summary_requests(), 1);
        let (_, discarded, _) = collect_loop_events(&events);
        assert!(discarded.is_empty());
        let loaded = crate::agent_loop::session::load_messages_for_turn(
            &state.turn_dependencies(),
            context.scope,
            chat_id,
        )
        .await
        .expect("session");
        assert!(loaded.context.checkpoint.is_none());
        assert_eq!(
            loaded.context.messages.last().and_then(|m| m.finish_reason),
            Some(TruncationReason::MaxOutputTokens)
        );
        let history = call_blocking(Arc::clone(&state.db), move |db| {
            db.get_all_messages(chat_id)
        })
        .await
        .expect("messages");
        assert!(
            history
                .iter()
                .any(|message| message.content == "partial answer"
                    && message.finish_reason.as_deref() == Some("max_output_tokens"))
        );
    }

    #[tokio::test]
    #[serial]
    async fn a_generation_that_used_the_whole_intended_output_is_not_retried() {
        // Arrange: output == intended limit, so compacting is not expected to
        // produce a complete answer.
        let dir = tempfile::tempdir().expect("tempdir");
        let provider = TruncatedScriptProvider::new(vec![TruncatedScriptStep::Truncated {
            content: "partial answer",
            output_tokens: 100,
            tool_calls: Vec::new(),
            stream_delta: "partial ",
        }]);
        let mut config = crate::agent_loop::test_support::test_config_with_compaction_window(
            dir.path().to_str().expect("utf8").to_string(),
            1,
            60_000,
        );
        config
            .providers
            .get_mut(&crate::config::ProviderId::new("test"))
            .expect("test provider")
            .models
            .get_mut("test-model")
            .expect("test model")
            .max_output_tokens = Some(100);
        let state = build_state(config, Box::new(provider.clone()));
        let mut context = cli_context("truncation-at-limit");
        context.request_key = "cli:truncation-at-limit:1".to_string();
        seed_short_session(&state, &context).await;
        let events = Arc::new(std::sync::Mutex::new(Vec::<AgentEvent>::new()));
        let event_log = Arc::clone(&events);

        // Act
        let reply = process_turn_with_events(
            &state.turn_dependencies(),
            &context,
            "fresh question",
            move |event| event_log.lock().expect("events").push(event),
        )
        .await
        .expect("the partial answer is kept");

        // Assert: no retry, no summary, and the truncation is recorded on the
        // persisted final message. The published partial text is kept, so it
        // must not be discarded.
        assert_eq!(reply, "partial answer");
        let (deltas, discarded, final_id) = collect_loop_events(&events);
        assert!(discarded.is_empty(), "kept output must not be discarded");
        assert_eq!(
            final_id, deltas[0].0,
            "the final row adopts the streamed id"
        );
        assert_eq!(provider.model_requests(), 1);
        assert_eq!(provider.summary_requests(), 0);
        let chat_id =
            crate::agent_loop::session::resolve_chat_id(&state.turn_dependencies(), &context)
                .await
                .expect("chat id");
        let loaded = crate::agent_loop::session::load_messages_for_turn(
            &state.turn_dependencies(),
            context.scope,
            chat_id,
        )
        .await
        .expect("session");
        assert_eq!(
            loaded.context.messages.last().and_then(|m| m.finish_reason),
            Some(TruncationReason::MaxOutputTokens)
        );
        let history = call_blocking(Arc::clone(&state.db), move |db| {
            db.get_all_messages(chat_id)
        })
        .await
        .expect("messages");
        let persisted = history
            .iter()
            .find(|message| message.content == "partial answer")
            .expect("the partial answer must be persisted in the transcript");
        assert_eq!(
            persisted.finish_reason.as_deref(),
            Some("max_output_tokens")
        );

        // A re-accepted request replays the persisted answer with the same
        // truncation state instead of presenting it as a complete one.
        let replay_events = Arc::new(std::sync::Mutex::new(Vec::<AgentEvent>::new()));
        let replay_log = Arc::clone(&replay_events);
        let replay = process_turn_with_events(
            &state.turn_dependencies(),
            &context,
            "fresh question",
            move |event| replay_log.lock().expect("events").push(event),
        )
        .await
        .expect("replayed turn");
        assert_eq!(replay, "partial answer");
        let replay_reason = replay_events
            .lock()
            .expect("events")
            .iter()
            .find_map(|event| match event {
                AgentEvent::FinalResponse { finish_reason, .. } => *finish_reason,
                _ => None,
            });
        assert_eq!(replay_reason, Some(TruncationReason::MaxOutputTokens));
        assert_eq!(provider.model_requests(), 1);
    }

    #[tokio::test]
    #[serial]
    async fn a_truncated_tool_call_is_persisted_as_a_failed_result() {
        // Arrange: the generation stops inside a tool call. The call must not
        // run, and its failure must be visible to the next model request.
        let dir = tempfile::tempdir().expect("tempdir");
        let target = format!("tests/{}/truncated-write.txt", uuid::Uuid::new_v4());
        let provider = TruncatedScriptProvider::new(vec![
            TruncatedScriptStep::Truncated {
                content: "Writing the file",
                output_tokens: 100,
                tool_calls: vec![ToolCall {
                    id: "truncated-write".to_string(),
                    name: "write".to_string(),
                    arguments: serde_json::json!({"path": target, "content": "hello"}),
                    invalid_arguments: None,
                }],
                stream_delta: "",
            },
            TruncatedScriptStep::Complete("I could not write the file"),
        ]);
        let mut config = crate::agent_loop::test_support::test_config_with_compaction_window(
            dir.path().to_str().expect("utf8").to_string(),
            1,
            60_000,
        );
        config.compaction.enabled = false;
        let state = build_state(config, Box::new(provider.clone()));
        let context = cli_context("truncated-tool-call");
        seed_short_session(&state, &context).await;
        let workspace = state.config.workspace_dir().expect("workspace_dir");

        // Act
        let reply = process_turn(&state.turn_dependencies(), &context, "fresh question")
            .await
            .expect("the failure result is fed back to the model");

        // Assert: the model saw the failed call, the Tool did not run, and the
        // turn ends with the model's next answer.
        assert_eq!(reply, "I could not write the file");
        assert_eq!(provider.model_requests(), 2);
        assert_eq!(provider.summary_requests(), 0);
        let requests = provider.seen_messages();
        assert!(
            requests[1].iter().any(|message| {
                message.role == "tool"
                    && message.tool_call_id.as_deref() == Some("truncated-write")
                    && message.content.as_text_lossy().contains("truncated")
            }),
            "the retried request must carry the failed tool result"
        );
        assert!(!workspace.join(&target).exists());
    }

    #[tokio::test]
    #[serial]
    async fn a_second_truncated_generation_keeps_the_retried_partial_text() {
        // Arrange: the retried generation is truncated again.
        let dir = tempfile::tempdir().expect("tempdir");
        let provider = TruncatedScriptProvider::new(vec![
            TruncatedScriptStep::Truncated {
                content: "partial answer",
                output_tokens: 100,
                tool_calls: Vec::new(),
                stream_delta: "",
            },
            TruncatedScriptStep::Truncated {
                content: "partial again",
                output_tokens: 100,
                tool_calls: Vec::new(),
                stream_delta: "",
            },
        ]);
        let state =
            build_state_with_compaction_window(dir.path(), 60_000, Box::new(provider.clone()));
        let context = cli_context("truncation-twice");
        seed_short_session(&state, &context).await;

        // Act
        let reply = process_turn(&state.turn_dependencies(), &context, "fresh question")
            .await
            .expect("the second partial answer is kept");

        // Assert: exactly one recovery attempt, then the retried text is final.
        assert_eq!(reply, "partial again");
        assert_eq!(provider.model_requests(), 2);
        assert_eq!(provider.summary_requests(), 1);
    }

    #[tokio::test]
    #[serial]
    async fn disabled_compaction_surfaces_a_context_overflow_without_summarizing() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let provider = RecordingProvider::new(
            vec![
                Err(crate::error::LlmError::ContextOverflow {
                    status: reqwest::StatusCode::BAD_REQUEST,
                    body_preview: "maximum context length exceeded".to_string(),
                }),
                Ok(final_response("must not be requested")),
            ],
            vec![0, 0],
        );
        let mut config = crate::agent_loop::test_support::test_config_with_compaction_window(
            dir.path().to_str().expect("utf8").to_string(),
            1,
            60_000,
        );
        config.compaction.enabled = false;
        let state = build_state(config, Box::new(provider.clone()));
        let context = cli_context("overflow-disabled");
        seed_short_session(&state, &context).await;

        // Act
        let result = process_turn(&state.turn_dependencies(), &context, "fresh question").await;

        // Assert: no automatic summarization and no retry.
        assert!(matches!(
            result,
            Err(crate::error::EgoPulseError::Llm(
                crate::error::LlmError::ContextOverflow { .. }
            ))
        ));
        assert_eq!(provider.seen_systems().len(), 1);
    }

    #[tokio::test]
    #[serial]
    async fn disabled_compaction_does_not_recover_a_truncated_generation() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let provider = TruncatedScriptProvider::new(vec![
            TruncatedScriptStep::Truncated {
                content: "partial answer",
                output_tokens: 100,
                tool_calls: Vec::new(),
                stream_delta: "",
            },
            TruncatedScriptStep::Complete("must not be requested"),
        ]);
        let mut config = crate::agent_loop::test_support::test_config_with_compaction_window(
            dir.path().to_str().expect("utf8").to_string(),
            1,
            60_000,
        );
        config.compaction.enabled = false;
        let state = build_state(config, Box::new(provider.clone()));
        let context = cli_context("truncation-disabled");
        seed_short_session(&state, &context).await;

        // Act
        let reply = process_turn(&state.turn_dependencies(), &context, "fresh question")
            .await
            .expect("disabled compaction keeps the partial answer");

        // Assert: no summary and no retry, but the generated text survives.
        assert_eq!(reply, "partial answer");
        assert_eq!(provider.model_requests(), 1);
        assert_eq!(provider.summary_requests(), 0);
    }

    #[tokio::test]
    #[serial]
    async fn disabled_compaction_keeps_a_response_above_the_window() {
        // Arrange: a successful response reports a prompt larger than the
        // window; with compaction disabled it must not be summarized.
        let dir = tempfile::tempdir().expect("tempdir");
        let mut response = final_response("answer");
        response.usage = Some(crate::llm::LlmUsage {
            input_tokens: 70_000,
            output_tokens: 10,
            cached_input_tokens: None,
        });
        let provider = RecordingProvider::new(
            vec![Ok(response), Ok(final_response("history summary"))],
            vec![0, 0],
        );
        let mut config = crate::agent_loop::test_support::test_config_with_compaction_window(
            dir.path().to_str().expect("utf8").to_string(),
            1,
            60_000,
        );
        config.compaction.enabled = false;
        let state = build_state(config, Box::new(provider.clone()));
        let context = cli_context("silent-overflow-disabled");
        let chat_id = seed_short_session(&state, &context).await;

        // Act
        let reply = process_turn(&state.turn_dependencies(), &context, "fresh question")
            .await
            .expect("turn should finalize");

        // Assert
        assert_eq!(reply, "answer");
        assert_eq!(provider.seen_systems().len(), 1);
        let snapshot = state
            .db
            .load_session_snapshot(chat_id, 50)
            .expect("snapshot")
            .messages_json
            .expect("snapshot body");
        assert!(!snapshot.contains("history summary"));
    }

    #[tokio::test]
    #[serial]
    async fn a_checkpoint_is_persisted_before_the_model_request() {
        // Arrange: a small window and a long history make the first request
        // compact. The window still leaves room for the summary templates.
        let dir = tempfile::tempdir().expect("tempdir");
        let mut config = crate::agent_loop::test_support::test_config_with_compaction_window(
            dir.path().to_str().expect("utf8").to_string(),
            1,
            2_000,
        );
        config.compaction.reserve_tokens = 0;
        let provider = CheckpointObservingProvider::new();
        let state = build_state(config, Box::new(provider.clone()));
        let context = cli_context("checkpoint-durable");
        let runtime = state.turn_dependencies();
        let chat_id = crate::agent_loop::session::resolve_chat_id(&runtime, &context)
            .await
            .expect("chat id");
        crate::agent_loop::session::save_test_session(
            &state.assets,
            &state.db,
            chat_id,
            vec![
                Message::text("user", "old question"),
                Message::text("assistant", "old answer ".repeat(500)),
                Message::text("user", "another question"),
                Message::text("assistant", "another answer"),
            ],
        );
        provider.watch(Arc::clone(&state.db), chat_id);

        // Act
        let reply = process_turn(&state.turn_dependencies(), &context, "fresh question")
            .await
            .expect("turn should finalize");

        // Assert
        assert_eq!(reply, "answer after compaction");
        assert!(
            provider
                .persisted_before_request
                .load(std::sync::atomic::Ordering::SeqCst),
            "the checkpoint must be committed before the model request is sent"
        );
    }

    /// Provider that reports truncated generations and records how often the
    /// loop asked for a summary.
    ///
    /// Every handle shares one script so the test observes the counters that
    /// the runtime's copy mutated.
    #[derive(Clone)]
    struct TruncatedScriptProvider {
        inner: Arc<TruncatedScriptInner>,
    }

    struct TruncatedScriptInner {
        steps: std::sync::Mutex<Vec<TruncatedScriptStep>>,
        model_requests: std::sync::atomic::AtomicUsize,
        summary_requests: std::sync::atomic::AtomicUsize,
        fail_summary: std::sync::atomic::AtomicBool,
        seen_messages: std::sync::Mutex<Vec<Vec<crate::llm::Message>>>,
    }

    #[derive(Clone)]
    enum TruncatedScriptStep {
        Truncated {
            content: &'static str,
            output_tokens: i64,
            tool_calls: Vec<ToolCall>,
            /// Deltas published before the provider reports the truncation.
            stream_delta: &'static str,
        },
        Complete(&'static str),
    }

    impl TruncatedScriptProvider {
        fn new(steps: Vec<TruncatedScriptStep>) -> Self {
            Self {
                inner: Arc::new(TruncatedScriptInner {
                    steps: std::sync::Mutex::new(steps),
                    model_requests: std::sync::atomic::AtomicUsize::new(0),
                    summary_requests: std::sync::atomic::AtomicUsize::new(0),
                    fail_summary: std::sync::atomic::AtomicBool::new(false),
                    seen_messages: std::sync::Mutex::new(Vec::new()),
                }),
            }
        }

        fn model_requests(&self) -> usize {
            self.inner
                .model_requests
                .load(std::sync::atomic::Ordering::SeqCst)
        }

        fn summary_requests(&self) -> usize {
            self.inner
                .summary_requests
                .load(std::sync::atomic::Ordering::SeqCst)
        }

        fn seen_messages(&self) -> Vec<Vec<crate::llm::Message>> {
            self.inner.seen_messages.lock().expect("messages").clone()
        }
    }

    #[async_trait::async_trait]
    impl crate::llm::LlmProvider for TruncatedScriptProvider {
        async fn send_message(
            &self,
            _system: &str,
            _messages: Arc<Vec<crate::llm::Message>>,
            _tools: Option<Arc<Vec<crate::llm::ToolDefinition>>>,
        ) -> Result<MessagesResponse, crate::error::LlmError> {
            self.inner
                .summary_requests
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if self
                .inner
                .fail_summary
                .load(std::sync::atomic::Ordering::SeqCst)
            {
                return Err(crate::error::LlmError::InvalidResponse(
                    "summary failed".to_string(),
                ));
            }
            Ok(final_response("history summary"))
        }

        async fn send_message_streaming(
            &self,
            system: &str,
            messages: Arc<Vec<crate::llm::Message>>,
            tools: Option<Arc<Vec<crate::llm::ToolDefinition>>>,
            on_delta: &(dyn Fn(String) + Send + Sync),
        ) -> Result<MessagesResponse, crate::error::LlmError> {
            match self
                .send_message_streaming_with_truncation(system, messages, tools, on_delta)
                .await?
            {
                crate::llm::StreamedResponse::Complete(response) => Ok(response),
                crate::llm::StreamedResponse::Truncated { reason, .. } => {
                    Err(crate::error::LlmError::IncompleteResponse(format!(
                        "truncated ({})",
                        reason.as_str()
                    )))
                }
            }
        }

        async fn send_message_streaming_with_truncation(
            &self,
            _system: &str,
            messages: Arc<Vec<crate::llm::Message>>,
            _tools: Option<Arc<Vec<crate::llm::ToolDefinition>>>,
            on_delta: &(dyn Fn(String) + Send + Sync),
        ) -> Result<crate::llm::StreamedResponse, crate::error::LlmError> {
            self.inner
                .model_requests
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.inner
                .seen_messages
                .lock()
                .expect("messages")
                .push((*messages).clone());
            let step = self.inner.steps.lock().expect("steps").remove(0);
            Ok(match step {
                TruncatedScriptStep::Truncated {
                    content,
                    output_tokens,
                    tool_calls,
                    stream_delta,
                } => {
                    if !stream_delta.is_empty() {
                        on_delta(stream_delta.to_string());
                    }
                    crate::llm::StreamedResponse::Truncated {
                        response: MessagesResponse {
                            content: content.to_string(),
                            reasoning_content: None,
                            tool_calls,
                            usage: Some(crate::llm::LlmUsage {
                                input_tokens: 1_000,
                                output_tokens,
                                cached_input_tokens: None,
                            }),
                        },
                        reason: crate::llm::TruncationReason::MaxOutputTokens,
                    }
                }
                TruncatedScriptStep::Complete(content) => {
                    crate::llm::StreamedResponse::Complete(final_response(content))
                }
            })
        }

        fn provider_name(&self) -> &str {
            "test"
        }

        fn model_name(&self) -> &str {
            "test-model"
        }
    }

    /// Saves a two-message session so compaction has something to summarize.
    async fn seed_short_session(state: &crate::runtime::AppState, context: &SurfaceContext) -> i64 {
        let runtime = state.turn_dependencies();
        let chat_id = crate::agent_loop::session::resolve_chat_id(&runtime, context)
            .await
            .expect("chat id");
        crate::agent_loop::session::save_test_session(
            &state.assets,
            &state.db,
            chat_id,
            vec![
                crate::llm::Message::text("user", "old question"),
                crate::llm::Message::text("assistant", "old answer"),
            ],
        );
        chat_id
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
                    invalid_arguments: None,
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
        assert!(loaded.context.messages.iter().all(|message| {
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
                            invalid_arguments: None,
                        },
                        ToolCall {
                            id: "wait-b".to_string(),
                            name: "wait_test".to_string(),
                            arguments: serde_json::json!({"name": "b"}),
                            invalid_arguments: None,
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
        let mut retry_attempted = false;

        // Act
        let action = evaluate_end_turn("I will inspect the files.", None, &mut retry_attempted)
            .expect("declarative response should be recoverable");

        // Assert
        assert!(retry_attempted);
        match action {
            LoopAction::Retry(suffix) => {
                assert_eq!(suffix.len(), 2);
                assert_eq!(suffix[0].role, "assistant");
                assert!(suffix[1].content.as_text_lossy().contains("runtime_guard"));
            }
            LoopAction::Done { .. } => {
                panic!("declarative response should inject a corrective retry")
            }
        }
    }

    fn collect_loop_events(
        events: &Arc<std::sync::Mutex<Vec<AgentEvent>>>,
    ) -> (Vec<(String, String)>, Vec<String>, String) {
        let events = events.lock().expect("events");
        let deltas: Vec<(String, String)> = events
            .iter()
            .filter_map(|event| match event {
                AgentEvent::Delta { message_id, text } => Some((message_id.clone(), text.clone())),
                _ => None,
            })
            .collect();
        let discarded: Vec<String> = events
            .iter()
            .filter_map(|event| match event {
                AgentEvent::AssistantMessageDiscarded { message_id } => Some(message_id.clone()),
                _ => None,
            })
            .collect();
        let final_id = events
            .iter()
            .find_map(|event| match event {
                AgentEvent::FinalResponse {
                    assistant_message_id,
                    ..
                } => Some(assistant_message_id.clone()),
                _ => None,
            })
            .expect("final response names its message");
        (deltas, discarded, final_id)
    }

    async fn chat_and_turn_ids(
        state: &crate::runtime::AppState,
        session: &str,
    ) -> (i64, String, Option<String>) {
        let chat_id = call_blocking(Arc::clone(&state.db), {
            let session = session.to_string();
            move |db| {
                db.resolve_or_create_chat_id(
                    "cli",
                    &format!("cli:{session}:agent:default"),
                    "cli",
                    "default",
                )
            }
        })
        .await
        .expect("chat id");
        let (turn_id, final_message_id): (String, Option<String>) =
            call_blocking(Arc::clone(&state.db), move |db| {
                let conn = db.get_conn()?;
                conn.query_row(
                    "SELECT turn_id, final_message_id FROM turn_runs WHERE chat_id = ?1",
                    rusqlite::params![chat_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .map_err(crate::error::StorageError::from)
            })
            .await
            .expect("turn row");
        (chat_id, turn_id, final_message_id)
    }

    #[tokio::test]
    #[serial]
    async fn declarative_retry_discards_the_streamed_iteration() {
        // Arrange: iteration 1 streams a declarative-only reply, iteration 2
        // answers for real.
        let dir = tempfile::tempdir().expect("tempdir");
        let provider = ScriptedStreamingProvider::new(vec![
            streamed_final(&["Sure, ", "I'll help you with that."]),
            streamed_final(&["Here is ", "the answer you need."]),
        ]);
        let state = build_state_with_provider(
            dir.path().to_str().expect("utf8").to_string(),
            Box::new(provider),
        );
        let mut context = cli_context("discard-declarative");
        context.request_key = "cli:discard-declarative:1".to_string();
        let events = Arc::new(std::sync::Mutex::new(Vec::<AgentEvent>::new()));
        let event_log = Arc::clone(&events);

        // Act
        let reply = process_turn_with_events(
            &state.turn_dependencies(),
            &context,
            "help me",
            move |event| event_log.lock().expect("events").push(event),
        )
        .await
        .expect("turn succeeds after retry");
        assert_eq!(reply, "Here is the answer you need.");

        // Assert: iteration 1 streamed under its own id, was discarded, and
        // never persisted; iteration 2 owns the final id everywhere.
        let (deltas, discarded, final_id) = collect_loop_events(&events);
        let first_id = deltas[0].0.clone();
        let second_id = deltas[2].0.clone();
        assert!(first_id.ends_with(":assistant:1"), "id was {first_id}");
        assert!(second_id.ends_with(":assistant:2"), "id was {second_id}");
        assert_ne!(first_id, second_id);
        assert_eq!(discarded, vec![first_id.clone()]);
        assert_eq!(final_id, second_id);

        let (chat_id, turn_id, stored_final_id) =
            chat_and_turn_ids(&state, "discard-declarative").await;
        assert_eq!(stored_final_id.as_deref(), Some(final_id.as_str()));
        assert_eq!(
            final_id,
            format!("turn:{turn_id}:assistant:2"),
            "final row keeps the streamed iteration id"
        );
        let history = call_blocking(Arc::clone(&state.db), move |db| {
            db.get_all_messages(chat_id)
        })
        .await
        .expect("history");
        let ids: Vec<&str> = history.iter().map(|message| message.id.as_str()).collect();
        assert!(
            ids.contains(&"cli:discard-declarative:1"),
            "input keeps the request key, not a generated id: {ids:?}"
        );
        assert!(
            ids.contains(&final_id.as_str()),
            "final row id matches the streamed id: {ids:?}"
        );
        assert!(
            !ids.iter().any(|id| id.ends_with(":assistant:1")),
            "discarded iteration must not persist: {ids:?}"
        );
    }

    #[tokio::test]
    #[serial]
    async fn malformed_tool_reply_retry_discards_the_streamed_iteration() {
        // Arrange: iteration 1 streams declarative text with an unusable tool
        // call, iteration 2 answers for real.
        let dir = tempfile::tempdir().expect("tempdir");
        let malformed = MessagesResponse {
            content: "Let me look into that.".to_string(),
            reasoning_content: None,
            tool_calls: vec![ToolCall {
                id: "bad-call".to_string(),
                name: String::new(),
                arguments: serde_json::json!({}),
                invalid_arguments: None,
            }],
            usage: None,
        };
        let provider = ScriptedStreamingProvider::new(vec![
            (vec!["Let me look into that.".to_string()], malformed),
            streamed_final(&["Here is the answer."]),
        ]);
        let state = build_state_with_provider(
            dir.path().to_str().expect("utf8").to_string(),
            Box::new(provider),
        );
        let context = cli_context("discard-malformed");
        let events = Arc::new(std::sync::Mutex::new(Vec::<AgentEvent>::new()));
        let event_log = Arc::clone(&events);

        // Act
        let reply = process_turn_with_events(
            &state.turn_dependencies(),
            &context,
            "help me",
            move |event| event_log.lock().expect("events").push(event),
        )
        .await
        .expect("turn succeeds after retry");
        assert_eq!(reply, "Here is the answer.");

        // Assert: the streamed-but-unpersistable iteration is discarded by id.
        let (deltas, discarded, final_id) = collect_loop_events(&events);
        assert_eq!(deltas.len(), 2);
        assert!(deltas[0].0.ends_with(":assistant:1"));
        assert!(deltas[1].0.ends_with(":assistant:2"));
        assert_eq!(discarded, vec![deltas[0].0.clone()]);
        assert_eq!(final_id, deltas[1].0);
    }

    #[tokio::test]
    #[serial]
    async fn tool_narration_and_final_share_their_iteration_ids() {
        // Arrange: iteration 1 narrates a tool call, iteration 2 finalizes.
        let dir = tempfile::tempdir().expect("tempdir");
        let relative_path = format!("tests/{}/notes.txt", uuid::Uuid::new_v4());
        let provider = ScriptedStreamingProvider::new(vec![
            (
                vec!["checking the note".to_string()],
                MessagesResponse {
                    content: "checking the note".to_string(),
                    reasoning_content: None,
                    tool_calls: vec![ToolCall {
                        id: "call-note".to_string(),
                        name: "read".to_string(),
                        arguments: serde_json::json!({"path": relative_path}),
                        invalid_arguments: None,
                    }],
                    usage: None,
                },
            ),
            streamed_final(&["done"]),
        ]);
        let state = build_state_with_provider(
            dir.path().to_str().expect("utf8").to_string(),
            Box::new(provider),
        );
        let workspace = state.config.workspace_dir().expect("workspace_dir");
        let note_path = workspace.join(&relative_path);
        std::fs::create_dir_all(note_path.parent().expect("parent")).expect("dir");
        std::fs::write(&note_path, "content").expect("file");
        let context = cli_context("iteration-ids");
        let events = Arc::new(std::sync::Mutex::new(Vec::<AgentEvent>::new()));
        let event_log = Arc::clone(&events);

        // Act
        let reply = process_turn_with_events(
            &state.turn_dependencies(),
            &context,
            "read the note",
            move |event| event_log.lock().expect("events").push(event),
        )
        .await
        .expect("turn");
        assert_eq!(reply, "done");

        // Assert: narration live id == persisted preview id, and the final
        // live id == persisted final id == turn_runs.final_message_id.
        let (deltas, discarded, final_id) = collect_loop_events(&events);
        assert!(discarded.is_empty());
        assert_eq!(deltas.len(), 2);
        let narration_id = deltas[0].0.clone();
        let final_live_id = deltas[1].0.clone();
        assert!(narration_id.ends_with(":assistant:1"));
        assert!(final_live_id.ends_with(":assistant:2"));
        assert_eq!(final_id, final_live_id);

        let (chat_id, turn_id, stored_final_id) = chat_and_turn_ids(&state, "iteration-ids").await;
        assert_eq!(stored_final_id.as_deref(), Some(final_live_id.as_str()));
        let history = call_blocking(Arc::clone(&state.db), move |db| {
            db.get_all_messages(chat_id)
        })
        .await
        .expect("history");
        let preview = history
            .iter()
            .find(|message| message.id == narration_id)
            .expect("preview row shares the narration id");
        assert!(
            preview.content.contains("checking the note"),
            "preview keeps the streamed narration: {}",
            preview.content
        );
        assert_eq!(
            narration_id,
            format!("turn:{turn_id}:assistant:1"),
            "tool phase keeps the pre-assigned iteration id"
        );
    }
}
