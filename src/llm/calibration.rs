//! Runtime calibration for prompt token estimates.

use std::collections::HashMap;

use tokio::sync::RwLock;

const EMA_ALPHA: f64 = 0.3;
const MIN_FACTOR: f64 = 0.5;
const MAX_FACTOR: f64 = 3.0;

/// Conservative factor for unmeasured prompt estimates.
pub(crate) const DEFAULT_FACTOR: f64 = 1.6;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct CalibrationKey {
    pub(crate) provider: String,
    pub(crate) model: String,
    pub(crate) request_kind: &'static str,
    pub(crate) has_tools: bool,
}

impl CalibrationKey {
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

#[derive(Default)]
pub(crate) struct UsageCalibrator {
    factors: RwLock<HashMap<CalibrationKey, f64>>,
}

impl UsageCalibrator {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) async fn factor(&self, key: &CalibrationKey) -> f64 {
        self.factors
            .read()
            .await
            .get(key)
            .copied()
            .unwrap_or(DEFAULT_FACTOR)
    }

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
        let calibrator = UsageCalibrator::new();
        let key = key("agent_loop", true);

        calibrator.record(key.clone(), 100, 200).await;

        assert!(calibrator.factor(&key).await > DEFAULT_FACTOR);
    }

    #[tokio::test]
    async fn ema_moves_gradually_toward_repeated_observations() {
        let calibrator = UsageCalibrator::new();
        let key = key("agent_loop", true);

        calibrator.record(key.clone(), 100, 300).await;
        let first = calibrator.factor(&key).await;
        calibrator.record(key.clone(), 100, 300).await;
        let second = calibrator.factor(&key).await;

        assert!(first > DEFAULT_FACTOR);
        assert!(first < MAX_FACTOR);
        assert!(second > first);
        assert!(second < MAX_FACTOR);
    }

    #[tokio::test]
    async fn record_clips_factor_to_bounds() {
        let calibrator = UsageCalibrator::new();
        let upper_key = key("agent_loop", true);
        let lower_key = key("compaction", false);

        for _ in 0..20 {
            calibrator.record(upper_key.clone(), 1, 100).await;
            calibrator.record(lower_key.clone(), 100, 1).await;
        }

        let upper = calibrator.factor(&upper_key).await;
        let lower = calibrator.factor(&lower_key).await;
        assert!(upper <= MAX_FACTOR);
        assert!(upper > MAX_FACTOR - 0.01);
        assert!(lower >= MIN_FACTOR);
        assert!(lower < MIN_FACTOR + 0.01);
    }

    #[tokio::test]
    async fn record_ignores_zero_estimate_or_non_positive_actual() {
        let calibrator = UsageCalibrator::new();
        let key = key("agent_loop", true);

        calibrator.record(key.clone(), 0, 100).await;
        calibrator.record(key.clone(), 100, 0).await;
        calibrator.record(key.clone(), 100, -1).await;

        assert_eq!(calibrator.factor(&key).await, DEFAULT_FACTOR);
    }

    #[tokio::test]
    async fn factor_returns_default_for_unmeasured_key() {
        let calibrator = UsageCalibrator::new();

        assert_eq!(
            calibrator.factor(&key("agent_loop", true)).await,
            DEFAULT_FACTOR
        );
    }

    #[tokio::test]
    async fn key_keeps_request_kind_and_tool_shape_separate() {
        let calibrator = UsageCalibrator::new();
        let agent_key = key("agent_loop", true);
        let compaction_key = key("compaction", false);

        calibrator.record(agent_key.clone(), 100, 300).await;

        assert!(calibrator.factor(&agent_key).await > DEFAULT_FACTOR);
        assert_eq!(calibrator.factor(&compaction_key).await, DEFAULT_FACTOR);
    }
}
