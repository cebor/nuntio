#![cfg(windows)]

use std::sync::mpsc;
use std::time::Duration;

use nuntio_term::{Program, Shell, SpawnOptions, TermEvent, TermHandle, TermSize, Transport};

mod common;
use common::{SIZE, blue_pixel_png, line_text, shell, spawn_handle, start, wait_for_exit};

fn spawn(program: &str, args: &[&str]) -> (TermHandle, mpsc::Receiver<TermEvent>) {
    start(common::options(Program::Command(shell(program, args))))
}

/// Like [`spawn`], over the pipes nuntio-wsl uses.
fn spawn_helper(
    program: &str,
    args: &[&str],
    fallback: Shell,
) -> (TermHandle, mpsc::Receiver<TermEvent>) {
    start(SpawnOptions {
        transport: Transport::WslHelper { fallback },
        ..common::options(Program::Command(shell(program, args)))
    })
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
    let (handle, rx) = spawn("cmd.exe", &["/c", "exit 3"]);
    let events = wait_for_exit(&rx);
    assert_eq!(events.last(), Some(&TermEvent::Exit));
    // ConPTY announces the program's path as the title at startup; that
    // must have become a reset.
    assert!(!matches!(handle.take_title(), Some(Some(t)) if t.to_lowercase().ends_with("cmd.exe")));
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

#[test]
fn inline_image_passes_through_conpty() {
    let data = blue_pixel_png();
    let (handle, rx) = spawn(
        "powershell.exe",
        &[
            "-NoProfile",
            "-Command",
            &format!(
                "[Console]::Write(\"$([char]27)]1337;File=inline=1;width=2;height=1:{data}$([char]7)X\")"
            ),
        ],
    );
    wait_for_exit(&rx);

    let snapshot = handle.snapshot();
    let [image] = snapshot.images.as_slice() else {
        panic!("expected one image, got {}", snapshot.images.len());
    };
    assert_eq!(image.columns, 2);
    // ConPTY may move output below a clear, so the line is wherever the image landed.
    assert_eq!(snapshot.cell(image.column + 2, image.line).c, 'X');
}

/// ConPTY passes an image through in one piece. When more of it waits than
/// one read pass takes, output that follows later must still arrive without
/// further input.
#[test]
fn large_output_is_read_without_input() {
    // A slow listener lets the output pile up between read passes.
    let handle = spawn_handle(
        common::options(Program::Command(shell(
            "powershell.exe",
            &[
                "-NoProfile",
                "-Command",
                "[Console]::Write(\"$([char]27)]1337;File=inline=0:$('A' * 300000)$([char]7)\"); Start-Sleep -Seconds 1; [Console]::Write('END'); Start-Sleep -Seconds 30",
            ],
        ))),
        |event| {
            if event == TermEvent::Wakeup {
                std::thread::sleep(Duration::from_millis(200));
            }
        },
    );
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    while !screen_contains(&handle, "END") {
        assert!(
            std::time::Instant::now() < deadline,
            "output after the large sequence did not arrive"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// A stand-in for nuntio-wsl that reports ready and prints: its output
/// reaches the grid without the marker, and its exit closes the pane.
#[test]
fn helper_output_reaches_the_grid() {
    let (handle, rx) = spawn_helper(
        "powershell.exe",
        &[
            "-NoProfile",
            "-Command",
            "[Console]::Out.Write(\"$([char]27)]nuntio-wsl;ready$([char]7)hello\")",
        ],
        shell("cmd.exe", &["/c", "echo wrong-fallback"]),
    );
    wait_for_exit(&rx);
    assert!(screen_contains(&handle, "hello"));
    assert!(!screen_contains(&handle, "nuntio-wsl"));
    assert!(!screen_contains(&handle, "wrong-fallback"));
}

/// A helper that exits without reporting ready hands the pane to ConPTY and
/// says so.
#[test]
fn failed_helper_falls_back_to_conpty() {
    let (handle, rx) = spawn_helper(
        "cmd.exe",
        &["/c", "exit 1"],
        shell("cmd.exe", &["/c", "echo via-conpty"]),
    );
    let events = wait_for_exit(&rx);
    assert!(events.contains(&TermEvent::HelperFailed));
    assert!(screen_contains(&handle, "via-conpty"));
}

/// ConPTY panes don't ask omp for images: Windows' ConPTY would misplace them.
/// The variable is blanked, so one inherited from a nuntio pane is gone too.
#[test]
fn conpty_panes_get_no_image_protocol() {
    let (handle, rx) = spawn("cmd.exe", &["/c", "echo [%PI_FORCE_IMAGE_PROTOCOL%]"]);
    wait_for_exit(&rx);
    assert!(screen_contains(&handle, "["));
    assert!(!screen_contains(&handle, "iterm2"));
}
