//! `nuntio-apps`: terminal apps that nuntio opens in its panes. One binary
//! with a subcommand per app; only `notes` so far.

mod notes;
mod term;

use anyhow::Result;

const HELP: &str = "Usage: nuntio-apps <command>\n\nCommands:\n  notes  Markdown notes\n\nOptions:\n  -V, --version  Print the version\n  -h, --help     Print this help\n";

pub fn main() -> Result<()> {
    use lexopt::prelude::*;
    let mut parser = lexopt::Parser::from_env();
    match parser.next()? {
        Some(Short('h') | Long("help")) => {
            print!("{HELP}");
            Ok(())
        }
        Some(Short('V') | Long("version")) => {
            println!("nuntio-apps {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Some(Value(command)) if command == "notes" => notes::run(parser),
        _ => anyhow::bail!("unknown command; run `nuntio-apps --help`"),
    }
}
