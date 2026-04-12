use super::*;
use super::{messages::*, responses::*, sse::*};
use reqwest::StatusCode;

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
            .connect_timeout(std::time::Duration::from_secs(10))
            .timeout(std::time::Duration::from_secs(120))
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
        let response = self
            .http
            .post(url)
            .headers(self.build_headers()?)
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
            return Err(Self::api_error(
                status,
                response.text().await.unwrap_or_default(),
            ));
        }

        let body: ResponsesApiResponse = response.json().await?;
        parse_responses_response(body)
    }

    fn build_headers(&self) -> Result<HeaderMap, LlmError> {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

        let Some(api_key) = &self.api_key else {
            return Ok(headers);
        };

        let auth_value = HeaderValue::from_str(&format!("Bearer {api_key}"))
            .map_err(|error| LlmError::RequestConstructionFailed(error.to_string()))?;
        headers.insert(AUTHORIZATION, auth_value);
        Ok(headers)
    }

    fn api_error(status: StatusCode, body: String) -> LlmError {
        LlmError::ApiError {
            status,
            body_preview: preview_body(&body),
        }
    }

    fn stream_text_piece(
        piece: Option<String>,
        text: &mut String,
        text_tx: Option<&UnboundedSender<String>>,
    ) {
        let Some(piece) = piece.filter(|piece| !piece.is_empty()) else {
            return;
        };

        text.push_str(&piece);
        if let Some(tx) = text_tx {
            let _ = tx.send(piece);
        }
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
        let headers = self.build_headers()?;

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
            return Err(Self::api_error(
                status,
                response.text().await.unwrap_or_default(),
            ));
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
        if should_use_responses_api(&messages)
            || tools.as_ref().is_some_and(|tools| !tools.is_empty())
        {
            let response = self.send_message(system, messages, tools).await?;
            if let Some(tx) = text_tx
                && !response.content.is_empty()
            {
                let _ = tx.send(response.content.clone());
            }
            return Ok(response);
        }

        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let headers = self.build_headers()?;

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
            return Err(Self::api_error(
                status,
                response.text().await.unwrap_or_default(),
            ));
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
                Self::stream_text_piece(process_openai_stream_event(&data), &mut text, text_tx);
            }
        }

        if !done {
            for data in sse.finish() {
                if data == "[DONE]" {
                    done = true;
                    break;
                }
                Self::stream_text_piece(process_openai_stream_event(&data), &mut text, text_tx);
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
