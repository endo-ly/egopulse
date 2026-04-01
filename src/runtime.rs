use std::path::PathBuf;
use std::sync::Arc;

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
