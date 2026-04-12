//! 対話型セットアップウィザード。
//!
//! Ratatui ベースのローカル UI で設定値を収集し、既存 YAML を必要最小限だけ保ちながら
//! `egopulse.config.yaml` を生成・更新する。

mod channels;
mod provider;
mod summary;

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::layout::{Constraint, Direction, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::{Terminal, backend::CrosstermBackend};

use crate::config::default_config_path;

pub(crate) use channels::*;
pub(crate) use provider::*;
pub(crate) use summary::*;

#[derive(Clone)]
pub(crate) struct Field {
    pub key: String,
    pub label: String,
    pub value: String,
    pub required: bool,
    pub secret: bool,
    pub help: Option<String>,
}

pub(crate) enum SetupMode {
    Navigate,
    Edit,
    Selector(SelectorState),
}

pub(crate) struct SelectorState {
    pub field_key: String,
    pub filter: String,
    pub items: Vec<SelectorItem>,
    pub selected: usize,
    pub original_value: String,
}

pub(crate) struct SelectorItem {
    pub display: String,
    pub value: String,
}

impl Field {
    fn display_value(&self, editing: bool) -> String {
        if editing || !self.secret {
            return self.value.clone();
        }
        if self.value.is_empty() {
            String::new()
        } else {
            mask_secret(&self.value)
        }
    }
}

pub(crate) struct SetupApp {
    pub fields: Vec<Field>,
    pub selected: usize,
    pub mode: SetupMode,
    pub status: String,
    pub completed: bool,
    pub backup_path: Option<String>,
    pub completion_summary: Vec<String>,
    pub config_path: PathBuf,
    pub original_yaml: Option<serde_yml::Value>,
}

impl SetupApp {
    fn new(config_path: Option<PathBuf>) -> Result<Self, String> {
        let config_path = match config_path {
            Some(path) => path,
            None => default_config_path().map_err(|e| e.to_string())?,
        };
        let (existing, original_yaml) = Self::load_existing_config(&config_path);
        let provider_id = existing
            .get("PROVIDER")
            .cloned()
            .unwrap_or_else(|| "openai".into());
        let provider_model = existing
            .get("MODEL")
            .cloned()
            .or_else(|| provider_default_model(&provider_id).map(|value| value.to_string()))
            .unwrap_or_default();
        let provider_base_url = existing
            .get("BASE_URL")
            .cloned()
            .or_else(|| provider_default_base_url(&provider_id).map(|value| value.to_string()))
            .unwrap_or_default();

        let mut fields = vec![
            Field {
                key: "PROVIDER".into(),
                label: "Provider profile ID".into(),
                value: provider_id.clone(),
                required: true,
                secret: false,
                help: Some(format!(
                    "Profile id used as default_provider ({})",
                    provider_choices()
                )),
            },
            Field {
                key: "MODEL".into(),
                label: "LLM model".into(),
                value: provider_model,
                required: false,
                secret: false,
                help: Some("Model name for the selected provider profile".into()),
            },
            Field {
                key: "BASE_URL".into(),
                label: "API base URL".into(),
                value: provider_base_url,
                required: true,
                secret: false,
                help: Some(
                    "OpenAI-compatible API endpoint for the selected provider profile".into(),
                ),
            },
            Field {
                key: "API_KEY".into(),
                label: "API key".into(),
                value: existing.get("API_KEY").cloned().unwrap_or_default(),
                required: true,
                secret: true,
                help: Some("Leave empty for local endpoints (localhost/127.0.0.1)".into()),
            },
            Field {
                key: "DISCORD_ENABLED".into(),
                label: "Enable Discord channel".into(),
                value: existing
                    .get("DISCORD_ENABLED")
                    .cloned()
                    .unwrap_or_else(|| "false".into()),
                required: false,
                secret: false,
                help: Some("true/false".into()),
            },
            Field {
                key: "DISCORD_BOT_TOKEN".into(),
                label: "Discord bot token".into(),
                value: existing
                    .get("DISCORD_BOT_TOKEN")
                    .cloned()
                    .unwrap_or_default(),
                required: false,
                secret: true,
                help: Some("From Discord Developer Portal".into()),
            },
            Field {
                key: "TELEGRAM_ENABLED".into(),
                label: "Enable Telegram channel".into(),
                value: existing
                    .get("TELEGRAM_ENABLED")
                    .cloned()
                    .unwrap_or_else(|| "false".into()),
                required: false,
                secret: false,
                help: Some("true/false".into()),
            },
            Field {
                key: "TELEGRAM_BOT_TOKEN".into(),
                label: "Telegram bot token".into(),
                value: existing
                    .get("TELEGRAM_BOT_TOKEN")
                    .cloned()
                    .unwrap_or_default(),
                required: false,
                secret: true,
                help: Some("From @BotFather on Telegram".into()),
            },
            Field {
                key: "TELEGRAM_BOT_USERNAME".into(),
                label: "Telegram bot username".into(),
                value: existing
                    .get("TELEGRAM_BOT_USERNAME")
                    .cloned()
                    .unwrap_or_default(),
                required: false,
                secret: false,
                help: Some("Without @, e.g. my_egopulse_bot".into()),
            },
        ];

        update_field_visibility(&mut fields);

        Ok(Self {
            fields,
            selected: 0,
            mode: SetupMode::Navigate,
            status: "Enter: edit | Up/Down: navigate | Ctrl+S: save & exit | Ctrl+C: cancel".into(),
            completed: false,
            backup_path: None,
            completion_summary: Vec::new(),
            config_path,
            original_yaml,
        })
    }

    fn load_existing_config(
        config_path: &Path,
    ) -> (HashMap<String, String>, Option<serde_yml::Value>) {
        let mut result = HashMap::new();

        let contents = match fs::read_to_string(config_path) {
            Ok(c) => c,
            Err(_) => return (result, None),
        };

        let parsed: serde_yml::Value = match serde_yml::from_str(&contents) {
            Ok(v) => v,
            Err(_) => return (result, None),
        };

        if let Some(map) = parsed.as_mapping() {
            if let Some(default_provider) = map
                .get(serde_yml::Value::String("default_provider".into()))
                .and_then(|value| value.as_str())
            {
                let provider_id = normalize_provider_id(default_provider);
                result.insert("PROVIDER".into(), provider_id.clone());
                if let Some(top_level_model) = map
                    .get(serde_yml::Value::String("default_model".into()))
                    .and_then(|v| v.as_str())
                {
                    result.insert("MODEL".into(), top_level_model.to_string());
                } else if let Some(providers) = map
                    .get(serde_yml::Value::String("providers".into()))
                    .and_then(|value| value.as_mapping())
                    && let Some(provider) = providers
                        .get(serde_yml::Value::String(default_provider.into()))
                        .and_then(|value| value.as_mapping())
                {
                    if let Some(model) = provider
                        .get(serde_yml::Value::String("default_model".into()))
                        .and_then(|value| value.as_str())
                    {
                        result.insert("MODEL".into(), model.to_string());
                    } else if let Some(model) = provider_default_model(&provider_id) {
                        result.insert("MODEL".into(), model.to_string());
                    }
                    if let Some(base_url) = provider
                        .get(serde_yml::Value::String("base_url".into()))
                        .and_then(|value| value.as_str())
                    {
                        result.insert("BASE_URL".into(), base_url.to_string());
                    } else if let Some(base_url) = provider_default_base_url(&provider_id) {
                        result.insert("BASE_URL".into(), base_url.to_string());
                    }
                    if let Some(api_key) = provider
                        .get(serde_yml::Value::String("api_key".into()))
                        .and_then(|value| value.as_str())
                    {
                        result.insert("API_KEY".into(), api_key.to_string());
                    }
                }
            }

            if let Some(channels) = map.get(serde_yml::Value::String("channels".into())) {
                load_channel_fields(channels, &mut result);
            }
        }

        (result, Some(parsed))
    }

    fn visible_fields(&self) -> Vec<usize> {
        let mut indices = Vec::new();

        for field in self.fields.iter().enumerate() {
            let should_skip = match field.1.key.as_str() {
                "DISCORD_BOT_TOKEN" => !self
                    .fields
                    .iter()
                    .find(|f| f.key == "DISCORD_ENABLED")
                    .map(|f| parse_bool(&f.value).unwrap_or(false))
                    .unwrap_or(false),
                "TELEGRAM_BOT_TOKEN" | "TELEGRAM_BOT_USERNAME" => !self
                    .fields
                    .iter()
                    .find(|f| f.key == "TELEGRAM_ENABLED")
                    .map(|f| parse_bool(&f.value).unwrap_or(false))
                    .unwrap_or(false),
                _ => false,
            };

            if !should_skip {
                indices.push(field.0);
            }
        }

        indices
    }

    fn move_selection(&mut self, delta: isize) {
        let visible = self.visible_fields();
        if visible.is_empty() {
            return;
        }

        let current_pos = visible
            .iter()
            .position(|&idx| idx == self.selected)
            .unwrap_or(0);

        let next_pos = (current_pos as isize + delta).clamp(0, visible.len() as isize - 1) as usize;

        self.selected = visible[next_pos];
    }

    fn current_field(&self) -> Option<&Field> {
        self.fields.get(self.selected)
    }

    fn current_field_mut(&mut self) -> Option<&mut Field> {
        self.fields.get_mut(self.selected)
    }

    fn save(&mut self) -> Result<(), String> {
        let (backup_path, completion_summary) =
            save_config(&self.fields, &self.original_yaml, &self.config_path)?;
        self.backup_path = backup_path;
        self.completion_summary = completion_summary;
        self.completed = true;
        Ok(())
    }
}

fn parse_bool(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "on" => Some(true),
        "false" | "0" | "no" | "off" => Some(false),
        _ => None,
    }
}

fn filtered_items<'a>(items: &'a [SelectorItem], filter: &str) -> Vec<&'a SelectorItem> {
    if filter.is_empty() {
        return items.iter().collect();
    }
    let lower = filter.to_ascii_lowercase();
    items
        .iter()
        .filter(|item| {
            item.display.to_ascii_lowercase().contains(&lower)
                || item.value.to_ascii_lowercase().contains(&lower)
        })
        .collect()
}

fn draw(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>, app: &SetupApp) {
    let _ = terminal.draw(|frame| {
        let area = frame.area();
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Min(5),
                Constraint::Length(3),
            ])
            .split(area);

        let header = Paragraph::new(vec![
            Line::from(vec![Span::styled(
                "EgoPulse Setup Wizard",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )]),
            Line::from("Configure egopulse.config.yaml interactively"),
        ])
        .block(Block::default().borders(Borders::ALL))
        .wrap(Wrap { trim: true });
        frame.render_widget(header, chunks[0]);

        if app.completed {
            draw_completion_summary(frame, app, chunks[1]);
        } else {
            draw_fields(frame, app, chunks[1]);
        }

        if let SetupMode::Selector(ref state) = app.mode {
            draw_selector_popup(frame, state, area);
        }

        let footer_text = if app.completed {
            vec![Line::from(
                "Setup complete. Run egopulse for the TUI, or egopulse run for channels.",
            )]
        } else {
            vec![
                Line::from(app.status.clone()),
                if let Some(field) = app.current_field() {
                    if let Some(ref help) = field.help {
                        Line::from(format!("hint: {help}"))
                    } else {
                        Line::from("")
                    }
                } else {
                    Line::from("")
                },
            ]
        };

        let footer = Paragraph::new(footer_text)
            .block(Block::default().borders(Borders::ALL))
            .wrap(Wrap { trim: true });
        frame.render_widget(footer, chunks[2]);

        if matches!(app.mode, SetupMode::Edit) && !app.completed {
            if let Some(field) = app.current_field() {
                let visible = app.visible_fields();
                let field_pos = visible
                    .iter()
                    .position(|&idx| idx == app.selected)
                    .unwrap_or(0);

                let content_height = chunks[1].height.saturating_sub(2) as usize;
                let mut window_start = 0usize;
                if field_pos < window_start {
                    window_start = field_pos;
                } else if field_pos >= window_start + content_height {
                    window_start = field_pos - content_height + 1;
                }
                let window_end = window_start + content_height;

                if (window_start..window_end).contains(&field_pos) {
                    let row = chunks[1].y + 1 + (field_pos - window_start) as u16;
                    let label_width = max_label_width(&app.fields, &visible);
                    let displayed_len = if field.value.is_empty() {
                        "(type value...)".chars().count()
                    } else {
                        field.value.chars().count()
                    };
                    let cursor_x = chunks[1].x + label_width + 3 + displayed_len as u16;
                    let cursor_y = row;
                    frame.set_cursor_position(Position::new(cursor_x, cursor_y));
                }
            }
        }
    });
}

fn max_label_width(fields: &[Field], visible: &[usize]) -> u16 {
    let mut max = 0;
    for &idx in visible {
        if let Some(f) = fields.get(idx) {
            let len = f.label.chars().count();
            if len > max {
                max = len;
            }
        }
    }
    (max + 2) as u16
}

fn draw_fields(frame: &mut ratatui::Frame<'_>, app: &SetupApp, area: Rect) {
    let visible = app.visible_fields();
    if visible.is_empty() {
        return;
    }

    let content_height = area.height.saturating_sub(2) as usize;
    if content_height == 0 {
        return;
    }

    let field_pos = visible
        .iter()
        .position(|&idx| idx == app.selected)
        .unwrap_or(0);

    let mut window_start = 0usize;
    if field_pos < window_start {
        window_start = field_pos;
    } else if field_pos >= window_start + content_height {
        window_start = field_pos - content_height + 1;
    }

    let label_width = max_label_width(&app.fields, &visible);
    let window_end = (window_start + content_height).min(visible.len());

    let is_selector_active = matches!(app.mode, SetupMode::Selector(_));

    let mut lines = Vec::new();
    for &idx in visible.iter().take(window_end).skip(window_start) {
        let field = &app.fields[idx];
        let is_selected = idx == app.selected;
        let is_editing = is_selected && matches!(app.mode, SetupMode::Edit);

        let display = field.display_value(is_editing);
        let prefix = if is_selected { "> " } else { "  " };

        let base_style = if is_selector_active {
            Style::default().fg(Color::DarkGray)
        } else {
            Style::default()
        };

        let mut spans = vec![
            Span::styled(prefix, base_style),
            Span::styled(
                &field.label,
                if is_selector_active {
                    base_style
                } else if is_selected {
                    Style::default().fg(Color::Yellow)
                } else {
                    Style::default().fg(Color::White)
                },
            ),
        ];

        let sep_len = label_width.saturating_sub(field.label.chars().count() as u16);
        if sep_len > 0 {
            spans.push(Span::raw(" ".repeat(sep_len as usize)));
        }

        spans.push(Span::raw(" "));

        if is_editing {
            spans.push(Span::styled(
                if display.is_empty() {
                    "(type value...)".into()
                } else {
                    display
                },
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::UNDERLINED),
            ));
        } else if field.secret && !display.is_empty() {
            spans.push(Span::styled(
                display,
                if is_selector_active {
                    base_style
                } else {
                    Style::default().fg(Color::DarkGray)
                },
            ));
        } else if display.is_empty() {
            spans.push(Span::styled(
                "(empty)",
                if is_selector_active {
                    base_style
                } else {
                    Style::default().fg(Color::DarkGray)
                },
            ));
        } else {
            spans.push(Span::styled(display, base_style));
        }

        if field.required && !is_editing {
            spans.push(Span::styled(
                " *",
                if is_selector_active {
                    base_style
                } else {
                    Style::default().fg(Color::Red)
                },
            ));
        }

        lines.push(Line::from(spans));
    }

    let body = Paragraph::new(lines)
        .block(
            Block::default()
                .title("Configuration Fields")
                .borders(Borders::ALL),
        )
        .wrap(Wrap { trim: true });
    frame.render_widget(body, area);
}

fn draw_selector_popup(frame: &mut ratatui::Frame<'_>, state: &SelectorState, area: Rect) {
    let title = match state.field_key.as_str() {
        "PROVIDER" => "Select Provider",
        "MODEL" => "Select Model",
        _ => "Select",
    };

    let filtered = filtered_items(&state.items, &state.filter);

    let popup_width = (area.width as usize).clamp(40, 70);
    let popup_height = (7 + filtered.len()).clamp(10, 20) as u16;
    let max_list_height = (popup_height as usize).saturating_sub(7);

    let popup_x = (area.width as usize).saturating_sub(popup_width) / 2;
    let popup_y = (area.height as usize).saturating_sub(popup_height as usize) / 2;

    let popup_area = Rect::new(
        popup_x as u16,
        popup_y as u16,
        popup_width as u16,
        popup_height,
    );

    let inner_width = popup_width.saturating_sub(2);

    let mut lines: Vec<Line<'_>> = Vec::new();

    lines.push(Line::from(vec![Span::styled(
        title,
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )]));
    lines.push(Line::from(vec![Span::styled(
        "─".repeat(inner_width),
        Style::default().fg(Color::DarkGray),
    )]));

    let filter_display = format!("Filter: {}", state.filter);
    lines.push(Line::from(vec![Span::styled(
        filter_display,
        Style::default().fg(Color::White),
    )]));

    lines.push(Line::from(vec![Span::styled(
        "─".repeat(inner_width),
        Style::default().fg(Color::DarkGray),
    )]));

    if filtered.is_empty() {
        lines.push(Line::from(vec![Span::styled(
            "No matches. Enter to use as free input.",
            Style::default().fg(Color::Yellow),
        )]));
    } else {
        let mut window_start = 0usize;
        if state.selected >= max_list_height {
            window_start = state.selected - max_list_height + 1;
        }
        let window_end = (window_start + max_list_height).min(filtered.len());

        for (i, item) in filtered
            .iter()
            .enumerate()
            .skip(window_start)
            .take(window_end - window_start)
        {
            let is_selected = i == state.selected;
            let prefix = if is_selected { "▸ " } else { "  " };
            let display_text = if item.display.chars().count() > inner_width.saturating_sub(4) {
                let truncated: String = item
                    .display
                    .chars()
                    .take(inner_width.saturating_sub(5))
                    .collect();
                format!("{truncated}…")
            } else {
                item.display.clone()
            };

            if is_selected {
                lines.push(Line::from(vec![Span::styled(
                    format!("{prefix}{display_text}"),
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::White)
                        .add_modifier(Modifier::BOLD),
                )]));
            } else {
                lines.push(Line::from(vec![Span::styled(
                    format!("{prefix}{display_text}"),
                    Style::default().fg(Color::White),
                )]));
            }
        }
    }

    let remaining = (popup_height as usize).saturating_sub(lines.len() + 3);
    for _ in 0..remaining {
        lines.push(Line::from(""));
    }

    let match_info = format!(
        "{} matches │ Esc:cancel Enter:select ↑↓:navigate",
        filtered.len()
    );
    lines.push(Line::from(vec![Span::styled(
        match_info,
        Style::default().fg(Color::DarkGray),
    )]));

    let popup = Paragraph::new(lines)
        .block(Block::default().borders(Borders::ALL))
        .wrap(Wrap { trim: true });
    frame.render_widget(popup, popup_area);

    let filter_cursor_x = popup_area.x + 1 + 8 + state.filter.chars().count() as u16;
    let filter_cursor_y = popup_area.y + 3;
    frame.set_cursor_position(Position::new(filter_cursor_x, filter_cursor_y));
}

/// Runs the interactive setup wizard and writes the resulting configuration file.
pub async fn run_setup_wizard(config_path: Option<PathBuf>) -> Result<(), String> {
    let mut app = SetupApp::new(config_path)?;
    let terminal = init_terminal()?;

    run_loop(terminal, &mut app).await
}

fn init_terminal() -> Result<Terminal<CrosstermBackend<io::Stdout>>, String> {
    enable_raw_mode().map_err(|e| e.to_string())?;
    let mut stdout = io::stdout();
    if let Err(e) = execute!(stdout, EnterAlternateScreen) {
        let _ = disable_raw_mode();
        return Err(e.to_string());
    }
    let backend = CrosstermBackend::new(stdout);
    match Terminal::new(backend) {
        Ok(t) => Ok(t),
        Err(e) => {
            let _ = execute!(io::stdout(), LeaveAlternateScreen);
            let _ = disable_raw_mode();
            Err(e.to_string())
        }
    }
}

async fn run_loop(
    mut terminal: Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut SetupApp,
) -> Result<(), String> {
    let result = run_inner(&mut terminal, app).await;
    let _ = disable_raw_mode();
    let _ = execute!(io::stdout(), LeaveAlternateScreen);
    result
}

async fn run_inner(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut SetupApp,
) -> Result<(), String> {
    loop {
        draw(terminal, app);

        if app.completed {
            if event::poll(std::time::Duration::from_millis(200)).map_err(|e| e.to_string())? {
                if let Event::Key(key) = event::read().map_err(|e| e.to_string())? {
                    if key.kind == KeyEventKind::Press {
                        return Ok(());
                    }
                }
            }
            continue;
        }

        if event::poll(std::time::Duration::from_millis(200)).map_err(|e| e.to_string())? {
            let Event::Key(key) = event::read().map_err(|e| e.to_string())? else {
                continue;
            };

            if key.kind != KeyEventKind::Press {
                continue;
            }

            match app.mode {
                SetupMode::Selector(ref mut state) => match key.code {
                    KeyCode::Esc => {
                        if let Some(field) =
                            app.fields.iter_mut().find(|f| f.key == state.field_key)
                        {
                            field.value = state.original_value.clone();
                        }
                        app.mode = SetupMode::Navigate;
                        app.status =
                                "Enter: edit | Up/Down: navigate | Ctrl+S: save & exit | Ctrl+C: cancel"
                                    .into();
                    }
                    KeyCode::Enter => {
                        let filtered = filtered_items(&state.items, &state.filter);
                        if filtered.is_empty() {
                            if let Some(field) =
                                app.fields.iter_mut().find(|f| f.key == state.field_key)
                            {
                                field.value = state.filter.clone();
                            }
                        } else {
                            state.selected = (state.selected).min(filtered.len() - 1);
                            let selected_value = filtered[state.selected].value.clone();
                            if let Some(field) =
                                app.fields.iter_mut().find(|f| f.key == state.field_key)
                            {
                                field.value = selected_value;
                            }
                        }
                        let field_key = state.field_key.clone();
                        app.apply_selector_selection(&field_key);
                        app.mode = SetupMode::Navigate;
                        app.status =
                                "Enter: edit | Up/Down: navigate | Ctrl+S: save & exit | Ctrl+C: cancel"
                                    .into();
                    }
                    KeyCode::Up | KeyCode::Char('k')
                        if !key.modifiers.contains(KeyModifiers::CONTROL) =>
                    {
                        let filtered = filtered_items(&state.items, &state.filter);
                        if !filtered.is_empty() && state.selected > 0 {
                            state.selected -= 1;
                        }
                    }
                    KeyCode::Down | KeyCode::Char('j')
                        if !key.modifiers.contains(KeyModifiers::CONTROL) =>
                    {
                        let filtered = filtered_items(&state.items, &state.filter);
                        if !filtered.is_empty() {
                            state.selected = (state.selected + 1).min(filtered.len() - 1);
                        }
                    }
                    KeyCode::Backspace => {
                        state.filter.pop();
                        let filtered = filtered_items(&state.items, &state.filter);
                        if !filtered.is_empty() {
                            state.selected = state.selected.min(filtered.len() - 1);
                        } else {
                            state.selected = 0;
                        }
                    }
                    KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                        state.filter.push(c);
                        let filtered = filtered_items(&state.items, &state.filter);
                        if !filtered.is_empty() {
                            state.selected = state.selected.min(filtered.len() - 1);
                        } else {
                            state.selected = 0;
                        }
                    }
                    _ => {}
                },
                SetupMode::Edit => match key.code {
                    KeyCode::Esc | KeyCode::Enter => {
                        if let Some(field) = app.current_field() {
                            if field.key == "DISCORD_ENABLED" || field.key == "TELEGRAM_ENABLED" {
                                update_field_visibility(&mut app.fields);
                            }
                        }
                        app.mode = SetupMode::Navigate;
                        app.status =
                                "Enter: edit | Up/Down: navigate | Ctrl+S: save & exit | Ctrl+C: cancel"
                                    .into();
                    }
                    KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        match app.save() {
                            Ok(()) => {
                                app.mode = SetupMode::Navigate;
                                app.status = "Config saved successfully!".into();
                            }
                            Err(e) => {
                                app.status = format!("Save failed: {e}");
                            }
                        }
                    }
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        return Err("Setup cancelled".into());
                    }
                    KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                        if let Some(field) = app.current_field_mut() {
                            field.value.push(c);
                        }
                    }
                    KeyCode::Backspace => {
                        if let Some(field) = app.current_field_mut() {
                            field.value.pop();
                        }
                    }
                    _ => {}
                },
                SetupMode::Navigate => match key.code {
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        return Err("Setup cancelled".into());
                    }
                    KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        match app.save() {
                            Ok(()) => {
                                app.status = "Config saved successfully!".into();
                            }
                            Err(e) => {
                                app.status = format!("Save failed: {e}");
                            }
                        }
                    }
                    KeyCode::Enter => {
                        if let Some(field) = app.current_field() {
                            let key_name = field.key.clone();
                            match key_name.as_str() {
                                "PROVIDER" | "MODEL" => {
                                    app.mode = SetupMode::Selector(app.enter_selector(&key_name));
                                    app.status =
                                        "Selector: type to filter, Enter: select, Esc: cancel"
                                            .into();
                                }
                                _ => {
                                    app.mode = SetupMode::Edit;
                                    app.status = "Editing... (Enter/Esc to finish)".into();
                                }
                            }
                        }
                    }
                    KeyCode::Up | KeyCode::Char('k')
                        if !key.modifiers.contains(KeyModifiers::CONTROL) =>
                    {
                        app.move_selection(-1);
                    }
                    KeyCode::Down | KeyCode::Char('j')
                        if !key.modifiers.contains(KeyModifiers::CONTROL) =>
                    {
                        app.move_selection(1);
                    }
                    _ => {}
                },
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{SelectorItem, SelectorState, SetupMode};
    use super::{SetupApp, filtered_items};

    #[test]
    fn load_existing_config_prefers_new_provider_schema() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let config_path = temp_dir.path().join("egopulse.config.yaml");
        std::fs::write(
            &config_path,
            r#"default_provider: openai
providers:
  openai:
    label: OpenAI
    base_url: https://api.openai.com/v1
    api_key: sk-openai
    default_model: gpt-4o-mini
    models:
      - gpt-4o-mini
      - gpt-5
channels:
  web:
    enabled: true
    auth_token: web-token
"#,
        )
        .expect("write config");

        let (existing, _) = SetupApp::load_existing_config(&config_path);

        assert_eq!(existing.get("PROVIDER"), Some(&"openai".to_string()));
        assert_eq!(existing.get("MODEL"), Some(&"gpt-4o-mini".to_string()));
        assert_eq!(
            existing.get("BASE_URL"),
            Some(&"https://api.openai.com/v1".to_string())
        );
        assert_eq!(existing.get("API_KEY"), Some(&"sk-openai".to_string()));
        assert_eq!(
            existing.get("WEB_AUTH_TOKEN"),
            Some(&"web-token".to_string())
        );
    }

    #[test]
    fn load_existing_config_ignores_legacy_top_level_llm_fields() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let config_path = temp_dir.path().join("egopulse.config.yaml");
        std::fs::write(
            &config_path,
            r#"model: gpt-4o-mini
base_url: https://api.openai.com/v1
api_key: sk-legacy
"#,
        )
        .expect("write config");

        let (existing, _) = SetupApp::load_existing_config(&config_path);

        assert!(!existing.contains_key("PROVIDER"));
        assert!(!existing.contains_key("MODEL"));
        assert!(!existing.contains_key("BASE_URL"));
        assert!(!existing.contains_key("API_KEY"));
    }

    #[test]
    fn filtered_items_returns_all_when_filter_empty() {
        let items = vec![
            SelectorItem {
                display: "openai (gpt-5.2, gpt-5)".into(),
                value: "openai".into(),
            },
            SelectorItem {
                display: "ollama (llama3.2)".into(),
                value: "ollama".into(),
            },
        ];
        let result = filtered_items(&items, "");
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn filtered_items_matches_substring_case_insensitive() {
        let items = vec![
            SelectorItem {
                display: "openai (gpt-5.2, gpt-5)".into(),
                value: "openai".into(),
            },
            SelectorItem {
                display: "Ollama (local)".into(),
                value: "ollama".into(),
            },
            SelectorItem {
                display: "OpenRouter".into(),
                value: "openrouter".into(),
            },
        ];
        let result = filtered_items(&items, "OPEN");
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].value, "openai");
        assert_eq!(result[1].value, "openrouter");
    }

    #[test]
    fn filtered_items_returns_none_when_no_match() {
        let items = vec![SelectorItem {
            display: "openai".into(),
            value: "openai".into(),
        }];
        let result = filtered_items(&items, "zzzzz");
        assert!(result.is_empty());
    }

    #[test]
    fn setup_mode_navigate_default() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let config_path = temp_dir.path().join("egopulse.config.yaml");
        let app = SetupApp::new(Some(config_path)).expect("setup app");
        assert!(matches!(app.mode, SetupMode::Navigate));
    }

    #[test]
    fn selector_state_holds_original_value() {
        let state = SelectorState {
            field_key: "PROVIDER".into(),
            filter: String::new(),
            items: vec![],
            selected: 0,
            original_value: "openai".into(),
        };
        assert_eq!(state.field_key, "PROVIDER");
        assert_eq!(state.original_value, "openai");
    }
}
