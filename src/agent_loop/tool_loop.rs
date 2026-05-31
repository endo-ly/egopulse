//! Shared LLM tool-loop utilities used by normal turns and Pulse activations.

use std::sync::Arc;

use futures_util::future::join_all;
use tracing::warn;

use crate::agent_loop::formatting::{format_tool_result, tool_message_content};
use crate::error::EgoPulseError;
use crate::llm::{LlmProvider, LlmUsage, Message, ToolCall};
use crate::runtime::AppState;
use crate::storage::call_blocking;
use crate::tools::{ToolExecutionContext, ToolResult};

pub(crate) const MAX_TOOL_ITERATIONS: usize = 50;

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

pub(crate) fn log_llm_usage(
    state: &AppState,
    chat_id: i64,
    caller_channel: &str,
    llm: &dyn LlmProvider,
    usage: &LlmUsage,
    request_kind: &'static str,
    failure_message: &'static str,
) {
    let db = Arc::clone(&state.db);
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

    use super::*;

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
}
