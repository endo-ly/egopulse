//! EgoPulse ランタイム全体の依存を組み立てるモジュール。
//!
//! `AppState` の構築、単発 LLM 実行、各チャネルの起動と監視を提供する。

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use tokio::task::{JoinError, JoinHandle};
use tracing::info;

use crate::assets::AssetStore;
use crate::channel_adapter::ChannelRegistry;
use crate::channels;
use crate::config::Config;
use crate::error::{ChannelError, EgoPulseError};
use crate::llm::{Message, create_provider};
use crate::skills::SkillManager;
use crate::soul_agents::SoulAgentsLoader;
use crate::status::{
    self, ChannelEntry, ChannelsStatus, ProviderStatus, StatusSnapshot, WebChannelStatus,
};
use crate::storage::Database;
use crate::tools::ToolRegistry;
use crate::web::WebAdapter;

/// Holds the shared runtime dependencies used across all channels.
pub struct AppState {
    pub db: Arc<Database>,
    pub config: Config,
    pub config_path: Option<PathBuf>,
    pub llm_override: Option<Arc<dyn crate::llm::LlmProvider>>,
    pub channels: Arc<ChannelRegistry>,
    pub skills: Arc<SkillManager>,
    pub tools: Arc<ToolRegistry>,
    pub mcp_manager: Option<Arc<tokio::sync::RwLock<crate::mcp::McpManager>>>,
    pub assets: Arc<AssetStore>,
    pub soul_agents: Arc<SoulAgentsLoader>,
}

impl Clone for AppState {
    fn clone(&self) -> Self {
        Self {
            db: Arc::clone(&self.db),
            config: self.config.clone(),
            config_path: self.config_path.clone(),
            llm_override: self.llm_override.clone(),
            channels: Arc::clone(&self.channels),
            skills: Arc::clone(&self.skills),
            tools: Arc::clone(&self.tools),
            mcp_manager: self.mcp_manager.clone(),
            assets: Arc::clone(&self.assets),
            soul_agents: Arc::clone(&self.soul_agents),
        }
    }
}

impl AppState {
    /// 現在の設定スナップショットを返す。
    pub fn current_config(&self) -> Arc<Config> {
        Arc::new(self.config.clone())
    }

    /// 設定ファイルパスがある場合はディスクから再読込した最新設定を返す。
    pub fn try_current_config(&self) -> Result<Arc<Config>, EgoPulseError> {
        match self.config_path.as_deref() {
            Some(path) => Ok(Arc::new(Config::load_allow_missing_api_key(Some(path))?)),
            None => Ok(self.current_config()),
        }
    }

    /// Returns the LLM provider resolved for the given channel.
    pub fn llm_for_channel(
        &self,
        channel: &str,
    ) -> Result<Arc<dyn crate::llm::LlmProvider>, EgoPulseError> {
        if let Some(provider) = self.llm_override.clone() {
            return Ok(provider);
        }

        let config = self.try_current_config()?;
        Ok(Arc::from(create_provider(
            &config.resolve_llm_for_channel(channel)?,
        )?))
    }

    /// Returns the global default LLM provider for CLI/TUI surfaces.
    pub fn global_llm(&self) -> Result<Arc<dyn crate::llm::LlmProvider>, EgoPulseError> {
        if let Some(provider) = self.llm_override.clone() {
            return Ok(provider);
        }

        let config = self.try_current_config()?;
        Ok(Arc::from(create_provider(&config.resolve_global_llm())?))
    }
}

/// Builds the application state without recording a config file path.
pub async fn build_app_state(config: Config) -> Result<AppState, EgoPulseError> {
    build_app_state_with_path(config, None).await
}

/// Builds the application state and keeps the config path for later saves.
pub async fn build_app_state_with_path(
    config: Config,
    config_path: Option<PathBuf>,
) -> Result<AppState, EgoPulseError> {
    let db = Arc::new(Database::new(&config.db_path())?);
    let assets = Arc::new(AssetStore::new(&config.assets_dir())?);
    let skills = Arc::new(SkillManager::from_dirs(
        config.user_skills_dir()?,
        config.skills_dir()?,
    ));

    let mut channels = ChannelRegistry::new();
    channels.register(Arc::new(WebAdapter));

    #[cfg(feature = "channel-discord")]
    if let Some(token) = config.discord_bot_token() {
        channels.register(Arc::new(crate::channels::discord::DiscordAdapter::new(
            token,
        )));
    }

    #[cfg(feature = "channel-telegram")]
    if let Some(token) = config.telegram_bot_token() {
        let bot = teloxide::Bot::new(&token);
        channels.register(Arc::new(crate::channels::telegram::TelegramAdapter::new(
            bot,
        )));
    }

    let channels = Arc::new(channels);
    let mut tools = ToolRegistry::new(&config, Arc::clone(&skills));

    let workspace_dir = config.workspace_dir()?;
    let mcp_manager = crate::mcp::McpManager::new(&workspace_dir).await?;
    let mcp_arc = Arc::new(tokio::sync::RwLock::new(mcp_manager));

    // Register MCP tools as adapters
    let adapters = crate::mcp::McpManager::create_tool_adapters(&mcp_arc).await;
    for adapter in adapters {
        tools.register_tool(adapter);
    }

    let tools = Arc::new(tools);

    let soul_agents = Arc::new(SoulAgentsLoader::new(&config));
    if let Err(error) = soul_agents.provision_default_soul() {
        tracing::warn!("failed to provision default SOUL.md: {error}");
    }

    Ok(AppState {
        db,
        config,
        config_path,
        llm_override: None,
        channels,
        skills,
        tools,
        mcp_manager: Some(mcp_arc),
        assets,
        soul_agents,
    })
}

/// Sends a single prompt to the configured LLM without session state.
pub async fn ask(config: Config, prompt: &str) -> Result<String, EgoPulseError> {
    let llm = create_provider(&config.resolve_global_llm())?;
    let messages = vec![Message::text("user", prompt)];

    tokio::select! {
        response = llm.send_message("", messages, None) => Ok(response?.content),
        _ = tokio::signal::ctrl_c() => Err(EgoPulseError::ShutdownRequested),
    }
}

/// Starts the local TUI channel with a fully built application state.
pub async fn run_tui(config: Config, config_path: Option<PathBuf>) -> Result<(), EgoPulseError> {
    let state = build_app_state_with_path(config, config_path).await?;
    channels::tui::run(state).await
}

/// 全有効チャネルを一括起動
///
/// `egopulse run` から呼び出される。
/// 設定ベースでチャネルを構築 → spawn → ctrl_c 待機。
///
/// spawn したタスクの JoinHandle を監視し、即時終了 (起動失敗) を検知する。
/// Starts all enabled channels and supervises them until shutdown or failure.
pub async fn start_channels(state: AppState) -> Result<(), EgoPulseError> {
    write_startup_status(&state).await;

    let mut has_active_channels = false;
    let mut handles: Vec<(&'static str, JoinHandle<Result<(), EgoPulseError>>)> = Vec::new();

    // Web サーバー起動
    if state.config.web_enabled() {
        has_active_channels = true;
        let web_state = state.clone();
        let host = state.config.web_host().to_owned();
        let port = state.config.web_port();
        info!("Starting Web UI server on {host}:{port}");
        let handle =
            tokio::spawn(async move { crate::web::run_server(web_state, &host, port).await });
        handles.push(("web", handle));
    }

    // Discord bot 起動
    #[cfg(feature = "channel-discord")]
    if state.config.channel_enabled("discord") {
        if let Some(token) = state.config.discord_bot_token() {
            has_active_channels = true;
            let discord_state = Arc::new(state.clone());
            info!("Starting Discord bot...");
            let handle = tokio::spawn(async move {
                crate::channels::discord::start_discord_bot(discord_state, token)
                    .await
                    .map_err(|error| {
                        EgoPulseError::Channel(ChannelError::SendFailed(format!(
                            "discord bot failed: {error}"
                        )))
                    })
            });
            handles.push(("discord", handle));
        } else {
            tracing::warn!(
                "Discord channel is enabled but no bot_token is configured. \
                 Set channels.discord.bot_token in egopulse.config.yaml \
                 or set EGOPULSE_DISCORD_BOT_TOKEN environment variable."
            );
        }
    }

    // Telegram bot 起動
    #[cfg(feature = "channel-telegram")]
    if state.config.channel_enabled("telegram") {
        if let Some(token) = state.config.telegram_bot_token() {
            has_active_channels = true;
            let telegram_state = Arc::new(state.clone());
            let bot_username = state.config.telegram_bot_username().unwrap_or_default();
            info!("Starting Telegram bot as @{bot_username}...");
            let handle = tokio::spawn(async move {
                crate::channels::telegram::start_telegram_bot(telegram_state, token)
                    .await
                    .map_err(|error| {
                        EgoPulseError::Channel(ChannelError::SendFailed(format!(
                            "telegram bot failed: {error}"
                        )))
                    })
            });
            handles.push(("telegram", handle));
        } else {
            tracing::warn!(
                "Telegram channel is enabled but no bot_token is configured. \
                 Set channels.telegram.bot_token in egopulse.config.yaml \
                 or set EGOPULSE_TELEGRAM_BOT_TOKEN environment variable."
            );
        }
    }

    if !has_active_channels {
        return Err(EgoPulseError::Config(
            crate::error::ConfigError::NoActiveChannels,
        ));
    }

    info!("Runtime active; waiting for Ctrl-C or channel failure");

    // spawn したタスクの即時終了 (起動失敗) を検知
    loop {
        if let Some(finished_index) = handles.iter().position(|(_, handle)| handle.is_finished()) {
            let (name, handle) = handles.swap_remove(finished_index);
            let result = handle.await;
            shutdown_channel_tasks(handles).await;
            return match result {
                Ok(Ok(())) => Err(EgoPulseError::Channel(ChannelError::SendFailed(format!(
                    "channel '{name}' exited unexpectedly"
                )))),
                Ok(Err(error)) => Err(error),
                Err(error) => Err(channel_join_error(name, error)),
            };
        }

        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                shutdown_channel_tasks(handles).await;
                return Ok(());
            },
            _ = tokio::time::sleep(Duration::from_secs(2)) => {}
        }
    }
}

async fn shutdown_channel_tasks(
    handles: Vec<(&'static str, JoinHandle<Result<(), EgoPulseError>>)>,
) {
    for (name, mut handle) in handles {
        let shutdown_result = tokio::time::timeout(Duration::from_secs(10), &mut handle).await;
        match shutdown_result {
            Ok(Ok(Ok(()))) => {}
            Ok(Ok(Err(error))) => {
                tracing::warn!("Channel '{name}' exited during shutdown: {error}");
            }
            Ok(Err(error)) => {
                tracing::warn!("Channel '{name}' join failed during shutdown: {error}");
            }
            Err(_) => {
                tracing::warn!("Channel '{name}' did not stop in time; aborting task");
                handle.abort();
                if let Err(error) = handle.await {
                    if !error.is_cancelled() {
                        tracing::warn!(
                            "Channel '{name}' join failed after abort during shutdown: {error}"
                        );
                    }
                }
            }
        }
    }
}

fn channel_join_error(name: &str, error: JoinError) -> EgoPulseError {
    EgoPulseError::Channel(ChannelError::SendFailed(format!(
        "channel '{name}' task join failed: {error}"
    )))
}

async fn write_startup_status(state: &AppState) {
    let mcp = if let Some(m) = &state.mcp_manager {
        m.read().await.status_snapshot()
    } else {
        Default::default()
    };

    let resolved_llm = state.config.resolve_global_llm();

    let web = if state.config.web_enabled() {
        Some(WebChannelStatus {
            enabled: true,
            host: Some(state.config.web_host().to_owned()),
            port: Some(state.config.web_port()),
        })
    } else {
        None
    };

    let discord = state
        .config
        .channel_enabled("discord")
        .then_some(ChannelEntry { enabled: true });

    let telegram = state
        .config
        .channel_enabled("telegram")
        .then_some(ChannelEntry { enabled: true });

    let snapshot = StatusSnapshot {
        version: env!("CARGO_PKG_VERSION").to_string(),
        pid: std::process::id(),
        started_at: Utc::now().to_rfc3339(),
        config_path: state
            .config_path
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "unknown".to_string()),
        mcp,
        channels: ChannelsStatus {
            web,
            discord,
            telegram,
        },
        provider: ProviderStatus {
            default: resolved_llm.provider.clone(),
            model: resolved_llm.model.clone(),
        },
    };

    let state_root = PathBuf::from(&state.config.state_root);
    if let Err(error) = status::write_status(&state_root, &snapshot) {
        tracing::warn!("failed to write startup status: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::soul_agents::SoulAgentsLoader;

    fn test_config_for_runtime(state_root: String) -> crate::config::Config {
        use crate::config::ProviderId;
        crate::config::Config {
            default_provider: ProviderId::new("openai"),
            default_model: Some("gpt-4o-mini".to_string()),
            providers: std::collections::HashMap::from([(
                ProviderId::new("openai"),
                crate::config::ProviderConfig {
                    label: "OpenAI".to_string(),
                    base_url: "https://api.openai.com/v1".to_string(),
                    api_key: Some(secrecy::SecretString::new(
                        "sk-test".to_string().into_boxed_str(),
                    )),
                    default_model: "gpt-4o-mini".to_string(),
                    models: vec!["gpt-4o-mini".to_string()],
                },
            )]),
            state_root,
            log_level: "info".to_string(),
            compaction_timeout_secs: 180,
            max_history_messages: 50,
            max_session_messages: 40,
            compact_keep_recent: 20,
            channels: std::collections::HashMap::new(),
        }
    }

    #[tokio::test]
    async fn build_app_state_contains_soul_agents_loader() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = test_config_for_runtime(dir.path().to_str().expect("utf8").to_string());
        let state = build_app_state(config).await.expect("build state");
        // soul_agents が初期化されてアクセス可能であることを検証
        let _ = &*state.soul_agents;
    }

    #[tokio::test]
    async fn soul_agents_loader_loads_agents_from_config_paths() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state_root = dir.path().to_str().expect("utf8").to_string();
        let config = test_config_for_runtime(state_root);
        let loader = SoulAgentsLoader::new(&config);

        // ファイルが存在しない場合は None
        assert!(loader.load_global_agents().is_none());

        // AGENTS.md を書き込むと読み取れる
        std::fs::write(dir.path().join("AGENTS.md"), "test agents content").expect("write");
        assert_eq!(
            loader.load_global_agents(),
            Some("test agents content".to_string())
        );
    }
}
