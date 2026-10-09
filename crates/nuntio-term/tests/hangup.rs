//! Its own test binary: the CPU time of the "PTY reader" threads is measured,
//! so no other pane may run in this process.
#![cfg(target_os = "linux")]

use std::sync::mpsc;
use std::time::Duration;

use nuntio_term::{Shell, SpawnOptions, TermEvent, TermHandle, TermOptions, TermSize, Transport};

const SIZE: TermSize = TermSize {
    columns: 40,
    lines: 5,
    cell_width: 8,
    cell_height: 16,
};

const OPTIONS: TermOptions = TermOptions {
    scrollback: 100,
    clipboard_write: true,
    kitty_keyboard: true,
};

fn options(script: &str) -> SpawnOptions {
    SpawnOptions {
        shell: Some(Shell {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), script.into()],
        }),
        login_shell: false,
        working_directory: None,
        term: OPTIONS,
        palette: Default::default(),
        env: Vec::new(),
        window_id: None,
        transport: Transport::Pty,
    }
}

/// Clock ticks of user and system time used by all "PTY reader" threads.
fn reader_ticks() -> u64 {
    let mut total = 0;
    for task in std::fs::read_dir("/proc/self/task").unwrap().flatten() {
        let path = task.path();
        let Ok(comm) = std::fs::read_to_string(path.join("comm")) else {
            continue;
        };
        if comm.trim() != "PTY reader" {
            continue;
        }
        let Ok(stat) = std::fs::read_to_string(path.join("stat")) else {
            continue;
        };
        // Fields after the command name, which may contain spaces: utime and
        // stime are the 14th and 15th overall, so index 11 and 12 here.
        let rest = &stat[stat.rfind(')').unwrap() + 1..];
        let fields: Vec<&str> = rest.split_whitespace().collect();
        total += fields[11].parse::<u64>().unwrap() + fields[12].parse::<u64>().unwrap();
    }
    total
}

#[test]
fn hung_up_pty_does_not_spin() {
    let (tx, rx) = mpsc::channel();
    // The child closes the slave side but keeps running.
    let _handle = TermHandle::spawn(
        options("exec sleep 3 </dev/null >/dev/null 2>&1"),
        SIZE,
        move |event| {
            let _ = tx.send(event);
        },
    )
    .expect("spawn");

    std::thread::sleep(Duration::from_millis(500));
    let before = reader_ticks();
    std::thread::sleep(Duration::from_secs(1));
    let used = reader_ticks() - before;
    assert!(used < 20, "the PTY thread used {used} ticks in a second");

    // The exit still arrives.
    loop {
        let event = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("shell did not exit");
        if event == TermEvent::Exit {
            break;
        }
    }
}
