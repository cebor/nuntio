//! The text of a note: lines, cursor, selection and undo.

use std::ops::Range;

use unicode_segmentation::UnicodeSegmentation;

/// Undo steps kept.
const UNDO_LIMIT: usize = 200;

/// A position in the text; `col` is a byte offset on a grapheme boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Pos {
    pub line: usize,
    pub col: usize,
}

/// How an edit groups into undo steps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditKind {
    /// Typed characters: consecutive ones without cursor movement share a
    /// step.
    Typing,
    Other,
}

#[derive(Debug, Clone)]
struct Snapshot {
    lines: Vec<String>,
    cursor: Pos,
}

#[derive(Debug, Clone)]
pub struct Buffer {
    /// Never empty; no line contains `\n` or a trailing `\r`.
    lines: Vec<String>,
    crlf: bool,
    cursor: Pos,
    anchor: Option<Pos>,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    typing: bool,
    revision: u64,
}

/// `col` moved down to the start of the grapheme it is in, at most the line
/// length.
fn snap(line: &str, col: usize) -> usize {
    if col >= line.len() {
        return line.len();
    }
    let mut start = 0;
    for (i, _) in line.grapheme_indices(true) {
        if i > col {
            break;
        }
        start = i;
    }
    start
}

fn is_blank(segment: &str) -> bool {
    segment.chars().all(char::is_whitespace)
}

/// Where `pos` goes when `range` of its line is replaced by `new_len` bytes.
fn shifted(pos: &mut Pos, line: usize, range: &Range<usize>, new_len: usize) {
    if pos.line != line || pos.col <= range.start {
        return;
    }
    pos.col = if pos.col >= range.end {
        pos.col - range.len() + new_len
    } else {
        pos.col.min(range.start + new_len)
    };
}

impl Buffer {
    pub fn new(text: &str) -> Self {
        let crlf = text.contains("\r\n");
        let lines = text
            .split('\n')
            .map(|l| l.strip_suffix('\r').unwrap_or(l).to_owned())
            .collect();
        Self {
            lines,
            crlf,
            cursor: Pos::default(),
            anchor: None,
            undo: Vec::new(),
            redo: Vec::new(),
            typing: false,
            revision: 0,
        }
    }

    pub fn lines(&self) -> &[String] {
        &self.lines
    }

    pub fn cursor(&self) -> Pos {
        self.cursor
    }

    pub fn anchor(&self) -> Option<Pos> {
        self.anchor
    }

    /// Changes with every edit, undo and redo.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// The text as it is saved: lines joined with the file's line ending.
    pub fn text(&self) -> String {
        self.lines.join(if self.crlf { "\r\n" } else { "\n" })
    }

    // --- Undo ---

    /// Call before every edit to make it an undo step.
    pub fn begin(&mut self, kind: EditKind) {
        if kind == EditKind::Typing && self.typing {
            return;
        }
        self.typing = kind == EditKind::Typing;
        if let Some(last) = self.undo.last()
            && last.cursor == self.cursor
            && last.lines == self.lines
        {
            return;
        }
        let snapshot = self.snapshot();
        self.undo.push(snapshot);
        if self.undo.len() > UNDO_LIMIT {
            self.undo.remove(0);
        }
        self.redo.clear();
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            lines: self.lines.clone(),
            cursor: self.cursor,
        }
    }

    fn restore(&mut self, snapshot: Snapshot) {
        self.lines = snapshot.lines;
        self.cursor = snapshot.cursor;
        self.anchor = None;
        self.typing = false;
        self.revision += 1;
    }

    /// Returns whether there was something to undo.
    pub fn undo(&mut self) -> bool {
        let Some(snapshot) = self.undo.pop() else {
            return false;
        };
        let current = self.snapshot();
        self.redo.push(current);
        self.restore(snapshot);
        true
    }

    /// Returns whether there was something to redo.
    pub fn redo(&mut self) -> bool {
        let Some(snapshot) = self.redo.pop() else {
            return false;
        };
        let current = self.snapshot();
        self.undo.push(current);
        self.restore(snapshot);
        true
    }

    // --- Selection ---

    /// The selected range, ordered; `None` when nothing is selected.
    pub fn selection(&self) -> Option<(Pos, Pos)> {
        let anchor = self.anchor?;
        match anchor.cmp(&self.cursor) {
            std::cmp::Ordering::Equal => None,
            std::cmp::Ordering::Less => Some((anchor, self.cursor)),
            std::cmp::Ordering::Greater => Some((self.cursor, anchor)),
        }
    }

    pub fn selected_text(&self) -> Option<String> {
        let (start, end) = self.selection()?;
        if start.line == end.line {
            return Some(self.lines[start.line][start.col..end.col].to_owned());
        }
        let mut text = self.lines[start.line][start.col..].to_owned();
        for line in &self.lines[start.line + 1..end.line] {
            text.push('\n');
            text.push_str(line);
        }
        text.push('\n');
        text.push_str(&self.lines[end.line][..end.col]);
        Some(text)
    }

    pub fn select_all(&mut self) {
        self.typing = false;
        self.anchor = Some(Pos::default());
        self.cursor = self.doc_end_pos();
    }

    /// Delete the selection; returns whether there was one.
    pub fn delete_selection(&mut self) -> bool {
        let Some((start, end)) = self.selection() else {
            self.anchor = None;
            return false;
        };
        self.remove(start, end);
        self.revision += 1;
        true
    }

    /// Remove the text between two ordered positions and put the cursor at
    /// the start.
    fn remove(&mut self, start: Pos, end: Pos) {
        if start.line == end.line {
            self.lines[start.line].replace_range(start.col..end.col, "");
        } else {
            let tail = self.lines[end.line][end.col..].to_owned();
            self.lines[start.line].truncate(start.col);
            self.lines[start.line].push_str(&tail);
            self.lines.drain(start.line + 1..=end.line);
        }
        self.cursor = start;
        self.anchor = None;
    }

    // --- Edits ---

    /// Insert `text` at the cursor, replacing the selection. `\r` is dropped.
    pub fn insert(&mut self, text: &str) {
        let had_selection = self.delete_selection();
        let text = text.replace('\r', "");
        if text.is_empty() {
            return;
        }
        let Pos { line, col } = self.cursor;
        let mut parts = text.split('\n');
        let first = parts.next().unwrap_or_default();
        let tail = self.lines[line].split_off(col);
        self.lines[line].push_str(first);
        let mut at = line;
        for part in parts {
            at += 1;
            self.lines.insert(at, part.to_owned());
        }
        let col = self.lines[at].len();
        self.lines[at].push_str(&tail);
        self.cursor = Pos { line: at, col };
        self.anchor = None;
        if !had_selection {
            self.revision += 1;
        }
    }

    /// Delete the selection, else the grapheme before the cursor, else join
    /// with the previous line.
    pub fn backspace(&mut self) {
        if self.delete_selection() {
            return;
        }
        let Pos { line, col } = self.cursor;
        if col > 0 {
            let start = col - self.prev_len(line, col);
            self.lines[line].replace_range(start..col, "");
            self.cursor.col = start;
        } else if line > 0 {
            let current = self.lines.remove(line);
            let col = self.lines[line - 1].len();
            self.lines[line - 1].push_str(&current);
            self.cursor = Pos {
                line: line - 1,
                col,
            };
        } else {
            return;
        }
        self.revision += 1;
    }

    /// Delete the selection, else the grapheme after the cursor, else join
    /// with the next line.
    pub fn delete(&mut self) {
        if self.delete_selection() {
            return;
        }
        let Pos { line, col } = self.cursor;
        if col < self.lines[line].len() {
            let len = self.next_len(line, col);
            self.lines[line].replace_range(col..col + len, "");
        } else if line + 1 < self.lines.len() {
            let next = self.lines.remove(line + 1);
            self.lines[line].push_str(&next);
        } else {
            return;
        }
        self.revision += 1;
    }

    /// Replace `range` of `line` with `text` (no line breaks). Cursor and
    /// anchor on the line move with the text; text before them is
    /// untouched.
    pub fn replace_range(&mut self, line: usize, range: Range<usize>, text: &str) {
        let Some(current) = self.lines.get_mut(line) else {
            return;
        };
        let end = range.end.min(current.len());
        let range = range.start.min(end)..end;
        current.replace_range(range.clone(), text);
        for pos in [Some(&mut self.cursor), self.anchor.as_mut()]
            .into_iter()
            .flatten()
        {
            shifted(pos, line, &range, text.len());
            if pos.line == line {
                pos.col = snap(&self.lines[line], pos.col);
            }
        }
        self.revision += 1;
    }

    /// Replace the whole of `line`.
    pub fn set_line(&mut self, line: usize, text: String) {
        let len = self.lines.get(line).map_or(0, String::len);
        self.replace_range(line, 0..len, &text);
    }

    /// Break the line at the cursor (replacing the selection); the new line
    /// starts with `prefix` and the cursor goes behind it.
    pub fn split_line_with(&mut self, prefix: &str) {
        self.delete_selection();
        let Pos { line, col } = self.cursor;
        let tail = self.lines[line].split_off(col);
        self.lines.insert(line + 1, format!("{prefix}{tail}"));
        self.cursor = Pos {
            line: line + 1,
            col: prefix.len(),
        };
        self.anchor = None;
        self.revision += 1;
    }

    // --- Movement ---

    fn prev_len(&self, line: usize, col: usize) -> usize {
        self.lines[line][..col]
            .graphemes(true)
            .next_back()
            .map_or(0, str::len)
    }

    fn next_len(&self, line: usize, col: usize) -> usize {
        self.lines[line][col..]
            .graphemes(true)
            .next()
            .map_or(0, str::len)
    }

    fn doc_end_pos(&self) -> Pos {
        let line = self.lines.len() - 1;
        Pos {
            line,
            col: self.lines[line].len(),
        }
    }

    fn clamp(&self, pos: Pos) -> Pos {
        let line = pos.line.min(self.lines.len() - 1);
        Pos {
            line,
            col: snap(&self.lines[line], pos.col),
        }
    }

    /// Move the cursor, extending the selection when `select` is set and
    /// clearing it otherwise.
    fn go(&mut self, pos: Pos, select: bool) {
        self.typing = false;
        if select {
            self.anchor.get_or_insert(self.cursor);
        } else {
            self.anchor = None;
        }
        self.cursor = pos;
    }

    /// Move to `pos`, clamped to the text and to a grapheme boundary.
    pub fn move_to(&mut self, pos: Pos, select: bool) {
        let pos = self.clamp(pos);
        self.go(pos, select);
    }

    pub fn left(&mut self, select: bool) {
        if !select && let Some((start, _)) = self.selection() {
            self.go(start, false);
            return;
        }
        let Pos { line, col } = self.cursor;
        let target = if col > 0 {
            Pos {
                line,
                col: col - self.prev_len(line, col),
            }
        } else if line > 0 {
            Pos {
                line: line - 1,
                col: self.lines[line - 1].len(),
            }
        } else {
            self.cursor
        };
        self.go(target, select);
    }

    pub fn right(&mut self, select: bool) {
        if !select && let Some((_, end)) = self.selection() {
            self.go(end, false);
            return;
        }
        let Pos { line, col } = self.cursor;
        let target = if col < self.lines[line].len() {
            Pos {
                line,
                col: col + self.next_len(line, col),
            }
        } else if line + 1 < self.lines.len() {
            Pos {
                line: line + 1,
                col: 0,
            }
        } else {
            self.cursor
        };
        self.go(target, select);
    }

    /// To the start of the previous word; at a line start to the end of the
    /// line above.
    pub fn word_left(&mut self, select: bool) {
        let Pos { line, col } = self.cursor;
        let target = if col > 0 {
            let mut start = 0;
            for (i, segment) in self.lines[line][..col].split_word_bound_indices().rev() {
                start = i;
                if !is_blank(segment) {
                    break;
                }
            }
            Pos { line, col: start }
        } else if line > 0 {
            Pos {
                line: line - 1,
                col: self.lines[line - 1].len(),
            }
        } else {
            self.cursor
        };
        self.go(target, select);
    }

    /// To the end of the next word; at a line end to the start of the line
    /// below.
    pub fn word_right(&mut self, select: bool) {
        let Pos { line, col } = self.cursor;
        let target = if col < self.lines[line].len() {
            let mut end = col;
            for (i, segment) in self.lines[line][col..].split_word_bound_indices() {
                end = col + i + segment.len();
                if !is_blank(segment) {
                    break;
                }
            }
            Pos { line, col: end }
        } else if line + 1 < self.lines.len() {
            Pos {
                line: line + 1,
                col: 0,
            }
        } else {
            self.cursor
        };
        self.go(target, select);
    }

    /// To the start of the logical line.
    pub fn home(&mut self, select: bool) {
        let line = self.cursor.line;
        self.go(Pos { line, col: 0 }, select);
    }

    /// To the end of the logical line.
    pub fn end(&mut self, select: bool) {
        let line = self.cursor.line;
        let col = self.lines[line].len();
        self.go(Pos { line, col }, select);
    }

    pub fn doc_start(&mut self, select: bool) {
        self.go(Pos::default(), select);
    }

    pub fn doc_end(&mut self, select: bool) {
        let end = self.doc_end_pos();
        self.go(end, select);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pos(line: usize, col: usize) -> Pos {
        Pos { line, col }
    }

    fn typed(buf: &mut Buffer, text: &str) {
        for c in text.chars() {
            buf.begin(EditKind::Typing);
            buf.insert(&c.to_string());
        }
    }

    #[test]
    fn text_round_trips_with_its_line_endings() {
        for text in ["", "a", "a\n", "a\n\nb\n", "a\r\nb\r\n", "a\r\nb", "\r\n"] {
            assert_eq!(Buffer::new(text).text(), text, "{text:?}");
        }
        let buf = Buffer::new("a\r\nb\r\n");
        assert_eq!(buf.lines(), ["a", "b", ""]);
    }

    #[test]
    fn insert_splits_lines_and_replaces_the_selection() {
        let mut buf = Buffer::new("hello world");
        buf.move_to(pos(0, 5), false);
        buf.insert("\r\nx\ny");
        assert_eq!(buf.lines(), ["hello", "x", "y world"]);
        assert_eq!(buf.cursor(), pos(2, 1));
        buf.move_to(pos(0, 1), false);
        buf.move_to(pos(2, 1), true);
        let revision = buf.revision();
        buf.insert("-");
        assert_eq!(buf.lines(), ["h- world"]);
        assert_eq!(buf.cursor(), pos(0, 2));
        assert_eq!(buf.anchor(), None);
        assert!(buf.revision() > revision);
    }

    #[test]
    fn backspace_and_delete_join_lines_and_remove_graphemes() {
        let mut buf = Buffer::new("a\nb");
        buf.move_to(pos(1, 0), false);
        buf.backspace();
        assert_eq!(
            (buf.lines(), buf.cursor()),
            (&["ab".to_owned()][..], pos(0, 1))
        );
        buf.backspace();
        buf.backspace();
        assert_eq!(buf.lines(), ["b"]);
        let mut buf = Buffer::new("a\nb");
        buf.end(false);
        buf.delete();
        assert_eq!(buf.lines(), ["ab"]);
        buf.doc_end(false);
        let revision = buf.revision();
        buf.delete();
        assert_eq!(buf.revision(), revision);
        // A grapheme goes in one piece: `e` + combining acute, a flag.
        let mut buf = Buffer::new("e\u{301}🇩🇪x");
        buf.move_to(pos(0, 0), false);
        buf.delete();
        assert_eq!(buf.lines(), ["🇩🇪x"]);
        buf.right(false);
        buf.backspace();
        assert_eq!(buf.lines(), ["x"]);
    }

    #[test]
    fn graphemes_move_as_one() {
        let text = "e\u{301}🇩🇪👨‍👩‍👧x";
        let mut buf = Buffer::new(text);
        let mut cols = vec![0];
        while buf.cursor() != buf.doc_end_pos() {
            buf.right(false);
            cols.push(buf.cursor().col);
        }
        assert_eq!(cols, [0, 3, 11, 29, 30]);
        for want in cols.iter().rev().skip(1) {
            buf.left(false);
            assert_eq!(buf.cursor().col, *want);
        }
        // A byte offset inside a grapheme snaps down to its start.
        buf.move_to(pos(0, 1), false);
        assert_eq!(buf.cursor(), pos(0, 0));
        buf.move_to(pos(9, 100), false);
        assert_eq!(buf.cursor(), pos(0, 30));
        let mut buf = Buffer::new("é");
        buf.move_to(pos(0, 1), false);
        assert_eq!(buf.cursor(), pos(0, 0));
    }

    #[test]
    fn left_and_right_cross_lines_and_collapse_selections() {
        let mut buf = Buffer::new("ab\ncd");
        buf.move_to(pos(1, 0), false);
        buf.left(false);
        assert_eq!(buf.cursor(), pos(0, 2));
        buf.right(false);
        assert_eq!(buf.cursor(), pos(1, 0));
        buf.move_to(pos(0, 1), false);
        buf.move_to(pos(1, 1), true);
        assert_eq!(buf.selection(), Some((pos(0, 1), pos(1, 1))));
        buf.left(false);
        assert_eq!((buf.cursor(), buf.anchor()), (pos(0, 1), None));
        buf.move_to(pos(0, 1), false);
        buf.move_to(pos(1, 1), true);
        buf.right(false);
        assert_eq!((buf.cursor(), buf.anchor()), (pos(1, 1), None));
        buf.doc_start(false);
        buf.left(false);
        assert_eq!(buf.cursor(), pos(0, 0));
    }

    #[test]
    fn words_move_over_whitespace_runs() {
        let mut buf = Buffer::new("foo  bar, baz\nqux");
        let mut cols = Vec::new();
        for _ in 0..6 {
            buf.word_right(false);
            cols.push(buf.cursor());
        }
        assert_eq!(
            cols,
            [
                pos(0, 3),
                pos(0, 8),
                pos(0, 9),
                pos(0, 13),
                pos(1, 0),
                pos(1, 3)
            ]
        );
        buf.word_left(false);
        assert_eq!(buf.cursor(), pos(1, 0));
        buf.word_left(false);
        assert_eq!(buf.cursor(), pos(0, 13));
        buf.word_left(false);
        assert_eq!(buf.cursor(), pos(0, 10));
        buf.word_left(false);
        assert_eq!(buf.cursor(), pos(0, 8));
        buf.move_to(pos(0, 5), false);
        buf.word_left(false);
        assert_eq!(buf.cursor(), pos(0, 0));
    }

    #[test]
    fn selection_extends_and_orders() {
        let mut buf = Buffer::new("one\ntwo\nthree");
        buf.move_to(pos(1, 1), false);
        assert_eq!(buf.selection(), None);
        buf.left(true);
        buf.left(true);
        assert_eq!(buf.selection(), Some((pos(0, 3), pos(1, 1))));
        assert_eq!(buf.selected_text().as_deref(), Some("\nt"));
        buf.move_to(pos(2, 2), true);
        assert_eq!(buf.selected_text().as_deref(), Some("wo\nth"));
        buf.select_all();
        assert_eq!(buf.selected_text().as_deref(), Some("one\ntwo\nthree"));
        buf.home(false);
        assert_eq!(buf.selected_text(), None);
        assert_eq!(buf.anchor(), None);
        buf.move_to(pos(0, 1), false);
        buf.end(true);
        assert_eq!(buf.selected_text().as_deref(), Some("ne"));
        assert!(buf.delete_selection());
        assert_eq!(buf.lines()[0], "o");
        assert!(!buf.delete_selection());
    }

    #[test]
    fn replace_range_and_set_line_keep_the_cursor_in_place() {
        let mut buf = Buffer::new("- [ ] milk");
        buf.end(false);
        buf.replace_range(0, 3..4, "x");
        assert_eq!(
            (buf.lines()[0].as_str(), buf.cursor()),
            ("- [x] milk", pos(0, 10))
        );
        buf.replace_range(0, 0..0, "  ");
        assert_eq!(buf.cursor(), pos(0, 12));
        buf.move_to(pos(0, 3), false);
        buf.replace_range(0, 0..0, "  ");
        assert_eq!(buf.cursor(), pos(0, 5));
        buf.move_to(pos(0, 0), false);
        buf.replace_range(0, 0..0, "  ");
        assert_eq!(buf.cursor(), pos(0, 0));
        buf.end(false);
        buf.set_line(0, String::new());
        assert_eq!((buf.lines()[0].as_str(), buf.cursor()), ("", pos(0, 0)));
        buf.replace_range(7, 0..1, "x");
        assert_eq!(buf.text(), "");
    }

    #[test]
    fn split_line_with_a_prefix() {
        let mut buf = Buffer::new("- ab");
        buf.move_to(pos(0, 3), false);
        buf.split_line_with("- ");
        assert_eq!(buf.lines(), ["- a", "- b"]);
        assert_eq!(buf.cursor(), pos(1, 2));
        buf.move_to(pos(0, 0), false);
        buf.move_to(pos(1, 2), true);
        buf.split_line_with("");
        assert_eq!(buf.lines(), ["", "b"]);
        assert_eq!(buf.cursor(), pos(1, 0));
    }

    #[test]
    fn typing_groups_into_one_undo_step() {
        let mut buf = Buffer::new("");
        typed(&mut buf, "abc");
        buf.left(false);
        typed(&mut buf, "X");
        assert_eq!(buf.text(), "abXc");
        assert!(buf.undo());
        assert_eq!(buf.text(), "abc");
        assert!(buf.undo());
        assert_eq!(buf.text(), "");
        assert!(!buf.undo());
        assert!(buf.redo());
        assert_eq!(buf.text(), "abc");
        assert_eq!(buf.cursor(), pos(0, 2));
        assert!(buf.redo());
        assert_eq!(buf.text(), "abXc");
        assert!(!buf.redo());
    }

    #[test]
    fn other_edits_are_their_own_steps() {
        let mut buf = Buffer::new("");
        typed(&mut buf, "ab");
        buf.begin(EditKind::Other);
        buf.split_line_with("");
        typed(&mut buf, "c");
        assert_eq!(buf.lines(), ["ab", "c"]);
        assert!(buf.undo());
        assert_eq!(buf.lines(), ["ab", ""]);
        assert!(buf.undo());
        assert_eq!(buf.lines(), ["ab"]);
        assert_eq!(buf.cursor(), pos(0, 2));
    }

    #[test]
    fn a_new_edit_clears_redo() {
        let mut buf = Buffer::new("");
        typed(&mut buf, "a");
        assert!(buf.undo());
        buf.begin(EditKind::Other);
        buf.insert("b");
        assert!(!buf.redo());
        assert_eq!(buf.text(), "b");
    }

    #[test]
    fn undo_bumps_the_revision() {
        let mut buf = Buffer::new("");
        buf.begin(EditKind::Other);
        buf.insert("a");
        let edited = buf.revision();
        assert!(buf.undo());
        assert!(buf.revision() > edited);
    }

    #[test]
    fn no_op_edits_do_not_add_steps() {
        let mut buf = Buffer::new("a");
        buf.begin(EditKind::Other);
        buf.backspace();
        buf.begin(EditKind::Other);
        buf.backspace();
        assert_eq!(buf.undo.len(), 1);
    }

    #[test]
    fn undo_is_capped() {
        let mut buf = Buffer::new("");
        for _ in 0..UNDO_LIMIT + 50 {
            buf.begin(EditKind::Other);
            buf.insert("x");
        }
        assert_eq!(buf.undo.len(), UNDO_LIMIT);
        while buf.undo() {}
        assert_eq!(buf.text().len(), 50);
    }
}
