//! The find bar: query editing and drawing. The search itself runs in
//! `nuntio_term`.

use nuntio_render::{CellMetrics, UiRect, UiText};
use nuntio_term::{Rgb, Search, TermHandle};
use unicode_width::UnicodeWidthStr;

use crate::pane_tree::Rect;
use crate::tab_bar::mix;

/// Widest the bar gets, in cells.
const MAX_CELLS: f32 = 44.0;

pub struct SearchBar {
    pub query: String,
    pub regex: bool,
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

    /// Recompile after the query or mode changed and jump to the nearest
    /// match above the current position.
    pub fn update(&mut self, term: &TermHandle) {
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
                self.error = Some(err);
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

    fn status(&self) -> &str {
        if let Some(error) = &self.error {
            error
        } else if self.search.as_ref().is_some_and(|s| !s.has_match()) {
            "no match"
        } else {
            ""
        }
    }

    /// Area of the bar: the top-right corner of the pane.
    fn bounds(&self, pane: Rect, cell: CellMetrics, scale: f64) -> Rect {
        let margin = (6.0 * scale).round() as f32;
        let padding = (4.0 * scale).round() as f32;
        let width = (MAX_CELLS * cell.width as f32 + 2.0 * padding).min(pane.width - 2.0 * margin);
        Rect {
            x: pane.x + pane.width - margin - width,
            y: pane.y + margin,
            width: width.max(0.0),
            height: cell.height as f32 + 2.0 * padding,
        }
    }

    pub fn contains(&self, pane: Rect, cell: CellMetrics, scale: f64, x: f32, y: f32) -> bool {
        self.bounds(pane, cell, scale).contains(x, y)
    }

    pub fn draw(
        &self,
        pane: Rect,
        cell: CellMetrics,
        scale: f64,
        background: Rgb,
        foreground: Rgb,
    ) -> (Vec<UiRect>, Vec<UiText>) {
        let bounds = self.bounds(pane, cell, scale);
        let padding = (bounds.height - cell.height as f32) / 2.0;
        let border = scale.round().max(1.0) as f32;
        let columns = ((bounds.width - 2.0 * padding) / cell.width as f32).max(0.0) as usize;

        let rect = |r: Rect, color| UiRect {
            x: r.x,
            y: r.y,
            width: r.width,
            height: r.height,
            color,
        };
        let inner = Rect {
            x: bounds.x + border,
            y: bounds.y + border,
            width: bounds.width - 2.0 * border,
            height: bounds.height - 2.0 * border,
        };
        let rects = vec![
            rect(bounds, mix(background, foreground, 0.35)),
            rect(inner, mix(background, foreground, 0.12)),
        ];

        // "query▏" on the left, ".* status" on the right, cut to fit.
        let mut right = String::new();
        if self.regex {
            right.push_str(".*");
        }
        let status = self.status();
        if !status.is_empty() {
            if !right.is_empty() {
                right.push(' ');
            }
            right.push_str(status);
        }
        let right_width = right.width().min(columns.saturating_sub(4));
        let right: String = right.chars().take(right_width).collect();
        let query = tail(&self.query, columns.saturating_sub(right_width + 2));

        let right_color = if status.is_empty() {
            mix(foreground, background, 0.4)
        } else {
            mix(
                foreground,
                Rgb {
                    r: 0xff,
                    g: 0x55,
                    b: 0x55,
                },
                0.6,
            )
        };
        let (x, y) = (bounds.x + padding, bounds.y + padding);
        let cw = cell.width as f32;
        let texts = vec![
            UiText {
                x,
                y,
                text: format!("{query}▏"),
                color: foreground,
                bold: false,
            },
            UiText {
                x: x + (columns - right.width()) as f32 * cw,
                y,
                text: right,
                color: right_color,
                bold: false,
            },
        ];
        (rects, texts)
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

    const CELL: CellMetrics = CellMetrics {
        width: 10,
        height: 20,
        baseline: 15,
        underline_y: 17,
        stroke: 1,
        strikeout_y: 10,
    };
    const PANE: Rect = Rect {
        x: 0.0,
        y: 30.0,
        width: 1000.0,
        height: 600.0,
    };

    #[test]
    fn sits_top_right_and_fits_narrow_panes() {
        let bar = SearchBar::new();
        let b = bar.bounds(PANE, CELL, 1.0);
        assert_eq!((b.x + b.width, b.y), (994.0, 36.0));
        assert_eq!(b.width, 448.0);

        let narrow = Rect {
            width: 200.0,
            ..PANE
        };
        assert_eq!(bar.bounds(narrow, CELL, 1.0).width, 188.0);
    }

    #[test]
    fn text_shows_query_cursor_and_status() {
        let mut bar = SearchBar::new();
        bar.query = "foo".into();
        bar.regex = true;
        bar.error = Some("bad".into());
        let (_, texts) = bar.draw(PANE, CELL, 1.0, Rgb::default(), Rgb::default());
        assert_eq!(texts[0].text, "foo▏");
        assert_eq!(texts[1].text, ".* bad");
        // Right-aligned: 44 columns wide, starting after the padding.
        assert_eq!(texts[1].x, 546.0 + 4.0 + (44 - 6) as f32 * 10.0);
    }

    #[test]
    fn long_queries_keep_their_end_visible() {
        assert_eq!(tail("abcdef", 3), "def");
        assert_eq!(tail("ab", 3), "ab");
    }
}
