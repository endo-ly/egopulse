//! Session overlay state and startup selection.

use crossterm::event::{KeyCode, KeyEvent};
use std::ops::Range;

use crate::runtime::local_api::protocol::{SessionReference, SessionSummary};

/// A local action emitted by the session overlay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SessionAction {
    Close,
    Open(SessionSummary),
    New,
    None,
}

/// Session list overlay state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SessionsOverlay {
    sessions: Vec<SessionSummary>,
    selected: usize,
}

impl SessionsOverlay {
    pub(crate) fn new(sessions: Vec<SessionSummary>) -> Self {
        Self {
            sessions,
            selected: 0,
        }
    }

    pub(crate) fn sessions(&self) -> &[SessionSummary] {
        &self.sessions
    }

    pub(crate) fn selected(&self) -> usize {
        self.selected
    }

    /// Returns the smallest session window that keeps the selection visible.
    pub(crate) fn visible_range(&self, max_items: usize) -> Range<usize> {
        let visible_count = max_items.max(1).min(self.sessions.len());
        let max_start = self.sessions.len().saturating_sub(visible_count);
        let start = self
            .selected
            .saturating_sub(visible_count.saturating_sub(1))
            .min(max_start);
        start..start + visible_count
    }

    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> SessionAction {
        match key.code {
            KeyCode::Esc => SessionAction::Close,
            KeyCode::Char('n') => SessionAction::New,
            KeyCode::Enter => self
                .sessions
                .get(self.selected)
                .cloned()
                .map_or(SessionAction::None, SessionAction::Open),
            KeyCode::Up | KeyCode::Char('k') => {
                self.move_selection(-1);
                SessionAction::None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.move_selection(1);
                SessionAction::None
            }
            _ => SessionAction::None,
        }
    }

    fn move_selection(&mut self, delta: isize) {
        if self.sessions.is_empty() {
            self.selected = 0;
            return;
        }
        let len = self.sessions.len() as isize;
        self.selected = (self.selected as isize + delta).rem_euclid(len) as usize;
    }
}

/// The initial session chosen for the TUI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum StartupSession {
    Existing(SessionSummary),
    Named(String),
    New,
}

impl StartupSession {
    pub(crate) fn into_reference(self) -> SessionReference {
        match self {
            Self::Existing(summary) => SessionReference::Existing {
                chat_id: summary.chat_id,
            },
            Self::Named(name) => SessionReference::Named { name },
            Self::New => SessionReference::Named {
                name: format!("local-{}", uuid::Uuid::new_v4()),
            },
        }
    }
}

/// Chooses the explicit session, latest session, or a new context.
pub(crate) fn choose_startup_session(
    requested: Option<&str>,
    sessions: &[SessionSummary],
) -> StartupSession {
    if let Some(requested) = requested {
        if let Some(summary) = sessions
            .iter()
            .find(|summary| summary.surface_thread == requested)
        {
            return StartupSession::Existing(summary.clone());
        }
        return StartupSession::Named(requested.to_string());
    }
    sessions
        .first()
        .cloned()
        .map_or(StartupSession::New, StartupSession::Existing)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary(thread: &str) -> SessionSummary {
        SessionSummary {
            chat_id: 1,
            channel: "tui".to_string(),
            surface_thread: thread.to_string(),
            chat_title: None,
            last_message_time: "2026-01-01T00:00:00Z".to_string(),
            last_message_preview: None,
            agent_id: "default".to_string(),
        }
    }

    #[test]
    fn startup_opens_latest_context() {
        // Arrange
        let sessions = vec![summary("latest"), summary("older")];

        // Act
        let choice = choose_startup_session(None, &sessions);

        // Assert
        assert_eq!(choice, StartupSession::Existing(summary("latest")));
    }

    #[test]
    fn select_session_builds_overlay_open_action() {
        // Arrange
        let selected = summary("selected");
        let mut overlay = SessionsOverlay::new(vec![selected.clone()]);

        // Act
        let action = overlay.handle_key(KeyEvent::from(KeyCode::Enter));

        // Assert
        assert_eq!(action, SessionAction::Open(selected));
    }

    #[test]
    fn explicit_unknown_session_is_named_context() {
        // Arrange / Act
        let choice = choose_startup_session(Some("new-name"), &[]);

        // Assert
        assert_eq!(choice, StartupSession::Named("new-name".to_string()));
    }

    #[test]
    fn visible_range_keeps_selected_session_on_screen() {
        // Arrange
        let sessions = (0..20)
            .map(|index| summary(&format!("session-{index}")))
            .collect();
        let mut overlay = SessionsOverlay::new(sessions);

        // Act
        overlay.handle_key(KeyEvent::from(KeyCode::Char('j')));
        overlay.handle_key(KeyEvent::from(KeyCode::Char('j')));
        overlay.handle_key(KeyEvent::from(KeyCode::Char('j')));
        overlay.handle_key(KeyEvent::from(KeyCode::Char('j')));
        overlay.handle_key(KeyEvent::from(KeyCode::Char('j')));
        overlay.handle_key(KeyEvent::from(KeyCode::Char('j')));
        overlay.handle_key(KeyEvent::from(KeyCode::Char('j')));

        // Assert
        let visible = overlay.visible_range(5);
        assert_eq!(visible, 3..8);
        assert!(visible.contains(&overlay.selected()));
    }
}
