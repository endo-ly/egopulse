//! LLM プロバイダークライアント。
//!
//! OpenAI 互換 Chat Completions API および Responses API へのリクエスト構築・送信・
//! レスポンス解析を行う。ストリーミング (SSE) とツールコールに対応する。

use async_trait::async_trait;
use futures_util::StreamExt;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::UnboundedSender;

use crate::config::ResolvedLlmConfig;
use crate::error::LlmError;

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

/// OpenAI-compatible LLM provider using Chat Completions and Responses APIs.
pub struct OpenAiProvider {
    http: reqwest::Client,
    api_key: Option<String>,
    model: String,
    base_url: String,
}

impl OpenAiProvider {
    /// Build a new provider from the given configuration.
    pub fn new(config: &ResolvedLlmConfig) -> Result<Self, LlmError> {
        let http = reqwest::Client::builder()
            .user_agent(format!("egopulse/{}", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|error| LlmError::InitFailed(error.to_string()))?;

        Ok(Self {
            http,
            api_key: config.api_key.clone(),
            model: config.model.clone(),
            base_url: config.base_url.clone(),
        })
    }

    async fn send_message_via_responses(
        &self,
        system: &str,
        messages: Vec<Message>,
        tools: Option<Vec<ToolDefinition>>,
    ) -> Result<MessagesResponse, LlmError> {
        let url = format!("{}/responses", self.base_url.trim_end_matches('/'));
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        if let Some(api_key) = &self.api_key {
            let auth_value = HeaderValue::from_str(&format!("Bearer {api_key}"))
                .map_err(|error| LlmError::RequestConstructionFailed(error.to_string()))?;
            headers.insert(AUTHORIZATION, auth_value);
        }

        let response = self
            .http
            .post(url)
            .headers(headers)
            .json(&build_responses_request_body(
                &self.model,
                system,
                &messages,
                tools.as_deref(),
            ))
            .send()
            .await?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(LlmError::ApiError {
                status,
                body_preview: preview_body(&body),
            });
        }

        let body: ResponsesApiResponse = response.json().await?;
        parse_responses_response(body)
    }
}

#[async_trait]
impl LlmProvider for OpenAiProvider {
    async fn send_message(
        &self,
        system: &str,
        messages: Vec<Message>,
        tools: Option<Vec<ToolDefinition>>,
    ) -> Result<MessagesResponse, LlmError> {
        if should_use_responses_api(&messages) {
            return self
                .send_message_via_responses(system, messages, tools)
                .await;
        }

        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        if let Some(api_key) = &self.api_key {
            let auth_value = HeaderValue::from_str(&format!("Bearer {api_key}"))
                .map_err(|error| LlmError::RequestConstructionFailed(error.to_string()))?;
            headers.insert(AUTHORIZATION, auth_value);
        }

        let response = self
            .http
            .post(url)
            .headers(headers)
            .json(&build_request_body(
                &self.model,
                system,
                &messages,
                tools.as_deref(),
                None,
            ))
            .send()
            .await?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(LlmError::ApiError {
                status,
                body_preview: preview_body(&body),
            });
        }

        let body: OpenAiResponse = response.json().await?;
        parse_openai_response(body)
    }

    async fn send_message_stream(
        &self,
        system: &str,
        messages: Vec<Message>,
        tools: Option<Vec<ToolDefinition>>,
        text_tx: Option<&UnboundedSender<String>>,
    ) -> Result<MessagesResponse, LlmError> {
        if tools.as_ref().is_some_and(|tools| !tools.is_empty()) {
            let response = self.send_message(system, messages, tools).await?;
            if let Some(tx) = text_tx
                && !response.content.is_empty()
            {
                let _ = tx.send(response.content.clone());
            }
            return Ok(response);
        }

        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        if let Some(api_key) = &self.api_key {
            let auth_value = HeaderValue::from_str(&format!("Bearer {api_key}"))
                .map_err(|error| LlmError::RequestConstructionFailed(error.to_string()))?;
            headers.insert(AUTHORIZATION, auth_value);
        }

        let response = self
            .http
            .post(url)
            .headers(headers)
            .json(&build_request_body(
                &self.model,
                system,
                &messages,
                None,
                Some(true),
            ))
            .send()
            .await?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(LlmError::ApiError {
                status,
                body_preview: preview_body(&body),
            });
        }

        let mut byte_stream = response.bytes_stream();
        let mut sse = SseEventParser::default();
        let mut text = String::new();
        let mut done = false;

        'outer: while let Some(chunk_res) = byte_stream.next().await {
            let chunk = match chunk_res {
                Ok(c) => c,
                Err(error) => return Err(LlmError::RequestFailed(error)),
            };
            for data in sse.push_chunk(chunk.as_ref()) {
                if data == "[DONE]" {
                    done = true;
                    break 'outer;
                }
                if let Some(piece) = process_openai_stream_event(&data)
                    && !piece.is_empty()
                {
                    text.push_str(&piece);
                    if let Some(tx) = text_tx {
                        let _ = tx.send(piece);
                    }
                }
            }
        }

        if !done {
            for data in sse.finish() {
                if data == "[DONE]" {
                    done = true;
                    break;
                }
                if let Some(piece) = process_openai_stream_event(&data)
                    && !piece.is_empty()
                {
                    text.push_str(&piece);
                    if let Some(tx) = text_tx {
                        let _ = tx.send(piece);
                    }
                }
            }
        }

        if !done {
            return Err(LlmError::InvalidResponse(
                "stream ended before [DONE]".to_string(),
            ));
        }

        let text = text.trim().to_string();
        if text.is_empty() {
            return Err(LlmError::InvalidResponse(
                "assistant content was empty".to_string(),
            ));
        }

        Ok(MessagesResponse {
            content: text,
            tool_calls: Vec::new(),
        })
    }
}

fn build_request_body(
    model: &str,
    system: &str,
    messages: &[Message],
    tools: Option<&[ToolDefinition]>,
    stream: Option<bool>,
) -> serde_json::Value {
    let mut translated = Vec::new();
    if !system.trim().is_empty() {
        translated.push(serde_json::json!({
            "role": "system",
            "content": system,
        }));
    }
    translated.extend(messages.iter().map(translate_message_to_openai));

    let mut body = serde_json::json!({
        "model": model,
        "messages": translated,
    });
    if let Some(stream) = stream {
        body["stream"] = serde_json::Value::Bool(stream);
    }
    if let Some(tools) = tools
        && !tools.is_empty()
    {
        body["tools"] = serde_json::Value::Array(
            tools
                .iter()
                .map(|tool| {
                    serde_json::json!({
                        "type": "function",
                        "function": {
                            "name": tool.name,
                            "description": tool.description,
                            "parameters": tool.parameters,
                        }
                    })
                })
                .collect(),
        );
    }
    body
}

fn build_responses_request_body(
    model: &str,
    system: &str,
    messages: &[Message],
    tools: Option<&[ToolDefinition]>,
) -> serde_json::Value {
    let mut body = serde_json::json!({
        "model": model,
        "input": messages
            .iter()
            .flat_map(translate_message_to_responses)
            .collect::<Vec<_>>(),
    });
    if !system.trim().is_empty() {
        body["instructions"] = serde_json::Value::String(system.to_string());
    }
    if let Some(tools) = tools
        && !tools.is_empty()
    {
        body["tools"] = serde_json::Value::Array(
            tools
                .iter()
                .map(|tool| {
                    serde_json::json!({
                        "type": "function",
                        "name": tool.name,
                        "description": tool.description,
                        "parameters": tool.parameters,
                    })
                })
                .collect(),
        );
    }
    body
}

fn translate_message_to_openai(message: &Message) -> serde_json::Value {
    match message.role.as_str() {
        "assistant" if !message.tool_calls.is_empty() => serde_json::json!({
            "role": "assistant",
            "content": if message.content.is_empty_textual() {
                serde_json::Value::Null
            } else {
                translate_content_to_chat_completions(&message.content)
            },
            "tool_calls": message.tool_calls.iter().map(|tool_call| {
                serde_json::json!({
                    "id": tool_call.id,
                    "type": "function",
                    "function": {
                        "name": tool_call.name,
                        "arguments": tool_call.arguments.to_string(),
                    }
                })
            }).collect::<Vec<_>>(),
        }),
        "tool" => serde_json::json!({
            "role": "tool",
            "content": message.content.as_text_lossy(),
            "tool_call_id": message.tool_call_id,
        }),
        _ => serde_json::json!({
            "role": message.role,
            "content": translate_content_to_chat_completions(&message.content),
        }),
    }
}

fn translate_content_to_chat_completions(content: &MessageContent) -> serde_json::Value {
    match content {
        MessageContent::Text(text) => serde_json::Value::String(text.clone()),
        MessageContent::Parts(parts) => serde_json::Value::Array(
            parts
                .iter()
                .map(|part| match part {
                    MessageContentPart::InputText { text } => serde_json::json!({
                        "type": "text",
                        "text": text,
                    }),
                    MessageContentPart::InputImage { image_url, detail } => serde_json::json!({
                        "type": "image_url",
                        "image_url": {
                            "url": image_url,
                            "detail": detail.clone().unwrap_or_else(|| "auto".to_string()),
                        }
                    }),
                })
                .collect(),
        ),
    }
}

fn translate_message_to_responses(message: &Message) -> Vec<serde_json::Value> {
    match message.role.as_str() {
        "assistant" if !message.tool_calls.is_empty() => {
            let mut items = Vec::new();
            if !message.content.is_empty_textual() {
                items.push(serde_json::json!({
                    "type": "message",
                    "role": "assistant",
                    "content": translate_content_to_responses_message(&message.content),
                }));
            }
            items.extend(message.tool_calls.iter().map(|tool_call| {
                serde_json::json!({
                    "type": "function_call",
                    "call_id": tool_call.id,
                    "name": tool_call.name,
                    "arguments": tool_call.arguments.to_string(),
                })
            }));
            items
        }
        "tool" => {
            let mut items = vec![serde_json::json!({
                "type": "function_call_output",
                "call_id": message.tool_call_id.clone().unwrap_or_default(),
                "output": message.content.as_text_lossy(),
            })];
            if message.content.is_multimodal() {
                items.push(serde_json::json!({
                    "type": "message",
                    "role": "user",
                    "content": synthetic_tool_attachment_parts(&message.content),
                }));
            }
            items
        }
        _ => vec![serde_json::json!({
            "type": "message",
            "role": message.role,
            "content": translate_content_to_responses_message(&message.content),
        })],
    }
}

fn translate_content_to_responses_message(content: &MessageContent) -> Vec<serde_json::Value> {
    match content {
        MessageContent::Text(text) => vec![serde_json::json!({
            "type": "input_text",
            "text": text,
        })],
        MessageContent::Parts(parts) => parts
            .iter()
            .map(|part| match part {
                MessageContentPart::InputText { text } => serde_json::json!({
                    "type": "input_text",
                    "text": text,
                }),
                MessageContentPart::InputImage { image_url, detail } => serde_json::json!({
                    "type": "input_image",
                    "image_url": image_url,
                    "detail": detail.clone().unwrap_or_else(|| "auto".to_string()),
                }),
            })
            .collect(),
    }
}

fn synthetic_tool_attachment_parts(content: &MessageContent) -> Vec<serde_json::Value> {
    let mut parts = vec![serde_json::json!({
        "type": "input_text",
        "text": "System-generated attachment for the immediately preceding tool result. Treat it as tool output context, not as a new user request.",
    })];
    if let MessageContent::Parts(items) = content {
        parts.extend(items.iter().filter_map(|part| match part {
            MessageContentPart::InputText { .. } => None,
            MessageContentPart::InputImage { image_url, detail } => Some(serde_json::json!({
                "type": "input_image",
                "image_url": image_url,
                "detail": detail.clone().unwrap_or_else(|| "auto".to_string()),
            })),
        }));
    }
    parts
}

fn parse_openai_response(body: OpenAiResponse) -> Result<MessagesResponse, LlmError> {
    let choice = body
        .choices
        .into_iter()
        .next()
        .ok_or_else(|| LlmError::InvalidResponse("choices[0] missing".to_string()))?;
    let mut tool_calls = choice
        .message
        .tool_calls
        .unwrap_or_default()
        .into_iter()
        .map(parse_tool_call)
        .collect::<Result<Vec<_>, _>>()?;

    let mut content = extract_text(
        choice
            .message
            .content
            .unwrap_or(OpenAiMessageContent::Text(String::new())),
    );

    if tool_calls.is_empty() {
        if let Some(raw_calls) = extract_raw_tool_use_blocks(&content) {
            for raw in raw_calls {
                let arguments = if raw.input_json.trim().is_empty() {
                    serde_json::json!({})
                } else {
                    serde_json::from_str(&raw.input_json).unwrap_or_else(|_| serde_json::json!({}))
                };
                tool_calls.push(ToolCall {
                    id: raw.id,
                    name: raw.name,
                    arguments,
                });
            }
            content = strip_raw_tool_use_text(&content);
        }
    }

    if content.is_empty() && tool_calls.is_empty() {
        return Err(LlmError::InvalidResponse(
            "assistant content was empty".to_string(),
        ));
    }

    Ok(MessagesResponse {
        content,
        tool_calls,
    })
}

fn parse_responses_response(body: ResponsesApiResponse) -> Result<MessagesResponse, LlmError> {
    let mut content_parts = Vec::new();
    let mut tool_calls = Vec::new();

    for item in body.output {
        match item {
            ResponsesOutputItem::Message { role, content } if role == "assistant" => {
                for part in content {
                    match part {
                        ResponsesOutputPart::OutputText { text } => content_parts.push(text),
                        ResponsesOutputPart::Refusal { refusal } => content_parts.push(refusal),
                        ResponsesOutputPart::Ignored => {}
                    }
                }
            }
            ResponsesOutputItem::FunctionCall {
                call_id,
                name,
                arguments,
            } => {
                let arguments = if arguments.trim().is_empty() {
                    serde_json::json!({})
                } else {
                    serde_json::from_str::<serde_json::Value>(&arguments).map_err(|error| {
                        LlmError::InvalidResponse(format!(
                            "invalid tool arguments for '{}': {error}",
                            name
                        ))
                    })?
                };
                tool_calls.push(ToolCall {
                    id: call_id,
                    name,
                    arguments,
                });
            }
            ResponsesOutputItem::Ignored | ResponsesOutputItem::Message { .. } => {}
        }
    }

    let mut content = content_parts.join("\n").trim().to_string();

    if tool_calls.is_empty() {
        if let Some(raw_calls) = extract_raw_tool_use_blocks(&content) {
            for raw in raw_calls {
                let arguments = if raw.input_json.trim().is_empty() {
                    serde_json::json!({})
                } else {
                    serde_json::from_str(&raw.input_json).unwrap_or_else(|_| serde_json::json!({}))
                };
                tool_calls.push(ToolCall {
                    id: raw.id,
                    name: raw.name,
                    arguments,
                });
            }
            content = strip_raw_tool_use_text(&content);
        }
    }

    if content.is_empty() && tool_calls.is_empty() {
        return Err(LlmError::InvalidResponse(
            "assistant content was empty".to_string(),
        ));
    }

    Ok(MessagesResponse {
        content,
        tool_calls,
    })
}

fn parse_tool_call(raw: OaiToolCall) -> Result<ToolCall, LlmError> {
    let arguments = if raw.function.arguments.trim().is_empty() {
        serde_json::json!({})
    } else {
        serde_json::from_str::<serde_json::Value>(&raw.function.arguments).map_err(|error| {
            LlmError::InvalidResponse(format!(
                "invalid tool arguments for '{}': {error}",
                raw.function.name
            ))
        })?
    };

    Ok(ToolCall {
        id: raw.id,
        name: raw.function.name,
        arguments,
    })
}

fn should_use_responses_api(messages: &[Message]) -> bool {
    messages
        .iter()
        .any(|message| message.content.is_multimodal())
}

#[derive(Default)]
struct SseEventParser {
    pending: Vec<u8>,
    data_lines: Vec<String>,
}

impl SseEventParser {
    fn push_chunk(&mut self, chunk: impl AsRef<[u8]>) -> Vec<String> {
        self.pending.extend_from_slice(chunk.as_ref());
        let mut events = Vec::new();
        while let Some(pos) = self.pending.iter().position(|byte| *byte == b'\n') {
            let mut line: Vec<u8> = self.pending.drain(..=pos).collect();
            if line.last() == Some(&b'\n') {
                line.pop();
            }
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            let line = Self::decode_line(line);
            if let Some(event_data) = self.handle_line(&line) {
                events.push(event_data);
            }
        }
        events
    }

    fn finish(&mut self) -> Vec<String> {
        let mut events = Vec::new();
        if !self.pending.is_empty() {
            let mut line = std::mem::take(&mut self.pending);
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            let line = Self::decode_line(line);
            if let Some(event_data) = self.handle_line(&line) {
                events.push(event_data);
            }
        }
        if let Some(event_data) = self.flush_event() {
            events.push(event_data);
        }
        events
    }

    fn decode_line(line: Vec<u8>) -> String {
        match String::from_utf8(line) {
            Ok(line) => line,
            Err(error) => String::from_utf8_lossy(&error.into_bytes()).into_owned(),
        }
    }

    fn handle_line(&mut self, line: &str) -> Option<String> {
        if line.is_empty() {
            return self.flush_event();
        }
        if line.starts_with(':') {
            return None;
        }
        let (field, value) = match line.split_once(':') {
            Some((field, value)) => (field, value.strip_prefix(' ').unwrap_or(value)),
            None => (line, ""),
        };
        if field == "data" {
            self.data_lines.push(value.to_string());
        }
        None
    }

    fn flush_event(&mut self) -> Option<String> {
        if self.data_lines.is_empty() {
            return None;
        }
        let data = self.data_lines.join("\n");
        self.data_lines.clear();
        Some(data)
    }
}

fn process_openai_stream_event(data: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(data).ok()?;
    let choice = value.get("choices")?.as_array()?.first()?;
    let delta = choice.get("delta")?;
    let content = delta.get("content")?;
    match content {
        serde_json::Value::String(text) if !text.is_empty() => Some(text.clone()),
        serde_json::Value::Array(parts) => {
            let text = parts
                .iter()
                .filter_map(|part| part.get("text")?.as_str())
                .collect::<String>();
            if text.is_empty() { None } else { Some(text) }
        }
        _ => None,
    }
}

fn extract_text(content: OpenAiMessageContent) -> String {
    match content {
        OpenAiMessageContent::Text(text) => text.trim().to_string(),
        OpenAiMessageContent::Parts(parts) => parts
            .into_iter()
            .filter_map(|part| match part {
                OpenAiContentPart::Text { text } => Some(text),
                OpenAiContentPart::Refusal { refusal } => Some(refusal),
                OpenAiContentPart::Ignored => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
            .trim()
            .to_string(),
    }
}

fn preview_body(body: &str) -> String {
    let normalized = body.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut preview = normalized.chars().take(200).collect::<String>();
    if normalized.chars().count() > 200 {
        preview.push_str("...");
    }
    if preview.is_empty() {
        "<empty>".to_string()
    } else {
        preview
    }
}

// Raw tool-use rescue: some LLM providers emit tool calls as raw text
// `[tool_use: name(args)]` instead of structured `tool_calls` fields.

struct RawToolUseBlock {
    id: String,
    name: String,
    input_json: String,
}

fn strip_minimax_tool_wrappers(text: &str) -> String {
    use std::sync::LazyLock;
    static RE: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r"</?(?:minimax:tool_call|invoke|parameter)>")
            .expect("MiniMax tool wrapper regex must compile")
    });
    RE.replace_all(text, " ").into_owned()
}

fn parse_raw_tool_use_block(input: &str, call_number: usize) -> Option<(RawToolUseBlock, &str)> {
    let rest = input.trim_start();
    let prefix = "[tool_use:";
    if !rest.starts_with(prefix) {
        return None;
    }

    let mut cursor = prefix.len();
    let after_prefix = &rest[cursor..];
    let name_and_args = after_prefix.trim_start();
    cursor += after_prefix.len().saturating_sub(name_and_args.len());

    let open_paren_rel = name_and_args.find('(')?;
    let name = name_and_args[..open_paren_rel].trim();
    if name.is_empty() {
        return None;
    }
    cursor += open_paren_rel + 1;

    let mut depth = 1usize;
    let mut in_string = false;
    let mut escaping = false;
    let mut close_paren_at: Option<usize> = None;

    for (offset, ch) in rest[cursor..].char_indices() {
        if in_string {
            if escaping {
                escaping = false;
                continue;
            }
            match ch {
                '\\' => escaping = true,
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }

        match ch {
            '"' => in_string = true,
            '(' => depth += 1,
            ')' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    close_paren_at = Some(cursor + offset);
                    break;
                }
            }
            _ => {}
        }
    }

    let close_paren_at = close_paren_at?;
    let args = rest[cursor..close_paren_at].trim();
    let mut tail = &rest[close_paren_at + 1..];
    tail = tail.trim_start();
    if !tail.starts_with(']') {
        return None;
    }

    Some((
        RawToolUseBlock {
            id: format!("raw_tool_call_{call_number}_{}", uuid::Uuid::new_v4()),
            name: name.to_string(),
            input_json: if args.is_empty() {
                "{}".to_string()
            } else {
                args.to_string()
            },
        },
        &tail[1..],
    ))
}

fn extract_raw_tool_use_blocks(text: &str) -> Option<Vec<RawToolUseBlock>> {
    let normalized = strip_minimax_tool_wrappers(text);

    let mut calls = Vec::new();
    let mut rest = normalized.as_str();
    while let Some(pos) = rest.find("[tool_use:") {
        if let Some((call, tail)) = parse_raw_tool_use_block(&rest[pos..], calls.len() + 1) {
            calls.push(call);
            rest = tail;
        } else {
            // Skip malformed block and continue scanning
            rest = &rest[pos + "[tool_use:".len()..];
        }
    }

    if calls.is_empty() { None } else { Some(calls) }
}

/// Removes raw `[tool_use: ...]` blocks from text, returning cleaned text.
fn strip_raw_tool_use_text(text: &str) -> String {
    let normalized = strip_minimax_tool_wrappers(text);
    let mut result = String::with_capacity(normalized.len());
    let mut rest = normalized.as_str();

    while let Some(pos) = rest.find("[tool_use:") {
        result.push_str(&rest[..pos]);
        let remaining = &rest[pos..];
        if let Some(close) = find_raw_tool_use_end(remaining) {
            rest = &remaining[close..];
        } else {
            result.push_str(remaining);
            break;
        }
    }
    result.push_str(rest);
    result.trim().to_string()
}

/// Finds the end position (after `]`) of a `[tool_use: ...]` block.
fn find_raw_tool_use_end(text: &str) -> Option<usize> {
    let prefix = "[tool_use:";
    if !text.starts_with(prefix) {
        return None;
    }
    let after_prefix = &text[prefix.len()..].trim_start();
    let open_paren = after_prefix.find('(')?;
    let offset_to_open_paren = text[prefix.len()..].len() - after_prefix.len();
    let cursor = prefix.len() + offset_to_open_paren + open_paren + 1;

    let mut depth = 1usize;
    let mut in_string = false;
    let mut escaping = false;

    for (offset, ch) in text[cursor..].char_indices() {
        if in_string {
            if escaping {
                escaping = false;
                continue;
            }
            match ch {
                '\\' => escaping = true,
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match ch {
            '"' => in_string = true,
            '(' => depth += 1,
            ')' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    let after_close = cursor + offset + 1;
                    let remaining = &text[after_close..];
                    let trimmed = remaining.trim_start();
                    let skipped = remaining.len() - trimmed.len();
                    if trimmed.starts_with(']') {
                        return Some(after_close + skipped + 1);
                    }
                    return None;
                }
            }
            _ => {}
        }
    }
    None
}

#[derive(Debug, Deserialize)]
struct OpenAiResponse {
    choices: Vec<Choice>,
}

#[derive(Debug, Deserialize)]
struct ResponsesApiResponse {
    output: Vec<ResponsesOutputItem>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum ResponsesOutputItem {
    #[serde(rename = "message")]
    Message {
        role: String,
        content: Vec<ResponsesOutputPart>,
    },
    #[serde(rename = "function_call")]
    FunctionCall {
        call_id: String,
        name: String,
        arguments: String,
    },
    #[serde(other)]
    Ignored,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum ResponsesOutputPart {
    #[serde(rename = "output_text")]
    OutputText { text: String },
    #[serde(rename = "refusal")]
    Refusal { refusal: String },
    #[serde(other)]
    Ignored,
}

#[derive(Debug, Deserialize)]
struct Choice {
    message: AssistantMessage,
}

#[derive(Debug, Deserialize)]
struct AssistantMessage {
    #[serde(default)]
    content: Option<OpenAiMessageContent>,
    #[serde(default)]
    tool_calls: Option<Vec<OaiToolCall>>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum OpenAiMessageContent {
    Text(String),
    Parts(Vec<OpenAiContentPart>),
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum OpenAiContentPart {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "refusal")]
    Refusal { refusal: String },
    #[serde(other)]
    Ignored,
}

#[derive(Debug, Deserialize)]
struct OaiToolCall {
    id: String,
    function: OaiFunction,
}

#[derive(Debug, Deserialize)]
struct OaiFunction {
    name: String,
    arguments: String,
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
            api_key: api_key.map(ToString::to_string),
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
