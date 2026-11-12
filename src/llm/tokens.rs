//! Provider-independent token estimation.
//!
//! The estimator is deliberately cheap and language-agnostic: it divides the
//! UTF-16 length of the text by [`CHARS_PER_TOKEN_ESTIMATE`]. CJK text needs
//! more tokens per character than ASCII, so the usage calibration corrects the
//! estimate from real provider usage rather than hard-coding a per-language
//! coefficient.
//!
//! Both the compaction trigger and the request output limit use these
//! functions so that one request is never sized by two different estimators.

use crate::llm::{Message, MessageContent, MessageContentPart};

/// Characters per token used by the raw estimate.
pub(crate) const CHARS_PER_TOKEN_ESTIMATE: usize = 4;

/// Character budget attributed to one image part.
pub(crate) const ESTIMATED_IMAGE_CHARS: usize = 4_800;

/// Room left unused when the model's context window bounds the output limit.
///
/// The estimate is approximate, so clamping the output limit to the exact
/// remaining window could still reject the request.
pub(crate) const CONTEXT_SAFETY_TOKENS: usize = 4_096;

/// Number of characters a text contributes to the token estimate.
///
/// The reference heuristic measures JavaScript string length, which counts
/// UTF-16 code units. Counting Rust `char`s instead would undercount astral
/// characters such as emoji by one unit each.
pub(crate) fn text_chars(text: &str) -> usize {
    text.encode_utf16().count()
}

pub(crate) fn chars_to_tokens(chars: usize) -> usize {
    chars.div_ceil(CHARS_PER_TOKEN_ESTIMATE)
}

/// Estimates the tokens of one message from its serialized size.
///
/// Role names are not part of the estimate: they are fixed per message and the
/// reference heuristic only measures content. Reasoning is counted here and
/// excluded only when the conversation is serialized for a summary.
pub(crate) fn estimate_message_tokens(message: &Message) -> usize {
    let mut chars = content_chars(&message.content);
    if let Some(reasoning) = &message.reasoning_content {
        chars += text_chars(reasoning);
    }
    for tool_call in &message.tool_calls {
        chars += text_chars(&tool_call.name);
        chars += text_chars(&tool_call.arguments.to_string());
    }
    chars_to_tokens(chars)
}

fn content_chars(content: &MessageContent) -> usize {
    match content {
        MessageContent::Text(text) => text_chars(text),
        MessageContent::Parts(parts) => parts
            .iter()
            .map(|part| match part {
                MessageContentPart::InputText { text } => text_chars(text),
                MessageContentPart::InputImage { .. } => ESTIMATED_IMAGE_CHARS,
            })
            .sum(),
    }
}

pub(crate) fn estimate_messages_tokens(messages: &[Message]) -> usize {
    messages.iter().map(estimate_message_tokens).sum()
}

/// Estimates the tokens of one fixed request part such as a system prompt.
pub(crate) fn estimate_text_tokens(text: &str) -> usize {
    chars_to_tokens(text_chars(text))
}

/// Estimates the raw prompt size of a request without a checkpoint.
pub(crate) fn estimate_prompt_tokens(
    system_prompt: &str,
    tools_json: Option<&str>,
    messages: &[Message],
) -> usize {
    let mut tokens = estimate_text_tokens(system_prompt);
    if let Some(tools) = tools_json {
        tokens += estimate_text_tokens(tools);
    }
    tokens + estimate_messages_tokens(messages)
}

/// Caps the requested output limit so the prompt and the answer fit together.
///
/// `intended` is the model's output limit before this clamp; callers must keep
/// using it when deciding whether a truncated generation was cut short, because
/// the provider only knows the clamped value.
pub(crate) fn clamp_max_output_tokens_to_context(
    context_window: usize,
    context_tokens: usize,
    intended: usize,
) -> usize {
    if context_window == 0 {
        return intended.max(1);
    }
    let available = context_window
        .saturating_sub(context_tokens)
        .saturating_sub(CONTEXT_SAFETY_TOKENS);
    intended.min(available.max(1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::{Message, ToolCall};

    fn text(role: &str, content: &str) -> Message {
        Message::text(role, content)
    }

    #[test]
    fn token_estimation_matches_the_reference_heuristic() {
        // Arrange: ASCII, CJK, emoji, reasoning, and tool arguments exercise
        // the UTF-16 length and the non-content fields.
        let assistant = Message {
            role: "assistant".to_string(),
            content: MessageContent::text("ok"),
            reasoning_content: Some("1234".to_string()),
            tool_calls: vec![ToolCall {
                id: "call_1".to_string(),
                name: "read".to_string(),
                arguments: serde_json::json!({"path": "a"}),
                invalid_arguments: None,
            }],
            tool_call_id: None,
        };

        // Act & Assert
        assert_eq!(estimate_message_tokens(&text("user", "hello world")), 3);
        assert_eq!(estimate_message_tokens(&text("user", "こんにちは世界")), 2);
        assert_eq!(estimate_message_tokens(&text("user", "😀😀")), 1);
        assert_eq!(estimate_message_tokens(&assistant), 6);
    }

    #[test]
    fn image_parts_use_a_fixed_character_budget() {
        // Arrange
        let message = Message {
            role: "user".to_string(),
            content: MessageContent::Parts(vec![MessageContentPart::InputImage {
                image_url: "data:image/png;base64,AAAA".to_string(),
                detail: None,
            }]),
            reasoning_content: None,
            tool_calls: Vec::new(),
            tool_call_id: None,
        };

        // Act
        let tokens = estimate_message_tokens(&message);

        // Assert
        assert_eq!(
            tokens,
            ESTIMATED_IMAGE_CHARS.div_ceil(CHARS_PER_TOKEN_ESTIMATE)
        );
    }

    #[test]
    fn output_limit_is_clamped_to_the_remaining_context() {
        // Arrange
        let window = 32_768;
        let prompt = 20_000;

        // Act
        let clamped = clamp_max_output_tokens_to_context(window, prompt, 16_384);

        // Assert: window - prompt - safety leaves 8,768 tokens for the answer.
        assert_eq!(clamped, 8_672);
    }

    #[test]
    fn a_full_context_leaves_one_output_token() {
        // Act
        let clamped = clamp_max_output_tokens_to_context(32_768, 40_000, 16_384);

        // Assert
        assert_eq!(clamped, 1);
    }

    #[test]
    fn an_unknown_window_keeps_the_intended_limit() {
        // Act
        let clamped = clamp_max_output_tokens_to_context(0, 10_000, 16_384);

        // Assert
        assert_eq!(clamped, 16_384);
    }
}
