use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::{LazyLock, Mutex};

use fs2::FileExt;
use secrecy::ExposeSecret;
use serde::Serialize;

use super::Config;
use crate::error::EgoPulseError;

static CONFIG_WRITE_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

#[derive(Serialize)]
struct SerializableConfig {
    default_provider: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    default_model: Option<String>,
    state_root: String,
    log_level: String,
    compaction_timeout_secs: u64,
    max_history_messages: usize,
    max_session_messages: usize,
    compact_keep_recent: usize,
    providers: HashMap<String, SerializableProvider>,
    channels: HashMap<String, SerializableChannel>,
}

#[derive(Serialize)]
struct SerializableProvider {
    label: String,
    base_url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    api_key: Option<String>,
    default_model: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    models: Vec<String>,
}

#[derive(Serialize)]
struct SerializableChannel {
    #[serde(skip_serializing_if = "Option::is_none")]
    enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    port: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    host: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    auth_token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    allowed_origins: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    bot_token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    bot_username: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    allowed_user_ids: Option<Vec<i64>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    allowed_channels: Option<Vec<u64>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    soul_path: Option<String>,
}

impl From<&Config> for SerializableConfig {
    fn from(config: &Config) -> Self {
        let providers = config
            .providers
            .iter()
            .map(|(id, p)| {
                (
                    id.to_string(),
                    SerializableProvider {
                        label: p.label.clone(),
                        base_url: p.base_url.clone(),
                        api_key: p
                            .api_key
                            .as_ref()
                            .map(|s| ExposeSecret::expose_secret(s).to_string()),
                        default_model: p.default_model.clone(),
                        models: p.models.clone(),
                    },
                )
            })
            .collect();

        let channels = config
            .channels
            .iter()
            .map(|(id, c)| {
                (
                    id.to_string(),
                    SerializableChannel {
                        enabled: c.enabled,
                        port: c.port,
                        host: c.host.clone(),
                        provider: c.provider.clone(),
                        model: c.model.clone(),
                        auth_token: c.file_auth_token.clone(),
                        allowed_origins: c.allowed_origins.clone(),
                        bot_token: c.file_bot_token.clone(),
                        bot_username: c.bot_username.clone(),
                        allowed_user_ids: c.allowed_user_ids.clone(),
                        allowed_channels: c.allowed_channels.clone(),
                        soul_path: c.soul_path.clone(),
                    },
                )
            })
            .collect();

        Self {
            default_provider: config.default_provider.to_string(),
            default_model: config.default_model.clone(),
            state_root: config.state_root.clone(),
            log_level: config.log_level.clone(),
            compaction_timeout_secs: config.compaction_timeout_secs,
            max_history_messages: config.max_history_messages,
            max_session_messages: config.max_session_messages,
            compact_keep_recent: config.compact_keep_recent,
            providers,
            channels,
        }
    }
}

/// Atomically writes the current config to a YAML file.
///
/// Uses the global `CONFIG_WRITE_LOCK` for in-process mutual exclusion and an
/// file-level lock (`fs2`) for cross-process safety. The write is atomic via
/// temp-file + rename.
pub fn save_yaml(config: &Config, path: &Path) -> Result<(), EgoPulseError> {
    let _guard = CONFIG_WRITE_LOCK
        .lock()
        .map_err(|_| EgoPulseError::Internal("config write lock poisoned".to_string()))?;
    let _lock_file = acquire_config_lock(path)?;

    let yaml = serde_yml::to_string(&SerializableConfig::from(config))
        .map_err(|error| EgoPulseError::Internal(error.to_string()))?;
    write_atomically(path, &yaml)
}

fn acquire_config_lock(path: &Path) -> Result<File, EgoPulseError> {
    let lock_path = {
        let parent = path
            .parent()
            .ok_or_else(|| EgoPulseError::Internal("config path has no parent".to_string()))?;
        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("egopulse.config.yaml");
        parent.join(format!(".{file_name}.lock"))
    };
    if let Some(parent) = lock_path.parent() {
        fs::create_dir_all(parent).map_err(|error| EgoPulseError::Internal(error.to_string()))?;
    }

    let lock_file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|error| EgoPulseError::Internal(error.to_string()))?;
    lock_file
        .lock_exclusive()
        .map_err(|error| EgoPulseError::Internal(error.to_string()))?;
    Ok(lock_file)
}

fn write_atomically(path: &Path, content: &str) -> Result<(), EgoPulseError> {
    let parent = path
        .parent()
        .ok_or_else(|| EgoPulseError::Internal("config path has no parent".to_string()))?;
    fs::create_dir_all(parent).map_err(|error| EgoPulseError::Internal(error.to_string()))?;

    let temp_path = parent.join(format!(
        ".{}.tmp-{}-{}",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("egopulse.config.yaml"),
        std::process::id(),
        uuid::Uuid::new_v4()
    ));

    let mut opts = OpenOptions::new();
    opts.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut temp_file = opts
        .open(&temp_path)
        .map_err(|error| EgoPulseError::Internal(error.to_string()))?;
    temp_file
        .write_all(content.as_bytes())
        .map_err(|error| EgoPulseError::Internal(error.to_string()))?;
    temp_file
        .flush()
        .map_err(|error| EgoPulseError::Internal(error.to_string()))?;
    temp_file
        .sync_all()
        .map_err(|error| EgoPulseError::Internal(error.to_string()))?;
    drop(temp_file);

    if let Err(error) = fs::rename(&temp_path, path) {
        let _ = fs::remove_file(&temp_path);
        return Err(EgoPulseError::Internal(error.to_string()));
    }

    if let Ok(dir) = OpenOptions::new().read(true).open(parent) {
        let _ = dir.sync_all();
    }

    Ok(())
}
