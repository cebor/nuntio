//! Per-window state: tabs, layout (tab bar + terminal area), drawing, and
//! the mouse/keyboard state that belongs to the window.

use std::sync::Arc;
use std::time::{Duration, Instant};

use nuntio_config::Config;
use nuntio_render::{CellMetrics, Frame, FrameStatus, PaneView, Renderer};
use nuntio_term::{CursorStyle, GridPoint, TermHandle, TermMode, TermSize};
use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::event::Modifiers;
use winit::window::{ResizeDirection, Window};

use crate::banner::Banner;
use crate::event::PaneId;
use crate::ime;
use crate::mouse::{self, Button, ClickCounter, MouseAction, MouseMods};
use crate::tab_bar::{BarHit, TabBar, TabLabel};
use crate::tabs::Tabs;

pub const DEFAULT_TITLE: &str = "nuntio";
pub const BLINK_INTERVAL: Duration = Duration::from_millis(530);
/// Width of the edge that resizes an undecorated window, in logical pixels.
const RESIZE_BORDER: f64 = 5.0;

pub struct Pane {
    pub id: PaneId,
    pub term: TermHandle,
}

/// Pointer state for selection, mouse reporting and the tab bar.
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
    /// No decorations (WSLg): the tab bar moves the window, edges resize it.
    Undecorated,
    /// macOS transparent title bar: tab bar sits in it, right of the buttons.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    TitlebarInset { left: f64 },
}

pub struct WindowState {
    pub window: Arc<Window>,
    pub renderer: Renderer,
    pub tabs: Tabs<Pane>,
    pub chrome: Chrome,
    pub grid: TermSize,
    pub modifiers: Modifiers,
    pub mouse: MouseState,
    pub focused: bool,
    /// Uncommitted IME text, shown at the cursor.
    pub preedit: Option<String>,
    pub blink: Blink,
    /// Cell the IME candidate window was last anchored to.
    ime_cell: Option<(usize, usize)>,
    title: String,
}

impl WindowState {
    pub fn new(window: Arc<Window>, renderer: Renderer, first: Pane, chrome: Chrome) -> Self {
        let grid = TermSize {
            columns: 1,
            lines: 1,
            cell_width: 1,
            cell_height: 1,
        };
        Self {
            window,
            renderer,
            tabs: Tabs::new(first),
            chrome,
            grid,
            modifiers: Modifiers::default(),
            mouse: MouseState::default(),
            focused: true,
            preedit: None,
            blink: Blink {
                active: false,
                visible: true,
                next_toggle: Instant::now(),
            },
            ime_cell: None,
            title: String::new(),
        }
    }

    /// The terminal of the active tab.
    pub fn term(&self) -> &TermHandle {
        &self.tabs.active().pane.term
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

    fn bar_height(&self, config: &Config) -> u32 {
        self.tab_bar(config).map_or(0, |bar| bar.height as u32)
    }

    /// Padding around the grid in physical pixels.
    fn padding(&self, config: &Config) -> (u32, u32) {
        let p = config.window.padding;
        let scale = self.scale();
        (
            (p.x as f64 * scale).round() as u32,
            (p.y as f64 * scale).round() as u32,
        )
    }

    /// Top-left corner of the terminal grid.
    fn grid_origin(&self, config: &Config) -> (u32, u32) {
        let (pad_x, pad_y) = self.padding(config);
        (pad_x, self.bar_height(config) + pad_y)
    }

    /// Recompute the grid size and resize every tab's terminal.
    pub fn resize_terms(&mut self, config: &Config) {
        let size = self.window.inner_size();
        let bar = self.bar_height(config);
        let area = PhysicalSize::new(size.width, size.height.saturating_sub(bar));
        let grid = grid_size(area, self.padding(config), self.renderer.cell_metrics());
        if grid != self.grid {
            self.grid = grid;
            for tab in self.tabs.iter() {
                tab.pane.term.resize(grid);
            }
        }
    }

    pub fn tab_title(&self, index: usize) -> String {
        let tab = self.tabs.iter().nth(index).expect("tab index in range");
        tab.title
            .clone()
            .unwrap_or_else(|| tab.pane.term.process_name())
    }

    pub fn redraw(&mut self, config: &Config, banner: Option<&Banner>) -> FrameStatus {
        let mut snapshot = self.term().snapshot();
        if let Some(preedit) = &self.preedit {
            ime::overlay_preedit(&mut snapshot, preedit);
        }

        let blinking =
            self.focused && self.preedit.is_none() && snapshot.cursor.is_some_and(|c| c.blinking);
        if blinking != self.blink.active {
            self.blink.active = blinking;
            self.blink.reset();
        }
        if let Some(cursor) = snapshot.cursor.as_mut() {
            self.update_ime_area(config, cursor.column, cursor.line);
            if !self.focused {
                cursor.style = CursorStyle::HollowBlock;
            }
        }
        if self.blink.active && !self.blink.visible {
            snapshot.cursor = None;
        }

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
                    snapshot.background,
                    snapshot.foreground,
                )
            }
            None => Default::default(),
        };

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

        let (x, y) = self.grid_origin(config);
        let panes = [PaneView {
            snapshot: &snapshot,
            x: x as f32,
            y: y as f32,
        }];
        self.renderer.render(&Frame {
            background: snapshot.background,
            panes: &panes,
            rects: &rects,
            texts: &texts,
        })
    }

    /// Keep the IME candidate window next to the cursor.
    fn update_ime_area(&mut self, config: &Config, column: usize, line: usize) {
        if self.ime_cell == Some((column, line)) {
            return;
        }
        self.ime_cell = Some((column, line));
        let (x, y) = self.grid_origin(config);
        let cell = self.renderer.cell_metrics();
        self.window.set_ime_cursor_area(
            PhysicalPosition::new(
                x + column as u32 * cell.width,
                y + line as u32 * cell.height,
            ),
            PhysicalSize::new(cell.width, cell.height),
        );
    }

    /// Grid cell under a window position, clamped to the grid.
    pub fn cell_at(&self, config: &Config, pos: PhysicalPosition<f64>) -> GridPoint {
        let (x0, y0) = self.grid_origin(config);
        let cell = self.renderer.cell_metrics();
        let x = (pos.x - x0 as f64).max(0.0);
        let y = (pos.y - y0 as f64).max(0.0);
        let column = ((x / cell.width as f64) as usize).min(self.grid.columns as usize - 1);
        let line = ((y / cell.height as f64) as usize).min(self.grid.lines as usize - 1);
        let within = x - column as f64 * cell.width as f64;
        GridPoint {
            column,
            line,
            right_half: within >= cell.width as f64 / 2.0,
        }
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

    /// Switch tabs, telling applications that asked about focus changes.
    pub fn select_tab(&mut self, index: usize) {
        if index == self.tabs.active_index() || index >= self.tabs.len() {
            return;
        }
        self.send_focus(false);
        self.tabs.select(index);
        self.send_focus(true);
        self.mouse.selecting = false;
        self.mouse.reported_button = None;
        self.ime_cell = None;
        self.window.request_redraw();
    }

    /// Report focus in/out to the active terminal if it enabled that mode.
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

/// How many cells fit into an area, minus padding.
fn grid_size(area: PhysicalSize<u32>, padding: (u32, u32), cell: CellMetrics) -> TermSize {
    let fit = |available: u32, pad: u32, cell: u32| {
        (available.saturating_sub(2 * pad) / cell).clamp(1, u16::MAX as u32) as u16
    };
    TermSize {
        columns: fit(area.width, padding.0, cell.width),
        lines: fit(area.height, padding.1, cell.height),
        cell_width: cell.width.min(u16::MAX as u32) as u16,
        cell_height: cell.height.min(u16::MAX as u32) as u16,
    }
}
