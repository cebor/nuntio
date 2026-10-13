//! Colors and shapes shared by the tab bar, status bar and overlays.

use nuntio_render::{CellMetrics, Rect, UiRect};
use nuntio_term::{Rgb, rgb};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Sizes the UI is laid out with, in physical pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct UiMetrics {
    /// Cells of the terminal font.
    pub cell: CellMetrics,
    /// Cells of the small UI font (`UiText::small`).
    pub small: CellMetrics,
    /// Physical pixels per logical pixel.
    pub scale: f64,
}

impl UiMetrics {
    /// `px` logical pixels in whole physical pixels.
    pub fn logical(&self, px: f64) -> f32 {
        (px * self.scale).round() as f32
    }

    /// `px` logical pixels in whole physical pixels, in single precision as
    /// the bars' icon and inset sizes are computed.
    pub fn physical(&self, px: f32) -> f32 {
        (px * self.scale as f32).round()
    }

    /// Width of a one-pixel line, in whole physical pixels.
    pub fn hairline(&self) -> f32 {
        self.scale.round().max(1.0) as f32
    }
}

pub const WHITE: Rgb = rgb(0xffffff);
pub const BLACK: Rgb = rgb(0x000000);
/// The blue of informational banners and the update badge.
pub const INFO_COLOR: Rgb = rgb(0x1f5fa8);

/// Perceived brightness, 0.0 (black) to 1.0 (white).
pub fn luminance(c: Rgb) -> f32 {
    nuntio_config::Color {
        r: c.r,
        g: c.g,
        b: c.b,
    }
    .luminance()
}

/// `a` moved towards `b` by `t` (0.0 = `a`, 1.0 = `b`).
pub fn mix(a: Rgb, b: Rgb, t: f32) -> Rgb {
    let m = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Rgb {
        r: m(a.r, b.r),
        g: m(a.g, b.g),
        b: m(a.b, b.b),
    }
}

/// The colors the bars and overlays are drawn in, derived once per frame
/// from the terminal's `background` and `foreground`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Palette {
    pub background: Rgb,
    pub foreground: Rgb,
    /// Background of the tab and status bars: darker than the terminal.
    pub bar: Rgb,
    /// A hairline between neighbouring items of a bar.
    pub separator: Rgb,
    /// The lines between panes.
    pub divider: Rgb,
    /// Frame of the find bar, link hint and menus.
    pub frame: Rgb,
    /// Fill of the find bar, link hint and menus.
    pub fill: Rgb,
}

impl Palette {
    pub fn new(background: Rgb, foreground: Rgb) -> Self {
        let bar = mix(background, BLACK, 0.18);
        Self {
            background,
            foreground,
            bar,
            separator: mix(bar, foreground, 0.15),
            divider: mix(background, foreground, 0.25),
            frame: mix(background, foreground, 0.35),
            fill: mix(background, foreground, 0.12),
        }
    }

    /// A box of `bounds` with a `border` pixels wide frame, as the find bar
    /// and the link hint draw it.
    pub fn framed_box(&self, bounds: Rect, border: f32) -> Vec<UiRect> {
        vec![
            UiRect::fill(bounds, self.frame),
            UiRect::fill(bounds.inset(border), self.fill),
        ]
    }
}

/// A sharp rectangle filled with `color`.
pub fn rect(x: f32, y: f32, width: f32, height: f32, color: Rgb) -> UiRect {
    UiRect::fill(Rect::new(x, y, width, height), color)
}

/// A corner of a pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Corner {
    TopRight,
    BottomLeft,
    BottomRight,
}

/// A box of `size` in `corner` of `pane`, `margin` away from both edges and
/// narrowed to fit between the margins.
pub fn corner_box(pane: Rect, (width, height): (f32, f32), margin: f32, corner: Corner) -> Rect {
    let width = width.min(pane.width - 2.0 * margin);
    let right = pane.x + pane.width - margin - width;
    let bottom = pane.y + pane.height - margin - height;
    let (x, y) = match corner {
        Corner::TopRight => (right, pane.y + margin),
        Corner::BottomLeft => (pane.x + margin, bottom),
        Corner::BottomRight => (right, bottom),
    };
    Rect {
        x,
        y,
        width: width.max(0.0),
        height,
    }
}

/// Cut `text` to at most `cells` columns, ending in an ellipsis if cut.
pub fn truncate(text: &str, cells: usize) -> String {
    if text.width() <= cells {
        return text.to_owned();
    }
    if cells == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in text.chars() {
        let w = c.width().unwrap_or(0);
        if used + w > cells - 1 {
            break;
        }
        used += w;
        out.push(c);
    }
    out.push('…');
    out
}

/// The last `cells` columns of `text`.
pub fn tail(text: &str, cells: usize) -> &str {
    let mut used = 0;
    let mut start = text.len();
    for (i, c) in text.char_indices().rev() {
        used += c.width().unwrap_or(0);
        if used > cells {
            break;
        }
        start = i;
    }
    &text[start..]
}

/// Keep the last `cells` columns of `text`, starting with an ellipsis if cut.
pub fn truncate_start(text: &str, cells: usize) -> String {
    if text.width() <= cells {
        return text.to_owned();
    }
    if cells == 0 {
        return String::new();
    }
    format!("…{}", tail(text, cells - 1))
}

/// Metrics for layout tests: 10×20 pixel cells, in both fonts, at scale 1.
#[cfg(test)]
pub mod test_metrics {
    use super::*;

    pub const CELL: CellMetrics = CellMetrics {
        width: 10,
        height: 20,
        baseline: 15,
        underline_y: 17,
        stroke: 1,
        strikeout_y: 10,
    };

    pub const METRICS: UiMetrics = UiMetrics {
        cell: CELL,
        small: CELL,
        scale: 1.0,
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_is_truncated_by_display_width() {
        assert_eq!(truncate("short", 10), "short");
        assert_eq!(truncate("a long title", 6), "a lon…");
        assert_eq!(truncate("日本語タイトル", 5), "日本…");
        assert_eq!(truncate("abc", 0), "");
    }

    #[test]
    fn the_tail_keeps_the_end_visible() {
        assert_eq!(tail("abcdef", 3), "def");
        assert_eq!(tail("ab", 3), "ab");
    }

    #[test]
    fn colors_mix_linearly() {
        assert_eq!(mix(BLACK, WHITE, 0.0), BLACK);
        assert_eq!(mix(BLACK, WHITE, 1.0), WHITE);
        assert_eq!(mix(BLACK, WHITE, 0.5), rgb(0x808080));
    }
}
