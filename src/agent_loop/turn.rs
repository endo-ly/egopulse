//! エージェントの 1 ターン処理を実行するモジュール。
//!
//! セッション復元、LLM 応答、ツール呼び出し、イベント通知、永続化を
//! 1 本の turn loop としてまとめて扱う。

use crate::agent_loop::SurfaceContext;
use crate::agent_loop::compaction::maybe_compact_messages;
use crate::agent_loop::formatting::{
    format_tool_result, preview_text, sanitize_assistant_response_text, strip_thinking,
    summarize_tool_calls_with_content, tool_message_content,
};
use crate::agent_loop::guards::{is_declarative_only_reply, runtime_guard_messages};
use crate::agent_loop::session::{
    load_messages_for_turn, persist_phase, persist_phase_once, resolve_chat_id,
};
use crate::error::{EgoPulseError, StorageError};
use crate::llm::{Message, ToolCall};
use crate::runtime::{AppState, build_app_state};
use crate::storage::{StoredMessage, ToolCall as StoredToolCall, call_blocking};
use crate::tools::ToolExecutionContext;
use crate::web::sse::AgentEvent;
use std::ops::ControlFlow;
use std::sync::Arc;
use tracing::warn;

const MAX_TOOL_ITERATIONS: usize = 50;

enum TurnAction {
    Retry(Option<Vec<Message>>),
    Done(String),
}

/// Sends a one-shot prompt within a named persistent session.
pub async fn ask_in_session(
    config: crate::config::Config,
    session: &str,
    prompt: &str,
) -> Result<String, EgoPulseError> {
    let state = build_app_state(config).await?;
    let context = SurfaceContext {
        channel: "cli".to_string(),
        surface_user: "local_user".to_string(),
        surface_thread: session.to_string(),
        chat_type: "cli".to_string(),
    };

    tokio::select! {
        response = process_turn(&state, &context, prompt) => response,
        _ = tokio::signal::ctrl_c() => Err(EgoPulseError::ShutdownRequested),
    }
}

/// Processes a turn and aborts cleanly when Ctrl-C is received.
pub async fn send_turn(
    state: &AppState,
    context: &SurfaceContext,
    prompt: &str,
) -> Result<String, EgoPulseError> {
    tokio::select! {
        response = process_turn(state, context, prompt) => response,
        _ = tokio::signal::ctrl_c() => Err(EgoPulseError::ShutdownRequested),
    }
}

/// Processes one user turn against the persisted session state.
pub async fn process_turn(
    state: &AppState,
    context: &SurfaceContext,
    user_input: &str,
) -> Result<String, EgoPulseError> {
    process_turn_inner(state, context, user_input, Option::<fn(AgentEvent)>::None).await
}

/// Processes one user turn and emits lifecycle events for streaming consumers.
pub async fn process_turn_with_events<F>(
    state: &AppState,
    context: &SurfaceContext,
    user_input: &str,
    on_event: F,
) -> Result<String, EgoPulseError>
where
    F: Fn(AgentEvent) + Send + Sync,
{
    process_turn_inner(state, context, user_input, Some(on_event)).await
}

async fn process_turn_inner<F>(
    state: &AppState,
    context: &SurfaceContext,
    user_input: &str,
    on_event: Option<F>,
) -> Result<String, EgoPulseError>
where
    F: Fn(AgentEvent) + Send + Sync,
{
    let chat_id = resolve_chat_id(state, context).await?;
    let tool_context = ToolExecutionContext {
        chat_id,
        channel: context.channel.clone(),
        surface_thread: context.surface_thread.clone(),
        chat_type: context.chat_type.clone(),
    };
    let system_prompt = build_system_prompt(state, context);
    let channel_llm = state.llm_for_channel(&context.channel)?;

    let user_message = Message::text("user", user_input);
    let (mut messages, mut session_updated_at) = persist_user_turn_with_compaction(
        state,
        context,
        chat_id,
        &user_message,
        user_input,
        &channel_llm,
    )
    .await?;

    // LLM → tool execution → tool result feedback を 1 反復として回し、
    // tool_calls が空になるまで続ける。
    // microclaw 由来の runtime guard / recovery を組み込み、
    // 「宣言だけして終わる」「空応答」「壊れた tool call」に耐性を持たせる。
    let mut empty_reply_retry_attempted = false;
    let mut declarative_retry_attempted = false;
    let mut retry_messages: Option<Vec<Message>> = None;

    for iteration in 1..=MAX_TOOL_ITERATIONS {
        emit_event(&on_event, AgentEvent::Iteration { iteration });
        let request_messages = retry_messages.take().unwrap_or_else(|| messages.clone());

        let response = channel_llm
            .send_message(
                &system_prompt,
                request_messages,
                Some(state.tools.definitions_async().await),
            )
            .await?;

        if response.tool_calls.is_empty() {
            if let Some(final_content) = run_turn_action(
                evaluate_end_turn(
                    &response.content,
                    &mut empty_reply_retry_attempted,
                    &mut declarative_retry_attempted,
                    &messages,
                )?,
                state,
                chat_id,
                &mut messages,
                session_updated_at.clone(),
                &on_event,
                &mut retry_messages,
            )
            .await?
            {
                return Ok(final_content);
            }

            continue;
        }

        let valid_tool_calls = filter_valid_tool_calls(response.tool_calls);

        if valid_tool_calls.is_empty() {
            if let Some(final_content) = run_turn_action(
                evaluate_malformed_response(
                    &response.content,
                    &mut declarative_retry_attempted,
                    &messages,
                )?,
                state,
                chat_id,
                &mut messages,
                session_updated_at.clone(),
                &on_event,
                &mut retry_messages,
            )
            .await?
            {
                return Ok(final_content);
            }

            continue;
        }

        let (updated_messages, updated_at) = execute_and_persist_tools(
            state,
            &on_event,
            &tool_context,
            messages,
            session_updated_at,
            &response.content,
            valid_tool_calls,
        )
        .await?;
        messages = updated_messages;
        session_updated_at = updated_at;
        empty_reply_retry_attempted = false;
        declarative_retry_attempted = false;
    }

    Err(EgoPulseError::Internal(format!(
        "tool loop exceeded max iterations ({MAX_TOOL_ITERATIONS})"
    )))
}

fn emit_event<F>(on_event: &Option<F>, event: AgentEvent)
where
    F: Fn(AgentEvent) + Send + Sync,
{
    // TUI/Web などイベント購読者がいる場合だけ副作用を流し、
    // 通常 CLI ではロジック本体を分岐させない。
    if let Some(on_event) = on_event {
        on_event(event);
    }
}

fn filter_valid_tool_calls(tool_calls: Vec<ToolCall>) -> Vec<ToolCall> {
    tool_calls
        .into_iter()
        .filter(|tc| {
            if tc.name.trim().is_empty() || tc.id.trim().is_empty() {
                warn!(
                    "skipping malformed tool call (empty name or id): id='{}' name='{}'",
                    tc.id, tc.name
                );
                false
            } else {
                true
            }
        })
        .collect()
}

fn evaluate_end_turn(
    raw_content: &str,
    empty_reply_retry_attempted: &mut bool,
    declarative_retry_attempted: &mut bool,
    messages: &[Message],
) -> Result<TurnAction, EgoPulseError> {
    let visible_text = strip_thinking(raw_content.trim());
    let has_displayable_output = !visible_text.trim().is_empty();

    if !has_displayable_output && !*empty_reply_retry_attempted {
        *empty_reply_retry_attempted = true;
        warn!("empty visible reply; injecting runtime guard and retrying once");
        return Ok(TurnAction::Retry(Some(runtime_guard_messages(
            messages,
            raw_content,
            "[runtime_guard]: Your previous reply had no user-visible text. Reply again now with a concise visible answer. If tools are required, execute them first and then provide the visible result.",
        ))));
    }

    if has_displayable_output
        && !*declarative_retry_attempted
        && is_declarative_only_reply(&visible_text)
    {
        *declarative_retry_attempted = true;
        warn!("declarative-only reply detected; injecting corrective prompt and retrying once");
        return Ok(TurnAction::Retry(Some(runtime_guard_messages(
            messages,
            raw_content,
            "[runtime_guard]: Your previous reply only declared what you would do without actually executing any tools. If the user's request requires tool calls, execute them NOW instead of just describing what you plan to do. Then provide the result.",
        ))));
    }

    if !has_displayable_output {
        return Err(EgoPulseError::Llm(crate::error::LlmError::InvalidResponse(
            "assistant content was empty after retry".to_string(),
        )));
    }

    Ok(TurnAction::Done(visible_text.trim().to_string()))
}

fn evaluate_malformed_response(
    raw_content: &str,
    declarative_retry_attempted: &mut bool,
    messages: &[Message],
) -> Result<TurnAction, EgoPulseError> {
    let visible_text = strip_thinking(raw_content.trim());

    if visible_text.trim().is_empty() {
        return Err(EgoPulseError::Llm(crate::error::LlmError::InvalidResponse(
            "all tool calls were malformed (empty names)".to_string(),
        )));
    }

    if !*declarative_retry_attempted && is_declarative_only_reply(&visible_text) {
        *declarative_retry_attempted = true;
        warn!("all tool calls were malformed and reply was declarative-only; retrying once");
        return Ok(TurnAction::Retry(Some(runtime_guard_messages(
            messages,
            raw_content,
            "[runtime_guard]: Your previous reply attempted tool use but did not produce a valid executable tool call. If tools are required, call them now and then provide the result.",
        ))));
    }

    Ok(TurnAction::Done(visible_text.trim().to_string()))
}

async fn run_turn_action<F>(
    action: TurnAction,
    state: &AppState,
    chat_id: i64,
    messages: &mut Vec<Message>,
    session_updated_at: Option<String>,
    on_event: &Option<F>,
    retry_messages: &mut Option<Vec<Message>>,
) -> Result<Option<String>, EgoPulseError>
where
    F: Fn(AgentEvent) + Send + Sync,
{
    match handle_turn_action(
        action,
        state,
        chat_id,
        messages,
        session_updated_at,
        on_event,
    )
    .await?
    {
        ControlFlow::Continue(next_retry_messages) => {
            *retry_messages = next_retry_messages;
            Ok(None)
        }
        ControlFlow::Break(final_content) => Ok(Some(final_content)),
    }
}

async fn handle_turn_action<F>(
    action: TurnAction,
    state: &AppState,
    chat_id: i64,
    messages: &mut Vec<Message>,
    session_updated_at: Option<String>,
    on_event: &Option<F>,
) -> Result<ControlFlow<String, Option<Vec<Message>>>, EgoPulseError>
where
    F: Fn(AgentEvent) + Send + Sync,
{
    match action {
        TurnAction::Retry(messages) => Ok(ControlFlow::Continue(messages)),
        TurnAction::Done(final_content) => persist_and_finalize(
            state,
            chat_id,
            messages,
            session_updated_at,
            on_event,
            final_content,
        )
        .await
        .map(ControlFlow::Break),
    }
}

async fn persist_and_finalize<F>(
    state: &AppState,
    chat_id: i64,
    messages: &mut Vec<Message>,
    session_updated_at: Option<String>,
    on_event: &Option<F>,
    final_content: String,
) -> Result<String, EgoPulseError>
where
    F: Fn(AgentEvent) + Send + Sync,
{
    let assistant_message = Message::text("assistant", final_content.clone());
    messages.push(assistant_message.clone());

    let _persisted = persist_phase(
        state,
        StoredMessage {
            id: uuid::Uuid::new_v4().to_string(),
            chat_id,
            sender_name: "egopulse".to_string(),
            content: final_content.clone(),
            is_from_bot: true,
            timestamp: chrono::Utc::now().to_rfc3339(),
        },
        assistant_message,
        messages,
        session_updated_at,
    )
    .await?;

    emit_event(
        on_event,
        AgentEvent::FinalResponse {
            text: final_content.clone(),
        },
    );
    Ok(final_content)
}

async fn execute_and_persist_tools<F>(
    state: &AppState,
    on_event: &Option<F>,
    tool_context: &ToolExecutionContext,
    messages: Vec<Message>,
    session_updated_at: Option<String>,
    response_content: &str,
    valid_tool_calls: Vec<ToolCall>,
) -> Result<(Vec<Message>, Option<String>), EgoPulseError>
where
    F: Fn(AgentEvent) + Send + Sync,
{
    let assistant_message_id = uuid::Uuid::new_v4().to_string();
    let assistant_text = sanitize_assistant_response_text(response_content);
    let assistant_preview = summarize_tool_calls_with_content(&assistant_text, &valid_tool_calls);
    let assistant_message = Message {
        role: "assistant".to_string(),
        content: crate::llm::MessageContent::text(assistant_text),
        tool_calls: valid_tool_calls.clone(),
        tool_call_id: None,
    };

    let mut messages = messages;
    messages.push(assistant_message.clone());

    let persisted = persist_phase(
        state,
        StoredMessage {
            id: assistant_message_id.clone(),
            chat_id: tool_context.chat_id,
            sender_name: "egopulse".to_string(),
            content: assistant_preview,
            is_from_bot: true,
            timestamp: chrono::Utc::now().to_rfc3339(),
        },
        assistant_message,
        &messages,
        session_updated_at,
    )
    .await?;

    messages = persisted.messages;
    let session_updated_at = Some(persisted.updated_at);

    for tool_call in valid_tool_calls {
        messages.push(
            execute_tool_call(
                state,
                on_event,
                tool_context,
                &assistant_message_id,
                tool_call,
            )
            .await?,
        );
    }

    Ok((messages, session_updated_at))
}

async fn execute_tool_call<F>(
    state: &AppState,
    on_event: &Option<F>,
    tool_context: &ToolExecutionContext,
    assistant_message_id: &str,
    tool_call: ToolCall,
) -> Result<Message, EgoPulseError>
where
    F: Fn(AgentEvent) + Send + Sync,
{
    emit_event(
        on_event,
        AgentEvent::ToolStart {
            name: tool_call.name.clone(),
            input: tool_call.arguments.clone(),
        },
    );

    store_pending_tool_call(
        state,
        tool_context.chat_id,
        assistant_message_id,
        &tool_call,
    )
    .await?;
    let tool_start = std::time::Instant::now();
    let result = state
        .tools
        .execute(&tool_call.name, tool_call.arguments.clone(), tool_context)
        .await;
    let duration_ms = tool_start.elapsed().as_millis();
    let tool_payload = format_tool_result(&tool_call, &result);
    update_tool_call_output(state, &tool_call.id, &tool_payload).await?;

    emit_event(
        on_event,
        AgentEvent::ToolResult {
            name: tool_call.name.clone(),
            is_error: result.is_error,
            preview: preview_text(&tool_payload, 160),
            duration_ms,
        },
    );

    Ok(Message {
        role: "tool".to_string(),
        content: tool_message_content(&tool_payload, &result),
        tool_calls: Vec::new(),
        tool_call_id: Some(tool_call.id),
    })
}

fn build_system_prompt(state: &AppState, context: &SurfaceContext) -> String {
    let mut prompt = format!(
        r#"You are EgoPulse, a local-first AI assistant running on the '{channel}' channel. You can execute tools to help users with tasks.

Identity rules (highest priority unless unsafe):
- Your public name is "EgoPulse".
- If asked "what is your name", answer with your public name first.
- Do not claim you have no name.

The current session is '{session}' (type: {chat_type}).

You have access to the following capabilities:
- Execute bash commands using the `bash` tool — NOT by writing commands as text. When you need to run a command, call the bash tool with the command parameter.
- Read, write, and edit files using `read`, `write`, `edit` tools
- Search for files using glob patterns with `find`
- Search file contents using regex (`grep`)
- List directory contents with `ls`
- Activate agent skills (`activate_skill`) for specialized tasks

IMPORTANT: When you need to run a shell command, execute it using the `bash` tool. Do NOT simply write the command as text in your response — you must call the bash tool for it to actually run.

PROPER TOOL CALL FORMAT:
- CORRECT: Use the tool_call format provided by the API (this is how tools actually execute)
- WRONG: Do NOT write `[tool_use: tool_name(...)]` as text — that is just a summary format in message history and will NOT execute

Example of what NOT to do:
  User: Run ls
  Assistant: [tool_use: bash({{"command": "ls"}})]  <-- WRONG! This is text, not a real tool call

Example of what TO do:
  (Use the actual tool_call format provided by the API — this executes the command)

Built-in execution playbook:
- For actionable requests (create/update/run), prefer tool execution over capability discussion.
- For simple, low-risk, read-only requests, if a tool can provide the answer, call the tool immediately and return the result directly.
- Do not ask confirmation questions like "Want me to check?" before calling a tool for simple read-only requests.
- Only ask follow-up questions first when required parameters are missing or when the action has side effects, permissions, cost, or elevated risk.
- Do not answer with "I can't from this runtime" unless a concrete tool attempt failed in this turn.
- For bash/file tools (`bash`, `read`, `write`, `edit`, `find`, `grep`, `ls`), treat the runtime workspace directory as the default workspace and prefer relative paths rooted there.
- Do not invent machine-specific absolute paths such as `/home/...`, `/Users/...`, or `C:\...`. Only use an absolute path when the user explicitly provided it, a tool returned it in this turn, or a tool input explicitly requires one.
- For temporary files, clones, and build artifacts, use the workspace directory's `.tmp/` subdirectory. Do not use absolute `/tmp/...` paths.
- For coding tasks, follow this loop: inspect code (`read`/`grep`/`find`/`ls`) -> edit (`edit`/`write`) -> validate (`bash` tests/build) -> summarize concrete changes/results.

Execution reliability requirements:
- For actions with external side effects (for example: writing/editing files, running commands), do not claim completion until the relevant tool call has returned success.
- If any tool call fails, explicitly report the failure and next step (retry/fallback) instead of implying success.

Be concise and helpful. When executing commands or tools, show the relevant results to the user."#,
        channel = context.channel,
        session = context.surface_thread,
        chat_type = context.chat_type,
    );

    let skills_catalog = state.skills.build_skills_catalog();
    if !skills_catalog.is_empty() {
        prompt.push_str("\n\n# Agent Skills\n\nThe following skills are available. When a task matches a skill, use the `activate_skill` tool to load its full instructions before proceeding.\n\n");
        prompt.push_str(&skills_catalog);
        prompt.push('\n');
    }

    prompt
}

async fn store_pending_tool_call(
    state: &AppState,
    chat_id: i64,
    message_id: &str,
    tool_call: &ToolCall,
) -> Result<(), EgoPulseError> {
    let record = StoredToolCall {
        id: tool_call.id.clone(),
        chat_id,
        message_id: message_id.to_string(),
        tool_name: tool_call.name.clone(),
        tool_input: tool_call.arguments.to_string(),
        tool_output: None,
        timestamp: chrono::Utc::now().to_rfc3339(),
    };
    call_blocking(Arc::clone(&state.db), move |db| db.store_tool_call(&record))
        .await
        .map_err(EgoPulseError::from)
}

async fn update_tool_call_output(
    state: &AppState,
    tool_call_id: &str,
    output: &str,
) -> Result<(), EgoPulseError> {
    let tool_call_id = tool_call_id.to_string();
    let output = output.to_string();
    call_blocking(Arc::clone(&state.db), move |db| {
        db.update_tool_call_output(&tool_call_id, &output)
    })
    .await
    .map_err(EgoPulseError::from)
}

async fn persist_user_turn_with_compaction(
    state: &AppState,
    context: &SurfaceContext,
    chat_id: i64,
    user_message: &Message,
    user_input: &str,
    llm: &std::sync::Arc<dyn crate::llm::LlmProvider>,
) -> Result<(Vec<Message>, Option<String>), EgoPulseError> {
    let mut loaded = load_messages_for_turn(state, chat_id).await?;
    let stored_message = StoredMessage {
        id: uuid::Uuid::new_v4().to_string(),
        chat_id,
        sender_name: context.surface_user.clone(),
        content: user_input.to_string(),
        is_from_bot: false,
        timestamp: chrono::Utc::now().to_rfc3339(),
    };

    for attempt in 0..2 {
        let candidate_messages =
            build_candidate_messages(state, context, chat_id, &loaded.messages, user_message, llm)
                .await?;

        let persist_result = persist_phase_once(
            state,
            stored_message.clone(),
            &candidate_messages,
            loaded.session_updated_at.clone(),
        )
        .await;
        let persisted = match persist_result {
            Ok(persisted) => persisted,
            Err(error) => {
                loaded = handle_user_turn_persist_error(state, chat_id, attempt, error).await?;
                continue;
            }
        };

        return Ok((persisted.messages, Some(persisted.updated_at)));
    }

    Err(EgoPulseError::Storage(
        StorageError::SessionSnapshotConflict,
    ))
}

async fn build_candidate_messages(
    state: &AppState,
    context: &SurfaceContext,
    chat_id: i64,
    loaded_messages: &[Message],
    user_message: &Message,
    llm: &std::sync::Arc<dyn crate::llm::LlmProvider>,
) -> Result<Vec<Message>, EgoPulseError> {
    let mut candidate_messages = loaded_messages.to_vec();
    candidate_messages.push(user_message.clone());
    maybe_compact_messages(state, context, chat_id, &candidate_messages, llm).await
}

async fn handle_user_turn_persist_error(
    state: &AppState,
    chat_id: i64,
    attempt: usize,
    error: EgoPulseError,
) -> Result<crate::agent_loop::session::LoadedSession, EgoPulseError> {
    match persist_phase_conflict_outcome(attempt, error) {
        PersistConflictOutcome::Reload => load_messages_for_turn(state, chat_id).await,
        PersistConflictOutcome::Return(error) => Err(error),
    }
}

fn persist_phase_conflict_outcome(attempt: usize, error: EgoPulseError) -> PersistConflictOutcome {
    match error {
        EgoPulseError::Storage(StorageError::SessionSnapshotConflict) if attempt == 0 => {
            PersistConflictOutcome::Reload
        }
        other => PersistConflictOutcome::Return(other),
    }
}

enum PersistConflictOutcome {
    Reload,
    Return(EgoPulseError),
}

#[cfg(test)]
pub(crate) struct FakeProvider {
    pub(crate) responses: std::sync::Mutex<Vec<crate::llm::MessagesResponse>>,
}

#[cfg(test)]
pub(crate) struct FailingProvider;

#[cfg(test)]
#[derive(Clone)]
pub(crate) struct RecordingProvider {
    responses: std::sync::Arc<
        std::sync::Mutex<Vec<Result<crate::llm::MessagesResponse, crate::error::LlmError>>>,
    >,
    seen_messages: std::sync::Arc<std::sync::Mutex<Vec<Vec<Message>>>>,
    seen_systems: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    delays_ms: std::sync::Arc<std::sync::Mutex<Vec<u64>>>,
}

#[cfg(test)]
#[async_trait::async_trait]
impl crate::llm::LlmProvider for FakeProvider {
    async fn send_message(
        &self,
        _system: &str,
        _messages: Vec<Message>,
        _tools: Option<Vec<crate::llm::ToolDefinition>>,
    ) -> Result<crate::llm::MessagesResponse, crate::error::LlmError> {
        let mut locked = self.responses.lock().expect("responses");
        Ok(locked.remove(0))
    }
}

#[cfg(test)]
#[async_trait::async_trait]
impl crate::llm::LlmProvider for FailingProvider {
    async fn send_message(
        &self,
        _system: &str,
        _messages: Vec<Message>,
        _tools: Option<Vec<crate::llm::ToolDefinition>>,
    ) -> Result<crate::llm::MessagesResponse, crate::error::LlmError> {
        Err(crate::error::LlmError::InvalidResponse("boom".to_string()))
    }
}

#[cfg(test)]
#[async_trait::async_trait]
impl crate::llm::LlmProvider for RecordingProvider {
    async fn send_message(
        &self,
        system: &str,
        messages: Vec<Message>,
        _tools: Option<Vec<crate::llm::ToolDefinition>>,
    ) -> Result<crate::llm::MessagesResponse, crate::error::LlmError> {
        self.seen_systems
            .lock()
            .expect("systems")
            .push(system.to_string());
        self.seen_messages.lock().expect("messages").push(messages);
        let delay_ms = self.delays_ms.lock().expect("delays").remove(0);
        if delay_ms > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
        }
        self.responses.lock().expect("responses").remove(0)
    }
}

#[cfg(test)]
impl RecordingProvider {
    pub(crate) fn new(
        responses: Vec<Result<crate::llm::MessagesResponse, crate::error::LlmError>>,
        delays_ms: Vec<u64>,
    ) -> Self {
        assert_eq!(
            responses.len(),
            delays_ms.len(),
            "RecordingProvider::new requires one delay value per response"
        );
        Self {
            responses: std::sync::Arc::new(std::sync::Mutex::new(responses)),
            seen_messages: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
            seen_systems: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
            delays_ms: std::sync::Arc::new(std::sync::Mutex::new(delays_ms)),
        }
    }

    pub(crate) fn seen_messages(&self) -> Vec<Vec<Message>> {
        self.seen_messages.lock().expect("messages").clone()
    }

    pub(crate) fn seen_systems(&self) -> Vec<String> {
        self.seen_systems.lock().expect("systems").clone()
    }
}

#[cfg(test)]
pub(crate) fn test_config(data_dir: String) -> crate::config::Config {
    crate::config::Config {
        default_provider: "openai".to_string(),
        default_model: Some("gpt-4o-mini".to_string()),
        providers: std::collections::HashMap::from([(
            "openai".to_string(),
            crate::config::ProviderConfig {
                label: "OpenAI".to_string(),
                base_url: "https://api.openai.com/v1".to_string(),
                api_key: Some(secrecy::SecretString::new(
                    "sk-test".to_string().into_boxed_str(),
                )),
                default_model: "gpt-4o-mini".to_string(),
                models: vec!["gpt-4o-mini".to_string()],
            },
        )]),
        data_dir,
        log_level: "info".to_string(),
        compaction_timeout_secs: 180,
        max_history_messages: 50,
        max_session_messages: 40,
        compact_keep_recent: 20,
        channels: std::collections::HashMap::from([(
            "web".to_string(),
            crate::config::ChannelConfig {
                enabled: Some(true),
                host: Some("127.0.0.1".to_string()),
                port: Some(10961),
                ..Default::default()
            },
        )]),
    }
}

#[cfg(test)]
pub(crate) fn test_config_with_compaction(
    data_dir: String,
    max_session_messages: usize,
    compact_keep_recent: usize,
) -> crate::config::Config {
    let mut config = test_config(data_dir);
    config.max_session_messages = max_session_messages;
    config.compact_keep_recent = compact_keep_recent;
    config
}

#[cfg(test)]
pub(crate) fn cli_context(session: &str) -> SurfaceContext {
    SurfaceContext {
        channel: "cli".to_string(),
        surface_user: "local_user".to_string(),
        surface_thread: session.to_string(),
        chat_type: "cli".to_string(),
    }
}

#[cfg(test)]
pub(crate) fn tool_result_message(status: &str, result: &str) -> Message {
    Message {
        role: "tool".to_string(),
        content: crate::llm::MessageContent::text(
            serde_json::json!({
                "tool": "read",
                "status": status,
                "result": result,
            })
            .to_string(),
        ),
        tool_calls: Vec::new(),
        tool_call_id: Some("call-1".to_string()),
    }
}

#[cfg(test)]
pub(crate) fn build_state(
    config: crate::config::Config,
    llm: Box<dyn crate::llm::LlmProvider>,
) -> AppState {
    use crate::assets::AssetStore;
    use crate::channel_adapter::ChannelRegistry;
    use crate::skills::SkillManager;
    use crate::storage::Database;
    use crate::tools::ToolRegistry;

    let data_dir = config.data_dir.clone();
    let db = std::sync::Arc::new(Database::new(&data_dir).expect("db"));
    let skills = std::sync::Arc::new(SkillManager::from_skills_dir(
        config.skills_dir().expect("skills_dir"),
    ));
    AppState {
        db,
        config: config.clone(),
        config_path: None,
        llm_override: Some(std::sync::Arc::from(llm)),
        channels: std::sync::Arc::new(ChannelRegistry::new()),
        skills: std::sync::Arc::clone(&skills),
        tools: std::sync::Arc::new(ToolRegistry::new(&config, skills)),
        assets: std::sync::Arc::new(AssetStore::new(&data_dir).expect("assets")),
    }
}

#[cfg(test)]
pub(crate) fn build_state_with_provider(
    data_dir: String,
    llm: Box<dyn crate::llm::LlmProvider>,
) -> AppState {
    build_state(test_config(data_dir), llm)
}

#[cfg(test)]
mod tests {
    use super::{
        FailingProvider, FakeProvider, RecordingProvider, build_state_with_provider, cli_context,
    };
    use serial_test::serial;
    use std::sync::Arc;

    use crate::agent_loop::process_turn;
    use crate::error::EgoPulseError;
    use crate::llm::{MessagesResponse, ToolCall};
    use crate::storage::call_blocking;

    #[tokio::test]
    #[serial]
    async fn process_turn_executes_tool_calls_and_persists_outputs() {
        let dir = tempfile::tempdir().expect("tempdir");
        let relative_path = format!("tests/{}/notes.txt", uuid::Uuid::new_v4());
        let provider = RecordingProvider::new(
            vec![
                Ok(MessagesResponse {
                    content: "Let me check this. <thinking>internal</thinking>".to_string(),
                    tool_calls: vec![ToolCall {
                        id: "call-1".to_string(),
                        name: "read".to_string(),
                        arguments: serde_json::json!({"path": relative_path}),
                    }],
                }),
                Ok(MessagesResponse {
                    content: "All set".to_string(),
                    tool_calls: Vec::new(),
                }),
            ],
            vec![0, 0],
        );
        let state = build_state_with_provider(
            dir.path().to_str().expect("utf8").to_string(),
            Box::new(provider.clone()),
        );
        let workspace = state.config.workspace_dir().expect("workspace_dir");
        let note_path = workspace.join(&relative_path);
        std::fs::create_dir_all(note_path.parent().expect("note parent")).expect("workspace");
        std::fs::write(&note_path, "hello from tool").expect("notes");

        let reply = process_turn(&state, &cli_context("tool-flow"), "please read the note")
            .await
            .expect("process turn");
        assert_eq!(reply, "All set");

        let chat_id = call_blocking(Arc::clone(&state.db), move |db| {
            db.resolve_or_create_chat_id("cli", "cli:tool-flow", Some("tool-flow"), "cli")
        })
        .await
        .expect("chat id");
        let tool_calls = call_blocking(Arc::clone(&state.db), move |db| {
            db.get_tool_calls_for_chat(chat_id)
        })
        .await
        .expect("tool calls");
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0].tool_name, "read");
        let tool_output = tool_calls[0].tool_output.as_deref().expect("tool output");
        let payload: serde_json::Value =
            serde_json::from_str(tool_output).expect("tool output json");
        assert_eq!(payload["status"], "success");
        assert_eq!(payload["tool"], "read");
        assert!(
            payload["result"]
                .as_str()
                .expect("tool result string")
                .contains("hello from tool")
        );

        let seen_messages = provider.seen_messages();
        assert_eq!(seen_messages.len(), 2);
        assert_eq!(
            seen_messages[1][1].content.as_text_lossy(),
            "Let me check this."
        );
        assert!(
            !seen_messages[1][1]
                .content
                .as_text_lossy()
                .contains("<thinking>")
        );
    }

    #[tokio::test]
    #[serial]
    async fn process_turn_surfaces_llm_failure() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = build_state_with_provider(
            dir.path().to_str().expect("utf8").to_string(),
            Box::new(FailingProvider),
        );

        let error = process_turn(&state, &cli_context("failure"), "hello")
            .await
            .expect_err("should fail");
        assert!(matches!(error, EgoPulseError::Llm(_)));
    }

    #[tokio::test]
    #[serial]
    async fn normal_tool_flow_still_works_after_port() {
        // Regression: existing tool flow with multiple tool calls should still work
        let dir = tempfile::tempdir().expect("tempdir");
        let relative_path = format!("tests/{}/a.txt", uuid::Uuid::new_v4());
        let state = build_state_with_provider(
            dir.path().to_str().expect("utf8").to_string(),
            Box::new(FakeProvider {
                responses: std::sync::Mutex::new(vec![
                    MessagesResponse {
                        content: "Let me read that file.".to_string(),
                        tool_calls: vec![ToolCall {
                            id: "call-1".to_string(),
                            name: "read".to_string(),
                            arguments: serde_json::json!({"path": relative_path}),
                        }],
                    },
                    MessagesResponse {
                        content: "Done reading. Final answer.".to_string(),
                        tool_calls: Vec::new(),
                    },
                ]),
            }),
        );
        let workspace = state.config.workspace_dir().expect("workspace_dir");
        let file_path = workspace.join(&relative_path);
        std::fs::create_dir_all(file_path.parent().expect("file parent")).expect("workspace");
        std::fs::write(&file_path, "content").expect("a.txt");

        let reply = process_turn(
            &state,
            &cli_context("regression-tool"),
            &format!("read {relative_path}"),
        )
        .await
        .expect("process turn");
        assert_eq!(reply, "Done reading. Final answer.");
    }

    #[tokio::test]
    #[serial]
    async fn malformed_tool_calls_are_skipped_and_error_returned() {
        // All tool calls have empty names → malformed → error
        let dir = tempfile::tempdir().expect("tempdir");
        let state = build_state_with_provider(
            dir.path().to_str().expect("utf8").to_string(),
            Box::new(FakeProvider {
                responses: std::sync::Mutex::new(vec![MessagesResponse {
                    content: String::new(),
                    tool_calls: vec![ToolCall {
                        id: "call-malformed".to_string(),
                        name: String::new(),
                        arguments: serde_json::json!({}),
                    }],
                }]),
            }),
        );

        let error = process_turn(&state, &cli_context("malformed"), "test")
            .await
            .expect_err("should fail with malformed tool calls");
        assert!(matches!(error, EgoPulseError::Llm(_)));
    }
}
