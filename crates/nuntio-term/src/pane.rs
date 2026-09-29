use std::borrow::Cow;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use alacritty_terminal::event::WindowSize;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::term::{self, TermMode};
use alacritty_terminal::tty;
use thiserror::Error;

use crate::alacritty::AlacrittyPane;
use crate::osc_cwd::ReportedDir;
use crate::palette::Palette;
use crate::process;
use crate::search::Search;
use crate::snapshot::Snapshot;
use crate::url::Link;

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

/// The emulator core that parses a pane's output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Engine {
    #[default]
    Alacritty,
    #[cfg(feature = "wezterm")]
    Wezterm,
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
    /// The emulator core; it stays with the pane.
    pub engine: Engine,
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
    pub(crate) fn window_size(self) -> WindowSize {
        WindowSize {
            num_lines: self.lines,
            num_cols: self.columns,
            cell_width: self.cell_width,
            cell_height: self.cell_height,
        }
    }

    /// alacritty panics on wide characters in grids narrower than
    /// `MIN_COLUMNS`.
    pub(crate) fn clamped(self) -> Self {
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

/// Where both engines report to the application.
pub(crate) struct EventSink {
    callback: Box<Callback>,
    /// Coalesces wakeups: only one is in flight until the next snapshot.
    wakeup_pending: AtomicBool,
    /// File stem of the program nuntio started, to recognize conhost's
    /// default title.
    shell_name: String,
}

impl EventSink {
    /// New content; sent only if the last wakeup was taken.
    pub(crate) fn wakeup(&self) {
        if !self.wakeup_pending.swap(true, Ordering::AcqRel) {
            (self.callback)(TermEvent::Wakeup);
        }
    }

    pub(crate) fn title(&self, title: String) {
        // ConPTY announces the program's path as the title at startup;
        // that's no title of its own, so the tab keeps its fallback.
        if is_console_default_title(&title, &self.shell_name) {
            (self.callback)(TermEvent::ResetTitle);
        } else {
            (self.callback)(TermEvent::Title(title));
        }
    }

    pub(crate) fn send(&self, event: TermEvent) {
        (self.callback)(event);
    }

    /// The pending wakeup was taken; the next output sends another.
    pub(crate) fn ack(&self) {
        self.wakeup_pending.store(false, Ordering::Release);
    }
}

/// A started shell, before an engine is attached to its PTY.
struct OpenedPty {
    pty: tty::Pty,
    child_pid: Option<u32>,
    via_login: bool,
    runs_command: bool,
    shell_name: String,
    reported_dir: Arc<Mutex<Option<ReportedDir>>>,
}

/// Start the shell in a new PTY. `options.shell` and `options.env` are
/// taken.
fn open_pty(options: &mut SpawnOptions, size: TermSize) -> Result<OpenedPty, SpawnError> {
    let shell_program = options
        .shell
        .as_ref()
        .map(|s| s.program.clone())
        .unwrap_or_else(default_shell_name);
    let shell_name = Path::new(&shell_program)
        .file_stem()
        .map_or(shell_program.clone(), |s| s.to_string_lossy().into_owned());

    let mut env = HashMap::from([
        ("TERM".into(), "xterm-256color".into()),
        ("COLORTERM".into(), "truecolor".into()),
        ("TERM_PROGRAM".into(), "nuntio".into()),
        (
            "TERM_PROGRAM_VERSION".into(),
            env!("CARGO_PKG_VERSION").into(),
        ),
    ]);
    env.extend(std::mem::take(&mut options.env));
    #[cfg(windows)]
    env.insert(
        "WSLENV".into(),
        wslenv(std::env::var("WSLENV").ok().as_deref()),
    );
    // Without a shell, alacritty runs the default one through `login`
    // on macOS as well.
    let via_login = cfg!(target_os = "macos") && (options.shell.is_none() || options.login_shell);
    let runs_command = options.shell.is_some() && !options.login_shell;
    let shell = match options.shell.take() {
        #[cfg(target_os = "macos")]
        Some(shell) if options.login_shell => Some(login_command(&shell)),
        shell => shell,
    };
    let pty_options = tty::Options {
        shell: shell.map(|s| tty::Shell::new(s.program, s.args)),
        working_directory: options.working_directory.clone(),
        drain_on_exit: true,
        env,
        #[cfg(target_os = "windows")]
        escape_args: true,
    };
    let pty = tty::new(&pty_options, size.window_size(), 0).map_err(SpawnError::Pty)?;
    #[cfg(unix)]
    let child_pid = Some(pty.child().id());
    #[cfg(windows)]
    let child_pid = pty.child_watcher().pid().map(|pid| pid.get());

    Ok(OpenedPty {
        pty,
        child_pid,
        via_login,
        runs_command,
        shell_name,
        reported_dir: Arc::new(Mutex::new(None)),
    })
}

/// The engine-specific part of a pane.
enum EnginePane {
    Alacritty(AlacrittyPane),
    #[cfg(feature = "wezterm")]
    Wezterm(crate::wezterm::WeztermPane),
}

/// Forward a call to the pane's engine.
macro_rules! dispatch {
    ($self:ident, $pane:ident => $call:expr) => {
        match &$self.engine {
            EnginePane::Alacritty($pane) => $call,
            #[cfg(feature = "wezterm")]
            EnginePane::Wezterm($pane) => $call,
        }
    };
}

/// One terminal: grid state, the shell's PTY and its IO thread.
pub struct TermHandle {
    engine: EnginePane,
    sink: Arc<EventSink>,
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
        mut options: SpawnOptions,
        size: TermSize,
        callback: impl Fn(TermEvent) + Send + Sync + 'static,
    ) -> Result<Self, SpawnError> {
        let size = size.clamped();
        let opened = open_pty(&mut options, size)?;
        let sink = Arc::new(EventSink {
            callback: Box::new(callback),
            wakeup_pending: AtomicBool::new(false),
            shell_name: opened.shell_name.clone(),
        });
        let reported_dir = opened.reported_dir.clone();
        let engine = match options.engine {
            Engine::Alacritty => EnginePane::Alacritty(AlacrittyPane::spawn(
                opened.pty,
                reported_dir,
                &options,
                size,
                sink.clone(),
            )?),
            #[cfg(feature = "wezterm")]
            Engine::Wezterm => EnginePane::Wezterm(crate::wezterm::WeztermPane::spawn(
                opened.pty,
                reported_dir,
                &options,
                size,
                sink.clone(),
            )?),
        };

        Ok(Self {
            engine,
            sink,
            child_pid: opened.child_pid,
            via_login: opened.via_login,
            shell_pid: OnceLock::new(),
            shell_name: opened.shell_name,
            runs_command: opened.runs_command,
            reported_dir: opened.reported_dir,
        })
    }

    /// The emulator core of this pane.
    pub fn engine(&self) -> Engine {
        match self.engine {
            EnginePane::Alacritty(_) => Engine::Alacritty,
            #[cfg(feature = "wezterm")]
            EnginePane::Wezterm(_) => Engine::Wezterm,
        }
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

    /// Send user input to the shell. Scrolls back to the bottom, like
    /// every terminal does.
    pub fn write(&self, bytes: impl Into<Cow<'static, [u8]>>) {
        let bytes = bytes.into();
        if bytes.is_empty() {
            return;
        }
        dispatch!(self, pane => pane.write(bytes))
    }

    /// Paste text, wrapped in bracketed-paste markers if the app asked for them.
    pub fn paste(&self, text: &str) {
        let bracketed = self.mode().contains(TermMode::BRACKETED_PASTE);
        self.write(encode_paste(text, bracketed));
    }

    /// Scroll the viewport; positive values move up into the scrollback.
    pub fn scroll(&self, lines: i32) {
        dispatch!(self, pane => pane.scroll(lines))
    }

    pub fn scroll_page(&self, up: bool) {
        dispatch!(self, pane => pane.scroll_page(up))
    }

    /// Drop the scrollback, keeping the visible screen.
    pub fn clear_history(&self) {
        dispatch!(self, pane => pane.clear_history())
    }

    pub fn start_selection(&self, kind: SelectionKind, point: GridPoint) {
        dispatch!(self, pane => pane.start_selection(kind, point))
    }

    pub fn update_selection(&self, point: GridPoint) {
        dispatch!(self, pane => pane.update_selection(point))
    }

    pub fn has_selection(&self) -> bool {
        dispatch!(self, pane => pane.has_selection())
    }

    pub fn clear_selection(&self) {
        dispatch!(self, pane => pane.clear_selection())
    }

    pub fn selection_text(&self) -> Option<String> {
        dispatch!(self, pane => pane.selection_text())
    }

    pub fn resize(&self, size: TermSize) {
        let size = size.clamped();
        dispatch!(self, pane => pane.resize(size))
    }

    /// Name of the foreground process, or the shell's name if unknown.
    pub fn process_name(&self) -> String {
        self.shell_pid()
            .and_then(process::foreground_name)
            .unwrap_or_else(|| self.shell_name.clone())
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
        self.shell_pid()
            .and_then(process::working_directory)
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
        dispatch!(self, pane => pane.set_options(options))
    }

    /// Change the colors, e.g. after a theme switch.
    pub fn set_palette(&self, palette: Palette) {
        dispatch!(self, pane => pane.set_palette(palette))
    }

    /// Terminal modes, e.g. for application cursor keys.
    pub fn mode(&self) -> TermMode {
        dispatch!(self, pane => pane.mode())
    }

    /// Copy the visible screen for rendering.
    pub fn snapshot(&self) -> Snapshot {
        dispatch!(self, pane => pane.snapshot())
    }

    /// Answer the color queries programs sent (see
    /// [`TermEvent::ColorQuery`]), with runtime overrides taking precedence
    /// over the palette.
    pub fn answer_color_queries(&self) {
        dispatch!(self, pane => pane.answer_color_queries())
    }

    /// Let the next output send a wakeup without taking a snapshot, for a
    /// pane whose output is awaited rather than drawn.
    pub fn ack_wakeup(&self) {
        self.sink.ack();
    }

    /// Like [`snapshot`](Self::snapshot), with the matches of `search`
    /// highlighted.
    pub fn search_snapshot(&self, search: &mut Search) -> Snapshot {
        dispatch!(self, pane => pane.search_snapshot(search))
    }

    /// Select the next match upwards (older output) or downwards and scroll
    /// to it. Returns whether there is a match.
    pub fn search(&self, search: &mut Search, up: bool) -> bool {
        dispatch!(self, pane => pane.search(search, up))
    }

    /// The link (OSC 8 hyperlink or URL) at a viewport position.
    pub fn link_at(&self, point: GridPoint) -> Option<Link> {
        dispatch!(self, pane => pane.link_at(point))
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

/// A position in the visible grid, as seen by the mouse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GridPoint {
    pub column: usize,
    /// Viewport line, 0 = top of the window.
    pub line: usize,
    /// The pointer is on the right half of the cell.
    pub right_half: bool,
}

/// The shell alacritty starts when none is configured.
fn default_shell_name() -> String {
    if cfg!(windows) {
        "powershell".into()
    } else {
        std::env::var("SHELL").unwrap_or_else(|_| "sh".into())
    }
}

/// Run `shell` as a login shell the way alacritty runs the default one:
/// through `login -flp`, which registers the session, with `exec -a` giving
/// the shell a `-` in front of its name (`-l` keeps `login` from doing
/// that, and from changing to the home directory).
#[cfg(target_os = "macos")]
fn login_command(shell: &Shell) -> Shell {
    let user = std::env::var("USER").unwrap_or_default();
    if user.is_empty() {
        tracing::debug!("USER is not set, starting the shell without login");
        return shell.clone();
    }
    // `login` looks for `.hushlogin` in the current directory only.
    let hush =
        std::env::var_os("HOME").is_some_and(|home| Path::new(&home).join(".hushlogin").exists());
    let flags = if hush { "-qflp" } else { "-flp" };
    Shell {
        program: "/usr/bin/login".into(),
        args: vec![
            flags.into(),
            user,
            "/bin/zsh".into(),
            "-fc".into(),
            exec_as_login(shell),
        ],
    }
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
}
