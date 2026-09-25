//! A one-line notification at the bottom of the window, e.g. for config
//! errors. Clicking it shows the next message, the × dismisses it.

use nuntio_render::{CellMetrics, UiRect, UiText};
use nuntio_term::Rgb;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

const CONFIG_ERROR: &str = "Config error, not applied";
const CONFIG_WARNING: &str = "Config warning";

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
    /// Index of the message shown.
    shown: usize,
}

impl Banner {
    pub fn new(severity: Severity, title: &'static str, messages: Vec<String>) -> Option<Self> {
        (!messages.is_empty()).then_some(Self {
            severity,
            title,
            messages,
            shown: 0,
        })
    }

    /// Problems with the config; an error means it was not applied.
    pub fn config(severity: Severity, messages: Vec<String>) -> Option<Self> {
        let title = match severity {
            Severity::Error => CONFIG_ERROR,
            Severity::Warning => CONFIG_WARNING,
        };
        Self::new(severity, title, messages)
    }

    /// About the config, so a reload replaces it.
    pub fn is_config(&self) -> bool {
        self.title == CONFIG_ERROR || self.title == CONFIG_WARNING
    }

    fn text(&self) -> String {
        let message = &self.messages[self.shown];
        match self.messages.len() {
            1 => format!("{}: {message}", self.title),
            n => format!("{} ({}/{n}): {message}", self.title, self.shown + 1),
        }
    }

    /// Handle a click at `x`: the × (or a banner with only one message)
    /// closes it, anywhere else shows the next message. Returns whether the
    /// banner stays open.
    pub fn click(&mut self, x: f32, window_width: f32, cell: CellMetrics, scale: f64) -> bool {
        let (_, height) = self.bounds(0.0, cell, scale);
        let padding = (height - cell.height as f32) / 2.0;
        let on_close = x >= window_width - padding - 3.0 * cell.width as f32;
        if on_close || self.messages.len() == 1 {
            return false;
        }
        self.shown = (self.shown + 1) % self.messages.len();
        true
    }

    /// Top edge and height of the banner, which ends at `bottom`.
    pub fn bounds(&self, bottom: f32, cell: CellMetrics, scale: f64) -> (f32, f32) {
        let padding = (4.0 * scale).round() as f32;
        let height = cell.height as f32 + 2.0 * padding;
        (bottom - height, height)
    }

    pub fn contains(&self, y: f32, bottom: f32, cell: CellMetrics, scale: f64) -> bool {
        let (top, height) = self.bounds(bottom, cell, scale);
        y >= top && y < top + height
    }

    pub fn draw(
        &self,
        window_width: f32,
        bottom: f32,
        cell: CellMetrics,
        scale: f64,
    ) -> (UiRect, UiText) {
        let (top, height) = self.bounds(bottom, cell, scale);
        let (background, foreground) = match self.severity {
            Severity::Error => (rgb(0xb3261e), rgb(0xffffff)),
            Severity::Warning => (rgb(0x7a5c00), rgb(0xffffff)),
        };
        let padding = (height - cell.height as f32) / 2.0;
        // Leave room for the closing "×" at the right.
        let columns = ((window_width - 2.0 * padding) / cell.width as f32) as usize;
        let text = fit(&self.text(), columns.saturating_sub(2));
        // Pad by display width, so the × stays at the edge after wide text.
        let pad = columns.saturating_sub(2).saturating_sub(text.width());
        let text = format!("{text}{} ×", " ".repeat(pad));
        (
            UiRect {
                x: 0.0,
                y: top,
                width: window_width,
                height,
                color: background,
                radius: 0.0,
            },
            UiText {
                x: padding,
                y: top + padding,
                text,
                color: foreground,
                bold: false,
                small: false,
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
        assert_eq!(banner.text(), "Config warning (1/3): a");
        let banner = Banner::new(Severity::Warning, "Link", vec!["x".into()]).unwrap();
        assert_eq!(banner.text(), "Link: x");
    }

    #[test]
    fn clicks_page_through_messages_and_the_cross_closes() {
        let mut banner =
            Banner::config(Severity::Warning, vec!["a".into(), "b".into(), "c".into()]).unwrap();
        assert!(banner.click(10.0, 400.0, CELL, 1.0));
        assert_eq!(banner.text(), "Config warning (2/3): b");
        assert!(banner.click(10.0, 400.0, CELL, 1.0));
        assert!(banner.click(10.0, 400.0, CELL, 1.0));
        assert_eq!(banner.text(), "Config warning (1/3): a", "wraps around");
        assert!(!banner.click(395.0, 400.0, CELL, 1.0), "the ×");

        let mut single = Banner::new(Severity::Warning, "Link", vec!["x".into()]).unwrap();
        assert!(!single.click(10.0, 400.0, CELL, 1.0));
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

    #[test]
    fn wide_text_keeps_the_close_button_at_the_edge() {
        let banner = Banner::config(Severity::Warning, vec!["日本".into()]).unwrap();
        let (_, text) = banner.draw(400.0, 600.0, CELL, 1.0);
        assert_eq!(text.text.width(), 39);
        assert!(text.text.ends_with(" ×"));
    }
}
