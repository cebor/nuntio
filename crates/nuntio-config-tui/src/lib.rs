//! `nuntio-config`: a terminal UI for nuntio's config file. Started inside
//! nuntio, it edits the file that nuntio uses (`NUNTIO_CONFIG`), and every
//! change is applied live through nuntio's hot reload.

mod args;
mod detect;
mod state;
mod ui;
mod widgets;

use std::io;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use nuntio_config::ThemeSet;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::state::{App, Key, Sources, Store};

/// Set by nuntio in its panes: the config file it uses.
const CONFIG_ENV: &str = "NUNTIO_CONFIG";

struct FileStore(PathBuf);

impl Store for FileStore {
    fn read(&self) -> io::Result<Option<String>> {
        match std::fs::read_to_string(&self.0) {
            Ok(text) => Ok(Some(text)),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(err),
        }
    }

    fn write(&mut self, contents: &str) -> io::Result<()> {
        nuntio_config::write_config(&self.0, contents)
    }
}

fn parse_args() -> Result<Option<PathBuf>, lexopt::Error> {
    use lexopt::prelude::*;

    let mut config = None;
    let mut parser = lexopt::Parser::from_env();
    while let Some(arg) = parser.next()? {
        match arg {
            Long("config") => config = Some(parser.value()?.into()),
            Short('h') | Long("help") => {
                println!(
                    "Usage: nuntio-config [--config <path>]\n\n\
                     Edit nuntio's config file. Changes are written right away."
                );
                std::process::exit(0);
            }
            Short('V') | Long("version") => {
                println!("nuntio-config {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            _ => return Err(arg.unexpected()),
        }
    }
    Ok(config)
}

/// `--config`, else the file nuntio told us about, else the usual lookup.
fn config_path(arg: Option<PathBuf>) -> Result<PathBuf> {
    arg.or_else(|| std::env::var_os(CONFIG_ENV).map(PathBuf::from))
        .or_else(|| nuntio_config::locate_config().map(|l| l.path))
        .context("no home directory to look for the config in")
}

/// The path with the home directory shortened to `~`.
fn display_path(path: &Path) -> String {
    if let Some(home) = dirs::home_dir()
        && let Ok(rest) = path.strip_prefix(&home)
    {
        return Path::new("~").join(rest).display().to_string();
    }
    path.display().to_string()
}

/// Families of the installed monospace fonts, sorted.
fn installed_fonts() -> Vec<String> {
    let mut db = fontdb::Database::new();
    db.load_system_fonts();
    let mut families: Vec<String> = db
        .faces()
        .filter(|face| face.monospaced)
        .filter_map(|face| face.families.first().map(|(name, _)| name.clone()))
        .collect();
    families.sort_by_key(|f| f.to_lowercase());
    families.dedup();
    families
}

fn map_key(event: KeyEvent) -> Option<Key> {
    if event.kind == KeyEventKind::Release {
        return None;
    }
    let ctrl = event.modifiers.contains(KeyModifiers::CONTROL);
    let shift = event.modifiers.contains(KeyModifiers::SHIFT);
    Some(match event.code {
        KeyCode::Char(c) if ctrl => Key::Ctrl(c.to_ascii_lowercase()),
        KeyCode::Char(c) => Key::Char(c),
        KeyCode::Up if shift => Key::ShiftUp,
        KeyCode::Down if shift => Key::ShiftDown,
        KeyCode::Up => Key::Up,
        KeyCode::Down => Key::Down,
        KeyCode::Left => Key::Left,
        KeyCode::Right => Key::Right,
        KeyCode::PageUp => Key::PageUp,
        KeyCode::PageDown => Key::PageDown,
        KeyCode::Home => Key::Home,
        KeyCode::End => Key::End,
        KeyCode::Enter => Key::Enter,
        KeyCode::Esc => Key::Esc,
        KeyCode::Tab => Key::Tab,
        KeyCode::BackTab => Key::BackTab,
        KeyCode::Backspace => Key::Backspace,
        KeyCode::Delete => Key::Delete,
        _ => return None,
    })
}

pub fn main() -> Result<()> {
    let path = config_path(parse_args()?)?;
    let (themes, theme_warnings) = ThemeSet::load(nuntio_config::themes_dir(&path).as_deref());
    let mut app = App::new(
        Box::new(FileStore(path.clone())),
        display_path(&path),
        themes,
        Sources {
            fonts: Box::new(installed_fonts),
            shells: Box::new(detect::installed_shells),
            wsl_distributions: Box::new(detect::wsl_distributions),
        },
    )
    .map_err(anyhow::Error::msg)?;
    app.set_theme_warnings(theme_warnings);

    let mut view = ui::View::default();
    let mut terminal = ratatui::init();
    let result = (|| -> Result<()> {
        while !app.quit {
            terminal.draw(|frame| ui::draw(frame, &app, &mut view))?;
            // Blocks until there is input: no CPU use while idle.
            if let Event::Key(key) = event::read()?
                && let Some(key) = map_key(key)
            {
                app.key(key);
            }
        }
        Ok(())
    })();
    ratatui::restore();
    result
}
