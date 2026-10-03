//! The PTY thread of a pane: reads the program's output into the terminal
//! and writes input and replies back. nuntio runs its own loop instead of
//! alacritty's so it sees the output stream before the parser does.

use std::borrow::Cow;
use std::collections::VecDeque;
use std::io::{self, ErrorKind, Read, Write};
use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, mpsc};
use std::thread::JoinHandle;
use std::time::Instant;

use alacritty_terminal::event::{Event, EventListener, OnResize, WindowSize};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::Term;
use alacritty_terminal::tty::{self, ChildEvent, EventedPty, EventedReadWrite};
use alacritty_terminal::vte::ansi::Processor;
use polling::{Events, PollMode, Poller};

use crate::image::{self, CellGeometry, ImageKey, ImageStore, TermImage};
use crate::image_scan::{ImageRequest, ImageScanner, ScanEvent};
use crate::osc_cwd::{CwdScanner, ReportedDir};
use crate::pane::Listener;

/// Size of the read buffer.
const READ_BUFFER_SIZE: usize = 0x10_0000;
/// Bytes parsed per read pass before the term lock is given up again, so
/// the main thread can draw during floods of output.
const MAX_LOCKED_READ: usize = u16::MAX as usize;

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
    pty: tty::Pty,
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
}

impl IoLoop {
    pub(crate) fn new(
        pty: tty::Pty,
        term: Arc<FairMutex<Term<Listener>>>,
        listener: Listener,
        reported_dir: Arc<Mutex<Option<ReportedDir>>>,
        images: Arc<Mutex<ImageStore>>,
    ) -> io::Result<Self> {
        let (tx, rx) = mpsc::channel();
        Ok(Self {
            poll: Arc::new(Poller::new()?),
            pty,
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
        })
    }

    pub(crate) fn sender(&self) -> LoopSender {
        LoopSender {
            tx: self.tx.clone(),
            poller: self.poll.clone(),
        }
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

        loop {
            // Wake up when a synchronized update (DEC 2026) times out.
            let timeout = self
                .parser
                .sync_timeout()
                .sync_timeout()
                .map(|t| t.saturating_duration_since(Instant::now()));
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
                    Msg::Resize(size) => self.pty.on_resize(size),
                    Msg::Shutdown => {
                        shutdown = true;
                        break;
                    }
                }
            }
            if shutdown {
                break;
            }

            if events.is_empty() && !received {
                self.parser.stop_sync(&mut *self.term.lock());
                self.listener.send_event(Event::Wakeup);
                continue;
            }

            // alacritty's event keys are private; the child event check
            // doesn't block, so it runs on every pass.
            let (mut readable, mut writable) = (false, false);
            for event in events.iter().filter(|e| !e.is_interrupt()) {
                readable |= event.readable;
                writable |= event.writable;
            }

            if let Some(ChildEvent::Exited(status)) = self.pty.next_child_event() {
                if let Some(status) = status {
                    self.listener.send_event(Event::ChildExit(status));
                }
                let _ = self.pty_read(&mut buf);
                self.term.lock().exit();
                self.listener.send_event(Event::Wakeup);
                break;
            }

            if readable && let Err(err) = self.pty_read(&mut buf) {
                // On Linux, reading the master side fails with EIO once the
                // client side hangs up; the exit event follows.
                #[cfg(target_os = "linux")]
                if err.raw_os_error() == Some(libc::EIO) {
                    continue;
                }
                tracing::error!(%err, "reading from the PTY failed");
                break;
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
    fn pty_read(&mut self, buf: &mut [u8]) -> io::Result<()> {
        let term = self.term.clone();
        // Reserve the next term lock for the PTY thread.
        let mut lease = Some(term.lease());
        let mut guard = None;
        let mut processed = 0;
        loop {
            let n = match self.pty.reader().read(buf) {
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
                    let key = ImageKey::new(&request, geometry);
                    let cached = {
                        let store = lock(images);
                        store
                            .cached(key)
                            .and_then(|id| Some((id, store.get(id)?.clone())))
                    };
                    let (id, image) = match cached {
                        Some(hit) => hit,
                        None => {
                            // Let the main thread draw while decoding.
                            drop(guard.take());
                            drop(lease.take());
                            let decoded = decode(&request, &args, geometry);
                            lease = Some(term.lease());
                            let Some(decoded) = decoded else {
                                return;
                            };
                            let mut store = lock(images);
                            let id = store.insert(key, decoded);
                            let image = store.get(id).expect("just inserted").clone();
                            (id, image)
                        }
                    };
                    let term = &mut **guard.get_or_insert_with(|| term.lock_unfair());
                    with_flushed_sync(parser, term, |term| image::place(term, id, &image));
                    lock(images).evict(term);
                }
            });
            processed += n;
            if processed >= MAX_LOCKED_READ {
                break;
            }
        }
        drop(guard);
        drop(lease);
        // Redraw unless everything went into a synchronized update.
        if processed > 0 && self.parser.sync_bytes_count() < processed {
            self.listener.send_event(Event::Wakeup);
        }
        Ok(())
    }

    /// Write queued input until the PTY would block.
    fn pty_write(&mut self) -> io::Result<()> {
        while let Some(front) = self.write_list.front() {
            match self.pty.writer().write(&front[self.written..]) {
                Ok(0) => break,
                Ok(n) => {
                    self.written += n;
                    if self.written == front.len() {
                        self.write_list.pop_front();
                        self.written = 0;
                    }
                }
                Err(err)
                    if matches!(err.kind(), ErrorKind::WouldBlock | ErrorKind::Interrupted) =>
                {
                    break;
                }
                Err(err) => return Err(err),
            }
        }
        Ok(())
    }
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
