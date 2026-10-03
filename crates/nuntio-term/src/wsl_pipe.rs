//! A WSL pane without ConPTY: `wsl.exe` runs nuntio-wsl with plain pipes,
//! and nuntio-wsl runs the shell in a Linux PTY. Its output arrives byte
//! for byte, so inline images stay in order with the cursor moves around
//! them, which Windows' ConPTY doesn't guarantee.
//!
//! Pipes can't be polled on Windows: threads read and write them and wake
//! the poller with posted completion packets, as alacritty does for ConPTY.

use std::collections::{HashMap, VecDeque};
use std::io::{self, ErrorKind, Read, Write};
use std::os::windows::process::CommandExt;
use std::process::{Command, ExitStatus, Stdio};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, mpsc};
use std::time::Duration;

use alacritty_terminal::event::WindowSize;
use alacritty_terminal::thread::spawn_named;
use nuntio_wsl::proto::{self, ReadyFilter, Size};
use polling::Poller;
use polling::os::iocp::{CompletionPacket, PollerIocpExt};

use crate::backend::BackendEvent;

/// No console window for `wsl.exe`.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const READ_SIZE: usize = 0x1_0000;
/// How long the exit waits for the output pipes to close after `wsl.exe`
/// exited, in case a process it started still holds them.
const DRAIN_TIMEOUT: Duration = Duration::from_secs(1);

pub(crate) struct WslPipe {
    shared: Arc<Shared>,
    /// Encoded frames for the input thread.
    frames: mpsc::Sender<Vec<u8>>,
    pid: u32,
    reported: bool,
}

struct Shared {
    state: Mutex<State>,
    changed: Condvar,
}

#[derive(Default)]
struct State {
    /// Output not yet read by the IO loop: stdout without the ready marker,
    /// and stderr.
    output: VecDeque<u8>,
    registration: Option<(Arc<Poller>, polling::Event)>,
    /// nuntio-wsl has started the shell.
    ready: bool,
    /// stdout and stderr readers still running.
    open_readers: u8,
    /// `wsl.exe` exited and its output is read; the status if known.
    exit: Option<Option<ExitStatus>>,
}

impl State {
    /// Tell the IO loop there is output or an exit to look at.
    fn wake(&self) {
        if let Some((poller, interest)) = &self.registration
            && interest.readable
        {
            let _ = poller.post(CompletionPacket::new(polling::Event::readable(
                interest.key,
            )));
        }
    }

    fn pending(&self) -> bool {
        !self.output.is_empty() || self.exit.is_some()
    }
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Append output, waking the loop if it had drained everything.
    fn push(&self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        let mut state = self.lock();
        let was_empty = state.output.is_empty();
        state.output.extend(bytes);
        if was_empty {
            state.wake();
        }
    }

    fn reader_done(&self) {
        let mut state = self.lock();
        state.open_readers -= 1;
        self.changed.notify_all();
    }
}

impl WslPipe {
    /// Start `program` (`wsl.exe` running nuntio-wsl) with the window `size`.
    pub(crate) fn spawn(
        program: &str,
        args: &[String],
        env: &HashMap<String, String>,
        size: WindowSize,
    ) -> io::Result<Self> {
        // No `current_dir`: `wsl.exe --cd` picks the directory.
        let mut child = Command::new(program)
            .args(args)
            .envs(env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()?;
        let pid = child.id();
        let (Some(mut stdin), Some(mut stdout), Some(mut stderr)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            unreachable!("all three are piped");
        };
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                open_readers: 2,
                ..State::default()
            }),
            changed: Condvar::new(),
        });

        let (frames, frame_rx) = mpsc::channel::<Vec<u8>>();
        // nuntio-wsl needs the size before it can open its PTY.
        let mut first = Vec::new();
        proto::encode_resize(proto_size(size), &mut first);
        let _ = frames.send(first);
        spawn_named("nuntio-wsl input", move || {
            for frame in frame_rx {
                if stdin.write_all(&frame).is_err() {
                    break;
                }
            }
        });

        let out = shared.clone();
        spawn_named("nuntio-wsl stdout", move || {
            let mut filter = ReadyFilter::default();
            let mut ready = false;
            let mut buf = vec![0; READ_SIZE];
            loop {
                let n = match stdout.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => n,
                    Err(err) if err.kind() == ErrorKind::Interrupted => continue,
                    Err(_) => break,
                };
                filter.feed(&buf[..n], |bytes| out.push(bytes));
                if !ready && filter.is_ready() {
                    ready = true;
                    out.lock().ready = true;
                }
            }
            filter.finish(|bytes| out.push(bytes));
            out.reader_done();
        });

        let err = shared.clone();
        spawn_named("nuntio-wsl stderr", move || {
            let mut buf = vec![0; READ_SIZE];
            loop {
                match stderr.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => err.push(&buf[..n]),
                    Err(e) if e.kind() == ErrorKind::Interrupted => {}
                    Err(_) => break,
                }
            }
            err.reader_done();
        });

        let waiter = shared.clone();
        spawn_named("nuntio-wsl waiter", move || {
            let status = child.wait().ok();
            let state = waiter.lock();
            // All output comes before the exit.
            let (mut state, _) = waiter
                .changed
                .wait_timeout_while(state, DRAIN_TIMEOUT, |s| s.open_readers > 0)
                .unwrap_or_else(PoisonError::into_inner);
            state.exit = Some(status);
            state.wake();
        });

        Ok(Self {
            shared,
            frames,
            pid,
            reported: false,
        })
    }

    pub(crate) fn pid(&self) -> u32 {
        self.pid
    }

    /// Register for `interest`, replacing an earlier registration. Writes
    /// never block, so a writable interest is answered at once.
    pub(crate) fn register(&mut self, poller: &Arc<Poller>, interest: polling::Event) {
        let mut state = self.shared.lock();
        state.registration = Some((poller.clone(), interest));
        if interest.readable && state.pending() {
            state.wake();
        }
        if interest.writable {
            let _ = poller.post(CompletionPacket::new(polling::Event::writable(
                interest.key,
            )));
        }
    }

    pub(crate) fn deregister(&mut self) {
        self.shared.lock().registration = None;
    }

    pub(crate) fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let mut state = self.shared.lock();
        if state.output.is_empty() {
            return Err(ErrorKind::WouldBlock.into());
        }
        let (front, _) = state.output.as_slices();
        let n = front.len().min(buf.len());
        buf[..n].copy_from_slice(&front[..n]);
        state.output.drain(..n);
        Ok(n)
    }

    /// Queue `buf` for the shell. Succeeds even after nuntio-wsl is gone:
    /// its exit closes the pane.
    pub(crate) fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut frame = Vec::with_capacity(buf.len() + 5);
        proto::encode_data(buf, &mut frame);
        let _ = self.frames.send(frame);
        Ok(buf.len())
    }

    pub(crate) fn resize(&mut self, size: WindowSize) {
        let mut frame = Vec::new();
        proto::encode_resize(proto_size(size), &mut frame);
        let _ = self.frames.send(frame);
    }

    /// Once, after `wsl.exe` exited: the shell exited if nuntio-wsl had
    /// started it, otherwise nuntio-wsl failed.
    pub(crate) fn next_event(&mut self) -> Option<BackendEvent> {
        if self.reported {
            return None;
        }
        let state = self.shared.lock();
        let status = state.exit?;
        let ready = state.ready;
        drop(state);
        self.reported = true;
        Some(if ready {
            BackendEvent::Exited(status)
        } else {
            BackendEvent::HelperFailed(status)
        })
    }
}

fn proto_size(size: WindowSize) -> Size {
    Size {
        columns: size.num_cols,
        lines: size.num_lines,
        cell_width: size.cell_width,
        cell_height: size.cell_height,
    }
}
