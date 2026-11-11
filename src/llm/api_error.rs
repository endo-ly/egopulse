//! Provider error classification shared by every transport.
//!
//! Providers describe a rejected request with an HTTP status plus a structured
//! `code`/`type` and a human-readable message. Classifying the failure here
//! keeps provider vocabulary out of the callers that react to it.

use reqwest::StatusCode;

use crate::error::LlmError;

/// Statuses a provider may use to reject an oversized prompt.
const OVERFLOW_STATUSES: &[u16] = &[400, 413, 422];

/// Stable provider codes that mean the prompt exceeded the context window.
///
/// Ambiguous codes such as `max_tokens_exceeded` are left out: they also
/// describe a request whose output limit is too large, which compaction cannot
/// fix. Those still qualify through the message markers below.
const CONTEXT_OVERFLOW_CODES: &[&str] = &[
    "context_length_exceeded",
    "context_overflow",
    "request_too_large",
];

/// Message fragments providers use instead of a stable code.
const CONTEXT_OVERFLOW_MARKERS: &[&str] = &[
    "context_length_exceeded",
    "context window",
    "context_overflow",
    "maximum context length",
    "prompt is too long",
    "input is too long",
    "too many tokens",
    "reduce the length",
    "request too large",
];

/// Classifies a provider rejection into an [`LlmError`].
///
/// Used for both an HTTP error response and a failure reported inside a
/// streamed response.
///
/// A `400` alone is not enough to call it an overflow: malformed requests and
/// unsupported parameters return it too. The provider must name a context
/// limit, either in its structured `code`/`type` or in the message.
pub(crate) fn classify(
    status: StatusCode,
    code: Option<&str>,
    error_type: Option<&str>,
    message: &str,
    retry_after_secs: Option<u64>,
) -> LlmError {
    if is_context_overflow(status, code, error_type, message) {
        return LlmError::ContextOverflow {
            status,
            body_preview: crate::llm::preview_body(message),
        };
    }
    LlmError::ApiError {
        status,
        body_preview: crate::llm::preview_body(message),
        retry_after_secs,
    }
}

/// Returns `true` when the provider rejected the prompt for exceeding the
/// model's context window.
fn is_context_overflow(
    status: StatusCode,
    code: Option<&str>,
    error_type: Option<&str>,
    message: &str,
) -> bool {
    if !OVERFLOW_STATUSES.contains(&status.as_u16()) {
        return false;
    }
    let named_code = code
        .into_iter()
        .chain(error_type)
        .map(str::to_ascii_lowercase)
        .any(|value| CONTEXT_OVERFLOW_CODES.contains(&value.as_str()));
    if named_code {
        return true;
    }
    let message = message.to_ascii_lowercase();
    CONTEXT_OVERFLOW_MARKERS
        .iter()
        .any(|marker| message.contains(marker))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overflow_code_is_classified_without_a_marker_in_the_message() {
        // Arrange
        let status = StatusCode::BAD_REQUEST;

        // Act
        let error = classify(
            status,
            Some("context_length_exceeded"),
            None,
            "invalid request",
            None,
        );

        // Assert
        assert!(error.is_context_overflow());
        assert_eq!(
            error.to_string(),
            "llm_context_overflow: status=400 Bad Request body_preview=invalid request"
        );
    }

    #[test]
    fn overflow_marker_is_classified_without_a_code() {
        // Arrange
        let status = StatusCode::BAD_REQUEST;

        // Act
        let error = classify(
            status,
            None,
            Some("invalid_request_error"),
            "This model's maximum context length is 131072 tokens",
            None,
        );

        // Assert
        assert!(error.is_context_overflow());
    }

    #[test]
    fn other_bad_requests_stay_api_errors() {
        // Arrange
        let status = StatusCode::BAD_REQUEST;

        // Act
        let error = classify(
            status,
            Some("invalid_request_error"),
            None,
            "unknown parameter: temperature",
            None,
        );

        // Assert
        assert!(!error.is_context_overflow());
        assert!(!error.is_retryable());
    }

    #[test]
    fn overflow_markers_on_unrelated_statuses_are_not_classified() {
        // Arrange
        let status = StatusCode::TOO_MANY_REQUESTS;

        // Act
        let error = classify(
            status,
            None,
            None,
            "context_length_exceeded: slow down",
            Some(3),
        );

        // Assert
        assert!(!error.is_context_overflow());
        assert!(error.is_retryable());
        assert_eq!(error.retry_after_secs(), Some(3));
    }
}
