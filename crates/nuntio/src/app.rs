use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use nuntio_config::{
    Color, Config, ConfigWatcher, DEFAULT_THEME, OptionAsMeta, Theme, ThemeSelection, ThemeSet,
};
use nuntio_render::{FrameStatus, Renderer};
use nuntio_term::{
    Palette, Rgb, SelectionKind, Shell, SpawnOptions, TermEvent, TermHandle, TermMode,
};
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalPosition};
use winit::event::{
    ElementState, Ime, KeyEvent, Modifiers, MouseButton, MouseScrollDelta, WindowEvent,
};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoopProxy};
use winit::keyboard::{Key, ModifiersKeyState, NamedKey};
use winit::platform::modifier_supplement::KeyEventExtModifierSupplement;
use winit::window::{
    CursorIcon, ResizeDirection, Theme as WindowTheme, Window, WindowAttributes, WindowId,
};

use crate::actions::{Action, Bindings};
use crate::banner::{Banner, Severity};
use crate::event::{PaneId, UserEvent};
use crate::input;
use crate::mouse::{Button, MouseAction};
use crate::pane_tree::{Axis, Direction, PaneTree};
use crate::search_bar::SearchBar;
use crate::status_bar::Stats;
use crate::sysmon::SystemMonitor;
use crate::tab_bar::BarHit;
use crate::window::{
    BLINK_INTERVAL, Chrome, DEFAULT_TITLE, Pane, TabContent, TabDrag, WindowState,
};

const MIN_FONT_SIZE: f32 = 4.0;
const MAX_FONT_SIZE: f32 = 72.0;
/// Lines scrolled per wheel notch.
const WHEEL_LINES: f64 = 3.0;
/// Two clicks on the tab bar within this time maximize the window.
const DOUBLE_CLICK: Duration = Duration::from_millis(400);
/// Pointer travel before a pressed tab starts moving.
const TAB_DRAG_THRESHOLD: f64 = 4.0;

/// WSLg's Weston (9.0, RDP backend) segfaults on pointer motion over windows
/// with winit's client-side decorations, taking every Wayland client down with
/// it. Under WSLg on Wayland we therefore create the window undecorated.
#[cfg(target_os = "linux")]
fn is_wslg_wayland(event_loop: &ActiveEventLoop) -> bool {
    use winit::platform::wayland::ActiveEventLoopExtWayland;

    let wslg =
        std::env::var_os("WSL_DISTRO_NAME").is_some() && std::path::Path::new("/mnt/wslg").is_dir();
    let wayland = event_loop.is_wayland();
    if wslg && wayland {
        tracing::info!("WSLg detected, disabling client-side decorations");
    }
    wslg && wayland
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn is_wslg_wayland(_event_loop: &ActiveEventLoop) -> bool {
    false
}

/// Window decorations for this platform and config.
fn chrome(
    event_loop: &ActiveEventLoop,
    config: &Config,
    attrs: WindowAttributes,
) -> (WindowAttributes, Chrome) {
    #[cfg(target_os = "macos")]
    {
        use nuntio_config::MacosTitlebar;
        use winit::platform::macos::WindowAttributesExtMacOS;

        let _ = event_loop;
        match config.window.effective_macos_titlebar() {
            MacosTitlebar::Native => (attrs, Chrome::System),
            // Room for the traffic-light buttons, in logical pixels.
            MacosTitlebar::Transparent => (
                attrs
                    .with_titlebar_transparent(true)
                    .with_fullsize_content_view(true)
                    .with_title_hidden(true),
                Chrome::TitlebarInset { left: 78.0 },
            ),
            MacosTitlebar::None => (
                attrs
                    .with_titlebar_transparent(true)
                    .with_fullsize_content_view(true)
                    .with_title_hidden(true)
                    .with_titlebar_buttons_hidden(true),
                Chrome::TitlebarInset { left: 0.0 },
            ),
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        use nuntio_config::Decorations;

        if config.window.decorations == Decorations::Custom || is_wslg_wayland(event_loop) {
            // Keep the drop shadow a decorated window would have.
            #[cfg(windows)]
            let attrs = {
                use winit::platform::windows::WindowAttributesExtWindows;
                attrs.with_undecorated_shadow(true)
            };
            // Transparent, so that the rounded corners we cut out show the
            // desktop behind them.
            let attrs = attrs.with_transparent(Chrome::Undecorated.draws_corners());
            (attrs.with_decorations(false), Chrome::Undecorated)
        } else {
            (attrs, Chrome::System)
        }
    }
}

/// Ask Windows 11 to round the corners of our undecorated window, as it
/// does for decorated ones. Windows 10 doesn't know the attribute and keeps
/// them square, like all its windows.
#[cfg(windows)]
fn round_corners(window: &Window) {
    use windows_sys::Win32::Graphics::Dwm::{
        DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND, DwmSetWindowAttribute,
    };
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};

    let Ok(handle) = window.window_handle() else {
        return;
    };
    let RawWindowHandle::Win32(handle) = handle.as_raw() else {
        return;
    };
    let preference = DWMWCP_ROUND;
    // SAFETY: `hwnd` is our live window, and the pointer and size describe
    // `preference`, which outlives the call.
    let result = unsafe {
        DwmSetWindowAttribute(
            handle.hwnd.get() as _,
            DWMWA_WINDOW_CORNER_PREFERENCE as _,
            (&raw const preference).cast(),
            size_of_val(&preference) as u32,
        )
    };
    if result != 0 {
        tracing::debug!(result, "Windows did not round the window corners");
    }
}

/// Whether Alt should act as Meta (ESC prefix). On macOS Option composes
/// characters unless Option-as-Meta is enabled for the pressed side.
fn alt_is_meta(mods: &Modifiers, option_as_meta: OptionAsMeta) -> bool {
    if !cfg!(target_os = "macos") {
        return mods.state().alt_key();
    }
    let left = mods.lalt_state() == ModifiersKeyState::Pressed;
    let right = mods.ralt_state() == ModifiersKeyState::Pressed;
    match option_as_meta {
        OptionAsMeta::None => false,
        OptionAsMeta::Left => left,
        OptionAsMeta::Right => right,
        OptionAsMeta::Both => left || right,
    }
}

fn resize_cursor(direction: ResizeDirection) -> CursorIcon {
    match direction {
        ResizeDirection::East => CursorIcon::EResize,
        ResizeDirection::West => CursorIcon::WResize,
        ResizeDirection::North => CursorIcon::NResize,
        ResizeDirection::South => CursorIcon::SResize,
        ResizeDirection::NorthEast => CursorIcon::NeResize,
        ResizeDirection::NorthWest => CursorIcon::NwResize,
        ResizeDirection::SouthEast => CursorIcon::SeResize,
        ResizeDirection::SouthWest => CursorIcon::SwResize,
    }
}

fn to_rgb(c: Color) -> Rgb {
    Rgb {
        r: c.r,
        g: c.g,
        b: c.b,
    }
}

fn palette_from(theme: &Theme) -> Palette {
    let ansi = theme.ansi().map(to_rgb);
    let mut palette = Palette::new(
        &ansi,
        to_rgb(theme.foreground),
        to_rgb(theme.background),
        to_rgb(theme.cursor),
    );
    palette.selection_foreground = to_rgb(theme.selection_foreground);
    palette.selection_background = to_rgb(theme.selection_background);
    palette
}

fn themes_dir(config_path: Option<&Path>) -> Option<PathBuf> {
    nuntio_config::themes_dir(config_path?)
}

pub struct App {
    config: Config,
    config_path: Option<PathBuf>,
    /// Keeps hot reload running.
    _watcher: Option<ConfigWatcher>,
    themes: ThemeSet,
    /// Colors of the active theme, given to every pane.
    palette: Palette,
    /// The OS prefers dark mode (for `theme = { light, dark }`).
    os_dark: bool,
    banner: Option<Banner>,
    proxy: EventLoopProxy<UserEvent>,
    bindings: Bindings,
    /// Samples for the status bar while it is shown.
    system_monitor: Option<SystemMonitor>,
    stats: Stats,
    clipboard: Option<arboard::Clipboard>,
    /// Current font size in points; changed by zoom shortcuts.
    font_size: f32,
    next_pane_id: u64,
    state: Option<WindowState>,
    /// The last tab was closed; quit at the next opportunity.
    exit_requested: bool,
    /// A fatal error that ended the event loop.
    error: Option<anyhow::Error>,
}

impl App {
    pub fn new(
        config: Config,
        config_path: Option<PathBuf>,
        banner: Option<Banner>,
        proxy: EventLoopProxy<UserEvent>,
    ) -> Self {
        let clipboard = arboard::Clipboard::new()
            .inspect_err(|err| tracing::warn!("clipboard unavailable: {err}"))
            .ok();
        let themes_dir = themes_dir(config_path.as_deref());
        let watcher = config_path.as_deref().and_then(|path| {
            let proxy = proxy.clone();
            ConfigWatcher::new(path, themes_dir.as_deref(), move || {
                let _ = proxy.send_event(UserEvent::ConfigChanged);
            })
            .inspect_err(|err| tracing::info!("config hot reload unavailable: {err}"))
            .ok()
        });
        let (themes, theme_warnings) = ThemeSet::load(themes_dir.as_deref());
        let (bindings, binding_warnings) = Bindings::from_config(&config.keybindings);

        let mut app = Self {
            font_size: config.font.size,
            config,
            config_path,
            _watcher: watcher,
            themes,
            palette: Palette::default(),
            os_dark: true,
            banner,
            proxy,
            bindings,
            system_monitor: None,
            stats: Stats::default(),
            clipboard,
            next_pane_id: 0,
            state: None,
            exit_requested: false,
            error: None,
        };
        let theme_warning = app.update_palette();
        app.sync_system_monitor();
        app.notify(Banner::config(
            Severity::Warning,
            theme_warnings
                .into_iter()
                .chain(binding_warnings)
                .chain(theme_warning)
                .collect(),
        ));
        app
    }

    /// Show a banner (and log its messages). Errors take precedence.
    fn notify(&mut self, banner: Option<Banner>) {
        let Some(new) = banner else {
            return;
        };
        for message in &new.messages {
            match new.severity {
                Severity::Error => tracing::error!("{message}"),
                Severity::Warning => tracing::warn!("{message}"),
            }
        }
        self.banner = match self.banner.take() {
            Some(mut old) if old.severity == new.severity && old.title == new.title => {
                old.messages.extend(new.messages);
                Some(old)
            }
            Some(old) if old.severity == Severity::Error => Some(old),
            _ => Some(new),
        };
        if let Some(state) = self.state.as_ref() {
            state.window.request_redraw();
        }
    }

    /// Pick the theme for the current config and OS appearance and apply
    /// it to all panes. Returns a warning if the theme doesn't exist.
    fn update_palette(&mut self) -> Option<String> {
        let name = match &self.config.theme {
            ThemeSelection::Single(name) => name,
            ThemeSelection::Auto { light, dark } => {
                if self.os_dark {
                    dark
                } else {
                    light
                }
            }
        };
        let (theme, warning) = match self.themes.get(name) {
            Some(theme) => (theme, None),
            None => {
                let available: Vec<&str> = self.themes.names().collect();
                let warning = format!(
                    "theme \"{name}\" not found (available: {})",
                    available.join(", ")
                );
                let default = self.themes.get(DEFAULT_THEME).expect("built-in theme");
                (default, Some(warning))
            }
        };
        self.palette = palette_from(theme);
        if let Some(state) = self.state.as_ref() {
            for pane in state.tabs.iter().flat_map(|t| &t.content.panes) {
                pane.term.set_palette(self.palette.clone());
            }
            state.window.request_redraw();
        }
        warning
    }

    /// Start, restart or stop sampling to match the status bar config.
    fn sync_system_monitor(&mut self) {
        let bar = &self.config.status_bar;
        let wanted = bar.visible().then_some(bar.items.as_slice());
        let running = self.system_monitor.as_ref().map(SystemMonitor::items);
        if wanted == running {
            return;
        }
        // Dropping the old monitor stops its thread.
        self.system_monitor = None;
        self.stats.clear();
        if let Some(items) = wanted {
            tracing::debug!("starting system monitor");
            self.system_monitor = Some(SystemMonitor::start(items, self.proxy.clone()));
        }
    }

    /// Re-read the config file and apply what changed. An invalid file
    /// leaves the current settings untouched.
    fn reload_config(&mut self) {
        let Some(path) = self.config_path.clone() else {
            return;
        };
        let loaded = match nuntio_config::load(&path) {
            Ok(loaded) => loaded,
            Err(err) => {
                self.banner = None;
                self.notify(Banner::config(Severity::Error, vec![err.to_string()]));
                return;
            }
        };
        tracing::info!(path = %path.display(), "config reloaded");
        let mut warnings = loaded.warnings;
        let (themes, theme_warnings) = ThemeSet::load(themes_dir(Some(&path)).as_deref());
        let (bindings, binding_warnings) = Bindings::from_config(&loaded.config.keybindings);
        warnings.extend(theme_warnings);
        warnings.extend(binding_warnings);

        let old = std::mem::replace(&mut self.config, loaded.config);
        self.themes = themes;
        self.bindings = bindings;
        warnings.extend(self.update_palette());
        self.sync_system_monitor();

        if let Some(state) = self.state.as_mut() {
            if old.font.family != self.config.font.family {
                warnings.extend(
                    state
                        .renderer
                        .set_font_family(self.config.font.family.clone()),
                );
            }
            if old.font.size != self.config.font.size {
                self.font_size = self.config.font.size;
                let scale = state.window.scale_factor();
                state.renderer.set_font_size(self.font_size, scale);
            }
            // Padding, font and tab bar settings all affect the grid.
            state.resize_terms(&self.config);
            state.window.request_redraw();
        }
        self.banner = None;
        self.notify(Banner::config(Severity::Warning, warnings));
    }

    pub fn into_result(self) -> Result<()> {
        self.error.map_or(Ok(()), Err)
    }

    fn create_renderer(&self, window: &Arc<Window>, chrome: Chrome) -> Result<Renderer> {
        let size = window.inner_size();
        Ok(Renderer::new(
            window.clone(),
            size.width,
            size.height,
            window.scale_factor(),
            self.config.font.family.clone(),
            self.font_size,
            chrome.draws_corners(),
        )?)
    }

    /// Start a shell in a new pane. The size is corrected by the next
    /// `resize_terms`.
    fn spawn_pane(&mut self, cwd: Option<PathBuf>) -> Result<Pane> {
        let id = PaneId(self.next_pane_id);
        self.next_pane_id += 1;
        let proxy = self.proxy.clone();
        let shell = self.config.shell.as_ref();
        // `wsl.exe --cd ~` picks the directory; a Windows one would be ignored.
        let cwd = cwd.filter(|_| !shell.is_some_and(|s| s.is_wsl()));
        let options = SpawnOptions {
            shell: shell.map(|s| {
                let (program, args) = s.command();
                Shell { program, args }
            }),
            working_directory: cwd,
            scrollback: self.config.scrollback,
            palette: self.palette.clone(),
        };
        let size = self.state.as_ref().map_or(
            nuntio_term::TermSize {
                columns: 80,
                lines: 24,
                cell_width: 1,
                cell_height: 1,
            },
            |s| s.grid(),
        );
        let term = TermHandle::spawn(options, size, move |event| {
            let _ = proxy.send_event(UserEvent::Term(id, event));
        })?;
        Ok(Pane::new(id, term))
    }

    fn create_window(&mut self, event_loop: &ActiveEventLoop) -> Result<WindowState> {
        let attrs = Window::default_attributes()
            .with_title(DEFAULT_TITLE)
            .with_inner_size(LogicalSize::new(900.0, 600.0));
        // App id / WM_CLASS, matching the .desktop file (Wayland and X11).
        #[cfg(target_os = "linux")]
        let attrs = winit::platform::wayland::WindowAttributesExtWayland::with_name(
            attrs, "nuntio", "nuntio",
        );
        let (attrs, chrome) = chrome(event_loop, &self.config, attrs);
        let window = Arc::new(
            event_loop
                .create_window(attrs)
                .context("failed to create window")?,
        );
        window.set_ime_allowed(true);
        #[cfg(windows)]
        if chrome == Chrome::Undecorated {
            round_corners(&window);
        }
        let mut renderer = self.create_renderer(&window, chrome)?;
        if let Some(warning) = renderer.take_font_warning() {
            self.notify(Banner::config(Severity::Warning, vec![warning]));
        }
        // Follow the OS appearance for `theme = { light, dark }`.
        let os_dark = window.theme() != Some(WindowTheme::Light);
        if os_dark != self.os_dark {
            self.os_dark = os_dark;
            if matches!(self.config.theme, ThemeSelection::Auto { .. }) {
                let warning = self.update_palette();
                self.notify(Banner::config(
                    Severity::Warning,
                    warning.into_iter().collect(),
                ));
            }
        }
        let pane = self.spawn_pane(None)?;
        let mut state = WindowState::new(window, renderer, pane, chrome);
        state.resize_terms(&self.config);
        Ok(state)
    }

    fn fail(&mut self, event_loop: &ActiveEventLoop, err: anyhow::Error) {
        tracing::error!("{err:#}");
        self.error = Some(err);
        event_loop.exit();
    }

    fn set_clipboard(&mut self, text: String) {
        if let Some(clipboard) = self.clipboard.as_mut()
            && let Err(err) = clipboard.set_text(text)
        {
            tracing::warn!("failed to write clipboard: {err}");
        }
    }

    fn clipboard_text(&mut self) -> Option<String> {
        let clipboard = self.clipboard.as_mut()?;
        clipboard
            .get_text()
            .inspect_err(|err| tracing::debug!("failed to read clipboard: {err}"))
            .ok()
    }

    fn copy_selection(&mut self) {
        if let Some(text) = self.state.as_ref().and_then(|s| s.term().selection_text()) {
            self.set_clipboard(text);
        }
    }

    fn set_font_size(&mut self, size: f32) {
        self.font_size = size.clamp(MIN_FONT_SIZE, MAX_FONT_SIZE);
        if let Some(state) = self.state.as_mut() {
            let scale = state.window.scale_factor();
            state.renderer.set_font_size(self.font_size, scale);
            state.resize_terms(&self.config);
            state.window.request_redraw();
        }
    }

    fn new_tab(&mut self) {
        let cwd = self
            .state
            .as_ref()
            .and_then(|s| s.term().working_directory());
        let pane = match self.spawn_pane(cwd) {
            Ok(pane) => pane,
            Err(err) => {
                tracing::error!("failed to open tab: {err:#}");
                return;
            }
        };
        let Some(state) = self.state.as_mut() else {
            return;
        };
        state.send_focus(false);
        state.tabs.open(TabContent::new(pane));
        state.reset_focus_state();
        // The first extra tab may show the tab bar and shrink the grid.
        state.resize_terms(&self.config);
        state.window.request_redraw();
    }

    /// Split the focused pane; the new pane starts in the same directory.
    fn split(&mut self, axis: Axis) {
        let cwd = self
            .state
            .as_ref()
            .and_then(|s| s.term().working_directory());
        let pane = match self.spawn_pane(cwd) {
            Ok(pane) => pane,
            Err(err) => {
                tracing::error!("failed to split pane: {err:#}");
                return;
            }
        };
        let Some(state) = self.state.as_mut() else {
            return;
        };
        let new = pane.id;
        let content = state.content_mut();
        let target = content.focused;
        content.tree.split(target, new, axis);
        content.panes.push(pane);
        state.focus_pane(new);
        state.resize_terms(&self.config);
    }

    /// Close a pane; closing a tab's last pane closes the tab.
    fn close_pane(&mut self, id: PaneId) {
        let Some(state) = self.state.as_mut() else {
            return;
        };
        let Some(index) = state.tabs.position(|c| c.contains(id)) else {
            return;
        };
        let active = index == state.tabs.active_index();
        let Some(tab) = state.tabs.get_mut(index) else {
            return;
        };
        let content = &mut tab.content;
        let Some(next) = content.tree.remove(id) else {
            // The tab's last pane.
            self.close_tab(index);
            return;
        };
        let was_focused = content.focused == id;
        content.panes.retain(|p| p.id != id);
        if was_focused {
            content.focused = next;
            if active {
                state.send_focus(true);
                state.reset_focus_state();
            }
        }
        state.mouse.divider_drag = None;
        state.resize_terms(&self.config);
        state.window.request_redraw();
    }

    fn resize_pane(&mut self, direction: Direction) {
        let Some(state) = self.state.as_mut() else {
            return;
        };
        let layout = state.layout(&self.config);
        let cell = state.renderer.cell_metrics();
        // Move by two cells, so each press is clearly visible.
        let step = match direction {
            Direction::Left | Direction::Right => 2 * cell.width,
            Direction::Up | Direction::Down => 2 * cell.height,
        } as f32;
        let content = state.content_mut();
        let focused = content.focused;
        content.tree.resize(focused, direction, step, &layout);
        state.resize_terms(&self.config);
        state.window.request_redraw();
    }

    /// Close a tab; closing the last one quits.
    fn close_tab(&mut self, index: usize) {
        let Some(state) = self.state.as_mut() else {
            return;
        };
        if state.tabs.len() == 1 {
            self.exit_requested = true;
            return;
        }
        let was_active = index == state.tabs.active_index();
        state.tabs.close(index);
        if was_active {
            state.send_focus(true);
            state.reset_focus_state();
        }
        state.mouse.hovered_bar = None;
        state.mouse.tab_drag = None;
        state.resize_terms(&self.config);
        state.window.request_redraw();
    }

    fn run_action(&mut self, action: Action) {
        let Some(state) = self.state.as_mut() else {
            return;
        };
        match action {
            Action::Copy => self.copy_selection(),
            Action::Paste => {
                if let Some(text) = self.clipboard_text()
                    && let Some(state) = self.state.as_mut()
                {
                    match state.search.as_mut() {
                        // Pasting into the find bar extends the query.
                        Some(bar) => {
                            bar.query.push_str(text.lines().next().unwrap_or(""));
                            bar.update(&state.tabs.active().content.focused_pane().term);
                        }
                        None => state.term().paste(&text),
                    }
                }
            }
            Action::ScrollPageUp => state.term().scroll_page(true),
            Action::ScrollPageDown => state.term().scroll_page(false),
            Action::ScrollLineUp => state.term().scroll(1),
            Action::ScrollLineDown => state.term().scroll(-1),
            Action::ClearScrollback => state.term().clear_history(),
            Action::FontIncrease => self.set_font_size(self.font_size + 1.0),
            Action::FontDecrease => self.set_font_size(self.font_size - 1.0),
            Action::FontReset => self.set_font_size(self.config.font.size),
            Action::NewTab => self.new_tab(),
            Action::CloseTab => {
                let index = state.tabs.active_index();
                self.close_tab(index);
            }
            Action::NextTab => {
                let next = (state.tabs.active_index() + 1) % state.tabs.len();
                state.select_tab(next);
            }
            Action::PreviousTab => {
                let len = state.tabs.len();
                state.select_tab((state.tabs.active_index() + len - 1) % len);
            }
            Action::SelectTab(index) => state.select_tab(index),
            Action::ReloadConfig => self.reload_config(),
            Action::ClosePane => {
                let id = state.content().focused;
                self.close_pane(id);
            }
            Action::SplitVertical => self.split(Axis::Vertical),
            Action::SplitHorizontal => self.split(Axis::Horizontal),
            Action::FocusPane(direction) => {
                let layout = state.layout(&self.config);
                let focused = state.content().focused;
                if let Some(id) = PaneTree::neighbor(focused, direction, &layout) {
                    state.focus_pane(id);
                }
            }
            Action::ResizePane(direction) => self.resize_pane(direction),
            Action::Search => {
                if state.search.is_none() {
                    state.search = Some(SearchBar::new());
                }
            }
            Action::ZoomPane => {
                let content = state.content_mut();
                let focused = content.focused;
                content.tree.toggle_zoom(focused);
                state.resize_terms(&self.config);
            }
        }
        if let Some(state) = self.state.as_ref() {
            state.window.request_redraw();
        }
    }

    fn keyboard_input(&mut self, event: KeyEvent) {
        let Some(state) = self.state.as_ref() else {
            return;
        };
        if event.state != ElementState::Pressed {
            return;
        }
        let mods = state.modifiers.state();
        if state.search.is_some() && self.search_key(&event) {
            return;
        }
        let Some(state) = self.state.as_ref() else {
            return;
        };
        let unmodified = event.key_without_modifiers();
        if let Some(action) = self.bindings.lookup(&unmodified, mods) {
            self.run_action(action);
            return;
        }
        // Unbound Cmd/Super combinations are shortcuts, not text.
        if mods.super_key() {
            return;
        }
        let key_input = input::KeyInput {
            key: &event.logical_key,
            unmodified: &unmodified,
            text: event.text.as_deref(),
            location: event.location,
            shift: mods.shift_key(),
            ctrl: mods.control_key(),
            meta: alt_is_meta(&state.modifiers, self.config.macos.option_as_meta),
        };
        if let Some(bytes) = input::encode_key(&key_input, state.term().mode()) {
            state.term().clear_selection();
            state.term().write(bytes);
            if let Some(state) = self.state.as_mut() {
                state.blink.reset();
            }
        }
    }

    /// Keys for the open find bar. Returns whether the key was used;
    /// shortcuts not handled here still work while searching.
    fn search_key(&mut self, event: &KeyEvent) -> bool {
        let Some(state) = self.state.as_mut() else {
            return false;
        };
        let mods = state.modifiers.state();
        let term = &state.tabs.active().content.focused_pane().term;
        let Some(bar) = state.search.as_mut() else {
            return false;
        };
        match &event.logical_key {
            Key::Named(NamedKey::Escape) => state.search = None,
            Key::Named(NamedKey::Enter) => bar.next(term, !mods.shift_key()),
            Key::Named(NamedKey::Backspace) => {
                bar.query.pop();
                bar.update(term);
            }
            // Alt+R toggles regex mode, like in many editors.
            Key::Character(c) if mods.alt_key() && c.eq_ignore_ascii_case("r") => {
                bar.regex = !bar.regex;
                bar.update(term);
            }
            _ => {
                let text = event.text.as_deref().unwrap_or("");
                let typed = !text.is_empty() && !text.chars().any(char::is_control);
                if !typed || mods.control_key() || mods.super_key() || mods.alt_key() {
                    return false;
                }
                bar.query.push_str(text);
                bar.update(term);
            }
        }
        state.window.request_redraw();
        true
    }

    /// The link under the pointer, if the link modifier (Ctrl, Cmd on
    /// macOS) is held.
    fn update_hover_link(&mut self) {
        let Some(state) = self.state.as_mut() else {
            return;
        };
        let mods = state.modifiers.state();
        let held = if cfg!(target_os = "macos") {
            mods.super_key()
        } else {
            mods.control_key()
        };
        let link = state.mouse.position.filter(|_| held).and_then(|pos| {
            let id = state.pane_at(&self.config, pos)?;
            let point = state.cell_in(&self.config, id, pos);
            let link = state.content().pane(id)?.term.link_at(point)?;
            Some((id, link))
        });
        if link != state.mouse.hover_link {
            state.mouse.hover_link = link;
            state.window.request_redraw();
        }
    }

    fn mouse_input(&mut self, button: MouseButton, pressed: bool) {
        let Some(state) = self.state.as_mut() else {
            return;
        };
        let Some(pos) = state.mouse.position else {
            return;
        };
        let button = match button {
            MouseButton::Left => Button::Left,
            MouseButton::Middle => Button::Middle,
            MouseButton::Right => Button::Right,
            _ => return,
        };

        if !pressed
            && button == Button::Left
            && (state.mouse.tab_drag.take().is_some() || state.mouse.divider_drag.take().is_some())
        {
            return;
        }
        // A click on the banner dismisses it.
        if pressed
            && let Some(banner) = &self.banner
            && state.banner_contains(banner, pos)
        {
            self.banner = None;
            state.window.request_redraw();
            return;
        }

        // Window edges (undecorated windows) and the tab bar come first.
        if pressed && button == Button::Left {
            if let Some(direction) = state.resize_edge(pos) {
                let _ = state.window.drag_resize_window(direction);
                return;
            }
            if let Some(bar) = state.tab_bar(&self.config)
                && let Some(hit) = bar.hit(pos.x as f32, pos.y as f32)
            {
                match hit {
                    BarHit::Tab(index) => {
                        state.select_tab(index);
                        state.mouse.tab_drag = Some(TabDrag {
                            index,
                            press_x: pos.x,
                            moved: false,
                        });
                    }
                    BarHit::Close(index) => self.close_tab(index),
                    BarHit::Empty => {
                        // Double click maximizes, like a title bar.
                        let now = Instant::now();
                        let double = state
                            .mouse
                            .last_bar_click
                            .is_some_and(|t| now.duration_since(t) < DOUBLE_CLICK);
                        if double {
                            state.mouse.last_bar_click = None;
                            let maximized = state.window.is_maximized();
                            state.window.set_maximized(!maximized);
                        } else {
                            state.mouse.last_bar_click = Some(now);
                            let _ = state.window.drag_window();
                        }
                    }
                    BarHit::Minimize => state.window.set_minimized(true),
                    BarHit::Maximize => {
                        let maximized = state.window.is_maximized();
                        state.window.set_maximized(!maximized);
                    }
                    BarHit::CloseWindow => self.exit_requested = true,
                }
                return;
            }
        }
        // Middle click closes a tab, like in browsers and iTerm2.
        if pressed
            && button == Button::Middle
            && let Some(bar) = state.tab_bar(&self.config)
            && let Some(BarHit::Tab(index) | BarHit::Close(index)) =
                bar.hit(pos.x as f32, pos.y as f32)
        {
            self.close_tab(index);
            return;
        }

        if pressed
            && (state.search_bar_contains(&self.config, pos)
                || state.status_bar_contains(&self.config, pos))
        {
            return;
        }
        if pressed
            && button == Button::Left
            && let Some((_, link)) = &state.mouse.hover_link
        {
            let url = link.url.clone();
            if let Err(err) = open::that_detached(&url) {
                self.notify(Banner::new(
                    Severity::Warning,
                    "Link",
                    vec![format!("failed to open {url}: {err}")],
                ));
            }
            return;
        }
        if pressed {
            if button == Button::Left
                && let Some(divider) = state.divider_at(&self.config, pos)
            {
                state.mouse.divider_drag = Some(divider);
                return;
            }
            // Clicking a pane focuses it; the click then acts inside it.
            if let Some(id) = state.pane_at(&self.config, pos) {
                state.focus_pane(id);
            }
        }

        let point = state.cell_at(&self.config, pos);
        // Application mouse mode: forward presses and matching releases.
        let mode = state.term().mode();
        if pressed && state.reports_mouse(mode) {
            state.report(Some(button), MouseAction::Press, point);
            state.mouse.reported_button = Some(button);
            return;
        }
        if !pressed && state.mouse.reported_button == Some(button) {
            state.mouse.reported_button = None;
            state.report(Some(button), MouseAction::Release, point);
            return;
        }

        if button != Button::Left {
            return;
        }
        if pressed {
            let kind = match state
                .mouse
                .clicks
                .click(Instant::now(), point.column, point.line)
            {
                2 => SelectionKind::Semantic,
                3 => SelectionKind::Lines,
                _ if state.modifiers.state().alt_key() => SelectionKind::Block,
                _ => SelectionKind::Simple,
            };
            state.term().start_selection(kind, point);
            state.mouse.selecting = true;
            state.window.request_redraw();
        } else if state.mouse.selecting {
            state.mouse.selecting = false;
            if self.config.mouse.copy_on_select {
                self.copy_selection();
            }
        }
    }

    fn cursor_moved(&mut self, pos: PhysicalPosition<f64>) {
        let Some(state) = self.state.as_mut() else {
            return;
        };
        state.mouse.position = Some(pos);
        self.update_hover_link();
        let Some(state) = self.state.as_mut() else {
            return;
        };
        let over_link = state.mouse.hover_link.is_some();

        if let Some(divider) = &state.mouse.divider_drag {
            let at = match divider.axis {
                Axis::Vertical => pos.x,
                Axis::Horizontal => pos.y,
            } as f32;
            let divider = divider.clone();
            state.content_mut().tree.drag_divider(&divider, at);
            state.resize_terms(&self.config);
            state.window.request_redraw();
            return;
        }

        let divider_icon = state.divider_at(&self.config, pos).map(|d| match d.axis {
            Axis::Vertical => CursorIcon::ColResize,
            Axis::Horizontal => CursorIcon::RowResize,
        });
        let icon = state
            .resize_edge(pos)
            .map(resize_cursor)
            .or(divider_icon)
            .unwrap_or(CursorIcon::Default);
        let bar = state.tab_bar(&self.config);
        let bar_hit = bar.as_ref().and_then(|b| b.hit(pos.x as f32, pos.y as f32));
        let in_terminal = bar_hit.is_none() && icon == CursorIcon::Default;
        state.window.set_cursor(if over_link {
            CursorIcon::Pointer
        } else if in_terminal {
            CursorIcon::Text
        } else {
            icon
        });

        if bar_hit != state.mouse.hovered_bar {
            state.mouse.hovered_bar = bar_hit;
            state.window.request_redraw();
        }

        // Reorder tabs by dragging them along the bar.
        if let Some(mut drag) = state.mouse.tab_drag {
            if (pos.x - drag.press_x).abs() > TAB_DRAG_THRESHOLD {
                drag.moved = true;
            }
            if drag.moved
                && let Some(bar) = &bar
            {
                let target = bar.slot_at(pos.x as f32).unwrap_or(state.tabs.len() - 1);
                if target != drag.index {
                    state.tabs.move_tab(drag.index, target);
                    drag.index = target;
                    state.window.request_redraw();
                }
            }
            state.mouse.tab_drag = Some(drag);
            return;
        }

        if state.mouse.selecting {
            let point = state.cell_at(&self.config, pos);
            state.term().update_selection(point);
            state.window.request_redraw();
            return;
        }
        let over_focused = state.pane_at(&self.config, pos) == Some(state.content().focused);
        if !in_terminal || !over_focused {
            return;
        }

        let point = state.cell_at(&self.config, pos);
        let mode = state.term().mode();
        let cell = (point.column, point.line);
        if state.reports_mouse(mode) && state.mouse.last_reported_cell != Some(cell) {
            state.mouse.last_reported_cell = Some(cell);
            let held = state.mouse.reported_button;
            state.report(held, MouseAction::Motion, point);
        }
    }

    fn mouse_wheel(&mut self, delta: MouseScrollDelta) {
        let Some(state) = self.state.as_mut() else {
            return;
        };
        let cell_height = state.renderer.cell_metrics().height as f64;
        // Positive = scroll up (content moves down, towards older lines).
        let lines = match delta {
            MouseScrollDelta::LineDelta(_, y) => {
                state.mouse.scroll_pixels = 0.0;
                (y as f64 * WHEEL_LINES).round() as i32
            }
            MouseScrollDelta::PixelDelta(p) => {
                // A leftover from the other direction must not eat this scroll.
                if state.mouse.scroll_pixels * p.y < 0.0 {
                    state.mouse.scroll_pixels = 0.0;
                }
                state.mouse.scroll_pixels += p.y;
                let lines = (state.mouse.scroll_pixels / cell_height).trunc();
                state.mouse.scroll_pixels -= lines * cell_height;
                lines as i32
            }
        };
        let mode = state.term().mode();
        tracing::trace!(?delta, lines, ?mode, "mouse wheel");
        if lines == 0 {
            return;
        }

        // Scrolling over a background pane scrolls its history without
        // moving focus.
        let hovered = state
            .mouse
            .position
            .and_then(|pos| state.pane_at(&self.config, pos));
        if let Some(id) = hovered
            && id != state.content().focused
        {
            if let Some(pane) = state.content().pane(id) {
                pane.term.scroll(lines);
            }
            state.window.request_redraw();
            return;
        }

        if state.reports_mouse(mode) {
            let Some(pos) = state.mouse.position else {
                return;
            };
            let point = state.cell_at(&self.config, pos);
            let button = if lines > 0 {
                Button::WheelUp
            } else {
                Button::WheelDown
            };
            for _ in 0..lines.unsigned_abs() {
                state.report(Some(button), MouseAction::Press, point);
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
            state
                .term()
                .write(arrow.repeat(lines.unsigned_abs() as usize));
        } else {
            state.term().scroll(lines);
            state.window.request_redraw();
        }
    }

    fn term_event(&mut self, pane: PaneId, event: TermEvent) {
        let Some(state) = self.state.as_mut() else {
            return;
        };
        let Some(index) = state.tabs.position(|c| c.contains(pane)) else {
            return;
        };
        let active = index == state.tabs.active_index();
        match event {
            TermEvent::Wakeup => {
                if !active && let Some(tab) = state.tabs.get_mut(index) {
                    tab.activity = true;
                }
                state.window.request_redraw();
            }
            TermEvent::Title(_) | TermEvent::ResetTitle => {
                let title = match event {
                    TermEvent::Title(title) => Some(title),
                    _ => None,
                };
                if let Some(tab) = state.tabs.get_mut(index)
                    && let Some(p) = tab.content.pane_mut(pane)
                {
                    p.title = title;
                }
                state.window.request_redraw();
            }
            TermEvent::Bell => {
                if !active && let Some(tab) = state.tabs.get_mut(index) {
                    tab.bell = true;
                }
                if !active || !state.focused {
                    state.window.request_user_attention(None);
                }
                state.window.request_redraw();
            }
            TermEvent::Exit => self.close_pane(pane),
            TermEvent::ClipboardStore(text) => self.set_clipboard(text),
        }
    }
}

impl ApplicationHandler<UserEvent> for App {
    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if self.exit_requested {
            event_loop.exit();
            return;
        }
        let Some(state) = self.state.as_mut() else {
            return;
        };
        if !state.blink.active {
            event_loop.set_control_flow(ControlFlow::Wait);
            return;
        }
        if Instant::now() >= state.blink.next_toggle {
            state.blink.visible = !state.blink.visible;
            state.blink.next_toggle = Instant::now() + BLINK_INTERVAL;
            state.window.request_redraw();
        }
        event_loop.set_control_flow(ControlFlow::WaitUntil(state.blink.next_toggle));
    }

    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        // Only render on demand; no redraw loop while idle.
        event_loop.set_control_flow(ControlFlow::Wait);
        if self.state.is_some() {
            return;
        }
        match self.create_window(event_loop) {
            Ok(state) => {
                state.window.request_redraw();
                self.state = Some(state);
            }
            Err(err) => self.fail(event_loop, err),
        }
    }

    fn user_event(&mut self, _event_loop: &ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::Term(pane, event) => self.term_event(pane, event),
            UserEvent::ConfigChanged => self.reload_config(),
            UserEvent::SystemStats(sample) => {
                // Late samples from a monitor that was just stopped.
                if self.system_monitor.is_none() {
                    return;
                }
                self.stats.push(sample);
                if let Some(state) = self.state.as_ref() {
                    state.window.request_redraw();
                }
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let Some(state) = self.state.as_mut() else {
            return;
        };
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                state.renderer.resize(size.width, size.height);
                state.resize_terms(&self.config);
                state.window.request_redraw();
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                state.renderer.set_font_size(self.font_size, scale_factor);
                state.resize_terms(&self.config);
                state.window.request_redraw();
            }
            WindowEvent::Focused(focused) => {
                // Report while still marked focused, so focus-out gets through.
                if focused {
                    state.focused = true;
                    state.send_focus(true);
                } else {
                    state.send_focus(false);
                    state.focused = false;
                }
                state.window.request_redraw();
            }
            WindowEvent::ModifiersChanged(mods) => {
                state.modifiers = mods;
                self.update_hover_link();
            }
            WindowEvent::KeyboardInput { event, .. } => self.keyboard_input(event),
            WindowEvent::Ime(Ime::Preedit(text, _)) => {
                state.preedit = (!text.is_empty()).then_some(text);
                state.window.request_redraw();
            }
            WindowEvent::Ime(Ime::Commit(text)) => {
                state.preedit = None;
                match state.search.as_mut() {
                    Some(bar) => {
                        bar.query.push_str(&text);
                        bar.update(&state.tabs.active().content.focused_pane().term);
                        state.window.request_redraw();
                    }
                    None => state.term().write(text.into_bytes()),
                }
            }
            WindowEvent::CursorMoved { position, .. } => self.cursor_moved(position),
            WindowEvent::CursorLeft { .. } => {
                state.mouse.position = None;
                if state.mouse.hovered_bar.take().is_some() {
                    state.window.request_redraw();
                }
            }
            WindowEvent::MouseInput {
                state: button_state,
                button,
                ..
            } => self.mouse_input(button, button_state == ElementState::Pressed),
            WindowEvent::MouseWheel { delta, .. } => self.mouse_wheel(delta),
            // Frames are paused while occluded; catch up once visible.
            WindowEvent::Occluded(false) => state.window.request_redraw(),
            WindowEvent::ThemeChanged(theme) => {
                self.os_dark = theme == WindowTheme::Dark;
                if matches!(self.config.theme, ThemeSelection::Auto { .. }) {
                    let warning = self.update_palette();
                    self.notify(Banner::config(
                        Severity::Warning,
                        warning.into_iter().collect(),
                    ));
                }
            }
            WindowEvent::RedrawRequested => {
                match state.redraw(&self.config, &self.stats, self.banner.as_ref()) {
                    FrameStatus::Presented | FrameStatus::Paused => {}
                    FrameStatus::Skipped => state.window.request_redraw(),
                    FrameStatus::Lost => {
                        tracing::warn!("surface lost, recreating renderer");
                        let (window, chrome) = (state.window.clone(), state.chrome);
                        match self.create_renderer(&window, chrome) {
                            Ok(renderer) => {
                                if let Some(state) = self.state.as_mut() {
                                    state.renderer = renderer;
                                }
                                window.request_redraw();
                            }
                            Err(err) => self.fail(event_loop, err),
                        }
                    }
                }
            }
            _ => {}
        }
    }
}
