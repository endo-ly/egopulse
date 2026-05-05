//! エージェントの 1 ターン処理を実行するモジュール。
//!
//! セッション復元、LLM 応答、ツール呼び出し、イベント通知、永続化を
//! 1 本の turn loop としてまとめて扱う。

use crate::agent_loop::SurfaceContext;
use crate::agent_loop::compaction::{PromptContext, maybe_compact_messages};
use crate::agent_loop::formatting::{
    format_tool_result, preview_text, sanitize_assistant_response_text, strip_thinking,
    summarize_tool_calls_with_content, tool_message_content,
};
use crate::agent_loop::guards::{is_declarative_only_reply, runtime_guard_messages};
pub(crate) use crate::agent_loop::prompt_builder::build_system_prompt;
use crate::agent_loop::session::{
    PersistedTurn, load_messages_for_turn, persist_phase, persist_phase_messages,
    persist_phase_once, resolve_chat_id,
};
use crate::channels::web::sse::AgentEvent;
use crate::error::{EgoPulseError, StorageError};
use crate::llm::{Message, ToolCall};
use crate::runtime::{AppState, build_app_state};
use crate::storage::{StoredMessage, ToolCall as StoredToolCall, call_blocking};
use crate::tools::ToolExecutionContext;
use futures_util::future::join_all;
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
        agent_id: state.config.default_agent.to_string(),
    };

    tokio::select! {
        response = process_turn(&state, &context, prompt) => response,
        _ = tokio::signal::ctrl_c() => Err(EgoPulseError::ShutdownRequested),
    }
}

/// Processes a turn and aborts cleanly when Ctrl-C is received.
pub(crate) async fn send_turn(
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
pub(crate) async fn process_turn(
    state: &AppState,
    context: &SurfaceContext,
    user_input: &str,
) -> Result<String, EgoPulseError> {
    process_turn_inner(state, context, user_input, Option::<fn(AgentEvent)>::None).await
}

/// Processes one user turn and emits lifecycle events for streaming consumers.
pub(crate) async fn process_turn_with_events<F>(
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
    let chat_id = resolve_chat_id(state, context).await.inspect_err(|e| {
        warn!(
            error_kind = e.error_kind(),
            error = %e,
            channel = context.channel,
            surface_thread = context.surface_thread,
            "resolve_chat_id failed"
        );
    })?;
    let tool_context = ToolExecutionContext {
        chat_id,
        channel: context.channel.clone(),
        surface_thread: context.surface_thread.clone(),
        chat_type: context.chat_type.clone(),
    };
    let system_prompt = build_system_prompt(state, context);
    let channel_llm = state.llm_for_context(context).inspect_err(|e| {
        warn!(
            error_kind = e.error_kind(),
            error = %e,
            channel = context.channel,
            "llm_for_context failed"
        );
    })?;

    let user_message = Message::text("user", user_input);

    let tool_defs = state.tools.definitions_async().await;
    let tools_json = serde_json::to_string(&tool_defs).ok();
    let prompt_ctx = PromptContext {
        system_prompt: &system_prompt,
        tools_json: tools_json.as_deref(),
    };

    let (mut messages, mut session_updated_at) = persist_user_turn_with_compaction(
        state,
        context,
        chat_id,
        &user_message,
        user_input,
        &channel_llm,
        &prompt_ctx,
    )
    .await?;

    // LLM → tool execution → tool result feedback を 1 反復として回し、
    // tool_calls が空になるまで続ける。
    // 「宣言だけして終わる」「空応答」「壊れた tool call」に耐性を持たせる。
    let mut empty_reply_retry_attempted = false;
    let mut declarative_retry_attempted = false;
    let mut retry_messages: Option<Vec<Message>> = None;

    for iteration in 1..=MAX_TOOL_ITERATIONS {
        emit_event(&on_event, AgentEvent::Iteration { iteration });
        let request_messages = retry_messages.take().unwrap_or_else(|| messages.clone());

        let response = channel_llm
            .send_message(&system_prompt, request_messages, Some(tool_defs.clone()))
            .await
            .inspect_err(|e| {
                warn!(error = %e, iteration, "LLM send_message failed");
            })?;

        if let Some(usage) = &response.usage {
            let db = Arc::clone(&state.db);
            let channel = context.channel.clone();
            let provider = channel_llm.provider_name().to_string();
            let model = channel_llm.model_name().to_string();
            let input_tokens = usage.input_tokens;
            let output_tokens = usage.output_tokens;
            tokio::spawn(async move {
                let _ = call_blocking(db, move |db| {
                    db.log_llm_usage(&crate::storage::LlmUsageLogEntry {
                        chat_id,
                        caller_channel: &channel,
                        provider: &provider,
                        model: &model,
                        input_tokens,
                        output_tokens,
                        request_kind: "agent_loop",
                    })
                })
                .await
                .inspect_err(|e| warn!(error = %e, "llm usage logging failed"));
            });
        }

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

        if let Ok(compacted) = maybe_compact_messages(
            state,
            context,
            chat_id,
            &messages,
            &channel_llm,
            &prompt_ctx,
        )
        .await
        {
            messages = compacted;
        }
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
    let mut index_by_id = std::collections::HashMap::new();
    let mut valid = Vec::new();

    for tool_call in tool_calls {
        if tool_call.name.trim().is_empty() || tool_call.id.trim().is_empty() {
            warn!(
                "skipping malformed tool call (empty name or id): id='{}' name='{}'",
                tool_call.id, tool_call.name
            );
            continue;
        }

        if let Some(index) = index_by_id.get(&tool_call.id).copied() {
            warn!(
                "replacing duplicate tool call id in assistant response with latest item: id='{}' name='{}'",
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
    let mut messages = messages;
    let persisted = persist_tool_call_assistant_message(
        state,
        tool_context.chat_id,
        &assistant_message_id,
        response_content,
        &valid_tool_calls,
        messages,
        session_updated_at,
    )
    .await?;
    messages = persisted.messages;
    let session_updated_at = Some(persisted.updated_at);

    let tool_messages = execute_tool_calls(
        state,
        on_event,
        tool_context,
        &assistant_message_id,
        valid_tool_calls,
    )
    .await?;
    let persisted = persist_tool_result_messages(
        state,
        tool_context.chat_id,
        messages,
        tool_messages,
        session_updated_at,
    )
    .await?;
    messages = persisted.messages;
    let session_updated_at = Some(persisted.updated_at);

    Ok((messages, session_updated_at))
}

async fn persist_tool_call_assistant_message(
    state: &AppState,
    chat_id: i64,
    assistant_message_id: &str,
    response_content: &str,
    valid_tool_calls: &[ToolCall],
    mut messages: Vec<Message>,
    session_updated_at: Option<String>,
) -> Result<PersistedTurn, EgoPulseError> {
    let assistant_text = sanitize_assistant_response_text(response_content);
    let assistant_preview = summarize_tool_calls_with_content(&assistant_text, valid_tool_calls);
    let assistant_message = Message {
        role: "assistant".to_string(),
        content: crate::llm::MessageContent::text(assistant_text),
        tool_calls: valid_tool_calls.to_vec(),
        tool_call_id: None,
    };

    messages.push(assistant_message.clone());

    persist_phase(
        state,
        StoredMessage {
            id: assistant_message_id.to_string(),
            chat_id,
            sender_name: "egopulse".to_string(),
            content: assistant_preview,
            is_from_bot: true,
            timestamp: chrono::Utc::now().to_rfc3339(),
        },
        assistant_message,
        &messages,
        session_updated_at,
    )
    .await
}

async fn persist_tool_result_messages(
    state: &AppState,
    chat_id: i64,
    messages: Vec<Message>,
    tool_messages: Vec<Message>,
    session_updated_at: Option<String>,
) -> Result<PersistedTurn, EgoPulseError> {
    if tool_messages.is_empty() {
        return Ok(PersistedTurn {
            updated_at: session_updated_at.unwrap_or_default(),
            messages,
        });
    }

    let mut messages_with_tools = messages;
    messages_with_tools.extend(tool_messages.iter().cloned());
    let preview = summarize_tool_result_messages(&tool_messages);
    persist_phase_messages(
        state,
        StoredMessage {
            id: uuid::Uuid::new_v4().to_string(),
            chat_id,
            sender_name: "egopulse".to_string(),
            content: preview,
            is_from_bot: true,
            timestamp: chrono::Utc::now().to_rfc3339(),
        },
        tool_messages,
        &messages_with_tools,
        session_updated_at,
    )
    .await
}

fn summarize_tool_result_messages(tool_messages: &[Message]) -> String {
    let joined = tool_messages
        .iter()
        .map(|message| message.content.as_text_lossy())
        .collect::<Vec<_>>()
        .join("\n");
    preview_text(&joined, 160)
}

async fn execute_tool_calls<F>(
    state: &AppState,
    on_event: &Option<F>,
    tool_context: &ToolExecutionContext,
    assistant_message_id: &str,
    valid_tool_calls: Vec<ToolCall>,
) -> Result<Vec<Message>, EgoPulseError>
where
    F: Fn(AgentEvent) + Send + Sync,
{
    let all_read_only = valid_tool_calls
        .iter()
        .all(|tc| state.tools.is_read_only(&tc.name));

    if all_read_only {
        return execute_tool_calls_parallel(
            state,
            on_event,
            tool_context,
            assistant_message_id,
            valid_tool_calls,
        )
        .await;
    }

    execute_tool_calls_sequential(
        state,
        on_event,
        tool_context,
        assistant_message_id,
        valid_tool_calls,
    )
    .await
}

async fn execute_tool_calls_parallel<F>(
    state: &AppState,
    on_event: &Option<F>,
    tool_context: &ToolExecutionContext,
    assistant_message_id: &str,
    valid_tool_calls: Vec<ToolCall>,
) -> Result<Vec<Message>, EgoPulseError>
where
    F: Fn(AgentEvent) + Send + Sync,
{
    let tool_futures: Vec<_> = valid_tool_calls
        .into_iter()
        .map(|tool_call| {
            execute_tool_call(
                state,
                on_event,
                tool_context,
                assistant_message_id,
                tool_call,
            )
        })
        .collect();
    let results = join_all(tool_futures).await;
    results.into_iter().collect()
}

async fn execute_tool_calls_sequential<F>(
    state: &AppState,
    on_event: &Option<F>,
    tool_context: &ToolExecutionContext,
    assistant_message_id: &str,
    valid_tool_calls: Vec<ToolCall>,
) -> Result<Vec<Message>, EgoPulseError>
where
    F: Fn(AgentEvent) + Send + Sync,
{
    let mut messages = Vec::with_capacity(valid_tool_calls.len());
    for tool_call in valid_tool_calls {
        messages.push(
            execute_tool_call(
                state,
                on_event,
                tool_context,
                assistant_message_id,
                tool_call,
            )
            .await?,
        );
    }
    Ok(messages)
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
    update_tool_call_output(
        state,
        tool_context.chat_id,
        assistant_message_id,
        &tool_call.id,
        &tool_payload,
    )
    .await?;

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
    chat_id: i64,
    message_id: &str,
    tool_call_id: &str,
    output: &str,
) -> Result<(), EgoPulseError> {
    let message_id = message_id.to_string();
    let tool_call_id = tool_call_id.to_string();
    let output = output.to_string();
    call_blocking(Arc::clone(&state.db), move |db| {
        db.update_tool_call_output_for_message(chat_id, &message_id, &tool_call_id, &output)
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
    prompt_ctx: &PromptContext<'_>,
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
        let mut candidate_messages = loaded.messages.clone();
        candidate_messages.push(user_message.clone());
        let candidate_messages = maybe_compact_messages(
            state,
            context,
            chat_id,
            &candidate_messages,
            llm,
            prompt_ctx,
        )
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

    fn provider_name(&self) -> &str {
        "test"
    }

    fn model_name(&self) -> &str {
        "test-model"
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

    fn provider_name(&self) -> &str {
        "test"
    }

    fn model_name(&self) -> &str {
        "test-model"
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

    fn provider_name(&self) -> &str {
        "test"
    }

    fn model_name(&self) -> &str {
        "test-model"
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
pub(crate) fn test_config(state_root: String) -> crate::config::Config {
    crate::test_util::test_config(&state_root)
}

#[cfg(test)]
pub(crate) fn test_config_with_compaction(
    state_root: String,
    _max_session_messages: usize,
    compact_keep_recent: usize,
) -> crate::config::Config {
    let mut config = crate::test_util::test_config(&state_root);
    config.compact_keep_recent = compact_keep_recent;
    config.default_context_window_tokens = 9000;
    config.compaction_threshold_ratio = 0.01;
    config
}

#[cfg(test)]
pub(crate) fn cli_context(session: &str) -> SurfaceContext {
    crate::test_util::cli_context(session)
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
    use crate::channels::adapter::ChannelRegistry;
    use crate::skills::SkillManager;
    use crate::storage::Database;
    use crate::tools::ToolRegistry;

    let db = std::sync::Arc::new(Database::new(&config.db_path()).expect("db"));
    let skills = std::sync::Arc::new(SkillManager::from_dirs(
        config.user_skills_dir().expect("user_skills_dir"),
        config.skills_dir().expect("skills_dir"),
    ));
    let soul_agents = std::sync::Arc::new(crate::soul_agents::SoulAgentsLoader::new(&config));
    AppState {
        db,
        config: config.clone(),
        config_path: None,
        llm_override: Some(std::sync::Arc::from(llm)),
        channels: std::sync::Arc::new(ChannelRegistry::new()),
        skills: std::sync::Arc::clone(&skills),
        tools: std::sync::Arc::new(ToolRegistry::new(&config, skills)),
        mcp_manager: None,
        assets: std::sync::Arc::new(AssetStore::new(&config.assets_dir()).expect("assets")),
        soul_agents,
        llm_cache: std::sync::Mutex::new(std::collections::HashMap::new()),
    }
}

#[cfg(test)]
pub(crate) fn build_state_with_provider(
    state_root: String,
    llm: Box<dyn crate::llm::LlmProvider>,
) -> AppState {
    build_state(test_config(state_root), llm)
}

#[cfg(test)]
mod tests {
    use super::{
        FailingProvider, FakeProvider, RecordingProvider, SurfaceContext, build_state,
        build_state_with_provider, build_system_prompt, cli_context, test_config,
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
                    usage: None,
                }),
                Ok(MessagesResponse {
                    content: "All set".to_string(),
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
                        usage: None,
                    },
                    MessagesResponse {
                        content: "Done reading. Final answer.".to_string(),
                        tool_calls: Vec::new(),
                        usage: None,
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
    async fn repeated_provider_tool_call_ids_do_not_break_later_turns() {
        let dir = tempfile::tempdir().expect("tempdir");
        let relative_path = format!("tests/{}/repeat.txt", uuid::Uuid::new_v4());
        let state = build_state_with_provider(
            dir.path().to_str().expect("utf8").to_string(),
            Box::new(FakeProvider {
                responses: std::sync::Mutex::new(vec![
                    MessagesResponse {
                        content: "Reading once.".to_string(),
                        tool_calls: vec![ToolCall {
                            id: "call-repeat".to_string(),
                            name: "read".to_string(),
                            arguments: serde_json::json!({"path": relative_path.clone()}),
                        }],
                        usage: None,
                    },
                    MessagesResponse {
                        content: "First done.".to_string(),
                        tool_calls: Vec::new(),
                        usage: None,
                    },
                    MessagesResponse {
                        content: "Reading again.".to_string(),
                        tool_calls: vec![ToolCall {
                            id: "call-repeat".to_string(),
                            name: "read".to_string(),
                            arguments: serde_json::json!({"path": relative_path.clone()}),
                        }],
                        usage: None,
                    },
                    MessagesResponse {
                        content: "Second done.".to_string(),
                        tool_calls: Vec::new(),
                        usage: None,
                    },
                ]),
            }),
        );
        let workspace = state.config.workspace_dir().expect("workspace_dir");
        let file_path = workspace.join(&relative_path);
        std::fs::create_dir_all(file_path.parent().expect("file parent")).expect("workspace");
        std::fs::write(&file_path, "repeat content").expect("repeat.txt");

        let context = cli_context("repeated-tool-call-id");
        let first = process_turn(&state, &context, "read once")
            .await
            .expect("first turn");
        let second = process_turn(&state, &context, "read again")
            .await
            .expect("second turn");

        assert_eq!(first, "First done.");
        assert_eq!(second, "Second done.");
        let chat_id = call_blocking(Arc::clone(&state.db), move |db| {
            db.resolve_or_create_chat_id(
                "cli",
                "cli:repeated-tool-call-id",
                Some("repeated-tool-call-id"),
                "cli",
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
        assert!(tool_calls.iter().all(|call| call.id == "call-repeat"));
        assert!(tool_calls.iter().all(|call| call.tool_output.is_some()));
    }

    #[tokio::test]
    #[serial]
    async fn duplicate_tool_call_ids_in_same_response_are_executed_once() {
        let dir = tempfile::tempdir().expect("tempdir");
        let relative_path = format!("tests/{}/duplicate.txt", uuid::Uuid::new_v4());
        let provider = RecordingProvider::new(
            vec![
                Ok(MessagesResponse {
                    content: "Reading.".to_string(),
                    tool_calls: vec![
                        ToolCall {
                            id: "call-duplicate".to_string(),
                            name: "read".to_string(),
                            arguments: serde_json::json!({}),
                        },
                        ToolCall {
                            id: "call-duplicate".to_string(),
                            name: "read".to_string(),
                            arguments: serde_json::json!({"path": relative_path.clone()}),
                        },
                    ],
                    usage: None,
                }),
                Ok(MessagesResponse {
                    content: "Done.".to_string(),
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
        let file_path = workspace.join(&relative_path);
        std::fs::create_dir_all(file_path.parent().expect("file parent")).expect("workspace");
        std::fs::write(&file_path, "duplicate content").expect("duplicate.txt");

        let reply = process_turn(&state, &cli_context("duplicate-tool-call-id"), "read it")
            .await
            .expect("process turn");

        assert_eq!(reply, "Done.");
        let chat_id = call_blocking(Arc::clone(&state.db), move |db| {
            db.resolve_or_create_chat_id(
                "cli",
                "cli:duplicate-tool-call-id",
                Some("duplicate-tool-call-id"),
                "cli",
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
        assert_eq!(tool_calls[0].id, "call-duplicate");
        assert!(tool_calls[0].tool_input.contains(&relative_path));
        assert!(tool_calls[0].tool_output.is_some());

        let seen_messages = provider.seen_messages();
        assert_eq!(seen_messages.len(), 2);
        assert_eq!(seen_messages[1][1].role, "assistant");
        assert_eq!(seen_messages[1][1].tool_calls.len(), 1);
        assert_eq!(seen_messages[1][1].tool_calls[0].id, "call-duplicate");
        assert_eq!(
            seen_messages[1][1].tool_calls[0].arguments["path"],
            relative_path
        );
        assert_eq!(seen_messages[1][2].role, "tool");
        assert_eq!(
            seen_messages[1][2].tool_call_id.as_deref(),
            Some("call-duplicate")
        );
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
                    usage: None,
                }]),
            }),
        );

        let error = process_turn(&state, &cli_context("malformed"), "test")
            .await
            .expect_err("should fail with malformed tool calls");
        assert!(matches!(error, EgoPulseError::Llm(_)));
    }

    fn web_context(session: &str) -> SurfaceContext {
        SurfaceContext {
            channel: "web".to_string(),
            surface_user: "user".to_string(),
            surface_thread: session.to_string(),
            chat_type: "web".to_string(),
            agent_id: "default".to_string(),
        }
    }

    fn write_file(path: &std::path::Path, content: &str) {
        std::fs::create_dir_all(path.parent().expect("parent")).expect("create_dir");
        std::fs::write(path, content).expect("write");
    }

    #[test]
    fn system_prompt_contains_soul_section_when_file_exists() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_file(&dir.path().join("SOUL.md"), "I am a wise assistant.");
        let state =
            build_state_with_provider(dir.path().to_str().expect("utf8").to_string(), no_tools());
        let prompt = build_system_prompt(&state, &web_context("s1"));

        assert!(prompt.contains("<soul>"), "should contain <soul> tag");
        assert!(prompt.contains("</soul>"), "should contain </soul> tag");
        assert!(
            prompt.contains("I am a wise assistant."),
            "should contain SOUL.md content"
        );
    }

    #[test]
    fn system_prompt_uses_default_identity_when_no_soul() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state =
            build_state_with_provider(dir.path().to_str().expect("utf8").to_string(), no_tools());
        let prompt = build_system_prompt(&state, &web_context("s1"));

        assert!(
            !prompt.contains("<soul>"),
            "should not contain <soul> tag when no SOUL.md"
        );
        assert!(
            prompt.contains("You are an AI assistant running on the"),
            "should contain identity text"
        );
    }

    #[test]
    fn system_prompt_contains_agents_section_when_file_exists() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_file(
            &dir.path().join("AGENTS.md"),
            "Use Rust for all code tasks.",
        );
        let state =
            build_state_with_provider(dir.path().to_str().expect("utf8").to_string(), no_tools());
        let prompt = build_system_prompt(&state, &web_context("s1"));

        assert!(prompt.contains("# Memories"), "should contain # Memories");
        assert!(prompt.contains("<agents>"), "should contain <agents>");
        assert!(
            prompt.contains("Use Rust for all code tasks."),
            "should contain AGENTS.md content"
        );
    }

    #[test]
    fn system_prompt_no_agents_section_when_no_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state =
            build_state_with_provider(dir.path().to_str().expect("utf8").to_string(), no_tools());
        let prompt = build_system_prompt(&state, &web_context("s1"));

        assert!(
            !prompt.contains("# Memories"),
            "should not contain # Memories when no AGENTS.md"
        );
        assert!(
            !prompt.contains("<agents>"),
            "should not contain <agents> when no AGENTS.md"
        );
    }

    #[test]
    fn system_prompt_order_soul_before_identity() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_file(&dir.path().join("SOUL.md"), "Soul content here");
        let state =
            build_state_with_provider(dir.path().to_str().expect("utf8").to_string(), no_tools());
        let prompt = build_system_prompt(&state, &web_context("s1"));

        let soul_pos = prompt.find("<soul>").expect("should find <soul>");
        let identity_pos = prompt
            .find("Built-in execution playbook")
            .expect("should find execution playbook");
        assert!(
            soul_pos < identity_pos,
            "<soul> should appear before execution playbook"
        );
    }

    #[test]
    fn system_prompt_order_agents_before_skills() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_file(&dir.path().join("AGENTS.md"), "Agents content");
        std::fs::create_dir_all(dir.path().join("workspace/skills")).expect("workspace/skills");
        let skill_dir = dir.path().join("skills/test-skill");
        write_file(
            &skill_dir.join("SKILL.md"),
            "---\nname: test-skill\ndescription: A test skill\n---\nInstructions",
        );
        let state =
            build_state_with_provider(dir.path().to_str().expect("utf8").to_string(), no_tools());
        let prompt = build_system_prompt(&state, &web_context("s1"));

        let memories_pos = prompt.find("# Memories").expect("should find # Memories");
        let skills_pos = prompt
            .find("# Agent Skills")
            .expect("should find # Agent Skills");
        assert!(
            memories_pos < skills_pos,
            "# Memories should appear before # Agent Skills"
        );
    }

    #[test]
    fn system_prompt_chat_agents_ignored_in_favor_of_global() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_file(&dir.path().join("AGENTS.md"), "Global agents content");
        let chat_agents = dir.path().join("runtime/groups/web/thread1/AGENTS.md");
        write_file(&chat_agents, "Chat-specific agents content");
        let state =
            build_state_with_provider(dir.path().to_str().expect("utf8").to_string(), no_tools());
        let prompt = build_system_prompt(&state, &web_context("thread1"));

        assert!(prompt.contains("<agents>"), "should contain <agents>");
        assert!(
            prompt.contains("Global agents content"),
            "should contain global AGENTS.md content"
        );
        assert!(
            !prompt.contains("<chat-agents>"),
            "should NOT contain <chat-agents>"
        );
        assert!(
            !prompt.contains("Chat-specific agents content"),
            "should NOT contain chat AGENTS.md content"
        );
    }

    #[test]
    fn system_prompt_chat_soul_no_longer_overrides_global() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_file(&dir.path().join("SOUL.md"), "global soul content");
        let chat_soul = dir.path().join("runtime/groups/web/thread1/SOUL.md");
        write_file(&chat_soul, "chat soul content");
        let state =
            build_state_with_provider(dir.path().to_str().expect("utf8").to_string(), no_tools());
        let prompt = build_system_prompt(&state, &web_context("thread1"));

        assert!(
            prompt.contains("global soul content"),
            "should contain global SOUL content"
        );
        assert!(
            !prompt.contains("chat soul content"),
            "should NOT contain chat SOUL content"
        );
    }

    #[test]
    fn system_prompt_channel_soul_from_config() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_file(&dir.path().join("souls/work.md"), "Work soul content");
        let mut config = test_config(dir.path().to_str().expect("utf8").to_string());
        config.channels.insert(
            crate::config::ChannelName::new("web"),
            crate::config::ChannelConfig {
                enabled: Some(true),
                soul_path: Some("work".to_string()),
                ..Default::default()
            },
        );
        let state = build_state(config, no_tools());
        let prompt = build_system_prompt(&state, &web_context("s1"));

        assert!(
            prompt.contains("Work soul content"),
            "should contain channel soul_path content"
        );
    }

    #[test]
    fn system_prompt_channel_soul_fallback_to_default() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_file(&dir.path().join("SOUL.md"), "Default soul content");
        let state =
            build_state_with_provider(dir.path().to_str().expect("utf8").to_string(), no_tools());
        let prompt = build_system_prompt(&state, &web_context("s1"));

        assert!(
            prompt.contains("Default soul content"),
            "should contain default SOUL.md content"
        );
    }

    #[test]
    fn system_prompt_account_soul_does_not_break_when_not_implemented() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_file(&dir.path().join("SOUL.md"), "Default soul");
        let state =
            build_state_with_provider(dir.path().to_str().expect("utf8").to_string(), no_tools());
        let prompt = build_system_prompt(&state, &web_context("s1"));

        assert!(
            prompt.contains("Default soul"),
            "account_id=None should not break soul loading"
        );
        assert!(
            prompt.contains("Built-in execution playbook"),
            "should still contain identity section"
        );
    }

    fn no_tools() -> Box<dyn crate::llm::LlmProvider> {
        Box::new(FakeProvider {
            responses: std::sync::Mutex::new(vec![]),
        })
    }

    #[tokio::test]
    #[serial]
    async fn process_turn_logs_llm_usage_on_agent_loop() {
        let dir = tempfile::tempdir().expect("tempdir");
        let provider = RecordingProvider::new(
            vec![Ok(MessagesResponse {
                content: "hello world".to_string(),
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
            Box::new(provider),
        );

        let reply = process_turn(&state, &cli_context("usage-log-single"), "hi")
            .await
            .expect("process turn");
        assert_eq!(reply, "hello world");

        let chat_id = call_blocking(Arc::clone(&state.db), move |db| {
            db.resolve_or_create_chat_id(
                "cli",
                "cli:usage-log-single",
                Some("usage-log-single"),
                "cli",
            )
        })
        .await
        .expect("chat id");

        // Wait for the spawned logging task to complete
        for _ in 0..20 {
            let (requests, input_tokens, output_tokens, total_tokens) =
                call_blocking(Arc::clone(&state.db), move |db| {
                    db.get_llm_usage_summary(Some(chat_id), None, None)
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
    async fn process_turn_logs_each_iteration() {
        let dir = tempfile::tempdir().expect("tempdir");
        let relative_path = format!("tests/{}/data.txt", uuid::Uuid::new_v4());
        let provider = RecordingProvider::new(
            vec![
                Ok(MessagesResponse {
                    content: "checking".to_string(),
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
                "cli:usage-log-multi",
                Some("usage-log-multi"),
                "cli",
            )
        })
        .await
        .expect("chat id");

        for _ in 0..20 {
            let (requests, input_tokens, output_tokens, total_tokens) =
                call_blocking(Arc::clone(&state.db), move |db| {
                    db.get_llm_usage_summary(Some(chat_id), None, None)
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
    async fn usage_not_logged_when_response_has_no_usage() {
        let dir = tempfile::tempdir().expect("tempdir");
        let provider = RecordingProvider::new(
            vec![Ok(MessagesResponse {
                content: "no usage info".to_string(),
                tool_calls: vec![],
                usage: None,
            })],
            vec![0],
        );
        let state = build_state_with_provider(
            dir.path().to_str().expect("utf8").to_string(),
            Box::new(provider),
        );

        let reply = process_turn(&state, &cli_context("no-usage"), "hi")
            .await
            .expect("process turn");
        assert_eq!(reply, "no usage info");

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        let (requests, _input_tokens, _output_tokens, _total_tokens) =
            call_blocking(Arc::clone(&state.db), move |db| {
                db.get_llm_usage_summary(None, None, None)
            })
            .await
            .expect("summary");

        assert_eq!(
            requests, 0,
            "no usage records should exist when response has no usage"
        );
    }

    #[tokio::test]
    #[serial]
    async fn turn_uses_agent_llm_resolution() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let provider = RecordingProvider::new(
            vec![Ok(MessagesResponse {
                content: "agent reply".to_string(),
                tool_calls: Vec::new(),
                usage: None,
            })],
            vec![0],
        );
        let state = build_state_with_provider(
            dir.path().to_str().expect("utf8").to_string(),
            Box::new(provider.clone()),
        );

        // Act
        let result = process_turn(&state, &cli_context("agent-llm-test"), "hello")
            .await
            .expect("turn");

        // Assert
        assert_eq!(result, "agent reply");
        let systems = provider.seen_systems();
        assert_eq!(systems.len(), 1, "should have exactly one LLM call");
    }

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
            db.resolve_or_create_chat_id("cli", "cli:parallel-read", Some("parallel-read"), "cli")
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
            db.resolve_or_create_chat_id("cli", "cli:mixed-tools", Some("mixed-tools"), "cli")
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
}
