//! Session overlay state and startup selection.

use crossterm::event::{KeyCode, KeyEvent};

use crate::storage::SessionSummary;

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
            external_chat_id: format!("tui:{thread}"),
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
}
