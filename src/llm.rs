use async_trait::async_trait;
use futures_util::StreamExt;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::UnboundedSender;

use crate::config::{Config, authorization_token};
use crate::error::LlmError;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Message {
    pub role: String,
    pub content: String,
}

#[derive(Clone, Debug, Serialize)]
struct MessagesRequest {
    model: String,
    messages: Vec<Message>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MessagesResponse {
    pub content: String,
}

#[async_trait]
pub trait LlmProvider: Send + Sync {
    /// Non-streaming message send.
    async fn send_message(
        &self,
        _system: &str,
        messages: Vec<Message>,
    ) -> Result<MessagesResponse, LlmError>;

    /// Streaming message send.
    /// text_tx receives each text chunk as it arrives from the LLM.
    async fn send_message_stream(
        &self,
        system: &str,
        messages: Vec<Message>,
        text_tx: Option<&UnboundedSender<String>>,
    ) -> Result<MessagesResponse, LlmError> {
        // Default: fall back to non-streaming
        let response = self.send_message(system, messages).await?;
        if let Some(tx) = text_tx {
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
        _system: &str,
        messages: Vec<Message>,
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
            .json(&MessagesRequest {
                model: self.model.clone(),
                messages,
                stream: None,
            })
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
        let choice = body
            .choices
            .into_iter()
            .next()
            .ok_or_else(|| LlmError::InvalidResponse("choices[0] missing".to_string()))?;
        let content = extract_text(choice.message.content)?;
        Ok(MessagesResponse { content })
    }

    async fn send_message_stream(
        &self,
        _system: &str,
        messages: Vec<Message>,
        text_tx: Option<&UnboundedSender<String>>,
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
            .json(&MessagesRequest {
                model: self.model.clone(),
                messages,
                stream: Some(true),
            })
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

        // Process streaming response
        let mut byte_stream = response.bytes_stream();
        let mut sse = SseEventParser::default();
        let mut text = String::new();
        let mut done = false;

        'outer: while let Some(chunk_res) = byte_stream.next().await {
            let chunk = match chunk_res {
                Ok(c) => c,
                Err(e) => return Err(LlmError::RequestFailed(e)),
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

        // Flush any remaining data (skip if [DONE] was already seen)
        if !done {
            for data in sse.finish() {
                if data == "[DONE]" {
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

        let text = text.trim().to_string();
        if text.is_empty() {
            return Err(LlmError::InvalidResponse(
                "assistant content was empty".to_string(),
            ));
        }

        Ok(MessagesResponse { content: text })
    }
}

/// SSE event parser for streaming responses.
/// Based on Microclaw's SseEventParser implementation.
#[derive(Default)]
struct SseEventParser {
    pending: Vec<u8>,
    data_lines: Vec<String>,
}

impl SseEventParser {
    fn push_chunk(&mut self, chunk: impl AsRef<[u8]>) -> Vec<String> {
        self.pending.extend_from_slice(chunk.as_ref());
        let mut events = Vec::new();
        while let Some(pos) = self.pending.iter().position(|b| *b == b'\n') {
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
            Err(err) => String::from_utf8_lossy(&err.into_bytes()).into_owned(),
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
            Some((f, v)) => {
                let v = v.strip_prefix(' ').unwrap_or(v);
                (f, v)
            }
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

/// Process a single OpenAI streaming event and extract text delta.
fn process_openai_stream_event(data: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(data).ok()?;
    let choice = v.get("choices")?.as_array()?.first()?;
    let delta = choice.get("delta")?;
    let content = delta.get("content")?;
    match content {
        serde_json::Value::String(s) if !s.is_empty() => Some(s.clone()),
        serde_json::Value::Array(arr) => {
            let text: String = arr
                .iter()
                .filter_map(|item| item.get("text")?.as_str())
                .collect();
            if text.is_empty() { None } else { Some(text) }
        }
        _ => None,
    }
}

fn extract_text(content: MessageContent) -> Result<String, LlmError> {
    let text = match content {
        MessageContent::Text(text) => text,
        MessageContent::Parts(parts) => parts
            .into_iter()
            .filter_map(|part| match part {
                ContentPart::Text { text } => Some(text),
                ContentPart::Refusal { refusal } => Some(refusal),
                ContentPart::Ignored => None,
            })
            .collect::<Vec<_>>()
            .join("\n"),
    };
    let text = text.trim().to_string();
    if text.is_empty() {
        return Err(LlmError::InvalidResponse(
            "assistant content was empty".to_string(),
        ));
    }
    Ok(text)
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
    content: MessageContent,
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

#[cfg(test)]
mod tests {
    use wiremock::matchers::{body_partial_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use crate::config::Config;

    use super::{Message, create_provider};

    fn message(content: &str) -> Vec<Message> {
        vec![Message {
            role: "user".to_string(),
            content: content.to_string(),
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
            .send_message("", message("hello"))
            .await
            .expect("response");

        assert_eq!(response.content, "hello back");
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
            .send_message("", message("hello"))
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
            .send_message("", message("hello"))
            .await
            .expect("response");

        assert_eq!(response.content, "local answer");
    }
}
