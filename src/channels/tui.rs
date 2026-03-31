use std::io::{self, Stdout};
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use uuid::Uuid;

use crate::agent_loop::SurfaceContext;
use crate::error::{EgoPulseError, TuiError};
use crate::runtime;
use crate::runtime::AppState;
use crate::storage::SessionSummary;

struct TuiSession {
    terminal: Terminal<CrosstermBackend<Stdout>>,
}

impl TuiSession {
    fn new() -> Result<Self, TuiError> {
        enable_raw_mode().map_err(|error| TuiError::InitFailed(error.to_string()))?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen)
            .map_err(|error| TuiError::InitFailed(error.to_string()))?;
        let terminal = Terminal::new(CrosstermBackend::new(stdout))
            .map_err(|error| TuiError::InitFailed(error.to_string()))?;
        Ok(Self { terminal })
    }
}

impl Drop for TuiSession {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum View {
    Browser,
    Chat(ChatState),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ChatState {
    context: SurfaceContext,
    input: String,
    status: String,
    messages: Vec<RenderedMessage>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RenderedMessage {
    role: String,
    content: String,
}

struct TuiApp {
    state: AppState,
    sessions: Vec<SessionSummary>,
    selected: usize,
    view: View,
    status: String,
}

impl TuiApp {
    fn new(state: AppState, sessions: Vec<SessionSummary>) -> Self {
        Self {
            state,
            sessions,
            selected: 0,
            view: View::Browser,
            status: "j/k or arrows to move, Enter to open, n or /new for a session".to_string(),
        }
    }

    fn browser_status(&self) -> String {
        if self.sessions.is_empty() {
            "No sessions yet. Press n or /new to create one.".to_string()
        } else {
            format!(
                "{} sessions | selected {}/{} | Enter open | n /new | r /refresh | q /quit",
                self.sessions.len(),
                self.selected.saturating_add(1),
                self.sessions.len()
            )
        }
    }

    fn browser_help(&self) -> String {
        "j/k or arrows, Ctrl-N/P, g/G, PgUp/PgDn, Enter open, n /new, r /refresh, q /quit"
            .to_string()
    }

    fn chat_status() -> String {
        "Enter to send, Esc to go back, /help for commands".to_string()
    }

    fn move_selection(&mut self, delta: isize) {
        if self.sessions.is_empty() {
            self.selected = 0;
            return;
        }
        let len = self.sessions.len() as isize;
        let next = (self.selected as isize + delta).clamp(0, len.saturating_sub(1));
        self.selected = next as usize;
    }

    fn select_first(&mut self) {
        if !self.sessions.is_empty() {
            self.selected = 0;
        }
    }

    fn select_last(&mut self) {
        if !self.sessions.is_empty() {
            self.selected = self.sessions.len().saturating_sub(1);
        }
    }

    async fn refresh_sessions(&mut self) -> Result<(), EgoPulseError> {
        self.sessions = runtime::list_sessions(&self.state).await?;
        if self.sessions.is_empty() {
            self.selected = 0;
        } else {
            self.selected = self.selected.min(self.sessions.len().saturating_sub(1));
        }
        self.status = self.browser_status();
        Ok(())
    }

    async fn open_selected_session(&mut self) -> Result<(), EgoPulseError> {
        let Some(summary) = self.sessions.get(self.selected).cloned() else {
            self.status = "No session selected".to_string();
            return Ok(());
        };
        self.open_session(summary).await
    }

    async fn open_new_session(&mut self) -> Result<(), EgoPulseError> {
        let session_id = format!("local-{}", short_uuid());
        let context = SurfaceContext {
            channel: "tui".to_string(),
            surface_user: "local_user".to_string(),
            surface_thread: session_id,
            chat_type: "tui".to_string(),
        };
        let messages = runtime::load_session_messages(&self.state, &context).await?;
        self.view = View::Chat(ChatState {
            context,
            input: String::new(),
            status: Self::chat_status(),
            messages: messages
                .into_iter()
                .map(|message| RenderedMessage {
                    role: message.role,
                    content: message.content,
                })
                .collect(),
        });
        Ok(())
    }

    async fn open_session(&mut self, summary: SessionSummary) -> Result<(), EgoPulseError> {
        let context = SurfaceContext {
            channel: summary.channel.clone(),
            surface_user: "local_user".to_string(),
            surface_thread: summary.surface_thread.clone(),
            chat_type: summary.channel,
        };
        let messages = runtime::load_session_messages(&self.state, &context).await?;
        self.view = View::Chat(ChatState {
            context,
            input: String::new(),
            status: Self::chat_status(),
            messages: messages
                .into_iter()
                .map(|message| RenderedMessage {
                    role: message.role,
                    content: message.content,
                })
                .collect(),
        });
        Ok(())
    }
}

pub async fn run(state: AppState) -> Result<(), EgoPulseError> {
    let sessions = runtime::list_sessions(&state).await?;
    let mut app = TuiApp::new(state, sessions);
    if app.sessions.is_empty() {
        app.status = "No sessions yet. Press n to create one.".to_string();
    }

    let mut session = TuiSession::new()?;
    run_loop(&mut session.terminal, &mut app).await
}

async fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    app: &mut TuiApp,
) -> Result<(), EgoPulseError> {
    loop {
        terminal
            .draw(|frame| draw(frame, app))
            .map_err(|error| TuiError::RenderFailed(error.to_string()))?;

        if event::poll(Duration::from_millis(200))
            .map_err(|error| TuiError::EventFailed(error.to_string()))?
        {
            let Event::Key(key) =
                event::read().map_err(|error| TuiError::EventFailed(error.to_string()))?
            else {
                continue;
            };

            if key.kind != KeyEventKind::Press {
                continue;
            }

            if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
                return Ok(());
            }

            let mut next_action: Option<PendingAction> = None;
            match &mut app.view {
                View::Browser => match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                    KeyCode::Char('r') => next_action = Some(PendingAction::RefreshSessions),
                    KeyCode::Char('n') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        app.move_selection(5);
                        app.status = app.browser_status();
                    }
                    KeyCode::Char('p') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        app.move_selection(-5);
                        app.status = app.browser_status();
                    }
                    KeyCode::Char('n') => next_action = Some(PendingAction::NewSession),
                    KeyCode::Enter => next_action = Some(PendingAction::OpenSelected),
                    KeyCode::Char('j') | KeyCode::Down => {
                        app.move_selection(1);
                        app.status = app.browser_status();
                    }
                    KeyCode::Char('k') | KeyCode::Up => {
                        app.move_selection(-1);
                        app.status = app.browser_status();
                    }
                    KeyCode::Char('g') => {
                        app.select_first();
                        app.status = app.browser_status();
                    }
                    KeyCode::Char('G') => {
                        app.select_last();
                        app.status = app.browser_status();
                    }
                    KeyCode::PageDown => {
                        app.move_selection(5);
                        app.status = app.browser_status();
                    }
                    KeyCode::PageUp => {
                        app.move_selection(-5);
                        app.status = app.browser_status();
                    }
                    _ => {}
                },
                View::Chat(chat) => match key.code {
                    KeyCode::Esc => next_action = Some(PendingAction::GoBrowser),
                    KeyCode::Backspace => {
                        chat.input.pop();
                    }
                    KeyCode::Enter => {
                        let raw_input = chat.input.trim().to_string();
                        if let Some(command) = parse_chat_command(&raw_input) {
                            chat.input.clear();
                            next_action = Some(PendingAction::ChatCommand(command));
                        } else if !raw_input.is_empty() {
                            next_action = Some(PendingAction::SendMessage(raw_input));
                            chat.input.clear();
                        }
                    }
                    KeyCode::Char(c) => {
                        if !key.modifiers.contains(KeyModifiers::CONTROL) {
                            chat.input.push(c);
                        }
                    }
                    _ => {}
                },
            }

            if let Some(action) = next_action {
                match action {
                    PendingAction::RefreshSessions => {
                        app.refresh_sessions().await?;
                    }
                    PendingAction::NewSession => {
                        app.open_new_session().await?;
                    }
                    PendingAction::OpenSelected => {
                        app.open_selected_session().await?;
                    }
                    PendingAction::GoBrowser => {
                        app.refresh_sessions().await?;
                        app.view = View::Browser;
                        app.status = app.browser_status();
                    }
                    PendingAction::SendMessage(prompt) => {
                        let response = send_chat_message(app, prompt).await?;
                        if let View::Chat(chat) = &mut app.view {
                            chat.messages.push(RenderedMessage {
                                role: "assistant".to_string(),
                                content: response,
                            });
                            chat.status = "Message sent".to_string();
                        }
                        app.refresh_sessions().await?;
                    }
                    PendingAction::ChatCommand(command) => {
                        handle_chat_command(app, command).await?;
                    }
                }
            }
        }
    }
}

async fn send_chat_message(app: &mut TuiApp, prompt: String) -> Result<String, EgoPulseError> {
    let context = match &app.view {
        View::Chat(chat) => chat.context.clone(),
        View::Browser => {
            return Err(EgoPulseError::Tui(TuiError::EventFailed(
                "chat view missing".to_string(),
            )));
        }
    };

    if let View::Chat(chat) = &mut app.view {
        chat.messages.push(RenderedMessage {
            role: "user".to_string(),
            content: prompt.clone(),
        });
        chat.status = "Sending...".to_string();
    }

    let response = runtime::send_turn(&app.state, &context, &prompt).await?;
    Ok(response)
}

fn draw(frame: &mut ratatui::Frame<'_>, app: &TuiApp) {
    match &app.view {
        View::Browser => draw_browser(frame, app),
        View::Chat(chat) => draw_chat(frame, app, chat),
    }
}

fn draw_browser(frame: &mut ratatui::Frame<'_>, app: &TuiApp) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(5),
            Constraint::Min(5),
            Constraint::Length(3),
        ])
        .split(frame.area());

    let header = Paragraph::new(vec![
        Line::from(vec![
            Span::styled(
                "EgoPulse",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("  local TUI"),
        ]),
        Line::from(format!("status: {}", app.status)),
        Line::from(format!(
            "sessions: {}  selected: {}",
            app.sessions.len(),
            if app.sessions.is_empty() {
                0
            } else {
                app.selected.saturating_add(1)
            }
        )),
    ])
    .block(Block::default().title("Browser").borders(Borders::ALL))
    .wrap(Wrap { trim: true });
    frame.render_widget(header, chunks[0]);

    let body_lines = if app.sessions.is_empty() {
        vec![Line::from("No sessions yet. Press n to create one.")]
    } else {
        app.sessions
            .iter()
            .enumerate()
            .map(|(index, session)| {
                let prefix = if index == app.selected { ">" } else { " " };
                let title = session
                    .chat_title
                    .as_deref()
                    .unwrap_or(session.surface_thread.as_str());
                let preview = session
                    .last_message_preview
                    .as_deref()
                    .map(truncate_preview)
                    .unwrap_or_else(|| "(empty)".to_string());
                Line::from(vec![
                    Span::raw(prefix),
                    Span::styled(
                        format!(" {} / {}", session.channel, session.surface_thread),
                        Style::default().fg(Color::Yellow),
                    ),
                    Span::raw("  "),
                    Span::styled(title, Style::default().add_modifier(Modifier::BOLD)),
                    Span::raw("  "),
                    Span::styled(preview, Style::default().fg(Color::Gray)),
                ])
            })
            .collect()
    };
    let body = Paragraph::new(body_lines)
        .block(Block::default().title("Sessions").borders(Borders::ALL))
        .wrap(Wrap { trim: true });
    frame.render_widget(body, chunks[1]);

    let footer = Paragraph::new(vec![
        Line::from(vec![
            Span::styled("j/k", Style::default().fg(Color::Green)),
            Span::raw(" or "),
            Span::styled("↑/↓", Style::default().fg(Color::Green)),
            Span::raw(" move"),
            Span::raw("  "),
            Span::styled("Ctrl-N/P", Style::default().fg(Color::Green)),
            Span::raw(" page"),
            Span::raw("  "),
            Span::styled("g/G", Style::default().fg(Color::Green)),
            Span::raw(" top/bottom"),
            Span::raw("  "),
            Span::styled("Enter", Style::default().fg(Color::Green)),
            Span::raw(" open"),
        ]),
        Line::from(app.browser_help()),
    ])
    .block(Block::default().title("Controls").borders(Borders::ALL));
    frame.render_widget(footer, chunks[2]);
}

fn draw_chat(frame: &mut ratatui::Frame<'_>, app: &TuiApp, chat: &ChatState) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(4),
            Constraint::Min(5),
            Constraint::Length(3),
        ])
        .split(frame.area());

    let header = Paragraph::new(vec![
        Line::from(vec![
            Span::styled(
                "Session",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(": "),
            Span::styled(
                session_key(&chat.context),
                Style::default().fg(Color::Yellow),
            ),
        ]),
        Line::from(format!("status: {}", chat.status)),
        Line::from(format!("model: {}", app.state.config.model)),
    ])
    .block(Block::default().title("Chat").borders(Borders::ALL))
    .wrap(Wrap { trim: true });
    frame.render_widget(header, chunks[0]);

    let mut lines = Vec::new();
    for message in &chat.messages {
        let prefix = if message.role == "assistant" {
            "assistant"
        } else {
            "you"
        };
        lines.push(Line::from(vec![
            Span::styled(
                format!("{prefix}: "),
                Style::default().fg(if message.role == "assistant" {
                    Color::Cyan
                } else {
                    Color::Green
                }),
            ),
            Span::raw(&message.content),
        ]));
    }
    if lines.is_empty() {
        lines.push(Line::from(
            "No messages yet. Type something and press Enter.",
        ));
    }
    let body = Paragraph::new(Text::from(lines))
        .block(Block::default().title("Conversation").borders(Borders::ALL))
        .wrap(Wrap { trim: false });
    frame.render_widget(body, chunks[1]);

    let footer = Paragraph::new(vec![Line::from(vec![
        Span::styled("Esc", Style::default().fg(Color::Green)),
        Span::raw(" back"),
        Span::raw("  "),
        Span::styled("Enter", Style::default().fg(Color::Green)),
        Span::raw(" send"),
        Span::raw("  "),
        Span::styled("Ctrl-C", Style::default().fg(Color::Green)),
        Span::raw(" quit"),
        Span::raw("  "),
        Span::styled("/", Style::default().fg(Color::Green)),
        Span::raw(" commands"),
        Span::raw("  "),
        Span::raw("input: "),
        Span::styled(&chat.input, Style::default().fg(Color::Yellow)),
    ])])
    .block(Block::default().title("Input").borders(Borders::ALL));
    frame.render_widget(footer, chunks[2]);
}

fn parse_chat_command(input: &str) -> Option<ChatCommand> {
    match input {
        "/new" => Some(ChatCommand::New),
        "/browser" => Some(ChatCommand::Browser),
        "/refresh" => Some(ChatCommand::Refresh),
        "/quit" => Some(ChatCommand::Quit),
        "/help" => Some(ChatCommand::Help),
        _ => None,
    }
}

async fn handle_chat_command(app: &mut TuiApp, command: ChatCommand) -> Result<(), EgoPulseError> {
    match command {
        ChatCommand::New => {
            app.open_new_session().await?;
        }
        ChatCommand::Browser => {
            app.refresh_sessions().await?;
            app.view = View::Browser;
            app.status = app.browser_status();
        }
        ChatCommand::Refresh => {
            let context = match &app.view {
                View::Chat(chat) => chat.context.clone(),
                View::Browser => {
                    app.refresh_sessions().await?;
                    return Ok(());
                }
            };
            let messages = runtime::load_session_messages(&app.state, &context).await?;
            if let View::Chat(chat) = &mut app.view {
                chat.messages = messages
                    .into_iter()
                    .map(|message| RenderedMessage {
                        role: message.role,
                        content: message.content,
                    })
                    .collect();
                chat.status = "Refreshed chat messages".to_string();
            }
            app.refresh_sessions().await?;
        }
        ChatCommand::Quit => return Err(EgoPulseError::ShutdownRequested),
        ChatCommand::Help => {
            if let View::Chat(chat) = &mut app.view {
                chat.status = "Commands: /new /browser /refresh /quit /help".to_string();
            }
        }
    }
    Ok(())
}

#[derive(Debug, Clone)]
enum PendingAction {
    RefreshSessions,
    NewSession,
    OpenSelected,
    GoBrowser,
    SendMessage(String),
    ChatCommand(ChatCommand),
}

#[derive(Debug, Clone)]
enum ChatCommand {
    New,
    Browser,
    Refresh,
    Quit,
    Help,
}

fn session_key(context: &SurfaceContext) -> String {
    format!("{}:{}", context.channel, context.surface_thread)
}

fn short_uuid() -> String {
    Uuid::new_v4()
        .simple()
        .to_string()
        .chars()
        .take(8)
        .collect()
}

fn truncate_preview(value: &str) -> String {
    const MAX_LEN: usize = 60;
    let mut preview = value.chars().take(MAX_LEN).collect::<String>();
    if value.chars().count() > MAX_LEN {
        preview.push_str("...");
    }
    preview
}
