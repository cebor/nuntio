//! Remote control for debugging (feature `debug-server`): a client such as
//! `cargo xtask drive` looks at the window (screenshots, grid text, UI
//! state) and sends keys, text, mouse input and actions through the same
//! paths as real input.
//!
//! nuntio listens on a random port on 127.0.0.1 and writes the port and a
//! token to the file given with `--debug-server`. Each request is one line
//! of JSON with the token and a `cmd`; each reply is one line of JSON,
//! followed by `bytes` bytes of raw RGBA pixels for images. Requests are
//! handled on the main thread, one at a time.

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use nuntio_config::{KeyCombo, KeyName};
use nuntio_render::{Capture, FrameStatus};
use nuntio_term::{Rgb, Snapshot, SnapshotCell};
use serde::Deserialize;
use serde_json::{Value, json};
use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::event::{Modifiers, MouseButton, MouseScrollDelta};
use winit::event_loop::EventLoopProxy;
use winit::keyboard::{
    Key, KeyCode, KeyLocation, ModifiersState, NamedKey, NativeKeyCode, PhysicalKey,
};

use super::Core;
use crate::actions::{Action, named_key};
use crate::actions_menu::MenuKind;
use crate::banner::Banner;
use crate::event::{PaneId, UserEvent};
use crate::input::{KeyEventKind, KeyPress};
use crate::window::WindowState;
use nuntio_render::Rect;

/// How long a connection waits for the main thread to answer.
const REPLY_TIMEOUT: Duration = Duration::from_secs(30);
/// Frames kept by a recording unless the client asks for another limit.
const DEFAULT_MAX_FRAMES: usize = 60;

/// A request and where its reply goes.
#[derive(Debug)]
pub struct DebugCall {
    pub request: Request,
    pub reply: mpsc::Sender<Reply>,
}

#[derive(Debug)]
pub struct Reply {
    pub json: Value,
    /// Raw pixel data announced by `json`.
    pub binary: Vec<u8>,
}

impl Reply {
    fn ok(mut json: Value) -> Self {
        if let Value::Object(map) = &mut json {
            map.insert("ok".into(), true.into());
        }
        Self {
            json,
            binary: Vec::new(),
        }
    }

    fn error(message: impl Into<String>) -> Self {
        Self {
            json: json!({ "ok": false, "error": message.into() }),
            binary: Vec::new(),
        }
    }
}

#[derive(Debug, Deserialize)]
struct Envelope {
    token: String,
    #[serde(flatten)]
    request: Request,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Request {
    State,
    /// The visible lines of a pane (the focused one by default).
    Text {
        pane: Option<u64>,
    },
    /// Colors and attributes of the cells in one line.
    Cells {
        pane: Option<u64>,
        line: usize,
        #[serde(default)]
        from: usize,
        to: Option<usize>,
    },
    /// The window as drawn, or only the area of a visible pane.
    Screenshot {
        pane: Option<u64>,
    },
    /// Key combinations like `ctrl+shift+t` or `enter`, pressed and released
    /// one after the other.
    Key {
        keys: Vec<String>,
    },
    /// Text as the input method would commit it.
    Type {
        text: String,
    },
    Paste {
        text: String,
    },
    Action {
        name: String,
    },
    Mouse(MouseRequest),
    /// New inner size in physical pixels.
    Resize {
        width: u32,
        height: u32,
    },
    /// Minimize, maximize or restore the window, as the window manager would.
    Window {
        minimized: Option<bool>,
        maximized: Option<bool>,
    },
    RecordStart {
        max_frames: Option<usize>,
    },
    RecordStop,
    Quit,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct MouseRequest {
    kind: MouseKind,
    /// Window position in physical pixels…
    x: Option<f64>,
    y: Option<f64>,
    /// …or the center of a cell of a pane (the focused one by default).
    pane: Option<u64>,
    column: Option<usize>,
    line: Option<usize>,
    #[serde(default)]
    button: ButtonName,
    /// Clicks for `click` (2 for a double click).
    #[serde(default = "one")]
    count: u32,
    /// Wheel notches for `scroll`; positive scrolls up.
    #[serde(default)]
    lines: f32,
    /// Modifiers held meanwhile: `shift`, `ctrl`, `alt`, `super`.
    #[serde(default)]
    mods: Vec<String>,
}

fn one() -> u32 {
    1
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum MouseKind {
    Move,
    Down,
    Up,
    Click,
    Scroll,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ButtonName {
    #[default]
    Left,
    Middle,
    Right,
}

/// Debug server state kept by the app.
#[derive(Default)]
pub struct DebugState {
    /// The server runs: the window acts as focused all the time.
    pub enabled: bool,
    /// The window stays off-screen and out of the taskbar. Not hidden:
    /// hidden windows get no redraws on Windows.
    pub headless: bool,
    /// When a pane last had output, for `idle_ms`.
    pub last_output: Option<Instant>,
    recording: Option<Recording>,
}

struct Recording {
    start: Instant,
    max_frames: usize,
    frames: Vec<RecordedFrame>,
}

struct RecordedFrame {
    at: Duration,
    status: FrameStatus,
    /// Missing past `max_frames` or if the capture failed.
    capture: Option<Capture>,
}

/// Listen on 127.0.0.1 and write the port and token to `state_file`.
pub fn start(state_file: &Path, proxy: EventLoopProxy<UserEvent>) -> Result<()> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).context("debug server: failed to bind")?;
    let port = listener.local_addr()?.port();
    let token = token();
    let info = json!({ "port": port, "token": token, "pid": std::process::id() });
    write_private(state_file, &info.to_string())
        .with_context(|| format!("failed to write {}", state_file.display()))?;
    tracing::info!(port, file = %state_file.display(), "debug server listening");
    std::thread::Builder::new()
        .name("debug-server".into())
        .spawn(move || {
            for stream in listener.incoming().flatten() {
                let (proxy, token) = (proxy.clone(), token.clone());
                std::thread::spawn(move || {
                    if let Err(err) = serve(stream, &token, &proxy) {
                        tracing::debug!("debug connection ended: {err}");
                    }
                });
            }
        })?;
    Ok(())
}

/// Write `contents` to a file only the user can read: whoever knows the
/// token can type into the shells.
fn write_private(path: &Path, contents: &str) -> std::io::Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options.open(path)?;
    // An older file keeps its permissions when it is opened.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    file.write_all(contents.as_bytes())
}

/// A token nobody else on the machine can guess: `RandomState` is seeded
/// from the OS's random source.
fn token() -> String {
    use std::hash::{BuildHasher, RandomState};
    let a = RandomState::new().hash_one(std::process::id());
    let b = RandomState::new().hash_one(Instant::now());
    format!("{a:016x}{b:016x}")
}

fn serve(stream: TcpStream, token: &str, proxy: &EventLoopProxy<UserEvent>) -> Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut writer = stream;
    let mut line = String::new();
    while reader.read_line(&mut line)? > 0 {
        let reply = match parse(&line, token) {
            Ok(request) => call(request, proxy),
            Err(err) => Reply::error(err),
        };
        writer.write_all(reply.json.to_string().as_bytes())?;
        writer.write_all(b"\n")?;
        writer.write_all(&reply.binary)?;
        writer.flush()?;
        line.clear();
    }
    Ok(())
}

fn parse(line: &str, token: &str) -> Result<Request, String> {
    let envelope: Envelope = serde_json::from_str(line).map_err(|err| err.to_string())?;
    if !same_token(&envelope.token, token) {
        return Err("wrong token".into());
    }
    Ok(envelope.request)
}

/// Compare in constant time, so the time an answer takes doesn't tell how
/// much of a guessed token was right.
fn same_token(given: &str, token: &str) -> bool {
    let (given, token) = (given.as_bytes(), token.as_bytes());
    given.len() == token.len()
        && given
            .iter()
            .zip(token)
            .fold(0, |diff, (a, b)| diff | (a ^ b))
            == 0
}

/// Hand the request to the main thread and wait for its reply.
fn call(request: Request, proxy: &EventLoopProxy<UserEvent>) -> Reply {
    let (tx, rx) = mpsc::channel();
    let call = DebugCall { request, reply: tx };
    if proxy.send_event(UserEvent::Debug(call)).is_err() {
        return Reply::error("nuntio is shutting down");
    }
    rx.recv_timeout(REPLY_TIMEOUT)
        .unwrap_or_else(|_| Reply::error("no reply from the main thread"))
}

impl Core {
    pub(super) fn debug_call(&mut self, state: Option<&mut WindowState>, call: DebugCall) {
        let reply = match state {
            Some(state) => self.debug_request(state, call.request),
            None => Err("no window yet".into()),
        };
        let _ = call.reply.send(reply.unwrap_or_else(Reply::error));
    }

    fn debug_request(
        &mut self,
        state: &mut WindowState,
        request: Request,
    ) -> Result<Reply, String> {
        match request {
            Request::State => Ok(Reply::ok(self.debug_state(state))),
            Request::Text { pane } => {
                let snapshot = pane_snapshot(state, pane)?;
                Ok(Reply::ok(json!({
                    "columns": snapshot.columns,
                    "lines": snapshot.lines,
                    "cursor": snapshot.cursor.map(|c| [c.column, c.line]),
                    "text": screen_lines(&snapshot),
                })))
            }
            Request::Cells {
                pane,
                line,
                from,
                to,
            } => {
                let snapshot = pane_snapshot(state, pane)?;
                if line >= snapshot.lines {
                    return Err(format!("line {line} is outside the grid"));
                }
                let to = to.unwrap_or(snapshot.columns).min(snapshot.columns);
                let cells: Vec<Value> = (from..to)
                    .map(|column| cell_json(column, snapshot.cell(column, line)))
                    .collect();
                Ok(Reply::ok(json!({ "cells": cells })))
            }
            Request::Screenshot { pane } => self.screenshot(state, pane),
            Request::Key { keys } => {
                let combos = keys
                    .iter()
                    .map(|key| KeyCombo::parse(key).map_err(|err| format!("{key}: {err}")))
                    .collect::<Result<Vec<_>, _>>()?;
                for combo in combos {
                    let (press, mods) = key_press(&combo);
                    self.inject_key(state, press, mods);
                }
                Ok(Reply::ok(json!({})))
            }
            Request::Type { text } => {
                state.type_text(&text);
                state.window.request_redraw();
                Ok(Reply::ok(json!({})))
            }
            Request::Paste { text } => {
                self.paste(state, text);
                Ok(Reply::ok(json!({})))
            }
            Request::Action { name } => {
                let action = Action::from_name(&name)?.ok_or("`none` is not an action")?;
                self.run_action(state, action);
                Ok(Reply::ok(json!({})))
            }
            Request::Mouse(mouse) => {
                self.inject_mouse(state, &mouse)?;
                Ok(Reply::ok(json!({})))
            }
            Request::Resize { width, height } => {
                if let Some(size) = state
                    .window
                    .request_inner_size(PhysicalSize::new(width, height))
                {
                    state.renderer.resize(size.width, size.height);
                    state.resize_terms(&self.config);
                }
                state.window.request_redraw();
                Ok(Reply::ok(json!({})))
            }
            Request::Window {
                minimized,
                maximized,
            } => {
                if let Some(minimized) = minimized {
                    state.window.set_minimized(minimized);
                }
                if let Some(maximized) = maximized {
                    state.window.set_maximized(maximized);
                }
                Ok(Reply::ok(json!({})))
            }
            Request::RecordStart { max_frames } => {
                self.debug.recording = Some(Recording {
                    start: Instant::now(),
                    max_frames: max_frames.unwrap_or(DEFAULT_MAX_FRAMES),
                    frames: Vec::new(),
                });
                Ok(Reply::ok(json!({})))
            }
            Request::RecordStop => {
                let recording = self.debug.recording.take().ok_or("not recording")?;
                Ok(recording_reply(recording))
            }
            Request::Quit => {
                // No confirmation: the client wants it gone.
                self.pending_close = None;
                self.exit_requested = true;
                Ok(Reply::ok(json!({})))
            }
        }
    }

    fn debug_state(&mut self, state: &mut WindowState) -> Value {
        let config = &self.config;
        let size = state.window.inner_size();
        let cell = state.renderer.cell_metrics();
        let layout = state.layout(config);
        let active = state.tabs.active_index();
        let now = Instant::now();
        let mut tabs = Vec::new();
        for (index, tab) in state.tabs.iter_mut().enumerate() {
            let focused = tab.content.focused;
            let mut panes = Vec::new();
            for pane in &mut tab.content.panes {
                let rect = (index == active)
                    .then(|| layout.rect(pane.id))
                    .flatten()
                    .map(|r| [r.x, r.y, r.width, r.height]);
                let grid = pane.grid_size().map(|g| [g.columns, g.lines]);
                panes.push(json!({
                    "id": pane.id.0,
                    "focused": pane.id == focused,
                    "rect": rect,
                    "grid": grid,
                    "process": pane.term.process_name(),
                    "app_title": pane.title,
                    "shell": pane.shell_name,
                }));
            }
            let title = match tab.content.pane_mut(focused) {
                Some(pane) => pane.cached_title(config.tabs.title, now).0,
                None => String::new(),
            };
            tabs.push(json!({
                "index": index,
                "active": index == active,
                "title": title,
                "activity": tab.activity,
                "bell": tab.bell,
                "panes": panes,
            }));
        }
        let banner = self.banner.as_ref().map(|b: &Banner| {
            json!({
                "severity": format!("{:?}", b.severity),
                "title": b.title,
                "messages": b.messages,
            })
        });
        json!({
            "window": {
                "width": size.width,
                "height": size.height,
                "scale": state.window.scale_factor(),
                "focused": state.focused,
                "maximized": state.window.is_maximized(),
                "minimized": state.window.is_minimized(),
                "position": state.window.outer_position().ok().map(|p| [p.x, p.y]),
                "headless": self.debug.headless,
            },
            "cell": { "width": cell.width, "height": cell.height },
            "font_size": self.font_size,
            "tabs": tabs,
            "search": state.search.as_ref().map(|bar| json!({ "query": bar.query(), "regex": bar.regex() })),
            "banner": banner,
            "actions_menu": state.actions_menu.as_ref().map(|menu| json!({
                "kind": match menu.kind() {
                    MenuKind::Actions => "actions",
                    MenuKind::Shells => "shells",
                },
                "selected": menu.selected_label(),
                "entries": menu.labels(),
            })),
            "pending_close": self.pending_close.is_some(),
            "pending_paste": self.pending_paste.is_some(),
            "idle_ms": self.debug.last_output.map(|t| now.duration_since(t).as_millis() as u64),
            "recording": self.debug.recording.is_some(),
        })
    }

    fn screenshot(&mut self, state: &mut WindowState, pane: Option<u64>) -> Result<Reply, String> {
        let config = &self.config;
        let area = match pane {
            None => None,
            Some(id) => Some(
                state
                    .layout(config)
                    .rect(PaneId(id))
                    .ok_or(format!("pane {id} is not visible"))?,
            ),
        };
        let capture = state
            .with_frame(
                config,
                &self.stats,
                self.banner.as_ref(),
                |renderer, frame| renderer.capture(frame),
            )
            .ok_or("nothing to draw")?
            .map_err(|err| err.to_string())?;
        let capture = match area {
            Some(rect) => crop(&capture, rect),
            None => capture,
        };
        Ok(image_reply(capture))
    }

    /// Press and release a key with `mods` held.
    fn inject_key(&mut self, state: &mut WindowState, press: KeyPress, mods: ModifiersState) {
        let saved = hold_modifiers(state, mods);
        self.synthetic_input = true;
        self.keyboard_input(state, &press);
        self.keyboard_input(
            state,
            &KeyPress {
                kind: KeyEventKind::Release,
                ..press
            },
        );
        self.synthetic_input = false;
        state.modifiers = saved;
    }

    fn inject_mouse(
        &mut self,
        state: &mut WindowState,
        mouse: &MouseRequest,
    ) -> Result<(), String> {
        let position = self.mouse_position(state, mouse)?;
        let mods = parse_mods(&mouse.mods)?;
        let button = match mouse.button {
            ButtonName::Left => MouseButton::Left,
            ButtonName::Middle => MouseButton::Middle,
            ButtonName::Right => MouseButton::Right,
        };
        let saved = hold_modifiers(state, mods);
        self.synthetic_input = true;
        if let Some(pos) = position {
            self.cursor_moved(state, pos);
        }
        match mouse.kind {
            MouseKind::Move => {}
            MouseKind::Down => self.mouse_input(state, button, true),
            MouseKind::Up => self.mouse_input(state, button, false),
            MouseKind::Click => {
                for _ in 0..mouse.count {
                    self.mouse_input(state, button, true);
                    self.mouse_input(state, button, false);
                }
            }
            MouseKind::Scroll => {
                let delta = MouseScrollDelta::LineDelta(0.0, mouse.lines);
                state.mouse_wheel(&self.config, delta);
            }
        }
        self.synthetic_input = false;
        state.modifiers = saved;
        state.window.request_redraw();
        Ok(())
    }

    /// Where the request puts the pointer, if anywhere.
    fn mouse_position(
        &self,
        state: &WindowState,
        mouse: &MouseRequest,
    ) -> Result<Option<PhysicalPosition<f64>>, String> {
        match (mouse.x, mouse.y, mouse.column, mouse.line) {
            (Some(x), Some(y), None, None) => Ok(Some(PhysicalPosition::new(x, y))),
            (None, None, Some(column), Some(line)) => {
                let id = mouse.pane.map_or(state.content().focused, PaneId);
                let rect = state
                    .layout(&self.config)
                    .rect(id)
                    .ok_or(format!("pane {} is not visible", id.0))?;
                let (x0, y0) = state.grid_origin(&self.config, rect);
                let cell = state.renderer.cell_metrics();
                Ok(Some(PhysicalPosition::new(
                    f64::from(x0) + (column as f64 + 0.5) * f64::from(cell.width),
                    f64::from(y0) + (line as f64 + 0.5) * f64::from(cell.height),
                )))
            }
            (None, None, None, None) => Ok(None),
            _ => Err("give either x and y or column and line".into()),
        }
    }

    /// Redraw while recording: the frame is also captured.
    pub(super) fn debug_redraw(&mut self, state: &mut WindowState) -> Option<FrameStatus> {
        let recording = self.debug.recording.as_mut()?;
        let room = recording.frames.len() < recording.max_frames;
        let (status, capture) = state
            .with_frame(
                &self.config,
                &self.stats,
                self.banner.as_ref(),
                |renderer, frame| {
                    let status = renderer.render(frame);
                    let capture = (room && status == FrameStatus::Presented)
                        .then(|| renderer.capture(frame))
                        .and_then(|capture| {
                            capture.inspect_err(|err| tracing::warn!("{err}")).ok()
                        });
                    (status, capture)
                },
            )
            .unwrap_or((FrameStatus::Skipped, None));
        recording.frames.push(RecordedFrame {
            at: recording.start.elapsed(),
            status,
            capture,
        });
        Some(status)
    }
}

/// A pane of any tab, the focused one of the active tab by default.
fn pane_snapshot(state: &WindowState, pane: Option<u64>) -> Result<Snapshot, String> {
    let pane = match pane {
        None => state.content().focused_pane(),
        Some(id) => state
            .tabs
            .iter()
            .find_map(|tab| tab.content.pane(PaneId(id)))
            .ok_or(format!("no pane {id}"))?,
    };
    Ok(pane.term.snapshot())
}

/// Set the modifiers as if held; returns the real ones to restore.
fn hold_modifiers(state: &mut WindowState, mods: ModifiersState) -> Modifiers {
    std::mem::replace(&mut state.modifiers, Modifiers::from(mods))
}

/// Force focus on for a freshly created window.
pub fn focus(state: &mut WindowState) {
    state.focused = true;
    state.send_focus(true);
}

/// A key event for a combination, and the modifiers it needs.
fn key_press(combo: &KeyCombo) -> (KeyPress, ModifiersState) {
    let m = combo.mods;
    let mut mods = ModifiersState::empty();
    for (on, flag) in [
        (m.ctrl, ModifiersState::CONTROL),
        (m.shift, ModifiersState::SHIFT),
        (m.alt, ModifiersState::ALT),
        (m.super_key, ModifiersState::SUPER),
    ] {
        if on {
            mods |= flag;
        }
    }
    let press = match combo.key {
        KeyName::Char(c) => {
            let shifted = if m.shift {
                c.to_uppercase().next().unwrap_or(c)
            } else {
                c
            };
            let physical = char_code(c).map_or(
                PhysicalKey::Unidentified(NativeKeyCode::Unidentified),
                PhysicalKey::Code,
            );
            KeyPress {
                logical: Key::Character(shifted.to_string().into()),
                unmodified: Key::Character(c.to_string().into()),
                // Like winit: Ctrl and Cmd combinations type nothing.
                text: (!m.ctrl && !m.super_key).then(|| shifted.to_string()),
                location: KeyLocation::Standard,
                physical,
                kind: KeyEventKind::Press,
            }
        }
        KeyName::Named(name) => {
            let named = named_key(name);
            let text = match named {
                NamedKey::Enter => Some("\r"),
                NamedKey::Tab => Some("\t"),
                NamedKey::Space => Some(" "),
                NamedKey::Backspace => Some("\u{8}"),
                NamedKey::Escape => Some("\u{1b}"),
                NamedKey::Delete => Some("\u{7f}"),
                _ => None,
            };
            KeyPress {
                logical: Key::Named(named),
                unmodified: Key::Named(named),
                text: text.map(str::to_owned),
                location: KeyLocation::Standard,
                physical: named_code(named).map_or(
                    PhysicalKey::Unidentified(NativeKeyCode::Unidentified),
                    PhysicalKey::Code,
                ),
                kind: KeyEventKind::Press,
            }
        }
    };
    (press, mods)
}

/// The US-layout key for a letter or digit.
fn char_code(c: char) -> Option<KeyCode> {
    use KeyCode::*;
    const LETTERS: [KeyCode; 26] = [
        KeyA, KeyB, KeyC, KeyD, KeyE, KeyF, KeyG, KeyH, KeyI, KeyJ, KeyK, KeyL, KeyM, KeyN, KeyO,
        KeyP, KeyQ, KeyR, KeyS, KeyT, KeyU, KeyV, KeyW, KeyX, KeyY, KeyZ,
    ];
    const DIGITS: [KeyCode; 10] = [
        Digit0, Digit1, Digit2, Digit3, Digit4, Digit5, Digit6, Digit7, Digit8, Digit9,
    ];
    match c {
        'a'..='z' => Some(LETTERS[(c as u8 - b'a') as usize]),
        '0'..='9' => Some(DIGITS[(c as u8 - b'0') as usize]),
        _ => None,
    }
}

fn named_code(key: NamedKey) -> Option<KeyCode> {
    let code = match key {
        NamedKey::Enter => KeyCode::Enter,
        NamedKey::Tab => KeyCode::Tab,
        NamedKey::Escape => KeyCode::Escape,
        NamedKey::Space => KeyCode::Space,
        NamedKey::Backspace => KeyCode::Backspace,
        NamedKey::Delete => KeyCode::Delete,
        NamedKey::Insert => KeyCode::Insert,
        NamedKey::Home => KeyCode::Home,
        NamedKey::End => KeyCode::End,
        NamedKey::PageUp => KeyCode::PageUp,
        NamedKey::PageDown => KeyCode::PageDown,
        NamedKey::ArrowUp => KeyCode::ArrowUp,
        NamedKey::ArrowDown => KeyCode::ArrowDown,
        NamedKey::ArrowLeft => KeyCode::ArrowLeft,
        NamedKey::ArrowRight => KeyCode::ArrowRight,
        NamedKey::F1 => KeyCode::F1,
        NamedKey::F2 => KeyCode::F2,
        NamedKey::F3 => KeyCode::F3,
        NamedKey::F4 => KeyCode::F4,
        NamedKey::F5 => KeyCode::F5,
        NamedKey::F6 => KeyCode::F6,
        NamedKey::F7 => KeyCode::F7,
        NamedKey::F8 => KeyCode::F8,
        NamedKey::F9 => KeyCode::F9,
        NamedKey::F10 => KeyCode::F10,
        NamedKey::F11 => KeyCode::F11,
        NamedKey::F12 => KeyCode::F12,
        _ => return None,
    };
    Some(code)
}

fn parse_mods(names: &[String]) -> Result<ModifiersState, String> {
    let mut mods = ModifiersState::empty();
    for name in names {
        mods |= match name.to_lowercase().as_str() {
            "shift" => ModifiersState::SHIFT,
            "ctrl" | "control" => ModifiersState::CONTROL,
            "alt" | "opt" | "option" => ModifiersState::ALT,
            "super" | "cmd" | "win" | "meta" => ModifiersState::SUPER,
            _ => return Err(format!("unknown modifier `{name}`")),
        };
    }
    Ok(mods)
}

/// The screen as text: one string per line, a wide character counted
/// once, trailing blanks dropped.
fn screen_lines(snapshot: &Snapshot) -> Vec<String> {
    (0..snapshot.lines)
        .map(|line| {
            let mut text = String::with_capacity(snapshot.columns);
            let mut column = 0;
            while column < snapshot.columns {
                let cell = snapshot.cell(column, line);
                text.push(cell.c);
                if let Some(marks) = &cell.zerowidth {
                    text.extend(marks.iter());
                }
                column += if cell.style.wide { 2 } else { 1 };
            }
            text.truncate(text.trim_end().len());
            text
        })
        .collect()
}

fn hex(c: Rgb) -> String {
    format!("#{:02x}{:02x}{:02x}", c.r, c.g, c.b)
}

fn cell_json(column: usize, cell: &SnapshotCell) -> Value {
    let mut text = cell.c.to_string();
    if let Some(marks) = &cell.zerowidth {
        text.extend(marks.iter());
    }
    json!({
        "column": column,
        "text": text,
        "fg": hex(cell.fg),
        "bg": hex(cell.bg),
        "bold": cell.style.bold,
        "italic": cell.style.italic,
        "underline": cell.style.underline.map(|u| format!("{u:?}")),
        "underline_color": cell.underline_color.map(hex),
        "strikeout": cell.style.strikeout,
        "wide": cell.style.wide,
    })
}

/// The part of `capture` under `rect`, cut to the image.
fn crop(capture: &Capture, rect: Rect) -> Capture {
    let clamp = |v: f32, max: u32| (v.max(0.0) as u32).min(max);
    let (left, top) = (clamp(rect.x, capture.width), clamp(rect.y, capture.height));
    let right = clamp(rect.x + rect.width, capture.width);
    let bottom = clamp(rect.y + rect.height, capture.height);
    let (width, height) = (right - left, bottom - top);
    let mut rgba = Vec::with_capacity((width * height * 4) as usize);
    for y in top..bottom {
        let start = ((y * capture.width + left) * 4) as usize;
        rgba.extend_from_slice(&capture.rgba[start..start + width as usize * 4]);
    }
    Capture {
        width,
        height,
        rgba,
    }
}

fn image_reply(capture: Capture) -> Reply {
    Reply {
        json: json!({
            "ok": true,
            "width": capture.width,
            "height": capture.height,
            "bytes": capture.rgba.len(),
        }),
        binary: capture.rgba,
    }
}

fn recording_reply(recording: Recording) -> Reply {
    let mut binary = Vec::new();
    let frames: Vec<Value> = recording
        .frames
        .into_iter()
        .map(|frame| {
            let (width, height, bytes) = match frame.capture {
                Some(capture) => {
                    let bytes = capture.rgba.len();
                    binary.extend_from_slice(&capture.rgba);
                    (capture.width, capture.height, bytes)
                }
                None => (0, 0, 0),
            };
            json!({
                "ms": frame.at.as_secs_f64() * 1000.0,
                "status": format!("{:?}", frame.status),
                "width": width,
                "height": height,
                "bytes": bytes,
            })
        })
        .collect();
    Reply {
        json: json!({ "ok": true, "frames": frames, "bytes": binary.len() }),
        binary,
    }
}

impl Core {
    /// A pane had output (for `idle_ms`).
    pub(super) fn debug_output(&mut self) {
        self.debug.last_output = Some(Instant::now());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nuntio_term::CellStyle;

    fn request(line: &str) -> Result<Request, String> {
        parse(line, "secret")
    }

    #[test]
    fn tokens_must_match_exactly() {
        assert!(same_token("abc123", "abc123"));
        assert!(!same_token("abc124", "abc123"));
        assert!(!same_token("abc", "abc123"));
        assert!(!same_token("", "abc123"));
    }

    #[test]
    #[cfg(unix)]
    fn the_state_file_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let path = std::env::temp_dir().join(format!("nuntio-debug-{}", std::process::id()));
        std::fs::write(&path, "old").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        write_private(&path, "new").unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        assert_eq!(mode & 0o777, 0o600);
        assert_eq!(text, "new");
    }

    #[test]
    fn requests_need_the_token() {
        assert_eq!(
            request(r#"{"token":"secret","cmd":"state"}"#),
            Ok(Request::State)
        );
        assert_eq!(
            request(r#"{"token":"guess","cmd":"state"}"#),
            Err("wrong token".into())
        );
        assert!(request(r#"{"cmd":"state"}"#).is_err());
    }

    #[test]
    fn requests_parse_with_defaults() {
        assert_eq!(
            request(r#"{"token":"secret","cmd":"key","keys":["ctrl+c","enter"]}"#),
            Ok(Request::Key {
                keys: vec!["ctrl+c".into(), "enter".into()]
            })
        );
        let Ok(Request::Mouse(mouse)) =
            request(r#"{"token":"secret","cmd":"mouse","kind":"click","x":10,"y":20}"#)
        else {
            panic!("not a mouse request");
        };
        assert_eq!(mouse.kind, MouseKind::Click);
        assert_eq!((mouse.x, mouse.y), (Some(10.0), Some(20.0)));
        assert_eq!((mouse.button, mouse.count), (ButtonName::Left, 1));
        assert_eq!(
            request(r#"{"token":"secret","cmd":"window","minimized":false}"#),
            Ok(Request::Window {
                minimized: Some(false),
                maximized: None
            })
        );
        assert!(request(r#"{"token":"secret","cmd":"fly"}"#).is_err());
    }

    #[test]
    fn combos_become_key_presses() {
        let (press, mods) = key_press(&KeyCombo::parse("ctrl+c").unwrap());
        assert_eq!(mods, ModifiersState::CONTROL);
        assert_eq!(press.unmodified, Key::Character("c".into()));
        assert_eq!(press.physical, PhysicalKey::Code(KeyCode::KeyC));
        assert_eq!(press.text, None);

        let (press, mods) = key_press(&KeyCombo::parse("shift+a").unwrap());
        assert_eq!(mods, ModifiersState::SHIFT);
        assert_eq!(press.logical, Key::Character("A".into()));
        assert_eq!(press.unmodified, Key::Character("a".into()));
        assert_eq!(press.text.as_deref(), Some("A"));

        let (press, mods) = key_press(&KeyCombo::parse("enter").unwrap());
        assert_eq!(mods, ModifiersState::empty());
        assert_eq!(press.logical, Key::Named(NamedKey::Enter));
        assert_eq!(press.physical, PhysicalKey::Code(KeyCode::Enter));
        assert_eq!(press.text.as_deref(), Some("\r"));
    }

    fn cell(c: char, wide: bool) -> SnapshotCell {
        SnapshotCell {
            c,
            zerowidth: None,
            fg: Rgb { r: 0, g: 0, b: 0 },
            bg: Rgb { r: 0, g: 0, b: 0 },
            style: CellStyle {
                wide,
                ..CellStyle::default()
            },
            underline_color: None,
        }
    }

    #[test]
    fn screen_lines_count_wide_characters_once() {
        let snapshot = Snapshot {
            columns: 5,
            lines: 2,
            cells: vec![
                cell('a', false),
                cell('日', true),
                cell(' ', false),
                cell('b', false),
                cell(' ', false),
                cell(' ', false),
                cell(' ', false),
                cell(' ', false),
                cell(' ', false),
                cell(' ', false),
            ],
            ..Snapshot::default()
        };
        assert_eq!(screen_lines(&snapshot), ["a日b", ""]);
    }

    #[test]
    fn crops_stay_inside_the_image() {
        // 3x2 pixels numbered 0..6.
        let capture = Capture {
            width: 3,
            height: 2,
            rgba: (0..6u8).flat_map(|i| [i; 4]).collect(),
        };
        let part = crop(
            &capture,
            Rect {
                x: 1.0,
                y: 1.0,
                width: 5.0,
                height: 5.0,
            },
        );
        assert_eq!((part.width, part.height), (2, 1));
        assert_eq!(part.rgba, [4, 4, 4, 4, 5, 5, 5, 5]);
    }
}
