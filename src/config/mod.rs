use std::collections::HashMap;
use std::path::Path;

use secrecy::SecretString;

use crate::error::ConfigError;

pub mod loader;
pub mod persist;
pub mod resolve;

pub use loader::base_url_allows_empty_api_key;
pub use resolve::{default_config_path, default_state_root, default_workspace_dir};

/// 小文字正規化済みのプロバイダー識別子。
#[derive(Clone, Debug, Hash, Eq, PartialEq, Ord, PartialOrd)]
pub struct ProviderId(String);

impl ProviderId {
    pub fn new(s: &str) -> Self {
        Self(s.trim().to_ascii_lowercase())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ProviderId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::borrow::Borrow<str> for ProviderId {
    fn borrow(&self) -> &str {
        &self.0
    }
}

impl From<&str> for ProviderId {
    fn from(s: &str) -> Self {
        Self::new(s)
    }
}

/// トリム + 小文字正規化済みのチャネル名。
#[derive(Clone, Debug, Hash, Eq, PartialEq, Ord, PartialOrd)]
pub struct ChannelName(String);

impl ChannelName {
    pub fn new(s: &str) -> Self {
        Self(s.trim().to_ascii_lowercase())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ChannelName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::borrow::Borrow<str> for ChannelName {
    fn borrow(&self) -> &str {
        &self.0
    }
}

impl From<&str> for ChannelName {
    fn from(s: &str) -> Self {
        Self::new(s)
    }
}

/// Per-channel configuration (web, discord, telegram).
#[derive(Clone, Default)]
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
    pub file_auth_token: Option<String>,
    /// Web: allowed Origin values for WebSocket connections.
    pub allowed_origins: Option<Vec<String>>,
    /// Discord / Telegram 共通: bot token
    pub bot_token: Option<String>,
    pub file_bot_token: Option<String>,
    /// Telegram: bot username (group メンション検知用)
    pub bot_username: Option<String>,
    /// Telegram: DM 許可ユーザー ID (空 = 全員許可)
    pub allowed_user_ids: Option<Vec<i64>>,
    /// Discord: 許可チャンネル ID (空 = 全チャンネル許可)
    pub allowed_channels: Option<Vec<u64>>,
    /// Soul file path for this channel. Relative path resolves from souls/ directory.
    pub soul_path: Option<String>,
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
            .field("soul_path", &self.soul_path)
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

/// Top-level application configuration resolved from file and environment variables.
#[derive(Clone)]
pub struct Config {
    pub default_provider: ProviderId,
    /// Optional global model override (YAML `default_model`).
    pub default_model: Option<String>,
    pub providers: HashMap<ProviderId, ProviderConfig>,
    pub state_root: String,
    pub log_level: String,
    pub compaction_timeout_secs: u64,
    pub max_history_messages: usize,
    pub max_session_messages: usize,
    pub compact_keep_recent: usize,
    pub channels: HashMap<ChannelName, ChannelConfig>,
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("default_provider", &self.default_provider)
            .field("default_model", &self.default_model)
            .field("providers", &self.providers)
            .field("state_root", &self.state_root)
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
        loader::build_config(config_path, false)
    }

    /// Load configuration, allowing a missing API key (used by setup/config editing).
    pub fn load_allow_missing_api_key(config_path: Option<&Path>) -> Result<Self, ConfigError> {
        loader::build_config(config_path, true)
    }
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

    use super::{Config, default_state_root, default_workspace_dir};
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

        assert_eq!(config.default_provider.as_str(), "openai");
        assert_eq!(config.global_provider().label, "OpenAI");
        assert_eq!(
            PathBuf::from(&config.state_root),
            default_state_root().unwrap()
        );
        assert_eq!(
            config.workspace_dir().unwrap(),
            default_workspace_dir().unwrap()
        );
        assert_eq!(
            config.skills_dir().unwrap(),
            default_state_root().unwrap().join("skills")
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
    fn soul_path_returns_state_root_soul_md() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let _home = EnvVarGuard::set("HOME", temp_dir.path());
        let file_path = write_config(&temp_dir, sample_config());
        let config = Config::load(Some(&file_path)).expect("load config");

        assert_eq!(
            config.soul_path(),
            PathBuf::from(&config.state_root).join("SOUL.md")
        );
    }

    #[test]
    #[serial]
    fn agents_path_returns_state_root_agents_md() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let _home = EnvVarGuard::set("HOME", temp_dir.path());
        let file_path = write_config(&temp_dir, sample_config());
        let config = Config::load(Some(&file_path)).expect("load config");

        assert_eq!(
            config.agents_path(),
            PathBuf::from(&config.state_root).join("AGENTS.md")
        );
    }

    #[test]
    #[serial]
    fn chat_agents_path_returns_groups_channel_chatid() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let _home = EnvVarGuard::set("HOME", temp_dir.path());
        let file_path = write_config(&temp_dir, sample_config());
        let config = Config::load(Some(&file_path)).expect("load config");

        assert_eq!(
            config.chat_agents_path("web", "thread-1"),
            PathBuf::from(&config.state_root)
                .join("runtime")
                .join("groups")
                .join("web")
                .join("thread-1")
                .join("AGENTS.md")
        );
    }

    #[test]
    #[serial]
    fn souls_dir_returns_state_root_souls() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let _home = EnvVarGuard::set("HOME", temp_dir.path());
        let file_path = write_config(&temp_dir, sample_config());
        let config = Config::load(Some(&file_path)).expect("load config");

        assert_eq!(
            config.souls_dir(),
            PathBuf::from(&config.state_root).join("souls")
        );
    }

    #[test]
    #[serial]
    fn chat_soul_path_returns_groups_channel_chatid() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let _home = EnvVarGuard::set("HOME", temp_dir.path());
        let file_path = write_config(&temp_dir, sample_config());
        let config = Config::load(Some(&file_path)).expect("load config");

        assert_eq!(
            config.chat_soul_path("discord", "thread-42"),
            PathBuf::from(&config.state_root)
                .join("runtime")
                .join("groups")
                .join("discord")
                .join("thread-42")
                .join("SOUL.md")
        );
    }

    #[test]
    #[serial]
    fn channel_soul_path_reads_from_config() {
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
    soul_path: work"#,
        );
        let config = Config::load(Some(&file_path)).expect("load config");

        let web = config.channels.get("web").expect("web channel");
        assert_eq!(web.soul_path.as_deref(), Some("work"));
    }

    #[test]
    #[serial]
    fn channel_soul_path_none_when_unset() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let _home = EnvVarGuard::set("HOME", temp_dir.path());
        let file_path = write_config(&temp_dir, sample_config());
        let config = Config::load(Some(&file_path)).expect("load config");

        let web = config.channels.get("web").expect("web channel");
        assert!(web.soul_path.is_none());
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

    #[test]
    fn provider_id_normalizes_case() {
        let id = super::ProviderId::new("OpenAI");
        assert_eq!(id.as_str(), "openai");
    }

    #[test]
    fn channel_name_trims_whitespace() {
        let name = super::ChannelName::new(" Web ");
        assert_eq!(name.as_str(), "web");
    }
}
