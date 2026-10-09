use nuntio_term::TermEvent;

use crate::actions::Action;
use crate::sysmon::Sample;
use crate::update::Checked;

/// Identifies a pane across tabs and splits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PaneId(pub u64);

/// Where a paste reads from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PasteSource {
    Clipboard,
    /// The primary selection, which only Linux has.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    Primary,
}

/// What a paste found.
#[derive(Debug)]
pub enum Pasted {
    Nothing,
    Text(String),
    Image(arboard::ImageData<'static>),
    /// An image nuntio can't decode; the program may read it itself.
    UnreadableImage,
}

/// The result of reading the clipboard for a paste into `pane`.
#[derive(Debug)]
pub struct ClipboardPaste {
    pub pane: PaneId,
    pub source: PasteSource,
    pub content: Pasted,
}

/// Events sent to the main thread from PTY-IO and watcher threads.
#[derive(Debug)]
pub enum UserEvent {
    Term(PaneId, TermEvent),
    /// The config file or a theme file changed on disk.
    ConfigChanged,
    /// A new reading for the status bar.
    SystemStats(Sample),
    /// The answer of an update check.
    Update(Checked),
    /// A command from the macOS menu bar.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    Menu(MenuCommand),
    /// A request from the debug server.
    #[cfg(feature = "debug-server")]
    Debug(crate::app::debug_server::DebugCall),
    /// Clipboard contents read on a worker thread, for a paste.
    Paste(ClipboardPaste),
    /// Opening a link failed or was refused; the text is for the user.
    LinkFailed(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub enum MenuCommand {
    Action(Action),
    /// Quit, after asking if programs still run.
    Quit,
}
