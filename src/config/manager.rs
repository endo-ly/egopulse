//! ConfigManager: immutable configuration snapshots with revision/fingerprint.
//!
//! * `ConfigSnapshot` — an immutable point-in-time view of validated config
//! * `ConfigManager` — owns the current snapshot
//! * Fingerprint computed from the config file content (SHA-256)
//! * Monotonically increasing revision
//!
//! A Turn acquires `Arc<ConfigSnapshot>` at start time and holds it until
//! completion, preventing generation-mixing when config changes mid-flight.
//!
//! The snapshot is taken **once** at Turn start (see `TurnExecutor::run`) and
//! shared by both `accept_turn` and `prepare_turn`, so the fingerprint stored
//! in `turn_runs` and the snapshot actually used for Prompt/Provider
//! generation always belong to the same Config generation.
//!
//! `ConfigManager` serialises every update so persistence, snapshot exchange,
//! and notification form one update boundary. Turns keep the `Arc` acquired at
//! their start and therefore never observe a mixed configuration generation.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::{Mutex, RwLock};

use sha2::{Digest, Sha256};
use tokio::sync::watch;

use super::Config;
use crate::error::{ConfigError, EgoPulseError};

/// Notification payload sent after a configuration snapshot is exchanged.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ConfigChange {
    /// Monotonically increasing configuration revision.
    pub revision: u64,
    /// SHA-256 fingerprint of the persisted YAML source.
    pub fingerprint: String,
}

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
            Some(path) => match std::fs::read(path) {
                Ok(content) => sha256_hex(&content),
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        path = %path.display(),
                        "failed to read config file for fingerprinting; using fallback fingerprint"
                    );
                    fallback_fingerprint(&config)
                }
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

/// Returns the fingerprint of the persisted configuration source.
pub(crate) fn source_fingerprint(path: &Path) -> Result<String, ConfigError> {
    let content = std::fs::read(path).map_err(|source| ConfigError::ConfigReadFailed {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(sha256_hex(&content))
}

/// Owns the current `ConfigSnapshot`.
///
/// `current_blocking()` returns a cheap `Arc` clone; callers must not hold
/// the read lock across await points.
pub(crate) struct ConfigManager {
    inner: RwLock<Arc<ConfigSnapshot>>,
    update_lock: Mutex<()>,
    config_path: Option<PathBuf>,
    changes: watch::Sender<ConfigChange>,
}

impl ConfigManager {
    /// Initialises the manager with revision `1`.
    pub(crate) fn new(config: Config, config_path: Option<&Path>) -> Self {
        let snapshot = Arc::new(ConfigSnapshot::new(1, config, config_path));
        let change = ConfigChange {
            revision: snapshot.revision,
            fingerprint: snapshot.fingerprint.clone(),
        };
        let (changes, _) = watch::channel(change);
        Self {
            inner: RwLock::new(snapshot),
            update_lock: Mutex::new(()),
            config_path: config_path.map(Path::to_path_buf),
            changes,
        }
    }

    /// Returns a clone of the current snapshot reference.
    pub(crate) fn current_blocking(&self) -> Arc<ConfigSnapshot> {
        Arc::clone(&*self.inner.read().expect("ConfigManager lock"))
    }

    /// Subscribes to the latest configuration revision.
    pub(crate) fn subscribe(&self) -> watch::Receiver<ConfigChange> {
        self.changes.subscribe()
    }

    /// Applies a validated candidate and persists it before publishing it.
    ///
    /// `expected_fingerprint` provides optimistic concurrency control for
    /// callers that built the candidate from a previously observed snapshot.
    /// The lock covers the comparison, validation, persistence, and snapshot
    /// exchange, so concurrent callers cannot silently overwrite one another.
    pub(crate) fn apply_candidate(
        &self,
        candidate: Config,
        expected_fingerprint: Option<&str>,
    ) -> Result<Arc<ConfigSnapshot>, EgoPulseError> {
        let _update_guard = self
            .update_lock
            .lock()
            .map_err(|_| EgoPulseError::Internal("config update lock poisoned".to_string()))?;
        let current = self.current_blocking();

        if let Some(expected) = expected_fingerprint
            && expected != current.fingerprint
        {
            return Err(ConfigError::ConfigConflict {
                expected: expected.to_string(),
                current: current.fingerprint.clone(),
            }
            .into());
        }

        super::loader::validate_runtime_candidate(&candidate)?;
        ensure_reloadable(&current.config, &candidate)?;

        let revision = current
            .revision
            .checked_add(1)
            .ok_or(ConfigError::ConfigRevisionExhausted)?;

        let path = self
            .config_path
            .as_deref()
            .ok_or(ConfigError::ConfigPathUnavailable)?;
        super::persist::save_config_with_secrets(&candidate, path)?;

        let snapshot = Arc::new(ConfigSnapshot::new(revision, candidate, Some(path)));
        if snapshot.fingerprint == current.fingerprint {
            return Ok(current);
        }

        self.publish(snapshot)
    }

    /// Reloads the persisted configuration without rewriting the source file.
    ///
    /// Invalid or non-reloadable external edits return an error and leave the
    /// current snapshot untouched. A file whose fingerprint is already active
    /// is treated as a no-op.
    pub(crate) fn reload_from_file(&self) -> Result<Arc<ConfigSnapshot>, EgoPulseError> {
        let _update_guard = self
            .update_lock
            .lock()
            .map_err(|_| EgoPulseError::Internal("config update lock poisoned".to_string()))?;
        let current = self.current_blocking();
        let path = self
            .config_path
            .as_deref()
            .ok_or(ConfigError::ConfigPathUnavailable)?;
        let fingerprint = source_fingerprint(path)?;
        if fingerprint == current.fingerprint {
            return Ok(current);
        }

        let candidate = Config::load_allow_missing_api_key(Some(path))?;
        super::loader::validate_runtime_candidate(&candidate)?;
        ensure_reloadable(&current.config, &candidate)?;

        let revision = current
            .revision
            .checked_add(1)
            .ok_or(ConfigError::ConfigRevisionExhausted)?;
        let snapshot = Arc::new(ConfigSnapshot::new(revision, candidate, Some(path)));
        self.publish(snapshot)
    }

    fn publish(&self, snapshot: Arc<ConfigSnapshot>) -> Result<Arc<ConfigSnapshot>, EgoPulseError> {
        let change = ConfigChange {
            revision: snapshot.revision,
            fingerprint: snapshot.fingerprint.clone(),
        };
        *self.inner.write().expect("ConfigManager lock") = Arc::clone(&snapshot);
        let _ = self.changes.send(change);
        Ok(snapshot)
    }
}

fn ensure_reloadable(current: &Config, candidate: &Config) -> Result<(), ConfigError> {
    if current.log_level != candidate.log_level {
        return Err(reload_forbidden("log_level"));
    }
    if current.web_fetch != candidate.web_fetch {
        return Err(reload_forbidden("web_fetch"));
    }
    if current.db.backup != candidate.db.backup {
        return Err(reload_forbidden("db.backup"));
    }
    if current.state_root != candidate.state_root {
        return Err(reload_forbidden("state_root"));
    }
    if current.needs_secret_db() != candidate.needs_secret_db() {
        return Err(reload_forbidden("secret_db"));
    }

    if current.web_enabled() != candidate.web_enabled() {
        return Err(reload_forbidden("channels.web.enabled"));
    }
    if current.web_host() != candidate.web_host() {
        return Err(reload_forbidden("channels.web.host"));
    }
    if current.web_port() != candidate.web_port() {
        return Err(reload_forbidden("channels.web.port"));
    }

    if channel_enabled(current, "discord") != channel_enabled(candidate, "discord") {
        return Err(reload_forbidden("channels.discord.enabled"));
    }
    if discord_bots(current) != discord_bots(candidate) {
        return Err(reload_forbidden("channels.discord.bots"));
    }

    if channel_enabled(current, "telegram") != channel_enabled(candidate, "telegram") {
        return Err(reload_forbidden("channels.telegram.enabled"));
    }
    if telegram_bots(current) != telegram_bots(candidate) {
        return Err(reload_forbidden("channels.telegram.telegram_bots"));
    }

    if current.voice_enabled() != candidate.voice_enabled() {
        return Err(reload_forbidden("channels.voice.enabled"));
    }

    Ok(())
}

fn reload_forbidden(field: &str) -> ConfigError {
    ConfigError::ConfigReloadForbidden {
        field: field.to_string(),
    }
}

fn channel_enabled(config: &Config, channel: &str) -> bool {
    config.channel_enabled(channel)
}

fn discord_bots(config: &Config) -> Option<Vec<(String, Option<String>)>> {
    let mut bots = config
        .channels
        .get("discord")
        .and_then(|channel| channel.discord_bots.as_ref())
        .map(|bots| {
            bots.iter()
                .map(|(id, bot)| {
                    (
                        id.to_string(),
                        bot.token.as_ref().map(|token| token.value().to_string()),
                    )
                })
                .collect::<Vec<_>>()
        })?;
    bots.sort_by(|left, right| left.0.cmp(&right.0));
    Some(bots)
}

fn telegram_bots(config: &Config) -> Option<Vec<(String, Option<String>)>> {
    let mut bots = config
        .channels
        .get("telegram")
        .and_then(|channel| channel.telegram_bots.as_ref())
        .map(|bots| {
            bots.iter()
                .map(|(id, bot)| {
                    (
                        id.to_string(),
                        bot.token.as_ref().map(|token| token.value().to_string()),
                    )
                })
                .collect::<Vec<_>>()
        })?;
    bots.sort_by(|left, right| left.0.cmp(&right.0));
    Some(bots)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    fn test_config() -> Config {
        crate::test_util::test_config("/tmp/egopulse-manager-test")
    }

    fn file_manager() -> (tempfile::TempDir, ConfigManager, std::path::PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("egopulse.config.yaml");
        let mut config = crate::test_util::test_config(dir.path().to_str().expect("utf8"));
        config
            .channels
            .get_mut(&crate::config::ChannelName::new("web"))
            .expect("test web channel")
            .enabled = Some(false);
        crate::config::persist::save_config_with_secrets(&config, &path).expect("save config");
        let manager = ConfigManager::new(config, Some(&path));
        (dir, manager, path)
    }

    #[test]
    fn snapshot_stores_revision_and_fingerprint() {
        let config = test_config();
        let snap = ConfigSnapshot::new(1, config.clone(), None);
        assert_eq!(snap.revision, 1);
        assert!(!snap.fingerprint.is_empty());
        assert_eq!(snap.config.state_root, config.state_root);
    }

    #[test]
    fn same_config_same_fallback_fingerprint() {
        let config = test_config();
        let a = ConfigSnapshot::new(1, config.clone(), None);
        let b = ConfigSnapshot::new(2, config, None);
        assert_eq!(
            a.fingerprint, b.fingerprint,
            "same config should yield same fingerprint"
        );
    }

    #[test]
    fn different_config_different_fallback_fingerprint() {
        let mut config_a = test_config();
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
        let config = test_config();
        let manager = ConfigManager::new(config, None);
        let snap = manager.current_blocking();
        assert_eq!(snap.revision, 1);
        assert!(!snap.fingerprint.is_empty());
    }

    #[test]
    fn apply_candidate_persists_swaps_and_notifies() {
        let (_dir, manager, path) = file_manager();
        let before = manager.current_blocking();
        let mut candidate = before.config.clone();
        candidate.timezone = "Asia/Tokyo".to_string();
        let changes = manager.subscribe();

        let after = manager
            .apply_candidate(candidate, Some(&before.fingerprint))
            .expect("apply candidate");

        assert_eq!(after.revision, before.revision + 1);
        assert_ne!(after.fingerprint, before.fingerprint);
        assert_eq!(after.config.timezone, "Asia/Tokyo");
        assert_eq!(changes.borrow().revision, after.revision);
        let persisted = Config::load_allow_missing_api_key(Some(&path)).expect("reload config");
        assert_eq!(persisted.timezone, "Asia/Tokyo");
    }

    #[test]
    fn invalid_candidate_keeps_snapshot_and_file_unchanged() {
        let (_dir, manager, path) = file_manager();
        let before = manager.current_blocking();
        let file_before = std::fs::read(&path).expect("read config");
        let mut candidate = before.config.clone();
        candidate.default_provider = crate::config::ProviderId::new("missing");

        let error = manager
            .apply_candidate(candidate, Some(&before.fingerprint))
            .expect_err("invalid candidate must fail");

        assert!(matches!(
            error,
            EgoPulseError::Config(ConfigError::InvalidProviderReference { .. })
        ));
        assert_eq!(manager.current_blocking().revision, before.revision);
        assert_eq!(std::fs::read(&path).expect("read config"), file_before);
    }

    #[test]
    fn non_reloadable_candidate_is_rejected_without_partial_apply() {
        let (dir, manager, path) = file_manager();
        let before = manager.current_blocking();
        let file_before = std::fs::read(&path).expect("read config");
        let mut candidate = before.config.clone();
        candidate.state_root = dir.path().join("other").display().to_string();

        let error = manager
            .apply_candidate(candidate, Some(&before.fingerprint))
            .expect_err("state root change must fail");

        assert!(matches!(
            error,
            EgoPulseError::Config(ConfigError::ConfigReloadForbidden { field })
                if field == "state_root"
        ));
        assert_eq!(manager.current_blocking().revision, before.revision);
        assert_eq!(std::fs::read(&path).expect("read config"), file_before);
    }

    #[test]
    fn expected_fingerprint_rejects_stale_concurrent_update() {
        let (_dir, manager, _path) = file_manager();
        let manager = Arc::new(manager);
        let before = manager.current_blocking();

        let mut first = before.config.clone();
        first.timezone = "Asia/Tokyo".to_string();
        let mut second = before.config.clone();
        second.timezone = "America/New_York".to_string();
        let expected = before.fingerprint.clone();

        let manager_a = Arc::clone(&manager);
        let expected_a = expected.clone();
        let first_handle =
            std::thread::spawn(move || manager_a.apply_candidate(first, Some(&expected_a)));
        let manager_b = Arc::clone(&manager);
        let second_handle =
            std::thread::spawn(move || manager_b.apply_candidate(second, Some(&expected)));

        let first_result = first_handle.join().expect("first thread");
        let second_result = second_handle.join().expect("second thread");
        let conflict_count = [first_result, second_result]
            .into_iter()
            .filter(|result| {
                matches!(
                    result,
                    Err(EgoPulseError::Config(ConfigError::ConfigConflict { .. }))
                )
            })
            .count();
        assert_eq!(conflict_count, 1);
        assert_eq!(manager.current_blocking().revision, 2);
    }

    #[test]
    fn reload_from_file_swaps_only_after_valid_external_edit() {
        let (_dir, manager, path) = file_manager();
        let before = manager.current_blocking();
        let mut candidate = before.config.clone();
        candidate.timezone = "Asia/Tokyo".to_string();
        crate::config::persist::save_config_with_secrets(&candidate, &path).expect("edit config");

        let after = manager.reload_from_file().expect("reload config");

        assert_eq!(after.revision, 2);
        assert_eq!(after.config.timezone, "Asia/Tokyo");
    }
}
