//! The find bar: query editing and drawing. The search itself runs in
//! `nuntio_term`.

use nuntio_render::{Rect, UiRect, UiText};
use nuntio_term::{MatchPosition, Rgb, Search, TermHandle, rgb};
use unicode_width::UnicodeWidthStr;

use crate::style::{UiMetrics, framed_box, hairline, mix};

/// Widest the bar gets, in cells.
const MAX_CELLS: f32 = 44.0;
/// Distance from the pane's top-right corner, in logical pixels.
const MARGIN: f64 = 6.0;
/// Space between the border and the text, in logical pixels.
const PADDING: f64 = 4.0;
/// Tint of the status text for errors and "no match".
const ERROR_RED: Rgb = rgb(0xff5555);

pub struct SearchBar {
    query: String,
    regex: bool,
    search: Option<Search>,
    /// The query is not a valid regex.
    error: Option<String>,
}

impl SearchBar {
    pub fn new() -> Self {
        Self {
            query: String::new(),
            regex: false,
            search: None,
            error: None,
        }
    }

    pub fn search_mut(&mut self) -> Option<&mut Search> {
        self.search.as_mut()
    }

    #[cfg_attr(not(feature = "debug-server"), allow(dead_code))]
    pub fn query(&self) -> &str {
        &self.query
    }

    #[cfg_attr(not(feature = "debug-server"), allow(dead_code))]
    pub fn regex(&self) -> bool {
        self.regex
    }

    /// Add typed or pasted text to the query, and search.
    pub fn append(&mut self, text: &str, term: &TermHandle) {
        self.query.push_str(text);
        self.update(term);
    }

    /// Delete the query's last character, and search.
    pub fn backspace(&mut self, term: &TermHandle) {
        self.query.pop();
        self.update(term);
    }

    /// Switch between plain text and regex search.
    pub fn toggle_regex(&mut self, term: &TermHandle) {
        self.regex = !self.regex;
        self.update(term);
    }

    /// Recompile after the query or mode changed and jump to the nearest
    /// match above the current position.
    fn update(&mut self, term: &TermHandle) {
        self.error = None;
        if self.query.is_empty() {
            self.search = None;
            return;
        }
        match Search::new(&self.query, self.regex) {
            Ok(search) => {
                let mut search = match &self.search {
                    Some(previous) => search.continue_from(previous),
                    None => search,
                };
                term.search(&mut search, true);
                self.search = Some(search);
            }
            Err(err) => {
                self.error = Some(err.to_string());
                self.search = None;
            }
        }
    }

    /// Go to the next match upwards (older) or downwards.
    pub fn next(&mut self, term: &TermHandle, up: bool) {
        if let Some(search) = self.search.as_mut() {
            term.search(search, up);
        }
    }

    /// Replace the query, e.g. with the selected text, and search.
    pub fn set_query(&mut self, query: String, term: &TermHandle) {
        self.query = query;
        self.update(term);
    }

    /// The regex error, "no match", or where the current match is ("3/17").
    fn status(&self) -> (String, bool) {
        if let Some(error) = &self.error {
            return (error.clone(), true);
        }
        let Some(search) = &self.search else {
            return (String::new(), false);
        };
        match search.position() {
            None => ("no match".into(), true),
            Some(position) => (format_position(position), false),
        }
    }

    /// Area of the bar: the top-right corner of the pane.
    fn bounds(&self, pane: Rect, metrics: UiMetrics) -> Rect {
        let cell = metrics.cell;
        let margin = metrics.logical(MARGIN);
        let padding = metrics.logical(PADDING);
        let width = (MAX_CELLS * cell.width as f32 + 2.0 * padding).min(pane.width - 2.0 * margin);
        Rect {
            x: pane.x + pane.width - margin - width,
            y: pane.y + margin,
            width: width.max(0.0),
            height: cell.height as f32 + 2.0 * padding,
        }
    }

    pub fn contains(&self, pane: Rect, metrics: UiMetrics, x: f32, y: f32) -> bool {
        self.bounds(pane, metrics).contains(x, y)
    }

    pub fn draw(
        &self,
        pane: Rect,
        metrics: UiMetrics,
        background: Rgb,
        foreground: Rgb,
    ) -> (Vec<UiRect>, Vec<UiText>) {
        let cell = metrics.cell;
        let bounds = self.bounds(pane, metrics);
        let padding = (bounds.height - cell.height as f32) / 2.0;
        let columns = ((bounds.width - 2.0 * padding) / cell.width as f32).max(0.0) as usize;

        let rects = framed_box(bounds, hairline(metrics.scale), background, foreground);

        // "query▏" on the left, ".* status" on the right, cut to fit.
        let mut right = String::new();
        if self.regex {
            right.push_str(".*");
        }
        let (status, is_error) = self.status();
        if !status.is_empty() {
            if !right.is_empty() {
                right.push(' ');
            }
            right.push_str(&status);
        }
        let right = crate::style::truncate(&right, columns.saturating_sub(4));
        let right_width = right.width();
        let query = tail(&self.query, columns.saturating_sub(right_width + 2));

        let right_color = if is_error {
            mix(foreground, ERROR_RED, 0.6)
        } else {
            mix(foreground, background, 0.4)
        };
        let (x, y) = (bounds.x + padding, bounds.y + padding);
        let cw = cell.width as f32;
        let texts = vec![
            UiText::new(x, y, format!("{query}▏"), foreground),
            UiText::new(
                x + columns.saturating_sub(right_width) as f32 * cw,
                y,
                right,
                right_color,
            ),
        ];
        (rects, texts)
    }
}

/// "3/17", or "999+" when counting stopped before the current match.
fn format_position(position: MatchPosition) -> String {
    let more = if position.more { "+" } else { "" };
    match position.index {
        Some(index) => format!("{index}/{}{more}", position.total),
        None => format!("{}{more}", position.total),
    }
}

/// The last `columns` display columns of `text` (keeps the typed end visible).
fn tail(text: &str, columns: usize) -> String {
    let mut width = 0;
    let mut out: Vec<char> = text
        .chars()
        .rev()
        .take_while(|c| {
            width += unicode_width::UnicodeWidthChar::width(*c).unwrap_or(0);
            width <= columns
        })
        .collect();
    out.reverse();
    out.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::style::test_metrics::*;
    const PANE: Rect = Rect {
        x: 0.0,
        y: 30.0,
        width: 1000.0,
        height: 600.0,
    };

    #[test]
    fn sits_top_right_and_fits_narrow_panes() {
        let bar = SearchBar::new();
        let b = bar.bounds(PANE, METRICS);
        assert_eq!((b.x + b.width, b.y), (994.0, 36.0));
        assert_eq!(b.width, 448.0);

        let narrow = Rect {
            width: 200.0,
            ..PANE
        };
        assert_eq!(bar.bounds(narrow, METRICS).width, 188.0);
    }

    #[test]
    fn text_shows_query_cursor_and_status() {
        let mut bar = SearchBar::new();
        bar.query = "foo".into();
        bar.regex = true;
        bar.error = Some("bad".into());
        let (_, texts) = bar.draw(PANE, METRICS, Rgb::default(), Rgb::default());
        assert_eq!(texts[0].text, "foo▏");
        assert_eq!(texts[1].text, ".* bad");
        // Right-aligned: 44 columns wide, starting after the padding.
        assert_eq!(texts[1].x, 546.0 + 4.0 + (44 - 6) as f32 * 10.0);
    }

    #[test]
    fn match_positions() {
        let position = |index, total, more| MatchPosition { index, total, more };
        assert_eq!(format_position(position(Some(3), 17, false)), "3/17");
        assert_eq!(format_position(position(Some(3), 999, true)), "3/999+");
        assert_eq!(format_position(position(None, 999, true)), "999+");
    }

    #[test]
    fn long_queries_keep_their_end_visible() {
        assert_eq!(tail("abcdef", 3), "def");
        assert_eq!(tail("ab", 3), "ab");
    }

    #[test]
    fn status_is_cut_by_display_width() {
        assert_eq!(crate::style::truncate("日本語", 4), "日…");
        let mut bar = SearchBar::new();
        bar.error = Some("日本語のエラー".repeat(10));
        let narrow = Rect {
            width: 120.0,
            ..PANE
        };
        let (_, texts) = bar.draw(narrow, METRICS, Rgb::default(), Rgb::default());
        assert!(texts[1].text.width() <= 10);
        assert!(texts[1].x >= narrow.x);
    }
}
