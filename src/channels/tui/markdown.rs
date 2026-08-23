//! Markdown-to-ratatui rendering for committed assistant blocks.

use std::sync::OnceLock;

use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use syntect::easy::HighlightLines;
use syntect::highlighting::{Style as SyntectStyle, ThemeSet};
use syntect::parsing::SyntaxSet;
use syntect::util::LinesWithEndings;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Renders Markdown into terminal lines while preserving basic inline styling.
pub(crate) fn render_markdown(markdown: &str, width: usize) -> Vec<Line<'static>> {
    let mut renderer = Renderer::new(width);
    for event in Parser::new_ext(markdown, Options::all()) {
        renderer.handle(event);
    }
    renderer.finish()
}

struct Renderer {
    lines: Vec<Line<'static>>,
    current: Vec<Span<'static>>,
    width: usize,
    styles: Vec<Style>,
    list_depth: usize,
    quote_depth: usize,
    link_destinations: Vec<String>,
    code_block: Option<CodeBlockState>,
}

struct CodeBlockState {
    language: Option<String>,
    content: String,
}

struct HighlightAssets {
    syntax_set: SyntaxSet,
    theme_set: ThemeSet,
}

static HIGHLIGHT_ASSETS: OnceLock<HighlightAssets> = OnceLock::new();

fn highlight_assets() -> &'static HighlightAssets {
    HIGHLIGHT_ASSETS.get_or_init(|| HighlightAssets {
        syntax_set: SyntaxSet::load_defaults_newlines(),
        theme_set: ThemeSet::load_defaults(),
    })
}

impl Renderer {
    fn new(width: usize) -> Self {
        Self {
            lines: Vec::new(),
            current: Vec::new(),
            width: width.max(1),
            styles: Vec::new(),
            list_depth: 0,
            quote_depth: 0,
            link_destinations: Vec::new(),
            code_block: None,
        }
    }

    fn handle<'a>(&mut self, event: Event<'a>) {
        if let Some(code) = &mut self.code_block {
            match event {
                Event::End(TagEnd::CodeBlock) => {
                    let code = self.code_block.take().expect("code block exists");
                    self.render_code_block(&code.content, code.language.as_deref());
                }
                Event::Text(text) => code.content.push_str(&text),
                Event::SoftBreak | Event::HardBreak => code.content.push('\n'),
                _ => {}
            }
            return;
        }

        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(text) => self.append_text(&text, self.current_style()),
            Event::Code(text) => self.append_text(
                &text,
                self.current_style()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Event::InlineMath(text) | Event::DisplayMath(text) => {
                self.append_text(&text, self.current_style().fg(Color::Cyan))
            }
            Event::SoftBreak => self.append_text(" ", self.current_style()),
            Event::HardBreak => self.flush_line(),
            Event::Rule => {
                self.ensure_prefix();
                self.append_text("────────────────", Style::default().fg(Color::DarkGray));
                self.flush_line();
            }
            Event::Html(text) | Event::InlineHtml(text) => {
                self.append_text(&text, self.current_style().fg(Color::DarkGray))
            }
            Event::TaskListMarker(checked) => {
                self.append_text(if checked { "☑ " } else { "☐ " }, self.current_style())
            }
            _ => {}
        }
    }

    fn start<'a>(&mut self, tag: Tag<'a>) {
        match tag {
            Tag::Heading { .. } => {
                self.ensure_prefix();
                self.styles.push(
                    self.current_style()
                        .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
                );
            }
            Tag::BlockQuote(_) => self.quote_depth += 1,
            Tag::List(_) => self.list_depth += 1,
            Tag::Item => {
                self.flush_if_nonempty();
                self.ensure_prefix();
            }
            Tag::CodeBlock(kind) => {
                self.flush_if_nonempty();
                let language = match kind {
                    CodeBlockKind::Fenced(language) if !language.is_empty() => {
                        Some(language.into_string())
                    }
                    _ => None,
                };
                self.code_block = Some(CodeBlockState {
                    language,
                    content: String::new(),
                });
            }
            Tag::Emphasis => self
                .styles
                .push(self.current_style().add_modifier(Modifier::ITALIC)),
            Tag::Strong => self
                .styles
                .push(self.current_style().add_modifier(Modifier::BOLD)),
            Tag::Strikethrough => self
                .styles
                .push(self.current_style().add_modifier(Modifier::CROSSED_OUT)),
            Tag::Link { dest_url, .. } => {
                self.link_destinations.push(dest_url.into_string());
                self.styles.push(
                    self.current_style()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::UNDERLINED),
                );
            }
            Tag::Paragraph => self.ensure_prefix(),
            _ => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph | TagEnd::Heading(_) => {
                if matches!(tag, TagEnd::Heading(_)) {
                    self.styles.pop();
                }
                self.flush_if_nonempty();
            }
            TagEnd::BlockQuote(_) => {
                self.quote_depth = self.quote_depth.saturating_sub(1);
                self.flush_if_nonempty();
            }
            TagEnd::List(_) => {
                self.list_depth = self.list_depth.saturating_sub(1);
                self.flush_if_nonempty();
            }
            TagEnd::Emphasis
            | TagEnd::Strong
            | TagEnd::Strikethrough
            | TagEnd::Superscript
            | TagEnd::Subscript => {
                self.styles.pop();
            }
            TagEnd::Link => {
                self.styles.pop();
                if let Some(destination) = self.link_destinations.pop() {
                    self.append_text(
                        &format!(" ({destination})"),
                        self.current_style().fg(Color::DarkGray),
                    );
                }
            }
            TagEnd::Item => self.flush_if_nonempty(),
            _ => {}
        }
    }

    fn current_style(&self) -> Style {
        self.styles.last().copied().unwrap_or_default()
    }

    fn ensure_prefix(&mut self) {
        if !self.current.is_empty() {
            return;
        }
        let mut prefix = String::new();
        if self.quote_depth > 0 {
            prefix.push_str(&"> ".repeat(self.quote_depth));
        }
        if self.list_depth > 0 {
            prefix.push_str(&"  ".repeat(self.list_depth.saturating_sub(1)));
            prefix.push_str("• ");
        }
        if !prefix.is_empty() {
            self.current
                .push(Span::styled(prefix, Style::default().fg(Color::DarkGray)));
        }
    }

    fn append_text(&mut self, text: &str, style: Style) {
        for character in text.chars() {
            if character == '\n' {
                self.flush_line();
                continue;
            }
            self.ensure_prefix();
            let character_width = character.width().unwrap_or(0);
            if character_width > 0
                && !self.current.is_empty()
                && self.line_width() + character_width > self.width
            {
                self.flush_line();
                self.ensure_prefix();
            }
            self.current
                .push(Span::styled(character.to_string(), style));
        }
    }

    fn line_width(&self) -> usize {
        self.current.iter().map(|span| span.content.width()).sum()
    }

    fn flush_if_nonempty(&mut self) {
        if !self.current.is_empty() {
            self.flush_line();
        }
    }

    fn flush_line(&mut self) {
        self.lines
            .push(Line::from(std::mem::take(&mut self.current)));
    }

    fn render_code_block(&mut self, content: &str, language: Option<&str>) {
        let assets = highlight_assets();
        let syntax = language.and_then(|language| assets.syntax_set.find_syntax_by_token(language));
        let theme = assets
            .theme_set
            .themes
            .get("base16-ocean.dark")
            .or_else(|| assets.theme_set.themes.values().next())
            .expect("syntect ships at least one default theme");
        let mut highlighter = syntax.map(|syntax| HighlightLines::new(syntax, theme));
        let mut emitted = false;

        for source_line in LinesWithEndings::from(content) {
            emitted = true;
            self.ensure_prefix();
            self.append_text("│ ", Style::default().fg(Color::DarkGray));
            let source_line = source_line.trim_end_matches(['\r', '\n']);
            if let Some(highlighter) = &mut highlighter {
                match highlighter.highlight_line(source_line, &assets.syntax_set) {
                    Ok(ranges) => {
                        for (style, text) in ranges {
                            self.append_text(text, syntect_style(style));
                        }
                    }
                    Err(_) => self.append_text(source_line, Style::default()),
                }
            } else {
                self.append_text(source_line, Style::default());
            }
            self.flush_line();
        }

        if !emitted {
            self.ensure_prefix();
            self.append_text("│ ", Style::default().fg(Color::DarkGray));
            self.flush_line();
        }
    }

    fn finish(mut self) -> Vec<Line<'static>> {
        self.flush_if_nonempty();
        if self.lines.is_empty() {
            self.lines.push(Line::default());
        }
        self.lines
    }
}

fn syntect_style(style: SyntectStyle) -> Style {
    Style::default().fg(Color::Rgb(
        style.foreground.r,
        style.foreground.g,
        style.foreground.b,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Modifier;
    use unicode_width::UnicodeWidthStr;

    fn text(lines: &[Line<'static>]) -> String {
        lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn renders_heading_with_emphasis() {
        let lines = render_markdown("# Title", 80);

        assert!(
            lines[0]
                .spans
                .iter()
                .any(|span| span.style.has_modifier(Modifier::BOLD))
        );
        assert!(
            lines[0]
                .spans
                .iter()
                .any(|span| span.style.has_modifier(Modifier::UNDERLINED))
        );
    }

    #[test]
    fn renders_list_and_blockquote_prefixes() {
        let lines = render_markdown("> note\n\n- item", 80);
        let rendered = text(&lines);

        assert!(rendered.contains("> note"));
        assert!(rendered.contains("• item"));
    }

    #[test]
    fn renders_inline_code_and_links() {
        let lines = render_markdown("Use `cargo test` and [docs](https://example.com).", 80);
        let rendered = text(&lines);

        assert!(rendered.contains("cargo test"));
        assert!(rendered.contains("(https://example.com)"));
        assert!(
            lines
                .iter()
                .flat_map(|line| &line.spans)
                .any(|span| { span.style.fg == Some(Color::Yellow) })
        );
    }

    #[test]
    fn highlights_known_code_and_keeps_unknown_code_plain() {
        let known = render_markdown("```rust\nfn main() {}\n```", 80);
        let unknown = render_markdown("```not-a-language\nplain\n```", 80);

        assert!(text(&known).contains("fn main"));
        assert!(
            known
                .iter()
                .flat_map(|line| &line.spans)
                .any(|span| span.style.fg.is_some())
        );
        assert!(text(&unknown).contains("plain"));
    }

    #[test]
    fn wraps_using_unicode_display_width() {
        let lines = render_markdown("あいうえお", 6);

        assert!(lines.len() > 1);
        assert!(lines.iter().all(|line| {
            line.spans
                .iter()
                .map(|span| span.content.width())
                .sum::<usize>()
                <= 6
        }));
    }
}
