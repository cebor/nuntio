//! Per-window state: tabs of split panes, layout (tab bar + terminal area),
//! and the keyboard state that belongs to the window. Pointer input is in
//! `window_pointer`, putting frames together in `window_frame`.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use nuntio_config::{Config, StatusBarPosition, StatusItem, TabTitle};
use nuntio_render::{CellMetrics, Rect, Renderer, UiRect, UiText};
use nuntio_term::{Rgb, TermHandle, TermMode, TermSize};
use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::event::Modifiers;
use winit::keyboard::PhysicalKey;
use winit::window::{Fullscreen, Window};

use crate::banner::Banner;
use crate::event::PaneId;
use crate::pane_tree::{Direction, Layout, PaneTree};
use crate::search_bar::SearchBar;
use crate::status_bar::{Stats, StatusBar};
use crate::style::{UiMetrics, hairline};
use crate::tab_bar::{TabBar, TabBarOptions};
use crate::tab_title::{self, TitleInfo};
use crate::tabs::Tabs;

#[path = "window_frame.rs"]
mod frame;
#[path = "window_pointer.rs"]
mod pointer;

pub use pointer::{MouseState, TabDrag};

pub const DEFAULT_TITLE: &str = "nuntio";
const BLINK_INTERVAL: Duration = Duration::from_millis(530);
/// Height of the macOS title bar that holds the traffic lights, in logical
/// pixels. A tab bar in it must be at least as high to keep them centered.
const TITLEBAR_HEIGHT: f64 = 28.0;
/// How long a tab title is reused before the process info is read again.
const TITLE_REFRESH: Duration = Duration::from_millis(250);
/// A new pane is shown once its output has paused this long, so the shell's
/// startup output (a greeting, the prompt) appears in one frame.
const REVEAL_QUIET: Duration = Duration::from_millis(20);
/// Latest time a new pane is shown, however busy or silent its shell is.
const REVEAL_MAX: Duration = Duration::from_millis(300);
/// Cells a pane grows or shrinks per resize shortcut, so each press is
/// clearly visible.
const RESIZE_STEP_CELLS: u32 = 2;
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
    /// The shell runs in WSL, so it takes Linux paths.
    pub wsl: bool,
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
    pub fn new(id: PaneId, term: TermHandle, wsl: bool) -> Self {
        Self {
            id,
            term,
            wsl,
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
    pub fn cached_title(&mut self, mode: TabTitle, now: Instant) -> (String, Option<Instant>) {
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

    /// Current grid size, once the pane has been laid out.
    #[cfg_attr(not(feature = "debug-server"), allow(dead_code))]
    pub fn grid_size(&self) -> Option<TermSize> {
        self.size
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
    /// A newer release exists: the tab bar shows a badge.
    pub update_badge: bool,
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
            update_badge: false,
        }
    }

    pub fn content(&self) -> &TabContent {
        &self.tabs.active().content
    }

    pub fn content_mut(&mut self) -> &mut TabContent {
        &mut self.tabs.active_mut().content
    }

    /// The panes of all tabs.
    pub fn panes(&self) -> impl Iterator<Item = &Pane> {
        self.tabs.iter().flat_map(|tab| &tab.content.panes)
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

    /// Cell sizes and scale factor the UI is laid out with.
    pub fn ui_metrics(&self) -> UiMetrics {
        UiMetrics {
            cell: self.renderer.cell_metrics(),
            small: self.renderer.small_cell_metrics(),
            scale: self.scale(),
        }
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
            self.ui_metrics(),
            TabBarOptions {
                left_inset: left_inset as f32,
                min_height: min_height as f32,
                window_controls: self.chrome == Chrome::Undecorated,
                update_badge: self.update_badge,
            },
        ))
    }

    /// Top edge and height of the status bar, if shown.
    fn status_bar_bounds(&self, config: &Config) -> Option<(f32, f32)> {
        if !config.status_bar.visible() {
            return None;
        }
        let height = StatusBar::height(self.ui_metrics());
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
            self.ui_metrics(),
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
        let drawn = self.status_bar(config, stats, &datetime).map(|bar| {
            bar.draw(
                stats,
                &datetime,
                background,
                foreground,
                config.status_bar.rainbow,
            )
        });
        if drawn == self.status_drawn {
            return false;
        }
        self.status_drawn = drawn;
        true
    }

    /// The status bar item at `pos`, for clicks.
    pub fn status_item_at(
        &self,
        config: &Config,
        stats: &Stats,
        pos: PhysicalPosition<f64>,
    ) -> Option<StatusItem> {
        if !self.status_bar_contains(config, pos) {
            return None;
        }
        self.status_bar(config, stats, &datetime(config))?
            .item_at(pos.x as f32)
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
        hairline(self.scale())
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
    pub fn grid_origin(&self, config: &Config, rect: Rect) -> (f32, f32) {
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

    /// The pointer is over the find bar.
    pub fn search_bar_contains(&self, config: &Config, pos: PhysicalPosition<f64>) -> bool {
        let Some(bar) = &self.search else {
            return false;
        };
        let rect = self.layout(config).rect(self.content().focused);
        rect.is_some_and(|rect| bar.contains(rect, self.ui_metrics(), pos.x as f32, pos.y as f32))
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
        banner.contains(pos.y as f32, self.banner_bottom(config), self.ui_metrics())
    }

    /// Add `text` to the query of the find bar. Returns false if the bar
    /// is closed.
    pub fn search_append(&mut self, text: &str) -> bool {
        let Some((bar, term)) = self.search_and_term() else {
            return false;
        };
        bar.append(text, term);
        self.window.request_redraw();
        true
    }

    /// Type `text` where the keyboard types: into the find bar while it
    /// is open, otherwise into the focused pane.
    pub fn type_text(&mut self, text: &str) {
        if !self.search_append(text) {
            self.type_bytes(text.as_bytes().to_vec());
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

    pub fn select_next_tab(&mut self) {
        self.select_tab((self.tabs.active_index() + 1) % self.tabs.len());
    }

    pub fn select_previous_tab(&mut self) {
        let len = self.tabs.len();
        self.select_tab((self.tabs.active_index() + len - 1) % len);
    }

    /// Move the focus to the pane next to the focused one.
    pub fn focus_neighbor(&mut self, config: &Config, direction: Direction) {
        let focused = self.content().focused;
        // A zoomed pane has no visible neighbors: find them in the split
        // layout and leave zoom only if there is one.
        let zoomed = self.content().tree.is_zoomed();
        if zoomed {
            self.content_mut().tree.toggle_zoom(focused);
        }
        match PaneTree::neighbor(focused, direction, &self.layout(config)) {
            Some(id) => {
                self.focus_pane(id);
                if zoomed {
                    self.resize_terms(config);
                }
            }
            None if zoomed => self.content_mut().tree.toggle_zoom(focused),
            None => {}
        }
    }

    /// Show the focused pane alone, or return to the split layout.
    pub fn toggle_zoom(&mut self, config: &Config) {
        let content = self.content_mut();
        content.tree.toggle_zoom(content.focused);
        self.resize_terms(config);
    }

    /// Grow or shrink the focused pane towards `direction`.
    pub fn resize_pane(&mut self, config: &Config, direction: Direction) {
        let layout = self.layout(config);
        let cell = self.renderer.cell_metrics();
        let step = match direction {
            Direction::Left | Direction::Right => RESIZE_STEP_CELLS * cell.width,
            Direction::Up | Direction::Down => RESIZE_STEP_CELLS * cell.height,
        } as f32;
        let content = self.content_mut();
        content
            .tree
            .resize(content.focused, direction, step, &layout);
        self.resize_terms(config);
    }

    /// Open the find bar. A selected piece of a line becomes the query.
    pub fn open_search(&mut self) {
        let selection = self
            .term()
            .selection_text()
            .map(|text| text.trim_end_matches(['\r', '\n']).to_owned())
            .filter(|text| !text.trim().is_empty() && !text.contains('\n'));
        self.search.get_or_insert_with(SearchBar::new);
        if let Some(text) = selection
            && let Some((bar, term)) = self.search_and_term()
        {
            term.clear_selection();
            bar.set_query(text, term);
        }
    }

    pub fn toggle_fullscreen(&self) {
        // Borderless is the native full screen (its own Space) on macOS.
        let fullscreen = self.window.fullscreen().is_some();
        let target = (!fullscreen).then_some(Fullscreen::Borderless(None));
        self.window.set_fullscreen(target);
    }

    pub fn toggle_maximized(&self) {
        self.window.set_maximized(!self.window.is_maximized());
    }

    /// Run the timers that are due at `now` (cursor blinking, title
    /// refresh, autoscroll, reveal) and return when the next one is.
    pub fn run_timers(&mut self, config: &Config, now: Instant) -> Option<Instant> {
        if self.blink.active && now >= self.blink.next_toggle {
            self.blink.visible = !self.blink.visible;
            self.blink.next_toggle = now + BLINK_INTERVAL;
            self.window.request_redraw();
        }
        if self.title_refresh.is_some_and(|t| now >= t) {
            // The redraw computes fresh titles and schedules no further one.
            self.invalidate_titles();
            self.window.request_redraw();
        }
        self.step_autoscroll(config, now);
        if self.reveal.is_some() && !self.reveal_holds(now) {
            self.window.request_redraw();
        }
        [
            self.reveal.map(|reveal| reveal.due()),
            self.blink.active.then_some(self.blink.next_toggle),
            self.title_refresh,
            self.mouse.autoscroll.map(|scroll| scroll.next),
        ]
        .into_iter()
        .flatten()
        .min()
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
    use crate::style::test_metrics::*;

    use super::*;

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
}
