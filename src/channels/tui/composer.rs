//! Input composer state machine.
//!
//! The composer deliberately has no rendering or terminal I/O dependencies
//! beyond crossterm's key value types. This keeps UTF-8 editing and history
//! behavior testable without a terminal.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

const MAX_VISIBLE_LINES: usize = 8;
const MAX_HISTORY: usize = 50;

/// A cursor position measured in Unicode scalar values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Cursor {
    pub(crate) line: usize,
    pub(crate) column: usize,
}

/// Input delivered to the composer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum InputEvent {
    Key(KeyEvent),
    Paste(String),
}

/// Side effects requested by the composer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Effect {
    None,
    Send(String),
    Quit,
}

/// Slash-command candidates currently shown by the UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CompletionState {
    candidates: Vec<String>,
    selected: usize,
}

impl CompletionState {
    pub(crate) fn candidates(&self) -> &[String] {
        &self.candidates
    }

    pub(crate) fn selected(&self) -> usize {
        self.selected
    }
}

/// Pure state machine for multiline terminal input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Composer {
    lines: Vec<String>,
    cursor: Cursor,
    history: Vec<String>,
    history_index: Option<usize>,
    draft: Option<String>,
    preferred_column: Option<usize>,
    completion: Option<CompletionState>,
}

impl Default for Composer {
    fn default() -> Self {
        Self::new()
    }
}

impl Composer {
    pub(crate) fn new() -> Self {
        Self {
            lines: vec![String::new()],
            cursor: Cursor { line: 0, column: 0 },
            history: Vec::new(),
            history_index: None,
            draft: None,
            preferred_column: None,
            completion: None,
        }
    }

    #[cfg(test)]
    fn with_text(text: &str) -> Self {
        let mut composer = Self::new();
        composer.insert_text(text);
        composer
    }

    pub(crate) fn text(&self) -> String {
        self.lines.join("\n")
    }

    pub(crate) fn lines(&self) -> &[String] {
        &self.lines
    }

    pub(crate) fn cursor(&self) -> Cursor {
        self.cursor
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.text().trim().is_empty()
    }

    pub(crate) fn height(&self) -> u16 {
        self.lines.len().clamp(1, MAX_VISIBLE_LINES) as u16
    }

    pub(crate) fn completion(&self) -> Option<&CompletionState> {
        self.completion.as_ref()
    }

    /// Applies one terminal input event and returns a requested side effect.
    pub(crate) fn handle(&mut self, event: InputEvent) -> Effect {
        match event {
            InputEvent::Paste(text) => {
                self.insert_text(&text);
                Effect::None
            }
            InputEvent::Key(key) => self.handle_key(key),
        }
    }

    /// Replaces the completion candidates without changing the input buffer.
    pub(crate) fn set_completion_candidates<I, S>(&mut self, candidates: I)
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let candidates: Vec<String> = candidates.into_iter().map(Into::into).collect();
        if candidates.is_empty() {
            self.completion = None;
            return;
        }

        let selected = self
            .completion
            .as_ref()
            .map_or(0, |state| state.selected.min(candidates.len() - 1));
        self.completion = Some(CompletionState {
            candidates,
            selected,
        });
    }

    pub(crate) fn clear_completion(&mut self) {
        self.completion = None;
    }

    pub(crate) fn move_completion(&mut self, delta: isize) {
        let Some(completion) = &mut self.completion else {
            return;
        };
        let len = completion.candidates.len() as isize;
        let next = (completion.selected as isize + delta).rem_euclid(len);
        completion.selected = next as usize;
    }

    fn handle_key(&mut self, key: KeyEvent) -> Effect {
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);

        if self.completion.is_some() {
            match key.code {
                KeyCode::Up => {
                    self.move_completion(-1);
                    return Effect::None;
                }
                KeyCode::Down => {
                    self.move_completion(1);
                    return Effect::None;
                }
                KeyCode::Tab | KeyCode::Enter => {
                    self.accept_completion();
                    return Effect::None;
                }
                KeyCode::Esc => {
                    self.clear_completion();
                    return Effect::None;
                }
                _ => self.clear_completion(),
            }
        }

        match key.code {
            KeyCode::Enter
                if key
                    .modifiers
                    .intersects(KeyModifiers::ALT | KeyModifiers::SHIFT) =>
            {
                self.insert_text("\n");
                Effect::None
            }
            KeyCode::Enter => self.submit(),
            KeyCode::Char('c') if control => {
                if self.is_empty() {
                    Effect::Quit
                } else {
                    self.clear();
                    Effect::None
                }
            }
            KeyCode::Char('d') if control => {
                if self.is_empty() {
                    Effect::Quit
                } else {
                    self.delete_forward();
                    Effect::None
                }
            }
            KeyCode::Char('a') if control => {
                self.move_line_start();
                Effect::None
            }
            KeyCode::Char('e') if control => {
                self.move_line_end();
                Effect::None
            }
            KeyCode::Char('b') if control || alt => {
                self.move_word_left();
                Effect::None
            }
            KeyCode::Char('f') if control || alt => {
                self.move_word_right();
                Effect::None
            }
            KeyCode::Char('w') if control => {
                self.kill_word();
                Effect::None
            }
            KeyCode::Char('u') if control => {
                self.kill_to_line_start();
                Effect::None
            }
            KeyCode::Char('k') if control => {
                self.kill_to_line_end();
                Effect::None
            }
            KeyCode::Left => {
                self.move_left();
                Effect::None
            }
            KeyCode::Right => {
                self.move_right();
                Effect::None
            }
            KeyCode::Home => {
                self.move_line_start();
                Effect::None
            }
            KeyCode::End => {
                self.move_line_end();
                Effect::None
            }
            KeyCode::Up => {
                self.move_up();
                Effect::None
            }
            KeyCode::Down => {
                self.move_down();
                Effect::None
            }
            KeyCode::Backspace => {
                self.backspace();
                Effect::None
            }
            KeyCode::Delete => {
                self.delete_forward();
                Effect::None
            }
            KeyCode::Tab => Effect::None,
            KeyCode::Char(value) if !control => {
                self.insert_char(value);
                Effect::None
            }
            _ => Effect::None,
        }
    }

    fn submit(&mut self) -> Effect {
        if self.is_empty() {
            return Effect::None;
        }
        Effect::Send(self.text())
    }

    /// Commits the current send request after the owning UI accepts it.
    pub(crate) fn accept_submission(&mut self) {
        let text = self.text();
        self.push_history(text);
        self.clear();
    }

    fn clear(&mut self) {
        self.lines = vec![String::new()];
        self.cursor = Cursor { line: 0, column: 0 };
        self.history_index = None;
        self.draft = None;
        self.preferred_column = None;
        self.completion = None;
    }

    fn insert_char(&mut self, value: char) {
        self.reset_navigation();
        let byte_index = char_to_byte_index(&self.lines[self.cursor.line], self.cursor.column);
        self.lines[self.cursor.line].insert(byte_index, value);
        self.cursor.column += 1;
    }

    fn insert_text(&mut self, text: &str) {
        self.reset_navigation();
        let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
        for value in normalized.chars() {
            if value == '\n' {
                self.split_line();
            } else {
                self.insert_char_without_reset(value);
            }
        }
    }

    fn insert_char_without_reset(&mut self, value: char) {
        let byte_index = char_to_byte_index(&self.lines[self.cursor.line], self.cursor.column);
        self.lines[self.cursor.line].insert(byte_index, value);
        self.cursor.column += 1;
    }

    fn split_line(&mut self) {
        let byte_index = char_to_byte_index(&self.lines[self.cursor.line], self.cursor.column);
        let tail = self.lines[self.cursor.line].split_off(byte_index);
        self.lines.insert(self.cursor.line + 1, tail);
        self.cursor.line += 1;
        self.cursor.column = 0;
    }

    fn backspace(&mut self) {
        self.reset_navigation();
        if self.cursor.column > 0 {
            let end = char_to_byte_index(&self.lines[self.cursor.line], self.cursor.column);
            let start = char_to_byte_index(&self.lines[self.cursor.line], self.cursor.column - 1);
            self.lines[self.cursor.line].replace_range(start..end, "");
            self.cursor.column -= 1;
        } else if self.cursor.line > 0 {
            let current = self.lines.remove(self.cursor.line);
            self.cursor.line -= 1;
            self.cursor.column = self.lines[self.cursor.line].chars().count();
            self.lines[self.cursor.line].push_str(&current);
        }
    }

    fn delete_forward(&mut self) {
        self.reset_navigation();
        let line_len = self.lines[self.cursor.line].chars().count();
        if self.cursor.column < line_len {
            let start = char_to_byte_index(&self.lines[self.cursor.line], self.cursor.column);
            let end = char_to_byte_index(&self.lines[self.cursor.line], self.cursor.column + 1);
            self.lines[self.cursor.line].replace_range(start..end, "");
        } else if self.cursor.line + 1 < self.lines.len() {
            let next = self.lines.remove(self.cursor.line + 1);
            self.lines[self.cursor.line].push_str(&next);
        }
    }

    fn move_left(&mut self) {
        self.reset_navigation();
        if self.cursor.column > 0 {
            self.cursor.column -= 1;
        } else if self.cursor.line > 0 {
            self.cursor.line -= 1;
            self.cursor.column = self.lines[self.cursor.line].chars().count();
        }
        self.preferred_column = None;
    }

    fn move_right(&mut self) {
        self.reset_navigation();
        let line_len = self.lines[self.cursor.line].chars().count();
        if self.cursor.column < line_len {
            self.cursor.column += 1;
        } else if self.cursor.line + 1 < self.lines.len() {
            self.cursor.line += 1;
            self.cursor.column = 0;
        }
        self.preferred_column = None;
    }

    fn move_line_start(&mut self) {
        self.reset_navigation();
        self.cursor.column = 0;
        self.preferred_column = None;
    }

    fn move_line_end(&mut self) {
        self.reset_navigation();
        self.cursor.column = self.lines[self.cursor.line].chars().count();
        self.preferred_column = None;
    }

    fn move_up(&mut self) {
        if self.cursor.line == 0 {
            self.history_up();
            return;
        }
        self.reset_navigation();
        let preferred = self.preferred_column.unwrap_or(self.cursor.column);
        self.cursor.line -= 1;
        self.cursor.column = preferred.min(self.lines[self.cursor.line].chars().count());
        self.preferred_column = Some(preferred);
    }

    fn move_down(&mut self) {
        if self.cursor.line + 1 >= self.lines.len() {
            self.history_down();
            return;
        }
        self.reset_navigation();
        let preferred = self.preferred_column.unwrap_or(self.cursor.column);
        self.cursor.line += 1;
        self.cursor.column = preferred.min(self.lines[self.cursor.line].chars().count());
        self.preferred_column = Some(preferred);
    }

    fn move_word_left(&mut self) {
        self.reset_navigation();
        let line: Vec<char> = self.lines[self.cursor.line].chars().collect();
        let mut index = self.cursor.column.min(line.len());
        while index > 0 && !is_word_char(line[index - 1]) {
            index -= 1;
        }
        while index > 0 && is_word_char(line[index - 1]) {
            index -= 1;
        }
        self.cursor.column = index;
        self.preferred_column = None;
    }

    fn move_word_right(&mut self) {
        self.reset_navigation();
        let line: Vec<char> = self.lines[self.cursor.line].chars().collect();
        let mut index = self.cursor.column.min(line.len());
        while index < line.len() && is_word_char(line[index]) {
            index += 1;
        }
        while index < line.len() && !is_word_char(line[index]) {
            index += 1;
        }
        self.cursor.column = index;
        self.preferred_column = None;
    }

    fn kill_word(&mut self) {
        self.reset_navigation();
        if self.cursor.column > 0 {
            let line: Vec<char> = self.lines[self.cursor.line].chars().collect();
            let mut start = self.cursor.column.min(line.len());
            while start > 0 && !is_word_char(line[start - 1]) {
                start -= 1;
            }
            while start > 0 && is_word_char(line[start - 1]) {
                start -= 1;
            }
            let start_byte = char_to_byte_index(&self.lines[self.cursor.line], start);
            let end_byte = char_to_byte_index(&self.lines[self.cursor.line], self.cursor.column);
            self.lines[self.cursor.line].replace_range(start_byte..end_byte, "");
            self.cursor.column = start;
        } else if self.cursor.line > 0 {
            self.lines.remove(self.cursor.line);
            self.cursor.line -= 1;
            self.cursor.column = self.lines[self.cursor.line].chars().count();
        }
    }

    fn kill_to_line_start(&mut self) {
        self.reset_navigation();
        let byte_index = char_to_byte_index(&self.lines[self.cursor.line], self.cursor.column);
        self.lines[self.cursor.line].replace_range(..byte_index, "");
        self.cursor.column = 0;
    }

    fn kill_to_line_end(&mut self) {
        self.reset_navigation();
        let byte_index = char_to_byte_index(&self.lines[self.cursor.line], self.cursor.column);
        self.lines[self.cursor.line].truncate(byte_index);
    }

    fn reset_navigation(&mut self) {
        self.history_index = None;
        self.draft = None;
        self.preferred_column = None;
    }

    fn push_history(&mut self, value: String) {
        if self.history.last() == Some(&value) {
            return;
        }
        self.history.push(value);
        if self.history.len() > MAX_HISTORY {
            let overflow = self.history.len() - MAX_HISTORY;
            self.history.drain(..overflow);
        }
    }

    fn history_up(&mut self) {
        let Some(last) = self.history.len().checked_sub(1) else {
            return;
        };
        if self.history_index.is_none() {
            self.draft = Some(self.text());
        }
        let index = self
            .history_index
            .map_or(last, |index| index.saturating_sub(1));
        self.history_index = Some(index);
        self.replace_buffer(&self.history[index].clone());
    }

    fn history_down(&mut self) {
        let Some(index) = self.history_index else {
            return;
        };
        if index + 1 < self.history.len() {
            let next = index + 1;
            self.history_index = Some(next);
            self.replace_buffer(&self.history[next].clone());
        } else {
            self.history_index = None;
            let draft = self.draft.take().unwrap_or_default();
            self.replace_buffer(&draft);
        }
    }

    fn replace_buffer(&mut self, text: &str) {
        self.lines = text.split('\n').map(str::to_string).collect();
        if self.lines.is_empty() {
            self.lines.push(String::new());
        }
        self.cursor.line = self.lines.len() - 1;
        self.cursor.column = self.lines[self.cursor.line].chars().count();
        self.preferred_column = None;
    }

    fn accept_completion(&mut self) {
        let Some(completion) = self.completion.take() else {
            return;
        };
        let Some(candidate) = completion.candidates.get(completion.selected) else {
            return;
        };
        let text = self.text();
        let token_end = text
            .char_indices()
            .find(|(_, value)| value.is_whitespace())
            .map_or(text.len(), |(index, _)| index);
        let replacement = if candidate.starts_with('/') {
            candidate.clone()
        } else {
            format!("/{candidate}")
        };
        let suffix = &text[token_end..];
        self.replace_buffer(&format!("{replacement}{suffix}"));
    }
}

fn is_word_char(value: char) -> bool {
    !value.is_whitespace() && !value.is_ascii_punctuation()
}

fn char_to_byte_index(value: &str, char_index: usize) -> usize {
    value
        .char_indices()
        .nth(char_index)
        .map_or(value.len(), |(index, _)| index)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode, modifiers: KeyModifiers) -> InputEvent {
        InputEvent::Key(KeyEvent::new(code, modifiers))
    }

    fn type_text(composer: &mut Composer, text: &str) {
        for value in text.chars() {
            assert_eq!(
                composer.handle(key(KeyCode::Char(value), KeyModifiers::NONE)),
                Effect::None
            );
        }
    }

    #[test]
    fn insert_char_keeps_cjk_boundary() {
        // Arrange
        let mut composer = Composer::with_text("日本語");
        composer.handle(key(KeyCode::Left, KeyModifiers::NONE));
        composer.handle(key(KeyCode::Left, KeyModifiers::NONE));

        // Act
        composer.handle(key(KeyCode::Char('X'), KeyModifiers::NONE));

        // Assert
        assert_eq!(composer.text(), "日X本語");
        assert_eq!(composer.cursor(), Cursor { line: 0, column: 2 });
    }

    #[test]
    fn backspace_deletes_previous_char() {
        // Arrange
        let mut composer = Composer::with_text("あ🙂い");
        composer.handle(key(KeyCode::Left, KeyModifiers::NONE));

        // Act
        composer.handle(key(KeyCode::Backspace, KeyModifiers::NONE));

        // Assert
        assert_eq!(composer.text(), "あい");
        assert_eq!(composer.cursor().column, 1);
    }

    #[test]
    fn cursor_moves_by_word() {
        // Arrange
        let mut composer = Composer::with_text("one, two  三");

        // Act
        composer.handle(key(KeyCode::Char('b'), KeyModifiers::ALT));
        let first = composer.cursor().column;
        composer.handle(key(KeyCode::Char('b'), KeyModifiers::CONTROL));
        let second = composer.cursor().column;
        composer.handle(key(KeyCode::Char('f'), KeyModifiers::CONTROL));

        // Assert
        assert_eq!(first, 10);
        assert_eq!(second, 5);
        assert_eq!(composer.cursor().column, 10);
    }

    #[test]
    fn kill_word_line_edits() {
        // Arrange
        let mut composer = Composer::with_text("hello world");

        // Act
        composer.handle(key(KeyCode::Char('w'), KeyModifiers::CONTROL));

        // Assert the word operation before testing the line operations.
        assert_eq!(composer.text(), "hello ");

        // Act
        composer.handle(key(KeyCode::Char('u'), KeyModifiers::CONTROL));
        type_text(&mut composer, "new text");
        composer.handle(key(KeyCode::Home, KeyModifiers::NONE));
        composer.handle(key(KeyCode::Char('k'), KeyModifiers::CONTROL));

        // Assert
        assert_eq!(composer.text(), "");
    }

    #[test]
    fn multiline_cursor_navigation() {
        // Arrange
        let mut composer = Composer::with_text("abcd\nxy\nz");
        composer.handle(key(KeyCode::Home, KeyModifiers::NONE));
        composer.handle(key(KeyCode::Up, KeyModifiers::NONE));

        // Act
        composer.handle(key(KeyCode::Down, KeyModifiers::NONE));
        composer.handle(key(KeyCode::Down, KeyModifiers::NONE));

        // Assert
        assert_eq!(composer.cursor(), Cursor { line: 2, column: 0 });
    }

    #[test]
    fn enter_emits_send_effect() {
        // Arrange
        let mut composer = Composer::with_text("hello");

        // Act
        let effect = composer.handle(key(KeyCode::Enter, KeyModifiers::NONE));

        // Assert
        assert_eq!(effect, Effect::Send("hello".to_string()));
        assert_eq!(composer.text(), "hello");

        // Act: the caller accepts the send request.
        composer.accept_submission();

        // Assert
        assert!(composer.is_empty());
    }

    #[test]
    fn history_roundtrip_preserves_draft() {
        // Arrange
        let mut composer = Composer::with_text("first");
        assert_eq!(
            composer.handle(key(KeyCode::Enter, KeyModifiers::NONE)),
            Effect::Send("first".to_string())
        );
        composer.accept_submission();
        type_text(&mut composer, "draft");

        // Act
        composer.handle(key(KeyCode::Up, KeyModifiers::NONE));
        composer.handle(key(KeyCode::Down, KeyModifiers::NONE));

        // Assert
        assert_eq!(composer.text(), "draft");
    }

    #[test]
    fn history_navigation_only_at_edges() {
        // Arrange
        let mut composer = Composer::with_text("first");
        composer.handle(key(KeyCode::Enter, KeyModifiers::NONE));
        composer.accept_submission();
        composer.handle(InputEvent::Paste("line1\nline2".to_string()));
        composer.handle(key(KeyCode::Home, KeyModifiers::NONE));

        // Act
        composer.handle(key(KeyCode::Up, KeyModifiers::NONE));

        // Assert
        assert_eq!(composer.cursor().line, 0);
        assert_eq!(composer.text(), "line1\nline2");
    }

    #[test]
    fn paste_inserts_multiline_text() {
        // Arrange
        let mut composer = Composer::new();

        // Act
        composer.handle(InputEvent::Paste("a\n日本語".to_string()));

        // Assert
        assert_eq!(composer.lines(), &["a".to_string(), "日本語".to_string()]);
        assert_eq!(composer.cursor(), Cursor { line: 1, column: 3 });
    }

    #[test]
    fn paste_preserves_lines_beyond_visible_limit() {
        // Arrange
        let mut composer = Composer::new();
        let pasted = "1\n2\n3\n4\n5\n6\n7\n8\n9\n10";

        // Act
        composer.handle(InputEvent::Paste(pasted.to_string()));

        // Assert
        assert_eq!(composer.text(), pasted);
        assert_eq!(composer.lines().len(), 10);
        assert_eq!(composer.height(), 8);
    }

    #[test]
    fn ctrl_c_clears_or_quits() {
        // Arrange
        let mut empty = Composer::new();
        let mut non_empty = Composer::with_text("value");

        // Act
        let quit = empty.handle(key(KeyCode::Char('c'), KeyModifiers::CONTROL));
        let clear = non_empty.handle(key(KeyCode::Char('c'), KeyModifiers::CONTROL));

        // Assert
        assert_eq!(quit, Effect::Quit);
        assert_eq!(clear, Effect::None);
        assert!(non_empty.is_empty());
    }

    #[test]
    fn tab_accepts_candidate() {
        // Arrange
        let mut composer = Composer::with_text("/mo");
        composer.set_completion_candidates(["/models", "/model"]);

        // Act
        composer.handle(key(KeyCode::Tab, KeyModifiers::NONE));

        // Assert
        assert_eq!(composer.text(), "/models");
    }
}
