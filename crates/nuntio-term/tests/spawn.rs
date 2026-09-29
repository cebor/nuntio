#![cfg(unix)]

use std::sync::mpsc;
use std::time::Duration;

use nuntio_term::{
    Engine, GridPoint, ReportedDir, SelectionKind, Shell, SpawnOptions, TermEvent, TermHandle,
    TermMode, TermOptions, TermSize,
};

const SIZE: TermSize = TermSize {
    columns: 40,
    lines: 5,
    cell_width: 8,
    cell_height: 16,
};

fn spawn(engine: Engine, script: &str) -> (TermHandle, mpsc::Receiver<TermEvent>) {
    spawn_with_env(engine, script, Vec::new())
}

const OPTIONS: TermOptions = TermOptions {
    scrollback: 100,
    clipboard_write: true,
    kitty_keyboard: true,
};

fn spawn_with_env(
    engine: Engine,
    script: &str,
    env: Vec<(String, String)>,
) -> (TermHandle, mpsc::Receiver<TermEvent>) {
    spawn_with(engine, script, env, OPTIONS)
}

fn spawn_with(
    engine: Engine,
    script: &str,
    env: Vec<(String, String)>,
    term: TermOptions,
) -> (TermHandle, mpsc::Receiver<TermEvent>) {
    let (tx, rx) = mpsc::channel();
    let options = SpawnOptions {
        shell: Some(Shell {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), script.into()],
        }),
        login_shell: false,
        working_directory: None,
        term,
        palette: Default::default(),
        env,
        engine,
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

fn output_reaches_the_grid(engine: Engine) {
    let (handle, rx) = spawn(engine, "printf 'hello\\r\\n\\033[1;31mred\\033[0m'");
    wait_for_exit(&rx);

    assert_eq!(line_text(&handle, 0), "hello");
    assert_eq!(line_text(&handle, 1), "red");
    let snapshot = handle.snapshot();
    let red = snapshot.cell(0, 1);
    assert!(red.style.bold);
    assert_eq!((red.fg.r, red.fg.g, red.fg.b), (0xc9, 0x1b, 0x00));
}

fn environment_and_size(engine: Engine) {
    let (handle, rx) = spawn(engine, "printf \"$TERM $COLORTERM\"; stty size");
    wait_for_exit(&rx);

    assert_eq!(line_text(&handle, 0), "xterm-256color truecolor5 40");
}

fn extra_environment(engine: Engine) {
    let env = vec![
        ("NUNTIO_CONFIG".to_owned(), "/tmp/x.toml".to_owned()),
        ("TERM".to_owned(), "dumb".to_owned()),
    ];
    let (handle, rx) = spawn_with_env(engine, "printf \"$NUNTIO_CONFIG $TERM\"", env);
    wait_for_exit(&rx);

    assert_eq!(line_text(&handle, 0), "/tmp/x.toml dumb");
}

fn title_is_reported(engine: Engine) {
    let (_handle, rx) = spawn(engine, "printf '\\033]0;my title\\007'");
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

fn selection_kinds(engine: Engine) {
    let (handle, rx) = spawn(engine, "printf 'hello world\\r\\nsecond line'");
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

fn scrolling_stops_at_both_ends(engine: Engine) {
    let (handle, rx) = spawn(
        engine,
        "for i in $(seq 1 30); do echo line$i; done; printf end",
    );
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

#[cfg(target_os = "linux")]
fn foreground_process_and_directory(engine: Engine) {
    let (handle, _rx) = spawn(engine, "cd /tmp && exec sleep 5");
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

/// A pane that runs a command has no prompt, even though the command is
/// its own process group leader.
#[cfg(unix)]
fn a_command_is_not_an_idle_shell(engine: Engine) {
    let (handle, _rx) = spawn(engine, "exec sleep 5");
    assert_eq!(handle.foreground_is_shell(), None);
}

fn reported_directory(engine: Engine) {
    let (handle, rx) = spawn(
        engine,
        r"printf '\033]7;file://box/home/me/My%%20Files\033\\'",
    );
    wait_for_exit(&rx);
    assert_eq!(
        handle.reported_directory(),
        Some(ReportedDir::Posix("/home/me/My Files".into()))
    );
}

/// On macOS the user's shell runs behind `login`, which is the process
/// nuntio started; the shell at its prompt still counts as idle.
#[cfg(target_os = "macos")]
fn login_shell_is_seen_behind_login(engine: Engine) {
    let (tx, _rx) = mpsc::channel();
    let options = SpawnOptions {
        shell: Some(Shell {
            program: "/bin/zsh".into(),
            args: vec!["-f".into()],
        }),
        login_shell: true,
        working_directory: Some("/tmp".into()),
        term: OPTIONS,
        palette: Default::default(),
        env: Vec::new(),
        engine,
    };
    let handle = TermHandle::spawn(options, SIZE, move |event| {
        let _ = tx.send(event);
    })
    .expect("spawn");
    let wait_until = |done: &dyn Fn() -> bool| {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !done() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
    };

    wait_until(&|| handle.foreground_is_shell() == Some(true));
    assert_eq!(handle.foreground_is_shell(), Some(true));
    assert_eq!(handle.process_name(), "zsh");
    assert_eq!(
        handle
            .working_directory()
            .map(|dir| dir.canonicalize().unwrap()),
        Some(std::path::PathBuf::from("/private/tmp"))
    );

    handle.write(&b"sleep 5\r"[..]);
    wait_until(&|| handle.process_name() == "sleep");
    assert_eq!(handle.process_name(), "sleep");
    assert_eq!(handle.foreground_is_shell(), Some(false));
}

fn search_through_scrollback(engine: Engine) {
    let (handle, rx) = spawn(
        engine,
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

fn links_in_text_and_osc8(engine: Engine) {
    let (handle, rx) = spawn(
        engine,
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

fn links_across_wrapped_lines(engine: Engine) {
    // 40 columns: the URL wraps onto the second row.
    let url = format!("https://example.com/{}", "a".repeat(40));
    let (handle, rx) = spawn(engine, &format!("printf '{url}'"));
    wait_for_exit(&rx);

    let link = handle.link_at(at(3, 1)).unwrap();
    assert_eq!(link.url, url);
    assert_eq!((link.start, link.end), ((0, 0), (19, 1)));
}

fn search_survives_a_cleared_scrollback(engine: Engine) {
    let script = "echo marker; for i in $(seq 1 60); do echo x; done; printf end";
    let (handle, rx) = spawn(engine, script);
    wait_for_exit(&rx);
    let mut search = nuntio_term::Search::new("marker", false).unwrap();
    assert!(handle.search(&mut search, true));

    // The match was in the scrollback, which is gone now (`clear`, CSI 3 J).
    handle.clear_history();
    handle.search_snapshot(&mut search);
    assert!(!search.has_match(), "stale match moved onto the screen");
    assert!(!handle.search(&mut search, true));
    assert!(!handle.search(&mut search, false));

    // Refining the query continues from the old match position.
    let (handle, rx) = spawn(engine, script);
    wait_for_exit(&rx);
    let mut search = nuntio_term::Search::new("marker", false).unwrap();
    assert!(handle.search(&mut search, true));
    handle.clear_history();
    let mut refined = nuntio_term::Search::new("markerx", false)
        .unwrap()
        .continue_from(&search);
    assert!(!handle.search(&mut refined, true));
}

fn wide_characters_in_a_one_column_pane(engine: Engine) {
    let (handle, rx) = spawn(engine, "sleep 0.3; printf '漢字'");
    handle.resize(TermSize { columns: 1, ..SIZE });
    wait_for_exit(&rx);
}

fn color_queries_see_runtime_overrides(engine: Engine) {
    let (handle, rx) = spawn(
        engine,
        "stty raw -echo; printf '\\033]11;#102030\\007\\033]11;?\\007'; \
         r=$(dd bs=1 count=24 2>/dev/null); stty sane; \
         case \"$r\" in *1010/2020/3030*) printf OK;; *) printf NO;; esac",
    );
    loop {
        match rx
            .recv_timeout(Duration::from_secs(5))
            .expect("shell did not exit")
        {
            TermEvent::ColorQuery => handle.answer_color_queries(),
            TermEvent::Exit => break,
            _ => {}
        }
    }
    assert!(screen_contains(&handle, "OK"));
}

fn osc8_links_with_unknown_schemes_are_ignored(engine: Engine) {
    let (handle, rx) = spawn(
        engine,
        "printf '\\033]8;;ms-msdt:/id x\\033\\\\click\\033]8;;\\033\\\\'",
    );
    wait_for_exit(&rx);

    assert_eq!(line_text(&handle, 0), "click");
    assert_eq!(handle.link_at(at(2, 0)), None);
}

fn every_visible_match_is_highlighted(engine: Engine) {
    let (handle, rx) = spawn(engine, "printf 'ab ab\\r\\nx ab'");
    wait_for_exit(&rx);
    let mut search = nuntio_term::Search::new("ab", false).unwrap();
    let snapshot = handle.search_snapshot(&mut search);

    let marked = |line: usize| -> String {
        (0..5)
            .map(|c| {
                let bg = snapshot.cell(c, line).bg;
                if bg == snapshot.background { '.' } else { '#' }
            })
            .collect()
    };
    assert_eq!(marked(0), "##.##");
    assert_eq!(marked(1), "..##.");
}

/// More output than the scrollback holds, so the oldest lines are dropped
/// while new ones arrive.
fn output_beyond_the_scrollback(engine: Engine) {
    let (handle, rx) = spawn(
        engine,
        "i=1; while [ $i -le 300 ]; do echo line$i; i=$((i+1)); done",
    );
    wait_for_exit(&rx);
    assert_eq!(line_text(&handle, 3), "line300");

    // 100 lines of history above the 5 on screen.
    handle.scroll(1000);
    assert_eq!(line_text(&handle, 0), "line197");
    handle.start_selection(SelectionKind::Lines, at(0, 0));
    assert_eq!(handle.selection_text().as_deref(), Some("line197\n"));

    let mut dropped = nuntio_term::Search::new("line150", false).unwrap();
    assert!(!handle.search(&mut dropped, true));
    let mut kept = nuntio_term::Search::new("line250", false).unwrap();
    assert!(handle.search(&mut kept, true));
    assert!(screen_contains(&handle, "line250"));
}

fn scrollback_can_shrink(engine: Engine) {
    let (handle, rx) = spawn(
        engine,
        "i=1; while [ $i -le 50 ]; do echo line$i; i=$((i+1)); done",
    );
    wait_for_exit(&rx);
    // line47 to line50 and an empty line are on screen, the rest is history.
    handle.scroll(1000);
    assert_eq!(line_text(&handle, 0), "line1");

    handle.set_options(TermOptions {
        scrollback: 10,
        ..OPTIONS
    });
    handle.scroll(1000);
    assert_eq!(line_text(&handle, 0), "line37");
}

fn clipboard_writes_can_be_denied(engine: Engine) {
    // OSC 52 with "hi" in base64.
    let script = "printf '\\033]52;c;aGk=\\a'";
    let stored = |clipboard_write| {
        let term = TermOptions {
            clipboard_write,
            ..OPTIONS
        };
        let (_handle, rx) = spawn_with(engine, script, Vec::new(), term);
        wait_for_exit(&rx).contains(&TermEvent::ClipboardStore("hi".into()))
    };
    assert!(stored(true));
    assert!(!stored(false));
}

fn kitty_keyboard_can_be_turned_off(engine: Engine) {
    // Push "disambiguate escape codes" onto the keyboard mode stack.
    let script = "printf '\\033[>1u'";
    let mode = |kitty_keyboard| {
        let term = TermOptions {
            kitty_keyboard,
            ..OPTIONS
        };
        let (handle, rx) = spawn_with(engine, script, Vec::new(), term);
        wait_for_exit(&rx);
        handle.mode()
    };
    assert!(mode(true).contains(TermMode::DISAMBIGUATE_ESC_CODES));
    assert!(!mode(false).intersects(TermMode::KITTY_KEYBOARD_PROTOCOL));
}

/// Every test above, once per engine.
macro_rules! engine_tests {
    ($($(#[$attr:meta])* $name:ident),* $(,)?) => {
        mod alacritty {
            use super::*;
            $( $(#[$attr])* #[test] fn $name() { super::$name(Engine::Alacritty) } )*
        }
        #[cfg(feature = "wezterm")]
        mod wezterm {
            use super::*;
            $( $(#[$attr])* #[test] fn $name() { super::$name(Engine::Wezterm) } )*
        }
    };
}

engine_tests!(
    output_reaches_the_grid,
    environment_and_size,
    extra_environment,
    title_is_reported,
    selection_kinds,
    scrolling_stops_at_both_ends,
    #[cfg(target_os = "linux")]
    foreground_process_and_directory,
    #[cfg(unix)]
    a_command_is_not_an_idle_shell,
    reported_directory,
    #[cfg(target_os = "macos")]
    login_shell_is_seen_behind_login,
    search_through_scrollback,
    links_in_text_and_osc8,
    links_across_wrapped_lines,
    search_survives_a_cleared_scrollback,
    wide_characters_in_a_one_column_pane,
    color_queries_see_runtime_overrides,
    osc8_links_with_unknown_schemes_are_ignored,
    every_visible_match_is_highlighted,
    scrollback_can_shrink,
    output_beyond_the_scrollback,
    clipboard_writes_can_be_denied,
    kitty_keyboard_can_be_turned_off,
);
