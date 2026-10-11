//! The notes app without a terminal: the list of notes, the open note,
//! the todo overview, prompts and saving. `ui` draws it, `mod.rs` feeds it
//! events.

use std::collections::HashMap;
use std::time::{Duration, Instant, SystemTime};

use ratatui::crossterm::event::{
    KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::{Position, Rect, Size};

use super::buffer::{Buffer, EditKind, Pos};
use super::edit;
use super::layout::{self, Row};
use super::markdown::{self, LineView, PieceKind};
use super::store::{Stamp, Store, validate_title};
use super::ui::{self, Areas};
use super::widgets::TextInput;

/// How long after the last edit a note is saved.
pub const AUTOSAVE: Duration = Duration::from_secs(1);
/// Rows or items one wheel step moves.
const WHEEL: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Dim,
    Warn,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    List,
    Editor,
}

/// A note in the list.
#[derive(Debug, Clone)]
pub struct Entry {
    pub title: String,
    pub modified: SystemTime,
    pub done: usize,
    pub total: usize,
}

/// The note in the editor.
pub struct Open {
    pub title: String,
    pub buffer: Buffer,
    /// `markdown::render` of the buffer at revision `views_revision`.
    pub views: Vec<LineView>,
    views_revision: u64,
    /// The file as last read or written; `None` if it didn't exist.
    stamp: Option<Stamp>,
    /// The buffer revision that is on disk.
    saved: u64,
    /// The first unsaved edit since the last save attempt.
    edited: Option<Instant>,
    /// After a conflict was dismissed: no autosave until Ctrl+S.
    paused: bool,
}

impl Open {
    pub fn dirty(&self) -> bool {
        self.buffer.revision() != self.saved
    }

    fn sync_views(&mut self) {
        if self.views_revision != self.buffer.revision()
            || self.views.len() != self.buffer.lines().len()
        {
            self.views = markdown::render(self.buffer.lines());
            self.views_revision = self.buffer.revision();
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TodoItem {
    pub title: String,
    pub line: usize,
    /// The source line.
    pub text: String,
    pub checked: bool,
}

pub struct Todos {
    pub items: Vec<TodoItem>,
    pub selected: usize,
    pub show_done: bool,
    /// First visible row of `rows()`.
    pub scroll: usize,
}

/// A row of the todo overview.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TodoRow {
    /// The title of the note of the item with this index.
    Note(usize),
    Item(usize),
}

impl Todos {
    pub fn rows(&self) -> Vec<TodoRow> {
        let mut rows = Vec::new();
        for (i, item) in self.items.iter().enumerate() {
            if i == 0 || self.items[i - 1].title != item.title {
                rows.push(TodoRow::Note(i));
            }
            rows.push(TodoRow::Item(i));
        }
        rows
    }

    pub fn open_count(&self) -> usize {
        self.items.iter().filter(|i| !i.checked).count()
    }
}

pub enum Prompt {
    NewNote(TextInput, Option<&'static str>),
    Rename(TextInput, Option<&'static str>),
    Delete,
    /// The open note changed on disk and has unsaved changes.
    Conflict,
    /// Quitting although saving failed, for this reason.
    QuitUnsaved(String),
    Help,
}

pub struct App {
    store: Box<dyn Store>,
    pub dir_label: String,
    /// Newest first.
    pub notes: Vec<Entry>,
    pub filter: Option<TextInput>,
    /// Typing goes to the filter.
    pub filtering: bool,
    /// Index into `visible()`.
    pub selected: usize,
    pub list_scroll: usize,
    pub open: Option<Open>,
    cursors: HashMap<String, Pos>,
    pub focus: Focus,
    pub todos: Option<Todos>,
    pub prompt: Option<Prompt>,
    pub sidebar: bool,
    /// First visible row of `rows`.
    pub scroll: usize,
    follow: bool,
    /// Column kept across vertical moves.
    goal: Option<u16>,
    pub areas: Areas,
    /// The laid-out open note: all rows, the first row of each line, and
    /// the line shown raw.
    pub rows: Vec<Row>,
    first_row: Vec<usize>,
    pub raw_line: Option<usize>,
    pub cursor_xy: Option<Position>,
    pub message: Option<(Tone, String)>,
    register: String,
    clipboard_out: Option<String>,
    pub quit: bool,
    /// The notes folder can't be used: only this message is shown.
    pub fatal: Option<String>,
}

/// The lines of a note's text, as `Buffer` splits them.
fn lines_of(text: &str) -> Vec<String> {
    text.split('\n')
        .map(|l| l.strip_suffix('\r').unwrap_or(l).to_owned())
        .collect()
}

fn counts_of(lines: &[String]) -> (usize, usize) {
    markdown::counts(&markdown::render(lines))
}

fn ctrl(key: &KeyEvent) -> Option<char> {
    match key.code {
        KeyCode::Char(c) if key.modifiers.contains(KeyModifiers::CONTROL) => Some(c),
        _ => None,
    }
}

impl App {
    /// The app on `store` with its newest note open.
    pub fn new(store: Box<dyn Store>, dir_label: String) -> Self {
        let mut app = Self::new_empty(store, dir_label);
        app.refresh();
        if !app.notes.is_empty() {
            app.open_title(&app.notes[0].title.clone());
        }
        app
    }

    /// An app that only shows `message` and quits on `q`.
    pub fn failed(store: Box<dyn Store>, dir_label: String, message: String) -> Self {
        let mut app = Self::new_empty(store, dir_label);
        app.fatal = Some(message);
        app
    }

    fn new_empty(store: Box<dyn Store>, dir_label: String) -> Self {
        Self {
            store,
            dir_label,
            notes: Vec::new(),
            filter: None,
            filtering: false,
            selected: 0,
            list_scroll: 0,
            open: None,
            cursors: HashMap::new(),
            focus: Focus::List,
            todos: None,
            prompt: None,
            sidebar: true,
            scroll: 0,
            follow: false,
            goal: None,
            areas: Areas::default(),
            rows: Vec::new(),
            first_row: Vec::new(),
            raw_line: None,
            cursor_xy: None,
            message: None,
            register: String::new(),
            clipboard_out: None,
            quit: false,
            fatal: None,
        }
    }

    // ----- Queries -----------------------------------------------------------

    /// Indices into `notes` that match the filter.
    pub fn visible(&self) -> Vec<usize> {
        let needle = self
            .filter
            .as_ref()
            .map(|f| f.text().to_lowercase())
            .unwrap_or_default();
        (0..self.notes.len())
            .filter(|&i| needle.is_empty() || self.notes[i].title.to_lowercase().contains(&needle))
            .collect()
    }

    fn selected_title(&self) -> Option<String> {
        let visible = self.visible();
        visible
            .get(self.selected)
            .map(|&i| self.notes[i].title.clone())
    }

    #[cfg(test)]
    fn dirty(&self) -> bool {
        self.open.as_ref().is_some_and(Open::dirty)
    }

    /// When `tick` has something to do: a pending autosave.
    pub fn deadline(&self) -> Option<Instant> {
        let open = self.open.as_ref()?;
        if open.paused || !open.dirty() {
            return None;
        }
        open.edited.map(|at| at + AUTOSAVE)
    }

    /// Text to put on the clipboard (OSC 52), once.
    pub fn take_clipboard(&mut self) -> Option<String> {
        self.clipboard_out.take()
    }

    // ----- Loading -----------------------------------------------------------

    /// Re-read the folder: the list with its todo counts, and the open note
    /// if it changed on disk and has no unsaved changes.
    pub fn refresh(&mut self) {
        if self.fatal.is_some() {
            return;
        }
        let selected = self.selected_title();
        match self.store.list() {
            Ok(files) => {
                let mut notes: Vec<Entry> = files
                    .into_iter()
                    .map(|f| {
                        let (done, total) = counts_of(&lines_of(&f.text));
                        Entry {
                            title: f.title,
                            modified: f.modified,
                            done,
                            total,
                        }
                    })
                    .collect();
                notes.sort_by(|a, b| b.modified.cmp(&a.modified).then(a.title.cmp(&b.title)));
                self.notes = notes;
            }
            Err(err) => {
                self.message = Some((Tone::Error, format!("Couldn't list the notes: {err}")));
            }
        }
        self.check_open_on_disk();
        self.update_open_counts();
        if let Some(title) = selected {
            self.select_title_quietly(&title);
        }
        self.clamp_selection();
    }

    fn check_open_on_disk(&mut self) {
        let Some(open) = &self.open else {
            return;
        };
        if open.dirty() {
            return;
        }
        let title = open.title.clone();
        match self.store.stamp(&title) {
            Ok(None) => {
                self.close_open();
                self.message = Some((Tone::Warn, format!("{title} was deleted elsewhere")));
            }
            Ok(Some(stamp)) if Some(stamp) != open.stamp => {
                let cursor = open.buffer.cursor();
                self.load(&title, cursor);
            }
            _ => {}
        }
    }

    /// The open note's entry counts its live todos.
    fn update_open_counts(&mut self) {
        let Some(open) = &mut self.open else {
            return;
        };
        open.sync_views();
        let (done, total) = markdown::counts(&open.views);
        if let Some(entry) = self.notes.iter_mut().find(|e| e.title == open.title) {
            entry.done = done;
            entry.total = total;
        }
    }

    fn close_open(&mut self) {
        if let Some(open) = self.open.take() {
            self.cursors.insert(open.title, open.buffer.cursor());
        }
        self.rows.clear();
        self.first_row.clear();
        self.cursor_xy = None;
        if self.focus == Focus::Editor {
            self.focus = Focus::List;
        }
    }

    /// Read `title` into the editor with the cursor at `cursor` (clamped).
    /// Returns whether it worked.
    fn load(&mut self, title: &str, cursor: Pos) -> bool {
        match self.store.read(title) {
            Ok((text, stamp)) => {
                let mut buffer = Buffer::new(&text);
                buffer.move_to(cursor, false);
                let revision = buffer.revision();
                let same = self.open.as_ref().is_some_and(|o| o.title == title);
                self.open = Some(Open {
                    title: title.to_owned(),
                    buffer,
                    views: Vec::new(),
                    views_revision: u64::MAX,
                    stamp: Some(stamp),
                    saved: revision,
                    edited: None,
                    paused: false,
                });
                if !same {
                    self.scroll = 0;
                }
                self.goal = None;
                self.update_open_counts();
                true
            }
            Err(err) => {
                self.message = Some((Tone::Error, format!("Couldn't open {title}: {err}")));
                false
            }
        }
    }

    /// Save the open note and open `title` instead. Returns whether it is
    /// open now.
    fn open_title(&mut self, title: &str) -> bool {
        if self.open.as_ref().is_some_and(|o| o.title == title) {
            return true;
        }
        if self.save().is_err() {
            return false;
        }
        if let Some(open) = self.open.take() {
            self.cursors.insert(open.title, open.buffer.cursor());
        }
        let cursor = self.cursors.get(title).copied().unwrap_or_default();
        self.scroll = 0;
        let loaded = self.load(title, cursor);
        self.select_title_quietly(title);
        loaded
    }

    fn select_title_quietly(&mut self, title: &str) {
        if let Some(i) = self
            .visible()
            .iter()
            .position(|&i| self.notes[i].title == title)
        {
            self.selected = i;
        }
    }

    fn clamp_selection(&mut self) {
        let len = self.visible().len();
        self.selected = self.selected.min(len.saturating_sub(1));
    }

    /// Select entry `index` of the visible list and open it.
    fn select(&mut self, index: usize) {
        let visible = self.visible();
        let Some(&i) = visible.get(index) else {
            return;
        };
        let title = self.notes[i].title.clone();
        if self.open_title(&title) {
            self.selected = index;
        } else if let Some(open) = &self.open {
            let open = open.title.clone();
            self.select_title_quietly(&open);
        }
    }

    // ----- Saving ------------------------------------------------------------

    /// Write the open note if it has unsaved changes. A conflict with a
    /// change on disk opens `Prompt::Conflict` instead. The error is the
    /// reason it isn't saved.
    pub fn save(&mut self) -> Result<(), String> {
        let Some(open) = &mut self.open else {
            return Ok(());
        };
        if !open.dirty() {
            return Ok(());
        }
        match self.store.stamp(&open.title) {
            Ok(Some(stamp)) if Some(stamp) != open.stamp => {
                open.edited = None;
                self.prompt = Some(Prompt::Conflict);
                return Err("The note changed on disk".into());
            }
            Ok(_) => {}
            Err(err) => {
                open.edited = None;
                let reason = format!("Couldn't save: {err}");
                self.message = Some((Tone::Error, reason.clone()));
                return Err(reason);
            }
        }
        self.write_open()
    }

    /// Write the open note without looking at the file on disk.
    fn write_open(&mut self) -> Result<(), String> {
        let Some(open) = &mut self.open else {
            return Ok(());
        };
        open.edited = None;
        match self.store.write(&open.title, &open.buffer.text()) {
            Ok(stamp) => {
                open.stamp = Some(stamp);
                open.saved = open.buffer.revision();
                open.paused = false;
                if let Some(entry) = self.notes.iter_mut().find(|e| e.title == open.title) {
                    entry.modified = stamp.modified;
                }
                self.update_open_counts();
                Ok(())
            }
            Err(err) => {
                let reason = format!("Couldn't save: {err}");
                self.message = Some((Tone::Error, reason.clone()));
                Err(reason)
            }
        }
    }

    /// Time passed: autosave when due.
    pub fn tick(&mut self, now: Instant) {
        if self.deadline().is_some_and(|at| at <= now) && self.prompt.is_none() {
            let _ = self.save();
        }
    }

    fn quit_save(&mut self) {
        match self.save() {
            Ok(()) => self.quit = true,
            Err(reason) => self.prompt = Some(Prompt::QuitUnsaved(reason)),
        }
    }

    /// After every edit of the open note: autosave `AUTOSAVE` after the
    /// last one.
    fn edited(&mut self, now: Instant) {
        if let Some(open) = &mut self.open {
            open.edited = Some(now);
        }
        self.follow = true;
        self.goal = None;
        self.update_open_counts();
    }

    // ----- Layout ------------------------------------------------------------

    /// Lay out the frame for `size`: the areas, the open note's rows, the
    /// scroll positions.
    pub fn prepare(&mut self, size: Size) {
        self.areas = ui::areas(size, self);
        self.cursor_xy = None;
        self.rows.clear();
        self.first_row.clear();
        if let Some(text) = self.areas.text {
            self.layout_editor(text);
        }
        if let Some(list) = self.areas.list {
            let height = list.height.saturating_sub(u16::from(self.filter.is_some())) as usize;
            self.list_scroll = keep_visible(self.list_scroll, self.selected, height.max(1));
        }
        if let Some(todos) = &mut self.todos {
            let height = self.areas.body.height.saturating_sub(2) as usize;
            let rows = todos.rows();
            let at = rows
                .iter()
                .position(|r| *r == TodoRow::Item(todos.selected))
                .unwrap_or(0);
            // Show the note's title above its first item.
            let top = keep_visible(todos.scroll, at, height.max(1));
            todos.scroll = if at > 0 && top == at && matches!(rows[at - 1], TodoRow::Note(_)) {
                at - 1
            } else {
                top
            };
        }
    }

    /// The rows of `line`, raw or rendered.
    fn line_rows(&self, line: usize, raw: bool, width: u16) -> Vec<Row> {
        let Some(open) = &self.open else {
            return Vec::new();
        };
        let text = &open.buffer.lines()[line];
        let view = &open.views[line];
        if raw {
            layout::wrap(line, text, &view.raw, view.raw_hang, width)
        } else {
            layout::wrap(line, text, &view.rendered, view.hang, width)
        }
    }

    fn text_width(&self) -> u16 {
        self.areas
            .text
            .map_or(80, |t| t.width.saturating_sub(1))
            .max(1)
    }

    fn layout_editor(&mut self, text: Rect) {
        let width = self.text_width();
        let editing = self.focus == Focus::Editor && self.todos.is_none();
        let Some(open) = &mut self.open else {
            return;
        };
        open.sync_views();
        let cursor = open.buffer.cursor();
        self.raw_line = editing.then_some(cursor.line);
        let mut rows = Vec::new();
        let mut first_row = Vec::with_capacity(open.views.len());
        for line in 0..open.buffer.lines().len() {
            first_row.push(rows.len());
            rows.extend(self.line_rows(line, self.raw_line == Some(line), width));
        }
        let cursor_row = {
            let start = first_row[cursor.line];
            let end = first_row
                .get(cursor.line + 1)
                .copied()
                .unwrap_or(rows.len());
            let (row, col) = layout::locate(&rows[start..end], cursor.col);
            (start + row, col)
        };
        let height = text.height as usize;
        if self.follow {
            self.scroll = keep_visible(self.scroll, cursor_row.0, height.max(1));
            self.follow = false;
        }
        self.scroll = self.scroll.min(rows.len().saturating_sub(height.max(1)));
        if editing
            && self.prompt.is_none()
            && (self.scroll..self.scroll + height).contains(&cursor_row.0)
        {
            self.cursor_xy = Some(Position::new(
                text.x + cursor_row.1.min(text.width.saturating_sub(1)),
                text.y + (cursor_row.0 - self.scroll) as u16,
            ));
        }
        self.rows = rows;
        self.first_row = first_row;
    }

    // ----- Keys --------------------------------------------------------------

    /// Handle a key; returns whether to redraw.
    pub fn key(&mut self, key: KeyEvent, now: Instant) -> bool {
        self.message = None;
        if self.fatal.is_some() {
            if matches!(key.code, KeyCode::Esc | KeyCode::Char('q')) || ctrl(&key) == Some('q') {
                self.quit = true;
            }
            return true;
        }
        if self.prompt.is_some() {
            self.prompt_key(key);
            return true;
        }
        match ctrl(&key) {
            Some('q') => {
                self.quit_save();
                return true;
            }
            Some('n') => {
                self.prompt = Some(Prompt::NewNote(TextInput::new(""), None));
                return true;
            }
            Some('o') => {
                self.toggle_todos();
                return true;
            }
            Some('b') => {
                self.sidebar = !self.sidebar;
                return true;
            }
            Some('s') => {
                if let Some(open) = &mut self.open {
                    open.paused = false;
                }
                let _ = self.save();
                return true;
            }
            _ => {}
        }
        if key.code == KeyCode::F(1) {
            self.prompt = Some(Prompt::Help);
            return true;
        }
        if self.todos.is_some() {
            self.todos_key(key, now);
        } else if self.focus == Focus::Editor && self.open.is_some() {
            self.editor_key(key, now);
        } else {
            self.list_key(key);
        }
        true
    }

    fn list_key(&mut self, key: KeyEvent) {
        let len = self.visible().len();
        let page = self.areas.list.map_or(10, |l| l.height as usize).max(2) - 1;
        let target = match key.code {
            KeyCode::Up => Some(self.selected.saturating_sub(1)),
            KeyCode::Down => Some((self.selected + 1).min(len.saturating_sub(1))),
            KeyCode::PageUp => Some(self.selected.saturating_sub(page)),
            KeyCode::PageDown => Some((self.selected + page).min(len.saturating_sub(1))),
            KeyCode::Home if !self.filtering => Some(0),
            KeyCode::End if !self.filtering => Some(len.saturating_sub(1)),
            _ => None,
        };
        if let Some(target) = target {
            self.select(target);
            return;
        }
        if self.filtering {
            match key.code {
                KeyCode::Enter => {
                    self.filtering = false;
                    if self.filter.as_ref().is_some_and(|f| f.text().is_empty()) {
                        self.filter = None;
                    }
                }
                KeyCode::Esc => self.clear_filter(),
                _ => {
                    if let Some(filter) = &mut self.filter
                        && filter.key(key)
                    {
                        self.selected = 0;
                        self.select(0);
                    }
                }
            }
            return;
        }
        match key.code {
            KeyCode::Enter | KeyCode::Right | KeyCode::Tab => {
                if self.open.is_some() {
                    self.focus = Focus::Editor;
                    self.follow = true;
                }
            }
            KeyCode::Char('/') => {
                self.filter = Some(TextInput::new(""));
                self.filtering = true;
            }
            KeyCode::Char('n') => self.prompt = Some(Prompt::NewNote(TextInput::new(""), None)),
            KeyCode::Char('r') | KeyCode::F(2) => self.start_rename(),
            KeyCode::Char('d') | KeyCode::Delete => {
                if self.selected_title().is_some() {
                    self.prompt = Some(Prompt::Delete);
                }
            }
            KeyCode::Char('?') => self.prompt = Some(Prompt::Help),
            KeyCode::Esc if self.filter.is_some() => self.clear_filter(),
            KeyCode::Char('q') | KeyCode::Esc => self.quit_save(),
            _ => {}
        }
    }

    fn clear_filter(&mut self) {
        let open = self.open.as_ref().map(|o| o.title.clone());
        self.filter = None;
        self.filtering = false;
        if let Some(title) = open {
            self.select_title_quietly(&title);
        }
        self.clamp_selection();
    }

    fn start_rename(&mut self) {
        if let Some(title) = self.selected_title() {
            self.prompt = Some(Prompt::Rename(TextInput::new(&title), None));
        }
    }

    fn editor_key(&mut self, key: KeyEvent, now: Instant) {
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        if let Some(c) = ctrl(&key) {
            match c {
                'a' => self.with_buffer(|b| b.select_all()),
                'c' => self.copy(false, now),
                'x' => self.copy(true, now),
                'v' => {
                    if self.register.is_empty() {
                        let keys = if cfg!(target_os = "macos") {
                            "Cmd+V"
                        } else {
                            "Ctrl+Shift+V"
                        };
                        self.message =
                            Some((Tone::Dim, format!("Paste from the clipboard with {keys}")));
                    } else {
                        let text = self.register.clone();
                        self.insert_text(&text, EditKind::Other, now);
                    }
                }
                'z' => self.undo_redo(true, now),
                'y' => self.undo_redo(false, now),
                't' => self.edit_with(now, edit::toggle_todo),
                _ => {}
            }
            return;
        }
        let vertical = matches!(
            key.code,
            KeyCode::Up | KeyCode::Down | KeyCode::PageUp | KeyCode::PageDown
        );
        if !vertical {
            self.goal = None;
        }
        match key.code {
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::ALT) => {
                self.insert_text(c.encode_utf8(&mut [0; 4]), EditKind::Typing, now);
            }
            KeyCode::Enter => self.edit_with(now, edit::enter),
            KeyCode::Tab => self.edit_with(now, edit::indent),
            KeyCode::BackTab => self.edit_with(now, edit::outdent),
            KeyCode::Backspace => self.edit_simple(now, Buffer::backspace),
            KeyCode::Delete => self.edit_simple(now, Buffer::delete),
            KeyCode::Left if control => self.with_buffer(|b| b.word_left(shift)),
            KeyCode::Right if control => self.with_buffer(|b| b.word_right(shift)),
            KeyCode::Left => self.with_buffer(|b| b.left(shift)),
            KeyCode::Right => self.with_buffer(|b| b.right(shift)),
            KeyCode::Home if control => self.with_buffer(|b| b.doc_start(shift)),
            KeyCode::End if control => self.with_buffer(|b| b.doc_end(shift)),
            KeyCode::Home => self.with_buffer(|b| b.home(shift)),
            KeyCode::End => self.with_buffer(|b| b.end(shift)),
            KeyCode::Up => self.move_rows(-1, shift),
            KeyCode::Down => self.move_rows(1, shift),
            KeyCode::PageUp => self.move_rows(-self.page_rows(), shift),
            KeyCode::PageDown => self.move_rows(self.page_rows(), shift),
            KeyCode::F(2) => self.start_rename(),
            KeyCode::Esc => {
                let selecting = self
                    .open
                    .as_ref()
                    .is_some_and(|o| o.buffer.selection().is_some());
                if selecting {
                    self.with_buffer(|b| {
                        let at = b.cursor();
                        b.move_to(at, false);
                    });
                } else {
                    let _ = self.save();
                    self.focus = Focus::List;
                }
            }
            _ => {}
        }
    }

    fn page_rows(&self) -> isize {
        self.areas.text.map_or(10, |t| t.height as isize - 1).max(1)
    }

    /// A cursor movement or other change without an edit.
    fn with_buffer(&mut self, f: impl FnOnce(&mut Buffer)) {
        if let Some(open) = &mut self.open {
            f(&mut open.buffer);
            self.follow = true;
        }
    }

    fn edit_simple(&mut self, now: Instant, f: impl FnOnce(&mut Buffer)) {
        let Some(open) = &mut self.open else {
            return;
        };
        let before = open.buffer.revision();
        open.buffer.begin(EditKind::Other);
        f(&mut open.buffer);
        if open.buffer.revision() != before {
            self.edited(now);
        }
        self.follow = true;
    }

    fn edit_with(&mut self, now: Instant, f: impl FnOnce(&mut Buffer, &[LineView])) {
        let Some(open) = &mut self.open else {
            return;
        };
        open.sync_views();
        let before = open.buffer.revision();
        f(&mut open.buffer, &open.views);
        if open.buffer.revision() != before {
            self.edited(now);
        }
        self.follow = true;
    }

    fn insert_text(&mut self, text: &str, kind: EditKind, now: Instant) {
        let Some(open) = &mut self.open else {
            return;
        };
        open.buffer.begin(kind);
        open.buffer.insert(text);
        self.edited(now);
    }

    fn copy(&mut self, cut: bool, now: Instant) {
        let Some(open) = &mut self.open else {
            return;
        };
        let Some(text) = open.buffer.selected_text() else {
            return;
        };
        self.register = text.clone();
        self.clipboard_out = Some(text);
        if cut {
            open.buffer.begin(EditKind::Other);
            open.buffer.delete_selection();
            self.edited(now);
        }
    }

    fn undo_redo(&mut self, undo: bool, now: Instant) {
        let Some(open) = &mut self.open else {
            return;
        };
        let changed = if undo {
            open.buffer.undo()
        } else {
            open.buffer.redo()
        };
        if changed {
            self.edited(now);
        }
    }

    /// Move the cursor by `delta` visual rows, keeping the goal column. The
    /// line it lands on is laid out raw: it becomes the cursor line.
    fn move_rows(&mut self, delta: isize, select: bool) {
        let width = self.text_width();
        let Some(open) = &mut self.open else {
            return;
        };
        open.sync_views();
        let cursor = open.buffer.cursor();
        let last_line = open.buffer.lines().len() - 1;
        let mut line = cursor.line;
        let mut rows = self.line_rows(line, true, width);
        let (mut row, col) = layout::locate(&rows, cursor.col);
        let goal = *self.goal.get_or_insert(col);
        let mut moved = 0;
        while moved < delta.unsigned_abs() {
            if delta > 0 {
                if row + 1 < rows.len() {
                    row += 1;
                } else if line < last_line {
                    line += 1;
                    rows = self.line_rows(line, true, width);
                    row = 0;
                } else {
                    break;
                }
            } else if row > 0 {
                row -= 1;
            } else if line > 0 {
                line -= 1;
                rows = self.line_rows(line, true, width);
                row = rows.len() - 1;
            } else {
                break;
            }
            moved += 1;
        }
        let Some(open) = &mut self.open else {
            return;
        };
        if moved == 0 {
            // Past the first or last row: to the start or end.
            if delta < 0 {
                open.buffer.doc_start(select);
            } else {
                open.buffer.doc_end(select);
            }
            self.goal = None;
        } else {
            let (byte, _) = layout::hit(&rows[row], goal);
            open.buffer.move_to(Pos { line, col: byte }, select);
        }
        self.follow = true;
    }

    fn todos_key(&mut self, key: KeyEvent, now: Instant) {
        if ctrl(&key) == Some('t') {
            self.toggle_todo_item(now);
            return;
        }
        let Some(todos) = &mut self.todos else {
            return;
        };
        let len = todos.items.len();
        let page = self.areas.body.height.saturating_sub(3).max(1) as usize;
        match key.code {
            KeyCode::Up => todos.selected = todos.selected.saturating_sub(1),
            KeyCode::Down => todos.selected = (todos.selected + 1).min(len.saturating_sub(1)),
            KeyCode::PageUp => todos.selected = todos.selected.saturating_sub(page),
            KeyCode::PageDown => {
                todos.selected = (todos.selected + page).min(len.saturating_sub(1));
            }
            KeyCode::Home => todos.selected = 0,
            KeyCode::End => todos.selected = len.saturating_sub(1),
            KeyCode::Char(' ') => self.toggle_todo_item(now),
            KeyCode::Char('a') => {
                let show_done = !todos.show_done;
                self.build_todos(show_done);
            }
            KeyCode::Enter => self.open_todo_item(),
            KeyCode::Esc => self.todos = None,
            _ => {}
        }
    }

    fn prompt_key(&mut self, key: KeyEvent) {
        let Some(prompt) = &mut self.prompt else {
            return;
        };
        match prompt {
            Prompt::Help => self.prompt = None,
            Prompt::Delete => match key.code {
                KeyCode::Char('y') | KeyCode::Enter => {
                    self.prompt = None;
                    self.delete_selected();
                }
                KeyCode::Char('n') | KeyCode::Esc => self.prompt = None,
                _ => {}
            },
            Prompt::QuitUnsaved(_) => match key.code {
                KeyCode::Char('y') => self.quit = true,
                KeyCode::Char('n') | KeyCode::Esc => self.prompt = None,
                _ => {}
            },
            Prompt::Conflict => match key.code {
                KeyCode::Char('o') => {
                    self.prompt = None;
                    let _ = self.write_open();
                }
                KeyCode::Char('r') => {
                    self.prompt = None;
                    if let Some(open) = &self.open {
                        let (title, cursor) = (open.title.clone(), open.buffer.cursor());
                        self.load(&title, cursor);
                    }
                }
                KeyCode::Esc => {
                    self.prompt = None;
                    if let Some(open) = &mut self.open {
                        open.paused = true;
                    }
                }
                _ => {}
            },
            Prompt::NewNote(input, error) | Prompt::Rename(input, error) => match key.code {
                KeyCode::Esc => self.prompt = None,
                KeyCode::Enter => {
                    let text = input.text().to_owned();
                    if matches!(prompt, Prompt::NewNote(..)) {
                        self.create(&text);
                    } else {
                        self.rename(&text);
                    }
                }
                _ => {
                    if input.key(key) {
                        *error = None;
                    }
                }
            },
        }
    }

    // ----- Paste and mouse -----------------------------------------------------

    /// Bracketed paste; returns whether to redraw.
    pub fn paste(&mut self, text: &str, now: Instant) -> bool {
        if self.fatal.is_some() {
            return false;
        }
        match &mut self.prompt {
            Some(Prompt::NewNote(input, error) | Prompt::Rename(input, error)) => {
                let first = text.lines().next().unwrap_or_default();
                input.insert_str(first);
                *error = None;
                return true;
            }
            Some(_) => return false,
            None => {}
        }
        if self.filtering {
            if let Some(filter) = &mut self.filter {
                filter.insert_str(text.lines().next().unwrap_or_default());
                self.select(0);
            }
            return true;
        }
        if self.todos.is_some() || self.focus != Focus::Editor || self.open.is_none() {
            return false;
        }
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        self.insert_text(&text, EditKind::Other, now);
        true
    }

    /// A mouse event; returns whether to redraw.
    pub fn mouse(&mut self, event: MouseEvent, now: Instant) -> bool {
        if self.fatal.is_some() || self.prompt.is_some() {
            return false;
        }
        let at = Position::new(event.column, event.row);
        if self.todos.is_some() {
            return self.todos_mouse(event, at, now);
        }
        let in_list = self.areas.list.is_some_and(|r| r.contains(at));
        let in_editor = self.areas.editor.is_some_and(|r| r.contains(at));
        match event.kind {
            MouseEventKind::Down(MouseButton::Left) if in_list => {
                let list = self.areas.list.unwrap_or_default();
                let top = list.y + u16::from(self.filter.is_some());
                if at.y >= top {
                    let index = self.list_scroll + (at.y - top) as usize;
                    if index < self.visible().len() {
                        self.select(index);
                    }
                }
                self.focus = Focus::List;
                true
            }
            MouseEventKind::Down(MouseButton::Left) if in_editor && self.open.is_some() => {
                self.editor_click(at, event.modifiers.contains(KeyModifiers::SHIFT), now);
                true
            }
            MouseEventKind::Drag(MouseButton::Left) if self.focus == Focus::Editor => {
                if let Some(pos) = self.editor_pos(at) {
                    self.with_buffer(|b| b.move_to(pos.0, true));
                }
                true
            }
            MouseEventKind::ScrollDown if in_list => {
                let target = (self.selected + WHEEL).min(self.visible().len().saturating_sub(1));
                self.select(target);
                true
            }
            MouseEventKind::ScrollUp if in_list => {
                self.select(self.selected.saturating_sub(WHEEL));
                true
            }
            MouseEventKind::ScrollDown if in_editor => {
                self.scroll += WHEEL;
                true
            }
            MouseEventKind::ScrollUp if in_editor => {
                self.scroll = self.scroll.saturating_sub(WHEEL);
                true
            }
            MouseEventKind::Moved => false,
            _ => false,
        }
    }

    /// The source position under `at` in the editor text and what is drawn
    /// there.
    fn editor_pos(&self, at: Position) -> Option<(Pos, PieceKind)> {
        let text = self.areas.text?;
        if self.rows.is_empty() {
            return None;
        }
        let y = at.y.saturating_sub(text.y) as usize;
        let index = if at.y < text.y {
            self.scroll.saturating_sub(1)
        } else {
            self.scroll + y
        };
        let row = &self.rows[index.min(self.rows.len() - 1)];
        let x = at.x.saturating_sub(text.x);
        let (byte, kind) = layout::hit(row, x);
        Some((
            Pos {
                line: row.line,
                col: byte,
            },
            kind,
        ))
    }

    fn editor_click(&mut self, at: Position, shift: bool, now: Instant) {
        let Some((pos, kind)) = self.editor_pos(at) else {
            self.focus = Focus::Editor;
            return;
        };
        let task = self
            .open
            .as_ref()
            .and_then(|o| o.views.get(pos.line))
            .and_then(|v| v.task.clone());
        if kind == PieceKind::Checkbox
            && let Some(task) = task
        {
            let Some(open) = &mut self.open else {
                return;
            };
            let line = edit::toggle_task_line(&open.buffer.lines()[pos.line], &task);
            open.buffer.begin(EditKind::Other);
            open.buffer.set_line(pos.line, line);
            self.edited(now);
            // Keep the scroll position: the click didn't move the cursor.
            self.follow = false;
            return;
        }
        self.focus = Focus::Editor;
        self.goal = None;
        self.with_buffer(|b| b.move_to(pos, shift));
    }

    fn todos_mouse(&mut self, event: MouseEvent, at: Position, now: Instant) -> bool {
        let body = self.areas.body;
        let Some(todos) = &mut self.todos else {
            return false;
        };
        match event.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                let top = body.y + 2;
                if at.y < top {
                    return false;
                }
                let index = todos.scroll + (at.y - top) as usize;
                if let Some(TodoRow::Item(i)) = todos.rows().get(index).copied() {
                    todos.selected = i;
                    if (ui::TODO_BOX_X..ui::TODO_BOX_X + 2).contains(&at.x.saturating_sub(body.x)) {
                        self.toggle_todo_item(now);
                    }
                }
                true
            }
            MouseEventKind::ScrollDown => {
                todos.selected = (todos.selected + WHEEL).min(todos.items.len().saturating_sub(1));
                true
            }
            MouseEventKind::ScrollUp => {
                todos.selected = todos.selected.saturating_sub(WHEEL);
                true
            }
            _ => false,
        }
    }

    // ----- Notes: create, rename, delete -----------------------------------------

    fn titles(&self) -> Vec<&str> {
        self.notes.iter().map(|e| e.title.as_str()).collect()
    }

    fn set_prompt_error(&mut self, message: &'static str) {
        if let Some(Prompt::NewNote(_, error) | Prompt::Rename(_, error)) = &mut self.prompt {
            *error = Some(message);
        }
    }

    fn create(&mut self, input: &str) {
        let title = match validate_title(input, &self.titles()) {
            Ok(title) => title,
            Err(message) => return self.set_prompt_error(message),
        };
        self.prompt = None;
        if self.save().is_err() {
            return;
        }
        if let Err(err) = self.store.create(&title) {
            self.message = Some((Tone::Error, format!("Couldn't create {title}: {err}")));
            return;
        }
        self.todos = None;
        self.filter = None;
        self.filtering = false;
        self.refresh();
        if self.open_title(&title) {
            self.focus = Focus::Editor;
            self.follow = true;
        }
    }

    fn rename(&mut self, input: &str) {
        let Some(old) = self.selected_title() else {
            self.prompt = None;
            return;
        };
        let taken: Vec<&str> = self
            .titles()
            .into_iter()
            .filter(|t| t.to_lowercase() != old.to_lowercase())
            .collect();
        let title = match validate_title(input, &taken) {
            Ok(title) => title,
            Err(message) => return self.set_prompt_error(message),
        };
        self.prompt = None;
        if title == old {
            return;
        }
        let is_open = self.open.as_ref().is_some_and(|o| o.title == old);
        if is_open && self.save().is_err() {
            return;
        }
        if let Err(err) = self.store.rename(&old, &title) {
            self.message = Some((Tone::Error, format!("Couldn't rename {old}: {err}")));
            return;
        }
        if let Some(pos) = self.cursors.remove(&old) {
            self.cursors.insert(title.clone(), pos);
        }
        if is_open && let Some(open) = &mut self.open {
            open.title = title.clone();
            open.stamp = self.store.stamp(&title).ok().flatten();
        }
        self.filter = None;
        self.filtering = false;
        self.refresh();
        self.select_title_quietly(&title);
    }

    fn delete_selected(&mut self) {
        let Some(title) = self.selected_title() else {
            return;
        };
        let index = self.selected;
        if self.open.as_ref().is_some_and(|o| o.title == title) {
            // The trash gets the latest text; the note goes either way.
            let _ = self.save();
            self.prompt = None;
        }
        if let Err(err) = self.store.trash(&title) {
            self.message = Some((Tone::Error, format!("Couldn't delete {title}: {err}")));
            return;
        }
        self.cursors.remove(&title);
        if self.open.as_ref().is_some_and(|o| o.title == title) {
            self.open = None;
            self.focus = Focus::List;
        }
        self.refresh();
        self.selected = index;
        self.clamp_selection();
        if let Some(next) = self.selected_title() {
            self.open_title(&next);
        }
    }

    // ----- Todo overview -----------------------------------------------------

    fn toggle_todos(&mut self) {
        if self.todos.is_some() {
            self.todos = None;
            return;
        }
        let _ = self.save();
        self.build_todos(false);
    }

    /// Collect the tasks of every note, in list order.
    fn build_todos(&mut self, show_done: bool) {
        let files = match self.store.list() {
            Ok(files) => files,
            Err(err) => {
                self.message = Some((Tone::Error, format!("Couldn't list the notes: {err}")));
                return;
            }
        };
        let mut texts: HashMap<String, String> =
            files.into_iter().map(|f| (f.title, f.text)).collect();
        if let Some(open) = &self.open {
            texts.insert(open.title.clone(), open.buffer.text());
        }
        let mut items = Vec::new();
        for entry in &self.notes {
            let Some(text) = texts.get(&entry.title) else {
                continue;
            };
            let lines = lines_of(text);
            for (line, view) in markdown::render(&lines).iter().enumerate() {
                if let Some(task) = &view.task
                    && (show_done || !task.checked)
                {
                    items.push(TodoItem {
                        title: entry.title.clone(),
                        line,
                        text: lines[line].clone(),
                        checked: task.checked,
                    });
                }
            }
        }
        let selected = self.todos.as_ref().map_or(0, |t| t.selected);
        self.todos = Some(Todos {
            selected: selected.min(items.len().saturating_sub(1)),
            items,
            show_done,
            scroll: 0,
        });
    }

    fn toggle_todo_item(&mut self, now: Instant) {
        let Some(item) = self
            .todos
            .as_ref()
            .and_then(|t| t.items.get(t.selected))
            .cloned()
        else {
            return;
        };
        let toggled = if self.open.as_ref().is_some_and(|o| o.title == item.title) {
            self.toggle_in_open(&item, now)
        } else {
            self.toggle_in_file(&item)
        };
        match toggled {
            Some(text) => {
                if let Some(todos) = &mut self.todos
                    && let Some(it) = todos.items.get_mut(todos.selected)
                {
                    it.checked = !it.checked;
                    it.text = text;
                }
            }
            None => {
                let show_done = self.todos.as_ref().is_some_and(|t| t.show_done);
                self.build_todos(show_done);
                if self.message.is_none() {
                    self.message = Some((Tone::Warn, "The note changed; list updated".into()));
                }
            }
        }
    }

    /// Toggle `item` in the open note and save it. The new line, or `None`
    /// if the line no longer is that task.
    fn toggle_in_open(&mut self, item: &TodoItem, now: Instant) -> Option<String> {
        let open = self.open.as_mut()?;
        open.sync_views();
        let task = open.views.get(item.line)?.task.clone()?;
        if open.buffer.lines()[item.line] != item.text {
            return None;
        }
        let line = edit::toggle_task_line(&item.text, &task);
        open.buffer.begin(EditKind::Other);
        open.buffer.set_line(item.line, line.clone());
        self.edited(now);
        let _ = self.save();
        Some(line)
    }

    /// Toggle `item` in its file.
    fn toggle_in_file(&mut self, item: &TodoItem) -> Option<String> {
        let (text, _) = match self.store.read(&item.title) {
            Ok(read) => read,
            Err(err) => {
                self.message = Some((Tone::Error, format!("Couldn't open {}: {err}", item.title)));
                return None;
            }
        };
        let mut buffer = Buffer::new(&text);
        let views = markdown::render(buffer.lines());
        let task = views.get(item.line)?.task.clone()?;
        if buffer.lines()[item.line] != item.text {
            return None;
        }
        let line = edit::toggle_task_line(&item.text, &task);
        buffer.set_line(item.line, line.clone());
        match self.store.write(&item.title, &buffer.text()) {
            Ok(stamp) => {
                let (done, total) = counts_of(buffer.lines());
                if let Some(entry) = self.notes.iter_mut().find(|e| e.title == item.title) {
                    entry.done = done;
                    entry.total = total;
                    entry.modified = stamp.modified;
                }
                Some(line)
            }
            Err(err) => {
                self.message = Some((Tone::Error, format!("Couldn't save: {err}")));
                None
            }
        }
    }

    fn open_todo_item(&mut self) {
        let Some(item) = self
            .todos
            .as_ref()
            .and_then(|t| t.items.get(t.selected))
            .cloned()
        else {
            return;
        };
        if self.filter.is_some()
            && !self
                .visible()
                .iter()
                .any(|&i| self.notes[i].title == item.title)
        {
            self.filter = None;
            self.filtering = false;
        }
        if !self.open_title(&item.title) {
            return;
        }
        self.todos = None;
        self.focus = Focus::Editor;
        if let Some(open) = &mut self.open {
            let col = open.buffer.lines().get(item.line).map_or(0, String::len);
            open.buffer.move_to(
                Pos {
                    line: item.line,
                    col,
                },
                false,
            );
        }
        self.follow = true;
    }
}

/// The scroll offset that keeps `index` within `height` rows.
fn keep_visible(scroll: usize, index: usize, height: usize) -> usize {
    if index < scroll {
        index
    } else if index >= scroll + height {
        index + 1 - height
    } else {
        scroll
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::BTreeMap;
    use std::io;
    use std::rc::Rc;
    use std::time::UNIX_EPOCH;

    use super::*;
    use crate::notes::store::NoteFile;

    #[derive(Default)]
    struct Files {
        notes: BTreeMap<String, (String, Stamp)>,
        clock: u64,
        trashed: Vec<String>,
        fail_writes: bool,
    }

    impl Files {
        fn stamp(&mut self, text: &str) -> Stamp {
            self.clock += 1;
            Stamp {
                modified: UNIX_EPOCH + Duration::from_secs(self.clock),
                len: text.len() as u64,
            }
        }
    }

    /// An in-memory folder.
    #[derive(Clone, Default)]
    struct Memory(Rc<RefCell<Files>>);

    impl Memory {
        fn with(notes: &[(&str, &str)]) -> Self {
            let memory = Self::default();
            for (title, text) in notes {
                memory.set_external(title, text);
            }
            memory
        }

        /// Another program writes the note.
        fn set_external(&self, title: &str, text: &str) {
            let mut files = self.0.borrow_mut();
            let stamp = files.stamp(text);
            files
                .notes
                .insert(title.to_owned(), (text.to_owned(), stamp));
        }

        fn text(&self, title: &str) -> Option<String> {
            self.0.borrow().notes.get(title).map(|(t, _)| t.clone())
        }
    }

    fn not_found() -> io::Error {
        io::Error::from(io::ErrorKind::NotFound)
    }

    impl Store for Memory {
        fn list(&self) -> io::Result<Vec<NoteFile>> {
            Ok(self
                .0
                .borrow()
                .notes
                .iter()
                .map(|(title, (text, stamp))| NoteFile {
                    title: title.clone(),
                    modified: stamp.modified,
                    text: text.clone(),
                })
                .collect())
        }

        fn read(&self, title: &str) -> io::Result<(String, Stamp)> {
            self.0
                .borrow()
                .notes
                .get(title)
                .cloned()
                .ok_or_else(not_found)
        }

        fn stamp(&self, title: &str) -> io::Result<Option<Stamp>> {
            Ok(self.0.borrow().notes.get(title).map(|(_, s)| *s))
        }

        fn write(&mut self, title: &str, text: &str) -> io::Result<Stamp> {
            let mut files = self.0.borrow_mut();
            if files.fail_writes {
                return Err(io::Error::other("disk full"));
            }
            let stamp = files.stamp(text);
            files
                .notes
                .insert(title.to_owned(), (text.to_owned(), stamp));
            Ok(stamp)
        }

        fn create(&mut self, title: &str) -> io::Result<()> {
            if self.0.borrow().notes.contains_key(title) {
                return Err(io::ErrorKind::AlreadyExists.into());
            }
            self.write(title, "").map(|_| ())
        }

        fn rename(&mut self, from: &str, to: &str) -> io::Result<()> {
            let mut files = self.0.borrow_mut();
            let note = files.notes.remove(from).ok_or_else(not_found)?;
            files.notes.insert(to.to_owned(), note);
            Ok(())
        }

        fn trash(&mut self, title: &str) -> io::Result<()> {
            let mut files = self.0.borrow_mut();
            files.notes.remove(title).ok_or_else(not_found)?;
            files.trashed.push(title.to_owned());
            Ok(())
        }
    }

    fn app(memory: &Memory) -> App {
        let mut app = App::new(Box::new(memory.clone()), "~/notes".into());
        app.prepare(Size::new(100, 30));
        app
    }

    fn key(app: &mut App, code: KeyCode, now: Instant) {
        app.key(KeyEvent::new(code, KeyModifiers::NONE), now);
        app.prepare(Size::new(100, 30));
    }

    fn ctrl_key(app: &mut App, c: char, now: Instant) {
        app.key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL), now);
        app.prepare(Size::new(100, 30));
    }

    fn type_text(app: &mut App, text: &str, now: Instant) {
        for c in text.chars() {
            key(app, KeyCode::Char(c), now);
        }
    }

    #[test]
    fn opens_the_newest_note_and_autosaves_after_a_second() {
        let memory = Memory::with(&[("Old", "old\n"), ("New", "new\n")]);
        let mut app = app(&memory);
        assert_eq!(app.open.as_ref().unwrap().title, "New");
        let now = Instant::now();
        key(&mut app, KeyCode::Enter, now);
        assert_eq!(app.focus, Focus::Editor);
        type_text(&mut app, "x", now);
        assert_eq!(app.deadline(), Some(now + AUTOSAVE));
        app.tick(now + Duration::from_millis(500));
        assert_eq!(memory.text("New").unwrap(), "new\n");
        app.tick(now + AUTOSAVE);
        assert_eq!(memory.text("New").unwrap(), "xnew\n");
        assert!(!app.dirty());
        assert_eq!(app.deadline(), None);
    }

    #[test]
    fn a_clean_note_changed_elsewhere_is_reloaded() {
        let memory = Memory::with(&[("A", "one\n")]);
        let mut app = app(&memory);
        memory.set_external("A", "two\n");
        app.refresh();
        assert_eq!(app.open.as_ref().unwrap().buffer.text(), "two\n");
    }

    #[test]
    fn a_dirty_note_changed_elsewhere_is_a_conflict() {
        let memory = Memory::with(&[("A", "one\n")]);
        let mut app = app(&memory);
        let now = Instant::now();
        key(&mut app, KeyCode::Enter, now);
        type_text(&mut app, "x", now);
        memory.set_external("A", "theirs\n");
        app.refresh();
        assert!(app.dirty(), "unsaved changes are kept");
        assert!(app.save().is_err());
        assert!(matches!(app.prompt, Some(Prompt::Conflict)));
        assert_eq!(memory.text("A").unwrap(), "theirs\n");
        // Overwrite.
        key(&mut app, KeyCode::Char('o'), now);
        assert_eq!(memory.text("A").unwrap(), "xone\n");
    }

    #[test]
    fn the_overview_toggles_a_note_that_is_not_open() {
        let memory = Memory::with(&[("Other", "- [ ] milk\n- [x] bread\n"), ("Open", "text\n")]);
        let mut app = app(&memory);
        assert_eq!(app.open.as_ref().unwrap().title, "Open");
        let now = Instant::now();
        ctrl_key(&mut app, 'o', now);
        let todos = app.todos.as_ref().unwrap();
        assert_eq!(todos.items.len(), 1, "done items are hidden");
        key(&mut app, KeyCode::Char(' '), now);
        assert_eq!(memory.text("Other").unwrap(), "- [x] milk\n- [x] bread\n");
        let entry = app.notes.iter().find(|e| e.title == "Other").unwrap();
        assert_eq!((entry.done, entry.total), (2, 2));
        // The toggled item stays visible.
        assert!(app.todos.as_ref().unwrap().items[0].checked);
    }

    #[test]
    fn creating_a_taken_title_is_refused() {
        let memory = Memory::with(&[("Groceries", "")]);
        let mut app = app(&memory);
        let now = Instant::now();
        ctrl_key(&mut app, 'n', now);
        type_text(&mut app, "groceries", now);
        key(&mut app, KeyCode::Enter, now);
        assert!(matches!(
            app.prompt,
            Some(Prompt::NewNote(_, Some("A note with this title exists")))
        ));
        for _ in 0.."groceries".len() {
            key(&mut app, KeyCode::Backspace, now);
        }
        type_text(&mut app, "Todo", now);
        key(&mut app, KeyCode::Enter, now);
        assert!(app.prompt.is_none());
        assert_eq!(memory.text("Todo").as_deref(), Some(""));
        assert_eq!(app.open.as_ref().unwrap().title, "Todo");
        assert_eq!(app.focus, Focus::Editor);
    }

    #[test]
    fn delete_moves_to_the_trash_and_opens_the_next() {
        let memory = Memory::with(&[("B", "b"), ("A", "a")]);
        let mut app = app(&memory);
        assert_eq!(app.open.as_ref().unwrap().title, "A");
        let now = Instant::now();
        key(&mut app, KeyCode::Char('d'), now);
        key(&mut app, KeyCode::Char('y'), now);
        assert_eq!(memory.0.borrow().trashed, ["A"]);
        assert_eq!(app.open.as_ref().unwrap().title, "B");
        assert_eq!(app.notes.len(), 1);
    }

    #[test]
    fn a_failed_save_asks_before_quitting() {
        let memory = Memory::with(&[("A", "a")]);
        let mut app = app(&memory);
        let now = Instant::now();
        key(&mut app, KeyCode::Enter, now);
        type_text(&mut app, "x", now);
        memory.0.borrow_mut().fail_writes = true;
        ctrl_key(&mut app, 'q', now);
        assert!(!app.quit);
        assert!(matches!(app.prompt, Some(Prompt::QuitUnsaved(_))));
        key(&mut app, KeyCode::Char('y'), now);
        assert!(app.quit);
    }

    #[test]
    fn clicking_a_checkbox_toggles_it_without_moving_the_cursor() {
        let memory = Memory::with(&[("A", "# T\n- [ ] milk\n")]);
        let mut app = app(&memory);
        let now = Instant::now();
        let row = app.first_row[1] - app.scroll;
        let text = app.areas.text.unwrap();
        let event = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: text.x,
            row: text.y + row as u16,
            modifiers: KeyModifiers::NONE,
        };
        app.mouse(event, now);
        let open = app.open.as_ref().unwrap();
        assert_eq!(open.buffer.lines()[1], "- [x] milk");
        assert_eq!(open.buffer.cursor(), Pos::default());
    }
}
