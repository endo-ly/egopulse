//! Ratatui rendering for the inline viewport and temporary overlays.

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::conversation::SurfaceContext;

use super::composer::Composer;
use super::markdown::render_markdown;
use super::sessions::SessionsOverlay;
use super::transcript::{Block as TranscriptBlock, ToolBlock, ToolStatus, Transcript};

pub(super) struct DrawState<'a> {
    pub(super) context: &'a SurfaceContext,
    pub(super) transcript: &'a Transcript,
    pub(super) composer: &'a Composer,
    pub(super) status: &'a str,
    pub(super) model: &'a str,
    pub(super) sessions: Option<&'a SessionsOverlay>,
}

pub(super) fn draw(frame: &mut ratatui::Frame<'_>, state: DrawState<'_>) {
    let area = frame.area();
    if let Some(sessions) = state.sessions {
        draw_sessions(frame, area, sessions);
        return;
    }

    let composer_height = state
        .composer
        .height()
        .saturating_add(2)
        .min(area.height.saturating_sub(3))
        .max(3);
    let [conversation_area, composer_area, status_area] = Layout::vertical([
        Constraint::Min(1),
        Constraint::Length(composer_height),
        Constraint::Length(2),
    ])
    .areas(area);

    draw_active_turn(frame, conversation_area, state.transcript);
    draw_composer(frame, composer_area, state.composer);
    draw_status(frame, status_area, state.context, state.status, state.model);

    if let Some(completion) = state.composer.completion() {
        let popup_height = completion.candidates().len().min(6) as u16 + 2;
        let popup_y = composer_area.y.saturating_sub(popup_height);
        let popup_width = completion
            .candidates()
            .iter()
            .map(|candidate| candidate.width())
            .max()
            .unwrap_or(12)
            .saturating_add(6)
            .min(area.width as usize) as u16;
        let popup = Rect {
            x: composer_area.x,
            y: popup_y,
            width: popup_width,
            height: popup_height.min(area.height),
        };
        draw_completion(frame, popup, completion.candidates(), completion.selected());
    }
}

pub(super) fn render_block(block: &TranscriptBlock, width: usize) -> Vec<Line<'static>> {
    let width = width.max(1);
    match block {
        TranscriptBlock::User(text) => vec![Line::from(vec![Span::styled(
            format!("You: {text}"),
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        )])],
        TranscriptBlock::Assistant(text) => {
            let mut lines = vec![Line::from(Span::styled(
                "Assistant",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ))];
            lines.extend(render_markdown(text, width));
            lines
        }
        TranscriptBlock::Tool(tool) => render_tool(tool),
        TranscriptBlock::Error(text) => vec![Line::from(Span::styled(
            format!("Error: {text}"),
            Style::default().fg(Color::Red),
        ))],
    }
}

fn draw_active_turn(frame: &mut ratatui::Frame<'_>, area: Rect, transcript: &Transcript) {
    let lines = active_lines(transcript, area.width as usize);
    let scroll = lines.len().saturating_sub(area.height as usize) as u16;
    frame.render_widget(Paragraph::new(Text::from(lines)).scroll((scroll, 0)), area);
}

fn active_lines(transcript: &Transcript, width: usize) -> Vec<Line<'static>> {
    let Some(active) = transcript.active() else {
        return vec![Line::from(Span::styled(
            "Ready — type a message and press Enter",
            Style::default().fg(Color::DarkGray),
        ))];
    };

    let iteration = active.iteration.map_or_else(
        || "starting".to_string(),
        |value| format!("iteration {value}"),
    );
    let mut lines = vec![Line::from(Span::styled(
        format!("⟳ {iteration}"),
        Style::default().fg(Color::Yellow),
    ))];
    if !active.buffer.is_empty() {
        lines.extend(render_plain(&active.buffer, width));
    }
    for tool in &active.tool_cards {
        lines.extend(render_tool(tool));
    }
    lines
}

fn render_plain(text: &str, width: usize) -> Vec<Line<'static>> {
    let width = width.max(1);
    let mut lines = Vec::new();
    let mut current = String::new();
    let mut current_width = 0;
    for character in text.chars() {
        if character == '\n' {
            lines.push(Line::raw(std::mem::take(&mut current)));
            current_width = 0;
            continue;
        }
        let character_width = character.width().unwrap_or(0);
        if character_width > 0 && current_width + character_width > width {
            lines.push(Line::raw(std::mem::take(&mut current)));
            current_width = 0;
        }
        current.push(character);
        current_width += character_width;
    }
    if !current.is_empty() || lines.is_empty() {
        lines.push(Line::raw(current));
    }
    lines
}

fn render_tool(tool: &ToolBlock) -> Vec<Line<'static>> {
    let (marker, style, detail) = match &tool.status {
        ToolStatus::Running => ("…", Style::default().fg(Color::Yellow), tool.input.clone()),
        ToolStatus::Completed {
            is_error,
            preview,
            duration_ms,
        } => {
            let style = if *is_error {
                Style::default().fg(Color::Red)
            } else {
                Style::default().fg(Color::Green)
            };
            (
                if *is_error { "×" } else { "✓" },
                style,
                format!("{preview} ({duration_ms}ms)"),
            )
        }
    };
    vec![Line::from(vec![
        Span::styled(format!("{marker} {} ", tool.name), style),
        Span::styled(detail, Style::default().fg(Color::DarkGray)),
    ])]
}

fn draw_composer(frame: &mut ratatui::Frame<'_>, area: Rect, composer: &Composer) {
    let lines: Vec<Line<'static>> = composer
        .lines()
        .iter()
        .map(|line| Line::from(line.clone()))
        .collect();
    let cursor = composer.cursor();
    let visible_lines = area.height.saturating_sub(2) as usize;
    let scroll = cursor.line.saturating_sub(visible_lines.saturating_sub(1)) as u16;
    frame.render_widget(
        Paragraph::new(Text::from(lines))
            .scroll((scroll, 0))
            .block(Block::default().borders(Borders::ALL).title(" Message ")),
        area,
    );

    let line = composer.lines().get(cursor.line).map_or("", String::as_str);
    let cursor_x = area
        .x
        .saturating_add(1)
        .saturating_add(line.chars().take(cursor.column).collect::<String>().width() as u16);
    let cursor_y = area
        .y
        .saturating_add(1)
        .saturating_add(cursor.line.saturating_sub(scroll as usize) as u16);
    frame.set_cursor_position((cursor_x, cursor_y));
}

fn draw_status(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    context: &SurfaceContext,
    status: &str,
    model: &str,
) {
    let line = Line::from(vec![
        Span::styled(
            format!(" {} ", context.surface_thread),
            Style::default().fg(Color::Cyan),
        ),
        Span::styled(
            format!("agent={} model={} ", context.agent_id, model),
            Style::default().fg(Color::DarkGray),
        ),
        Span::raw(status),
        Span::styled("  Ctrl-C/q quit", Style::default().fg(Color::DarkGray)),
    ]);
    frame.render_widget(Paragraph::new(line), area);
}

fn draw_completion(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    candidates: &[String],
    selected: usize,
) {
    let lines: Vec<Line<'static>> = candidates
        .iter()
        .enumerate()
        .take(area.height.saturating_sub(2) as usize)
        .map(|(index, candidate)| {
            let style = if index == selected {
                Style::default().fg(Color::Black).bg(Color::Cyan)
            } else {
                Style::default()
            };
            Line::from(Span::styled(format!(" {candidate}"), style))
        })
        .collect();
    frame.render_widget(
        Paragraph::new(Text::from(lines))
            .block(Block::default().borders(Borders::ALL).title(" Commands ")),
        area,
    );
}

fn draw_sessions(frame: &mut ratatui::Frame<'_>, area: Rect, sessions: &SessionsOverlay) {
    frame.render_widget(Clear, area);
    let lines: Vec<Line<'static>> = if sessions.sessions().is_empty() {
        vec![
            Line::from("No saved sessions"),
            Line::from("n: new  Esc: close"),
        ]
    } else {
        sessions
            .sessions()
            .iter()
            .enumerate()
            .map(|(index, session)| {
                let marker = if index == sessions.selected() {
                    "▸"
                } else {
                    " "
                };
                let title = session
                    .chat_title
                    .as_deref()
                    .unwrap_or(&session.surface_thread);
                Line::from(format!(
                    "{marker} {title}  [{}]  {}",
                    session.agent_id, session.last_message_time
                ))
            })
            .collect()
    };
    frame.render_widget(
        Paragraph::new(Text::from(lines))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Sessions — Enter open, n new, Esc close "),
            )
            .wrap(ratatui::widgets::Wrap { trim: false }),
        area,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversation::SurfaceContext;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    #[test]
    fn test_backend_contains_composer_and_status() {
        // Arrange
        let context = SurfaceContext::new(
            "tui".to_string(),
            "local_user".to_string(),
            "session".to_string(),
            "tui".to_string(),
            "default".to_string(),
        );
        let transcript = Transcript::new();
        let composer = Composer::new();
        let mut terminal = Terminal::new(TestBackend::new(60, 10)).expect("test terminal");

        // Act
        terminal
            .draw(|frame| {
                draw(
                    frame,
                    DrawState {
                        context: &context,
                        transcript: &transcript,
                        composer: &composer,
                        status: "Ready",
                        model: "test-model",
                        sessions: None,
                    },
                );
            })
            .expect("draw succeeds");

        // Assert
        let rendered: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(rendered.contains("Message"));
        assert!(rendered.contains("Ready"));
        assert!(rendered.contains("test-model"));
    }
}
