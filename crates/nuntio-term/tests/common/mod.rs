//! Helpers shared by the integration tests; each test binary uses a subset.
#![allow(dead_code)]

use std::sync::mpsc;
use std::time::Duration;

use base64::Engine;
use nuntio_term::{
    Program, Shell, SpawnOptions, TermEvent, TermHandle, TermOptions, TermSize, Transport,
};

pub const SIZE: TermSize = TermSize {
    columns: 40,
    lines: 5,
    cell_width: 8,
    cell_height: 16,
};

pub const OPTIONS: TermOptions = TermOptions {
    scrollback: 100,
    clipboard_write: true,
    kitty_keyboard: true,
};

/// ConPTY and PowerShell start slowly, hence the generous timeout there.
const EXIT_TIMEOUT: Duration = Duration::from_secs(if cfg!(windows) { 15 } else { 5 });

pub fn shell(program: &str, args: &[&str]) -> Shell {
    Shell {
        program: program.into(),
        args: args.iter().map(|&a| a.into()).collect(),
    }
}

/// `/bin/sh -c script`.
pub fn sh(script: &str) -> Shell {
    shell("/bin/sh", &["-c", script])
}

/// Options for a [`SIZE`] pane on the PTY; tests override fields with
/// struct update syntax.
pub fn options(program: Program) -> SpawnOptions {
    SpawnOptions {
        program,
        working_directory: None,
        term: OPTIONS,
        palette: Default::default(),
        env: Vec::new(),
        window_id: None,
        transport: Transport::Pty,
    }
}

pub fn spawn_handle(
    options: SpawnOptions,
    callback: impl Fn(TermEvent) + Send + Sync + 'static,
) -> TermHandle {
    TermHandle::spawn(options, SIZE, callback).expect("spawn")
}

/// Spawn a pane whose events arrive on the returned channel.
pub fn start(options: SpawnOptions) -> (TermHandle, mpsc::Receiver<TermEvent>) {
    let (tx, rx) = mpsc::channel();
    let handle = spawn_handle(options, move |event| {
        let _ = tx.send(event);
    });
    (handle, rx)
}

/// The events up to and including the exit.
pub fn wait_for_exit(rx: &mpsc::Receiver<TermEvent>) -> Vec<TermEvent> {
    let mut events = Vec::new();
    loop {
        let event = rx.recv_timeout(EXIT_TIMEOUT).expect("program did not exit");
        let exit = event == TermEvent::Exit;
        events.push(event);
        if exit {
            return events;
        }
    }
}

pub fn line_text(handle: &TermHandle, line: usize) -> String {
    let snapshot = handle.snapshot();
    (0..snapshot.columns)
        .map(|c| snapshot.cell(c, line).c)
        .collect::<String>()
        .trim_end()
        .into()
}

/// A 1x1 blue PNG, base64 encoded, as the payload of an inline image.
pub fn blue_pixel_png() -> String {
    let mut png = std::io::Cursor::new(Vec::new());
    image::RgbaImage::from_pixel(1, 1, image::Rgba([0, 0, 255, 255]))
        .write_to(&mut png, image::ImageFormat::Png)
        .unwrap();
    base64::engine::general_purpose::STANDARD.encode(png.into_inner())
}
