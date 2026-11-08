//! Retry policy shared by every LLM call path.
//!
//! A provider performs exactly one HTTP attempt per call. Deciding whether to
//! send that request again belongs to the caller, because only the caller knows
//! its deadline, whether it already exposed partial output, and whether it may
//! retry safely. This module holds that shared decision plus the execution
//! helper for callers that only use the finished response.

use std::future::Future;
use std::time::Duration;

use tokio::time::Instant;

use crate::error::LlmError;

use super::MessagesResponse;

/// Base backoff (milliseconds) for exponential LLM retry.
const BASE_BACKOFF_MS: u64 = 500;

/// Attempts per logical request, including the first.
pub(crate) const MAX_ATTEMPTS: usize = 3;

/// Limits for retrying one logical LLM request.
pub(crate) struct RetryPolicy {
    /// Time by which the request must finish, including waits between attempts.
    pub(crate) deadline: Instant,
    /// Attempts including the first.
    pub(crate) max_attempts: usize,
    /// Whether the caller already exposed streamed output. Published output
    /// cannot be taken back, so a retry would duplicate it.
    pub(crate) output_published: bool,
    /// Caller-specific ceiling for a single wait. A server `Retry-After` above
    /// this is not honored either.
    pub(crate) max_backoff: Option<Duration>,
    /// Caller-specific ceiling for a server `Retry-After`. A longer request is
    /// failed instead of retried early: the provider is still limited.
    pub(crate) max_retry_after: Option<Duration>,
}

impl RetryPolicy {
    /// Creates a policy with the shared attempt cap and no caller limits.
    pub(crate) fn new(deadline: Instant) -> Self {
        Self {
            deadline,
            max_attempts: MAX_ATTEMPTS,
            output_published: false,
            max_backoff: None,
            max_retry_after: None,
        }
    }
}

/// Why a request stopped instead of retrying.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RetryStop {
    /// The failure is deterministic or the provider already produced output
    /// that must not be discarded.
    NotRetryable,
    /// The attempt budget is spent.
    AttemptsExhausted,
    /// The caller already exposed partial output; retrying would duplicate it.
    OutputPublished,
    /// The server asked to wait longer than the caller allows.
    RetryAfterTooLong,
    /// The caller's per-wait ceiling is smaller than the computed delay.
    BackoffTooLong,
    /// Waiting would leave no time before the caller's deadline.
    DeadlineExceeded,
}

/// What the caller should do after an attempt failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RetryDecision {
    Retry(Duration),
    Stop(RetryStop),
}

/// Calculates the delay before the next LLM transport retry.
///
/// A server-supplied `Retry-After` is honored verbatim. Otherwise the delay is
/// exponentially backed off and randomized: several callers can hit the same
/// rate limit at once, and a fixed delay would retry them in lockstep.
///
/// `attempt` is 1-based: the first retry passes 1 and waits at most
/// [`BASE_BACKOFF_MS`].
fn backoff(attempt: usize, retry_after_secs: Option<u64>) -> Duration {
    if let Some(secs) = retry_after_secs {
        return Duration::from_secs(secs);
    }
    let exponent = attempt.saturating_sub(1) as u32;
    let ceiling_ms = BASE_BACKOFF_MS.saturating_mul(2u64.saturating_pow(exponent));
    Duration::from_millis(rand::random_range(0..=ceiling_ms))
}

/// Decides whether `error` from `attempt` may be retried under `policy`.
///
/// The caller still owns the loop: it can record the attempt, publish state, or
/// stop for a reason not represented here before sleeping.
pub(crate) fn decide(error: &LlmError, attempt: usize, policy: &RetryPolicy) -> RetryDecision {
    if !error.is_retryable() {
        return RetryDecision::Stop(RetryStop::NotRetryable);
    }
    if policy.output_published {
        return RetryDecision::Stop(RetryStop::OutputPublished);
    }
    if attempt >= policy.max_attempts {
        return RetryDecision::Stop(RetryStop::AttemptsExhausted);
    }
    if let (Some(limit), Some(secs)) = (policy.max_retry_after, error.retry_after_secs())
        && Duration::from_secs(secs) > limit
    {
        return RetryDecision::Stop(RetryStop::RetryAfterTooLong);
    }
    let delay = backoff(attempt, error.retry_after_secs());
    if policy.max_backoff.is_some_and(|limit| delay > limit) {
        return RetryDecision::Stop(RetryStop::BackoffTooLong);
    }
    if delay >= policy.deadline.saturating_duration_since(Instant::now()) {
        return RetryDecision::Stop(RetryStop::DeadlineExceeded);
    }
    RetryDecision::Retry(delay)
}

/// The outcome of a request that ran out of attempts or time.
#[derive(Debug)]
pub(crate) struct RetryFailure {
    /// The last provider error. `None` when an attempt was cut off by the
    /// deadline before returning anything.
    pub(crate) error: Option<LlmError>,
    pub(crate) stop: RetryStop,
}

impl RetryFailure {
    /// Returns the last provider error, or a deadline error when no attempt
    /// finished.
    pub(crate) fn into_llm_error(self) -> LlmError {
        self.error.unwrap_or(LlmError::DeadlineExceeded)
    }
}

/// Sends one complete-response request, retrying under `policy`.
///
/// This is the single retry owner for callers that consume only the finished
/// response (sleep, compaction, one-shot ask). Every attempt is capped by the
/// remaining deadline, and the wait between attempts is bounded by the same
/// deadline, so retries never extend one logical request past `policy.deadline`.
/// Interrupted generations are discarded because only the returned response is
/// used.
pub(crate) async fn complete_response<F, Fut, G>(
    policy: RetryPolicy,
    mut request: F,
    mut on_retry: G,
) -> Result<MessagesResponse, RetryFailure>
where
    F: FnMut() -> Fut + Send,
    Fut: Future<Output = Result<MessagesResponse, LlmError>> + Send,
    G: FnMut(usize, Duration, &LlmError) + Send,
{
    let mut attempt = 1;
    loop {
        let remaining = policy.deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(RetryFailure {
                error: None,
                stop: RetryStop::DeadlineExceeded,
            });
        }
        match tokio::time::timeout(remaining, request()).await {
            Ok(Ok(response)) => return Ok(response),
            Ok(Err(error)) => match decide(&error, attempt, &policy) {
                RetryDecision::Retry(delay) => {
                    let waited = tokio::time::timeout_at(policy.deadline, async {
                        on_retry(attempt, delay, &error);
                        tokio::time::sleep(delay).await;
                    })
                    .await;
                    if waited.is_err() {
                        return Err(RetryFailure {
                            error: Some(error),
                            stop: RetryStop::DeadlineExceeded,
                        });
                    }
                    attempt += 1;
                }
                RetryDecision::Stop(stop) => {
                    return Err(RetryFailure {
                        error: Some(error),
                        stop,
                    });
                }
            },
            Err(_elapsed) => {
                return Err(RetryFailure {
                    error: None,
                    stop: RetryStop::DeadlineExceeded,
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use reqwest::StatusCode;

    use super::*;

    fn retryable_error() -> LlmError {
        LlmError::ApiError {
            status: StatusCode::SERVICE_UNAVAILABLE,
            body_preview: "provider error".to_string(),
            retry_after_secs: None,
        }
    }

    fn response(content: &str) -> MessagesResponse {
        MessagesResponse {
            content: content.to_string(),
            reasoning_content: None,
            tool_calls: Vec::new(),
            usage: None,
        }
    }

    #[test]
    fn retries_retryable_failures_within_the_attempt_budget() {
        // Arrange
        let policy = RetryPolicy::new(Instant::now() + Duration::from_secs(60));

        // Act + Assert
        assert!(matches!(
            decide(&retryable_error(), 1, &policy),
            RetryDecision::Retry(_)
        ));
        assert_eq!(
            decide(&retryable_error(), MAX_ATTEMPTS, &policy),
            RetryDecision::Stop(RetryStop::AttemptsExhausted)
        );
        assert_eq!(
            decide(
                &LlmError::InvalidResponse("deterministic".to_string()),
                1,
                &policy
            ),
            RetryDecision::Stop(RetryStop::NotRetryable)
        );
    }

    #[test]
    fn published_output_and_caller_limits_stop_the_retry() {
        // Arrange
        let deadline = Instant::now() + Duration::from_secs(60);
        let published = RetryPolicy {
            output_published: true,
            ..RetryPolicy::new(deadline)
        };
        let capped = RetryPolicy {
            max_retry_after: Some(Duration::from_secs(120)),
            ..RetryPolicy::new(deadline)
        };
        let rate_limited = LlmError::ApiError {
            status: StatusCode::TOO_MANY_REQUESTS,
            body_preview: "rate limited".to_string(),
            retry_after_secs: Some(600),
        };

        // Act + Assert
        assert_eq!(
            decide(&retryable_error(), 1, &published),
            RetryDecision::Stop(RetryStop::OutputPublished)
        );
        assert_eq!(
            decide(&rate_limited, 1, &capped),
            RetryDecision::Stop(RetryStop::RetryAfterTooLong)
        );
    }

    #[test]
    fn honors_the_server_retry_after_and_the_per_wait_cap() {
        // Arrange
        let deadline = Instant::now() + Duration::from_secs(3_600);
        let rate_limited = LlmError::ApiError {
            status: StatusCode::TOO_MANY_REQUESTS,
            body_preview: "rate limited".to_string(),
            retry_after_secs: Some(7),
        };
        let capped = RetryPolicy {
            max_backoff: Some(Duration::from_secs(5)),
            ..RetryPolicy::new(deadline)
        };

        // Act + Assert
        assert_eq!(
            decide(&rate_limited, 1, &RetryPolicy::new(deadline)),
            RetryDecision::Retry(Duration::from_secs(7))
        );
        assert_eq!(
            decide(&rate_limited, 1, &capped),
            RetryDecision::Stop(RetryStop::BackoffTooLong)
        );
    }

    #[test]
    fn a_wait_past_the_deadline_is_not_attempted() {
        // Arrange: the server asks for a delay longer than the remaining time.
        let policy = RetryPolicy::new(Instant::now() + Duration::from_secs(10));
        let rate_limited = LlmError::ApiError {
            status: StatusCode::TOO_MANY_REQUESTS,
            body_preview: "rate limited".to_string(),
            retry_after_secs: Some(30),
        };

        // Act + Assert
        assert_eq!(
            decide(&rate_limited, 1, &policy),
            RetryDecision::Stop(RetryStop::DeadlineExceeded)
        );
    }

    #[tokio::test(start_paused = true)]
    async fn complete_response_retries_until_success() {
        // Arrange: fail twice with a zero `Retry-After`, then succeed.
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_for_request = Arc::clone(&calls);
        let policy = RetryPolicy::new(Instant::now() + Duration::from_secs(60));

        // Act
        let result = complete_response(
            policy,
            move || {
                let calls = Arc::clone(&calls_for_request);
                async move {
                    if calls.fetch_add(1, Ordering::SeqCst) < 2 {
                        return Err(LlmError::ApiError {
                            status: StatusCode::TOO_MANY_REQUESTS,
                            body_preview: "rate limited".to_string(),
                            retry_after_secs: Some(0),
                        });
                    }
                    Ok(response("done"))
                }
            },
            |_, _, _| {},
        )
        .await;

        // Assert
        assert_eq!(result.expect("response").content, "done");
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    #[tokio::test(start_paused = true)]
    async fn complete_response_stops_at_the_deadline() {
        // Arrange: every attempt hangs, so the whole request is cut by the
        // deadline instead of multiplying the transport timeout.
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_for_request = Arc::clone(&calls);
        let started = Instant::now();
        let policy = RetryPolicy::new(started + Duration::from_secs(10));

        // Act
        let result = complete_response(
            policy,
            move || {
                let calls = Arc::clone(&calls_for_request);
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_secs(30)).await;
                    Ok(response("never"))
                }
            },
            |_, _, _| {},
        )
        .await;

        // Assert
        let failure = result.expect_err("deadline");
        assert!(failure.error.is_none());
        assert_eq!(failure.stop, RetryStop::DeadlineExceeded);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(started.elapsed(), Duration::from_secs(10));
    }
}
