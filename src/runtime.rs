use std::path::PathBuf;
use std::sync::Arc;

use tokio::task::JoinHandle;
use tracing::info;

use crate::agent_loop::{SurfaceContext, process_turn};
use crate::channel_adapter::ChannelRegistry;
use crate::channels;
use crate::config::Config;
use crate::error::EgoPulseError;
use crate::llm::{Message, create_provider};
use crate::storage::{Database, SessionSummary, call_blocking};
use crate::web::WebAdapter;

pub struct AppState {
    pub db: Arc<Database>,
    pub config: Config,
    pub config_path: Option<PathBuf>,
    pub llm: Arc<dyn crate::llm::LlmProvider>,
    pub channels: Arc<ChannelRegistry>,
}

impl Clone for AppState {
    fn clone(&self) -> Self {
        Self {
            db: Arc::clone(&self.db),
            config: self.config.clone(),
            config_path: self.config_path.clone(),
            llm: Arc::clone(&self.llm),
            channels: Arc::clone(&self.channels),
        }
    }
}

pub fn build_app_state(config: Config) -> Result<AppState, EgoPulseError> {
    build_app_state_with_path(config, None)
}

pub fn build_app_state_with_path(
    config: Config,
    config_path: Option<PathBuf>,
) -> Result<AppState, EgoPulseError> {
    let db = Arc::new(Database::new(&config.data_dir)?);
    let llm = Arc::from(create_provider(&config)?);

    // Build channel registry
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

    Ok(AppState {
        db,
        config,
        config_path,
        llm,
        channels: Arc::new(channels),
    })
}

pub async fn ask(config: Config, prompt: &str) -> Result<String, EgoPulseError> {
    let llm = create_provider(&config)?;
    let messages = vec![Message {
        role: "user".to_string(),
        content: prompt.to_string(),
    }];

    tokio::select! {
        response = llm.send_message("", messages) => Ok(response?.content),
        _ = tokio::signal::ctrl_c() => Err(EgoPulseError::ShutdownRequested),
    }
}

pub async fn ask_in_session(
    config: Config,
    session: &str,
    prompt: &str,
) -> Result<String, EgoPulseError> {
    let state = build_app_state(config)?;
    let context = SurfaceContext {
        channel: "cli".to_string(),
        surface_user: "local_user".to_string(),
        surface_thread: session.to_string(),
        chat_type: "cli".to_string(),
    };

    tokio::select! {
        response = process_turn(&state, &context, prompt) => response,
        _ = tokio::signal::ctrl_c() => Err(EgoPulseError::ShutdownRequested),
    }
}

pub async fn list_sessions(state: &AppState) -> Result<Vec<SessionSummary>, EgoPulseError> {
    call_blocking(state.db.clone(), move |db| db.list_sessions())
        .await
        .map_err(EgoPulseError::from)
}

pub async fn load_session_messages(
    state: &AppState,
    context: &SurfaceContext,
) -> Result<Vec<Message>, EgoPulseError> {
    let chat_id = call_blocking(state.db.clone(), {
        let channel = context.channel.clone();
        let session_key = context.session_key();
        let surface_thread = context.surface_thread.clone();
        let chat_type = context.chat_type.clone();
        move |db| {
            db.resolve_or_create_chat_id(&channel, &session_key, Some(&surface_thread), &chat_type)
        }
    })
    .await?;

    let history = call_blocking(state.db.clone(), move |db| db.get_all_messages(chat_id)).await?;
    Ok(history
        .into_iter()
        .map(|message| Message {
            role: if message.is_from_bot {
                "assistant".to_string()
            } else {
                "user".to_string()
            },
            content: message.content,
        })
        .collect())
}

pub async fn send_turn(
    state: &AppState,
    context: &SurfaceContext,
    prompt: &str,
) -> Result<String, EgoPulseError> {
    tokio::select! {
        response = process_turn(state, context, prompt) => response,
        _ = tokio::signal::ctrl_c() => Err(EgoPulseError::ShutdownRequested),
    }
}

pub async fn run_tui(config: Config) -> Result<(), EgoPulseError> {
    let state = build_app_state(config)?;
    channels::tui::run(state).await
}

/// 全有効チャネルを一括起動 (microclaw 互換)。
///
/// `egopulse start` から呼び出される。
/// microclaw `src/runtime.rs::run()` と同じパターン:
/// 設定ベースでチャネルを構築 → spawn → ctrl_c 待機。
///
/// spawn したタスクの JoinHandle を監視し、即時終了 (起動失敗) を検知する。
pub async fn start_channels(state: AppState) -> Result<(), EgoPulseError> {
    let mut has_active_channels = false;
    let mut handles: Vec<(&str, JoinHandle<()>)> = Vec::new();

    // Web サーバー起動
    if state.config.web_enabled() {
        has_active_channels = true;
        let web_state = state.clone();
        let host = state.config.web_host();
        let port = state.config.web_port();
        info!("Starting Web UI server on {host}:{port}");
        let handle = tokio::spawn(async move {
            if let Err(e) = crate::web::run_server(web_state, &host, port).await {
                tracing::error!("Web server error: {e}");
            }
        });
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
                crate::channels::discord::start_discord_bot(discord_state, token).await;
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
                crate::channels::telegram::start_telegram_bot(telegram_state, token).await;
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
            crate::error::ConfigError::MissingApiKey,
        ));
    }

    info!("Runtime active; waiting for Ctrl-C or channel failure");

    // spawn したタスクの即時終了 (起動失敗) を検知
    loop {
        let mut any_finished = false;
        for (name, handle) in &mut handles {
            if handle.is_finished() {
                any_finished = true;
                match handle.await {
                    Ok(()) => tracing::error!("Channel '{name}' exited unexpectedly"),
                    Err(e) => tracing::error!("Channel '{name}' failed: {e}"),
                }
            }
        }
        if any_finished {
            break;
        }

        tokio::select! {
            _ = tokio::signal::ctrl_c() => return Ok(()),
            _ = tokio::time::sleep(std::time::Duration::from_secs(2)) => {},
        }
    }

    Ok(())
}
