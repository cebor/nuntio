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

use crate::state::{App, Key, Sources, Store, Tone};

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

/// `$VISUAL`, else `$EDITOR`, else a default editor. A shell command
/// like `code --wait` or `"C:\Program Files\…\code.exe" --wait`.
fn editor() -> String {
    ["VISUAL", "EDITOR"]
        .iter()
        .filter_map(std::env::var_os)
        .map(|value| value.to_string_lossy().trim().to_owned())
        .find(|editor| !editor.is_empty())
        .unwrap_or_else(|| if cfg!(windows) { "notepad" } else { "vi" }.into())
}

/// A short name for the editor command, for the footer: the program's file
/// name without directory, extension and arguments (`"C:\…\Code.exe" --wait`
/// is `Code`). Names longer than 10 characters become `editor`, so the
/// footer's length stays predictable.
fn editor_name(command: &str) -> String {
    let command = command.trim_start();
    let program = match command.chars().next() {
        Some(quote @ ('"' | '\'')) => command[1..].split(quote).next(),
        _ => command.split_whitespace().next(),
    }
    .unwrap_or_default();
    let file = program.rsplit(['/', '\\']).next().unwrap_or_default();
    let name = match file.rsplit_once('.') {
        Some((stem, ext))
            if ["exe", "cmd", "bat", "com"]
                .iter()
                .any(|e| ext.eq_ignore_ascii_case(e)) =>
        {
            stem
        }
        _ => file,
    };
    match name.chars().count() {
        1..=10 => name.into(),
        _ => "editor".into(),
    }
}

/// The process that runs `editor` on `path`: through the shell, like git
/// does, so quoted paths and arguments in the variable work and Windows
/// finds `code.cmd`.
fn editor_process(editor: &str, path: &Path) -> std::process::Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;

        // `/S` strips just the outer quotes. Windows paths can't contain `"`.
        let mut command = std::process::Command::new("cmd");
        command.raw_arg(format!("/S /C \"{editor} \"{}\"\"", path.display()));
        command
    }
    #[cfg(not(windows))]
    {
        let mut command = std::process::Command::new("sh");
        command
            .arg("-c")
            .arg(format!("{editor} \"$@\""))
            .arg("sh")
            .arg(path);
        command
    }
}

/// Run the editor on `path` and wait for it. The caller leaves and
/// re-enters the TUI around it.
fn run_editor(path: &Path) -> Result<()> {
    let editor = editor();
    let status = editor_process(&editor, path)
        .status()
        .with_context(|| format!("can't start the editor `{editor}`"))?;
    anyhow::ensure!(status.success(), "the editor `{editor}` {status}");
    Ok(())
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
    app.editor_name = editor_name(&editor());

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
            if app.open_editor {
                app.open_editor = false;
                ratatui::restore();
                let edited = run_editor(&path);
                terminal = ratatui::init();
                match edited {
                    Ok(()) => app.reload(),
                    Err(err) => app.message = Some((Tone::Error, format!("{err:#}"))),
                }
            }
        }
        Ok(())
    })();
    ratatui::restore();
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editor_names() {
        for (command, name) in [
            ("vim", "vim"),
            ("hx", "hx"),
            ("  /usr/bin/nvim -p", "nvim"),
            ("code --wait", "code"),
            (
                r#""C:\Program Files\Microsoft VS Code\Code.exe" --wait"#,
                "Code",
            ),
            ("'/opt/my editor/kak'", "kak"),
            ("code.cmd", "code"),
            ("notepad", "notepad"),
            ("emacsclient -t", "editor"),
            ("", "editor"),
            ("\"\"", "editor"),
        ] {
            assert_eq!(editor_name(command), name, "{command}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn editor_runs_through_the_shell() {
        let dir = std::env::temp_dir().join(format!("nuntio-editor-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("my config.toml");
        std::fs::write(&path, "").unwrap();
        let runs = |editor: &str| editor_process(editor, &path).status().unwrap().success();
        // Quoted programs, arguments, and a path with a space.
        assert!(runs("'/bin/sh' -c 'test -f \"$1\"' sh"));
        assert!(runs("test -f"));
        assert!(!runs("test -d"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
