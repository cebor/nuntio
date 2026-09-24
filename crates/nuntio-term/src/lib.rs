//! Thin wrapper around `alacritty_terminal` and the PTY of a pane.

mod palette;
mod pane;
mod snapshot;

pub use alacritty_terminal::term::TermMode;
pub use alacritty_terminal::vte::ansi::Rgb;
pub use palette::Palette;
pub use pane::{Shell, SpawnError, SpawnOptions, TermEvent, TermHandle, TermSize};
pub use snapshot::{CellStyle, CursorStyle, Snapshot, SnapshotCell, SnapshotCursor};
