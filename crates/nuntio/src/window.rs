//! Per-window state: tabs of split panes, layout (tab bar + terminal area),
//! drawing, and the mouse/keyboard state that belongs to the window.

use std::sync::Arc;
use std::time::{Duration, Instant};

use nuntio_config::Config;
use nuntio_render::{CellMetrics, Frame, FrameStatus, PaneView, Renderer, UiRect};
use nuntio_term::{CursorStyle, GridPoint, Link, Snapshot, TermHandle, TermMode, TermSize};
use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::event::Modifiers;
use winit::window::{ResizeDirection, Window};

use crate::banner::Banner;
use crate::event::PaneId;
use crate::ime;
use crate::mouse::{self, Button, ClickCounter, MouseAction, MouseMods};
use crate::pane_tree::{Divider, Layout, PaneTree, Rect};
use crate::search_bar::SearchBar;
use crate::tab_bar::{BarHit, TabBar, TabLabel, mix};
use crate::tabs::Tabs;

pub const DEFAULT_TITLE: &str = "nuntio";
pub const BLINK_INTERVAL: Duration = Duration::from_millis(530);
/// Width of the edge that resizes an undecorated window, in logical pixels.
const RESIZE_BORDER: f64 = 5.0;
/// Extra grab area around pane dividers, in logical pixels.
const DIVIDER_SLOP: f64 = 3.0;

pub struct Pane {
    pub id: PaneId,
    pub term: TermHandle,
    /// Title set by the application (OSC 0/2).
    pub title: Option<String>,
    /// Current grid size, to skip redundant resizes.
    size: Option<TermSize>,
}

impl Pane {
    pub fn new(id: PaneId, term: TermHandle) -> Self {
        Self {
            id,
            term,
            title: None,
            size: None,
        }
    }

    fn title(&self) -> String {
        self.title
            .clone()
            .unwrap_or_else(|| self.term.process_name())
    }

    fn resize(&mut self, size: TermSize) {
        if self.size != Some(size) {
            self.size = Some(size);
            self.term.resize(size);
        }
    }
}

/// The panes of one tab and how they are split.
pub struct TabContent {
    pub tree: PaneTree,
    pub panes: Vec<Pane>,
    pub focused: PaneId,
}

impl TabContent {
    pub fn new(pane: Pane) -> Self {
        Self {
            tree: PaneTree::new(pane.id),
            focused: pane.id,
            panes: vec![pane],
        }
    }

    pub fn pane(&self, id: PaneId) -> Option<&Pane> {
        self.panes.iter().find(|p| p.id == id)
    }

    pub fn pane_mut(&mut self, id: PaneId) -> Option<&mut Pane> {
        self.panes.iter_mut().find(|p| p.id == id)
    }

    pub fn focused_pane(&self) -> &Pane {
        self.pane(self.focused).expect("focused pane exists")
    }

    pub fn contains(&self, id: PaneId) -> bool {
        self.pane(id).is_some()
    }
}

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
    /// Sub-line remainder of pixel-precise (trackpad) scrolling.
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
}

#[derive(Debug, Clone, Copy)]
pub struct TabDrag {
    pub index: usize,
    pub press_x: f64,
    pub moved: bool,
}

/// Cursor blinking, only while the application asks for it (DECSCUSR).
pub struct Blink {
    pub active: bool,
    pub visible: bool,
    pub next_toggle: Instant,
}

impl Blink {
    /// Show the cursor and restart the interval, e.g. after typing.
    pub fn reset(&mut self) {
        self.visible = true;
        self.next_toggle = Instant::now() + BLINK_INTERVAL;
    }
}

/// How the window frame is provided, which decides whether our tab bar
/// must stay visible (to drag the window) and leave room for buttons.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Chrome {
    /// Normal system decorations.
    System,
    /// No decorations (our own header, or WSLg): the tab bar moves the
    /// window, edges resize it.
    Undecorated,
    /// macOS transparent title bar: tab bar sits in it, right of the buttons.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    TitlebarInset { left: f64 },
}

pub struct WindowState {
    pub window: Arc<Window>,
    pub renderer: Renderer,
    pub tabs: Tabs<TabContent>,
    pub chrome: Chrome,
    pub modifiers: Modifiers,
    pub mouse: MouseState,
    pub focused: bool,
    /// Uncommitted IME text, shown at the cursor.
    pub preedit: Option<String>,
    /// The find bar, searching the focused pane.
    pub search: Option<SearchBar>,
    pub blink: Blink,
    /// Cell the IME candidate window was last anchored to.
    ime_cell: Option<(u32, u32)>,
    title: String,
}

impl WindowState {
    pub fn new(window: Arc<Window>, renderer: Renderer, first: Pane, chrome: Chrome) -> Self {
        Self {
            window,
            renderer,
            tabs: Tabs::new(TabContent::new(first)),
            chrome,
            modifiers: Modifiers::default(),
            mouse: MouseState::default(),
            focused: true,
            preedit: None,
            search: None,
            blink: Blink {
                active: false,
                visible: true,
                next_toggle: Instant::now(),
            },
            ime_cell: None,
            title: String::new(),
        }
    }

    pub fn content(&self) -> &TabContent {
        &self.tabs.active().content
    }

    pub fn content_mut(&mut self) -> &mut TabContent {
        &mut self.tabs.active_mut().content
    }

    /// The terminal of the focused pane in the active tab.
    pub fn term(&self) -> &TermHandle {
        &self.content().focused_pane().term
    }

    /// Current grid size of the focused pane.
    pub fn grid(&self) -> TermSize {
        self.content().focused_pane().size.unwrap_or(TermSize {
            columns: 1,
            lines: 1,
            cell_width: 1,
            cell_height: 1,
        })
    }

    fn scale(&self) -> f64 {
        self.window.scale_factor()
    }

    fn bar_visible(&self, config: &Config) -> bool {
        self.tabs.len() > 1 || !config.tabs.hide_when_single || self.chrome != Chrome::System
    }

    /// The tab bar, if shown.
    pub fn tab_bar(&self, config: &Config) -> Option<TabBar> {
        if !self.bar_visible(config) {
            return None;
        }
        let left_inset = match self.chrome {
            Chrome::TitlebarInset { left } => (left * self.scale()) as f32,
            _ => 0.0,
        };
        Some(TabBar::new(
            self.window.inner_size().width as f32,
            self.tabs.len(),
            self.renderer.cell_metrics(),
            self.scale(),
            left_inset,
            self.chrome == Chrome::Undecorated,
        ))
    }

    /// Area below the tab bar that the panes share.
    fn terminal_area(&self, config: &Config) -> Rect {
        let size = self.window.inner_size();
        let bar = self.tab_bar(config).map_or(0.0, |bar| bar.height);
        Rect {
            x: 0.0,
            y: bar,
            width: size.width as f32,
            height: (size.height as f32 - bar).max(0.0),
        }
    }

    /// Thickness of the lines between panes.
    fn divider_width(&self) -> f32 {
        self.scale().round().max(1.0) as f32
    }

    /// Pane layout of the active tab.
    pub fn layout(&self, config: &Config) -> Layout {
        self.content()
            .tree
            .layout(self.terminal_area(config), self.divider_width())
    }

    /// Padding around each pane's grid in physical pixels.
    fn padding(&self, config: &Config) -> (f32, f32) {
        let p = config.window.padding;
        let scale = self.scale();
        (
            (p.x as f64 * scale).round() as f32,
            (p.y as f64 * scale).round() as f32,
        )
    }

    /// Top-left corner of a pane's grid.
    fn grid_origin(&self, config: &Config, rect: Rect) -> (f32, f32) {
        let (pad_x, pad_y) = self.padding(config);
        (rect.x + pad_x, rect.y + pad_y)
    }

    /// Fit every pane's terminal to its area, in all tabs.
    pub fn resize_terms(&mut self, config: &Config) {
        let area = self.terminal_area(config);
        let gap = self.divider_width();
        let padding = self.padding(config);
        let cell = self.renderer.cell_metrics();
        for tab in self.tabs.iter_mut() {
            let content = &mut tab.content;
            let layout = content.tree.layout(area, gap);
            for (id, rect) in layout.panes {
                if let Some(pane) = content.pane_mut(id) {
                    pane.resize(grid_size(rect, padding, cell));
                }
            }
        }
    }

    pub fn tab_title(&self, index: usize) -> String {
        let tab = self.tabs.iter().nth(index).expect("tab index in range");
        tab.content.focused_pane().title()
    }

    pub fn redraw(&mut self, config: &Config, banner: Option<&Banner>) -> FrameStatus {
        let layout = self.layout(config);
        let focused_id = self.content().focused;
        let split = layout.panes.len() > 1;

        let mut snapshots: Vec<(Snapshot, Rect, bool)> = Vec::with_capacity(layout.panes.len());
        for &(id, rect) in &layout.panes {
            let Some(pane) = self.tabs.active().content.pane(id) else {
                continue;
            };
            let is_focused = id == focused_id;
            let search = self.search.as_mut().and_then(|bar| bar.search_mut());
            let mut snapshot = match search {
                Some(search) if is_focused => pane.term.search_snapshot(search),
                _ => pane.term.snapshot(),
            };
            if let Some((link_pane, link)) = &self.mouse.hover_link
                && *link_pane == id
            {
                underline(&mut snapshot, link);
            }
            if is_focused {
                self.prepare_focused(config, &mut snapshot, rect);
            } else if let Some(cursor) = snapshot.cursor.as_mut() {
                cursor.style = CursorStyle::HollowBlock;
            }
            snapshots.push((snapshot, rect, is_focused));
        }
        let Some((first, ..)) = snapshots.first() else {
            return FrameStatus::Skipped;
        };
        let (background, foreground) = (first.background, first.foreground);

        let title = self.tab_title(self.tabs.active_index());
        if title != self.title {
            self.window.set_title(&title);
            self.title = title;
        }

        let (mut rects, mut texts) = match self.tab_bar(config) {
            Some(bar) => {
                let labels: Vec<TabLabel> = (0..self.tabs.len())
                    .map(|i| {
                        let tab = self.tabs.iter().nth(i).expect("tab index in range");
                        TabLabel {
                            title: self.tab_title(i),
                            active: i == self.tabs.active_index(),
                            activity: tab.activity,
                            bell: tab.bell,
                        }
                    })
                    .collect();
                bar.draw(
                    &labels,
                    self.mouse.hovered_bar,
                    self.window.is_maximized(),
                    background,
                    foreground,
                )
            }
            None => Default::default(),
        };

        let divider_color = mix(background, foreground, 0.25);
        for divider in &layout.dividers {
            let r = divider.rect;
            rects.push(UiRect {
                x: r.x,
                y: r.y,
                width: r.width,
                height: r.height,
                color: divider_color,
                radius: 0.0,
            });
        }

        if let Some(bar) = &self.search
            && let Some(rect) = layout.rect(focused_id)
        {
            let (bar_rects, bar_texts) = bar.draw(
                rect,
                self.renderer.cell_metrics(),
                self.scale(),
                background,
                foreground,
            );
            rects.extend(bar_rects);
            texts.extend(bar_texts);
        }

        if let Some(banner) = banner {
            let size = self.window.inner_size();
            let (rect, text) = banner.draw(
                size.width as f32,
                size.height as f32,
                self.renderer.cell_metrics(),
                self.scale(),
            );
            rects.push(rect);
            texts.push(text);
        }

        let panes: Vec<PaneView> = snapshots
            .iter()
            .map(|(snapshot, rect, is_focused)| {
                let (x, y) = self.grid_origin(config, *rect);
                PaneView {
                    snapshot,
                    x,
                    y,
                    area: [rect.x, rect.y, rect.width, rect.height],
                    dim: if split && !is_focused {
                        config.panes.dim_inactive
                    } else {
                        0.0
                    },
                }
            })
            .collect();
        self.renderer.render(&Frame {
            background,
            panes: &panes,
            rects: &rects,
            texts: &texts,
        })
    }

    /// Preedit, blinking and IME placement for the focused pane.
    fn prepare_focused(&mut self, config: &Config, snapshot: &mut Snapshot, rect: Rect) {
        if let Some(preedit) = &self.preedit {
            ime::overlay_preedit(snapshot, preedit);
        }
        let blinking =
            self.focused && self.preedit.is_none() && snapshot.cursor.is_some_and(|c| c.blinking);
        if blinking != self.blink.active {
            self.blink.active = blinking;
            self.blink.reset();
        }
        if let Some(cursor) = snapshot.cursor.as_mut() {
            let (x, y) = self.grid_origin(config, rect);
            let cell = self.renderer.cell_metrics();
            self.update_ime_area(
                x as u32 + cursor.column as u32 * cell.width,
                y as u32 + cursor.line as u32 * cell.height,
            );
            if !self.focused {
                cursor.style = CursorStyle::HollowBlock;
            }
        }
        if self.blink.active && !self.blink.visible {
            snapshot.cursor = None;
        }
    }

    /// Keep the IME candidate window next to the cursor.
    fn update_ime_area(&mut self, x: u32, y: u32) {
        if self.ime_cell == Some((x, y)) {
            return;
        }
        self.ime_cell = Some((x, y));
        let cell = self.renderer.cell_metrics();
        self.window.set_ime_cursor_area(
            PhysicalPosition::new(x, y),
            PhysicalSize::new(cell.width, cell.height),
        );
    }

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

    /// The pointer is over the find bar.
    pub fn search_bar_contains(&self, config: &Config, pos: PhysicalPosition<f64>) -> bool {
        let Some(bar) = &self.search else {
            return false;
        };
        let rect = self.layout(config).rect(self.content().focused);
        rect.is_some_and(|rect| {
            bar.contains(
                rect,
                self.renderer.cell_metrics(),
                self.scale(),
                pos.x as f32,
                pos.y as f32,
            )
        })
    }

    /// The pointer is over the notification banner.
    pub fn banner_contains(&self, banner: &Banner, pos: PhysicalPosition<f64>) -> bool {
        let height = self.window.inner_size().height as f32;
        banner.contains(
            pos.y as f32,
            height,
            self.renderer.cell_metrics(),
            self.scale(),
        )
    }

    /// Resize edge under the pointer, for windows without decorations.
    pub fn resize_edge(&self, pos: PhysicalPosition<f64>) -> Option<ResizeDirection> {
        if self.chrome != Chrome::Undecorated {
            return None;
        }
        let size = self.window.inner_size();
        let border = RESIZE_BORDER * self.scale();
        let (w, h) = (size.width as f64, size.height as f64);
        let (left, right) = (pos.x < border, pos.x >= w - border);
        let (top, bottom) = (pos.y < border, pos.y >= h - border);
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

    fn mouse_mods(&self) -> MouseMods {
        let mods = self.modifiers.state();
        MouseMods {
            shift: mods.shift_key(),
            alt: mods.alt_key(),
            ctrl: mods.control_key(),
        }
    }

    /// Mouse events go to the application unless Shift is held, which
    /// forces local selection like in xterm.
    pub fn reports_mouse(&self, mode: TermMode) -> bool {
        mouse::reporting_enabled(mode) && !self.modifiers.state().shift_key()
    }

    pub fn report(&mut self, button: Option<Button>, action: MouseAction, point: GridPoint) {
        let mode = self.term().mode();
        let mods = self.mouse_mods();
        if let Some(bytes) =
            mouse::encode_report(button, action, mods, point.column, point.line, mode)
        {
            self.term().write(bytes);
        }
    }

    /// Forget per-pane pointer and IME state after focus moves.
    pub fn reset_focus_state(&mut self) {
        self.search = None;
        self.mouse.hover_link = None;
        self.mouse.selecting = false;
        self.mouse.reported_button = None;
        self.mouse.last_reported_cell = None;
        self.ime_cell = None;
        self.window.request_redraw();
    }

    /// Switch tabs, telling applications that asked about focus changes.
    pub fn select_tab(&mut self, index: usize) {
        if index == self.tabs.active_index() || index >= self.tabs.len() {
            return;
        }
        self.send_focus(false);
        self.tabs.select(index);
        self.send_focus(true);
        self.reset_focus_state();
    }

    /// Move keyboard focus to another pane of the active tab.
    pub fn focus_pane(&mut self, id: PaneId) {
        if id == self.content().focused || !self.content().contains(id) {
            return;
        }
        self.send_focus(false);
        self.content_mut().focused = id;
        self.send_focus(true);
        self.reset_focus_state();
    }

    /// Report focus in/out to the focused terminal if it enabled that mode.
    pub fn send_focus(&self, focused: bool) {
        let term = self.term();
        if self.focused && term.mode().contains(TermMode::FOCUS_IN_OUT) {
            term.write(if focused {
                &b"\x1b[I"[..]
            } else {
                &b"\x1b[O"[..]
            });
        }
    }
}

/// Underline the cells of a link (viewport positions, inclusive).
fn underline(snapshot: &mut Snapshot, link: &Link) {
    let start = (link.start.1, link.start.0 as i32);
    let end = (link.end.1, link.end.0 as i32);
    for line in 0..snapshot.lines {
        for column in 0..snapshot.columns {
            let pos = (line as i32, column as i32);
            if pos >= start && pos <= end {
                snapshot.cells[line * snapshot.columns + column]
                    .style
                    .underline = true;
            }
        }
    }
}

/// How many cells fit into a pane, minus padding.
fn grid_size(rect: Rect, padding: (f32, f32), cell: CellMetrics) -> TermSize {
    let fit = |available: f32, pad: f32, cell: u32| {
        (((available - 2.0 * pad).max(0.0) as u32) / cell).clamp(1, u16::MAX as u32) as u16
    };
    TermSize {
        columns: fit(rect.width, padding.0, cell.width),
        lines: fit(rect.height, padding.1, cell.height),
        cell_width: cell.width.min(u16::MAX as u32) as u16,
        cell_height: cell.height.min(u16::MAX as u32) as u16,
    }
}
