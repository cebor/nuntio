mod app;
mod event;
mod input;

use std::path::PathBuf;

use anyhow::{Context, Result};
use tracing_subscriber::EnvFilter;
use winit::event_loop::EventLoop;

use crate::app::App;
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

fn main() -> Result<()> {
    let args = parse_args()?;

    let filter = match &args.log_level {
        Some(level) => EnvFilter::try_new(level)?,
        None => EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
    };
    tracing_subscriber::fmt().with_env_filter(filter).init();

    // Loading from `args.config` / the default path follows in M4.
    if let Some(path) = &args.config {
        tracing::warn!(path = %path.display(), "config loading not implemented yet, using defaults");
    }
    let config = nuntio_config::Config::default();

    let event_loop = EventLoop::<UserEvent>::with_user_event()
        .build()
        .context("failed to create event loop")?;
    let mut app = App::new(config, event_loop.create_proxy());
    event_loop.run_app(&mut app).context("event loop failed")?;
    app.into_result()
}
