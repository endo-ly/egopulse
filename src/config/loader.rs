use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use secrecy::SecretString;
use serde::Deserialize;
use url::Url;

use super::{ChannelConfig, ChannelName, Config, ProviderConfig, ProviderId};
use crate::error::ConfigError;

#[derive(Debug, Deserialize, Default)]
struct FileProviderConfig {
    label: Option<String>,
    base_url: Option<String>,
    api_key: Option<String>,
    default_model: Option<String>,
    models: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, Default)]
struct FileChannelConfig {
    enabled: Option<bool>,
    port: Option<u16>,
    host: Option<String>,
    provider: Option<String>,
    model: Option<String>,
    auth_token: Option<String>,
    allowed_origins: Option<Vec<String>>,
    bot_token: Option<String>,
    bot_username: Option<String>,
    allowed_user_ids: Option<Vec<i64>>,
    allowed_channels: Option<Vec<u64>>,
    soul_path: Option<String>,
}

impl From<FileChannelConfig> for ChannelConfig {
    fn from(fc: FileChannelConfig) -> Self {
        Self {
            enabled: fc.enabled,
            port: fc.port,
            host: fc.host,
            provider: normalize_string(fc.provider),
            model: normalize_string(fc.model),
            auth_token: fc.auth_token,
            file_auth_token: None,
            allowed_origins: fc.allowed_origins,
            bot_token: fc.bot_token,
            file_bot_token: None,
            bot_username: fc.bot_username,
            allowed_user_ids: fc.allowed_user_ids,
            allowed_channels: fc.allowed_channels,
            soul_path: fc.soul_path,
        }
    }
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
    channels: Option<HashMap<String, FileChannelConfig>>,
}

pub(super) fn build_config(
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
    let default_provider = ProviderId::new(&default_provider);
    let providers = normalize_provider_map(
        file_providers.ok_or(ConfigError::MissingProviders)?,
        allow_missing_api_key,
    )?;
    if !providers.contains_key(&default_provider) {
        return Err(ConfigError::InvalidProviderReference {
            provider: default_provider.to_string(),
        });
    }

    let default_model = normalize_string(file_default_model);

    let state_root = super::resolve::default_state_root()?.to_string_lossy().into_owned();

    let log_level = first_non_empty([env_var("EGOPULSE_LOG_LEVEL"), file_log_level])
        .unwrap_or_else(|| "info".to_string());

    let compaction_timeout_secs =
        file_compaction_timeout_secs.unwrap_or_else(super::resolve::default_compaction_timeout_secs);
    let max_history_messages =
        file_max_history_messages.unwrap_or_else(super::resolve::default_max_history_messages);
    let max_session_messages =
        file_max_session_messages.unwrap_or_else(super::resolve::default_max_session_messages);
    let compact_keep_recent = file_compact_keep_recent.unwrap_or_else(super::resolve::default_compact_keep_recent);

    let mut channels = normalize_channels(file_channels.unwrap_or_default());
    apply_web_channel_env_overrides(&mut channels);
    apply_channel_bot_token_env_override(&mut channels, "discord", "EGOPULSE_DISCORD_BOT_TOKEN");
    apply_channel_bot_token_env_override(&mut channels, "telegram", "EGOPULSE_TELEGRAM_BOT_TOKEN");

    validate_channel_provider_references(&providers, &channels)?;

    let config = Config {
        default_provider,
        default_model,
        providers,
        state_root,
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

fn normalize_channels(
    channels: HashMap<String, FileChannelConfig>,
) -> HashMap<ChannelName, ChannelConfig> {
    let mut normalized = HashMap::new();
    for (name, fc) in channels {
        let key = ChannelName::new(&name);
        if key.as_str().is_empty() {
            continue;
        }
        let mut config = ChannelConfig::from(fc);
        config.file_auth_token = normalize_string(config.auth_token.clone());
        config.file_bot_token = normalize_string(config.bot_token.clone());
        normalized.insert(key, config);
    }

    if let Some(web) = normalized.get_mut("web") {
        if web.host.is_none() {
            web.host = Some(super::resolve::default_web_host().to_string());
        }
        if web.port.is_none() {
            web.port = Some(super::resolve::default_web_port());
        }
    }

    normalized
}

fn normalize_provider_map(
    providers: HashMap<String, FileProviderConfig>,
    allow_missing_api_key: bool,
) -> Result<HashMap<ProviderId, ProviderConfig>, ConfigError> {
    let mut normalized = HashMap::new();

    for (name, file_provider) in providers {
        let key = ProviderId::new(&normalize_string(Some(name)).ok_or(ConfigError::MissingProvider)?);
        let label = normalize_string(file_provider.label).unwrap_or_else(|| key.to_string());
        let base_url = normalize_string(file_provider.base_url).ok_or_else(|| {
            ConfigError::MissingProviderBaseUrl {
                provider: key.to_string(),
            }
        })?;
        validate_base_url(&base_url)?;

        let default_model = normalize_string(file_provider.default_model).ok_or_else(|| {
            ConfigError::MissingProviderDefaultModel {
                provider: key.to_string(),
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
                provider: key.to_string(),
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

fn validate_channel_provider_references(
    providers: &HashMap<ProviderId, ProviderConfig>,
    channels: &HashMap<ChannelName, ChannelConfig>,
) -> Result<(), ConfigError> {
    for channel in channels.values() {
        if let Some(provider) = channel.provider.as_ref()
            && !providers.contains_key(provider.as_str())
        {
            return Err(ConfigError::InvalidProviderReference {
                provider: provider.clone(),
            });
        }
    }

    Ok(())
}

fn apply_web_channel_env_overrides(channels: &mut HashMap<ChannelName, ChannelConfig>) {
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

    let web = channels.entry(ChannelName::new("web")).or_default();
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
        web.host = Some(super::resolve::default_web_host().to_string());
    }
    if web.port.is_none() {
        web.port = Some(super::resolve::default_web_port());
    }
}

fn apply_channel_bot_token_env_override(
    channels: &mut HashMap<ChannelName, ChannelConfig>,
    channel_name: &str,
    env_key: &str,
) {
    if let Some(token) = env_var(env_key) {
        let channel = channels.entry(ChannelName::new(channel_name)).or_default();
        channel.bot_token = Some(token);
    }
}

fn env_var(key: &str) -> Option<String> {
    env::var(key)
        .ok()
        .and_then(|value| normalize_string(Some(value)))
}

fn first_non_empty<const N: usize>(values: [Option<String>; N]) -> Option<String> {
    values.into_iter().find_map(normalize_string)
}

pub(super) fn normalize_string(value: Option<String>) -> Option<String> {
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

/// Returns `true` if the base URL points to a local address that does not require an API key.
pub fn base_url_allows_empty_api_key(base_url: &str) -> bool {
    is_local_url(base_url)
}

fn parse_bool(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}
