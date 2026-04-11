//! CLI チャットチャネル。
//!
//! 標準入出力を使った永続化チャットセッションを提供し、入力ごとに agent loop を実行する。

use std::io::{self, BufRead, Write};

use crate::agent_loop::{SurfaceContext, process_turn};
use crate::error::EgoPulseError;
use crate::llm_profile;
use crate::runtime::AppState;

/// Runs an interactive CLI chat loop for the given persistent session.
pub async fn run_chat(state: &AppState, session: &str) -> Result<(), EgoPulseError> {
    let stdin = io::stdin();
    let mut stdout = io::stdout();
    let context = SurfaceContext {
        channel: "cli".to_string(),
        surface_user: "local_user".to_string(),
        surface_thread: session.to_string(),
        chat_type: "cli".to_string(),
    };

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

        if let Some(response) = llm_profile::handle_command(state, &context, trimmed).await? {
            writeln!(stdout, "assistant: {response}")
                .map_err(crate::error::StorageError::Io)
                .map_err(EgoPulseError::from)?;
            stdout
                .flush()
                .map_err(crate::error::StorageError::Io)
                .map_err(EgoPulseError::from)?;
            continue;
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
