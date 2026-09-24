//! A one-line notification at the bottom of the window, e.g. for config
//! errors. Clicking it dismisses it.

use nuntio_render::{CellMetrics, UiRect, UiText};
use nuntio_term::Rgb;
use unicode_width::UnicodeWidthChar;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Banner {
    pub severity: Severity,
    /// What the messages are about, shown before the first one.
    pub title: &'static str,
    pub messages: Vec<String>,
}

impl Banner {
    pub fn new(severity: Severity, title: &'static str, messages: Vec<String>) -> Option<Self> {
        (!messages.is_empty()).then_some(Self {
            severity,
            title,
            messages,
        })
    }

    /// Problems with the config; an error means it was not applied.
    pub fn config(severity: Severity, messages: Vec<String>) -> Option<Self> {
        let title = match severity {
            Severity::Error => "Config error, not applied",
            Severity::Warning => "Config warning",
        };
        Self::new(severity, title, messages)
    }

    fn text(&self) -> String {
        let more = match self.messages.len() {
            1 => String::new(),
            n => format!(" (+{} more, see log)", n - 1),
        };
        format!("{}: {}{more}", self.title, self.messages[0])
    }

    /// Top edge and height of the banner in a window of `height` pixels.
    fn bounds(&self, window_height: f32, cell: CellMetrics, scale: f64) -> (f32, f32) {
        let padding = (4.0 * scale).round() as f32;
        let height = cell.height as f32 + 2.0 * padding;
        (window_height - height, height)
    }

    pub fn contains(&self, y: f32, window_height: f32, cell: CellMetrics, scale: f64) -> bool {
        let (top, height) = self.bounds(window_height, cell, scale);
        y >= top && y < top + height
    }

    pub fn draw(
        &self,
        window_width: f32,
        window_height: f32,
        cell: CellMetrics,
        scale: f64,
    ) -> (UiRect, UiText) {
        let (top, height) = self.bounds(window_height, cell, scale);
        let (background, foreground) = match self.severity {
            Severity::Error => (rgb(0xb3261e), rgb(0xffffff)),
            Severity::Warning => (rgb(0x7a5c00), rgb(0xffffff)),
        };
        let padding = (height - cell.height as f32) / 2.0;
        // Leave room for the closing "×" at the right.
        let columns = ((window_width - 2.0 * padding) / cell.width as f32) as usize;
        let text = fit(&self.text(), columns.saturating_sub(2));
        let text = format!("{text:<width$} ×", width = columns.saturating_sub(2));
        (
            UiRect {
                x: 0.0,
                y: top,
                width: window_width,
                height,
                color: background,
            },
            UiText {
                x: padding,
                y: top + padding,
                text,
                color: foreground,
                bold: false,
            },
        )
    }
}

/// Cut `text` to `columns` display columns.
fn fit(text: &str, columns: usize) -> String {
    let mut used = 0;
    text.chars()
        .take_while(|c| {
            used += c.width().unwrap_or(0);
            used <= columns
        })
        .collect()
}

const fn rgb(hex: u32) -> Rgb {
    Rgb {
        r: (hex >> 16) as u8,
        g: (hex >> 8) as u8,
        b: hex as u8,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CELL: CellMetrics = CellMetrics {
        width: 10,
        height: 20,
        baseline: 15,
        underline_y: 17,
        stroke: 1,
        strikeout_y: 10,
    };

    #[test]
    fn empty_messages_give_no_banner() {
        assert_eq!(Banner::config(Severity::Error, vec![]), None);
    }

    #[test]
    fn text_summarizes_messages() {
        let banner =
            Banner::config(Severity::Warning, vec!["a".into(), "b".into(), "c".into()]).unwrap();
        assert_eq!(banner.text(), "Config warning: a (+2 more, see log)");
        let banner = Banner::new(Severity::Warning, "Link", vec!["x".into()]).unwrap();
        assert_eq!(banner.text(), "Link: x");
    }

    #[test]
    fn sits_at_the_bottom_and_fits_the_width() {
        let banner = Banner::config(Severity::Error, vec!["x".repeat(500)]).unwrap();
        let (rect, text) = banner.draw(400.0, 600.0, CELL, 1.0);
        assert_eq!((rect.y, rect.height), (572.0, 28.0));
        assert!(banner.contains(580.0, 600.0, CELL, 1.0));
        assert!(!banner.contains(560.0, 600.0, CELL, 1.0));
        // 400px minus padding fit 39 columns.
        assert_eq!(text.text.chars().count(), 39);
        assert!(text.text.ends_with(" ×"));
    }
}
