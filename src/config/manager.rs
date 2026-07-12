//! ConfigManager: immutable configuration snapshots with revision/fingerprint.
//!
//! * `ConfigSnapshot` — an immutable point-in-time view of validated config
//! * `ConfigManager` — owns the current snapshot
//! * Fingerprint computed from the config file content (SHA-256)
//! * Monotonically increasing revision
//!
//! A Turn acquires `Arc<ConfigSnapshot>` at start time and holds it until
//! completion, preventing generation-mixing when config changes mid-flight.

use std::path::Path;
use std::sync::Arc;
use std::sync::RwLock;

use sha2::{Digest, Sha256};

use super::Config;

/// Immutable snapshot of the configuration at a specific revision.
///
/// Created once when `ConfigManager` is initialized (or swapped) and never
/// mutated.  Turns should hold `Arc<ConfigSnapshot>` for their lifetime.
#[derive(Clone, Debug)]
pub(crate) struct ConfigSnapshot {
    /// Monotonically increasing generation number (1, 2, 3, …).
    pub revision: u64,

    /// SHA-256 hex digest of the config source at the time of snapshotting.
    /// Used to detect whether the config has changed since a Turn started.
    pub fingerprint: String,

    /// The validated configuration.
    pub config: Config,
}

impl ConfigSnapshot {
    /// Builds a snapshot from a validated `Config`.
    ///
    /// When `config_path` is present the **file content** is hashed, so any
    /// edit to the YAML on disk produces a different fingerprint even if the
    /// parsed `Config` happens to be equivalent.  This is simpler than a full
    /// `Serialize` derive for every config sub-struct and more stable than
    /// `Debug` output.
    ///
    /// When no path is given a fallback deterministic hash of the config
    /// fields is used.
    pub(crate) fn new(revision: u64, config: Config, config_path: Option<&Path>) -> Self {
        let fingerprint = match config_path {
            Some(path) => match std::fs::read_to_string(path) {
                Ok(content) => sha256_hex(content.as_bytes()),
                Err(_) => fallback_fingerprint(&config),
            },
            None => fallback_fingerprint(&config),
        };
        Self {
            revision,
            fingerprint,
            config,
        }
    }
}

/// Computes a SHA-256 hex digest of arbitrary bytes.
fn sha256_hex(bytes: &[u8]) -> String {
    let hash = Sha256::digest(bytes);
    format!("{hash:x}")
}

/// Fallback fingerprint when the original file is unavailable.
///
/// Hashes the most stability-sensitive fields.  This is not exhaustive, but
/// sufficient for the fallback case (e.g. tests or in-memory builds).
fn fallback_fingerprint(config: &Config) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    config.default_provider.as_str().hash(&mut hasher);
    config.default_model.hash(&mut hasher);
    config.state_root.hash(&mut hasher);
    config.timezone.hash(&mut hasher);
    config.compaction_timeout_secs.hash(&mut hasher);
    config.max_history_messages.hash(&mut hasher);
    config.compact_keep_recent.hash(&mut hasher);
    config.default_context_window_tokens.hash(&mut hasher);
    config
        .compaction_threshold_ratio
        .to_bits()
        .hash(&mut hasher);
    config.compaction_target_ratio.to_bits().hash(&mut hasher);
    config.default_agent.as_str().hash(&mut hasher);
    for (k, v) in &config.providers {
        k.as_str().hash(&mut hasher);
        v.label.hash(&mut hasher);
        v.base_url.hash(&mut hasher);
        v.default_model.hash(&mut hasher);
    }
    sha256_hex(&hasher.finish().to_le_bytes())
}

/// Owns the current `ConfigSnapshot` and supports atomic swap.
///
/// `current()` / `current_blocking()` return a cheap `Arc` clone; callers
/// must not hold the read lock across await points.
pub(crate) struct ConfigManager {
    inner: RwLock<Arc<ConfigSnapshot>>,
}

impl ConfigManager {
    /// Initialises the manager with revision `1`.
    pub(crate) fn new(config: Config, config_path: Option<&Path>) -> Self {
        let snapshot = Arc::new(ConfigSnapshot::new(1, config, config_path));
        Self {
            inner: RwLock::new(snapshot),
        }
    }

    /// Returns a clone of the current snapshot reference.
    pub(crate) fn current_blocking(&self) -> Arc<ConfigSnapshot> {
        Arc::clone(&*self.inner.read().expect("ConfigManager lock"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_stores_revision_and_fingerprint() {
        let config = Config::load_allow_missing_api_key(None).expect("load default config");
        let snap = ConfigSnapshot::new(1, config.clone(), None);
        assert_eq!(snap.revision, 1);
        assert!(!snap.fingerprint.is_empty());
        assert_eq!(snap.config.state_root, config.state_root);
    }

    #[test]
    fn same_config_same_fallback_fingerprint() {
        let config = Config::load_allow_missing_api_key(None).expect("load default config");
        let a = ConfigSnapshot::new(1, config.clone(), None);
        let b = ConfigSnapshot::new(2, config, None);
        assert_eq!(
            a.fingerprint, b.fingerprint,
            "same config should yield same fingerprint"
        );
    }

    #[test]
    fn different_config_different_fallback_fingerprint() {
        let mut config_a = Config::load_allow_missing_api_key(None).expect("load default config");
        let config_b = config_a.clone();
        config_a.timezone = String::from("UTC+different");
        let a = ConfigSnapshot::new(1, config_a, None);
        let b = ConfigSnapshot::new(1, config_b, None);
        assert_ne!(
            a.fingerprint, b.fingerprint,
            "different config should yield different fingerprint"
        );
    }

    #[test]
    fn manager_returns_current_snapshot() {
        let config = Config::load_allow_missing_api_key(None).expect("load default config");
        let manager = ConfigManager::new(config, None);
        let snap = manager.current_blocking();
        assert_eq!(snap.revision, 1);
        assert!(!snap.fingerprint.is_empty());
    }
}
