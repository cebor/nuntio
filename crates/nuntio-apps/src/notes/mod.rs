//! `nuntio-apps notes`: Markdown notes, one `.md` file per note in one
//! folder, edited with a live preview.

mod areas;
mod buffer;
mod edit;
mod failed;
mod layout;
mod markdown;
mod state;
mod store;
mod ui;
mod widgets;

use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result};
use ratatui::crossterm::event::{self, Event};

use failed::Failed;
use nuntio_tui::style::Tone;
use nuntio_tui::terminal::Modes;
use state::App;
use store::DirStore;

/// Mouse clicks and focus changes both matter to the notes.
const MODES: Modes = Modes {
    mouse: true,
    focus: true,
};
const HELP: &str = "Usage: nuntio-apps notes [--dir <path>] [--config <path>]\n\nMarkdown notes, saved as .md files in one folder.\n\nOptions:\n  --dir <path>     Use this folder instead of notes.directory\n  --config <path>  Read the [notes] settings from this config file\n  -h, --help       Print this help\n";

struct Args {
    dir: Option<PathBuf>,
    config: Option<PathBuf>,
}

/// `None`: the help was printed.
fn parse_args(mut parser: lexopt::Parser) -> Result<Option<Args>, lexopt::Error> {
    use lexopt::prelude::*;
    let mut args = Args {
        dir: None,
        config: None,
    };
    while let Some(arg) = parser.next()? {
        match arg {
            Long("dir") => args.dir = Some(parser.value()?.into()),
            Long("config") => args.config = Some(parser.value()?.into()),
            Short('h') | Long("help") => {
                print!("{HELP}");
                return Ok(None);
            }
            _ => return Err(arg.unexpected()),
        }
    }
    Ok(Some(args))
}

/// The `[notes]` settings at `config` and a warning to show at startup.
fn notes_settings(config: Option<&Path>) -> (nuntio_config::Notes, Option<String>) {
    match config.map(nuntio_config::load) {
        Some(Ok(loaded)) => (loaded.config.notes, None),
        Some(Err(err)) => (
            nuntio_config::Notes::default(),
            Some(format!("config: {}", err.message)),
        ),
        None => (nuntio_config::Notes::default(), None),
    }
}

/// The notes folder. `Err`: no folder.
fn notes_dir(args: &Args, notes: &nuntio_config::Notes) -> Result<PathBuf, String> {
    if let Some(dir) = &args.dir {
        return std::path::absolute(dir)
            .map_err(|err| format!("Cannot open the notes folder {}: {err}", dir.display()));
    }
    notes
        .resolved_directory()
        .ok_or_else(|| "No home directory for the notes folder".to_owned())
}

/// The app, or the reason the notes folder can't be used.
fn open(args: &Args, config: Option<&Path>) -> Result<App, Failed> {
    let (notes, warning) = notes_settings(config);
    match notes_dir(args, &notes) {
        Ok(dir) => {
            let label = nuntio_config::tilde_path(&dir, dirs::home_dir().as_deref());
            match std::fs::create_dir_all(&dir) {
                Ok(()) => {
                    let mut app = App::new(Box::new(DirStore(dir)), label, notes.sidebar_width);
                    if let Some(warning) = warning {
                        app.message = Some((Tone::Warn, warning));
                    }
                    Ok(app)
                }
                Err(err) => Err(Failed {
                    message: format!("Cannot open the notes folder {label}: {err}"),
                    dir_label: label,
                }),
            }
        }
        // A process that exits closes its pane: errors are shown in the TUI.
        Err(message) => Err(Failed {
            message,
            dir_label: String::new(),
        }),
    }
}

pub fn run(parser: lexopt::Parser) -> Result<()> {
    let Some(args) = parse_args(parser)? else {
        return Ok(());
    };
    let config = nuntio_config::config_path(args.config.clone());
    let opened = open(&args, config.as_deref());

    nuntio_tui::terminal::run(MODES, |terminal| match opened {
        Ok(app) => run_notes(terminal, app, config.as_deref()),
        Err(failed) => run_failed(terminal, &failed),
    })
    .context("the notes app failed")
}

/// Show the failure until the user quits.
fn run_failed(terminal: &mut ratatui::DefaultTerminal, failed: &Failed) -> Result<()> {
    loop {
        terminal.draw(|frame| ui::draw_failed(frame, failed))?;
        if let Event::Key(key) = event::read()?
            && nuntio_tui::terminal::normalize_key(key).is_some_and(Failed::quits)
        {
            return Ok(());
        }
    }
}

fn run_notes(
    terminal: &mut ratatui::DefaultTerminal,
    mut app: App,
    config: Option<&Path>,
) -> Result<()> {
    let mut redraw = true;
    while !app.quit {
        if redraw {
            // Lay out for the size the frame is drawn at.
            terminal.draw(|frame| {
                app.prepare(frame.area().as_size());
                ui::draw(frame, &app);
            })?;
            if let Some(text) = app.take_clipboard() {
                nuntio_tui::terminal::copy_to_clipboard(&text)?;
            }
        }
        // Blocks until there is input or an autosave is due: no CPU use
        // while idle.
        let event = match app.deadline() {
            Some(at) => {
                let wait = at.saturating_duration_since(Instant::now());
                if event::poll(wait)? {
                    Some(event::read()?)
                } else {
                    None
                }
            }
            None => Some(event::read()?),
        };
        let now = Instant::now();
        redraw = match event {
            None => {
                app.tick(now);
                true
            }
            Some(Event::Key(key)) => {
                nuntio_tui::terminal::normalize_key(key).is_some_and(|k| app.key(k, now))
            }
            Some(Event::Paste(text)) => app.paste(&text, now),
            Some(Event::Mouse(mouse)) => app.mouse(mouse, now),
            Some(Event::FocusGained) => {
                app.refresh();
                true
            }
            Some(Event::Resize(..)) => true,
            _ => false,
        };
        if let Some(width) = app.take_sidebar_width()
            && let Some(path) = config
            && let Err(err) =
                nuntio_config::set_in_file(path, "notes.sidebar_width", i64::from(width))
        {
            app.message = Some((Tone::Warn, format!("Cannot save the list width: {err}")));
            redraw = true;
        }
    }
    Ok(())
}
