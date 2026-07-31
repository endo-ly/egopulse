//! Configuration source polling and hot-reload orchestration.

use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::error::EgoPulseError;
use crate::runtime::AppState;

const POLL_INTERVAL: Duration = Duration::from_millis(250);
const DEBOUNCE: Duration = Duration::from_millis(300);

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
    let mut observed = Some(initial);
    let mut pending: Option<(String, Instant)> = None;
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
                        if observed.take().is_some() {
                            warn!(error = %error, "config reload watcher: source became unavailable");
                        }
                        pending = None;
                        continue;
                    }
                };

                if fingerprint == state.config_manager.current_blocking().fingerprint {
                    observed = Some(fingerprint);
                    pending = None;
                    continue;
                }

                if observed.as_deref() != Some(fingerprint.as_str()) {
                    observed = Some(fingerprint.clone());
                    pending = Some((fingerprint.clone(), Instant::now()));
                }

                let should_reload = pending.as_ref().is_some_and(|(candidate, since)| {
                    candidate == &fingerprint && since.elapsed() >= DEBOUNCE
                });
                if !should_reload {
                    continue;
                }

                match state.config_manager.reload_from_file() {
                    Ok(snapshot) => {
                        if snapshot.fingerprint == fingerprint {
                            info!(
                                revision = snapshot.revision,
                                "configuration reloaded from file"
                            );
                        }
                    }
                    Err(error) => {
                        warn!(error = %error, "config reload watcher: rejected file change");
                    }
                }
                pending = None;
            }
        }
    }
}
