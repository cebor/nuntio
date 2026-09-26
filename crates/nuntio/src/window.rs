//! Per-window state: tabs of split panes, layout (tab bar + terminal area),
//! drawing, and the mouse/keyboard state that belongs to the window.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use nuntio_config::{Config, StatusBarPosition, TabTitle};
use nuntio_render::{CellMetrics, Frame, FrameStatus, PaneView, Renderer, UiRect, UiText};
use nuntio_term::{
    CursorStyle, GridPoint, Link, Rgb, Snapshot, TermHandle, TermMode, TermSize, UnderlineStyle,
};
use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::event::Modifiers;
use winit::keyboard::PhysicalKey;
use winit::window::{ResizeDirection, Window};

use crate::banner::Banner;
use crate::event::PaneId;
use crate::ime;
use crate::link;
use crate::mouse::{self, Button, ClickCounter, MouseAction, MouseMods};
use crate::pane_tree::{Divider, Layout, PaneTree, Rect};
use crate::search_bar::SearchBar;
use crate::status_bar::{Stats, StatusBar};
use crate::tab_bar::{BarHit, TabBar, TabLabel, mix};
use crate::tab_title::{self, TitleInfo};
use crate::tabs::Tabs;

pub const DEFAULT_TITLE: &str = "nuntio";
pub const BLINK_INTERVAL: Duration = Duration::from_millis(530);
/// Width of the edge that resizes an undecorated window, in logical pixels.
const RESIZE_BORDER: f64 = 5.0;
/// Radius of the window's corners where nuntio rounds them itself (Linux),
/// in logical pixels, as GNOME's.
const WINDOW_RADIUS: f64 = 12.0;
/// Height of the macOS title bar that holds the traffic lights, in logical
/// pixels. A tab bar in it must be at least as high to keep them centered.
const TITLEBAR_HEIGHT: f64 = 28.0;
/// Extra grab area around pane dividers, in logical pixels.
const DIVIDER_SLOP: f64 = 3.0;
/// Time between steps while a selection drag scrolls the pane.
pub const AUTOSCROLL_INTERVAL: Duration = Duration::from_millis(50);
/// Fastest autoscroll, in lines per step.
const AUTOSCROLL_MAX_LINES: i32 = 5;
/// How long a tab title is reused before the process info is read again.
const TITLE_REFRESH: Duration = Duration::from_millis(250);
/// A new pane is shown once its output has paused this long, so the shell's
/// startup output (a greeting, the prompt) appears in one frame.
const REVEAL_QUIET: Duration = Duration::from_millis(20);
/// Latest time a new pane is shown, however busy or silent its shell is.
const REVEAL_MAX: Duration = Duration::from_millis(300);

/// Holds back frames after a new tab or split opens, until its shell has
/// drawn its first screen. Otherwise the window flashes the empty pane and
/// then the startup output line by line, as each read of the pty is drawn.
#[derive(Debug, Clone, Copy)]
pub struct Reveal {
    pane: PaneId,
    /// Show the pane by then even if the shell is still busy.
    deadline: Instant,
    /// Output has arrived; show the pane unless more comes before then.
    settle: Option<Instant>,
}

impl Reveal {
    fn new(pane: PaneId, now: Instant) -> Self {
        Self {
            pane,
            deadline: now + REVEAL_MAX,
            settle: None,
        }
    }

    /// Output from the held pane pushes the reveal back.
    fn output(&mut self, now: Instant) {
        self.settle = Some(now + REVEAL_QUIET);
    }

    /// When the pane is shown.
    pub fn due(&self) -> Instant {
        self.settle.map_or(self.deadline, |s| s.min(self.deadline))
    }
}

pub struct Pane {
    pub id: PaneId,
    pub term: TermHandle,
    /// Title set by the application (OSC 0/2).
    pub title: Option<String>,
    /// Current grid size, to skip redundant resizes.
    size: Option<TermSize>,
    /// The last computed title: reading the process info on every frame
    /// would cost several syscalls per tab under heavy output.
    title_cache: Option<CachedTitle>,
}

struct CachedTitle {
    mode: TabTitle,
    title: String,
    computed: Instant,
}

impl CachedTitle {
    /// The title, if it is recent enough and for this mode.
    fn get(&self, mode: TabTitle, now: Instant) -> Option<&str> {
        (self.mode == mode && now < self.expiry()).then_some(self.title.as_str())
    }

    fn expiry(&self) -> Instant {
        self.computed + TITLE_REFRESH
    }
}

impl Pane {
    pub fn new(id: PaneId, term: TermHandle) -> Self {
        Self {
            id,
            term,
            title: None,
            size: None,
            title_cache: None,
        }
    }

    /// Set the application's title (OSC 0/2); `None` resets it.
    pub fn set_title(&mut self, title: Option<String>) {
        self.title = title;
        self.title_cache = None;
    }

    /// The tab title, and when it expires if it came from the cache (it
    /// may be stale then, so it should be looked at again).
    fn cached_title(&mut self, mode: TabTitle, now: Instant) -> (String, Option<Instant>) {
        if let Some(cache) = &self.title_cache
            && let Some(title) = cache.get(mode, now)
        {
            return (title.to_owned(), Some(cache.expiry()));
        }
        let title = self.title(mode);
        self.title_cache = Some(CachedTitle {
            mode,
            title: title.clone(),
            computed: now,
        });
        (title, None)
    }

    fn title(&self, mode: TabTitle) -> String {
        let wants_directory = matches!(mode, TabTitle::Auto | TabTitle::Path);
        let info = TitleInfo {
            application: self.title.clone(),
            process: self.term.process_name(),
            directory: wants_directory
                .then(|| self.term.working_directory())
                .flatten(),
            shell_idle: (mode == TabTitle::Auto)
                .then(|| self.term.foreground_is_shell())
                .flatten(),
        };
        tab_title::title(mode, info, dirs::home_dir().as_deref())
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

impl Chrome {
    /// Whether nuntio cuts the window's rounded corners itself. Windows 11
    /// (via DWM) and macOS round their windows on their own.
    pub fn draws_corners(self) -> bool {
        cfg!(target_os = "linux") && self == Chrome::Undecorated
    }
}

pub struct WindowState {
    pub window: Arc<Window>,
    pub renderer: Renderer,
    pub tabs: Tabs<TabContent>,
    pub chrome: Chrome,
    /// The window was created transparent, so `window.opacity` can apply.
    pub transparent: bool,
    pub modifiers: Modifiers,
    /// Held keys nuntio used itself (shortcuts, the find bar), whose
    /// release a program must not see either.
    pub used_keys: HashSet<PhysicalKey>,
    pub mouse: MouseState,
    pub focused: bool,
    /// Uncommitted IME text, shown at the cursor.
    pub preedit: Option<String>,
    /// The find bar, searching the focused pane.
    pub search: Option<SearchBar>,
    pub blink: Blink,
    /// Frames skipped in a row, to stop retrying a stuck surface.
    pub skipped_frames: u32,
    /// A frame showed cached tab titles; redraw once they expire so they
    /// catch up with the processes.
    pub title_refresh: Option<Instant>,
    /// A new pane waiting for its shell's first screen; frames are held.
    pub reveal: Option<Reveal>,
    /// Cell the IME candidate window was last anchored to.
    ime_cell: Option<(u32, u32)>,
    /// The status bar as of the last new sample, to skip redraws that
    /// wouldn't change it.
    status_drawn: Option<(Vec<UiRect>, Vec<UiText>)>,
    title: String,
}

impl WindowState {
    pub fn new(
        window: Arc<Window>,
        renderer: Renderer,
        first: Pane,
        chrome: Chrome,
        transparent: bool,
    ) -> Self {
        Self {
            window,
            renderer,
            tabs: Tabs::new(TabContent::new(first)),
            chrome,
            transparent,
            modifiers: Modifiers::default(),
            used_keys: HashSet::new(),
            mouse: MouseState::default(),
            focused: true,
            preedit: None,
            search: None,
            blink: Blink {
                active: false,
                visible: true,
                next_toggle: Instant::now(),
            },
            skipped_frames: 0,
            title_refresh: None,
            reveal: None,
            ime_cell: None,
            status_drawn: None,
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

    /// The open find bar and the terminal it searches.
    pub fn search_and_term(&mut self) -> Option<(&mut SearchBar, &TermHandle)> {
        let term = &self.tabs.active().content.focused_pane().term;
        Some((self.search.as_mut()?, term))
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

    /// Keep showing the current frame until `pane`, the new focused pane,
    /// has drawn its first screen.
    pub fn hold_reveal(&mut self, pane: PaneId, now: Instant) {
        self.reveal = Some(Reveal::new(pane, now));
    }

    /// Output from `pane`. True if it is the held pane, which is then
    /// ready for the next wakeup without being drawn.
    pub fn reveal_output(&mut self, pane: PaneId, now: Instant) -> bool {
        match &mut self.reveal {
            Some(reveal) if reveal.pane == pane => {
                reveal.output(now);
                if let Some(p) = self.content().pane(pane) {
                    p.term.ack_wakeup();
                }
                true
            }
            _ => false,
        }
    }

    /// Whether frames are still held back. Ends the hold once it is due,
    /// or once the held pane is no longer the focused one.
    pub fn reveal_holds(&mut self, now: Instant) -> bool {
        let Some(reveal) = self.reveal else {
            return false;
        };
        let holds = now < reveal.due() && self.content().focused == reveal.pane;
        if !holds {
            self.reveal = None;
        }
        holds
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
        // macOS hides the window buttons in full screen.
        let (left_inset, min_height) = match self.chrome {
            Chrome::TitlebarInset { left } if left > 0.0 && self.window.fullscreen().is_none() => {
                (left * self.scale(), TITLEBAR_HEIGHT * self.scale())
            }
            _ => (0.0, 0.0),
        };
        Some(TabBar::new(
            self.window.inner_size().width as f32,
            self.tabs.len(),
            self.renderer.cell_metrics(),
            self.scale(),
            left_inset as f32,
            min_height as f32,
            self.chrome == Chrome::Undecorated,
        ))
    }

    /// Top edge and height of the status bar, if shown.
    fn status_bar_bounds(&self, config: &Config) -> Option<(f32, f32)> {
        if !config.status_bar.visible() {
            return None;
        }
        let height = StatusBar::height(self.renderer.small_cell_metrics(), self.scale());
        let top = match config.status_bar.position {
            StatusBarPosition::Top => self.tab_bar(config).map_or(0.0, |bar| bar.height),
            StatusBarPosition::Bottom => self.window.inner_size().height as f32 - height,
        };
        Some((top, height))
    }

    /// The status bar, if shown.
    fn status_bar(&self, config: &Config, stats: &Stats, datetime: &str) -> Option<StatusBar> {
        let (top, _) = self.status_bar_bounds(config)?;
        Some(StatusBar::new(
            self.window.inner_size().width as f32,
            top,
            &config.status_bar.arranged_items(),
            stats,
            datetime,
            self.renderer.small_cell_metrics(),
            self.renderer.cell_metrics(),
            self.scale(),
        ))
    }

    /// Whether the status bar looks different from the last time this
    /// was asked, e.g. after a new sample. Most samples change nothing
    /// visible (same rounded values, a clock without seconds), and a whole
    /// frame per second would keep an idle terminal busy.
    pub fn status_bar_changed(&mut self, config: &Config, stats: &Stats) -> bool {
        let datetime = datetime(config);
        // Colors come from the theme, which redraws on its own when it
        // changes; fixed ones compare the rest.
        let (background, foreground) = (
            Rgb::default(),
            Rgb {
                r: 255,
                g: 255,
                b: 255,
            },
        );
        let drawn = self
            .status_bar(config, stats, &datetime)
            .map(|bar| bar.draw(stats, &datetime, background, foreground));
        if drawn == self.status_drawn {
            return false;
        }
        self.status_drawn = drawn;
        true
    }

    /// The pointer is over the status bar.
    pub fn status_bar_contains(&self, config: &Config, pos: PhysicalPosition<f64>) -> bool {
        self.status_bar_bounds(config)
            .is_some_and(|(top, height)| pos.y as f32 >= top && (pos.y as f32) < top + height)
    }

    /// Area between the tab bar and the status bar that the panes share.
    fn terminal_area(&self, config: &Config) -> Rect {
        let size = self.window.inner_size();
        let bar = self.tab_bar(config).map_or(0.0, |bar| bar.height);
        let status = self.status_bar_bounds(config).map_or(0.0, |(_, h)| h);
        let y = match config.status_bar.position {
            StatusBarPosition::Top => bar + status,
            StatusBarPosition::Bottom => bar,
        };
        Rect {
            x: 0.0,
            y,
            width: size.width as f32,
            height: (size.height as f32 - bar - status).max(0.0),
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

    /// Inner window size for the configured grid (`window.columns` and
    /// `window.lines`) in a single pane, with the bars around it.
    pub fn size_for_grid(&self, config: &Config) -> PhysicalSize<u32> {
        let bars = self.tab_bar(config).map_or(0.0, |bar| bar.height)
            + self.status_bar_bounds(config).map_or(0.0, |(_, h)| h);
        let (width, height) = window_size(
            (config.window.columns, config.window.lines),
            self.renderer.cell_metrics(),
            self.padding(config),
            bars,
        );
        PhysicalSize::new(width, height)
    }

    /// Drop every cached tab title, so the next frame computes them all.
    /// Refreshing only the expired ones would leave others cached, which
    /// would schedule yet another refresh, tab after tab.
    pub fn invalidate_titles(&mut self) {
        for tab in self.tabs.iter_mut() {
            for pane in &mut tab.content.panes {
                pane.title_cache = None;
            }
        }
        self.title_refresh = None;
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

    pub fn redraw(
        &mut self,
        config: &Config,
        stats: &Stats,
        banner: Option<&Banner>,
    ) -> FrameStatus {
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

        let active = self.tabs.active_index();
        let bar = self.tab_bar(config);
        // Titles look up the foreground process, so only compute the ones
        // that are shown.
        let now = Instant::now();
        let mut refresh: Option<Instant> = None;
        let labels: Vec<TabLabel> = self
            .tabs
            .iter_mut()
            .enumerate()
            .filter(|&(i, _)| bar.is_some() || i == active)
            .map(|(i, tab)| {
                let focused = tab.content.focused;
                let (title, expiry) = match tab.content.pane_mut(focused) {
                    Some(pane) => pane.cached_title(config.tabs.title, now),
                    None => (String::new(), None),
                };
                if let Some(expiry) = expiry {
                    refresh = Some(refresh.map_or(expiry, |r| r.min(expiry)));
                }
                TabLabel {
                    title,
                    active: i == active,
                    activity: tab.activity,
                    bell: tab.bell,
                }
            })
            .collect();
        self.title_refresh = refresh;
        if let Some(label) = labels.iter().find(|l| l.active)
            && label.title != self.title
        {
            self.window.set_title(&label.title);
            self.title = label.title.clone();
        }

        let (mut rects, mut texts) = match bar {
            Some(bar) => bar.draw(
                &labels,
                self.mouse.hovered_bar,
                self.window.is_maximized(),
                background,
                foreground,
            ),
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

        if config.status_bar.visible() {
            let datetime = datetime(config);
            if let Some(bar) = self.status_bar(config, stats, &datetime) {
                let (bar_rects, bar_texts) = bar.draw(stats, &datetime, background, foreground);
                rects.extend(bar_rects);
                texts.extend(bar_texts);
            }
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

        // Where the link under the pointer leads: OSC 8 links may show
        // other text.
        if let Some((id, link)) = &self.mouse.hover_link
            && let Some(mut rect) = layout.rect(*id)
            && let Some(pos) = self.mouse.position
        {
            // Stay above the banner, which covers the bottom of the window.
            if let Some(banner) = banner {
                let cell = self.renderer.cell_metrics();
                let (top, _) = banner.bounds(self.banner_bottom(config), cell, self.scale());
                rect.height = rect.height.min(top - rect.y).max(0.0);
            }
            let (hint_rects, hint_texts) = link::draw_hint(
                &link.url,
                rect,
                (pos.x as f32, pos.y as f32),
                self.renderer.small_cell_metrics(),
                self.scale(),
                background,
                foreground,
            );
            rects.extend(hint_rects);
            texts.extend(hint_texts);
        }

        if let Some(banner) = banner {
            let size = self.window.inner_size();
            let (rect, text) = banner.draw(
                size.width as f32,
                self.banner_bottom(config),
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
        // Maximized windows sit flush with the screen edges: square corners.
        let corner_radius = if self.chrome.draws_corners() && !self.window.is_maximized() {
            (WINDOW_RADIUS * self.scale()) as f32
        } else {
            0.0
        };
        self.renderer.render(&Frame {
            background,
            background_opacity: config.window.opacity,
            panes: &panes,
            rects: &rects,
            texts: &texts,
            corner_radius,
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

    /// Bottom edge of the banner: above a status bar at the bottom.
    fn banner_bottom(&self, config: &Config) -> f32 {
        match (config.status_bar.position, self.status_bar_bounds(config)) {
            (StatusBarPosition::Bottom, Some((top, _))) => top,
            _ => self.window.inner_size().height as f32,
        }
    }

    /// The pointer is over the notification banner.
    pub fn banner_contains(
        &self,
        config: &Config,
        banner: &Banner,
        pos: PhysicalPosition<f64>,
    ) -> bool {
        banner.contains(
            pos.y as f32,
            self.banner_bottom(config),
            self.renderer.cell_metrics(),
            self.scale(),
        )
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

    pub fn report(&mut self, button: Option<Button>, action: MouseAction, point: GridPoint) {
        let mode = self.term().mode();
        let mods = self.mouse_mods();
        if let Some(bytes) =
            mouse::encode_report(button, action, mods, point.column, point.line, mode)
        {
            self.term().write(bytes);
        }
    }

    /// Send typed input to the focused pane, as the keyboard would.
    pub fn type_bytes(&mut self, bytes: Vec<u8>) {
        let term = self.term();
        term.clear_selection();
        term.write(bytes);
        self.blink.reset();
    }

    /// Re-place the IME candidate window at the next frame, e.g. after the
    /// scale factor changed.
    pub fn invalidate_ime_area(&mut self) {
        self.ime_cell = None;
    }

    /// Forget per-pane pointer and IME state after focus moves.
    pub fn reset_focus_state(&mut self) {
        self.search = None;
        self.mouse.hover_link = None;
        self.mouse.selecting = false;
        self.mouse.autoscroll = None;
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

/// The status bar's date and time, now.
fn datetime(config: &Config) -> String {
    // Validated when the config was loaded, so formatting can't fail.
    chrono::Local::now()
        .format(&config.status_bar.datetime_format)
        .to_string()
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
                    .underline = Some(UnderlineStyle::Single);
            }
        }
    }
}

/// Window size for a grid of `columns`×`lines` cells plus padding and
/// `bars` (the height of the tab and status bars); the inverse of
/// `grid_size`.
fn window_size(
    (columns, lines): (u16, u16),
    cell: CellMetrics,
    padding: (f32, f32),
    bars: f32,
) -> (u32, u32) {
    let width = columns as f32 * cell.width as f32 + 2.0 * padding.0;
    let height = lines as f32 * cell.height as f32 + 2.0 * padding.1 + bars;
    (width.ceil() as u32, height.ceil() as u32)
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

#[cfg(test)]
mod tests {
    use nuntio_term::{CellStyle, Rgb, SnapshotCell};

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
    fn reveal_waits_for_output_to_pause_but_not_forever() {
        let now = Instant::now();
        let mut reveal = Reveal::new(PaneId(1), now);
        assert_eq!(reveal.due(), now + REVEAL_MAX, "silent shell");

        let ms = Duration::from_millis;
        reveal.output(now + ms(50));
        assert_eq!(reveal.due(), now + ms(50) + REVEAL_QUIET);
        reveal.output(now + ms(60));
        assert_eq!(reveal.due(), now + ms(60) + REVEAL_QUIET, "more output");

        reveal.output(now + REVEAL_MAX - ms(1));
        assert_eq!(reveal.due(), now + REVEAL_MAX, "busy shell");
    }

    #[test]
    fn cached_titles_expire_and_follow_the_mode() {
        let now = Instant::now();
        let cache = CachedTitle {
            mode: TabTitle::Auto,
            title: "~/code".into(),
            computed: now,
        };
        assert_eq!(cache.get(TabTitle::Auto, now), Some("~/code"));
        assert_eq!(cache.get(TabTitle::Process, now), None, "other mode");
        assert_eq!(
            cache.get(TabTitle::Auto, now + TITLE_REFRESH),
            None,
            "expired"
        );
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

    #[test]
    fn grid_fits_cells_inside_the_padding() {
        let rect = Rect {
            x: 0.0,
            y: 0.0,
            width: 105.0,
            height: 65.0,
        };
        let size = grid_size(rect, (2.0, 2.0), CELL);
        assert_eq!((size.columns, size.lines), (10, 3));
        assert_eq!((size.cell_width, size.cell_height), (10, 20));
    }

    #[test]
    fn window_size_fits_the_grid_exactly() {
        let (width, height) = window_size((80, 24), CELL, (8.0, 6.0), 30.0);
        assert_eq!((width, height), (816, 522));
        let rect = Rect {
            x: 0.0,
            y: 30.0,
            width: width as f32,
            height: height as f32 - 30.0,
        };
        let size = grid_size(rect, (8.0, 6.0), CELL);
        assert_eq!((size.columns, size.lines), (80, 24));
    }

    #[test]
    fn grid_never_collapses_to_zero() {
        let rect = Rect {
            x: 0.0,
            y: 0.0,
            width: 30.0,
            height: 30.0,
        };
        let size = grid_size(rect, (200.0, 200.0), CELL);
        assert_eq!((size.columns, size.lines), (1, 1));
    }

    #[test]
    fn underline_covers_a_wrapped_link() {
        let cell = SnapshotCell {
            c: 'x',
            zerowidth: None,
            fg: Rgb::default(),
            bg: Rgb::default(),
            style: CellStyle::default(),
            underline_color: None,
        };
        let mut snapshot = Snapshot {
            columns: 4,
            lines: 3,
            cells: vec![cell; 12],
            ..Snapshot::default()
        };
        let link = Link {
            url: "https://x".into(),
            start: (2, 0),
            end: (1, 1),
        };
        underline(&mut snapshot, &link);
        let underlined: Vec<bool> = snapshot
            .cells
            .iter()
            .map(|c| c.style.underline.is_some())
            .collect();
        #[rustfmt::skip]
        let expected = [
            false, false, true, true,
            true, true, false, false,
            false, false, false, false,
        ];
        assert_eq!(underlined, expected);
    }
}
