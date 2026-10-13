//! A one-line notification at the bottom of the window, e.g. for config
//! errors. Clicking it shows the next message (or opens its link), the ×
//! dismisses it.

use nuntio_render::{Rect, UiRect, UiText};
use nuntio_term::rgb;

use crate::style::{INFO_COLOR, UiMetrics};
use unicode_width::UnicodeWidthStr;

/// Space above and below the text, in logical pixels.
const PADDING: f64 = 4.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Info,
    Warning,
    Error,
}

/// What a banner is about. Banners of one topic and severity merge; some
/// topics are dismissed or replaced by their own events.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Topic {
    Config,
    Update,
    UpdateCheck,
    Link,
    Shell,
    Settings,
    Notes,
    WorkingDirectory,
    Paste,
    Close,
    Gpu,
    #[cfg(windows)]
    Wsl,
}

impl Topic {
    /// Shown before the first message.
    pub fn title(self, severity: Severity) -> &'static str {
        match self {
            Self::Config => match severity {
                Severity::Error => "Config error, not applied",
                Severity::Warning | Severity::Info => "Config warning",
            },
            Self::Update => "Update",
            Self::UpdateCheck => "Update check",
            Self::Link => "Link",
            Self::Shell => "Shell",
            Self::Settings => "Settings",
            Self::Notes => "Notes",
            Self::WorkingDirectory => "Working directory",
            Self::Paste => "Paste",
            Self::Close => "Close",
            Self::Gpu => "GPU error",
            #[cfg(windows)]
            Self::Wsl => "WSL",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Banner {
    pub severity: Severity,
    pub topic: Topic,
    pub messages: Vec<String>,
    /// Opened by a click outside the ×.
    pub url: Option<String>,
    /// Index of the message shown.
    shown: usize,
}

impl Banner {
    pub fn new(severity: Severity, topic: Topic, messages: Vec<String>) -> Option<Self> {
        (!messages.is_empty()).then_some(Self {
            severity,
            topic,
            messages,
            url: None,
            shown: 0,
        })
    }

    pub fn with_url(self, url: String) -> Self {
        Self {
            url: Some(url),
            ..self
        }
    }

    /// Problems with the config; an error means it was not applied.
    pub fn config(severity: Severity, messages: Vec<String>) -> Option<Self> {
        Self::new(severity, Topic::Config, messages)
    }

    /// What the messages are about, shown before the first one.
    pub fn title(&self) -> &'static str {
        self.topic.title(self.severity)
    }

    /// About the config, so a reload replaces it.
    pub fn is_config(&self) -> bool {
        self.topic == Topic::Config
    }

    /// Drop `message`. Returns whether any message is left.
    pub fn remove_message(&mut self, message: &str) -> bool {
        self.messages.retain(|m| m != message);
        if self.shown >= self.messages.len() {
            self.shown = 0;
        }
        !self.messages.is_empty()
    }

    fn text(&self) -> String {
        let message = &self.messages[self.shown];
        match self.messages.len() {
            1 => format!("{}: {message}", self.title()),
            n => format!("{} ({}/{n}): {message}", self.title(), self.shown + 1),
        }
    }

    /// Handle a click at `x`: the × (or a banner with only one message)
    /// closes it, anywhere else shows the next message. Returns whether the
    /// banner stays open.
    pub fn click(&mut self, x: f32, window_width: f32, metrics: UiMetrics) -> bool {
        if self.on_close(x, window_width, metrics) || self.messages.len() == 1 {
            return false;
        }
        self.shown = (self.shown + 1) % self.messages.len();
        true
    }

    /// `x` is on the ×.
    pub fn on_close(&self, x: f32, window_width: f32, metrics: UiMetrics) -> bool {
        let padding = Layout::new(0.0, metrics).padding;
        x >= window_width - padding - 3.0 * metrics.cell.width as f32
    }

    /// Top edge and height of the banner, which ends at `bottom`.
    pub fn bounds(&self, bottom: f32, metrics: UiMetrics) -> (f32, f32) {
        let Layout { top, height, .. } = Layout::new(bottom, metrics);
        (top, height)
    }

    pub fn contains(&self, y: f32, bottom: f32, metrics: UiMetrics) -> bool {
        let (top, height) = self.bounds(bottom, metrics);
        y >= top && y < top + height
    }

    pub fn draw(
        &self,
        window_width: f32,
        bottom: f32,
        metrics: UiMetrics,
    ) -> (Vec<UiRect>, Vec<UiText>) {
        let cell = metrics.cell;
        let Layout {
            top,
            height,
            padding,
        } = Layout::new(bottom, metrics);
        let (background, foreground) = match self.severity {
            Severity::Error => (rgb(0xb3261e), rgb(0xffffff)),
            Severity::Warning => (rgb(0x7a5c00), rgb(0xffffff)),
            Severity::Info => (INFO_COLOR, rgb(0xffffff)),
        };
        // Leave room for the closing "×" at the right.
        let columns = ((window_width - 2.0 * padding) / cell.width as f32) as usize;
        let text = crate::style::truncate(&self.text(), columns.saturating_sub(2));
        // Pad by display width, so the × stays at the edge after wide text.
        let pad = columns.saturating_sub(2).saturating_sub(text.width());
        let text = format!("{text}{} ×", " ".repeat(pad));
        (
            vec![UiRect::fill(
                Rect::new(0.0, top, window_width, height),
                background,
            )],
            vec![UiText::new(padding, top + padding, text, foreground)],
        )
    }
}

/// Where a banner sits: the strip of `height` at `top`, with `padding`
/// above and below the text.
struct Layout {
    top: f32,
    height: f32,
    padding: f32,
}

impl Layout {
    /// The banner that ends at `bottom`.
    fn new(bottom: f32, metrics: UiMetrics) -> Self {
        let cell_height = metrics.cell.height as f32;
        let height = cell_height + 2.0 * metrics.logical(PADDING);
        Self {
            top: bottom - height,
            height,
            padding: (height - cell_height) / 2.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::style::test_metrics::*;

    #[test]
    fn empty_messages_give_no_banner() {
        assert_eq!(Banner::config(Severity::Error, vec![]), None);
    }

    #[test]
    fn text_summarizes_messages() {
        let banner =
            Banner::config(Severity::Warning, vec!["a".into(), "b".into(), "c".into()]).unwrap();
        assert_eq!(banner.text(), "Config warning (1/3): a");
        let banner = Banner::new(Severity::Warning, Topic::Link, vec!["x".into()]).unwrap();
        assert_eq!(banner.text(), "Link: x");
    }

    #[test]
    fn clicks_page_through_messages_and_the_cross_closes() {
        let mut banner =
            Banner::config(Severity::Warning, vec!["a".into(), "b".into(), "c".into()]).unwrap();
        assert!(banner.click(10.0, 400.0, METRICS));
        assert_eq!(banner.text(), "Config warning (2/3): b");
        assert!(banner.click(10.0, 400.0, METRICS));
        assert!(banner.click(10.0, 400.0, METRICS));
        assert_eq!(banner.text(), "Config warning (1/3): a", "wraps around");
        assert!(!banner.click(395.0, 400.0, METRICS), "the ×");

        let mut single = Banner::new(Severity::Warning, Topic::Link, vec!["x".into()]).unwrap();
        assert!(!single.click(10.0, 400.0, METRICS));
    }

    #[test]
    fn links_open_outside_the_cross() {
        let banner = Banner::new(Severity::Info, Topic::Update, vec!["x".into()])
            .unwrap()
            .with_url("https://example.com".into());
        assert_eq!(banner.url.as_deref(), Some("https://example.com"));
        assert!(!banner.on_close(10.0, 400.0, METRICS));
        assert!(banner.on_close(395.0, 400.0, METRICS));
    }

    #[test]
    fn sits_at_the_bottom_and_fits_the_width() {
        let banner = Banner::config(Severity::Error, vec!["x".repeat(500)]).unwrap();
        let (rects, texts) = banner.draw(400.0, 600.0, METRICS);
        assert_eq!((rects[0].y, rects[0].height), (572.0, 28.0));
        assert!(banner.contains(580.0, 600.0, METRICS));
        assert!(!banner.contains(560.0, 600.0, METRICS));
        // 400px minus padding fit 39 columns.
        assert_eq!(texts[0].text.chars().count(), 39);
        assert!(texts[0].text.ends_with(" ×"));
    }

    #[test]
    fn wide_text_keeps_the_close_button_at_the_edge() {
        let banner = Banner::config(Severity::Warning, vec!["日本".into()]).unwrap();
        let (_, texts) = banner.draw(400.0, 600.0, METRICS);
        assert_eq!(texts[0].text.width(), 39);
        assert!(texts[0].text.ends_with(" ×"));
    }
}
