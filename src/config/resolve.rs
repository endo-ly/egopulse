use std::env;
use std::path::{Path, PathBuf};

use super::{ChannelName, Config, ProviderConfig, ProviderId, ResolvedLlmConfig};
use crate::error::ConfigError;

impl Config {
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
            provider: self.default_provider.to_string(),
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
    pub fn effective_provider_name(&self, channel: &str) -> String {
        let channel_key = ChannelName::new(channel);
        self.channels
            .get(&channel_key)
            .and_then(|config| config.provider.as_deref())
            .map(|p| p.to_string())
            .unwrap_or_else(|| self.default_provider.to_string())
    }

    /// Resolves the provider/model pair used for a request from the given channel.
    pub fn resolve_llm_for_channel(&self, channel: &str) -> Result<ResolvedLlmConfig, ConfigError> {
        let channel_key = ChannelName::new(channel);
        let provider_name = self.effective_provider_name(channel_key.as_str());
        let provider_id = ProviderId::new(&provider_name);
        let provider = self.providers.get(&provider_id).ok_or_else(|| {
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
    pub fn web_host(&self) -> &str {
        self.channels
            .get("web")
            .and_then(|c| c.host.as_deref())
            .unwrap_or(default_web_host())
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
            .filter_map(|origin| super::loader::normalize_string(Some(origin)))
            .collect()
    }

    /// Returns `true` if the named channel is enabled.
    pub fn channel_enabled(&self, channel: &str) -> bool {
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

    /// Discord bot token (env override or config file).
    pub fn discord_bot_token(&self) -> Option<String> {
        env::var("EGOPULSE_DISCORD_BOT_TOKEN")
            .ok()
            .and_then(|v| super::loader::normalize_string(Some(v)))
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
            .and_then(|v| super::loader::normalize_string(Some(v)))
            .or_else(|| {
                self.channels
                    .get("telegram")
                    .and_then(|c| c.bot_token.clone())
            })
    }

    /// Telegram bot username for group mention detection.
    pub fn telegram_bot_username(&self) -> Option<&str> {
        self.channels
            .get("telegram")
            .and_then(|c| c.bot_username.as_deref())
    }

    /// 組み込みスキルディレクトリ: `state_root/skills`。
    pub fn skills_dir(&self) -> Result<PathBuf, ConfigError> {
        Ok(Path::new(&self.state_root).join("skills"))
    }

    /// ユーザースキルディレクトリ: `state_root/workspace/skills`。
    pub fn user_skills_dir(&self) -> Result<PathBuf, ConfigError> {
        Ok(self.workspace_dir()?.join("skills"))
    }

    /// エージェント作業ディレクトリ: `state_root/workspace`。
    pub fn workspace_dir(&self) -> Result<PathBuf, ConfigError> {
        Ok(Path::new(&self.state_root).join("workspace"))
    }

    /// ランタイムデータディレクトリ: `state_root/runtime`。
    pub fn runtime_dir(&self) -> PathBuf {
        Path::new(&self.state_root).join("runtime")
    }

    /// データベースファイルパス: `state_root/runtime/egopulse.db`。
    pub fn db_path(&self) -> PathBuf {
        self.runtime_dir().join("egopulse.db")
    }

    /// アセットディレクトリ: `state_root/runtime/assets`。
    pub fn assets_dir(&self) -> PathBuf {
        self.runtime_dir().join("assets")
    }

    /// アーカイブディレクトリ: `state_root/runtime/groups`。
    pub fn groups_dir(&self) -> PathBuf {
        self.runtime_dir().join("groups")
    }

    /// デフォルト SOUL.md パス: `state_root/SOUL.md`。
    pub fn soul_path(&self) -> PathBuf {
        Path::new(&self.state_root).join("SOUL.md")
    }

    /// デフォルト AGENTS.md パス: `state_root/AGENTS.md`。
    pub fn agents_path(&self) -> PathBuf {
        Path::new(&self.state_root).join("AGENTS.md")
    }

    /// マルチソウル用ディレクトリ: `state_root/souls`。
    pub fn souls_dir(&self) -> PathBuf {
        Path::new(&self.state_root).join("souls")
    }

    /// チャット別 AGENTS.md: `state_root/runtime/groups/{channel}/{thread}/AGENTS.md`。
    pub fn chat_agents_path(&self, channel: &str, thread: &str) -> PathBuf {
        self.groups_dir()
            .join(channel)
            .join(thread)
            .join("AGENTS.md")
    }

    /// チャット別 SOUL.md: `state_root/runtime/groups/{channel}/{thread}/SOUL.md`。
    pub fn chat_soul_path(&self, channel: &str, thread: &str) -> PathBuf {
        self.groups_dir().join(channel).join(thread).join("SOUL.md")
    }

    /// ステータスファイルパス: `state_root/runtime/status.json`。
    pub fn status_json_path(&self) -> PathBuf {
        self.runtime_dir().join("status.json")
    }

    /// Atomically writes the current config to a YAML file.
    ///
    /// Uses the global `CONFIG_WRITE_LOCK` for in-process mutual exclusion and an
    /// file-level lock (`fs2`) for cross-process safety. The write is atomic via
    /// temp-file + rename.
    pub fn save_yaml(&self, path: &Path) -> Result<(), crate::error::EgoPulseError> {
        super::persist::save_yaml(self, path)
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

/// Default workspace directory: `~/.egopulse/workspace`.
pub fn default_workspace_dir() -> Result<PathBuf, ConfigError> {
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

pub(super) fn default_max_session_messages() -> usize {
    40
}

pub(super) fn default_compact_keep_recent() -> usize {
    20
}
