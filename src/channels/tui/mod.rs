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
use std::sync::Arc;

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

use crate::agent_loop;
use crate::agent_loop::event::AgentEvent;
use crate::config::AgentId;
use crate::conversation::SurfaceContext;
use crate::error::{EgoPulseError, TuiError};
use crate::llm::Message;
use crate::runtime::AppState;
use crate::slash_commands::{self, SlashCommandOutcome};
use crate::storage::{SessionSummary, call_blocking};

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
    Agent(AgentEvent),
    TurnFinished(Result<String, EgoPulseError>),
    SlashFinished {
        prompt: String,
        outcome: SlashCommandOutcome,
    },
    SessionsLoaded(Result<Vec<SessionSummary>, EgoPulseError>),
    SessionLoaded(Result<LoadedContext, EgoPulseError>),
}

struct LoadedContext {
    context: SurfaceContext,
    messages: Vec<Message>,
}

struct TuiApp {
    state: Arc<AppState>,
    context: SurfaceContext,
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
        state: Arc<AppState>,
        loaded: LoadedContext,
        runtime_tx: UnboundedSender<RuntimeEvent>,
        runtime_rx: UnboundedReceiver<RuntimeEvent>,
    ) -> Self {
        let model = model_for(&state, &loaded.context);
        Self {
            state,
            context: loaded.context,
            transcript: Transcript::from_messages(&loaded.messages),
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
        let context = self.context.clone();
        let status = self.status.clone();
        let model = self.model.clone();
        terminal
            .terminal
            .draw(|frame| {
                draw::draw(
                    frame,
                    draw::DrawState {
                        context: &context,
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
                self.apply_composer_effect(effect);
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
                self.apply_composer_effect(effect);
            }
        }
        self.insert_pending(terminal)?;
        Ok(false)
    }

    fn apply_composer_effect(&mut self, effect: Effect) {
        match effect {
            Effect::None => {}
            Effect::Quit => {}
            Effect::Send(prompt) => self.submit_prompt(prompt),
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

    fn submit_prompt(&mut self, prompt: String) {
        if self.busy {
            if self.pending_prompt.is_none() {
                self.pending_prompt = Some(prompt);
                self.status = "Turn running — queued one prompt".to_string();
            } else {
                self.status = "Turn running — queue is full".to_string();
            }
            return;
        }
        self.dispatch_prompt(prompt);
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
        let state = Arc::clone(&self.state);
        let context = self.context.clone();
        let tx = self.runtime_tx.clone();
        self.state.supervisor.spawn_turn(async move {
            let outcome =
                slash_commands::process_slash_command(&state, &context, &prompt, None).await;
            let _ = tx.send(RuntimeEvent::SlashFinished { prompt, outcome });
        });
    }

    fn start_agent_turn(&mut self, prompt: String) {
        self.transcript.begin_turn(prompt.clone());
        self.busy = true;
        self.status = "Working…".to_string();
        let state = Arc::clone(&self.state);
        let dependencies = state.turn_dependencies();
        let context = self.context.clone();
        let event_tx = self.runtime_tx.clone();
        let completion_tx = self.runtime_tx.clone();
        self.state.supervisor.spawn_turn(async move {
            let result = agent_loop::process_turn_with_events(
                &dependencies,
                &context,
                &prompt,
                move |event| {
                    let _ = event_tx.send(RuntimeEvent::Agent(event));
                },
            )
            .await;
            let _ = completion_tx.send(RuntimeEvent::TurnFinished(result));
        });
    }

    fn open_sessions(&mut self) {
        self.busy = true;
        self.status = "Loading sessions…".to_string();
        let state = Arc::clone(&self.state);
        let tx = self.runtime_tx.clone();
        self.state.supervisor.spawn_turn(async move {
            let result = agent_loop::list_sessions(&state.turn_dependencies()).await;
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
        let state = Arc::clone(&self.state);
        let tx = self.runtime_tx.clone();
        self.state.supervisor.spawn_turn(async move {
            let result = load_context(&state, startup).await;
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
                self.transcript.apply_agent_event(event);
            }
            RuntimeEvent::TurnFinished(result) => {
                match result {
                    Ok(response) if self.transcript.active().is_some() => {
                        self.transcript
                            .apply_agent_event(AgentEvent::FinalResponse { text: response });
                    }
                    Err(error) if self.transcript.active().is_some() => {
                        self.transcript.apply_agent_event(AgentEvent::Error {
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
            RuntimeEvent::SlashFinished { prompt, outcome } => {
                let handled = !matches!(&outcome, SlashCommandOutcome::NotHandled);
                match outcome {
                    SlashCommandOutcome::Respond(response) => {
                        if is_new_command(&prompt) {
                            self.transcript.clear();
                        }
                        self.transcript.begin_turn(prompt);
                        self.transcript
                            .apply_agent_event(AgentEvent::FinalResponse { text: response });
                    }
                    SlashCommandOutcome::Error(message) => {
                        self.transcript.begin_turn(prompt);
                        self.transcript
                            .apply_agent_event(AgentEvent::Error { message });
                    }
                    SlashCommandOutcome::NotHandled => self.start_agent_turn(prompt),
                }
                if handled {
                    self.busy = false;
                    self.model = model_for(&self.state, &self.context);
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
                        self.context = loaded.context;
                        self.model = model_for(&self.state, &self.context);
                        self.transcript = Transcript::from_messages(&loaded.messages);
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

/// Starts the inline TUI.
pub(crate) async fn run(
    state: Arc<AppState>,
    requested_session: Option<&str>,
) -> Result<(), EgoPulseError> {
    let sessions = agent_loop::list_sessions(&state.turn_dependencies()).await?;
    let startup = choose_startup_session(requested_session, &sessions);
    let loaded = load_context(&state, startup).await?;
    let mut terminal = TuiSession::new()?;
    let (runtime_tx, runtime_rx) = unbounded_channel();
    let mut app = TuiApp::new(state.clone(), loaded, runtime_tx, runtime_rx);
    app.insert_pending(&mut terminal)?;

    let mut event_stream = crossterm::event::EventStream::new();
    let shutdown = state.supervisor.shutdown_token();
    let mut redraw_tick = tokio::time::interval(MAX_FRAME_INTERVAL);
    let mut dirty = true;
    let mut critical_failure = None;

    loop {
        tokio::select! {
            _ = redraw_tick.tick() => {
                if let Some(outcome) = state.supervisor.poll_long_lived() {
                    let summary = outcome.failure_summary();
                    state.runtime_status.record_critical_task_failure(&summary);
                    tracing::warn!(task = %outcome.name(), result = ?outcome.result(), %summary, "critical task exited; initiating TUI shutdown");
                    critical_failure = Some(summary);
                    break;
                }
                if dirty {
                    app.draw(&mut terminal)?;
                    dirty = false;
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
            _ = shutdown.cancelled() => break,
        }
    }

    state.supervisor.shutdown().await;
    if let Some(message) = critical_failure {
        return Err(EgoPulseError::Internal(message));
    }
    Ok(())
}

async fn load_context(
    state: &AppState,
    startup: StartupSession,
) -> Result<LoadedContext, EgoPulseError> {
    let context = match startup {
        StartupSession::Existing(summary) => {
            let chat_info = call_blocking(Arc::clone(&state.db), {
                let chat_id = summary.chat_id;
                move |db| db.get_chat_by_id(chat_id)
            })
            .await?
            .ok_or_else(|| EgoPulseError::Internal("session chat was not found".to_string()))?;
            let agent_id = if summary.agent_id.is_empty() {
                state.current_config().default_agent.to_string()
            } else {
                summary.agent_id.clone()
            };
            SurfaceContext::new(
                chat_info.channel,
                "local_user".to_string(),
                persisted_thread(&summary),
                chat_info.chat_type,
                agent_id,
            )
        }
        StartupSession::Named(name) => new_context(state, name),
        StartupSession::New => new_context(state, new_session_name()),
    };
    let messages = agent_loop::load_session_messages(&state.turn_dependencies(), &context).await?;
    Ok(LoadedContext { context, messages })
}

fn new_context(state: &AppState, session: String) -> SurfaceContext {
    SurfaceContext::new(
        "tui".to_string(),
        "local_user".to_string(),
        session,
        "tui".to_string(),
        state.current_config().default_agent.to_string(),
    )
}

fn persisted_thread(summary: &SessionSummary) -> String {
    summary.surface_thread.clone()
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

fn model_for(state: &AppState, context: &SurfaceContext) -> String {
    let config = state.current_config();
    config
        .resolve_llm_for_agent_channel(&AgentId::new(&context.agent_id), &context.channel)
        .map(|resolved| resolved.model)
        .unwrap_or_else(|_| config.resolve_global_llm().model)
}

fn new_session_name() -> String {
    format!("local-{}", uuid::Uuid::new_v4())
}

fn is_new_command(prompt: &str) -> bool {
    prompt.split_whitespace().next() == Some("/new")
}

fn agent_status(event: &AgentEvent) -> String {
    match event {
        AgentEvent::Iteration { iteration } => format!("Iteration {iteration}"),
        AgentEvent::Delta { .. } => "Streaming response…".to_string(),
        AgentEvent::ToolStart { name, .. } => format!("Running {name}…"),
        AgentEvent::ToolResult {
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
        AgentEvent::FinalResponse { .. } => "Ready".to_string(),
        AgentEvent::Error { .. } => "Turn failed".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::completion_candidates;

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
}
