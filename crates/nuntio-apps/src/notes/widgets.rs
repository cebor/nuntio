//! Single-line text input for the filter and the prompts.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use unicode_width::UnicodeWidthStr;

/// One line of editable text.
#[derive(Debug, Clone, Default)]
pub struct TextInput {
    text: String,
    /// In characters.
    cursor: usize,
}

impl TextInput {
    pub fn new(text: &str) -> Self {
        Self {
            text: text.to_owned(),
            cursor: text.chars().count(),
        }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    /// Display width of the text before the cursor.
    pub fn cursor_width(&self) -> u16 {
        let before = &self.text[..self.byte_index(self.cursor)];
        u16::try_from(before.width()).unwrap_or(u16::MAX)
    }

    fn byte_index(&self, char_index: usize) -> usize {
        self.text
            .char_indices()
            .nth(char_index)
            .map_or(self.text.len(), |(i, _)| i)
    }

    /// Insert `s` at the cursor; line breaks are dropped.
    pub fn insert_str(&mut self, s: &str) {
        let s: String = s.chars().filter(|c| !matches!(c, '\n' | '\r')).collect();
        let at = self.byte_index(self.cursor);
        self.text.insert_str(at, &s);
        self.cursor += s.chars().count();
    }

    /// Handle a key; returns whether it was used.
    pub fn key(&mut self, key: KeyEvent) -> bool {
        let len = self.text.chars().count();
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        match key.code {
            KeyCode::Char(c) if !ctrl && !alt => {
                let at = self.byte_index(self.cursor);
                self.text.insert(at, c);
                self.cursor += 1;
            }
            KeyCode::Char('u') if ctrl => {
                self.text.clear();
                self.cursor = 0;
            }
            KeyCode::Char('a') if ctrl => self.cursor = 0,
            KeyCode::Char('e') if ctrl => self.cursor = len,
            KeyCode::Backspace if self.cursor > 0 => {
                self.cursor -= 1;
                let at = self.byte_index(self.cursor);
                self.text.remove(at);
            }
            KeyCode::Delete if self.cursor < len => {
                let at = self.byte_index(self.cursor);
                self.text.remove(at);
            }
            KeyCode::Left => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Right => self.cursor = (self.cursor + 1).min(len),
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = len,
            // A key that would edit but has nothing to act on is still ours.
            KeyCode::Backspace | KeyCode::Delete => {}
            _ => return false,
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn edits_at_the_cursor() {
        let mut input = TextInput::new("ab");
        assert!(input.key(press(KeyCode::Left)));
        assert!(input.key(press(KeyCode::Char('é'))));
        assert_eq!(input.text(), "aéb");
        assert!(input.key(press(KeyCode::Backspace)));
        assert!(input.key(press(KeyCode::Delete)));
        assert_eq!(input.text(), "a");
        assert!(!input.key(press(KeyCode::Enter)));
        assert!(!input.key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL)));
    }

    #[test]
    fn ctrl_u_clears_and_home_end_move() {
        let mut input = TextInput::new("hello");
        input.key(press(KeyCode::Home));
        assert_eq!(input.cursor_width(), 0);
        input.key(press(KeyCode::End));
        assert_eq!(input.cursor_width(), 5);
        input.key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert_eq!(input.text(), "");
    }

    #[test]
    fn insert_str_drops_line_breaks_and_measures_wide_chars() {
        let mut input = TextInput::new("");
        input.insert_str("a\r\n日");
        assert_eq!(input.text(), "a日");
        assert_eq!(input.cursor_width(), 3);
    }
}
