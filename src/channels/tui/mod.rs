//! Inline viewport TUI.
//!
//! Committed blocks are written to terminal scrollback once. Ratatui owns only
//! the active turn, composer, and temporary session overlay at the bottom.

mod composer;
mod draw;
mod event;
mod markdown;
mod sessions;
mod transcript;

use std::io::{self, Stdout};
use std::path::PathBuf;

use crossterm::cursor::MoveToColumn;
use crossterm::event::{DisableBracketedPaste, EnableBracketedPaste};
use crossterm::execute;
use crossterm::style::Print;
use crossterm::terminal::{Clear, ClearType, disable_raw_mode, enable_raw_mode};
use futures_util::StreamExt;
use ratatui::backend::CrosstermBackend;
use ratatui::text::Text;
use ratatui::widgets::{Paragraph, Widget};
use ratatui::{Terminal, TerminalOptions, Viewport};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::time::Duration;

use crate::error::{EgoPulseError, TuiError};
use crate::runtime::local_api::LocalRuntimeClient;
use crate::runtime::local_api::client::CommandResult;
use crate::runtime::local_api::protocol::{CommandOutcome, SessionSummary, SessionView, TurnEvent};
use crate::slash_commands;

use self::composer::{Composer, Effect, InputEvent};
use self::event::UiEvent;
use self::sessions::{SessionAction, SessionsOverlay, StartupSession, choose_startup_session};
use self::transcript::Transcript;

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
        let mut stdout = io::stdout();
        if let Err(error) = execute!(stdout, EnableBracketedPaste) {
            let _ = disable_raw_mode();
            return Err(TuiError::InitFailed(error.to_string()));
        }

        let terminal = Terminal::with_options(
            CrosstermBackend::new(io::stdout()),
            TerminalOptions {
                viewport: Viewport::Inline(INLINE_VIEWPORT_HEIGHT),
            },
        )
        .map_err(|error| {
            let mut stdout = io::stdout();
            let _ = execute!(stdout, DisableBracketedPaste);
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
        let mut stdout = io::stdout();
        let _ = execute!(stdout, DisableBracketedPaste);
        let _ = disable_raw_mode();
        let _ = execute!(
            stdout,
            Clear(ClearType::FromCursorDown),
            MoveToColumn(0),
            Print("\r\n")
        );
    }
}

enum RuntimeEvent {
    Agent(TurnEvent),
    TurnFinished(Result<String, EgoPulseError>),
    SlashFinished {
        prompt: String,
        result: Result<CommandResult, EgoPulseError>,
    },
    SessionsLoaded(Result<Vec<SessionSummary>, EgoPulseError>),
    SessionLoaded(Result<SessionView, EgoPulseError>),
}

struct TuiApp {
    client: LocalRuntimeClient,
    session: SessionView,
    transcript: Transcript,
    composer: Composer,
    status: String,
    model: String,
    sessions: Option<SessionsOverlay>,
    runtime_tx: UnboundedSender<RuntimeEvent>,
    runtime_rx: UnboundedReceiver<RuntimeEvent>,
    busy: bool,
    pending_prompt: Option<String>,
}

impl TuiApp {
    fn new(
        client: LocalRuntimeClient,
        session: SessionView,
        runtime_tx: UnboundedSender<RuntimeEvent>,
        runtime_rx: UnboundedReceiver<RuntimeEvent>,
    ) -> Self {
        let model = session.effective_model.clone();
        Self {
            client,
            transcript: Transcript::from_entries(&session.transcript),
            session,
            composer: Composer::new(),
            status: "Ready".to_string(),
            model,
            sessions: None,
            runtime_tx,
            runtime_rx,
            busy: false,
            pending_prompt: None,
        }
    }

    fn draw(&self, terminal: &mut TuiSession) -> Result<(), EgoPulseError> {
        let surface_thread = self.session.surface_thread.clone();
        let agent_id = self.session.agent_id.clone();
        let status = self.status.clone();
        let model = self.model.clone();
        terminal
            .terminal
            .draw(|frame| {
                draw::draw(
                    frame,
                    draw::DrawState {
                        surface_thread: &surface_thread,
                        agent_id: &agent_id,
                        transcript: &self.transcript,
                        composer: &self.composer,
                        status: &status,
                        model: &model,
                        sessions: self.sessions.as_ref(),
                    },
                );
            })
            .map_err(|error| EgoPulseError::from(TuiError::RenderFailed(error.to_string())))?;
        Ok(())
    }

    fn insert_pending(&mut self, terminal: &mut TuiSession) -> Result<(), EgoPulseError> {
        let blocks = self.transcript.drain_pending();
        if blocks.is_empty() {
            return Ok(());
        }

        let width = terminal
            .terminal
            .size()
            .map_err(|error| TuiError::RenderFailed(error.to_string()))?
            .width as usize;
        let lines: Vec<_> = blocks
            .iter()
            .flat_map(|block| draw::render_block(block, width))
            .collect();
        let height = lines.len().max(1).min(u16::MAX as usize) as u16;
        terminal
            .terminal
            .insert_before(height, move |buffer| {
                Paragraph::new(Text::from(lines)).render(buffer.area, buffer);
            })
            .map_err(|error| EgoPulseError::from(TuiError::RenderFailed(error.to_string())))?;
        Ok(())
    }

    async fn handle_ui(
        &mut self,
        event: UiEvent,
        terminal: &mut TuiSession,
    ) -> Result<bool, EgoPulseError> {
        match event {
            UiEvent::Resize { .. } => {
                terminal
                    .terminal
                    .autoresize()
                    .map_err(|error| TuiError::RenderFailed(error.to_string()))?;
            }
            UiEvent::Paste(text) => {
                let effect = self.composer.handle(InputEvent::Paste(text));
                self.update_completion();
                self.apply_composer_effect(effect).await;
            }
            UiEvent::Key(key) if self.sessions.is_some() => {
                let action = self
                    .sessions
                    .as_mut()
                    .map(|sessions| sessions.handle_key(key))
                    .unwrap_or(SessionAction::None);
                self.apply_session_action(action);
            }
            UiEvent::Key(key) => {
                let effect = self.composer.handle(InputEvent::Key(key));
                self.update_completion();
                if matches!(effect, Effect::Quit) {
                    return Ok(true);
                }
                self.apply_composer_effect(effect).await;
            }
        }
        self.insert_pending(terminal)?;
        Ok(false)
    }

    async fn apply_composer_effect(&mut self, effect: Effect) {
        match effect {
            Effect::None => {}
            Effect::Quit => {}
            Effect::Send(prompt) => {
                if self.submit_prompt(prompt).await {
                    self.composer.accept_submission();
                }
            }
        }
    }

    fn update_completion(&mut self) {
        let text = self.composer.text();
        let prefix = text.split_whitespace().next().unwrap_or("");
        if !prefix.starts_with('/') || text.split_whitespace().count() > 1 {
            self.composer.clear_completion();
            return;
        }

        let candidates = completion_candidates(prefix);
        if candidates.len() == 1 && candidates[0] == prefix {
            self.composer.clear_completion();
        } else {
            self.composer.set_completion_candidates(candidates);
        }
    }

    async fn submit_prompt(&mut self, prompt: String) -> bool {
        if self.busy {
            if !slash_commands::is_slash_command(&prompt) {
                match self
                    .client
                    .stage_followup(self.session.reference.clone(), prompt.clone())
                    .await
                {
                    Ok(crate::runtime::local_api::protocol::FollowupOutcome::Accepted) => {
                        self.status = "Turn running — follow-up queued".to_string();
                        return true;
                    }
                    Ok(crate::runtime::local_api::protocol::FollowupOutcome::NoToolPhase) => {}
                    Err(error) => {
                        self.status = format!("Follow-up rejected: {error}");
                        return false;
                    }
                }
            }
            if queue_prompt(&mut self.pending_prompt, prompt) {
                self.status = "Turn running — queued one prompt".to_string();
                true
            } else {
                self.status = "Turn running — queue is full".to_string();
                false
            }
        } else {
            self.dispatch_prompt(prompt);
            true
        }
    }

    fn dispatch_prompt(&mut self, prompt: String) {
        if prompt.trim() == "/sessions" {
            self.open_sessions();
            return;
        }

        if !slash_commands::is_slash_command(&prompt) {
            self.start_agent_turn(prompt);
            return;
        }

        self.busy = true;
        self.status = "Processing…".to_string();
        let client = self.client.clone();
        let session = self.session.reference.clone();
        let tx = self.runtime_tx.clone();
        tokio::spawn(async move {
            let result = client.execute_command(session, prompt.clone()).await;
            let _ = tx.send(RuntimeEvent::SlashFinished { prompt, result });
        });
    }

    fn start_agent_turn(&mut self, prompt: String) {
        self.transcript.begin_turn(prompt.clone());
        self.busy = true;
        self.status = "Working…".to_string();
        let client = self.client.clone();
        let session = self.session.reference.clone();
        let event_tx = self.runtime_tx.clone();
        let completion_tx = self.runtime_tx.clone();
        tokio::spawn(async move {
            let result = client
                .execute_turn(session, prompt, move |event| {
                    let _ = event_tx.send(RuntimeEvent::Agent(event));
                })
                .await;
            let _ = completion_tx.send(RuntimeEvent::TurnFinished(result));
        });
    }

    fn open_sessions(&mut self) {
        self.busy = true;
        self.status = "Loading sessions…".to_string();
        let client = self.client.clone();
        let tx = self.runtime_tx.clone();
        tokio::spawn(async move {
            let result = client.list_sessions().await;
            let _ = tx.send(RuntimeEvent::SessionsLoaded(result));
        });
    }

    fn apply_session_action(&mut self, action: SessionAction) {
        match action {
            SessionAction::Close => {
                self.sessions = None;
                self.status = "Ready".to_string();
                self.dispatch_queued_prompt();
            }
            SessionAction::Open(summary) => {
                self.sessions = None;
                self.request_session_load(StartupSession::Existing(summary));
            }
            SessionAction::New => {
                self.sessions = None;
                self.request_session_load(StartupSession::Named(new_session_name()));
            }
            SessionAction::None => {}
        }
    }

    fn request_session_load(&mut self, startup: StartupSession) {
        self.busy = true;
        self.status = "Loading session…".to_string();
        let client = self.client.clone();
        let tx = self.runtime_tx.clone();
        tokio::spawn(async move {
            let result = client.open_session(startup.into_reference()).await;
            let _ = tx.send(RuntimeEvent::SessionLoaded(result));
        });
    }

    async fn handle_runtime(
        &mut self,
        event: RuntimeEvent,
        terminal: &mut TuiSession,
    ) -> Result<(), EgoPulseError> {
        match event {
            RuntimeEvent::Agent(event) => {
                self.status = agent_status(&event);
                self.transcript.apply_turn_event(event);
            }
            RuntimeEvent::TurnFinished(result) => {
                match result {
                    Ok(response) if self.transcript.active().is_some() => {
                        self.transcript
                            .apply_turn_event(TurnEvent::FinalResponse { text: response });
                    }
                    Err(error) if self.transcript.active().is_some() => {
                        self.transcript.apply_turn_event(TurnEvent::Error {
                            message: error.user_message(),
                        });
                    }
                    _ => {}
                }
                self.busy = false;
                self.status = "Ready".to_string();
                self.insert_pending(terminal)?;
                self.dispatch_queued_prompt();
            }
            RuntimeEvent::SlashFinished { prompt, result } => {
                let result = match result {
                    Ok(result) => result,
                    Err(error) => {
                        self.busy = false;
                        self.status = error.user_message();
                        self.insert_pending(terminal)?;
                        self.dispatch_queued_prompt();
                        return Ok(());
                    }
                };
                self.session.effective_provider = result.effective_provider.clone();
                self.model = result.effective_model.clone();
                let handled = !matches!(&result.outcome, CommandOutcome::NotHandled);
                match result.outcome {
                    CommandOutcome::Respond { text: response } => {
                        if is_new_command(&prompt) {
                            self.transcript.clear();
                        }
                        self.transcript.begin_turn(prompt);
                        self.transcript
                            .apply_turn_event(TurnEvent::FinalResponse { text: response });
                    }
                    CommandOutcome::Error { message } => {
                        self.transcript.begin_turn(prompt);
                        self.transcript
                            .apply_turn_event(TurnEvent::Error { message });
                    }
                    CommandOutcome::NotHandled => self.start_agent_turn(prompt),
                }
                if handled {
                    self.busy = false;
                    self.status = "Ready".to_string();
                    self.insert_pending(terminal)?;
                    self.dispatch_queued_prompt();
                }
            }
            RuntimeEvent::SessionsLoaded(result) => {
                self.busy = false;
                match result {
                    Ok(sessions) => {
                        self.sessions = Some(SessionsOverlay::new(sessions));
                        self.status = "Select a session".to_string();
                    }
                    Err(error) => {
                        self.status = error.user_message();
                        self.dispatch_queued_prompt();
                    }
                }
            }
            RuntimeEvent::SessionLoaded(result) => {
                self.busy = false;
                match result {
                    Ok(loaded) => {
                        self.model = loaded.effective_model.clone();
                        self.session = loaded.clone();
                        self.transcript = Transcript::from_entries(&loaded.transcript);
                        self.status = "Ready".to_string();
                        self.insert_pending(terminal)?;
                        self.dispatch_queued_prompt();
                    }
                    Err(error) => {
                        self.status = error.user_message();
                        self.dispatch_queued_prompt();
                    }
                }
            }
        }
        Ok(())
    }

    fn dispatch_queued_prompt(&mut self) {
        if self.busy {
            return;
        }
        if let Some(prompt) = self.pending_prompt.take() {
            self.dispatch_prompt(prompt);
        }
    }
}

fn queue_prompt(pending_prompt: &mut Option<String>, prompt: String) -> bool {
    if pending_prompt.is_some() {
        return false;
    }
    *pending_prompt = Some(prompt);
    true
}

/// Starts the inline TUI as a client of the already-running runtime.
pub(crate) async fn run(
    socket_path: PathBuf,
    requested_session: Option<&str>,
) -> Result<(), EgoPulseError> {
    let client = LocalRuntimeClient::connect(socket_path).await?;
    let sessions = client.list_sessions().await?;
    let startup = choose_startup_session(requested_session, &sessions);
    let loaded = client.open_session(startup.into_reference()).await?;
    let mut terminal = TuiSession::new()?;
    let (runtime_tx, runtime_rx) = unbounded_channel();
    let mut app = TuiApp::new(client, loaded, runtime_tx, runtime_rx);
    app.insert_pending(&mut terminal)?;

    let mut event_stream = crossterm::event::EventStream::new();
    let mut redraw_tick = tokio::time::interval(MAX_FRAME_INTERVAL);
    let mut health_tick = tokio::time::interval(Duration::from_secs(1));
    let mut dirty = true;
    let mut disconnect_error = None;

    loop {
        tokio::select! {
            _ = redraw_tick.tick() => {
                if dirty {
                    app.draw(&mut terminal)?;
                    dirty = false;
                }
            }
            _ = health_tick.tick() => {
                if let Err(error) = app.client.runtime_info().await {
                    disconnect_error = Some(error);
                    break;
                }
            }
            maybe_event = event_stream.next() => {
                let Some(event) = maybe_event else { break; };
                let event = event.map_err(|error| TuiError::EventFailed(error.to_string()))?;
                if let Some(event) = UiEvent::from_terminal(event)
                    && app.handle_ui(event, &mut terminal).await?
                {
                    break;
                }
                dirty = true;
            }
            Some(event) = app.runtime_rx.recv() => {
                app.handle_runtime(event, &mut terminal).await?;
                dirty = true;
            }
        }
    }

    disconnect_error.map_or(Ok(()), Err)
}

const TUI_ONLY_COMMANDS: &[&str] = &["/sessions"];

fn completion_candidates(prefix: &str) -> Vec<String> {
    let prefix = prefix.trim_start();
    if !prefix.starts_with('/') || prefix.split_whitespace().count() != 1 {
        return Vec::new();
    }

    let normalized_prefix = prefix.to_ascii_lowercase();
    let mut candidates = slash_commands::completion_candidates(prefix);
    candidates.extend(
        TUI_ONLY_COMMANDS
            .iter()
            .filter(|command| command.starts_with(&normalized_prefix))
            .map(|command| (*command).to_string()),
    );
    candidates
}

fn new_session_name() -> String {
    format!("local-{}", uuid::Uuid::new_v4())
}

fn is_new_command(prompt: &str) -> bool {
    prompt.split_whitespace().next() == Some("/new")
}

fn agent_status(event: &TurnEvent) -> String {
    match event {
        TurnEvent::Iteration { iteration } => format!("Iteration {iteration}"),
        TurnEvent::Delta { .. } => "Streaming response…".to_string(),
        TurnEvent::ToolStart { name, .. } => format!("Running {name}…"),
        TurnEvent::ToolResult {
            name,
            is_error,
            duration_ms,
            ..
        } => format!(
            "{} {name} ({duration_ms}ms)",
            if *is_error {
                "Tool failed:"
            } else {
                "Tool completed:"
            }
        ),
        TurnEvent::FinalResponse { .. } => "Ready".to_string(),
        TurnEvent::Error { .. } => "Turn failed".to_string(),
        TurnEvent::UserInputInjected { .. } => "Queued input".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::{completion_candidates, queue_prompt};

    #[test]
    fn completion_includes_tui_only_sessions_command() {
        // Arrange / Act
        let candidates = completion_candidates("/se");

        // Assert
        assert_eq!(candidates, ["/sessions"]);
    }

    #[test]
    fn completion_keeps_shared_commands() {
        // Arrange / Act
        let candidates = completion_candidates("/mo");

        // Assert
        assert_eq!(candidates, ["/models", "/model"]);
    }

    #[test]
    fn full_prompt_queue_rejects_second_prompt() {
        // Arrange
        let mut pending = Some("first".to_string());

        // Act
        let accepted = queue_prompt(&mut pending, "second".to_string());

        // Assert
        assert!(!accepted);
        assert_eq!(pending.as_deref(), Some("first"));
    }
}
