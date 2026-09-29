//! Selections on a wezterm screen, with the semantics of
//! `alacritty_terminal` 0.26 (`selection.rs`, `term/search.rs`,
//! `Term::selection_to_string`).

use std::mem;
use std::ops::Range;

use alacritty_terminal::term::SEMANTIC_ESCAPE_CHARS;
use wezterm_term::StableRowIndex;

use super::grid::{Col, Grid, Pos, Side};
use crate::pane::SelectionKind;

/// A double click on one of these selects up to its partner.
const BRACKET_PAIRS: [(char, char); 4] = [('(', ')'), ('[', ']'), ('{', '}'), ('<', '>')];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Anchor {
    pos: Pos,
    side: Side,
}

#[derive(Debug, Clone)]
pub(crate) struct Selection {
    kind: SelectionKind,
    start: Anchor,
    end: Anchor,
}

/// The selected cells: from `start` to `end`, inclusive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SelectionRange {
    pub(crate) start: Pos,
    pub(crate) end: Pos,
    pub(crate) is_block: bool,
}

impl SelectionRange {
    pub(crate) fn contains(&self, pos: Pos) -> bool {
        self.start.row <= pos.row
            && self.end.row >= pos.row
            && (self.start.column <= pos.column || (self.start.row != pos.row && !self.is_block))
            && (self.end.column >= pos.column || (self.end.row != pos.row && !self.is_block))
    }
}

impl Selection {
    pub(crate) fn new(kind: SelectionKind, pos: Pos, side: Side) -> Self {
        let anchor = Anchor { pos, side };
        Self {
            kind,
            start: anchor,
            end: anchor,
        }
    }

    pub(crate) fn update(&mut self, pos: Pos, side: Side) {
        self.end = Anchor { pos, side };
    }

    /// Both ends lie in rows that are gone.
    pub(crate) fn is_stale(&self, grid: &Grid<'_>) -> bool {
        !grid.contains_row(self.start.pos.row) && !grid.contains_row(self.end.pos.row)
    }

    fn is_empty(&self) -> bool {
        let (start, end) = (self.start, self.end);
        match self.kind {
            SelectionKind::Simple => {
                let (start, end) = if start.pos > end.pos {
                    (end, start)
                } else {
                    (start, end)
                };
                start == end
                    || (start.side == Side::Right
                        && end.side == Side::Left
                        && start.pos.row == end.pos.row
                        && start.pos.column + 1 == end.pos.column)
            }
            SelectionKind::Block => {
                (start.pos.column == end.pos.column && start.side == end.side)
                    || (start.pos.column + 1 == end.pos.column
                        && start.side == Side::Right
                        && end.side == Side::Left)
                    || (end.pos.column + 1 == start.pos.column
                        && start.side == Side::Left
                        && end.side == Side::Right)
            }
            SelectionKind::Semantic | SelectionKind::Lines => false,
        }
    }

    pub(crate) fn to_range(&self, grid: &Grid<'_>) -> Option<SelectionRange> {
        let (mut start, mut end) = (self.start, self.end);
        if start.pos > end.pos {
            mem::swap(&mut start, &mut end);
        }
        if end.pos.row < grid.first() || grid.end() <= grid.first() {
            return None;
        }
        start.pos = clamp(grid, start.pos);
        end.pos = clamp(grid, end.pos);

        match self.kind {
            SelectionKind::Simple => self.range_simple(start, end, grid.cols),
            SelectionKind::Block => self.range_block(start, end),
            SelectionKind::Semantic => Some(range_semantic(grid, start.pos, end.pos)),
            SelectionKind::Lines => Some(SelectionRange {
                start: line_search_left(grid, start.pos),
                end: line_search_right(grid, end.pos),
                is_block: false,
            }),
        }
    }

    fn range_simple(
        &self,
        mut start: Anchor,
        mut end: Anchor,
        cols: usize,
    ) -> Option<SelectionRange> {
        if self.is_empty() {
            return None;
        }
        // Leave out the last cell if the selection ends on its left half.
        if end.side == Side::Left && start.pos != end.pos {
            if end.pos.column == 0 {
                end.pos = Pos::new(end.pos.row - 1, cols - 1);
            } else {
                end.pos.column -= 1;
            }
        }
        // Leave out the first cell if the selection starts on its right half.
        if start.side == Side::Right && start.pos != end.pos {
            start.pos.column += 1;
            if start.pos.column == cols {
                start.pos = Pos::new(start.pos.row + 1, 0);
            }
        }
        Some(SelectionRange {
            start: start.pos,
            end: end.pos,
            is_block: false,
        })
    }

    fn range_block(&self, mut start: Anchor, mut end: Anchor) -> Option<SelectionRange> {
        if self.is_empty() {
            return None;
        }
        // Always top left to bottom right.
        if start.pos.column > end.pos.column {
            mem::swap(&mut start.side, &mut end.side);
            mem::swap(&mut start.pos.column, &mut end.pos.column);
        }
        if end.side == Side::Left && start.pos != end.pos && end.pos.column > 0 {
            end.pos.column -= 1;
        }
        if start.side == Side::Right && start.pos != end.pos {
            start.pos.column += 1;
        }
        Some(SelectionRange {
            start: start.pos,
            end: end.pos,
            is_block: true,
        })
    }

    /// The selected text; `None` if nothing is selected.
    pub(crate) fn text(&self, grid: &Grid<'_>) -> Option<String> {
        let SelectionRange { start, end, .. } = self.to_range(grid)?;
        let text = match self.kind {
            SelectionKind::Block => {
                let columns = start.column..end.column;
                let mut text = String::new();
                for row in start.row..end.row {
                    text += line_to_string(grid, row, columns.clone()).trim_end();
                    text.push('\n');
                }
                text += line_to_string(grid, end.row, columns).trim_end();
                text
            }
            SelectionKind::Lines => bounds_to_string(grid, start, end) + "\n",
            SelectionKind::Simple | SelectionKind::Semantic => bounds_to_string(grid, start, end),
        };
        Some(text).filter(|t| !t.is_empty())
    }
}

/// `pos` moved into the grid, like alacritty's `Boundary::Grid` clamp.
fn clamp(grid: &Grid<'_>, pos: Pos) -> Pos {
    if pos.row < grid.first() {
        Pos::new(grid.first(), 0)
    } else if pos.row >= grid.end() {
        Pos::new(grid.end() - 1, grid.cols - 1)
    } else {
        Pos::new(pos.row, pos.column.min(grid.cols - 1))
    }
}

/// Steps through cells, reading each row once.
struct Walker<'g, 'a> {
    grid: &'g Grid<'a>,
    row: Option<(StableRowIndex, Vec<Col<'a>>)>,
}

impl<'g, 'a> Walker<'g, 'a> {
    fn new(grid: &'g Grid<'a>) -> Self {
        Self { grid, row: None }
    }

    fn col(&mut self, pos: Pos) -> Col<'a> {
        if self.row.as_ref().is_none_or(|(row, _)| *row != pos.row) {
            self.row = Some((pos.row, self.grid.row(pos.row)));
        }
        let (_, cols) = self.row.as_ref().expect("just set");
        cols.get(pos.column).copied().unwrap_or(Col::Blank)
    }

    fn next(&self, pos: Pos) -> Option<Pos> {
        if pos.column + 1 < self.grid.cols {
            Some(Pos::new(pos.row, pos.column + 1))
        } else if pos.row + 1 < self.grid.end() {
            Some(Pos::new(pos.row + 1, 0))
        } else {
            None
        }
    }

    fn prev(&self, pos: Pos) -> Option<Pos> {
        if pos.column > 0 {
            Some(Pos::new(pos.row, pos.column - 1))
        } else if pos.row > self.grid.first() {
            Some(Pos::new(pos.row - 1, self.grid.cols - 1))
        } else {
            None
        }
    }
}

fn range_semantic(grid: &Grid<'_>, mut start: Pos, mut end: Pos) -> SelectionRange {
    if start == end
        && let Some(matching) = bracket_search(grid, start)
    {
        if matching < start {
            start = matching;
        } else {
            end = matching;
        }
        return SelectionRange {
            start,
            end,
            is_block: false,
        };
    }
    SelectionRange {
        start: semantic_search_left(grid, start),
        end: semantic_search_right(grid, end),
        is_block: false,
    }
}

/// The bracket matching the one at `pos`.
fn bracket_search(grid: &Grid<'_>, pos: Pos) -> Option<Pos> {
    let mut walker = Walker::new(grid);
    let start_char = walker.col(pos).first_char();
    let (forward, end_char) = BRACKET_PAIRS.iter().find_map(|&(open, close)| {
        if open == start_char {
            Some((true, close))
        } else if close == start_char {
            Some((false, open))
        } else {
            None
        }
    })?;
    // Each further bracket like the first one skips one partner.
    let mut skip_pairs = 0;
    let mut pos = pos;
    loop {
        pos = if forward {
            walker.next(pos)?
        } else {
            walker.prev(pos)?
        };
        let c = walker.col(pos).first_char();
        if c == end_char && skip_pairs == 0 {
            return Some(pos);
        } else if c == start_char {
            skip_pairs += 1;
        } else if c == end_char {
            skip_pairs -= 1;
        }
    }
}

fn is_separator(col: Col<'_>) -> bool {
    !col.is_continuation() && SEMANTIC_ESCAPE_CHARS.contains(col.first_char())
}

fn semantic_search_left(grid: &Grid<'_>, pos: Pos) -> Pos {
    let mut walker = Walker::new(grid);
    let last = grid.cols - 1;
    let mut pos = pos;
    let mut found = false;
    while let Some(prev) = walker.prev(pos) {
        if prev.column == last && !grid.wraps(prev.row) {
            break;
        }
        pos = prev;
        if is_separator(walker.col(pos)) {
            found = true;
            break;
        }
    }
    if !found {
        return pos;
    }
    // Back to the first cell after the separator.
    let mut next = pos;
    while let Some(p) = walker.next(next) {
        next = p;
        if !walker.col(next).is_continuation() {
            return next;
        }
    }
    pos
}

fn semantic_search_right(grid: &Grid<'_>, pos: Pos) -> Pos {
    let mut walker = Walker::new(grid);
    let last = grid.cols - 1;
    if pos.column == last && !grid.wraps(pos.row) {
        return pos;
    }
    let mut pos = pos;
    while let Some(next) = walker.next(pos) {
        pos = next;
        if is_separator(walker.col(pos)) {
            return walker.prev(pos).unwrap_or(pos);
        }
        if pos.column == last && !grid.wraps(pos.row) {
            break;
        }
    }
    pos
}

/// The start of the logical line.
fn line_search_left(grid: &Grid<'_>, mut pos: Pos) -> Pos {
    while pos.row > grid.first() && grid.wraps(pos.row - 1) {
        pos.row -= 1;
    }
    Pos::new(pos.row, 0)
}

/// The end of the logical line.
fn line_search_right(grid: &Grid<'_>, mut pos: Pos) -> Pos {
    while pos.row + 1 < grid.end() && grid.wraps(pos.row) {
        pos.row += 1;
    }
    Pos::new(pos.row, grid.cols - 1)
}

fn bounds_to_string(grid: &Grid<'_>, start: Pos, end: Pos) -> String {
    let mut text = String::new();
    for row in start.row..=end.row {
        let first = if row == start.row { start.column } else { 0 };
        let last = if row == end.row {
            end.column
        } else {
            grid.cols - 1
        };
        text += &line_to_string(grid, row, first..last);
    }
    match text.strip_suffix('\n') {
        Some(stripped) => stripped.to_owned(),
        None => text,
    }
}

/// The text of `columns` of a row, where `columns.end` is the last column
/// included. Rows that don't wrap end in a newline when the range reaches
/// the last column; their trailing blanks are left out.
fn line_to_string(grid: &Grid<'_>, row: StableRowIndex, columns: Range<usize>) -> String {
    let cols = grid.row(row);
    let wraps = grid.wraps(row);
    // Up to the last non-blank cell; all of a row that wraps.
    let length = if wraps {
        grid.cols
    } else {
        cols.iter()
            .rposition(|c| !c.is_continuation() && c.text() != " ")
            .map_or(0, |i| i + 1)
    };
    let length = length.min(columns.end + 1);
    // A selected continuation column takes its wide character along.
    let mut start = columns.start;
    if start > 0 && cols.get(start).is_some_and(Col::is_continuation) {
        start -= 1;
    }
    let mut text = String::new();
    for col in cols.get(start..length).into_iter().flatten() {
        text.push_str(col.text());
    }
    if columns.end >= grid.cols - 1 && !wraps {
        text.push('\n');
    }
    text
}
