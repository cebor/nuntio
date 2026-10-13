//! How keys edit the single-line text input of the filter and the prompts.

use nuntio_tui::text_input::{Edit, TextInput};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Feed `key` to `input`; returns whether the key was used.
pub fn edit_input(input: &mut TextInput, key: KeyEvent) -> bool {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let edit = match key.code {
        KeyCode::Char(c) if !ctrl && !alt => Edit::Insert(c),
        KeyCode::Char('u') if ctrl => Edit::Clear,
        KeyCode::Char('a') if ctrl => Edit::Home,
        KeyCode::Char('e') if ctrl => Edit::End,
        KeyCode::Backspace => Edit::Backspace,
        KeyCode::Delete => Edit::Delete,
        KeyCode::Left => Edit::Left,
        KeyCode::Right => Edit::Right,
        KeyCode::Home => Edit::Home,
        KeyCode::End => Edit::End,
        _ => return false,
    };
    input.apply(edit);
    true
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
        assert!(edit_input(&mut input, press(KeyCode::Left)));
        assert!(edit_input(&mut input, press(KeyCode::Char('é'))));
        assert_eq!(input.text(), "aéb");
        assert!(edit_input(&mut input, press(KeyCode::Backspace)));
        assert!(edit_input(&mut input, press(KeyCode::Delete)));
        assert_eq!(input.text(), "a");
        assert!(!edit_input(&mut input, press(KeyCode::Enter)));
        assert!(!edit_input(
            &mut input,
            KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL)
        ));
    }

    #[test]
    fn ctrl_u_clears_and_home_end_move() {
        let mut input = TextInput::new("hello");
        edit_input(&mut input, press(KeyCode::Home));
        assert_eq!(input.cursor_width(), 0);
        edit_input(&mut input, press(KeyCode::End));
        assert_eq!(input.cursor_width(), 5);
        edit_input(
            &mut input,
            KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL),
        );
        assert_eq!(input.text(), "");
    }
}
