use std::io::{self, Stdout};
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Position;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use tokio::task::JoinHandle;
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
        if let Err(error) = execute!(stdout, EnterAlternateScreen) {
            let _ = disable_raw_mode();
            return Err(TuiError::InitFailed(error.to_string()));
        }
        let terminal = match Terminal::new(CrosstermBackend::new(stdout)) {
            Ok(terminal) => terminal,
            Err(error) => {
                let _ = execute!(io::stdout(), LeaveAlternateScreen);
                let _ = disable_raw_mode();
                return Err(TuiError::InitFailed(error.to_string()));
            }
        };
        Ok(Self { terminal })
    }
}

impl Drop for TuiSession {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
    }
}

enum View {
    Browser,
    Chat(Box<ChatState>),
}

struct ChatState {
    context: SurfaceContext,
    input: String,
    input_cursor: usize,
    input_history: Vec<String>,
    history_index: Option<usize>,
    draft_input: Option<String>,
    status: String,
    messages: Vec<RenderedMessage>,
    pending_send: Option<PendingSend>,
    conversation_scroll: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RenderedMessage {
    role: String,
    content: String,
}

struct PendingSend {
    prompt: String,
    handle: JoinHandle<Result<String, EgoPulseError>>,
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
        "Enter to send | Esc back | /help for commands".to_string()
    }

    fn chat_help() -> String {
        "/new start fresh, /browser go back, /refresh reload, /quit exit, /help show commands"
            .to_string()
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
        self.view = View::Chat(Box::new(ChatState {
            context,
            input: String::new(),
            input_cursor: 0,
            input_history: Vec::new(),
            history_index: None,
            draft_input: None,
            status: Self::chat_status(),
            messages: messages
                .into_iter()
                .map(|message| RenderedMessage {
                    role: message.role,
                    content: message.content,
                })
                .collect(),
            pending_send: None,
            conversation_scroll: 0,
        }));
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
        self.view = View::Chat(Box::new(ChatState {
            context,
            input: String::new(),
            input_cursor: 0,
            input_history: Vec::new(),
            history_index: None,
            draft_input: None,
            status: Self::chat_status(),
            messages: messages
                .into_iter()
                .map(|message| RenderedMessage {
                    role: message.role,
                    content: message.content,
                })
                .collect(),
            pending_send: None,
            conversation_scroll: 0,
        }));
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
        poll_pending_send(app).await;
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
                        backspace_input(chat);
                    }
                    KeyCode::Delete => {
                        delete_input(chat);
                    }
                    KeyCode::Left => {
                        chat.input_cursor = chat.input_cursor.saturating_sub(1);
                    }
                    KeyCode::Right => {
                        chat.input_cursor = (chat.input_cursor + 1).min(chat.input.chars().count());
                    }
                    KeyCode::Up => {
                        handle_up_arrow(chat);
                    }
                    KeyCode::Down => {
                        if chat.history_index.is_some() {
                            handle_down_arrow(chat);
                        } else {
                            chat.conversation_scroll = chat.conversation_scroll.saturating_sub(1);
                        }
                    }
                    KeyCode::Enter => {
                        if chat.pending_send.is_some() {
                            chat.status = "A request is already in progress".to_string();
                            continue;
                        }
                        let raw_input = chat.input.trim().to_string();
                        if let Some(parsed) = parse_chat_input(&raw_input) {
                            if !raw_input.is_empty() {
                                push_input_history(chat, raw_input.clone());
                            }
                            chat.input.clear();
                            chat.input_cursor = 0;
                            chat.history_index = None;
                            chat.draft_input = None;
                            next_action = Some(match parsed {
                                ParsedChatInput::Message(message) => {
                                    PendingAction::SendMessage(message)
                                }
                                ParsedChatInput::Command(command) => {
                                    PendingAction::ChatCommand(command)
                                }
                                ParsedChatInput::UnknownCommand(command) => {
                                    PendingAction::UnknownCommand(command)
                                }
                            });
                        }
                    }
                    KeyCode::Char(c) => {
                        if !key.modifiers.contains(KeyModifiers::CONTROL) {
                            insert_input_char(chat, c);
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
                        start_send(app, prompt);
                    }
                    PendingAction::ChatCommand(command) => {
                        handle_chat_command(app, command).await?;
                    }
                    PendingAction::UnknownCommand(command) => {
                        if let View::Chat(chat) = &mut app.view {
                            chat.status = format!(
                                "Unknown command: {command}. Try /help for available commands."
                            );
                        }
                    }
                }
            }
        }
    }
}

fn start_send(app: &mut TuiApp, prompt: String) {
    let View::Chat(chat) = &mut app.view else {
        return;
    };

    let context = chat.context.clone();
    let state = app.state.clone();
    chat.messages.push(RenderedMessage {
        role: "user".to_string(),
        content: prompt.clone(),
    });
    chat.status = "Sending...".to_string();
    chat.conversation_scroll = 0;
    let send_prompt = prompt.clone();
    let handle =
        tokio::spawn(async move { runtime::send_turn(&state, &context, &send_prompt).await });
    chat.pending_send = Some(PendingSend { prompt, handle });
}

async fn poll_pending_send(app: &mut TuiApp) {
    let Some((prompt, handle)) = take_finished_send(app) else {
        return;
    };

    let result = match handle.await {
        Ok(result) => result,
        Err(error) => Err(EgoPulseError::Tui(TuiError::EventFailed(error.to_string()))),
    };

    match result {
        Ok(response) => {
            if let View::Chat(chat) = &mut app.view {
                chat.messages.push(RenderedMessage {
                    role: "assistant".to_string(),
                    content: response,
                });
                chat.status = "Message sent".to_string();
                chat.conversation_scroll = 0;
            }
            if let Err(error) = app.refresh_sessions().await
                && let View::Chat(chat) = &mut app.view
            {
                chat.status = format!("Message sent, but refresh failed: {error}");
            }
        }
        Err(error) => {
            if let View::Chat(chat) = &mut app.view {
                chat.status = format!("Send failed: {error}");
                if chat
                    .messages
                    .last()
                    .is_some_and(|message| message.role == "user" && message.content == prompt)
                {
                    chat.messages.pop();
                }
                if chat.input.is_empty() {
                    chat.input = prompt;
                    chat.input_cursor = chat.input.chars().count();
                }
            }
        }
    }
}

fn take_finished_send(
    app: &mut TuiApp,
) -> Option<(String, JoinHandle<Result<String, EgoPulseError>>)> {
    let View::Chat(chat) = &mut app.view else {
        return None;
    };
    let finished = chat
        .pending_send
        .as_ref()
        .is_some_and(|pending| pending.handle.is_finished());
    if !finished {
        return None;
    }
    let pending = chat.pending_send.take()?;
    Some((pending.prompt, pending.handle))
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
            Constraint::Length(4),
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
            Constraint::Length(5),
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
    let visible_line_count = chunks[1].height.saturating_sub(2) as usize;
    let start_index = lines
        .len()
        .saturating_sub(visible_line_count.saturating_add(chat.conversation_scroll));
    let end_index = if chat.conversation_scroll == 0 {
        lines.len()
    } else {
        lines.len().saturating_sub(chat.conversation_scroll)
    };
    let body = Paragraph::new(Text::from(lines[start_index..end_index].to_vec()))
        .block(Block::default().title("Conversation").borders(Borders::ALL))
        .wrap(Wrap { trim: false });
    frame.render_widget(body, chunks[1]);

    let footer = Paragraph::new(vec![
        Line::from(vec![
            Span::styled("Esc", Style::default().fg(Color::Green)),
            Span::raw(" back"),
            Span::raw("  "),
            Span::styled("Enter", Style::default().fg(Color::Green)),
            Span::raw(" send"),
            Span::raw("  "),
            Span::styled("Ctrl-C", Style::default().fg(Color::Green)),
            Span::raw(" quit"),
        ]),
        Line::from(vec![
            Span::raw("slash: "),
            Span::styled("/new", Style::default().fg(Color::Green)),
            Span::raw(" "),
            Span::styled("/browser", Style::default().fg(Color::Green)),
            Span::raw(" "),
            Span::styled("/refresh", Style::default().fg(Color::Green)),
            Span::raw(" "),
            Span::styled("/quit", Style::default().fg(Color::Green)),
            Span::raw(" "),
            Span::styled("/help", Style::default().fg(Color::Green)),
        ]),
        Line::from(vec![
            Span::raw("input: "),
            Span::styled(&chat.input, Style::default().fg(Color::Yellow)),
        ]),
    ])
    .block(Block::default().title("Input").borders(Borders::ALL));
    frame.render_widget(footer, chunks[2]);

    let input_x = chunks[2].x.saturating_add(9 + chat.input_cursor as u16);
    let max_x = chunks[2].x + chunks[2].width.saturating_sub(2);
    let cursor_x = input_x.min(max_x);
    let cursor_y = chunks[2].y.saturating_add(3);
    frame.set_cursor_position(Position::new(cursor_x, cursor_y));
}

fn insert_input_char(chat: &mut ChatState, value: char) {
    let byte_index = char_to_byte_index(&chat.input, chat.input_cursor);
    chat.input.insert(byte_index, value);
    chat.input_cursor += 1;
    chat.history_index = None;
    chat.draft_input = None;
}

fn backspace_input(chat: &mut ChatState) {
    if chat.input_cursor == 0 {
        return;
    }
    let end = char_to_byte_index(&chat.input, chat.input_cursor);
    let start = char_to_byte_index(&chat.input, chat.input_cursor - 1);
    chat.input.replace_range(start..end, "");
    chat.input_cursor -= 1;
}

fn delete_input(chat: &mut ChatState) {
    if chat.input_cursor >= chat.input.chars().count() {
        return;
    }
    let start = char_to_byte_index(&chat.input, chat.input_cursor);
    let end = char_to_byte_index(&chat.input, chat.input_cursor + 1);
    chat.input.replace_range(start..end, "");
}

fn handle_up_arrow(chat: &mut ChatState) {
    if chat.input_cursor > 0 {
        chat.input_cursor = 0;
        return;
    }

    if chat.input_history.is_empty() {
        return;
    }

    let next_index = match chat.history_index {
        Some(index) => index.saturating_sub(1),
        None => {
            chat.draft_input = Some(chat.input.clone());
            chat.input_history.len().saturating_sub(1)
        }
    };
    chat.history_index = Some(next_index);
    chat.input = chat.input_history[next_index].clone();
    chat.input_cursor = 0;
}

fn handle_down_arrow(chat: &mut ChatState) {
    let Some(index) = chat.history_index else {
        return;
    };

    if index + 1 < chat.input_history.len() {
        let next_index = index + 1;
        chat.history_index = Some(next_index);
        chat.input = chat.input_history[next_index].clone();
        chat.input_cursor = 0;
        return;
    }

    chat.history_index = None;
    chat.input = chat.draft_input.take().unwrap_or_default();
    chat.input_cursor = chat.input.chars().count();
}

fn push_input_history(chat: &mut ChatState, raw_input: String) {
    if chat.input_history.last() == Some(&raw_input) {
        return;
    }
    chat.input_history.push(raw_input);
    if chat.input_history.len() > 50 {
        let overflow = chat.input_history.len() - 50;
        chat.input_history.drain(0..overflow);
    }
}

fn char_to_byte_index(value: &str, char_index: usize) -> usize {
    value
        .char_indices()
        .nth(char_index)
        .map(|(index, _)| index)
        .unwrap_or(value.len())
}

fn parse_chat_input(input: &str) -> Option<ParsedChatInput> {
    if input.is_empty() {
        return None;
    }

    if let Some(command) = match input {
        "/new" => Some(ChatCommand::New),
        "/browser" => Some(ChatCommand::Browser),
        "/refresh" => Some(ChatCommand::Refresh),
        "/quit" => Some(ChatCommand::Quit),
        "/help" => Some(ChatCommand::Help),
        _ => None,
    } {
        return Some(ParsedChatInput::Command(command));
    }

    if input.starts_with('/') {
        return Some(ParsedChatInput::UnknownCommand(input.to_string()));
    }

    Some(ParsedChatInput::Message(input.to_string()))
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
                chat.status = TuiApp::chat_help();
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
    UnknownCommand(String),
}

#[derive(Debug, Clone)]
enum ChatCommand {
    New,
    Browser,
    Refresh,
    Quit,
    Help,
}

#[derive(Debug, Clone)]
enum ParsedChatInput {
    Message(String),
    Command(ChatCommand),
    UnknownCommand(String),
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
