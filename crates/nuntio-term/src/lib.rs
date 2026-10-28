//! Thin wrapper around `alacritty_terminal` and the PTY of a pane.

mod backend;
mod image;
mod image_scan;
mod io_loop;
mod kitty;
mod osc_cwd;
mod palette;
mod pane;
mod process;
mod search;
mod snapshot;
mod sync;
#[cfg(test)]
mod test_support;
mod url;
#[cfg(windows)]
mod wsl_pipe;

pub use crate::image::{ImagePiece, TermImage};
pub use alacritty_terminal::term::TermMode;
pub use alacritty_terminal::vte::ansi::Rgb;
pub use osc_cwd::{ReportedDir, wsl_share};
pub use palette::{Palette, rgb};
pub use pane::{
    ForegroundInfo, GridPoint, Program, SelectionKind, Shell, SpawnError, SpawnOptions, TermEvent,
    TermHandle, TermOptions, TermSize, Transport, default_shell_name, is_local_windows_path,
};
pub use search::{MatchPosition, Search, SearchError};
pub use snapshot::{
    CellStyle, CursorStyle, MAX_ZEROWIDTH, Snapshot, SnapshotCell, SnapshotCursor, UnderlineStyle,
};
pub use url::{Link, LinkKind};
