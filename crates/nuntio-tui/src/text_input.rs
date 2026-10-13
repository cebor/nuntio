//! A line of editable text with a cursor, for filters, prompts and input
//! fields. Each app maps its own key type to [`Edit`].

use unicode_width::UnicodeWidthStr;

/// What a key does to a [`TextInput`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edit {
    Insert(char),
    /// Delete the character before the cursor.
    Backspace,
    /// Delete the character at the cursor.
    Delete,
    /// Delete everything.
    Clear,
    Left,
    Right,
    Home,
    End,
}

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

    /// The cursor's position in characters.
    pub fn cursor(&self) -> usize {
        self.cursor
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

    /// Apply `edit`; returns whether the text changed. Clearing counts as a
    /// change even when the text was empty.
    pub fn apply(&mut self, edit: Edit) -> bool {
        let len = self.text.chars().count();
        match edit {
            Edit::Insert(c) => {
                let at = self.byte_index(self.cursor);
                self.text.insert(at, c);
                self.cursor += 1;
                true
            }
            Edit::Backspace if self.cursor > 0 => {
                self.cursor -= 1;
                let at = self.byte_index(self.cursor);
                self.text.remove(at);
                true
            }
            Edit::Delete if self.cursor < len => {
                let at = self.byte_index(self.cursor);
                self.text.remove(at);
                true
            }
            Edit::Clear => {
                self.text.clear();
                self.cursor = 0;
                true
            }
            Edit::Left => {
                self.cursor = self.cursor.saturating_sub(1);
                false
            }
            Edit::Right => {
                self.cursor = (self.cursor + 1).min(len);
                false
            }
            Edit::Home => {
                self.cursor = 0;
                false
            }
            Edit::End => {
                self.cursor = len;
                false
            }
            Edit::Backspace | Edit::Delete => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edits_at_the_cursor() {
        let mut input = TextInput::new("%H:%M");
        input.apply(Edit::Home);
        assert!(input.apply(Edit::Insert('ä')));
        input.apply(Edit::End);
        assert!(input.apply(Edit::Backspace));
        input.apply(Edit::Left);
        assert!(input.apply(Edit::Delete));
        assert_eq!(input.text(), "ä%H:");
        assert_eq!(input.cursor(), 4);
        assert!(input.apply(Edit::Clear));
        assert_eq!(input.text(), "");
    }

    #[test]
    fn moving_and_edits_without_effect_change_nothing() {
        let mut input = TextInput::new("ab");
        assert!(!input.apply(Edit::Left));
        assert!(!input.apply(Edit::Right));
        assert!(!input.apply(Edit::Delete));
        assert!(!input.apply(Edit::Home));
        assert!(!input.apply(Edit::Backspace));
        assert_eq!(input.text(), "ab");
    }

    #[test]
    fn insert_str_drops_line_breaks_and_measures_wide_chars() {
        let mut input = TextInput::new("");
        input.insert_str("a\r\n日");
        assert_eq!(input.text(), "a日");
        assert_eq!(input.cursor_width(), 3);
        input.apply(Edit::Home);
        assert_eq!(input.cursor_width(), 0);
        input.apply(Edit::End);
        assert_eq!(input.cursor_width(), 3);
    }
}
