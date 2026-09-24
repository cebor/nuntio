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
        let padding = (5.0 * scale).round() as f32;
        let height = cell.height as f32 + 2.0 * padding;
        let control_width = if window_controls {
            (CONTROL_WIDTH * scale).round() as f32
        } else {
            0.0
        };
        let available = (width - left_inset - 3.0 * control_width).max(0.0);
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
        hovered: Option<BarHit>,
        maximized: bool,
        background: Rgb,
        foreground: Rgb,
    ) -> (Vec<UiRect>, Vec<UiText>) {
        let hovered_tab = match hovered {
            Some(BarHit::Tab(i) | BarHit::Close(i)) => Some(i),
            _ => None,
        };
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

            if label.active || hovered_tab == Some(i) {
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
        if self.control_width > 0.0 {
            self.draw_controls(&mut rects, hovered, maximized, bar_bg, foreground);
        }
        (rects, texts)
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
        let mut rect = |x: f32, y: f32, width: f32, height: f32, color: Rgb| {
            rects.push(UiRect {
                x,
                y,
                width,
                height,
                color,
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
            let is_hovered = hovered == Some(control);
            let mut color = foreground;
            if is_hovered {
                let hover_bg = if control == BarHit::CloseWindow {
                    color = WHITE;
                    CLOSE_HOVER
                } else {
                    mix(bar_bg, foreground, 0.12)
                };
                rect(bx, 0.0, self.control_width, self.height, hover_bg);
            }

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
        let bar = TabBar::new(1000.0, 2, CELL, 1.0, 0.0, false);
        assert_eq!(bar.slots[0].width, 280.0, "capped at 28 cells");
        assert_eq!(bar.slots[1].x, 280.0);

        let bar = TabBar::new(1000.0, 8, CELL, 1.0, 0.0, false);
        assert_eq!(bar.slots[0].width, 125.0);
        assert_eq!(bar.height, 30.0);
    }

    #[test]
    fn hit_testing() {
        let bar = TabBar::new(1000.0, 2, CELL, 1.0, 0.0, false);
        assert_eq!(bar.hit(10.0, 10.0), Some(BarHit::Tab(0)));
        assert_eq!(bar.hit(290.0, 10.0), Some(BarHit::Tab(1)));
        // Close button: 20px square, 5px from the tab's right edge.
        assert_eq!(bar.hit(270.0, 10.0), Some(BarHit::Close(0)));
        assert_eq!(bar.hit(800.0, 10.0), Some(BarHit::Empty));
        assert_eq!(bar.hit(10.0, 40.0), None, "below the bar");
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
        // 3 × 46px of controls leave 862px for 8 tabs.
        assert_eq!(bar.slots[0].width, 107.0);
        assert_eq!(bar.hit(870.0, 10.0), Some(BarHit::Minimize));
        assert_eq!(bar.hit(930.0, 10.0), Some(BarHit::Maximize));
        assert_eq!(bar.hit(999.0, 10.0), Some(BarHit::CloseWindow));
        assert_eq!(bar.hit(861.0, 10.0), Some(BarHit::Empty));
    }

    #[test]
    fn titles_are_truncated_by_display_width() {
        assert_eq!(truncate("short", 10), "short");
        assert_eq!(truncate("a long title", 6), "a lon…");
        assert_eq!(truncate("日本語タイトル", 5), "日本…");
        assert_eq!(truncate("abc", 0), "");
    }
}
