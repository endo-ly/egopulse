//! Terminal events used by the inline TUI.

use crossterm::event::{Event, KeyEvent, KeyEventKind};

/// A terminal event that can change the TUI state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum UiEvent {
    Key(KeyEvent),
    Paste(String),
    Resize { width: u16, height: u16 },
}

impl UiEvent {
    /// Converts a crossterm event into an event understood by the TUI.
    pub(crate) fn from_terminal(event: Event) -> Option<Self> {
        match event {
            Event::Key(key) if key.kind == KeyEventKind::Press => Some(Self::Key(key)),
            Event::Paste(text) => Some(Self::Paste(text)),
            Event::Resize(width, height) => Some(Self::Resize { width, height }),
            _ => None,
        }
    }
}
