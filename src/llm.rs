use async_trait::async_trait;
use futures_util::StreamExt;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::UnboundedSender;

use crate::config::{Config, authorization_token};
use crate::error::LlmError;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Message {
    pub role: String,
    pub content: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MessagesResponse {
    pub content: String,
    pub tool_calls: Vec<ToolCall>,
}

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

pub fn create_provider(config: &Config) -> Result<Box<dyn LlmProvider>, LlmError> {
    Ok(Box::new(OpenAiProvider::new(config)?))
}

pub struct OpenAiProvider {
    http: reqwest::Client,
    api_key: Option<String>,
    model: String,
    base_url: String,
}

impl OpenAiProvider {
    pub fn new(config: &Config) -> Result<Self, LlmError> {
        let http = reqwest::Client::builder()
            .user_agent(format!("egopulse/{}", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|error| LlmError::InitFailed(error.to_string()))?;

        Ok(Self {
            http,
            api_key: authorization_token(config).map(ToString::to_string),
            model: config.model.clone(),
            base_url: config.llm_base_url.clone(),
        })
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
            let response = self.send_message(system, messages, None).await?;
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

fn translate_message_to_openai(message: &Message) -> serde_json::Value {
    match message.role.as_str() {
        "assistant" if !message.tool_calls.is_empty() => serde_json::json!({
            "role": "assistant",
            "content": if message.content.is_empty() {
                serde_json::Value::Null
            } else {
                serde_json::Value::String(message.content.clone())
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
            "content": message.content,
            "tool_call_id": message.tool_call_id,
        }),
        _ => serde_json::json!({
            "role": message.role,
            "content": message.content,
        }),
    }
}

fn parse_openai_response(body: OpenAiResponse) -> Result<MessagesResponse, LlmError> {
    let choice = body
        .choices
        .into_iter()
        .next()
        .ok_or_else(|| LlmError::InvalidResponse("choices[0] missing".to_string()))?;
    let tool_calls = choice
        .message
        .tool_calls
        .unwrap_or_default()
        .into_iter()
        .map(parse_tool_call)
        .collect::<Result<Vec<_>, _>>()?;

    let content = extract_text(
        choice
            .message
            .content
            .unwrap_or(MessageContent::Text(String::new())),
    );
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

fn extract_text(content: MessageContent) -> String {
    match content {
        MessageContent::Text(text) => text.trim().to_string(),
        MessageContent::Parts(parts) => parts
            .into_iter()
            .filter_map(|part| match part {
                ContentPart::Text { text } => Some(text),
                ContentPart::Refusal { refusal } => Some(refusal),
                ContentPart::Ignored => None,
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

#[derive(Debug, Deserialize)]
struct OpenAiResponse {
    choices: Vec<Choice>,
}

#[derive(Debug, Deserialize)]
struct Choice {
    message: AssistantMessage,
}

#[derive(Debug, Deserialize)]
struct AssistantMessage {
    #[serde(default)]
    content: Option<MessageContent>,
    #[serde(default)]
    tool_calls: Option<Vec<OaiToolCall>>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum MessageContent {
    Text(String),
    Parts(Vec<ContentPart>),
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum ContentPart {
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
    use wiremock::matchers::{body_partial_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use crate::config::Config;

    use super::{Message, ToolCall, ToolDefinition, create_provider};

    fn message(content: &str) -> Vec<Message> {
        vec![Message {
            role: "user".to_string(),
            content: content.to_string(),
            tool_calls: Vec::new(),
            tool_call_id: None,
        }]
    }

    fn config(model: &str, base_url: String, api_key: Option<&str>) -> Config {
        Config {
            model: model.to_string(),
            api_key: api_key
                .map(|value| secrecy::SecretString::new(value.to_string().into_boxed_str())),
            llm_base_url: base_url,
            data_dir: ".egopulse-test".to_string(),
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
                        "name": "read_file"
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
                                "name": "read_file",
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
                    name: "read_file".to_string(),
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
                name: "read_file".to_string(),
                arguments: serde_json::json!({"path": "README.md"}),
            }]
        );
    }
}
