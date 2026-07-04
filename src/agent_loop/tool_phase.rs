//! Shared LLM tool-phase utilities used by normal turns and Pulse activations.

use std::sync::Arc;

use futures_util::future::join_all;
use tracing::warn;

use crate::agent_loop::ConversationScope;
use crate::agent_loop::compaction::estimate_prompt_tokens;
use crate::agent_loop::formatting::{
    format_tool_result, message_to_text, sanitize_assistant_response_text,
    summarize_tool_calls_with_content, tool_message_content,
};
use crate::channels::utils::text::truncate_by_chars;
use crate::error::EgoPulseError;
use crate::llm::calibration::CalibrationKey;
use crate::llm::{LlmProvider, LlmUsage, Message, MessagesResponse, ToolCall, ToolDefinition};
use crate::runtime::AppState;
use crate::storage::call_blocking;
use crate::tools::{ToolExecutionContext, ToolResult};

pub(crate) const MAX_TOOL_ITERATIONS: usize = 50;
pub(crate) const MAX_TOOL_RESULT_TEXT_CHARS: usize = 200;

type ToolStartHook<'a> = Arc<dyn Fn(&ToolCall) + Send + Sync + 'a>;
type ToolResultHook<'a> = Arc<dyn Fn(&ExecutedToolCall) + Send + Sync + 'a>;

#[derive(Clone)]
pub(crate) struct ToolExecutionHooks<'a> {
    pub(crate) on_start: Option<ToolStartHook<'a>>,
    pub(crate) on_result: Option<ToolResultHook<'a>>,
}

impl ToolExecutionHooks<'_> {
    pub(crate) fn none() -> Self {
        Self {
            on_start: None,
            on_result: None,
        }
    }
}

#[derive(Debug)]
pub(crate) struct ExecutedToolCall {
    pub(crate) tool_call: ToolCall,
    pub(crate) result: ToolResult,
    pub(crate) payload: String,
    pub(crate) message: Message,
    pub(crate) duration_ms: u128,
    pub(crate) timestamp: String,
}

pub(crate) struct AssistantToolPhase {
    pub(crate) assistant_message: Message,
    pub(crate) assistant_preview: String,
    pub(crate) tool_calls: Vec<ToolCall>,
}

pub(crate) struct ToolResultPhase {
    pub(crate) tool_messages: Vec<Message>,
    pub(crate) tool_result_preview: String,
}

pub(crate) fn ignore_delta(_: String) {}

pub(crate) enum ToolPhaseResponse {
    Final(MessagesResponse),
    MalformedToolCalls(MessagesResponse),
    ToolCalls(AssistantToolPhase),
}

pub(crate) struct ToolPhaseRequest<'a> {
    pub(crate) state: &'a AppState,
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

pub(crate) fn filter_valid_tool_calls(tool_calls: Vec<ToolCall>, log_scope: &str) -> Vec<ToolCall> {
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

pub(crate) async fn send_tool_phase_request(
    request: ToolPhaseRequest<'_>,
) -> Result<ToolPhaseResponse, EgoPulseError> {
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
            request.messages,
            request.tools,
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
        log_llm_usage(
            request.state,
            request.scope,
            request.chat_id,
            request.caller_channel,
            request.llm,
            usage,
            request.request_kind,
            request.usage_log_failure,
        );
    }

    if response.tool_calls.is_empty() {
        return Ok(ToolPhaseResponse::Final(response));
    }

    let valid_tool_calls = filter_valid_tool_calls(response.tool_calls.clone(), request.log_scope);
    if valid_tool_calls.is_empty() {
        return Ok(ToolPhaseResponse::MalformedToolCalls(response));
    }

    Ok(ToolPhaseResponse::ToolCalls(build_assistant_tool_phase(
        response.content,
        response.reasoning_content,
        valid_tool_calls,
    )))
}

pub(crate) fn build_assistant_tool_phase(
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

pub(crate) fn build_tool_result_phase(outcomes: Vec<ExecutedToolCall>) -> ToolResultPhase {
    let tool_messages = outcomes
        .into_iter()
        .map(|outcome| outcome.message)
        .collect::<Vec<_>>();
    let tool_result_preview = summarize_tool_result_messages(&tool_messages);
    ToolResultPhase {
        tool_messages,
        tool_result_preview,
    }
}

fn summarize_tool_result_messages(tool_messages: &[Message]) -> String {
    let joined = tool_messages
        .iter()
        .map(message_to_text)
        .collect::<Vec<_>>()
        .join("\n");
    truncate_by_chars(&joined, MAX_TOOL_RESULT_TEXT_CHARS)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn log_llm_usage(
    state: &AppState,
    scope: ConversationScope,
    chat_id: i64,
    caller_channel: &str,
    llm: &dyn LlmProvider,
    usage: &LlmUsage,
    request_kind: &'static str,
    failure_message: &'static str,
) {
    let db = Arc::clone(state.db_for(scope));
    let channel = caller_channel.to_string();
    let provider = llm.provider_name().to_string();
    let model = llm.model_name().to_string();
    let input_tokens = usage.input_tokens;
    let output_tokens = usage.output_tokens;

    crate::runtime::metrics::inc_llm_tokens_total("input", &provider, input_tokens);
    crate::runtime::metrics::inc_llm_tokens_total("output", &provider, output_tokens);

    tokio::spawn(async move {
        let _ = call_blocking(db, move |db| {
            db.log_llm_usage(&crate::storage::LlmUsageLogEntry {
                chat_id,
                caller_channel: &channel,
                provider: &provider,
                model: &model,
                input_tokens,
                output_tokens,
                request_kind,
            })
        })
        .await
        .inspect_err(|e| warn!(error = %e, failure_message, "llm usage logging failed"));
    });
}

pub(crate) async fn execute_tool_calls<'a>(
    state: &AppState,
    tool_context: &ToolExecutionContext,
    valid_tool_calls: Vec<ToolCall>,
    hooks: ToolExecutionHooks<'a>,
) -> Result<Vec<ExecutedToolCall>, EgoPulseError> {
    if valid_tool_calls.is_empty() {
        return Ok(Vec::new());
    }

    let read_only_flags = read_only_flags(state, &valid_tool_calls).await;
    let mut outcomes = Vec::with_capacity(valid_tool_calls.len());
    let mut cursor = 0;

    while cursor < valid_tool_calls.len() {
        if read_only_flags[cursor] {
            let block_start = cursor;
            while cursor < valid_tool_calls.len() && read_only_flags[cursor] {
                cursor += 1;
            }
            let block_futures = valid_tool_calls[block_start..cursor]
                .iter()
                .cloned()
                .map(|tool_call| execute_single_tool(state, tool_context, tool_call, hooks.clone()))
                .collect::<Vec<_>>();
            let block_results = join_all(block_futures).await;
            for result in block_results {
                outcomes.push(result?);
            }
        } else {
            outcomes.push(
                execute_single_tool(
                    state,
                    tool_context,
                    valid_tool_calls[cursor].clone(),
                    hooks.clone(),
                )
                .await?,
            );
            cursor += 1;
        }
    }

    Ok(outcomes)
}

async fn read_only_flags(state: &AppState, valid_tool_calls: &[ToolCall]) -> Vec<bool> {
    let mut flags = Vec::with_capacity(valid_tool_calls.len());
    for tool_call in valid_tool_calls {
        flags.push(state.tools.is_read_only(&tool_call.name).await);
    }
    flags
}

async fn execute_single_tool(
    state: &AppState,
    tool_context: &ToolExecutionContext,
    tool_call: ToolCall,
    hooks: ToolExecutionHooks<'_>,
) -> Result<ExecutedToolCall, EgoPulseError> {
    if let Some(on_start) = &hooks.on_start {
        on_start(&tool_call);
    }

    let tool_start = std::time::Instant::now();
    let result = state
        .tools
        .execute(&tool_call.name, tool_call.arguments.clone(), tool_context)
        .await;
    let duration_ms = tool_start.elapsed().as_millis();
    let payload = format_tool_result(&tool_call, &result);
    let timestamp = chrono::Utc::now().to_rfc3339();

    crate::runtime::metrics::inc_tool_calls_total(
        &tool_call.name,
        if result.is_error { "error" } else { "ok" },
    );

    let message = Message {
        role: "tool".to_string(),
        content: tool_message_content(&payload, &result),
        reasoning_content: None,
        tool_calls: Vec::new(),
        tool_call_id: Some(tool_call.id.clone()),
    };

    let outcome = ExecutedToolCall {
        tool_call,
        result,
        payload,
        message,
        duration_ms,
        timestamp,
    };

    if let Some(on_result) = &hooks.on_result {
        on_result(&outcome);
    }

    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use serial_test::serial;
    use std::sync::Arc;

    use super::*;
    use crate::agent_loop::process_turn;
    use crate::agent_loop::turn::{RecordingProvider, build_state_with_provider, cli_context};
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
    fn build_tool_result_phase_preserves_order_and_previews_results() {
        // Arrange
        let first = ExecutedToolCall {
            tool_call: tool_call("call-1", "read", json!({"path": "a.txt"})),
            result: crate::tools::ToolResult::success("alpha".to_string()),
            payload: json!({"tool": "read", "status": "success", "result": "alpha"}).to_string(),
            message: Message {
                role: "tool".to_string(),
                content: crate::llm::MessageContent::text(
                    json!({"tool": "read", "status": "success", "result": "alpha"}).to_string(),
                ),
                reasoning_content: None,
                tool_calls: Vec::new(),
                tool_call_id: Some("call-1".to_string()),
            },
            duration_ms: 1,
            timestamp: "2026-05-31T00:00:00Z".to_string(),
        };
        let second = ExecutedToolCall {
            tool_call: tool_call("call-2", "grep", json!({"pattern": "beta"})),
            result: crate::tools::ToolResult::success("beta".to_string()),
            payload: json!({"tool": "grep", "status": "success", "result": "beta"}).to_string(),
            message: Message {
                role: "tool".to_string(),
                content: crate::llm::MessageContent::text(
                    json!({"tool": "grep", "status": "success", "result": "beta"}).to_string(),
                ),
                reasoning_content: None,
                tool_calls: Vec::new(),
                tool_call_id: Some("call-2".to_string()),
            },
            duration_ms: 2,
            timestamp: "2026-05-31T00:00:01Z".to_string(),
        };

        // Act
        let phase = build_tool_result_phase(vec![first, second]);

        // Assert
        assert_eq!(phase.tool_messages.len(), 2);
        assert_eq!(
            phase.tool_messages[0].tool_call_id.as_deref(),
            Some("call-1")
        );
        assert_eq!(
            phase.tool_messages[1].tool_call_id.as_deref(),
            Some("call-2")
        );
        assert!(phase.tool_result_preview.contains("alpha"));
        assert!(phase.tool_result_preview.contains("beta"));
    }

    // -----------------------------------------------------------------------
    // Tool execution strategy
    // -----------------------------------------------------------------------

    #[tokio::test]
    #[serial]
    async fn parallel_read_only_tools_execute_concurrently() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file_a = format!("tests/{}/a.txt", uuid::Uuid::new_v4());
        let file_b = format!("tests/{}/b.txt", uuid::Uuid::new_v4());
        let provider = RecordingProvider::new(
            vec![
                Ok(MessagesResponse {
                    content: "Reading.".to_string(),
                    reasoning_content: None,
                    tool_calls: vec![
                        ToolCall {
                            id: "call-1".to_string(),
                            name: "read".to_string(),
                            arguments: serde_json::json!({"path": file_a.clone()}),
                        },
                        ToolCall {
                            id: "call-2".to_string(),
                            name: "read".to_string(),
                            arguments: serde_json::json!({"path": file_b.clone()}),
                        },
                    ],
                    usage: None,
                }),
                Ok(MessagesResponse {
                    content: "Done.".to_string(),
                    reasoning_content: None,
                    tool_calls: Vec::new(),
                    usage: None,
                }),
            ],
            vec![0, 0],
        );
        let state = build_state_with_provider(
            dir.path().to_str().expect("utf8").to_string(),
            Box::new(provider),
        );
        let workspace = state.config.workspace_dir().expect("workspace_dir");
        for path in &[&file_a, &file_b] {
            let full = workspace.join(path);
            std::fs::create_dir_all(full.parent().expect("parent")).expect("dir");
            std::fs::write(&full, format!("content of {}", path)).expect("write");
        }

        let reply = process_turn(&state, &cli_context("parallel-read"), "read both")
            .await
            .expect("turn");
        assert_eq!(reply, "Done.");

        let chat_id = call_blocking(Arc::clone(&state.db), move |db| {
            db.resolve_or_create_chat_id(
                "cli",
                "cli:parallel-read:agent:default",
                Some("parallel-read"),
                "cli",
                "default",
            )
        })
        .await
        .expect("chat id");
        let tool_calls = call_blocking(Arc::clone(&state.db), move |db| {
            db.get_tool_calls_for_chat(chat_id)
        })
        .await
        .expect("tool calls");
        assert_eq!(tool_calls.len(), 2);
        assert!(tool_calls.iter().all(|tc| tc.tool_output.is_some()));
    }

    #[tokio::test]
    #[serial]
    async fn mixed_tools_execute_sequentially() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file_a = format!("tests/{}/a.txt", uuid::Uuid::new_v4());
        let provider = RecordingProvider::new(
            vec![
                Ok(MessagesResponse {
                    content: "Mixed.".to_string(),
                    reasoning_content: None,
                    tool_calls: vec![
                        ToolCall {
                            id: "call-1".to_string(),
                            name: "read".to_string(),
                            arguments: serde_json::json!({"path": file_a.clone()}),
                        },
                        ToolCall {
                            id: "call-2".to_string(),
                            name: "bash".to_string(),
                            arguments: serde_json::json!({"command": "echo ok"}),
                        },
                    ],
                    usage: None,
                }),
                Ok(MessagesResponse {
                    content: "Done.".to_string(),
                    reasoning_content: None,
                    tool_calls: Vec::new(),
                    usage: None,
                }),
            ],
            vec![0, 0],
        );
        let state = build_state_with_provider(
            dir.path().to_str().expect("utf8").to_string(),
            Box::new(provider),
        );
        let workspace = state.config.workspace_dir().expect("workspace_dir");
        let full = workspace.join(&file_a);
        std::fs::create_dir_all(full.parent().expect("parent")).expect("dir");
        std::fs::write(&full, "hello").expect("write");

        let reply = process_turn(&state, &cli_context("mixed-tools"), "mixed")
            .await
            .expect("turn");
        assert_eq!(reply, "Done.");

        let chat_id = call_blocking(Arc::clone(&state.db), move |db| {
            db.resolve_or_create_chat_id(
                "cli",
                "cli:mixed-tools:agent:default",
                Some("mixed-tools"),
                "cli",
                "default",
            )
        })
        .await
        .expect("chat id");
        let tool_calls = call_blocking(Arc::clone(&state.db), move |db| {
            db.get_tool_calls_for_chat(chat_id)
        })
        .await
        .expect("tool calls");
        assert_eq!(tool_calls.len(), 2);
        assert!(tool_calls.iter().all(|tc| tc.tool_output.is_some()));
    }

    // -----------------------------------------------------------------------
    // Usage logging
    // -----------------------------------------------------------------------

    #[tokio::test]
    #[serial]
    async fn process_turn_logs_llm_usage_on_agent_loop() {
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

        let reply = process_turn(&state, &cli_context("usage-log-single"), "hi")
            .await
            .expect("process turn");
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
    async fn send_tool_phase_request_records_usage_calibration_before_payload_move() {
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
        let llm = state.llm_for_context(&context).expect("llm");
        let tools = Arc::new(vec![ToolDefinition {
            name: "read".to_string(),
            description: "Read a file".to_string(),
            parameters: serde_json::json!({"type": "object"}),
        }]);

        let response = send_tool_phase_request(ToolPhaseRequest {
            state: &state,
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

        assert!(matches!(response, ToolPhaseResponse::Final(_)));
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
    async fn send_tool_phase_request_skips_calibration_when_usage_missing() {
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
        let llm = state.llm_for_context(&context).expect("llm");

        let response = send_tool_phase_request(ToolPhaseRequest {
            state: &state,
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

        assert!(matches!(response, ToolPhaseResponse::Final(_)));
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
    async fn send_tool_phase_request_treats_empty_tools_as_no_tools_for_calibration() {
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
        let llm = state.llm_for_context(&context).expect("llm");

        let response = send_tool_phase_request(ToolPhaseRequest {
            state: &state,
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

        assert!(matches!(response, ToolPhaseResponse::Final(_)));
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

        let reply = process_turn(&state, &cli_context("usage-log-multi"), "read the file")
            .await
            .expect("process turn");
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

    // -----------------------------------------------------------------------
    // Order-preserving partial parallelization
    // -----------------------------------------------------------------------

    #[tokio::test]
    #[serial]
    async fn parallel_read_only_block() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file_a = format!("tests/{}/a.txt", uuid::Uuid::new_v4());
        let file_b = format!("tests/{}/b.txt", uuid::Uuid::new_v4());
        let file_c = format!("tests/{}/c.txt", uuid::Uuid::new_v4());
        let provider = RecordingProvider::new(
            vec![
                Ok(MessagesResponse {
                    content: "Mixed read/write.".to_string(),
                    reasoning_content: None,
                    tool_calls: vec![
                        ToolCall {
                            id: "call-r1".to_string(),
                            name: "read".to_string(),
                            arguments: serde_json::json!({"path": file_a.clone()}),
                        },
                        ToolCall {
                            id: "call-r2".to_string(),
                            name: "read".to_string(),
                            arguments: serde_json::json!({"path": file_b.clone()}),
                        },
                        ToolCall {
                            id: "call-b1".to_string(),
                            name: "bash".to_string(),
                            arguments: serde_json::json!({"command": "echo ok"}),
                        },
                        ToolCall {
                            id: "call-r3".to_string(),
                            name: "read".to_string(),
                            arguments: serde_json::json!({"path": file_c.clone()}),
                        },
                    ],
                    usage: None,
                }),
                Ok(MessagesResponse {
                    content: "Done.".to_string(),
                    reasoning_content: None,
                    tool_calls: Vec::new(),
                    usage: None,
                }),
            ],
            vec![0, 0],
        );
        let state = build_state_with_provider(
            dir.path().to_str().expect("utf8").to_string(),
            Box::new(provider),
        );
        let workspace = state.config.workspace_dir().expect("workspace_dir");
        for path in &[&file_a, &file_b, &file_c] {
            let full = workspace.join(path);
            std::fs::create_dir_all(full.parent().expect("parent")).expect("dir");
            std::fs::write(&full, format!("content of {}", path)).expect("write");
        }

        let reply = process_turn(&state, &cli_context("partial-parallel"), "mixed")
            .await
            .expect("turn");
        assert_eq!(reply, "Done.");

        let chat_id = call_blocking(Arc::clone(&state.db), move |db| {
            db.resolve_or_create_chat_id(
                "cli",
                "cli:partial-parallel:agent:default",
                Some("partial-parallel"),
                "cli",
                "default",
            )
        })
        .await
        .expect("chat id");
        let tool_calls = call_blocking(Arc::clone(&state.db), move |db| {
            db.get_tool_calls_for_chat(chat_id)
        })
        .await
        .expect("tool calls");
        assert_eq!(tool_calls.len(), 4);
        assert!(tool_calls.iter().all(|tc| tc.tool_output.is_some()));
        assert_eq!(tool_calls[0].tool_name, "read");
        assert_eq!(tool_calls[1].tool_name, "read");
        assert_eq!(tool_calls[2].tool_name, "bash");
        assert_eq!(tool_calls[3].tool_name, "read");
    }

    #[tokio::test]
    #[serial]
    async fn sequential_write_tools() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file_a = format!("tests/{}/seq.txt", uuid::Uuid::new_v4());
        let provider = RecordingProvider::new(
            vec![
                Ok(MessagesResponse {
                    content: "Writing.".to_string(),
                    reasoning_content: None,
                    tool_calls: vec![
                        ToolCall {
                            id: "call-b1".to_string(),
                            name: "bash".to_string(),
                            arguments: serde_json::json!({"command": "echo step1"}),
                        },
                        ToolCall {
                            id: "call-w1".to_string(),
                            name: "write".to_string(),
                            arguments: serde_json::json!({
                                "path": file_a.clone(),
                                "content": "hello"
                            }),
                        },
                    ],
                    usage: None,
                }),
                Ok(MessagesResponse {
                    content: "Done.".to_string(),
                    reasoning_content: None,
                    tool_calls: Vec::new(),
                    usage: None,
                }),
            ],
            vec![0, 0],
        );
        let state = build_state_with_provider(
            dir.path().to_str().expect("utf8").to_string(),
            Box::new(provider),
        );
        let workspace = state.config.workspace_dir().expect("workspace_dir");
        std::fs::create_dir_all(
            workspace
                .join("tests")
                .join(uuid::Uuid::new_v4().to_string()),
        )
        .expect("dir");

        let reply = process_turn(&state, &cli_context("seq-write"), "write it")
            .await
            .expect("turn");
        assert_eq!(reply, "Done.");

        let chat_id = call_blocking(Arc::clone(&state.db), move |db| {
            db.resolve_or_create_chat_id(
                "cli",
                "cli:seq-write:agent:default",
                Some("seq-write"),
                "cli",
                "default",
            )
        })
        .await
        .expect("chat id");
        let tool_calls = call_blocking(Arc::clone(&state.db), move |db| {
            db.get_tool_calls_for_chat(chat_id)
        })
        .await
        .expect("tool calls");
        assert_eq!(tool_calls.len(), 2);
        assert_eq!(tool_calls[0].tool_name, "bash");
        assert_eq!(tool_calls[1].tool_name, "write");
        assert!(tool_calls.iter().all(|tc| tc.tool_output.is_some()));
    }

    #[tokio::test]
    #[serial]
    async fn preserves_transcript_order() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file_a = format!("tests/{}/order.txt", uuid::Uuid::new_v4());
        let file_b = format!("tests/{}/order2.txt", uuid::Uuid::new_v4());
        let provider = RecordingProvider::new(
            vec![
                Ok(MessagesResponse {
                    content: "Mixed.".to_string(),
                    reasoning_content: None,
                    tool_calls: vec![
                        ToolCall {
                            id: "call-r1".to_string(),
                            name: "read".to_string(),
                            arguments: serde_json::json!({"path": file_a.clone()}),
                        },
                        ToolCall {
                            id: "call-b1".to_string(),
                            name: "bash".to_string(),
                            arguments: serde_json::json!({"command": "echo step2"}),
                        },
                        ToolCall {
                            id: "call-r2".to_string(),
                            name: "read".to_string(),
                            arguments: serde_json::json!({"path": file_b.clone()}),
                        },
                    ],
                    usage: None,
                }),
                Ok(MessagesResponse {
                    content: "Done.".to_string(),
                    reasoning_content: None,
                    tool_calls: Vec::new(),
                    usage: None,
                }),
            ],
            vec![0, 0],
        );
        let state = build_state_with_provider(
            dir.path().to_str().expect("utf8").to_string(),
            Box::new(provider.clone()),
        );
        let workspace = state.config.workspace_dir().expect("workspace_dir");
        for path in &[&file_a, &file_b] {
            let full = workspace.join(path);
            std::fs::create_dir_all(full.parent().expect("parent")).expect("dir");
            std::fs::write(&full, format!("content of {}", path)).expect("write");
        }

        let reply = process_turn(&state, &cli_context("transcript-order"), "ordered")
            .await
            .expect("turn");
        assert_eq!(reply, "Done.");

        let seen = provider.seen_messages();
        assert_eq!(seen.len(), 2, "should have 2 LLM calls");
        let second_call = &seen[1];
        let tool_msgs: Vec<_> = second_call.iter().filter(|m| m.role == "tool").collect();
        assert_eq!(tool_msgs.len(), 3);
        assert_eq!(
            tool_msgs[0].tool_call_id.as_deref(),
            Some("call-r1"),
            "first tool message must match first tool call"
        );
        assert_eq!(
            tool_msgs[1].tool_call_id.as_deref(),
            Some("call-b1"),
            "second tool message must match second tool call"
        );
        assert_eq!(
            tool_msgs[2].tool_call_id.as_deref(),
            Some("call-r2"),
            "third tool message must match third tool call"
        );
    }
}
