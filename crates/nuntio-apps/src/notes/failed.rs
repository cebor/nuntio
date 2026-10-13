//! The screen of a notes app that cannot start because the notes folder is
//! unusable: only the reason is shown.

use ratatui::crossterm::event::{KeyCode, KeyEvent};

pub struct Failed {
    /// The notes folder as shown in the header, empty if there is none.
    pub dir_label: String,
    pub message: String,
}

impl Failed {
    /// Whether `key` leaves the screen.
    pub fn quits(key: KeyEvent) -> bool {
        matches!(key.code, KeyCode::Esc | KeyCode::Char('q'))
    }
}

#[cfg(test)]
mod tests {
    use ratatui::crossterm::event::KeyModifiers;

    use super::*;

    #[test]
    fn q_and_escape_quit() {
        let press = |code, modifiers| Failed::quits(KeyEvent::new(code, modifiers));
        assert!(press(KeyCode::Char('q'), KeyModifiers::NONE));
        assert!(press(KeyCode::Char('q'), KeyModifiers::CONTROL));
        assert!(press(KeyCode::Esc, KeyModifiers::NONE));
        assert!(!press(KeyCode::Char('x'), KeyModifiers::NONE));
    }
}
