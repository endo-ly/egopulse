//! Configuration source polling and hot-reload orchestration.

use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::error::{ConfigError, EgoPulseError};
use crate::runtime::AppState;

const POLL_INTERVAL: Duration = Duration::from_millis(250);
// Two polling intervals keep the debounce window aligned with observation ticks.
const DEBOUNCE: Duration = Duration::from_millis(500);

struct ReloadDebouncer {
    observed: Option<String>,
    pending: Option<(String, Instant)>,
}

impl ReloadDebouncer {
    fn new(initial: String) -> Self {
        Self {
            observed: Some(initial),
            pending: None,
        }
    }

    fn observe(&mut self, current: &str, fingerprint: &str, now: Instant) -> bool {
        if fingerprint == current {
            self.observed = Some(fingerprint.to_string());
            self.pending = None;
            return false;
        }

        if self.observed.as_deref() != Some(fingerprint) {
            self.observed = Some(fingerprint.to_string());
            self.pending = Some((fingerprint.to_string(), now));
        }

        self.pending.as_ref().is_some_and(|(candidate, since)| {
            candidate == fingerprint && now.duration_since(*since) >= DEBOUNCE
        })
    }

    fn clear_pending(&mut self) {
        self.pending = None;
    }

    fn retry_pending(&mut self, now: Instant) {
        if let Some((_, since)) = self.pending.as_mut() {
            *since = now;
        }
    }

    fn source_unavailable(&mut self) -> bool {
        self.pending = None;
        self.observed.take().is_some()
    }
}

/// Watches the configuration source and applies stable edits through the
/// shared [`crate::config::ConfigManager`] update boundary.
pub(crate) async fn run_config_reload_loop(
    state: Arc<AppState>,
    shutdown: CancellationToken,
) -> Result<(), EgoPulseError> {
    let Some(path) = state.config_path.clone() else {
        return Ok(());
    };

    let initial = state.config_manager.current_blocking().fingerprint.clone();
    let mut debouncer = ReloadDebouncer::new(initial);
    let mut interval = tokio::time::interval(POLL_INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            _ = shutdown.cancelled() => {
                info!("config reload watcher: shutdown requested, exiting loop");
                return Ok(());
            }
            _ = interval.tick() => {
                let fingerprint = match crate::config::manager::source_fingerprint(&path) {
                    Ok(value) => value,
                    Err(error) => {
                        if debouncer.source_unavailable() {
                            warn!(error = %error, "config reload watcher: source became unavailable");
                        }
                        continue;
                    }
                };

                let now = Instant::now();
                let current = state.config_manager.current_blocking();
                if !debouncer.observe(
                    &current.fingerprint,
                    &fingerprint,
                    now,
                ) {
                    continue;
                }

                match state.config_manager.reload_from_file() {
                    Ok(snapshot) => {
                        if snapshot.fingerprint == fingerprint {
                            info!(
                                revision = snapshot.revision,
                                "configuration reloaded from file"
                            );
                        } else {
                            debug!(
                                revision = snapshot.revision,
                                observed_fingerprint = %fingerprint,
                                active_fingerprint = %snapshot.fingerprint,
                                "config reload watcher: source changed during reload; re-observing"
                            );
                        }
                    }
                    Err(error) => {
                        warn!(error = %error, "config reload watcher: rejected file change");
                        if is_retryable_reload_error(&error) {
                            debouncer.retry_pending(now);
                            continue;
                        }
                    }
                }
                debouncer.clear_pending();
            }
        }
    }
}

fn is_retryable_reload_error(error: &EgoPulseError) -> bool {
    matches!(
        error,
        EgoPulseError::Config(
            ConfigError::ConfigReadFailed { .. } | ConfigError::ConfigSourceChangedDuringReload
        ) | EgoPulseError::Internal(_)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_edit_reloads_only_after_debounce_window() {
        let start = Instant::now();
        let mut debouncer = ReloadDebouncer::new("initial".to_string());

        assert!(!debouncer.observe("initial", "edited", start));
        assert!(!debouncer.observe(
            "initial",
            "edited",
            start + DEBOUNCE - Duration::from_millis(1)
        ));
        assert!(debouncer.observe("initial", "edited", start + DEBOUNCE));
    }

    #[test]
    fn reverted_edit_clears_pending_reload() {
        let start = Instant::now();
        let mut debouncer = ReloadDebouncer::new("initial".to_string());

        assert!(!debouncer.observe("initial", "edited", start));
        assert!(!debouncer.observe("initial", "initial", start + DEBOUNCE));
        assert!(!debouncer.observe(
            "initial",
            "edited",
            start + DEBOUNCE + Duration::from_millis(1)
        ));
    }

    #[test]
    fn reload_error_does_not_retry_same_fingerprint() {
        let start = Instant::now();
        let mut debouncer = ReloadDebouncer::new("initial".to_string());

        assert!(!debouncer.observe("initial", "edited", start));
        assert!(debouncer.observe("initial", "edited", start + DEBOUNCE));
        debouncer.clear_pending();
        assert!(!debouncer.observe(
            "initial",
            "edited",
            start + DEBOUNCE + Duration::from_secs(1)
        ));
    }

    #[test]
    fn transient_reload_error_retries_after_another_stable_window() {
        let start = Instant::now();
        let mut debouncer = ReloadDebouncer::new("initial".to_string());

        assert!(!debouncer.observe("initial", "edited", start));
        assert!(debouncer.observe("initial", "edited", start + DEBOUNCE));
        debouncer.retry_pending(start + DEBOUNCE);
        assert!(!debouncer.observe(
            "initial",
            "edited",
            start + DEBOUNCE + Duration::from_millis(1)
        ));
        assert!(debouncer.observe("initial", "edited", start + DEBOUNCE * 2));
    }

    #[test]
    fn reload_error_retry_classification_keeps_validation_errors_terminal() {
        let read_error = EgoPulseError::Config(ConfigError::ConfigReadFailed {
            path: std::path::PathBuf::from("config.yaml"),
            source: std::io::Error::other("temporary read failure"),
        });
        let validation_error = EgoPulseError::Config(ConfigError::ConfigParseFailed {
            path: std::path::PathBuf::from("config.yaml"),
            detail: "invalid".to_string(),
        });

        assert!(is_retryable_reload_error(&read_error));
        assert!(!is_retryable_reload_error(&validation_error));
    }

    #[test]
    fn source_unavailability_clears_observed_state() {
        let mut debouncer = ReloadDebouncer::new("initial".to_string());

        assert!(debouncer.source_unavailable());
        assert!(!debouncer.source_unavailable());
    }
}
