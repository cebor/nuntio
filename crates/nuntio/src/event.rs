use nuntio_term::TermEvent;

use crate::actions::Action;
use crate::sysmon::Sample;

/// Identifies a pane across tabs and splits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PaneId(pub u64);

/// Events sent to the main thread from PTY-IO and watcher threads.
#[derive(Debug)]
pub enum UserEvent {
    Term(PaneId, TermEvent),
    /// The config file or a theme file changed on disk.
    ConfigChanged,
    /// A new reading for the status bar.
    SystemStats(Sample),
    /// A command from the macOS menu bar.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    Menu(MenuCommand),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub enum MenuCommand {
    Action(Action),
    /// Quit, after asking if programs still run.
    Quit,
}
