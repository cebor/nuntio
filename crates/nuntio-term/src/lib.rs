//! Terminal emulation and the PTY of a pane, with `alacritty_terminal` or
//! (feature `wezterm`) `wezterm-term` as the emulator core.

mod alacritty;
mod osc_cwd;
mod palette;
mod pane;
mod process;
mod search;
mod snapshot;
mod url;
#[cfg(feature = "wezterm")]
mod wezterm;

pub use alacritty_terminal::term::TermMode;
pub use alacritty_terminal::vte::ansi::Rgb;
pub use osc_cwd::ReportedDir;
pub use palette::{Palette, rgb};
pub use pane::{
    Engine, GridPoint, SelectionKind, Shell, SpawnError, SpawnOptions, TermEvent, TermHandle,
    TermOptions, TermSize,
};
pub use search::{MatchPosition, Search, SearchError};
pub use snapshot::{
    CellStyle, CursorStyle, Snapshot, SnapshotCell, SnapshotCursor, UnderlineStyle,
};
pub use url::Link;
