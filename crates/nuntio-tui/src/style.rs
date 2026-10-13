//! Colors and text tones both apps draw with. Only ANSI colors and
//! modifiers, so the apps follow the terminal's theme.

use ratatui::style::{Color, Modifier, Style};

/// The color of titles, borders and prompts.
pub const ACCENT: Color = Color::Cyan;

/// How a line of text is meant: its style is the same in both apps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Normal,
    Dim,
    Accent,
    Warn,
    Error,
}

impl Tone {
    pub fn style(self) -> Style {
        match self {
            Tone::Normal => Style::new(),
            Tone::Dim => Style::new().add_modifier(Modifier::DIM),
            Tone::Accent => Style::new().fg(ACCENT),
            Tone::Warn => Style::new().fg(Color::Yellow),
            Tone::Error => Style::new().fg(Color::Red),
        }
    }
}
