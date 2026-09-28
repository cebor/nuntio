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
}

pub const WHITE: Rgb = rgb(0xffffff);
pub const BLACK: Rgb = rgb(0x000000);

/// `a` moved towards `b` by `t` (0.0 = `a`, 1.0 = `b`).
pub fn mix(a: Rgb, b: Rgb, t: f32) -> Rgb {
    let m = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Rgb {
        r: m(a.r, b.r),
        g: m(a.g, b.g),
        b: m(a.b, b.b),
    }
}

/// Background of the tab and status bars: darker than the terminal.
pub fn bar_background(background: Rgb) -> Rgb {
    mix(background, BLACK, 0.18)
}

/// A sharp rectangle filled with `color`.
pub fn rect(x: f32, y: f32, width: f32, height: f32, color: Rgb) -> UiRect {
    UiRect::fill(Rect::new(x, y, width, height), color)
}

/// A box of `bounds` with a `border` pixels wide frame, as the find bar
/// and the link hint draw it.
pub fn framed_box(bounds: Rect, border: f32, background: Rgb, foreground: Rgb) -> Vec<UiRect> {
    vec![
        UiRect::fill(bounds, mix(background, foreground, 0.35)),
        UiRect::fill(bounds.inset(border), mix(background, foreground, 0.12)),
    ]
}

/// Width of a one-pixel line at `scale`, in whole physical pixels.
pub fn hairline(scale: f64) -> f32 {
    scale.round().max(1.0) as f32
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
    fn colors_mix_linearly() {
        assert_eq!(mix(BLACK, WHITE, 0.0), BLACK);
        assert_eq!(mix(BLACK, WHITE, 1.0), WHITE);
        assert_eq!(mix(BLACK, WHITE, 0.5), rgb(0x808080));
    }
}
