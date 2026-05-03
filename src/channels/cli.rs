//! CLI チャットチャネル。
//!
//! 標準入出力を使った永続化チャットセッションを提供し、入力ごとに agent loop を実行する。

use std::io::{self, BufRead, Write};

use crate::agent_loop::{SurfaceContext, process_turn};
use crate::error::EgoPulseError;
use crate::runtime::AppState;
use crate::slash_commands::{SlashCommandOutcome, process_slash_command};

/// Runs an interactive CLI chat loop for the given persistent session.
pub async fn run_chat(state: &AppState, session: &str) -> Result<(), EgoPulseError> {
    let stdin = io::stdin();
    let mut stdout = io::stdout();
    let context = SurfaceContext::new(
        "cli".to_string(),
        "local_user".to_string(),
        session.to_string(),
        "cli".to_string(),
        state.config.default_agent.to_string(),
    );

    writeln!(stdout, "session: {}", session)
        .map_err(crate::error::StorageError::Io)
        .map_err(EgoPulseError::from)?;
    writeln!(stdout, "type `/exit` to leave the chat")
        .map_err(crate::error::StorageError::Io)
        .map_err(EgoPulseError::from)?;

    for line in stdin.lock().lines() {
        let input = line
            .map_err(crate::error::StorageError::Io)
            .map_err(EgoPulseError::from)?;
        let trimmed = input.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed == "/exit" {
            break;
        }

        match process_slash_command(state, &context, trimmed, None).await {
            SlashCommandOutcome::Respond(response) | SlashCommandOutcome::Error(response) => {
                writeln!(stdout, "assistant: {response}")
                    .map_err(crate::error::StorageError::Io)
                    .map_err(EgoPulseError::from)?;
                stdout
                    .flush()
                    .map_err(crate::error::StorageError::Io)
                    .map_err(EgoPulseError::from)?;
                continue;
            }
            SlashCommandOutcome::NotHandled => {}
        }

        writeln!(stdout, "you: {trimmed}")
            .map_err(crate::error::StorageError::Io)
            .map_err(EgoPulseError::from)?;
        let response = process_turn(state, &context, trimmed).await?;
        writeln!(stdout, "assistant: {response}")
            .map_err(crate::error::StorageError::Io)
            .map_err(EgoPulseError::from)?;
        stdout
            .flush()
            .map_err(crate::error::StorageError::Io)
            .map_err(EgoPulseError::from)?;
    }

    Ok(())
}
