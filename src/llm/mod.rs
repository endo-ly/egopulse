//! LLM プロバイダークライアント。
//!
//! OpenAI 互換 Chat Completions API および Responses API へのリクエスト構築・送信・
//! レスポンス解析を行う。ツールコールに対応する。

mod api_error;
mod cache;
pub(crate) mod calibration;
pub(crate) mod codex_auth;
mod headers;
mod messages;
mod openai;
mod responses;
mod retry;
mod sse;

use std::sync::Arc;

use async_trait::async_trait;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue};
use secrecy::ExposeSecret;
use serde::{Deserialize, Serialize};

use crate::config::{ProviderHeaderValue, ResolvedLlmConfig};
use crate::error::LlmError;

pub(crate) use cache::{PROVIDER_CACHE_CAP, ProviderCache};
pub(crate) use headers::{HeaderContext, rendered_config};
#[allow(unused_imports)]
pub(crate) use messages::*;
pub(crate) use openai::*;
#[allow(unused_imports)]
pub(crate) use responses::*;
pub(crate) use retry::{
    MAX_ATTEMPTS, RetryDecision, RetryFailure, RetryPolicy, RetryStop, complete_response, decide,
};

/// A single tool call requested by the LLM.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
    /// Parse error when the provider sent arguments that are not valid JSON.
    ///
    /// `arguments` is `{}` in that case. The call is reported back to the
    /// model as a tool error instead of being executed, and this marker is not
    /// persisted.
    #[serde(skip)]
    pub invalid_arguments: Option<String>,
}

impl ToolCall {
    /// Builds a tool call from the provider's raw JSON argument string.
    ///
    /// Empty arguments mean `{}`. Arguments that are not valid JSON do not fail
    /// the whole response; the call keeps `{}` and records the parse error in
    /// [`Self::invalid_arguments`].
    pub(crate) fn from_raw_arguments(id: String, name: String, raw_arguments: &str) -> Self {
        let (arguments, invalid_arguments) = if raw_arguments.trim().is_empty() {
            (serde_json::json!({}), None)
        } else {
            match serde_json::from_str::<serde_json::Value>(raw_arguments) {
                Ok(arguments) => (arguments, None),
                Err(error) => (serde_json::json!({}), Some(error.to_string())),
            }
        };
        Self {
            id,
            name,
            arguments,
            invalid_arguments,
        }
    }
}

/// Tool definition passed to the LLM for function calling.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

/// Chat message in a conversation.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct Message {
    pub role: String,
    pub content: MessageContent,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

/// Message content: either plain text or multimodal parts.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub(crate) enum MessageContent {
    Text(String),
    Parts(Vec<MessageContentPart>),
}

/// A single part of a multimodal message.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type")]
#[allow(clippy::enum_variant_names)]
pub(crate) enum MessageContentPart {
    #[serde(rename = "input_text")]
    InputText { text: String },
    #[serde(rename = "input_image")]
    InputImage {
        image_url: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
}

impl Default for MessageContent {
    fn default() -> Self {
        Self::Text(String::new())
    }
}

impl MessageContent {
    /// Wrap plain text into `MessageContent::Text`.
    pub(crate) fn text(text: impl Into<String>) -> Self {
        Self::Text(text.into())
    }

    /// Wrap multimodal parts into `MessageContent::Parts`.
    pub(crate) fn parts(parts: Vec<MessageContentPart>) -> Self {
        Self::Parts(parts)
    }

    /// Returns `true` if the content includes at least one image part.
    pub(crate) fn is_multimodal(&self) -> bool {
        matches!(self, Self::Parts(parts) if parts.iter().any(|part| matches!(part, MessageContentPart::InputImage { .. })))
    }

    /// Extract all text, discarding images (lossy conversion).
    pub(crate) fn as_text_lossy(&self) -> String {
        match self {
            Self::Text(text) => text.clone(),
            Self::Parts(parts) => parts
                .iter()
                .filter_map(|part| match part {
                    MessageContentPart::InputText { text } => Some(text.clone()),
                    MessageContentPart::InputImage { .. } => None,
                })
                .collect::<Vec<_>>()
                .join("\n"),
        }
    }

    /// Returns `true` if there is no textual content after trimming.
    pub(crate) fn is_empty_textual(&self) -> bool {
        self.as_text_lossy().trim().is_empty()
    }
}

impl Message {
    /// Create a plain-text message with the given role.
    pub(crate) fn text(role: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: role.into(),
            content: MessageContent::text(content),
            reasoning_content: None,
            tool_calls: Vec::new(),
            tool_call_id: None,
        }
    }
}

/// LLM API レスポンスに含まれるトークン使用量。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LlmUsage {
    pub input_tokens: i64,
    pub output_tokens: i64,
    /// `input_tokens` のうち prompt cache から供給された分。Provider が報告しない場合は `None`。
    pub cached_input_tokens: Option<i64>,
}

/// Parsed response from the LLM containing text and/or tool calls.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MessagesResponse {
    pub content: String,
    pub reasoning_content: Option<String>,
    pub tool_calls: Vec<ToolCall>,
    pub usage: Option<LlmUsage>,
}

/// Trait for LLM providers that send a single request and return a complete message response.
#[async_trait]
pub(crate) trait LlmProvider: Send + Sync {
    fn provider_name(&self) -> &str;
    fn model_name(&self) -> &str;

    /// Sends one request and returns the complete response.
    ///
    /// The body is consumed as a stream internally, even when the caller has
    /// nowhere to narrate progress to (sleep, compaction, one-shot ask).
    /// The transport's inactivity limit can only tell a stalled provider from
    /// a long generation while bytes keep arriving, so a non-streaming request
    /// would be cut off by the same silence a healthy generation produces.
    ///
    /// Usage is best-effort: providers that omit it from streamed responses
    /// yield `None`, and callers treat missing usage as zero.
    async fn send_message(
        &self,
        system: &str,
        messages: Arc<Vec<Message>>,
        tools: Option<Arc<Vec<ToolDefinition>>>,
    ) -> Result<MessagesResponse, LlmError> {
        self.send_message_streaming(system, messages, tools, &|_| {})
            .await
    }

    async fn send_message_streaming(
        &self,
        system: &str,
        messages: Arc<Vec<Message>>,
        tools: Option<Arc<Vec<ToolDefinition>>>,
        on_delta: &(dyn Fn(String) + Send + Sync),
    ) -> Result<MessagesResponse, LlmError>;
}

/// Creates the LLM provider for `config` and the given conversation.
///
/// Headers configured without a value are filled in from `context`.
pub(crate) fn create_provider(
    config: &ResolvedLlmConfig,
    context: &HeaderContext,
) -> Result<Box<dyn LlmProvider>, LlmError> {
    create_provider_rendered(&rendered_config(config, context))
}

/// Creates the LLM provider for a configuration whose header values are final.
pub(crate) fn create_provider_rendered(
    config: &ResolvedLlmConfig,
) -> Result<Box<dyn LlmProvider>, LlmError> {
    Ok(Box::new(OpenAiProvider::new(config)?))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use wiremock::matchers::{body_partial_json, body_string_contains, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use crate::config::{ProviderHeaderValue, ResolvedLlmConfig};

    use super::{
        HeaderContext, LlmError, LlmProvider, Message, MessageContent, MessageContentPart,
        MessagesResponse, ToolCall, ToolDefinition, create_provider, extract_raw_tool_use_blocks,
    };

    fn message(content: &str) -> Arc<Vec<Message>> {
        Arc::new(vec![Message::text("user", content)])
    }

    #[test]
    fn tool_call_from_raw_arguments_parses_valid_json() {
        // Arrange
        let raw = r#"{"path": "README.md"}"#;

        // Act
        let call = ToolCall::from_raw_arguments("call_1".to_string(), "read".to_string(), raw);

        // Assert
        assert_eq!(call.arguments, serde_json::json!({"path": "README.md"}));
        assert_eq!(call.invalid_arguments, None);
    }

    #[test]
    fn tool_call_from_raw_arguments_treats_empty_as_empty_object() {
        // Arrange
        let raw = "  ";

        // Act
        let call = ToolCall::from_raw_arguments("call_1".to_string(), "ls".to_string(), raw);

        // Assert
        assert_eq!(call.arguments, serde_json::json!({}));
        assert_eq!(call.invalid_arguments, None);
    }

    #[test]
    fn tool_call_from_raw_arguments_records_parse_error_without_failing() {
        // Arrange
        let raw = r#"{"path": "README.md""#;

        // Act
        let call = ToolCall::from_raw_arguments("call_1".to_string(), "read".to_string(), raw);

        // Assert
        assert_eq!(call.arguments, serde_json::json!({}));
        assert!(call.invalid_arguments.is_some());
    }

    #[test]
    fn tool_call_invalid_arguments_marker_is_not_serialized() {
        // Arrange
        let call = ToolCall::from_raw_arguments("call_1".to_string(), "read".to_string(), "{");

        // Act
        let json = serde_json::to_value(&call).expect("serialize");

        // Assert
        assert!(json.get("invalid_arguments").is_none());
    }

    fn config(model: &str, base_url: String, api_key: Option<&str>) -> ResolvedLlmConfig {
        crate::test_util::llm_config("test", model, base_url, api_key)
    }

    fn chat_sse(content: &str) -> ResponseTemplate {
        let body = format!(
            "data: {{\"choices\":[{{\"delta\":{{\"content\":{}}}}}]}}\n\ndata: [DONE]\n\n",
            serde_json::to_string(content).expect("json string")
        );
        ResponseTemplate::new(200).set_body_raw(body, "text/event-stream")
    }

    fn chat_sse_tool_call(id: &str, name: &str, arguments: &str) -> ResponseTemplate {
        let delta = serde_json::json!({
            "choices": [{
                "delta": {
                    "tool_calls": [{
                        "index": 0,
                        "id": id,
                        "type": "function",
                        "function": {"name": name, "arguments": arguments}
                    }]
                }
            }]
        });
        let body = format!("data: {delta}\n\ndata: [DONE]\n\n");
        ResponseTemplate::new(200).set_body_raw(body, "text/event-stream")
    }

    fn responses_sse(text: &str) -> ResponseTemplate {
        let payload = serde_json::json!({
            "type": "response.completed",
            "response": {
                "status": "completed",
                "output": [{
                    "type": "message",
                    "role": "assistant",
                    "content": [{"type": "output_text", "text": text}]
                }],
                "usage": {"input_tokens": 1, "output_tokens": 1}
            }
        });
        let body = format!("event: response.completed\ndata: {payload}\n\n");
        ResponseTemplate::new(200).set_body_raw(body, "text/event-stream")
    }

    #[tokio::test]
    async fn sends_openai_request() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .and(header("authorization", "Bearer sk-test"))
            .and(body_partial_json(serde_json::json!({
                "model": "gpt-4o-mini",
                "messages": [{"role": "user", "content": "hello"}],
                "reasoning_effort": "high"
            })))
            .respond_with(chat_sse("hello back"))
            .mount(&server)
            .await;

        let mut llm_config = config(
            "gpt-4o-mini",
            format!("{}/v1", server.uri()),
            Some("sk-test"),
        );
        llm_config.reasoning_effort = Some("high".to_string());
        let provider =
            create_provider(&llm_config, &HeaderContext::process_global()).expect("provider");

        let response = provider
            .send_message("", message("hello"), None)
            .await
            .expect("response");

        assert_eq!(response.content, "hello back");
        assert!(response.tool_calls.is_empty());
    }

    #[tokio::test]
    async fn sends_configured_provider_headers() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .and(header("x-opencode-session", "egopulse-musa"))
            .respond_with(chat_sse("hello back"))
            .mount(&server)
            .await;

        let mut llm_config = config(
            "gpt-4o-mini",
            format!("{}/v1", server.uri()),
            Some("sk-test"),
        );
        llm_config.headers.insert(
            "x-opencode-session".to_string(),
            ProviderHeaderValue::Literal("egopulse-musa".to_string()),
        );
        let provider =
            create_provider(&llm_config, &HeaderContext::process_global()).expect("provider");

        let response = provider
            .send_message("", message("hello"), None)
            .await
            .expect("response");

        assert_eq!(response.content, "hello back");
    }

    #[tokio::test]
    async fn valueless_header_is_rendered_per_conversation() {
        // Arrange
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(chat_sse("hello back"))
            .mount(&server)
            .await;

        let mut llm_config = config(
            "gpt-4o-mini",
            format!("{}/v1", server.uri()),
            Some("sk-test"),
        );
        llm_config
            .headers
            .insert("x-session".to_string(), ProviderHeaderValue::Generated);

        // Act
        for agent in ["musa", "lyre"] {
            create_provider(
                &llm_config,
                &HeaderContext::for_agent("installation", agent),
            )
            .expect("provider")
            .send_message("", message("hello"), None)
            .await
            .expect("response");
        }

        // Assert
        let requests = server.received_requests().await.expect("requests");
        assert_eq!(requests.len(), 2);
        let values: Vec<String> = requests
            .iter()
            .map(|request| {
                request
                    .headers
                    .get("x-session")
                    .expect("header")
                    .to_str()
                    .expect("utf8")
                    .to_string()
            })
            .collect();
        assert!(
            values.iter().all(|value| value.len() == 36
                && value.split('-').count() == 5
                && value.chars().all(|c| c.is_ascii_hexdigit() || c == '-')),
            "{values:?}"
        );
        assert_ne!(values[0], values[1]);
    }

    #[test]
    fn preserves_reasoning_content_only_for_deepseek_compatible_targets() {
        assert!(super::should_preserve_reasoning_content(
            "deepseek",
            "https://api.example.com/v1",
            "deepseek-v4-pro"
        ));
        assert!(super::should_preserve_reasoning_content(
            "openai-compatible",
            "https://api.deepseek.com/v1",
            "v4-pro"
        ));
        assert!(super::should_preserve_reasoning_content(
            "opencode-go",
            "https://api.example.com/v1",
            "deepseek-v4-pro"
        ));
        assert!(!super::should_preserve_reasoning_content(
            "openai",
            "https://api.openai.com/v1",
            "gpt-5.5"
        ));
        assert!(!super::should_preserve_reasoning_content(
            "openai-compatible",
            "https://proxy.example.com/deepseek/v1",
            "v4-pro"
        ));
    }

    #[tokio::test]
    async fn sends_request_to_router_style_openai_compatible_endpoint() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/chat/completions"))
            .and(header("authorization", "Bearer sk-openrouter"))
            .respond_with(chat_sse("through router"))
            .mount(&server)
            .await;

        let provider = create_provider(
            &config(
                "openai/gpt-4o-mini",
                format!("{}/api/v1", server.uri()),
                Some("sk-openrouter"),
            ),
            &HeaderContext::process_global(),
        )
        .expect("provider");

        let response = provider
            .send_message("", message("hello"), None)
            .await
            .expect("response");

        assert_eq!(response.content, "through router");
    }

    #[tokio::test]
    async fn sends_request_to_local_openai_compatible_endpoint_without_auth_header() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(chat_sse("local answer"))
            .mount(&server)
            .await;

        let provider = create_provider(
            &config("local-model", format!("{}/v1", server.uri()), None),
            &HeaderContext::process_global(),
        )
        .expect("provider");

        let response = provider
            .send_message("", message("hello"), None)
            .await
            .expect("response");

        assert_eq!(response.content, "local answer");
    }

    #[tokio::test]
    async fn sends_tools_and_parses_tool_calls() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .and(body_partial_json(serde_json::json!({
                "tools": [{
                    "type": "function",
                    "function": {
                        "name": "read"
                    }
                }]
            })))
            .respond_with(chat_sse_tool_call(
                "call_1",
                "read",
                "{\"path\":\"README.md\"}",
            ))
            .mount(&server)
            .await;

        let provider = create_provider(
            &config(
                "gpt-4o-mini",
                format!("{}/v1", server.uri()),
                Some("sk-test"),
            ),
            &HeaderContext::process_global(),
        )
        .expect("provider");

        let response = provider
            .send_message(
                "system prompt",
                message("read it"),
                Some(Arc::new(vec![ToolDefinition {
                    name: "read".to_string(),
                    description: "Read a file".to_string(),
                    parameters: serde_json::json!({"type": "object"}),
                }])),
            )
            .await
            .expect("response");

        assert_eq!(
            response.tool_calls,
            vec![ToolCall {
                id: "call_1".to_string(),
                name: "read".to_string(),
                arguments: serde_json::json!({"path": "README.md"}),
                invalid_arguments: None,
            }]
        );
    }

    #[tokio::test]
    async fn uses_responses_api_for_multimodal_tool_context() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/responses"))
            .and(body_string_contains("\"type\":\"function_call_output\""))
            .and(body_string_contains("\"call_id\":\"call_1\""))
            .and(body_string_contains("\"type\":\"input_image\""))
            .respond_with(responses_sse("I can see the image"))
            .mount(&server)
            .await;

        let provider = create_provider(
            &config(
                "gpt-4o-mini",
                format!("{}/v1", server.uri()),
                Some("sk-test"),
            ),
            &HeaderContext::process_global(),
        )
        .expect("provider");

        let response = provider
            .send_message(
                "",
                Arc::new(vec![
                    Message {
                        role: "assistant".to_string(),
                        content: MessageContent::text(""),
                        reasoning_content: None,
                        tool_calls: vec![ToolCall {
                            id: "call_1".to_string(),
                            name: "read".to_string(),
                            arguments: serde_json::json!({"path": "image.png"}),
                            invalid_arguments: None,
                        }],
                        tool_call_id: None,
                    },
                    Message {
                        role: "tool".to_string(),
                        content: MessageContent::parts(vec![
                            MessageContentPart::InputText {
                                text: "{\"tool\":\"read\",\"status\":\"success\",\"result\":\"Read image file [image/png]\"}".to_string(),
                            },
                            MessageContentPart::InputImage {
                                image_url: "data:image/png;base64,AAAA".to_string(),
                                detail: Some("auto".to_string()),
                            },
                        ]),
                        reasoning_content: None,
                        tool_calls: Vec::new(),
                        tool_call_id: Some("call_1".to_string()),
                    },
                ]),
                Some(Arc::new(vec![ToolDefinition {
                    name: "read".to_string(),
                    description: "Read a file".to_string(),
                    parameters: serde_json::json!({"type": "object"}),
                }])),
            )
            .await
            .expect("response");

        assert_eq!(response.content, "I can see the image");
    }

    #[test]
    fn extracts_raw_tool_use_from_text() {
        let text = "[tool_use: bash({\"command\": \"ls\"})]";
        let blocks = extract_raw_tool_use_blocks(text);
        assert!(blocks.is_some());
        let blocks = blocks.unwrap();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].name, "bash");
        assert_eq!(blocks[0].input_json, "{\"command\": \"ls\"}");
    }

    #[test]
    fn extracts_multiple_raw_tool_use_blocks() {
        let text = "[tool_use: bash({\"command\": \"ls\"})]\n[tool_use: read_file({\"path\": \"test.txt\"})]";
        let blocks = extract_raw_tool_use_blocks(text).unwrap();
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].name, "bash");
        assert_eq!(blocks[1].name, "read_file");
    }

    #[test]
    fn ignores_text_without_raw_tool_use() {
        let text = "This is just regular text";
        assert!(extract_raw_tool_use_blocks(text).is_none());
    }

    #[test]
    fn assemble_response_rescues_raw_tool_use() {
        let response = super::assemble_response(
            "[tool_use: bash({\"command\": \"pwd\"})]".to_string(),
            None,
            Vec::new(),
            None,
        )
        .expect("response");

        assert_eq!(response.tool_calls.len(), 1);
        assert_eq!(response.tool_calls[0].name, "bash");
        assert_eq!(
            response.tool_calls[0].arguments,
            serde_json::json!({"command": "pwd"})
        );
        assert!(response.content.trim().is_empty());
    }

    #[test]
    fn assemble_response_prefers_explicit_tool_calls_over_raw() {
        let response = super::assemble_response(
            "Let me help with that".to_string(),
            None,
            vec![super::ToolCall {
                id: "call_1".to_string(),
                name: "read_file".to_string(),
                arguments: serde_json::json!({"path": "test.txt"}),
                invalid_arguments: None,
            }],
            None,
        )
        .expect("response");

        assert_eq!(response.tool_calls.len(), 1);
        assert_eq!(response.tool_calls[0].name, "read_file");
        assert_eq!(response.content, "Let me help with that");
    }

    #[test]
    fn extracts_raw_tool_use_embedded_in_normal_text() {
        let text = "了解。[tool_use: bash({\"command\": \"ls\"})] を実行します。";
        let blocks = extract_raw_tool_use_blocks(text).unwrap();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].name, "bash");
    }

    #[test]
    fn parse_responses_api_extracts_usage() {
        let body: super::ResponsesApiResponse = serde_json::from_value(serde_json::json!({
            "output": [{
                "type": "message",
                "role": "assistant",
                "content": [{"type": "output_text", "text": "hi"}]
            }],
            "usage": {
                "input_tokens": 15,
                "output_tokens": 25
            }
        }))
        .unwrap();

        let response = super::parse_responses_response(body).unwrap();
        assert_eq!(
            response.usage,
            Some(super::LlmUsage {
                input_tokens: 15,
                output_tokens: 25,
                cached_input_tokens: None,
            })
        );
    }

    #[test]
    fn parse_responses_api_handles_missing_usage() {
        let body: super::ResponsesApiResponse = serde_json::from_value(serde_json::json!({
            "output": [{
                "type": "message",
                "role": "assistant",
                "content": [{"type": "output_text", "text": "hi"}]
            }]
        }))
        .unwrap();

        let response = super::parse_responses_response(body).unwrap();
        assert_eq!(response.usage, None);
    }

    #[test]
    fn parse_responses_api_handles_partial_usage() {
        let body: super::ResponsesApiResponse = serde_json::from_value(serde_json::json!({
            "output": [{
                "type": "message",
                "role": "assistant",
                "content": [{"type": "output_text", "text": "hi"}]
            }],
            "usage": {
                "input_tokens": 15
            }
        }))
        .unwrap();

        let response = super::parse_responses_response(body).unwrap();
        assert_eq!(response.usage, None);
    }

    #[tokio::test]
    async fn llm_provider_metadata_returns_config() {
        let server = wiremock::MockServer::start().await;
        let provider = create_provider(
            &config(
                "gpt-4o-mini",
                format!("{}/v1", server.uri()),
                Some("sk-test"),
            ),
            &HeaderContext::process_global(),
        )
        .expect("provider");

        assert_eq!(provider.provider_name(), "test");
        assert_eq!(provider.model_name(), "gpt-4o-mini");
    }

    #[test]
    fn send_message_accepts_arc_tools() {
        fn assert_arc_tools_param(
            _provider: &dyn LlmProvider,
            _tools: Option<Arc<Vec<ToolDefinition>>>,
        ) {
        }

        struct StubProvider;

        #[async_trait::async_trait]
        impl LlmProvider for StubProvider {
            fn provider_name(&self) -> &str {
                "stub"
            }
            fn model_name(&self) -> &str {
                "stub-model"
            }
            async fn send_message(
                &self,
                _system: &str,
                _messages: Arc<Vec<Message>>,
                tools: Option<Arc<Vec<ToolDefinition>>>,
            ) -> Result<MessagesResponse, LlmError> {
                let _ = tools;
                unreachable!()
            }

            async fn send_message_streaming(
                &self,
                _system: &str,
                _messages: Arc<Vec<Message>>,
                tools: Option<Arc<Vec<ToolDefinition>>>,
                on_delta: &(dyn Fn(String) + Send + Sync),
            ) -> Result<MessagesResponse, LlmError> {
                let _ = (tools, on_delta);
                unreachable!()
            }
        }

        let stub = StubProvider;
        let tools: Option<Arc<Vec<ToolDefinition>>> = Some(Arc::new(vec![ToolDefinition {
            name: "read".to_string(),
            description: "Read a file".to_string(),
            parameters: serde_json::json!({"type": "object"}),
        }]));
        assert_arc_tools_param(&stub, tools);
        assert_arc_tools_param(&stub, None);
    }

    /// Verifies that `send_message` accepts shared `Arc<Vec<Message>>` without
    /// requiring the caller to clone the entire message history.
    #[test]
    fn send_message_accepts_shared_messages() {
        fn assert_shared_messages(_provider: &dyn LlmProvider, _messages: Arc<Vec<Message>>) {}

        struct SharedMessagesProvider;

        #[async_trait::async_trait]
        impl LlmProvider for SharedMessagesProvider {
            fn provider_name(&self) -> &str {
                "shared"
            }
            fn model_name(&self) -> &str {
                "shared-model"
            }
            async fn send_message(
                &self,
                _system: &str,
                _messages: Arc<Vec<Message>>,
                _tools: Option<Arc<Vec<ToolDefinition>>>,
            ) -> Result<MessagesResponse, LlmError> {
                unreachable!()
            }

            async fn send_message_streaming(
                &self,
                _system: &str,
                _messages: Arc<Vec<Message>>,
                _tools: Option<Arc<Vec<ToolDefinition>>>,
                _on_delta: &(dyn Fn(String) + Send + Sync),
            ) -> Result<MessagesResponse, LlmError> {
                unreachable!()
            }
        }

        let provider = SharedMessagesProvider;
        let messages: Arc<Vec<Message>> = Arc::new(vec![
            Message::text("user", "hello"),
            Message::text("assistant", "world"),
        ]);

        let shared = Arc::clone(&messages);
        assert_eq!(Arc::strong_count(&messages), 2);
        assert_shared_messages(&provider, shared);
        assert_eq!(Arc::strong_count(&messages), 1);
    }
}
