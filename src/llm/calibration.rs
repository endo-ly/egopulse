//! Runtime calibration for prompt token estimates.

use std::collections::{HashMap, hash_map::Entry};

use tokio::sync::RwLock;

const EMA_ALPHA: f64 = 0.3;
const MIN_FACTOR: f64 = 0.5;
const MAX_FACTOR: f64 = 3.0;

/// Conservative factor for unmeasured prompt estimates.
///
/// Slightly above 1.0 so unknown provider/model combinations over-estimate
/// until the first observation arrives. Calibration factors reconstructed
/// from persisted observations replace this default after startup, so it only
/// applies to the very first turn of a genuinely unseen key.
pub(crate) const DEFAULT_FACTOR: f64 = 1.3;

/// Identifies a prompt-estimation calibration bucket.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct CalibrationKey {
    /// LLM provider name.
    pub(crate) provider: String,
    /// LLM model name.
    pub(crate) model: String,
    /// Request path that produced the prompt, such as `agent_loop` or `compaction`.
    pub(crate) request_kind: String,
    /// Whether the request payload included tool definitions.
    pub(crate) has_tools: bool,
}

impl CalibrationKey {
    /// Creates a key for one provider/model/request-shape bucket.
    pub(crate) fn new(
        provider: impl Into<String>,
        model: impl Into<String>,
        request_kind: impl Into<String>,
        has_tools: bool,
    ) -> Self {
        Self {
            provider: provider.into(),
            model: model.into(),
            request_kind: request_kind.into(),
            has_tools,
        }
    }
}

/// A persisted observation used to rebuild calibration factors on startup.
///
/// Pairs the raw prompt estimate (`estimated_tokens`, chars/3) with the
/// actual input token count (`input_tokens`) reported by the provider for one
/// LLM call. The ratio is the measured correction factor for that call.
/// `created_at` lets the caller merge observations from multiple databases
/// in true chronological order before replaying the EMA.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CalibrationObservation {
    /// LLM provider name.
    pub(crate) provider: String,
    /// LLM model name.
    pub(crate) model: String,
    /// Request path that produced the prompt.
    pub(crate) request_kind: String,
    /// Whether the request payload included tool definitions.
    pub(crate) has_tools: bool,
    /// Raw prompt estimate (chars / 3) for the call.
    pub(crate) estimated_tokens: usize,
    /// Actual input token count reported by the provider.
    pub(crate) input_tokens: i64,
    /// RFC3339 timestamp of the call, used only to order observations.
    pub(crate) created_at: String,
}

/// Learns correction factors between raw prompt estimates and observed usage.
#[derive(Default)]
pub(crate) struct UsageCalibrator {
    factors: RwLock<HashMap<CalibrationKey, f64>>,
}

impl UsageCalibrator {
    /// Creates an empty in-memory calibrator.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Returns the current factor for `key`, or [`DEFAULT_FACTOR`] before measurement.
    pub(crate) async fn factor(&self, key: &CalibrationKey) -> f64 {
        self.factors
            .read()
            .await
            .get(key)
            .copied()
            .unwrap_or(DEFAULT_FACTOR)
    }

    /// Records an observed input token count for one raw prompt estimate.
    pub(crate) async fn record(&self, key: CalibrationKey, estimated: usize, actual: i64) {
        if estimated == 0 || actual <= 0 {
            return;
        }

        let observed = (actual as f64 / estimated as f64).clamp(MIN_FACTOR, MAX_FACTOR);
        let mut factors = self.factors.write().await;
        let current = factors.get(&key).copied().unwrap_or(DEFAULT_FACTOR);
        let updated =
            (current * (1.0 - EMA_ALPHA) + observed * EMA_ALPHA).clamp(MIN_FACTOR, MAX_FACTOR);
        factors.insert(key, updated);
    }

    /// Rebuilds factors from persisted observations.
    ///
    /// `observations` must already be ordered oldest-first within each key
    /// (the storage layer enforces this). Each key's history is replayed
    /// through the same EMA used by `record`, so the resulting factors match
    /// what in-process `record` calls would have produced. This lets restarts
    /// transparently restore the learned calibration state.
    ///
    /// Observations with non-positive `estimated_tokens` or `input_tokens` are
    /// skipped. All existing in-memory factors are replaced.
    pub(crate) async fn replay(&self, observations: &[CalibrationObservation]) {
        let mut grouped: HashMap<CalibrationKey, Vec<(usize, i64)>> = HashMap::new();
        for obs in observations {
            if obs.estimated_tokens == 0 || obs.input_tokens <= 0 {
                continue;
            }
            let key = CalibrationKey {
                provider: obs.provider.clone(),
                model: obs.model.clone(),
                request_kind: obs.request_kind.clone(),
                has_tools: obs.has_tools,
            };
            match grouped.entry(key) {
                Entry::Occupied(mut entry) => {
                    entry
                        .get_mut()
                        .push((obs.estimated_tokens, obs.input_tokens));
                }
                Entry::Vacant(entry) => {
                    entry.insert(vec![(obs.estimated_tokens, obs.input_tokens)]);
                }
            }
        }

        let mut factors = self.factors.write().await;
        factors.clear();
        for (key, pairs) in grouped {
            let mut factor = DEFAULT_FACTOR;
            for (estimated, actual) in pairs {
                let observed = (actual as f64 / estimated as f64).clamp(MIN_FACTOR, MAX_FACTOR);
                factor = (factor * (1.0 - EMA_ALPHA) + observed * EMA_ALPHA)
                    .clamp(MIN_FACTOR, MAX_FACTOR);
            }
            factors.insert(key, factor);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(request_kind: &'static str, has_tools: bool) -> CalibrationKey {
        CalibrationKey::new("provider", "model", request_kind, has_tools)
    }

    #[tokio::test]
    async fn record_raises_factor_when_actual_exceeds_estimate() {
        // Arrange
        let calibrator = UsageCalibrator::new();
        let key = key("agent_loop", true);

        // Act
        calibrator.record(key.clone(), 100, 200).await;

        // Assert
        assert!(calibrator.factor(&key).await > DEFAULT_FACTOR);
    }

    #[tokio::test]
    async fn ema_moves_gradually_toward_repeated_observations() {
        // Arrange
        let calibrator = UsageCalibrator::new();
        let key = key("agent_loop", true);

        // Act
        calibrator.record(key.clone(), 100, 300).await;
        let first = calibrator.factor(&key).await;
        calibrator.record(key.clone(), 100, 300).await;
        let second = calibrator.factor(&key).await;

        // Assert
        assert!(first > DEFAULT_FACTOR);
        assert!(first < MAX_FACTOR);
        assert!(second > first);
        assert!(second < MAX_FACTOR);
    }

    #[tokio::test]
    async fn record_clips_factor_to_bounds() {
        // Arrange
        let calibrator = UsageCalibrator::new();
        let upper_key = key("agent_loop", true);
        let lower_key = key("compaction", false);

        // Act
        for _ in 0..20 {
            calibrator.record(upper_key.clone(), 1, 100).await;
            calibrator.record(lower_key.clone(), 100, 1).await;
        }

        // Assert
        let upper = calibrator.factor(&upper_key).await;
        let lower = calibrator.factor(&lower_key).await;
        assert!(upper <= MAX_FACTOR);
        assert!(upper > MAX_FACTOR - 0.01);
        assert!(lower >= MIN_FACTOR);
        assert!(lower < MIN_FACTOR + 0.01);
    }

    #[tokio::test]
    async fn record_ignores_zero_estimate_or_non_positive_actual() {
        // Arrange
        let calibrator = UsageCalibrator::new();
        let key = key("agent_loop", true);

        // Act
        calibrator.record(key.clone(), 0, 100).await;
        calibrator.record(key.clone(), 100, 0).await;
        calibrator.record(key.clone(), 100, -1).await;

        // Assert
        assert_eq!(calibrator.factor(&key).await, DEFAULT_FACTOR);
    }

    #[tokio::test]
    async fn factor_returns_default_for_unmeasured_key() {
        // Arrange
        let calibrator = UsageCalibrator::new();

        // Act + Assert
        assert_eq!(
            calibrator.factor(&key("agent_loop", true)).await,
            DEFAULT_FACTOR
        );
    }

    #[tokio::test]
    async fn key_keeps_request_kind_and_tool_shape_separate() {
        // Arrange
        let calibrator = UsageCalibrator::new();
        let agent_key = key("agent_loop", true);
        let compaction_key = key("compaction", false);

        // Act
        calibrator.record(agent_key.clone(), 100, 300).await;

        // Assert
        assert!(calibrator.factor(&agent_key).await > DEFAULT_FACTOR);
        assert_eq!(calibrator.factor(&compaction_key).await, DEFAULT_FACTOR);
    }

    fn observation(
        request_kind: &str,
        has_tools: bool,
        estimated: usize,
        input: i64,
    ) -> CalibrationObservation {
        CalibrationObservation {
            provider: "provider".into(),
            model: "model".into(),
            request_kind: request_kind.into(),
            has_tools,
            estimated_tokens: estimated,
            input_tokens: input,
            created_at: String::new(),
        }
    }

    #[tokio::test]
    async fn replay_rebuilds_factor_from_observations() {
        // Arrange
        let calibrator = UsageCalibrator::new();
        let observations = vec![
            observation("agent_loop", true, 100, 200),
            observation("agent_loop", true, 100, 300),
        ];

        // Act
        calibrator.replay(&observations).await;

        // Assert: converges above DEFAULT_FACTOR toward the observed ratios
        let factor = calibrator.factor(&key("agent_loop", true)).await;
        assert!(factor > DEFAULT_FACTOR);
        assert!(factor < MAX_FACTOR);
    }

    #[tokio::test]
    async fn replay_matches_incremental_record_for_same_history() {
        // Arrange: one calibrator fed via replay, one via incremental record
        let replayed = UsageCalibrator::new();
        let incremental = UsageCalibrator::new();
        let target = key("agent_loop", true);
        let observations = vec![
            observation("agent_loop", true, 100, 250),
            observation("agent_loop", true, 100, 250),
        ];

        // Act
        replayed.replay(&observations).await;
        incremental.record(target.clone(), 100, 250).await;
        incremental.record(target.clone(), 100, 250).await;

        // Assert: identical history produces identical factor
        assert_eq!(
            replayed.factor(&target).await,
            incremental.factor(&target).await
        );
    }

    #[tokio::test]
    async fn replay_skips_observations_with_non_positive_values() {
        // Arrange
        let calibrator = UsageCalibrator::new();
        let observations = vec![
            observation("agent_loop", true, 0, 200),
            observation("agent_loop", true, 100, 0),
        ];

        // Act
        calibrator.replay(&observations).await;

        // Assert: no valid observation leaves factor at default
        assert_eq!(
            calibrator.factor(&key("agent_loop", true)).await,
            DEFAULT_FACTOR
        );
    }

    #[tokio::test]
    async fn replay_with_empty_observations_resets_to_default() {
        // Arrange: seed a learned factor, then replay with nothing
        let calibrator = UsageCalibrator::new();
        let target = key("agent_loop", true);
        calibrator.record(target.clone(), 100, 300).await;
        assert_ne!(calibrator.factor(&target).await, DEFAULT_FACTOR);

        // Act
        calibrator.replay(&[]).await;

        // Assert: replay replaces all factors, so the learned value is gone
        assert_eq!(calibrator.factor(&target).await, DEFAULT_FACTOR);
    }
}
