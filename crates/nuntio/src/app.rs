//! The application: the window's lifecycle, the config and its hot reload,
//! banners and update checks. Input handling lives in `app_input`, tabs and
//! panes in `app_panes`.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use nuntio_config::{
    Color, Config, ConfigWatcher, DEFAULT_THEME, StatusItem, Theme, ThemeSelection, ThemeSet,
};
use nuntio_render::{FrameStatus, GpuOptions, Renderer};
use nuntio_term::{Palette, Rgb, TermEvent, TermOptions, TermSize};
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalPosition};
use winit::event::{ElementState, Ime, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoopProxy};
use winit::window::{Theme as WindowTheme, Window, WindowAttributes, WindowId};

use crate::actions::Bindings;
use crate::banner::{Banner, Severity};
use crate::event::{MenuCommand, PaneId, UserEvent};
use crate::input::KeyPress;
use crate::status_bar::Stats;
use crate::sysmon::SystemMonitor;
use crate::update::{self, Build, Checker, Update};
use crate::window::{Chrome, DEFAULT_TITLE, WindowState};

// Child modules, so that their handlers can reach the app's state.
#[path = "app_input.rs"]
mod app_input;
#[path = "app_panes.rs"]
mod app_panes;
#[cfg(feature = "debug-server")]
#[path = "debug_server.rs"]
pub(crate) mod debug_server;

use app_input::PendingPaste;
use app_panes::{CloseTarget, PendingClose};

const MIN_FONT_SIZE: f32 = 4.0;
const MAX_FONT_SIZE: f32 = 72.0;

/// Shown when `window.gpu_acceleration` is off but the system has no software renderer.
const NO_SOFTWARE_RENDERER: &str = "no software renderer available, nuntio keeps using the GPU";

/// Room for macOS's traffic-light buttons, in logical pixels.
#[cfg(target_os = "macos")]
const TRAFFIC_LIGHTS_WIDTH: f64 = 78.0;
/// Immediate retries of a skipped frame before waiting for the next event.
const MAX_FRAME_RETRIES: u32 = 3;
/// How long an action that asked for confirmation waits to be repeated.
const CONFIRM_REPEAT: Duration = Duration::from_secs(5);
/// Size of a pane spawned before the window is laid out; the first
/// `resize_terms` corrects it.
const INITIAL_GRID: TermSize = TermSize {
    columns: 80,
    lines: 24,
    cell_width: 1,
    cell_height: 1,
};

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
            MacosTitlebar::Transparent => (
                attrs
                    .with_titlebar_transparent(true)
                    .with_fullsize_content_view(true)
                    .with_title_hidden(true),
                Chrome::TitlebarInset {
                    left: TRAFFIC_LIGHTS_WIDTH,
                },
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
            (attrs.with_decorations(false), Chrome::Undecorated)
        } else {
            (attrs, Chrome::System)
        }
    }
}

/// Whether to create the window transparent: for rounded corners cut out
/// by nuntio, or a translucent background. Transparent windows may cost
/// compositing performance, so only then.
fn wants_transparency(chrome: Chrome, config: &Config) -> bool {
    chrome.draws_corners() || config.window.opacity < 1.0
}

/// A warning if `software` was wanted but the renderer runs on the GPU.
fn software_warning(software: bool, renderer: &Renderer) -> Option<String> {
    (software && !renderer.software()).then(|| NO_SOFTWARE_RENDERER.into())
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

/// The AppKit view winit draws the window's content in.
#[cfg(target_os = "macos")]
fn ns_view(window: &Window) -> Option<objc2::rc::Retained<objc2_app_kit::NSView>> {
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};

    let RawWindowHandle::AppKit(handle) = window.window_handle().ok()?.as_raw() else {
        return None;
    };
    // SAFETY: `ns_view` is winit's live NSView; retaining it keeps it valid.
    unsafe { objc2::rc::Retained::retain(handle.ns_view.as_ptr().cast()) }
}

/// Center the window on its screen the way macOS does: in the area left
/// by the menu bar and the Dock, a bit above the middle.
#[cfg(target_os = "macos")]
fn center(window: &Window) {
    if let Some(ns_window) = ns_view(window).and_then(|view| view.window()) {
        ns_window.center();
    }
}

/// Tells macOS which Option keys act as Alt, so they don't start
/// composing dead keys like Option+U (¨) while nuntio sends Meta.
#[cfg(target_os = "macos")]
fn option_as_alt(
    option_as_meta: nuntio_config::OptionAsMeta,
) -> winit::platform::macos::OptionAsAlt {
    use nuntio_config::OptionAsMeta;
    use winit::platform::macos::OptionAsAlt;

    match option_as_meta {
        OptionAsMeta::None => OptionAsAlt::None,
        OptionAsMeta::Left => OptionAsAlt::OnlyLeft,
        OptionAsMeta::Right => OptionAsAlt::OnlyRight,
        OptionAsMeta::Both => OptionAsAlt::Both,
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

/// The terminal options of a pane, from the config.
fn term_options(config: &Config) -> TermOptions {
    TermOptions {
        scrollback: config.scrollback,
        clipboard_write: config.clipboard_write,
        kitty_keyboard: config.kitty_keyboard,
    }
}

fn themes_dir(config_path: Option<&Path>) -> Option<PathBuf> {
    nuntio_config::themes_dir(config_path?)
}

/// How the first pane starts, from the command line.
#[derive(Debug, Default)]
pub struct Startup {
    /// Program and arguments to run instead of the shell; never empty.
    pub command: Option<Vec<String>>,
    pub working_directory: Option<PathBuf>,
}

pub struct App {
    core: Core,
    /// The window, once the event loop has resumed.
    state: Option<WindowState>,
}

/// Everything the app keeps besides the window's state, so that handlers
/// can borrow both at once.
struct Core {
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
    /// The window, to redraw it when a banner changes.
    window: Option<Arc<Window>>,
    proxy: EventLoopProxy<UserEvent>,
    bindings: Bindings,
    /// Samples for the status bar while it is shown.
    system_monitor: Option<SystemMonitor>,
    /// The window is hidden (minimized or covered); sampling pauses.
    occluded: bool,
    stats: Stats,
    /// The daily update check, while an update indicator is on.
    update_checker: Option<Checker>,
    /// A newer release, once a check found one.
    update: Option<Update>,
    clipboard: Option<arboard::Clipboard>,
    /// Current font size in points; changed by zoom shortcuts.
    font_size: f32,
    next_pane_id: u64,
    /// Home directories in WSL, to start new panes in `~/…`.
    wsl_homes: crate::wsl::Homes,
    /// Used up by the first pane.
    startup: Startup,
    /// The last tab was closed; quit at the next opportunity.
    exit_requested: bool,
    /// A close waiting for confirmation (`window.confirm_close`).
    pending_close: Option<PendingClose>,
    /// A paste waiting for confirmation (`confirm_paste`).
    pending_paste: Option<PendingPaste>,
    /// A fatal error that ended the event loop.
    error: Option<anyhow::Error>,
    #[cfg(target_os = "macos")]
    menu_bar: Option<crate::macos_menu::MenuBar>,
    /// Input comes from the debug server, not the user: nothing may grab
    /// the real pointer (window drags).
    synthetic_input: bool,
    #[cfg(feature = "debug-server")]
    debug: debug_server::DebugState,
}

impl App {
    pub fn new(
        config: Config,
        config_path: Option<PathBuf>,
        banner: Option<Banner>,
        startup: Startup,
        proxy: EventLoopProxy<UserEvent>,
    ) -> Self {
        Self {
            core: Core::new(config, config_path, banner, startup, proxy),
            state: None,
        }
    }

    /// The debug server runs: open the window in the background and keep
    /// it acting as focused; `headless` keeps it off-screen.
    #[cfg(feature = "debug-server")]
    pub fn enable_debug_server(&mut self, headless: bool) {
        self.core.debug.enabled = true;
        self.core.debug.headless = headless;
    }

    pub fn into_result(self) -> Result<()> {
        self.core.error.map_or(Ok(()), Err)
    }
}

impl Core {
    fn new(
        config: Config,
        config_path: Option<PathBuf>,
        banner: Option<Banner>,
        startup: Startup,
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

        let mut core = Self {
            font_size: config.font.size,
            config,
            config_path,
            _watcher: watcher,
            themes,
            palette: Palette::default(),
            os_dark: true,
            banner,
            window: None,
            proxy,
            bindings,
            system_monitor: None,
            occluded: false,
            stats: Stats::default(),
            update_checker: None,
            update: None,
            clipboard,
            next_pane_id: 0,
            wsl_homes: Default::default(),
            startup,
            exit_requested: false,
            pending_close: None,
            pending_paste: None,
            error: None,
            #[cfg(target_os = "macos")]
            menu_bar: None,
            synthetic_input: false,
            #[cfg(feature = "debug-server")]
            debug: Default::default(),
        };
        let theme_warning = core.update_palette(None);
        core.sync_system_monitor();
        core.sync_update_checker(None);
        core.notify(Banner::config(
            Severity::Warning,
            theme_warnings
                .into_iter()
                .chain(binding_warnings)
                .chain(theme_warning)
                .collect(),
        ));
        core
    }

    fn request_redraw(&self) {
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }

    // ----- Banners ---------------------------------------------------------

    /// Show a banner (and log its messages). Errors take precedence.
    fn notify(&mut self, banner: Option<Banner>) {
        let Some(new) = banner else {
            return;
        };
        for message in &new.messages {
            match new.severity {
                Severity::Error => tracing::error!("{message}"),
                Severity::Warning => tracing::warn!("{message}"),
                Severity::Info => tracing::info!("{message}"),
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
        self.request_redraw();
    }

    /// Drop the banner titled `title`, if it is shown.
    fn dismiss_banner(&mut self, title: &str) {
        if self.banner.as_ref().is_some_and(|b| b.title == title) {
            self.banner = None;
            self.request_redraw();
        }
    }

    /// Drop a banner about the previous config; others (e.g. a failed
    /// link) stay until dismissed.
    fn clear_config_banner(&mut self) {
        if self.banner.as_ref().is_some_and(Banner::is_config) {
            self.banner = None;
        }
    }

    /// Show a banner asking to repeat an action to confirm it. It replaces
    /// any other banner, even an error: the action must not look like it
    /// failed.
    fn ask_to_repeat(&mut self, title: &'static str, message: String) {
        tracing::info!("{message}");
        self.banner = Banner::new(Severity::Warning, title, vec![message]);
        self.request_redraw();
    }

    /// Open `url` in the browser, or say why not.
    fn open_url(&mut self, url: &str) {
        let problem = match crate::link::check(url) {
            Err(reason) => format!("not opening {url}: {reason}"),
            Ok(()) => match open::that_detached(url) {
                Err(err) => format!("failed to open {url}: {err}"),
                Ok(()) => return,
            },
        };
        self.notify(Banner::new(Severity::Warning, "Link", vec![problem]));
    }

    /// Open the page of the newer release.
    fn open_update(&mut self) {
        if let Some(url) = self.update.as_ref().map(|u| u.url.clone()) {
            self.open_url(&url);
        }
    }

    // ----- Theme -------------------------------------------------------------

    /// Pick the theme for the current config and OS appearance and apply
    /// it to all panes. Returns a warning if the theme doesn't exist.
    fn update_palette(&mut self, state: Option<&mut WindowState>) -> Option<String> {
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
        if let Some(state) = state {
            for pane in state.panes() {
                pane.term.set_palette(self.palette.clone());
            }
            state.window.request_redraw();
        }
        warning
    }

    /// Follow the OS appearance for `theme = { light, dark }`.
    fn set_os_dark(&mut self, state: Option<&mut WindowState>, dark: bool) {
        if dark == self.os_dark {
            return;
        }
        self.os_dark = dark;
        if matches!(self.config.theme, ThemeSelection::Auto { .. }) {
            let warning = self.update_palette(state);
            self.notify(Banner::config(
                Severity::Warning,
                warning.into_iter().collect(),
            ));
        }
    }

    // ----- Status bar and updates -------------------------------------------

    /// Start, restart or stop sampling to match the status bar config.
    fn sync_system_monitor(&mut self) {
        let bar = &self.config.status_bar;
        // Without springs, so that moving one doesn't restart sampling.
        let wanted: Option<Vec<StatusItem>> = (bar.visible() && !self.occluded).then(|| {
            let items = bar.items.iter().copied();
            items.filter(|&item| item != StatusItem::Spring).collect()
        });
        let running = self.system_monitor.as_ref().map(SystemMonitor::items);
        if wanted.as_deref() == running {
            return;
        }
        // Dropping the old monitor stops its thread.
        self.system_monitor = None;
        self.stats.clear();
        if let Some(items) = wanted {
            tracing::debug!("starting system monitor");
            self.system_monitor = Some(SystemMonitor::start(&items, self.proxy.clone()));
        }
    }

    /// The tab bar shows a badge for a newer release.
    fn update_badge(&self) -> bool {
        self.update.is_some() && self.config.updates.tab_bar
    }

    /// Start or stop the daily update check to match the update
    /// indicators, and show or hide the tab bar badge.
    fn sync_update_checker(&mut self, mut state: Option<&mut WindowState>) {
        match (self.config.update_check(), self.update_checker.is_some()) {
            (true, false) => {
                tracing::debug!("starting update check");
                self.update_checker = Some(Checker::start(self.proxy.clone()));
            }
            (false, true) => {
                // Dropping it stops the thread; forget what it found.
                self.update_checker = None;
                self.set_update(state.as_deref_mut(), None);
                self.dismiss_banner(update::BANNER);
            }
            _ => {}
        }
        if let Some(state) = state
            && state.update_badge != self.update_badge()
        {
            state.update_badge = self.update_badge();
            state.window.request_redraw();
        }
    }

    /// Show what an update check found.
    fn update_checked(&mut self, state: Option<&mut WindowState>, checked: update::Checked) {
        // Late answers of a check that was just turned off.
        if !checked.manual && self.update_checker.is_none() {
            return;
        }
        // Without the banner indicator, only a manual check speaks up.
        let show_banner = checked.manual || self.config.updates.banner;
        let release = match checked.result {
            Ok(release) => release,
            Err(err) if show_banner => {
                self.notify(Banner::new(Severity::Warning, "Update check", vec![err]));
                return;
            }
            Err(err) => {
                tracing::warn!("update check: {err}");
                return;
            }
        };
        let update = Update::new(&release, Build::current());
        // A repeated answer replaces the banner instead of adding to it.
        self.dismiss_banner(update::BANNER);
        let message = match &update {
            Some(update) if checked.manual || (show_banner && !checked.dismissed) => {
                Some((update.message(), Some(update.url.clone())))
            }
            None if checked.manual => Some((update::up_to_date(), None)),
            _ => None,
        };
        if let Some((message, url)) = message {
            let banner = Banner::new(Severity::Info, update::BANNER, vec![message]);
            self.notify(banner.map(|b| match url {
                Some(url) => b.with_url(url),
                None => b,
            }));
        }
        self.set_update(state, update);
    }

    fn set_update(&mut self, state: Option<&mut WindowState>, update: Option<Update>) {
        self.stats.set_update(
            update
                .as_ref()
                .filter(|_| self.config.updates.status_bar)
                .map(|u| u.version.clone()),
        );
        self.update = update;
        if let Some(state) = state {
            state.update_badge = self.update_badge();
            state.status_bar_changed(&self.config, &self.stats);
            state.window.request_redraw();
        }
    }

    // ----- Config ------------------------------------------------------------

    /// Re-read the config file and apply what changed. An invalid file
    /// leaves the current settings untouched.
    fn reload_config(&mut self, mut state: Option<&mut WindowState>) {
        let Some(path) = self.config_path.clone() else {
            return;
        };
        let loaded = match nuntio_config::load(&path) {
            Ok(loaded) => loaded,
            Err(err) => {
                self.clear_config_banner();
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
        warnings.extend(self.update_palette(state.as_deref_mut()));
        self.sync_system_monitor();
        self.sync_update_checker(state.as_deref_mut());
        if old.updates.banner && !self.config.updates.banner {
            self.dismiss_banner(update::BANNER);
        }
        if old.updates.status_bar != self.config.updates.status_bar {
            self.set_update(state.as_deref_mut(), self.update.clone());
        }
        warnings.extend(self.restart_warnings(&old, state.as_deref()));
        if let Some(state) = state {
            self.apply_config_to_window(&old, state, &mut warnings);
        }
        self.clear_config_banner();
        self.notify(Banner::config(Severity::Warning, warnings));
    }

    /// Changes that only take effect when nuntio is restarted.
    fn restart_warnings(&self, old: &Config, state: Option<&WindowState>) -> Vec<String> {
        let mut warnings = Vec::new();
        let chrome_changed = if cfg!(target_os = "macos") {
            old.window.effective_macos_titlebar() != self.config.window.effective_macos_titlebar()
        } else {
            old.window.decorations != self.config.window.decorations
        };
        if chrome_changed {
            warnings.push("window decorations change when nuntio is restarted".into());
        }
        // A window created opaque can't become transparent.
        let opaque = state.is_some_and(|s| !s.transparent);
        if opaque && self.config.window.opacity < 1.0 && old.window.opacity >= 1.0 {
            warnings.push("window opacity takes effect when nuntio is restarted".into());
        }
        warnings
    }

    /// Apply a reloaded config to the window and its panes; `old` is the
    /// config before.
    fn apply_config_to_window(
        &mut self,
        old: &Config,
        state: &mut WindowState,
        warnings: &mut Vec<String>,
    ) {
        #[cfg(target_os = "macos")]
        if old.macos.option_as_meta != self.config.macos.option_as_meta {
            use winit::platform::macos::WindowExtMacOS;
            let option = option_as_alt(self.config.macos.option_as_meta);
            state.window.set_option_as_alt(option);
        }
        let rebuilt = old.window.gpu_acceleration != self.config.window.gpu_acceleration;
        if rebuilt {
            let software = !self.config.window.gpu_acceleration;
            match self.rebuild_renderer(state, software, false) {
                Ok(renderer_warnings) => warnings.extend(renderer_warnings),
                Err(err) => warnings.push(format!("failed to switch the renderer: {err:#}")),
            }
        }
        // A rebuilt renderer has the new family already.
        if !rebuilt && old.font.family != self.config.font.family {
            warnings.extend(
                state
                    .renderer
                    .set_font_family(self.config.font.family.clone()),
            );
        }
        let options = term_options(&self.config);
        if term_options(old) != options {
            for pane in state.panes() {
                pane.term.set_options(options);
            }
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

    fn set_font_size(&mut self, state: &mut WindowState, size: f32) {
        self.font_size = size.clamp(MIN_FONT_SIZE, MAX_FONT_SIZE);
        let scale = state.window.scale_factor();
        state.renderer.set_font_size(self.font_size, scale);
        state.resize_terms(&self.config);
        state.window.request_redraw();
    }

    // ----- Clipboard ---------------------------------------------------------

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

    /// Put text into the primary selection, for middle-click paste. Only
    /// Linux has one; Wayland needs the data-control protocol for it.
    fn set_primary(&mut self, text: String) {
        #[cfg(target_os = "linux")]
        if let Some(clipboard) = self.clipboard.as_mut() {
            use arboard::{LinuxClipboardKind, SetExtLinux};

            if let Err(err) = clipboard
                .set()
                .clipboard(LinuxClipboardKind::Primary)
                .text(text)
            {
                tracing::debug!("failed to write primary selection: {err}");
            }
        }
        #[cfg(not(target_os = "linux"))]
        let _ = text;
    }

    fn primary_text(&mut self) -> Option<String> {
        #[cfg(target_os = "linux")]
        {
            use arboard::{GetExtLinux, LinuxClipboardKind};

            let clipboard = self.clipboard.as_mut()?;
            clipboard
                .get()
                .clipboard(LinuxClipboardKind::Primary)
                .text()
                .inspect_err(|err| tracing::debug!("failed to read primary selection: {err}"))
                .ok()
        }
        #[cfg(not(target_os = "linux"))]
        None
    }

    // ----- The window ------------------------------------------------------

    /// The window is kept off-screen for the debug server.
    fn headless(&self) -> bool {
        #[cfg(feature = "debug-server")]
        return self.debug.headless;
        #[cfg(not(feature = "debug-server"))]
        false
    }

    fn fail(&mut self, event_loop: &ActiveEventLoop, err: anyhow::Error) {
        tracing::error!("{err:#}");
        self.error = Some(err);
        event_loop.exit();
    }

    /// `transparent`: the window was created transparent (see
    /// `wants_transparency`).
    /// `software`: prefer a software renderer over the GPU.
    fn create_renderer(
        &self,
        window: &Arc<Window>,
        transparent: bool,
        software: bool,
    ) -> Result<Renderer> {
        let size = window.inner_size();
        Ok(Renderer::new(
            window.clone(),
            size.width,
            size.height,
            window.scale_factor(),
            self.config.font.family.clone(),
            self.font_size,
            GpuOptions {
                transparent,
                software,
            },
        )?)
    }

    /// Swap the window's renderer for a new one. The old one gives up the
    /// window's surface first, as a window takes only one swapchain. If the
    /// renderer for `software` can't be created, the other kind is tried, so
    /// the window doesn't end up without one. Returns the warnings about the
    /// new renderer, and an error only if neither could be created. `quiet`:
    /// leave out those that only repeat the setup (the font, no software
    /// renderer), for a renderer that replaces a lost one.
    fn rebuild_renderer(
        &self,
        state: &mut WindowState,
        software: bool,
        quiet: bool,
    ) -> Result<Vec<String>> {
        let kind = |software| if software { "software" } else { "GPU" };
        state.renderer.release_surface();
        let mut warnings = Vec::new();
        let mut renderer = match self.create_renderer(&state.window, state.transparent, software) {
            Ok(renderer) => {
                if !quiet {
                    warnings.extend(software_warning(software, &renderer));
                }
                renderer
            }
            Err(err) => {
                warnings.push(format!(
                    "failed to create the {} renderer, using the {} one instead: {err:#}",
                    kind(software),
                    kind(!software),
                ));
                self.create_renderer(&state.window, state.transparent, !software)?
            }
        };
        if !quiet {
            warnings.extend(renderer.take_font_warning());
        }
        state.renderer = renderer;
        state.window.request_redraw();
        Ok(warnings)
    }

    fn window_attributes(&self) -> WindowAttributes {
        let attrs = Window::default_attributes()
            .with_title(DEFAULT_TITLE)
            .with_inner_size(LogicalSize::new(900.0, 600.0))
            // Shown once it has the size of the configured grid, which is
            // only known with the font (where the platform allows hiding).
            .with_visible(false);
        // Driven remotely: don't take the focus from the user's window.
        #[cfg(feature = "debug-server")]
        let attrs = attrs.with_active(!self.debug.enabled);
        // Headless: shown, since hidden windows get no redraws on Windows,
        // but where nobody sees it.
        let attrs = if self.headless() {
            let attrs = attrs.with_position(PhysicalPosition::new(-32000, -32000));
            #[cfg(windows)]
            let attrs = winit::platform::windows::WindowAttributesExtWindows::with_skip_taskbar(
                attrs, true,
            );
            attrs
        } else {
            attrs
        };
        // App id / WM_CLASS, matching the .desktop file (Wayland and X11).
        #[cfg(target_os = "linux")]
        let attrs = winit::platform::wayland::WindowAttributesExtWayland::with_name(
            attrs, "nuntio", "nuntio",
        );
        #[cfg(target_os = "macos")]
        let attrs = winit::platform::macos::WindowAttributesExtMacOS::with_option_as_alt(
            attrs,
            option_as_alt(self.config.macos.option_as_meta),
        );
        attrs
    }

    fn create_window(&mut self, event_loop: &ActiveEventLoop) -> Result<WindowState> {
        let (attrs, chrome) = chrome(event_loop, &self.config, self.window_attributes());
        let transparent = wants_transparency(chrome, &self.config);
        let window = Arc::new(
            event_loop
                .create_window(attrs.with_transparent(transparent))
                .context("failed to create window")?,
        );
        window.set_ime_allowed(true);
        #[cfg(windows)]
        if chrome == Chrome::Undecorated {
            round_corners(&window);
        }
        let software = !self.config.window.gpu_acceleration;
        let mut renderer = self.create_renderer(&window, transparent, software)?;
        let warnings: Vec<_> = renderer
            .take_font_warning()
            .into_iter()
            .chain(software_warning(software, &renderer))
            .collect();
        self.notify(Banner::config(Severity::Warning, warnings));
        self.set_os_dark(None, window.theme() != Some(WindowTheme::Light));
        let pane = self.spawn_first_pane()?;
        self.window = Some(window.clone());
        let mut state = WindowState::new(window, renderer, pane, chrome, transparent);
        state.update_badge = self.update_badge();
        // The cell size is only known now that the renderer has the font.
        // Where the size applies at once, there may be no `Resized` event.
        let size = state.size_for_grid(&self.config);
        if let Some(size) = state.window.request_inner_size(size) {
            state.renderer.resize(size.width, size.height);
        }
        // macOS centered the window at its first size.
        #[cfg(target_os = "macos")]
        if !self.headless() {
            center(&state.window);
        }
        state.window.set_visible(true);
        state.resize_terms(&self.config);
        #[cfg(feature = "debug-server")]
        if self.debug.enabled {
            debug_server::focus(&mut state);
        }
        Ok(state)
    }

    /// Draw the window, and capture the frame while the debug server
    /// records.
    fn redraw(&mut self, state: &mut WindowState) -> FrameStatus {
        #[cfg(feature = "debug-server")]
        if let Some(status) = self.debug_redraw(state) {
            return status;
        }
        state.redraw(&self.config, &self.stats, self.banner.as_ref())
    }

    fn redraw_requested(&mut self, state: &mut WindowState, event_loop: &ActiveEventLoop) {
        // Keep the link underline on the text under the pointer.
        state.refresh_hover_link(&self.config);
        // The previous frame stays up while a new pane starts.
        if state.reveal_holds(Instant::now()) {
            return;
        }
        let status = self.redraw(state);
        if status == FrameStatus::Skipped {
            state.skipped_frames += 1;
        } else {
            state.skipped_frames = 0;
        }
        match status {
            FrameStatus::Presented | FrameStatus::Paused => {}
            // Retry a few times; a surface that keeps timing out must not
            // spin. The next event redraws anyway.
            FrameStatus::Skipped if state.skipped_frames <= MAX_FRAME_RETRIES => {
                state.window.request_redraw();
            }
            FrameStatus::Skipped => {
                tracing::debug!("giving up on this frame after repeated skips");
            }
            FrameStatus::Lost => {
                tracing::warn!("surface lost, recreating renderer");
                let software = !self.config.window.gpu_acceleration;
                match self.rebuild_renderer(state, software, true) {
                    Ok(warnings) => self.notify(Banner::config(Severity::Warning, warnings)),
                    Err(err) => self.fail(event_loop, err),
                }
            }
        }
    }

    fn term_event(&mut self, state: &mut WindowState, pane: PaneId, event: TermEvent) {
        #[cfg(feature = "debug-server")]
        if matches!(event, TermEvent::Wakeup) {
            self.debug_output();
        }
        let Some(index) = state.tabs.position(|c| c.contains(pane)) else {
            return;
        };
        let active = index == state.tabs.active_index();
        match event {
            TermEvent::Wakeup => {
                if active {
                    // A held new pane is drawn once its output pauses.
                    if !state.reveal_output(pane, Instant::now()) {
                        state.window.request_redraw();
                    }
                } else if let Some(tab) = state.tabs.get_mut(index)
                    && !tab.activity
                {
                    // Hidden output only changes the tab's activity mark.
                    tab.activity = true;
                    state.window.request_redraw();
                }
            }
            TermEvent::Title(title) => set_pane_title(state, index, pane, Some(title)),
            TermEvent::ResetTitle => set_pane_title(state, index, pane, None),
            TermEvent::Bell => {
                if !active && let Some(tab) = state.tabs.get_mut(index) {
                    tab.bell = true;
                }
                if !active || !state.focused {
                    state.window.request_user_attention(None);
                }
                state.window.request_redraw();
            }
            TermEvent::Exit => self.close_pane(state, pane),
            TermEvent::ClipboardStore(text) => self.set_clipboard(text),
        }
    }

    fn window_event(
        &mut self,
        state: &mut WindowState,
        event_loop: &ActiveEventLoop,
        event: WindowEvent,
    ) {
        match event {
            WindowEvent::CloseRequested => self.request_close(state, CloseTarget::Window),
            // Minimized: keep the grids, don't reflow to a tiny size. Windows
            // reports 0x0 or the size of the minimized caption (160x28).
            WindowEvent::Resized(size)
                if size.width == 0
                    || size.height == 0
                    || state.window.is_minimized() == Some(true) => {}
            WindowEvent::Resized(size) => {
                // A resized window needs a fresh frame right away.
                state.reveal = None;
                state.renderer.resize(size.width, size.height);
                state.resize_terms(&self.config);
                state.window.request_redraw();
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                state.reveal = None;
                state.renderer.set_font_size(self.font_size, scale_factor);
                state.invalidate_ime_area();
                state.resize_terms(&self.config);
                state.window.request_redraw();
            }
            // The debug server's window stays focused (see `enable_debug_server`).
            #[cfg(feature = "debug-server")]
            WindowEvent::Focused(_) if self.debug.enabled => {}
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
                state.update_hover_link(&self.config);
            }
            WindowEvent::KeyboardInput { event, .. } => {
                self.keyboard_input(state, &KeyPress::from(&event));
            }
            WindowEvent::Ime(Ime::Preedit(text, _)) => {
                state.preedit = (!text.is_empty()).then_some(text);
                state.window.request_redraw();
            }
            WindowEvent::Ime(Ime::Commit(text)) => {
                state.preedit = None;
                state.type_text(&text);
            }
            WindowEvent::Ime(Ime::Disabled) => {
                if state.preedit.take().is_some() {
                    state.window.request_redraw();
                }
            }
            WindowEvent::CursorMoved { position, .. } => self.cursor_moved(state, position),
            WindowEvent::CursorLeft { .. } => state.cursor_left(),
            WindowEvent::MouseInput {
                state: button_state,
                button,
                ..
            } => self.mouse_input(state, button, button_state == ElementState::Pressed),
            WindowEvent::MouseWheel { delta, .. } => state.mouse_wheel(&self.config, delta),
            WindowEvent::DroppedFile(path) => self.drop_file(state, &path),
            WindowEvent::Occluded(occluded) => {
                // Frames are paused while occluded; catch up once visible.
                if !occluded {
                    state.window.request_redraw();
                }
                self.occluded = occluded;
                self.sync_system_monitor();
            }
            WindowEvent::ThemeChanged(theme) => {
                self.set_os_dark(Some(state), theme == WindowTheme::Dark);
            }
            WindowEvent::RedrawRequested => self.redraw_requested(state, event_loop),
            _ => {}
        }
    }
}

/// Set the application's title of `pane` in the tab at `index`; `None`
/// resets it.
fn set_pane_title(state: &mut WindowState, index: usize, pane: PaneId, title: Option<String>) {
    if let Some(tab) = state.tabs.get_mut(index)
        && let Some(pane) = tab.content.pane_mut(pane)
    {
        pane.set_title(title);
    }
    state.window.request_redraw();
}

impl ApplicationHandler<UserEvent> for App {
    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if self.core.exit_requested {
            event_loop.exit();
            return;
        }
        let Some(state) = self.state.as_mut() else {
            return;
        };
        // Sleep until the next timer, or until an event if there is none.
        let deadline = state.run_timers(&self.core.config, Instant::now());
        event_loop.set_control_flow(deadline.map_or(ControlFlow::Wait, ControlFlow::WaitUntil));
    }

    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        // Only render on demand; no redraw loop while idle.
        event_loop.set_control_flow(ControlFlow::Wait);
        if self.state.is_some() {
            return;
        }
        #[cfg(target_os = "macos")]
        if self.core.menu_bar.is_none() {
            self.core.menu_bar = crate::macos_menu::install(self.core.proxy.clone());
        }
        match self.core.create_window(event_loop) {
            Ok(state) => {
                state.window.request_redraw();
                self.state = Some(state);
            }
            Err(err) => self.core.fail(event_loop, err),
        }
    }

    fn user_event(&mut self, _event_loop: &ActiveEventLoop, event: UserEvent) {
        let core = &mut self.core;
        let state = self.state.as_mut();
        match event {
            UserEvent::ConfigChanged => core.reload_config(state),
            UserEvent::Update(checked) => core.update_checked(state, checked),
            UserEvent::SystemStats(sample) => {
                // Late samples from a monitor that was just stopped.
                if core.system_monitor.is_none() {
                    return;
                }
                core.stats.push(sample);
                if let Some(state) = state
                    && state.status_bar_changed(&core.config, &core.stats)
                {
                    state.window.request_redraw();
                }
            }
            UserEvent::Menu(MenuCommand::Quit) => match state {
                Some(state) => core.request_close(state, CloseTarget::Window),
                None => core.exit_requested = true,
            },
            #[cfg(feature = "debug-server")]
            UserEvent::Debug(call) => core.debug_call(state, call),
            // The rest needs the window.
            UserEvent::Term(pane, event) => {
                if let Some(state) = state {
                    core.term_event(state, pane, event);
                }
            }
            UserEvent::Menu(MenuCommand::Action(action)) => {
                if let Some(state) = state {
                    core.run_action(state, action);
                }
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        if let Some(state) = self.state.as_mut() {
            self.core.window_event(state, event_loop, event);
        }
    }
}
