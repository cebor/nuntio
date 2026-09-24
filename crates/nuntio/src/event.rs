use nuntio_term::TermEvent;

/// Identifies a pane across tabs and splits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PaneId(pub u64);

/// Events sent to the main thread from PTY-IO and watcher threads.
#[derive(Debug)]
pub enum UserEvent {
    #[allow(dead_code)] // the pane id routes events once there are tabs (M3)
    Term(PaneId, TermEvent),
    #[allow(dead_code)] // producer arrives with the config watcher in M4
    ConfigReloaded(Box<nuntio_config::Config>),
}
