use std::sync::Arc;

use crate::agent_loop::{SurfaceContext, process_turn};
use crate::channels;
use crate::config::Config;
use crate::error::EgoPulseError;
use crate::llm::{Message, create_provider};
use crate::storage::Database;

pub struct AppState {
    pub db: Arc<Database>,
    pub config: Config,
    pub llm: Box<dyn crate::llm::LlmProvider>,
}

pub fn build_app_state(config: Config) -> Result<AppState, EgoPulseError> {
    let db = Arc::new(Database::new(&config.data_dir)?);
    let llm = create_provider(&config)?;
    Ok(AppState { db, config, llm })
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

pub async fn run_tui(config: Config) -> Result<(), EgoPulseError> {
    channels::tui::run(&config).await
}
