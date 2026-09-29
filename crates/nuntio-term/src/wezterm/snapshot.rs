//! Copying the visible part of a wezterm screen for rendering.

use termwiz::surface::{CursorShape, CursorVisibility};
use wezterm_term::color::ColorAttribute;
use wezterm_term::{Intensity, Underline};

use super::State;
use super::config::rgb;
use super::grid::{self, Col, Pos};
use super::search::WezMatch;
use crate::palette::dim;
use crate::snapshot::{
    BLACK, CellStyle, CursorStyle, Snapshot, SnapshotCell, SnapshotCursor, UnderlineStyle,
};

/// Copy the viewport. `matches` are highlighted as search results, in
/// grid order; `current` as the selected one.
pub(crate) fn capture(
    state: &mut State,
    matches: &[WezMatch],
    current: Option<&WezMatch>,
) -> Snapshot {
    let top = state.viewport_top();
    let palette = &state.palette;
    let term = &mut state.term;
    // The runtime palette, with OSC 4/10/11/12 overrides.
    let colors = term.palette();
    let cursor = term.cursor_pos();
    let screen = term.screen_mut();
    let cursor_line = screen.visible_row_to_stable_row(cursor.y) - top;
    let columns = screen.physical_cols;
    let lines = screen.physical_rows;
    let background = rgb(colors.background);
    let foreground = rgb(colors.foreground);
    let blank = SnapshotCell {
        c: ' ',
        zerowidth: None,
        fg: foreground,
        bg: background,
        style: CellStyle::default(),
        underline_color: None,
    };
    let mut cells = vec![blank.clone(); columns * lines];
    let mut next_match = 0;

    grid::with_grid(screen, |grid| {
        let selection = state.selection.as_ref().and_then(|s| s.to_range(grid));
        for line in 0..lines {
            let stable = top + line as isize;
            let Some(row) = grid.line(stable) else {
                continue;
            };
            // Colors of the wide character before a continuation column.
            let mut wide_colors = (foreground, background);
            for (column, col) in grid::columns(row, columns).into_iter().enumerate() {
                let pos = Pos::new(stable, column);
                let mut cell = match col {
                    Col::Cell(cell) => {
                        let attrs = cell.attrs();
                        let mut fg = rgb(colors.resolve_fg(attrs.foreground()));
                        if attrs.intensity() == Intensity::Half {
                            fg = dim(fg);
                        }
                        let mut bg = rgb(colors.resolve_bg(attrs.background()));
                        if attrs.reverse() {
                            std::mem::swap(&mut fg, &mut bg);
                        }
                        wide_colors = (fg, bg);
                        let hidden = attrs.invisible();
                        let mut chars = col.text().chars();
                        let c = if hidden {
                            ' '
                        } else {
                            chars.next().unwrap_or(' ')
                        };
                        let zerowidth: Box<[char]> = chars.collect();
                        SnapshotCell {
                            c,
                            zerowidth: (!hidden && !zerowidth.is_empty()).then_some(zerowidth),
                            fg,
                            bg,
                            style: CellStyle {
                                bold: attrs.intensity() == Intensity::Bold,
                                italic: attrs.italic(),
                                underline: underline(attrs.underline()),
                                strikeout: attrs.strikethrough(),
                                wide: cell.width() == 2,
                            },
                            underline_color: match attrs.underline_color() {
                                ColorAttribute::Default => None,
                                color => Some(rgb(colors.resolve_fg(color))),
                            },
                        }
                    }
                    Col::Blank => blank.clone(),
                    Col::Continuation => SnapshotCell {
                        fg: wide_colors.0,
                        bg: wide_colors.1,
                        ..blank.clone()
                    },
                };
                if selection.is_some_and(|s| s.contains(pos)) {
                    cell.fg = palette.selection_foreground;
                    cell.bg = palette.selection_background;
                }
                if current.is_some_and(|m| m.contains(pos)) {
                    (cell.fg, cell.bg) = (BLACK, palette.search_current);
                } else {
                    while matches.get(next_match).is_some_and(|m| m.end < pos) {
                        next_match += 1;
                    }
                    if matches.get(next_match).is_some_and(|m| m.contains(pos)) {
                        (cell.fg, cell.bg) = (BLACK, palette.search_match);
                    }
                }
                cells[line * columns + column] = cell;
            }
        }
    });

    let style = match cursor.shape {
        CursorShape::Default | CursorShape::SteadyBlock => (CursorStyle::Block, false),
        CursorShape::BlinkingBlock => (CursorStyle::Block, true),
        CursorShape::SteadyUnderline => (CursorStyle::Underline, false),
        CursorShape::BlinkingUnderline => (CursorStyle::Underline, true),
        CursorShape::SteadyBar => (CursorStyle::Beam, false),
        CursorShape::BlinkingBar => (CursorStyle::Beam, true),
    };
    let cursor = (cursor.visibility == CursorVisibility::Visible
        && (0..lines as isize).contains(&cursor_line)
        && columns > 0)
        .then(|| {
            let (line, column) = (cursor_line as usize, cursor.x.min(columns - 1));
            SnapshotCursor {
                column,
                line,
                style: style.0,
                color: rgb(colors.cursor_bg),
                wide: cells[line * columns + column].style.wide,
                blinking: style.1,
            }
        });

    Snapshot {
        columns,
        lines,
        cells,
        cursor,
        background,
        foreground,
    }
}

fn underline(underline: Underline) -> Option<UnderlineStyle> {
    Some(match underline {
        Underline::None => return None,
        Underline::Single => UnderlineStyle::Single,
        Underline::Double => UnderlineStyle::Double,
        Underline::Curly => UnderlineStyle::Curly,
        Underline::Dotted => UnderlineStyle::Dotted,
        Underline::Dashed => UnderlineStyle::Dashed,
    })
}
