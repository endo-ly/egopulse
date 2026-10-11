//! Safe execution of validated tool calls and durable tool outcomes.

use std::sync::Arc;

use crate::agent_loop::TurnDependencies;
use crate::agent_loop::message_format::{
    format_tool_result, message_to_text, tool_message_content,
};
use crate::channels::utils::text::truncate_by_chars;
use crate::error::EgoPulseError;
use crate::llm::{Message, ToolCall};
use crate::storage::call_blocking;
use crate::storage::{
    ClaimOutcome, ClaimParams, ReadOnlyToolOutcome, canonical_tool_input, input_hash,
};
use crate::tools::{ToolExecutionContext, ToolResult};
use futures_util::future::join_all;

/// Maximum number of characters included in tool-result event previews.
pub(crate) const MAX_TOOL_RESULT_TEXT_CHARS: usize = 200;

/// Number of `guard_denied` tool results allowed per Turn / Pulse activation.
///
/// Reaching the limit disables all further Tool execution for the run and
/// injects a wrap-up guard so the model reports instead of looking for workarounds.
pub(crate) const GUARD_DENIAL_LIMIT: usize = 3;

/// Counts security-guard denials for one run and flags when the limit is hit.
#[derive(Default)]
pub(crate) struct GuardDenialTracker {
    count: std::sync::atomic::AtomicUsize,
    limit_reached: std::sync::atomic::AtomicBool,
}

impl GuardDenialTracker {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Records one denial. Returns `true` only on the call that reaches the limit.
    pub(crate) fn record(&self) -> bool {
        let previous = self.count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if previous + 1 == GUARD_DENIAL_LIMIT {
            self.limit_reached
                .store(true, std::sync::atomic::Ordering::SeqCst);
            return true;
        }
        false
    }

    /// Whether the denial limit has been reached.
    pub(crate) fn limit_reached(&self) -> bool {
        self.limit_reached.load(std::sync::atomic::Ordering::SeqCst)
    }
}

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
    runtime: &'a TurnDependencies,
    context: &'a ToolExecutionContext,
    hooks: ToolExecutionHooks<'a>,
    guard_tracker: Option<Arc<GuardDenialTracker>>,
}

impl<'a> ToolExecutor<'a> {
    /// Creates an executor for one assistant tool-call phase.
    pub(crate) fn new(
        runtime: &'a TurnDependencies,
        context: &'a ToolExecutionContext,
        hooks: ToolExecutionHooks<'a>,
    ) -> Self {
        Self {
            runtime,
            context,
            hooks,
            guard_tracker: None,
        }
    }

    /// Tracks security-guard denials for the run and stops `bash` at the limit.
    pub(crate) fn with_guard_denial_tracker(mut self, tracker: Arc<GuardDenialTracker>) -> Self {
        self.guard_tracker = Some(tracker);
        self
    }

    /// Executes the supplied calls and returns their tool result messages.
    pub(crate) async fn execute(
        &self,
        assistant_message_id: &str,
        tool_calls: Vec<ToolCall>,
    ) -> Result<Vec<ExecutedToolCall>, EgoPulseError> {
        if tool_calls.is_empty() {
            return Ok(Vec::new());
        }

        let read_only_flags = self.read_only_flags(&tool_calls).await;
        let mut outcomes = Vec::with_capacity(tool_calls.len());
        let mut cursor = 0;

        while cursor < tool_calls.len() {
            if self.is_run_blocked() {
                outcomes.push(self.reject_after_guard_limit(tool_calls[cursor].clone()));
                cursor += 1;
                continue;
            }

            if read_only_flags[cursor] {
                let block_start = cursor;
                while cursor < tool_calls.len() && read_only_flags[cursor] {
                    cursor += 1;
                }
                let block_futures = tool_calls[block_start..cursor]
                    .iter()
                    .cloned()
                    .map(|tool_call| {
                        // The whole parallel block is already in flight. A
                        // denial inside it does not cancel sibling reads.
                        self.execute_single_tool(assistant_message_id, tool_call, true, true)
                    })
                    .collect::<Vec<_>>();
                let block_results = join_all(block_futures).await;
                let block_len = block_results.len();
                for result in block_results {
                    outcomes.push(result?);
                }
                self.record_read_only_block(
                    assistant_message_id,
                    &outcomes[outcomes.len() - block_len..],
                )
                .await?;
            } else {
                outcomes.push(
                    self.execute_single_tool(
                        assistant_message_id,
                        tool_calls[cursor].clone(),
                        read_only_flags[cursor],
                        false,
                    )
                    .await?,
                );
                cursor += 1;
            }
        }

        Ok(outcomes)
    }

    async fn read_only_flags(&self, tool_calls: &[ToolCall]) -> Vec<bool> {
        let mut flags = Vec::with_capacity(tool_calls.len());
        for tool_call in tool_calls {
            flags.push(self.runtime.tools.is_read_only(&tool_call.name).await);
        }
        flags
    }

    async fn execute_single_tool(
        &self,
        assistant_message_id: &str,
        tool_call: ToolCall,
        is_read_only: bool,
        allow_after_guard_limit: bool,
    ) -> Result<ExecutedToolCall, EgoPulseError> {
        if let Some(on_start) = &self.hooks.on_start {
            on_start(&tool_call);
        }

        let tool_start = std::time::Instant::now();

        if !allow_after_guard_limit && self.is_run_blocked() {
            return Ok(self.build_guard_limit_rejection(tool_call, tool_start));
        }

        let (result, payload, executed) = match tool_call.invalid_arguments.as_deref() {
            // The provider sent arguments that are not valid JSON. Report it to
            // the model so it can resend the call instead of failing the Turn.
            Some(parse_error) => {
                let result = ToolResult::error_with_kind(
                    crate::tools::ToolErrorKind::InvalidInput,
                    invalid_arguments_message(&tool_call.name, parse_error),
                );
                let payload = format_tool_result(&tool_call, &result);
                (result, payload, false)
            }
            None => {
                self.run_claimed_tool(assistant_message_id, &tool_call, is_read_only)
                    .await?
            }
        };

        if let Some(tracker) = &self.guard_tracker
            && result.error_kind == Some(crate::tools::ToolErrorKind::GuardDenied)
            && tracker.record()
        {
            // Guard denials are final (see Core Instructions). The stop
            // condition ends guard-evasion loops instead of letting them run
            // to the iteration limit unnoticed.
            tracing::warn!(
                turn_id = %self.context.turn_id,
                limit = GUARD_DENIAL_LIMIT,
                "guard denial limit reached; Tool execution is disabled for the rest of this run"
            );
        }

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

        if let Some(on_result) = &self.hooks.on_result {
            on_result(&outcome);
        }

        Ok(outcome)
    }

    /// Whether all further Tool execution must be refused for this run.
    fn is_run_blocked(&self) -> bool {
        self.guard_tracker
            .as_ref()
            .is_some_and(|tracker| tracker.limit_reached())
    }

    fn reject_after_guard_limit(&self, tool_call: ToolCall) -> ExecutedToolCall {
        if let Some(on_start) = &self.hooks.on_start {
            on_start(&tool_call);
        }
        self.build_guard_limit_rejection(tool_call, std::time::Instant::now())
    }

    fn build_guard_limit_rejection(
        &self,
        tool_call: ToolCall,
        started_at: std::time::Instant,
    ) -> ExecutedToolCall {
        let result = ToolResult::error_with_kind(
            crate::tools::ToolErrorKind::GuardDenied,
            format!(
                "[runtime_guard]: All Tool execution is stopped for this run because the security-guard denial limit ({GUARD_DENIAL_LIMIT}) was reached. Do not retry or request another Tool; report what was blocked to the user."
            ),
        );
        let payload = format_tool_result(&tool_call, &result);
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
            duration_ms: started_at.elapsed().as_millis(),
        };
        if let Some(on_result) = &self.hooks.on_result {
            on_result(&outcome);
        }
        outcome
    }

    /// Appends a finished read-only parallel block to the execution ledger.
    ///
    /// Read-only Tools skip claim-before-execute to keep parallel execution
    /// free of SQLite write contention, so the finished block is recorded in
    /// one transaction afterwards. This keeps `tool_calls` a complete record
    /// of every Tool call while preserving the ledger's recovery semantics
    /// (read-only rows are never `running`).
    async fn record_read_only_block(
        &self,
        assistant_message_id: &str,
        outcomes: &[ExecutedToolCall],
    ) -> Result<(), EgoPulseError> {
        if outcomes.is_empty() {
            return Ok(());
        }
        let chat_id = self.context.chat_id;
        let records = outcomes
            .iter()
            .map(|outcome| {
                let canonical =
                    canonical_tool_input(&outcome.tool_call.name, &outcome.tool_call.arguments);
                let is_error = outcome.result.is_error;
                ReadOnlyToolOutcome {
                    turn_id: self.context.turn_id.clone(),
                    chat_id,
                    message_id: assistant_message_id.to_string(),
                    tool_call_id: outcome.tool_call.id.clone(),
                    tool_name: outcome.tool_call.name.clone(),
                    tool_input: self
                        .runtime
                        .tools
                        .sanitize_tool_input(&outcome.tool_call.arguments, self.context)
                        .to_string(),
                    input_hash: input_hash(&canonical),
                    tool_output: (!is_error).then(|| outcome.payload.clone()),
                    succeeded: !is_error,
                    error_kind: outcome
                        .result
                        .error_kind
                        .map(|kind| kind.as_ledger_str().to_string()),
                    error_message: is_error.then(|| outcome.result.content.clone()),
                }
            })
            .collect();
        Ok(
            call_blocking(self.runtime.db_for(self.context.scope), move |db| {
                db.record_read_only_tool_outcomes(records)
            })
            .await?,
        )
    }

    /// Claims the ledger slot (for side-effecting Tools) and executes the call.
    ///
    /// Returns the result, the payload sent to the model, and whether the Tool
    /// actually ran.
    async fn run_claimed_tool(
        &self,
        assistant_message_id: &str,
        tool_call: &ToolCall,
        is_read_only: bool,
    ) -> Result<(ToolResult, String, bool), EgoPulseError> {
        // Claim the ledger slot before executing so a Tool call is never run
        // before its durable row exists. Read-only Tools skip the ledger because
        // they have no side effects and may be safely retried after a crash;
        // this avoids SQLite write contention during parallel execution.
        let claim = if is_read_only {
            ClaimOutcome::Acquired
        } else {
            self.claim_tool_slot(assistant_message_id, tool_call)
                .await?
        };

        Ok(match claim {
            ClaimOutcome::Acquired => {
                // Bind this execution's Tool Call ID into the context so tools
                // (e.g. `agent_send`) can build per-call idempotency keys.
                let mut exec_context = self.context.clone();
                exec_context.tool_call_id = tool_call.id.clone();
                let result = self
                    .runtime
                    .tools
                    .execute(&tool_call.name, tool_call.arguments.clone(), &exec_context)
                    .await;
                let payload = format_tool_result(tool_call, &result);
                if !is_read_only {
                    self.record_tool_outcome(tool_call, &result, &payload)
                        .await?;
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
                let result =
                    ToolResult::error(format!("tool call blocked: ledger state={tool_state}"));
                let payload = format_tool_result(tool_call, &result);
                (result, payload, false)
            }
        })
    }

    /// Claims a Tool execution slot in the `tool_calls` ledger.
    ///
    /// The canonical input and its hash are computed before any DB write so the
    /// retry identity is fixed at claim time. Idempotency classification comes
    /// from the [`ToolRegistry`] (derived from each tool's read-only declaration).
    async fn claim_tool_slot(
        &self,
        assistant_message_id: &str,
        tool_call: &ToolCall,
    ) -> Result<ClaimOutcome, EgoPulseError> {
        self.runtime
            .tools
            .prepare_execution_context(&tool_call.name, self.context);
        let canonical = canonical_tool_input(&tool_call.name, &tool_call.arguments);
        let hash = input_hash(&canonical);
        let tool_input = self
            .runtime
            .tools
            .sanitize_tool_input(&tool_call.arguments, self.context)
            .to_string();
        let class = self.runtime.tools.idempotency_class(&tool_call.name).await;
        let key = self
            .runtime
            .tools
            .idempotency_key(&tool_call.name, &tool_call.arguments)
            .await;
        let turn_id = self.context.turn_id.clone();
        let chat_id = self.context.chat_id;
        let message_id = assistant_message_id.to_string();
        let tool_call_id = tool_call.id.clone();
        let tool_name = tool_call.name.clone();
        let hash_for_closure = hash;
        let tool_input_for_closure = tool_input;
        let key_for_closure = key;
        Ok(
            call_blocking(self.runtime.db_for(self.context.scope), move |db| {
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
            .await?,
        )
    }

    /// Records the Tool execution outcome (success or failure) in the ledger.
    ///
    /// `payload` and `result.content` are already sanitized by [`ToolRegistry::execute`],
    /// so no secret reaches the persisted `tool_output` / `error_message`.
    async fn record_tool_outcome(
        &self,
        tool_call: &ToolCall,
        result: &ToolResult,
        payload: &str,
    ) -> Result<(), EgoPulseError> {
        let turn_id = self.context.turn_id.clone();
        let tool_call_id = tool_call.id.clone();
        if result.is_error {
            let error_message = result.content.clone();
            let error_kind = result
                .error_kind
                .map(|kind| kind.as_ledger_str())
                .unwrap_or("tool_error")
                .to_string();
            Ok(
                call_blocking(self.runtime.db_for(self.context.scope), move |db| {
                    db.record_tool_failure(&turn_id, &tool_call_id, &error_kind, &error_message)
                })
                .await?,
            )
        } else {
            let payload = payload.to_string();
            Ok(
                call_blocking(self.runtime.db_for(self.context.scope), move |db| {
                    db.record_tool_success(&turn_id, &tool_call_id, &payload)
                })
                .await?,
            )
        }
    }
}

fn invalid_arguments_message(tool_name: &str, parse_error: &str) -> String {
    format!(
        "Tool call was not executed: the arguments for '{tool_name}' are not valid JSON \
         ({parse_error}). Resend the call with complete, valid JSON arguments. For large \
         file content, use `write` or split the change into smaller `edit` calls."
    )
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tool_call(id: &str, name: &str, arguments: serde_json::Value) -> ToolCall {
        ToolCall {
            id: id.to_string(),
            name: name.to_string(),
            arguments,
            invalid_arguments: None,
        }
    }

    #[test]
    fn guard_denial_tracker_flags_limit_once_at_crossing() {
        // Arrange
        let tracker = GuardDenialTracker::new();

        // Act & Assert: below the limit nothing is flagged
        assert!(!tracker.limit_reached());
        assert!(!tracker.record());
        assert!(!tracker.record());
        assert!(!tracker.limit_reached());

        // Act & Assert: the call that reaches the limit flags it exactly once
        assert!(tracker.record());
        assert!(tracker.limit_reached());
        assert!(!tracker.record());
        assert!(tracker.limit_reached());
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
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;
    use tokio::sync::Barrier;

    use crate::agent_loop::process_turn;
    use crate::agent_loop::test_support::{
        RecordingProvider, build_state_with_provider, cli_context,
    };
    use crate::conversation::ConversationScope;
    use crate::llm::{MessagesResponse, ToolCall, ToolDefinition};
    use crate::storage::call_blocking;
    use crate::tools::{Tool, ToolExecutionContext, ToolResult};

    struct ConcurrentReadTool {
        barrier: Arc<Barrier>,
        active: Arc<AtomicUsize>,
        max_active: Arc<AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl Tool for ConcurrentReadTool {
        fn name(&self) -> &str {
            "concurrent_read"
        }

        fn definition(&self) -> ToolDefinition {
            ToolDefinition {
                name: self.name().to_string(),
                description: "Test-only read-only tool".to_string(),
                parameters: serde_json::json!({"type": "object"}),
            }
        }

        async fn execute(
            &self,
            _input: serde_json::Value,
            _context: &ToolExecutionContext,
        ) -> ToolResult {
            let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_active.fetch_max(active, Ordering::SeqCst);
            self.barrier.wait().await;
            self.active.fetch_sub(1, Ordering::SeqCst);
            ToolResult::success("concurrent result".to_string())
        }

        fn is_read_only(&self) -> bool {
            true
        }
    }

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
                description: "Test-only side-effecting tool".to_string(),
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

    struct InputCaptureTool {
        name: &'static str,
        read_only: bool,
        inputs: Arc<std::sync::Mutex<Vec<serde_json::Value>>>,
    }

    #[async_trait::async_trait]
    impl Tool for InputCaptureTool {
        fn name(&self) -> &str {
            self.name
        }

        fn definition(&self) -> ToolDefinition {
            ToolDefinition {
                name: self.name.to_string(),
                description: "Test-only input capture tool".to_string(),
                parameters: serde_json::json!({"type": "object"}),
            }
        }

        async fn execute(
            &self,
            input: serde_json::Value,
            _context: &ToolExecutionContext,
        ) -> ToolResult {
            self.inputs.lock().expect("inputs").push(input);
            ToolResult::success("captured".to_string())
        }

        fn is_read_only(&self) -> bool {
            self.read_only
        }
    }

    // -----------------------------------------------------------------------
    // Tool execution strategy
    // -----------------------------------------------------------------------

    #[tokio::test]
    #[serial]
    async fn parallel_read_only_tools_execute_concurrently() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let barrier = Arc::new(Barrier::new(2));
        let active = Arc::new(AtomicUsize::new(0));
        let max_active = Arc::new(AtomicUsize::new(0));
        let provider = RecordingProvider::new(
            vec![
                Ok(MessagesResponse {
                    content: "Reading.".to_string(),
                    reasoning_content: None,
                    tool_calls: vec![
                        ToolCall {
                            id: "call-1".to_string(),
                            name: "concurrent_read".to_string(),
                            arguments: serde_json::json!({}),
                            invalid_arguments: None,
                        },
                        ToolCall {
                            id: "call-2".to_string(),
                            name: "concurrent_read".to_string(),
                            arguments: serde_json::json!({}),
                            invalid_arguments: None,
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
        let mut state = build_state_with_provider(
            dir.path().to_str().expect("utf8").to_string(),
            Box::new(provider),
        );
        Arc::get_mut(&mut state.tools)
            .expect("tool registry is uniquely owned")
            .register_tool(Box::new(ConcurrentReadTool {
                barrier,
                active,
                max_active: Arc::clone(&max_active),
            }));

        // Act
        let reply = tokio::time::timeout(
            Duration::from_secs(2),
            process_turn(
                &state.turn_dependencies(),
                &cli_context("parallel-read"),
                "read both",
            ),
        )
        .await
        .expect("read-only tools should overlap")
        .expect("turn");

        // Assert
        assert_eq!(reply, "Done.");
        assert_eq!(max_active.load(Ordering::SeqCst), 2);

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
        assert!(
            tool_calls
                .iter()
                .all(|call| call.state == crate::storage::ToolState::Succeeded)
        );
        assert!(
            tool_calls.iter().all(|call| call.tool_output.is_some()),
            "read-only rows store the sanitized payload like non-read-only rows"
        );
    }

    #[tokio::test]
    #[serial]
    async fn invalid_read_only_arguments_are_reported_and_ledgered_as_invalid_input() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let provider = RecordingProvider::new(
            vec![
                Ok(MessagesResponse {
                    content: String::new(),
                    reasoning_content: None,
                    tool_calls: vec![ToolCall::from_raw_arguments(
                        "call-broken".to_string(),
                        "read".to_string(),
                        r#"{"path": "x.txt""#,
                    )],
                    usage: None,
                }),
                Ok(MessagesResponse {
                    content: "recovered".to_string(),
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

        // Act
        let reply = process_turn(
            &state.turn_dependencies(),
            &cli_context("invalid-tool-arguments"),
            "read a file",
        )
        .await
        .expect("turn should continue after invalid tool arguments");

        // Assert
        assert_eq!(reply, "recovered");
        let seen_messages = observer.seen_messages();
        assert_eq!(seen_messages.len(), 2);
        let tool_result = seen_messages[1].last().expect("tool result message");
        assert_eq!(tool_result.role, "tool");
        let tool_result_text = crate::agent_loop::message_format::message_to_text(tool_result);
        assert!(
            tool_result_text.contains("not valid JSON"),
            "{tool_result_text}"
        );
        let chat_id = call_blocking(Arc::clone(&state.db), move |db| {
            db.resolve_or_create_chat_id(
                "cli",
                "cli:invalid-tool-arguments:agent:default",
                Some("invalid-tool-arguments"),
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
        assert_eq!(tool_calls.len(), 1, "read-only invalid input is ledgered");
        assert_eq!(tool_calls[0].state, crate::storage::ToolState::Failed);
        assert_eq!(tool_calls[0].error_kind.as_deref(), Some("invalid_input"));
    }

    #[tokio::test]
    #[serial]
    async fn failed_read_only_tool_is_ledgered_with_its_error_kind() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let provider = RecordingProvider::new(
            vec![
                Ok(MessagesResponse {
                    content: "Looking.".to_string(),
                    reasoning_content: None,
                    tool_calls: vec![ToolCall {
                        id: "call-read-missing".to_string(),
                        name: "read".to_string(),
                        arguments: serde_json::json!({"path": "missing.txt"}),
                        invalid_arguments: None,
                    }],
                    usage: None,
                }),
                Ok(MessagesResponse {
                    content: "reported".to_string(),
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

        // Act
        let reply = process_turn(
            &state.turn_dependencies(),
            &cli_context("read-only-failure"),
            "read the file",
        )
        .await
        .expect("turn continues after read-only failure");

        // Assert
        assert_eq!(reply, "reported");
        let chat_id = call_blocking(Arc::clone(&state.db), move |db| {
            db.resolve_or_create_chat_id(
                "cli",
                "cli:read-only-failure:agent:default",
                Some("read-only-failure"),
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
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0].state, crate::storage::ToolState::Failed);
        assert_eq!(tool_calls[0].error_kind.as_deref(), Some("not_found"));
        assert!(tool_calls[0].tool_output.is_none());
    }

    #[tokio::test]
    #[serial]
    async fn tool_inputs_are_sanitized_for_both_ledger_paths_but_hashed_before_sanitizing() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let secret = "runtime-value-based-secret";
        let skill_secret = "runtime-skill-only-secret";
        let mut config = crate::test_util::test_config(dir.path().to_str().expect("utf8"));
        config
            .providers
            .values_mut()
            .next()
            .expect("provider")
            .api_key = Some(crate::config::secret_ref::ResolvedValue::Literal(
            secret.to_string(),
        ));
        let skills_dir = config.skills_dir().expect("skills directory");
        let skill_dir = skills_dir.join("ledger-input-redaction");
        std::fs::create_dir_all(&skill_dir).expect("skill directory");
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: ledger-input-redaction\ndescription: Test\nrequired_env:\n  - EGOPULSE_LEDGER_INPUT_SECRET\n---\nTest skill\n",
        )
        .expect("skill metadata");
        std::fs::write(
            dir.path().join(".env"),
            format!("EGOPULSE_LEDGER_INPUT_SECRET={skill_secret}\n"),
        )
        .expect("dotenv");
        let read_input = serde_json::json!({
            "url": format!("https://user:{secret}@example.test/path?api_key={secret}")
        });
        let write_input = serde_json::json!({
            "url": format!("https://user:{secret}@example.test/write?api_key={secret}")
        });
        let bash_input = serde_json::json!({
            "command": format!("printf '%s' '{skill_secret}'")
        });
        let provider = RecordingProvider::new(
            vec![
                Ok(MessagesResponse {
                    content: "Fetching and storing.".to_string(),
                    reasoning_content: None,
                    tool_calls: vec![
                        ToolCall {
                            id: "read-secret-input".to_string(),
                            name: "ledger_read".to_string(),
                            arguments: read_input.clone(),
                            invalid_arguments: None,
                        },
                        ToolCall {
                            id: "write-secret-input".to_string(),
                            name: "ledger_write".to_string(),
                            arguments: write_input.clone(),
                            invalid_arguments: None,
                        },
                        ToolCall {
                            id: "bash-secret-input".to_string(),
                            name: "bash".to_string(),
                            arguments: bash_input.clone(),
                            invalid_arguments: None,
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
        let received_inputs = Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut state = crate::test_util::build_state_with_config(
            config,
            Some(Arc::new(provider)),
            None,
            None,
            None,
        );
        let tools = Arc::get_mut(&mut state.tools).expect("tool registry is uniquely owned");
        for (name, read_only) in [("ledger_read", true), ("ledger_write", false)] {
            tools.register_tool(Box::new(InputCaptureTool {
                name,
                read_only,
                inputs: Arc::clone(&received_inputs),
            }));
        }

        // Act
        let reply = process_turn(
            &state.turn_dependencies(),
            &cli_context("ledger-input-redaction"),
            "use both tools",
        )
        .await
        .expect("turn");

        // Assert: tools receive the original secret-bearing values.
        assert_eq!(reply, "Done.");
        {
            let received_inputs = received_inputs.lock().expect("captured inputs");
            assert_eq!(received_inputs.len(), 2);
            assert!(received_inputs.iter().any(|input| input == &read_input));
            assert!(received_inputs.iter().any(|input| input == &write_input));
        }

        let chat_id = call_blocking(Arc::clone(&state.db), move |db| {
            db.resolve_or_create_chat_id(
                "cli",
                "cli:ledger-input-redaction:agent:default",
                Some("ledger-input-redaction"),
                "cli",
                "default",
            )
        })
        .await
        .expect("chat id");
        let calls = call_blocking(Arc::clone(&state.db), move |db| {
            db.get_tool_calls_for_chat(chat_id)
        })
        .await
        .expect("tool calls");
        assert_eq!(calls.len(), 3);
        for call in calls {
            assert!(!call.tool_input.contains(secret), "{}", call.tool_input);
            assert!(
                !call.tool_input.contains(skill_secret),
                "{}",
                call.tool_input
            );
            let persisted_input: serde_json::Value =
                serde_json::from_str(&call.tool_input).expect("persisted JSON");
            if call.tool_name == "bash" {
                assert!(
                    persisted_input["command"]
                        .as_str()
                        .expect("command string")
                        .contains("[REDACTED:skill_env.EGOPULSE_LEDGER_INPUT_SECRET]")
                );
            } else {
                let persisted_url = persisted_input["url"].as_str().expect("url string");
                assert!(persisted_url.contains("[REDACTED:provider.openai.api_key]"));
            }
            let raw_input = match call.id.as_str() {
                "read-secret-input" => &read_input,
                "write-secret-input" => &write_input,
                "bash-secret-input" => &bash_input,
                other => panic!("unexpected call id: {other}"),
            };
            let expected_hash = crate::storage::input_hash(&crate::storage::canonical_tool_input(
                &call.tool_name,
                raw_input,
            ));
            let stored_hash: String = state
                .db
                .get_conn()
                .expect("connection")
                .query_row(
                    "SELECT input_hash FROM tool_calls WHERE id = ?1",
                    rusqlite::params![call.id],
                    |row| row.get(0),
                )
                .expect("input hash");
            assert_eq!(stored_hash, expected_hash);
        }
    }

    #[tokio::test]
    #[serial]
    async fn guard_denied_bash_call_is_ledgered_with_guard_kind() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let provider = RecordingProvider::new(
            vec![
                Ok(MessagesResponse {
                    content: String::new(),
                    reasoning_content: None,
                    tool_calls: vec![ToolCall {
                        id: "call-guard".to_string(),
                        name: "bash".to_string(),
                        arguments: serde_json::json!({"command": "cat .env"}),
                        invalid_arguments: None,
                    }],
                    usage: None,
                }),
                Ok(MessagesResponse {
                    content: "reported".to_string(),
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

        // Act
        let reply = process_turn(
            &state.turn_dependencies(),
            &cli_context("guard-kind"),
            "read the env file",
        )
        .await
        .expect("turn continues after guard denial");

        // Assert
        assert_eq!(reply, "reported");
        let chat_id = call_blocking(Arc::clone(&state.db), move |db| {
            db.resolve_or_create_chat_id(
                "cli",
                "cli:guard-kind:agent:default",
                Some("guard-kind"),
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
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0].error_kind.as_deref(), Some("guard_denied"));
    }

    #[tokio::test]
    #[serial]
    async fn guard_denial_limit_blocks_side_effect_tools_and_injects_wrap_guard() {
        // Arrange: three denied bash calls cross the limit; a side-effecting
        // non-bash Tool is refused and the next model request exposes no tools.
        let dir = tempfile::tempdir().expect("tempdir");
        let write_calls = Arc::new(AtomicUsize::new(0));
        let denied_call = || ToolCall {
            id: format!("call-{}", uuid::Uuid::new_v4()),
            name: "bash".to_string(),
            arguments: serde_json::json!({"command": "cat .env"}),
            invalid_arguments: None,
        };
        let provider = RecordingProvider::new(
            vec![
                Ok(MessagesResponse {
                    content: "Trying.".to_string(),
                    reasoning_content: None,
                    tool_calls: vec![denied_call()],
                    usage: None,
                }),
                Ok(MessagesResponse {
                    content: "Retrying.".to_string(),
                    reasoning_content: None,
                    tool_calls: vec![denied_call()],
                    usage: None,
                }),
                Ok(MessagesResponse {
                    content: "Again.".to_string(),
                    reasoning_content: None,
                    tool_calls: vec![denied_call()],
                    usage: None,
                }),
                Ok(MessagesResponse {
                    content: "Once more.".to_string(),
                    reasoning_content: None,
                    tool_calls: vec![ToolCall {
                        id: "call-blocked".to_string(),
                        name: "write_probe".to_string(),
                        arguments: serde_json::json!({}),
                        invalid_arguments: None,
                    }],
                    usage: None,
                }),
                Ok(MessagesResponse {
                    content: "wrapped up".to_string(),
                    reasoning_content: None,
                    tool_calls: Vec::new(),
                    usage: None,
                }),
            ],
            vec![0, 0, 0, 0, 0],
        );
        let observer = provider.clone();
        let mut state = build_state_with_provider(
            dir.path().to_str().expect("utf8").to_string(),
            Box::new(provider),
        );
        Arc::get_mut(&mut state.tools)
            .expect("tool registry is uniquely owned")
            .register_tool(Box::new(SideEffectProbeTool {
                calls: Arc::clone(&write_calls),
            }));

        // Act
        let reply = process_turn(
            &state.turn_dependencies(),
            &cli_context("guard-limit"),
            "read the env file",
        )
        .await
        .expect("turn wraps up after the denial limit");

        // Assert: the wrap-up guard rode along with the fourth request.
        assert_eq!(reply, "wrapped up");
        let seen_messages = observer.seen_messages();
        assert_eq!(seen_messages.len(), 5);
        let fourth_request = seen_messages[3]
            .iter()
            .map(crate::agent_loop::message_format::message_to_text)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            fourth_request.contains("security-guard denial limit has been reached"),
            "wrap guard must be injected: {fourth_request}"
        );
        assert!(
            observer.seen_tools()[3].as_ref().is_some_and(Vec::is_empty),
            "subsequent model requests must not expose Tool definitions"
        );

        // Assert: the blocked bash never executed; its tool result reports the
        // stop condition instead of command output.
        let blocked_result = seen_messages[4]
            .iter()
            .find(|message| message.tool_call_id.as_deref() == Some("call-blocked"))
            .expect("blocked call must receive a tool result");
        let blocked_text = crate::agent_loop::message_format::message_to_text(blocked_result);
        assert!(
            blocked_text.contains("All Tool execution is stopped for this run"),
            "the blocked side-effecting Tool must be reported: {blocked_text}"
        );
        assert_eq!(write_calls.load(Ordering::SeqCst), 0);

        // Assert: only the three executed denials are ledgered.
        let chat_id = call_blocking(Arc::clone(&state.db), move |db| {
            db.resolve_or_create_chat_id(
                "cli",
                "cli:guard-limit:agent:default",
                Some("guard-limit"),
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
        assert_eq!(tool_calls.len(), 3);
        assert!(
            tool_calls
                .iter()
                .all(|call| call.error_kind.as_deref() == Some("guard_denied"))
        );
    }

    #[tokio::test]
    #[serial]
    async fn non_guard_tool_errors_do_not_count_toward_denial_limit() {
        // Arrange: two policy denials plus one normal file-not-found result
        // must leave the tracker below the limit so the next write can run.
        let dir = tempfile::tempdir().expect("tempdir");
        let write_calls = Arc::new(AtomicUsize::new(0));
        let provider = RecordingProvider::new(
            vec![
                Ok(MessagesResponse {
                    content: "Trying two blocked paths.".to_string(),
                    reasoning_content: None,
                    tool_calls: vec![
                        ToolCall {
                            id: "denied-1".to_string(),
                            name: "bash".to_string(),
                            arguments: serde_json::json!({"command": "cat .env"}),
                            invalid_arguments: None,
                        },
                        ToolCall {
                            id: "denied-2".to_string(),
                            name: "bash".to_string(),
                            arguments: serde_json::json!({"command": "cat .ssh/id_rsa"}),
                            invalid_arguments: None,
                        },
                    ],
                    usage: None,
                }),
                Ok(MessagesResponse {
                    content: "Check the missing file.".to_string(),
                    reasoning_content: None,
                    tool_calls: vec![ToolCall {
                        id: "missing-read".to_string(),
                        name: "read".to_string(),
                        arguments: serde_json::json!({"path": "missing.txt"}),
                        invalid_arguments: None,
                    }],
                    usage: None,
                }),
                Ok(MessagesResponse {
                    content: "Now write.".to_string(),
                    reasoning_content: None,
                    tool_calls: vec![ToolCall {
                        id: "write-after-normal-error".to_string(),
                        name: "write_probe".to_string(),
                        arguments: serde_json::json!({}),
                        invalid_arguments: None,
                    }],
                    usage: None,
                }),
                Ok(MessagesResponse {
                    content: "Finished.".to_string(),
                    reasoning_content: None,
                    tool_calls: Vec::new(),
                    usage: None,
                }),
            ],
            vec![0; 4],
        );
        let mut state = build_state_with_provider(
            dir.path().to_str().expect("utf8").to_string(),
            Box::new(provider),
        );
        Arc::get_mut(&mut state.tools)
            .expect("tool registry is uniquely owned")
            .register_tool(Box::new(SideEffectProbeTool {
                calls: Arc::clone(&write_calls),
            }));

        // Act
        let reply = process_turn(
            &state.turn_dependencies(),
            &cli_context("normal-tool-error-count"),
            "try these tools",
        )
        .await
        .expect("turn continues");

        // Assert
        assert_eq!(reply, "Finished.");
        assert_eq!(write_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    #[serial]
    async fn secret_turn_writes_tool_ledger_to_secret_db_only() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let provider = RecordingProvider::new(
            vec![
                Ok(MessagesResponse {
                    content: String::new(),
                    reasoning_content: None,
                    tool_calls: vec![ToolCall {
                        id: "call-secret".to_string(),
                        name: "bash".to_string(),
                        arguments: serde_json::json!({"command": "echo secret-side-effect"}),
                        invalid_arguments: None,
                    }],
                    usage: None,
                }),
                Ok(MessagesResponse {
                    content: "done".to_string(),
                    reasoning_content: None,
                    tool_calls: Vec::new(),
                    usage: None,
                }),
            ],
            vec![0, 0],
        );
        let mut state = build_state_with_provider(
            dir.path().to_str().expect("utf8").to_string(),
            Box::new(provider),
        );
        let secret_path = dir.path().join("runtime").join("secret.db");
        state.secret_db = Some(Arc::new(
            crate::storage::Database::new_secret(&secret_path).expect("secret db"),
        ));
        let mut context = cli_context("secret-tool-ledger");
        context.scope = ConversationScope::Secret;

        // Act
        let reply = process_turn(&state.turn_dependencies(), &context, "run a command")
            .await
            .expect("process turn");

        // Assert
        assert_eq!(reply, "done");
        let normal_count: i64 = state
            .db
            .get_conn()
            .expect("normal conn")
            .query_row("SELECT COUNT(*) FROM tool_calls", [], |row| row.get(0))
            .expect("normal tool ledger count");
        assert_eq!(normal_count, 0, "secret ledger must not leak to normal db");
        let secret_count: i64 = state
            .secret_db
            .as_ref()
            .expect("secret db")
            .get_conn()
            .expect("secret conn")
            .query_row("SELECT COUNT(*) FROM tool_calls", [], |row| row.get(0))
            .expect("secret tool ledger count");
        assert_eq!(
            secret_count, 1,
            "secret ledger must be written to secret db"
        );
    }

    #[tokio::test]
    #[serial]
    async fn write_tools_persist_in_execution_order() {
        // Arrange
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
                            invalid_arguments: None,
                        },
                        ToolCall {
                            id: "call-w1".to_string(),
                            name: "write".to_string(),
                            arguments: serde_json::json!({
                                "path": file_a.clone(),
                                "content": "hello"
                            }),
                            invalid_arguments: None,
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

        // Act
        let reply = process_turn(
            &state.turn_dependencies(),
            &cli_context("seq-write"),
            "write it",
        )
        .await
        .expect("turn");

        // Assert
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
        // Arrange
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
                            invalid_arguments: None,
                        },
                        ToolCall {
                            id: "call-b1".to_string(),
                            name: "bash".to_string(),
                            arguments: serde_json::json!({"command": "echo step2"}),
                            invalid_arguments: None,
                        },
                        ToolCall {
                            id: "call-r2".to_string(),
                            name: "read".to_string(),
                            arguments: serde_json::json!({"path": file_b.clone()}),
                            invalid_arguments: None,
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

        // Act
        let reply = process_turn(
            &state.turn_dependencies(),
            &cli_context("transcript-order"),
            "ordered",
        )
        .await
        .expect("turn");

        // Assert
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
