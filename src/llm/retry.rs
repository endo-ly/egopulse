//! Backoff policy shared by LLM transport retries.

use std::time::Duration;

/// Base backoff (milliseconds) for exponential LLM retry.
const BASE_BACKOFF_MS: u64 = 500;

/// Calculates the delay before the next LLM transport retry.
///
/// A server-supplied `Retry-After` is honored verbatim. Otherwise the delay is
/// exponentially backed off and randomized: several callers can hit the same
/// rate limit at once, and a fixed delay would retry them in lockstep.
///
/// `attempt` is 1-based: the first retry passes 1 and waits at most
/// [`BASE_BACKOFF_MS`].
pub(crate) fn backoff(attempt: usize, retry_after_secs: Option<u64>) -> Duration {
    if let Some(secs) = retry_after_secs {
        return Duration::from_secs(secs);
    }
    let exponent = attempt.saturating_sub(1) as u32;
    let ceiling_ms = BASE_BACKOFF_MS.saturating_mul(2u64.saturating_pow(exponent));
    Duration::from_millis(rand::random_range(0..=ceiling_ms))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn honors_server_retry_after_verbatim() {
        assert_eq!(backoff(1, Some(7)), Duration::from_secs(7));
        assert_eq!(backoff(5, Some(30)), Duration::from_secs(30));
    }

    #[test]
    fn jitter_stays_within_the_exponential_ceiling() {
        assert!(backoff(1, None) <= Duration::from_millis(500));
        assert!(backoff(2, None) <= Duration::from_millis(1_000));
        assert!(backoff(3, None) <= Duration::from_millis(2_000));
    }
}
