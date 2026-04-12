//! LLM プロバイダークライアント。
//!
//! OpenAI 互換 Chat Completions API および Responses API へのリクエスト構築・送信・
//! レスポンス解析を行う。ストリーミング (SSE) とツールコールに対応する。

mod messages;
mod openai;
mod responses;
mod sse;

use async_trait::async_trait;
use futures_util::StreamExt;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue};
use secrecy::ExposeSecret;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::UnboundedSender;

use crate::config::ResolvedLlmConfig;
use crate::error::LlmError;

#[allow(unused_imports)]
pub(crate) use messages::*;
pub(crate) use openai::*;
#[allow(unused_imports)]
pub(crate) use responses::*;
#[allow(unused_imports)]
pub(crate) use sse::*;

/// A single tool call requested by the LLM.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

/// Tool definition passed to the LLM for function calling.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

/// Chat message in a conversation.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Message {
    pub role: String,
    pub content: MessageContent,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

/// Message content: either plain text or multimodal parts.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum MessageContent {
    Text(String),
    Parts(Vec<MessageContentPart>),
}

/// A single part of a multimodal message.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type")]
pub enum MessageContentPart {
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
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text(text.into())
    }

    /// Wrap multimodal parts into `MessageContent::Parts`.
    pub fn parts(parts: Vec<MessageContentPart>) -> Self {
        Self::Parts(parts)
    }

    /// Returns `true` if the content includes at least one image part.
    pub fn is_multimodal(&self) -> bool {
        matches!(self, Self::Parts(parts) if parts.iter().any(|part| matches!(part, MessageContentPart::InputImage { .. })))
    }

    /// Extract all text, discarding images (lossy conversion).
    pub fn as_text_lossy(&self) -> String {
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
    pub fn is_empty_textual(&self) -> bool {
        self.as_text_lossy().trim().is_empty()
    }
}

impl Message {
    /// Create a plain-text message with the given role.
    pub fn text(role: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: role.into(),
            content: MessageContent::text(content),
            tool_calls: Vec::new(),
            tool_call_id: None,
        }
    }
}

/// Parsed response from the LLM containing text and/or tool calls.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MessagesResponse {
    pub content: String,
    pub tool_calls: Vec<ToolCall>,
}

/// Trait for LLM providers supporting non-streaming and streaming message sending.
#[async_trait]
pub trait LlmProvider: Send + Sync {
    async fn send_message(
        &self,
        system: &str,
        messages: Vec<Message>,
        tools: Option<Vec<ToolDefinition>>,
    ) -> Result<MessagesResponse, LlmError>;

    async fn send_message_stream(
        &self,
        system: &str,
        messages: Vec<Message>,
        tools: Option<Vec<ToolDefinition>>,
        text_tx: Option<&UnboundedSender<String>>,
    ) -> Result<MessagesResponse, LlmError> {
        let response = self.send_message(system, messages, tools).await?;
        if let Some(tx) = text_tx
            && !response.content.is_empty()
        {
            let _ = tx.send(response.content.clone());
        }
        Ok(response)
    }
}

/// Create the default LLM provider based on the resolved request-time configuration.
pub fn create_provider(config: &ResolvedLlmConfig) -> Result<Box<dyn LlmProvider>, LlmError> {
    Ok(Box::new(OpenAiProvider::new(config)?))
}

#[cfg(test)]
mod tests {
    use wiremock::matchers::{body_partial_json, body_string_contains, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use crate::config::ResolvedLlmConfig;

    use super::{
        Message, MessageContent, MessageContentPart, ToolCall, ToolDefinition, create_provider,
        extract_raw_tool_use_blocks,
    };

    fn message(content: &str) -> Vec<Message> {
        vec![Message::text("user", content)]
    }

    fn config(model: &str, base_url: String, api_key: Option<&str>) -> ResolvedLlmConfig {
        ResolvedLlmConfig {
            provider: "test".to_string(),
            label: "Test".to_string(),
            base_url,
            api_key: api_key
                .map(|value| secrecy::SecretString::new(value.to_string().into_boxed_str())),
            model: model.to_string(),
        }
    }

    #[tokio::test]
    async fn sends_openai_request() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .and(header("authorization", "Bearer sk-test"))
            .and(body_partial_json(serde_json::json!({
                "model": "gpt-4o-mini",
                "messages": [{"role": "user", "content": "hello"}]
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "choices": [{
                    "message": {
                        "content": "hello back"
                    }
                }]
            })))
            .mount(&server)
            .await;

        let provider = create_provider(&config(
            "gpt-4o-mini",
            format!("{}/v1", server.uri()),
            Some("sk-test"),
        ))
        .expect("provider");

        let response = provider
            .send_message("", message("hello"), None)
            .await
            .expect("response");

        assert_eq!(response.content, "hello back");
        assert!(response.tool_calls.is_empty());
    }

    #[tokio::test]
    async fn sends_request_to_router_style_openai_compatible_endpoint() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/chat/completions"))
            .and(header("authorization", "Bearer sk-openrouter"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "choices": [{
                    "message": {
                        "content": [{"type": "text", "text": "through router"}]
                    }
                }]
            })))
            .mount(&server)
            .await;

        let provider = create_provider(&config(
            "openai/gpt-4o-mini",
            format!("{}/api/v1", server.uri()),
            Some("sk-openrouter"),
        ))
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
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "choices": [{
                    "message": {
                        "content": "local answer"
                    }
                }]
            })))
            .mount(&server)
            .await;

        let provider =
            create_provider(&config("local-model", format!("{}/v1", server.uri()), None))
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
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "choices": [{
                    "message": {
                        "content": "",
                        "tool_calls": [{
                            "id": "call_1",
                            "function": {
                                "name": "read",
                                "arguments": "{\"path\":\"README.md\"}"
                            }
                        }]
                    }
                }]
            })))
            .mount(&server)
            .await;

        let provider = create_provider(&config(
            "gpt-4o-mini",
            format!("{}/v1", server.uri()),
            Some("sk-test"),
        ))
        .expect("provider");

        let response = provider
            .send_message(
                "system prompt",
                message("read it"),
                Some(vec![ToolDefinition {
                    name: "read".to_string(),
                    description: "Read a file".to_string(),
                    parameters: serde_json::json!({"type": "object"}),
                }]),
            )
            .await
            .expect("response");

        assert_eq!(
            response.tool_calls,
            vec![ToolCall {
                id: "call_1".to_string(),
                name: "read".to_string(),
                arguments: serde_json::json!({"path": "README.md"}),
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
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "output": [{
                    "type": "message",
                    "role": "assistant",
                    "content": [{"type": "output_text", "text": "I can see the image"}]
                }]
            })))
            .mount(&server)
            .await;

        let provider = create_provider(&config(
            "gpt-4o-mini",
            format!("{}/v1", server.uri()),
            Some("sk-test"),
        ))
        .expect("provider");

        let response = provider
            .send_message(
                "",
                vec![
                    Message {
                        role: "assistant".to_string(),
                        content: MessageContent::text(""),
                        tool_calls: vec![ToolCall {
                            id: "call_1".to_string(),
                            name: "read".to_string(),
                            arguments: serde_json::json!({"path": "image.png"}),
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
                        tool_calls: Vec::new(),
                        tool_call_id: Some("call_1".to_string()),
                    },
                ],
                Some(vec![ToolDefinition {
                    name: "read".to_string(),
                    description: "Read a file".to_string(),
                    parameters: serde_json::json!({"type": "object"}),
                }]),
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
    fn raw_tool_use_rescue_in_parse_openai_response() {
        let body: super::OpenAiResponse = serde_json::from_value(serde_json::json!({
            "choices": [{
                "message": {
                    "content": "[tool_use: bash({\"command\": \"pwd\"})]",
                    "tool_calls": null
                }
            }]
        }))
        .unwrap();

        let response = super::parse_openai_response(body).unwrap();
        assert_eq!(response.tool_calls.len(), 1);
        assert_eq!(response.tool_calls[0].name, "bash");
        assert_eq!(
            response.tool_calls[0].arguments,
            serde_json::json!({"command": "pwd"})
        );
        assert!(response.content.is_empty() || response.content.trim().is_empty());
    }

    #[test]
    fn explicit_tool_calls_take_priority_over_raw() {
        let body: super::OpenAiResponse = serde_json::from_value(serde_json::json!({
            "choices": [{
                "message": {
                    "content": "Let me help with that",
                    "tool_calls": [{
                        "id": "call_1",
                        "function": {
                            "name": "read_file",
                            "arguments": "{\"path\": \"test.txt\"}"
                        }
                    }]
                }
            }]
        }))
        .unwrap();

        let response = super::parse_openai_response(body).unwrap();
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
}
