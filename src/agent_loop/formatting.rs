//! メッセージのフォーマット、サニタイズ、表示用テキスト変換。

use crate::llm::{Message, ToolCall};

const MAX_TOOL_RESULT_CHARS: usize = 16_000;
const MAX_TOOL_RESULT_TEXT_CHARS: usize = 200;

pub(crate) fn format_tool_result(
    tool_call: &ToolCall,
    result: &crate::tools::ToolResult,
) -> String {
    let mut content = result.content.clone();
    let details = result.details.clone();

    loop {
        let mut payload = serde_json::json!({
            "tool": tool_call.name,
            "status": if result.is_error { "error" } else { "success" },
            "result": content,
        });
        if let Some(ref d) = details {
            payload["details"] = d.clone();
        }

        let serialized = payload.to_string();
        let char_count = serialized.chars().count();

        if char_count <= MAX_TOOL_RESULT_CHARS {
            return serialized;
        }

        // If over limit, first try removing details
        if details.is_some() {
            let payload_no_details = serde_json::json!({
                "tool": tool_call.name,
                "status": if result.is_error { "error" } else { "success" },
                "result": content,
            });
            let no_details_str = payload_no_details.to_string();
            if no_details_str.chars().count() <= MAX_TOOL_RESULT_CHARS {
                return no_details_str;
            }
        }

        // Still over limit, truncate content further
        // Calculate how much we need to reduce content by
        let excess = char_count.saturating_sub(MAX_TOOL_RESULT_CHARS);
        let current_content_len = content.chars().count();
        // Reduce content by excess + buffer for JSON overhead
        let new_len = current_content_len.saturating_sub(excess + 100);
        if new_len == 0 {
            // Can't truncate further, return minimal payload
            return serde_json::json!({
                "tool": tool_call.name,
                "status": if result.is_error { "error" } else { "success" },
                "result": "...",
            })
            .to_string();
        }
        content = format!("{}...", content.chars().take(new_len).collect::<String>());
    }
}

pub(crate) fn tool_message_content(
    payload: &str,
    result: &crate::tools::ToolResult,
) -> crate::llm::MessageContent {
    match &result.llm_content {
        crate::llm::MessageContent::Text(_) => crate::llm::MessageContent::text(payload),
        crate::llm::MessageContent::Parts(parts) => {
            let mut content = Vec::with_capacity(parts.len() + 1);
            content.push(crate::llm::MessageContentPart::InputText {
                text: payload.to_string(),
            });
            content.extend(parts.iter().filter_map(|part| match part {
                crate::llm::MessageContentPart::InputText { .. } => None,
                crate::llm::MessageContentPart::InputImage { image_url, detail } => {
                    Some(crate::llm::MessageContentPart::InputImage {
                        image_url: image_url.clone(),
                        detail: detail.clone(),
                    })
                }
            }));
            crate::llm::MessageContent::parts(content)
        }
    }
}

pub(crate) fn summarize_tool_calls_with_content(content: &str, tool_calls: &[ToolCall]) -> String {
    let names = tool_calls
        .iter()
        .map(|tool_call| tool_call.name.as_str())
        .collect::<Vec<_>>();
    if content.trim().is_empty() {
        format!("[tool_call] {}", names.join(", "))
    } else {
        format!("{} [tool_call] {}", content.trim(), names.join(", "))
    }
}

pub(crate) fn preview_text(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }
    format!("{}...", value.chars().take(max_chars).collect::<String>())
}

pub(crate) fn message_to_text(message: &Message) -> String {
    let should_strip_thinking = should_strip_thinking_for_role(&message.role);
    let content = match &message.content {
        crate::llm::MessageContent::Text(text) => {
            if should_strip_thinking {
                strip_thinking(text)
            } else {
                text.clone()
            }
        }
        crate::llm::MessageContent::Parts(parts) => parts
            .iter()
            .map(|part| match part {
                crate::llm::MessageContentPart::InputText { text } => {
                    if should_strip_thinking {
                        strip_thinking(text)
                    } else {
                        text.clone()
                    }
                }
                crate::llm::MessageContentPart::InputImage { .. } => "[image]".to_string(),
            })
            .collect::<Vec<_>>()
            .join("\n"),
    };
    if message.tool_call_id.is_some() {
        let payload = tool_result_payload(message).unwrap_or_default();
        let body = strip_thinking(&tool_result_body(payload));
        let truncated = truncate_summary_text(&body, MAX_TOOL_RESULT_TEXT_CHARS);
        let prefix = if is_tool_error_message(message) {
            "[tool_error]: "
        } else {
            "[tool_result]: "
        };
        return format!("{prefix}{truncated}");
    }

    let mut parts = Vec::new();
    if !content.trim().is_empty() {
        parts.push(content);
    }
    for tool_call in &message.tool_calls {
        parts.push(format!(
            "[tool_use: {}({})]",
            tool_call.name, tool_call.arguments
        ));
    }
    parts.join("\n")
}

pub(crate) fn is_tool_error_message(message: &Message) -> bool {
    let Some(payload) = tool_result_payload(message) else {
        return false;
    };
    serde_json::from_str::<serde_json::Value>(payload)
        .ok()
        .and_then(|value| {
            value
                .get("status")
                .and_then(|status| status.as_str())
                .map(|status| status == "error")
        })
        .unwrap_or(false)
}

pub(crate) fn tool_result_body(payload: &str) -> String {
    match serde_json::from_str::<serde_json::Value>(payload) {
        Ok(value) => value
            .get("result")
            .and_then(|result| result.as_str())
            .map(ToString::to_string)
            .unwrap_or_else(|| payload.to_string()),
        Err(_) => payload.to_string(),
    }
}

pub(crate) fn sanitize_assistant_response_text(content: &str) -> String {
    strip_thinking(content.trim())
}

pub(crate) fn should_strip_thinking_for_role(role: &str) -> bool {
    matches!(role, "assistant" | "tool")
}

pub(crate) fn message_to_archive_text(message: &Message) -> String {
    let should_strip_thinking = should_strip_thinking_for_role(&message.role);
    let content = match &message.content {
        crate::llm::MessageContent::Text(text) => {
            if should_strip_thinking {
                strip_thinking(text)
            } else {
                text.clone()
            }
        }
        crate::llm::MessageContent::Parts(parts) => parts
            .iter()
            .map(|part| match part {
                crate::llm::MessageContentPart::InputText { text } => {
                    if should_strip_thinking {
                        strip_thinking(text)
                    } else {
                        text.clone()
                    }
                }
                crate::llm::MessageContentPart::InputImage { image_url, detail } => match detail {
                    Some(detail) => format!("[image: {image_url} detail={detail}]"),
                    None => format!("[image: {image_url}]"),
                },
            })
            .collect::<Vec<_>>()
            .join("\n"),
    };

    if message.tool_call_id.is_some() {
        let payload = tool_result_payload(message).unwrap_or_default();
        let body = strip_thinking(payload);
        let prefix = if is_tool_error_message(message) {
            "[tool_error]: "
        } else {
            "[tool_result]: "
        };
        return format!("{prefix}{body}");
    }

    let mut parts = Vec::new();
    if !content.trim().is_empty() {
        parts.push(content);
    }
    for tool_call in &message.tool_calls {
        parts.push(format!(
            "[tool_use: {}({})]",
            tool_call.name, tool_call.arguments
        ));
    }
    parts.join("\n")
}

pub(crate) fn tool_result_payload(message: &Message) -> Option<&str> {
    match &message.content {
        crate::llm::MessageContent::Text(text) => Some(text.as_str()),
        crate::llm::MessageContent::Parts(parts) => parts.iter().find_map(|part| match part {
            crate::llm::MessageContentPart::InputText { text } => Some(text.as_str()),
            crate::llm::MessageContentPart::InputImage { .. } => None,
        }),
    }
}

pub(crate) fn truncate_summary_text(text: &str, max_chars: usize) -> String {
    let char_count = text.chars().count();
    if char_count <= max_chars {
        return text.to_string();
    }
    let truncated = text.chars().take(max_chars).collect::<String>();
    format!("{truncated}...")
}

/// `<think`>`...`</think`>` や `<thought`>`...`</thought`>` などの
/// thinking タグブロックをモデル出力から除去する。
/// microclaw agent_engine.rs から移植。
pub(crate) fn strip_thinking(text: &str) -> String {
    fn strip_tag_blocks(input: &str, open: &str, close: &str) -> String {
        let mut result = String::with_capacity(input.len());
        let mut rest = input;
        while let Some(start) = rest.find(open) {
            result.push_str(&rest[..start]);
            if let Some(end) = rest[start..].find(close) {
                rest = &rest[start + end + close.len()..];
            } else {
                rest = "";
                break;
            }
        }
        result.push_str(rest);
        result
    }

    let no_think = strip_tag_blocks(text, "\u{3C}think\u{3E}", "\u{3C}/think\u{3E}");
    let no_thought = strip_tag_blocks(&no_think, "\u{3C}thought\u{3E}", "\u{3C}/thought\u{3E}");
    let no_thinking =
        strip_tag_blocks(&no_thought, "\u{3C}thinking\u{3E}", "\u{3C}/thinking\u{3E}");
    let no_reasoning = strip_tag_blocks(
        &no_thinking,
        "\u{3C}reasoning\u{3E}",
        "\u{3C}/reasoning\u{3E}",
    );
    no_reasoning.trim().to_string()
}
