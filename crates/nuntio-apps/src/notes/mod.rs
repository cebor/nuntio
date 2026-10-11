//! `nuntio-apps notes`: Markdown notes, one `.md` file per note in one
//! folder, edited with a live preview.

mod buffer;
mod edit;
mod layout;
mod markdown;
mod state;
mod store;
mod ui;
mod widgets;

use std::path::PathBuf;
use std::time::Instant;

use anyhow::{Context, Result};
use ratatui::crossterm::event::{self, Event};

use crate::term;
use state::{App, Tone};
use store::{DirStore, display_path};

/// Set by nuntio in its panes: the config file it uses.
const CONFIG_ENV: &str = "NUNTIO_CONFIG";

const HELP: &str = "Usage: nuntio-apps notes [--dir <path>] [--config <path>]\n\nMarkdown notes, saved as .md files in one folder.\n\nOptions:\n  --dir <path>     Use this folder instead of notes.directory\n  --config <path>  Read notes.directory from this config file\n  -h, --help       Print this help\n";

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

/// The notes folder and a warning to show at startup. `Err`: no folder.
fn notes_dir(args: &Args) -> Result<(PathBuf, Option<String>), String> {
    if let Some(dir) = &args.dir {
        return std::path::absolute(dir)
            .map(|dir| (dir, None))
            .map_err(|err| format!("Cannot open the notes folder {}: {err}", dir.display()));
    }
    let path = args
        .config
        .clone()
        .or_else(|| std::env::var_os(CONFIG_ENV).map(PathBuf::from))
        .or_else(|| nuntio_config::locate_config().map(|l| l.path));
    let (notes, warning) = match path.map(|p| nuntio_config::load(&p)) {
        Some(Ok(loaded)) => (loaded.config.notes, None),
        Some(Err(err)) => (
            nuntio_config::Notes::default(),
            Some(format!("config: {}", err.message)),
        ),
        None => (nuntio_config::Notes::default(), None),
    };
    notes
        .resolved_directory()
        .map(|dir| (dir, warning))
        .ok_or_else(|| "No home directory for the notes folder".to_owned())
}

pub fn run(parser: lexopt::Parser) -> Result<()> {
    let Some(args) = parse_args(parser)? else {
        return Ok(());
    };
    let mut app = match notes_dir(&args) {
        Ok((dir, warning)) => {
            let label = display_path(&dir);
            match std::fs::create_dir_all(&dir) {
                Ok(()) => {
                    let mut app = App::new(Box::new(DirStore(dir)), label);
                    if let Some(warning) = warning {
                        app.message = Some((Tone::Warn, warning));
                    }
                    app
                }
                Err(err) => {
                    let message = format!("Cannot open the notes folder {label}: {err}");
                    App::failed(Box::new(DirStore(dir)), label, message)
                }
            }
        }
        // A process that exits closes its pane: errors are shown in the TUI.
        Err(message) => App::failed(Box::new(DirStore(PathBuf::new())), String::new(), message),
    };

    let mut terminal = term::init()?;
    let result = (|| -> Result<()> {
        // Starts crossterm's event reader, which only then listens for
        // resizes: nuntio resizes a new split's pane right after starting it.
        event::poll(std::time::Duration::ZERO)?;
        let mut redraw = true;
        while !app.quit {
            if redraw {
                // Lay out for the size the frame is drawn at.
                terminal.draw(|frame| {
                    app.prepare(frame.area().as_size());
                    ui::draw(frame, &app);
                })?;
                if let Some(text) = app.take_clipboard() {
                    term::copy_to_clipboard(&text)?;
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
                Some(Event::Key(key)) => term::normalize(key).is_some_and(|k| app.key(k, now)),
                Some(Event::Paste(text)) => app.paste(&text, now),
                Some(Event::Mouse(mouse)) => app.mouse(mouse, now),
                Some(Event::FocusGained) => {
                    app.refresh();
                    true
                }
                Some(Event::Resize(..)) => true,
                _ => false,
            };
        }
        Ok(())
    })();
    term::restore();
    result.context("the notes app failed")
}
