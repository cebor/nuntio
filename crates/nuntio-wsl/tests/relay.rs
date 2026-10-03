#![cfg(target_os = "linux")]

use std::io::{Read, Write};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use nuntio_wsl::proto::{READY, Size, encode_data, encode_resize};

const TIMEOUT: Duration = Duration::from_secs(10);

struct Helper {
    child: Child,
    stdin: Option<std::process::ChildStdin>,
    chunks: Receiver<Vec<u8>>,
    output: Vec<u8>,
}

impl Helper {
    fn start(args: &[&str]) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_nuntio-wsl"))
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take();
        let mut stdout = child.stdout.take().unwrap();
        let (tx, chunks) = mpsc::channel();
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            while let Ok(n) = stdout.read(&mut buf) {
                if n == 0 || tx.send(buf[..n].to_vec()).is_err() {
                    break;
                }
            }
        });
        Self {
            child,
            stdin,
            chunks,
            output: Vec::new(),
        }
    }

    fn send(&mut self, bytes: &[u8]) {
        let stdin = self.stdin.as_mut().unwrap();
        stdin.write_all(bytes).unwrap();
        stdin.flush().unwrap();
    }

    fn resize(&mut self, columns: u16, lines: u16) {
        let mut frame = Vec::new();
        encode_resize(
            Size {
                columns,
                lines,
                cell_width: 8,
                cell_height: 16,
            },
            &mut frame,
        );
        self.send(&frame);
    }

    fn data(&mut self, bytes: &[u8]) {
        let mut frame = Vec::new();
        encode_data(bytes, &mut frame);
        self.send(&frame);
    }

    /// Collects output until it contains `needle` after the first `from` bytes.
    fn wait_for(&mut self, needle: &[u8], from: usize) -> usize {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            if let Some(at) = find(&self.output[from..], needle) {
                return from + at + needle.len();
            }
            let left = deadline.saturating_duration_since(Instant::now());
            match self.chunks.recv_timeout(left) {
                Ok(chunk) => self.output.extend_from_slice(&chunk),
                Err(RecvTimeoutError::Timeout) => panic!(
                    "timeout waiting for {:?}, got {:?}",
                    String::from_utf8_lossy(needle),
                    String::from_utf8_lossy(&self.output)
                ),
                Err(RecvTimeoutError::Disconnected) => panic!(
                    "output ended without {:?}, got {:?}",
                    String::from_utf8_lossy(needle),
                    String::from_utf8_lossy(&self.output)
                ),
            }
        }
    }

    /// Waits for the exit and the end of the output.
    fn finish(&mut self) -> i32 {
        let deadline = Instant::now() + TIMEOUT;
        let code = loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                break status.code().expect("killed by a signal");
            }
            assert!(Instant::now() < deadline, "the helper did not exit");
            std::thread::sleep(Duration::from_millis(10));
        };
        while let Ok(chunk) = self.chunks.recv_timeout(Duration::from_secs(1)) {
            self.output.extend_from_slice(&chunk);
        }
        code
    }
}

impl Drop for Helper {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

#[test]
fn runs_a_program_with_the_initial_size_and_returns_its_exit_code() {
    let mut helper = Helper::start(&["--", "sh", "-c", "stty size; exit 3"]);
    helper.resize(50, 7);
    assert_eq!(helper.finish(), 3);
    assert!(helper.output.starts_with(READY));
    assert!(find(&helper.output, b"7 50").is_some());
}

#[test]
fn resize_frames_change_the_pty_size() {
    let mut helper = Helper::start(&["--", "sh"]);
    helper.resize(80, 24);
    helper.data(b"stty size\n");
    let seen = helper.wait_for(b"24 80", 0);
    helper.resize(100, 30);
    helper.data(b"stty size; exit\n");
    helper.wait_for(b"30 100", seen);
    assert_eq!(helper.finish(), 0);
}

#[test]
fn closing_stdin_ends_the_session() {
    let mut helper = Helper::start(&["--", "sleep", "30"]);
    helper.resize(80, 24);
    helper.wait_for(READY, 0);
    helper.stdin = None;
    let started = Instant::now();
    helper.finish();
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[test]
fn data_before_the_size_is_an_error() {
    let mut helper = Helper::start(&["--", "sh"]);
    helper.data(b"echo hi\n");
    assert_eq!(helper.finish(), 2);
    assert!(find(&helper.output, READY).is_none());
}

#[test]
fn a_program_that_does_not_exist_exits_127_without_ready() {
    let mut helper = Helper::start(&["--", "/nonexistent/nuntio-wsl-test"]);
    helper.resize(80, 24);
    assert_eq!(helper.finish(), 127);
    assert!(find(&helper.output, READY).is_none());
}
