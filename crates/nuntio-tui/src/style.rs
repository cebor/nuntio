//! Colors, text tones and the small drawing pieces both apps share: panels,
//! headings, centered text and key hints. Only ANSI colors and modifiers, so
//! the apps follow the terminal's theme.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Paragraph};

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

/// The style of titles and headings: the accent color, bold.
pub fn heading() -> Style {
    Style::new().fg(ACCENT).add_modifier(Modifier::BOLD)
}

/// The style of a panel's border: the accent color while `active`, quiet
/// otherwise.
pub fn border(active: bool) -> Style {
    if active {
        Tone::Accent.style()
    } else {
        Tone::Dim.style()
    }
}

/// A rounded, bordered panel; it takes the accent color while `active`.
pub fn panel(active: bool) -> Block<'static> {
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(border(active))
}

/// `lines`, centered in `area` both ways.
pub fn center(frame: &mut Frame, area: Rect, lines: Vec<Line>) {
    let height = lines.len() as u16;
    let [_, middle, _] = Layout::vertical([
        Constraint::Fill(1),
        Constraint::Length(height),
        Constraint::Fill(1),
    ])
    .areas(area);
    frame.render_widget(Paragraph::new(lines).centered(), middle);
}

/// The screen of an app that has no room to draw.
pub fn too_small(frame: &mut Frame) {
    center(frame, frame.area(), vec![Line::from("Terminal too small")]);
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
