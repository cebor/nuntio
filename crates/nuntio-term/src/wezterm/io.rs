//! The PTY thread of a wezterm pane, ported from alacritty's
//! `event_loop.rs`: reads the shell's output into the terminal, writes
//! input and replies to the shell, and holds back synchronized updates.

use std::borrow::Cow;
use std::collections::VecDeque;
use std::io::{self, ErrorKind, Read, Write};
use std::num::NonZeroUsize;
use std::sync::mpsc::{self, TryRecvError};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use alacritty_terminal::event::{OnResize, WindowSize};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::tty::{self, ChildEvent, EventedPty, EventedReadWrite};
use polling::{Event, Events, PollMode, Poller};

use super::State;
use super::scanner::{ModeScanner, SyncEvent};
use crate::osc_cwd::{CwdScanner, ReportedDir};
use crate::pane::{EventSink, SpawnError, TermEvent};

// alacritty's `tty` tokens, which it keeps crate-private.
#[cfg(unix)]
const RW: usize = 0;
#[cfg(unix)]
const CHILD: usize = 1;
#[cfg(windows)]
const RW: usize = 2;
#[cfg(windows)]
const CHILD: usize = 1;

/// Bytes read before the terminal must take them.
const READ_BUFFER_SIZE: usize = 0x10_0000;
/// Bytes parsed while holding the terminal lock.
const MAX_LOCKED_READ: usize = u16::MAX as usize;
/// A synchronized update ends at the latest after this long…
const SYNC_TIMEOUT: Duration = Duration::from_millis(150);
/// …or this many bytes.
const SYNC_BUFFER_SIZE: usize = 0x20_0000;

pub(crate) enum Msg {
    Input(Cow<'static, [u8]>),
    Resize(WindowSize),
    Shutdown,
}

/// Sends to the PTY thread and wakes it.
#[derive(Clone)]
pub(crate) struct Sender {
    tx: mpsc::Sender<Msg>,
    poller: Arc<Poller>,
}

impl Sender {
    pub(crate) fn send(&self, msg: Msg) {
        if self.tx.send(msg).is_ok() {
            let _ = self.poller.notify();
        }
    }
}

/// The writer wezterm-term answers queries through (DA, cursor and color
/// reports).
pub(crate) struct PtyWriter(pub(crate) Sender);

impl Write for PtyWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.send(Msg::Input(Cow::Owned(buf.to_vec())));
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// The PTY thread, before it runs.
pub(crate) struct Io {
    pty: tty::Pty,
    poller: Arc<Poller>,
    rx: mpsc::Receiver<Msg>,
    state: Arc<FairMutex<State>>,
    sink: Arc<EventSink>,
    parse: Parse,
    writes: VecDeque<Cow<'static, [u8]>>,
    /// Bytes of the front write already written.
    written: usize,
}

/// Output on its way into the terminal.
struct Parse {
    cwd: CwdScanner,
    reported: Arc<Mutex<Option<ReportedDir>>>,
    modes: ModeScanner,
    /// End of the synchronized update in progress, if any.
    sync_deadline: Option<Instant>,
    /// Output held back during it.
    sync_buffer: Vec<u8>,
}

impl Io {
    /// Prepare the thread for `pty`. The sender comes first: the terminal
    /// writes its replies through it.
    pub(crate) fn channel() -> Result<(Sender, mpsc::Receiver<Msg>), SpawnError> {
        let poller = Arc::new(Poller::new().map_err(SpawnError::EventLoop)?);
        let (tx, rx) = mpsc::channel();
        Ok((Sender { tx, poller }, rx))
    }

    pub(crate) fn new(
        pty: tty::Pty,
        sender: &Sender,
        rx: mpsc::Receiver<Msg>,
        state: Arc<FairMutex<State>>,
        sink: Arc<EventSink>,
        reported: Arc<Mutex<Option<ReportedDir>>>,
    ) -> Self {
        Self {
            pty,
            poller: sender.poller.clone(),
            rx,
            state,
            sink,
            parse: Parse {
                cwd: CwdScanner::default(),
                reported,
                modes: ModeScanner::default(),
                sync_deadline: None,
                sync_buffer: Vec::new(),
            },
            writes: VecDeque::new(),
            written: 0,
        }
    }

    pub(crate) fn spawn(self) -> Result<(), SpawnError> {
        std::thread::Builder::new()
            .name("PTY reader".into())
            .spawn(move || self.run())
            .map(drop)
            .map_err(SpawnError::EventLoop)
    }

    fn run(mut self) {
        let mut buf = vec![0u8; READ_BUFFER_SIZE];
        let mode = PollMode::Level;
        let mut interest = Event::readable(0);
        // SAFETY: the PTY lives in `self` for as long as it is registered;
        // it is deregistered below before being dropped.
        if let Err(err) = unsafe { self.pty.register(&self.poller, interest, mode) } {
            tracing::error!("PTY registration failed: {err}");
            return;
        }
        let mut events = Events::with_capacity(NonZeroUsize::new(1024).expect("non-zero"));

        'event_loop: loop {
            let timeout = self
                .parse
                .sync_deadline
                .map(|deadline| deadline.saturating_duration_since(Instant::now()));
            events.clear();
            if let Err(err) = self.poller.wait(&mut events, timeout) {
                if err.kind() == ErrorKind::Interrupted {
                    continue;
                }
                tracing::error!("PTY polling failed: {err}");
                break;
            }

            if self
                .parse
                .sync_deadline
                .is_some_and(|deadline| Instant::now() >= deadline)
            {
                self.parse.end_sync(&mut self.state.lock());
                self.sink.wakeup();
            }

            if !self.drain_channel() {
                break;
            }

            for event in events.iter() {
                match event.key {
                    CHILD => {
                        if let Some(ChildEvent::Exited(status)) = self.pty.next_child_event() {
                            if let Some(status) = status {
                                tracing::debug!(%status, "shell exited");
                            }
                            // The output the shell left behind.
                            let _ = self.read(&mut buf);
                            self.parse.end_sync(&mut self.state.lock());
                            self.sink.wakeup();
                            self.sink.send(TermEvent::Exit);
                            break 'event_loop;
                        }
                    }
                    RW => {
                        if event.is_interrupt() {
                            // No I/O on a dead PTY.
                            continue;
                        }
                        if event.readable
                            && let Err(err) = self.read(&mut buf)
                        {
                            // On Linux, reading the PTY fails with EIO once
                            // the shell hangs up; the exit event follows.
                            #[cfg(target_os = "linux")]
                            if err.raw_os_error() == Some(EIO) {
                                continue;
                            }
                            tracing::error!("reading the PTY failed: {err}");
                            break 'event_loop;
                        }
                        if event.writable
                            && let Err(err) = self.write()
                        {
                            tracing::error!("writing the PTY failed: {err}");
                            break 'event_loop;
                        }
                    }
                    _ => (),
                }
            }

            let needs_write = !self.writes.is_empty();
            if needs_write != interest.writable {
                interest.writable = needs_write;
                if let Err(err) = self.pty.reregister(&self.poller, interest, mode) {
                    tracing::error!("PTY registration failed: {err}");
                    break;
                }
            }
        }

        let _ = self.pty.deregister(&self.poller);
    }

    /// Take the queued messages. `false` on shutdown.
    fn drain_channel(&mut self) -> bool {
        loop {
            match self.rx.try_recv() {
                Ok(Msg::Input(bytes)) => self.writes.push_back(bytes),
                Ok(Msg::Resize(size)) => self.pty.on_resize(size),
                Ok(Msg::Shutdown) | Err(TryRecvError::Disconnected) => return false,
                Err(TryRecvError::Empty) => return true,
            }
        }
    }

    /// Read what the PTY has and parse it.
    fn read(&mut self, buf: &mut [u8]) -> io::Result<()> {
        let mut unprocessed = 0;
        let mut processed = 0;
        let mut advanced = false;

        // Reserve the next terminal lock for reading.
        let _lease = self.state.lease();
        let mut state = None;

        loop {
            match self.pty.reader().read(&mut buf[unprocessed..]) {
                // Windows and macOS report no more data this way.
                Ok(0) if unprocessed == 0 => break,
                Ok(got) => unprocessed += got,
                Err(err) => match err.kind() {
                    ErrorKind::Interrupted | ErrorKind::WouldBlock => {
                        if unprocessed == 0 {
                            break;
                        }
                    }
                    _ => return Err(err),
                },
            }

            let state = match &mut state {
                Some(state) => state,
                None => state.insert(match self.state.try_lock_unfair() {
                    // Block once the buffer is full.
                    None if unprocessed >= READ_BUFFER_SIZE => self.state.lock_unfair(),
                    None => continue,
                    Some(state) => state,
                }),
            };
            advanced |= self.parse.feed(state, &buf[..unprocessed]);
            processed += unprocessed;
            unprocessed = 0;

            // Don't hold the terminal too long.
            if processed >= MAX_LOCKED_READ {
                break;
            }
        }

        if advanced {
            self.sink.wakeup();
        }
        Ok(())
    }

    /// Write queued input until the PTY would block.
    fn write(&mut self) -> io::Result<()> {
        while let Some(front) = self.writes.front() {
            match self.pty.writer().write(&front[self.written..]) {
                Ok(0) => break,
                Ok(n) => {
                    self.written += n;
                    if self.written == front.len() {
                        self.writes.pop_front();
                        self.written = 0;
                    }
                }
                Err(err) => match err.kind() {
                    ErrorKind::Interrupted | ErrorKind::WouldBlock => break,
                    _ => return Err(err),
                },
            }
        }
        Ok(())
    }
}

#[cfg(target_os = "linux")]
const EIO: i32 = 5;

impl Parse {
    /// Pass output on to the terminal, holding back synchronized updates.
    /// Returns whether the terminal changed.
    fn feed(&mut self, state: &mut State, bytes: &[u8]) -> bool {
        if let Some(dir) = self.cwd.feed(bytes) {
            *self.reported.lock().unwrap_or_else(PoisonError::into_inner) = Some(dir);
        }
        let events = self.modes.feed(bytes);
        state.alt_scroll = self.modes.alt_scroll();

        let mut advanced = false;
        let mut start = 0;
        for event in events {
            let (SyncEvent::Begin(end) | SyncEvent::End(end)) = event;
            let chunk = &bytes[start..end];
            start = end;
            if self.sync_deadline.is_some() {
                self.sync_buffer.extend_from_slice(chunk);
            } else {
                state.term.advance_bytes(chunk);
                advanced = true;
            }
            match event {
                // A new BSU extends an update in progress.
                SyncEvent::Begin(_) => self.sync_deadline = Some(Instant::now() + SYNC_TIMEOUT),
                SyncEvent::End(_) => advanced |= self.end_sync(state),
            }
        }

        let rest = &bytes[start..];
        if self.sync_deadline.is_none() {
            if !rest.is_empty() {
                state.term.advance_bytes(rest);
                advanced = true;
            }
        } else {
            self.sync_buffer.extend_from_slice(rest);
            if self.sync_buffer.len() >= SYNC_BUFFER_SIZE - 1 {
                advanced |= self.end_sync(state);
            }
        }
        advanced
    }

    /// Apply the held-back output. Returns whether there was any.
    fn end_sync(&mut self, state: &mut State) -> bool {
        self.sync_deadline = None;
        if self.sync_buffer.is_empty() {
            return false;
        }
        state.term.advance_bytes(&self.sync_buffer);
        self.sync_buffer.clear();
        true
    }
}
