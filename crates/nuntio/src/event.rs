use nuntio_term::TermEvent;

/// Identifies a pane across tabs and splits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PaneId(pub u64);

/// Events sent to the main thread from PTY-IO and watcher threads.
#[derive(Debug)]
pub enum UserEvent {
    Term(PaneId, TermEvent),
    /// The config file or a theme file changed on disk.
    ConfigChanged,
}
