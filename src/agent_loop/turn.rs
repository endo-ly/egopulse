//! エージェントの 1 ターン処理を実行するモジュール。
//!
//! セッション復元、LLM 応答、ツール呼び出し、イベント通知、永続化を
//! 1 本の turn loop としてまとめて扱う。

use crate::agent_loop::SurfaceContext;
use crate::agent_loop::session::{load_messages_for_turn, persist_phase, resolve_chat_id};
use crate::error::EgoPulseError;
use crate::llm::{Message, ToolCall};
use crate::runtime::{AppState, build_app_state};
use crate::storage::{StoredMessage, ToolCall as StoredToolCall, call_blocking};
use crate::tools::ToolExecutionContext;
use crate::web::sse::AgentEvent;
use tracing::warn;

const MAX_TOOL_ITERATIONS: usize = 50;
const MAX_TOOL_RESULT_CHARS: usize = 16_000;

/// Sends a one-shot prompt within a named persistent session.
pub async fn ask_in_session(
    config: crate::config::Config,
    session: &str,
    prompt: &str,
) -> Result<String, EgoPulseError> {
    let state = build_app_state(config)?;
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

    let crate::agent_loop::session::LoadedSession {
        mut messages,
        session_updated_at,
    } = load_messages_for_turn(state, chat_id).await?;
    let mut session_updated_at = session_updated_at;

    let user_message = Message::text("user", user_input);
    messages.push(user_message.clone());

    let persisted_user_turn = persist_phase(
        state,
        StoredMessage {
            id: uuid::Uuid::new_v4().to_string(),
            chat_id,
            sender_name: context.surface_user.clone(),
            content: user_input.to_string(),
            is_from_bot: false,
            timestamp: chrono::Utc::now().to_rfc3339(),
        },
        user_message,
        &messages,
        session_updated_at,
    )
    .await?;
    messages = persisted_user_turn.messages;
    session_updated_at = Some(persisted_user_turn.updated_at);

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

        let response = state
            .llm
            .send_message(
                &system_prompt,
                request_messages,
                Some(state.tools.definitions()),
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
        let assistant_preview =
            summarize_tool_calls_with_content(&response.content, &valid_tool_calls);
        let assistant_message = Message {
            role: "assistant".to_string(),
            content: crate::llm::MessageContent::text(response.content.clone()),
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

    use crate::agent_loop::turn::{is_declarative_only_reply, strip_thinking};
    use crate::agent_loop::{SurfaceContext, process_turn};
    use crate::config::Config;
    use crate::error::{EgoPulseError, LlmError};
    use crate::llm::{LlmProvider, Message, MessagesResponse, ToolCall, ToolDefinition};
    use crate::runtime::AppState;
    use crate::skills::SkillManager;
    use crate::storage::{Database, call_blocking};
    use crate::tools::ToolRegistry;

    struct FakeProvider {
        responses: std::sync::Mutex<Vec<MessagesResponse>>,
    }

    struct FailingProvider;

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

    fn test_config(data_dir: String) -> Config {
        Config {
            model: "gpt-4o-mini".to_string(),
            api_key: Some(SecretString::new("sk-test".to_string().into_boxed_str())),
            llm_base_url: "https://api.openai.com/v1".to_string(),
            data_dir,
            log_level: "info".to_string(),
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

    fn cli_context(session: &str) -> SurfaceContext {
        SurfaceContext {
            channel: "cli".to_string(),
            surface_user: "local_user".to_string(),
            surface_thread: session.to_string(),
            chat_type: "cli".to_string(),
        }
    }

    fn build_state_with_provider(data_dir: String, llm: Box<dyn LlmProvider>) -> AppState {
        use crate::assets::AssetStore;
        use crate::channel_adapter::ChannelRegistry;
        let config = test_config(data_dir.clone());
        let db = Arc::new(Database::new(&data_dir).expect("db"));
        let skills = Arc::new(SkillManager::from_skills_dir(config.skills_dir()));
        AppState {
            db,
            config: config.clone(),
            config_path: None,
            llm: Arc::from(llm),
            channels: Arc::new(ChannelRegistry::new()),
            skills: Arc::clone(&skills),
            tools: Arc::new(ToolRegistry::new(&config, skills)),
            assets: Arc::new(AssetStore::new(&data_dir).expect("assets")),
        }
    }

    #[tokio::test]
    async fn process_turn_executes_tool_calls_and_persists_outputs() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = build_state_with_provider(
            dir.path().to_str().expect("utf8").to_string(),
            Box::new(FakeProvider {
                responses: std::sync::Mutex::new(vec![
                    MessagesResponse {
                        content: String::new(),
                        tool_calls: vec![ToolCall {
                            id: "call-1".to_string(),
                            name: "read".to_string(),
                            arguments: serde_json::json!({"path": "notes.txt"}),
                        }],
                    },
                    MessagesResponse {
                        content: "All set".to_string(),
                        tool_calls: Vec::new(),
                    },
                ]),
            }),
        );
        let workspace = state.config.workspace_dir();
        std::fs::create_dir_all(&workspace).expect("workspace");
        std::fs::write(workspace.join("notes.txt"), "hello from tool").expect("notes");

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
        assert!(
            tool_calls[0]
                .tool_output
                .as_deref()
                .expect("tool output")
                .contains("\"status\":\"success\"")
        );
        assert!(
            tool_calls[0]
                .tool_output
                .as_deref()
                .expect("tool output")
                .contains("hello from tool")
        );
    }

    #[tokio::test]
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
    async fn normal_tool_flow_still_works_after_port() {
        // Regression: existing tool flow with multiple tool calls should still work
        let dir = tempfile::tempdir().expect("tempdir");
        let state = build_state_with_provider(
            dir.path().to_str().expect("utf8").to_string(),
            Box::new(FakeProvider {
                responses: std::sync::Mutex::new(vec![
                    MessagesResponse {
                        content: "Let me read that file.".to_string(),
                        tool_calls: vec![ToolCall {
                            id: "call-1".to_string(),
                            name: "read".to_string(),
                            arguments: serde_json::json!({"path": "a.txt"}),
                        }],
                    },
                    MessagesResponse {
                        content: "Done reading. Final answer.".to_string(),
                        tool_calls: Vec::new(),
                    },
                ]),
            }),
        );
        let workspace = state.config.workspace_dir();
        std::fs::create_dir_all(&workspace).expect("workspace");
        std::fs::write(workspace.join("a.txt"), "content").expect("a.txt");

        let reply = process_turn(&state, &cli_context("regression-tool"), "read a.txt")
            .await
            .expect("process turn");
        assert_eq!(reply, "Done reading. Final answer.");
    }

    #[tokio::test]
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
