//! Runtime calibration for prompt token estimates.

use std::collections::HashMap;

use tokio::sync::RwLock;

const EMA_ALPHA: f64 = 0.3;
const MIN_FACTOR: f64 = 0.5;
const MAX_FACTOR: f64 = 3.0;

/// Conservative factor for unmeasured prompt estimates.
pub(crate) const DEFAULT_FACTOR: f64 = 1.6;

/// Identifies a prompt-estimation calibration bucket.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct CalibrationKey {
    /// LLM provider name.
    pub(crate) provider: String,
    /// LLM model name.
    pub(crate) model: String,
    /// Request path that produced the prompt, such as `agent_loop` or `compaction`.
    pub(crate) request_kind: &'static str,
    /// Whether the request payload included tool definitions.
    pub(crate) has_tools: bool,
}

impl CalibrationKey {
    /// Creates a key for one provider/model/request-shape bucket.
    pub(crate) fn new(
        provider: impl Into<String>,
        model: impl Into<String>,
        request_kind: &'static str,
        has_tools: bool,
    ) -> Self {
        Self {
            provider: provider.into(),
            model: model.into(),
            request_kind,
            has_tools,
        }
    }
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
}
