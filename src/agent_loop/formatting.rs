//! メッセージのフォーマット、サニタイズ、表示用テキスト変換。

use crate::llm::{Message, MessageContent, MessageContentPart, ToolCall};

const MAX_TOOL_RESULT_CHARS: usize = 16_000;
const MAX_TOOL_RESULT_TEXT_CHARS: usize = 200;

pub(crate) fn format_tool_result(
    tool_call: &ToolCall,
    result: &crate::tools::ToolResult,
) -> String {
    let mut content = result.content.clone();
    let details = result.details.clone();

    loop {
        let serialized =
            serialize_tool_payload(tool_call, result.is_error, &content, details.as_ref());
        let char_count = serialized.chars().count();

        if char_count <= MAX_TOOL_RESULT_CHARS {
            return serialized;
        }

        if let Some(serialized_without_details) = details
            .as_ref()
            .map(|_| serialize_tool_payload(tool_call, result.is_error, &content, None))
            .filter(|serialized| serialized.chars().count() <= MAX_TOOL_RESULT_CHARS)
        {
            return serialized_without_details;
        }

        let excess = char_count.saturating_sub(MAX_TOOL_RESULT_CHARS);
        let current_content_len = content.chars().count();
        let new_len = current_content_len.saturating_sub(excess + 100);
        if new_len == 0 {
            return serialize_tool_payload(tool_call, result.is_error, "...", None);
        }
        content = format!("{}...", content.chars().take(new_len).collect::<String>());
    }
}

fn serialize_tool_payload(
    tool_call: &ToolCall,
    is_error: bool,
    content: &str,
    details: Option<&serde_json::Value>,
) -> String {
    let mut payload = serde_json::json!({
        "tool": tool_call.name,
        "status": if is_error { "error" } else { "success" },
        "result": content,
    });
    if let Some(details) = details {
        payload["details"] = details.clone();
    }
    payload.to_string()
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
    if message.tool_call_id.is_some() {
        return render_tool_message_text(message, true);
    }

    render_message_with_tool_calls(
        format_content_for_display(message, should_strip_thinking_for_role(&message.role)),
        &message.tool_calls,
    )
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
    serde_json::from_str::<serde_json::Value>(payload)
        .ok()
        .and_then(|value| {
            value
                .get("result")
                .and_then(|result| result.as_str())
                .map(ToString::to_string)
        })
        .unwrap_or_else(|| payload.to_string())
}

pub(crate) fn sanitize_assistant_response_text(content: &str) -> String {
    strip_thinking(content.trim())
}

pub(crate) fn should_strip_thinking_for_role(role: &str) -> bool {
    matches!(role, "assistant" | "tool")
}

pub(crate) fn message_to_archive_text(message: &Message) -> String {
    if message.tool_call_id.is_some() {
        return render_tool_message_text(message, false);
    }

    render_message_with_tool_calls(
        format_content_for_archive(message, should_strip_thinking_for_role(&message.role)),
        &message.tool_calls,
    )
}

fn render_tool_message_text(message: &Message, truncate: bool) -> String {
    let payload = tool_result_payload(message).unwrap_or_default();
    let body = render_tool_body(payload, truncate);
    format!("{}{body}", tool_message_prefix(message))
}

fn render_tool_body(payload: &str, truncate: bool) -> String {
    if truncate {
        return truncate_summary_text(
            &strip_thinking(&tool_result_body(payload)),
            MAX_TOOL_RESULT_TEXT_CHARS,
        );
    }

    strip_thinking(payload)
}

fn tool_message_prefix(message: &Message) -> &'static str {
    if is_tool_error_message(message) {
        "[tool_error]: "
    } else {
        "[tool_result]: "
    }
}

fn render_message_with_tool_calls(content: String, tool_calls: &[ToolCall]) -> String {
    let mut parts = Vec::new();
    if !content.trim().is_empty() {
        parts.push(content);
    }
    for tool_call in tool_calls {
        parts.push(format!(
            "[tool_use: {}({})]",
            tool_call.name, tool_call.arguments
        ));
    }
    parts.join("\n")
}

fn format_content_for_display(message: &Message, should_strip: bool) -> String {
    format_message_content(&message.content, should_strip, false)
}

fn format_content_for_archive(message: &Message, should_strip: bool) -> String {
    format_message_content(&message.content, should_strip, true)
}

fn format_message_content(
    content: &MessageContent,
    should_strip: bool,
    include_image_payload: bool,
) -> String {
    match content {
        MessageContent::Text(text) => sanitize_message_text(text, should_strip),
        MessageContent::Parts(parts) => parts
            .iter()
            .filter_map(|part| format_message_part(part, should_strip, include_image_payload))
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

fn format_message_part(
    part: &MessageContentPart,
    should_strip: bool,
    include_image_payload: bool,
) -> Option<String> {
    match part {
        MessageContentPart::InputText { text } => {
            let text = sanitize_message_text(text, should_strip);
            (!text.is_empty()).then_some(text)
        }
        MessageContentPart::InputImage { image_url, detail } => {
            Some(match (include_image_payload, detail) {
                (false, _) => "[image]".to_string(),
                (true, Some(detail)) => format!("[image: {image_url} detail={detail}]"),
                (true, None) => format!("[image: {image_url}]"),
            })
        }
    }
}

fn sanitize_message_text(text: &str, should_strip: bool) -> String {
    if should_strip {
        strip_thinking(text)
    } else {
        text.to_string()
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_loop::turn::tool_result_message;
    use crate::llm::{Message, MessageContent, MessageContentPart, ToolCall};

    #[test]
    fn message_to_text_preserves_plain_text() {
        let message = Message::text("assistant", "hello world");

        assert_eq!(message_to_text(&message), "hello world");
    }

    #[test]
    fn message_to_text_strips_hidden_reasoning_from_text() {
        let message = Message::text("assistant", "hello <thought>secret</thought> world");

        assert_eq!(message_to_text(&message), "hello  world");
    }

    #[test]
    fn message_to_text_preserves_user_literal_thinking_tags() {
        let message = Message::text("user", "hello <think>literal</think> world");

        assert_eq!(
            message_to_text(&message),
            "hello <think>literal</think> world"
        );
    }

    #[test]
    fn message_to_text_renders_multimodal_images() {
        let message = Message {
            role: "user".to_string(),
            content: MessageContent::parts(vec![
                MessageContentPart::InputText {
                    text: "hello".to_string(),
                },
                MessageContentPart::InputImage {
                    image_url: "data:image/png;base64,abc".to_string(),
                    detail: None,
                },
            ]),
            tool_calls: Vec::new(),
            tool_call_id: None,
        };

        assert_eq!(message_to_text(&message), "hello\n[image]");
    }

    #[test]
    fn message_to_text_strips_hidden_reasoning_from_input_text_and_tool_results() {
        let message = Message {
            role: "tool".to_string(),
            content: MessageContent::parts(vec![MessageContentPart::InputText {
                text: "prefix <think>secret</think> suffix".to_string(),
            }]),
            tool_calls: Vec::new(),
            tool_call_id: Some("call-1".to_string()),
        };

        assert_eq!(message_to_text(&message), "[tool_result]: prefix  suffix");
    }

    #[test]
    fn message_to_text_renders_tool_use() {
        let message = Message {
            role: "assistant".to_string(),
            content: MessageContent::text(""),
            tool_calls: vec![ToolCall {
                id: "call-1".to_string(),
                name: "search".to_string(),
                arguments: serde_json::json!({"query": "egopulse"}),
            }],
            tool_call_id: None,
        };

        assert_eq!(
            message_to_text(&message),
            "[tool_use: search({\"query\":\"egopulse\"})]"
        );
    }

    #[test]
    fn message_to_text_renders_tool_result() {
        let message = tool_result_message("success", "all good");

        assert_eq!(message_to_text(&message), "[tool_result]: all good");
    }

    #[test]
    fn message_to_text_renders_tool_error() {
        let message = tool_result_message("error", "something went wrong");

        assert_eq!(
            message_to_text(&message),
            "[tool_error]: something went wrong"
        );
    }

    #[test]
    fn message_to_text_truncates_tool_result_to_200_chars() {
        let result = "あ".repeat(260);
        let message = tool_result_message("success", &result);
        let rendered = message_to_text(&message);
        let prefix = "[tool_result]: ";
        assert!(rendered.starts_with(prefix));

        let body = &rendered[prefix.len()..];
        assert!(body.ends_with("..."));
        assert_eq!(body.chars().count(), 203);
        assert_eq!(body[..body.len() - 3].chars().count(), 200);
    }

    #[test]
    fn message_to_archive_text_preserves_full_tool_payload() {
        let result = "a".repeat(260);
        let message = tool_result_message("success", &result);

        let rendered = message_to_archive_text(&message);
        assert!(rendered.starts_with("[tool_result]: "));
        assert!(rendered.contains(&result));
        assert!(!rendered.contains("..."));
    }

    #[test]
    fn message_to_archive_text_renders_full_image_and_text_content() {
        let message = Message {
            role: "assistant".to_string(),
            content: MessageContent::parts(vec![
                MessageContentPart::InputText {
                    text: "hello <thinking>internal</thinking> world".to_string(),
                },
                MessageContentPart::InputImage {
                    image_url: "data:image/png;base64,abc".to_string(),
                    detail: Some("high".to_string()),
                },
            ]),
            tool_calls: vec![ToolCall {
                id: "call-1".to_string(),
                name: "search".to_string(),
                arguments: serde_json::json!({"query": "egopulse"}),
            }],
            tool_call_id: None,
        };

        assert_eq!(
            message_to_archive_text(&message),
            "hello  world\n[image: data:image/png;base64,abc detail=high]\n[tool_use: search({\"query\":\"egopulse\"})]"
        );
    }

    #[test]
    fn message_to_archive_text_preserves_user_literal_thinking_tags() {
        let message = Message::text("user", "hello <thought>literal</thought> world");

        assert_eq!(
            message_to_archive_text(&message),
            "hello <thought>literal</thought> world"
        );
    }

    #[test]
    fn message_to_text_falls_back_to_raw_payload_when_result_is_missing() {
        let message = Message {
            role: "tool".to_string(),
            content: MessageContent::text(r#"{"tool":"read","status":"success"}"#),
            tool_calls: Vec::new(),
            tool_call_id: Some("call-1".to_string()),
        };

        assert_eq!(
            message_to_text(&message),
            r#"[tool_result]: {"tool":"read","status":"success"}"#
        );
    }

    #[test]
    fn strip_thinking_removes_thinking_tags() {
        assert_eq!(strip_thinking("hello world"), "hello world");
        assert_eq!(
            strip_thinking("<thought>internal</thought>visible"),
            "visible"
        );
        assert_eq!(strip_thinking("<thinking>deep</thinking>result"), "result");
        assert_eq!(
            strip_thinking("<reasoning>logic</reasoning>output"),
            "output"
        );
        assert_eq!(
            strip_thinking("<thought>a</thought><thinking>b</thinking>final"),
            "final"
        );
    }
}
