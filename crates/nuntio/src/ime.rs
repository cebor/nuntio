//! Showing the IME's uncommitted (preedit) text at the cursor.

use nuntio_term::{Snapshot, SnapshotCell, UnderlineStyle};
use unicode_width::UnicodeWidthChar;

/// Draw `preedit` over the snapshot, starting at the cursor, underlined.
/// The cursor moves to the end of the preedit text. Text that doesn't fit
/// on the cursor line is cut off.
pub fn overlay_preedit(snapshot: &mut Snapshot, preedit: &str) {
    let Some(cursor) = snapshot.cursor.as_mut() else {
        return;
    };
    let (line, columns) = (cursor.line, snapshot.columns);
    let base = snapshot.cells[line * columns + cursor.column].clone();
    let mut column = cursor.column;

    for c in preedit.chars() {
        let width = c.width().unwrap_or(0);
        if width == 0 {
            // Combining mark: attach to the previous preedit cell.
            if column > cursor.column {
                let prev = &mut snapshot.cells[line * columns + column - 1];
                let mut marks = prev.zerowidth.take().map(Vec::from).unwrap_or_default();
                marks.push(c);
                prev.zerowidth = Some(marks.into());
            }
            continue;
        }
        if column + width > columns {
            break;
        }
        let mut cell = SnapshotCell {
            c,
            zerowidth: None,
            ..base.clone()
        };
        cell.style.underline = Some(UnderlineStyle::Single);
        cell.underline_color = None;
        cell.style.wide = width == 2;
        snapshot.cells[line * columns + column] = cell.clone();
        if width == 2 {
            snapshot.cells[line * columns + column + 1] = SnapshotCell {
                c: ' ',
                style: Default::default(),
                ..cell
            };
        }
        column += width;
    }

    cursor.column = column.min(columns - 1);
    cursor.wide = false;
}

#[cfg(test)]
mod tests {
    use nuntio_term::{CellStyle, CursorStyle, Rgb, SnapshotCursor};

    use super::*;

    fn snapshot(columns: usize) -> Snapshot {
        let blank = SnapshotCell {
            c: ' ',
            zerowidth: None,
            fg: Rgb::default(),
            bg: Rgb::default(),
            style: CellStyle::default(),
            underline_color: None,
        };
        Snapshot {
            columns,
            lines: 1,
            cells: vec![blank; columns],
            cursor: Some(SnapshotCursor {
                column: 1,
                line: 0,
                style: CursorStyle::Block,
                color: Rgb::default(),
                wide: false,
                blinking: false,
            }),
            background: Rgb::default(),
            foreground: Rgb::default(),
        }
    }

    fn text(s: &Snapshot) -> String {
        s.cells.iter().map(|c| c.c).collect()
    }

    #[test]
    fn ascii_preedit_is_underlined_and_moves_cursor() {
        let mut s = snapshot(6);
        overlay_preedit(&mut s, "ab");
        assert_eq!(text(&s), " ab   ");
        assert!(s.cell(1, 0).style.underline.is_some());
        assert!(s.cell(3, 0).style.underline.is_none());
        assert_eq!(s.cursor.unwrap().column, 3);
    }

    #[test]
    fn wide_characters_take_two_cells() {
        let mut s = snapshot(6);
        overlay_preedit(&mut s, "日本");
        assert_eq!(text(&s), " 日 本  ");
        assert!(s.cell(1, 0).style.wide);
        assert_eq!(s.cursor.unwrap().column, 5);
    }

    #[test]
    fn overflow_is_cut_off() {
        let mut s = snapshot(4);
        overlay_preedit(&mut s, "abcdef");
        assert_eq!(text(&s), " abc");
        assert_eq!(s.cursor.unwrap().column, 3);
    }
}
