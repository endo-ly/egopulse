use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::{
    AgentId, BotId, ChannelName, Config, DiscordChannelConfig, ProviderConfig, ProviderId,
    PulseConfig, ResolvedLlmConfig,
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

    /// Resolves the provider/model pair for a specific agent and channel.
    ///
    /// Resolution chain (highest priority first):
    /// 1. `agent.profiles[channel].provider` / `agent.profiles[channel].model`
    /// 2. `agent.provider` / `agent.model`
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

        let profile = agent.profiles.get(channel);

        let provider_name = profile
            .and_then(|p| p.provider.as_deref())
            .map(|p| p.trim().to_ascii_lowercase())
            .or_else(|| {
                agent
                    .provider
                    .as_deref()
                    .map(|p| p.trim().to_ascii_lowercase())
            })
            .unwrap_or_else(|| self.default_provider.to_string());

        let provider_id = ProviderId::new(&provider_name);
        let provider = self.providers.get(&provider_id).ok_or_else(|| {
            ConfigError::InvalidProviderReference {
                provider: provider_name.clone(),
            }
        })?;

        let model = profile
            .and_then(|p| p.model.as_deref().map(String::from))
            .or_else(|| agent.model.as_deref().map(String::from))
            .or_else(|| self.default_model.clone())
            .unwrap_or_else(|| provider.default_model.clone());

        Ok(ResolvedLlmConfig {
            provider: provider_name,
            label: provider.label.clone(),
            base_url: provider.base_url.clone(),
            api_key: provider.api_key.as_ref().map(|rv| rv.to_secret_string()),
            model,
        })
    }

    /// Resolves the provider/model pair for sleep batch LLM processing.
    ///
    /// Resolution chain:
    /// 1. `sleep_batch.provider` → that provider, else `default_provider`
    /// 2. `sleep_batch.model` → that model, else `default_model`,
    ///    else the resolved provider's `default_model`
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::InvalidProviderReference`] if `sleep_batch.provider`
    /// does not reference an existing provider.
    pub(crate) fn resolve_sleep_batch_llm(&self) -> Result<ResolvedLlmConfig, ConfigError> {
        let provider_id = self
            .sleep_batch
            .provider
            .as_ref()
            .unwrap_or(&self.default_provider);

        let provider = self.providers.get(provider_id).ok_or_else(|| {
            ConfigError::InvalidProviderReference {
                provider: provider_id.to_string(),
            }
        })?;

        let model = self
            .sleep_batch
            .model
            .as_deref()
            .map(String::from)
            .or_else(|| self.default_model.clone())
            .unwrap_or_else(|| provider.default_model.clone());

        Ok(ResolvedLlmConfig {
            provider: provider_id.to_string(),
            label: provider.label.clone(),
            base_url: provider.base_url.clone(),
            api_key: provider.api_key.as_ref().map(|rv| rv.to_secret_string()),
            model,
        })
    }

    /// Returns `true` if the web channel is enabled.
    pub(crate) fn web_enabled(&self) -> bool {
        self.channels
            .get("web")
            .and_then(|c| c.enabled)
            .unwrap_or(false)
    }

    pub(crate) fn pulse(&self) -> &PulseConfig {
        &self.pulse
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

    /// Returns whether the `voice` channel is enabled.
    pub(crate) fn voice_enabled(&self) -> bool {
        self.channel_enabled("voice")
    }

    /// Returns the configured, trimmed Voice API token when non-empty.
    pub(crate) fn voice_auth_token(&self) -> Option<&str> {
        self.channels
            .get("voice")
            .and_then(|c| c.auth_token.as_ref().map(|rv| rv.value()))
            .map(str::trim)
            .filter(|token| !token.is_empty())
    }

    /// Returns the configured default voice surface, or `voice`.
    pub(crate) fn voice_default_surface(&self) -> &str {
        self.channels
            .get("voice")
            .and_then(|c| c.default_surface.as_deref())
            .unwrap_or("voice")
    }

    /// Returns the configured default voice session, or `main`.
    pub(crate) fn voice_default_session(&self) -> &str {
        self.channels
            .get("voice")
            .and_then(|c| c.default_session.as_deref())
            .unwrap_or("main")
    }

    /// Returns the allowed voice surfaces, or an empty unrestricted slice.
    pub(crate) fn voice_allowed_surfaces(&self) -> &[String] {
        self.channels
            .get("voice")
            .and_then(|c| c.allowed_surfaces.as_deref())
            .unwrap_or(&[])
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

    /// Resolves the context window for a given provider+model pair.
    ///
    /// Falls back to `default_context_window_tokens` when the model entry
    /// has no explicit `context_window_tokens`.
    pub(crate) fn resolve_context_window_tokens(
        &self,
        provider_id: &ProviderId,
        model: &str,
    ) -> usize {
        self.providers
            .get(provider_id)
            .and_then(|provider| provider.models.get(model))
            .and_then(|model_config| model_config.context_window_tokens)
            .unwrap_or(self.default_context_window_tokens)
    }

    /// Resolves the model-specific instructions content for a provider+model pair.
    ///
    /// Returns the trimmed inline `model_instructions` content when set.
    /// Surrounding whitespace is trimmed; empty/whitespace-only content yields `None`.
    pub(crate) fn resolve_model_instructions(
        &self,
        provider_id: &ProviderId,
        model: &str,
        base_dir: &std::path::Path,
    ) -> Result<Option<String>, ConfigError> {
        let _ = base_dir;
        let Some(provider) = self.providers.get(provider_id) else {
            return Ok(None);
        };
        let Some(model_config) = provider.models.get(model) else {
            return Ok(None);
        };
        if let Some(inline) = &model_config.model_instructions {
            let trimmed = inline.trim();
            return Ok(if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            });
        }
        Ok(None)
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
                Some(DiscordBotRuntime {
                    bot_id,
                    token: token.value(),
                })
            })
            .collect();
        runtime_bots.sort_by_key(|b| b.bot_id.as_str());
        runtime_bots
    }
    /// Returns the shared Discord channel config, or an empty map when none is configured.
    /// Channel membership for each bot is determined by agent bindings
    /// (`config.agents[].discord_bot == bot_id`), not by explicit per-bot listing.
    pub(crate) fn discord_channels(&self) -> HashMap<u64, DiscordChannelConfig> {
        self.channels
            .get("discord")
            .and_then(|ch| ch.discord_channels.as_ref())
            .cloned()
            .unwrap_or_default()
    }

    /// Returns runtime info for every bot under `channels.telegram.telegram_bots` that has
    /// a resolved token.
    ///
    /// Sorted by `bot_id` for deterministic startup. Empty when Telegram is disabled.
    pub(crate) fn telegram_bots(&self) -> Vec<TelegramBotRuntime<'_>> {
        if !self.channel_enabled("telegram") {
            return vec![];
        }

        let Some(telegram) = self.channels.get("telegram") else {
            return vec![];
        };
        let Some(bots) = &telegram.telegram_bots else {
            return vec![];
        };

        let mut runtime_bots: Vec<_> = bots
            .iter()
            .filter_map(|(bot_id, bot)| {
                let token = bot.token.as_ref()?;
                Some(TelegramBotRuntime {
                    bot_id,
                    token: token.value(),
                })
            })
            .collect();
        runtime_bots.sort_by_key(|b| b.bot_id.as_str());
        runtime_bots
    }

    /// Returns the Telegram channel (group/supergroup) configs, or an empty map
    /// when none is configured.
    pub(crate) fn telegram_channels(&self) -> HashMap<i64, super::TelegramChatConfig> {
        self.channels
            .get("telegram")
            .and_then(|ch| ch.telegram_channels.as_ref())
            .cloned()
            .unwrap_or_default()
    }
}

/// Runtime data needed to start and operate one Discord bot.
#[derive(Clone, Debug)]
pub(crate) struct DiscordBotRuntime<'a> {
    pub bot_id: &'a BotId,
    pub token: &'a str,
}

/// Runtime data needed to start and operate one Telegram bot.
#[derive(Clone, Debug)]
pub(crate) struct TelegramBotRuntime<'a> {
    pub bot_id: &'a BotId,
    pub token: &'a str,
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
