use std::io::{self, Stdout};
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use crate::config::Config;
use crate::error::{EgoPulseError, TuiError};

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

struct TuiApp {
    model: String,
    base_url: String,
    data_dir: String,
    status: String,
}

impl TuiApp {
    fn from_config(config: &Config) -> Self {
        Self {
            model: config.model.clone(),
            base_url: config.llm_base_url.clone(),
            data_dir: config.data_dir.clone(),
            status: "Press q or Esc to exit".to_string(),
        }
    }
}

pub async fn run(config: &Config) -> Result<(), EgoPulseError> {
    let mut session = TuiSession::new()?;
    let app = TuiApp::from_config(config);
    let result = run_loop(&mut session.terminal, app);
    result.map_err(EgoPulseError::from)
}

fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    app: TuiApp,
) -> Result<(), TuiError> {
    loop {
        terminal
            .draw(|frame| {
                let chunks = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([
                        Constraint::Length(5),
                        Constraint::Min(5),
                        Constraint::Length(3),
                    ])
                    .split(frame.area());
                let header = chunks[0];
                let body = chunks[1];
                let footer = chunks[2];

                let header_text = vec![
                    Line::from(vec![
                        Span::styled(
                            "EgoPulse",
                            Style::default()
                                .fg(Color::Cyan)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::raw("  local TUI scaffold"),
                    ]),
                    Line::from(format!("model: {}", app.model)),
                    Line::from(format!("base_url: {}", app.base_url)),
                    Line::from(format!("data_dir: {}", app.data_dir)),
                ];
                let header_widget = Paragraph::new(header_text)
                    .block(Block::default().title("Status").borders(Borders::ALL))
                    .wrap(Wrap { trim: true });
                frame.render_widget(header_widget, header);

                let body_widget = Paragraph::new(vec![
                    Line::from("This is the thin local surface for Issue 2.5."),
                    Line::from("The persistent agent core stays unchanged."),
                    Line::from(
                        "Next slice will wire session browsing and resume into this surface.",
                    ),
                ])
                .block(Block::default().title("Workspace").borders(Borders::ALL))
                .wrap(Wrap { trim: true });
                frame.render_widget(body_widget, body);

                let footer_widget = Paragraph::new(vec![Line::from(vec![
                    Span::styled(
                        "q",
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(" / "),
                    Span::styled(
                        "Esc",
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(" quit"),
                    Span::raw("  "),
                    Span::styled("config", Style::default().fg(Color::Green)),
                    Span::raw(": egopulse.toml auto-discovery only"),
                    Span::raw("  "),
                    Span::styled("status", Style::default().fg(Color::Green)),
                    Span::raw(": "),
                    Span::raw(&app.status),
                ])])
                .block(Block::default().title("Controls").borders(Borders::ALL));
                frame.render_widget(footer_widget, footer);
            })
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

            match key.code {
                KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                _ => {}
            }
        }
    }
}
