use super::*;

use reqwest::StatusCode;

/// Assembles the final `MessagesResponse` from accumulated Chat Completions
/// fields, applying raw tool-use rescue when structured tool calls are absent.
pub(super) fn assemble_response(
    content: String,
    reasoning_content: Option<String>,
    tool_calls: Vec<ToolCall>,
    usage: Option<LlmUsage>,
) -> Result<MessagesResponse, LlmError> {
    let mut content = content.trim().to_string();
    let mut tool_calls = tool_calls;
    if tool_calls.is_empty()
        && let Some((rescued_calls, stripped_content)) = rescue_raw_tool_calls(&content)
    {
        tool_calls = rescued_calls;
        content = stripped_content;
    }

    if content.is_empty() && tool_calls.is_empty() {
        return Err(LlmError::InvalidResponse(
            "assistant content was empty".to_string(),
        ));
    }

    Ok(MessagesResponse {
        content,
        reasoning_content,
        tool_calls,
        usage,
    })
}

pub(crate) fn parse_responses_response(
    body: ResponsesApiResponse,
) -> Result<MessagesResponse, LlmError> {
    ensure_response_completed(&body)?;
    let diagnostics = body.diagnostics();
    let mut content_parts = Vec::new();
    let mut tool_calls = Vec::new();

    for item in body.output {
        match item {
            ResponsesOutputItem::Message { role, content } if role.as_deref() != Some("user") => {
                content_parts.extend(
                    content
                        .into_iter()
                        .filter_map(extract_response_content_part),
                );
            }
            ResponsesOutputItem::FunctionCall {
                call_id,
                id,
                name,
                arguments,
            } => {
                let call_id = call_id.or(id).unwrap_or_default();
                tool_calls.push(ToolCall::from_raw_arguments(call_id, name, &arguments));
            }
            ResponsesOutputItem::Ignored | ResponsesOutputItem::Message { .. } => {}
        }
    }

    let mut content = content_parts.join("\n").trim().to_string();

    if tool_calls.is_empty()
        && let Some((rescued_calls, stripped_content)) = rescue_raw_tool_calls(&content)
    {
        tool_calls = rescued_calls;
        content = stripped_content;
    }

    if content.is_empty() && tool_calls.is_empty() {
        return Err(LlmError::InvalidResponse(format!(
            "assistant content was empty ({diagnostics})"
        )));
    }

    Ok(MessagesResponse {
        content,
        reasoning_content: None,
        tool_calls,
        usage: body.usage.and_then(|u| {
            let cached_input_tokens = u.input_tokens_details.and_then(|d| d.cached_tokens);
            u.input_tokens
                .zip(u.output_tokens)
                .map(|(it, ot)| LlmUsage {
                    input_tokens: it,
                    output_tokens: ot,
                    cached_input_tokens,
                })
        }),
    })
}

impl ResponsesApiResponse {
    fn diagnostics(&self) -> String {
        let output_items = self.output.len();
        let status = self.status.as_deref().unwrap_or("unknown");
        let incomplete_reason = self
            .incomplete_details
            .as_ref()
            .and_then(|details| details.reason.as_deref())
            .unwrap_or("none");

        let input_tokens = self
            .usage
            .as_ref()
            .and_then(|usage| usage.input_tokens)
            .map_or_else(|| "unknown".to_string(), |tokens| tokens.to_string());
        let output_tokens = self
            .usage
            .as_ref()
            .and_then(|usage| usage.output_tokens)
            .map_or_else(|| "unknown".to_string(), |tokens| tokens.to_string());
        let reasoning_tokens = self
            .usage
            .as_ref()
            .and_then(|usage| usage.output_tokens_details.as_ref())
            .and_then(|details| details.reasoning_tokens)
            .map_or_else(|| "unknown".to_string(), |tokens| tokens.to_string());

        format!(
            "status={status}, output_items={output_items}, input_tokens={input_tokens}, output_tokens={output_tokens}, reasoning_tokens={reasoning_tokens}, incomplete_reason={incomplete_reason}"
        )
    }
}

/// Rejects terminal responses that did not complete successfully.
///
/// `response.done` is used by some providers instead of `response.completed`,
/// and its `status` is authoritative: a failed response can still carry output
/// items (including tool calls), so the status must be checked before the body
/// is treated as a usable answer. A missing status keeps the previous
/// behaviour for providers that omit it.
fn ensure_response_completed(response: &ResponsesApiResponse) -> Result<(), LlmError> {
    match response.status.as_deref() {
        None | Some("completed") => Ok(()),
        Some("incomplete") => Err(incomplete_response_error(response)),
        Some("failed") => Err(stream_api_error(
            response.error.as_ref(),
            response.error_type.as_deref(),
        )),
        Some(status) => Err(LlmError::InvalidResponse(format!(
            "unexpected responses terminal status: {status} ({})",
            response.diagnostics()
        ))),
    }
}

/// Builds the non-retryable error for a generation cut off at a terminal
/// boundary (output limit, content filter, or another incomplete reason).
fn incomplete_response_error(response: &ResponsesApiResponse) -> LlmError {
    LlmError::IncompleteResponse(format!(
        "responses generation was incomplete ({})",
        response.diagnostics()
    ))
}

fn extract_response_content_part(part: ResponsesOutputPart) -> Option<String> {
    match part {
        ResponsesOutputPart::OutputText { text } => Some(text),
        ResponsesOutputPart::Refusal { refusal } => Some(refusal),
        ResponsesOutputPart::Ignored => None,
    }
}

fn parse_rescued_tool_arguments(input_json: &str) -> Option<serde_json::Value> {
    if input_json.trim().is_empty() {
        return Some(serde_json::json!({}));
    }

    serde_json::from_str(input_json).ok()
}

fn rescue_raw_tool_calls(content: &str) -> Option<(Vec<ToolCall>, String)> {
    let raw_calls = extract_raw_tool_use_blocks(content)?;
    let tool_calls = raw_calls
        .into_iter()
        .map(|raw| {
            Some(ToolCall {
                id: raw.id,
                name: raw.name,
                arguments: parse_rescued_tool_arguments(&raw.input_json)?,
                invalid_arguments: None,
            })
        })
        .collect::<Option<Vec<_>>>()?;
    Some((tool_calls, strip_raw_tool_use_text(content)))
}

pub(crate) fn should_use_responses_api(messages: &[Message]) -> bool {
    messages
        .iter()
        .any(|message| message.content.is_multimodal())
}

/// Accumulates Responses API SSE events into a final [`ResponsesApiResponse`].
///
/// Shared by the streaming Responses and Codex paths in `send_message_streaming`
/// to keep delta extraction and final response assembly consistent.
pub(super) struct ResponsesAccumulator {
    streamed_text: String,
    streamed_items: Vec<ResponsesOutputItem>,
    completed_response: Option<ResponsesApiResponse>,
}

impl ResponsesAccumulator {
    pub(super) fn new() -> Self {
        Self {
            streamed_text: String::with_capacity(8192),
            streamed_items: Vec::with_capacity(32),
            completed_response: None,
        }
    }

    /// Processes one SSE `data:` payload (JSON text without the `data:` prefix).
    ///
    /// Calls `on_delta` for each `response.output_text.delta` event. Returns
    /// `Ok(true)` when a terminal event (`response.completed` / `response.done`)
    /// carrying a valid `response` object has been consumed. Returns an error
    /// for explicit failure events (`response.failed` / `error`) so a partially
    /// streamed generation is never mistaken for a complete response.
    pub(super) fn process_event(
        &mut self,
        payload: &str,
        on_delta: &dyn Fn(String),
    ) -> Result<bool, LlmError> {
        let mut value = serde_json::from_str::<serde_json::Value>(payload).map_err(|error| {
            // A broken event can drop output-text deltas while a later
            // completion event still makes the partial text look complete.
            LlmError::InvalidResponse(format!("malformed responses SSE event: {error}"))
        })?;

        let event_type = value
            .get("type")
            .and_then(|v| v.as_str())
            .map(str::to_string);

        match event_type.as_deref() {
            Some("response.failed") => {
                let response = value.get("response");
                let error = response.and_then(|response| response.get("error"));
                let error_type = response
                    .and_then(|response| response.get("error_type"))
                    .and_then(|error_type| error_type.as_str());
                return Err(stream_api_error(error, error_type));
            }
            Some("error") => {
                let error = value.get("error").unwrap_or(&value);
                let error_type = error
                    .get("error_type")
                    .and_then(|error_type| error_type.as_str())
                    .or_else(|| value.get("error_type").and_then(|value| value.as_str()));
                return Err(stream_api_error(Some(error), error_type));
            }
            Some("response.incomplete") => {
                let response = value.get("response").and_then(|response| {
                    serde_json::from_value::<ResponsesApiResponse>(response.clone()).ok()
                });
                return Err(response.map_or_else(
                    || LlmError::IncompleteResponse("response.incomplete".to_string()),
                    |response| incomplete_response_error(&response),
                ));
            }
            _ => {}
        }

        // Only the `done` item is complete; `added` carries an in_progress
        // partial item that would duplicate the finalized one.
        if event_type.as_deref() == Some("response.output_item.done")
            && let Some(item_value) = value.get_mut("item")
            && let Ok(item) = serde_json::from_value::<ResponsesOutputItem>(item_value.take())
            && !matches!(item, ResponsesOutputItem::Ignored)
        {
            self.streamed_items.push(item);
        }

        match event_type.as_deref() {
            Some("response.output_text.delta") => {
                if let Some(delta) = value.get("delta").and_then(|v| v.as_str()) {
                    on_delta(delta.to_string());
                    self.streamed_text.push_str(delta);
                }
            }
            Some("response.output_text.done") => {
                if let Some(done_text) = value.get("text").and_then(|v| v.as_str())
                    && !done_text.is_empty()
                {
                    self.streamed_text.clear();
                    self.streamed_text.push_str(done_text);
                }
            }
            _ => {}
        }

        let is_terminal = matches!(
            event_type.as_deref(),
            Some("response.completed" | "response.done")
        );
        if is_terminal && let Some(response_value) = value.get_mut("response") {
            let parsed = serde_json::from_value::<ResponsesApiResponse>(response_value.take())
                .map_err(|error| {
                    LlmError::InvalidResponse(format!(
                        "malformed responses terminal event: {error}"
                    ))
                })?;
            ensure_response_completed(&parsed)?;
            self.completed_response = Some(parsed);
            return Ok(true);
        }

        Ok(false)
    }

    /// Assembles the final [`ResponsesApiResponse`] from accumulated state.
    ///
    /// Prefers the `response` object from a terminal event. Falls back to
    /// streamed text/items when the terminal response had empty output.
    ///
    /// # Errors
    ///
    /// Returns [`LlmError::IncompleteStream`] when no completion event
    /// (`response.completed` / `response.done`) was received; partial streamed
    /// data is rejected rather than synthesized into a success. The caller may
    /// retry because the generation was cut off, not because it was invalid.
    pub(super) fn into_api_response(self) -> Result<ResponsesApiResponse, LlmError> {
        let Some(mut response) = self.completed_response else {
            return Err(LlmError::IncompleteStream(
                "Responses API stream ended without a completion event".to_string(),
            ));
        };
        if response.output.is_empty() {
            if !self.streamed_items.is_empty() {
                response.output = self.streamed_items;
            } else if !self.streamed_text.trim().is_empty() {
                response.output.push(ResponsesOutputItem::Message {
                    role: Some("assistant".to_string()),
                    content: vec![ResponsesOutputPart::OutputText {
                        text: self.streamed_text,
                    }],
                });
            }
        }
        Ok(response)
    }

    /// Convenience: assembles and parses the final response in one call.
    ///
    /// # Errors
    ///
    /// See [`Self::into_api_response`] and [`parse_responses_response`].
    pub(super) fn finish(self) -> Result<MessagesResponse, LlmError> {
        parse_responses_response(self.into_api_response()?)
    }
}

pub(crate) fn preview_body(body: &str) -> String {
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

/// Builds an [`LlmError`] for a failure reported inside a streamed response.
///
/// The provider has already answered with HTTP 200 when the failure arrives, so
/// there is no status line to classify. A typed `error_type` is preferred over
/// the native `code` because the code collapses distinct causes (OpenRouter
/// reports authentication and overload failures as the same Responses
/// `server_error`). Both are mapped to their HTTP equivalent so the shared
/// retry policy can tell transient failures from deterministic ones; unknown
/// failures default to 502 because they usually mean the generation was cut off
/// upstream.
pub(super) fn stream_api_error(
    error: Option<&serde_json::Value>,
    error_type: Option<&str>,
) -> LlmError {
    let message = error
        .and_then(|value| {
            value
                .get("message")
                .and_then(|message| message.as_str())
                .or_else(|| value.as_str())
        })
        .unwrap_or("stream failed without an error message");
    let code = error
        .and_then(|value| value.get("code"))
        .and_then(|value| value.as_str());
    let status = stream_error_status(error_type)
        .or_else(|| {
            error
                .and_then(|value| value.get("code"))
                .and_then(stream_error_code)
        })
        .unwrap_or(StatusCode::BAD_GATEWAY);
    crate::llm::api_error::classify(status, code, error_type, message, None)
}

/// Maps OpenRouter's typed error vocabulary to the HTTP status it stands for.
fn stream_error_status(error_type: Option<&str>) -> Option<StatusCode> {
    Some(match error_type? {
        "authentication" => StatusCode::UNAUTHORIZED,
        "permission_denied" | "content_policy_violation" | "refusal" => StatusCode::FORBIDDEN,
        "payment_required" => StatusCode::PAYMENT_REQUIRED,
        "rate_limit_exceeded" => StatusCode::TOO_MANY_REQUESTS,
        "provider_overloaded" => StatusCode::SERVICE_UNAVAILABLE,
        "provider_unavailable" => StatusCode::BAD_GATEWAY,
        "server" | "unmapped" => StatusCode::INTERNAL_SERVER_ERROR,
        "timeout" => StatusCode::GATEWAY_TIMEOUT,
        "not_found" | "image_not_found" => StatusCode::NOT_FOUND,
        "precondition_failed" => StatusCode::PRECONDITION_FAILED,
        "payload_too_large" => StatusCode::PAYLOAD_TOO_LARGE,
        "unprocessable" => StatusCode::UNPROCESSABLE_ENTITY,
        "context_length_exceeded"
        | "max_tokens_exceeded"
        | "token_limit_exceeded"
        | "string_too_long"
        | "invalid_request"
        | "invalid_prompt"
        | "invalid_image"
        | "image_too_large"
        | "image_too_small"
        | "unsupported_image_format"
        | "image_download_failed" => StatusCode::BAD_REQUEST,
        _ => return None,
    })
}

/// Maps native error codes to the HTTP status they stand for.
fn stream_error_code(code: &serde_json::Value) -> Option<StatusCode> {
    if let Some(code) = code
        .as_u64()
        .and_then(|code| u16::try_from(code).ok())
        .and_then(|code| StatusCode::from_u16(code).ok())
    {
        return Some(code);
    }
    match code.as_str()? {
        "rate_limit_exceeded" | "rate_limit_error" => Some(StatusCode::TOO_MANY_REQUESTS),
        "insufficient_quota" => Some(StatusCode::PAYMENT_REQUIRED),
        "overloaded_error" => Some(StatusCode::SERVICE_UNAVAILABLE),
        "invalid_request_error" | "context_length_exceeded" | "content_filter" => {
            Some(StatusCode::BAD_REQUEST)
        }
        "authentication_error" | "invalid_api_key" => Some(StatusCode::UNAUTHORIZED),
        "permission_error" => Some(StatusCode::FORBIDDEN),
        "not_found_error" | "model_not_found" => Some(StatusCode::NOT_FOUND),
        _ => None,
    }
}

// Raw tool-use rescue: some LLM providers emit tool calls as raw text
// `[tool_use: name(args)]` instead of structured `tool_calls` fields.

pub(crate) struct RawToolUseBlock {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) input_json: String,
}

pub(crate) fn strip_minimax_tool_wrappers(text: &str) -> String {
    use std::sync::LazyLock;
    static RE: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r"</?(?:minimax:tool_call|invoke|parameter)>")
            .expect("MiniMax tool wrapper regex must compile")
    });
    RE.replace_all(text, " ").into_owned()
}

pub(crate) fn parse_raw_tool_use_block(
    input: &str,
    call_number: usize,
) -> Option<(RawToolUseBlock, &str)> {
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

pub(crate) fn extract_raw_tool_use_blocks(text: &str) -> Option<Vec<RawToolUseBlock>> {
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
pub(crate) fn strip_raw_tool_use_text(text: &str) -> String {
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
pub(crate) fn find_raw_tool_use_end(text: &str) -> Option<usize> {
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
pub(crate) struct ResponsesApiResponse {
    #[serde(default)]
    pub(crate) status: Option<String>,
    pub(crate) output: Vec<ResponsesOutputItem>,
    #[serde(default)]
    pub(crate) usage: Option<ResponsesApiUsage>,
    #[serde(default)]
    pub(crate) incomplete_details: Option<ResponsesIncompleteDetails>,
    #[serde(default)]
    pub(crate) error: Option<serde_json::Value>,
    #[serde(default)]
    pub(crate) error_type: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ResponsesApiUsage {
    #[serde(default)]
    pub(crate) input_tokens: Option<i64>,
    #[serde(default)]
    pub(crate) output_tokens: Option<i64>,
    #[serde(default)]
    pub(crate) input_tokens_details: Option<ResponsesInputTokensDetails>,
    #[serde(default)]
    pub(crate) output_tokens_details: Option<ResponsesOutputTokensDetails>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ResponsesInputTokensDetails {
    #[serde(default)]
    pub(crate) cached_tokens: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ResponsesOutputTokensDetails {
    #[serde(default)]
    pub(crate) reasoning_tokens: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ResponsesIncompleteDetails {
    #[serde(default)]
    pub(crate) reason: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
pub(crate) enum ResponsesOutputItem {
    #[serde(rename = "message")]
    Message {
        #[serde(default)]
        role: Option<String>,
        content: Vec<ResponsesOutputPart>,
    },
    #[serde(rename = "function_call")]
    FunctionCall {
        #[serde(default)]
        call_id: Option<String>,
        #[serde(default)]
        id: Option<String>,
        name: String,
        arguments: String,
    },
    #[serde(other)]
    Ignored,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
pub(crate) enum ResponsesOutputPart {
    #[serde(rename = "output_text")]
    OutputText { text: String },
    #[serde(rename = "refusal")]
    Refusal { refusal: String },
    #[serde(other)]
    Ignored,
}

#[derive(Debug, Deserialize)]
pub(crate) struct OaiErrorResponse {
    pub(crate) error: OaiErrorDetail,
}

#[derive(Debug, Deserialize)]
pub(crate) struct OaiErrorDetail {
    pub(crate) message: String,
    /// Stable provider code for the failure, when the provider sends one.
    #[serde(default)]
    pub(crate) code: Option<String>,
    /// Coarse provider error type, used when `code` is absent.
    #[serde(default, rename = "type")]
    pub(crate) error_type: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Feeds `data:` lines from a Codex SSE body through the same accumulator
    /// the streaming path uses, so the tests exercise production parsing.
    fn accumulate(sse: &str) -> Result<MessagesResponse, LlmError> {
        let mut accumulator = ResponsesAccumulator::new();
        for line in sse.lines() {
            let Some(crate::llm::sse::SseItem::Data(payload)) =
                crate::llm::sse::classify_line(line.trim())
            else {
                continue;
            };
            if accumulator.process_event(&payload, &|_| {})? {
                break;
            }
        }
        accumulator.finish()
    }

    #[test]
    fn parses_completed_response_sse_payload() {
        let payload = r#"
event: response.created
data: {"type":"response.created","response":{"output":[]}}

event: response.completed
data: {"type":"response.completed","response":{"output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"ok"}]}],"usage":{"input_tokens":1,"output_tokens":2,"input_tokens_details":{"cached_tokens":1}}}}
"#;

        let response = accumulate(payload).expect("response");

        assert_eq!(response.content, "ok");
        assert_eq!(
            response.usage,
            Some(LlmUsage {
                input_tokens: 1,
                output_tokens: 2,
                cached_input_tokens: Some(1),
            })
        );
    }

    #[test]
    fn uses_streamed_text_when_completed_output_is_empty() {
        let payload = r#"
event: response.output_text.delta
data: {"type":"response.output_text.delta","delta":"hello "}

event: response.output_text.delta
data: {"type":"response.output_text.delta","delta":"there"}

event: response.completed
data: {"type":"response.completed","response":{"status":"completed","output":[],"usage":{"input_tokens":1,"output_tokens":2,"output_tokens_details":{"reasoning_tokens":0}}}}
"#;

        let response = accumulate(payload).expect("response");

        assert_eq!(response.content, "hello there");
    }

    #[test]
    fn uses_streamed_output_item_when_completed_output_is_empty() {
        let payload = r#"
event: response.output_item.done
data: {"type":"response.output_item.done","item":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"from item"}]}}

event: response.completed
data: {"type":"response.completed","response":{"status":"completed","output":[],"usage":{"input_tokens":1,"output_tokens":2}}}
"#;

        let response = accumulate(payload).expect("response");

        assert_eq!(response.content, "from item");
    }

    #[test]
    fn empty_responses_api_output_reports_diagnostics() {
        let body: ResponsesApiResponse = serde_json::from_value(serde_json::json!({
            "status": "completed",
            "output": [],
            "incomplete_details": null,
            "usage": {
                "input_tokens": 123,
                "output_tokens": 20,
                "output_tokens_details": {
                    "reasoning_tokens": 20
                }
            }
        }))
        .expect("body");

        let error = parse_responses_response(body).expect_err("empty output");
        let text = error.to_string();

        assert!(text.contains("status=completed"), "{text}");
        assert!(text.contains("output_items=0"), "{text}");
        assert!(text.contains("output_tokens=20"), "{text}");
        assert!(text.contains("reasoning_tokens=20"), "{text}");
    }

    #[test]
    fn dedupes_output_item_across_added_and_done_events() {
        let payload = r#"
event: response.output_item.added
data: {"type":"response.output_item.added","item":{"type":"function_call","call_id":"call_1","name":"get_weather","arguments":"{}"}}

event: response.output_item.done
data: {"type":"response.output_item.done","item":{"type":"function_call","call_id":"call_1","name":"get_weather","arguments":"{}"}}

event: response.completed
data: {"type":"response.completed","response":{"status":"completed","output":[],"usage":{"input_tokens":1,"output_tokens":2}}}
"#;

        let response = accumulate(payload).expect("response");

        assert_eq!(response.tool_calls.len(), 1);
        assert_eq!(response.tool_calls[0].name, "get_weather");
    }

    #[test]
    fn rejects_stream_without_completion_event() {
        // `response.created` carries a `response` object but is not terminal;
        // the stream must still be rejected without `response.completed`.
        let payload = r#"
event: response.created
data: {"type":"response.created","response":{"output":[]}}

event: response.output_text.delta
data: {"type":"response.output_text.delta","delta":"partial"}

event: response.output_item.done
data: {"type":"response.output_item.done","item":{"type":"function_call","call_id":"call_1","name":"get_weather","arguments":"{}"}}
"#;

        let error = accumulate(payload).expect_err("incomplete payload");

        let text = error.to_string();
        assert!(
            text.contains("completion event"),
            "expected completion-event error, got: {text}"
        );
    }

    #[test]
    fn fails_on_response_failed_after_partial_text() {
        let payload = r#"
event: response.output_text.delta
data: {"type":"response.output_text.delta","delta":"partial"}

event: response.failed
data: {"type":"response.failed","response":{"status":"failed","error":{"code":"server_error","message":"upstream disconnected"}}}
"#;

        let error = accumulate(payload).expect_err("failed event");

        assert!(error.is_retryable());
        assert!(
            error.to_string().contains("upstream disconnected"),
            "{error}"
        );
    }

    #[test]
    fn fails_on_error_event_with_top_level_fields() {
        let payload = r#"
event: response.output_text.delta
data: {"type":"response.output_text.delta","delta":"partial"}

event: error
data: {"type":"error","code":"rate_limit_exceeded","message":"slow down"}
"#;

        let error = accumulate(payload).expect_err("error event");

        assert!(error.is_retryable());
        assert!(error.to_string().contains("slow down"), "{error}");
        assert!(error.to_string().contains("429"), "{error}");
    }

    #[test]
    fn typed_authentication_failure_wins_over_a_generic_server_code() {
        // OpenRouter collapses authentication failures to `server_error` in the
        // native code; retrying that as a transient 5xx would never recover.
        let payload = r#"
event: response.failed
data: {"type":"response.failed","response":{"status":"failed","error":{"code":"server_error","message":"Invalid credentials"},"error_type":"authentication"}}
"#;

        let error = accumulate(payload).expect_err("failed event");

        assert!(!error.is_retryable(), "{error}");
        assert!(error.to_string().contains("401"), "{error}");
    }

    #[test]
    fn typed_overload_failure_stays_retryable() {
        let payload = r#"
event: response.failed
data: {"type":"response.failed","response":{"status":"failed","error":{"code":"server_error","message":"overloaded"},"error_type":"provider_overloaded"}}
"#;

        let error = accumulate(payload).expect_err("failed event");

        assert!(error.is_retryable(), "{error}");
        assert!(error.to_string().contains("503"), "{error}");
    }

    #[test]
    fn fails_on_incomplete_response_event() {
        // `response.incomplete` is a terminal event: the generation hit the
        // output limit or was filtered, so retrying the same request will not
        // complete it.
        let payload = r#"
event: response.output_text.delta
data: {"type":"response.output_text.delta","delta":"partial"}

event: response.incomplete
data: {"type":"response.incomplete","response":{"status":"incomplete","incomplete_details":{"reason":"max_output_tokens"},"output":[]}}
"#;

        let error = accumulate(payload).expect_err("incomplete response");

        assert!(!error.is_retryable(), "{error}");
        assert!(matches!(
            error,
            crate::error::LlmError::IncompleteResponse(_)
        ));
        assert!(error.to_string().contains("max_output_tokens"), "{error}");
    }

    #[test]
    fn fails_on_done_event_with_incomplete_status() {
        // Some compatible providers end with `response.done` instead of
        // `response.incomplete`; the response status is authoritative.
        let payload = r#"
event: response.output_text.delta
data: {"type":"response.output_text.delta","delta":"partial"}

event: response.done
data: {"type":"response.done","response":{"status":"incomplete","incomplete_details":{"reason":"content_filter"},"output":[]}}
"#;

        let error = accumulate(payload).expect_err("incomplete status");

        assert!(!error.is_retryable(), "{error}");
        assert!(error.to_string().contains("content_filter"), "{error}");
    }

    #[test]
    fn failed_done_response_is_never_returned_as_tool_calls() {
        // A failed terminal response can still contain output items. It must
        // not reach the tool executor as a valid tool-call request.
        let payload = r#"
event: response.done
data: {"type":"response.done","response":{"status":"failed","error":{"code":"server_error","message":"Invalid credentials"},"error_type":"authentication","output":[{"type":"function_call","call_id":"call_1","name":"delete_everything","arguments":"{}"}]}}
"#;

        let error = accumulate(payload).expect_err("failed status");

        assert!(!error.is_retryable(), "{error}");
        assert!(error.to_string().contains("Invalid credentials"), "{error}");
    }

    #[test]
    fn parse_rejects_an_incomplete_response() {
        // The parser itself must not turn a terminal-but-incomplete body into
        // a successful response.
        let body: ResponsesApiResponse = serde_json::from_value(serde_json::json!({
            "status": "incomplete",
            "incomplete_details": {"reason": "max_output_tokens"},
            "output": [{"type":"message","role":"assistant","content":[{"type":"output_text","text":"partial"}]}]
        }))
        .expect("body");

        let error = parse_responses_response(body).expect_err("incomplete response");

        assert!(!error.is_retryable(), "{error}");
        assert!(error.to_string().contains("max_output_tokens"), "{error}");
    }

    #[test]
    fn parse_rejects_a_failed_response_with_tool_calls() {
        let body: ResponsesApiResponse = serde_json::from_value(serde_json::json!({
            "status": "failed",
            "error": {"code": "server_error", "message": "upstream failed"},
            "output": [{"type":"function_call","call_id":"call_1","name":"delete_everything","arguments":"{}"}]
        }))
        .expect("body");

        let error = parse_responses_response(body).expect_err("failed response");

        assert!(error.to_string().contains("upstream failed"), "{error}");
    }

    #[test]
    fn accumulates_long_streamed_text() {
        let mut payload = String::with_capacity(65536);
        let segment = "ABCDEFGHIJ";
        let segments = 2000;

        for _ in 0..segments {
            payload.push_str("event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"");
            payload.push_str(segment);
            payload.push_str("\"}\n\n");
        }

        payload.push_str(
            "event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"output\":[],\"usage\":null}}\n",
        );

        let response = accumulate(&payload).expect("response");

        let expected: String = segment.repeat(segments);
        assert_eq!(response.content, expected);
        assert!(response.content.len() > 8192);
    }
}
