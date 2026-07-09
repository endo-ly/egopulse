use super::*;
use super::{messages::*, responses::*};
use futures_util::StreamExt;
use reqwest::StatusCode;
use reqwest::header::HeaderName;
use std::collections::BTreeMap;
use std::sync::Arc;

const REQUEST_TIMEOUT_SECS: u64 = 300;

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
            .timeout(std::time::Duration::from_secs(REQUEST_TIMEOUT_SECS))
            .build()
            .map_err(|error| LlmError::InitFailed(error.to_string()))?;

        let is_codex = crate::llm::codex_auth::is_codex_provider(&config.provider);
        let (api_key, account_id) = if is_codex {
            let auth = crate::llm::codex_auth::resolve_codex_auth()
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
        messages: Arc<Vec<Message>>,
        tools: Option<Arc<Vec<ToolDefinition>>>,
    ) -> Result<MessagesResponse, LlmError> {
        let url = format!("{}/responses", self.base_url.trim_end_matches('/'));
        let mut body = build_responses_request_body(
            &self.model,
            system,
            &messages,
            tools.as_deref().map(|arc| arc.as_slice()),
        );
        if self.is_codex {
            body["store"] = serde_json::Value::Bool(false);
            body["stream"] = serde_json::Value::Bool(true);
        }

        if self.is_codex {
            let response = self.send_codex_with_retry(&url, &body).await?;
            let text = response.text().await.map_err(LlmError::RequestFailed)?;
            return parse_codex_responses_payload(&text).and_then(parse_responses_response);
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
            let retry_after = parse_retry_after(response.headers());
            return Err(Self::api_error(
                status,
                response.text().await.unwrap_or_default(),
                retry_after,
            ));
        }

        let resp_body: ResponsesApiResponse = response.json().await?;
        parse_responses_response(resp_body)
    }

    /// Sends a Codex request with exponential-backoff retry on 429 responses.
    ///
    /// On success, returns the streaming `reqwest::Response` so callers can
    /// either consume the body as text (non-stream path) or iterate its
    /// `bytes_stream` for incremental delta emission (stream path).
    /// On non-429 failure, attempts to extract a structured error message from
    /// the response body before returning.
    async fn send_codex_with_retry(
        &self,
        url: &str,
        body: &serde_json::Value,
    ) -> Result<reqwest::Response, LlmError> {
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
                return Ok(response);
            }

            if status.as_u16() == 429 && retries < max_retries {
                retries += 1;
                let retry_after = parse_retry_after(response.headers())
                    .map(std::time::Duration::from_secs)
                    .unwrap_or_else(|| std::time::Duration::from_secs(2u64.pow(retries)));
                tracing::warn!(
                    "Codex rate limited, retrying in {retry_after:?} (attempt {retries}/{max_retries})"
                );
                tokio::time::sleep(retry_after).await;
                continue;
            }

            let retry_after = parse_retry_after(response.headers());
            let text = response.text().await.unwrap_or_default();
            return Err(Self::structured_api_error(status, &text, retry_after));
        }
    }

    fn build_headers(&self) -> Result<HeaderMap, LlmError> {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

        if self.is_codex {
            let auth = crate::llm::codex_auth::resolve_codex_auth()
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

    fn api_error(status: StatusCode, body: String, retry_after_secs: Option<u64>) -> LlmError {
        LlmError::ApiError {
            status,
            body_preview: preview_body(&body),
            retry_after_secs,
        }
    }

    fn structured_api_error(
        status: StatusCode,
        body: &str,
        retry_after_secs: Option<u64>,
    ) -> LlmError {
        if let Ok(err) = serde_json::from_str::<OaiErrorResponse>(body) {
            return LlmError::ApiError {
                status,
                body_preview: err.error.message,
                retry_after_secs,
            };
        }
        LlmError::ApiError {
            status,
            body_preview: preview_body(body),
            retry_after_secs,
        }
    }

    async fn stream_responses_api(
        &self,
        system: &str,
        messages: Arc<Vec<Message>>,
        tools: Option<Arc<Vec<ToolDefinition>>>,
        on_delta: &(dyn Fn(String) + Send + Sync),
    ) -> Result<MessagesResponse, LlmError> {
        let url = format!("{}/responses", self.base_url.trim_end_matches('/'));
        let mut body = build_responses_request_body(
            &self.model,
            system,
            &messages,
            tools.as_deref().map(|arc| arc.as_slice()),
        );
        body["stream"] = serde_json::Value::Bool(true);

        let response = self
            .http
            .post(url)
            .headers(self.build_headers()?)
            .json(&body)
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            let retry_after = parse_retry_after(response.headers());
            return Err(Self::api_error(
                status,
                response.text().await.unwrap_or_default(),
                retry_after,
            ));
        }

        let mut accumulator = ResponsesAccumulator::new();
        let mut data_stream = crate::llm::sse::data_lines(response.bytes_stream());
        while let Some(payload) = data_stream.next().await {
            if accumulator.process_event(&payload, on_delta) {
                break;
            }
        }
        accumulator.finish()
    }

    async fn stream_codex_responses(
        &self,
        system: &str,
        messages: Arc<Vec<Message>>,
        tools: Option<Arc<Vec<ToolDefinition>>>,
        on_delta: &(dyn Fn(String) + Send + Sync),
    ) -> Result<MessagesResponse, LlmError> {
        crate::llm::codex_auth::refresh_if_needed(&self.http).await;

        let url = format!("{}/responses", self.base_url.trim_end_matches('/'));
        let mut body = build_responses_request_body(
            &self.model,
            system,
            &messages,
            tools.as_deref().map(|arc| arc.as_slice()),
        );
        body["store"] = serde_json::Value::Bool(false);
        body["stream"] = serde_json::Value::Bool(true);

        let response = self.send_codex_with_retry(&url, &body).await?;

        let mut accumulator = ResponsesAccumulator::new();
        let mut data_stream = crate::llm::sse::data_lines(response.bytes_stream());
        while let Some(payload) = data_stream.next().await {
            if accumulator.process_event(&payload, on_delta) {
                break;
            }
        }
        accumulator.finish()
    }
}

fn parse_retry_after(headers: &reqwest::header::HeaderMap) -> Option<u64> {
    headers
        .get("retry-after")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
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
        messages: Arc<Vec<Message>>,
        tools: Option<Arc<Vec<ToolDefinition>>>,
    ) -> Result<MessagesResponse, LlmError> {
        if self.is_codex {
            crate::llm::codex_auth::refresh_if_needed(&self.http).await;
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
                tools.as_deref().map(|arc| arc.as_slice()),
                None,
                should_preserve_reasoning_content(&self.provider, &self.base_url, &self.model),
            ))
            .send()
            .await?;

        let status = response.status();
        if !status.is_success() {
            let retry_after = parse_retry_after(response.headers());
            return Err(Self::api_error(
                status,
                response.text().await.unwrap_or_default(),
                retry_after,
            ));
        }

        let body: OpenAiResponse = response.json().await?;
        parse_openai_response(body)
    }

    async fn send_message_streaming(
        &self,
        system: &str,
        messages: Arc<Vec<Message>>,
        tools: Option<Arc<Vec<ToolDefinition>>>,
        on_delta: &(dyn Fn(String) + Send + Sync),
    ) -> Result<MessagesResponse, LlmError> {
        if self.is_codex {
            return self
                .stream_codex_responses(system, messages, tools, on_delta)
                .await;
        }
        if should_use_responses_api(&messages) {
            return self
                .stream_responses_api(system, messages, tools, on_delta)
                .await;
        }

        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let headers = self.build_headers()?;
        let body = build_request_body(
            &self.model,
            system,
            &messages,
            tools.as_deref().map(|arc| arc.as_slice()),
            Some(true),
            should_preserve_reasoning_content(&self.provider, &self.base_url, &self.model),
        );

        let response = self
            .http
            .post(url)
            .headers(headers)
            .json(&body)
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            let retry_after = parse_retry_after(response.headers());
            return Err(Self::api_error(
                status,
                response.text().await.unwrap_or_default(),
                retry_after,
            ));
        }

        let mut accumulator = ChatCompletionAccumulator::new();
        let mut data_stream = crate::llm::sse::data_lines(response.bytes_stream());
        while let Some(payload) = data_stream.next().await {
            accumulator.process_chunk(&payload, on_delta);
        }
        accumulator.finish()
    }
}

pub(crate) fn should_preserve_reasoning_content(
    provider: &str,
    base_url: &str,
    model: &str,
) -> bool {
    let provider = provider.to_ascii_lowercase();
    let model = model.to_ascii_lowercase();
    provider.contains("deepseek") || model.contains("deepseek") || is_deepseek_base_url(base_url)
}

fn is_deepseek_base_url(base_url: &str) -> bool {
    reqwest::Url::parse(base_url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_ascii_lowercase))
        .is_some_and(|host| host == "deepseek.com" || host.ends_with(".deepseek.com"))
}

fn parse_chat_completion_usage(usage: &serde_json::Value) -> Option<LlmUsage> {
    let prompt_tokens = usage.get("prompt_tokens").and_then(|v| v.as_i64())?;
    let completion_tokens = usage.get("completion_tokens").and_then(|v| v.as_i64())?;
    Some(LlmUsage {
        input_tokens: prompt_tokens,
        output_tokens: completion_tokens,
    })
}

struct ChatCompletionAccumulator {
    content: String,
    reasoning_content: Option<String>,
    tool_calls: BTreeMap<usize, StreamingToolCall>,
    usage: Option<LlmUsage>,
}

impl ChatCompletionAccumulator {
    fn new() -> Self {
        Self {
            content: String::new(),
            reasoning_content: None,
            tool_calls: BTreeMap::new(),
            usage: None,
        }
    }

    fn process_chunk(&mut self, payload: &str, on_delta: &dyn Fn(String)) {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(payload) else {
            tracing::warn!("skipping malformed SSE data line");
            return;
        };
        if let Some(delta) = value
            .get("choices")
            .and_then(|choices| choices.get(0))
            .and_then(|choice| choice.get("delta"))
        {
            if let Some(text) = delta.get("content").and_then(|v| v.as_str()) {
                if !text.is_empty() {
                    on_delta(text.to_string());
                }
                self.content.push_str(text);
            }
            if let Some(reasoning) = delta.get("reasoning_content").and_then(|v| v.as_str()) {
                match &mut self.reasoning_content {
                    Some(existing) => existing.push_str(reasoning),
                    None => self.reasoning_content = Some(reasoning.to_string()),
                }
            }
            if let Some(tool_calls) = delta.get("tool_calls").and_then(|v| v.as_array()) {
                for entry in tool_calls {
                    self.ingest_tool_call_delta(entry);
                }
            }
        }
        if let Some(usage_value) = value.get("usage") {
            self.usage = parse_chat_completion_usage(usage_value);
        }
    }

    fn ingest_tool_call_delta(&mut self, entry: &serde_json::Value) {
        let Some(index) = entry.get("index").and_then(|v| v.as_u64()) else {
            return;
        };
        let slot = self.tool_calls.entry(index as usize).or_default();
        if let Some(id) = entry.get("id").and_then(|v| v.as_str()) {
            slot.id = Some(id.to_string());
        }
        if let Some(function) = entry.get("function") {
            if let Some(name) = function.get("name").and_then(|v| v.as_str()) {
                slot.name = Some(name.to_string());
            }
            if let Some(arguments) = function.get("arguments").and_then(|v| v.as_str()) {
                slot.arguments.push_str(arguments);
            }
        }
    }

    fn finish(self) -> Result<MessagesResponse, LlmError> {
        let tool_calls = self
            .tool_calls
            .into_values()
            .map(|slot| {
                let name = slot.name.unwrap_or_default();
                let arguments = parse_tool_arguments(&slot.arguments, &name)?;
                Ok(ToolCall {
                    id: slot.id.unwrap_or_default(),
                    name,
                    arguments,
                })
            })
            .collect::<Result<Vec<_>, LlmError>>()?;
        assemble_response(self.content, self.reasoning_content, tool_calls, self.usage)
    }
}

#[derive(Default)]
struct StreamingToolCall {
    id: Option<String>,
    name: Option<String>,
    arguments: String,
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use crate::config::ResolvedLlmConfig;
    use crate::llm::{
        LlmProvider, LlmUsage, Message, MessageContent, MessageContentPart, ToolCall,
    };

    use super::OpenAiProvider;

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

    fn message(content: &str) -> Arc<Vec<Message>> {
        Arc::new(vec![Message::text("user", content)])
    }

    #[tokio::test]
    async fn streams_chat_completions_text_deltas_via_on_delta() {
        let server = MockServer::start().await;
        let sse_body = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"ファイルを\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"確認します\",\"reasoning_content\":\"考えている\"}}]}\n\n",
            "data: {\"choices\":[]}\n\n",
            "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":5}}\n\n",
            "data: [DONE]\n\n",
            "garbage line\n",
        );
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(sse_body, "text/event-stream"))
            .mount(&server)
            .await;

        let provider = OpenAiProvider::new(&config(
            "gpt-4o-mini",
            format!("{}/v1", server.uri()),
            Some("sk-test"),
        ))
        .expect("provider");

        let recorded = Arc::new(Mutex::new(Vec::<String>::new()));
        let recorded_for_closure = Arc::clone(&recorded);
        let on_delta = move |delta: String| {
            recorded_for_closure
                .lock()
                .expect("on_delta lock poisoned")
                .push(delta);
        };

        let response = provider
            .send_message_streaming("", message("hello"), None, &on_delta)
            .await
            .expect("response");

        assert_eq!(
            recorded.lock().expect("lock poisoned").as_slice(),
            ["ファイルを".to_string(), "確認します".to_string()]
        );
        assert_eq!(response.content, "ファイルを確認します");
        assert_eq!(response.reasoning_content.as_deref(), Some("考えている"));
        assert_eq!(
            response.usage,
            Some(LlmUsage {
                input_tokens: 10,
                output_tokens: 5,
            })
        );
        assert!(response.tool_calls.is_empty());
    }

    #[tokio::test]
    async fn accumulates_streaming_tool_calls_by_index_in_order() {
        let server = MockServer::start().await;
        // index:1 arguments arrive before index:0 finishes, yet the result
        // must be ordered [0, 1].
        let sse_body = concat!(
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_a\",\"type\":\"function\",\"function\":{\"name\":\"read\",\"arguments\":\"\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":1,\"id\":\"call_b\",\"type\":\"function\",\"function\":{\"name\":\"grep\",\"arguments\":\"\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":1,\"function\":{\"arguments\":\"{\\\"pattern\\\":\\\"foo\\\"}\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{\\\"path\\\":\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"\\\"README.md\\\"}\"}}]}}]}\n\n",
            "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":2}}\n\n",
            "data: [DONE]\n\n",
        );
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(sse_body, "text/event-stream"))
            .mount(&server)
            .await;

        let provider = OpenAiProvider::new(&config(
            "gpt-4o-mini",
            format!("{}/v1", server.uri()),
            Some("sk-test"),
        ))
        .expect("provider");

        let recorded = Arc::new(Mutex::new(Vec::<String>::new()));
        let recorded_for_closure = Arc::clone(&recorded);
        let on_delta = move |delta: String| {
            recorded_for_closure
                .lock()
                .expect("on_delta lock poisoned")
                .push(delta);
        };

        let response = provider
            .send_message_streaming("", message("use tools"), None, &on_delta)
            .await
            .expect("response");

        assert_eq!(
            response.tool_calls,
            vec![
                ToolCall {
                    id: "call_a".to_string(),
                    name: "read".to_string(),
                    arguments: serde_json::json!({"path": "README.md"}),
                },
                ToolCall {
                    id: "call_b".to_string(),
                    name: "grep".to_string(),
                    arguments: serde_json::json!({"pattern": "foo"}),
                },
            ]
        );
        assert!(response.content.is_empty());
        assert!(recorded.lock().expect("lock poisoned").is_empty());
    }

    #[tokio::test]
    async fn no_delta_when_content_empty_tool_only() {
        let server = MockServer::start().await;
        let sse_body = concat!(
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_x\",\"type\":\"function\",\"function\":{\"name\":\"list\",\"arguments\":\"{}\"}}]}}]}\n\n",
            "data: {\"choices\":[]}\n\n",
            "data: [DONE]\n\n",
        );
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(sse_body, "text/event-stream"))
            .mount(&server)
            .await;

        let provider = OpenAiProvider::new(&config(
            "gpt-4o-mini",
            format!("{}/v1", server.uri()),
            Some("sk-test"),
        ))
        .expect("provider");

        let recorded = Arc::new(Mutex::new(Vec::<String>::new()));
        let recorded_for_closure = Arc::clone(&recorded);
        let on_delta = move |delta: String| {
            recorded_for_closure
                .lock()
                .expect("on_delta lock poisoned")
                .push(delta);
        };

        let response = provider
            .send_message_streaming("", message("tool only"), None, &on_delta)
            .await
            .expect("response");

        assert!(recorded.lock().expect("lock poisoned").is_empty());
        assert!(response.content.is_empty());
        assert_eq!(
            response.tool_calls,
            vec![ToolCall {
                id: "call_x".to_string(),
                name: "list".to_string(),
                arguments: serde_json::json!({}),
            }]
        );
    }

    fn multimodal_message() -> Arc<Vec<Message>> {
        Arc::new(vec![Message {
            role: "user".to_string(),
            content: MessageContent::parts(vec![
                MessageContentPart::InputText {
                    text: "describe this image".to_string(),
                },
                MessageContentPart::InputImage {
                    image_url: "data:image/png;base64,AAAA".to_string(),
                    detail: Some("auto".to_string()),
                },
            ]),
            reasoning_content: None,
            tool_calls: Vec::new(),
            tool_call_id: None,
        }])
    }

    #[tokio::test]
    async fn streams_responses_api_text_deltas_via_on_delta() {
        let server = MockServer::start().await;
        let sse_body = concat!(
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"ファイルを\"}\n\n",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"確認します\"}\n\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"output\":[{\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"ファイルを確認します\"}]}],\"usage\":{\"input_tokens\":10,\"output_tokens\":5}}}\n\n",
            "data: [DONE]\n\n",
        );
        Mock::given(method("POST"))
            .and(path("/v1/responses"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(sse_body, "text/event-stream"))
            .mount(&server)
            .await;

        let provider = OpenAiProvider::new(&config(
            "gpt-4o-mini",
            format!("{}/v1", server.uri()),
            Some("sk-test"),
        ))
        .expect("provider");

        let recorded = Arc::new(Mutex::new(Vec::<String>::new()));
        let recorded_for_closure = Arc::clone(&recorded);
        let on_delta = move |delta: String| {
            recorded_for_closure
                .lock()
                .expect("on_delta lock poisoned")
                .push(delta);
        };

        let response = provider
            .send_message_streaming("", multimodal_message(), None, &on_delta)
            .await
            .expect("response");

        assert_eq!(
            recorded.lock().expect("lock poisoned").as_slice(),
            ["ファイルを".to_string(), "確認します".to_string()]
        );
        assert_eq!(response.content, "ファイルを確認します");
        assert_eq!(
            response.usage,
            Some(LlmUsage {
                input_tokens: 10,
                output_tokens: 5,
            })
        );
    }

    #[tokio::test]
    async fn streams_codex_text_deltas_via_on_delta() {
        let codex_dir = tempfile::tempdir().expect("tempdir");
        let _env =
            crate::test_env::EnvVarGuard::set("OPENAI_CODEX_ACCESS_TOKEN", "test-codex-token")
                .also_set("CODEX_HOME", codex_dir.path());
        crate::llm::codex_auth::clear_auth_cache();

        let server = MockServer::start().await;
        let sse_body = concat!(
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"thinking\"}\n\n",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\" carefully\"}\n\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"output\":[],\"usage\":{\"input_tokens\":3,\"output_tokens\":7}}}\n\n",
            "data: [DONE]\n\n",
        );
        Mock::given(method("POST"))
            .and(path("/v1/responses"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(sse_body, "text/event-stream"))
            .mount(&server)
            .await;

        let codex_config = ResolvedLlmConfig {
            provider: "openai-codex".to_string(),
            label: "Codex".to_string(),
            base_url: format!("{}/v1", server.uri()),
            api_key: None,
            model: "gpt-5.3-codex".to_string(),
        };
        let provider = OpenAiProvider::new(&codex_config).expect("provider");

        let recorded = Arc::new(Mutex::new(Vec::<String>::new()));
        let recorded_for_closure = Arc::clone(&recorded);
        let on_delta = move |delta: String| {
            recorded_for_closure
                .lock()
                .expect("on_delta lock poisoned")
                .push(delta);
        };

        let response = provider
            .send_message_streaming("", message("hello"), None, &on_delta)
            .await
            .expect("response");

        assert_eq!(
            recorded.lock().expect("lock poisoned").as_slice(),
            ["thinking".to_string(), " carefully".to_string()]
        );
        assert_eq!(response.content, "thinking carefully");
        assert_eq!(
            response.usage,
            Some(LlmUsage {
                input_tokens: 3,
                output_tokens: 7,
            })
        );
    }
}
