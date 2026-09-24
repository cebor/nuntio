use std::borrow::Cow;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};

use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::event_loop::{EventLoop, EventLoopSender, Msg};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::{self, Term, viewport_to_point};
use alacritty_terminal::tty;
use thiserror::Error;

use crate::palette::Palette;
use crate::process;
use crate::search::{self, Search};
use crate::snapshot::Snapshot;
use crate::url::{self, Link};

#[derive(Debug, Error)]
pub enum SpawnError {
    #[error("failed to spawn shell: {0}")]
    Pty(#[source] std::io::Error),
    #[error("failed to start PTY event loop: {0}")]
    EventLoop(#[source] std::io::Error),
}

/// Events a pane reports to the application. Sent from the PTY thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TermEvent {
    /// New content is ready to be drawn.
    Wakeup,
    Title(String),
    ResetTitle,
    Bell,
    /// The shell exited.
    Exit,
    /// OSC 52 clipboard write.
    ClipboardStore(String),
}

#[derive(Debug, Clone, Default)]
pub struct Shell {
    pub program: String,
    pub args: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct SpawnOptions {
    /// `None` spawns the user's default shell.
    pub shell: Option<Shell>,
    pub working_directory: Option<PathBuf>,
    pub scrollback: usize,
    pub palette: Palette,
}

/// Terminal grid size plus the cell size in physical pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TermSize {
    pub columns: u16,
    pub lines: u16,
    pub cell_width: u16,
    pub cell_height: u16,
}

impl TermSize {
    fn window_size(self) -> WindowSize {
        WindowSize {
            num_lines: self.lines,
            num_cols: self.columns,
            cell_width: self.cell_width,
            cell_height: self.cell_height,
        }
    }
}

impl Dimensions for TermSize {
    fn total_lines(&self) -> usize {
        self.lines as usize
    }

    fn screen_lines(&self) -> usize {
        self.lines as usize
    }

    fn columns(&self) -> usize {
        self.columns as usize
    }
}

type Callback = dyn Fn(TermEvent) + Send + Sync;

/// Bridges alacritty's events to the application and answers terminal
/// queries (DA, color/size reports) directly on the PTY thread.
#[derive(Clone)]
struct Listener {
    inner: Arc<ListenerInner>,
}

struct ListenerInner {
    callback: Box<Callback>,
    /// Set once the event loop exists; replies are written back through it.
    sender: OnceLock<EventLoopSender>,
    palette: RwLock<Palette>,
    size: Mutex<WindowSize>,
    /// Coalesces wakeups: only one is in flight until the next snapshot.
    wakeup_pending: AtomicBool,
}

impl ListenerInner {
    fn write(&self, text: String) {
        if let Some(sender) = self.sender.get() {
            let _ = sender.send(Msg::Input(Cow::Owned(text.into_bytes())));
        }
    }
}

impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        let inner = &self.inner;
        let forward = match event {
            Event::Wakeup => {
                if inner.wakeup_pending.swap(true, Ordering::AcqRel) {
                    return;
                }
                TermEvent::Wakeup
            }
            Event::Title(title) => TermEvent::Title(title),
            Event::ResetTitle => TermEvent::ResetTitle,
            Event::Bell => TermEvent::Bell,
            // `ChildExit` comes before the remaining output is drained;
            // `Exit` follows once the terminal is done.
            Event::Exit => TermEvent::Exit,
            Event::ChildExit(status) => {
                tracing::debug!(%status, "shell exited");
                return;
            }
            Event::ClipboardStore(_, text) => TermEvent::ClipboardStore(text),
            Event::PtyWrite(text) => return inner.write(text),
            Event::ColorRequest(index, format) => {
                let color = inner.palette.read().unwrap().get(index);
                return inner.write(format(color));
            }
            Event::TextAreaSizeRequest(format) => {
                let size = *inner.size.lock().unwrap();
                return inner.write(format(size));
            }
            // Clipboard reads, mouse cursor and blinking follow in M2.
            Event::ClipboardLoad(..) | Event::MouseCursorDirty | Event::CursorBlinkingChange => {
                return;
            }
        };
        (inner.callback)(forward);
    }
}

/// One terminal: grid state, the shell's PTY and its IO thread.
pub struct TermHandle {
    term: Arc<FairMutex<Term<Listener>>>,
    listener: Listener,
    sender: EventLoopSender,
    shell_pid: Option<u32>,
    shell_name: String,
}

impl TermHandle {
    /// Spawn a shell. `callback` is invoked from the PTY thread.
    pub fn spawn(
        options: SpawnOptions,
        size: TermSize,
        callback: impl Fn(TermEvent) + Send + Sync + 'static,
    ) -> Result<Self, SpawnError> {
        let listener = Listener {
            inner: Arc::new(ListenerInner {
                callback: Box::new(callback),
                sender: OnceLock::new(),
                palette: RwLock::new(options.palette),
                size: Mutex::new(size.window_size()),
                wakeup_pending: AtomicBool::new(false),
            }),
        };

        let config = term::Config {
            scrolling_history: options.scrollback,
            ..Default::default()
        };
        let term = Arc::new(FairMutex::new(Term::new(config, &size, listener.clone())));

        let shell_program = options
            .shell
            .as_ref()
            .map(|s| s.program.clone())
            .unwrap_or_else(default_shell_name);
        #[cfg_attr(not(windows), allow(unused_mut))]
        let mut env = HashMap::from([
            ("TERM".into(), "xterm-256color".into()),
            ("COLORTERM".into(), "truecolor".into()),
            ("TERM_PROGRAM".into(), "nuntio".into()),
            (
                "TERM_PROGRAM_VERSION".into(),
                env!("CARGO_PKG_VERSION").into(),
            ),
        ]);
        #[cfg(windows)]
        env.insert(
            "WSLENV".into(),
            wslenv(std::env::var("WSLENV").ok().as_deref()),
        );
        let pty_options = tty::Options {
            shell: options.shell.map(|s| tty::Shell::new(s.program, s.args)),
            working_directory: options.working_directory,
            drain_on_exit: true,
            env,
            #[cfg(target_os = "windows")]
            escape_args: true,
        };
        let pty = tty::new(&pty_options, size.window_size(), 0).map_err(SpawnError::Pty)?;
        #[cfg(unix)]
        let shell_pid = Some(pty.child().id());
        #[cfg(windows)]
        let shell_pid = pty.child_watcher().pid().map(|pid| pid.get());
        let shell_name = Path::new(&shell_program)
            .file_stem()
            .map_or(shell_program.clone(), |s| s.to_string_lossy().into_owned());

        let event_loop = EventLoop::new(term.clone(), listener.clone(), pty, true, false)
            .map_err(SpawnError::EventLoop)?;
        let sender = event_loop.channel();
        let _ = listener.inner.sender.set(sender.clone());
        event_loop.spawn();

        Ok(Self {
            term,
            listener,
            sender,
            shell_pid,
            shell_name,
        })
    }

    /// Send user input to the shell.
    pub fn write(&self, bytes: impl Into<Cow<'static, [u8]>>) {
        let bytes = bytes.into();
        if bytes.is_empty() {
            return;
        }
        // Typing jumps back to the bottom, like every terminal does.
        self.term.lock().scroll_display(Scroll::Bottom);
        let _ = self.sender.send(Msg::Input(bytes));
    }

    /// Paste text, wrapped in bracketed-paste markers if the app asked for them.
    pub fn paste(&self, text: &str) {
        let bracketed = self.mode().contains(term::TermMode::BRACKETED_PASTE);
        self.write(encode_paste(text, bracketed));
    }

    /// Scroll the viewport; positive values move up into the scrollback.
    pub fn scroll(&self, lines: i32) {
        self.term.lock().scroll_display(Scroll::Delta(lines));
    }

    pub fn scroll_page(&self, up: bool) {
        self.term
            .lock()
            .scroll_display(if up { Scroll::PageUp } else { Scroll::PageDown });
    }

    /// Drop the scrollback, keeping the visible screen.
    pub fn clear_history(&self) {
        use alacritty_terminal::vte::ansi::{ClearMode, Handler};
        let mut term = self.term.lock();
        term.scroll_display(Scroll::Bottom);
        term.clear_screen(ClearMode::Saved);
    }

    pub fn start_selection(&self, kind: SelectionKind, point: GridPoint) {
        let mut term = self.term.lock();
        let (point, side) = point.to_grid(term.grid().display_offset());
        term.selection = Some(Selection::new(kind.into(), point, side));
    }

    pub fn update_selection(&self, point: GridPoint) {
        let mut term = self.term.lock();
        let (point, side) = point.to_grid(term.grid().display_offset());
        if let Some(selection) = term.selection.as_mut() {
            selection.update(point, side);
        }
    }

    pub fn clear_selection(&self) {
        self.term.lock().selection = None;
    }

    pub fn selection_text(&self) -> Option<String> {
        self.term
            .lock()
            .selection_to_string()
            .filter(|s| !s.is_empty())
    }

    pub fn resize(&self, size: TermSize) {
        *self.listener.inner.size.lock().unwrap() = size.window_size();
        self.term.lock().resize(size);
        let _ = self.sender.send(Msg::Resize(size.window_size()));
    }

    /// Name of the foreground process, or the shell's name if unknown.
    /// Used as the tab title when the application sets none.
    pub fn process_name(&self) -> String {
        self.shell_pid
            .and_then(process::foreground_name)
            .unwrap_or_else(|| self.shell_name.clone())
    }

    /// Working directory of the foreground process, if it can be determined.
    pub fn working_directory(&self) -> Option<PathBuf> {
        self.shell_pid.and_then(process::working_directory)
    }

    /// Change the colors, e.g. after a theme switch.
    pub fn set_palette(&self, palette: Palette) {
        *self.listener.inner.palette.write().unwrap() = palette;
    }

    /// Terminal modes, e.g. for application cursor keys.
    pub fn mode(&self) -> term::TermMode {
        *self.term.lock().mode()
    }

    /// Copy the visible screen for rendering.
    pub fn snapshot(&self) -> Snapshot {
        self.listener
            .inner
            .wakeup_pending
            .store(false, Ordering::Release);
        let term = self.term.lock();
        let palette = self.listener.inner.palette.read().unwrap();
        Snapshot::capture(&term, &palette, &[], None)
    }

    /// Like [`snapshot`](Self::snapshot), with the matches of `search`
    /// highlighted.
    pub fn search_snapshot(&self, search: &mut Search) -> Snapshot {
        self.listener
            .inner
            .wakeup_pending
            .store(false, Ordering::Release);
        let term = self.term.lock();
        let matches = search::visible_matches(&term, search);
        let palette = self.listener.inner.palette.read().unwrap();
        Snapshot::capture(&term, &palette, &matches, search.current())
    }

    /// Select the next match upwards (older output) or downwards and scroll
    /// to it. Returns whether there is a match.
    pub fn search(&self, search: &mut Search, up: bool) -> bool {
        search::find(&mut self.term.lock(), search, up)
    }

    /// The link (OSC 8 hyperlink or URL) at a viewport position.
    pub fn link_at(&self, point: GridPoint) -> Option<Link> {
        let term = self.term.lock();
        let (point, _) = point.to_grid(term.grid().display_offset());
        url::link_at(&term, point)
    }
}

impl Drop for TermHandle {
    fn drop(&mut self) {
        let _ = self.sender.send(Msg::Shutdown);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionKind {
    /// Character-wise.
    Simple,
    /// Rectangular (Alt+drag).
    Block,
    /// Word-wise (double click).
    Semantic,
    /// Line-wise (triple click).
    Lines,
}

impl From<SelectionKind> for SelectionType {
    fn from(kind: SelectionKind) -> Self {
        match kind {
            SelectionKind::Simple => SelectionType::Simple,
            SelectionKind::Block => SelectionType::Block,
            SelectionKind::Semantic => SelectionType::Semantic,
            SelectionKind::Lines => SelectionType::Lines,
        }
    }
}

/// A position in the visible grid, as seen by the mouse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GridPoint {
    pub column: usize,
    /// Viewport line, 0 = top of the window.
    pub line: usize,
    /// The pointer is on the right half of the cell.
    pub right_half: bool,
}

impl GridPoint {
    fn to_grid(self, display_offset: usize) -> (Point, Side) {
        let point = viewport_to_point(display_offset, Point::new(self.line, Column(self.column)));
        let side = if self.right_half {
            Side::Right
        } else {
            Side::Left
        };
        (point, side)
    }
}

/// The shell alacritty starts when none is configured.
fn default_shell_name() -> String {
    if cfg!(windows) {
        "powershell".into()
    } else {
        std::env::var("SHELL").unwrap_or_else(|_| "sh".into())
    }
}

/// `WSLENV` that also passes our terminal variables into WSL, keeping
/// entries the user already has.
#[cfg_attr(not(windows), allow(dead_code))]
fn wslenv(existing: Option<&str>) -> String {
    let mut entries: Vec<&str> = existing
        .unwrap_or_default()
        .split(':')
        .filter(|e| !e.is_empty())
        .collect();
    for var in ["TERM", "COLORTERM", "TERM_PROGRAM", "TERM_PROGRAM_VERSION"] {
        // Entries may carry flags, like `TERM/u`.
        if !entries.iter().any(|e| e.split('/').next() == Some(var)) {
            entries.push(var);
        }
    }
    entries.join(":")
}

/// Paste payload: with bracketed paste the text is wrapped in markers, with
/// every ESC removed so no end marker can be smuggled in (removing only
/// `ESC [201~` once would turn `ESC [20ESC [201~1~` into a new one); without
/// it, newlines become carriage returns like a typed Enter.
fn encode_paste(text: &str, bracketed: bool) -> Vec<u8> {
    if bracketed {
        let body = text.replace('\x1b', "");
        [b"\x1b[200~", body.as_bytes(), b"\x1b[201~"].concat()
    } else {
        text.replace("\r\n", "\r").replace('\n', "\r").into_bytes()
    }
}

#[cfg(test)]
mod tests {
    use alacritty_terminal::index::Line;

    use super::*;

    #[test]
    fn wslenv_keeps_user_entries() {
        let all = "TERM:COLORTERM:TERM_PROGRAM:TERM_PROGRAM_VERSION";
        assert_eq!(wslenv(None), all);
        assert_eq!(wslenv(Some("")), all);
        assert_eq!(wslenv(Some("GOPATH/l")), format!("GOPATH/l:{all}"));
        assert_eq!(
            wslenv(Some("TERM/u:FOO")),
            "TERM/u:FOO:COLORTERM:TERM_PROGRAM:TERM_PROGRAM_VERSION"
        );
    }

    #[test]
    fn paste_without_brackets_uses_carriage_returns() {
        assert_eq!(encode_paste("a\nb\r\nc", false), b"a\rb\rc");
    }

    #[test]
    fn bracketed_paste_cannot_be_escaped() {
        assert_eq!(
            encode_paste("x\x1b[201~rm -rf ~\n", true),
            b"\x1b[200~x[201~rm -rf ~\n\x1b[201~"
        );
        // Removing the marker would reassemble it from the pieces around it.
        assert_eq!(
            encode_paste("\x1b[20\x1b[201~1~echo pwned\n", true),
            b"\x1b[200~[20[201~1~echo pwned\n\x1b[201~"
        );
    }

    #[test]
    fn grid_point_respects_scrollback() {
        let p = GridPoint {
            column: 3,
            line: 0,
            right_half: true,
        };
        let (point, side) = p.to_grid(5);
        assert_eq!(point, Point::new(Line(-5), Column(3)));
        assert_eq!(side, Side::Right);
    }
}
