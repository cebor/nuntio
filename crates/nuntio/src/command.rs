//! Running helper programs (`wsl.exe`, herdr, tmux) without letting them
//! hang nuntio or flash up a console window.

use std::io::{self, Read};
use std::process::{Command, Output, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// `Command::output`, but a command that runs longer than `limit` is
/// killed and gives `None`. Stdin and stderr are null.
pub fn output_within(command: &mut Command, limit: Duration) -> io::Result<Option<Output>> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("no stdout"))?;
    // Read on the side, so that a full pipe doesn't stall the command.
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stdout.read_to_end(&mut bytes);
        let _ = sender.send(bytes);
    });
    let deadline = Instant::now() + limit;
    loop {
        if let Some(status) = child.try_wait()? {
            let stdout = receiver
                .recv_timeout(Duration::from_secs(1))
                .unwrap_or_default();
            return Ok(Some(Output {
                status,
                stdout,
                stderr: Vec::new(),
            }));
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Ok(None);
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// No console window flashing up for a console program started from the
/// GUI on Windows; nothing elsewhere.
pub fn no_window(command: &mut Command) -> &mut Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
}

// Needs `sh`.
#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn slow_commands_are_killed() {
        let started = Instant::now();
        let slow = output_within(
            Command::new("sh").args(["-c", "sleep 5"]),
            Duration::from_millis(100),
        )
        .unwrap();
        assert!(slow.is_none());
        assert!(started.elapsed() < Duration::from_secs(2));

        let quick = output_within(
            Command::new("sh").args(["-c", "printf %s hi"]),
            Duration::from_secs(5),
        )
        .unwrap()
        .unwrap();
        assert_eq!(quick.stdout, b"hi");
    }
}
