//! LLM request helper shared by sleep steps.
//!
//! Sleep runs unattended, so one transient provider failure should not cost the
//! night's step. The retry lives here instead of in the provider so every step
//! observes the same budget and logging.

use std::sync::Arc;
use std::time::Duration;

use tracing::warn;

use crate::llm::{LlmProvider, Message, MessagesResponse};

use super::SleepBatchError;

/// Attempts per request, including the first.
const MAX_ATTEMPTS: usize = 3;

/// Total time one logical request may take, including all attempts and waits.
///
/// One attempt may use the transport's 600s cap (a full memory rewrite), so
/// this budget leaves room for one full retry while still bounding how long a
/// repeatedly failing request can hold the sequential sleep scheduler.
const MAX_REQUEST_DURATION: Duration = Duration::from_secs(1_200);

/// Longest server-supplied `Retry-After` worth honoring.
///
/// When the provider asks for a longer wait, retrying earlier would only hit
/// the same limit again, so the request fails instead of sleeping and retrying
/// too soon. The bound also keeps an unattended batch from being parked for an
/// arbitrary span by a single header.
const MAX_RETRY_AFTER_SECS: u64 = 120;

/// Sends one request, retrying transient transport failures.
///
/// Retryable failures are network errors/timeouts (before or during the
/// response body) and 429/5xx responses (`LlmError::is_retryable`). A
/// `Retry-After` longer than [`MAX_RETRY_AFTER_SECS`] ends the request instead
/// of retrying early; otherwise the delay backs off exponentially with jitter.
/// Every attempt and wait shares [`MAX_REQUEST_DURATION`], so a request that
/// keeps failing cannot occupy the scheduler indefinitely. When the budget is
/// exhausted, or the failure is deterministic, the error is returned as
/// [`SleepBatchError::Llm`] for the step to record.
pub(super) async fn send(
    provider: &Arc<dyn LlmProvider>,
    agent_id: &str,
    system_prompt: &str,
    messages: Arc<Vec<Message>>,
    step: &'static str,
) -> Result<MessagesResponse, SleepBatchError> {
    let deadline = tokio::time::Instant::now() + MAX_REQUEST_DURATION;
    let mut attempt = 1;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return Err(SleepBatchError::Llm(format!(
                "request exceeded the {}s budget",
                MAX_REQUEST_DURATION.as_secs()
            )));
        }
        let error = match tokio::time::timeout(
            remaining,
            provider.send_message(system_prompt, Arc::clone(&messages), None),
        )
        .await
        {
            Ok(Ok(response)) => return Ok(response),
            Ok(Err(error)) => error,
            Err(_elapsed) => {
                return Err(SleepBatchError::Llm(format!(
                    "no response within the {}s request budget",
                    MAX_REQUEST_DURATION.as_secs()
                )));
            }
        };

        if !error.is_retryable() || attempt >= MAX_ATTEMPTS {
            return Err(SleepBatchError::Llm(error.to_string()));
        }
        if let Some(secs) = error.retry_after_secs()
            && secs > MAX_RETRY_AFTER_SECS
        {
            warn!(
                agent_id = %agent_id,
                step,
                provider = provider.provider_name(),
                attempt,
                max_attempts = MAX_ATTEMPTS,
                retry_after_secs = secs,
                max_retry_after_secs = MAX_RETRY_AFTER_SECS,
                error = %error,
                "sleep LLM request asked to wait beyond the retry cap; giving up"
            );
            return Err(SleepBatchError::Llm(error.to_string()));
        }

        let delay = crate::llm::backoff(attempt, error.retry_after_secs());
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if delay >= remaining {
            warn!(
                agent_id = %agent_id,
                step,
                provider = provider.provider_name(),
                attempt,
                max_attempts = MAX_ATTEMPTS,
                delay_ms = delay.as_millis() as u64,
                error = %error,
                "sleep LLM retry budget exhausted; giving up"
            );
            return Err(SleepBatchError::Llm(error.to_string()));
        }

        warn!(
            agent_id = %agent_id,
            step,
            provider = provider.provider_name(),
            attempt,
            max_attempts = MAX_ATTEMPTS,
            delay_ms = delay.as_millis() as u64,
            error = %error,
            "sleep LLM request failed; retrying"
        );
        tokio::time::sleep(delay).await;
        attempt += 1;
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use async_trait::async_trait;
    use reqwest::StatusCode;

    use crate::error::LlmError;
    use crate::llm::{LlmProvider, Message, MessagesResponse, ToolDefinition};

    use super::{MAX_ATTEMPTS, MAX_REQUEST_DURATION, send};

    enum Failure {
        Transient { times: usize },
        Deterministic,
        RateLimited { retry_after_secs: Option<u64> },
        Slow { duration: Duration },
    }

    struct ScriptedProvider {
        failure: Failure,
        attempts: AtomicUsize,
    }

    impl ScriptedProvider {
        fn new(failure: Failure) -> Self {
            Self {
                failure,
                attempts: AtomicUsize::new(0),
            }
        }

        fn attempts(&self) -> usize {
            self.attempts.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl LlmProvider for ScriptedProvider {
        fn provider_name(&self) -> &str {
            "scripted"
        }

        fn model_name(&self) -> &str {
            "scripted-model"
        }

        async fn send_message(
            &self,
            _system: &str,
            _messages: Arc<Vec<Message>>,
            _tools: Option<Arc<Vec<ToolDefinition>>>,
        ) -> Result<MessagesResponse, LlmError> {
            let attempt = self.attempts.fetch_add(1, Ordering::SeqCst) + 1;
            match self.failure {
                Failure::Transient { times } if attempt <= times => Err(LlmError::ApiError {
                    status: StatusCode::SERVICE_UNAVAILABLE,
                    body_preview: String::new(),
                    retry_after_secs: None,
                }),
                Failure::Transient { .. } => Ok(MessagesResponse {
                    content: "ok".to_string(),
                    reasoning_content: None,
                    tool_calls: Vec::new(),
                    usage: None,
                }),
                Failure::Deterministic => {
                    Err(LlmError::InvalidResponse("deterministic".to_string()))
                }
                Failure::RateLimited { retry_after_secs } => Err(LlmError::ApiError {
                    status: StatusCode::TOO_MANY_REQUESTS,
                    body_preview: String::new(),
                    retry_after_secs,
                }),
                Failure::Slow { duration } => {
                    tokio::time::sleep(duration).await;
                    Err(LlmError::ApiError {
                        status: StatusCode::SERVICE_UNAVAILABLE,
                        body_preview: String::new(),
                        retry_after_secs: None,
                    })
                }
            }
        }

        async fn send_message_streaming(
            &self,
            system: &str,
            messages: Arc<Vec<Message>>,
            tools: Option<Arc<Vec<ToolDefinition>>>,
            _on_delta: &(dyn Fn(String) + Send + Sync),
        ) -> Result<MessagesResponse, LlmError> {
            self.send_message(system, messages, tools).await
        }
    }

    fn provider(failure: Failure) -> Arc<ScriptedProvider> {
        Arc::new(ScriptedProvider::new(failure))
    }

    fn request() -> Arc<Vec<Message>> {
        Arc::new(vec![Message::text("user", "hello")])
    }

    #[tokio::test(start_paused = true)]
    async fn retries_transient_failures_until_success() {
        // Arrange
        let provider = provider(Failure::Transient { times: 2 });

        // Act
        let result = send(
            &(provider.clone() as Arc<dyn LlmProvider>),
            "test-agent",
            "",
            request(),
            "test",
        )
        .await;

        // Assert
        assert_eq!(result.expect("response").content, "ok");
        assert_eq!(provider.attempts(), 3);
    }

    #[tokio::test(start_paused = true)]
    async fn stops_after_the_attempt_budget() {
        // Arrange
        let provider = provider(Failure::Transient { times: usize::MAX });

        // Act
        let result = send(
            &(provider.clone() as Arc<dyn LlmProvider>),
            "test-agent",
            "",
            request(),
            "test",
        )
        .await;

        // Assert
        assert!(result.is_err());
        assert_eq!(provider.attempts(), MAX_ATTEMPTS);
    }

    #[tokio::test(start_paused = true)]
    async fn stops_after_the_attempt_budget_on_repeated_429() {
        // Arrange
        let provider = provider(Failure::RateLimited {
            retry_after_secs: None,
        });

        // Act
        let result = send(
            &(provider.clone() as Arc<dyn LlmProvider>),
            "test-agent",
            "",
            request(),
            "test",
        )
        .await;

        // Assert
        assert!(result.is_err());
        assert_eq!(provider.attempts(), MAX_ATTEMPTS);
    }

    #[tokio::test(start_paused = true)]
    async fn does_not_retry_when_retry_after_exceeds_the_cap() {
        // Arrange
        let provider = provider(Failure::RateLimited {
            retry_after_secs: Some(600),
        });

        // Act
        let result = send(
            &(provider.clone() as Arc<dyn LlmProvider>),
            "test-agent",
            "",
            request(),
            "test",
        )
        .await;

        // Assert
        assert!(result.is_err());
        assert_eq!(provider.attempts(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn stops_when_the_total_request_budget_is_exhausted() {
        // Arrange: each attempt is slower than half the budget, so the second
        // attempt is cut off instead of running to completion.
        let provider = provider(Failure::Slow {
            duration: Duration::from_secs(700),
        });
        let started = tokio::time::Instant::now();

        // Act
        let result = send(
            &(provider.clone() as Arc<dyn LlmProvider>),
            "test-agent",
            "",
            request(),
            "test",
        )
        .await;

        // Assert
        let error = result.expect_err("budget exhausted");
        assert!(error.to_string().contains("request budget"), "{error}");
        assert_eq!(provider.attempts(), 2);
        assert_eq!(started.elapsed(), MAX_REQUEST_DURATION);
    }

    #[tokio::test(start_paused = true)]
    async fn does_not_retry_deterministic_failures() {
        // Arrange
        let provider = provider(Failure::Deterministic);

        // Act
        let result = send(
            &(provider.clone() as Arc<dyn LlmProvider>),
            "test-agent",
            "",
            request(),
            "test",
        )
        .await;

        // Assert
        assert!(result.is_err());
        assert_eq!(provider.attempts(), 1);
    }
}
