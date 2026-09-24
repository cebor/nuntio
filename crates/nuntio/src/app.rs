use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use nuntio_config::{Config, OptionAsMeta};
use nuntio_render::{CellMetrics, FrameStatus, Renderer, Viewport};
use nuntio_term::{
    CursorStyle, GridPoint, SelectionKind, Shell, SpawnOptions, TermEvent, TermHandle, TermMode,
    TermSize,
};
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalPosition, PhysicalSize};
use winit::event::{
    ElementState, Ime, KeyEvent, Modifiers, MouseButton, MouseScrollDelta, WindowEvent,
};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoopProxy};
use winit::keyboard::ModifiersKeyState;
use winit::platform::modifier_supplement::KeyEventExtModifierSupplement;
use winit::window::{Window, WindowId};

use crate::actions::{Action, Bindings};
use crate::event::{PaneId, UserEvent};
use crate::mouse::{self, Button, ClickCounter, MouseAction, MouseMods};
use crate::{ime, input};

const DEFAULT_TITLE: &str = "nuntio";
const MIN_FONT_SIZE: f32 = 4.0;
const MAX_FONT_SIZE: f32 = 72.0;
/// Lines scrolled per wheel notch.
const WHEEL_LINES: f64 = 3.0;
const BLINK_INTERVAL: Duration = Duration::from_millis(530);

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

#[cfg(not(target_os = "linux"))]
fn is_wslg_wayland(_event_loop: &ActiveEventLoop) -> bool {
    false
}

/// Pointer state for selection and mouse reporting.
#[derive(Default)]
struct MouseState {
    position: Option<PhysicalPosition<f64>>,
    /// Button held down that is reported to the application.
    reported_button: Option<Button>,
    /// Last cell a motion event was reported for, to avoid duplicates.
    last_reported_cell: Option<(usize, usize)>,
    /// A local selection drag is in progress.
    selecting: bool,
    clicks: ClickCounter,
    /// Sub-line remainder of pixel-precise (trackpad) scrolling.
    scroll_pixels: f64,
}

struct WindowState {
    window: Arc<Window>,
    renderer: Renderer,
    term: TermHandle,
    grid: TermSize,
    modifiers: Modifiers,
    mouse: MouseState,
    focused: bool,
    /// Uncommitted IME text, shown at the cursor.
    preedit: Option<String>,
    blink: Blink,
    /// Cell the IME candidate window was last anchored to.
    ime_cell: Option<(usize, usize)>,
}

/// Cursor blinking, only while the application asks for it (DECSCUSR).
struct Blink {
    active: bool,
    visible: bool,
    next_toggle: Instant,
}

impl Blink {
    /// Show the cursor and restart the interval, e.g. after typing.
    fn reset(&mut self) {
        self.visible = true;
        self.next_toggle = Instant::now() + BLINK_INTERVAL;
    }
}

impl WindowState {
    /// Padding around the grid in physical pixels.
    fn padding(&self, config: &Config) -> (u32, u32) {
        padding(config, self.window.scale_factor())
    }

    fn resize_term(&mut self, config: &Config) {
        self.grid = grid_size(
            self.window.inner_size(),
            self.padding(config),
            self.renderer.cell_metrics(),
        );
        self.term.resize(self.grid);
    }

    fn redraw(&mut self, config: &Config) -> FrameStatus {
        let mut snapshot = self.term.snapshot();
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

        let (x, y) = self.padding(config);
        self.renderer.render(&snapshot, Viewport { x, y })
    }

    /// Keep the IME candidate window next to the cursor.
    fn update_ime_area(&mut self, config: &Config, column: usize, line: usize) {
        if self.ime_cell == Some((column, line)) {
            return;
        }
        self.ime_cell = Some((column, line));
        let (pad_x, pad_y) = self.padding(config);
        let cell = self.renderer.cell_metrics();
        self.window.set_ime_cursor_area(
            PhysicalPosition::new(
                pad_x + column as u32 * cell.width,
                pad_y + line as u32 * cell.height,
            ),
            PhysicalSize::new(cell.width, cell.height),
        );
    }

    /// Grid cell under a window position, clamped to the grid.
    fn cell_at(&self, config: &Config, pos: PhysicalPosition<f64>) -> GridPoint {
        let (pad_x, pad_y) = self.padding(config);
        let cell = self.renderer.cell_metrics();
        let x = (pos.x - pad_x as f64).max(0.0);
        let y = (pos.y - pad_y as f64).max(0.0);
        let column = ((x / cell.width as f64) as usize).min(self.grid.columns as usize - 1);
        let line = ((y / cell.height as f64) as usize).min(self.grid.lines as usize - 1);
        let within = x - column as f64 * cell.width as f64;
        GridPoint {
            column,
            line,
            right_half: within >= cell.width as f64 / 2.0,
        }
    }

    fn mouse_mods(&self) -> MouseMods {
        MouseMods {
            shift: self.modifiers.state().shift_key(),
            alt: self.modifiers.state().alt_key(),
            ctrl: self.modifiers.state().control_key(),
        }
    }

    /// Mouse events go to the application unless Shift is held, which
    /// forces local selection like in xterm.
    fn reports_mouse(&self, mode: TermMode) -> bool {
        mouse::reporting_enabled(mode) && !self.modifiers.state().shift_key()
    }

    fn report(&mut self, button: Option<Button>, action: MouseAction, point: GridPoint) {
        let mode = self.term.mode();
        let mods = self.mouse_mods();
        if let Some(bytes) =
            mouse::encode_report(button, action, mods, point.column, point.line, mode)
        {
            self.term.write(bytes);
        }
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

fn padding(config: &Config, scale: f64) -> (u32, u32) {
    let p = config.window.padding;
    (
        (p.x as f64 * scale).round() as u32,
        (p.y as f64 * scale).round() as u32,
    )
}

/// How many cells fit into the window, minus padding.
fn grid_size(window: PhysicalSize<u32>, padding: (u32, u32), cell: CellMetrics) -> TermSize {
    let fit = |available: u32, pad: u32, cell: u32| {
        (available.saturating_sub(2 * pad) / cell).clamp(1, u16::MAX as u32) as u16
    };
    TermSize {
        columns: fit(window.width, padding.0, cell.width),
        lines: fit(window.height, padding.1, cell.height),
        cell_width: cell.width.min(u16::MAX as u32) as u16,
        cell_height: cell.height.min(u16::MAX as u32) as u16,
    }
}

pub struct App {
    config: Config,
    proxy: EventLoopProxy<UserEvent>,
    bindings: Bindings,
    clipboard: Option<arboard::Clipboard>,
    /// Current font size in points; changed by zoom shortcuts.
    font_size: f32,
    state: Option<WindowState>,
    /// A fatal error that ended the event loop.
    error: Option<anyhow::Error>,
}

impl App {
    pub fn new(config: Config, proxy: EventLoopProxy<UserEvent>) -> Self {
        let clipboard = arboard::Clipboard::new()
            .inspect_err(|err| tracing::warn!("clipboard unavailable: {err}"))
            .ok();
        Self {
            font_size: config.font.size,
            config,
            proxy,
            bindings: Bindings::platform_defaults(),
            clipboard,
            state: None,
            error: None,
        }
    }

    pub fn into_result(self) -> Result<()> {
        self.error.map_or(Ok(()), Err)
    }

    fn create_renderer(&self, window: &Arc<Window>) -> Result<Renderer> {
        let size = window.inner_size();
        Ok(Renderer::new(
            window.clone(),
            size.width,
            size.height,
            window.scale_factor(),
            self.config.font.family.clone(),
            self.font_size,
        )?)
    }

    fn create_window(&self, event_loop: &ActiveEventLoop) -> Result<WindowState> {
        let attrs = Window::default_attributes()
            .with_title(DEFAULT_TITLE)
            .with_inner_size(LogicalSize::new(900.0, 600.0))
            .with_decorations(!is_wslg_wayland(event_loop));
        let window = Arc::new(
            event_loop
                .create_window(attrs)
                .context("failed to create window")?,
        );
        window.set_ime_allowed(true);
        let renderer = self.create_renderer(&window)?;
        let grid = grid_size(
            window.inner_size(),
            padding(&self.config, window.scale_factor()),
            renderer.cell_metrics(),
        );

        let pane = PaneId(0);
        let proxy = self.proxy.clone();
        let options = SpawnOptions {
            shell: self.config.shell.as_ref().map(|s| Shell {
                program: s.program.clone(),
                args: s.args.clone(),
            }),
            working_directory: None,
            scrollback: self.config.scrollback,
        };
        let term = TermHandle::spawn(options, grid, move |event| {
            let _ = proxy.send_event(UserEvent::Term(pane, event));
        })?;

        Ok(WindowState {
            window,
            renderer,
            term,
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
        })
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
        if let Some(text) = self.state.as_ref().and_then(|s| s.term.selection_text()) {
            self.set_clipboard(text);
        }
    }

    fn set_font_size(&mut self, size: f32) {
        self.font_size = size.clamp(MIN_FONT_SIZE, MAX_FONT_SIZE);
        if let Some(state) = self.state.as_mut() {
            let scale = state.window.scale_factor();
            state.renderer.set_font_size(self.font_size, scale);
            state.resize_term(&self.config);
            state.window.request_redraw();
        }
    }

    fn run_action(&mut self, action: Action) {
        let Some(state) = self.state.as_mut() else {
            return;
        };
        match action {
            Action::Copy => self.copy_selection(),
            Action::Paste => {
                if let Some(text) = self.clipboard_text()
                    && let Some(state) = self.state.as_ref()
                {
                    state.term.paste(&text);
                }
            }
            Action::ScrollPageUp => state.term.scroll_page(true),
            Action::ScrollPageDown => state.term.scroll_page(false),
            Action::ScrollLineUp => state.term.scroll(1),
            Action::ScrollLineDown => state.term.scroll(-1),
            Action::ClearScrollback => state.term.clear_history(),
            Action::FontIncrease => self.set_font_size(self.font_size + 1.0),
            Action::FontDecrease => self.set_font_size(self.font_size - 1.0),
            Action::FontReset => self.set_font_size(self.config.font.size),
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
        if let Some(bytes) = input::encode_key(&key_input, state.term.mode()) {
            state.term.clear_selection();
            state.term.write(bytes);
            if let Some(state) = self.state.as_mut() {
                state.blink.reset();
            }
        }
    }

    fn mouse_input(&mut self, button: MouseButton, pressed: bool) {
        let Some(state) = self.state.as_mut() else {
            return;
        };
        let Some(pos) = state.mouse.position else {
            return;
        };
        let point = state.cell_at(&self.config, pos);
        let button = match button {
            MouseButton::Left => Button::Left,
            MouseButton::Middle => Button::Middle,
            MouseButton::Right => Button::Right,
            _ => return,
        };

        // Application mouse mode: forward presses and matching releases.
        let mode = state.term.mode();
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
            state.term.start_selection(kind, point);
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
        let point = state.cell_at(&self.config, pos);

        if state.mouse.selecting {
            state.term.update_selection(point);
            state.window.request_redraw();
            return;
        }

        let mode = state.term.mode();
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
            MouseScrollDelta::LineDelta(_, y) => (y as f64 * WHEEL_LINES).round() as i32,
            MouseScrollDelta::PixelDelta(p) => {
                state.mouse.scroll_pixels += p.y;
                let lines = (state.mouse.scroll_pixels / cell_height).trunc();
                state.mouse.scroll_pixels -= lines * cell_height;
                lines as i32
            }
        };
        if lines == 0 {
            return;
        }

        let mode = state.term.mode();
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
                .term
                .write(arrow.repeat(lines.unsigned_abs() as usize));
        } else {
            state.term.scroll(lines);
            state.window.request_redraw();
        }
    }
}

impl ApplicationHandler<UserEvent> for App {
    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
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

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        let Some(state) = self.state.as_mut() else {
            return;
        };
        match event {
            UserEvent::Term(_, TermEvent::Wakeup) => state.window.request_redraw(),
            UserEvent::Term(_, TermEvent::Title(title)) => state.window.set_title(&title),
            UserEvent::Term(_, TermEvent::ResetTitle) => state.window.set_title(DEFAULT_TITLE),
            UserEvent::Term(_, TermEvent::Exit) => event_loop.exit(),
            UserEvent::Term(_, TermEvent::ClipboardStore(text)) => self.set_clipboard(text),
            UserEvent::Term(_, TermEvent::Bell) => state.window.request_user_attention(None),
            UserEvent::ConfigReloaded(_) => {}
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
                state.resize_term(&self.config);
                state.window.request_redraw();
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                state.renderer.set_font_size(self.font_size, scale_factor);
                state.resize_term(&self.config);
                state.window.request_redraw();
            }
            WindowEvent::Focused(focused) => {
                state.focused = focused;
                state.window.request_redraw();
                if state.term.mode().contains(TermMode::FOCUS_IN_OUT) {
                    state.term.write(if focused {
                        &b"\x1b[I"[..]
                    } else {
                        &b"\x1b[O"[..]
                    });
                }
            }
            WindowEvent::ModifiersChanged(mods) => state.modifiers = mods,
            WindowEvent::KeyboardInput { event, .. } => self.keyboard_input(event),
            WindowEvent::Ime(Ime::Preedit(text, _)) => {
                state.preedit = (!text.is_empty()).then_some(text);
                state.window.request_redraw();
            }
            WindowEvent::Ime(Ime::Commit(text)) => {
                state.preedit = None;
                state.term.write(text.into_bytes());
            }
            WindowEvent::CursorMoved { position, .. } => self.cursor_moved(position),
            WindowEvent::CursorLeft { .. } => state.mouse.position = None,
            WindowEvent::MouseInput {
                state: button_state,
                button,
                ..
            } => self.mouse_input(button, button_state == ElementState::Pressed),
            WindowEvent::MouseWheel { delta, .. } => self.mouse_wheel(delta),
            WindowEvent::RedrawRequested => match state.redraw(&self.config) {
                FrameStatus::Presented => {}
                FrameStatus::Skipped => state.window.request_redraw(),
                FrameStatus::Lost => {
                    tracing::warn!("surface lost, recreating renderer");
                    let window = state.window.clone();
                    match self.create_renderer(&window) {
                        Ok(renderer) => {
                            if let Some(state) = self.state.as_mut() {
                                state.renderer = renderer;
                            }
                            window.request_redraw();
                        }
                        Err(err) => self.fail(event_loop, err),
                    }
                }
            },
            _ => {}
        }
    }
}
