use std::collections::HashMap;
use std::env;
use std::path::{Path, PathBuf};

use super::{
    AgentId, BotId, ChannelName, Config, DiscordChannelConfig, ProviderConfig, ProviderId,
    ResolvedLlmConfig,
};
use crate::error::ConfigError;

impl Config {
    /// Invariant: `build_config` validates that `default_provider` exists in `providers`.
    /// This accessor relies on that validated config construction path.
    pub(crate) fn global_provider(&self) -> &ProviderConfig {
        self.providers
            .get(&self.default_provider)
            .expect("default_provider must reference an existing provider")
    }

    /// Resolves the global default provider/model pair used by CLI/TUI.
    pub(crate) fn resolve_global_llm(&self) -> ResolvedLlmConfig {
        let provider = self.global_provider();
        ResolvedLlmConfig {
            provider: self.default_provider.to_string(),
            label: provider.label.clone(),
            base_url: provider.base_url.clone(),
            api_key: provider.api_key.as_ref().map(|rv| rv.to_secret_string()),
            model: self
                .default_model
                .clone()
                .unwrap_or_else(|| provider.default_model.clone()),
        }
    }

    /// Returns the normalized provider key used for the given channel.
    pub(crate) fn effective_provider_name(&self, channel: &str) -> String {
        let channel_key = ChannelName::new(channel);
        self.channels
            .get(&channel_key)
            .and_then(|config| config.provider.as_deref())
            .map(|p| p.to_string())
            .unwrap_or_else(|| self.default_provider.to_string())
    }

    /// Resolves the provider/model pair for a specific agent on a given channel.
    ///
    /// Resolution chain (highest priority first):
    /// 1. `agent.provider` / `agent.model`
    /// 2. `channel.provider` / `channel.model`
    /// 3. `config.default_provider` / `config.default_model`
    /// 4. `provider.default_model`
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::AgentNotFound`] if `agent_id` is not in `self.agents`.
    /// Returns [`ConfigError::InvalidProviderReference`] if the resolved provider name
    /// does not reference an existing provider.
    pub(crate) fn resolve_llm_for_agent_channel(
        &self,
        agent_id: &AgentId,
        channel: &str,
    ) -> Result<ResolvedLlmConfig, ConfigError> {
        let agent = self
            .agents
            .get(agent_id)
            .ok_or_else(|| ConfigError::AgentNotFound {
                agent_id: agent_id.to_string(),
            })?;

        let channel_key = ChannelName::new(channel);

        let provider_name = agent
            .provider
            .as_deref()
            .map(|p| p.trim().to_ascii_lowercase())
            .unwrap_or_else(|| self.effective_provider_name(channel_key.as_str()));

        let provider_id = ProviderId::new(&provider_name);
        let provider = self.providers.get(&provider_id).ok_or_else(|| {
            ConfigError::InvalidProviderReference {
                provider: provider_name.clone(),
            }
        })?;

        let model = agent
            .model
            .as_deref()
            .map(String::from)
            .or_else(|| {
                self.channels
                    .get(&channel_key)
                    .and_then(|config| config.model.clone())
            })
            .unwrap_or_else(|| {
                self.default_model
                    .clone()
                    .unwrap_or_else(|| provider.default_model.clone())
            });

        Ok(ResolvedLlmConfig {
            provider: provider_name,
            label: provider.label.clone(),
            base_url: provider.base_url.clone(),
            api_key: provider.api_key.as_ref().map(|rv| rv.to_secret_string()),
            model,
        })
    }

    /// Resolves the provider/model pair used for a request from the given channel
    /// using the default agent.
    ///
    /// # Errors
    ///
    /// See [`resolve_llm_for_agent_channel`].
    pub(crate) fn resolve_llm_for_channel(&self, channel: &str) -> Result<ResolvedLlmConfig, ConfigError> {
        self.resolve_llm_for_agent_channel(&self.default_agent, channel)
    }

    /// Returns the web channel's resolved LLM settings.
    pub(crate) fn web_llm(&self) -> Result<ResolvedLlmConfig, ConfigError> {
        self.resolve_llm_for_channel("web")
    }

    /// Returns `true` if the web channel is enabled.
    pub(crate) fn web_enabled(&self) -> bool {
        self.channels
            .get("web")
            .and_then(|c| c.enabled)
            .unwrap_or(false)
    }

    /// Returns the web channel host, defaulting to `127.0.0.1`.
    pub(crate) fn web_host(&self) -> &str {
        self.channels
            .get("web")
            .and_then(|c| c.host.as_deref())
            .unwrap_or(default_web_host())
    }

    /// Returns the web channel port, defaulting to `10961`.
    pub(crate) fn web_port(&self) -> u16 {
        self.channels
            .get("web")
            .and_then(|c| c.port)
            .unwrap_or_else(default_web_port)
    }

    /// Returns the web auth token if configured and non-empty.
    pub(crate) fn web_auth_token(&self) -> Option<&str> {
        self.channels
            .get("web")
            .and_then(|c| c.auth_token.as_ref().map(|rv| rv.value()))
            .map(str::trim)
            .filter(|token| !token.is_empty())
    }

    /// Returns the list of allowed WebSocket origins for the web channel.
    pub(crate) fn web_allowed_origins(&self) -> Vec<String> {
        self.channels
            .get("web")
            .and_then(|c| c.allowed_origins.clone())
            .unwrap_or_default()
            .into_iter()
            .filter_map(|origin| super::loader::normalize_string(Some(origin)))
            .collect()
    }

    /// Returns `true` if the named channel is enabled.
    pub(crate) fn channel_enabled(&self, channel: &str) -> bool {
        let needle = ChannelName::new(channel);
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

    /// Telegram bot token (env override or config file).
    pub(crate) fn telegram_bot_token(&self) -> Option<String> {
        env::var("TELEGRAM_BOT_TOKEN")
            .ok()
            .and_then(|v| super::loader::normalize_string(Some(v)))
            .or_else(|| {
                self.channels
                    .get("telegram")
                    .and_then(|c| c.bot_token.as_ref().map(|rv| rv.value().to_string()))
            })
    }

    /// Telegram bot username for group mention detection.
    pub(crate) fn telegram_bot_username(&self) -> Option<&str> {
        self.channels
            .get("telegram")
            .and_then(|c| c.bot_username.as_deref())
    }

    /// 組み込みスキルディレクトリ: `state_root/skills`。
    pub(crate) fn skills_dir(&self) -> Result<PathBuf, ConfigError> {
        Ok(Path::new(&self.state_root).join("skills"))
    }

    /// ユーザースキルディレクトリ: `state_root/workspace/skills`。
    pub(crate) fn user_skills_dir(&self) -> Result<PathBuf, ConfigError> {
        Ok(self.workspace_dir()?.join("skills"))
    }

    /// エージェント作業ディレクトリ: `state_root/workspace`。
    pub(crate) fn workspace_dir(&self) -> Result<PathBuf, ConfigError> {
        Ok(Path::new(&self.state_root).join("workspace"))
    }

    /// ランタイムデータディレクトリ: `state_root/runtime`。
    pub(crate) fn runtime_dir(&self) -> PathBuf {
        Path::new(&self.state_root).join("runtime")
    }

    /// データベースファイルパス: `state_root/runtime/egopulse.db`。
    pub(crate) fn db_path(&self) -> PathBuf {
        self.runtime_dir().join("egopulse.db")
    }

    /// アセットディレクトリ: `state_root/runtime/assets`。
    pub(crate) fn assets_dir(&self) -> PathBuf {
        self.runtime_dir().join("assets")
    }

    /// アーカイブディレクトリ: `state_root/runtime/groups`。
    pub(crate) fn groups_dir(&self) -> PathBuf {
        self.runtime_dir().join("groups")
    }

    /// デフォルト SOUL.md パス: `state_root/SOUL.md`。
    pub(crate) fn soul_path(&self) -> PathBuf {
        Path::new(&self.state_root).join("SOUL.md")
    }

    /// デフォルト AGENTS.md パス: `state_root/AGENTS.md`。
    pub(crate) fn agents_path(&self) -> PathBuf {
        Path::new(&self.state_root).join("AGENTS.md")
    }

    /// Agent-specific SOUL.md: `state_root/agents/{agent_id}/SOUL.md`.
    pub(crate) fn agent_soul_path(&self, agent_id: &AgentId) -> PathBuf {
        Path::new(&self.state_root)
            .join("agents")
            .join(agent_id.as_str())
            .join("SOUL.md")
    }

    /// Agent-specific AGENTS.md: `state_root/agents/{agent_id}/AGENTS.md`.
    pub(crate) fn agent_agents_path(&self, agent_id: &AgentId) -> PathBuf {
        Path::new(&self.state_root)
            .join("agents")
            .join(agent_id.as_str())
            .join("AGENTS.md")
    }

    /// Discord session thread: `{channel_id}:bot:{bot_id}:agent:{agent_id}`.
    pub(crate) fn discord_surface_thread(
        &self,
        channel_id: &str,
        bot_id: &BotId,
        agent_id: &AgentId,
    ) -> String {
        format!("{channel_id}:bot:{bot_id}:agent:{agent_id}")
    }

    /// マルチソウル用ディレクトリ: `state_root/souls`。
    pub(crate) fn souls_dir(&self) -> PathBuf {
        Path::new(&self.state_root).join("souls")
    }

    /// チャット別 AGENTS.md: `state_root/runtime/groups/{channel}/{thread}/AGENTS.md`。
    pub(crate) fn chat_agents_path(&self, channel: &str, thread: &str) -> PathBuf {
        self.groups_dir()
            .join(channel)
            .join(thread)
            .join("AGENTS.md")
    }

    /// チャット別 SOUL.md: `state_root/runtime/groups/{channel}/{thread}/SOUL.md`。
    pub(crate) fn chat_soul_path(&self, channel: &str, thread: &str) -> PathBuf {
        self.groups_dir().join(channel).join(thread).join("SOUL.md")
    }

    /// ステータスファイルパス: `state_root/runtime/status.json`。
    pub(crate) fn status_json_path(&self) -> PathBuf {
        self.runtime_dir().join("status.json")
    }

    /// Resolves the context window for a given provider+model pair.
    ///
    /// Falls back to `default_context_window_tokens` when the model entry
    /// has no explicit `context_window_tokens`.
    pub(crate) fn resolve_context_window_tokens(&self, provider_id: &ProviderId, model: &str) -> usize {
        self.providers
            .get(provider_id)
            .and_then(|provider| provider.models.get(model))
            .and_then(|model_config| model_config.context_window_tokens)
            .unwrap_or(self.default_context_window_tokens)
    }

    /// Atomically writes the current config to a YAML file.
    ///
    /// Uses the global `CONFIG_WRITE_LOCK` for in-process mutual exclusion and an
    /// file-level lock (`fs2`) for cross-process safety. The write is atomic via
    /// temp-file + rename.
    pub(crate) fn save_yaml(&self, path: &Path) -> Result<(), crate::error::EgoPulseError> {
        super::persist::save_yaml(self, path)
    }

    /// Saves config with SecretRef-aware YAML and .env file.
    pub(crate) fn save_config_with_secrets(
        &self,
        yaml_path: &Path,
    ) -> Result<(), crate::error::EgoPulseError> {
        super::persist::save_config_with_secrets(self, yaml_path)
    }

    /// Returns runtime info for every bot under `channels.discord.bots` that has
    /// a resolved token.
    ///
    /// Sorted by `bot_id` for deterministic startup. Empty when Discord is disabled.
    pub(crate) fn discord_bots(&self) -> Vec<DiscordBotRuntime<'_>> {
        if !self.channel_enabled("discord") {
            return vec![];
        }

        let Some(discord) = self.channels.get("discord") else {
            return vec![];
        };
        let Some(bots) = &discord.discord_bots else {
            return vec![];
        };

        let mut runtime_bots: Vec<_> = bots
            .iter()
            .filter_map(|(bot_id, bot)| {
                let token = bot.token.as_ref()?;
                let channels: HashMap<u64, DiscordChannelConfig> =
                    bot.channels.clone().unwrap_or_default();
                Some(DiscordBotRuntime {
                    bot_id,
                    token: token.value(),
                    default_agent: &bot.default_agent,
                    channels,
                })
            })
            .collect();
        runtime_bots.sort_by_key(|b| b.bot_id.as_str());
        runtime_bots
    }
}

/// Runtime data needed to start and operate one Discord bot.
#[derive(Clone, Debug)]
pub(crate) struct DiscordBotRuntime<'a> {
    pub bot_id: &'a BotId,
    pub token: &'a str,
    pub default_agent: &'a AgentId,
    /// Per-channel configuration. Empty map means no guild messages are allowed (DM-only).
    pub channels: HashMap<u64, DiscordChannelConfig>,
}

/// Default config file path: `~/.egopulse/egopulse.config.yaml`.
pub fn default_config_path() -> Result<PathBuf, ConfigError> {
    default_state_root().map(|root| root.join("egopulse.config.yaml"))
}

/// Default state root directory: `~/.egopulse`.
pub(crate) fn default_state_root() -> Result<PathBuf, ConfigError> {
    dirs::home_dir()
        .map(|home| home.join(".egopulse"))
        .ok_or(ConfigError::HomeDirectoryUnresolved)
}

/// Default workspace directory: `~/.egopulse/workspace`.
pub(crate) fn default_workspace_dir() -> Result<PathBuf, ConfigError> {
    default_state_root().map(|root| root.join("workspace"))
}

pub(super) fn default_web_host() -> &'static str {
    "127.0.0.1"
}

pub(super) fn default_web_port() -> u16 {
    10961
}

pub(super) fn default_compaction_timeout_secs() -> u64 {
    180
}

pub(super) fn default_max_history_messages() -> usize {
    50
}

pub(super) fn default_context_window_tokens() -> usize {
    32768
}

pub(super) fn default_compaction_threshold_ratio() -> f64 {
    0.80
}

pub(super) fn default_compaction_target_ratio() -> f64 {
    0.40
}

pub(super) fn default_compact_keep_recent() -> usize {
    20
}
