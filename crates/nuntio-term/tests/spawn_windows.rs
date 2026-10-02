#![cfg(windows)]

use std::sync::mpsc;
use std::time::Duration;

use nuntio_term::{Shell, SpawnOptions, TermEvent, TermHandle, TermOptions, TermSize};

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

fn spawn(program: &str, args: &[&str]) -> (TermHandle, mpsc::Receiver<TermEvent>) {
    let (tx, rx) = mpsc::channel();
    let options = SpawnOptions {
        shell: Some(Shell {
            program: program.into(),
            args: args.iter().map(|&a| a.into()).collect(),
        }),
        login_shell: false,
        working_directory: None,
        term: OPTIONS,
        palette: Default::default(),
        env: vec![],
        window_id: None,
    };
    let handle = TermHandle::spawn(options, SIZE, move |event| {
        let _ = tx.send(event);
    })
    .expect("spawn");
    (handle, rx)
}

/// ConPTY and PowerShell start slowly, hence the generous timeout.
fn wait_for_exit(rx: &mpsc::Receiver<TermEvent>) -> Vec<TermEvent> {
    let mut events = Vec::new();
    loop {
        let event = rx
            .recv_timeout(Duration::from_secs(15))
            .expect("program did not exit");
        let exit = event == TermEvent::Exit;
        events.push(event);
        if exit {
            return events;
        }
    }
}

fn line_text(handle: &TermHandle, line: usize) -> String {
    let snapshot = handle.snapshot();
    (0..snapshot.columns)
        .map(|c| snapshot.cell(c, line).c)
        .collect::<String>()
        .trim_end()
        .into()
}

/// ConPTY may position output below a clear, so every line is searched.
fn screen_contains(handle: &TermHandle, needle: &str) -> bool {
    let lines = handle.snapshot().lines;
    (0..lines).any(|line| line_text(handle, line).contains(needle))
}

#[test]
fn output_reaches_the_grid() {
    let (handle, rx) = spawn("cmd.exe", &["/c", "echo hello %TERM_PROGRAM%"]);
    wait_for_exit(&rx);
    assert!(screen_contains(&handle, "hello nuntio"));
}

#[test]
fn exit_is_delivered() {
    let (_handle, rx) = spawn("cmd.exe", &["/c", "exit 3"]);
    let events = wait_for_exit(&rx);
    assert_eq!(events.last(), Some(&TermEvent::Exit));
    // ConPTY announces the program's path as the title at startup; that
    // must have become a reset.
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, TermEvent::Title(t) if t.to_lowercase().ends_with("cmd.exe")))
    );
}

#[test]
fn resize_is_applied() {
    let (handle, rx) = spawn(
        "powershell.exe",
        &[
            "-NoProfile",
            "-Command",
            "Start-Sleep -Milliseconds 2000; [Console]::WindowWidth",
        ],
    );
    handle.resize(TermSize {
        columns: 60,
        lines: 10,
        ..SIZE
    });
    wait_for_exit(&rx);
    assert!(screen_contains(&handle, "60"));
}
