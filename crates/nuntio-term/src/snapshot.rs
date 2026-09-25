use alacritty_terminal::event::EventListener;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::color::Colors;
use alacritty_terminal::term::search::Match;
use alacritty_terminal::term::{Term, TermMode};
use alacritty_terminal::vte::ansi::{Color, CursorShape, NamedColor, Rgb};

use crate::palette::{Palette, dim};

const BLACK: Rgb = Rgb { r: 0, g: 0, b: 0 };

/// Style bits the renderer cares about.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct CellStyle {
    pub bold: bool,
    pub italic: bool,
    pub underline: Option<UnderlineStyle>,
    pub strikeout: bool,
    /// The glyph spans two columns.
    pub wide: bool,
}

/// The underline variants of SGR 4 (`4:2` double, `4:3` curly, …).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UnderlineStyle {
    Single,
    Double,
    Curly,
    Dotted,
    Dashed,
}

impl UnderlineStyle {
    fn from_flags(flags: Flags) -> Option<Self> {
        let style = if flags.contains(Flags::DOUBLE_UNDERLINE) {
            Self::Double
        } else if flags.contains(Flags::UNDERCURL) {
            Self::Curly
        } else if flags.contains(Flags::DOTTED_UNDERLINE) {
            Self::Dotted
        } else if flags.contains(Flags::DASHED_UNDERLINE) {
            Self::Dashed
        } else if flags.contains(Flags::UNDERLINE) {
            Self::Single
        } else {
            return None;
        };
        Some(style)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SnapshotCell {
    pub c: char,
    /// Combining characters following `c`, if any.
    pub zerowidth: Option<Box<[char]>>,
    pub fg: Rgb,
    pub bg: Rgb,
    pub style: CellStyle,
    /// Color of the underline (SGR 58), if it differs from `fg`.
    pub underline_color: Option<Rgb>,
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
    /// The cell at a position. Panics outside the grid; see [`Self::get`].
    pub fn cell(&self, column: usize, line: usize) -> &SnapshotCell {
        &self.cells[line * self.columns + column]
    }

    /// The cell at a position, if it is inside the grid.
    pub fn get(&self, column: usize, line: usize) -> Option<&SnapshotCell> {
        if column >= self.columns || line >= self.lines {
            return None;
        }
        self.cells.get(line * self.columns + column)
    }

    /// Copy the visible screen. `matches` are highlighted as search
    /// results, `current` as the selected one.
    pub(crate) fn capture<T: EventListener>(
        term: &Term<T>,
        palette: &Palette,
        matches: &[Match],
        current: Option<&Match>,
    ) -> Self {
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
            underline_color: None,
        };
        let mut cells = vec![blank; columns * lines];
        let offset = content.display_offset as i32;
        let selection = content.selection;
        // Matches are in grid order and don't overlap, like the cells below,
        // so one index walks along with the cells.
        let mut next_match = 0;

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
            if current.is_some_and(|m| m.contains(&indexed.point)) {
                (fg, bg) = (BLACK, palette.search_current);
            } else {
                while matches
                    .get(next_match)
                    .is_some_and(|m| *m.end() < indexed.point)
                {
                    next_match += 1;
                }
                if matches
                    .get(next_match)
                    .is_some_and(|m| m.contains(&indexed.point))
                {
                    (fg, bg) = (BLACK, palette.search_match);
                }
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
                    underline: UnderlineStyle::from_flags(flags),
                    strikeout: flags.contains(Flags::STRIKEOUT),
                    wide: flags.contains(Flags::WIDE_CHAR),
                },
                underline_color: cell
                    .underline_color()
                    .map(|color| palette.resolve(color, overrides)),
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

#[cfg(test)]
mod tests {
    use alacritty_terminal::event::VoidListener;
    use alacritty_terminal::term::Config;
    use alacritty_terminal::vte::ansi::Processor;

    use super::*;

    fn capture(input: &str) -> Snapshot {
        let size = crate::TermSize {
            columns: 10,
            lines: 2,
            cell_width: 1,
            cell_height: 1,
        };
        let mut term = Term::new(Config::default(), &size, VoidListener);
        let mut parser: Processor = Processor::new();
        parser.advance(&mut term, input.as_bytes());
        Snapshot::capture(&term, &Palette::default(), &[], None)
    }

    #[test]
    fn underline_styles_and_colors() {
        let s = capture("\x1b[4ma\x1b[4:2mb\x1b[4:3mc\x1b[4:4md\x1b[4:5me\x1b[0mf");
        let styles: Vec<_> = (0..6).map(|c| s.cell(c, 0).style.underline).collect();
        assert_eq!(
            styles,
            [
                Some(UnderlineStyle::Single),
                Some(UnderlineStyle::Double),
                Some(UnderlineStyle::Curly),
                Some(UnderlineStyle::Dotted),
                Some(UnderlineStyle::Dashed),
                None,
            ]
        );

        let s = capture("\x1b[4:3;58:2::255:0:0mx\x1b[59my");
        let red = Rgb { r: 255, g: 0, b: 0 };
        assert_eq!(s.cell(0, 0).underline_color, Some(red));
        assert_eq!(s.cell(1, 0).underline_color, None);
    }
}
