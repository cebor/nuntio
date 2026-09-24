#![cfg(unix)]

use std::sync::mpsc;
use std::time::Duration;

use nuntio_term::{GridPoint, SelectionKind, Shell, SpawnOptions, TermEvent, TermHandle, TermSize};

const SIZE: TermSize = TermSize {
    columns: 40,
    lines: 5,
    cell_width: 8,
    cell_height: 16,
};

fn spawn(script: &str) -> (TermHandle, mpsc::Receiver<TermEvent>) {
    let (tx, rx) = mpsc::channel();
    let options = SpawnOptions {
        shell: Some(Shell {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), script.into()],
        }),
        working_directory: None,
        scrollback: 100,
        palette: Default::default(),
    };
    let handle = TermHandle::spawn(options, SIZE, move |event| {
        let _ = tx.send(event);
    })
    .expect("spawn");
    (handle, rx)
}

fn wait_for_exit(rx: &mpsc::Receiver<TermEvent>) -> Vec<TermEvent> {
    let mut events = Vec::new();
    loop {
        let event = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("shell did not exit");
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

#[test]
fn output_reaches_the_grid() {
    let (handle, rx) = spawn("printf 'hello\\r\\n\\033[1;31mred\\033[0m'");
    wait_for_exit(&rx);

    assert_eq!(line_text(&handle, 0), "hello");
    assert_eq!(line_text(&handle, 1), "red");
    let snapshot = handle.snapshot();
    let red = snapshot.cell(0, 1);
    assert!(red.style.bold);
    assert_eq!((red.fg.r, red.fg.g, red.fg.b), (0xc9, 0x1b, 0x00));
}

#[test]
fn environment_and_size() {
    let (handle, rx) = spawn("printf \"$TERM $COLORTERM\"; stty size");
    wait_for_exit(&rx);

    assert_eq!(line_text(&handle, 0), "xterm-256color truecolor5 40");
}

#[test]
fn title_is_reported() {
    let (_handle, rx) = spawn("printf '\\033]0;my title\\007'");
    let events = wait_for_exit(&rx);

    assert!(
        events.contains(&TermEvent::Title("my title".into())),
        "{events:?}"
    );
}

fn at(column: usize, line: usize) -> GridPoint {
    GridPoint {
        column,
        line,
        right_half: false,
    }
}

#[test]
fn selection_kinds() {
    let (handle, rx) = spawn("printf 'hello world\\r\\nsecond line'");
    wait_for_exit(&rx);

    handle.start_selection(SelectionKind::Semantic, at(7, 0));
    assert_eq!(handle.selection_text().as_deref(), Some("world"));

    handle.start_selection(SelectionKind::Lines, at(2, 1));
    assert_eq!(handle.selection_text().as_deref(), Some("second line\n"));

    handle.start_selection(SelectionKind::Simple, at(0, 0));
    handle.update_selection(GridPoint {
        right_half: true,
        ..at(4, 0)
    });
    assert_eq!(handle.selection_text().as_deref(), Some("hello"));
    assert_eq!(
        handle.snapshot().cell(0, 0).bg,
        handle.snapshot().cell(4, 0).bg
    );
    assert_ne!(
        handle.snapshot().cell(0, 0).bg,
        handle.snapshot().cell(6, 0).bg
    );

    handle.clear_selection();
    assert_eq!(handle.selection_text(), None);
}

#[test]
fn scrolling_stops_at_both_ends() {
    let (handle, rx) = spawn("for i in $(seq 1 30); do echo line$i; done; printf end");
    wait_for_exit(&rx);
    let top = |h: &TermHandle| line_text(h, 0);

    assert_eq!(top(&handle), "line27");
    handle.scroll(3);
    assert_eq!(top(&handle), "line24");
    handle.scroll(-10);
    assert_eq!(top(&handle), "line27");
    handle.scroll(1000);
    assert_eq!(top(&handle), "line1");
    handle.scroll_page(false);
    handle.scroll(-1000);
    assert_eq!(top(&handle), "line27");
}

#[test]
#[cfg(target_os = "linux")]
fn foreground_process_and_directory() {
    let (handle, _rx) = spawn("cd /tmp && exec sleep 5");
    // Wait until the shell has exec'd into sleep.
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while handle.process_name() != "sleep" && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(handle.process_name(), "sleep");
    assert_eq!(
        handle.working_directory().as_deref(),
        Some(std::path::Path::new("/tmp"))
    );
}
