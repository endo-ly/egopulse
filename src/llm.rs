use async_trait::async_trait;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue};
use serde::{Deserialize, Serialize};

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
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MessagesResponse {
    pub content: String,
}

#[async_trait]
pub trait LlmProvider: Send + Sync {
    async fn send_message(
        &self,
        _system: &str,
        messages: Vec<Message>,
    ) -> Result<MessagesResponse, LlmError>;
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
