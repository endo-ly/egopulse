use crate::agent_loop::SurfaceContext;
use crate::agent_loop::session::{load_messages_for_turn, persist_phase, resolve_chat_id};
use crate::error::EgoPulseError;
use crate::llm::{Message, MessagesResponse, ToolCall};
use crate::runtime::{AppState, build_app_state};
use crate::storage::{StoredMessage, ToolCall as StoredToolCall, call_blocking};
use crate::tools::ToolExecutionContext;
use crate::web::sse::AgentEvent;

const MAX_TOOL_ITERATIONS: usize = 16;
const MAX_TOOL_RESULT_CHARS: usize = 16_000;

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

pub async fn process_turn(
    state: &AppState,
    context: &SurfaceContext,
    user_input: &str,
) -> Result<String, EgoPulseError> {
    process_turn_inner(state, context, user_input, Option::<fn(AgentEvent)>::None).await
}

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

    for iteration in 1..=MAX_TOOL_ITERATIONS {
        emit_event(&on_event, AgentEvent::Iteration { iteration });

        let response = state
            .llm
            .send_message(
                &system_prompt,
                messages.clone(),
                Some(state.tools.definitions()),
            )
            .await?;

        if response.tool_calls.is_empty() {
            let final_content = response.content.trim().to_string();
            if final_content.is_empty() {
                return Err(EgoPulseError::Llm(crate::error::LlmError::InvalidResponse(
                    "assistant content was empty".to_string(),
                )));
            }

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
        let assistant_preview = summarize_tool_calls(&response);
        let assistant_message = Message {
            role: "assistant".to_string(),
            content: crate::llm::MessageContent::text(response.content.clone()),
            tool_calls: response.tool_calls.clone(),
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

        for tool_call in response.tool_calls {
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
    if let Some(on_event) = on_event {
        on_event(event);
    }
}

fn build_system_prompt(state: &AppState, context: &SurfaceContext) -> String {
    let mut prompt = format!(
        "You are EgoPulse, a local-first assistant running on the '{}' channel.\n\
         Call tools directly when you need external data or file access. Do not describe fake tool calls as plain text.\n\
         Prefer relative paths under the runtime workspace when using read, write, edit, find, ls, and grep.\n\
         Use activate_skill when a listed skill matches the task and you need its full instructions.\n\
         The current session is '{}' (type: {}).",
        context.channel, context.surface_thread, context.chat_type
    );

    let skills_catalog = state.skills.build_skills_catalog();
    if !skills_catalog.is_empty() {
        prompt.push_str("\n\n# Agent Skills\n\n");
        prompt.push_str(
            "The following skills are available. Load the full instructions with activate_skill before relying on one.\n\n",
        );
        prompt.push_str(&skills_catalog);
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
    if content.chars().count() > MAX_TOOL_RESULT_CHARS {
        content = format!(
            "{}...",
            content
                .chars()
                .take(MAX_TOOL_RESULT_CHARS)
                .collect::<String>()
        );
    }

    let mut payload = serde_json::json!({
        "tool": tool_call.name,
        "status": if result.is_error { "error" } else { "success" },
        "result": content,
    });
    if let Some(details) = &result.details {
        payload["details"] = details.clone();
    }
    payload.to_string()
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

fn summarize_tool_calls(response: &MessagesResponse) -> String {
    let names = response
        .tool_calls
        .iter()
        .map(|tool_call| tool_call.name.as_str())
        .collect::<Vec<_>>();
    if response.content.trim().is_empty() {
        format!("[tool_call] {}", names.join(", "))
    } else {
        format!(
            "{} [tool_call] {}",
            response.content.trim(),
            names.join(", ")
        )
    }
}

fn preview_text(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }
    format!("{}...", value.chars().take(max_chars).collect::<String>())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use async_trait::async_trait;
    use secrecy::SecretString;

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
}
