use alacritty_terminal::event::EventListener;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::color::Colors;
use alacritty_terminal::term::{Term, TermMode};
use alacritty_terminal::vte::ansi::{Color, CursorShape, NamedColor, Rgb};

use crate::palette::{Palette, dim};

/// Style bits the renderer cares about.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct CellStyle {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strikeout: bool,
    /// The glyph spans two columns.
    pub wide: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SnapshotCell {
    pub c: char,
    /// Combining characters following `c`, if any.
    pub zerowidth: Option<Box<[char]>>,
    pub fg: Rgb,
    pub bg: Rgb,
    pub style: CellStyle,
}

impl SnapshotCell {
    /// Nothing to draw besides the background.
    pub fn is_blank(&self) -> bool {
        (self.c == ' ' || self.c == '\t') && self.zerowidth.is_none()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorStyle {
    Block,
    HollowBlock,
    Underline,
    Beam,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SnapshotCursor {
    pub column: usize,
    pub line: usize,
    pub style: CursorStyle,
    pub color: Rgb,
    /// Cursor sits on a double-width glyph.
    pub wide: bool,
    /// The application asked for a blinking cursor (DECSCUSR).
    pub blinking: bool,
}

/// A copy of the visible screen, taken while briefly holding the term lock,
/// so rendering never blocks the PTY thread.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub columns: usize,
    pub lines: usize,
    /// Row-major, `columns * lines` entries. Spacer cells behind wide glyphs are blank.
    pub cells: Vec<SnapshotCell>,
    pub cursor: Option<SnapshotCursor>,
    pub background: Rgb,
    pub foreground: Rgb,
}

impl Snapshot {
    pub fn cell(&self, column: usize, line: usize) -> &SnapshotCell {
        &self.cells[line * self.columns + column]
    }

    pub(crate) fn capture<T: EventListener>(term: &Term<T>, palette: &Palette) -> Self {
        let content = term.renderable_content();
        let overrides = content.colors;
        let columns = term.columns();
        let lines = term.screen_lines();
        let background = palette.resolve(Color::Named(NamedColor::Background), overrides);
        let foreground = palette.resolve(Color::Named(NamedColor::Foreground), overrides);

        let blank = SnapshotCell {
            c: ' ',
            zerowidth: None,
            fg: foreground,
            bg: background,
            style: CellStyle::default(),
        };
        let mut cells = vec![blank; columns * lines];
        let offset = content.display_offset as i32;
        let selection = content.selection;

        for indexed in content.display_iter {
            let cell = indexed.cell;
            let line = indexed.point.line.0 + offset;
            if line < 0 || line as usize >= lines {
                continue;
            }
            let flags = cell.flags;
            let mut fg = resolve_fg(cell.fg, flags, palette, overrides);
            let mut bg = palette.resolve(cell.bg, overrides);
            if flags.contains(Flags::INVERSE) {
                std::mem::swap(&mut fg, &mut bg);
            }
            if selection.is_some_and(|s| s.contains(indexed.point)) {
                fg = palette.selection_foreground;
                bg = palette.selection_background;
            }
            let hidden = flags.contains(Flags::HIDDEN)
                || flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER);

            cells[line as usize * columns + indexed.point.column.0] = SnapshotCell {
                c: if hidden { ' ' } else { cell.c },
                zerowidth: if hidden {
                    None
                } else {
                    cell.zerowidth().map(Box::from)
                },
                fg,
                bg,
                style: CellStyle {
                    bold: flags.contains(Flags::BOLD),
                    italic: flags.contains(Flags::ITALIC),
                    underline: flags.intersects(Flags::ALL_UNDERLINES),
                    strikeout: flags.contains(Flags::STRIKEOUT),
                    wide: flags.contains(Flags::WIDE_CHAR),
                },
            };
        }

        let cursor_point = content.cursor.point;
        let cursor_line = cursor_point.line.0 + offset;
        let style = match content.cursor.shape {
            CursorShape::Block => Some(CursorStyle::Block),
            CursorShape::HollowBlock => Some(CursorStyle::HollowBlock),
            CursorShape::Underline => Some(CursorStyle::Underline),
            CursorShape::Beam => Some(CursorStyle::Beam),
            CursorShape::Hidden => None,
        };
        let cursor = style
            .filter(|_| content.mode.contains(TermMode::SHOW_CURSOR))
            .filter(|_| cursor_line >= 0 && (cursor_line as usize) < lines)
            .map(|style| {
                let (column, line) = (cursor_point.column.0, cursor_line as usize);
                SnapshotCursor {
                    column,
                    line,
                    style,
                    color: palette.resolve(Color::Named(NamedColor::Cursor), overrides),
                    wide: cells[line * columns + column].style.wide,
                    blinking: term.cursor_style().blinking,
                }
            });

        Self {
            columns,
            lines,
            cells,
            cursor,
            background,
            foreground,
        }
    }
}

fn resolve_fg(color: Color, flags: Flags, palette: &Palette, overrides: &Colors) -> Rgb {
    let rgb = palette.resolve(color, overrides);
    if flags.contains(Flags::DIM) {
        dim(rgb)
    } else {
        rgb
    }
}
