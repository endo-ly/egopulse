use std::collections::HashMap;
use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

use fs2::FileExt;
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use url::Url;

use crate::error::{ConfigError, EgoPulseError};

static CONFIG_WRITE_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

/// Per-channel configuration (web, discord, telegram).
#[derive(Clone, Deserialize, Default)]
pub struct ChannelConfig {
    pub enabled: Option<bool>,
    pub port: Option<u16>,
    pub host: Option<String>,
    /// LLM provider override for this channel.
    pub provider: Option<String>,
    /// LLM model override for this channel.
    pub model: Option<String>,
    /// Web: browser/client authentication token.
    pub auth_token: Option<String>,
    #[serde(skip)]
    pub file_auth_token: Option<String>,
    /// Web: allowed Origin values for WebSocket connections.
    pub allowed_origins: Option<Vec<String>>,
    /// Discord / Telegram 共通: bot token
    pub bot_token: Option<String>,
    #[serde(skip)]
    pub file_bot_token: Option<String>,
    /// Telegram: bot username (group メンション検知用)
    pub bot_username: Option<String>,
    /// Telegram: DM 許可ユーザー ID (空 = 全員許可)
    pub allowed_user_ids: Option<Vec<i64>>,
    /// Discord: 許可チャンネル ID (空 = 全チャンネル許可)
    pub allowed_channels: Option<Vec<u64>>,
}

impl std::fmt::Debug for ChannelConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChannelConfig")
            .field("enabled", &self.enabled)
            .field("port", &self.port)
            .field("host", &self.host)
            .field("provider", &self.provider)
            .field("model", &self.model)
            .field(
                "auth_token",
                &self
                    .auth_token
                    .as_ref()
                    .map(|_| "<redacted>")
                    .unwrap_or("<none>"),
            )
            .field("allowed_origins", &self.allowed_origins)
            .field(
                "bot_token",
                &self
                    .bot_token
                    .as_ref()
                    .map(|_| "<redacted>")
                    .unwrap_or("<none>"),
            )
            .field("bot_username", &self.bot_username)
            .field("allowed_user_ids", &self.allowed_user_ids)
            .field("allowed_channels", &self.allowed_channels)
            .finish()
    }
}

#[derive(Clone)]
pub struct ProviderConfig {
    pub label: String,
    pub base_url: String,
    pub api_key: Option<SecretString>,
    pub default_model: String,
    pub models: Vec<String>,
}

impl std::fmt::Debug for ProviderConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderConfig")
            .field("label", &self.label)
            .field("base_url", &self.base_url)
            .field(
                "api_key",
                &self
                    .api_key
                    .as_ref()
                    .map(|_| "<redacted>")
                    .unwrap_or("<none>"),
            )
            .field("default_model", &self.default_model)
            .field("models", &self.models)
            .finish()
    }
}

#[derive(Clone)]
pub struct ResolvedLlmConfig {
    pub provider: String,
    pub label: String,
    pub base_url: String,
    pub api_key: Option<SecretString>,
    pub model: String,
}

impl std::fmt::Debug for ResolvedLlmConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResolvedLlmConfig")
            .field("provider", &self.provider)
            .field("label", &self.label)
            .field("base_url", &self.base_url)
            .field(
                "api_key",
                &self
                    .api_key
                    .as_ref()
                    .map(|_| "<redacted>")
                    .unwrap_or("<none>"),
            )
            .field("model", &self.model)
            .finish()
    }
}

impl PartialEq for ResolvedLlmConfig {
    fn eq(&self, other: &Self) -> bool {
        self.provider == other.provider
            && self.label == other.label
            && self.base_url == other.base_url
            && self.model == other.model
    }
}

impl Eq for ResolvedLlmConfig {}

#[derive(Debug, Deserialize, Default)]
struct FileProviderConfig {
    label: Option<String>,
    base_url: Option<String>,
    api_key: Option<String>,
    default_model: Option<String>,
    models: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, Default)]
struct FileConfig {
    default_provider: Option<String>,
    /// グローバルでのモデル選択（YAMLトップレベル）。未指定時は default_provider の default_model にフォールバック。
    default_model: Option<String>,
    providers: Option<HashMap<String, FileProviderConfig>>,
    log_level: Option<String>,
    compaction_timeout_secs: Option<u64>,
    max_history_messages: Option<usize>,
    max_session_messages: Option<usize>,
    compact_keep_recent: Option<usize>,
    channels: Option<HashMap<String, ChannelConfig>>,
}

/// Top-level application configuration resolved from file and environment variables.
#[derive(Clone)]
pub struct Config {
    pub default_provider: String,
    /// Optional global model override (YAML `default_model`).
    pub default_model: Option<String>,
    pub providers: HashMap<String, ProviderConfig>,
    pub data_dir: String,
    pub log_level: String,
    pub compaction_timeout_secs: u64,
    pub max_history_messages: usize,
    pub max_session_messages: usize,
    pub compact_keep_recent: usize,
    pub channels: HashMap<String, ChannelConfig>,
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("default_provider", &self.default_provider)
            .field("default_model", &self.default_model)
            .field("providers", &self.providers)
            .field("data_dir", &self.data_dir)
            .field("log_level", &self.log_level)
            .field("compaction_timeout_secs", &self.compaction_timeout_secs)
            .field("max_history_messages", &self.max_history_messages)
            .field("max_session_messages", &self.max_session_messages)
            .field("compact_keep_recent", &self.compact_keep_recent)
            .field("channels", &self.channels)
            .finish()
    }
}

impl Config {
    /// Load configuration, requiring an API key for remote endpoints.
    pub fn load(config_path: Option<&Path>) -> Result<Self, ConfigError> {
        build_config(config_path, false)
    }

    /// Load configuration, allowing a missing API key (used by setup/config editing).
    pub fn load_allow_missing_api_key(config_path: Option<&Path>) -> Result<Self, ConfigError> {
        build_config(config_path, true)
    }

    /// Invariant: `build_config` validates that `default_provider` exists in `providers`.
    /// This accessor relies on that validated config construction path.
    pub fn global_provider(&self) -> &ProviderConfig {
        self.providers
            .get(&self.default_provider)
            .expect("default_provider must reference an existing provider")
    }

    /// Resolves the global default provider/model pair used by CLI/TUI.
    pub fn resolve_global_llm(&self) -> ResolvedLlmConfig {
        let provider = self.global_provider();
        ResolvedLlmConfig {
            provider: self.default_provider.clone(),
            label: provider.label.clone(),
            base_url: provider.base_url.clone(),
            api_key: provider.api_key.clone(),
            model: self
                .default_model
                .clone()
                .unwrap_or_else(|| provider.default_model.clone()),
        }
    }

    /// Returns the normalized provider key used for the given channel.
    pub fn effective_provider_name<'a>(&'a self, channel: &str) -> &'a str {
        self.channels
            .get(&channel.trim().to_ascii_lowercase())
            .and_then(|config| config.provider.as_deref())
            .unwrap_or(&self.default_provider)
    }

    /// Resolves the provider/model pair used for a request from the given channel.
    pub fn resolve_llm_for_channel(&self, channel: &str) -> Result<ResolvedLlmConfig, ConfigError> {
        let channel_key = channel.trim().to_ascii_lowercase();
        let provider_name = self.effective_provider_name(&channel_key).to_string();
        let provider = self.providers.get(&provider_name).ok_or_else(|| {
            ConfigError::InvalidProviderReference {
                provider: provider_name.clone(),
            }
        })?;

        let model = self
            .channels
            .get(&channel_key)
            .and_then(|config| config.model.clone())
            .unwrap_or_else(|| {
                self.default_model
                    .clone()
                    .unwrap_or_else(|| provider.default_model.clone())
            });

        Ok(ResolvedLlmConfig {
            provider: provider_name,
            label: provider.label.clone(),
            base_url: provider.base_url.clone(),
            api_key: provider.api_key.clone(),
            model,
        })
    }

    /// Returns the web channel's resolved LLM settings.
    pub fn web_llm(&self) -> Result<ResolvedLlmConfig, ConfigError> {
        self.resolve_llm_for_channel("web")
    }

    /// Returns `true` if the web channel is enabled.
    pub fn web_enabled(&self) -> bool {
        self.channels
            .get("web")
            .and_then(|c| c.enabled)
            .unwrap_or(false)
    }

    /// Returns the web channel host, defaulting to `127.0.0.1`.
    pub fn web_host(&self) -> String {
        self.channels
            .get("web")
            .and_then(|c| c.host.clone())
            .unwrap_or_else(|| default_web_host().to_string())
    }

    /// Returns the web channel port, defaulting to `10961`.
    pub fn web_port(&self) -> u16 {
        self.channels
            .get("web")
            .and_then(|c| c.port)
            .unwrap_or_else(default_web_port)
    }

    /// Returns the web auth token if configured and non-empty.
    pub fn web_auth_token(&self) -> Option<&str> {
        self.channels
            .get("web")
            .and_then(|c| c.auth_token.as_deref())
            .map(str::trim)
            .filter(|token| !token.is_empty())
    }

    /// Returns the list of allowed WebSocket origins for the web channel.
    pub fn web_allowed_origins(&self) -> Vec<String> {
        self.channels
            .get("web")
            .and_then(|c| c.allowed_origins.clone())
            .unwrap_or_default()
            .into_iter()
            .filter_map(|origin| normalize_string(Some(origin)))
            .collect()
    }

    /// Returns `true` if the named channel is enabled.
    pub fn channel_enabled(&self, channel: &str) -> bool {
        let needle = channel.trim().to_ascii_lowercase();
        self.channels
            .get(&needle)
            .and_then(|c| c.enabled)
            .unwrap_or(false)
    }

    /// Locate the default config file, or fail when absent.
    pub fn resolve_config_path() -> Result<Option<PathBuf>, ConfigError> {
        let candidate = default_config_path()?;
        if candidate.exists() {
            return Ok(Some(candidate));
        }

        Err(ConfigError::AutoConfigNotFound {
            searched_paths: vec![candidate],
        })
    }

    /// Discord bot token (env override or config file).
    pub fn discord_bot_token(&self) -> Option<String> {
        env::var("EGOPULSE_DISCORD_BOT_TOKEN")
            .ok()
            .and_then(|v| normalize_string(Some(v)))
            .or_else(|| {
                self.channels
                    .get("discord")
                    .and_then(|c| c.bot_token.clone())
            })
    }

    /// Telegram bot token (env override or config file).
    pub fn telegram_bot_token(&self) -> Option<String> {
        env::var("EGOPULSE_TELEGRAM_BOT_TOKEN")
            .ok()
            .and_then(|v| normalize_string(Some(v)))
            .or_else(|| {
                self.channels
                    .get("telegram")
                    .and_then(|c| c.bot_token.clone())
            })
    }

    /// Telegram bot username for group mention detection.
    pub fn telegram_bot_username(&self) -> Option<String> {
        self.channels
            .get("telegram")
            .and_then(|c| c.bot_username.clone())
    }

    /// Directory containing skill definitions.
    pub fn skills_dir(&self) -> Result<PathBuf, ConfigError> {
        default_workspace_dir().map(|dir| dir.join("skills"))
    }

    /// Workspace directory for agent file operations.
    pub fn workspace_dir(&self) -> Result<PathBuf, ConfigError> {
        default_workspace_dir()
    }

    /// Atomically writes the current config to a YAML file.
    ///
    /// Uses the global `CONFIG_WRITE_LOCK` for in-process mutual exclusion and an
    /// file-level lock (`fs2`) for cross-process safety. The write is atomic via
    /// temp-file + rename.
    pub fn save_yaml(&self, path: &Path) -> Result<(), EgoPulseError> {
        let _guard = CONFIG_WRITE_LOCK
            .lock()
            .map_err(|_| EgoPulseError::Internal("config write lock poisoned".to_string()))?;
        let _lock_file = acquire_config_lock(path)?;

        let yaml = serde_yml::to_string(&SerializableConfig::from(self))
            .map_err(|error| EgoPulseError::Internal(error.to_string()))?;
        write_atomically(path, &yaml)
    }
}

/// Default config file path: `~/.egopulse/egopulse.config.yaml`.
pub fn default_config_path() -> Result<PathBuf, ConfigError> {
    default_state_root().map(|root| root.join("egopulse.config.yaml"))
}

/// Default state root directory: `~/.egopulse`.
pub fn default_state_root() -> Result<PathBuf, ConfigError> {
    dirs::home_dir()
        .map(|home| home.join(".egopulse"))
        .ok_or(ConfigError::HomeDirectoryUnresolved)
}

/// Default data directory: `~/.egopulse/data`.
pub fn default_data_dir() -> Result<PathBuf, ConfigError> {
    default_state_root().map(|root| root.join("data"))
}

/// Default workspace directory: `~/.egopulse/workspace`.
pub fn default_workspace_dir() -> Result<PathBuf, ConfigError> {
    default_state_root().map(|root| root.join("workspace"))
}

fn normalize_channels(
    mut channels: HashMap<String, ChannelConfig>,
) -> HashMap<String, ChannelConfig> {
    let mut normalized = HashMap::new();
    for (name, mut config) in channels.drain() {
        let key = name.trim().to_ascii_lowercase();
        if key.is_empty() {
            continue;
        }
        config.file_auth_token = normalize_string(config.auth_token.clone());
        config.file_bot_token = normalize_string(config.bot_token.clone());
        config.provider = normalize_string(config.provider);
        config.model = normalize_string(config.model);
        normalized.insert(key, config);
    }

    if let Some(web) = normalized.get_mut("web") {
        if web.host.is_none() {
            web.host = Some(default_web_host().to_string());
        }
        if web.port.is_none() {
            web.port = Some(default_web_port());
        }
    }

    normalized
}

fn normalize_provider_map(
    providers: HashMap<String, FileProviderConfig>,
    allow_missing_api_key: bool,
) -> Result<HashMap<String, ProviderConfig>, ConfigError> {
    let mut normalized = HashMap::new();

    for (name, file_provider) in providers {
        let key = normalize_string(Some(name)).ok_or(ConfigError::MissingProvider)?;
        let label = normalize_string(file_provider.label).unwrap_or_else(|| key.clone());
        let base_url = normalize_string(file_provider.base_url).ok_or_else(|| {
            ConfigError::MissingProviderBaseUrl {
                provider: key.clone(),
            }
        })?;
        validate_base_url(&base_url)?;

        let default_model = normalize_string(file_provider.default_model).ok_or_else(|| {
            ConfigError::MissingProviderDefaultModel {
                provider: key.clone(),
            }
        })?;

        let mut models = file_provider
            .models
            .unwrap_or_default()
            .into_iter()
            .filter_map(|model| normalize_string(Some(model)))
            .collect::<Vec<_>>();
        if !models.iter().any(|model| model == &default_model) {
            models.insert(0, default_model.clone());
        }

        let api_key = normalize_string(file_provider.api_key)
            .map(|value| SecretString::new(value.into_boxed_str()));
        if !allow_missing_api_key && api_key.is_none() && !base_url_allows_empty_api_key(&base_url)
        {
            return Err(ConfigError::MissingProviderApiKey {
                provider: key.clone(),
            });
        }

        normalized.insert(
            key,
            ProviderConfig {
                label,
                base_url,
                api_key,
                default_model,
                models,
            },
        );
    }

    Ok(normalized)
}

fn apply_channel_bot_token_env_override(
    channels: &mut HashMap<String, ChannelConfig>,
    channel_name: &str,
    env_key: &str,
) {
    if let Some(token) = env_var(env_key) {
        let channel = channels.entry(channel_name.to_string()).or_default();
        channel.bot_token = Some(token);
    }
}

fn apply_web_channel_env_overrides(channels: &mut HashMap<String, ChannelConfig>) {
    let web_host = env_var("EGOPULSE_WEB_HOST");
    let web_port = env_var("EGOPULSE_WEB_PORT").and_then(|value| value.parse::<u16>().ok());
    let web_enabled = env_var("EGOPULSE_WEB_ENABLED").and_then(|value| parse_bool(&value));
    let web_auth_token = env_var("EGOPULSE_WEB_AUTH_TOKEN");
    let web_allowed_origins = env_var("EGOPULSE_WEB_ALLOWED_ORIGINS").map(|value| {
        value
            .split(',')
            .filter_map(|origin| normalize_string(Some(origin.to_string())))
            .collect::<Vec<_>>()
    });

    if web_host.is_none()
        && web_port.is_none()
        && web_enabled.is_none()
        && web_auth_token.is_none()
        && web_allowed_origins.is_none()
    {
        return;
    }

    let web = channels.entry("web".to_string()).or_default();
    if let Some(enabled) = web_enabled {
        web.enabled = Some(enabled);
    }
    if let Some(host) = web_host {
        web.host = Some(host);
    }
    if let Some(port) = web_port {
        web.port = Some(port);
    }
    if let Some(token) = web_auth_token {
        web.auth_token = Some(token);
    }
    if let Some(origins) = web_allowed_origins {
        web.allowed_origins = Some(origins);
    }

    if web.host.is_none() {
        web.host = Some(default_web_host().to_string());
    }
    if web.port.is_none() {
        web.port = Some(default_web_port());
    }
}

fn build_config(
    config_path: Option<&Path>,
    allow_missing_api_key: bool,
) -> Result<Config, ConfigError> {
    let resolved_config_path = match config_path {
        Some(path) => Some(PathBuf::from(path)),
        None => Config::resolve_config_path()?,
    };
    let FileConfig {
        default_provider: file_default_provider,
        default_model: file_default_model,
        providers: file_providers,
        log_level: file_log_level,
        compaction_timeout_secs: file_compaction_timeout_secs,
        max_history_messages: file_max_history_messages,
        max_session_messages: file_max_session_messages,
        compact_keep_recent: file_compact_keep_recent,
        channels: file_channels,
    } = read_file_config(resolved_config_path.as_deref())?;

    let default_provider =
        normalize_string(file_default_provider).ok_or(ConfigError::MissingDefaultProvider)?;
    let providers = normalize_provider_map(
        file_providers.ok_or(ConfigError::MissingProviders)?,
        allow_missing_api_key,
    )?;
    if !providers.contains_key(&default_provider) {
        return Err(ConfigError::InvalidProviderReference {
            provider: default_provider,
        });
    }

    let default_model = normalize_string(file_default_model);

    let data_dir = default_data_dir()?.to_string_lossy().into_owned();

    let log_level = first_non_empty([env_var("EGOPULSE_LOG_LEVEL"), file_log_level])
        .unwrap_or_else(|| "info".to_string());

    let compaction_timeout_secs =
        file_compaction_timeout_secs.unwrap_or_else(default_compaction_timeout_secs);
    let max_history_messages =
        file_max_history_messages.unwrap_or_else(default_max_history_messages);
    let max_session_messages =
        file_max_session_messages.unwrap_or_else(default_max_session_messages);
    let compact_keep_recent = file_compact_keep_recent.unwrap_or_else(default_compact_keep_recent);

    let mut channels = normalize_channels(file_channels.unwrap_or_default());
    apply_web_channel_env_overrides(&mut channels);
    apply_channel_bot_token_env_override(&mut channels, "discord", "EGOPULSE_DISCORD_BOT_TOKEN");
    apply_channel_bot_token_env_override(&mut channels, "telegram", "EGOPULSE_TELEGRAM_BOT_TOKEN");

    validate_channel_provider_references(&default_provider, &providers, &channels)?;

    let config = Config {
        default_provider,
        default_model,
        providers,
        data_dir,
        log_level,
        compaction_timeout_secs,
        max_history_messages,
        max_session_messages,
        compact_keep_recent,
        channels,
    };

    validate_compaction_config(&config)?;

    if config.web_enabled() && config.web_auth_token().is_none() {
        return Err(ConfigError::MissingWebAuthToken);
    }

    Ok(config)
}

fn validate_channel_provider_references(
    default_provider: &str,
    providers: &HashMap<String, ProviderConfig>,
    channels: &HashMap<String, ChannelConfig>,
) -> Result<(), ConfigError> {
    // `build_config` checks `default_provider` before calling this helper; keep the
    // parameter to make the relationship explicit for callers and future refactors.
    let _ = default_provider;

    for channel in channels.values() {
        if let Some(provider) = channel.provider.as_ref()
            && !providers.contains_key(provider)
        {
            return Err(ConfigError::InvalidProviderReference {
                provider: provider.clone(),
            });
        }
    }

    Ok(())
}

fn parse_bool(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

fn default_compaction_timeout_secs() -> u64 {
    180
}

fn default_max_history_messages() -> usize {
    50
}

fn default_max_session_messages() -> usize {
    40
}

fn default_compact_keep_recent() -> usize {
    20
}

fn default_web_host() -> &'static str {
    "127.0.0.1"
}

fn default_web_port() -> u16 {
    10961
}

/// Returns `true` if the base URL points to a local address that does not require an API key.
pub fn base_url_allows_empty_api_key(base_url: &str) -> bool {
    is_local_url(base_url)
}

fn validate_base_url(value: &str) -> Result<(), ConfigError> {
    Url::parse(value)
        .map(|_| ())
        .map_err(|_| ConfigError::InvalidBaseUrl)
}

fn validate_compaction_config(config: &Config) -> Result<(), ConfigError> {
    if config.compaction_timeout_secs == 0 {
        return Err(ConfigError::InvalidCompactionConfig(
            "compaction_timeout_secs must be at least 1".to_string(),
        ));
    }
    if config.max_history_messages == 0 {
        return Err(ConfigError::InvalidCompactionConfig(
            "max_history_messages must be at least 1".to_string(),
        ));
    }
    if config.max_session_messages == 0 {
        return Err(ConfigError::InvalidCompactionConfig(
            "max_session_messages must be at least 1".to_string(),
        ));
    }
    if config.compact_keep_recent == 0 {
        return Err(ConfigError::InvalidCompactionConfig(
            "compact_keep_recent must be at least 1".to_string(),
        ));
    }
    Ok(())
}

fn read_file_config(path: Option<&Path>) -> Result<FileConfig, ConfigError> {
    let Some(path) = path else {
        return Ok(FileConfig::default());
    };

    if !path.exists() {
        return Err(ConfigError::ConfigNotFound {
            path: PathBuf::from(path),
        });
    }

    let contents = fs::read_to_string(path).map_err(|source| ConfigError::ConfigReadFailed {
        path: PathBuf::from(path),
        source,
    })?;
    serde_yml::from_str(&contents).map_err(|source| ConfigError::ConfigParseFailed {
        path: PathBuf::from(path),
        detail: source.to_string(),
    })
}

fn env_var(key: &str) -> Option<String> {
    env::var(key)
        .ok()
        .and_then(|value| normalize_string(Some(value)))
}

fn first_non_empty<const N: usize>(values: [Option<String>; N]) -> Option<String> {
    values.into_iter().find_map(normalize_string)
}

fn normalize_string(value: Option<String>) -> Option<String> {
    value.and_then(|candidate| {
        let trimmed = candidate.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    })
}

fn is_local_url(value: &str) -> bool {
    let Ok(url) = Url::parse(value) else {
        return false;
    };

    matches!(
        url.host_str(),
        Some("localhost") | Some("127.0.0.1") | Some("0.0.0.0") | Some("::1")
    )
}

// --- Serialization helpers for save_yaml ---

#[derive(Serialize)]
struct SerializableConfig {
    default_provider: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    default_model: Option<String>,
    data_dir: String,
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
}

impl From<&Config> for SerializableConfig {
    fn from(config: &Config) -> Self {
        let providers = config
            .providers
            .iter()
            .map(|(id, p)| {
                (
                    id.clone(),
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
                    id.clone(),
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
                    },
                )
            })
            .collect();

        Self {
            default_provider: config.default_provider.clone(),
            default_model: config.default_model.clone(),
            data_dir: config.data_dir.clone(),
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

    let mut temp_file = OpenOptions::new()
        .create_new(true)
        .write(true)
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

#[cfg(test)]
mod tests {
    //! アプリケーション設定の読み込みと検証。
    //!
    //! YAML 設定ファイルから provider ベースの設定を構築し、
    //! channel ごとの override を実効 LLM 設定へ解決する。

    use std::io::Write;

    use secrecy::ExposeSecret;
    use serial_test::serial;

    use std::path::PathBuf;

    use super::{Config, default_data_dir, default_workspace_dir};
    use crate::error::ConfigError;
    use crate::test_env::EnvVarGuard;

    fn write_config(temp_dir: &tempfile::TempDir, body: &str) -> PathBuf {
        let file_path = temp_dir.path().join("egopulse.config.yaml");
        let mut file = std::fs::File::create(&file_path).expect("create config");
        writeln!(file, "{body}").expect("write config");
        file_path
    }

    fn sample_config() -> &'static str {
        r#"default_provider: openai
providers:
  openai:
    label: OpenAI
    base_url: https://api.openai.com/v1
    api_key: sk-openai
    default_model: gpt-4o-mini
    models:
      - gpt-4o-mini
      - gpt-5
  local:
    label: Local OpenAI-compatible
    base_url: http://127.0.0.1:1234/v1
    default_model: qwen2.5
channels:
  web:
    enabled: true
    auth_token: web-secret
  discord:
    enabled: false
    provider: local
    model: qwen2.5-coder"#
    }

    #[test]
    #[serial]
    fn home_directory_unresolved_error_displays_correctly() {
        let error = ConfigError::HomeDirectoryUnresolved;
        let message = error.to_string();
        assert!(message.contains("home_directory_unresolved"));
    }

    #[test]
    #[serial]
    fn loads_provider_based_config() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let _home = EnvVarGuard::set("HOME", temp_dir.path());
        let file_path = write_config(&temp_dir, sample_config());

        let config = Config::load(Some(&file_path)).expect("load config");

        assert_eq!(config.default_provider, "openai");
        assert_eq!(config.global_provider().label, "OpenAI");
        assert_eq!(PathBuf::from(&config.data_dir), default_data_dir().unwrap());
        assert_eq!(
            config.workspace_dir().unwrap(),
            default_workspace_dir().unwrap()
        );
        assert_eq!(
            config.skills_dir().unwrap(),
            default_workspace_dir().unwrap().join("skills")
        );
        assert!(config.web_enabled());
        assert_eq!(config.web_auth_token(), Some("web-secret"));

        let web_llm = config.web_llm().expect("web llm");
        assert_eq!(web_llm.provider, "openai");
        assert_eq!(web_llm.model, "gpt-4o-mini");
        assert_eq!(web_llm.base_url, "https://api.openai.com/v1");
        assert_eq!(
            web_llm.api_key.as_ref().map(ExposeSecret::expose_secret),
            Some("sk-openai")
        );

        let discord_llm = config
            .resolve_llm_for_channel("discord")
            .expect("discord llm");
        assert_eq!(discord_llm.provider, "local");
        assert_eq!(discord_llm.model, "qwen2.5-coder");
        assert!(discord_llm.api_key.is_none());
    }

    #[test]
    #[serial]
    fn allows_missing_api_key_for_local_provider() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let _home = EnvVarGuard::set("HOME", temp_dir.path());
        let file_path = write_config(
            &temp_dir,
            r#"default_provider: local
providers:
  local:
    label: Local
    base_url: http://127.0.0.1:1234/v1
    default_model: qwen2.5
channels:
  web:
    enabled: true
    auth_token: web-secret"#,
        );

        let config = Config::load(Some(&file_path)).expect("load local config");
        let resolved = config.web_llm().expect("resolved llm");
        assert!(resolved.api_key.is_none());
    }

    #[test]
    #[serial]
    fn rejects_missing_remote_api_key() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let _home = EnvVarGuard::set("HOME", temp_dir.path());
        let file_path = write_config(
            &temp_dir,
            r#"default_provider: openai
providers:
  openai:
    label: OpenAI
    base_url: https://api.openai.com/v1
    default_model: gpt-4o-mini
channels:
  web:
    enabled: true
    auth_token: web-secret"#,
        );

        let error = Config::load(Some(&file_path)).expect_err("missing api key");
        assert!(matches!(
            error,
            ConfigError::MissingProviderApiKey { provider } if provider == "openai"
        ));
    }

    #[test]
    #[serial]
    fn rejects_unknown_channel_provider() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let _home = EnvVarGuard::set("HOME", temp_dir.path());
        let file_path = write_config(
            &temp_dir,
            r#"default_provider: openai
providers:
  openai:
    label: OpenAI
    base_url: https://api.openai.com/v1
    api_key: sk-openai
    default_model: gpt-4o-mini
channels:
  web:
    enabled: true
    auth_token: web-secret
    provider: missing"#,
        );

        let error = Config::load(Some(&file_path)).expect_err("invalid provider");
        assert!(matches!(
            error,
            ConfigError::InvalidProviderReference { provider } if provider == "missing"
        ));
    }

    #[test]
    #[serial]
    fn load_allow_missing_api_key_accepts_incomplete_remote_provider() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let _home = EnvVarGuard::set("HOME", temp_dir.path());
        let file_path = write_config(
            &temp_dir,
            r#"default_provider: openai
providers:
  openai:
    label: OpenAI
    base_url: https://api.openai.com/v1
    default_model: gpt-4o-mini
channels:
  web:
    enabled: true
    auth_token: web-secret"#,
        );

        let config =
            Config::load_allow_missing_api_key(Some(&file_path)).expect("allow missing key");
        assert!(config.web_llm().expect("resolved").api_key.is_none());
    }

    #[test]
    #[serial]
    fn default_model_in_yaml_overrides_provider_default() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let _home = EnvVarGuard::set("HOME", temp_dir.path());
        let file_path = write_config(
            &temp_dir,
            r#"default_provider: openai
default_model: gpt-5
providers:
  openai:
    label: OpenAI
    base_url: https://api.openai.com/v1
    api_key: sk-openai
    default_model: gpt-4o-mini
channels:
  web:
    enabled: true
    auth_token: web-secret"#,
        );

        let config = Config::load(Some(&file_path)).expect("load config");

        // config.default_model preserves the YAML-level override as Some
        assert_eq!(config.default_model, Some("gpt-5".to_string()));

        // resolve_global_llm uses config.default_model
        let global = config.resolve_global_llm();
        assert_eq!(global.model, "gpt-5");

        // channel without model override also falls back to config.default_model
        let web_llm = config.web_llm().expect("web llm");
        assert_eq!(web_llm.model, "gpt-5");
    }

    #[test]
    #[serial]
    fn default_model_falls_back_to_provider_default() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let _home = EnvVarGuard::set("HOME", temp_dir.path());
        let file_path = write_config(
            &temp_dir,
            r#"default_provider: openai
providers:
  openai:
    label: OpenAI
    base_url: https://api.openai.com/v1
    api_key: sk-openai
    default_model: gpt-4o-mini
channels:
  web:
    enabled: true
    auth_token: web-secret"#,
        );

        let config = Config::load(Some(&file_path)).expect("load config");

        assert_eq!(config.default_model, None);
        let global = config.resolve_global_llm();
        assert_eq!(global.model, "gpt-4o-mini");
    }

    #[test]
    #[serial]
    fn model_resolution_chain_channel_overrides_global() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let _home = EnvVarGuard::set("HOME", temp_dir.path());
        let file_path = write_config(
            &temp_dir,
            r#"default_provider: openai
default_model: gpt-5
providers:
  openai:
    label: OpenAI
    base_url: https://api.openai.com/v1
    api_key: sk-openai
    default_model: gpt-4o-mini
  local:
    label: Local
    base_url: http://127.0.0.1:1234/v1
    default_model: qwen2.5
channels:
  web:
    enabled: true
    auth_token: web-secret
    model: gpt-4o
  discord:
    enabled: false
    provider: local
    model: qwen2.5-coder"#,
        );

        let config = Config::load(Some(&file_path)).expect("load config");

        // channel.model > config.default_model
        let web_llm = config.web_llm().expect("web llm");
        assert_eq!(web_llm.model, "gpt-4o");

        // channel.model > config.default_model (different provider)
        let discord_llm = config
            .resolve_llm_for_channel("discord")
            .expect("discord llm");
        assert_eq!(discord_llm.model, "qwen2.5-coder");
        assert_eq!(discord_llm.provider, "local");

        // channel without model → config.default_model (not provider.default_model)
        let telegram_llm = config
            .resolve_llm_for_channel("telegram")
            .expect("telegram llm");
        assert_eq!(telegram_llm.model, "gpt-5");
    }
}
