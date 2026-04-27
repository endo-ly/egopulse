use std::collections::HashMap;
use std::path::Path;

use secrecy::SecretString;

use crate::error::ConfigError;

pub mod loader;
pub mod persist;
pub mod resolve;
pub(crate) mod secret_ref;

pub use loader::{base_url_allows_empty_api_key, is_valid_base_url};
pub use resolve::{default_config_path, default_state_root, default_workspace_dir};

use self::secret_ref::ResolvedValue;

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

/// Trimmed + lowercased agent identifier.
#[derive(Clone, Debug, Hash, Eq, PartialEq, Ord, PartialOrd)]
pub struct AgentId(String);

impl AgentId {
    pub fn new(s: &str) -> Self {
        Self(s.trim().to_ascii_lowercase())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for AgentId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::borrow::Borrow<str> for AgentId {
    fn borrow(&self) -> &str {
        &self.0
    }
}

impl From<&str> for AgentId {
    fn from(s: &str) -> Self {
        Self::new(s)
    }
}

/// Trimmed + lowercased Discord bot identifier.
///
/// Rejects empty strings, `..`, `/`, `\`, and `:` to avoid path-traversal or
/// ambiguous session keys.
#[derive(Clone, Debug, Hash, Eq, PartialEq, Ord, PartialOrd)]
pub struct BotId(String);

impl BotId {
    /// Creates a new `BotId` after trimming whitespace and lowercasing.
    pub fn new(s: &str) -> Self {
        Self(s.trim().to_ascii_lowercase())
    }

    /// Returns the bot identifier as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for BotId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::borrow::Borrow<str> for BotId {
    fn borrow(&self) -> &str {
        &self.0
    }
}

impl From<&str> for BotId {
    fn from(s: &str) -> Self {
        Self::new(s)
    }
}

/// Per-bot Discord configuration stored under `channels.discord.bots.<bot_id>`.
///
/// Each bot connects to Discord with its own token and routes messages to agents
/// based on `channel_agents` or falls back to `default_agent`.
#[derive(Clone)]
pub struct DiscordBotConfig {
    pub token: Option<ResolvedValue>,
    pub file_token: Option<serde_yml::Value>,
    pub default_agent: Option<AgentId>,
    pub allowed_channels: Option<Vec<u64>>,
    pub channel_agents: Option<HashMap<u64, AgentId>>,
}

impl std::fmt::Debug for DiscordBotConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DiscordBotConfig")
            .field(
                "token",
                &self
                    .token
                    .as_ref()
                    .map(|_| "<redacted>")
                    .unwrap_or("<none>"),
            )
            .field("default_agent", &self.default_agent)
            .field("allowed_channels", &self.allowed_channels)
            .field("channel_agents", &self.channel_agents)
            .finish()
    }
}

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
    pub auth_token: Option<ResolvedValue>,
    /// YAML 保存用に auth_token の SecretRef 構造を保持する。
    pub file_auth_token: Option<serde_yml::Value>,
    /// Web: allowed Origin values for WebSocket connections.
    pub allowed_origins: Option<Vec<String>>,
    /// Discord / Telegram 共通: bot token
    pub bot_token: Option<ResolvedValue>,
    /// YAML 保存用に bot_token の SecretRef 構造を保持する。
    pub file_bot_token: Option<serde_yml::Value>,
    /// Telegram: bot username (group メンション検知用)
    pub bot_username: Option<String>,
    /// Discord: 許可チャンネル ID。空 = ギルドメッセージ全拒否（DM は常に許可）。
    /// 許可されたチャンネルでは @mention なしで即応答する。
    pub allowed_channels: Option<Vec<u64>>,
    /// Telegram: 許可グループ/スーパーグループの chat ID。空 = グループメッセージ全拒否（DM は常に許可）。
    /// 許可されたチャットでは @mention なしで即応答する。
    pub allowed_chat_ids: Option<Vec<i64>>,
    /// Soul file path for this channel. Relative path resolves from souls/ directory.
    pub soul_path: Option<String>,
    /// Discord bot definitions keyed by bot identifier.
    pub discord_bots: Option<HashMap<BotId, DiscordBotConfig>>,
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
            .field("allowed_channels", &self.allowed_channels)
            .field("allowed_chat_ids", &self.allowed_chat_ids)
            .field("soul_path", &self.soul_path)
            .field("discord_bots", &self.discord_bots)
            .finish()
    }
}

#[derive(Clone)]
pub struct ProviderConfig {
    pub label: String,
    pub base_url: String,
    pub api_key: Option<ResolvedValue>,
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

#[derive(Clone, Default)]
pub struct AgentDiscordConfig {
    pub bot_token: Option<ResolvedValue>,
    pub file_bot_token: Option<serde_yml::Value>,
    pub allowed_channels: Option<Vec<u64>>,
}

impl std::fmt::Debug for AgentDiscordConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentDiscordConfig")
            .field(
                "bot_token",
                &self
                    .bot_token
                    .as_ref()
                    .map(|_| "<redacted>")
                    .unwrap_or("<none>"),
            )
            .field("allowed_channels", &self.allowed_channels)
            .finish()
    }
}

#[derive(Clone, Default)]
pub struct AgentConfig {
    pub label: String,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub discord: AgentDiscordConfig,
}

impl std::fmt::Debug for AgentConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentConfig")
            .field("label", &self.label)
            .field("provider", &self.provider)
            .field("model", &self.model)
            .field("discord", &self.discord)
            .finish()
    }
}

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
    pub default_agent: AgentId,
    pub agents: HashMap<AgentId, AgentConfig>,
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
            .field("default_agent", &self.default_agent)
            .field("agents", &self.agents)
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

    use std::collections::HashMap;
    use std::io::Write;
    use std::path::PathBuf;

    use secrecy::ExposeSecret;
    use serial_test::serial;

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

    #[test]
    #[serial]
    fn loads_agents_with_default_agent() {
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
default_agent: alice
agents:
  alice:
    label: Alice"#,
        );

        let config = Config::load(Some(&file_path)).expect("load config");

        assert_eq!(config.default_agent.as_str(), "alice");
        let alice = config.agents.get("alice").expect("alice agent");
        assert_eq!(alice.label, "Alice");
    }

    #[test]
    #[serial]
    fn default_agent_falls_back_to_default_when_missing() {
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

        assert_eq!(config.default_agent.as_str(), "default");
        assert!(config.agents.contains_key("default"));
    }

    #[test]
    #[serial]
    fn rejects_default_agent_not_in_agents() {
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
default_agent: missing
agents:
  alice:
    label: Alice"#,
        );

        let error = Config::load(Some(&file_path)).expect_err("should fail");
        assert!(matches!(
            error,
            ConfigError::DefaultAgentNotFound { agent_id } if agent_id == "missing"
        ));
    }

    #[test]
    #[serial]
    fn rejects_agent_id_path_traversal() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let _home = EnvVarGuard::set("HOME", temp_dir.path());

        for bad_id in ["../etc", "/etc", "", "foo:bar"] {
            let yaml = format!(
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
default_agent: alice
agents:
  "{bad_id}":
    label: Bad
  alice:
    label: Alice"#
            );
            let file_path = write_config(&temp_dir, &yaml);
            let error = Config::load(Some(&file_path)).expect_err("should reject bad agent id");
            assert!(
                matches!(error, ConfigError::InvalidAgentId { .. }),
                "expected InvalidAgentId for '{bad_id}', got {error:?}"
            );
        }
    }

    #[test]
    #[serial]
    fn persists_agents_without_leaking_secret_values() {
        use crate::config::persist::save_config_with_secrets;
        use crate::config::secret_ref::{env_resolved_value, env_yaml_value};

        let temp_dir = tempfile::tempdir().expect("tempdir");
        let _home = EnvVarGuard::set("HOME", temp_dir.path());
        let path = temp_dir.path().join("egopulse.config.yaml");

        let mut agents = std::collections::HashMap::new();
        agents.insert(
            super::AgentId::new("alice"),
            super::AgentConfig {
                label: "Alice".to_string(),
                discord: super::AgentDiscordConfig {
                    bot_token: Some(env_resolved_value(
                        "DISCORD_BOT_TOKEN_ALICE",
                        "discord-token-alice",
                    )),
                    file_bot_token: Some(env_yaml_value("DISCORD_BOT_TOKEN_ALICE")),
                    allowed_channels: Some(vec![123456]),
                },
                ..Default::default()
            },
        );
        agents.insert(
            super::AgentId::new("default"),
            super::AgentConfig {
                label: "Default Agent".to_string(),
                ..Default::default()
            },
        );

        let config = Config {
            default_provider: super::ProviderId::new("openai"),
            default_model: None,
            providers: std::collections::HashMap::from([(
                super::ProviderId::new("openai"),
                super::ProviderConfig {
                    label: "OpenAI".to_string(),
                    base_url: "https://api.openai.com/v1".to_string(),
                    api_key: Some(env_resolved_value("OPENAI_API_KEY", "sk-test")),
                    default_model: "gpt-5".to_string(),
                    models: vec!["gpt-5".to_string()],
                },
            )]),
            state_root: temp_dir.path().to_str().expect("path").to_string(),
            log_level: "info".to_string(),
            compaction_timeout_secs: 180,
            max_history_messages: 50,
            max_session_messages: 40,
            compact_keep_recent: 20,
            channels: std::collections::HashMap::new(),
            default_agent: super::AgentId::new("alice"),
            agents,
        };

        save_config_with_secrets(&config, &path).expect("save config");

        let yaml = std::fs::read_to_string(&path).expect("yaml");
        assert!(yaml.contains("default_agent: alice"));
        assert!(yaml.contains("label: Alice"));
        assert!(yaml.contains("id: DISCORD_BOT_TOKEN_ALICE"));
        assert!(yaml.contains("source: env"));
        assert!(!yaml.contains("discord-token-alice"));

        let dotenv = std::fs::read_to_string(temp_dir.path().join(".env")).expect(".env");
        assert!(dotenv.contains("DISCORD_BOT_TOKEN_ALICE=discord-token-alice"));
    }

    // --- Step 2: Agent LLM Resolution tests ---

    fn agent_config() -> &'static str {
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
  discord:
    enabled: true
    provider: local
    model: qwen2.5-coder
default_agent: alice
agents:
  alice:
    label: Alice
  bob:
    label: Bob
    provider: openai
    model: gpt-5-mini
  carol:
    label: Carol
    model: custom-model"#
    }

    #[test]
    #[serial]
    fn resolve_llm_for_agent_channel_uses_channel_provider_when_agent_has_none() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let _home = EnvVarGuard::set("HOME", temp_dir.path());
        let file_path = write_config(&temp_dir, agent_config());
        let config = Config::load(Some(&file_path)).expect("load config");

        let resolved = config
            .resolve_llm_for_agent_channel(&super::AgentId::new("alice"), "discord")
            .expect("resolve");

        assert_eq!(resolved.provider, "local");
        assert_eq!(resolved.model, "qwen2.5-coder");
    }

    #[test]
    #[serial]
    fn resolve_llm_for_agent_channel_agent_provider_overrides_channel() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let _home = EnvVarGuard::set("HOME", temp_dir.path());
        let file_path = write_config(&temp_dir, agent_config());
        let config = Config::load(Some(&file_path)).expect("load config");

        let resolved = config
            .resolve_llm_for_agent_channel(&super::AgentId::new("bob"), "discord")
            .expect("resolve");

        assert_eq!(resolved.provider, "openai");
        assert_eq!(resolved.model, "gpt-5-mini");
        assert_eq!(resolved.base_url, "https://api.openai.com/v1");
    }

    #[test]
    #[serial]
    fn resolve_llm_for_agent_channel_agent_model_overrides_channel_model() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let _home = EnvVarGuard::set("HOME", temp_dir.path());
        let file_path = write_config(&temp_dir, agent_config());
        let config = Config::load(Some(&file_path)).expect("load config");

        let resolved = config
            .resolve_llm_for_agent_channel(&super::AgentId::new("carol"), "discord")
            .expect("resolve");

        assert_eq!(resolved.provider, "local");
        assert_eq!(resolved.model, "custom-model");
    }

    #[test]
    #[serial]
    fn resolve_llm_for_agent_channel_falls_back_to_defaults() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let _home = EnvVarGuard::set("HOME", temp_dir.path());
        let file_path = write_config(&temp_dir, agent_config());
        let config = Config::load(Some(&file_path)).expect("load config");

        let resolved = config
            .resolve_llm_for_agent_channel(&super::AgentId::new("alice"), "web")
            .expect("resolve");

        assert_eq!(resolved.provider, "openai");
        assert_eq!(resolved.model, "gpt-5");
    }

    #[test]
    #[serial]
    fn resolve_llm_for_agent_channel_rejects_unknown_agent() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let _home = EnvVarGuard::set("HOME", temp_dir.path());
        let file_path = write_config(&temp_dir, agent_config());
        let config = Config::load(Some(&file_path)).expect("load config");

        let error = config
            .resolve_llm_for_agent_channel(&super::AgentId::new("unknown"), "discord")
            .expect_err("should fail");

        assert!(
            matches!(error, ConfigError::AgentNotFound { ref agent_id } if agent_id == "unknown"),
            "expected AgentNotFound, got {error:?}"
        );
    }

    #[test]
    #[serial]
    fn resolve_llm_for_agent_channel_rejects_unknown_provider() {
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
default_agent: alice
agents:
  alice:
    label: Alice
    provider: nonexistent"#,
        );
        let error = Config::load(Some(&file_path)).expect_err("should fail");

        assert!(matches!(
            error,
            ConfigError::InvalidProviderReference { provider } if provider == "nonexistent"
        ));
    }

    #[test]
    #[serial]
    fn resolve_llm_for_channel_delegates_to_default_agent() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let _home = EnvVarGuard::set("HOME", temp_dir.path());
        let file_path = write_config(&temp_dir, agent_config());
        let config = Config::load(Some(&file_path)).expect("load config");

        let via_channel = config.resolve_llm_for_channel("web").expect("via channel");
        let via_agent = config
            .resolve_llm_for_agent_channel(&config.default_agent, "web")
            .expect("via agent");

        assert_eq!(via_channel, via_agent);
    }

    #[test]
    #[serial]
    fn agent_soul_path_returns_agents_dir_soul() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let _home = EnvVarGuard::set("HOME", temp_dir.path());
        let file_path = write_config(&temp_dir, agent_config());
        let config = Config::load(Some(&file_path)).expect("load config");

        assert_eq!(
            config.agent_soul_path(&super::AgentId::new("alice")),
            PathBuf::from(&config.state_root)
                .join("agents")
                .join("alice")
                .join("SOUL.md")
        );
    }

    #[test]
    #[serial]
    fn agent_agents_path_returns_agents_dir_agents() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let _home = EnvVarGuard::set("HOME", temp_dir.path());
        let file_path = write_config(&temp_dir, agent_config());
        let config = Config::load(Some(&file_path)).expect("load config");

        assert_eq!(
            config.agent_agents_path(&super::AgentId::new("bob")),
            PathBuf::from(&config.state_root)
                .join("agents")
                .join("bob")
                .join("AGENTS.md")
        );
    }

    // --- Step 1: Discord Agent Bot Config Helper tests ---

    use crate::config::secret_ref::{env_resolved_value as lit_val, env_yaml_value as lit_yaml};

    // --- Discord Bot Config tests ---

    fn write_env(temp_dir: &tempfile::TempDir, contents: &str) {
        use std::io::Write as IoWrite;
        let env_path = temp_dir.path().join(".env");
        let mut f = std::fs::File::create(&env_path).expect("create .env");
        write!(f, "{contents}").expect("write .env");
    }

    fn bot_config_yml(bot_section: &str) -> String {
        format!(
            r#"default_provider: openai
providers:
  openai:
    label: OpenAI
    base_url: https://api.openai.com/v1
    api_key: sk-openai
    default_model: gpt-4o-mini
default_agent: assistant
agents:
  assistant:
    label: Assistant
  reviewer:
    label: Reviewer
channels:
  discord:
    enabled: true
{bot_section}"#
        )
    }

    #[test]
    #[serial]
    fn loads_discord_bots_with_default_agent() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let _home = EnvVarGuard::set("HOME", temp_dir.path());
        write_env(&temp_dir, "MY_DISCORD_TOKEN=discord-bot-token-123\n");
        let file_path = write_config(
            &temp_dir,
            &bot_config_yml(
                r#"    bots:
      main:
        token:
          source: env
          id: MY_DISCORD_TOKEN
        default_agent: assistant
        allowed_channels:
          - 111222333
        channel_agents:
          "444555666": reviewer"#,
            ),
        );

        let config = Config::load(Some(&file_path)).expect("load config");

        let discord = config.channels.get("discord").expect("discord channel");
        let bots = discord.discord_bots.as_ref().expect("bots");
        assert_eq!(bots.len(), 1);

        let main_bot = bots.get("main").expect("main bot");
        assert_eq!(
            main_bot
                .default_agent
                .as_ref()
                .expect("default_agent")
                .as_str(),
            "assistant"
        );
        assert_eq!(
            main_bot.token.as_ref().expect("token").value(),
            "discord-bot-token-123"
        );
        assert_eq!(
            main_bot.allowed_channels.as_deref().map(|v| v.to_vec()),
            Some(vec![111222333u64])
        );
        let channel_agents = main_bot.channel_agents.as_ref().expect("channel_agents");
        assert_eq!(
            channel_agents.get(&444555666u64),
            Some(&super::AgentId::new("reviewer"))
        );
    }

    #[test]
    #[serial]
    fn discord_bots_validate_default_agent_exists() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let _home = EnvVarGuard::set("HOME", temp_dir.path());
        write_env(&temp_dir, "MY_DISCORD_TOKEN=tok\n");
        let file_path = write_config(
            &temp_dir,
            &bot_config_yml(
                r#"    bots:
      main:
        token:
          source: env
          id: MY_DISCORD_TOKEN
        default_agent: nonexistent_agent"#,
            ),
        );

        let error = Config::load(Some(&file_path)).expect_err("should fail");

        assert!(
            matches!(
                error,
                ConfigError::DiscordBotDefaultAgentNotFound { ref bot_id, ref agent_id }
                    if bot_id == "main" && agent_id == "nonexistent_agent"
            ),
            "expected DiscordBotDefaultAgentNotFound, got {error:?}"
        );
    }

    #[test]
    #[serial]
    fn discord_bots_validate_channel_agents_exist() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let _home = EnvVarGuard::set("HOME", temp_dir.path());
        write_env(&temp_dir, "MY_DISCORD_TOKEN=tok\n");
        let file_path = write_config(
            &temp_dir,
            &bot_config_yml(
                r#"    bots:
      main:
        token:
          source: env
          id: MY_DISCORD_TOKEN
        default_agent: assistant
        channel_agents:
          "999": ghost_agent"#,
            ),
        );

        let error = Config::load(Some(&file_path)).expect_err("should fail");

        assert!(
            matches!(
                error,
                ConfigError::DiscordBotChannelAgentNotFound { ref bot_id, channel_id: 999, ref agent_id }
                    if bot_id == "main" && agent_id == "ghost_agent"
            ),
            "expected DiscordBotChannelAgentNotFound, got {error:?}"
        );
    }

    #[test]
    #[serial]
    fn discord_bots_default_agent_falls_back_to_global_default() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let _home = EnvVarGuard::set("HOME", temp_dir.path());
        write_env(&temp_dir, "MY_DISCORD_TOKEN=tok\n");
        let file_path = write_config(
            &temp_dir,
            &bot_config_yml(
                r#"    bots:
      main:
        token:
          source: env
          id: MY_DISCORD_TOKEN"#,
            ),
        );

        let config = Config::load(Some(&file_path)).expect("load config");

        let discord = config.channels.get("discord").expect("discord channel");
        let bots = discord.discord_bots.as_ref().expect("bots");
        let main_bot = bots.get("main").expect("main bot");
        assert_eq!(main_bot.default_agent, None);
    }

    #[test]
    #[serial]
    fn discord_bots_preserve_secret_refs_on_save() {
        use crate::config::persist::save_config_with_secrets;

        // Arrange
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let _home = EnvVarGuard::set("HOME", temp_dir.path());
        let path = temp_dir.path().join("egopulse.config.yaml");

        let mut agents = HashMap::new();
        agents.insert(
            super::AgentId::new("assistant"),
            super::AgentConfig {
                label: "Assistant".to_string(),
                ..Default::default()
            },
        );

        let mut discord_bots = HashMap::new();
        discord_bots.insert(
            super::BotId::new("main"),
            super::DiscordBotConfig {
                token: Some(lit_val("DISCORD_BOT_TOKEN", "secret-bot-token")),
                file_token: Some(lit_yaml("DISCORD_BOT_TOKEN")),
                default_agent: Some(super::AgentId::new("assistant")),
                allowed_channels: Some(vec![123456]),
                channel_agents: None,
            },
        );

        let mut channels = HashMap::new();
        channels.insert(
            super::ChannelName::new("discord"),
            super::ChannelConfig {
                enabled: Some(true),
                discord_bots: Some(discord_bots),
                ..Default::default()
            },
        );

        let config = Config {
            default_provider: super::ProviderId::new("openai"),
            default_model: None,
            providers: HashMap::from([(
                super::ProviderId::new("openai"),
                super::ProviderConfig {
                    label: "OpenAI".to_string(),
                    base_url: "https://api.openai.com/v1".to_string(),
                    api_key: Some(lit_val("OPENAI_API_KEY", "sk-test")),
                    default_model: "gpt-5".to_string(),
                    models: vec!["gpt-5".to_string()],
                },
            )]),
            state_root: temp_dir.path().to_str().expect("path").to_string(),
            log_level: "info".to_string(),
            compaction_timeout_secs: 180,
            max_history_messages: 50,
            max_session_messages: 40,
            compact_keep_recent: 20,
            channels,
            default_agent: super::AgentId::new("assistant"),
            agents,
        };

        // Act
        save_config_with_secrets(&config, &path).expect("save config");

        // Assert — YAML has SecretRef, not plain token
        let yaml = std::fs::read_to_string(&path).expect("yaml");
        assert!(yaml.contains("source: env"));
        assert!(yaml.contains("id: DISCORD_BOT_TOKEN"));
        assert!(!yaml.contains("secret-bot-token"));

        // Assert — .env has the actual token
        let dotenv = std::fs::read_to_string(temp_dir.path().join(".env")).expect(".env");
        assert!(dotenv.contains("DISCORD_BOT_TOKEN=secret-bot-token"));
    }

    // --- Step 2: Discord Bot Resolver tests ---

    #[test]
    #[serial]
    fn discord_bots_returns_only_channel_bots_with_token() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let _home = EnvVarGuard::set("HOME", temp_dir.path());
        write_env(&temp_dir, "MY_TOKEN=bot-token\n");
        let file_path = write_config(
            &temp_dir,
            &bot_config_yml(
                r#"    bots:
      main:
        token:
          source: env
          id: MY_TOKEN
        default_agent: assistant
      no_token_bot:
        default_agent: reviewer"#,
            ),
        );

        let config = Config::load(Some(&file_path)).expect("load config");
        let bots = config.discord_bots();

        assert_eq!(bots.len(), 1);
        assert_eq!(bots[0].bot_id.as_str(), "main");
        assert_eq!(bots[0].token, "bot-token");
    }

    #[test]
    #[serial]
    fn discord_bots_sort_by_bot_id() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let _home = EnvVarGuard::set("HOME", temp_dir.path());
        write_env(&temp_dir, "T1=t1\nT2=t2\n");
        let file_path = write_config(
            &temp_dir,
            &bot_config_yml(
                r#"    bots:
      zeta:
        token:
          source: env
          id: T1
        default_agent: assistant
      alpha:
        token:
          source: env
          id: T2
        default_agent: assistant"#,
            ),
        );

        let config = Config::load(Some(&file_path)).expect("load config");
        let bots = config.discord_bots();

        assert_eq!(bots.len(), 2);
        assert_eq!(bots[0].bot_id.as_str(), "alpha");
        assert_eq!(bots[1].bot_id.as_str(), "zeta");
    }

    #[test]
    #[serial]
    fn discord_bots_disabled_channel_returns_empty() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let _home = EnvVarGuard::set("HOME", temp_dir.path());
        write_env(&temp_dir, "MY_TOKEN=tok\n");
        let file_path = write_config(
            &temp_dir,
            r#"default_provider: openai
providers:
  openai:
    label: OpenAI
    base_url: https://api.openai.com/v1
    api_key: sk-openai
    default_model: gpt-4o-mini
default_agent: assistant
agents:
  assistant:
    label: Assistant
channels:
  discord:
    enabled: false
    bots:
      main:
        token:
          source: env
          id: MY_TOKEN
        default_agent: assistant"#,
        );

        let config = Config::load(Some(&file_path)).expect("load config");
        let bots = config.discord_bots();

        assert!(bots.is_empty());
    }

    #[test]
    #[serial]
    fn discord_bot_allowed_channels_empty_means_guild_reject() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let _home = EnvVarGuard::set("HOME", temp_dir.path());
        write_env(&temp_dir, "MY_TOKEN=token\n");
        let file_path = write_config(
            &temp_dir,
            &bot_config_yml(
                r#"    bots:
      main:
        token:
          source: env
          id: MY_TOKEN
        default_agent: assistant"#,
            ),
        );

        let config = Config::load(Some(&file_path)).expect("load config");
        let bots = config.discord_bots();

        assert_eq!(bots.len(), 1);
        assert_eq!(bots[0].allowed_channels, &[] as &[u64]);
    }

    #[test]
    #[serial]
    fn discord_bot_channel_agents_are_preserved() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let _home = EnvVarGuard::set("HOME", temp_dir.path());
        write_env(&temp_dir, "MY_TOKEN=token\n");
        let file_path = write_config(
            &temp_dir,
            &bot_config_yml(
                r#"    bots:
      main:
        token:
          source: env
          id: MY_TOKEN
        default_agent: assistant
        channel_agents:
          "42": reviewer"#,
            ),
        );

        let config = Config::load(Some(&file_path)).expect("load config");
        let bots = config.discord_bots();

        assert_eq!(bots.len(), 1);
        let agents = &bots[0].channel_agents;
        assert_eq!(agents.get(&42), Some(&super::AgentId::new("reviewer")));
    }
}
