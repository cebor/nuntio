//! Thin wrapper around `alacritty_terminal` and the PTY of a pane.

mod palette;
mod pane;
mod process;
mod search;
mod snapshot;
mod url;

pub use alacritty_terminal::term::TermMode;
pub use alacritty_terminal::vte::ansi::Rgb;
pub use palette::Palette;
pub use pane::{
    GridPoint, SelectionKind, Shell, SpawnError, SpawnOptions, TermEvent, TermHandle, TermOptions,
    TermSize,
};
pub use search::{MatchPosition, Search, SearchError};
pub use snapshot::{
    CellStyle, CursorStyle, Snapshot, SnapshotCell, SnapshotCursor, UnderlineStyle,
};
pub use url::Link;
