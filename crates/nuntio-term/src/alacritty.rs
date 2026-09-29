//! The `alacritty` engine: `alacritty_terminal`'s `Term` and event loop.

use std::borrow::Cow;
use std::io;
use std::sync::{Arc, Mutex, OnceLock, PoisonError, RwLock};

use alacritty_terminal::event::{Event, EventListener, OnResize, WindowSize};
use alacritty_terminal::event_loop::{EventLoop, EventLoopSender, Msg};
use alacritty_terminal::grid::Scroll;
use alacritty_terminal::index::{Column, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::{self, Term, viewport_to_point};
use alacritty_terminal::tty::{self, ChildEvent, EventedPty, EventedReadWrite};
use alacritty_terminal::vte::ansi::Rgb;
use polling::{PollMode, Poller};

use crate::osc_cwd::{CwdScanner, ReportedDir};
use crate::palette::Palette;
use crate::pane::{
    EventSink, GridPoint, SelectionKind, SpawnError, SpawnOptions, TermEvent, TermOptions, TermSize,
};
use crate::search::{self, Search};
use crate::snapshot::Snapshot;
use crate::url::{self, Link};

type ColorFormat = Arc<dyn Fn(Rgb) -> String + Send + Sync>;

/// Bridges alacritty's events to the application and answers terminal
/// queries (DA, size reports) directly on the PTY thread. Color queries go
/// through the main thread, which can read the runtime overrides (OSC
/// 4/10/11/12): alacritty asks while the PTY thread holds the term lock.
#[derive(Clone)]
struct Listener {
    inner: Arc<ListenerInner>,
}

struct ListenerInner {
    sink: Arc<EventSink>,
    /// Set once the event loop exists; replies are written back through it.
    sender: OnceLock<EventLoopSender>,
    palette: RwLock<Palette>,
    size: Mutex<WindowSize>,
    /// Color queries waiting for [`AlacrittyPane::answer_color_queries`].
    color_queries: Mutex<Vec<(usize, ColorFormat)>>,
}

impl ListenerInner {
    fn write(&self, text: String) {
        if let Some(sender) = self.sender.get() {
            let _ = sender.send(Msg::Input(Cow::Owned(text.into_bytes())));
        }
    }
}

impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        let inner = &self.inner;
        let forward = match event {
            Event::Wakeup => return inner.sink.wakeup(),
            Event::Title(title) => return inner.sink.title(title),
            Event::ResetTitle => TermEvent::ResetTitle,
            Event::Bell => TermEvent::Bell,
            // `ChildExit` comes before the remaining output is drained;
            // `Exit` follows once the terminal is done.
            Event::Exit => TermEvent::Exit,
            Event::ChildExit(status) => {
                tracing::debug!(%status, "shell exited");
                return;
            }
            Event::ClipboardStore(_, text) => TermEvent::ClipboardStore(text),
            Event::PtyWrite(text) => return inner.write(text),
            Event::ColorRequest(index, format) => {
                inner
                    .color_queries
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push((index, format));
                TermEvent::ColorQuery
            }
            Event::TextAreaSizeRequest(format) => {
                let size = *inner.size.lock().unwrap_or_else(PoisonError::into_inner);
                return inner.write(format(size));
            }
            // Programs may not read the clipboard (OSC 52 queries), as in
            // xterm and iTerm2 by default: it could leak passwords. There is
            // no reply, so the program sees no answer at all.
            Event::ClipboardLoad(..) => {
                tracing::debug!("denied a clipboard read (OSC 52)");
                return;
            }
            // The cursor is re-read with every snapshot.
            Event::MouseCursorDirty | Event::CursorBlinkingChange => return,
        };
        inner.sink.send(forward);
    }
}

/// A pane run by alacritty's emulator and event loop.
pub(crate) struct AlacrittyPane {
    term: Arc<FairMutex<Term<Listener>>>,
    listener: Listener,
    sender: EventLoopSender,
}

impl AlacrittyPane {
    pub(crate) fn spawn(
        pty: tty::Pty,
        reported_dir: Arc<Mutex<Option<ReportedDir>>>,
        options: &SpawnOptions,
        size: TermSize,
        sink: Arc<EventSink>,
    ) -> Result<Self, SpawnError> {
        let listener = Listener {
            inner: Arc::new(ListenerInner {
                sink,
                sender: OnceLock::new(),
                palette: RwLock::new(options.palette.clone()),
                size: Mutex::new(size.window_size()),
                color_queries: Mutex::new(Vec::new()),
            }),
        };
        let config = term_config(options.term);
        let term = Arc::new(FairMutex::new(Term::new(config, &size, listener.clone())));
        let pty = TeePty {
            pty,
            scanner: CwdScanner::default(),
            reported: reported_dir,
        };
        let event_loop = EventLoop::new(term.clone(), listener.clone(), pty, true, false)
            .map_err(SpawnError::EventLoop)?;
        let sender = event_loop.channel();
        let _ = listener.inner.sender.set(sender.clone());
        event_loop.spawn();
        Ok(Self {
            term,
            listener,
            sender,
        })
    }

    pub(crate) fn write(&self, bytes: Cow<'static, [u8]>) {
        // Typing jumps back to the bottom, like every terminal does.
        self.term.lock().scroll_display(Scroll::Bottom);
        let _ = self.sender.send(Msg::Input(bytes));
    }

    pub(crate) fn scroll(&self, lines: i32) {
        self.term.lock().scroll_display(Scroll::Delta(lines));
    }

    pub(crate) fn scroll_page(&self, up: bool) {
        self.term
            .lock()
            .scroll_display(if up { Scroll::PageUp } else { Scroll::PageDown });
    }

    pub(crate) fn clear_history(&self) {
        use alacritty_terminal::vte::ansi::{ClearMode, Handler};
        let mut term = self.term.lock();
        term.scroll_display(Scroll::Bottom);
        term.clear_screen(ClearMode::Saved);
    }

    pub(crate) fn start_selection(&self, kind: SelectionKind, point: GridPoint) {
        let mut term = self.term.lock();
        let (point, side) = to_grid(point, term.grid().display_offset());
        term.selection = Some(Selection::new(kind.into(), point, side));
    }

    pub(crate) fn update_selection(&self, point: GridPoint) {
        let mut term = self.term.lock();
        let (point, side) = to_grid(point, term.grid().display_offset());
        if let Some(selection) = term.selection.as_mut() {
            selection.update(point, side);
        }
    }

    pub(crate) fn has_selection(&self) -> bool {
        self.term.lock().selection.is_some()
    }

    pub(crate) fn clear_selection(&self) {
        self.term.lock().selection = None;
    }

    pub(crate) fn selection_text(&self) -> Option<String> {
        self.term
            .lock()
            .selection_to_string()
            .filter(|s| !s.is_empty())
    }

    pub(crate) fn resize(&self, size: TermSize) {
        *self
            .listener
            .inner
            .size
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = size.window_size();
        self.term.lock().resize(size);
        let _ = self.sender.send(Msg::Resize(size.window_size()));
    }

    pub(crate) fn set_options(&self, options: TermOptions) {
        self.term.lock().set_options(term_config(options));
    }

    pub(crate) fn set_palette(&self, palette: Palette) {
        *self
            .listener
            .inner
            .palette
            .write()
            .unwrap_or_else(PoisonError::into_inner) = palette;
    }

    pub(crate) fn mode(&self) -> term::TermMode {
        *self.term.lock().mode()
    }

    pub(crate) fn snapshot(&self) -> Snapshot {
        self.listener.inner.sink.ack();
        let term = self.term.lock();
        let palette = self
            .listener
            .inner
            .palette
            .read()
            .unwrap_or_else(PoisonError::into_inner);
        Snapshot::capture(&term, &palette, &[], None)
    }

    pub(crate) fn answer_color_queries(&self) {
        // Same lock order as the PTY thread: term, then the queue.
        let replies: Vec<String> = {
            let term = self.term.lock();
            let queries = std::mem::take(
                &mut *self
                    .listener
                    .inner
                    .color_queries
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner),
            );
            if queries.is_empty() {
                return;
            }
            let palette = self
                .listener
                .inner
                .palette
                .read()
                .unwrap_or_else(PoisonError::into_inner);
            queries
                .into_iter()
                .map(|(index, format)| format(term.colors()[index].unwrap_or(palette.get(index))))
                .collect()
        };
        for reply in replies {
            self.listener.inner.write(reply);
        }
    }

    pub(crate) fn search_snapshot(&self, search: &mut Search) -> Snapshot {
        self.listener.inner.sink.ack();
        let term = self.term.lock();
        let matches = search::visible_matches(&term, search);
        let palette = self
            .listener
            .inner
            .palette
            .read()
            .unwrap_or_else(PoisonError::into_inner);
        Snapshot::capture(&term, &palette, &matches, search.current_in(&term))
    }

    pub(crate) fn search(&self, search: &mut Search, up: bool) -> bool {
        search::find(&mut self.term.lock(), search, up)
    }

    pub(crate) fn link_at(&self, point: GridPoint) -> Option<Link> {
        let term = self.term.lock();
        let (point, _) = to_grid(point, term.grid().display_offset());
        url::link_at(&term, point)
    }
}

impl Drop for AlacrittyPane {
    fn drop(&mut self) {
        let _ = self.sender.send(Msg::Shutdown);
    }
}

/// The PTY, with its output scanned for the directory the shell reports.
struct TeePty {
    pty: tty::Pty,
    scanner: CwdScanner,
    reported: Arc<Mutex<Option<ReportedDir>>>,
}

impl io::Read for TeePty {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.pty.reader().read(buf)?;
        if let Some(dir) = self.scanner.feed(&buf[..n]) {
            *self.reported.lock().unwrap_or_else(PoisonError::into_inner) = Some(dir);
        }
        Ok(n)
    }
}

impl EventedReadWrite for TeePty {
    type Reader = Self;
    type Writer = <tty::Pty as EventedReadWrite>::Writer;

    unsafe fn register(
        &mut self,
        poll: &Arc<Poller>,
        interest: polling::Event,
        mode: PollMode,
    ) -> io::Result<()> {
        // SAFETY: the PTY lives in `self`, as long as its registration.
        unsafe { self.pty.register(poll, interest, mode) }
    }

    fn reregister(
        &mut self,
        poll: &Arc<Poller>,
        interest: polling::Event,
        mode: PollMode,
    ) -> io::Result<()> {
        self.pty.reregister(poll, interest, mode)
    }

    fn deregister(&mut self, poll: &Arc<Poller>) -> io::Result<()> {
        self.pty.deregister(poll)
    }

    fn reader(&mut self) -> &mut Self {
        self
    }

    fn writer(&mut self) -> &mut Self::Writer {
        self.pty.writer()
    }
}

impl EventedPty for TeePty {
    fn next_child_event(&mut self) -> Option<ChildEvent> {
        self.pty.next_child_event()
    }
}

impl OnResize for TeePty {
    fn on_resize(&mut self, window_size: WindowSize) {
        self.pty.on_resize(window_size);
    }
}

impl From<SelectionKind> for SelectionType {
    fn from(kind: SelectionKind) -> Self {
        match kind {
            SelectionKind::Simple => SelectionType::Simple,
            SelectionKind::Block => SelectionType::Block,
            SelectionKind::Semantic => SelectionType::Semantic,
            SelectionKind::Lines => SelectionType::Lines,
        }
    }
}

/// A viewport point as a grid point, `display_offset` lines up in the
/// scrollback.
pub(crate) fn to_grid(point: GridPoint, display_offset: usize) -> (Point, Side) {
    let grid = viewport_to_point(display_offset, Point::new(point.line, Column(point.column)));
    let side = if point.right_half {
        Side::Right
    } else {
        Side::Left
    };
    (grid, side)
}

/// alacritty's terminal options.
fn term_config(options: TermOptions) -> term::Config {
    term::Config {
        scrolling_history: options.scrollback,
        osc52: if options.clipboard_write {
            term::Osc52::OnlyCopy
        } else {
            term::Osc52::Disabled
        },
        kitty_keyboard: options.kitty_keyboard,
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use alacritty_terminal::index::Line;

    use super::*;

    #[test]
    fn grid_point_respects_scrollback() {
        let p = GridPoint {
            column: 3,
            line: 0,
            right_half: true,
        };
        let (point, side) = to_grid(p, 5);
        assert_eq!(point, Point::new(Line(-5), Column(3)));
        assert_eq!(side, Side::Right);
    }
}
