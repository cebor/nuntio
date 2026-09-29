//! The `wezterm` engine: wezterm-term's `Terminal` behind the PTY thread
//! in [`io`], with the viewport, selection and search nuntio needs on top.

mod config;
mod grid;
mod io;
mod links;
mod scanner;
pub(crate) mod search;
mod selection;
mod snapshot;

use std::borrow::Cow;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::TermMode;
use alacritty_terminal::tty;
use termwiz::escape::csi::KittyKeyboardFlags;
use termwiz::input::KeyboardEncoding;
use termwiz::surface::CursorVisibility;
use wezterm_term::{
    Alert, AlertHandler, CellAttributes, Clipboard, ClipboardSelection, MouseEncoding,
    StableRowIndex, Terminal, TerminalSize,
};

use self::grid::{Pos, Side};
use self::io::{Io, Msg, PtyWriter};
use self::selection::Selection;
use crate::osc_cwd::ReportedDir;
use crate::palette::Palette;
use crate::pane::{
    EventSink, GridPoint, SelectionKind, SpawnError, SpawnOptions, TermEvent, TermOptions, TermSize,
};
use crate::search::Search;
use crate::snapshot::Snapshot;
use crate::url::Link;

/// A pane run by wezterm's emulator.
pub(crate) struct WeztermPane {
    state: Arc<FairMutex<State>>,
    sender: io::Sender,
    config: Arc<config::Config>,
    sink: Arc<EventSink>,
    clipboard_write: Arc<AtomicBool>,
}

/// Everything behind the terminal lock.
pub(crate) struct State {
    term: Terminal,
    /// The top row of the viewport; `None` follows the bottom.
    view_top: Option<StableRowIndex>,
    selection: Option<Selection>,
    /// Whether the alternate screen was active at the last look: stable
    /// rows of the two screens have nothing to do with each other.
    alt_screen: bool,
    /// Alternate scroll (DECSET 1007), which wezterm-term doesn't track;
    /// kept by the PTY thread.
    alt_scroll: bool,
    /// nuntio's palette, for selection and search colors.
    palette: Palette,
}

impl WeztermPane {
    pub(crate) fn spawn(
        pty: tty::Pty,
        reported_dir: Arc<Mutex<Option<ReportedDir>>>,
        options: &SpawnOptions,
        size: TermSize,
        sink: Arc<EventSink>,
    ) -> Result<Self, SpawnError> {
        let config = Arc::new(config::Config::new(options.term, &options.palette));
        let clipboard_write = Arc::new(AtomicBool::new(options.term.clipboard_write));
        let (sender, rx) = Io::channel()?;

        let mut term = Terminal::new(
            terminal_size(size),
            config.clone(),
            "nuntio",
            env!("CARGO_PKG_VERSION"),
            Box::new(PtyWriter(sender.clone())),
        );
        term.set_notification_handler(Box::new(Alerts(sink.clone())));
        let clipboard: Arc<dyn Clipboard> = Arc::new(ClipboardSink {
            sink: sink.clone(),
            allowed: clipboard_write.clone(),
        });
        term.set_clipboard(&clipboard);
        #[cfg(windows)]
        term.enable_conpty_quirks();

        let state = Arc::new(FairMutex::new(State {
            term,
            view_top: None,
            palette: options.palette.clone(),
            selection: None,
            alt_screen: false,
            alt_scroll: true,
        }));
        Io::new(pty, &sender, rx, state.clone(), sink.clone(), reported_dir).spawn()?;

        Ok(Self {
            state,
            sender,
            config,
            sink,
            clipboard_write,
        })
    }

    pub(crate) fn write(&self, bytes: Cow<'static, [u8]>) {
        // Typing jumps back to the bottom, like every terminal does.
        self.state.lock().view_top = None;
        self.sender.send(Msg::Input(bytes));
    }

    pub(crate) fn scroll(&self, lines: i32) {
        let mut state = self.state.lock();
        state.sync_screen();
        let offset = state.display_offset() as i64 + i64::from(lines);
        let offset = offset.clamp(0, state.history() as i64) as usize;
        state.set_offset(offset);
    }

    pub(crate) fn scroll_page(&self, up: bool) {
        let rows = self.state.lock().term.screen().physical_rows as i32;
        self.scroll(if up { rows } else { -rows });
    }

    pub(crate) fn clear_history(&self) {
        let mut state = self.state.lock();
        state.view_top = None;
        state.selection = None;
        state.term.erase_scrollback();
    }

    pub(crate) fn start_selection(&self, kind: SelectionKind, point: GridPoint) {
        let mut state = self.state.lock();
        state.sync_screen();
        let (pos, side) = state.to_pos(point);
        state.selection = Some(Selection::new(kind, pos, side));
    }

    pub(crate) fn update_selection(&self, point: GridPoint) {
        let mut state = self.state.lock();
        state.sync_screen();
        let (pos, side) = state.to_pos(point);
        if let Some(selection) = state.selection.as_mut() {
            selection.update(pos, side);
        }
    }

    pub(crate) fn has_selection(&self) -> bool {
        let mut state = self.state.lock();
        state.sync_screen();
        state.selection.is_some()
    }

    pub(crate) fn clear_selection(&self) {
        self.state.lock().selection = None;
    }

    pub(crate) fn selection_text(&self) -> Option<String> {
        let mut state = self.state.lock();
        state.sync_screen();
        let state = &mut *state;
        let selection = state.selection.as_ref()?;
        grid::with_grid(state.term.screen_mut(), |grid| selection.text(grid))
    }

    pub(crate) fn resize(&self, size: TermSize) {
        let mut state = self.state.lock();
        // Rows rewrap to the new width, which moves text between rows.
        if usize::from(size.columns) != state.term.screen().physical_cols {
            state.selection = None;
            state.view_top = None;
        }
        state.term.resize(terminal_size(size));
        drop(state);
        self.sender.send(Msg::Resize(size.window_size()));
    }

    pub(crate) fn set_options(&self, options: TermOptions) {
        self.config.update(options);
        self.clipboard_write
            .store(options.clipboard_write, Ordering::Relaxed);
        // wezterm-term drops lines beyond the scrollback only when the
        // screen scrolls; scrolling by nothing does it now. The alternate
        // screen has no scrollback, and the primary one catches up when it
        // scrolls next.
        let mut state = self.state.lock();
        let rows = state.term.screen().physical_rows as i64;
        let bidi = state.term.get_config().bidi_mode();
        let seqno = state.term.current_seqno();
        state
            .term
            .screen_mut()
            .scroll_up(&(0..rows), 0, seqno, CellAttributes::blank(), bidi);
    }

    pub(crate) fn set_palette(&self, palette: Palette) {
        // OSC overrides stay: wezterm-term forks its palette on the first.
        self.config.set_palette(&palette);
        self.state.lock().palette = palette;
    }

    pub(crate) fn mode(&self) -> TermMode {
        let state = self.state.lock();
        let term = &state.term;
        let mut mode = TermMode::URGENCY_HINTS;
        let mut set = |flag: TermMode, on: bool| mode.set(flag, on);
        set(
            TermMode::SHOW_CURSOR,
            term.cursor_pos().visibility == CursorVisibility::Visible,
        );
        set(TermMode::APP_CURSOR, term.application_cursor_keys_enabled());
        set(TermMode::APP_KEYPAD, term.application_keypad_enabled());
        set(TermMode::BRACKETED_PASTE, term.bracketed_paste_enabled());
        set(TermMode::FOCUS_IN_OUT, term.focus_tracking_enabled());
        set(TermMode::ALT_SCREEN, term.is_alt_screen_active());
        set(TermMode::MOUSE_REPORT_CLICK, term.mouse_tracking_enabled());
        set(TermMode::MOUSE_DRAG, term.button_event_mouse_enabled());
        set(TermMode::MOUSE_MOTION, term.any_event_mouse_enabled());
        let encoding = term.get_mouse_encoding();
        set(
            TermMode::SGR_MOUSE,
            matches!(encoding, MouseEncoding::SGR | MouseEncoding::SgrPixels),
        );
        set(TermMode::UTF8_MOUSE, encoding == MouseEncoding::Utf8);
        set(TermMode::LINE_WRAP, term.dec_auto_wrap_enabled());
        set(TermMode::ORIGIN, term.dec_origin_mode_enabled());
        set(TermMode::INSERT, term.insert_mode_enabled());
        set(TermMode::ALTERNATE_SCROLL, state.alt_scroll);
        if self.config.kitty_keyboard()
            && let KeyboardEncoding::Kitty(flags) = term.get_keyboard_encoding()
        {
            for (kitty, flag) in [
                (
                    KittyKeyboardFlags::DISAMBIGUATE_ESCAPE_CODES,
                    TermMode::DISAMBIGUATE_ESC_CODES,
                ),
                (
                    KittyKeyboardFlags::REPORT_EVENT_TYPES,
                    TermMode::REPORT_EVENT_TYPES,
                ),
                (
                    KittyKeyboardFlags::REPORT_ALTERNATE_KEYS,
                    TermMode::REPORT_ALTERNATE_KEYS,
                ),
                (
                    KittyKeyboardFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES,
                    TermMode::REPORT_ALL_KEYS_AS_ESC,
                ),
                (
                    KittyKeyboardFlags::REPORT_ASSOCIATED_TEXT,
                    TermMode::REPORT_ASSOCIATED_TEXT,
                ),
            ] {
                set(flag, flags.contains(kitty));
            }
        }
        mode
    }

    pub(crate) fn snapshot(&self) -> Snapshot {
        self.sink.ack();
        let mut state = self.state.lock();
        state.sync_screen();
        snapshot::capture(&mut state, &[], None)
    }

    /// wezterm-term answers color queries itself, from its runtime
    /// palette; this engine never sends [`TermEvent::ColorQuery`].
    pub(crate) fn answer_color_queries(&self) {}

    pub(crate) fn search_snapshot(&self, search: &mut Search) -> Snapshot {
        self.sink.ack();
        let mut state = self.state.lock();
        state.sync_screen();
        let matches = search::visible_matches(&mut state, search);
        snapshot::capture(&mut state, &matches, search.wez.current.as_ref())
    }

    pub(crate) fn search(&self, search: &mut Search, up: bool) -> bool {
        let mut state = self.state.lock();
        state.sync_screen();
        search::find(&mut state, search, up)
    }

    pub(crate) fn link_at(&self, point: GridPoint) -> Option<Link> {
        let mut state = self.state.lock();
        state.sync_screen();
        links::link_at(&mut state, point)
    }
}

impl Drop for WeztermPane {
    fn drop(&mut self) {
        self.sender.send(Msg::Shutdown);
    }
}

impl State {
    /// Forget the viewport and selection when the screen switched.
    fn sync_screen(&mut self) {
        let alt = self.term.is_alt_screen_active();
        if alt != self.alt_screen {
            self.alt_screen = alt;
            self.selection = None;
            self.view_top = None;
        }
        if let Some(selection) = &self.selection
            && grid::with_grid(self.term.screen_mut(), |grid| selection.is_stale(grid))
        {
            self.selection = None;
        }
    }

    /// Lines of scrollback.
    fn history(&self) -> usize {
        let screen = self.term.screen();
        screen
            .scrollback_rows()
            .saturating_sub(screen.physical_rows)
    }

    /// The top row of the screen when scrolled to the bottom.
    fn bottom_top(&self) -> StableRowIndex {
        self.term.screen().visible_row_to_stable_row(0)
    }

    /// Lines the viewport is scrolled up. A viewport whose top row was
    /// dropped from the scrollback shows the oldest row.
    fn display_offset(&self) -> usize {
        let Some(top) = self.view_top else {
            return 0;
        };
        (self.bottom_top() - top).clamp(0, self.history() as StableRowIndex) as usize
    }

    fn set_offset(&mut self, offset: usize) {
        self.view_top = (offset > 0).then(|| self.bottom_top() - offset as StableRowIndex);
    }

    /// The row at the top of the viewport.
    fn viewport_top(&self) -> StableRowIndex {
        self.bottom_top() - self.display_offset() as StableRowIndex
    }

    fn to_pos(&self, point: GridPoint) -> (Pos, Side) {
        let pos = Pos::new(
            self.viewport_top() + point.line as StableRowIndex,
            point.column,
        );
        let side = if point.right_half {
            Side::Right
        } else {
            Side::Left
        };
        (pos, side)
    }

    /// Scroll as little as needed to show `row`.
    fn scroll_to_row(&mut self, row: StableRowIndex) {
        let top = self.viewport_top();
        let rows = self.term.screen().physical_rows as StableRowIndex;
        let new_top = if row < top {
            row
        } else if row >= top + rows {
            row - rows + 1
        } else {
            return;
        };
        let offset = (self.bottom_top() - new_top).clamp(0, self.history() as StableRowIndex);
        self.set_offset(offset as usize);
    }
}

fn terminal_size(size: TermSize) -> TerminalSize {
    let size = size.clamped();
    TerminalSize {
        rows: size.lines.into(),
        cols: size.columns.into(),
        pixel_width: usize::from(size.columns) * usize::from(size.cell_width),
        pixel_height: usize::from(size.lines) * usize::from(size.cell_height),
        dpi: 0,
    }
}

/// Bell and title changes, to the application.
struct Alerts(Arc<EventSink>);

impl AlertHandler for Alerts {
    fn alert(&mut self, alert: Alert) {
        match alert {
            Alert::Bell => self.0.send(TermEvent::Bell),
            Alert::WindowTitleChanged(title) => self.0.title(title),
            // Like alacritty, nuntio shows no icon title (OSC 1).
            _ => (),
        }
    }
}

/// OSC 52 writes, if allowed. wezterm-term never reads the clipboard.
struct ClipboardSink {
    sink: Arc<EventSink>,
    allowed: Arc<AtomicBool>,
}

impl Clipboard for ClipboardSink {
    fn set_contents(
        &self,
        _selection: ClipboardSelection,
        data: Option<String>,
    ) -> anyhow::Result<()> {
        if let Some(text) = data
            && self.allowed.load(Ordering::Relaxed)
        {
            self.sink.send(TermEvent::ClipboardStore(text));
        }
        Ok(())
    }
}
