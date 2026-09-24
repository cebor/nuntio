use std::borrow::Cow;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::event_loop::{EventLoop, EventLoopSender, Msg};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::{self, Term};
use alacritty_terminal::tty;
use thiserror::Error;

use crate::palette::Palette;
use crate::snapshot::Snapshot;

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
    palette: Palette,
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
                return inner.write(format(inner.palette.get(index)));
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
                palette: Palette::default(),
                size: Mutex::new(size.window_size()),
                wakeup_pending: AtomicBool::new(false),
            }),
        };

        let config = term::Config {
            scrolling_history: options.scrollback,
            ..Default::default()
        };
        let term = Arc::new(FairMutex::new(Term::new(config, &size, listener.clone())));

        let pty_options = tty::Options {
            shell: options.shell.map(|s| tty::Shell::new(s.program, s.args)),
            working_directory: options.working_directory,
            drain_on_exit: true,
            env: HashMap::from([
                ("TERM".into(), "xterm-256color".into()),
                ("COLORTERM".into(), "truecolor".into()),
                ("TERM_PROGRAM".into(), "nuntio".into()),
                (
                    "TERM_PROGRAM_VERSION".into(),
                    env!("CARGO_PKG_VERSION").into(),
                ),
            ]),
            #[cfg(target_os = "windows")]
            escape_args: true,
        };
        let pty = tty::new(&pty_options, size.window_size(), 0).map_err(SpawnError::Pty)?;

        let event_loop = EventLoop::new(term.clone(), listener.clone(), pty, true, false)
            .map_err(SpawnError::EventLoop)?;
        let sender = event_loop.channel();
        let _ = listener.inner.sender.set(sender.clone());
        event_loop.spawn();

        Ok(Self {
            term,
            listener,
            sender,
        })
    }

    /// Send user input to the shell.
    pub fn write(&self, bytes: impl Into<Cow<'static, [u8]>>) {
        let bytes = bytes.into();
        if bytes.is_empty() {
            return;
        }
        // Typing jumps back to the bottom, like every terminal does.
        self.term
            .lock()
            .scroll_display(alacritty_terminal::grid::Scroll::Bottom);
        let _ = self.sender.send(Msg::Input(bytes));
    }

    pub fn resize(&self, size: TermSize) {
        *self.listener.inner.size.lock().unwrap() = size.window_size();
        self.term.lock().resize(size);
        let _ = self.sender.send(Msg::Resize(size.window_size()));
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
        Snapshot::capture(&term, &self.listener.inner.palette)
    }
}

impl Drop for TermHandle {
    fn drop(&mut self) {
        let _ = self.sender.send(Msg::Shutdown);
    }
}
