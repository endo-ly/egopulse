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
    estimate_prompt_tokens_and_images(system_prompt, tools_json, messages).0
}

/// Estimates the raw prompt size and reports whether a message contains an image.
///
/// The image check selects the API surface: an image request uses the Responses
/// API, whose output minimum is larger than Chat Completions. The estimator
/// borrows messages so compaction can size a projected request without cloning
/// large text or image payloads.
pub(crate) fn estimate_prompt_tokens_and_images<'a>(
    system_prompt: &str,
    tools_json: Option<&str>,
    messages: impl IntoIterator<Item = &'a Message>,
) -> (usize, bool) {
    let mut tokens = estimate_text_tokens(system_prompt);
    if let Some(tools) = tools_json {
        tokens += estimate_text_tokens(tools);
    }
    let mut has_images = false;
    for message in messages {
        tokens += estimate_message_tokens(message);
        has_images |= message.content.is_multimodal();
    }
    (tokens, has_images)
}

/// Why a request cannot be sized for the model.
///
/// The provider boundary turns this into a request error instead of sending a
/// generation limit that the API would reject, that guarantees an empty
/// answer, or that exceeds the configured output limit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OutputCapacityError {
    /// The prompt leaves less room than the API's minimum output.
    InsufficientRoom {
        /// Context window of the model, in tokens.
        context_window: usize,
        /// Estimated prompt size of the request, in tokens.
        prompt_tokens: usize,
        /// Minimum output limit required by the API surface.
        minimum_output_tokens: usize,
    },
    /// The configured output limit is below the API's minimum.
    ///
    /// Compacting the conversation cannot fix this: the limit itself is
    /// unusable for the selected API surface.
    BelowApiMinimum {
        /// Output limit configured for the model.
        intended: usize,
        /// Minimum output limit required by the API surface.
        minimum_output_tokens: usize,
    },
}

/// Fits the requested output limit into the room left by the prompt.
///
/// `intended` is the model's output limit before this clamp; callers must keep
/// using it when deciding whether a truncated generation was cut short, because
/// the provider only knows the clamped value. `minimum_output_tokens` is the
/// smallest limit the API surface accepts (Responses requires more than one
/// token, Chat Completions accepts one).
///
/// The returned limit never exceeds `intended`, so setting `max_output_tokens`
/// lower than the API minimum fails instead of silently sending a larger limit.
///
/// Returns an error when the configured limit is below the API minimum or the
/// prompt leaves less room than that minimum. The first is a configuration
/// mismatch that compaction cannot fix; the second is resolved by compacting
/// the context.
///
/// # Errors
///
/// Returns [`OutputCapacityError::BelowApiMinimum`] when `intended` is below
/// `minimum_output_tokens`, and [`OutputCapacityError::InsufficientRoom`] when
/// the remaining room is below that minimum.
pub(crate) fn max_output_tokens_for_request(
    context_window: usize,
    prompt_tokens: usize,
    intended: usize,
    minimum_output_tokens: usize,
) -> Result<usize, OutputCapacityError> {
    let minimum = minimum_output_tokens.max(1);
    if intended < minimum {
        return Err(OutputCapacityError::BelowApiMinimum {
            intended,
            minimum_output_tokens: minimum,
        });
    }
    let remaining = context_window.saturating_sub(prompt_tokens);
    // Reserve room for estimation error, but never more than half of what is
    // left: a small window must still be able to answer.
    let safety = CONTEXT_SAFETY_TOKENS.min(remaining / 2);
    let available = remaining.saturating_sub(safety);
    if available < minimum {
        return Err(OutputCapacityError::InsufficientRoom {
            context_window,
            prompt_tokens,
            minimum_output_tokens: minimum,
        });
    }
    Ok(intended.min(available))
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
            finish_reason: None,
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
            finish_reason: None,
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
        // Arrange: 32,768 - 20,000 leaves 12,768; the 4,096 safety reserve
        // leaves 8,672 for the answer.

        // Act
        let limit = max_output_tokens_for_request(32_768, 20_000, 16_384, 1)
            .expect("there is room for the answer");

        // Assert
        assert_eq!(limit, 8_672);
    }

    #[test]
    fn an_exhausted_context_is_rejected_instead_of_requesting_one_token() {
        // Arrange: the prompt already fills the window.

        // Act
        let error =
            max_output_tokens_for_request(32_768, 40_000, 16_384, 1).expect_err("no output room");

        // Assert
        assert_eq!(
            error,
            OutputCapacityError::InsufficientRoom {
                context_window: 32_768,
                prompt_tokens: 40_000,
                minimum_output_tokens: 1,
            }
        );
    }

    #[test]
    fn a_configured_limit_below_the_api_minimum_is_rejected() {
        // Arrange: the Responses API requires at least 16 tokens, and the
        // caller's intended limit is smaller than that.

        // Act
        let error = max_output_tokens_for_request(32_768, 20_000, 8, 16)
            .expect_err("the configured limit must not be exceeded");

        // Assert
        assert_eq!(
            error,
            OutputCapacityError::BelowApiMinimum {
                intended: 8,
                minimum_output_tokens: 16,
            }
        );
    }

    #[test]
    fn a_small_window_keeps_half_of_its_remaining_room() {
        // Arrange: the fixed safety reserve would consume this window.

        // Act
        let limit = max_output_tokens_for_request(1_000, 800, 500, 1)
            .expect("half of the remaining room is usable");

        // Assert
        assert_eq!(limit, 100);
    }
}
