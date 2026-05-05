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

/// Per-channel Discord configuration stored inside `DiscordBotConfig.channels`.
///
/// When `agent` is `None`, the bot's `default_agent` is used.
#[derive(Clone, Debug, Default)]
pub(crate) struct DiscordChannelConfig {
    /// Whether this channel requires an explicit @mention to trigger the bot.
    pub require_mention: bool,
    /// Agent override for this channel. `None` = use `default_agent`.
    pub agent: Option<AgentId>,
}

/// Per-chat Telegram configuration stored inside `ChannelConfig.chats`.
#[derive(Clone, Debug, Default)]
pub(crate) struct TelegramChatConfig {
    /// Whether this chat requires an explicit @mention to trigger the bot.
    pub require_mention: bool,
}

/// Per-bot Discord configuration stored under `channels.discord.bots.<bot_id>`.
///
/// Each bot connects to Discord with its own token and routes messages to agents
/// based on per-channel config or falls back to `default_agent`.
#[derive(Clone)]
pub(crate) struct DiscordBotConfig {
    pub token: Option<ResolvedValue>,
    pub file_token: Option<serde_yml::Value>,
    pub default_agent: AgentId,
    /// Per-channel configuration keyed by Discord channel ID.
    /// `None` means no channels are explicitly configured.
    pub channels: Option<HashMap<u64, DiscordChannelConfig>>,
}

impl std::fmt::Debug for DiscordBotConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DiscordBotConfig")
            .field("token", &debug_secret(self.token.as_ref()))
            .field("default_agent", &self.default_agent)
            .field("channels", &self.channels)
            .finish()
    }
}

#[derive(Clone, Default)]
pub(crate) struct ChannelConfig {
    pub enabled: Option<bool>,
    pub port: Option<u16>,
    pub host: Option<String>,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub auth_token: Option<ResolvedValue>,
    pub file_auth_token: Option<serde_yml::Value>,
    pub allowed_origins: Option<Vec<String>>,
    pub bot_token: Option<ResolvedValue>,
    pub file_bot_token: Option<serde_yml::Value>,
    pub bot_username: Option<String>,
    pub soul_path: Option<String>,
    /// Per-chat Telegram configuration keyed by chat ID.
    pub chats: Option<HashMap<i64, TelegramChatConfig>>,
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
            .field("auth_token", &debug_secret(self.auth_token.as_ref()))
            .field("allowed_origins", &self.allowed_origins)
            .field("bot_token", &debug_secret(self.bot_token.as_ref()))
            .field("bot_username", &self.bot_username)
            .field("chats", &self.chats)
            .field("soul_path", &self.soul_path)
            .field("discord_bots", &self.discord_bots)
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

#[derive(Clone, Default)]
pub(crate) struct AgentConfig {
    pub label: String,
    pub provider: Option<String>,
    pub model: Option<String>,
}

impl std::fmt::Debug for AgentConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentConfig")
            .field("label", &self.label)
            .field("provider", &self.provider)
            .field("model", &self.model)
            .finish()
    }
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
