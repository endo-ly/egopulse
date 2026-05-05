use super::*;
use super::{messages::*, responses::*};
use reqwest::StatusCode;
use reqwest::header::HeaderName;

/// OpenAI-compatible LLM provider using Chat Completions and Responses APIs.
pub(crate) struct OpenAiProvider {
    http: reqwest::Client,
    api_key: Option<String>,
    model: String,
    base_url: String,
    provider: String,
    account_id: Option<String>,
    is_codex: bool,
}

impl OpenAiProvider {
    /// Build a new provider from the given configuration.
    ///
    /// # Errors
    ///
    /// Returns `LlmError::InitFailed` if the HTTP client cannot be built, or if the
    /// `openai-codex` provider is selected but no Codex auth token is available.
    pub(crate) fn new(config: &ResolvedLlmConfig) -> Result<Self, LlmError> {
        let http = reqwest::Client::builder()
            .user_agent(format!("egopulse/{}", env!("CARGO_PKG_VERSION")))
            .connect_timeout(std::time::Duration::from_secs(10))
            .timeout(std::time::Duration::from_secs(120))
            .build()
            .map_err(|error| LlmError::InitFailed(error.to_string()))?;

        let is_codex = crate::codex_auth::is_codex_provider(&config.provider);
        let (api_key, account_id) = if is_codex {
            let auth = crate::codex_auth::resolve_codex_auth()
                .map_err(|error| LlmError::InitFailed(error.to_string()))?;
            (None, auth.account_id)
        } else {
            (
                config
                    .api_key
                    .as_ref()
                    .map(|key| key.expose_secret().to_string()),
                None,
            )
        };

        Ok(Self {
            http,
            api_key,
            model: config.model.clone(),
            base_url: config.base_url.clone(),
            provider: config.provider.clone(),
            account_id,
            is_codex,
        })
    }

    async fn send_message_via_responses(
        &self,
        system: &str,
        messages: Vec<Message>,
        tools: Option<Vec<ToolDefinition>>,
    ) -> Result<MessagesResponse, LlmError> {
        let url = format!("{}/responses", self.base_url.trim_end_matches('/'));
        let mut body =
            build_responses_request_body(&self.model, system, &messages, tools.as_deref());
        if self.is_codex {
            body["store"] = serde_json::Value::Bool(false);
            body["stream"] = serde_json::Value::Bool(true);
        }

        if self.is_codex {
            return self
                .send_codex_with_retry(&url, &body)
                .await
                .and_then(|text| parse_codex_responses_payload(&text))
                .and_then(parse_responses_response);
        }

        let response = self
            .http
            .post(url)
            .headers(self.build_headers()?)
            .json(&body)
            .send()
            .await?;

        let status = response.status();
        if !status.is_success() {
            return Err(Self::api_error(
                status,
                response.text().await.unwrap_or_default(),
            ));
        }

        let resp_body: ResponsesApiResponse = response.json().await?;
        parse_responses_response(resp_body)
    }

    /// Sends a Codex request with exponential-backoff retry on 429 responses.
    ///
    /// On success, returns the raw response text (which may be plain JSON or SSE).
    /// On non-429 failure, attempts to extract a structured error message from the
    /// response body before returning.
    async fn send_codex_with_retry(
        &self,
        url: &str,
        body: &serde_json::Value,
    ) -> Result<String, LlmError> {
        let max_retries: u32 = 3;
        let mut retries = 0u32;

        loop {
            let response = self
                .http
                .post(url)
                .headers(self.build_headers()?)
                .json(body)
                .send()
                .await?;

            let status = response.status();
            if status.is_success() {
                return response.text().await.map_err(LlmError::RequestFailed);
            }

            if status.as_u16() == 429 && retries < max_retries {
                retries += 1;
                let delay = std::time::Duration::from_secs(2u64.pow(retries));
                tracing::warn!(
                    "Codex rate limited, retrying in {delay:?} (attempt {retries}/{max_retries})"
                );
                tokio::time::sleep(delay).await;
                continue;
            }

            let text = response.text().await.unwrap_or_default();
            return Err(Self::structured_api_error(status, &text));
        }
    }

    fn build_headers(&self) -> Result<HeaderMap, LlmError> {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

        if self.is_codex {
            let auth = crate::codex_auth::resolve_codex_auth()
                .map_err(|error| LlmError::RequestConstructionFailed(error.to_string()))?;
            let auth_value = HeaderValue::from_str(&format!("Bearer {}", auth.bearer_token))
                .map_err(|error| LlmError::RequestConstructionFailed(error.to_string()))?;
            headers.insert(AUTHORIZATION, auth_value);

            if let Some(account_id) = &self.account_id {
                let header_name = HeaderName::from_static("chatgpt-account-id");
                let value = HeaderValue::from_str(account_id)
                    .map_err(|error| LlmError::RequestConstructionFailed(error.to_string()))?;
                headers.insert(header_name, value);
            }
        } else if let Some(api_key) = &self.api_key {
            let auth_value = HeaderValue::from_str(&format!("Bearer {api_key}"))
                .map_err(|error| LlmError::RequestConstructionFailed(error.to_string()))?;
            headers.insert(AUTHORIZATION, auth_value);
        }

        Ok(headers)
    }

    fn api_error(status: StatusCode, body: String) -> LlmError {
        LlmError::ApiError {
            status,
            body_preview: preview_body(&body),
        }
    }

    fn structured_api_error(status: StatusCode, body: &str) -> LlmError {
        if let Ok(err) = serde_json::from_str::<OaiErrorResponse>(body) {
            return LlmError::ApiError {
                status,
                body_preview: err.error.message,
            };
        }
        LlmError::ApiError {
            status,
            body_preview: preview_body(body),
        }
    }
}

#[async_trait]
impl LlmProvider for OpenAiProvider {
    fn provider_name(&self) -> &str {
        &self.provider
    }

    fn model_name(&self) -> &str {
        &self.model
    }

    async fn send_message(
        &self,
        system: &str,
        messages: Vec<Message>,
        tools: Option<Vec<ToolDefinition>>,
    ) -> Result<MessagesResponse, LlmError> {
        if self.is_codex {
            crate::codex_auth::refresh_if_needed(&self.http).await;
            return self
                .send_message_via_responses(system, messages, tools)
                .await;
        }

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


}
