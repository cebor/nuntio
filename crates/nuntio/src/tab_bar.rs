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
/// Hover color of the close-window button, as on Windows.
const CLOSE_HOVER: Rgb = Rgb {
    r: 0xe8,
    g: 0x11,
    b: 0x23,
};
const WHITE: Rgb = Rgb {
    r: 0xff,
    g: 0xff,
    b: 0xff,
};
const BLACK: Rgb = Rgb { r: 0, g: 0, b: 0 };
/// Space above and below the text, in logical pixels.
const BAR_PADDING: f64 = 7.0;
/// Space between the bar's edges and the tab pills, in logical pixels.
const PILL_INSET: f64 = 4.0;
/// Horizontal gap between two tab pills, in logical pixels.
const PILL_GAP: f64 = 4.0;
const PILL_RADIUS: f64 = 6.0;
/// Inset and corner radius of the highlight behind a hovered close button,
/// in logical pixels.
const CLOSE_INSET: f64 = 2.0;
const CLOSE_RADIUS: f64 = 4.0;
/// Window control button size in logical pixels (Windows' caption buttons).
const CONTROL_WIDTH: f64 = 46.0;
const CONTROL_ICON: f64 = 10.0;

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
    Minimize,
    Maximize,
    CloseWindow,
}

const CONTROLS: [BarHit; 3] = [BarHit::Minimize, BarHit::Maximize, BarHit::CloseWindow];

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
    /// Vertical inset of the pills, and horizontal margin of the tab row.
    inset: f32,
    gap: f32,
    radius: f32,
    scale: f32,
    cell: CellMetrics,
    slots: Vec<Slot>,
    /// Width of one window control button; 0 without controls.
    control_width: f32,
}

impl TabBar {
    /// Lay out `count` tabs across a bar of `width` pixels. `left_inset`
    /// keeps room free at the left edge (macOS window buttons);
    /// `window_controls` adds minimize/maximize/close at the right edge.
    pub fn new(
        width: f32,
        count: usize,
        cell: CellMetrics,
        scale: f64,
        left_inset: f32,
        window_controls: bool,
    ) -> Self {
        let logical = |px: f64| (px * scale).round() as f32;
        let padding = logical(BAR_PADDING);
        let inset = logical(PILL_INSET);
        let height = cell.height as f32 + 2.0 * padding;
        let control_width = if window_controls {
            logical(CONTROL_WIDTH)
        } else {
            0.0
        };
        let start = left_inset + inset;
        let available = (width - start - inset - 3.0 * control_width).max(0.0);
        let tab_width = (available / count.max(1) as f32)
            .min(MAX_TAB_CELLS * cell.width as f32)
            .floor();
        let slots = (0..count)
            .map(|i| Slot {
                x: start + i as f32 * tab_width,
                width: tab_width,
            })
            .collect();
        Self {
            height,
            width,
            padding,
            inset,
            gap: logical(PILL_GAP),
            radius: logical(PILL_RADIUS),
            scale: scale as f32,
            cell,
            slots,
            control_width,
        }
    }

    /// Left edge of the window control buttons.
    fn controls_x(&self) -> f32 {
        self.width - 3.0 * self.control_width
    }

    /// Narrow tabs have no close button, so clicks on them select the tab.
    /// There must be room for the button, the indicator and two cells of
    /// title.
    fn has_close(&self, slot: Slot) -> bool {
        let side = self.cell.height as f32;
        slot.width >= 2.0 * (self.padding + side) + 2.0 * self.cell.width as f32
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
        if self.control_width > 0.0 && x >= self.controls_x() {
            let i = ((x - self.controls_x()) / self.control_width) as usize;
            return Some(CONTROLS[i.min(2)]);
        }
        let Some(index) = self.slot_at(x) else {
            return Some(BarHit::Empty);
        };
        let slot = self.slots[index];
        let (cx, cy, size) = self.close_rect(slot);
        if self.has_close(slot) && x >= cx && x < cx + size && y >= cy && y < cy + size {
            Some(BarHit::Close(index))
        } else {
            Some(BarHit::Tab(index))
        }
    }

    /// Tab under a horizontal position, for clicks.
    fn slot_at(&self, x: f32) -> Option<usize> {
        self.slots
            .iter()
            .position(|s| x >= s.x && x < s.x + s.width)
    }

    /// Where a dragged tab goes when the pointer is at `x`: the tab under
    /// it, or the first or last position beyond the row's ends.
    pub fn drop_index(&self, x: f32) -> Option<usize> {
        let first = self.slots.first()?;
        Some(
            self.slot_at(x)
                .unwrap_or(if x < first.x { 0 } else { self.slots.len() - 1 }),
        )
    }

    pub fn draw(
        &self,
        labels: &[TabLabel],
        hovered: Option<BarHit>,
        maximized: bool,
        background: Rgb,
        foreground: Rgb,
    ) -> (Vec<UiRect>, Vec<UiText>) {
        let hovered_tab = match hovered {
            Some(BarHit::Tab(i) | BarHit::Close(i)) => Some(i),
            _ => None,
        };
        // Darker than the terminal, with the active tab as a lighter pill.
        let bar_bg = bar_background(background);
        let active_bg = mix(background, foreground, 0.08);
        let hover_bg = mix(bar_bg, foreground, 0.08);
        let inactive_text = mix(foreground, background, 0.45);
        let (cw, ch) = (self.cell.width as f32, self.cell.height as f32);

        let mut rects = vec![UiRect {
            x: 0.0,
            y: 0.0,
            width: self.width,
            height: self.height,
            color: bar_bg,
            radius: 0.0,
        }];
        let mut texts = Vec::new();

        for (i, (slot, label)) in self.slots.iter().zip(labels).enumerate() {
            let pill = if label.active {
                Some(active_bg)
            } else if hovered_tab == Some(i) {
                Some(hover_bg)
            } else {
                None
            };
            if let Some(color) = pill {
                rects.push(self.pill(slot.x + self.gap / 2.0, slot.width - self.gap, color));
            }

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
                bold: false,
                small: false,
            });

            let indicator_x = slot.x + self.padding + ((side - cw) / 2.0).floor();
            if label.bell {
                texts.push(UiText {
                    x: indicator_x,
                    y: self.padding,
                    text: "●".into(),
                    color: BELL_COLOR,
                    bold: false,
                    small: false,
                });
            } else if label.activity {
                texts.push(UiText {
                    x: indicator_x,
                    y: self.padding,
                    text: "•".into(),
                    color: inactive_text,
                    bold: false,
                    small: false,
                });
            }

            if (label.active || hovered_tab == Some(i)) && self.has_close(*slot) {
                let (cx, cy, size) = self.close_rect(*slot);
                let close_hovered = hovered == Some(BarHit::Close(i));
                if close_hovered {
                    let under = if label.active { active_bg } else { hover_bg };
                    let inset = (CLOSE_INSET as f32 * self.scale).round();
                    rects.push(UiRect {
                        x: cx + inset,
                        y: cy + inset,
                        width: size - 2.0 * inset,
                        height: size - 2.0 * inset,
                        color: mix(under, foreground, 0.10),
                        radius: (CLOSE_RADIUS as f32 * self.scale).round(),
                    });
                }
                texts.push(UiText {
                    x: cx + ((side - cw) / 2.0).floor(),
                    y: cy,
                    text: "×".into(),
                    color: if close_hovered {
                        foreground
                    } else {
                        inactive_text
                    },
                    bold: false,
                    small: false,
                });
            }
        }
        if self.control_width > 0.0 {
            self.draw_controls(&mut rects, hovered, maximized, bar_bg, foreground);
        }
        (rects, texts)
    }

    /// A rounded background spanning the bar's height minus the inset.
    fn pill(&self, x: f32, width: f32, color: Rgb) -> UiRect {
        UiRect {
            x,
            y: self.inset,
            width,
            height: self.height - 2.0 * self.inset,
            color,
            radius: self.radius,
        }
    }

    /// Minimize, maximize/restore and close, drawn from rectangles so the
    /// icons stay crisp at any scale.
    fn draw_controls(
        &self,
        rects: &mut Vec<UiRect>,
        hovered: Option<BarHit>,
        maximized: bool,
        bar_bg: Rgb,
        foreground: Rgb,
    ) {
        let stroke = self.scale.round().max(1.0);
        let icon = (CONTROL_ICON as f32 * self.scale).round();
        if let Some(i) = CONTROLS.iter().position(|&c| hovered == Some(c)) {
            let hover_bg = if CONTROLS[i] == BarHit::CloseWindow {
                CLOSE_HOVER
            } else {
                mix(bar_bg, foreground, 0.12)
            };
            let bx = self.controls_x() + i as f32 * self.control_width;
            let gap = self.gap / 2.0;
            rects.push(self.pill(bx + gap, self.control_width - 2.0 * gap, hover_bg));
        }
        let mut rect = |x: f32, y: f32, width: f32, height: f32, color: Rgb| {
            rects.push(UiRect {
                x,
                y,
                width,
                height,
                color,
                radius: 0.0,
            });
        };
        // An outlined square.
        let outline = |rect: &mut dyn FnMut(f32, f32, f32, f32, Rgb), x, y, size, color| {
            rect(x, y, size, stroke, color);
            rect(x, y + size - stroke, size, stroke, color);
            rect(x, y, stroke, size, color);
            rect(x + size - stroke, y, stroke, size, color);
        };

        for (i, control) in CONTROLS.into_iter().enumerate() {
            let bx = self.controls_x() + i as f32 * self.control_width;
            let color = if control == BarHit::CloseWindow && hovered == Some(control) {
                WHITE
            } else {
                foreground
            };

            // Icon box, centered in the button.
            let x0 = (bx + (self.control_width - icon) / 2.0).floor();
            let y0 = ((self.height - icon) / 2.0).floor();
            match control {
                BarHit::Minimize => {
                    rect(x0, (y0 + icon / 2.0).floor(), icon, stroke, color);
                }
                BarHit::Maximize if maximized => {
                    // Restore: a front square with another peeking out behind.
                    let offset = (2.0 * self.scale).round();
                    let front = icon - offset;
                    rect(x0 + offset, y0, front, stroke, color);
                    rect(x0 + icon - stroke, y0, stroke, front, color);
                    outline(&mut rect, x0, y0 + offset, front, color);
                }
                BarHit::Maximize => outline(&mut rect, x0, y0, icon, color),
                _ => {
                    // Close: two diagonals built from stroke-sized steps.
                    let steps = (icon / stroke) as usize;
                    for k in 0..steps {
                        let d = k as f32 * stroke;
                        rect(x0 + d, y0 + d, stroke, stroke, color);
                        rect(x0 + icon - stroke - d, y0 + d, stroke, stroke, color);
                    }
                }
            }
        }
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

/// Background of the tab and status bars: darker than the terminal.
pub fn bar_background(background: Rgb) -> Rgb {
    mix(background, BLACK, 0.18)
}

pub fn mix(a: Rgb, b: Rgb, t: f32) -> Rgb {
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
        let bar = TabBar::new(1000.0, 2, CELL, 1.0, 0.0, false);
        assert_eq!(bar.slots[0].x, 4.0, "margin at the left edge");
        assert_eq!(bar.slots[0].width, 280.0, "capped at 28 cells");
        assert_eq!(bar.slots[1].x, 284.0);

        // 4px margin on both sides leave 992px for 8 tabs.
        let bar = TabBar::new(1000.0, 8, CELL, 1.0, 0.0, false);
        assert_eq!(bar.slots[0].width, 124.0);
        assert_eq!(bar.height, 34.0);
    }

    #[test]
    fn hit_testing() {
        let bar = TabBar::new(1000.0, 2, CELL, 1.0, 0.0, false);
        assert_eq!(bar.hit(10.0, 10.0), Some(BarHit::Tab(0)));
        assert_eq!(bar.hit(290.0, 10.0), Some(BarHit::Tab(1)));
        // Close button: 20px square, 7px from the tab's right edge.
        assert_eq!(bar.hit(270.0, 10.0), Some(BarHit::Close(0)));
        assert_eq!(bar.hit(2.0, 10.0), Some(BarHit::Empty), "left margin");
        assert_eq!(bar.hit(800.0, 10.0), Some(BarHit::Empty));
        assert_eq!(bar.hit(10.0, 34.0), None, "below the bar");
    }

    #[test]
    fn narrow_tabs_have_no_close_button() {
        // 30 tabs leave 33px each: too narrow for the close button.
        let bar = TabBar::new(1000.0, 30, CELL, 1.0, 0.0, false);
        let slot = bar.slots[0];
        let (cx, cy, _) = bar.close_rect(slot);
        assert_eq!(bar.hit(cx + 1.0, cy + 1.0), Some(BarHit::Tab(0)));
    }

    #[test]
    fn drops_beyond_the_row_go_to_its_ends() {
        let bar = TabBar::new(1000.0, 3, CELL, 1.0, 80.0, false);
        assert_eq!(bar.drop_index(10.0), Some(0));
        assert_eq!(bar.drop_index(90.0), Some(0));
        assert_eq!(bar.drop_index(400.0), Some(1));
        assert_eq!(bar.drop_index(990.0), Some(2));
        assert_eq!(
            TabBar::new(1000.0, 0, CELL, 1.0, 0.0, false).drop_index(5.0),
            None
        );
    }

    #[test]
    fn left_inset_is_respected() {
        let bar = TabBar::new(1000.0, 1, CELL, 1.0, 80.0, false);
        assert_eq!(bar.hit(40.0, 10.0), Some(BarHit::Empty));
        assert_eq!(bar.hit(90.0, 10.0), Some(BarHit::Tab(0)));
    }

    #[test]
    fn window_controls_take_the_right_edge() {
        let bar = TabBar::new(1000.0, 8, CELL, 1.0, 0.0, true);
        // 3 × 46px of controls and the margins leave 854px for 8 tabs.
        assert_eq!(bar.slots[0].width, 106.0);
        assert_eq!(bar.hit(870.0, 10.0), Some(BarHit::Minimize));
        assert_eq!(bar.hit(930.0, 10.0), Some(BarHit::Maximize));
        assert_eq!(bar.hit(999.0, 10.0), Some(BarHit::CloseWindow));
        assert_eq!(bar.hit(861.0, 10.0), Some(BarHit::Empty));
    }

    #[test]
    fn hovered_close_button_is_highlighted() {
        let bar = TabBar::new(1000.0, 2, CELL, 1.0, 0.0, false);
        let labels = [true, false].map(|active| TabLabel {
            title: "~".into(),
            active,
            activity: false,
            bell: false,
        });
        let (bg, fg) = (Rgb { r: 0, g: 0, b: 0 }, WHITE);
        let close = |texts: &[UiText]| texts.iter().find(|t| t.text == "×").unwrap().color;

        let (plain, texts) = bar.draw(&labels, Some(BarHit::Tab(0)), false, bg, fg);
        assert_ne!(close(&texts), fg);

        let (rects, texts) = bar.draw(&labels, Some(BarHit::Close(0)), false, bg, fg);
        assert_eq!(rects.len(), plain.len() + 1);
        // Close button of tab 0: 20px square at x 257, inset by 2px.
        let highlight = rects.last().unwrap();
        assert_eq!(
            (highlight.x, highlight.y, highlight.width),
            (259.0, 9.0, 16.0)
        );
        assert!(highlight.radius > 0.0);
        assert_eq!(close(&texts), fg);
    }

    #[test]
    fn titles_are_truncated_by_display_width() {
        assert_eq!(truncate("short", 10), "short");
        assert_eq!(truncate("a long title", 6), "a lon…");
        assert_eq!(truncate("日本語タイトル", 5), "日本…");
        assert_eq!(truncate("abc", 0), "");
    }
}
