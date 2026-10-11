//! Soft wrapping of a line's pieces into screen rows, and the mapping
//! between source bytes and columns.
//!
//! A `RowPiece` without decoration is either a run of printable ASCII (one
//! cell per byte) or a single grapheme, so `locate` and `hit` need no text.
//! Columns are absolute within the text area: they include `Row::x`.

use std::ops::Range;

use ratatui::style::Style;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use super::markdown::{Piece, PieceKind};

/// Display width of a tab, in cells.
const TAB_WIDTH: u16 = 4;

/// One screen row of a line.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub line: usize,
    /// Indent of the row in cells; `pieces` start there.
    pub x: u16,
    pub pieces: Vec<RowPiece>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RowPiece {
    /// Byte range in the source line.
    pub src: Range<usize>,
    pub deco: Option<&'static str>,
    pub style: Style,
    pub kind: PieceKind,
    /// Display width in cells.
    pub width: u16,
}

/// A grapheme or a whole decoration: the unit of wrapping.
struct Atom {
    piece: usize,
    src: Range<usize>,
    deco: Option<&'static str>,
    width: u16,
    space: bool,
    /// A printable ASCII byte, mergeable with its neighbours.
    ascii: bool,
}

fn atoms(text: &str, pieces: &[Piece]) -> Vec<Atom> {
    let mut out = Vec::new();
    for (piece, p) in pieces.iter().enumerate() {
        if let Some(deco) = p.deco {
            out.push(Atom {
                piece,
                src: p.src.clone(),
                deco: Some(deco),
                width: clamp_width(deco.width()),
                space: false,
                ascii: false,
            });
            continue;
        }
        let base = p.src.start;
        for (off, g) in text[p.src.clone()].grapheme_indices(true) {
            let width = if g == "\t" {
                TAB_WIDTH
            } else {
                clamp_width(g.width())
            };
            out.push(Atom {
                piece,
                src: base + off..base + off + g.len(),
                deco: None,
                width,
                space: g.chars().all(char::is_whitespace),
                ascii: g.len() == 1 && (0x20..0x7f).contains(&g.as_bytes()[0]),
            });
        }
    }
    out
}

fn clamp_width(width: usize) -> u16 {
    width.min(usize::from(u16::MAX)) as u16
}

fn row_of(line: usize, x: u16, atoms: &[Atom], pieces: &[Piece]) -> Row {
    let mut row = Row {
        line,
        x,
        pieces: Vec::new(),
    };
    for a in atoms {
        if let Some(last) = row.pieces.last_mut()
            && a.ascii
            && last.deco.is_none()
            && last.src.end == a.src.start
            && last.width as usize == last.src.len()
            && same_look(last, &pieces[a.piece])
        {
            last.src.end = a.src.end;
            last.width += a.width;
            continue;
        }
        let p = &pieces[a.piece];
        row.pieces.push(RowPiece {
            src: a.src.clone(),
            deco: a.deco,
            style: p.style,
            kind: p.kind,
            width: a.width,
        });
    }
    row
}

/// Whether `piece` is drawn like `last`, so ASCII runs may merge.
fn same_look(last: &RowPiece, piece: &Piece) -> bool {
    last.style == piece.style && last.kind == piece.kind
}

/// Wraps `pieces` of source line `text` into rows of at most `width`
/// cells. Continuation rows start at `hang` when it is at most half the
/// width, else at 0. Breaks after the last space of a row, else mid-word;
/// a space that doesn't fit stays at the end of its row, one cell over.
pub fn wrap(line: usize, text: &str, pieces: &[Piece], hang: u16, width: u16) -> Vec<Row> {
    let width = width.max(1);
    if let Some(rule) = pieces.iter().find(|p| p.kind == PieceKind::Rule) {
        return vec![Row {
            line,
            x: 0,
            pieces: vec![RowPiece {
                src: rule.src.clone(),
                deco: None,
                style: rule.style,
                kind: PieceKind::Rule,
                width,
            }],
        }];
    }
    let all = atoms(text, pieces);
    let continuation = if hang <= width / 2 { hang } else { 0 };
    let mut rows = Vec::new();
    let mut x = 0u16;
    let mut start = 0; // first atom of the current row
    let mut used = 0u16;
    let mut last_space = 0; // atoms up to and including the last space
    let mut i = 0;
    while i < all.len() {
        let a = &all[i];
        let avail = (width - x).max(1);
        if i > start && used.saturating_add(a.width) > avail {
            let end = if a.space {
                i + 1
            } else if last_space > start {
                last_space
            } else {
                i
            };
            rows.push(row_of(line, x, &all[start..end], pieces));
            x = continuation;
            start = end;
            used = 0;
            last_space = start;
            i = end;
            continue;
        }
        used = used.saturating_add(a.width);
        i += 1;
        if a.space {
            last_space = i;
        }
    }
    rows.push(row_of(line, x, &all[start..], pieces));
    rows
}

/// The row of a line (index into `rows`) and the absolute column where
/// source byte `byte` is shown. A byte in hidden text maps to the next
/// shown one; the end of the line to after the last piece.
pub fn locate(rows: &[Row], byte: usize) -> (usize, u16) {
    for (index, row) in rows.iter().enumerate() {
        let mut col = row.x;
        for p in &row.pieces {
            if byte < p.src.end {
                let inside = byte.saturating_sub(p.src.start);
                if inside > 0 && p.deco.is_none() && usize::from(p.width) == p.src.len() {
                    col = col.saturating_add(inside as u16);
                }
                return (index, col);
            }
            col = col.saturating_add(p.width);
        }
    }
    let last = rows.len().saturating_sub(1);
    let col = rows.last().map_or(0, |row| {
        row.pieces
            .iter()
            .fold(row.x, |col, p| col.saturating_add(p.width))
    });
    (last, col)
}

/// The source byte at absolute column `col` of `row` and the kind of piece
/// there. Left of the row: its first byte; past its end: the end of the
/// row's text; a decoration: the start of its source. A row without pieces
/// yields byte 0.
pub fn hit(row: &Row, col: u16) -> (usize, PieceKind) {
    let mut at = row.x;
    for p in &row.pieces {
        if col < at.saturating_add(p.width) || (p.width == 0 && col <= at) {
            if col < at {
                return (p.src.start, p.kind);
            }
            let inside = usize::from(col - at);
            let ascii_run = p.deco.is_none() && usize::from(p.width) == p.src.len();
            let byte = if ascii_run {
                p.src.start + inside
            } else {
                p.src.start
            };
            return (byte, p.kind);
        }
        at = at.saturating_add(p.width);
    }
    match row.pieces.last() {
        Some(p) => (p.src.end, PieceKind::Text),
        None => (0, PieceKind::Text),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notes::markdown::render;

    fn piece(src: Range<usize>) -> Piece {
        Piece {
            src,
            deco: None,
            style: Style::new(),
            kind: PieceKind::Text,
        }
    }

    fn row_text(text: &str, row: &Row) -> String {
        row.pieces
            .iter()
            .map(|p| p.deco.unwrap_or(&text[p.src.clone()]))
            .collect()
    }

    #[test]
    fn wraps_at_spaces() {
        let text = "aaaa bbbb cccc dddd eeee";
        assert_eq!(text.len(), 24);
        let rows = wrap(3, text, &[piece(0..24)], 0, 10);
        let shown: Vec<_> = rows.iter().map(|r| row_text(text, r)).collect();
        assert_eq!(shown, ["aaaa bbbb ", "cccc dddd ", "eeee"]);
        assert!(rows.iter().all(|r| r.line == 3));
    }

    #[test]
    fn a_space_at_the_edge_stays_on_its_row() {
        let text = "aaaa bbbb cccc";
        let rows = wrap(0, text, &[piece(0..14)], 0, 9);
        let shown: Vec<_> = rows.iter().map(|r| row_text(text, r)).collect();
        assert_eq!(shown, ["aaaa bbbb ", "cccc"]);
    }

    #[test]
    fn hard_break_without_spaces() {
        let text = "abcdefghijklmno";
        let rows = wrap(0, text, &[piece(0..15)], 0, 10);
        let shown: Vec<_> = rows.iter().map(|r| row_text(text, r)).collect();
        assert_eq!(shown, ["abcdefghij", "klmno"]);
    }

    #[test]
    fn continuation_rows_hang() {
        let line = "- aaaa bbbb cccc dddd";
        let views = render(&[line.to_owned()]);
        let v = &views[0];
        assert_eq!(v.hang, 2);
        let rows = wrap(0, line, &v.rendered, v.hang, 12);
        assert!(rows.len() > 1);
        assert_eq!(rows[0].x, 0);
        assert!(rows[1..].iter().all(|r| r.x == 2));
        // The hang is ignored when it takes more than half the width.
        let rows = wrap(0, line, &v.rendered, v.hang, 3);
        assert!(rows.iter().all(|r| r.x == 0));
    }

    #[test]
    fn empty_line_and_tiny_width() {
        let rows = wrap(5, "", &[], 0, 10);
        assert_eq!(rows.len(), 1);
        assert!(rows[0].pieces.is_empty());
        let rows = wrap(0, "ab", &[piece(0..2)], 4, 0);
        assert_eq!(rows.len(), 2);
    }

    #[test]
    fn wide_and_tab_widths() {
        let text = "a\tb世";
        let rows = wrap(0, text, &[piece(0..text.len())], 0, 20);
        let widths: Vec<u16> = rows[0].pieces.iter().map(|p| p.width).collect();
        assert_eq!(widths, [1, 4, 1, 2]);
        assert_eq!(locate(&rows, 3), (0, 6));
        assert_eq!(hit(&rows[0], 7).0, 3);
    }

    #[test]
    fn locate_and_hit_round_trip() {
        let line = "a **bold** b";
        let v = &render(&[line.to_owned()])[0];
        for pieces in [&v.raw, &v.rendered] {
            let rows = wrap(0, line, pieces, 0, 40);
            let shown_bytes: Vec<usize> = pieces
                .iter()
                .flat_map(|p| p.src.clone())
                .filter(|&b| line.is_char_boundary(b))
                .collect();
            for &b in &shown_bytes {
                let (r, col) = locate(&rows, b);
                assert_eq!(r, 0);
                assert_eq!(hit(&rows[r], col).0, b, "byte {b}");
            }
            let (_, end) = locate(&rows, line.len());
            assert_eq!(hit(&rows[0], end + 5).0, pieces.last().unwrap().src.end);
        }
        // Rendered: the hidden `**` maps to the next visible byte.
        let rows = wrap(0, line, &v.rendered, 0, 40);
        assert_eq!(locate(&rows, 2), locate(&rows, 4));
        assert_eq!(locate(&rows, 4).1, 2);
    }

    #[test]
    fn decorations_are_atomic() {
        let line = "- [ ] milk";
        let v = &render(&[line.to_owned()])[0];
        let rows = wrap(0, line, &v.rendered, v.hang, 40);
        let (byte, kind) = hit(&rows[0], 0);
        assert_eq!((byte, kind), (0, PieceKind::Checkbox));
        assert_eq!(hit(&rows[0], 3).0, 7);
        assert_eq!(locate(&rows, 3), (0, 0));
        assert_eq!(locate(&rows, 5), (0, 1));
    }

    #[test]
    fn rule_fills_the_row() {
        let v = &render(&["---".to_owned()])[0];
        let rows = wrap(0, "---", &v.rendered, 0, 30);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].pieces[0].width, 30);
        assert_eq!(hit(&rows[0], 10).1, PieceKind::Rule);
    }
}
