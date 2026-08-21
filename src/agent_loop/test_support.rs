//! Shared providers and fixtures for agent-loop tests.

use std::sync::Arc;

use crate::conversation::SurfaceContext;
use crate::llm::{LlmProvider, Message};
use crate::runtime::AppState;

pub(crate) struct DeltaEmittingProvider {
    pub(crate) chunks: Vec<String>,
    pub(crate) final_response: String,
}

#[async_trait::async_trait]
impl LlmProvider for DeltaEmittingProvider {
    async fn send_message(
        &self,
        _system: &str,
        _messages: Arc<Vec<Message>>,
        _tools: Option<Arc<Vec<crate::llm::ToolDefinition>>>,
    ) -> Result<crate::llm::MessagesResponse, crate::error::LlmError> {
        Ok(crate::llm::MessagesResponse {
            content: self.final_response.clone(),
            reasoning_content: None,
            tool_calls: Vec::new(),
            usage: None,
        })
    }

    async fn send_message_streaming(
        &self,
        _system: &str,
        _messages: Arc<Vec<Message>>,
        _tools: Option<Arc<Vec<crate::llm::ToolDefinition>>>,
        on_delta: &(dyn Fn(String) + Send + Sync),
    ) -> Result<crate::llm::MessagesResponse, crate::error::LlmError> {
        for chunk in self.chunks.clone() {
            on_delta(chunk);
        }
        Ok(crate::llm::MessagesResponse {
            content: self.final_response.clone(),
            reasoning_content: None,
            tool_calls: Vec::new(),
            usage: None,
        })
    }

    fn provider_name(&self) -> &str {
        "delta-test"
    }

    fn model_name(&self) -> &str {
        "delta-model"
    }
}

pub(crate) struct FakeProvider {
    pub(crate) responses: std::sync::Mutex<Vec<crate::llm::MessagesResponse>>,
}

#[derive(Clone)]
pub(crate) struct RecordingProvider {
    responses:
        Arc<std::sync::Mutex<Vec<Result<crate::llm::MessagesResponse, crate::error::LlmError>>>>,
    seen_messages: Arc<std::sync::Mutex<Vec<Vec<Message>>>>,
    seen_systems: Arc<std::sync::Mutex<Vec<String>>>,
    delays_ms: Arc<std::sync::Mutex<Vec<u64>>>,
}

#[async_trait::async_trait]
impl LlmProvider for FakeProvider {
    async fn send_message(
        &self,
        _system: &str,
        _messages: Arc<Vec<Message>>,
        _tools: Option<Arc<Vec<crate::llm::ToolDefinition>>>,
    ) -> Result<crate::llm::MessagesResponse, crate::error::LlmError> {
        let mut locked = self.responses.lock().expect("responses");
        Ok(locked.remove(0))
    }

    async fn send_message_streaming(
        &self,
        system: &str,
        messages: Arc<Vec<Message>>,
        tools: Option<Arc<Vec<crate::llm::ToolDefinition>>>,
        on_delta: &(dyn Fn(String) + Send + Sync),
    ) -> Result<crate::llm::MessagesResponse, crate::error::LlmError> {
        let _ = on_delta;
        self.send_message(system, messages, tools).await
    }

    fn provider_name(&self) -> &str {
        "test"
    }

    fn model_name(&self) -> &str {
        "test-model"
    }
}

#[async_trait::async_trait]
impl LlmProvider for RecordingProvider {
    async fn send_message(
        &self,
        system: &str,
        messages: Arc<Vec<Message>>,
        _tools: Option<Arc<Vec<crate::llm::ToolDefinition>>>,
    ) -> Result<crate::llm::MessagesResponse, crate::error::LlmError> {
        self.seen_systems
            .lock()
            .expect("systems")
            .push(system.to_string());
        self.seen_messages
            .lock()
            .expect("messages")
            .push((*messages).clone());
        let delay_ms = self.delays_ms.lock().expect("delays").remove(0);
        if delay_ms > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
        }
        self.responses.lock().expect("responses").remove(0)
    }

    async fn send_message_streaming(
        &self,
        system: &str,
        messages: Arc<Vec<Message>>,
        tools: Option<Arc<Vec<crate::llm::ToolDefinition>>>,
        on_delta: &(dyn Fn(String) + Send + Sync),
    ) -> Result<crate::llm::MessagesResponse, crate::error::LlmError> {
        let _ = on_delta;
        self.send_message(system, messages, tools).await
    }

    fn provider_name(&self) -> &str {
        "test"
    }

    fn model_name(&self) -> &str {
        "test-model"
    }
}

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
            responses: Arc::new(std::sync::Mutex::new(responses)),
            seen_messages: Arc::new(std::sync::Mutex::new(Vec::new())),
            seen_systems: Arc::new(std::sync::Mutex::new(Vec::new())),
            delays_ms: Arc::new(std::sync::Mutex::new(delays_ms)),
        }
    }

    pub(crate) fn seen_messages(&self) -> Vec<Vec<Message>> {
        self.seen_messages.lock().expect("messages").clone()
    }

    pub(crate) fn seen_systems(&self) -> Vec<String> {
        self.seen_systems.lock().expect("systems").clone()
    }
}

pub(crate) fn test_config(state_root: String) -> crate::config::Config {
    crate::test_util::test_config(&state_root)
}

pub(crate) fn test_config_with_compaction(
    state_root: String,
    compact_keep_recent: usize,
) -> crate::config::Config {
    let mut config = crate::test_util::test_config(&state_root);
    config.compact_keep_recent = compact_keep_recent;
    config.default_context_window_tokens = 9000;
    config.compaction_threshold_ratio = 0.01;
    config
}

pub(crate) fn cli_context(session: &str) -> SurfaceContext {
    crate::test_util::cli_context(session)
}

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
        reasoning_content: None,
        tool_calls: Vec::new(),
        tool_call_id: Some("call-1".to_string()),
    }
}

pub(crate) fn build_state(
    config: crate::config::Config,
    llm: Box<dyn crate::llm::LlmProvider>,
) -> AppState {
    build_state_for_config_file(config, llm, None)
}

pub(crate) fn build_state_for_config_file(
    config: crate::config::Config,
    llm: Box<dyn crate::llm::LlmProvider>,
    config_path: Option<std::path::PathBuf>,
) -> AppState {
    crate::test_util::build_state_with_config(config, Some(Arc::from(llm)), config_path, None, None)
}

pub(crate) fn build_state_with_provider(
    state_root: String,
    llm: Box<dyn crate::llm::LlmProvider>,
) -> AppState {
    build_state(test_config(state_root), llm)
}

pub(crate) struct DeltaThenFailProvider {
    pub(crate) delta: String,
    pub(crate) calls: Arc<std::sync::atomic::AtomicUsize>,
}

#[async_trait::async_trait]
impl LlmProvider for DeltaThenFailProvider {
    async fn send_message(
        &self,
        _: &str,
        _: Arc<Vec<Message>>,
        _: Option<Arc<Vec<crate::llm::ToolDefinition>>>,
    ) -> Result<crate::llm::MessagesResponse, crate::error::LlmError> {
        unreachable!("agent loop uses the streaming path")
    }

    async fn send_message_streaming(
        &self,
        _: &str,
        _: Arc<Vec<Message>>,
        _: Option<Arc<Vec<crate::llm::ToolDefinition>>>,
        on_delta: &(dyn Fn(String) + Send + Sync),
    ) -> Result<crate::llm::MessagesResponse, crate::error::LlmError> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        on_delta(self.delta.clone());
        Err(crate::error::LlmError::ApiError {
            status: reqwest::StatusCode::INTERNAL_SERVER_ERROR,
            body_preview: "fail after delta".to_string(),
            retry_after_secs: None,
        })
    }

    fn provider_name(&self) -> &str {
        "delta-fail"
    }

    fn model_name(&self) -> &str {
        "delta-fail-model"
    }
}

pub(crate) struct DeltaThenThinkingProvider {
    pub(crate) delta: String,
    pub(crate) calls: Arc<std::sync::atomic::AtomicUsize>,
}

#[async_trait::async_trait]
impl LlmProvider for DeltaThenThinkingProvider {
    async fn send_message(
        &self,
        _: &str,
        _: Arc<Vec<Message>>,
        _: Option<Arc<Vec<crate::llm::ToolDefinition>>>,
    ) -> Result<crate::llm::MessagesResponse, crate::error::LlmError> {
        unreachable!("agent loop uses the streaming path")
    }

    async fn send_message_streaming(
        &self,
        _: &str,
        _: Arc<Vec<Message>>,
        _: Option<Arc<Vec<crate::llm::ToolDefinition>>>,
        on_delta: &(dyn Fn(String) + Send + Sync),
    ) -> Result<crate::llm::MessagesResponse, crate::error::LlmError> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        on_delta(self.delta.clone());
        Ok(crate::llm::MessagesResponse {
            content: "<thinking>internal</thinking>".to_string(),
            reasoning_content: None,
            tool_calls: Vec::new(),
            usage: None,
        })
    }

    fn provider_name(&self) -> &str {
        "delta-thinking"
    }

    fn model_name(&self) -> &str {
        "delta-thinking-model"
    }
}
