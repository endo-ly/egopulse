//! Inline viewport TUI.
//!
//! The terminal's scrollback owns committed conversation blocks. Ratatui only
//! renders the active turn and the composer in the bottom inline viewport.

mod event;

use std::io::{self, Stdout};
use std::sync::Arc;

use crossterm::execute;
use crossterm::terminal::{
    Clear, ClearType, MoveToColumn, Print, disable_raw_mode, enable_raw_mode,
};
use futures_util::StreamExt;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::terminal::{TerminalOptions, Viewport};
use tokio::time::{Duration, Instant};

use crate::error::{EgoPulseError, TuiError};
use crate::runtime::AppState;

use self::event::UiEvent;

const MIN_TERMINAL_HEIGHT: u16 = 10;
const INLINE_VIEWPORT_HEIGHT: u16 = 10;
const MAX_FRAME_INTERVAL: Duration = Duration::from_millis(16);

type Backend = CrosstermBackend<Stdout>;

/// Owns raw mode and the inline viewport for the duration of one TUI run.
struct TuiSession {
    terminal: Terminal<Backend>,
}

impl TuiSession {
    fn new() -> Result<Self, TuiError> {
        let (_, height) =
            crossterm::terminal::size().map_err(|error| TuiError::InitFailed(error.to_string()))?;
        if height < MIN_TERMINAL_HEIGHT {
            return Err(TuiError::InitFailed(format!(
                "terminal height must be at least {MIN_TERMINAL_HEIGHT} lines"
            )));
        }

        enable_raw_mode().map_err(|error| TuiError::InitFailed(error.to_string()))?;
        let stdout = io::stdout();
        let terminal = Terminal::with_options(
            CrosstermBackend::new(stdout),
            TerminalOptions {
                viewport: Viewport::Inline(INLINE_VIEWPORT_HEIGHT),
            },
        )
        .map_err(|error| {
            let _ = disable_raw_mode();
            TuiError::InitFailed(error.to_string())
        })?;

        Ok(Self { terminal })
    }
}

impl Drop for TuiSession {
    fn drop(&mut self) {
        let _ = self.terminal.show_cursor();
        let _ = self.terminal.clear();
        let _ = disable_raw_mode();
        let mut stdout = io::stdout();
        let _ = execute!(
            stdout,
            Clear(ClearType::FromCursorDown),
            MoveToColumn(0),
            Print("\r\n")
        );
    }
}

/// Starts the inline TUI.
pub(crate) async fn run(state: Arc<AppState>, _session: Option<&str>) -> Result<(), EgoPulseError> {
    let mut session = TuiSession::new()?;
    let mut event_stream = crossterm::event::EventStream::new();
    let shutdown = state.supervisor.shutdown_token();
    let mut dirty = true;
    let mut last_draw = Instant::now() - MAX_FRAME_INTERVAL;

    loop {
        if dirty && last_draw.elapsed() >= MAX_FRAME_INTERVAL {
            session
                .terminal
                .draw(|frame| {
                    let area = frame.area();
                    let text = format!(
                        "EgoPulse inline TUI  {}x{}\nPress q or Ctrl-C to quit",
                        area.width, area.height
                    );
                    frame.render_widget(ratatui::widgets::Paragraph::new(text), area);
                })
                .map_err(|error| TuiError::RenderFailed(error.to_string()))?;
            last_draw = Instant::now();
            dirty = false;
        }

        tokio::select! {
            maybe_event = event_stream.next() => {
                let Some(event) = maybe_event else { break; };
                let event = event.map_err(|error| TuiError::EventFailed(error.to_string()))?;
                if let Some(UiEvent::Key(key)) = UiEvent::from_terminal(event)
                    && (key.code == crossterm::event::KeyCode::Char('q')
                        || (key.code == crossterm::event::KeyCode::Char('c')
                            && key.modifiers.contains(crossterm::event::KeyModifiers::CONTROL)))
                {
                    break;
                }
                dirty = true;
            }
            _ = shutdown.cancelled() => break,
        }
    }

    state.supervisor.shutdown().await;
    Ok(())
}
