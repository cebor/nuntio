use std::borrow::Cow;
use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError, RwLock};

use alacritty_terminal::event::{Event, EventListener, OnResize, WindowSize};
use alacritty_terminal::event_loop::{EventLoop, EventLoopSender, Msg};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::{self, Term, viewport_to_point};
use alacritty_terminal::tty::{self, ChildEvent, EventedPty, EventedReadWrite};
use alacritty_terminal::vte::ansi::Rgb;
use polling::{PollMode, Poller};
use thiserror::Error;

use crate::osc_cwd::{CwdScanner, ReportedDir};
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
    /// A program asked for a color (OSC 4/10/11/12); answer with
    /// [`TermHandle::answer_color_queries`].
    ColorQuery,
}

#[derive(Debug, Clone, Default)]
pub struct Shell {
    pub program: String,
    pub args: Vec<String>,
}

/// Terminal options that can also change while a pane runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TermOptions {
    /// Lines of history.
    pub scrollback: usize,
    /// Programs may copy to the clipboard (OSC 52). Reading it is never
    /// allowed.
    pub clipboard_write: bool,
    /// Programs may turn on the kitty keyboard protocol.
    pub kitty_keyboard: bool,
}

#[derive(Debug, Clone)]
pub struct SpawnOptions {
    /// `None` spawns the user's default shell.
    pub shell: Option<Shell>,
    /// `shell` is the user's shell rather than a one-off command. On macOS
    /// it then runs as a login shell through `login(1)`, as the default
    /// shell does and as in Terminal.app: launched from the Dock, nuntio
    /// inherits no PATH from a profile.
    pub login_shell: bool,
    pub working_directory: Option<PathBuf>,
    pub term: TermOptions,
    pub palette: Palette,
    /// More environment variables for the shell; they replace inherited ones.
    pub env: Vec<(String, String)>,
    /// The X11 window id the pane lives in, for programs that read `$WINDOWID`.
    /// `None` on Wayland, macOS and Windows: the variable is then empty.
    pub window_id: Option<u64>,
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

    /// alacritty panics on wide characters in grids narrower than
    /// `MIN_COLUMNS`.
    fn clamped(self) -> Self {
        Self {
            columns: self.columns.max(term::MIN_COLUMNS as u16),
            ..self
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
type ColorFormat = Arc<dyn Fn(Rgb) -> String + Send + Sync>;

/// A reply to the program, in the order its queries came in.
enum Reply {
    Text(String),
    /// Needs the main thread's runtime colors: palette index and formatter.
    Color(usize, ColorFormat),
}

/// Bridges alacritty's events to the application and answers terminal
/// queries (DA, size reports) on the PTY thread, directly while no color
/// query is pending and queued behind it otherwise. Color queries go
/// through the main thread, which can read the runtime overrides (OSC
/// 4/10/11/12): alacritty asks while the PTY thread holds the term lock.
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
    /// File stem of the program nuntio started, to recognize conhost's
    /// default title.
    shell_name: String,
    /// Replies waiting for [`TermHandle::answer_color_queries`]. Once a color
    /// query is pending, every later reply queues behind it, so the program
    /// sees the answers in query order (a DA1 sentinel after OSC 11 must not
    /// overtake it).
    replies: Mutex<Vec<Reply>>,
}

impl ListenerInner {
    fn write(&self, text: String) {
        if let Some(sender) = self.sender.get() {
            let _ = sender.send(Msg::Input(Cow::Owned(text.into_bytes())));
        }
    }

    /// Write `text` now, or queue it behind pending color queries.
    fn reply(&self, text: String) {
        let mut queue = self.replies.lock().unwrap_or_else(PoisonError::into_inner);
        if queue.is_empty() {
            self.write(text);
        } else {
            queue.push(Reply::Text(text));
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
            // ConPTY announces the program's path as the title at startup;
            // that's no title of its own, so the tab keeps its fallback.
            Event::Title(title) if is_console_default_title(&title, &inner.shell_name) => {
                TermEvent::ResetTitle
            }
            Event::Title(title) => TermEvent::Title(title),
            Event::ResetTitle => TermEvent::ResetTitle,
            Event::Bell => TermEvent::Bell,
            // `ChildExit` comes before the remaining output is drained;
            // `Exit` follows once the terminal is done.
            Event::Exit => TermEvent::Exit,
            Event::ChildExit(status) => {
                if status.success() {
                    tracing::debug!(%status, "shell exited");
                } else {
                    tracing::info!(%status, "shell exited");
                }
                return;
            }
            Event::ClipboardStore(_, text) => TermEvent::ClipboardStore(text),
            Event::PtyWrite(text) => return inner.reply(text),
            Event::ColorRequest(index, format) => {
                inner
                    .replies
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(Reply::Color(index, format));
                TermEvent::ColorQuery
            }
            Event::TextAreaSizeRequest(format) => {
                let size = *inner.size.lock().unwrap_or_else(PoisonError::into_inner);
                return inner.reply(format(size));
            }
            // Programs may not read the clipboard (OSC 52 queries), as in
            // xterm and iTerm2 by default: it could leak passwords. There is
            // no reply, so the program sees no answer at all.
            Event::ClipboardLoad(..) => {
                tracing::debug!("denied a clipboard read (OSC 52)");
                return;
            }
            // The cursor is re-read with every snapshot.
            Event::MouseCursorDirty | Event::CursorBlinkingChange => return,
        };
        (inner.callback)(forward);
    }
}

/// What [`TermHandle::foreground_info`] reports.
pub struct ForegroundInfo {
    pub process_name: String,
    pub working_directory: Option<PathBuf>,
}

/// One terminal: grid state, the shell's PTY and its IO thread.
pub struct TermHandle {
    term: Arc<FairMutex<Term<Listener>>>,
    listener: Listener,
    sender: EventLoopSender,
    /// The process nuntio started: the shell, or `login` running it.
    child_pid: Option<u32>,
    /// `child_pid` is `login`; the shell is its child.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    via_login: bool,
    /// The shell's pid, once known.
    shell_pid: OnceLock<u32>,
    shell_name: String,
    /// The pane runs a one-off command, not a shell.
    runs_command: bool,
    /// The directory the shell last reported (OSC 7, OSC 9;9).
    reported_dir: Arc<Mutex<Option<ReportedDir>>>,
}

impl TermHandle {
    /// Spawn a shell. `callback` is invoked from the PTY thread.
    pub fn spawn(
        options: SpawnOptions,
        size: TermSize,
        callback: impl Fn(TermEvent) + Send + Sync + 'static,
    ) -> Result<Self, SpawnError> {
        let size = size.clamped();
        let shell_program = options
            .shell
            .as_ref()
            .map(|s| s.program.clone())
            .unwrap_or_else(default_shell_name);
        let shell_name = Path::new(&shell_program)
            .file_stem()
            .map_or(shell_program.clone(), |s| s.to_string_lossy().into_owned());

        let listener = Listener {
            inner: Arc::new(ListenerInner {
                callback: Box::new(callback),
                sender: OnceLock::new(),
                palette: RwLock::new(options.palette),
                size: Mutex::new(size.window_size()),
                wakeup_pending: AtomicBool::new(false),
                shell_name: shell_name.clone(),
                replies: Mutex::new(Vec::new()),
            }),
        };

        let config = term_config(options.term);
        let term = Arc::new(FairMutex::new(Term::new(config, &size, listener.clone())));

        let mut env = HashMap::from([
            ("TERM".into(), "xterm-256color".into()),
            ("COLORTERM".into(), "truecolor".into()),
            ("TERM_PROGRAM".into(), "nuntio".into()),
            (
                "TERM_PROGRAM_VERSION".into(),
                env!("CARGO_PKG_VERSION").into(),
            ),
        ]);
        // alacritty sets both to the window id it is given and offers no way to
        // unset them; `Options::env` is applied last, so empty values win. Empty
        // is the closest to unset: `[ -n "$WINDOWID" ]` behaves as if it were.
        #[cfg(unix)]
        env.extend([
            (
                "WINDOWID".into(),
                options
                    .window_id
                    .map(|id| id.to_string())
                    .unwrap_or_default(),
            ),
            ("ALACRITTY_WINDOW_ID".into(), String::new()),
        ]);
        env.extend(options.env);
        #[cfg(windows)]
        env.insert(
            "WSLENV".into(),
            wslenv(std::env::var("WSLENV").ok().as_deref()),
        );
        #[cfg(target_os = "macos")]
        let login = match &options.shell {
            Some(shell) if options.login_shell => {
                let user = std::env::var("USER").unwrap_or_default();
                // `login` looks for `.hushlogin` in the current directory only.
                let hush = std::env::var_os("HOME")
                    .is_some_and(|home| Path::new(&home).join(".hushlogin").exists());
                let login = login_command(shell, &user, hush);
                if login.is_none() {
                    tracing::debug!("USER is not set, starting the shell without login");
                }
                login
            }
            _ => None,
        };
        #[cfg(not(target_os = "macos"))]
        let login: Option<Shell> = None;
        // Without a shell, alacritty runs the default one through `login`
        // on macOS as well.
        let via_login = cfg!(target_os = "macos") && (options.shell.is_none() || login.is_some());
        let runs_command = options.shell.is_some() && !options.login_shell;
        let shell = login.or(options.shell);
        let pty_options = tty::Options {
            shell: shell.map(|s| tty::Shell::new(s.program, s.args)),
            working_directory: options.working_directory,
            drain_on_exit: true,
            env,
            #[cfg(target_os = "windows")]
            escape_args: true,
        };
        let pty = tty::new(
            &pty_options,
            size.window_size(),
            options.window_id.unwrap_or(0),
        )
        .map_err(SpawnError::Pty)?;
        #[cfg(unix)]
        let child_pid = Some(pty.child().id());
        #[cfg(windows)]
        let child_pid = pty.child_watcher().pid().map(|pid| pid.get());

        let reported_dir = Arc::new(Mutex::new(None));
        let pty = TeePty {
            pty,
            scanner: CwdScanner::default(),
            reported: reported_dir.clone(),
        };
        let event_loop = EventLoop::new(term.clone(), listener.clone(), pty, true, false)
            .map_err(SpawnError::EventLoop)?;
        let sender = event_loop.channel();
        let _ = listener.inner.sender.set(sender.clone());
        event_loop.spawn();

        Ok(Self {
            term,
            listener,
            sender,
            child_pid,
            via_login,
            shell_pid: OnceLock::new(),
            shell_name,
            runs_command,
            reported_dir,
        })
    }

    /// The shell's pid. Behind `login`, that's `login` itself until it
    /// has started the shell.
    fn shell_pid(&self) -> Option<u32> {
        if let Some(&pid) = self.shell_pid.get() {
            return Some(pid);
        }
        let child = self.child_pid?;
        #[cfg(target_os = "macos")]
        let shell = if self.via_login {
            process::login_child(child)
        } else {
            Some(child)
        };
        #[cfg(not(target_os = "macos"))]
        let shell = Some(child);
        match shell {
            Some(pid) => Some(*self.shell_pid.get_or_init(|| pid)),
            None => Some(child),
        }
    }

    /// Send typed or pasted input to the shell; jumps back to the bottom.
    pub fn write(&self, bytes: impl Into<Cow<'static, [u8]>>) {
        let bytes = bytes.into();
        if bytes.is_empty() {
            return;
        }
        // Typing jumps back to the bottom, like every terminal does.
        self.term.lock().scroll_display(Scroll::Bottom);
        let _ = self.sender.send(Msg::Input(bytes));
    }

    /// Send machine-generated input (reports) without moving the view.
    pub fn send(&self, bytes: impl Into<Cow<'static, [u8]>>) {
        let bytes = bytes.into();
        if bytes.is_empty() {
            return;
        }
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

    pub fn has_selection(&self) -> bool {
        self.term.lock().selection.is_some()
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
        let size = size.clamped();
        *self
            .listener
            .inner
            .size
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = size.window_size();
        self.term.lock().resize(size);
        let _ = self.sender.send(Msg::Resize(size.window_size()));
    }

    /// Name of the foreground process, or the shell's name if unknown.
    pub fn process_name(&self) -> String {
        self.foreground_info(false).process_name
    }

    /// Name and, if wanted, directory of the foreground process from one
    /// lookup of its pid.
    pub fn foreground_info(&self, want_directory: bool) -> ForegroundInfo {
        let found = self
            .shell_pid()
            .map(|pid| process::foreground(pid, want_directory));
        let (name, cwd) = found.map_or((None, None), |f| (f.name, f.cwd));
        ForegroundInfo {
            process_name: name.unwrap_or_else(|| self.shell_name.clone()),
            working_directory: if want_directory {
                self.directory_or_reported(cwd)
            } else {
                None
            },
        }
    }

    /// Whether the shell waits at its prompt, if it can be determined.
    /// `None` for a pane that runs a command: there is no prompt, and the
    /// command itself is the process group leader.
    pub fn foreground_is_shell(&self) -> Option<bool> {
        if self.runs_command {
            return None;
        }
        self.shell_pid().and_then(process::foreground_is_shell)
    }

    /// Working directory of the foreground process, if it can be
    /// determined. Otherwise the one the shell reported, if it exists here.
    pub fn working_directory(&self) -> Option<PathBuf> {
        self.foreground_info(true).working_directory
    }

    fn directory_or_reported(&self, cwd: Option<PathBuf>) -> Option<PathBuf> {
        cwd
            // A deleted directory; `chdir` into it would fail silently.
            .filter(|path| path.is_dir())
            .or_else(|| {
                let path = match self.reported_directory()? {
                    ReportedDir::Posix(path) if cfg!(unix) => path,
                    ReportedDir::Windows(path) if cfg!(windows) && is_local(&path) => path,
                    _ => return None,
                };
                Some(PathBuf::from(path)).filter(|path| path.is_dir())
            })
    }

    /// The directory the shell last reported with OSC 7 or OSC 9;9.
    pub fn reported_directory(&self) -> Option<ReportedDir> {
        self.reported_dir
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Change the terminal options. Less scrollback drops the oldest
    /// lines.
    pub fn set_options(&self, options: TermOptions) {
        self.term.lock().set_options(term_config(options));
    }

    /// Change the colors, e.g. after a theme switch.
    pub fn set_palette(&self, palette: Palette) {
        *self
            .listener
            .inner
            .palette
            .write()
            .unwrap_or_else(PoisonError::into_inner) = palette;
    }

    /// Terminal modes, e.g. for application cursor keys.
    pub fn mode(&self) -> term::TermMode {
        *self.term.lock().mode()
    }

    /// Copy the visible screen for rendering.
    pub fn snapshot(&self) -> Snapshot {
        let mut snapshot = Snapshot::default();
        self.snapshot_into(&mut snapshot);
        snapshot
    }

    /// Like [`snapshot`](Self::snapshot), into `out` and its cell buffer.
    pub fn snapshot_into(&self, out: &mut Snapshot) {
        self.listener
            .inner
            .wakeup_pending
            .store(false, Ordering::Release);
        let term = self.term.lock();
        let palette = self
            .listener
            .inner
            .palette
            .read()
            .unwrap_or_else(PoisonError::into_inner);
        out.refresh(&term, &palette, &[], None);
    }

    /// Answer the color queries programs sent (see
    /// [`TermEvent::ColorQuery`]), with runtime overrides taking precedence
    /// over the palette.
    pub fn answer_color_queries(&self) {
        let inner = &self.listener.inner;
        // Same lock order as the PTY thread: term, then the queue. The writes
        // happen under the queue lock so no reply can slip in between.
        let term = self.term.lock();
        let mut queue = inner.replies.lock().unwrap_or_else(PoisonError::into_inner);
        if queue.is_empty() {
            return;
        }
        let palette = inner.palette.read().unwrap_or_else(PoisonError::into_inner);
        for reply in queue.drain(..) {
            inner.write(match reply {
                Reply::Text(text) => text,
                Reply::Color(index, format) => {
                    format(term.colors()[index].unwrap_or(palette.get(index)))
                }
            });
        }
    }

    /// Let the next output send a wakeup without taking a snapshot, for a
    /// pane whose output is awaited rather than drawn.
    pub fn ack_wakeup(&self) {
        self.listener
            .inner
            .wakeup_pending
            .store(false, Ordering::Release);
    }

    /// Like [`snapshot`](Self::snapshot), with the matches of `search`
    /// highlighted.
    pub fn search_snapshot(&self, search: &mut Search) -> Snapshot {
        let mut snapshot = Snapshot::default();
        self.search_snapshot_into(search, &mut snapshot);
        snapshot
    }

    /// Like [`search_snapshot`](Self::search_snapshot), into `out` and its
    /// cell buffer.
    pub fn search_snapshot_into(&self, search: &mut Search, out: &mut Snapshot) {
        self.listener
            .inner
            .wakeup_pending
            .store(false, Ordering::Release);
        let term = self.term.lock();
        let matches = search::visible_matches(&term, search);
        let palette = self
            .listener
            .inner
            .palette
            .read()
            .unwrap_or_else(PoisonError::into_inner);
        out.refresh(&term, &palette, &matches, search.current_in(&term));
    }

    /// Select the next match upwards (older output) or downwards and scroll
    /// to it. Returns whether there is a match.
    pub fn search(&self, search: &mut Search, up: bool) -> bool {
        search::find(&mut self.term.lock(), search, up, None)
    }

    /// Find the nearest match upwards after the query changed. Scans at most
    /// `MAX_SEARCH_WHILE_TYPING` lines; the next/previous commands still find
    /// matches further away.
    pub fn search_while_typing(&self, search: &mut Search) -> bool {
        search::find(
            &mut self.term.lock(),
            search,
            true,
            Some(search::MAX_SEARCH_WHILE_TYPING),
        )
    }

    /// The link (OSC 8 hyperlink or URL) at a viewport position.
    pub fn link_at(&self, point: GridPoint) -> Option<Link> {
        let term = self.term.lock();
        let (point, _) = point.to_grid(term.grid().display_offset());
        url::link_at(&term, point)
    }
}

/// The PTY, with its output scanned for the directory the shell reports.
struct TeePty {
    pty: tty::Pty,
    scanner: CwdScanner,
    reported: Arc<Mutex<Option<ReportedDir>>>,
}

impl io::Read for TeePty {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.pty.reader().read(buf)?;
        if let Some(dir) = self.scanner.feed(&buf[..n]) {
            *self.reported.lock().unwrap_or_else(PoisonError::into_inner) = Some(dir);
        }
        Ok(n)
    }
}

impl EventedReadWrite for TeePty {
    type Reader = Self;
    type Writer = <tty::Pty as EventedReadWrite>::Writer;

    unsafe fn register(
        &mut self,
        poll: &Arc<Poller>,
        interest: polling::Event,
        mode: PollMode,
    ) -> io::Result<()> {
        // SAFETY: the PTY lives in `self`, as long as its registration.
        unsafe { self.pty.register(poll, interest, mode) }
    }

    fn reregister(
        &mut self,
        poll: &Arc<Poller>,
        interest: polling::Event,
        mode: PollMode,
    ) -> io::Result<()> {
        self.pty.reregister(poll, interest, mode)
    }

    fn deregister(&mut self, poll: &Arc<Poller>) -> io::Result<()> {
        self.pty.deregister(poll)
    }

    fn reader(&mut self) -> &mut Self {
        self
    }

    fn writer(&mut self) -> &mut Self::Writer {
        self.pty.writer()
    }
}

impl EventedPty for TeePty {
    fn next_child_event(&mut self) -> Option<ChildEvent> {
        self.pty.next_child_event()
    }
}

impl OnResize for TeePty {
    fn on_resize(&mut self, window_size: WindowSize) {
        self.pty.on_resize(window_size);
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

/// alacritty's terminal options.
fn term_config(options: TermOptions) -> term::Config {
    term::Config {
        scrolling_history: options.scrollback,
        osc52: if options.clipboard_write {
            term::Osc52::OnlyCopy
        } else {
            term::Osc52::Disabled
        },
        kitty_keyboard: options.kitty_keyboard,
        ..Default::default()
    }
}

/// The shell alacritty starts when none is configured: `$SHELL`, else the
/// user's login shell from the password database.
pub fn default_shell_name() -> String {
    if cfg!(windows) {
        return "powershell".into();
    }
    if let Ok(shell) = std::env::var("SHELL") {
        return shell;
    }
    #[cfg(unix)]
    if let Some(shell) = passwd_shell() {
        return shell;
    }
    "sh".into()
}

/// The current user's login shell from the password database.
#[cfg(unix)]
fn passwd_shell() -> Option<String> {
    use std::ffi::CStr;
    use std::mem::MaybeUninit;

    let mut entry = MaybeUninit::<libc::passwd>::uninit();
    let mut buffer = [0 as libc::c_char; 1024];
    let mut result = std::ptr::null_mut();
    // SAFETY: every pointer is valid for the call, `buffer.len()` is its size.
    let status = unsafe {
        libc::getpwuid_r(
            libc::getuid(),
            entry.as_mut_ptr(),
            buffer.as_mut_ptr(),
            buffer.len(),
            &mut result,
        )
    };
    if status != 0 || result.is_null() {
        return None;
    }
    // SAFETY: `getpwuid_r` filled `entry`, whose strings live in `buffer`.
    let shell = unsafe { entry.assume_init().pw_shell };
    if shell.is_null() {
        return None;
    }
    // SAFETY: a NUL-terminated string in `buffer`.
    let shell = unsafe { CStr::from_ptr(shell) }.to_string_lossy();
    (!shell.is_empty()).then(|| shell.into_owned())
}

/// Run `shell` as a login shell the way alacritty runs the default one:
/// through `login -flp`, which registers the session, with `exec -a` giving
/// the shell a `-` in front of its name (`-l` keeps `login` from doing
/// that, and from changing to the home directory).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn login_command(shell: &Shell, user: &str, hush: bool) -> Option<Shell> {
    if user.is_empty() {
        return None;
    }
    let flags = if hush { "-qflp" } else { "-flp" };
    Some(Shell {
        program: "/usr/bin/login".into(),
        args: vec![
            flags.into(),
            user.into(),
            "/bin/zsh".into(),
            "-fc".into(),
            exec_as_login(shell),
        ],
    })
}

/// The zsh command that starts `shell` with `-<name>` as `argv[0]`.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn exec_as_login(shell: &Shell) -> String {
    let name = Path::new(&shell.program)
        .file_name()
        .map_or(shell.program.clone(), |n| n.to_string_lossy().into_owned());
    let mut command = format!(
        "exec -a {} {}",
        sh_quote(&format!("-{name}")),
        sh_quote(&shell.program)
    );
    for arg in &shell.args {
        command.push(' ');
        command.push_str(&sh_quote(arg));
    }
    command
}

/// `s` as one single-quoted shell word.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
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

/// Whether `title` is conhost's default console title: the absolute path of
/// the program nuntio started (`C:\WINDOWS\system32\wsl.exe`), which ConPTY
/// sends at startup. Parsed by hand so it behaves the same on every OS.
fn is_console_default_title(title: &str, program: &str) -> bool {
    let bytes = title.as_bytes();
    let absolute = title.starts_with(r"\\")
        || (bytes.len() > 2 && bytes[0].is_ascii_alphabetic() && &bytes[1..3] == br":\");
    let Some((_, file)) = title.rsplit_once('\\') else {
        return false;
    };
    let stem = match file.len().checked_sub(4) {
        Some(dot) if file.is_char_boundary(dot) && file[dot..].eq_ignore_ascii_case(".exe") => {
            &file[..dot]
        }
        _ => file,
    };
    absolute && !program.is_empty() && stem.eq_ignore_ascii_case(program)
}

/// Whether a Windows path the shell reported is on this machine. Any
/// program's output can report one, and merely looking at a network share
/// (`\\host\share`) sends the user's credentials to that host. Only drive
/// paths (`C:\`, `c:/`) and shares of WSL distributions count as local.
fn is_local(path: &str) -> bool {
    let bytes = path.as_bytes();
    let separator = |i: usize| matches!(bytes.get(i), Some(b'\\' | b'/'));
    if bytes.first().is_some_and(u8::is_ascii_alphabetic) && bytes.get(1) == Some(&b':') {
        return separator(2);
    }
    if !(separator(0) && separator(1)) {
        return false;
    }
    let host = path[2..].split(['\\', '/']).next().unwrap_or_default();
    host.eq_ignore_ascii_case("wsl.localhost") || host.eq_ignore_ascii_case("wsl$")
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
    fn conhost_default_title_is_recognized() {
        assert!(is_console_default_title(
            r"C:\WINDOWS\system32\wsl.exe",
            "wsl"
        ));
        assert!(is_console_default_title(
            r"C:\Program Files\PowerShell\7\pwsh.exe",
            "pwsh"
        ));
        assert!(is_console_default_title(r"\\server\share\Tool.EXE", "tool"));
        assert!(!is_console_default_title("felix@box: ~", "wsl"));
        assert!(!is_console_default_title(r"C:\Users\felix", "wsl"));
        assert!(!is_console_default_title("wsl.exe", "wsl"));
        assert!(!is_console_default_title(
            r"C:\WINDOWS\system32\cmd.exe",
            "wsl"
        ));
        assert!(!is_console_default_title("/usr/bin/wsl", "wsl"));
    }

    #[test]
    fn login_shells_keep_their_arguments_quoted() {
        let shell = Shell {
            program: "/opt/homebrew/bin/fish".into(),
            args: vec!["--init-command".into(), "echo 'hi' $HOME".into()],
        };
        assert_eq!(
            exec_as_login(&shell),
            r#"exec -a '-fish' '/opt/homebrew/bin/fish' '--init-command' 'echo '\''hi'\'' $HOME'"#
        );
    }

    #[test]
    fn login_is_only_used_with_a_user() {
        let zsh = Shell {
            program: "/bin/zsh".into(),
            args: vec![],
        };
        assert!(login_command(&zsh, "", false).is_none());
        let l = login_command(&zsh, "felix", false).unwrap();
        assert_eq!(l.program, "/usr/bin/login");
        assert_eq!(
            l.args,
            [
                "-flp",
                "felix",
                "/bin/zsh",
                "-fc",
                "exec -a '-zsh' '/bin/zsh'"
            ]
        );
        assert_eq!(login_command(&zsh, "felix", true).unwrap().args[0], "-qflp");
    }

    #[test]
    fn only_local_windows_directories_are_used() {
        assert!(is_local(r"C:\Users\me"));
        assert!(is_local(r"\\wsl.localhost\Ubuntu\home\me"));
        assert!(is_local(r"\\WSL$\Debian"));
        assert!(!is_local(r"\\evil\share"));
        assert!(!is_local("//evil/share"));
        assert!(!is_local(r"\\?\UNC\evil\share"));
        assert!(!is_local(r"\\wsl.localhost.evil.com\x"));
        assert!(is_local("C:/Users"));
        assert!(!is_local(r"\??\UNC\evil\share"));
        assert!(!is_local(r"\??\GLOBALROOT\Device\Mup\evil\share"));
        assert!(!is_local(r"\evil"));
        assert!(!is_local("C:foo"));
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
