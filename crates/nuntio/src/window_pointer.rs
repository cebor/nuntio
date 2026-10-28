//! Pointer input on the window: hit testing, mouse reporting, selection
//! drags, dragged tabs and dividers, the wheel and hovered links.

use std::time::{Duration, Instant};

use nuntio_config::Config;
use nuntio_term::{GridPoint, Link, LinkKind, TermMode};
use winit::dpi::PhysicalPosition;
use winit::event::MouseScrollDelta;
use winit::window::ResizeDirection;

use super::{Chrome, Pane, WindowState};
use crate::event::PaneId;
use crate::input;
use crate::link::{BaseDir, Target};
use crate::link_lookup::{Answer, Request};
use crate::mouse::{self, Button, ClickCounter, HeldButtons, MouseAction, MouseMods};
use crate::mux::{MuxPane, Target as MuxTarget};
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

/// How long the link modifier rests, with no other key, before a
/// multiplexer is asked for its directory: Ctrl shortcuts start no process.
const MUX_DIR_DELAY: Duration = Duration::from_millis(150);

/// Pointer state for selection, mouse reporting, the tab bar and dividers.
#[derive(Default)]
pub struct MouseState {
    pub position: Option<PhysicalPosition<f64>>,
    /// Button held down that is reported to the application.
    pub reported_buttons: HeldButtons,
    /// Last cell a motion event was reported for, to avoid duplicates.
    pub last_reported_cell: Option<(usize, usize)>,
    /// A local selection drag is in progress.
    pub selecting: bool,
    pub clicks: ClickCounter,
    /// Sub-line remainder of trackpad and high-resolution wheel scrolling,
    /// in pixels.
    pub scroll_pixels: f64,
    /// Sub-tab remainder of scrolling the tab bar, in pixels.
    pub bar_scroll_pixels: f64,
    /// Tab bar element under the pointer.
    pub hovered_bar: Option<BarHit>,
    /// Clicks on free tab bar space, to detect double clicks.
    pub bar_clicks: ClickCounter,
    /// A tab is being pressed or dragged to reorder it.
    pub tab_drag: Option<TabDrag>,
    /// A pane divider is being dragged.
    pub divider_drag: Option<Divider>,
    /// A press on free tab bar space in full screen waits for the pointer
    /// to move.
    pub fullscreen_drag: Option<FullscreenDrag>,
    /// Link under the pointer while the link modifier is held.
    pub hover_link: Option<HoverLink>,
    /// The last relative path candidate looked up on disk, or being looked
    /// up, so a resting pointer isn't looked up again every frame.
    pub relative_lookup: Option<RelativeLookup>,
    /// A lookup for the app to start.
    pub relative_request: Option<Request>,
    next_lookup: u64,
    /// The directory of the focused pane of the multiplexer in a pane,
    /// asked for once while the link modifier is held.
    pub mux_dir: Option<MuxDir>,
    /// A `mux_dir` lookup for the app to start.
    pub mux_dir_request: Option<(PaneId, MuxPane)>,
    pub link_press: LinkPress,
    /// A selection drag is past the top or bottom of the pane and scrolls it.
    pub autoscroll: Option<Autoscroll>,
}

/// The link under the pointer and where a click on it leads.
#[derive(Debug, Clone, PartialEq)]
pub struct HoverLink {
    pub pane: PaneId,
    pub link: Link,
    pub target: Target,
}

/// A relative path candidate and what the file system said about it.
pub struct RelativeLookup {
    id: u64,
    pane: PaneId,
    text: String,
    /// `None` while the lookup runs.
    answer: Option<Option<Target>>,
}

/// The working directory of the focused pane of a multiplexer, a path where
/// the multiplexer runs. Kept across presses of the link modifier, so that a
/// quick Ctrl+click needn't wait for the multiplexer: each press asks again
/// once it points at links ([`LinkPress`]), and links follow the new answer
/// when it comes.
pub struct MuxDir {
    pane: PaneId,
    mux: MuxPane,
    /// Asked during this press of the link modifier.
    asked: bool,
    /// An answer has come; `dir` is `None` if the multiplexer couldn't tell.
    answered: bool,
    dir: Option<String>,
}

/// The current press of the link modifier, as far as asking a multiplexer goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LinkPress {
    /// Not held.
    #[default]
    Released,
    /// Held since `since`, pointer at rest, no other key; `waits`: a relative
    /// candidate in a multiplexer pane waits for the ask.
    Held { since: Instant, waits: bool },
    /// Another key was pressed while held: a shortcut. Only the pointer asks now.
    Shortcut,
    /// The pointer moved, a button was pressed or `MUX_DIR_DELAY` passed.
    Pointing,
}

impl LinkPress {
    /// The link modifier is `held` at `now`.
    pub fn update(&mut self, held: bool, now: Instant) {
        if !held {
            *self = Self::Released;
        } else if *self == Self::Released {
            *self = Self::Held {
                since: now,
                waits: false,
            };
        }
    }

    /// A key other than a modifier was pressed.
    pub fn key_pressed(&mut self) {
        if matches!(self, Self::Held { .. }) {
            *self = Self::Shortcut;
        }
    }

    /// The pointer moved or a button was pressed.
    pub fn pointer_used(&mut self) {
        if *self != Self::Released {
            *self = Self::Pointing;
        }
    }

    /// Whether a multiplexer may be asked now; a resting press remembers
    /// that a candidate waits, so that its timer runs.
    fn may_ask(&mut self) -> bool {
        match self {
            Self::Pointing => true,
            Self::Held { waits, .. } => {
                *waits = true;
                false
            }
            Self::Released | Self::Shortcut => false,
        }
    }

    /// When a resting press starts pointing, if a candidate waits.
    pub fn due(&self) -> Option<Instant> {
        match *self {
            Self::Held { since, waits: true } => Some(since + MUX_DIR_DELAY),
            _ => None,
        }
    }

    /// Start pointing if `due` has passed; returns whether it did.
    pub fn run_timer(&mut self, now: Instant) -> bool {
        if self.due().is_some_and(|due| now >= due) {
            *self = Self::Pointing;
            return true;
        }
        false
    }
}

impl MouseState {
    /// Let go of a dragged tab, divider or full screen window.
    pub fn end_drags(&mut self) {
        self.tab_drag = None;
        self.divider_drag = None;
        self.fullscreen_drag = None;
    }

    /// Whole lines to scroll for a wheel or trackpad movement of `pixels`.
    pub fn scroll_lines(&mut self, pixels: f64, line_height: f64) -> i32 {
        whole_steps(&mut self.scroll_pixels, pixels, line_height)
    }

    /// The directory `pane`'s multiplexer answered. Returns whether it is
    /// the pane whose directory is kept.
    pub fn mux_dir_found(&mut self, pane: PaneId, dir: Option<String>) -> bool {
        let Some(known) = self.mux_dir.as_mut().filter(|m| m.pane == pane) else {
            return false;
        };
        known.answered = true;
        known.dir = dir;
        self.relative_lookup = None;
        true
    }

    /// The link-lookup thread answered. Returns whether the candidate
    /// looked up last waited for it.
    pub fn relative_found(&mut self, answer: Answer) -> bool {
        match &mut self.relative_lookup {
            Some(seen) if seen.id == answer.id && seen.answer.is_none() => {
                seen.answer = Some(answer.target);
                true
            }
            _ => false,
        }
    }

    /// Have `pane`'s multiplexer asked for its directory, once per press of
    /// the link modifier and only once the press points at links.
    fn ask_mux_dir(&mut self, pane: PaneId) {
        if let Some(known) = self.mux_dir.as_mut().filter(|k| k.pane == pane && !k.asked)
            && self.link_press.may_ask()
        {
            known.asked = true;
            self.mux_dir_request = Some((pane, known.mux.clone()));
        }
    }
}

/// Whole steps of `step` pixels for a wheel or trackpad movement of
/// `pixels`. High-resolution wheels and trackpads send fractions of a
/// step; the remainder is kept so they add up instead of rounding to 0.
fn whole_steps(remainder: &mut f64, pixels: f64, step: f64) -> i32 {
    // A leftover from the other direction must not eat this scroll.
    if *remainder * pixels < 0.0 {
        *remainder = 0.0;
    }
    *remainder += pixels;
    let steps = (*remainder / step).trunc();
    *remainder -= steps * step;
    steps as i32
}

/// The directory relative paths in `pane` start from: the foreground
/// process's. In a WSL pane on Windows, what nuntio-wsl reported, else an
/// absolute directory from OSC 7 or the title; `~/…` would need `wsl.exe`
/// and gives none.
fn relative_base(pane: &Pane) -> Option<BaseDir> {
    match &pane.wsl {
        Some(distro) if cfg!(windows) => {
            let dir = pane
                .term
                .remote_directory()
                .or_else(|| {
                    crate::wsl::linux_dir(pane.term.reported_directory(), pane.title.as_deref())
                })
                .filter(|dir| dir.starts_with('/'))?;
            Some(BaseDir::Wsl {
                distro: distro.name.clone(),
                dir,
            })
        }
        _ => pane
            .term
            .foreground_info(true)
            .working_directory
            .map(BaseDir::Local),
    }
}

/// The directory relative paths start from in a multiplexer that runs in
/// `target` and whose focused pane is in `dir`.
fn mux_base(target: &MuxTarget, dir: String) -> Option<BaseDir> {
    match target {
        MuxTarget::Local => Some(BaseDir::Local(dir.into())),
        MuxTarget::Wsl(distro) => dir.starts_with('/').then(|| BaseDir::Wsl {
            distro: distro.name.clone(),
            dir,
        }),
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

/// Free tab bar space pressed while an undecorated window is full screen.
/// A full screen window can't be moved, so it first leaves full screen.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FullscreenDrag {
    /// Pressed at this window position; the pointer hasn't moved far yet.
    Pressed(PhysicalPosition<f64>),
    /// Full screen was left; the drag starts once the window reports it
    /// (Wayland only does after the compositor's configure).
    Leaving,
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
    /// Maximized and full screen windows can't be resized, so their edges
    /// stay clickable.
    pub fn resize_edge(&self, pos: PhysicalPosition<f64>) -> Option<ResizeDirection> {
        if self.chrome != Chrome::Undecorated || self.fills_screen() {
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
            term.send(bytes);
        }
    }

    /// Look up the link under the pointer, if the link modifier (Ctrl, Cmd
    /// on macOS) is held. Output, scrolling or a closed pane move the text
    /// under a resting pointer, so this also runs before every frame and
    /// click, and nothing is looked up while a menu is open. Returns whether
    /// it changed.
    pub fn refresh_hover_link(&mut self, config: &Config) -> bool {
        let mods = self.modifiers.state();
        let held = if cfg!(target_os = "macos") {
            mods.super_key()
        } else {
            mods.control_key()
        };
        // Nothing under an open menu reacts to the pointer.
        let held = held && self.menu.is_none();
        self.mouse.link_press.update(held, Instant::now());
        let found = self.mouse.position.filter(|_| held).and_then(|pos| {
            let id = self.pane_at(config, pos)?;
            let point = self.cell_in(config, id, pos);
            let link = self.content().pane(id)?.term.link_at(point)?;
            Some((id, link))
        });
        // A new press looks again: files may have appeared meanwhile, and a
        // multiplexer may show another pane.
        if !held {
            self.mouse.relative_lookup = None;
            self.mouse.relative_request = None;
            if let Some(known) = &mut self.mouse.mux_dir {
                known.asked = false;
            }
            self.mouse.mux_dir_request = None;
        }
        // A path that leads nowhere here is no link.
        let link = found.and_then(|(id, link)| {
            let target = self.link_target(config, id, &link)?;
            Some(HoverLink {
                pane: id,
                link,
                target,
            })
        });
        if link == self.mouse.hover_link {
            return false;
        }
        self.mouse.hover_link = link;
        true
    }

    /// Where a click on `link` in pane `id` leads, `None` if nowhere or not
    /// known yet. A relative path is looked up on disk once per pane and
    /// text, by the app's link-lookup thread; in a pane running herdr or
    /// tmux, from the directory of its focused pane, which the app is asked
    /// to look up first.
    fn link_target(&mut self, config: &Config, id: PaneId, link: &Link) -> Option<Target> {
        match link.kind {
            LinkKind::Url => Some(Target::Url(link.url.clone())),
            LinkKind::Path => {
                crate::link::path_target(&link.url, self.content().pane(id)?.wsl.as_ref())
            }
            LinkKind::Relative => {
                if let Some(seen) = &self.mouse.relative_lookup
                    && seen.pane == id
                    && seen.text == link.url
                {
                    // Found with the directory of an earlier press: this one asks too.
                    let answer = seen.answer.clone().flatten();
                    self.mouse.ask_mux_dir(id);
                    return answer;
                }
                let base = match crate::mux::pane_mux(self.content().pane(id)?, config) {
                    Some(mux) => {
                        match &mut self.mouse.mux_dir {
                            Some(known) if known.pane == id => known.mux = mux.clone(),
                            other => {
                                *other = Some(MuxDir {
                                    pane: id,
                                    mux: mux.clone(),
                                    asked: false,
                                    answered: false,
                                    dir: None,
                                });
                            }
                        }
                        self.mouse.ask_mux_dir(id);
                        let known = self.mouse.mux_dir.as_ref()?;
                        // Not cached until the first answer: it looks again then.
                        if !known.answered {
                            return None;
                        }
                        known.dir.clone().and_then(|dir| mux_base(&mux.target, dir))
                    }
                    None => relative_base(self.content().pane(id)?),
                };
                let Some(base) = base else {
                    self.mouse.relative_lookup = Some(RelativeLookup {
                        id: 0,
                        pane: id,
                        text: link.url.clone(),
                        answer: Some(None),
                    });
                    return None;
                };
                let lookup = self.mouse.next_lookup;
                self.mouse.next_lookup += 1;
                self.mouse.relative_lookup = Some(RelativeLookup {
                    id: lookup,
                    pane: id,
                    text: link.url.clone(),
                    answer: None,
                });
                self.mouse.relative_request = Some(Request {
                    id: lookup,
                    text: link.url.clone(),
                    base,
                });
                None
            }
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
            self.reveal_active_tab();
            drag.index = target;
            self.window.request_redraw();
        }
        self.mouse.tab_drag = Some(drag);
    }

    /// Free tab bar space was pressed (not a double click) at `pos`.
    /// Returns whether to drag the window now; a full screen undecorated
    /// window waits for the pointer to move (`drag_from_fullscreen`).
    pub fn press_drag_area(&mut self, pos: PhysicalPosition<f64>) -> bool {
        if self.chrome == Chrome::Undecorated && self.window.fullscreen().is_some() {
            self.mouse.fullscreen_drag = Some(FullscreenDrag::Pressed(pos));
            return false;
        }
        true
    }

    /// The pointer moved to `pos` during a `FullscreenDrag`. Past
    /// `TAB_DRAG_THRESHOLD` it leaves full screen; returns true once the
    /// window is out of full screen and should be dragged.
    pub fn drag_from_fullscreen(&mut self, pos: PhysicalPosition<f64>) -> bool {
        match self.mouse.fullscreen_drag {
            None => return false,
            Some(FullscreenDrag::Pressed(press)) => {
                let moved = (pos.x - press.x).abs().max((pos.y - press.y).abs());
                if moved <= TAB_DRAG_THRESHOLD {
                    return false;
                }
                self.leave_fullscreen_for_drag(pos);
                self.mouse.fullscreen_drag = Some(FullscreenDrag::Leaving);
            }
            Some(FullscreenDrag::Leaving) => {}
        }
        if self.window.fullscreen().is_some() {
            return false;
        }
        self.mouse.fullscreen_drag = None;
        true
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
            let held = self.mouse.reported_buttons.first();
            self.report(held, MouseAction::Motion, point);
        }
    }

    /// Scroll the pane under the pointer, as it would when focused; focus
    /// stays where it is.
    pub fn mouse_wheel(&mut self, config: &Config, delta: MouseScrollDelta) {
        // Over an overflowing tab bar the wheel scrolls the tabs.
        if self.menu.is_none()
            && let Some(pos) = self.mouse.position
            && let Some(bar) = self.tab_bar(config)
            && bar.overflows()
            && (pos.y as f32) < bar.height
        {
            self.wheel_tab_bar(&bar, delta);
            return;
        }
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
        // An open menu scrolls (if it doesn't fit) instead of the pane.
        let view = self.menu_view();
        if let Some(menu) = self.menu.as_mut() {
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
            let arrow = input::wheel_arrow(lines > 0, mode);
            term.write(arrow.repeat(lines.unsigned_abs() as usize));
        } else {
            term.scroll(lines);
            self.window.request_redraw();
        }
    }

    /// Scroll the tabs of `bar` by a wheel or trackpad movement on either
    /// axis: one notch or a tab's width of travel moves one tab.
    fn wheel_tab_bar(&mut self, bar: &TabBar, delta: MouseScrollDelta) {
        let step = f64::from(bar.tab_width().max(1.0));
        let dominant = |x: f64, y: f64| if x.abs() > y.abs() { x } else { y };
        let pixels = match delta {
            MouseScrollDelta::LineDelta(x, y) => dominant(x.into(), y.into()) * step,
            MouseScrollDelta::PixelDelta(p) => dominant(p.x, p.y),
        };
        let tabs = whole_steps(&mut self.mouse.bar_scroll_pixels, pixels, step);
        // Positive values reveal what is left or above: earlier tabs.
        self.scroll_tabs(-(tabs as isize));
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
    fn multiplexers_are_asked_only_when_pointing_at_links() {
        let t0 = Instant::now();
        let ms = Duration::from_millis;
        let mut press = LinkPress::default();
        press.update(true, t0);
        assert!(!press.may_ask());
        assert_eq!(press.due(), Some(t0 + MUX_DIR_DELAY));
        assert!(!press.run_timer(t0 + MUX_DIR_DELAY - ms(1)));

        // Ctrl+C and the like: only the pointer asks.
        press.key_pressed();
        assert_eq!(press.due(), None);
        assert!(!press.run_timer(t0 + ms(1000)));
        assert!(!press.may_ask());
        press.pointer_used();
        assert!(press.may_ask());

        // Resting on a candidate asks once the delay passed.
        press.update(false, t0);
        press.update(true, t0);
        assert!(!press.may_ask());
        assert!(press.run_timer(t0 + MUX_DIR_DELAY));
        assert!(press.may_ask());

        // Without the modifier the pointer asks nothing.
        press.update(false, t0);
        press.pointer_used();
        assert!(!press.may_ask());
    }

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
