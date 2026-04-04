use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use serde_yml;
use url::Url;

use crate::error::ConfigError;

#[derive(Clone, Deserialize, Default)]
pub struct ChannelConfig {
    pub enabled: Option<bool>,
    pub port: Option<u16>,
    pub host: Option<String>,
    /// Web: browser/client authentication token.
    pub auth_token: Option<String>,
    /// Web: allowed Origin values for WebSocket connections.
    pub allowed_origins: Option<Vec<String>>,
    /// Discord / Telegram 共通: bot token
    pub bot_token: Option<String>,
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

#[derive(Debug, Deserialize, Default)]
struct FileConfig {
    model: Option<String>,
    api_key: Option<String>,
    base_url: Option<String>,
    data_dir: Option<String>,
    log_level: Option<String>,
    channels: Option<HashMap<String, ChannelConfig>>,
}

#[derive(Clone)]
pub struct Config {
    pub model: String,
    pub api_key: Option<SecretString>,
    pub llm_base_url: String,
    pub data_dir: String,
    pub log_level: String,
    pub channels: HashMap<String, ChannelConfig>,
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("model", &self.model)
            .field(
                "api_key",
                &self
                    .api_key
                    .as_ref()
                    .map(|_| "<redacted>")
                    .unwrap_or("<none>"),
            )
            .field("llm_base_url", &self.llm_base_url)
            .field("data_dir", &self.data_dir)
            .field("log_level", &self.log_level)
            .field("channels", &self.channels)
            .finish()
    }
}

impl Config {
    pub fn load(config_path: Option<&Path>) -> Result<Self, ConfigError> {
        build_config(config_path, false)
    }

    pub fn load_allow_missing_api_key(config_path: Option<&Path>) -> Result<Self, ConfigError> {
        build_config(config_path, true)
    }

    pub fn web_enabled(&self) -> bool {
        self.channels
            .get("web")
            .and_then(|c| c.enabled)
            .unwrap_or(false)
    }

    pub fn web_host(&self) -> String {
        self.channels
            .get("web")
            .and_then(|c| c.host.clone())
            .unwrap_or_else(|| default_web_host().to_string())
    }

    pub fn web_port(&self) -> u16 {
        self.channels
            .get("web")
            .and_then(|c| c.port)
            .unwrap_or_else(default_web_port)
    }

    pub fn web_auth_token(&self) -> Option<&str> {
        self.channels
            .get("web")
            .and_then(|c| c.auth_token.as_deref())
            .map(str::trim)
            .filter(|token| !token.is_empty())
    }

    pub fn web_allowed_origins(&self) -> Vec<String> {
        self.channels
            .get("web")
            .and_then(|c| c.allowed_origins.clone())
            .unwrap_or_default()
            .into_iter()
            .filter_map(|origin| normalize_string(Some(origin)))
            .collect()
    }

    pub fn channel_enabled(&self, channel: &str) -> bool {
        let needle = channel.trim().to_ascii_lowercase();
        self.channels
            .get(&needle)
            .and_then(|c| c.enabled)
            .unwrap_or(false)
    }

    pub fn resolve_config_path() -> Result<Option<PathBuf>, ConfigError> {
        let candidate = PathBuf::from("./egopulse.config.yaml");
        if candidate.exists() {
            return Ok(Some(candidate));
        }

        if env_vars_sufficient_for_runtime() {
            return Ok(None);
        }

        Err(ConfigError::AutoConfigNotFound {
            searched_paths: vec![PathBuf::from("./egopulse.config.yaml")],
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
}

fn normalize_channels(
    mut channels: HashMap<String, ChannelConfig>,
) -> HashMap<String, ChannelConfig> {
    let mut normalized = HashMap::new();
    for (name, config) in channels.drain() {
        let key = name.trim().to_ascii_lowercase();
        if key.is_empty() {
            continue;
        }
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
    let file_config = read_file_config(resolved_config_path.as_deref())?;

    let model = first_non_empty([
        env_var("EGOPULSE_MODEL"),
        file_config.model,
        Some(default_model().to_string()),
    ])
    .ok_or(ConfigError::MissingModel)?;

    let llm_base_url = first_non_empty([
        env_var("EGOPULSE_BASE_URL"),
        file_config.base_url,
        Some(default_llm_base_url().to_string()),
    ])
    .ok_or(ConfigError::MissingBaseUrl)?;
    validate_base_url(&llm_base_url)?;

    let api_key = first_non_empty([env_var("EGOPULSE_API_KEY"), file_config.api_key])
        .map(|value| SecretString::new(value.into_boxed_str()));
    if !allow_missing_api_key && api_key.is_none() && !base_url_allows_empty_api_key(&llm_base_url)
    {
        return Err(ConfigError::MissingApiKey);
    }

    let data_dir = env_var("EGOPULSE_DATA_DIR")
        .or_else(|| resolve_data_dir(resolved_config_path.as_deref(), file_config.data_dir))
        .unwrap_or_else(|| default_data_dir().to_string());

    let log_level = first_non_empty([env_var("EGOPULSE_LOG_LEVEL"), file_config.log_level])
        .unwrap_or_else(|| "info".to_string());

    let mut channels = normalize_channels(file_config.channels.unwrap_or_default());
    apply_web_channel_env_overrides(&mut channels);
    apply_channel_bot_token_env_override(&mut channels, "discord", "EGOPULSE_DISCORD_BOT_TOKEN");
    apply_channel_bot_token_env_override(&mut channels, "telegram", "EGOPULSE_TELEGRAM_BOT_TOKEN");

    Ok(Config {
        model,
        api_key,
        llm_base_url,
        data_dir,
        log_level,
        channels,
    })
}

fn parse_bool(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

fn default_model() -> &'static str {
    "gpt-4o-mini"
}

fn default_llm_base_url() -> &'static str {
    "https://api.openai.com/v1"
}

fn default_data_dir() -> &'static str {
    ".egopulse"
}

fn default_web_host() -> &'static str {
    "127.0.0.1"
}

fn default_web_port() -> u16 {
    10961
}

fn env_vars_sufficient_for_runtime() -> bool {
    let Some(base_url) = env_var("EGOPULSE_BASE_URL") else {
        return false;
    };

    env_var("EGOPULSE_MODEL").is_some()
        && (env_var("EGOPULSE_API_KEY").is_some() || base_url_allows_empty_api_key(&base_url))
}

pub fn base_url_allows_empty_api_key(base_url: &str) -> bool {
    is_local_url(base_url)
}

fn validate_base_url(value: &str) -> Result<(), ConfigError> {
    Url::parse(value)
        .map(|_| ())
        .map_err(|_| ConfigError::InvalidBaseUrl)
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

fn resolve_data_dir(config_path: Option<&Path>, value: Option<String>) -> Option<String> {
    let raw = normalize_string(value)?;
    let path = PathBuf::from(&raw);
    if path.is_absolute() {
        return Some(raw);
    }

    let base_dir = config_path
        .and_then(Path::parent)
        .unwrap_or_else(|| Path::new("."));
    Some(base_dir.join(path).to_string_lossy().into_owned())
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

pub fn authorization_token(config: &Config) -> Option<&str> {
    config.api_key.as_ref().map(ExposeSecret::expose_secret)
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::path::PathBuf;

    use serial_test::serial;

    use super::{Config, authorization_token};
    use crate::error::ConfigError;

    fn clear_env() {
        unsafe {
            std::env::remove_var("EGOPULSE_MODEL");
            std::env::remove_var("EGOPULSE_API_KEY");
            std::env::remove_var("EGOPULSE_BASE_URL");
            std::env::remove_var("EGOPULSE_DATA_DIR");
            std::env::remove_var("EGOPULSE_LOG_LEVEL");
            std::env::remove_var("EGOPULSE_WEB_ENABLED");
            std::env::remove_var("EGOPULSE_WEB_HOST");
            std::env::remove_var("EGOPULSE_WEB_PORT");
            std::env::remove_var("EGOPULSE_WEB_AUTH_TOKEN");
            std::env::remove_var("EGOPULSE_WEB_ALLOWED_ORIGINS");
        }
    }

    #[test]
    #[serial]
    fn loads_from_config_file() {
        clear_env();
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let file_path = temp_dir.path().join("egopulse.config.yaml");
        let mut file = std::fs::File::create(&file_path).expect("create config");
        writeln!(
            file,
            "model: openai/gpt-4o-mini\napi_key: sk-file\nbase_url: https://openrouter.ai/api/v1\ndata_dir: ./runtime\nlog_level: debug\nchannels:\n  web:\n    enabled: true"
        )
        .expect("write config");

        let config = Config::load(Some(&file_path)).expect("load config");

        assert_eq!(config.model, "openai/gpt-4o-mini");
        assert_eq!(authorization_token(&config), Some("sk-file"));
        assert_eq!(config.llm_base_url, "https://openrouter.ai/api/v1");
        assert_eq!(
            PathBuf::from(&config.data_dir),
            file_path.parent().expect("dir").join("./runtime")
        );
        assert_eq!(config.log_level, "debug");
        assert!(config.web_enabled());
        assert_eq!(config.web_host(), "127.0.0.1");
        assert_eq!(config.web_port(), 10961);
        assert_eq!(config.web_auth_token(), None);
        assert!(config.web_allowed_origins().is_empty());
        assert!(config.channel_enabled("web"));
    }

    #[test]
    #[serial]
    fn environment_overrides_file_values() {
        clear_env();
        unsafe {
            std::env::set_var("EGOPULSE_MODEL", "gpt-4o-mini");
            std::env::set_var("EGOPULSE_API_KEY", "sk-env");
            std::env::set_var("EGOPULSE_BASE_URL", "https://api.openai.com/v1");
            std::env::set_var("EGOPULSE_DATA_DIR", "/tmp/egopulse-env");
            std::env::set_var("EGOPULSE_LOG_LEVEL", "trace");
            std::env::set_var("EGOPULSE_WEB_ENABLED", "false");
            std::env::set_var("EGOPULSE_WEB_HOST", "0.0.0.0");
            std::env::set_var("EGOPULSE_WEB_PORT", "8080");
            std::env::set_var("EGOPULSE_WEB_AUTH_TOKEN", "web-secret");
            std::env::set_var(
                "EGOPULSE_WEB_ALLOWED_ORIGINS",
                "https://egopulse.tailnet.ts.net, http://127.0.0.1:10961",
            );
        }

        let temp_dir = tempfile::tempdir().expect("tempdir");
        let file_path = temp_dir.path().join("egopulse.config.yaml");
        let mut file = std::fs::File::create(&file_path).expect("create config");
        writeln!(
            file,
            "model: local-model\nbase_url: http://127.0.0.1:1234/v1\nchannels:\n  web:\n    enabled: true\n    host: 127.0.0.1\n    port: 10961"
        )
        .expect("write config");

        let config = Config::load(Some(&file_path)).expect("load config");

        assert_eq!(config.model, "gpt-4o-mini");
        assert_eq!(authorization_token(&config), Some("sk-env"));
        assert_eq!(config.llm_base_url, "https://api.openai.com/v1");
        assert_eq!(config.data_dir, "/tmp/egopulse-env");
        assert_eq!(config.log_level, "trace");
        assert!(!config.web_enabled());
        assert_eq!(config.web_host(), "0.0.0.0");
        assert_eq!(config.web_port(), 8080);
        assert_eq!(config.web_auth_token(), Some("web-secret"));
        assert_eq!(
            config.web_allowed_origins(),
            vec![
                "https://egopulse.tailnet.ts.net".to_string(),
                "http://127.0.0.1:10961".to_string(),
            ]
        );
        assert!(!config.channel_enabled("web"));
        clear_env();
    }

    #[test]
    #[serial]
    fn loads_web_settings_from_config_file() {
        clear_env();
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let file_path = temp_dir.path().join("egopulse.config.yaml");
        let mut file = std::fs::File::create(&file_path).expect("create config");
        writeln!(
            file,
            "model: gpt-4o-mini\napi_key: sk-file\nbase_url: https://api.openai.com/v1\nchannels:\n  web:\n    enabled: false\n    host: 0.0.0.0\n    port: 4010\n    auth_token: web-secret\n    allowed_origins:\n      - https://egopulse.tailnet.ts.net"
        )
        .expect("write config");

        let config = Config::load(Some(&file_path)).expect("load config");

        assert!(!config.web_enabled());
        assert_eq!(config.web_host(), "0.0.0.0");
        assert_eq!(config.web_port(), 4010);
        assert_eq!(config.web_auth_token(), Some("web-secret"));
        assert_eq!(
            config.web_allowed_origins(),
            vec!["https://egopulse.tailnet.ts.net".to_string()]
        );
        assert!(!config.channel_enabled("web"));
    }

    #[test]
    #[serial]
    fn injects_default_host_and_port_for_web_channel() {
        clear_env();
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let file_path = temp_dir.path().join("egopulse.config.yaml");
        let mut file = std::fs::File::create(&file_path).expect("create config");
        writeln!(
            file,
            "model: gpt-4o-mini\napi_key: sk-file\nbase_url: https://api.openai.com/v1\nchannels:\n  web:\n    enabled: true"
        )
        .expect("write config");

        let config = Config::load(Some(&file_path)).expect("load config");

        let web = config.channels.get("web").expect("web channel");
        assert_eq!(web.enabled, Some(true));
        assert_eq!(web.host.as_deref(), Some("127.0.0.1"));
        assert_eq!(web.port, Some(10961));
        assert_eq!(web.auth_token.as_deref(), None);
        assert_eq!(web.allowed_origins.as_ref(), None);
        assert!(config.channel_enabled("web"));
    }

    #[test]
    #[serial]
    fn allows_lmstudio_without_api_key() {
        clear_env();
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let current_dir = std::env::current_dir().expect("current dir");
        std::env::set_current_dir(temp_dir.path()).expect("set current dir");
        unsafe {
            std::env::set_var("EGOPULSE_MODEL", "local-model");
            std::env::set_var("EGOPULSE_BASE_URL", "http://127.0.0.1:1234/v1");
        }

        let config = Config::load(None).expect("load config");
        std::env::set_current_dir(current_dir).expect("restore current dir");

        assert_eq!(config.model, "local-model");
        assert_eq!(authorization_token(&config), None);
        assert_eq!(config.llm_base_url, "http://127.0.0.1:1234/v1");
        assert_eq!(config.data_dir, ".egopulse");
        clear_env();
    }

    #[test]
    #[serial]
    fn allows_lmstudio_without_api_key_via_file_config() {
        clear_env();
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let file_path = temp_dir.path().join("egopulse.config.yaml");
        let mut file = std::fs::File::create(&file_path).expect("create config");
        writeln!(
            file,
            "model: local-model\nbase_url: http://127.0.0.1:1234/v1"
        )
        .expect("write config");

        let config = Config::load(Some(&file_path)).expect("load config");

        assert_eq!(config.model, "local-model");
        assert_eq!(authorization_token(&config), None);
        assert_eq!(config.llm_base_url, "http://127.0.0.1:1234/v1");
    }

    #[test]
    #[serial]
    fn rejects_missing_api_key_for_remote_base_url() {
        clear_env();
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let file_path = temp_dir.path().join("egopulse.config.yaml");
        let mut file = std::fs::File::create(&file_path).expect("create config");
        writeln!(
            file,
            "model: gpt-4o-mini\nbase_url: https://api.openai.com/v1"
        )
        .expect("write config");

        let error = Config::load(Some(&file_path)).expect_err("missing api key");
        assert!(matches!(error, ConfigError::MissingApiKey));
    }

    #[test]
    #[serial]
    fn auto_config_path_prefers_project_file() {
        clear_env();
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let file_path = temp_dir.path().join("egopulse.config.yaml");
        std::fs::write(
            &file_path,
            "model: gpt-4o-mini\napi_key: sk\nbase_url: https://api.openai.com/v1",
        )
        .expect("write config");

        let current_dir = std::env::current_dir().expect("current dir");
        std::env::set_current_dir(temp_dir.path()).expect("set current dir");

        let resolved = Config::resolve_config_path().expect("resolve config path");

        std::env::set_current_dir(current_dir).expect("restore current dir");
        assert_eq!(resolved, Some(PathBuf::from("./egopulse.config.yaml")));
    }

    #[test]
    #[serial]
    fn auto_config_path_accepts_env_only_runtime() {
        clear_env();
        unsafe {
            std::env::set_var("EGOPULSE_MODEL", "local-model");
            std::env::set_var("EGOPULSE_BASE_URL", "http://127.0.0.1:1234/v1");
        }

        let temp_dir = tempfile::tempdir().expect("tempdir");
        let current_dir = std::env::current_dir().expect("current dir");
        std::env::set_current_dir(temp_dir.path()).expect("set current dir");

        let resolved = Config::resolve_config_path().expect("resolve config path");

        std::env::set_current_dir(current_dir).expect("restore current dir");
        assert_eq!(resolved, None);
        clear_env();
    }

    #[test]
    #[serial]
    fn auto_config_path_errors_when_missing_file_and_env() {
        clear_env();
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let current_dir = std::env::current_dir().expect("current dir");
        std::env::set_current_dir(temp_dir.path()).expect("set current dir");

        let error = Config::resolve_config_path().expect_err("resolve failure");

        std::env::set_current_dir(current_dir).expect("restore current dir");
        assert!(matches!(error, ConfigError::AutoConfigNotFound { .. }));
    }
}
