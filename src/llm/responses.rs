use super::*;

pub(crate) fn parse_openai_response(body: OpenAiResponse) -> Result<MessagesResponse, LlmError> {
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
    })
}

pub(crate) fn parse_responses_response(
    body: ResponsesApiResponse,
) -> Result<MessagesResponse, LlmError> {
    let mut content_parts = Vec::new();
    let mut tool_calls = Vec::new();

    for item in body.output {
        match item {
            ResponsesOutputItem::Message { role, content } if role == "assistant" => {
                content_parts.extend(
                    content
                        .into_iter()
                        .filter_map(extract_response_content_part),
                );
            }
            ResponsesOutputItem::FunctionCall {
                call_id,
                name,
                arguments,
            } => {
                let arguments = parse_tool_arguments(&arguments, &name)?;
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
        return Err(LlmError::InvalidResponse(
            "assistant content was empty".to_string(),
        ));
    }

    Ok(MessagesResponse {
        content,
        tool_calls,
    })
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
}

#[derive(Debug, Deserialize)]
pub(crate) struct ResponsesApiResponse {
    pub(crate) output: Vec<ResponsesOutputItem>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
pub(crate) enum ResponsesOutputItem {
    #[serde(rename = "message")]
    Message {
        role: String,
        content: Vec<ResponsesOutputPart>,
    },
    #[serde(rename = "function_call")]
    FunctionCall {
        call_id: String,
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
