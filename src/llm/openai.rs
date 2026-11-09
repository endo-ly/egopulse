use super::*;
use super::{messages::*, responses::*};
use futures_util::StreamExt;
use reqwest::StatusCode;
use reqwest::header::HeaderName;
use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

/// Total time one HTTP request may take, from connect to the last body byte.
///
/// The turn path bounds each attempt with its own 300s iteration deadline, so
/// this cap only matters for callers without one (sleep, compaction, one-shot
/// ask). A full memory rewrite in sleep can legitimately exceed the turn
/// budget, so the transport cap is wider than the iteration deadline.
pub(crate) const REQUEST_TIMEOUT: Duration = Duration::from_secs(600);

/// Inactivity deadline between reads of one request.
///
/// A provider that accepts the connection but never produces a byte (a
/// saturated prefill queue, for example) fails here instead of holding the
/// request open until the total deadline. The timer resets on every successful
/// read, so a slow but progressing generation is unaffected.
const READ_TIMEOUT_SECS: u64 = 90;

/// HTTP client shared by every provider.
///
/// Providers are created per conversation, so each one must not build its own
/// connection pool.
static SHARED_HTTP: OnceLock<Result<reqwest::Client, String>> = OnceLock::new();

fn shared_http_client() -> Result<reqwest::Client, LlmError> {
    SHARED_HTTP
        .get_or_init(|| {
            reqwest::Client::builder()
                .user_agent(format!("egopulse/{}", env!("CARGO_PKG_VERSION")))
                .connect_timeout(Duration::from_secs(10))
                .timeout(REQUEST_TIMEOUT)
                .read_timeout(Duration::from_secs(READ_TIMEOUT_SECS))
                .build()
                .map_err(|error| error.to_string())
        })
        .clone()
        .map_err(LlmError::InitFailed)
}

/// OpenAI-compatible LLM provider using Chat Completions and Responses APIs.
pub(crate) struct OpenAiProvider {
    http: reqwest::Client,
    api_key: Option<String>,
    model: String,
    reasoning_effort: Option<String>,
    base_url: String,
    provider: String,
    account_id: Option<String>,
    is_codex: bool,
    extra_headers: Vec<(HeaderName, HeaderValue)>,
}

impl OpenAiProvider {
    /// Build a new provider from the given configuration.
    ///
    /// # Errors
    ///
    /// Returns `LlmError::InitFailed` if the HTTP client cannot be built, or if the
    /// `openai-codex` provider is selected but no Codex auth token is available.
    pub(crate) fn new(config: &ResolvedLlmConfig) -> Result<Self, LlmError> {
        let http = shared_http_client()?;

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

        let mut extra_headers = Vec::with_capacity(config.headers.len());
        for (name, value) in &config.headers {
            let parsed_name = HeaderName::from_bytes(name.as_bytes()).map_err(|error| {
                LlmError::InitFailed(format!("invalid header name '{name}': {error}"))
            })?;
            let ProviderHeaderValue::Literal(value) = value else {
                return Err(LlmError::InitFailed(format!(
                    "header '{name}' was not resolved for the request scope"
                )));
            };
            let parsed_value = HeaderValue::from_str(value).map_err(|error| {
                LlmError::InitFailed(format!("invalid value for header '{name}': {error}"))
            })?;
            extra_headers.push((parsed_name, parsed_value));
        }

        Ok(Self {
            http,
            api_key,
            model: config.model.clone(),
            reasoning_effort: config.reasoning_effort.clone(),
            base_url: config.base_url.clone(),
            provider: config.provider.clone(),
            account_id,
            is_codex,
            extra_headers,
        })
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

        for (name, value) in &self.extra_headers {
            headers.insert(name.clone(), value.clone());
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
            self.reasoning_effort.as_deref(),
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

        consume_responses_stream(response.bytes_stream(), on_delta).await
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
            self.reasoning_effort.as_deref(),
        );
        body["store"] = serde_json::Value::Bool(false);
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
            let text = response.text().await.unwrap_or_default();
            return Err(Self::structured_api_error(status, &text, retry_after));
        }

        consume_responses_stream(response.bytes_stream(), on_delta).await
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
        let mut body = build_streaming_request_body(
            &self.model,
            system,
            &messages,
            tools.as_deref().map(|arc| arc.as_slice()),
            self.reasoning_effort.as_deref(),
            should_preserve_reasoning_content(&self.provider, &self.base_url, &self.model),
        );
        request_stream_usage(&mut body, &self.base_url);

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

        consume_chat_completion_stream(response.bytes_stream(), on_delta).await
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

/// Adds `stream_options.include_usage` when the target is official OpenAI.
///
/// OpenAI omits usage from streamed responses unless it is requested. Only the
/// OpenAI-hosted API is known to accept the field, so compatible endpoints are
/// left untouched: usage stays best-effort for them rather than risking a 400.
fn request_stream_usage(body: &mut serde_json::Value, base_url: &str) {
    let is_openai = reqwest::Url::parse(base_url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_ascii_lowercase))
        .is_some_and(|host| host == "api.openai.com");
    if is_openai {
        body["stream_options"] = serde_json::json!({"include_usage": true});
    }
}

fn parse_chat_completion_usage(usage: &serde_json::Value) -> Option<LlmUsage> {
    let prompt_tokens = usage.get("prompt_tokens").and_then(|v| v.as_i64())?;
    let completion_tokens = usage.get("completion_tokens").and_then(|v| v.as_i64())?;
    let cached_input_tokens = usage
        .get("prompt_tokens_details")
        .and_then(|details| details.get("cached_tokens"))
        .and_then(|v| v.as_i64());
    Some(LlmUsage {
        input_tokens: prompt_tokens,
        output_tokens: completion_tokens,
        cached_input_tokens,
    })
}

struct ChatCompletionAccumulator {
    content: String,
    reasoning_content: Option<String>,
    tool_calls: BTreeMap<usize, StreamingToolCall>,
    usage: Option<LlmUsage>,
    has_finish: bool,
}

impl ChatCompletionAccumulator {
    fn new() -> Self {
        Self {
            content: String::new(),
            reasoning_content: None,
            tool_calls: BTreeMap::new(),
            usage: None,
            has_finish: false,
        }
    }

    fn process_chunk(&mut self, payload: &str, on_delta: &dyn Fn(String)) -> Result<(), LlmError> {
        let value = serde_json::from_str::<serde_json::Value>(payload).map_err(|error| {
            // The SSE `data:` line of a Chat Completions stream is always JSON.
            // Skipping a broken chunk would return a response with silently
            // missing text, so fail the request instead.
            LlmError::InvalidResponse(format!("malformed chat completions SSE chunk: {error}"))
        })?;
        if let Some(error) = value.get("error").filter(|error| !error.is_null()) {
            let error_type = error
                .get("metadata")
                .and_then(|metadata| metadata.get("error_type"))
                .and_then(|error_type| error_type.as_str())
                .or_else(|| error.get("error_type").and_then(|value| value.as_str()));
            return Err(stream_api_error(Some(error), error_type));
        }
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
        if let Some(finish_reason) = value
            .get("choices")
            .and_then(|choices| choices.get(0))
            .and_then(|choice| choice.get("finish_reason"))
            .and_then(|v| v.as_str())
            && !finish_reason.is_empty()
        {
            if finish_reason == "error" {
                return Err(LlmError::ApiError {
                    status: StatusCode::BAD_GATEWAY,
                    body_preview: "stream finished with finish_reason=error".to_string(),
                    retry_after_secs: None,
                });
            }
            self.has_finish = true;
        }
        if let Some(usage_value) = value.get("usage") {
            self.usage = parse_chat_completion_usage(usage_value);
        }
        Ok(())
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
                ToolCall::from_raw_arguments(
                    slot.id.unwrap_or_default(),
                    slot.name.unwrap_or_default(),
                    &slot.arguments,
                )
            })
            .collect::<Vec<_>>();
        assemble_response(self.content, self.reasoning_content, tool_calls, self.usage)
    }
}

#[derive(Default)]
struct StreamingToolCall {
    id: Option<String>,
    name: Option<String>,
    arguments: String,
}

async fn consume_chat_completion_stream(
    stream: impl futures_util::Stream<Item = Result<bytes::Bytes, reqwest::Error>> + Send + 'static,
    on_delta: &(dyn Fn(String) + Send + Sync),
) -> Result<MessagesResponse, LlmError> {
    let mut accumulator = ChatCompletionAccumulator::new();
    let mut data_stream = crate::llm::sse::data_lines(stream);
    let mut done = false;
    while let Some(result) = data_stream.next().await {
        match result {
            Ok(crate::llm::sse::SseItem::Data(payload)) => {
                accumulator.process_chunk(&payload, on_delta)?;
            }
            Ok(crate::llm::sse::SseItem::Done) => {
                done = true;
                break;
            }
            Err(e) => return Err(e),
        }
    }
    if !done && !accumulator.has_finish {
        return Err(LlmError::IncompleteStream(
            "chat completions stream ended without [DONE] or finish_reason".into(),
        ));
    }
    accumulator.finish()
}

async fn consume_responses_stream(
    stream: impl futures_util::Stream<Item = Result<bytes::Bytes, reqwest::Error>> + Send + 'static,
    on_delta: &(dyn Fn(String) + Send + Sync),
) -> Result<MessagesResponse, LlmError> {
    let mut accumulator = ResponsesAccumulator::new();
    let mut data_stream = crate::llm::sse::data_lines(stream);
    let mut completed = false;
    while let Some(result) = data_stream.next().await {
        match result {
            Ok(crate::llm::sse::SseItem::Data(payload)) => {
                if accumulator.process_event(&payload, on_delta)? {
                    completed = true;
                    break;
                }
            }
            Ok(crate::llm::sse::SseItem::Done) => {
                break;
            }
            Err(e) => return Err(e),
        }
    }
    if !completed {
        return Err(LlmError::IncompleteStream(
            "responses stream ended without completion event".into(),
        ));
    }
    accumulator.finish()
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use wiremock::matchers::{body_partial_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use crate::config::ResolvedLlmConfig;
    use crate::llm::{
        LlmProvider, LlmUsage, Message, MessageContent, MessageContentPart, ToolCall,
    };

    use bytes::Bytes;
    use futures_util::stream;

    use super::{
        OpenAiProvider, consume_chat_completion_stream, consume_responses_stream,
        parse_chat_completion_usage, request_stream_usage,
    };

    fn config(model: &str, base_url: String, api_key: Option<&str>) -> ResolvedLlmConfig {
        crate::test_util::llm_config("test", model, base_url, api_key)
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
            "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":5,\"prompt_tokens_details\":{\"cached_tokens\":8}}}\n\n",
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
                cached_input_tokens: Some(8),
            })
        );
        assert!(response.tool_calls.is_empty());
    }

    #[test]
    fn requests_stream_usage_only_from_official_openai() {
        let mut openai = serde_json::json!({});
        request_stream_usage(&mut openai, "https://api.openai.com/v1");
        assert_eq!(openai["stream_options"]["include_usage"], true);

        let mut openrouter = serde_json::json!({});
        request_stream_usage(&mut openrouter, "https://openrouter.ai/api/v1");
        assert!(openrouter.get("stream_options").is_none());

        let mut local = serde_json::json!({});
        request_stream_usage(&mut local, "http://127.0.0.1:8080/v1");
        assert!(local.get("stream_options").is_none());
    }

    #[test]
    fn parse_chat_completion_usage_requires_both_token_counts() {
        assert_eq!(
            parse_chat_completion_usage(&serde_json::json!({"prompt_tokens": 10})),
            None
        );
        assert_eq!(
            parse_chat_completion_usage(&serde_json::json!({"completion_tokens": 5})),
            None
        );
        assert_eq!(
            parse_chat_completion_usage(&serde_json::json!({
                "prompt_tokens": 10,
                "completion_tokens": 5,
            })),
            Some(LlmUsage {
                input_tokens: 10,
                output_tokens: 5,
                cached_input_tokens: None,
            })
        );
    }

    #[tokio::test]
    async fn send_message_collects_streamed_response() {
        let server = MockServer::start().await;
        let sse_body = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"long \"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"answer\"}}]}\n\n",
            "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":7,\"completion_tokens\":2}}\n\n",
            "data: [DONE]\n\n",
        );
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .and(body_partial_json(serde_json::json!({ "stream": true })))
            .respond_with(ResponseTemplate::new(200).set_body_raw(sse_body, "text/event-stream"))
            .mount(&server)
            .await;

        let provider = OpenAiProvider::new(&config(
            "gpt-4o-mini",
            format!("{}/v1", server.uri()),
            Some("sk-test"),
        ))
        .expect("provider");

        let response = provider
            .send_message("", message("hello"), None)
            .await
            .expect("response");

        assert_eq!(response.content, "long answer");
        assert_eq!(
            response.usage,
            Some(LlmUsage {
                input_tokens: 7,
                output_tokens: 2,
                cached_input_tokens: None,
            })
        );
    }

    #[tokio::test]
    async fn fails_when_the_stream_reports_an_error_after_partial_content() {
        let server = MockServer::start().await;
        let sse_body = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"partial answer\"}}]}\n\n",
            "data: {\"error\":{\"code\":\"server_error\",\"message\":\"Provider disconnected unexpectedly\"},\"choices\":[{\"delta\":{\"content\":\"\"},\"finish_reason\":\"error\"}]}\n\n",
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

        let error = provider
            .send_message("", message("hello"), None)
            .await
            .expect_err("stream error");

        assert!(error.is_retryable(), "{error}");
        assert!(
            error
                .to_string()
                .contains("Provider disconnected unexpectedly"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn fails_when_finish_reason_is_error_without_an_error_object() {
        let server = MockServer::start().await;
        let sse_body = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"partial answer\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"\"},\"finish_reason\":\"error\"}]}\n\n",
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

        let error = provider
            .send_message("", message("hello"), None)
            .await
            .expect_err("stream error");

        assert!(error.is_retryable(), "{error}");
        assert!(error.to_string().contains("finish_reason=error"), "{error}");
    }

    #[tokio::test]
    async fn fails_when_a_chat_stream_error_reports_a_deterministic_cause() {
        let server = MockServer::start().await;
        let sse_body = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"partial answer\"}}]}\n\n",
            "data: {\"error\":{\"code\":\"server_error\",\"message\":\"bad input\",\"metadata\":{\"error_type\":\"invalid_request\"}},\"choices\":[{\"delta\":{\"content\":\"\"},\"finish_reason\":\"error\"}]}\n\n",
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

        let error = provider
            .send_message("", message("hello"), None)
            .await
            .expect_err("stream error");

        assert!(!error.is_retryable(), "{error}");
        assert!(error.to_string().contains("400"), "{error}");
    }

    #[tokio::test]
    async fn chat_429_uses_one_http_attempt_per_shared_retry() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "0"))
            .mount(&server)
            .await;

        let provider = OpenAiProvider::new(&config(
            "gpt-4o-mini",
            format!("{}/v1", server.uri()),
            Some("sk-test"),
        ))
        .expect("provider");
        let policy = crate::llm::RetryPolicy::new(
            tokio::time::Instant::now() + std::time::Duration::from_secs(60),
        );

        let failure = crate::llm::complete_response(
            policy,
            || provider.send_message("", message("hello"), None),
            |_, _, _| {},
        )
        .await
        .expect_err("rate limited");

        assert!(failure.error.expect("provider error").is_retryable());
        let requests = server.received_requests().await.expect("requests");
        assert_eq!(requests.len(), crate::llm::MAX_ATTEMPTS);
    }

    #[tokio::test]
    async fn responses_429_uses_one_http_attempt_per_shared_retry() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/responses"))
            .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "0"))
            .mount(&server)
            .await;

        let provider = OpenAiProvider::new(&config(
            "gpt-4o-mini",
            format!("{}/v1", server.uri()),
            Some("sk-test"),
        ))
        .expect("provider");
        let policy = crate::llm::RetryPolicy::new(
            tokio::time::Instant::now() + std::time::Duration::from_secs(60),
        );

        let failure = crate::llm::complete_response(
            policy,
            || provider.send_message("", multimodal_message(), None),
            |_, _, _| {},
        )
        .await
        .expect_err("rate limited");

        assert!(failure.error.expect("provider error").is_retryable());
        let requests = server.received_requests().await.expect("requests");
        assert_eq!(requests.len(), crate::llm::MAX_ATTEMPTS);
    }

    #[tokio::test]
    async fn codex_429_is_not_retried_inside_the_provider() {
        let codex_dir = tempfile::tempdir().expect("tempdir");
        let _env =
            crate::test_env::EnvVarGuard::set("OPENAI_CODEX_ACCESS_TOKEN", "test-codex-token")
                .also_set("CODEX_HOME", codex_dir.path());
        crate::llm::codex_auth::clear_auth_cache();

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/responses"))
            .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "0"))
            .mount(&server)
            .await;

        let provider = OpenAiProvider::new(&crate::test_util::llm_config(
            "openai-codex",
            "gpt-5.3-codex",
            format!("{}/v1", server.uri()),
            None,
        ))
        .expect("provider");
        let policy = crate::llm::RetryPolicy::new(
            tokio::time::Instant::now() + std::time::Duration::from_secs(60),
        );

        let failure = crate::llm::complete_response(
            policy,
            || provider.send_message("", message("hello"), None),
            |_, _, _| {},
        )
        .await
        .expect_err("rate limited");

        assert!(failure.error.expect("provider error").is_retryable());
        let requests = server.received_requests().await.expect("requests");
        assert_eq!(requests.len(), crate::llm::MAX_ATTEMPTS);
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
                    invalid_arguments: None,
                },
                ToolCall {
                    id: "call_b".to_string(),
                    name: "grep".to_string(),
                    arguments: serde_json::json!({"pattern": "foo"}),
                    invalid_arguments: None,
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
                invalid_arguments: None,
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
                cached_input_tokens: None,
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
            .and(body_partial_json(serde_json::json!({
                "reasoning": {"effort": "high"},
                "stream": true,
                "store": false
            })))
            .respond_with(ResponseTemplate::new(200).set_body_raw(sse_body, "text/event-stream"))
            .mount(&server)
            .await;

        let mut codex_config = crate::test_util::llm_config(
            "openai-codex",
            "gpt-5.3-codex",
            format!("{}/v1", server.uri()),
            None,
        );
        codex_config.reasoning_effort = Some("high".to_string());
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
                cached_input_tokens: None,
            })
        );
    }

    #[tokio::test]
    async fn consume_chat_completion_stream_propagates_chunk_error() {
        // Arrange: create a real reqwest::Error by connecting to a closed port.
        let stream_error = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_millis(500))
            .build()
            .expect("client")
            .get("http://127.0.0.1:1/")
            .send()
            .await
            .expect_err("connection refused");

        let chunks: Vec<Result<Bytes, reqwest::Error>> = vec![
            Ok(Bytes::from(
                "data: {\"choices\":[{\"delta\":{\"content\":\"途中まで\"}}]}\n\n",
            )),
            Err(stream_error),
        ];

        // Act
        let result = consume_chat_completion_stream(stream::iter(chunks), &|_| {}).await;

        // Assert
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn consume_chat_completion_stream_errors_on_eof_without_done_or_finish() {
        // Arrange: stream that ends with a content delta but no [DONE] or
        // finish_reason.
        let chunks: Vec<Result<Bytes, reqwest::Error>> = vec![Ok(Bytes::from_static(
            b"data: {\"choices\":[{\"delta\":{\"content\":\"partial\"}}]}\n\n",
        ))];

        // Act
        let result = consume_chat_completion_stream(stream::iter(chunks), &|_| {}).await;

        // Assert: a truncated stream is retryable, unlike a malformed response.
        let error = result.expect_err("incomplete stream");
        assert!(error.is_retryable(), "{error}");
        assert!(matches!(error, crate::error::LlmError::IncompleteStream(_)));
        assert!(
            error
                .to_string()
                .contains("without [DONE] or finish_reason")
        );
    }

    #[tokio::test]
    async fn consume_chat_completion_stream_fails_on_malformed_chunk() {
        // Arrange: a broken chunk sits between valid content and [DONE].
        let chunks: Vec<Result<Bytes, reqwest::Error>> = vec![
            Ok(Bytes::from_static(
                b"data: {\"choices\":[{\"delta\":{\"content\":\"kept\"}}]}\n\n",
            )),
            Ok(Bytes::from_static(b"data: {\"choices\":[\n\n")),
            Ok(Bytes::from_static(b"data: [DONE]\n\n")),
        ];

        // Act
        let result = consume_chat_completion_stream(stream::iter(chunks), &|_| {}).await;

        // Assert: never return the partially assembled text as a response.
        let error = result.expect_err("malformed chunk");
        assert!(!error.is_retryable(), "{error}");
        assert!(
            error
                .to_string()
                .contains("malformed chat completions SSE chunk"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn consume_responses_stream_errors_on_eof_without_completion() {
        // Arrange: stream with a text delta but no response.completed event.
        let chunks: Vec<Result<Bytes, reqwest::Error>> = vec![Ok(Bytes::from_static(
            b"data: {\"type\":\"response.output_text.delta\",\"delta\":\"partial\"}\n\n",
        ))];

        // Act
        let result = consume_responses_stream(stream::iter(chunks), &|_| {}).await;

        // Assert: a stream that stops before completion is retryable.
        let error = result.expect_err("incomplete stream");
        assert!(error.is_retryable(), "{error}");
        assert!(matches!(error, crate::error::LlmError::IncompleteStream(_)));
        assert!(error.to_string().contains("without completion event"));
    }

    #[tokio::test]
    async fn consume_responses_stream_fails_on_malformed_event() {
        // Arrange: a broken event sits between a delta and completion.
        let chunks: Vec<Result<Bytes, reqwest::Error>> = vec![
            Ok(Bytes::from_static(
                b"data: {\"type\":\"response.output_text.delta\",\"delta\":\"kept\"}\n\n",
            )),
            Ok(Bytes::from_static(
                b"data: {\"type\":\"response.output_text.delta\",\n\n",
            )),
            Ok(Bytes::from_static(
                b"data: {\"type\":\"response.completed\",\"response\":{\"output\":[],\"usage\":null}}\n\n",
            )),
        ];

        // Act
        let result = consume_responses_stream(stream::iter(chunks), &|_| {}).await;

        // Assert: the malformed event fails the request instead of being skipped.
        let error = result.expect_err("malformed event");
        assert!(!error.is_retryable(), "{error}");
        assert!(
            error.to_string().contains("malformed responses SSE event"),
            "{error}"
        );
    }
}
