//! Pointer input on the window: hit testing, mouse reporting, selection
//! drags, dragged tabs and dividers, the wheel and hovered links.

use std::time::{Duration, Instant};

use nuntio_config::Config;
use nuntio_term::{GridPoint, Link, TermMode};
use winit::dpi::PhysicalPosition;
use winit::event::MouseScrollDelta;
use winit::window::ResizeDirection;

use super::{Chrome, WindowState};
use crate::event::PaneId;
use crate::mouse::{self, Button, ClickCounter, MouseAction, MouseMods};
use crate::pane_tree::{Axis, Divider};
use crate::tab_bar::{BarHit, TabBar};

/// Width of the edge that resizes an undecorated window, in logical pixels.
const RESIZE_BORDER: f64 = 5.0;

/// Extra grab area around pane dividers, in logical pixels.
const DIVIDER_SLOP: f64 = 3.0;

/// Time between steps while a selection drag scrolls the pane.
const AUTOSCROLL_INTERVAL: Duration = Duration::from_millis(50);

/// Fastest autoscroll, in lines per step.
const AUTOSCROLL_MAX_LINES: i32 = 5;

/// Lines scrolled per wheel notch.
const WHEEL_LINES: f64 = 3.0;

/// Pointer travel before a pressed tab starts moving.
const TAB_DRAG_THRESHOLD: f64 = 4.0;

/// Pointer state for selection, mouse reporting, the tab bar and dividers.
#[derive(Default)]
pub struct MouseState {
    pub position: Option<PhysicalPosition<f64>>,
    /// Button held down that is reported to the application.
    pub reported_button: Option<Button>,
    /// Last cell a motion event was reported for, to avoid duplicates.
    pub last_reported_cell: Option<(usize, usize)>,
    /// A local selection drag is in progress.
    pub selecting: bool,
    pub clicks: ClickCounter,
    /// Sub-line remainder of trackpad and high-resolution wheel scrolling,
    /// in pixels.
    pub scroll_pixels: f64,
    /// Tab bar element under the pointer.
    pub hovered_bar: Option<BarHit>,
    /// Time of the last click on free tab bar space, to detect double clicks.
    pub last_bar_click: Option<Instant>,
    /// A tab is being pressed or dragged to reorder it.
    pub tab_drag: Option<TabDrag>,
    /// A pane divider is being dragged.
    pub divider_drag: Option<Divider>,
    /// Link under the pointer while the link modifier is held.
    pub hover_link: Option<(PaneId, Link)>,
    /// A selection drag is past the top or bottom of the pane and scrolls it.
    pub autoscroll: Option<Autoscroll>,
}

impl MouseState {
    /// Let go of a dragged tab or divider.
    pub fn end_drags(&mut self) {
        self.tab_drag = None;
        self.divider_drag = None;
    }

    /// Whole lines to scroll for a wheel or trackpad movement of `pixels`.
    /// High-resolution wheels and trackpads send fractions of a line; the
    /// remainder is kept so they add up instead of rounding to 0.
    pub fn scroll_lines(&mut self, pixels: f64, line_height: f64) -> i32 {
        // A leftover from the other direction must not eat this scroll.
        if self.scroll_pixels * pixels < 0.0 {
            self.scroll_pixels = 0.0;
        }
        self.scroll_pixels += pixels;
        let lines = (self.scroll_pixels / line_height).trunc();
        self.scroll_pixels -= lines * line_height;
        lines as i32
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Autoscroll {
    /// Lines per step; positive scrolls up into the scrollback.
    pub lines: i32,
    pub next: Instant,
}

#[derive(Debug, Clone, Copy)]
pub struct TabDrag {
    pub index: usize,
    pub press_x: f64,
    pub moved: bool,
}

impl WindowState {
    /// Grid cell of the focused pane under a window position, clamped to
    /// its grid.
    pub fn cell_at(&self, config: &Config, pos: PhysicalPosition<f64>) -> GridPoint {
        self.cell_in(config, self.content().focused, pos)
    }

    /// Grid cell of pane `id` under a window position, clamped to its grid.
    pub fn cell_in(&self, config: &Config, id: PaneId, pos: PhysicalPosition<f64>) -> GridPoint {
        let layout = self.layout(config);
        let rect = layout.rect(id).unwrap_or_default();
        let (x0, y0) = self.grid_origin(config, rect);
        let cell = self.renderer.cell_metrics();
        let grid = self
            .content()
            .pane(id)
            .and_then(|p| p.size)
            .unwrap_or(self.grid());
        let x = (pos.x - x0 as f64).max(0.0);
        let y = (pos.y - y0 as f64).max(0.0);
        let column = ((x / cell.width as f64) as usize).min(grid.columns as usize - 1);
        let line = ((y / cell.height as f64) as usize).min(grid.lines as usize - 1);
        let within = x - column as f64 * cell.width as f64;
        GridPoint {
            column,
            line,
            right_half: within >= cell.width as f64 / 2.0,
        }
    }

    pub fn pane_at(&self, config: &Config, pos: PhysicalPosition<f64>) -> Option<PaneId> {
        self.layout(config).pane_at(pos.x as f32, pos.y as f32)
    }

    pub fn divider_at(&self, config: &Config, pos: PhysicalPosition<f64>) -> Option<Divider> {
        let slop = (DIVIDER_SLOP * self.scale()) as f32;
        self.layout(config)
            .divider_at(pos.x as f32, pos.y as f32, slop)
            .cloned()
    }

    /// Resize edge under the pointer, for windows without decorations.
    /// Maximized windows can't be resized, so their edges stay clickable.
    pub fn resize_edge(&self, pos: PhysicalPosition<f64>) -> Option<ResizeDirection> {
        if self.chrome != Chrome::Undecorated || self.window.is_maximized() {
            return None;
        }
        let size = self.window.inner_size();
        let border = RESIZE_BORDER * self.scale();
        edge_at(
            (pos.x, pos.y),
            (size.width as f64, size.height as f64),
            border,
        )
    }

    fn mouse_mods(&self) -> MouseMods {
        let mods = self.modifiers.state();
        MouseMods {
            shift: mods.shift_key(),
            alt: mods.alt_key(),
            ctrl: mods.control_key(),
        }
    }

    /// Lines per autoscroll step for a selection drag at `pos`: positive
    /// above the focused pane's grid, negative below it, 0 inside.
    pub fn autoscroll_lines(&self, config: &Config, pos: PhysicalPosition<f64>) -> i32 {
        let layout = self.layout(config);
        let Some(rect) = layout.rect(self.content().focused) else {
            return 0;
        };
        let (_, top) = self.grid_origin(config, rect);
        let cell = self.renderer.cell_metrics();
        let lines = self.grid().lines as f32;
        let bottom = top + lines * cell.height as f32;
        autoscroll_speed(pos.y as f32, top, bottom, cell.height as f32)
    }

    /// Mouse events go to the application unless Shift is held, which
    /// forces local selection like in xterm.
    pub fn reports_mouse(&self, mode: TermMode) -> bool {
        mouse::reporting_enabled(mode) && !self.modifiers.state().shift_key()
    }

    pub fn report(&self, button: Option<Button>, action: MouseAction, point: GridPoint) {
        self.report_to(self.content().focused, button, action, point);
    }

    /// Report a mouse event to pane `id` of the active tab, focused or not.
    pub fn report_to(
        &self,
        id: PaneId,
        button: Option<Button>,
        action: MouseAction,
        point: GridPoint,
    ) {
        let Some(term) = self.content().pane(id).map(|pane| &pane.term) else {
            return;
        };
        let mods = self.mouse_mods();
        if let Some(bytes) =
            mouse::encode_report(button, action, mods, point.column, point.line, term.mode())
        {
            term.write(bytes);
        }
    }

    /// Look up the link under the pointer, if the link modifier (Ctrl, Cmd
    /// on macOS) is held. Output, scrolling or a closed pane move the text
    /// under a resting pointer, so this also runs before every frame and
    /// click. Returns whether it changed.
    pub fn refresh_hover_link(&mut self, config: &Config) -> bool {
        let mods = self.modifiers.state();
        let held = if cfg!(target_os = "macos") {
            mods.super_key()
        } else {
            mods.control_key()
        };
        let link = self.mouse.position.filter(|_| held).and_then(|pos| {
            let id = self.pane_at(config, pos)?;
            let point = self.cell_in(config, id, pos);
            let link = self.content().pane(id)?.term.link_at(point)?;
            Some((id, link))
        });
        if link == self.mouse.hover_link {
            return false;
        }
        self.mouse.hover_link = link;
        true
    }

    /// `refresh_hover_link`, and redraw if the link changed.
    pub fn update_hover_link(&mut self, config: &Config) {
        if self.refresh_hover_link(config) {
            self.window.request_redraw();
        }
    }

    /// The pointer left the window.
    pub fn cursor_left(&mut self) {
        // A selection drag goes on outside the window (the button release
        // still arrives), so keep its last position.
        if !self.mouse.selecting {
            self.mouse.position = None;
        }
        let hovered = self.mouse.hovered_bar.take().is_some();
        let link = self.mouse.hover_link.take().is_some();
        if hovered || link {
            self.window.request_redraw();
        }
    }

    /// Move the dragged divider to the pointer at `pos`.
    pub fn drag_divider(&mut self, config: &Config, divider: &Divider, pos: PhysicalPosition<f64>) {
        let at = match divider.axis {
            Axis::Vertical => pos.x,
            Axis::Horizontal => pos.y,
        } as f32;
        self.content_mut().tree.drag_divider(divider, at);
        self.resize_terms(config);
        self.window.request_redraw();
    }

    /// Reorder tabs by dragging the pressed one along `bar`.
    pub fn drag_tab(
        &mut self,
        mut drag: TabDrag,
        bar: Option<&TabBar>,
        pos: PhysicalPosition<f64>,
    ) {
        if (pos.x - drag.press_x).abs() > TAB_DRAG_THRESHOLD {
            drag.moved = true;
        }
        if drag.moved
            && let Some(target) = bar.and_then(|b| b.drop_index(pos.x as f32))
            && target != drag.index
        {
            self.tabs.move_tab(drag.index, target);
            drag.index = target;
            self.window.request_redraw();
        }
        self.mouse.tab_drag = Some(drag);
    }

    /// Extend the selection being dragged to `pos`. Past the top or bottom
    /// edge, keep scrolling while the pointer stays there; `run_timers`
    /// runs the steps.
    pub fn extend_selection(&mut self, config: &Config, pos: PhysicalPosition<f64>) {
        let point = self.cell_at(config, pos);
        self.term().update_selection(point);
        let lines = self.autoscroll_lines(config, pos);
        self.mouse.autoscroll = match self.mouse.autoscroll {
            _ if lines == 0 => None,
            Some(scroll) => Some(Autoscroll { lines, ..scroll }),
            None => Some(Autoscroll {
                lines,
                next: Instant::now() + AUTOSCROLL_INTERVAL,
            }),
        };
        self.window.request_redraw();
    }

    /// Report pointer motion over the focused pane to its program, once
    /// per cell, if it asked for motion.
    pub fn report_motion(&mut self, config: &Config, pos: PhysicalPosition<f64>) {
        if self.pane_at(config, pos) != Some(self.content().focused) {
            return;
        }
        let point = self.cell_at(config, pos);
        let cell = (point.column, point.line);
        if self.reports_mouse(self.term().mode()) && self.mouse.last_reported_cell != Some(cell) {
            self.mouse.last_reported_cell = Some(cell);
            let held = self.mouse.reported_button;
            self.report(held, MouseAction::Motion, point);
        }
    }

    /// Scroll the pane under the pointer, as it would when focused; focus
    /// stays where it is.
    pub fn mouse_wheel(&mut self, config: &Config, delta: MouseScrollDelta) {
        let cell_height = self.renderer.cell_metrics().height as f64;
        // Positive = scroll up (content moves down, towards older lines).
        let pixels = match delta {
            MouseScrollDelta::LineDelta(_, y) => y as f64 * WHEEL_LINES * cell_height,
            MouseScrollDelta::PixelDelta(p) => p.y,
        };
        let lines = self.mouse.scroll_lines(pixels, cell_height);
        if lines == 0 {
            return;
        }
        // An open actions menu scrolls (if it doesn't fit) instead of the pane.
        let view = self.menu_view();
        if let Some(menu) = self.actions_menu.as_mut() {
            if menu.scroll_by(-(lines as isize), view) {
                self.window.request_redraw();
            }
            return;
        }
        let pos = self.mouse.position;
        let id = pos
            .and_then(|pos| self.pane_at(config, pos))
            .unwrap_or(self.content().focused);
        let Some(term) = self.content().pane(id).map(|pane| &pane.term) else {
            return;
        };
        let mode = term.mode();
        tracing::trace!(?delta, lines, ?mode, "mouse wheel");

        if self.reports_mouse(mode) {
            let Some(pos) = pos else {
                return;
            };
            let point = self.cell_in(config, id, pos);
            let button = if lines > 0 {
                Button::WheelUp
            } else {
                Button::WheelDown
            };
            for _ in 0..lines.unsigned_abs() {
                self.report_to(id, Some(button), MouseAction::Press, point);
            }
        } else if mode.contains(TermMode::ALT_SCREEN | TermMode::ALTERNATE_SCROLL) {
            // Full-screen apps without mouse support (less, man) get arrow keys.
            let app_cursor = mode.contains(TermMode::APP_CURSOR);
            let arrow: &[u8] = match (lines > 0, app_cursor) {
                (true, true) => b"\x1bOA",
                (true, false) => b"\x1b[A",
                (false, true) => b"\x1bOB",
                (false, false) => b"\x1b[B",
            };
            term.write(arrow.repeat(lines.unsigned_abs() as usize));
        } else {
            term.scroll(lines);
            self.window.request_redraw();
        }
    }

    /// Scroll a selection drag past the pane's edge one step further, if
    /// the step is due at `now`.
    pub(super) fn step_autoscroll(&mut self, config: &Config, now: Instant) {
        if let Some(scroll) = self.mouse.autoscroll
            && now >= scroll.next
            && let Some(pos) = self.mouse.position
        {
            self.term().scroll(scroll.lines);
            let point = self.cell_at(config, pos);
            self.term().update_selection(point);
            self.mouse.autoscroll = Some(Autoscroll {
                next: now + AUTOSCROLL_INTERVAL,
                ..scroll
            });
            self.window.request_redraw();
        }
    }
}

/// Which window edge or corner `pos` is on, within `border` of the edge.
fn edge_at(pos: (f64, f64), size: (f64, f64), border: f64) -> Option<ResizeDirection> {
    let (left, right) = (pos.0 < border, pos.0 >= size.0 - border);
    let (top, bottom) = (pos.1 < border, pos.1 >= size.1 - border);
    Some(match (left, right, top, bottom) {
        (true, _, true, _) => ResizeDirection::NorthWest,
        (_, true, true, _) => ResizeDirection::NorthEast,
        (true, _, _, true) => ResizeDirection::SouthWest,
        (_, true, _, true) => ResizeDirection::SouthEast,
        (true, ..) => ResizeDirection::West,
        (_, true, ..) => ResizeDirection::East,
        (_, _, true, _) => ResizeDirection::North,
        (_, _, _, true) => ResizeDirection::South,
        _ => return None,
    })
}

/// Autoscroll speed for a pointer at `y` with the grid spanning `top` to
/// `bottom`: one line per step at the edge, faster with distance.
fn autoscroll_speed(y: f32, top: f32, bottom: f32, cell_height: f32) -> i32 {
    let lines = |distance: f32| (1 + (distance / cell_height) as i32).min(AUTOSCROLL_MAX_LINES);
    if y < top {
        lines(top - y)
    } else if y >= bottom {
        -lines(y - bottom)
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrolling_adds_up_fractions_of_a_line() {
        let mut mouse = MouseState::default();
        assert_eq!(mouse.scroll_lines(15.0, 20.0), 0);
        assert_eq!(mouse.scroll_lines(15.0, 20.0), 1);
        assert_eq!(mouse.scroll_lines(45.0, 20.0), 2, "with the remainder");
        // Turning around drops what was left of the other direction.
        assert_eq!(mouse.scroll_lines(-15.0, 20.0), 0);
        assert_eq!(mouse.scroll_lines(-5.0, 20.0), -1);
    }

    #[test]
    fn autoscroll_speeds_up_with_distance() {
        let speed = |y| autoscroll_speed(y, 100.0, 500.0, 20.0);
        assert_eq!(speed(300.0), 0);
        assert_eq!(speed(100.0), 0);
        assert_eq!(speed(99.0), 1);
        assert_eq!(speed(60.0), 3);
        assert_eq!(speed(-1000.0), AUTOSCROLL_MAX_LINES);
        assert_eq!(speed(500.0), -1);
        assert_eq!(speed(545.0), -3);
    }

    #[test]
    fn edges_and_corners() {
        let size = (100.0, 50.0);
        assert_eq!(
            edge_at((2.0, 2.0), size, 5.0),
            Some(ResizeDirection::NorthWest)
        );
        assert_eq!(
            edge_at((98.0, 2.0), size, 5.0),
            Some(ResizeDirection::NorthEast)
        );
        assert_eq!(
            edge_at((50.0, 49.0), size, 5.0),
            Some(ResizeDirection::South)
        );
        assert_eq!(edge_at((0.0, 25.0), size, 5.0), Some(ResizeDirection::West));
        assert_eq!(edge_at((50.0, 25.0), size, 5.0), None);
    }
}
