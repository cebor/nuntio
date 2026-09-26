// Release builds on Windows are GUI apps without a console window.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod actions;
mod app;
mod banner;
mod event;
mod ime;
mod input;
mod kitty_keys;
mod link;
#[cfg(target_os = "macos")]
mod macos_menu;
mod mouse;
mod pane_env;
mod pane_tree;
mod search_bar;
mod status_bar;
mod sysmon;
mod tab_bar;
mod tab_title;
mod tabs;
mod window;

use std::io::IsTerminal;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use nuntio_config::Config;
use tracing_subscriber::EnvFilter;
use winit::event_loop::EventLoop;

use crate::app::{App, Startup};
use crate::banner::{Banner, Severity};
use crate::event::UserEvent;

const HELP: &str = "\
nuntio: a GPU-rendered terminal emulator with tabs and split panes

Usage: nuntio [options] [[-e] <command> [<args>...]]

Options:
  -e, --command <command> [<args>...]
                                Run a command instead of the shell in the first
                                tab; everything after it is passed to the command
      --working-directory <dir> Start the first tab in this directory
      --config <path>           Use this config file instead of the default one
      --log-level <level>       Log filter, such as `debug` or `nuntio=trace`
  -h, --help                    Print this help
  -V, --version                 Print the version
";

#[derive(Debug, Default)]
struct Args {
    config: Option<PathBuf>,
    log_level: Option<String>,
    startup: Startup,
}

fn parse_args() -> Result<Args, lexopt::Error> {
    use lexopt::prelude::*;

    let mut args = Args::default();
    let mut parser = lexopt::Parser::from_env();
    while let Some(arg) = parser.next()? {
        match arg {
            Long("config") => args.config = Some(parser.value()?.into()),
            Long("log-level") => args.log_level = Some(parser.value()?.string()?),
            Long("working-directory") => {
                args.startup.working_directory = Some(parser.value()?.into());
            }
            // The command takes all remaining arguments, options included.
            Short('e') | Long("command") => {
                let program = parser.value()?.string()?;
                args.startup.command = Some(command(program, &mut parser)?);
            }
            Value(program) => {
                args.startup.command = Some(command(program.string()?, &mut parser)?);
            }
            Short('h') | Long("help") => {
                print!("{HELP}");
                std::process::exit(0);
            }
            Short('V') | Long("version") => {
                println!("nuntio {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            _ => return Err(arg.unexpected()),
        }
    }
    Ok(args)
}

/// `program` followed by the rest of the command line.
fn command(program: String, parser: &mut lexopt::Parser) -> Result<Vec<String>, lexopt::Error> {
    let mut argv = vec![program];
    for arg in parser.raw_args()? {
        argv.push(arg.into_string().map_err(lexopt::Error::NonUnicodeValue)?);
    }
    Ok(argv)
}

/// Load the config for startup. Unlike a reload, an invalid file doesn't
/// stop nuntio: it starts with defaults and shows the error. `warnings`
/// are shown along with the config's own.
fn load_config(path: Option<&Path>, mut warnings: Vec<String>) -> (Config, Option<Banner>) {
    let Some(path) = path else {
        tracing::warn!("no home directory, using the default config");
        return (Config::default(), None);
    };
    match nuntio_config::load(path) {
        Ok(loaded) => {
            tracing::info!(path = %path.display(), "config loaded");
            warnings.extend(loaded.warnings);
            for warning in &warnings {
                tracing::warn!("{warning}");
            }
            (loaded.config, Banner::config(Severity::Warning, warnings))
        }
        Err(err) => {
            tracing::error!("{err}");
            let banner = Banner::config(Severity::Error, vec![err.to_string()]);
            (Config::default(), banner)
        }
    }
}

/// Log file for runs without a terminal: `~/.cache/nuntio/nuntio.log` (the
/// platform's cache directory). The previous run's log is kept as
/// `nuntio.old.log`, so starting a second window doesn't wipe the first
/// one's log.
fn log_file() -> Option<std::fs::File> {
    let dir = dirs::cache_dir()?.join("nuntio");
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join("nuntio.log");
    // Fails on Windows while another instance has it open; then it's
    // truncated like before.
    let _ = std::fs::rename(&path, dir.join("nuntio.old.log"));
    std::fs::File::create(path).ok()
}

/// Load DLLs only from our own directory and System32, not from `PATH`.
///
/// alacritty_terminal prefers a `conpty.dll` over the ConPTY built into
/// Windows and looks it up by name, so by default it takes whatever copy
/// some other program put on `PATH` (WezTerm ships one). With the 1.22 copy
/// every shell waited 3 seconds for its first prompt. A `conpty.dll` next to
/// nuntio.exe is still found.
#[cfg(windows)]
fn restrict_dll_search() {
    use windows_sys::Win32::System::LibraryLoader::{
        LOAD_LIBRARY_SEARCH_DEFAULT_DIRS, SetDefaultDllDirectories,
    };

    // SAFETY: only changes the search path for later LoadLibrary calls.
    if unsafe { SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_DEFAULT_DIRS) } == 0 {
        tracing::warn!(
            error = %std::io::Error::last_os_error(),
            "failed to restrict the DLL search path"
        );
    }
}

fn main() -> Result<()> {
    let mut args = parse_args()?;

    let filter = match &args.log_level {
        Some(level) => EnvFilter::try_new(level)?,
        None => EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
    };
    // Started from a desktop launcher there is no terminal to log to.
    match (!std::io::stderr().is_terminal()).then(log_file).flatten() {
        Some(file) => tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_ansi(false)
            .with_writer(std::sync::Mutex::new(file))
            .init(),
        None => tracing_subscriber::fmt().with_env_filter(filter).init(),
    }
    #[cfg(windows)]
    restrict_dll_search();
    // Started from the Finder or Dock, nuntio runs in `/`. Start the first
    // shell at home instead, like Terminal.app (`login`, which runs the
    // shell, stays in the current directory).
    if cfg!(target_os = "macos")
        && args.startup.working_directory.is_none()
        && std::env::current_dir().is_ok_and(|dir| dir == Path::new("/"))
    {
        args.startup.working_directory = dirs::home_dir();
    }

    let mut warnings = Vec::new();
    let config_path = args.config.or_else(|| {
        let location = nuntio_config::locate_config()?;
        if let Some(shadowed) = location.shadowed {
            warnings.push(format!(
                "{} is ignored because {} exists",
                shadowed.display(),
                location.path.display()
            ));
        }
        Some(location.path)
    });
    let (config, banner) = load_config(config_path.as_deref(), warnings);

    let mut builder = EventLoop::<UserEvent>::with_user_event();
    // nuntio sets up its own menu bar (`macos_menu`).
    #[cfg(target_os = "macos")]
    winit::platform::macos::EventLoopBuilderExtMacOS::with_default_menu(&mut builder, false);
    let event_loop = builder.build().context("failed to create event loop")?;
    let mut app = App::new(
        config,
        config_path,
        banner,
        args.startup,
        event_loop.create_proxy(),
    );
    event_loop.run_app(&mut app).context("event loop failed")?;
    app.into_result()
}
