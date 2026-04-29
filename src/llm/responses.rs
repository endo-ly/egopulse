use super::*;

pub(crate) fn parse_openai_response(body: OpenAiResponse) -> Result<MessagesResponse, LlmError> {
    let usage = body.usage;
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
        tool_calls,
        usage: usage.and_then(|u| {
            u.prompt_tokens
                .zip(u.completion_tokens)
                .map(|(pt, ct)| LlmUsage {
                    input_tokens: pt,
                    output_tokens: ct,
                })
        }),
    })
}

pub(crate) fn parse_responses_response(
    body: ResponsesApiResponse,
) -> Result<MessagesResponse, LlmError> {
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
                let arguments = parse_tool_arguments(&arguments, &name)?;
                let call_id = call_id.or(id).unwrap_or_default();
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
        tool_calls,
        usage: body.usage.and_then(|u| {
            u.input_tokens
                .zip(u.output_tokens)
                .map(|(it, ot)| LlmUsage {
                    input_tokens: it,
                    output_tokens: ot,
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

pub(crate) fn parse_tool_call(raw: OaiToolCall) -> Result<ToolCall, LlmError> {
    let arguments = parse_tool_arguments(&raw.function.arguments, &raw.function.name)?;

    Ok(ToolCall {
        id: raw.id,
        name: raw.function.name,
        arguments,
    })
}

fn extract_response_content_part(part: ResponsesOutputPart) -> Option<String> {
    match part {
        ResponsesOutputPart::OutputText { text } => Some(text),
        ResponsesOutputPart::Refusal { refusal } => Some(refusal),
        ResponsesOutputPart::Ignored => None,
    }
}

fn parse_tool_arguments(arguments: &str, name: &str) -> Result<serde_json::Value, LlmError> {
    if arguments.trim().is_empty() {
        return Ok(serde_json::json!({}));
    }

    serde_json::from_str::<serde_json::Value>(arguments).map_err(|error| {
        LlmError::InvalidResponse(format!("invalid tool arguments for '{}': {error}", name))
    })
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

pub(crate) fn extract_text(content: OpenAiMessageContent) -> String {
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

/// Parses a Codex Responses API payload that may arrive as either plain JSON or
/// SSE stream text containing `response.done` events.
///
/// The Codex endpoint requires `stream: true`, so the response body consists of
/// newline-delimited SSE events. The final `response.completed` (or
/// `response.done`) event carries the complete [`ResponsesApiResponse`]
/// under its `"response"` key.
pub(crate) fn parse_codex_responses_payload(text: &str) -> Result<ResponsesApiResponse, LlmError> {
    if let Ok(parsed) = serde_json::from_str::<ResponsesApiResponse>(text) {
        return Ok(parsed);
    }
    let mut last_response: Option<ResponsesApiResponse> = None;
    let mut streamed_text = String::new();
    let mut streamed_items = Vec::new();

    for line in text.lines() {
        let line = line.trim();
        if !line.starts_with("data:") {
            continue;
        }
        let payload = line.trim_start_matches("data:").trim();
        if payload.is_empty() || payload == "[DONE]" {
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(payload) else {
            continue;
        };

        if let Some(item_value) = value.get("item")
            && let Ok(item) = serde_json::from_value::<ResponsesOutputItem>(item_value.clone())
            && !matches!(item, ResponsesOutputItem::Ignored)
        {
            streamed_items.push(item);
        }

        let event_type = value.get("type").and_then(|v| v.as_str());
        match event_type {
            Some("response.output_text.delta") => {
                if let Some(delta) = value.get("delta").and_then(|v| v.as_str()) {
                    streamed_text.push_str(delta);
                }
            }
            Some("response.output_text.done") => {
                if let Some(done_text) = value.get("text").and_then(|v| v.as_str())
                    && !done_text.is_empty()
                {
                    streamed_text = done_text.to_string();
                }
            }
            _ => {}
        }

        if let Some(response_value) = value.get("response") {
            if let Ok(parsed) =
                serde_json::from_value::<ResponsesApiResponse>(response_value.clone())
            {
                last_response = Some(parsed);
                if event_type == Some("response.done") || event_type == Some("response.completed") {
                    break;
                }
            }
        }
    }

    if let Some(mut response) = last_response {
        if response.output.is_empty() {
            if !streamed_items.is_empty() {
                response.output = streamed_items;
            } else if !streamed_text.trim().is_empty() {
                response.output.push(ResponsesOutputItem::Message {
                    role: Some("assistant".to_string()),
                    content: vec![ResponsesOutputPart::OutputText {
                        text: streamed_text,
                    }],
                });
            }
        }
        return Ok(response);
    }

    if !streamed_items.is_empty() || !streamed_text.trim().is_empty() {
        let mut output = streamed_items;
        if output.is_empty() {
            output.push(ResponsesOutputItem::Message {
                role: Some("assistant".to_string()),
                content: vec![ResponsesOutputPart::OutputText {
                    text: streamed_text,
                }],
            });
        }
        return Ok(ResponsesApiResponse {
            status: Some("completed".to_string()),
            output,
            usage: None,
            incomplete_details: None,
        });
    }

    Err(LlmError::InvalidResponse(format!(
        "Failed to parse Codex response payload. Body: {}",
        preview_body(text)
    )))
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
pub(crate) struct OpenAiResponse {
    pub(crate) choices: Vec<Choice>,
    #[serde(default)]
    pub(crate) usage: Option<OpenAiUsage>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct OpenAiUsage {
    #[serde(default)]
    pub(crate) prompt_tokens: Option<i64>,
    #[serde(default)]
    pub(crate) completion_tokens: Option<i64>,
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
}

#[derive(Debug, Deserialize)]
pub(crate) struct ResponsesApiUsage {
    #[serde(default)]
    pub(crate) input_tokens: Option<i64>,
    #[serde(default)]
    pub(crate) output_tokens: Option<i64>,
    #[serde(default)]
    pub(crate) output_tokens_details: Option<ResponsesOutputTokensDetails>,
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
pub(crate) struct Choice {
    pub(crate) message: AssistantMessage,
}

#[derive(Debug, Deserialize)]
pub(crate) struct AssistantMessage {
    #[serde(default)]
    pub(crate) content: Option<OpenAiMessageContent>,
    #[serde(default)]
    pub(crate) tool_calls: Option<Vec<OaiToolCall>>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub(crate) enum OpenAiMessageContent {
    Text(String),
    Parts(Vec<OpenAiContentPart>),
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
pub(crate) enum OpenAiContentPart {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "refusal")]
    Refusal { refusal: String },
    #[serde(other)]
    Ignored,
}

#[derive(Debug, Deserialize)]
pub(crate) struct OaiToolCall {
    pub(crate) id: String,
    pub(crate) function: OaiFunction,
}

#[derive(Debug, Deserialize)]
pub(crate) struct OaiFunction {
    pub(crate) name: String,
    pub(crate) arguments: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct OaiErrorResponse {
    pub(crate) error: OaiErrorDetail,
}

#[derive(Debug, Deserialize)]
pub(crate) struct OaiErrorDetail {
    pub(crate) message: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_codex_response_completed_sse_payload() {
        let payload = r#"
event: response.created
data: {"type":"response.created","response":{"output":[]}}

event: response.completed
data: {"type":"response.completed","response":{"output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"ok"}]}],"usage":{"input_tokens":1,"output_tokens":2}}}
"#;

        let parsed = parse_codex_responses_payload(payload).expect("payload");
        let response = parse_responses_response(parsed).expect("response");

        assert_eq!(response.content, "ok");
        assert_eq!(
            response.usage,
            Some(LlmUsage {
                input_tokens: 1,
                output_tokens: 2,
            })
        );
    }

    #[test]
    fn parses_codex_streamed_text_when_completed_output_is_empty() {
        let payload = r#"
event: response.output_text.delta
data: {"type":"response.output_text.delta","delta":"hello "}

event: response.output_text.delta
data: {"type":"response.output_text.delta","delta":"there"}

event: response.completed
data: {"type":"response.completed","response":{"status":"completed","output":[],"usage":{"input_tokens":1,"output_tokens":2,"output_tokens_details":{"reasoning_tokens":0}}}}
"#;

        let parsed = parse_codex_responses_payload(payload).expect("payload");
        let response = parse_responses_response(parsed).expect("response");

        assert_eq!(response.content, "hello there");
    }

    #[test]
    fn parses_codex_streamed_output_item_when_completed_output_is_empty() {
        let payload = r#"
event: response.output_item.done
data: {"type":"response.output_item.done","item":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"from item"}]}}

event: response.completed
data: {"type":"response.completed","response":{"status":"completed","output":[],"usage":{"input_tokens":1,"output_tokens":2}}}
"#;

        let parsed = parse_codex_responses_payload(payload).expect("payload");
        let response = parse_responses_response(parsed).expect("response");

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
}
