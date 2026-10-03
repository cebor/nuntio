//! The PTY thread of a pane: reads the program's output into the terminal
//! and writes input and replies back. nuntio runs its own loop instead of
//! alacritty's so it sees the output stream before the parser does.

use std::borrow::Cow;
use std::collections::VecDeque;
use std::io::{self, ErrorKind};
use std::num::NonZeroUsize;
use std::sync::atomic::AtomicU32;
#[cfg(windows)]
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::{Term, TermMode};
#[cfg(windows)]
use alacritty_terminal::tty;
use alacritty_terminal::vte::ansi::Processor;
use polling::{Events, PollMode, Poller};

use crate::backend::{Backend, BackendEvent};
use crate::image::{self, CellGeometry, ImageKey, ImageStore, TermImage};
use crate::image_scan::{ImageRequest, ImageScanner, ScanEvent};
use crate::osc_cwd::{CwdScanner, ReportedDir};
use crate::pane::Listener;

/// Size of the read buffer.
const READ_BUFFER_SIZE: usize = 0x10_0000;
/// Bytes parsed per read pass before the term lock is given up again, so
/// the main thread can draw during floods of output.
const MAX_LOCKED_READ: usize = u16::MAX as usize;
/// How long the PTY thread waits for messages between checks whether an
/// image is decoded.
const DECODE_POLL: Duration = Duration::from_millis(5);

/// A message to the PTY thread.
pub(crate) enum Msg {
    /// Bytes for the program.
    Input(Cow<'static, [u8]>),
    Resize(WindowSize),
    Shutdown,
}

/// Sends [`Msg`]s to the PTY thread and wakes it up.
#[derive(Clone)]
pub(crate) struct LoopSender {
    tx: mpsc::Sender<Msg>,
    poller: Arc<Poller>,
}

impl LoopSender {
    /// Send `msg`; does nothing once the thread is gone.
    pub(crate) fn send(&self, msg: Msg) {
        if self.tx.send(msg).is_ok() {
            let _ = self.poller.notify();
        }
    }
}

pub(crate) struct IoLoop {
    poll: Arc<Poller>,
    pty: Backend,
    /// The pid of the process behind `pty`, shared with the pane; 0 if unknown.
    child_pid: Arc<AtomicU32>,
    /// For a WSL pane: the same shell through ConPTY, if nuntio-wsl fails.
    #[cfg(windows)]
    fallback: Option<tty::Options>,
    rx: mpsc::Receiver<Msg>,
    tx: mpsc::Sender<Msg>,
    term: Arc<FairMutex<Term<Listener>>>,
    listener: Listener,
    parser: Processor,
    cwd: CwdScanner,
    reported_dir: Arc<Mutex<Option<ReportedDir>>>,
    scanner: ImageScanner,
    images: Arc<Mutex<ImageStore>>,
    /// Input waiting for the PTY to become writable.
    write_list: VecDeque<Cow<'static, [u8]>>,
    /// Bytes of the front of `write_list` already written.
    written: usize,
    /// A [`Msg::Shutdown`] arrived while an image was decoding.
    shutdown_requested: bool,
}

impl IoLoop {
    pub(crate) fn new(
        pty: Backend,
        #[cfg(windows)] fallback: Option<tty::Options>,
        term: Arc<FairMutex<Term<Listener>>>,
        listener: Listener,
        reported_dir: Arc<Mutex<Option<ReportedDir>>>,
        images: Arc<Mutex<ImageStore>>,
    ) -> io::Result<Self> {
        let (tx, rx) = mpsc::channel();
        let child_pid = Arc::new(AtomicU32::new(pty.pid().unwrap_or(0)));
        Ok(Self {
            poll: Arc::new(Poller::new()?),
            pty,
            child_pid,
            #[cfg(windows)]
            fallback,
            rx,
            tx,
            term,
            listener,
            parser: Processor::new(),
            cwd: CwdScanner::default(),
            reported_dir,
            scanner: ImageScanner::default(),
            images,
            write_list: VecDeque::new(),
            written: 0,
            shutdown_requested: false,
        })
    }

    pub(crate) fn sender(&self) -> LoopSender {
        LoopSender {
            tx: self.tx.clone(),
            poller: self.poll.clone(),
        }
    }

    /// The pid of the process behind the pane, which changes when a WSL
    /// pane falls back to ConPTY; 0 if unknown.
    pub(crate) fn child_pid(&self) -> Arc<AtomicU32> {
        self.child_pid.clone()
    }

    pub(crate) fn spawn(self) -> JoinHandle<()> {
        alacritty_terminal::thread::spawn_named("PTY reader", move || self.run())
    }

    fn run(mut self) {
        let mut buf = vec![0u8; READ_BUFFER_SIZE];
        let poll = self.poll.clone();
        let mut interest = polling::Event::readable(0);
        // SAFETY: the PTY lives in `self` as long as its registration; it is
        // deregistered before the loop returns.
        if let Err(err) = unsafe { self.pty.register(&poll, interest, PollMode::Level) } {
            tracing::error!(%err, "registering the PTY failed");
            return;
        }
        let mut events = Events::with_capacity(NonZeroUsize::new(1024).expect("non-zero"));
        // A read pass that stopped at `MAX_LOCKED_READ` may have left output
        // behind. On Windows no further event announces it (alacritty's pipe
        // reader only wakes the poller once it was found empty), so the next
        // pass reads without waiting.
        let mut more_output = false;

        loop {
            // Wake up when a synchronized update (DEC 2026) times out.
            let timeout = if more_output {
                Some(Duration::ZERO)
            } else {
                self.parser
                    .sync_timeout()
                    .sync_timeout()
                    .map(|t| t.saturating_duration_since(Instant::now()))
            };
            events.clear();
            if let Err(err) = poll.wait(&mut events, timeout) {
                if err.kind() == ErrorKind::Interrupted {
                    continue;
                }
                tracing::error!(%err, "polling the PTY failed");
                break;
            }

            let mut received = false;
            let mut shutdown = false;
            while let Ok(msg) = self.rx.try_recv() {
                received = true;
                match msg {
                    // An empty write would keep the PTY registered as writable.
                    Msg::Input(bytes) if bytes.is_empty() => {}
                    Msg::Input(bytes) => self.write_list.push_back(bytes),
                    Msg::Resize(size) => self.pty.resize(size),
                    Msg::Shutdown => {
                        shutdown = true;
                        break;
                    }
                }
            }
            if shutdown {
                break;
            }

            if events.is_empty() && !received && !more_output {
                self.parser.stop_sync(&mut *self.term.lock());
                self.listener.send_event(Event::Wakeup);
                continue;
            }

            // alacritty's event keys are private; the child event check
            // doesn't block, so it runs on every pass.
            let (mut readable, mut writable) = (more_output, false);
            for event in events.iter().filter(|e| !e.is_interrupt()) {
                readable |= event.readable;
                writable |= event.writable;
            }

            match self.pty.next_event() {
                Some(BackendEvent::Exited(status)) => {
                    if let Some(status) = status {
                        self.listener.send_event(Event::ChildExit(status));
                    }
                    while let Ok(true) = self.pty_read(&mut buf) {}
                    self.term.lock().exit();
                    self.listener.send_event(Event::Wakeup);
                    break;
                }
                #[cfg(windows)]
                Some(BackendEvent::HelperFailed(status)) => {
                    while let Ok(true) = self.pty_read(&mut buf) {}
                    tracing::warn!(?status, "nuntio-wsl did not start, falling back to ConPTY");
                    if self.fall_back(&poll, interest) {
                        more_output = false;
                        continue;
                    }
                    self.term.lock().exit();
                    self.listener.send_event(Event::Wakeup);
                    break;
                }
                None => {}
            }

            more_output = false;
            if readable {
                let result = self.pty_read(&mut buf);
                if self.shutdown_requested {
                    break;
                }
                match result {
                    Ok(capped) => more_output = capped,
                    Err(err) => {
                        // On Linux, reading the master side fails with EIO once
                        // the client side hangs up; the exit event follows.
                        #[cfg(target_os = "linux")]
                        if err.raw_os_error() == Some(libc::EIO) {
                            continue;
                        }
                        tracing::error!(%err, "reading from the PTY failed");
                        break;
                    }
                }
            }

            if writable && let Err(err) = self.pty_write() {
                tracing::error!(%err, "writing to the PTY failed");
                break;
            }

            let needs_write = !self.write_list.is_empty();
            if needs_write != interest.writable {
                interest.writable = needs_write;
                if let Err(err) = self.pty.reregister(&poll, interest, PollMode::Level) {
                    tracing::error!(%err, "re-registering the PTY failed");
                    break;
                }
            }
        }

        let _ = self.pty.deregister(&poll);
    }

    /// Read and parse the available output, holding the term lock for at
    /// most [`MAX_LOCKED_READ`] bytes. Inline images and cell size queries
    /// are taken out of the stream and handled here, in stream order.
    /// Returns whether the pass stopped at the limit with output possibly left.
    fn pty_read(&mut self, buf: &mut [u8]) -> io::Result<bool> {
        let term = self.term.clone();
        // Reserve the next term lock for the PTY thread.
        let mut lease = Some(term.lease());
        let mut guard = None;
        let mut processed = 0;
        let mut capped = false;
        loop {
            let n = match self.pty.read(buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(err)
                    if matches!(err.kind(), ErrorKind::WouldBlock | ErrorKind::Interrupted) =>
                {
                    break;
                }
                Err(err) => return Err(err),
            };
            if let Some(dir) = self.cwd.feed(&buf[..n]) {
                *lock(&self.reported_dir) = Some(dir);
            }
            let parser = &mut self.parser;
            let listener = &self.listener;
            let images = &self.images;
            let mut input = PtyInput {
                pty: &mut self.pty,
                rx: &self.rx,
                write_list: &mut self.write_list,
                written: &mut self.written,
                shutdown_requested: &mut self.shutdown_requested,
            };
            self.scanner.feed(&buf[..n], &mut |event| match event {
                ScanEvent::Text(bytes) => {
                    parser.advance(
                        &mut **guard.get_or_insert_with(|| term.lock_unfair()),
                        bytes,
                    );
                }
                ScanEvent::CellSizeQuery => {
                    let term = &mut **guard.get_or_insert_with(|| term.lock_unfair());
                    with_flushed_sync(parser, term, |_| {
                        let size = listener.window_size();
                        listener.reply(format!("\x1b[6;{};{}t", size.cell_height, size.cell_width));
                    });
                }
                ScanEvent::Image(request) => {
                    let args = image::parse_args(&request.args);
                    if !args.inline {
                        tracing::debug!("inline image without inline=1 ignored");
                        return;
                    }
                    let geometry = CellGeometry::from(listener.window_size());
                    // Hashing the payload and decoding take a while: let the
                    // main thread draw meanwhile.
                    drop(guard.take());
                    drop(lease.take());
                    let key = ImageKey::new(&request, geometry);
                    let cached = {
                        let store = lock(images);
                        store
                            .cached(key)
                            .and_then(|id| Some((id, store.get(id)?.clone())))
                    };
                    let (id, image, inserted) = match cached {
                        Some((id, image)) => {
                            lease = Some(term.lease());
                            (id, image, false)
                        }
                        None => {
                            let decoded = decode_serving(&mut input, &request, &args, geometry);
                            lease = Some(term.lease());
                            let Some(decoded) = decoded else {
                                return;
                            };
                            let mut store = lock(images);
                            let id = store.insert(key, decoded);
                            let image = store.get(id).expect("just inserted").clone();
                            (id, image, true)
                        }
                    };
                    let term = &mut **guard.get_or_insert_with(|| term.lock_unfair());
                    // The screen is known only once the output buffered in a
                    // synchronized update is applied.
                    let mut alt = false;
                    with_flushed_sync(parser, term, |term| {
                        image::place(term, id, &image);
                        alt = term.mode().contains(TermMode::ALT_SCREEN);
                    });
                    let mut store = lock(images);
                    store.placed(id, alt);
                    // Memory only grows with new images; cache hits don't
                    // need to scan the grid.
                    if inserted {
                        store.evict(term);
                    }
                }
            });
            processed += n;
            if processed >= MAX_LOCKED_READ {
                capped = true;
                break;
            }
        }
        drop(guard);
        drop(lease);
        // Redraw, but not in the middle of a synchronized update: its end,
        // or its timeout in `run`, wakes the main thread.
        if processed > 0 && self.parser.sync_timeout().sync_timeout().is_none() {
            self.listener.send_event(Event::Wakeup);
        }
        Ok(capped)
    }

    /// Write queued input until the PTY would block.
    fn pty_write(&mut self) -> io::Result<()> {
        write_pending(&mut self.pty, &mut self.write_list, &mut self.written)
    }

    /// Replace a failed nuntio-wsl with the same shell through ConPTY.
    /// Returns false if there is no fallback or it didn't start either.
    #[cfg(windows)]
    fn fall_back(&mut self, poll: &Arc<Poller>, interest: polling::Event) -> bool {
        let _ = self.pty.deregister(poll);
        let Some(options) = self.fallback.take() else {
            return false;
        };
        let pty = match tty::new(&options, self.listener.window_size(), 0) {
            Ok(pty) => pty,
            Err(err) => {
                tracing::error!(%err, "starting the ConPTY fallback failed");
                return false;
            }
        };
        self.pty = Backend::Pty(pty);
        // SAFETY: as for the first registration in `run`.
        if let Err(err) = unsafe { self.pty.register(poll, interest, PollMode::Level) } {
            tracing::error!(%err, "registering the ConPTY fallback failed");
            return false;
        }
        self.child_pid
            .store(self.pty.pid().unwrap_or(0), Ordering::Relaxed);
        // Not written into the grid: ConPTY may clear the screen at startup.
        self.listener.helper_failed();
        true
    }
}

/// The parts of the loop that keep working while an image decodes.
struct PtyInput<'a> {
    pty: &'a mut Backend,
    rx: &'a mpsc::Receiver<Msg>,
    write_list: &'a mut VecDeque<Cow<'static, [u8]>>,
    written: &'a mut usize,
    shutdown_requested: &'a mut bool,
}

/// Write the queued input to the PTY until it would block.
fn write_pending(
    pty: &mut Backend,
    write_list: &mut VecDeque<Cow<'static, [u8]>>,
    written: &mut usize,
) -> io::Result<()> {
    while let Some(front) = write_list.front() {
        match pty.write(&front[*written..]) {
            Ok(0) => break,
            Ok(n) => {
                *written += n;
                if *written == front.len() {
                    write_list.pop_front();
                    *written = 0;
                }
            }
            Err(err) if matches!(err.kind(), ErrorKind::WouldBlock | ErrorKind::Interrupted) => {
                break;
            }
            Err(err) => return Err(err),
        }
    }
    Ok(())
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn decode(
    request: &ImageRequest,
    args: &image::ImageArgs,
    geometry: CellGeometry,
) -> Option<TermImage> {
    image::decode(request, args, geometry)
        .inspect_err(|err| tracing::debug!(%err, "inline image ignored"))
        .ok()
}

/// [`decode`] on a worker thread. This thread meanwhile takes the messages
/// for the PTY, so keystrokes and resizes aren't held up by a large image.
fn decode_serving(
    input: &mut PtyInput<'_>,
    request: &ImageRequest,
    args: &image::ImageArgs,
    geometry: CellGeometry,
) -> Option<TermImage> {
    std::thread::scope(|scope| {
        let worker = scope.spawn(|| decode(request, args, geometry));
        while !worker.is_finished() {
            match input.rx.recv_timeout(DECODE_POLL) {
                Ok(msg) => {
                    match msg {
                        Msg::Input(bytes) if bytes.is_empty() => {}
                        Msg::Input(bytes) => input.write_list.push_back(bytes),
                        Msg::Resize(size) => input.pty.resize(size),
                        Msg::Shutdown => *input.shutdown_requested = true,
                    }
                    // A failure shows again when the loop writes next.
                    let _ = write_pending(input.pty, input.write_list, input.written);
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => std::thread::sleep(DECODE_POLL),
            }
        }
        // A panic in the worker counts as a failed decode.
        worker.join().ok().flatten()
    })
}

/// Run `action` with the output buffered in a synchronized update (DEC
/// 2026) applied first, so images and replies stay in stream order. The
/// update continues afterwards.
fn with_flushed_sync(
    parser: &mut Processor,
    term: &mut Term<Listener>,
    action: impl FnOnce(&mut Term<Listener>),
) {
    if parser.sync_timeout().sync_timeout().is_some() {
        parser.stop_sync(term);
        action(term);
        parser.advance(term, b"\x1b[?2026h");
    } else {
        action(term);
    }
}
