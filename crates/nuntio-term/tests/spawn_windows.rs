#![cfg(windows)]

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

fn spawn(program: &str, args: &[&str]) -> (TermHandle, mpsc::Receiver<TermEvent>) {
    let (tx, rx) = mpsc::channel();
    let handle = spawn_with(program, args, move |event| {
        let _ = tx.send(event);
    });
    (handle, rx)
}

fn spawn_with(
    program: &str,
    args: &[&str],
    callback: impl Fn(TermEvent) + Send + Sync + 'static,
) -> TermHandle {
    spawn_via(program, args, Transport::Pty, callback)
}

fn spawn_via(
    program: &str,
    args: &[&str],
    transport: Transport,
    callback: impl Fn(TermEvent) + Send + Sync + 'static,
) -> TermHandle {
    let options = SpawnOptions {
        shell: Some(shell(program, args)),
        login_shell: false,
        working_directory: None,
        term: OPTIONS,
        palette: Default::default(),
        env: vec![],
        window_id: None,
        transport,
    };
    TermHandle::spawn(options, SIZE, callback).expect("spawn")
}

fn shell(program: &str, args: &[&str]) -> Shell {
    Shell {
        program: program.into(),
        args: args.iter().map(|&a| a.into()).collect(),
    }
}

/// Like [`spawn`], over the pipes nuntio-wsl uses.
fn spawn_helper(
    program: &str,
    args: &[&str],
    fallback: Shell,
) -> (TermHandle, mpsc::Receiver<TermEvent>) {
    let (tx, rx) = mpsc::channel();
    let handle = spawn_via(
        program,
        args,
        Transport::WslHelper { fallback },
        move |event| {
            let _ = tx.send(event);
        },
    );
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

#[test]
fn inline_image_passes_through_conpty() {
    use base64::Engine;

    let mut png = std::io::Cursor::new(Vec::new());
    image::RgbaImage::from_pixel(1, 1, image::Rgba([0, 0, 255, 255]))
        .write_to(&mut png, image::ImageFormat::Png)
        .unwrap();
    let data = base64::engine::general_purpose::STANDARD.encode(png.into_inner());
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
    let handle = spawn_with(
        "powershell.exe",
        &[
            "-NoProfile",
            "-Command",
            "[Console]::Write(\"$([char]27)]1337;File=inline=0:$('A' * 300000)$([char]7)\"); Start-Sleep -Seconds 1; [Console]::Write('END'); Start-Sleep -Seconds 30",
        ],
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

/// A helper that exits without reporting ready hands the pane to ConPTY.
#[test]
fn failed_helper_falls_back_to_conpty() {
    let (handle, rx) = spawn_helper(
        "cmd.exe",
        &["/c", "exit 1"],
        shell("cmd.exe", &["/c", "echo via-conpty"]),
    );
    wait_for_exit(&rx);
    assert!(screen_contains(&handle, "nuntio-wsl did not start"));
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
