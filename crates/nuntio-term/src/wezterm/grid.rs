//! Reading rows of a wezterm screen: positions by stable row, columns
//! with wide characters, and logical lines joined across soft wraps.

use std::ops::Range;

use wezterm_term::{CellAttributes, CellRef, Hyperlink, Line, Screen, StableRowIndex};

/// Which half of a cell the pointer is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Side {
    Left,
    Right,
}

/// A cell: stable row and column. Orders like the text, row by row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Pos {
    pub(crate) row: StableRowIndex,
    pub(crate) column: usize,
}

impl Pos {
    pub(crate) fn new(row: StableRowIndex, column: usize) -> Self {
        Self { row, column }
    }
}

/// What occupies one column of a row.
#[derive(Clone, Copy)]
pub(crate) enum Col<'a> {
    Cell(CellRef<'a>),
    /// Past the end of the stored cells.
    Blank,
    /// The second column of a wide character.
    Continuation,
}

impl<'a> Col<'a> {
    /// The grapheme; a space for an empty cell, nothing for a
    /// continuation.
    pub(crate) fn text(&self) -> &'a str {
        match self {
            Col::Cell(CellRef::CellRef { cell, .. }) => non_empty(cell.str()),
            Col::Cell(CellRef::ClusterRef { text, .. }) => non_empty(text),
            Col::Blank => " ",
            Col::Continuation => "",
        }
    }

    /// The grapheme's first character, as alacritty stores it per cell.
    pub(crate) fn first_char(&self) -> char {
        self.text().chars().next().unwrap_or(' ')
    }

    pub(crate) fn attrs(&self) -> Option<&'a CellAttributes> {
        match self {
            Col::Cell(CellRef::CellRef { cell, .. }) => Some(cell.attrs()),
            Col::Cell(CellRef::ClusterRef { attrs, .. }) => Some(attrs),
            Col::Blank | Col::Continuation => None,
        }
    }

    pub(crate) fn hyperlink(&self) -> Option<&'a std::sync::Arc<Hyperlink>> {
        self.attrs().and_then(CellAttributes::hyperlink)
    }

    pub(crate) fn is_continuation(&self) -> bool {
        matches!(self, Col::Continuation)
    }
}

fn non_empty(text: &str) -> &str {
    if text.is_empty() { " " } else { text }
}

/// The first `width` columns of `line`.
pub(crate) fn columns(line: &Line, width: usize) -> Vec<Col<'_>> {
    let mut cols = vec![Col::Blank; width];
    for cell in line.visible_cells() {
        let start = cell.cell_index();
        if start >= width {
            break;
        }
        cols[start] = Col::Cell(cell);
        for col in cols.iter_mut().take(start + cell.width()).skip(start + 1) {
            *col = Col::Continuation;
        }
    }
    cols
}

/// The stable rows the screen holds, scrollback included.
pub(crate) fn all_rows(screen: &Screen) -> Range<StableRowIndex> {
    let first = screen.phys_to_stable_row_index(0);
    first..first + screen.scrollback_rows() as StableRowIndex
}

/// All rows of the screen, for reading cells by position.
pub(crate) struct Grid<'a> {
    lines: &'a [&'a Line],
    first: StableRowIndex,
    pub(crate) cols: usize,
}

/// Run `f` with the whole screen. Takes the screen mutably only because
/// `Screen::with_phys_lines` indexes the second half of a wrapped ring
/// buffer wrongly (at the pinned rev); its `_mut` twin gets it right.
pub(crate) fn with_grid<R>(screen: &mut Screen, f: impl FnOnce(&Grid<'_>) -> R) -> R {
    let rows = all_rows(screen);
    let cols = screen.physical_cols;
    let len = screen.scrollback_rows();
    let mut f = Some(f);
    let mut out = None;
    screen.with_phys_lines_mut(0..len, |lines| {
        let lines: Vec<&Line> = lines.iter().map(|line| &**line).collect();
        if let Some(f) = f.take() {
            out = Some(f(&Grid {
                lines: &lines,
                first: rows.start,
                cols,
            }));
        }
    });
    out.expect("with_phys_lines_mut calls back once")
}

impl<'a> Grid<'a> {
    /// The oldest row.
    pub(crate) fn first(&self) -> StableRowIndex {
        self.first
    }

    /// One past the newest row.
    pub(crate) fn end(&self) -> StableRowIndex {
        self.first + self.lines.len() as StableRowIndex
    }

    pub(crate) fn contains_row(&self, row: StableRowIndex) -> bool {
        (self.first..self.end()).contains(&row)
    }

    pub(crate) fn line(&self, row: StableRowIndex) -> Option<&'a Line> {
        let index = usize::try_from(row - self.first).ok()?;
        self.lines.get(index).copied()
    }

    /// The columns of a row; blank for rows that don't exist.
    pub(crate) fn row(&self, row: StableRowIndex) -> Vec<Col<'a>> {
        match self.line(row) {
            Some(line) => columns(line, self.cols),
            None => vec![Col::Blank; self.cols],
        }
    }

    /// The row continues on the next one (soft wrap).
    pub(crate) fn wraps(&self, row: StableRowIndex) -> bool {
        self.line(row).is_some_and(Line::last_cell_was_wrapped)
    }

    /// The rows of the logical line through `row`, at most `max_rows` up
    /// and down from it.
    pub(crate) fn logical_rows(
        &self,
        row: StableRowIndex,
        max_rows: usize,
    ) -> Range<StableRowIndex> {
        let max = max_rows as StableRowIndex;
        let mut top = row;
        while top > self.first && self.wraps(top - 1) && row - top < max {
            top -= 1;
        }
        let mut bottom = row;
        while bottom + 1 < self.end() && self.wraps(bottom) && bottom - row < max {
            bottom += 1;
        }
        top..bottom + 1
    }

    /// Call `f` with each logical line from the one through `start` to
    /// the one through `end - 1`, until it returns `false`.
    pub(crate) fn logical_lines(
        &self,
        rows: Range<StableRowIndex>,
        mut f: impl FnMut(&LogicalLine) -> bool,
    ) {
        let end = rows.end.min(self.end());
        let mut row = rows.start.max(self.first);
        while row > self.first && self.wraps(row - 1) {
            row -= 1;
        }
        let mut line = LogicalLine::default();
        while row < end {
            let mut last = row;
            while last + 1 < self.end() && self.wraps(last) {
                last += 1;
            }
            self.fill(&mut line, row..last + 1);
            if !f(&line) {
                return;
            }
            row = last + 1;
        }
    }

    /// Read `rows` into `line`.
    pub(crate) fn fill(&self, line: &mut LogicalLine, rows: Range<StableRowIndex>) {
        line.text.clear();
        line.cells.clear();
        line.rows = rows.clone();
        for row in rows {
            for (column, col) in self.row(row).iter().enumerate() {
                if col.is_continuation() {
                    continue;
                }
                line.cells.push(TextCell {
                    pos: Pos::new(row, column),
                    byte: line.text.len(),
                });
                line.text.push_str(col.text());
            }
        }
    }
}

/// Rows joined by soft wraps, as text.
#[derive(Debug, Default)]
pub(crate) struct LogicalLine {
    pub(crate) rows: Range<StableRowIndex>,
    pub(crate) text: String,
    /// The cells in text order, continuation columns left out.
    pub(crate) cells: Vec<TextCell>,
}

/// Where a cell's grapheme starts in [`LogicalLine::text`].
#[derive(Debug, Clone, Copy)]
pub(crate) struct TextCell {
    pub(crate) pos: Pos,
    pub(crate) byte: usize,
}

impl LogicalLine {
    /// The cell whose grapheme holds byte `byte` of the text.
    pub(crate) fn cell_at(&self, byte: usize) -> Pos {
        let index = self.cells.partition_point(|c| c.byte <= byte);
        self.cells[index.saturating_sub(1)].pos
    }
}
