use std::collections::HashMap;
use std::path::Path;

use secrecy::SecretString;

use crate::error::ConfigError;

use super::secret_ref::ResolvedValue;

// ---------------------------------------------------------------------------
// Lowercase newtype macro
// ---------------------------------------------------------------------------

/// Declares a trim + lowercase string newtype with standard trait impls.
macro_rules! define_lowercase_id {
    (
        $(#[$struct_meta:meta])*
        $vis:vis struct $name:ident
    ) => {
        $(#[$struct_meta])*
        #[derive(Clone, Debug, Hash, Eq, PartialEq, Ord, PartialOrd)]
        $vis struct $name(String);

        impl $name {
            /// Creates a new identifier after trimming whitespace and lowercasing.
            $vis fn new(s: &str) -> Self {
                Self(s.trim().to_ascii_lowercase())
            }

            /// Returns the identifier as a string slice.
            $vis fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl std::borrow::Borrow<str> for $name {
            fn borrow(&self) -> &str {
                &self.0
            }
        }

        impl From<&str> for $name {
            fn from(s: &str) -> Self {
                Self::new(s)
            }
        }
    };
}

define_lowercase_id! {
    /// 小文字正規化済みのプロバイダー識別子。
    pub(crate) struct ProviderId
}

define_lowercase_id! {
    /// トリム + 小文字正規化済みのチャネル名。
    pub(crate) struct ChannelName
}

define_lowercase_id! {
    /// Trimmed + lowercased agent identifier.
    pub(crate) struct AgentId
}

define_lowercase_id! {
    /// Trimmed + lowercased Discord bot identifier.
    pub(crate) struct BotId
}

define_lowercase_id! {
    /// Trimmed + lowercased webhook receiver identifier.
    pub(crate) struct WebhookReceiverId
}

#[derive(Clone, Debug, Default)]
pub(crate) struct DiscordChannelConfig {
    pub require_mention: bool,
    pub agents: Vec<AgentId>,
    pub multi_agent: bool,
    /// Whether this channel routes conversations to the isolated `secret.db`.
    pub secret: bool,
    /// Whether long-running turns post an editable tool-progress log.
    pub tool_progress: bool,
}

/// Per-chat Telegram configuration stored inside `ChannelConfig.telegram_channels`.
#[derive(Clone, Debug, Default)]
pub(crate) struct TelegramChatConfig {
    /// Whether this chat requires an explicit @mention to trigger the bot.
    pub require_mention: bool,
    /// Agents assigned to this chat.
    pub agents: Vec<AgentId>,
    /// Whether this chat operates as a multi-agent room.
    pub multi_agent: bool,
    /// Whether this chat routes conversations to the isolated `secret.db`.
    pub secret: bool,
    /// Whether long-running turns post an editable tool-progress log.
    pub tool_progress: bool,
}

/// Per-bot Telegram configuration stored under `channels.telegram.telegram_bots.<bot_id>`.
///
/// Each bot connects to Telegram with its own token and routes messages to agents
/// based on shared channel config or falls back to the global `default_agent`.
/// Channel-to-agent mappings live at the Telegram level (`channels.telegram.telegram_channels`).
#[derive(Clone)]
pub(crate) struct TelegramBotConfig {
    pub token: Option<ResolvedValue>,
    pub file_token: Option<yaml_serde::Value>,
}

impl std::fmt::Debug for TelegramBotConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TelegramBotConfig")
            .field("token", &debug_secret(self.token.as_ref()))
            .finish()
    }
}

/// Per-bot Discord configuration stored under `channels.discord.bots.<bot_id>`.
///
/// Each bot connects to Discord with its own token and routes messages to agents
/// based on shared channel config or falls back to the global `default_agent`.
/// Channel-to-agent mappings live at the Discord level (`channels.discord.channels`).
#[derive(Clone)]
pub(crate) struct DiscordBotConfig {
    pub token: Option<ResolvedValue>,
    pub file_token: Option<yaml_serde::Value>,
}

impl std::fmt::Debug for DiscordBotConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DiscordBotConfig")
            .field("token", &debug_secret(self.token.as_ref()))
            .finish()
    }
}

#[derive(Clone, Default)]
pub(crate) struct ChannelConfig {
    pub enabled: Option<bool>,
    pub port: Option<u16>,
    pub host: Option<String>,
    pub auth_token: Option<ResolvedValue>,
    pub file_auth_token: Option<yaml_serde::Value>,
    pub allowed_origins: Option<Vec<String>>,
    pub default_surface: Option<String>,
    pub default_session: Option<String>,
    pub allowed_surfaces: Option<Vec<String>>,
    pub discord_bots: Option<HashMap<BotId, DiscordBotConfig>>,
    /// Shared Discord channel configs at the channel level (`channels.discord.channels`).
    /// Each bot determines channel membership by checking which agents in each
    /// channel's `agents` list have `discord_bot` set to the bot's ID.
    pub discord_channels: Option<HashMap<u64, DiscordChannelConfig>>,
    /// Telegram bot configs under `channels.telegram.telegram_bots`.
    pub telegram_bots: Option<HashMap<BotId, TelegramBotConfig>>,
    /// Telegram channel (group/supergroup) configs under `channels.telegram.telegram_channels`.
    pub telegram_channels: Option<HashMap<i64, TelegramChatConfig>>,
}

impl std::fmt::Debug for ChannelConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChannelConfig")
            .field("enabled", &self.enabled)
            .field("port", &self.port)
            .field("host", &self.host)
            .field("auth_token", &debug_secret(self.auth_token.as_ref()))
            .field("allowed_origins", &self.allowed_origins)
            .field("default_surface", &self.default_surface)
            .field("default_session", &self.default_session)
            .field("allowed_surfaces", &self.allowed_surfaces)
            .field("discord_bots", &self.discord_bots)
            .field("discord_channels", &self.discord_channels)
            .field("telegram_bots", &self.telegram_bots)
            .field("telegram_channels", &self.telegram_channels)
            .finish()
    }
}

/// Per-model metadata stored inside `ProviderConfig.models`.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub(crate) struct ModelConfig {
    /// Maximum context window in tokens for this model.
    /// When `None`, falls back to `Config.default_context_window_tokens`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window_tokens: Option<usize>,

    /// Optional inline model-specific instructions injected into the system
    /// prompt between SOUL and Core Instructions (wrapped in
    /// `<model-instructions>`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_instructions: Option<String>,

    /// Optional path to a file whose contents are used as model-specific
    /// instructions. Relative paths resolve against the config file directory.
    /// Mutually exclusive with [`ModelConfig::model_instructions`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_instructions_file: Option<String>,
}

#[derive(Clone)]
pub(crate) struct ProviderConfig {
    pub label: String,
    pub base_url: String,
    pub api_key: Option<ResolvedValue>,
    pub default_model: String,
    pub models: HashMap<String, ModelConfig>,
}

impl std::fmt::Debug for ProviderConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderConfig")
            .field("label", &self.label)
            .field("base_url", &self.base_url)
            .field("api_key", &debug_secret(self.api_key.as_ref()))
            .field("default_model", &self.default_model)
            .field("models", &self.models)
            .finish()
    }
}

#[derive(Clone)]
pub(crate) struct ResolvedLlmConfig {
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
            .field("api_key", &debug_secret(self.api_key.as_ref()))
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

impl ResolvedLlmConfig {
    /// Returns a deterministic hash of all config fields for use as a cache key.
    ///
    /// Includes `api_key` via `expose_secret()` so that different keys
    /// produce different hashes. The key value itself is never stored.
    pub(crate) fn cache_key(&self) -> u64 {
        use std::hash::{Hash, Hasher};

        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.provider.hash(&mut hasher);
        self.label.hash(&mut hasher);
        self.base_url.hash(&mut hasher);
        self.model.hash(&mut hasher);
        if let Some(key) = &self.api_key {
            secrecy::ExposeSecret::expose_secret(key).hash(&mut hasher);
        }
        hasher.finish()
    }
}

#[derive(Clone, Debug)]
pub(crate) struct SleepBatchConfig {
    pub provider: Option<ProviderId>,
    pub model: Option<String>,
    pub enabled: bool,
    pub schedule: Option<String>,
    pub agents: Option<Vec<AgentId>>,
    pub retry_max_attempts: u32,
    pub retry_interval_minutes: u32,
}

impl Default for SleepBatchConfig {
    fn default() -> Self {
        Self {
            provider: None,
            model: None,
            enabled: false,
            schedule: None,
            agents: None,
            retry_max_attempts: 3,
            retry_interval_minutes: 5,
        }
    }
}

impl SleepBatchConfig {
    pub(crate) fn scheduler_enabled(&self) -> bool {
        self.enabled
    }
}

#[derive(Clone, Debug)]
pub(crate) struct PulseConfig {
    pub enabled: bool,
    pub tick_interval_secs: u64,
}

impl Default for PulseConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            tick_interval_secs: 60,
        }
    }
}

impl PulseConfig {
    pub(crate) fn scheduler_enabled(&self) -> bool {
        self.enabled
    }
}

/// Backup schedule and retention settings for the SQLite DB.
///
/// `interval_days` expresses the cadence in days between periodic backups,
/// and `time` is an `HH:MM` string interpreted in `Config::timezone`. The
/// startup backup always runs (when `enabled`) and is independent of
/// `interval_days`. `max_generations` bounds the number of `egopulse-*.db`
/// snapshots retained on disk.
#[derive(Clone, Debug)]
pub(crate) struct BackupConfig {
    pub enabled: bool,
    pub interval_days: u32,
    pub time: String,
    pub max_generations: u32,
}

impl Default for BackupConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            interval_days: 7,
            time: "03:00".to_string(),
            max_generations: 12,
        }
    }
}

impl BackupConfig {
    pub(crate) fn scheduler_enabled(&self) -> bool {
        self.enabled
    }
}

/// Database-specific configuration section (`db:` in YAML).
#[derive(Clone, Debug, Default)]
pub(crate) struct DatabaseConfig {
    pub backup: BackupConfig,
}

/// Parse a human-friendly duration string into seconds.
///
/// Supported formats: `30s`, `5m`, `1h`, or combinations like `1h30m`.
///
/// # Errors
///
/// Returns a descriptive error string if the format is invalid or the value is zero.
pub(crate) fn parse_duration(input: &str) -> Result<u64, String> {
    let input = input.trim();
    if input.is_empty() {
        return Err("empty duration".to_string());
    }

    let mut total_secs: u64 = 0;
    let mut chars = input.chars().peekable();
    let mut parsed_something = false;

    while chars.peek().is_some() {
        let mut num_buf = String::new();
        while let Some(c) = chars.peek() {
            if c.is_ascii_digit() {
                num_buf.push(chars.next().unwrap());
            } else {
                break;
            }
        }
        let value: u64 = num_buf
            .parse()
            .map_err(|_| format!("invalid number in duration: {input}"))?;

        let unit = chars
            .next()
            .ok_or_else(|| format!("missing unit after {value} in duration: {input}"))?;

        match unit {
            's' => total_secs += value,
            'm' => total_secs += value * 60,
            'h' => total_secs += value * 3600,
            _ => return Err(format!("unknown unit '{unit}' in duration: {input}")),
        }
        parsed_something = true;
    }

    if !parsed_something || total_secs == 0 {
        return Err(format!("duration must be positive: {input}"));
    }

    Ok(total_secs)
}

/// Per-channel profile override within an agent definition.
/// Keyed by channel name (e.g. "voice") to override provider/model per channel.
#[derive(Clone, Debug, Default)]
pub(crate) struct AgentProfileConfig {
    pub provider: Option<String>,
    pub model: Option<String>,
}

#[derive(Clone, Default)]
pub(crate) struct AgentConfig {
    pub label: String,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub discord_bot: Option<BotId>,
    pub telegram_bot: Option<BotId>,
    pub profiles: HashMap<String, AgentProfileConfig>,
}

impl std::fmt::Debug for AgentConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentConfig")
            .field("label", &self.label)
            .field("provider", &self.provider)
            .field("model", &self.model)
            .field("discord_bot", &self.discord_bot)
            .field("telegram_bot", &self.telegram_bot)
            .field("profiles", &self.profiles)
            .finish()
    }
}

#[derive(Clone, Debug)]
pub(crate) struct WebhookTargetConfig {
    pub channel: ChannelName,
    pub thread: String,
    pub agent: Option<AgentId>,
}

#[derive(Clone)]
pub(crate) struct WebhookReceiverConfig {
    pub token: Option<ResolvedValue>,
    pub file_token: Option<yaml_serde::Value>,
    pub target: WebhookTargetConfig,
}

impl std::fmt::Debug for WebhookReceiverConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebhookReceiverConfig")
            .field("token", &debug_secret(self.token.as_ref()))
            .field("target", &self.target)
            .finish()
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct WebhooksConfig {
    pub receivers: HashMap<WebhookReceiverId, WebhookReceiverConfig>,
}

/// Top-level application configuration resolved from file and environment variables.
#[derive(Clone)]
pub struct Config {
    pub(crate) default_provider: ProviderId,
    pub(crate) default_model: Option<String>,
    pub(crate) providers: HashMap<ProviderId, ProviderConfig>,
    pub(crate) state_root: String,
    pub(crate) log_level: String,
    pub(crate) compaction_timeout_secs: u64,
    pub(crate) max_history_messages: usize,
    pub(crate) compact_keep_recent: usize,
    pub(crate) default_context_window_tokens: usize,
    pub(crate) compaction_threshold_ratio: f64,
    pub(crate) compaction_target_ratio: f64,
    pub(crate) channels: HashMap<ChannelName, ChannelConfig>,
    pub(crate) default_agent: AgentId,
    pub(crate) agents: HashMap<AgentId, AgentConfig>,
    pub timezone: String,
    pub(crate) sleep_batch: SleepBatchConfig,
    pub(crate) pulse: PulseConfig,
    pub(crate) db: DatabaseConfig,
    pub(crate) web_fetch: super::web_fetch::WebFetchConfig,
    pub(crate) webhooks: WebhooksConfig,
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
            .field("compact_keep_recent", &self.compact_keep_recent)
            .field(
                "default_context_window_tokens",
                &self.default_context_window_tokens,
            )
            .field(
                "compaction_threshold_ratio",
                &self.compaction_threshold_ratio,
            )
            .field("compaction_target_ratio", &self.compaction_target_ratio)
            .field("channels", &self.channels)
            .field("default_agent", &self.default_agent)
            .field("agents", &self.agents)
            .field("timezone", &self.timezone)
            .field("sleep_batch", &self.sleep_batch)
            .field("pulse", &self.pulse)
            .field("db", &self.db)
            .field("web_fetch", &self.web_fetch)
            .field("webhooks", &self.webhooks)
            .finish()
    }
}

impl Config {
    /// Returns the configured logging level.
    pub fn log_level(&self) -> &str {
        &self.log_level
    }

    /// Load configuration, requiring an API key for remote endpoints.
    pub fn load(config_path: Option<&Path>) -> Result<Self, ConfigError> {
        super::loader::build_config(config_path, false)
    }

    /// Load configuration, allowing a missing API key (used by setup/config editing).
    pub fn load_allow_missing_api_key(config_path: Option<&Path>) -> Result<Self, ConfigError> {
        super::loader::build_config(config_path, true)
    }
}

/// Formats an optional secret value for Debug output.
fn debug_secret<T>(opt: Option<&T>) -> &'static str {
    match opt {
        Some(_) => "<redacted>",
        None => "<none>",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_duration_seconds() {
        assert_eq!(parse_duration("30s").unwrap(), 30);
    }

    #[test]
    fn parse_duration_minutes() {
        assert_eq!(parse_duration("5m").unwrap(), 300);
    }

    #[test]
    fn parse_duration_hours() {
        assert_eq!(parse_duration("1h").unwrap(), 3600);
    }

    #[test]
    fn parse_duration_combination() {
        assert_eq!(parse_duration("1h30m").unwrap(), 5400);
    }

    #[test]
    fn parse_duration_rejects_empty() {
        assert!(parse_duration("").is_err());
    }

    #[test]
    fn parse_duration_rejects_zero() {
        assert!(parse_duration("0s").is_err());
    }

    #[test]
    fn parse_duration_rejects_unknown_unit() {
        assert!(parse_duration("5d").is_err());
    }

    #[test]
    fn parse_duration_rejects_bare_number() {
        assert!(parse_duration("60").is_err());
    }

    #[test]
    fn model_config_deserializes_inline_instructions() {
        let yaml = "context_window_tokens: 200000
model_instructions: |
  Be concise.
  Avoid preamble.
";

        let config: ModelConfig = yaml_serde::from_str(yaml).expect("deserialize ModelConfig");

        assert_eq!(config.context_window_tokens, Some(200000));
        assert_eq!(
            config.model_instructions.as_deref(),
            Some("Be concise.\nAvoid preamble.\n")
        );
    }
}
