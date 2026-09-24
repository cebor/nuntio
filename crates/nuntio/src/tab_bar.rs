//! Tab bar layout, hit testing and drawing primitives.

use nuntio_render::{CellMetrics, UiRect, UiText};
use nuntio_term::Rgb;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Widest a tab gets, in cells, so a few tabs don't stretch across the window.
const MAX_TAB_CELLS: f32 = 28.0;
const BELL_COLOR: Rgb = Rgb {
    r: 0xe5,
    g: 0xc0,
    b: 0x7b,
};

#[derive(Debug, Clone, Copy, PartialEq)]
struct Slot {
    x: f32,
    width: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BarHit {
    Tab(usize),
    Close(usize),
    /// Free space in the bar (drag area for moving the window).
    Empty,
}

/// What a tab shows.
#[derive(Debug, Clone)]
pub struct TabLabel {
    pub title: String,
    pub active: bool,
    pub activity: bool,
    pub bell: bool,
}

#[derive(Debug, Clone)]
pub struct TabBar {
    pub height: f32,
    width: f32,
    padding: f32,
    cell: CellMetrics,
    slots: Vec<Slot>,
}

impl TabBar {
    /// Lay out `count` tabs across a bar of `width` pixels. `left_inset`
    /// keeps room free at the left edge (macOS window buttons).
    pub fn new(width: f32, count: usize, cell: CellMetrics, scale: f64, left_inset: f32) -> Self {
        let padding = (5.0 * scale).round() as f32;
        let height = cell.height as f32 + 2.0 * padding;
        let available = (width - left_inset).max(0.0);
        let tab_width = (available / count.max(1) as f32)
            .min(MAX_TAB_CELLS * cell.width as f32)
            .floor();
        let slots = (0..count)
            .map(|i| Slot {
                x: left_inset + i as f32 * tab_width,
                width: tab_width,
            })
            .collect();
        Self {
            height,
            width,
            padding,
            cell,
            slots,
        }
    }

    /// Square area of the close button inside a tab.
    fn close_rect(&self, slot: Slot) -> (f32, f32, f32) {
        let size = self.cell.height as f32;
        (
            slot.x + slot.width - self.padding - size,
            self.padding,
            size,
        )
    }

    pub fn hit(&self, x: f32, y: f32) -> Option<BarHit> {
        if y < 0.0 || y >= self.height || x < 0.0 || x >= self.width {
            return None;
        }
        let Some(index) = self.slot_at(x) else {
            return Some(BarHit::Empty);
        };
        let (cx, cy, size) = self.close_rect(self.slots[index]);
        if x >= cx && x < cx + size && y >= cy && y < cy + size {
            Some(BarHit::Close(index))
        } else {
            Some(BarHit::Tab(index))
        }
    }

    /// Tab under a horizontal position, for clicks and drag-reordering.
    pub fn slot_at(&self, x: f32) -> Option<usize> {
        self.slots
            .iter()
            .position(|s| x >= s.x && x < s.x + s.width)
    }

    pub fn draw(
        &self,
        labels: &[TabLabel],
        hovered: Option<usize>,
        background: Rgb,
        foreground: Rgb,
    ) -> (Vec<UiRect>, Vec<UiText>) {
        let bar_bg = mix(background, foreground, 0.10);
        let separator = mix(background, foreground, 0.22);
        let inactive_text = mix(foreground, background, 0.40);
        let (cw, ch) = (self.cell.width as f32, self.cell.height as f32);

        let mut rects = vec![UiRect {
            x: 0.0,
            y: 0.0,
            width: self.width,
            height: self.height,
            color: bar_bg,
        }];
        let mut texts = Vec::new();

        for (i, (slot, label)) in self.slots.iter().zip(labels).enumerate() {
            if label.active {
                rects.push(UiRect {
                    x: slot.x,
                    y: 0.0,
                    width: slot.width,
                    height: self.height,
                    color: background,
                });
            }
            // Separator on the right edge.
            rects.push(UiRect {
                x: slot.x + slot.width - 1.0,
                y: self.padding,
                width: 1.0,
                height: ch,
                color: separator,
            });

            // Layout: [pad][indicator][title, centered][close][pad]
            let side = ch;
            let text_x0 = slot.x + self.padding + side;
            let text_width = slot.width - 2.0 * (self.padding + side);
            let max_cells = (text_width / cw).floor().max(0.0) as usize;
            let title = truncate(&label.title, max_cells);
            let title_cells = title.width() as f32;
            let color = if label.active {
                foreground
            } else {
                inactive_text
            };
            texts.push(UiText {
                x: (text_x0 + (text_width - title_cells * cw) / 2.0).floor(),
                y: self.padding,
                text: title,
                color,
                bold: label.active,
            });

            let indicator_x = slot.x + self.padding + ((side - cw) / 2.0).floor();
            if label.bell {
                texts.push(UiText {
                    x: indicator_x,
                    y: self.padding,
                    text: "●".into(),
                    color: BELL_COLOR,
                    bold: false,
                });
            } else if label.activity {
                texts.push(UiText {
                    x: indicator_x,
                    y: self.padding,
                    text: "•".into(),
                    color: inactive_text,
                    bold: false,
                });
            }

            if label.active || hovered == Some(i) {
                let (cx, cy, _) = self.close_rect(*slot);
                texts.push(UiText {
                    x: cx + ((side - cw) / 2.0).floor(),
                    y: cy,
                    text: "×".into(),
                    color: inactive_text,
                    bold: false,
                });
            }
        }
        (rects, texts)
    }
}

/// Cut `text` to at most `cells` columns, ending in an ellipsis if cut.
fn truncate(text: &str, cells: usize) -> String {
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

fn mix(a: Rgb, b: Rgb, t: f32) -> Rgb {
    let m = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Rgb {
        r: m(a.r, b.r),
        g: m(a.g, b.g),
        b: m(a.b, b.b),
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
    fn tabs_share_the_width_up_to_a_maximum() {
        let bar = TabBar::new(1000.0, 2, CELL, 1.0, 0.0);
        assert_eq!(bar.slots[0].width, 280.0, "capped at 28 cells");
        assert_eq!(bar.slots[1].x, 280.0);

        let bar = TabBar::new(1000.0, 8, CELL, 1.0, 0.0);
        assert_eq!(bar.slots[0].width, 125.0);
        assert_eq!(bar.height, 30.0);
    }

    #[test]
    fn hit_testing() {
        let bar = TabBar::new(1000.0, 2, CELL, 1.0, 0.0);
        assert_eq!(bar.hit(10.0, 10.0), Some(BarHit::Tab(0)));
        assert_eq!(bar.hit(290.0, 10.0), Some(BarHit::Tab(1)));
        // Close button: 20px square, 5px from the tab's right edge.
        assert_eq!(bar.hit(270.0, 10.0), Some(BarHit::Close(0)));
        assert_eq!(bar.hit(800.0, 10.0), Some(BarHit::Empty));
        assert_eq!(bar.hit(10.0, 40.0), None, "below the bar");
    }

    #[test]
    fn left_inset_is_respected() {
        let bar = TabBar::new(1000.0, 1, CELL, 1.0, 80.0);
        assert_eq!(bar.hit(40.0, 10.0), Some(BarHit::Empty));
        assert_eq!(bar.hit(90.0, 10.0), Some(BarHit::Tab(0)));
    }

    #[test]
    fn titles_are_truncated_by_display_width() {
        assert_eq!(truncate("short", 10), "short");
        assert_eq!(truncate("a long title", 6), "a lon…");
        assert_eq!(truncate("日本語タイトル", 5), "日本…");
        assert_eq!(truncate("abc", 0), "");
    }
}
