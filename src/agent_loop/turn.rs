//! エージェントの 1 ターン処理を実行するモジュール。
//!
//! セッション復元、LLM 応答、ツール呼び出し、イベント通知、永続化を
//! 1 本の turn loop としてまとめて扱う。

use crate::agent_loop::SurfaceContext;
use crate::agent_loop::session::{
    load_messages_for_turn, persist_phase, persist_phase_once, resolve_chat_id,
};
use crate::error::{EgoPulseError, StorageError};
use crate::llm::{Message, ToolCall};
use crate::runtime::{AppState, build_app_state};
use crate::storage::{StoredMessage, ToolCall as StoredToolCall, call_blocking};
use crate::tools::ToolExecutionContext;
use crate::web::sse::AgentEvent;
use tracing::{info, warn};

const MAX_TOOL_ITERATIONS: usize = 50;
const MAX_TOOL_RESULT_CHARS: usize = 16_000;
const MAX_COMPACTION_SUMMARY_CHARS: usize = 20_000;
const MAX_TOOL_RESULT_TEXT_CHARS: usize = 200;

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
            // --- end turn branch ---
            let raw_content = response.content.trim().to_string();
            let visible_text = strip_thinking(&raw_content);
            let has_displayable_output = !visible_text.trim().is_empty();

            // Guard 1: empty visible reply → retry once
            if !has_displayable_output && !empty_reply_retry_attempted {
                empty_reply_retry_attempted = true;
                warn!("empty visible reply; injecting runtime guard and retrying once");
                retry_messages = Some(runtime_guard_messages(
                    &messages,
                    &raw_content,
                    "[runtime_guard]: Your previous reply had no user-visible text. Reply again now with a concise visible answer. If tools are required, execute them first and then provide the visible result.",
                ));
                continue;
            }

            // Guard 2: declarative-only reply → retry once
            if has_displayable_output
                && !declarative_retry_attempted
                && is_declarative_only_reply(&visible_text)
            {
                declarative_retry_attempted = true;
                warn!(
                    "declarative-only reply detected; injecting corrective prompt and retrying once"
                );
                retry_messages = Some(runtime_guard_messages(
                    &messages,
                    &raw_content,
                    "[runtime_guard]: Your previous reply only declared what you would do without actually executing any tools. If the user's request requires tool calls, execute them NOW instead of just describing what you plan to do. Then provide the result.",
                ));
                continue;
            }

            // Finalize: return the response (even if empty after retries)
            if !has_displayable_output {
                return Err(EgoPulseError::Llm(crate::error::LlmError::InvalidResponse(
                    "assistant content was empty after retry".to_string(),
                )));
            }

            let final_content = visible_text.trim().to_string();
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
                &messages,
                session_updated_at,
            )
            .await?;

            emit_event(
                &on_event,
                AgentEvent::FinalResponse {
                    text: final_content.clone(),
                },
            );
            return Ok(final_content);
        }

        // --- tool use branch ---
        // Filter out malformed tool calls (empty name)
        let valid_tool_calls: Vec<ToolCall> = response
            .tool_calls
            .into_iter()
            .filter(|tc| {
                if tc.name.trim().is_empty() {
                    warn!(
                        "skipping malformed tool call with empty name (id={})",
                        tc.id
                    );
                    false
                } else {
                    true
                }
            })
            .collect();

        // If all tool calls were malformed, treat as end turn
        if valid_tool_calls.is_empty() {
            let raw_content = response.content.trim().to_string();
            let visible_text = strip_thinking(&raw_content);
            if visible_text.trim().is_empty() {
                return Err(EgoPulseError::Llm(crate::error::LlmError::InvalidResponse(
                    "all tool calls were malformed (empty names)".to_string(),
                )));
            }
            if !declarative_retry_attempted && is_declarative_only_reply(&visible_text) {
                declarative_retry_attempted = true;
                warn!(
                    "all tool calls were malformed and reply was declarative-only; retrying once"
                );
                retry_messages = Some(runtime_guard_messages(
                    &messages,
                    &raw_content,
                    "[runtime_guard]: Your previous reply attempted tool use but did not produce a valid executable tool call. If tools are required, call them now and then provide the result.",
                ));
                continue;
            }
            let final_content = visible_text.trim().to_string();

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
                &messages,
                session_updated_at,
            )
            .await?;

            emit_event(
                &on_event,
                AgentEvent::FinalResponse {
                    text: final_content.clone(),
                },
            );
            return Ok(final_content);
        }

        let assistant_message_id = uuid::Uuid::new_v4().to_string();
        let assistant_text = sanitize_assistant_response_text(&response.content);
        let assistant_preview =
            summarize_tool_calls_with_content(&assistant_text, &valid_tool_calls);
        let assistant_message = Message {
            role: "assistant".to_string(),
            content: crate::llm::MessageContent::text(assistant_text.clone()),
            tool_calls: valid_tool_calls.clone(),
            tool_call_id: None,
        };
        messages.push(assistant_message.clone());

        let persisted_assistant_turn = persist_phase(
            state,
            StoredMessage {
                id: assistant_message_id.clone(),
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
        .await?;
        messages = persisted_assistant_turn.messages;
        session_updated_at = Some(persisted_assistant_turn.updated_at);

        // Reset retry flags on successful tool use (tool execution is progress)
        empty_reply_retry_attempted = false;
        declarative_retry_attempted = false;

        for tool_call in valid_tool_calls {
            emit_event(
                &on_event,
                AgentEvent::ToolStart {
                    name: tool_call.name.clone(),
                    input: tool_call.arguments.clone(),
                },
            );

            store_pending_tool_call(state, chat_id, &assistant_message_id, &tool_call).await?;
            let tool_start = std::time::Instant::now();
            let result = state
                .tools
                .execute(&tool_call.name, tool_call.arguments.clone(), &tool_context)
                .await;
            let duration_ms = tool_start.elapsed().as_millis();
            let tool_payload = format_tool_result(&tool_call, &result);
            update_tool_call_output(state, &tool_call.id, &tool_payload).await?;

            emit_event(
                &on_event,
                AgentEvent::ToolResult {
                    name: tool_call.name.clone(),
                    is_error: result.is_error,
                    preview: preview_text(&tool_payload, 160),
                    duration_ms,
                },
            );

            messages.push(Message {
                role: "tool".to_string(),
                content: tool_message_content(&tool_payload, &result),
                tool_calls: Vec::new(),
                tool_call_id: Some(tool_call.id),
            });
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
    call_blocking(state.db.clone(), move |db| db.store_tool_call(&record))
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
    call_blocking(state.db.clone(), move |db| {
        db.update_tool_call_output(&tool_call_id, &output)
    })
    .await
    .map_err(EgoPulseError::from)
}

fn format_tool_result(tool_call: &ToolCall, result: &crate::tools::ToolResult) -> String {
    let mut content = result.content.clone();
    let details = result.details.clone();

    loop {
        let mut payload = serde_json::json!({
            "tool": tool_call.name,
            "status": if result.is_error { "error" } else { "success" },
            "result": content,
        });
        if let Some(ref d) = details {
            payload["details"] = d.clone();
        }

        let serialized = payload.to_string();
        let char_count = serialized.chars().count();

        if char_count <= MAX_TOOL_RESULT_CHARS {
            return serialized;
        }

        // If over limit, first try removing details
        if details.is_some() {
            let payload_no_details = serde_json::json!({
                "tool": tool_call.name,
                "status": if result.is_error { "error" } else { "success" },
                "result": content,
            });
            let no_details_str = payload_no_details.to_string();
            if no_details_str.chars().count() <= MAX_TOOL_RESULT_CHARS {
                return no_details_str;
            }
        }

        // Still over limit, truncate content further
        // Calculate how much we need to reduce content by
        let excess = char_count.saturating_sub(MAX_TOOL_RESULT_CHARS);
        let current_content_len = content.chars().count();
        // Reduce content by excess + buffer for JSON overhead
        let new_len = current_content_len.saturating_sub(excess + 100);
        if new_len == 0 {
            // Can't truncate further, return minimal payload
            return serde_json::json!({
                "tool": tool_call.name,
                "status": if result.is_error { "error" } else { "success" },
                "result": "...",
            })
            .to_string();
        }
        content = format!("{}...", content.chars().take(new_len).collect::<String>());
    }
}

fn tool_message_content(
    payload: &str,
    result: &crate::tools::ToolResult,
) -> crate::llm::MessageContent {
    match &result.llm_content {
        crate::llm::MessageContent::Text(_) => crate::llm::MessageContent::text(payload),
        crate::llm::MessageContent::Parts(parts) => {
            let mut content = Vec::with_capacity(parts.len() + 1);
            content.push(crate::llm::MessageContentPart::InputText {
                text: payload.to_string(),
            });
            content.extend(parts.iter().filter_map(|part| match part {
                crate::llm::MessageContentPart::InputText { .. } => None,
                crate::llm::MessageContentPart::InputImage { image_url, detail } => {
                    Some(crate::llm::MessageContentPart::InputImage {
                        image_url: image_url.clone(),
                        detail: detail.clone(),
                    })
                }
            }));
            crate::llm::MessageContent::parts(content)
        }
    }
}

fn summarize_tool_calls_with_content(content: &str, tool_calls: &[ToolCall]) -> String {
    let names = tool_calls
        .iter()
        .map(|tool_call| tool_call.name.as_str())
        .collect::<Vec<_>>();
    if content.trim().is_empty() {
        format!("[tool_call] {}", names.join(", "))
    } else {
        format!("{} [tool_call] {}", content.trim(), names.join(", "))
    }
}

fn preview_text(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }
    format!("{}...", value.chars().take(max_chars).collect::<String>())
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
        let mut candidate_messages = loaded.messages.clone();
        candidate_messages.push(user_message.clone());
        let candidate_messages =
            maybe_compact_messages(state, context, chat_id, &candidate_messages, llm).await?;

        match persist_phase_once(
            state,
            stored_message.clone(),
            &candidate_messages,
            loaded.session_updated_at.clone(),
        )
        .await
        {
            Ok(persisted) => {
                return Ok((persisted.messages, Some(persisted.updated_at)));
            }
            Err(EgoPulseError::Storage(StorageError::SessionSnapshotConflict)) if attempt == 0 => {
                loaded = load_messages_for_turn(state, chat_id).await?;
            }
            Err(error) => return Err(error),
        }
    }

    Err(EgoPulseError::Storage(
        StorageError::SessionSnapshotConflict,
    ))
}

async fn maybe_compact_messages(
    state: &AppState,
    context: &SurfaceContext,
    chat_id: i64,
    messages: &[Message],
    llm: &std::sync::Arc<dyn crate::llm::LlmProvider>,
) -> Result<Vec<Message>, EgoPulseError> {
    if messages.len() <= state.config.max_session_messages {
        return Ok(messages.to_vec());
    }

    archive_conversation(&state.config.data_dir, &context.channel, chat_id, messages).await;

    let keep_recent = state.config.compact_keep_recent.min(messages.len());
    if keep_recent == messages.len() {
        return Ok(messages.to_vec());
    }

    let split_at = messages.len() - keep_recent;
    let old_messages = &messages[..split_at];
    let recent_messages = &messages[split_at..];

    let mut summary_input = String::new();
    for message in old_messages {
        let role = &message.role;
        let text = message_to_text(message);
        summary_input.push_str(&format!("[{role}]: {text}\n\n"));
    }
    summary_input = truncate_compaction_summary_input(summary_input);

    let summarize_prompt = "Summarize the following conversation concisely, preserving key facts, decisions, tool results, and context needed to continue the conversation. Be brief but thorough.";
    let summarize_messages = vec![Message::text(
        "user",
        format!("{summarize_prompt}\n\n---\n\n{summary_input}"),
    )];
    let timeout_secs = state.config.compaction_timeout_secs;
    let summary_result = tokio::time::timeout(
        std::time::Duration::from_secs(timeout_secs),
        llm.send_message("You are a helpful summarizer.", summarize_messages, None),
    )
    .await;

    let summary = match summary_result {
        Ok(Ok(response)) => strip_thinking(&response.content),
        Ok(Err(error)) => {
            warn!("compaction summarization failed: {error}; falling back to recent messages");
            return Ok(recent_messages.to_vec());
        }
        Err(_) => {
            warn!(
                "compaction summarization timed out after {timeout_secs}s for {}:{}; falling back to recent messages",
                context.channel, chat_id
            );
            return Ok(recent_messages.to_vec());
        }
    };
    if summary.trim().is_empty() {
        warn!("compaction summarization returned empty text; falling back to recent messages");
        return Ok(recent_messages.to_vec());
    }

    let mut compacted = vec![Message::text(
        "user",
        format!("[Conversation Summary]\n{summary}"),
    )];
    if !matches!(recent_messages.first(), Some(message) if message.role == "assistant") {
        compacted.push(Message::text(
            "assistant",
            "Understood, I have the conversation context. How can I help?",
        ));
    }

    for message in recent_messages {
        append_compacted_message(&mut compacted, message);
    }

    if matches!(compacted.last(), Some(last) if last.role == "assistant") {
        compacted.pop();
    }

    Ok(compacted)
}

async fn archive_conversation(data_dir: &str, channel: &str, chat_id: i64, messages: &[Message]) {
    let data_dir = data_dir.to_string();
    let channel = channel.to_string();
    let messages = messages.to_vec();
    let join_channel = channel.clone();
    let join_result = tokio::task::spawn_blocking(move || {
        archive_conversation_blocking(&data_dir, &channel, chat_id, &messages);
    })
    .await;

    if let Err(error) = join_result {
        warn!(
            "failed to join archive task for {}:{}: {error}",
            join_channel, chat_id
        );
    }
}

fn archive_conversation_blocking(
    data_dir: &str,
    channel: &str,
    chat_id: i64,
    messages: &[Message],
) {
    let now = chrono::Utc::now().format("%Y%m%d-%H%M%S");
    let unique_suffix = uuid::Uuid::new_v4().simple();
    let channel_dir = if channel.trim().is_empty() {
        "unknown"
    } else {
        channel.trim()
    };
    let dir = std::path::PathBuf::from(data_dir)
        .join("groups")
        .join(channel_dir)
        .join(chat_id.to_string())
        .join("conversations");

    if let Err(error) = std::fs::create_dir_all(&dir) {
        warn!("failed to create archive dir {}: {error}", dir.display());
        return;
    }

    let path = dir.join(format!("{now}-{unique_suffix}.md"));
    let mut content = String::new();
    for message in messages {
        let role = &message.role;
        let text = message_to_archive_text(message);
        content.push_str(&format!("## {role}\n\n{text}\n\n---\n\n"));
    }

    if let Err(error) = std::fs::write(&path, content) {
        warn!(
            "failed to archive conversation to {}: {error}",
            path.display()
        );
    } else {
        info!(
            "archived conversation ({} messages) to {}",
            messages.len(),
            path.display()
        );
    }
}

/// Truncate the compaction summary input by character count, not by bytes.
///
/// The limit keeps UTF-8 text intact and appends `\n... (truncated)` when the
/// input exceeds `MAX_COMPACTION_SUMMARY_CHARS` characters.
fn truncate_compaction_summary_input(mut summary_input: String) -> String {
    if summary_input.chars().count() <= MAX_COMPACTION_SUMMARY_CHARS {
        return summary_input;
    }

    let cutoff = summary_input
        .char_indices()
        .nth(MAX_COMPACTION_SUMMARY_CHARS)
        .map(|(idx, _)| idx)
        .unwrap_or(summary_input.len());
    summary_input.truncate(cutoff);
    summary_input.push_str("\n... (truncated)");
    summary_input
}

fn append_compacted_message(compacted: &mut Vec<Message>, message: &Message) {
    let Some(last) = compacted.last_mut() else {
        compacted.push(message.clone());
        return;
    };

    if can_merge_compacted_messages(last, message) {
        let merged = format!(
            "{}\n{}",
            last.content.as_text_lossy(),
            message.content.as_text_lossy()
        );
        last.content = crate::llm::MessageContent::text(merged);
        return;
    }

    compacted.push(message.clone());
}

fn can_merge_compacted_messages(left: &Message, right: &Message) -> bool {
    left.role == right.role
        && left.tool_calls.is_empty()
        && right.tool_calls.is_empty()
        && left.tool_call_id.is_none()
        && right.tool_call_id.is_none()
        && matches!(left.content, crate::llm::MessageContent::Text(_))
        && matches!(right.content, crate::llm::MessageContent::Text(_))
}

fn message_to_text(message: &Message) -> String {
    let should_strip_thinking = should_strip_thinking_for_role(&message.role);
    let content = match &message.content {
        crate::llm::MessageContent::Text(text) => {
            if should_strip_thinking {
                strip_thinking(text)
            } else {
                text.clone()
            }
        }
        crate::llm::MessageContent::Parts(parts) => parts
            .iter()
            .map(|part| match part {
                crate::llm::MessageContentPart::InputText { text } => {
                    if should_strip_thinking {
                        strip_thinking(text)
                    } else {
                        text.clone()
                    }
                }
                crate::llm::MessageContentPart::InputImage { .. } => "[image]".to_string(),
            })
            .collect::<Vec<_>>()
            .join("\n"),
    };
    if message.tool_call_id.is_some() {
        let payload = tool_result_payload(message).unwrap_or_default();
        let body = strip_thinking(&tool_result_body(payload));
        let truncated = truncate_summary_text(&body, MAX_TOOL_RESULT_TEXT_CHARS);
        let prefix = if is_tool_error_message(message) {
            "[tool_error]: "
        } else {
            "[tool_result]: "
        };
        return format!("{prefix}{truncated}");
    }

    let mut parts = Vec::new();
    if !content.trim().is_empty() {
        parts.push(content);
    }
    for tool_call in &message.tool_calls {
        parts.push(format!(
            "[tool_use: {}({})]",
            tool_call.name, tool_call.arguments
        ));
    }
    parts.join("\n")
}

fn is_tool_error_message(message: &Message) -> bool {
    let Some(payload) = tool_result_payload(message) else {
        return false;
    };
    serde_json::from_str::<serde_json::Value>(payload)
        .ok()
        .and_then(|value| {
            value
                .get("status")
                .and_then(|status| status.as_str())
                .map(|status| status == "error")
        })
        .unwrap_or(false)
}

fn tool_result_body(payload: &str) -> String {
    match serde_json::from_str::<serde_json::Value>(payload) {
        Ok(value) => value
            .get("result")
            .and_then(|result| result.as_str())
            .map(ToString::to_string)
            .unwrap_or_else(|| payload.to_string()),
        Err(_) => payload.to_string(),
    }
}

fn sanitize_assistant_response_text(content: &str) -> String {
    strip_thinking(content.trim())
}

fn should_strip_thinking_for_role(role: &str) -> bool {
    matches!(role, "assistant" | "tool")
}

fn message_to_archive_text(message: &Message) -> String {
    let should_strip_thinking = should_strip_thinking_for_role(&message.role);
    let content = match &message.content {
        crate::llm::MessageContent::Text(text) => {
            if should_strip_thinking {
                strip_thinking(text)
            } else {
                text.clone()
            }
        }
        crate::llm::MessageContent::Parts(parts) => parts
            .iter()
            .map(|part| match part {
                crate::llm::MessageContentPart::InputText { text } => {
                    if should_strip_thinking {
                        strip_thinking(text)
                    } else {
                        text.clone()
                    }
                }
                crate::llm::MessageContentPart::InputImage { image_url, detail } => match detail {
                    Some(detail) => format!("[image: {image_url} detail={detail}]"),
                    None => format!("[image: {image_url}]"),
                },
            })
            .collect::<Vec<_>>()
            .join("\n"),
    };

    if message.tool_call_id.is_some() {
        let payload = tool_result_payload(message).unwrap_or_default();
        let body = strip_thinking(payload);
        let prefix = if is_tool_error_message(message) {
            "[tool_error]: "
        } else {
            "[tool_result]: "
        };
        return format!("{prefix}{body}");
    }

    let mut parts = Vec::new();
    if !content.trim().is_empty() {
        parts.push(content);
    }
    for tool_call in &message.tool_calls {
        parts.push(format!(
            "[tool_use: {}({})]",
            tool_call.name, tool_call.arguments
        ));
    }
    parts.join("\n")
}

fn tool_result_payload(message: &Message) -> Option<&str> {
    match &message.content {
        crate::llm::MessageContent::Text(text) => Some(text.as_str()),
        crate::llm::MessageContent::Parts(parts) => parts.iter().find_map(|part| match part {
            crate::llm::MessageContentPart::InputText { text } => Some(text.as_str()),
            crate::llm::MessageContentPart::InputImage { .. } => None,
        }),
    }
}

fn truncate_summary_text(text: &str, max_chars: usize) -> String {
    let char_count = text.chars().count();
    if char_count <= max_chars {
        return text.to_string();
    }
    let truncated = text.chars().take(max_chars).collect::<String>();
    format!("{truncated}...")
}

fn runtime_guard_messages(
    messages: &[Message],
    assistant_text: &str,
    guard_text: &str,
) -> Vec<Message> {
    let mut retry_messages = messages.to_vec();
    retry_messages.push(Message::text("assistant", assistant_text.to_string()));
    retry_messages.push(Message::text("user", guard_text.to_string()));
    retry_messages
}

/// `<think`>`...`</think`>` や `<thought`>`...`</thought`>` などの
/// thinking タグブロックをモデル出力から除去する。
/// microclaw agent_engine.rs から移植。
fn strip_thinking(text: &str) -> String {
    fn strip_tag_blocks(input: &str, open: &str, close: &str) -> String {
        let mut result = String::with_capacity(input.len());
        let mut rest = input;
        while let Some(start) = rest.find(open) {
            result.push_str(&rest[..start]);
            if let Some(end) = rest[start..].find(close) {
                rest = &rest[start + end + close.len()..];
            } else {
                rest = "";
                break;
            }
        }
        result.push_str(rest);
        result
    }

    let no_think = strip_tag_blocks(text, "<think>", "</think>");
    let no_thought = strip_tag_blocks(&no_think, "<thought>", "</thought>");
    let no_thinking = strip_tag_blocks(&no_thought, "<thinking>", "</thinking>");
    let no_reasoning = strip_tag_blocks(&no_thinking, "<reasoning>", "</reasoning>");
    no_reasoning.trim().to_string()
}

/// レスポンスが「宣言だけしてツールを実行しない」パターンに一致するか判定する。
/// microclaw の runtime guard パターンから移植。
fn is_declarative_only_reply(text: &str) -> bool {
    let lower = text.to_lowercase();
    let english_patterns = [
        "i'll ",
        "i will ",
        "i'll go ahead and ",
        "let me ",
        "sure, ",
        "of course, ",
        "i'd be happy to ",
        "absolutely, ",
        "i can help with that",
        "great, i'll",
        "alright, i'll",
        "okay, i'll",
        "sure thing",
        "i'm on it",
    ];
    let japanese_prefixes = [
        "了解しました",
        "承知しました",
        "わかりました",
        "かしこまりました",
        "はい、",
        "では、",
        "それでは、",
        "今から",
    ];
    let japanese_action_markers = [
        "実行します",
        "確認します",
        "試してみます",
        "やってみます",
        "見てみます",
        "調べます",
        "作成します",
        "書き込みます",
        "進めます",
    ];
    // 短いテキスト（≤200文字）だけを対象とする。長い応答は「宣言のみ」ではない。
    if lower.trim().chars().count() > 200 {
        return false;
    }
    let trimmed = text.trim();
    english_patterns.iter().any(|p| lower.trim().starts_with(p))
        || japanese_prefixes.iter().any(|p| trimmed.starts_with(p))
        || japanese_action_markers.iter().any(|p| trimmed.contains(p))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use async_trait::async_trait;
    use secrecy::SecretString;
    use serial_test::serial;

    use crate::agent_loop::turn::{
        is_declarative_only_reply, message_to_archive_text, message_to_text, strip_thinking,
        truncate_compaction_summary_input,
    };
    use crate::agent_loop::{SurfaceContext, process_turn};
    use crate::config::{Config, ProviderConfig};
    use crate::error::{EgoPulseError, LlmError};
    use crate::llm::{
        LlmProvider, Message, MessageContent, MessageContentPart, MessagesResponse, ToolCall,
        ToolDefinition,
    };
    use crate::runtime::AppState;
    use crate::skills::SkillManager;
    use crate::storage::{Database, call_blocking};
    use crate::tools::ToolRegistry;

    struct FakeProvider {
        responses: std::sync::Mutex<Vec<MessagesResponse>>,
    }

    struct FailingProvider;

    #[derive(Clone)]
    struct RecordingProvider {
        responses: Arc<std::sync::Mutex<Vec<Result<MessagesResponse, LlmError>>>>,
        seen_messages: Arc<std::sync::Mutex<Vec<Vec<Message>>>>,
        seen_systems: Arc<std::sync::Mutex<Vec<String>>>,
        delays_ms: Arc<std::sync::Mutex<Vec<u64>>>,
    }

    #[async_trait]
    impl LlmProvider for FakeProvider {
        async fn send_message(
            &self,
            _system: &str,
            _messages: Vec<Message>,
            _tools: Option<Vec<ToolDefinition>>,
        ) -> Result<MessagesResponse, LlmError> {
            let mut locked = self.responses.lock().expect("responses");
            Ok(locked.remove(0))
        }
    }

    #[async_trait]
    impl LlmProvider for FailingProvider {
        async fn send_message(
            &self,
            _system: &str,
            _messages: Vec<Message>,
            _tools: Option<Vec<ToolDefinition>>,
        ) -> Result<MessagesResponse, LlmError> {
            Err(LlmError::InvalidResponse("boom".to_string()))
        }
    }

    #[async_trait]
    impl LlmProvider for RecordingProvider {
        async fn send_message(
            &self,
            system: &str,
            messages: Vec<Message>,
            _tools: Option<Vec<ToolDefinition>>,
        ) -> Result<MessagesResponse, LlmError> {
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

    impl RecordingProvider {
        fn new(responses: Vec<Result<MessagesResponse, LlmError>>, delays_ms: Vec<u64>) -> Self {
            Self {
                responses: Arc::new(std::sync::Mutex::new(responses)),
                seen_messages: Arc::new(std::sync::Mutex::new(Vec::new())),
                seen_systems: Arc::new(std::sync::Mutex::new(Vec::new())),
                delays_ms: Arc::new(std::sync::Mutex::new(delays_ms)),
            }
        }

        fn seen_messages(&self) -> Vec<Vec<Message>> {
            self.seen_messages.lock().expect("messages").clone()
        }

        fn seen_systems(&self) -> Vec<String> {
            self.seen_systems.lock().expect("systems").clone()
        }
    }

    fn test_config(data_dir: String) -> Config {
        Config {
            default_provider: "openai".to_string(),
            default_model: Some("gpt-4o-mini".to_string()),
            providers: std::collections::HashMap::from([(
                "openai".to_string(),
                ProviderConfig {
                    label: "OpenAI".to_string(),
                    base_url: "https://api.openai.com/v1".to_string(),
                    api_key: Some(SecretString::new("sk-test".to_string().into_boxed_str())),
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

    fn test_config_with_compaction(
        data_dir: String,
        max_session_messages: usize,
        compact_keep_recent: usize,
    ) -> Config {
        let mut config = test_config(data_dir);
        config.max_session_messages = max_session_messages;
        config.compact_keep_recent = compact_keep_recent;
        config
    }

    fn cli_context(session: &str) -> SurfaceContext {
        SurfaceContext {
            channel: "cli".to_string(),
            surface_user: "local_user".to_string(),
            surface_thread: session.to_string(),
            chat_type: "cli".to_string(),
        }
    }

    fn tool_result_message(status: &str, result: &str) -> Message {
        Message {
            role: "tool".to_string(),
            content: MessageContent::text(
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

    fn build_state(config: Config, llm: Box<dyn LlmProvider>) -> AppState {
        use crate::assets::AssetStore;
        use crate::channel_adapter::ChannelRegistry;
        let data_dir = config.data_dir.clone();
        let db = Arc::new(Database::new(&data_dir).expect("db"));
        let skills = Arc::new(SkillManager::from_skills_dir(config.skills_dir().expect("skills_dir")));
        AppState {
            db,
            config: config.clone(),
            config_path: None,
            llm_override: Some(Arc::from(llm)),
            channels: Arc::new(ChannelRegistry::new()),
            skills: Arc::clone(&skills),
            tools: Arc::new(ToolRegistry::new(&config, skills)),
            assets: Arc::new(AssetStore::new(&data_dir).expect("assets")),
        }
    }

    fn build_state_with_provider(data_dir: String, llm: Box<dyn LlmProvider>) -> AppState {
        build_state(test_config(data_dir), llm)
    }

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

        let chat_id = call_blocking(state.db.clone(), move |db| {
            db.resolve_or_create_chat_id("cli", "cli:tool-flow", Some("tool-flow"), "cli")
        })
        .await
        .expect("chat id");
        let tool_calls = call_blocking(state.db.clone(), move |db| {
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
    async fn empty_reply_guard_retries_once_then_errors() {
        // LLM returns empty content twice → first triggers guard retry, second errors
        let dir = tempfile::tempdir().expect("tempdir");
        let state = build_state_with_provider(
            dir.path().to_str().expect("utf8").to_string(),
            Box::new(FakeProvider {
                responses: std::sync::Mutex::new(vec![
                    MessagesResponse {
                        content: String::new(),
                        tool_calls: Vec::new(),
                    },
                    MessagesResponse {
                        content: String::new(),
                        tool_calls: Vec::new(),
                    },
                ]),
            }),
        );

        let error = process_turn(&state, &cli_context("empty-guard"), "hello")
            .await
            .expect_err("should fail after retry");
        assert!(matches!(error, EgoPulseError::Llm(_)));
    }

    #[tokio::test]
    #[serial]
    async fn declarative_only_guard_retries_then_returns() {
        // First: declarative-only reply, Second: actual answer after guard retry
        let dir = tempfile::tempdir().expect("tempdir");
        let state = build_state_with_provider(
            dir.path().to_str().expect("utf8").to_string(),
            Box::new(FakeProvider {
                responses: std::sync::Mutex::new(vec![
                    MessagesResponse {
                        content: "Sure, I'll help you with that.".to_string(),
                        tool_calls: Vec::new(),
                    },
                    MessagesResponse {
                        content: "Here is the answer you need.".to_string(),
                        tool_calls: Vec::new(),
                    },
                ]),
            }),
        );

        let reply = process_turn(&state, &cli_context("declarative-guard"), "help me")
            .await
            .expect("should succeed after retry");
        assert_eq!(reply, "Here is the answer you need.");

        let chat_id = call_blocking(state.db.clone(), move |db| {
            db.resolve_or_create_chat_id(
                "cli",
                "cli:declarative-guard",
                Some("declarative-guard"),
                "cli",
            )
        })
        .await
        .expect("chat id");
        let loaded = crate::agent_loop::session::load_messages_for_turn(&state, chat_id)
            .await
            .expect("loaded session");
        assert!(
            loaded
                .messages
                .iter()
                .all(|message| !message.content.as_text_lossy().contains("[runtime_guard]"))
        );
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

    #[tokio::test]
    #[serial]
    async fn malformed_declarative_tool_reply_retries_then_returns() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = build_state_with_provider(
            dir.path().to_str().expect("utf8").to_string(),
            Box::new(FakeProvider {
                responses: std::sync::Mutex::new(vec![
                    MessagesResponse {
                        content: "了解しました。実行します。".to_string(),
                        tool_calls: vec![ToolCall {
                            id: "call-malformed".to_string(),
                            name: String::new(),
                            arguments: serde_json::json!({}),
                        }],
                    },
                    MessagesResponse {
                        content: "実行結果です。".to_string(),
                        tool_calls: Vec::new(),
                    },
                ]),
            }),
        );

        let reply = process_turn(&state, &cli_context("malformed-declarative"), "test")
            .await
            .expect("should recover after retry");
        assert_eq!(reply, "実行結果です。");
    }

    #[tokio::test]
    #[serial]
    async fn compaction_summarizes_old_messages_and_persists_summary_context() {
        let dir = tempfile::tempdir().expect("tempdir");
        let provider = RecordingProvider::new(
            vec![
                Ok(MessagesResponse {
                    content: "summary text".to_string(),
                    tool_calls: Vec::new(),
                }),
                Ok(MessagesResponse {
                    content: "final answer".to_string(),
                    tool_calls: Vec::new(),
                }),
            ],
            vec![0, 0],
        );
        let config =
            test_config_with_compaction(dir.path().to_str().expect("utf8").to_string(), 4, 2);
        let state = build_state(config, Box::new(provider.clone()));
        let context = cli_context("compaction-success");
        let chat_id = call_blocking(state.db.clone(), move |db| {
            db.resolve_or_create_chat_id(
                "cli",
                "cli:compaction-success",
                Some("compaction-success"),
                "cli",
            )
        })
        .await
        .expect("chat id");
        let seeded = vec![
            Message::text("user", "old-user-1"),
            Message::text("assistant", "old-assistant-1"),
            Message::text("user", "old-user-2"),
            Message::text("assistant", "old-assistant-2"),
        ];
        let seeded_json = serde_json::to_string(&seeded).expect("seeded json");
        call_blocking(state.db.clone(), move |db| {
            db.save_session(chat_id, &seeded_json)
        })
        .await
        .expect("save session");

        let reply = process_turn(&state, &context, "fresh question")
            .await
            .expect("process turn");
        assert_eq!(reply, "final answer");

        let seen_systems = provider.seen_systems();
        assert_eq!(seen_systems.len(), 2);
        assert_eq!(seen_systems[0], "You are a helpful summarizer.");

        let seen_messages = provider.seen_messages();
        assert_eq!(seen_messages.len(), 2);
        assert_eq!(
            seen_messages[1][0].content.as_text_lossy(),
            "[Conversation Summary]\nsummary text"
        );
        assert_eq!(seen_messages[1][1].role, "assistant");
        assert_eq!(
            seen_messages[1][1].content.as_text_lossy(),
            "old-assistant-2"
        );
        assert_eq!(
            seen_messages[1]
                .last()
                .expect("final request")
                .content
                .as_text_lossy(),
            "fresh question"
        );

        let loaded = crate::agent_loop::session::load_messages_for_turn(&state, chat_id)
            .await
            .expect("loaded session");
        assert_eq!(
            loaded.messages[0].content.as_text_lossy(),
            "[Conversation Summary]\nsummary text"
        );
        assert_eq!(
            loaded
                .messages
                .last()
                .expect("session last")
                .content
                .as_text_lossy(),
            "final answer"
        );

        let archive_dir = dir
            .path()
            .join("groups")
            .join("cli")
            .join(chat_id.to_string())
            .join("conversations");
        let archives = std::fs::read_dir(&archive_dir)
            .expect("archive dir")
            .collect::<Result<Vec<_>, _>>()
            .expect("archive entries");
        assert_eq!(archives.len(), 1);
        let archive_body = std::fs::read_to_string(archives[0].path()).expect("archive body");
        assert!(archive_body.contains("old-user-1"));
        assert!(archive_body.contains("fresh question"));
    }

    #[tokio::test]
    #[serial]
    async fn compaction_falls_back_to_recent_messages_when_summary_fails() {
        let dir = tempfile::tempdir().expect("tempdir");
        let provider = RecordingProvider::new(
            vec![
                Err(LlmError::InvalidResponse("summary failed".to_string())),
                Ok(MessagesResponse {
                    content: "final answer".to_string(),
                    tool_calls: Vec::new(),
                }),
            ],
            vec![0, 0],
        );
        let config =
            test_config_with_compaction(dir.path().to_str().expect("utf8").to_string(), 4, 2);
        let state = build_state(config, Box::new(provider.clone()));
        let context = cli_context("compaction-fallback");
        let chat_id = call_blocking(state.db.clone(), move |db| {
            db.resolve_or_create_chat_id(
                "cli",
                "cli:compaction-fallback",
                Some("compaction-fallback"),
                "cli",
            )
        })
        .await
        .expect("chat id");
        let seeded = vec![
            Message::text("user", "old-user-1"),
            Message::text("assistant", "old-assistant-1"),
            Message::text("user", "old-user-2"),
            Message::text("assistant", "old-assistant-2"),
        ];
        let seeded_json = serde_json::to_string(&seeded).expect("seeded json");
        call_blocking(state.db.clone(), move |db| {
            db.save_session(chat_id, &seeded_json)
        })
        .await
        .expect("save session");

        let reply = process_turn(&state, &context, "fresh question")
            .await
            .expect("process turn");
        assert_eq!(reply, "final answer");

        let seen_messages = provider.seen_messages();
        assert_eq!(seen_messages.len(), 2);
        assert!(seen_messages[1].iter().all(|message| {
            !message
                .content
                .as_text_lossy()
                .contains("[Conversation Summary]")
        }));
        assert_eq!(
            seen_messages[1][0].content.as_text_lossy(),
            "old-assistant-2"
        );
        assert_eq!(
            seen_messages[1]
                .last()
                .expect("final request")
                .content
                .as_text_lossy(),
            "fresh question"
        );

        let loaded = crate::agent_loop::session::load_messages_for_turn(&state, chat_id)
            .await
            .expect("loaded session");
        assert!(loaded.messages.iter().all(|message| {
            !message
                .content
                .as_text_lossy()
                .contains("[Conversation Summary]")
        }));
        assert_eq!(
            loaded.messages[0].content.as_text_lossy(),
            "old-assistant-2"
        );
        assert_eq!(
            loaded
                .messages
                .last()
                .expect("session last")
                .content
                .as_text_lossy(),
            "final answer"
        );
    }

    #[test]
    fn message_to_text_preserves_plain_text() {
        let message = Message::text("assistant", "hello world");

        assert_eq!(message_to_text(&message), "hello world");
    }

    #[test]
    fn message_to_text_strips_hidden_reasoning_from_text() {
        let message = Message::text("assistant", "hello <thought>secret</thought> world");

        assert_eq!(message_to_text(&message), "hello  world");
    }

    #[test]
    fn message_to_text_preserves_user_literal_thinking_tags() {
        let message = Message::text("user", "hello <think>literal</think> world");

        assert_eq!(
            message_to_text(&message),
            "hello <think>literal</think> world"
        );
    }

    #[test]
    fn message_to_text_renders_multimodal_images() {
        let message = Message {
            role: "user".to_string(),
            content: MessageContent::parts(vec![
                MessageContentPart::InputText {
                    text: "hello".to_string(),
                },
                MessageContentPart::InputImage {
                    image_url: "data:image/png;base64,abc".to_string(),
                    detail: None,
                },
            ]),
            tool_calls: Vec::new(),
            tool_call_id: None,
        };

        assert_eq!(message_to_text(&message), "hello\n[image]");
    }

    #[test]
    fn message_to_text_strips_hidden_reasoning_from_input_text_and_tool_results() {
        let message = Message {
            role: "tool".to_string(),
            content: MessageContent::parts(vec![MessageContentPart::InputText {
                text: "prefix <think>secret</think> suffix".to_string(),
            }]),
            tool_calls: Vec::new(),
            tool_call_id: Some("call-1".to_string()),
        };

        assert_eq!(message_to_text(&message), "[tool_result]: prefix  suffix");
    }

    #[test]
    fn message_to_text_renders_tool_use() {
        let message = Message {
            role: "assistant".to_string(),
            content: MessageContent::text(""),
            tool_calls: vec![ToolCall {
                id: "call-1".to_string(),
                name: "search".to_string(),
                arguments: serde_json::json!({"query": "egopulse"}),
            }],
            tool_call_id: None,
        };

        assert_eq!(
            message_to_text(&message),
            "[tool_use: search({\"query\":\"egopulse\"})]"
        );
    }

    #[test]
    fn message_to_text_renders_tool_result() {
        let message = tool_result_message("success", "all good");

        assert_eq!(message_to_text(&message), "[tool_result]: all good");
    }

    #[test]
    fn message_to_text_renders_tool_error() {
        let message = tool_result_message("error", "something went wrong");

        assert_eq!(
            message_to_text(&message),
            "[tool_error]: something went wrong"
        );
    }

    #[test]
    fn message_to_text_truncates_tool_result_to_200_chars() {
        let result = "あ".repeat(260);
        let message = tool_result_message("success", &result);
        let rendered = message_to_text(&message);
        let prefix = "[tool_result]: ";
        assert!(rendered.starts_with(prefix));

        let body = &rendered[prefix.len()..];
        assert!(body.ends_with("..."));
        assert_eq!(body.chars().count(), 203);
        assert_eq!(body[..body.len() - 3].chars().count(), 200);
    }

    #[test]
    fn message_to_archive_text_preserves_full_tool_payload() {
        let result = "a".repeat(260);
        let message = tool_result_message("success", &result);

        let rendered = message_to_archive_text(&message);
        assert!(rendered.starts_with("[tool_result]: "));
        assert!(rendered.contains(&result));
        assert!(!rendered.contains("..."));
    }

    #[test]
    fn message_to_archive_text_renders_full_image_and_text_content() {
        let message = Message {
            role: "assistant".to_string(),
            content: MessageContent::parts(vec![
                MessageContentPart::InputText {
                    text: "hello <thinking>internal</thinking> world".to_string(),
                },
                MessageContentPart::InputImage {
                    image_url: "data:image/png;base64,abc".to_string(),
                    detail: Some("high".to_string()),
                },
            ]),
            tool_calls: vec![ToolCall {
                id: "call-1".to_string(),
                name: "search".to_string(),
                arguments: serde_json::json!({"query": "egopulse"}),
            }],
            tool_call_id: None,
        };

        assert_eq!(
            message_to_archive_text(&message),
            "hello  world\n[image: data:image/png;base64,abc detail=high]\n[tool_use: search({\"query\":\"egopulse\"})]"
        );
    }

    #[test]
    fn message_to_archive_text_preserves_user_literal_thinking_tags() {
        let message = Message::text("user", "hello <thought>literal</thought> world");

        assert_eq!(
            message_to_archive_text(&message),
            "hello <thought>literal</thought> world"
        );
    }

    #[test]
    fn message_to_text_falls_back_to_raw_payload_when_result_is_missing() {
        let message = Message {
            role: "tool".to_string(),
            content: MessageContent::text(r#"{"tool":"read","status":"success"}"#),
            tool_calls: Vec::new(),
            tool_call_id: Some("call-1".to_string()),
        };

        assert_eq!(
            message_to_text(&message),
            r#"[tool_result]: {"tool":"read","status":"success"}"#
        );
    }

    #[test]
    fn truncate_compaction_summary_input_keeps_exact_character_limit() {
        let input = format!("{}{}{}", "a".repeat(19_998), "あ", "い");
        let truncated = truncate_compaction_summary_input(input.clone());

        assert_eq!(truncated, input);
    }

    #[test]
    fn truncate_compaction_summary_input_truncates_by_character_count() {
        let input = format!("{}{}{}", "a".repeat(19_999), "あ", "い");
        let truncated = truncate_compaction_summary_input(input);

        let expected = format!("{}\n... (truncated)", "a".repeat(19_999) + "あ");
        assert_eq!(truncated, expected);
    }

    #[test]
    fn strip_thinking_removes_thinking_tags() {
        assert_eq!(strip_thinking("hello world"), "hello world");
        assert_eq!(
            strip_thinking("<thought>internal</thought>visible"),
            "visible"
        );
        assert_eq!(strip_thinking("<thinking>deep</thinking>result"), "result");
        assert_eq!(
            strip_thinking("<reasoning>logic</reasoning>output"),
            "output"
        );
        assert_eq!(
            strip_thinking("<thought>a</thought><thinking>b</thinking>final"),
            "final"
        );
    }

    #[test]
    fn is_declarative_only_reply_detects_patterns() {
        // Short declarative replies
        assert!(is_declarative_only_reply("I'll help you with that."));
        assert!(is_declarative_only_reply("Sure, let me check that."));
        assert!(is_declarative_only_reply("Of course, I can do that."));
        assert!(is_declarative_only_reply("Let me look into that."));
        assert!(is_declarative_only_reply("了解しました。実行します。"));
        assert!(is_declarative_only_reply("承知しました。確認します。"));
        assert!(is_declarative_only_reply("今から試してみます。"));

        // Long responses are NOT declarative-only (regardless of pattern)
        let long = "I'll help you with that. ".repeat(20);
        assert!(!is_declarative_only_reply(&long));

        // Responses that don't start with declarative patterns
        assert!(!is_declarative_only_reply(
            "The file contains the following:"
        ));
        assert!(!is_declarative_only_reply(
            "Here is the result of the search:"
        ));
    }
}
