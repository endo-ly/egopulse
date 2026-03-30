use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use url::Url;

use crate::error::ConfigError;

const OPENAI_PROVIDER: &str = "openai";
const OPENROUTER_PROVIDER: &str = "openrouter";
const LMSTUDIO_PROVIDER: &str = "lmstudio";

#[derive(Debug, Deserialize, Default)]
struct FileConfig {
    provider: Option<String>,
    model: Option<String>,
    api_key: Option<String>,
    base_url: Option<String>,
    log_level: Option<String>,
}

#[derive(Clone)]
pub struct Config {
    pub llm_provider: String,
    pub model: String,
    pub api_key: Option<SecretString>,
    pub llm_base_url: String,
    pub log_level: String,
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("llm_provider", &self.llm_provider)
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
            .field("log_level", &self.log_level)
            .finish()
    }
}

impl Config {
    pub fn load(config_path: Option<&Path>) -> Result<Self, ConfigError> {
        let file_config = read_file_config(config_path)?;

        let llm_provider = first_non_empty([
            env_var("EGOPULSE_PROVIDER"),
            file_config.provider,
            Some(OPENAI_PROVIDER.to_string()),
        ])
        .ok_or_else(|| ConfigError::InvalidProvider {
            provider: String::new(),
        })?;
        let llm_provider = normalize_provider_name(&llm_provider)?;

        let model = first_non_empty([
            env_var("EGOPULSE_MODEL"),
            file_config.model,
            Some(default_model_for_provider_name(&llm_provider).to_string()),
        ])
        .ok_or(ConfigError::MissingModel)?;

        let llm_base_url = first_non_empty([
            env_var("EGOPULSE_BASE_URL"),
            file_config.base_url,
            Some(default_base_url_for_provider_name(&llm_provider).to_string()),
        ])
        .ok_or(ConfigError::MissingBaseUrl)?;
        validate_base_url(&llm_base_url)?;

        let api_key = first_non_empty([env_var("EGOPULSE_API_KEY"), file_config.api_key])
            .map(|value| SecretString::new(value.into_boxed_str()));
        if api_key.is_none() && !provider_allows_empty_api_key(&llm_provider, &llm_base_url) {
            return Err(ConfigError::MissingApiKey);
        }

        let log_level = first_non_empty([env_var("EGOPULSE_LOG_LEVEL"), file_config.log_level])
            .unwrap_or_else(|| "info".to_string());

        Ok(Self {
            llm_provider,
            model,
            api_key,
            llm_base_url,
            log_level,
        })
    }
}

pub fn default_model_for_provider_name(provider: &str) -> &'static str {
    match provider {
        OPENROUTER_PROVIDER => "openai/gpt-4o-mini",
        LMSTUDIO_PROVIDER => "local-model",
        _ => "gpt-4o-mini",
    }
}

pub fn default_base_url_for_provider_name(provider: &str) -> &'static str {
    match provider {
        OPENROUTER_PROVIDER => "https://openrouter.ai/api/v1",
        LMSTUDIO_PROVIDER => "http://127.0.0.1:1234/v1",
        _ => "https://api.openai.com/v1",
    }
}

pub fn normalize_provider_name(value: &str) -> Result<String, ConfigError> {
    let normalized = value.trim().to_ascii_lowercase();
    match normalized.as_str() {
        OPENAI_PROVIDER | OPENROUTER_PROVIDER | LMSTUDIO_PROVIDER => Ok(normalized),
        _ => Err(ConfigError::InvalidProvider {
            provider: value.trim().to_string(),
        }),
    }
}

pub fn provider_allows_empty_api_key(provider: &str, base_url: &str) -> bool {
    provider == LMSTUDIO_PROVIDER && is_local_url(base_url)
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
    toml::from_str(&contents).map_err(|source| ConfigError::ConfigParseFailed {
        path: PathBuf::from(path),
        source,
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

pub fn authorization_token(config: &Config) -> Option<&str> {
    config.api_key.as_ref().map(ExposeSecret::expose_secret)
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use serial_test::serial;

    use super::{Config, authorization_token};

    fn clear_env() {
        unsafe {
            std::env::remove_var("EGOPULSE_PROVIDER");
            std::env::remove_var("EGOPULSE_MODEL");
            std::env::remove_var("EGOPULSE_API_KEY");
            std::env::remove_var("EGOPULSE_BASE_URL");
            std::env::remove_var("EGOPULSE_LOG_LEVEL");
        }
    }

    #[test]
    #[serial]
    fn loads_from_config_file() {
        clear_env();
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let file_path = temp_dir.path().join("egopulse.toml");
        let mut file = std::fs::File::create(&file_path).expect("create config");
        writeln!(
            file,
            "provider = \"openrouter\"\nmodel = \"openai/gpt-4o-mini\"\napi_key = \"sk-file\"\nbase_url = \"https://openrouter.ai/api/v1\"\nlog_level = \"debug\""
        )
        .expect("write config");

        let config = Config::load(Some(&file_path)).expect("load config");

        assert_eq!(config.llm_provider, "openrouter");
        assert_eq!(config.model, "openai/gpt-4o-mini");
        assert_eq!(authorization_token(&config), Some("sk-file"));
        assert_eq!(config.llm_base_url, "https://openrouter.ai/api/v1");
        assert_eq!(config.log_level, "debug");
    }

    #[test]
    #[serial]
    fn environment_overrides_file_values() {
        clear_env();
        unsafe {
            std::env::set_var("EGOPULSE_PROVIDER", "openai");
            std::env::set_var("EGOPULSE_MODEL", "gpt-4o-mini");
            std::env::set_var("EGOPULSE_API_KEY", "sk-env");
            std::env::set_var("EGOPULSE_BASE_URL", "https://api.openai.com/v1");
            std::env::set_var("EGOPULSE_LOG_LEVEL", "trace");
        }

        let temp_dir = tempfile::tempdir().expect("tempdir");
        let file_path = temp_dir.path().join("egopulse.toml");
        let mut file = std::fs::File::create(&file_path).expect("create config");
        writeln!(
            file,
            "provider = \"lmstudio\"\nmodel = \"local-model\"\nbase_url = \"http://127.0.0.1:1234/v1\""
        )
        .expect("write config");

        let config = Config::load(Some(&file_path)).expect("load config");

        assert_eq!(config.llm_provider, "openai");
        assert_eq!(config.model, "gpt-4o-mini");
        assert_eq!(authorization_token(&config), Some("sk-env"));
        assert_eq!(config.llm_base_url, "https://api.openai.com/v1");
        assert_eq!(config.log_level, "trace");
        clear_env();
    }

    #[test]
    #[serial]
    fn allows_lmstudio_without_api_key() {
        clear_env();
        unsafe {
            std::env::set_var("EGOPULSE_PROVIDER", "lmstudio");
        }

        let config = Config::load(None).expect("load config");

        assert_eq!(config.llm_provider, "lmstudio");
        assert_eq!(authorization_token(&config), None);
        assert_eq!(config.llm_base_url, "http://127.0.0.1:1234/v1");
        clear_env();
    }
}
