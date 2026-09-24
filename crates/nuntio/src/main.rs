// Release builds on Windows are GUI apps without a console window.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod actions;
mod app;
mod banner;
mod event;
mod ime;
mod input;
mod mouse;
mod pane_tree;
mod search_bar;
mod tab_bar;
mod tabs;
mod window;

use std::io::IsTerminal;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use nuntio_config::Config;
use tracing_subscriber::EnvFilter;
use winit::event_loop::EventLoop;

use crate::app::App;
use crate::banner::{Banner, Severity};
use crate::event::UserEvent;

#[derive(Debug, Default)]
struct Args {
    config: Option<PathBuf>,
    log_level: Option<String>,
}

fn parse_args() -> Result<Args, lexopt::Error> {
    use lexopt::prelude::*;

    let mut args = Args::default();
    let mut parser = lexopt::Parser::from_env();
    while let Some(arg) = parser.next()? {
        match arg {
            Long("config") => args.config = Some(parser.value()?.into()),
            Long("log-level") => args.log_level = Some(parser.value()?.string()?),
            Short('h') | Long("help") => {
                println!("Usage: nuntio [--config <path>] [--log-level <level>]");
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

/// Load the config for startup. Unlike a reload, an invalid file doesn't
/// stop nuntio: it starts with defaults and shows the error.
fn load_config(path: Option<&Path>) -> (Config, Option<Banner>) {
    let Some(path) = path else {
        tracing::warn!("no home directory, using the default config");
        return (Config::default(), None);
    };
    match nuntio_config::load(path) {
        Ok(loaded) => {
            tracing::info!(path = %path.display(), "config loaded");
            for warning in &loaded.warnings {
                tracing::warn!("{warning}");
            }
            (
                loaded.config,
                Banner::new(Severity::Warning, loaded.warnings),
            )
        }
        Err(err) => {
            tracing::error!("{err}");
            let banner = Banner::new(Severity::Error, vec![err.to_string()]);
            (Config::default(), banner)
        }
    }
}

/// Log file for runs without a terminal, replaced on every start:
/// `~/.cache/nuntio/nuntio.log` (the platform's cache directory).
fn log_file() -> Option<std::fs::File> {
    let dir = dirs::cache_dir()?.join("nuntio");
    std::fs::create_dir_all(&dir).ok()?;
    std::fs::File::create(dir.join("nuntio.log")).ok()
}

fn main() -> Result<()> {
    let args = parse_args()?;

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

    let config_path = args.config.or_else(nuntio_config::default_config_path);
    let (config, banner) = load_config(config_path.as_deref());

    let event_loop = EventLoop::<UserEvent>::with_user_event()
        .build()
        .context("failed to create event loop")?;
    let mut app = App::new(config, config_path, banner, event_loop.create_proxy());
    event_loop.run_app(&mut app).context("event loop failed")?;
    app.into_result()
}
