//! Safe execution of validated tool calls and durable tool outcomes.

use std::sync::Arc;

use crate::agent_loop::TurnRuntime;
use crate::agent_loop::formatting::{format_tool_result, message_to_text, tool_message_content};
use crate::channels::utils::text::truncate_by_chars;
use crate::error::EgoPulseError;
use crate::llm::{Message, ToolCall};
use crate::storage::call_blocking;
use crate::storage::{ClaimOutcome, ClaimParams, canonical_tool_input, input_hash};
use crate::tools::{ToolExecutionContext, ToolResult};
use futures_util::future::join_all;

/// Maximum number of characters included in tool-result event previews.
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
}
pub(crate) struct ToolResultPhase {
    pub(crate) tool_messages: Vec<Message>,
    pub(crate) tool_result_preview: String,
}

/// Executes validated tool calls while preserving ordering and idempotency.
pub(crate) struct ToolExecutor<'a> {
    runtime: &'a TurnRuntime,
    context: &'a ToolExecutionContext,
    hooks: ToolExecutionHooks<'a>,
}

impl<'a> ToolExecutor<'a> {
    /// Creates an executor for one assistant tool-call phase.
    pub(crate) fn new(
        runtime: &'a TurnRuntime,
        context: &'a ToolExecutionContext,
        hooks: ToolExecutionHooks<'a>,
    ) -> Self {
        Self {
            runtime,
            context,
            hooks,
        }
    }

    /// Executes the supplied calls and returns their tool result messages.
    pub(crate) async fn execute(
        &self,
        assistant_message_id: &str,
        tool_calls: Vec<ToolCall>,
    ) -> Result<Vec<ExecutedToolCall>, EgoPulseError> {
        execute_tool_calls(
            self.runtime,
            self.context,
            assistant_message_id,
            tool_calls,
            self.hooks.clone(),
        )
        .await
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
async fn execute_tool_calls<'a>(
    state: &TurnRuntime,
    tool_context: &ToolExecutionContext,
    assistant_message_id: &str,
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
                .map(|tool_call| {
                    execute_single_tool(
                        state,
                        tool_context,
                        assistant_message_id,
                        tool_call,
                        hooks.clone(),
                    )
                })
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
                    assistant_message_id,
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

async fn read_only_flags(state: &TurnRuntime, valid_tool_calls: &[ToolCall]) -> Vec<bool> {
    let mut flags = Vec::with_capacity(valid_tool_calls.len());
    for tool_call in valid_tool_calls {
        flags.push(state.tools.is_read_only(&tool_call.name).await);
    }
    flags
}

async fn execute_single_tool(
    state: &TurnRuntime,
    tool_context: &ToolExecutionContext,
    assistant_message_id: &str,
    tool_call: ToolCall,
    hooks: ToolExecutionHooks<'_>,
) -> Result<ExecutedToolCall, EgoPulseError> {
    if let Some(on_start) = &hooks.on_start {
        on_start(&tool_call);
    }

    let tool_start = std::time::Instant::now();
    let is_read_only = state.tools.is_read_only(&tool_call.name).await;

    // Claim the ledger slot before executing so a Tool call is never run
    // before its durable row exists. Read-only Tools skip the ledger because
    // they have no side effects and may be safely retried after a crash;
    // this avoids SQLite write contention during parallel execution.
    let claim = if is_read_only {
        ClaimOutcome::Acquired
    } else {
        claim_tool_slot(state, tool_context, assistant_message_id, &tool_call).await?
    };

    let (result, payload, executed) = match claim {
        ClaimOutcome::Acquired => {
            // Bind this execution's Tool Call ID into the context so tools
            // (e.g. `agent_send`) can build per-call idempotency keys.
            let mut exec_context = tool_context.clone();
            exec_context.tool_call_id = tool_call.id.clone();
            let result = state
                .tools
                .execute(&tool_call.name, tool_call.arguments.clone(), &exec_context)
                .await;
            let payload = format_tool_result(&tool_call, &result);
            if !is_read_only {
                record_tool_outcome(state, tool_context, &tool_call, &result, &payload).await?;
            }
            (result, payload, true)
        }
        ClaimOutcome::Reused { tool_output } => {
            // A prior execution already succeeded; return the stored output
            // without re-running the Tool.
            let result = ToolResult::success(tool_output.clone());
            (result, tool_output, false)
        }
        ClaimOutcome::Blocked { state: tool_state } => {
            // The ledger forbids execution (failed / uncertain / in flight).
            let result = ToolResult::error(format!("tool call blocked: ledger state={tool_state}"));
            let payload = format_tool_result(&tool_call, &result);
            (result, payload, false)
        }
    };

    let duration_ms = tool_start.elapsed().as_millis();

    if executed {
        crate::runtime::metrics::inc_tool_calls_total(
            &tool_call.name,
            if result.is_error { "error" } else { "ok" },
        );
    }

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
    };

    if let Some(on_result) = &hooks.on_result {
        on_result(&outcome);
    }

    Ok(outcome)
}

/// Claims a Tool execution slot in the `tool_calls` ledger.
///
/// The canonical input and its hash are computed before any DB write so the
/// retry identity is fixed at claim time. Idempotency classification comes
/// from the [`ToolRegistry`] (derived from each tool's read-only declaration).
async fn claim_tool_slot(
    state: &TurnRuntime,
    tool_context: &ToolExecutionContext,
    assistant_message_id: &str,
    tool_call: &ToolCall,
) -> Result<ClaimOutcome, EgoPulseError> {
    let canonical = canonical_tool_input(&tool_call.name, &tool_call.arguments);
    let hash = input_hash(&canonical);
    let tool_input = tool_call.arguments.to_string();
    let class = state.tools.idempotency_class(&tool_call.name).await;
    let key = state
        .tools
        .idempotency_key(&tool_call.name, &tool_call.arguments)
        .await;
    let turn_id = tool_context.turn_id.clone();
    let chat_id = tool_context.chat_id;
    let message_id = assistant_message_id.to_string();
    let tool_call_id = tool_call.id.clone();
    let tool_name = tool_call.name.clone();
    let hash_for_closure = hash;
    let tool_input_for_closure = tool_input;
    let key_for_closure = key;
    Ok(call_blocking(state.db_for(tool_context.scope), move |db| {
        db.claim_tool_execution(ClaimParams {
            turn_id: &turn_id,
            chat_id,
            message_id: &message_id,
            tool_call_id: &tool_call_id,
            tool_name: &tool_name,
            tool_input: &tool_input_for_closure,
            input_hash: &hash_for_closure,
            idempotency_class: class,
            idempotency_key: key_for_closure.as_deref(),
        })
    })
    .await?)
}

/// Records the Tool execution outcome (success or failure) in the ledger.
///
/// `payload` and `result.content` are already sanitized by [`ToolRegistry::execute`],
/// so no secret reaches the persisted `tool_output` / `error_message`.
async fn record_tool_outcome(
    state: &TurnRuntime,
    tool_context: &ToolExecutionContext,
    tool_call: &ToolCall,
    result: &ToolResult,
    payload: &str,
) -> Result<(), EgoPulseError> {
    let turn_id = tool_context.turn_id.clone();
    let tool_call_id = tool_call.id.clone();
    if result.is_error {
        let error_message = result.content.clone();
        Ok(call_blocking(state.db_for(tool_context.scope), move |db| {
            db.record_tool_failure(&turn_id, &tool_call_id, "tool_error", &error_message)
        })
        .await?)
    } else {
        let payload = payload.to_string();
        Ok(call_blocking(state.db_for(tool_context.scope), move |db| {
            db.record_tool_success(&turn_id, &tool_call_id, &payload)
        })
        .await?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tool_call(id: &str, name: &str, arguments: serde_json::Value) -> ToolCall {
        ToolCall {
            id: id.to_string(),
            name: name.to_string(),
            arguments,
        }
    }

    #[test]
    fn build_tool_result_phase_preserves_order_and_previews_results() {
        // Arrange
        let first = ExecutedToolCall {
            tool_call: tool_call("call-1", "read", json!({"path": "a.txt"})),
            result: ToolResult::success("alpha".to_string()),
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
        };
        let second = ExecutedToolCall {
            tool_call: tool_call("call-2", "grep", json!({"pattern": "beta"})),
            result: ToolResult::success("beta".to_string()),
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
}

#[cfg(test)]
mod integration_tests {
    use serial_test::serial;
    use std::sync::Arc;

    use crate::agent_loop::process_turn;
    use crate::agent_loop::turn::{RecordingProvider, build_state_with_provider, cli_context};
    use crate::llm::{MessagesResponse, ToolCall};
    use crate::storage::call_blocking;
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

        let reply = process_turn(
            &state.turn_runtime(),
            &cli_context("parallel-read"),
            "read both",
        )
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
        assert_eq!(tool_calls.len(), 0, "read-only tools skip the ledger");
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

        let reply = process_turn(&state.turn_runtime(), &cli_context("mixed-tools"), "mixed")
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
        assert_eq!(tool_calls.len(), 1, "only non-idempotent/bash is persisted");
        assert_eq!(tool_calls[0].tool_name, "bash");
        assert!(tool_calls[0].tool_output.is_some());
    }

    // -----------------------------------------------------------------------
    // Usage logging
    // -----------------------------------------------------------------------

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

        let reply = process_turn(
            &state.turn_runtime(),
            &cli_context("partial-parallel"),
            "mixed",
        )
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
        assert_eq!(
            tool_calls.len(),
            1,
            "only bash is persisted; read-only tools skip the ledger"
        );
        assert_eq!(tool_calls[0].tool_name, "bash");
        assert!(tool_calls[0].tool_output.is_some());
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

        let reply = process_turn(&state.turn_runtime(), &cli_context("seq-write"), "write it")
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

        let reply = process_turn(
            &state.turn_runtime(),
            &cli_context("transcript-order"),
            "ordered",
        )
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
