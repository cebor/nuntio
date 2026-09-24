use std::sync::Arc;

use anyhow::{Context, Result};
use nuntio_config::Config;
use nuntio_render::{CellMetrics, FrameStatus, Renderer, Viewport};
use nuntio_term::{Shell, SpawnOptions, TermEvent, TermHandle, TermSize};
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalSize};
use winit::event::{ElementState, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoopProxy};
use winit::keyboard::ModifiersState;
use winit::window::{Window, WindowId};

use crate::event::{PaneId, UserEvent};
use crate::input;

const DEFAULT_TITLE: &str = "nuntio";

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

struct WindowState {
    window: Arc<Window>,
    renderer: Renderer,
    term: TermHandle,
    modifiers: ModifiersState,
}

impl WindowState {
    /// Padding around the grid in physical pixels.
    fn padding(&self, config: &Config) -> (u32, u32) {
        let scale = self.window.scale_factor();
        let p = config.window.padding;
        (
            (p.x as f64 * scale).round() as u32,
            (p.y as f64 * scale).round() as u32,
        )
    }

    fn resize_term(&self, config: &Config) {
        let size = grid_size(
            self.window.inner_size(),
            self.padding(config),
            self.renderer.cell_metrics(),
        );
        self.term.resize(size);
    }

    fn redraw(&mut self, config: &Config) -> FrameStatus {
        let snapshot = self.term.snapshot();
        let (x, y) = self.padding(config);
        self.renderer.render(&snapshot, Viewport { x, y })
    }
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
    state: Option<WindowState>,
    /// A fatal error that ended the event loop.
    error: Option<anyhow::Error>,
}

impl App {
    pub fn new(config: Config, proxy: EventLoopProxy<UserEvent>) -> Self {
        Self {
            config,
            proxy,
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
            self.config.font.size,
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

        let scale = window.scale_factor();
        let padding = self.config.window.padding;
        let padding = (
            (padding.x as f64 * scale).round() as u32,
            (padding.y as f64 * scale).round() as u32,
        );
        let size = grid_size(window.inner_size(), padding, renderer.cell_metrics());

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
        let term = TermHandle::spawn(options, size, move |event| {
            let _ = proxy.send_event(UserEvent::Term(pane, event));
        })?;

        Ok(WindowState {
            window,
            renderer,
            term,
            modifiers: ModifiersState::empty(),
        })
    }

    fn fail(&mut self, event_loop: &ActiveEventLoop, err: anyhow::Error) {
        tracing::error!("{err:#}");
        self.error = Some(err);
        event_loop.exit();
    }
}

impl ApplicationHandler<UserEvent> for App {
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
            UserEvent::Term(_, event) => tracing::debug!(?event, "unhandled terminal event"),
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
                state
                    .renderer
                    .set_font_size(self.config.font.size, scale_factor);
                state.resize_term(&self.config);
                state.window.request_redraw();
            }
            WindowEvent::ModifiersChanged(mods) => state.modifiers = mods.state(),
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                if let Some(bytes) = input::encode_key(&event, state.modifiers, state.term.mode()) {
                    state.term.write(bytes);
                }
            }
            WindowEvent::Ime(winit::event::Ime::Commit(text)) => {
                state.term.write(text.into_bytes());
            }
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
