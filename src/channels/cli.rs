//! CLI チャットチャネル。
//!
//! 標準入出力を使った永続化チャットセッションを提供し、入力ごとに agent loop を実行する。

use std::io::{self, BufRead, Write};

use crate::agent_loop::{SurfaceContext, process_turn};
use crate::error::EgoPulseError;
use crate::runtime::AppState;
use crate::slash_commands::{SlashCommandOutcome, process_slash_command};

fn map_io_error(error: std::io::Error) -> EgoPulseError {
    crate::error::StorageError::Io(error).into()
}

fn write_line(stdout: &mut impl Write, args: std::fmt::Arguments<'_>) -> Result<(), EgoPulseError> {
    writeln!(stdout, "{args}").map_err(map_io_error)
}

fn flush(stdout: &mut impl Write) -> Result<(), EgoPulseError> {
    stdout.flush().map_err(map_io_error)
}

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

    write_line(&mut stdout, format_args!("session: {session}"))?;
    write_line(&mut stdout, format_args!("type `/exit` to leave the chat"))?;

    for line in stdin.lock().lines() {
        let input = line.map_err(map_io_error)?;
        let trimmed = input.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed == "/exit" {
            break;
        }

        match process_slash_command(state, &context, trimmed, None).await {
            SlashCommandOutcome::Respond(response) | SlashCommandOutcome::Error(response) => {
                write_line(&mut stdout, format_args!("assistant: {response}"))?;
                flush(&mut stdout)?;
                continue;
            }
            SlashCommandOutcome::NotHandled => {}
        }

        write_line(&mut stdout, format_args!("you: {trimmed}"))?;
        let response = process_turn(state, &context, trimmed).await?;
        write_line(&mut stdout, format_args!("assistant: {response}"))?;
        flush(&mut stdout)?;
    }

    Ok(())
}
