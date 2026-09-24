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

#[test]
fn search_through_scrollback() {
    let (handle, rx) = spawn(
        "echo marker-A; for i in $(seq 1 30); do echo x; done; echo marker-B; printf 'x\\r\\nx\\r\\nend'",
    );
    wait_for_exit(&rx);
    let mut search = nuntio_term::Search::new("marker", false).unwrap();

    // Upwards from the bottom: the newest match first, then older, wrapping.
    assert!(handle.search(&mut search, true));
    assert!(screen_contains(&handle, "marker-B"));
    assert!(handle.search(&mut search, true));
    assert!(
        screen_contains(&handle, "marker-A"),
        "scrolled to the older match"
    );
    assert!(handle.search(&mut search, true));
    assert!(screen_contains(&handle, "marker-B"), "wrapped around");
    assert!(handle.search(&mut search, false));
    assert!(screen_contains(&handle, "marker-A"), "downwards wraps too");

    // The current match is highlighted.
    let snapshot = handle.search_snapshot(&mut search);
    let line = (0..snapshot.lines)
        .find(|&l| line_text(&handle, l).starts_with("marker-A"))
        .unwrap();
    let highlighted = snapshot.cell(0, line).bg;
    assert_ne!(highlighted, snapshot.background);
    assert_eq!(snapshot.cell(5, line).bg, highlighted, "whole match");
    assert_ne!(snapshot.cell(7, line).bg, highlighted, "not beyond");

    let mut none = nuntio_term::Search::new("absent", false).unwrap();
    assert!(!handle.search(&mut none, true));
}

fn screen_contains(handle: &TermHandle, text: &str) -> bool {
    (0..SIZE.lines as usize).any(|l| line_text(handle, l).contains(text))
}

#[test]
fn links_in_text_and_osc8() {
    let (handle, rx) = spawn(
        "printf 'see https://example.com/x, ok\\r\\n\\033]8;;https://nuntio.dev\\033\\\\click\\033]8;;\\033\\\\ here'",
    );
    wait_for_exit(&rx);

    let link = handle.link_at(at(10, 0)).unwrap();
    assert_eq!(link.url, "https://example.com/x");
    assert_eq!((link.start, link.end), ((4, 0), (24, 0)));
    assert_eq!(handle.link_at(at(27, 0)), None);

    let link = handle.link_at(at(2, 1)).unwrap();
    assert_eq!(link.url, "https://nuntio.dev");
    assert_eq!((link.start, link.end), ((0, 1), (4, 1)));
    assert_eq!(handle.link_at(at(7, 1)), None);
}

#[test]
fn links_across_wrapped_lines() {
    // 40 columns: the URL wraps onto the second row.
    let url = format!("https://example.com/{}", "a".repeat(40));
    let (handle, rx) = spawn(&format!("printf '{url}'"));
    wait_for_exit(&rx);

    let link = handle.link_at(at(3, 1)).unwrap();
    assert_eq!(link.url, url);
    assert_eq!((link.start, link.end), ((0, 0), (19, 1)));
}

#[test]
fn search_survives_a_cleared_scrollback() {
    let script = "echo marker; for i in $(seq 1 60); do echo x; done; printf end";
    let (handle, rx) = spawn(script);
    wait_for_exit(&rx);
    let mut search = nuntio_term::Search::new("marker", false).unwrap();
    assert!(handle.search(&mut search, true));

    // The match was in the scrollback, which is gone now (`clear`, CSI 3 J).
    handle.clear_history();
    handle.search_snapshot(&mut search);
    assert!(!handle.search(&mut search, true));
    assert!(!handle.search(&mut search, false));

    // Refining the query continues from the old match position.
    let (handle, rx) = spawn(script);
    wait_for_exit(&rx);
    let mut search = nuntio_term::Search::new("marker", false).unwrap();
    assert!(handle.search(&mut search, true));
    handle.clear_history();
    let mut refined = nuntio_term::Search::new("markerx", false)
        .unwrap()
        .continue_from(&search);
    assert!(!handle.search(&mut refined, true));
}

#[test]
fn osc8_links_with_unknown_schemes_are_ignored() {
    let (handle, rx) = spawn("printf '\\033]8;;ms-msdt:/id x\\033\\\\click\\033]8;;\\033\\\\'");
    wait_for_exit(&rx);

    assert_eq!(line_text(&handle, 0), "click");
    assert_eq!(handle.link_at(at(2, 0)), None);
}
