//! Tab bar layout, hit testing and drawing primitives.

use std::ops::Range;

use nuntio_render::{CellMetrics, Rect, UiRect, UiText};
use nuntio_term::{Rgb, rgb};

use crate::style::{INFO_COLOR, UiMetrics, WHITE, bar_background, hairline, mix, rect, truncate};
use unicode_width::UnicodeWidthStr;

/// Widest a tab gets, in cells, so a few tabs don't stretch across the window.
const MAX_TAB_CELLS: f32 = 28.0;
/// Narrowest a tab gets before the bar scrolls, in cells.
const MIN_TAB_CELLS: f32 = 10.0;
/// Width of each scroll arrow of an overflowing bar, in logical pixels.
const SCROLL_BUTTON: f64 = 24.0;
const BELL_COLOR: Rgb = rgb(0xe5c07b);
/// Hover color of the close-window button, as on Windows.
const CLOSE_HOVER: Rgb = rgb(0xe81123);
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
/// Free space the tabs always leave after the "+" button to drag the
/// window by, when the bar is the window's title bar, in logical pixels.
const DRAG_AREA: f64 = 48.0;

#[derive(Debug, Clone, Copy, PartialEq)]
struct Slot {
    x: f32,
    width: f32,
}

/// Direction of a chevron icon.
#[derive(Debug, Clone, Copy)]
enum Chevron {
    Left,
    Right,
    Down,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BarHit {
    Tab(usize),
    Close(usize),
    /// The "+" button after the last tab.
    NewTab,
    /// The arrow before the visible tabs of an overflowing bar.
    ScrollLeft,
    /// The arrow after the visible tabs of an overflowing bar.
    ScrollRight,
    /// The ⌄ button after "+" that lists all tabs, when the bar overflows.
    TabList,
    /// Free space in the bar (drag area for moving the window).
    Empty,
    /// The badge announcing a newer release.
    Update,
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
    /// Top of the text row, which is centered in the bar.
    top: f32,
    /// Vertical inset of the pills, and horizontal margin of the tab row.
    inset: f32,
    gap: f32,
    radius: f32,
    scale: f32,
    cell: CellMetrics,
    /// The visible tabs: `slots[n]` is tab `first + n`.
    slots: Vec<Slot>,
    /// Index of the tab in `slots[0]`.
    first: usize,
    /// Number of all tabs, visible or not.
    count: usize,
    /// Left edge of the tab row.
    start: f32,
    /// Width of each scroll arrow; 0 when all tabs fit.
    scroll_width: f32,
    /// Width of one window control button; 0 without controls.
    control_width: f32,
    /// Left edge of the new-tab button's area, right after the last tab.
    new_tab_x: f32,
    /// Width of the update badge's area; 0 without one.
    badge_width: f32,
}

/// What the tab bar makes room for besides the tabs.
#[derive(Debug, Clone, Copy, Default)]
pub struct TabBarOptions {
    /// Free space at the left edge (macOS window buttons).
    pub left_inset: f32,
    /// The bar is at least this high (the macOS title bar the buttons sit in).
    pub min_height: f32,
    /// Minimize, maximize and close at the right edge.
    pub window_controls: bool,
    /// The bar is the window's title bar: keep `DRAG_AREA` free after the
    /// tabs to move the window by.
    pub drag_area: bool,
    /// The badge for a newer release, left of the window controls.
    pub update_badge: bool,
    /// First tab to show when not all fit; clamped.
    pub scroll: usize,
}

impl TabBar {
    /// Lay out `count` tabs across a bar of `width` pixels, in the
    /// terminal font.
    pub fn new(width: f32, count: usize, metrics: UiMetrics, options: TabBarOptions) -> Self {
        let TabBarOptions {
            left_inset,
            min_height,
            window_controls,
            drag_area,
            update_badge,
            scroll,
        } = options;
        let (cell, scale) = (metrics.cell, metrics.scale);
        let (cw, ch) = (cell.width as f32, cell.height as f32);
        let logical = |px| metrics.logical(px);
        let padding = logical(BAR_PADDING);
        let inset = logical(PILL_INSET);
        let height = (cell.height as f32 + 2.0 * padding).max(min_height.round());
        let top = ((height - cell.height as f32) / 2.0).floor();
        let control_width = if window_controls {
            logical(CONTROL_WIDTH)
        } else {
            0.0
        };
        let start = left_inset + inset;
        let gap = 2.0 * (logical(PILL_GAP) / 2.0).floor();
        let new_tab_width = height - 2.0 * inset + gap;
        let badge_width = if update_badge { new_tab_width } else { 0.0 };
        let drag_width = if drag_area { logical(DRAG_AREA) } else { 0.0 };
        let room = (width
            - start
            - inset
            - 3.0 * control_width
            - new_tab_width
            - badge_width
            - drag_width)
            .max(0.0);
        // A tab at minimum width always has room for its close button.
        let min_tab = (MIN_TAB_CELLS * cw).max(2.0 * (padding + ch) + 2.0 * cw);
        let (first, visible, tab_width, scroll_width) = if count as f32 * min_tab <= room {
            let tab_width = (room / count.max(1) as f32).min(MAX_TAB_CELLS * cw).floor();
            (0, count, tab_width, 0.0)
        } else {
            let scroll_width = logical(SCROLL_BUTTON);
            // The ⌄ button is as wide as "+".
            let viewport = (room - 2.0 * scroll_width - new_tab_width).max(0.0);
            let visible = ((viewport / min_tab) as usize).clamp(1, count);
            let tab_width = (viewport / visible as f32).floor();
            (
                scroll.min(count - visible),
                visible,
                tab_width,
                scroll_width,
            )
        };
        let row = start + scroll_width;
        let slots = (0..visible)
            .map(|n| Slot {
                x: row + n as f32 * tab_width,
                width: tab_width,
            })
            .collect();
        let new_tab_x = row + visible as f32 * tab_width + scroll_width;
        Self {
            height,
            width,
            padding,
            top,
            inset,
            gap,
            radius: logical(PILL_RADIUS),
            scale: scale as f32,
            cell,
            slots,
            first,
            count,
            start,
            scroll_width,
            control_width,
            new_tab_x,
            badge_width,
        }
    }

    /// Left edge of the new-tab button's area.
    pub fn new_tab_x(&self) -> f32 {
        self.new_tab_x
    }

    /// Not all tabs fit: the row scrolls and shows the arrows and ⌄.
    pub fn overflows(&self) -> bool {
        self.scroll_width > 0.0
    }

    /// Indices of the tabs the bar shows.
    pub fn visible_tabs(&self) -> Range<usize> {
        self.first..self.first + self.slots.len()
    }

    /// Width of one tab; 0 without tabs.
    pub fn tab_width(&self) -> f32 {
        self.slots.first().map_or(0.0, |s| s.width)
    }

    /// Left edge of the ⌄ button, right after "+".
    pub fn tab_list_x(&self) -> f32 {
        self.new_tab_x + self.new_tab_width()
    }

    /// The first tab after scrolling by `tabs`, positive towards the last
    /// tab.
    pub fn scroll_target(&self, tabs: isize) -> usize {
        self.first
            .saturating_add_signed(tabs)
            .min(self.count - self.slots.len())
    }

    /// The first tab to show so that tab `index` is visible, scrolling as
    /// little as possible.
    pub fn reveal(&self, index: usize) -> usize {
        let visible = self.slots.len();
        if index < self.first {
            index
        } else if index >= self.first + visible {
            index + 1 - visible.max(1)
        } else {
            self.first
        }
    }

    /// Width of the new-tab button's area: a square pill plus the gap.
    fn new_tab_width(&self) -> f32 {
        self.height - 2.0 * self.inset + self.gap
    }

    /// Left edge of the window control buttons.
    fn controls_x(&self) -> f32 {
        self.width - 3.0 * self.control_width
    }

    /// Left edge of the update badge's area: before the window controls,
    /// or the margin at the right edge.
    fn badge_x(&self) -> f32 {
        let right = if self.control_width > 0.0 {
            self.controls_x()
        } else {
            self.width - self.inset
        };
        right - self.badge_width
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
        (slot.x + slot.width - self.padding - size, self.top, size)
    }

    pub fn hit(&self, x: f32, y: f32) -> Option<BarHit> {
        if y < 0.0 || y >= self.height || x < 0.0 || x >= self.width {
            return None;
        }
        if self.control_width > 0.0 && x >= self.controls_x() {
            let i = ((x - self.controls_x()) / self.control_width) as usize;
            return Some(CONTROLS[i.min(2)]);
        }
        if self.badge_width > 0.0 && x >= self.badge_x() && x < self.badge_x() + self.badge_width {
            return Some(BarHit::Update);
        }
        if x >= self.new_tab_x && x < self.new_tab_x + self.new_tab_width() {
            return Some(BarHit::NewTab);
        }
        if self.overflows() {
            let list_x = self.tab_list_x();
            if x >= list_x && x < list_x + self.new_tab_width() {
                return Some(BarHit::TabList);
            }
            if x >= self.start && x < self.start + self.scroll_width {
                return Some(BarHit::ScrollLeft);
            }
            if x >= self.new_tab_x - self.scroll_width && x < self.new_tab_x {
                return Some(BarHit::ScrollRight);
            }
        }
        let Some(index) = self.slot_at(x) else {
            return Some(BarHit::Empty);
        };
        let slot = self.slots[index - self.first];
        let (cx, cy, size) = self.close_rect(slot);
        if self.has_close(slot) && x >= cx && x < cx + size && y >= cy && y < cy + size {
            Some(BarHit::Close(index))
        } else {
            Some(BarHit::Tab(index))
        }
    }

    /// Index of the tab under a horizontal position, for clicks.
    fn slot_at(&self, x: f32) -> Option<usize> {
        self.slots
            .iter()
            .position(|s| x >= s.x && x < s.x + s.width)
            .map(|n| self.first + n)
    }

    /// Where a dragged tab goes when the pointer is at `x`: the tab under
    /// it, or the first or last visible position beyond the row's ends.
    pub fn drop_index(&self, x: f32) -> Option<usize> {
        let first = self.slots.first()?;
        Some(self.slot_at(x).unwrap_or(if x < first.x {
            self.first
        } else {
            self.first + self.slots.len() - 1
        }))
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

        let mut rects = vec![rect(0.0, 0.0, self.width, self.height, bar_bg)];
        let mut texts = Vec::new();
        self.draw_new_tab(
            &mut rects,
            hovered == Some(BarHit::NewTab),
            hover_bg,
            inactive_text,
            foreground,
        );

        // A hairline between two neighbouring tabs that have no pill, in the
        // status bar's separator color.
        let separator = mix(bar_bg, foreground, 0.15);
        let stroke = hairline(f64::from(self.scale));
        let has_pill = |i: usize| labels[i].active || hovered_tab == Some(i);

        for (n, (slot, label)) in self.slots.iter().zip(&labels[self.first..]).enumerate() {
            let i = self.first + n;
            if n > 0 && !has_pill(i - 1) && !has_pill(i) {
                rects.push(rect(
                    (slot.x - stroke / 2.0).round(),
                    self.top,
                    stroke,
                    ch,
                    separator,
                ));
            }
            if has_pill(i) {
                let color = if label.active { active_bg } else { hover_bg };
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
            texts.push(UiText::new(
                (text_x0 + (text_width - title_cells * cw) / 2.0).floor(),
                self.top,
                title,
                color,
            ));

            let indicator_x = slot.x + self.padding + ((side - cw) / 2.0).floor();
            if slot.width >= self.padding + side {
                if label.bell {
                    texts.push(UiText::new(indicator_x, self.top, "●", BELL_COLOR));
                } else if label.activity {
                    texts.push(UiText::new(indicator_x, self.top, "•", inactive_text));
                }
            }

            if has_pill(i) && self.has_close(*slot) {
                let (cx, cy, size) = self.close_rect(*slot);
                let close_hovered = hovered == Some(BarHit::Close(i));
                if close_hovered {
                    let under = if label.active { active_bg } else { hover_bg };
                    let inset = (CLOSE_INSET as f32 * self.scale).round();
                    rects.push(UiRect::rounded(
                        Rect::new(cx, cy, size, size).inset(inset),
                        mix(under, foreground, 0.10),
                        (CLOSE_RADIUS as f32 * self.scale).round(),
                    ));
                }
                let color = if close_hovered {
                    foreground
                } else {
                    inactive_text
                };
                texts.push(UiText::new(
                    cx + ((side - cw) / 2.0).floor(),
                    cy,
                    "×",
                    color,
                ));
            }
        }
        if self.overflows() {
            let disabled = mix(bar_bg, foreground, 0.2);
            let last = self.first + self.slots.len();
            for (hit, x, width, chevron, enabled) in [
                (
                    BarHit::ScrollLeft,
                    self.start,
                    self.scroll_width,
                    Chevron::Left,
                    self.first > 0,
                ),
                (
                    BarHit::ScrollRight,
                    self.new_tab_x - self.scroll_width,
                    self.scroll_width,
                    Chevron::Right,
                    last < self.count,
                ),
                (
                    BarHit::TabList,
                    self.tab_list_x(),
                    self.new_tab_width(),
                    Chevron::Down,
                    true,
                ),
            ] {
                let color = if !enabled {
                    disabled
                } else if hovered == Some(hit) {
                    rects.push(self.pill(x + self.gap / 2.0, width - self.gap, hover_bg));
                    foreground
                } else {
                    inactive_text
                };
                self.draw_chevron(&mut rects, x, width, chevron, color);
            }
        }
        if self.badge_width > 0.0 {
            self.draw_badge(&mut rects, &mut texts, hovered == Some(BarHit::Update));
        }
        if self.control_width > 0.0 {
            self.draw_controls(&mut rects, hovered, maximized, bar_bg, foreground);
        }
        (rects, texts)
    }

    /// A rounded background spanning the bar's height minus the inset.
    fn pill(&self, x: f32, width: f32, color: Rgb) -> UiRect {
        let height = self.height - 2.0 * self.inset;
        UiRect::rounded(Rect::new(x, self.inset, width, height), color, self.radius)
    }

    /// A chevron centered in the area from `x` over `width`, built from
    /// stroke-sized squares like the close-window diagonals.
    fn draw_chevron(
        &self,
        rects: &mut Vec<UiRect>,
        x: f32,
        width: f32,
        chevron: Chevron,
        color: Rgb,
    ) {
        let icon = (CONTROL_ICON as f32 * self.scale).round();
        let stroke = hairline(f64::from(self.scale));
        let half = (icon / 2.0).floor();
        let steps = (half / stroke) as usize;
        let (box_width, box_height) = match chevron {
            Chevron::Down => (icon, half),
            Chevron::Left | Chevron::Right => (half, icon),
        };
        let x0 = (x + (width - box_width) / 2.0).floor();
        let y0 = ((self.height - box_height) / 2.0).floor();
        for k in 0..steps {
            let d = k as f32 * stroke;
            let squares = match chevron {
                Chevron::Down => [(x0 + d, y0 + d), (x0 + icon - stroke - d, y0 + d)],
                Chevron::Right => [(x0 + d, y0 + d), (x0 + d, y0 + icon - stroke - d)],
                Chevron::Left => [
                    (x0 + half - stroke - d, y0 + d),
                    (x0 + half - stroke - d, y0 + icon - stroke - d),
                ],
            };
            for (x, y) in squares {
                rects.push(rect(x, y, stroke, stroke, color));
            }
        }
    }

    /// The "+" button, drawn from two rectangles like the window controls.
    fn draw_new_tab(
        &self,
        rects: &mut Vec<UiRect>,
        hovered: bool,
        hover_bg: Rgb,
        color: Rgb,
        hover_color: Rgb,
    ) {
        let x = self.new_tab_x + self.gap / 2.0;
        let size = self.new_tab_width() - self.gap;
        if hovered {
            rects.push(self.pill(x, size, hover_bg));
        }
        let color = if hovered { hover_color } else { color };
        let stroke = hairline(f64::from(self.scale));
        let icon = (CONTROL_ICON as f32 * self.scale).round();
        let x0 = (x + (size - icon) / 2.0).floor();
        let y0 = ((self.height - icon) / 2.0).floor();
        let middle = ((icon - stroke) / 2.0).round();
        for (x, y, width, height) in [
            (x0, y0 + middle, icon, stroke),
            (x0 + middle, y0, stroke, icon),
        ] {
            rects.push(rect(x, y, width, height, color));
        }
    }

    /// A blue pill with an arrow: a newer release exists.
    fn draw_badge(&self, rects: &mut Vec<UiRect>, texts: &mut Vec<UiText>, hovered: bool) {
        let x = self.badge_x() + self.gap / 2.0;
        let size = self.badge_width - self.gap;
        let color = if hovered {
            mix(INFO_COLOR, WHITE, 0.15)
        } else {
            INFO_COLOR
        };
        rects.push(self.pill(x, size, color));
        texts.push(UiText::new(
            (x + (size - self.cell.width as f32) / 2.0).floor(),
            self.top,
            "↑",
            WHITE,
        ));
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
        let stroke = hairline(f64::from(self.scale));
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
        let mut push = |x: f32, y: f32, width: f32, height: f32, color: Rgb| {
            rects.push(rect(x, y, width, height, color));
        };
        // An outlined square.
        let outline = |push: &mut dyn FnMut(f32, f32, f32, f32, Rgb), x, y, size, color| {
            push(x, y, size, stroke, color);
            push(x, y + size - stroke, size, stroke, color);
            push(x, y, stroke, size, color);
            push(x + size - stroke, y, stroke, size, color);
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
                    push(x0, (y0 + icon / 2.0).floor(), icon, stroke, color);
                }
                BarHit::Maximize if maximized => {
                    // Restore: a front square with another peeking out behind.
                    let offset = (2.0 * self.scale).round();
                    let front = icon - offset;
                    push(x0 + offset, y0, front, stroke, color);
                    push(x0 + icon - stroke, y0, stroke, front, color);
                    outline(&mut push, x0, y0 + offset, front, color);
                }
                BarHit::Maximize => outline(&mut push, x0, y0, icon, color),
                _ => {
                    // Close: two diagonals built from stroke-sized steps.
                    let steps = (icon / stroke) as usize;
                    for k in 0..steps {
                        let d = k as f32 * stroke;
                        push(x0 + d, y0 + d, stroke, stroke, color);
                        push(x0 + icon - stroke - d, y0 + d, stroke, stroke, color);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::style::test_metrics::*;

    #[test]
    fn tabs_share_the_width_up_to_a_maximum() {
        let bar = TabBar::new(1000.0, 2, METRICS, TabBarOptions::default());
        assert_eq!(bar.slots[0].x, 4.0, "margin at the left edge");
        assert_eq!(bar.slots[0].width, 280.0, "capped at 28 cells");
        assert_eq!(bar.slots[1].x, 284.0);

        // 4px margin on both sides and the 30px "+" button leave 962px
        // for 8 tabs.
        let bar = TabBar::new(1000.0, 8, METRICS, TabBarOptions::default());
        assert_eq!(bar.slots[0].width, 120.0);
        assert_eq!(bar.height, 34.0);
    }

    #[test]
    fn hit_testing() {
        let bar = TabBar::new(1000.0, 2, METRICS, TabBarOptions::default());
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
        // 142px of room minus the arrows and ⌄ leave one 64px tab: too
        // narrow for the close button.
        let bar = TabBar::new(180.0, 3, METRICS, TabBarOptions::default());
        let slot = bar.slots[0];
        assert_eq!(slot.width, 64.0);
        let (cx, cy, _) = bar.close_rect(slot);
        assert_eq!(bar.hit(cx + 1.0, cy + 1.0), Some(BarHit::Tab(0)));
    }

    #[test]
    fn tabs_keep_a_minimum_width_and_scroll() {
        let bar = TabBar::new(1000.0, 9, METRICS, TabBarOptions::default());
        assert!(!bar.overflows());
        assert_eq!(bar.tab_width(), 106.0);
        assert_eq!(bar.hit(10.0, 10.0), Some(BarHit::Tab(0)));
        assert_eq!(bar.hit(990.0, 10.0), Some(BarHit::Empty));

        let scrolled = |scroll| {
            TabBar::new(
                1000.0,
                20,
                METRICS,
                TabBarOptions {
                    scroll,
                    ..TabBarOptions::default()
                },
            )
        };
        // 962px of room minus two 24px arrows and ⌄ leave 884px: 8 tabs.
        let bar = scrolled(5);
        assert!(bar.overflows());
        assert_eq!(bar.visible_tabs(), 5..13);
        assert_eq!(bar.tab_width(), 110.0);
        for (x, hit) in [
            (10.0, BarHit::ScrollLeft),
            (30.0, BarHit::Tab(5)),
            (907.0, BarHit::Tab(12)),
            (910.0, BarHit::ScrollRight),
            (940.0, BarHit::NewTab),
            (970.0, BarHit::TabList),
            (995.0, BarHit::Empty),
        ] {
            assert_eq!(bar.hit(x, 10.0), Some(hit), "at {x}");
        }
        assert_eq!(bar.drop_index(10.0), Some(5));
        assert_eq!(bar.drop_index(950.0), Some(12));
        assert_eq!(bar.scroll_target(-1), 4);
        assert_eq!(bar.scroll_target(100), 12);
        assert_eq!(bar.scroll_target(-100), 0);
        assert_eq!(bar.reveal(2), 2);
        assert_eq!(bar.reveal(15), 8);
        assert_eq!(bar.reveal(7), 5);
        assert_eq!(scrolled(100).visible_tabs(), 12..20);
    }

    #[test]
    fn only_visible_tabs_are_drawn() {
        let bar = |scroll| {
            TabBar::new(
                1000.0,
                20,
                METRICS,
                TabBarOptions {
                    scroll,
                    ..TabBarOptions::default()
                },
            )
        };
        let labels: Vec<_> = (0..20)
            .map(|i| TabLabel {
                title: format!("t{i}"),
                active: i == 0,
                activity: false,
                bell: false,
            })
            .collect();
        let (bg, fg) = (Rgb { r: 0, g: 0, b: 0 }, WHITE);
        let (_, texts) = bar(5).draw(&labels, None, false, bg, fg);
        let shown = |title: &str| texts.iter().any(|t| t.text == title);
        assert!(shown("t5") && shown("t12"));
        assert!(!shown("t4") && !shown("t13"));

        // At the first tab the left arrow is disabled and gets no highlight.
        let bar = bar(0);
        let rects = |hovered| bar.draw(&labels, hovered, false, bg, fg).0.len();
        assert_eq!(rects(Some(BarHit::ScrollLeft)), rects(None));
        assert_eq!(rects(Some(BarHit::ScrollRight)), rects(None) + 1);
    }

    #[test]
    fn drops_beyond_the_row_go_to_its_ends() {
        let bar = TabBar::new(
            1000.0,
            3,
            METRICS,
            TabBarOptions {
                left_inset: 80.0,
                ..TabBarOptions::default()
            },
        );
        assert_eq!(bar.drop_index(10.0), Some(0));
        assert_eq!(bar.drop_index(90.0), Some(0));
        assert_eq!(bar.drop_index(400.0), Some(1));
        assert_eq!(bar.drop_index(990.0), Some(2));
        assert_eq!(
            TabBar::new(1000.0, 0, METRICS, TabBarOptions::default()).drop_index(5.0),
            None
        );
    }

    #[test]
    fn short_bars_grow_to_the_minimum_height_centered() {
        let bar = TabBar::new(
            1000.0,
            2,
            METRICS,
            TabBarOptions {
                left_inset: 80.0,
                ..TabBarOptions::default()
            },
        );
        let tall = TabBar::new(
            1000.0,
            2,
            METRICS,
            TabBarOptions {
                left_inset: 80.0,
                min_height: bar.height + 10.0,
                ..TabBarOptions::default()
            },
        );
        assert_eq!(tall.height, bar.height + 10.0);
        assert_eq!(tall.top, bar.top + 5.0);
        let short = TabBar::new(
            1000.0,
            2,
            METRICS,
            TabBarOptions {
                left_inset: 80.0,
                min_height: bar.height - 10.0,
                ..TabBarOptions::default()
            },
        );
        assert_eq!(short.height, bar.height);
    }

    #[test]
    fn left_inset_is_respected() {
        let bar = TabBar::new(
            1000.0,
            1,
            METRICS,
            TabBarOptions {
                left_inset: 80.0,
                ..TabBarOptions::default()
            },
        );
        assert_eq!(bar.hit(40.0, 10.0), Some(BarHit::Empty));
        assert_eq!(bar.hit(90.0, 10.0), Some(BarHit::Tab(0)));
    }

    #[test]
    fn window_controls_take_the_right_edge() {
        let bar = TabBar::new(
            1000.0,
            8,
            METRICS,
            TabBarOptions {
                window_controls: true,
                ..TabBarOptions::default()
            },
        );
        // 3 × 46px of controls, the margins and the "+" button leave 824px
        // for 8 tabs.
        assert_eq!(bar.slots[0].width, 103.0);
        assert_eq!(bar.hit(870.0, 10.0), Some(BarHit::Minimize));
        assert_eq!(bar.hit(930.0, 10.0), Some(BarHit::Maximize));
        assert_eq!(bar.hit(999.0, 10.0), Some(BarHit::CloseWindow));
        assert_eq!(bar.hit(861.0, 10.0), Some(BarHit::Empty));
    }

    #[test]
    fn full_bar_keeps_a_drag_area() {
        // Without the reserve, 117px tabs push the "+" button up to 853px.
        let bar = TabBar::new(
            1000.0,
            7,
            METRICS,
            TabBarOptions {
                window_controls: true,
                drag_area: true,
                ..TabBarOptions::default()
            },
        );
        let drag = DRAG_AREA as f32;
        assert_eq!(bar.hit(bar.controls_x() - drag, 10.0), Some(BarHit::Empty));
        assert_eq!(bar.hit(bar.controls_x() - 1.0, 10.0), Some(BarHit::Empty));
        assert_eq!(bar.hit(bar.new_tab_x() - 1.0, 10.0), Some(BarHit::Tab(6)));
    }

    #[test]
    fn update_badge_sits_left_of_the_window_controls() {
        // One more 30px square than the "+" button leaves 794px for 7 tabs.
        let bar = TabBar::new(
            1000.0,
            7,
            METRICS,
            TabBarOptions {
                window_controls: true,
                update_badge: true,
                ..TabBarOptions::default()
            },
        );
        assert_eq!(bar.slots[0].width, 113.0);
        assert_eq!(bar.hit(861.0, 10.0), Some(BarHit::Update));
        assert_eq!(bar.hit(832.0, 10.0), Some(BarHit::Update));
        assert_eq!(bar.hit(831.0, 10.0), Some(BarHit::Empty));
        assert_eq!(bar.hit(870.0, 10.0), Some(BarHit::Minimize));

        // Without controls it keeps the margin at the right edge.
        let bar = TabBar::new(
            1000.0,
            1,
            METRICS,
            TabBarOptions {
                update_badge: true,
                ..TabBarOptions::default()
            },
        );
        assert_eq!(bar.hit(995.0, 10.0), Some(BarHit::Update));
        assert_eq!(bar.hit(997.0, 10.0), Some(BarHit::Empty));
        assert_eq!(bar.hit(965.0, 10.0), Some(BarHit::Empty));
        let labels = [TabLabel {
            title: "~".into(),
            active: true,
            activity: false,
            bell: false,
        }];
        let (rects, texts) = bar.draw(&labels, None, false, Rgb { r: 0, g: 0, b: 0 }, WHITE);
        assert!(rects.iter().any(|r| r.color == INFO_COLOR));
        assert!(texts.iter().any(|t| t.text == "↑"));
    }

    #[test]
    fn new_tab_button_follows_the_last_tab() {
        // Two 280px tabs end at 564; the button takes the next 30px.
        let bar = TabBar::new(1000.0, 2, METRICS, TabBarOptions::default());
        assert_eq!(bar.hit(563.0, 10.0), Some(BarHit::Tab(1)));
        assert_eq!(bar.hit(564.0, 10.0), Some(BarHit::NewTab));
        assert_eq!(bar.hit(593.0, 10.0), Some(BarHit::NewTab));
        assert_eq!(bar.hit(594.0, 10.0), Some(BarHit::Empty));
        // Dropping a tab onto the button moves it to the end.
        assert_eq!(bar.drop_index(570.0), Some(1));

        // With many tabs the button still fits before the window controls.
        let bar = TabBar::new(
            1000.0,
            8,
            METRICS,
            TabBarOptions {
                window_controls: true,
                ..TabBarOptions::default()
            },
        );
        assert_eq!(bar.hit(830.0, 10.0), Some(BarHit::NewTab));
        assert_eq!(bar.hit(870.0, 10.0), Some(BarHit::Minimize));
    }

    #[test]
    fn hovered_new_tab_button_is_highlighted() {
        let bar = TabBar::new(1000.0, 1, METRICS, TabBarOptions::default());
        let labels = [TabLabel {
            title: "~".into(),
            active: true,
            activity: false,
            bell: false,
        }];
        let (bg, fg) = (Rgb { r: 0, g: 0, b: 0 }, WHITE);
        let (plain, _) = bar.draw(&labels, None, false, bg, fg);
        let (rects, _) = bar.draw(&labels, Some(BarHit::NewTab), false, bg, fg);
        assert_eq!(rects.len(), plain.len() + 1);
        // The "+" is drawn in the foreground color when hovered.
        assert_eq!(rects.iter().filter(|r| r.color == fg).count(), 2);
    }

    #[test]
    fn hovered_close_button_is_highlighted() {
        let bar = TabBar::new(1000.0, 2, METRICS, TabBarOptions::default());
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
    fn separators_only_between_plain_tabs() {
        let bar = TabBar::new(1000.0, 3, METRICS, TabBarOptions::default());
        let labels = |active: usize| {
            [0, 1, 2].map(|i| TabLabel {
                title: "~".into(),
                active: i == active,
                activity: false,
                bell: false,
            })
        };
        let (bg, fg) = (Rgb { r: 0, g: 0, b: 0 }, WHITE);
        let color = mix(bar_background(bg), fg, 0.15);
        let separators = |active: usize, hovered: Option<BarHit>| {
            let (rects, _) = bar.draw(&labels(active), hovered, false, bg, fg);
            rects
                .into_iter()
                .filter(|r| r.color == color)
                .map(|r| (r.x, r.y, r.width, r.height))
                .collect::<Vec<_>>()
        };

        // Slots are 280px wide at x 4, 284 and 564: only tabs 1 and 2 are plain.
        assert_eq!(separators(0, None), [(564.0, 7.0, 1.0, 20.0)]);
        assert_eq!(separators(0, Some(BarHit::Tab(2))), []);
        assert_eq!(separators(1, None), []);
    }
}
