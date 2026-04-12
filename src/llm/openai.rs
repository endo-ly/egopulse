use super::{messages::*, responses::*, sse::*};
use super::*;

/// OpenAI-compatible LLM provider using Chat Completions and Responses APIs.
pub(crate) struct OpenAiProvider {
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
            api_key: config
                .api_key
                .as_ref()
                .map(|key| key.expose_secret().to_string()),
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
