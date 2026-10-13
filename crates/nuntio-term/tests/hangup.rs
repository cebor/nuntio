//! Its own test binary: the CPU time of the "PTY reader" threads is measured,
//! so no other pane may run in this process.
#![cfg(target_os = "linux")]

use std::time::Duration;

use nuntio_term::Program;

mod common;
use common::{sh, start, wait_for_exit};

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
    // The child closes the slave side but keeps running.
    let (_handle, rx) = start(common::options(Program::Command(sh(
        "exec sleep 3 </dev/null >/dev/null 2>&1",
    ))));

    std::thread::sleep(Duration::from_millis(500));
    let before = reader_ticks();
    std::thread::sleep(Duration::from_secs(1));
    let used = reader_ticks() - before;
    assert!(used < 20, "the PTY thread used {used} ticks in a second");

    // The exit still arrives.
    wait_for_exit(&rx);
}
