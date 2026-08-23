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
        TranscriptBlock::User(text) => render_user(text, width),
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

fn render_user(text: &str, width: usize) -> Vec<Line<'static>> {
    let prefix = "You: ";
    let indent = " ".repeat(prefix.width());
    let first_width = width.saturating_sub(prefix.width()).max(1);
    let continuation_width = width.saturating_sub(indent.width()).max(1);
    let style = Style::default()
        .fg(Color::Green)
        .add_modifier(Modifier::BOLD);
    let mut lines = Vec::new();

    for (logical_index, logical_line) in text.split('\n').enumerate() {
        let line_width = if logical_index == 0 {
            first_width
        } else {
            continuation_width
        };
        for chunk in wrap_line(logical_line, line_width) {
            let label = if lines.is_empty() { prefix } else { &indent };
            lines.push(Line::from(Span::styled(format!("{label}{chunk}"), style)));
        }
    }

    lines
}

fn wrap_line(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines = Vec::new();
    let mut current = String::new();
    let mut current_width = 0;
    for character in text.chars() {
        let character_width = character.width().unwrap_or(0);
        if character_width > 0 && current_width > 0 && current_width + character_width > width {
            lines.push(std::mem::take(&mut current));
            current_width = 0;
        }
        current.push(character);
        current_width += character_width;
    }
    if !current.is_empty() || lines.is_empty() {
        lines.push(current);
    }
    lines
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
            let detail = duration_ms.map_or_else(
                || preview.clone(),
                |duration_ms| format!("{preview} ({duration_ms}ms)"),
            );
            (if *is_error { "×" } else { "✓" }, style, detail)
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
    let content_width = area.width.saturating_sub(2).max(1) as usize;
    let line = composer.lines().get(cursor.line).map_or("", String::as_str);
    let cursor_width = line_width_until(line, cursor.column);
    let horizontal_scroll = cursor_width.saturating_sub(content_width.saturating_sub(1));
    frame.render_widget(
        Paragraph::new(Text::from(lines))
            .scroll((scroll, horizontal_scroll.min(u16::MAX as usize) as u16))
            .block(Block::default().borders(Borders::ALL).title(" Message ")),
        area,
    );

    let cursor_x_offset = cursor_width
        .saturating_sub(horizontal_scroll)
        .min(content_width.saturating_sub(1)) as u16;
    let cursor_x = area.x.saturating_add(1).saturating_add(cursor_x_offset);
    let cursor_y = area
        .y
        .saturating_add(1)
        .saturating_add(cursor.line.saturating_sub(scroll as usize) as u16);
    frame.set_cursor_position((cursor_x, cursor_y));
}

fn line_width_until(line: &str, column: usize) -> usize {
    line.chars()
        .take(column)
        .map(|character| character.width().unwrap_or(0))
        .sum()
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
        Span::styled("  Ctrl-C/Ctrl-D quit", Style::default().fg(Color::DarkGray)),
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
        let visible = sessions.visible_range(area.height.saturating_sub(2) as usize);
        sessions
            .sessions()
            .iter()
            .enumerate()
            .skip(visible.start)
            .take(visible.len())
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
    use crate::agent_loop::event::AgentEvent;
    use crate::channels::tui::composer::InputEvent;
    use crate::conversation::SurfaceContext;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use serde_json::json;

    fn context() -> SurfaceContext {
        SurfaceContext::new(
            "tui".to_string(),
            "local_user".to_string(),
            "session".to_string(),
            "tui".to_string(),
            "default".to_string(),
        )
    }

    fn rendered(terminal: &Terminal<TestBackend>) -> String {
        terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    fn line_text(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    #[test]
    fn user_block_preserves_newlines_and_wraps_continuations() {
        // Arrange
        let block = TranscriptBlock::User("first\nsecond".to_string());

        // Act
        let lines = render_block(&block, 20);

        // Assert
        assert_eq!(lines.len(), 2);
        assert_eq!(line_text(&lines[0]), "You: first");
        assert_eq!(line_text(&lines[1]), "     second");

        // Act
        let wrapped = render_block(&TranscriptBlock::User("123456789".to_string()), 10);

        // Assert
        assert_eq!(wrapped.len(), 2);
        assert_eq!(line_text(&wrapped[0]), "You: 12345");
        assert_eq!(line_text(&wrapped[1]), "     6789");
    }

    #[test]
    fn test_backend_contains_composer_and_status() {
        // Arrange
        let context = context();
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
        let rendered = rendered(&terminal);
        assert!(rendered.contains("Message"));
        assert!(rendered.contains("Ready"));
        assert!(rendered.contains("test-model"));
    }

    #[test]
    fn completion_popup_lists_candidates() {
        // Arrange
        let context = context();
        let transcript = Transcript::new();
        let mut composer = Composer::new();
        composer.set_completion_candidates(["/model", "/models"]);
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
        let rendered = rendered(&terminal);
        assert!(rendered.contains("Commands"));
        assert!(rendered.contains("/model"));
        assert!(rendered.contains("/models"));
    }

    #[test]
    fn tool_card_shows_pending_and_result_states() {
        // Arrange
        let context = context();
        let mut transcript = Transcript::new();
        transcript.begin_turn("run");
        transcript.apply_agent_event(AgentEvent::ToolStart {
            call_id: "call-1".to_string(),
            name: "shell".to_string(),
            input: json!({"command": "pwd"}),
        });
        let composer = Composer::new();
        let mut terminal = Terminal::new(TestBackend::new(60, 12)).expect("test terminal");

        // Act
        terminal
            .draw(|frame| {
                draw(
                    frame,
                    DrawState {
                        context: &context,
                        transcript: &transcript,
                        composer: &composer,
                        status: "Working…",
                        model: "test-model",
                        sessions: None,
                    },
                );
            })
            .expect("draw pending tool");

        // Assert
        let pending = rendered(&terminal);
        assert!(pending.contains("… shell"));

        // Act
        transcript.apply_agent_event(AgentEvent::ToolResult {
            call_id: "call-1".to_string(),
            name: "shell".to_string(),
            is_error: false,
            preview: "/tmp".to_string(),
            duration_ms: 12,
        });
        terminal
            .draw(|frame| {
                draw(
                    frame,
                    DrawState {
                        context: &context,
                        transcript: &transcript,
                        composer: &composer,
                        status: "Working…",
                        model: "test-model",
                        sessions: None,
                    },
                );
            })
            .expect("draw completed tool");

        // Assert
        let completed = rendered(&terminal);
        assert!(completed.contains("✓ shell"));
        assert!(completed.contains("/tmp (12ms)"));
    }

    #[test]
    fn long_line_keeps_cursor_inside_composer() {
        // Arrange
        let context = context();
        let transcript = Transcript::new();
        let mut composer = Composer::new();
        composer.handle(InputEvent::Paste("01234567890123456789".to_string()));
        let mut terminal = Terminal::new(TestBackend::new(12, 10)).expect("test terminal");

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
        terminal.backend_mut().assert_cursor_position((10, 6));
    }
}
