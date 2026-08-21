//! Shared one-step LLM execution used by normal turns and Pulse activations.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tracing::warn;

use crate::agent_loop::TurnDependencies;
use crate::agent_loop::compaction::estimate_prompt_tokens;
use crate::agent_loop::message_format::{
    sanitize_assistant_response_text, strip_thinking, summarize_tool_calls_with_content,
};
use crate::agent_loop::response_guard::{EMPTY_REPLY_RUNTIME_GUARD, runtime_guard_messages};
use crate::conversation::ConversationScope;
use crate::error::{EgoPulseError, LlmError};
use crate::llm::calibration::CalibrationKey;
use crate::llm::{LlmProvider, LlmUsage, Message, MessagesResponse, ToolCall, ToolDefinition};
use crate::storage::call_blocking;

/// Maximum number of attempts for a single LLM model iteration.
pub(crate) const MAX_LLM_RETRIES: usize = 3;

/// Base backoff (milliseconds) for exponential LLM retry.
const LLM_RETRY_BASE_BACKOFF_MS: u64 = 500;

pub(crate) struct AssistantToolPhase {
    pub(crate) assistant_message: Message,
    pub(crate) assistant_preview: String,
    pub(crate) tool_calls: Vec<ToolCall>,
}

pub(crate) fn ignore_delta(_: String) {}

pub(crate) enum ModelStep {
    Final(MessagesResponse),
    MalformedToolCalls(MessagesResponse),
    ToolCalls(AssistantToolPhase),
}

#[derive(Clone)]
pub(crate) struct ModelStepRequest<'a> {
    pub(crate) state: &'a TurnDependencies,
    pub(crate) llm: &'a dyn LlmProvider,
    pub(crate) system_prompt: &'a str,
    pub(crate) messages: Arc<Vec<Message>>,
    pub(crate) tools: Option<Arc<Vec<ToolDefinition>>>,
    pub(crate) chat_id: i64,
    pub(crate) caller_channel: &'a str,
    pub(crate) request_kind: &'static str,
    pub(crate) usage_log_failure: &'static str,
    pub(crate) log_scope: &'a str,
    pub(crate) send_failure_log: &'static str,
    pub(crate) iteration: usize,
    pub(crate) scope: ConversationScope,
    pub(crate) on_delta: &'a (dyn Fn(String) + Send + Sync),
}

/// Runs one model step with the fixed dependencies for an agent activation.
pub(crate) struct ModelRunner<'a> {
    request: ModelStepRequest<'a>,
}

impl<'a> ModelRunner<'a> {
    /// Creates a model runner from the dependencies shared by one turn.
    pub(crate) fn new(request: ModelStepRequest<'a>) -> Self {
        Self { request }
    }

    /// Executes one model step, including the shared empty-response recovery.
    pub(crate) async fn run(
        &self,
        empty_retry_attempted: &mut bool,
        output_published: Option<&AtomicBool>,
    ) -> Result<ModelStep, ModelStepError> {
        send_model_step_with_empty_retry(
            self.request.clone(),
            empty_retry_attempted,
            output_published,
        )
        .await
    }

    /// Executes one durable model iteration with bounded transport retries.
    pub(crate) async fn run_with_retry(
        &self,
        turn_id: &str,
    ) -> Result<ModelStep, ModelRequestError> {
        let hash = model_request_hash(
            self.request.system_prompt,
            &self.request.messages,
            self.request
                .tools
                .as_deref()
                .filter(|tools| !tools.is_empty())
                .and_then(|tools| serde_json::to_string(tools).ok())
                .as_deref(),
        );
        let turn_id_for_init = turn_id.to_string();
        let hash_for_init = hash;
        let scope = self.request.scope;
        let iteration = self.request.iteration as i64;
        let db = self.request.state.db_for(scope);
        let advanced = call_blocking(db, move |db| {
            db.begin_turn_model_iteration(&turn_id_for_init, iteration, &hash_for_init)
        })
        .await
        .map_err(|error| ModelRequestError {
            error: EgoPulseError::from(error),
            output_published: false,
        })?;
        if !advanced {
            return Err(ModelRequestError {
                error: EgoPulseError::TurnConcurrencyConflict,
                output_published: false,
            });
        }

        let output_published = Arc::new(AtomicBool::new(false));
        let mut empty_reply_retry_attempted = false;
        let mut retry_messages = Arc::clone(&self.request.messages);
        for attempt in 1..=MAX_LLM_RETRIES {
            let published_flag = Arc::clone(&output_published);
            let caller_on_delta = self.request.on_delta;
            let on_delta = move |text: String| {
                published_flag.store(true, Ordering::SeqCst);
                caller_on_delta(text);
            };
            let response = self
                .run_once(
                    Arc::clone(&retry_messages),
                    &mut empty_reply_retry_attempted,
                    Some(output_published.as_ref()),
                    &on_delta,
                )
                .await;
            match response {
                Ok(step) => return Ok(step),
                Err(step_error) => {
                    let (error, next_retry_messages) = step_error.into_parts();
                    retry_messages = next_retry_messages;
                    let published = output_published.load(Ordering::SeqCst);
                    let retryable =
                        matches!(&error, EgoPulseError::Llm(error) if error.is_retryable());
                    if retryable && !published && attempt < MAX_LLM_RETRIES {
                        let turn_id_for_attempt = turn_id.to_string();
                        let _ = call_blocking(
                            self.request.state.db_for(self.request.scope),
                            move |db| db.increment_turn_model_attempt(&turn_id_for_attempt),
                        )
                        .await;
                        let backoff = llm_retry_backoff(attempt, &error);
                        warn!(
                            attempt,
                            max = MAX_LLM_RETRIES,
                            backoff_ms = backoff.as_millis() as u64,
                            error = %error,
                            "retryable llm error; retrying same iteration"
                        );
                        tokio::time::sleep(backoff).await;
                        continue;
                    }
                    return Err(ModelRequestError {
                        error,
                        output_published: published,
                    });
                }
            }
        }
        unreachable!("retry loop exits via return")
    }

    async fn run_once(
        &self,
        messages: Arc<Vec<Message>>,
        empty_retry_attempted: &mut bool,
        output_published: Option<&AtomicBool>,
        on_delta: &(dyn Fn(String) + Send + Sync),
    ) -> Result<ModelStep, ModelStepError> {
        send_model_step_with_empty_retry(
            ModelStepRequest {
                messages,
                on_delta,
                ..self.request.clone()
            },
            empty_retry_attempted,
            output_published,
        )
        .await
    }
}

/// A model-step error together with the request payload that can be retried.
#[derive(Debug)]
pub(crate) struct ModelStepError {
    error: EgoPulseError,
    retry_messages: Arc<Vec<Message>>,
}

/// An iteration-level model failure and whether it already published output.
pub(crate) struct ModelRequestError {
    error: EgoPulseError,
    output_published: bool,
}

impl ModelRequestError {
    pub(crate) fn into_parts(self) -> (EgoPulseError, bool) {
        (self.error, self.output_published)
    }
}

impl ModelStepError {
    fn new(error: EgoPulseError, retry_messages: Arc<Vec<Message>>) -> Self {
        Self {
            error,
            retry_messages,
        }
    }

    /// Returns the underlying error when the caller has no retry policy.
    pub(crate) fn into_error(self) -> EgoPulseError {
        self.error
    }

    /// Returns the underlying error and the exact messages for the next retry.
    pub(crate) fn into_parts(self) -> (EgoPulseError, Arc<Vec<Message>>) {
        (self.error, self.retry_messages)
    }
}

fn filter_valid_tool_calls(tool_calls: Vec<ToolCall>, log_scope: &str) -> Vec<ToolCall> {
    let mut index_by_id = std::collections::HashMap::new();
    let mut valid = Vec::new();

    for tool_call in tool_calls {
        if tool_call.name.trim().is_empty() || tool_call.id.trim().is_empty() {
            warn!(
                "{log_scope}: skipping malformed tool call (empty name or id): id='{}' name='{}'",
                tool_call.id, tool_call.name
            );
            continue;
        }

        if let Some(index) = index_by_id.get(&tool_call.id).copied() {
            warn!(
                "{log_scope}: replacing duplicate tool call id with latest item: id='{}' name='{}'",
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

async fn send_model_step(request: ModelStepRequest<'_>) -> Result<ModelStep, EgoPulseError> {
    let has_tools = request
        .tools
        .as_ref()
        .is_some_and(|tools| !tools.is_empty());
    let tools_json = request
        .tools
        .as_deref()
        .filter(|tools| !tools.is_empty())
        .and_then(|tools| serde_json::to_string(tools).ok());
    let raw_estimate = estimate_prompt_tokens(
        request.system_prompt,
        &request.messages,
        tools_json.as_deref(),
    );
    let calibration_key = CalibrationKey::new(
        request.llm.provider_name(),
        request.llm.model_name(),
        request.request_kind,
        has_tools,
    );

    let response = request
        .llm
        .send_message_streaming(
            request.system_prompt,
            Arc::clone(&request.messages),
            request.tools.clone(),
            request.on_delta,
        )
        .await
        .inspect_err(|e| {
            warn!(
                error = %e,
                iteration = request.iteration,
                "{}",
                request.send_failure_log
            );
        })?;

    if let Some(usage) = &response.usage {
        request
            .state
            .usage_calibrator
            .record(calibration_key, raw_estimate, usage.input_tokens)
            .await;
        log_llm_usage(&request, usage, raw_estimate, has_tools).await;
    }

    if response.tool_calls.is_empty() {
        return Ok(ModelStep::Final(response));
    }

    let valid_tool_calls = filter_valid_tool_calls(response.tool_calls.clone(), request.log_scope);
    if valid_tool_calls.is_empty() {
        return Ok(ModelStep::MalformedToolCalls(response));
    }

    Ok(ModelStep::ToolCalls(build_assistant_tool_phase(
        response.content,
        response.reasoning_content,
        valid_tool_calls,
    )))
}

/// Sends one model step and recovers once from an empty assistant response.
///
/// The same recovery contract is used by normal Turns and Pulse activations.
/// A parser-level empty response and a parsed response containing only hidden
/// thinking are handled identically: append a runtime guard and ask the model
/// for a visible answer again. Tool phases are not re-executed by this helper;
/// only the LLM request is repeated. If the guarded request fails with a
/// retryable error, the returned error retains the guarded message list so the
/// caller can retry the same request. When the caller has already published
/// output for the iteration, the empty response is returned without sending a
/// guard request because the partial output cannot be safely replayed.
async fn send_model_step_with_empty_retry(
    request: ModelStepRequest<'_>,
    empty_retry_attempted: &mut bool,
    output_published: Option<&AtomicBool>,
) -> Result<ModelStep, ModelStepError> {
    let first_response = send_model_step(request.clone()).await;
    if !model_step_response_is_empty(&first_response) {
        return first_response
            .map_err(|error| ModelStepError::new(error, Arc::clone(&request.messages)));
    }

    if output_published.is_some_and(|published| published.load(Ordering::SeqCst)) {
        return Err(ModelStepError::new(
            empty_response_after_published_output(),
            Arc::clone(&request.messages),
        ));
    }

    if *empty_retry_attempted {
        return Err(ModelStepError::new(
            empty_response_after_retry(),
            Arc::clone(&request.messages),
        ));
    }

    *empty_retry_attempted = true;
    warn!("empty assistant response; injecting runtime guard and retrying once");

    let (assistant_text, reasoning_content) = match &first_response {
        Ok(ModelStep::Final(response)) => (
            response.content.as_str(),
            response.reasoning_content.as_deref(),
        ),
        _ => ("", None),
    };
    let retry_messages = Arc::new(runtime_guard_messages(
        &request.messages,
        assistant_text,
        reasoning_content,
        EMPTY_REPLY_RUNTIME_GUARD,
    ));
    let retry_messages_for_error = Arc::clone(&retry_messages);
    let retry_response = send_model_step(ModelStepRequest {
        messages: retry_messages,
        ..request
    })
    .await;

    if model_step_response_is_empty(&retry_response) {
        Err(ModelStepError::new(
            empty_response_after_retry(),
            retry_messages_for_error,
        ))
    } else {
        retry_response.map_err(|error| ModelStepError::new(error, retry_messages_for_error))
    }
}

fn model_step_response_is_empty(response: &Result<ModelStep, EgoPulseError>) -> bool {
    match response {
        Err(EgoPulseError::Llm(error)) => error.is_empty_response(),
        Ok(ModelStep::Final(response)) => strip_thinking(response.content.trim()).trim().is_empty(),
        Ok(ModelStep::MalformedToolCalls(_)) | Ok(ModelStep::ToolCalls(_)) => false,
        Err(_) => false,
    }
}

fn empty_response_after_retry() -> EgoPulseError {
    EgoPulseError::Llm(LlmError::InvalidResponse(
        "assistant content was empty after retry".to_string(),
    ))
}

fn empty_response_after_published_output() -> EgoPulseError {
    EgoPulseError::Llm(LlmError::InvalidResponse(
        "assistant content was empty after output was published".to_string(),
    ))
}

/// SHA-256 over the fixed model-iteration inputs stored with a durable Turn.
fn model_request_hash(
    system_prompt: &str,
    messages: &[Message],
    tools_json: Option<&str>,
) -> String {
    use sha2::{Digest, Sha256};

    let mut hasher = Sha256::new();
    hasher.update(system_prompt.as_bytes());
    hasher.update(b"\x00");
    if let Ok(json) = serde_json::to_string(messages) {
        hasher.update(json.as_bytes());
    }
    hasher.update(b"\x00");
    if let Some(tools) = tools_json {
        hasher.update(tools.as_bytes());
    }
    format!("{:x}", hasher.finalize())
}

/// Calculates the delay before the next LLM transport retry.
fn llm_retry_backoff(attempt: usize, error: &EgoPulseError) -> Duration {
    if let EgoPulseError::Llm(LlmError::ApiError {
        retry_after_secs: Some(secs),
        ..
    }) = error
    {
        return Duration::from_secs(*secs);
    }
    Duration::from_millis(LLM_RETRY_BASE_BACKOFF_MS * 2u64.pow((attempt - 1) as u32))
}

fn build_assistant_tool_phase(
    content: String,
    reasoning_content: Option<String>,
    tool_calls: Vec<ToolCall>,
) -> AssistantToolPhase {
    let assistant_text = sanitize_assistant_response_text(&content);
    let assistant_preview = summarize_tool_calls_with_content(&assistant_text, &tool_calls);
    let assistant_message = Message {
        role: "assistant".to_string(),
        content: crate::llm::MessageContent::text(assistant_text),
        reasoning_content,
        tool_calls: tool_calls.clone(),
        tool_call_id: None,
    };

    AssistantToolPhase {
        assistant_message,
        assistant_preview,
        tool_calls,
    }
}

async fn log_llm_usage(
    request: &ModelStepRequest<'_>,
    usage: &LlmUsage,
    raw_estimate: usize,
    has_tools: bool,
) {
    let db = request.state.db_for(request.scope);
    let channel = request.caller_channel.to_string();
    let provider = request.llm.provider_name().to_string();
    let model = request.llm.model_name().to_string();
    let chat_id = request.chat_id;
    let request_kind = request.request_kind;
    let failure_message = request.usage_log_failure;
    let input_tokens = usage.input_tokens;
    let output_tokens = usage.output_tokens;
    let estimated_tokens: i64 = raw_estimate.try_into().unwrap_or(0);

    crate::runtime::metrics::inc_llm_tokens_total("input", &provider, input_tokens);
    crate::runtime::metrics::inc_llm_tokens_total("output", &provider, output_tokens);

    let _ = call_blocking(db, move |db| {
        db.log_llm_usage(&crate::storage::LlmUsageLogEntry {
            chat_id,
            caller_channel: &channel,
            provider: &provider,
            model: &model,
            input_tokens,
            output_tokens,
            request_kind,
            estimated_tokens,
            has_tools,
        })
    })
    .await
    .inspect_err(|e| warn!(error = %e, failure_message, "llm usage logging failed"));
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use serial_test::serial;
    use std::sync::Arc;
    use std::time::Duration;

    use super::*;
    use crate::agent_loop::message_format::message_to_text;
    use crate::agent_loop::process_turn;
    use crate::agent_loop::test_support::{
        DeltaThenFailProvider, DeltaThenThinkingProvider, RecordingProvider,
        build_state_with_provider, cli_context,
    };
    use crate::conversation::SurfaceContext;
    use crate::error::EgoPulseError;
    use crate::llm::calibration::{CalibrationKey, DEFAULT_FACTOR};
    use crate::llm::{Message, MessagesResponse, ToolCall, ToolDefinition};
    use crate::storage::call_blocking;

    fn tool_call(id: &str, name: &str, arguments: serde_json::Value) -> ToolCall {
        ToolCall {
            id: id.to_string(),
            name: name.to_string(),
            arguments,
        }
    }

    fn context_with_request_key(session: &str, request_key: &str) -> SurfaceContext {
        let mut context = cli_context(session);
        context.request_key = request_key.to_string();
        context
    }

    #[test]
    fn filter_valid_tool_calls_skips_empty_id_or_name() {
        // Arrange
        let tool_calls = vec![
            tool_call("", "read", json!({"path": "a.txt"})),
            tool_call("call-1", "", json!({"path": "b.txt"})),
            tool_call("call-2", "read", json!({"path": "c.txt"})),
        ];

        // Act
        let valid = filter_valid_tool_calls(tool_calls, "test");

        // Assert
        assert_eq!(
            valid,
            vec![tool_call("call-2", "read", json!({"path": "c.txt"}))]
        );
    }

    #[test]
    fn filter_valid_tool_calls_keeps_latest_duplicate_id_in_original_position() {
        // Arrange
        let tool_calls = vec![
            tool_call("call-1", "read", json!({"path": "old.txt"})),
            tool_call("call-2", "grep", json!({"pattern": "needle"})),
            tool_call("call-1", "read", json!({"path": "new.txt"})),
        ];

        // Act
        let valid = filter_valid_tool_calls(tool_calls, "test");

        // Assert
        assert_eq!(
            valid,
            vec![
                tool_call("call-1", "read", json!({"path": "new.txt"})),
                tool_call("call-2", "grep", json!({"pattern": "needle"})),
            ]
        );
    }

    #[test]
    fn build_assistant_tool_phase_sanitizes_content_and_summarizes_calls() {
        // Arrange
        let tool_calls = vec![tool_call("call-1", "read", json!({"path": "notes.txt"}))];

        // Act
        let phase = build_assistant_tool_phase(
            "<thinking>hidden</thinking>Reading notes".to_string(),
            Some("reasoning".to_string()),
            tool_calls.clone(),
        );

        // Assert
        assert_eq!(
            phase.assistant_message.content.as_text_lossy(),
            "Reading notes"
        );
        assert_eq!(
            phase.assistant_message.reasoning_content.as_deref(),
            Some("reasoning")
        );
        assert_eq!(phase.assistant_message.tool_calls, tool_calls);
        assert_eq!(phase.assistant_preview, "Reading notes [tool_call] read");
    }

    #[test]
    fn llm_retry_backoff_preserves_server_delay() {
        // Arrange
        let long_delay = EgoPulseError::Llm(crate::error::LlmError::ApiError {
            status: reqwest::StatusCode::TOO_MANY_REQUESTS,
            body_preview: "rate limited".to_string(),
            retry_after_secs: Some(3_600),
        });
        let short_delay = EgoPulseError::Llm(crate::error::LlmError::ApiError {
            status: reqwest::StatusCode::TOO_MANY_REQUESTS,
            body_preview: "rate limited".to_string(),
            retry_after_secs: Some(7),
        });

        // Act
        let long_delay = llm_retry_backoff(1, &long_delay);
        let preserved = llm_retry_backoff(1, &short_delay);

        // Assert
        assert_eq!(long_delay, Duration::from_secs(3_600));
        assert_eq!(preserved, Duration::from_secs(7));
    }

    #[tokio::test]
    #[serial]
    async fn process_turn_logs_llm_usage_on_agent_loop() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let provider = RecordingProvider::new(
            vec![Ok(MessagesResponse {
                content: "hello world".to_string(),
                reasoning_content: None,
                tool_calls: vec![],
                usage: Some(crate::llm::LlmUsage {
                    input_tokens: 10,
                    output_tokens: 20,
                }),
            })],
            vec![0],
        );
        let state = build_state_with_provider(
            dir.path().to_str().expect("utf8").to_string(),
            Box::new(provider.clone()),
        );

        // Act
        let reply = process_turn(
            &state.turn_dependencies(),
            &cli_context("usage-log-single"),
            "hi",
        )
        .await
        .expect("process turn");

        // Assert
        assert_eq!(reply, "hello world");

        // Verify LLM resolution: exactly one call with the right system prompt.
        let systems = provider.seen_systems();
        assert_eq!(systems.len(), 1, "should have exactly one LLM call");

        let chat_id = call_blocking(Arc::clone(&state.db), move |db| {
            db.resolve_or_create_chat_id(
                "cli",
                "cli:usage-log-single:agent:default",
                Some("usage-log-single"),
                "cli",
                "default",
            )
        })
        .await
        .expect("chat id");

        // Wait for the spawned logging task to complete
        for _ in 0..20 {
            let (requests, input_tokens, output_tokens, total_tokens) =
                call_blocking(Arc::clone(&state.db), move |db| {
                    db.get_llm_usage_summary(Some(chat_id))
                })
                .await
                .expect("summary");
            if requests > 0 {
                assert_eq!(requests, 1);
                assert_eq!(input_tokens, 10);
                assert_eq!(output_tokens, 20);
                assert_eq!(total_tokens, 30);
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        panic!("usage log was not written within the polling timeout");
    }

    #[tokio::test]
    #[serial]
    async fn send_model_step_records_usage_calibration_before_payload_move() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let provider = RecordingProvider::new(
            vec![Ok(MessagesResponse {
                content: "hello world".to_string(),
                reasoning_content: None,
                tool_calls: Vec::new(),
                usage: Some(crate::llm::LlmUsage {
                    input_tokens: 1_000,
                    output_tokens: 20,
                }),
            })],
            vec![0],
        );
        let state = build_state_with_provider(
            dir.path().to_str().expect("utf8").to_string(),
            Box::new(provider),
        );
        let context = cli_context("usage-calibration");
        let snapshot = state.config_manager.current_blocking();
        let llm = state
            .turn_dependencies()
            .llm_for_context_with_snapshot(&context, &snapshot)
            .expect("llm");
        let tools = Arc::new(vec![ToolDefinition {
            name: "read".to_string(),
            description: "Read a file".to_string(),
            parameters: serde_json::json!({"type": "object"}),
        }]);

        // Act
        let response = send_model_step(ModelStepRequest {
            state: &state.turn_dependencies(),
            llm: llm.as_ref(),
            system_prompt: "system prompt",
            messages: Arc::new(vec![Message::text("user", "hello")]),
            tools: Some(tools),
            chat_id: 1,
            caller_channel: "cli",
            request_kind: "agent_loop",
            usage_log_failure: "llm usage logging failed",
            log_scope: "agent_loop",
            send_failure_log: "LLM send_message failed",
            iteration: 1,
            scope: ConversationScope::Normal,
            on_delta: &ignore_delta,
        })
        .await
        .expect("tool phase response");

        // Assert
        assert!(matches!(response, ModelStep::Final(_)));
        let factor = state
            .usage_calibrator
            .factor(&CalibrationKey::new(
                "test",
                "test-model",
                "agent_loop",
                true,
            ))
            .await;
        assert!(factor > DEFAULT_FACTOR);
    }

    #[tokio::test]
    #[serial]
    async fn empty_response_retries_once_with_shared_runtime_guard() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let provider = RecordingProvider::new(
            vec![
                Err(crate::error::LlmError::InvalidResponse(
                    "assistant content was empty (output_items=1)".to_string(),
                )),
                Ok(MessagesResponse {
                    content: "recovered response".to_string(),
                    reasoning_content: None,
                    tool_calls: Vec::new(),
                    usage: None,
                }),
            ],
            vec![0, 0],
        );
        let observer = provider.clone();
        let state = build_state_with_provider(
            dir.path().to_str().expect("utf8").to_string(),
            Box::new(provider),
        );
        let context = cli_context("empty-response-retry");
        let snapshot = state.config_manager.current_blocking();
        let llm = state
            .turn_dependencies()
            .llm_for_context_with_snapshot(&context, &snapshot)
            .expect("llm");
        let mut retry_attempted = false;

        // Act
        let response = send_model_step_with_empty_retry(
            ModelStepRequest {
                state: &state.turn_dependencies(),
                llm: llm.as_ref(),
                system_prompt: "system prompt",
                messages: Arc::new(vec![Message::text("user", "hello")]),
                tools: None,
                chat_id: 1,
                caller_channel: "cli",
                request_kind: "agent_loop",
                usage_log_failure: "llm usage logging failed",
                log_scope: "agent_loop",
                send_failure_log: "LLM send_message failed",
                iteration: 1,
                scope: ConversationScope::Normal,
                on_delta: &ignore_delta,
            },
            &mut retry_attempted,
            None,
        )
        .await
        .expect("empty response should recover");

        // Assert
        assert!(retry_attempted);
        assert!(matches!(
            response,
            ModelStep::Final(response) if response.content == "recovered response"
        ));
        let seen_messages = observer.seen_messages();
        assert_eq!(seen_messages.len(), 2);
        let retry_message = seen_messages[1].last().expect("runtime guard message");
        assert_eq!(retry_message.role, "user");
        assert!(message_to_text(retry_message).contains("no user-visible text"));
    }

    #[tokio::test]
    #[serial]
    async fn non_empty_invalid_response_is_not_retried_by_empty_guard() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let provider = RecordingProvider::new(
            vec![Err(crate::error::LlmError::InvalidResponse(
                "choices[0] missing".to_string(),
            ))],
            vec![0],
        );
        let observer = provider.clone();
        let state = build_state_with_provider(
            dir.path().to_str().expect("utf8").to_string(),
            Box::new(provider),
        );
        let context = cli_context("non-empty-invalid-response");
        let snapshot = state.config_manager.current_blocking();
        let llm = state
            .turn_dependencies()
            .llm_for_context_with_snapshot(&context, &snapshot)
            .expect("llm");
        let mut retry_attempted = false;

        // Act
        let result = send_model_step_with_empty_retry(
            ModelStepRequest {
                state: &state.turn_dependencies(),
                llm: llm.as_ref(),
                system_prompt: "system prompt",
                messages: Arc::new(vec![Message::text("user", "hello")]),
                tools: None,
                chat_id: 1,
                caller_channel: "cli",
                request_kind: "agent_loop",
                usage_log_failure: "llm usage logging failed",
                log_scope: "agent_loop",
                send_failure_log: "LLM send_message failed",
                iteration: 1,
                scope: ConversationScope::Normal,
                on_delta: &ignore_delta,
            },
            &mut retry_attempted,
            None,
        )
        .await;
        let error = match result {
            Ok(_) => panic!("non-empty invalid response should remain an error"),
            Err(error) => error.into_error(),
        };

        // Assert
        assert!(!retry_attempted);
        assert!(matches!(
            error,
            EgoPulseError::Llm(crate::error::LlmError::InvalidResponse(message))
                if message == "choices[0] missing"
        ));
        assert_eq!(observer.seen_messages().len(), 1);
    }

    #[tokio::test]
    #[serial]
    async fn send_model_step_skips_calibration_when_usage_missing() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let provider = RecordingProvider::new(
            vec![Ok(MessagesResponse {
                content: "hello world".to_string(),
                reasoning_content: None,
                tool_calls: Vec::new(),
                usage: None,
            })],
            vec![0],
        );
        let state = build_state_with_provider(
            dir.path().to_str().expect("utf8").to_string(),
            Box::new(provider),
        );
        let context = cli_context("usage-calibration-none");
        let snapshot = state.config_manager.current_blocking();
        let llm = state
            .turn_dependencies()
            .llm_for_context_with_snapshot(&context, &snapshot)
            .expect("llm");

        // Act
        let response = send_model_step(ModelStepRequest {
            state: &state.turn_dependencies(),
            llm: llm.as_ref(),
            system_prompt: "system prompt",
            messages: Arc::new(vec![Message::text("user", "hello")]),
            tools: None,
            chat_id: 1,
            caller_channel: "cli",
            request_kind: "agent_loop",
            usage_log_failure: "llm usage logging failed",
            log_scope: "agent_loop",
            send_failure_log: "LLM send_message failed",
            iteration: 1,
            scope: ConversationScope::Normal,
            on_delta: &ignore_delta,
        })
        .await
        .expect("tool phase response");

        // Assert
        assert!(matches!(response, ModelStep::Final(_)));
        let factor = state
            .usage_calibrator
            .factor(&CalibrationKey::new(
                "test",
                "test-model",
                "agent_loop",
                false,
            ))
            .await;
        assert_eq!(factor, DEFAULT_FACTOR);
    }

    #[tokio::test]
    #[serial]
    async fn send_model_step_treats_empty_tools_as_no_tools_for_calibration() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let provider = RecordingProvider::new(
            vec![Ok(MessagesResponse {
                content: "hello world".to_string(),
                reasoning_content: None,
                tool_calls: Vec::new(),
                usage: Some(crate::llm::LlmUsage {
                    input_tokens: 1_000,
                    output_tokens: 20,
                }),
            })],
            vec![0],
        );
        let state = build_state_with_provider(
            dir.path().to_str().expect("utf8").to_string(),
            Box::new(provider),
        );
        let context = cli_context("usage-calibration-empty-tools");
        let snapshot = state.config_manager.current_blocking();
        let llm = state
            .turn_dependencies()
            .llm_for_context_with_snapshot(&context, &snapshot)
            .expect("llm");

        // Act
        let response = send_model_step(ModelStepRequest {
            state: &state.turn_dependencies(),
            llm: llm.as_ref(),
            system_prompt: "system prompt",
            messages: Arc::new(vec![Message::text("user", "hello")]),
            tools: Some(Arc::new(Vec::new())),
            chat_id: 1,
            caller_channel: "cli",
            request_kind: "agent_loop",
            usage_log_failure: "llm usage logging failed",
            log_scope: "agent_loop",
            send_failure_log: "LLM send_message failed",
            iteration: 1,
            scope: ConversationScope::Normal,
            on_delta: &ignore_delta,
        })
        .await
        .expect("tool phase response");

        // Assert
        assert!(matches!(response, ModelStep::Final(_)));
        let without_tools = state
            .usage_calibrator
            .factor(&CalibrationKey::new(
                "test",
                "test-model",
                "agent_loop",
                false,
            ))
            .await;
        let with_tools = state
            .usage_calibrator
            .factor(&CalibrationKey::new(
                "test",
                "test-model",
                "agent_loop",
                true,
            ))
            .await;
        assert!(without_tools > DEFAULT_FACTOR);
        assert_eq!(with_tools, DEFAULT_FACTOR);
    }

    #[tokio::test]
    #[serial]
    async fn process_turn_logs_each_iteration() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let relative_path = format!("tests/{}/data.txt", uuid::Uuid::new_v4());
        let provider = RecordingProvider::new(
            vec![
                Ok(MessagesResponse {
                    content: "checking".to_string(),
                    reasoning_content: None,
                    tool_calls: vec![ToolCall {
                        id: "call-iter-1".to_string(),
                        name: "read".to_string(),
                        arguments: serde_json::json!({"path": relative_path}),
                    }],
                    usage: Some(crate::llm::LlmUsage {
                        input_tokens: 15,
                        output_tokens: 25,
                    }),
                }),
                Ok(MessagesResponse {
                    content: "done".to_string(),
                    reasoning_content: None,
                    tool_calls: vec![],
                    usage: Some(crate::llm::LlmUsage {
                        input_tokens: 30,
                        output_tokens: 40,
                    }),
                }),
            ],
            vec![0, 0],
        );
        let state = build_state_with_provider(
            dir.path().to_str().expect("utf8").to_string(),
            Box::new(provider.clone()),
        );
        let workspace = state.config.workspace_dir().expect("workspace_dir");
        let file_path = workspace.join(&relative_path);
        std::fs::create_dir_all(file_path.parent().expect("parent")).expect("dirs");
        std::fs::write(&file_path, "data").expect("file");

        // Act
        let reply = process_turn(
            &state.turn_dependencies(),
            &cli_context("usage-log-multi"),
            "read the file",
        )
        .await
        .expect("process turn");

        // Assert
        assert_eq!(reply, "done");

        let chat_id = call_blocking(Arc::clone(&state.db), move |db| {
            db.resolve_or_create_chat_id(
                "cli",
                "cli:usage-log-multi:agent:default",
                Some("usage-log-multi"),
                "cli",
                "default",
            )
        })
        .await
        .expect("chat id");

        for _ in 0..20 {
            let (requests, input_tokens, output_tokens, total_tokens) =
                call_blocking(Arc::clone(&state.db), move |db| {
                    db.get_llm_usage_summary(Some(chat_id))
                })
                .await
                .expect("summary");
            if requests >= 2 {
                assert_eq!(
                    requests, 2,
                    "should have 2 usage records (one per iteration)"
                );
                assert_eq!(input_tokens, 45);
                assert_eq!(output_tokens, 65);
                assert_eq!(total_tokens, 110);
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        panic!("usage logs were not written within the polling timeout");
    }

    #[tokio::test]
    #[serial]
    async fn retryable_llm_error_is_retried_within_same_iteration() {
        // Arrange: fail twice with 429 (retry_after=0 for a fast test), then
        // succeed on the third attempt.
        let dir = tempfile::tempdir().expect("tempdir");
        let provider = RecordingProvider::new(
            vec![
                Err(crate::error::LlmError::ApiError {
                    status: reqwest::StatusCode::TOO_MANY_REQUESTS,
                    body_preview: "rate limited".to_string(),
                    retry_after_secs: Some(0),
                }),
                Err(crate::error::LlmError::ApiError {
                    status: reqwest::StatusCode::TOO_MANY_REQUESTS,
                    body_preview: "rate limited".to_string(),
                    retry_after_secs: Some(0),
                }),
                Ok(MessagesResponse {
                    content: "recovered".to_string(),
                    reasoning_content: None,
                    tool_calls: Vec::new(),
                    usage: None,
                }),
            ],
            vec![0, 0, 0],
        );
        let state = build_state_with_provider(
            dir.path().to_str().expect("utf8").to_string(),
            Box::new(provider.clone()),
        );
        let context = context_with_request_key("retry", "cli:retry:1");

        // Act
        let reply = process_turn(&state.turn_dependencies(), &context, "hello")
            .await
            .expect("turn");

        // Assert: the same iteration was retried and eventually succeeded.
        assert_eq!(reply, "recovered");
        assert_eq!(
            provider.seen_messages().len(),
            3,
            "LLM must be called 3 times (2 retries + 1 success)"
        );
    }

    #[tokio::test]
    #[serial]
    async fn empty_guard_retry_reuses_guard_messages_after_transient_failure() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let provider = RecordingProvider::new(
            vec![
                Ok(MessagesResponse {
                    content: String::new(),
                    reasoning_content: None,
                    tool_calls: Vec::new(),
                    usage: None,
                }),
                Err(crate::error::LlmError::ApiError {
                    status: reqwest::StatusCode::TOO_MANY_REQUESTS,
                    body_preview: "rate limited".to_string(),
                    retry_after_secs: Some(0),
                }),
                Ok(MessagesResponse {
                    content: "recovered after guarded retry".to_string(),
                    reasoning_content: None,
                    tool_calls: Vec::new(),
                    usage: None,
                }),
            ],
            vec![0, 0, 0],
        );
        let state = build_state_with_provider(
            dir.path().to_str().expect("utf8").to_string(),
            Box::new(provider.clone()),
        );
        let context = context_with_request_key("empty-guard-transient", "cli:empty-guard:1");

        // Act
        let reply = process_turn(&state.turn_dependencies(), &context, "hello")
            .await
            .expect("turn should recover with the guarded request");

        // Assert
        assert_eq!(reply, "recovered after guarded retry");
        let seen_messages = provider.seen_messages();
        assert_eq!(seen_messages.len(), 3);
        let first_guard = seen_messages[1].last().expect("first guard message");
        let second_guard = seen_messages[2].last().expect("second guard message");
        assert!(message_to_text(first_guard).contains("no user-visible text"));
        assert_eq!(message_to_text(first_guard), message_to_text(second_guard));
    }

    #[tokio::test]
    #[serial]
    async fn partial_delta_published_prevents_retry_and_marks_uncertain() {
        // Arrange: a provider that emits a delta then fails with a retryable
        // error. Because output was published, retry is refused.
        let dir = tempfile::tempdir().expect("tempdir");
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let state = build_state_with_provider(
            dir.path().to_str().expect("utf8").to_string(),
            Box::new(DeltaThenFailProvider {
                delta: "partial".to_string(),
                calls: Arc::clone(&calls),
            }),
        );
        let context = context_with_request_key("partial-delta", "cli:partial:1");

        // Act
        let error = process_turn(&state.turn_dependencies(), &context, "hello")
            .await
            .expect_err("should fail after partial delta");

        // Assert: no retry (called once), and the Turn is uncertain because a
        // delta was published.
        assert!(matches!(error, EgoPulseError::Llm(_)));
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "must not retry after a delta was published"
        );
        let chat_id = call_blocking(Arc::clone(&state.db), move |db| {
            db.resolve_or_create_chat_id(
                "cli",
                "cli:partial-delta:agent:default",
                Some("partial-delta"),
                "cli",
                "default",
            )
        })
        .await
        .expect("chat id");
        let (state_str, output_published): (String, i64) = state
            .db
            .get_conn()
            .expect("conn")
            .query_row(
                "SELECT state, output_published FROM turn_runs WHERE chat_id = ?1",
                rusqlite::params![chat_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("turn_run row");
        assert_eq!(state_str, "uncertain", "published output -> uncertain");
        assert_eq!(output_published, 1);
    }

    #[tokio::test]
    #[serial]
    async fn thinking_only_response_after_delta_skips_empty_guard_and_marks_uncertain() {
        // Arrange: the provider publishes a delta, then returns only hidden
        // thinking. The partial output makes a guarded retry unsafe.
        let dir = tempfile::tempdir().expect("tempdir");
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let state = build_state_with_provider(
            dir.path().to_str().expect("utf8").to_string(),
            Box::new(DeltaThenThinkingProvider {
                delta: "partial".to_string(),
                calls: Arc::clone(&calls),
            }),
        );
        let context = context_with_request_key("thinking-only-after-delta", "cli:thinking-only:1");

        // Act
        let error = process_turn(&state.turn_dependencies(), &context, "hello")
            .await
            .expect_err("should fail after published output");

        // Assert: the empty-response guard must not issue a second LLM call,
        // and the Turn must stop as uncertain because output was published.
        assert!(matches!(error, EgoPulseError::Llm(_)));
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "must not retry a thinking-only response after a delta"
        );
        let chat_id = call_blocking(Arc::clone(&state.db), move |db| {
            db.resolve_or_create_chat_id(
                "cli",
                "cli:thinking-only-after-delta:agent:default",
                Some("thinking-only-after-delta"),
                "cli",
                "default",
            )
        })
        .await
        .expect("chat id");
        let (state_str, output_published): (String, i64) = state
            .db
            .get_conn()
            .expect("conn")
            .query_row(
                "SELECT state, output_published FROM turn_runs WHERE chat_id = ?1",
                rusqlite::params![chat_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("turn_run row");
        assert_eq!(state_str, "uncertain", "published output -> uncertain");
        assert_eq!(output_published, 1);
    }

    #[tokio::test]
    #[serial]
    async fn tool_call_saved_prevents_whole_turn_retry_on_later_failure() {
        // Arrange: first response carries a Tool Call; the second LLM call
        // fails. The Turn must end uncertain (output was published via the
        // Tool Call) and the Tool must not be re-executed.
        let dir = tempfile::tempdir().expect("tempdir");
        let relative_path = format!("tests/{}/tc.txt", uuid::Uuid::new_v4());
        let provider = RecordingProvider::new(
            vec![
                Ok(MessagesResponse {
                    content: "reading".to_string(),
                    reasoning_content: None,
                    tool_calls: vec![ToolCall {
                        id: "call-tc-1".to_string(),
                        name: "read".to_string(),
                        arguments: serde_json::json!({"path": relative_path}),
                    }],
                    usage: None,
                }),
                Err(crate::error::LlmError::InvalidResponse("boom".to_string())),
            ],
            vec![0, 0],
        );
        let state = build_state_with_provider(
            dir.path().to_str().expect("utf8").to_string(),
            Box::new(provider),
        );
        let workspace = state.config.workspace_dir().expect("workspace_dir");
        let note_path = workspace.join(&relative_path);
        std::fs::create_dir_all(note_path.parent().expect("parent")).expect("dir");
        std::fs::write(&note_path, "content").expect("file");
        let context = context_with_request_key("tc-fail", "cli:tc-fail:1");

        // Act
        let error = process_turn(&state.turn_dependencies(), &context, "read the note")
            .await
            .expect_err("should fail on the second LLM call");

        // Assert
        assert!(matches!(error, EgoPulseError::Llm(_)));
        let chat_id = call_blocking(Arc::clone(&state.db), move |db| {
            db.resolve_or_create_chat_id(
                "cli",
                "cli:tc-fail:agent:default",
                Some("tc-fail"),
                "cli",
                "default",
            )
        })
        .await
        .expect("chat id");
        let state_str: String = state
            .db
            .get_conn()
            .expect("conn")
            .query_row(
                "SELECT state FROM turn_runs WHERE chat_id = ?1",
                rusqlite::params![chat_id],
                |row| row.get(0),
            )
            .expect("turn_run row");
        assert_eq!(
            state_str, "uncertain",
            "Tool Call was published -> uncertain, not failed"
        );
    }
}
