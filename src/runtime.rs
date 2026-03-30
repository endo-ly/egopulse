use crate::config::Config;
use crate::error::EgoPulseError;
use crate::llm::{Message, create_provider};

pub struct AppState {
    pub config: Config,
    pub llm: Box<dyn crate::llm::LlmProvider>,
}

// Issue 1 only establishes the bootstrap seam for Issue 2. Keep this runtime
// thin so the channel loop and session persistence can be pulled toward
// MicroClaw's runtime shape instead of extending a one-shot wrapper.
pub fn build_app_state(config: Config) -> Result<AppState, EgoPulseError> {
    let llm = create_provider(&config)?;
    Ok(AppState { config, llm })
}

pub async fn ask(config: Config, prompt: &str) -> Result<String, EgoPulseError> {
    let state = build_app_state(config)?;
    let messages = vec![Message {
        role: "user".to_string(),
        content: prompt.to_string(),
    }];

    tokio::select! {
        response = state.llm.send_message("", messages) => Ok(response?.content),
        _ = tokio::signal::ctrl_c() => Err(EgoPulseError::ShutdownRequested),
    }
}
