//! Colors and text tones both apps draw with. Only ANSI colors and
//! modifiers, so the apps follow the terminal's theme.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

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

/// A line of key hints: ` key what  key what`, the key in the accent color
/// and what it does dimmed.
pub fn hints(pairs: &[(&str, &str)]) -> Line<'static> {
    let mut spans = vec![Span::raw(" ")];
    for (i, (key, what)) in pairs.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw("  "));
        }
        spans.push(Span::styled((*key).to_owned(), Tone::Accent.style()));
        spans.push(Span::styled(format!(" {what}"), Tone::Dim.style()));
    }
    Line::from(spans)
}
