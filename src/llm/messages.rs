use super::*;
use std::collections::HashSet;

pub(crate) fn build_request_body(
    model: &str,
    system: &str,
    messages: &[Message],
    tools: Option<&[ToolDefinition]>,
    stream: Option<bool>,
) -> serde_json::Value {
    let mut translated = Vec::new();
    if !system.trim().is_empty() {
        translated.push(serde_json::json!({
            "role": "system",
            "content": system,
        }));
    }
    translated.extend(messages.iter().map(translate_message_to_openai));

    let mut body = serde_json::json!({
        "model": model,
        "messages": translated,
    });
    if let Some(stream) = stream {
        body["stream"] = serde_json::Value::Bool(stream);
    }
    append_chat_tools(&mut body, tools);
    body
}

pub(crate) fn build_responses_request_body(
    model: &str,
    system: &str,
    messages: &[Message],
    tools: Option<&[ToolDefinition]>,
) -> serde_json::Value {
    let mut body = serde_json::json!({
        "model": model,
        "input": translate_messages_to_responses(messages),
    });
    if !system.trim().is_empty() {
        body["instructions"] = serde_json::Value::String(system.to_string());
    }
    append_responses_tools(&mut body, tools);
    body
}

fn translate_messages_to_responses(messages: &[Message]) -> Vec<serde_json::Value> {
    let mut seen_function_call_ids = HashSet::new();
    let mut input = Vec::new();

    for message in messages {
        match message.role.as_str() {
            "assistant" if !message.tool_calls.is_empty() => {
                for tool_call in &message.tool_calls {
                    seen_function_call_ids.insert(tool_call.id.clone());
                }
                input.extend(translate_message_to_responses(message));
            }
            "tool" if tool_output_has_matching_call(message, &seen_function_call_ids) => {
                input.extend(translate_message_to_responses(message));
            }
            "tool" => input.extend(orphan_tool_output_to_responses_context(message)),
            _ => input.extend(translate_message_to_responses(message)),
        }
    }

    input
}

fn tool_output_has_matching_call(
    message: &Message,
    seen_function_call_ids: &HashSet<String>,
) -> bool {
    message
        .tool_call_id
        .as_ref()
        .is_some_and(|call_id| seen_function_call_ids.contains(call_id))
}

fn orphan_tool_output_to_responses_context(message: &Message) -> Vec<serde_json::Value> {
    let call_id = message
        .tool_call_id
        .as_deref()
        .filter(|call_id| !call_id.trim().is_empty())
        .unwrap_or("unknown");
    let text = format!(
        "Previous tool output could not be linked to a function call ({call_id}). Treat this as historical context only.\n{}",
        message.content.as_text_lossy()
    );
    let mut items = vec![serde_json::json!({
        "type": "message",
        "role": "user",
        "content": text,
    })];
    if message.content.is_multimodal() {
        items.push(serde_json::json!({
            "type": "message",
            "role": "user",
            "content": synthetic_tool_attachment_parts(&message.content),
        }));
    }
    items
}

pub(crate) fn translate_message_to_openai(message: &Message) -> serde_json::Value {
    match message.role.as_str() {
        "assistant" if !message.tool_calls.is_empty() => serde_json::json!({
            "role": "assistant",
            "content": if message.content.is_empty_textual() {
                serde_json::Value::Null
            } else {
                translate_content_to_chat_completions(&message.content)
            },
            "tool_calls": message.tool_calls.iter().map(|tool_call| {
                serde_json::json!({
                    "id": tool_call.id,
                    "type": "function",
                    "function": {
                        "name": tool_call.name,
                        "arguments": tool_call.arguments.to_string(),
                    }
                })
            }).collect::<Vec<_>>(),
        }),
        "tool" => serde_json::json!({
            "role": "tool",
            "content": message.content.as_text_lossy(),
            "tool_call_id": message.tool_call_id,
        }),
        _ => serde_json::json!({
            "role": message.role,
            "content": translate_content_to_chat_completions(&message.content),
        }),
    }
}

pub(crate) fn translate_content_to_chat_completions(content: &MessageContent) -> serde_json::Value {
    match content {
        MessageContent::Text(text) => serde_json::Value::String(text.clone()),
        MessageContent::Parts(parts) => {
            serde_json::Value::Array(parts.iter().map(chat_content_part).collect())
        }
    }
}

pub(crate) fn translate_message_to_responses(message: &Message) -> Vec<serde_json::Value> {
    match message.role.as_str() {
        "assistant" if !message.tool_calls.is_empty() => {
            let mut items = Vec::new();
            if !message.content.is_empty_textual() {
                items.push(serde_json::json!({
                    "type": "message",
                    "role": "assistant",
                    "content": translate_text_first_responses_content(&message.content),
                }));
            }
            items.extend(message.tool_calls.iter().map(|tool_call| {
                serde_json::json!({
                    "type": "function_call",
                    "call_id": tool_call.id,
                    "name": tool_call.name,
                    "arguments": tool_call.arguments.to_string(),
                })
            }));
            items
        }
        "tool" => {
            let mut items = vec![serde_json::json!({
                "type": "function_call_output",
                "call_id": message.tool_call_id.clone().unwrap_or_default(),
                "output": message.content.as_text_lossy(),
            })];
            if message.content.is_multimodal() {
                items.push(serde_json::json!({
                    "type": "message",
                    "role": "user",
                    "content": synthetic_tool_attachment_parts(&message.content),
                }));
            }
            items
        }
        _ => vec![serde_json::json!({
            "type": "message",
            "role": message.role,
            "content": translate_text_first_responses_content(&message.content),
        })],
    }
}

pub(crate) fn translate_content_to_responses_message(
    content: &MessageContent,
) -> Vec<serde_json::Value> {
    match content {
        MessageContent::Text(text) => vec![serde_json::json!({
            "type": "input_text",
            "text": text,
        })],
        MessageContent::Parts(parts) => parts.iter().map(responses_content_part).collect(),
    }
}

pub(crate) fn synthetic_tool_attachment_parts(content: &MessageContent) -> Vec<serde_json::Value> {
    let mut parts = vec![serde_json::json!({
        "type": "input_text",
        "text": "System-generated attachment for the immediately preceding tool result. Treat it as tool output context, not as a new user request.",
    })];
    if let MessageContent::Parts(items) = content {
        parts.extend(items.iter().filter_map(|part| match part {
            MessageContentPart::InputText { .. } => None,
            MessageContentPart::InputImage { image_url, detail } => Some(serde_json::json!({
                "type": "input_image",
                "image_url": image_url,
                "detail": detail.clone().unwrap_or_else(|| "auto".to_string()),
            })),
        }));
    }
    parts
}

fn append_chat_tools(body: &mut serde_json::Value, tools: Option<&[ToolDefinition]>) {
    let Some(tools) = tools.filter(|tools| !tools.is_empty()) else {
        return;
    };

    body["tools"] = serde_json::Value::Array(
        tools
            .iter()
            .map(|tool| {
                serde_json::json!({
                    "type": "function",
                    "function": {
                        "name": tool.name,
                        "description": tool.description,
                        "parameters": tool.parameters,
                    }
                })
            })
            .collect(),
    );
}

fn append_responses_tools(body: &mut serde_json::Value, tools: Option<&[ToolDefinition]>) {
    let Some(tools) = tools.filter(|tools| !tools.is_empty()) else {
        return;
    };

    body["tools"] = serde_json::Value::Array(
        tools
            .iter()
            .map(|tool| {
                serde_json::json!({
                    "type": "function",
                    "name": tool.name,
                    "description": tool.description,
                    "parameters": tool.parameters,
                })
            })
            .collect(),
    );
    body["tool_choice"] = serde_json::Value::String("auto".to_string());
}

fn default_detail(detail: &Option<String>) -> String {
    detail.clone().unwrap_or_else(|| "auto".to_string())
}

fn chat_content_part(part: &MessageContentPart) -> serde_json::Value {
    match part {
        MessageContentPart::InputText { text } => serde_json::json!({
            "type": "text",
            "text": text,
        }),
        MessageContentPart::InputImage { image_url, detail } => serde_json::json!({
            "type": "image_url",
            "image_url": {
                "url": image_url,
                "detail": default_detail(detail),
            }
        }),
    }
}

fn responses_content_part(part: &MessageContentPart) -> serde_json::Value {
    match part {
        MessageContentPart::InputText { text } => serde_json::json!({
            "type": "input_text",
            "text": text,
        }),
        MessageContentPart::InputImage { image_url, detail } => serde_json::json!({
            "type": "input_image",
            "image_url": image_url,
            "detail": default_detail(detail),
        }),
    }
}

fn translate_text_first_responses_content(content: &MessageContent) -> serde_json::Value {
    match content {
        MessageContent::Text(text) => serde_json::Value::String(text.clone()),
        MessageContent::Parts(parts)
            if parts
                .iter()
                .all(|part| matches!(part, MessageContentPart::InputText { .. })) =>
        {
            serde_json::Value::String(content.as_text_lossy())
        }
        MessageContent::Parts(_) => {
            serde_json::Value::Array(translate_content_to_responses_message(content))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn responses_request_uses_string_content_for_text_messages() {
        let body = build_responses_request_body(
            "gpt-5.3-codex",
            "system",
            &[Message::text("user", "hello")],
            None,
        );

        assert_eq!(body["instructions"], "system");
        assert_eq!(body["input"][0]["type"], "message");
        assert_eq!(body["input"][0]["role"], "user");
        assert_eq!(body["input"][0]["content"], "hello");
    }

    #[test]
    fn responses_request_keeps_part_content_for_images() {
        let body = build_responses_request_body(
            "gpt-4o-mini",
            "",
            &[Message {
                role: "user".to_string(),
                content: MessageContent::parts(vec![
                    MessageContentPart::InputText {
                        text: "describe".to_string(),
                    },
                    MessageContentPart::InputImage {
                        image_url: "data:image/png;base64,AAAA".to_string(),
                        detail: Some("auto".to_string()),
                    },
                ]),
                tool_calls: Vec::new(),
                tool_call_id: None,
            }],
            None,
        );

        assert_eq!(body["input"][0]["content"][0]["type"], "input_text");
        assert_eq!(body["input"][0]["content"][1]["type"], "input_image");
    }

    #[test]
    fn responses_request_sets_auto_tool_choice_when_tools_exist() {
        let body = build_responses_request_body(
            "gpt-5.3-codex",
            "",
            &[Message::text("user", "hello")],
            Some(&[ToolDefinition {
                name: "read".to_string(),
                description: "Read a file".to_string(),
                parameters: serde_json::json!({"type": "object"}),
            }]),
        );

        assert_eq!(body["tool_choice"], "auto");
        assert_eq!(body["tools"][0]["type"], "function");
        assert_eq!(body["tools"][0]["name"], "read");
    }

    #[test]
    fn responses_request_keeps_matched_tool_output_as_function_call_output() {
        let body = build_responses_request_body(
            "gpt-5.3-codex",
            "",
            &[
                Message {
                    role: "assistant".to_string(),
                    content: MessageContent::text(""),
                    tool_calls: vec![ToolCall {
                        id: "call_1".to_string(),
                        name: "read".to_string(),
                        arguments: serde_json::json!({"path": "README.md"}),
                    }],
                    tool_call_id: None,
                },
                Message {
                    role: "tool".to_string(),
                    content: MessageContent::text("read result"),
                    tool_calls: Vec::new(),
                    tool_call_id: Some("call_1".to_string()),
                },
            ],
            None,
        );

        assert_eq!(body["input"][0]["type"], "function_call");
        assert_eq!(body["input"][1]["type"], "function_call_output");
        assert_eq!(body["input"][1]["call_id"], "call_1");
    }

    #[test]
    fn responses_request_converts_orphan_tool_output_to_context_message() {
        let body = build_responses_request_body(
            "gpt-5.3-codex",
            "",
            &[Message {
                role: "tool".to_string(),
                content: MessageContent::text("orphan result"),
                tool_calls: Vec::new(),
                tool_call_id: Some("call_missing".to_string()),
            }],
            None,
        );

        assert_eq!(body["input"][0]["type"], "message");
        assert_eq!(body["input"][0]["role"], "user");
        assert!(
            body["input"][0]["content"]
                .as_str()
                .expect("content string")
                .contains("call_missing")
        );
    }
}
