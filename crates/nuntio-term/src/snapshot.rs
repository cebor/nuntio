use std::sync::Arc;

use alacritty_terminal::event::EventListener;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::color::Colors;
use alacritty_terminal::term::search::Match;
use alacritty_terminal::term::{Term, TermMode};
use alacritty_terminal::vte::ansi::{Color, CursorShape, NamedColor, Rgb};

use crate::image::{ImagePiece, ImageStore, image_cell_ref};
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
    /// Image runs on the visible screen, row by row.
    pub images: Vec<ImagePiece>,
}

impl Snapshot {
    /// The cell at a position. Panics outside the grid.
    pub fn cell(&self, column: usize, line: usize) -> &SnapshotCell {
        &self.cells[line * self.columns + column]
    }

    /// Overwrite `self` with the visible screen, reusing the cell buffer.
    /// `matches` are highlighted as search results, `current` as the
    /// selected one.
    pub(crate) fn refresh<T: EventListener>(
        &mut self,
        term: &Term<T>,
        palette: &Palette,
        images: &ImageStore,
        matches: &[Match],
        current: Option<&Match>,
    ) {
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
        self.cells.clear();
        self.images.clear();
        self.cells.resize(columns * lines, blank);
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
            let column = indexed.point.column.0;
            let image_ref = image_cell_ref(cell);
            if let Some((id, image_line, image_column)) = image_ref {
                self.push_image(images, id, line as usize, column, image_line, image_column);
            }
            let hidden = image_ref.is_some()
                || flags.contains(Flags::HIDDEN)
                || flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER);
            let spacer = flags.contains(Flags::WIDE_CHAR_SPACER);

            self.cells[line as usize * columns + column] = SnapshotCell {
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
                    underline: if spacer {
                        None
                    } else {
                        UnderlineStyle::from_flags(flags)
                    },
                    strikeout: !spacer && flags.contains(Flags::STRIKEOUT),
                    wide: flags.contains(Flags::WIDE_CHAR),
                },
                underline_color: if spacer {
                    None
                } else {
                    cell.underline_color()
                        .map(|color| palette.resolve(color, overrides))
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
                    wide: self.cells[line * columns + column].style.wide,
                    blinking: term.cursor_style().blinking,
                }
            });

        self.columns = columns;
        self.lines = lines;
        self.cursor = cursor;
        self.background = background;
        self.foreground = foreground;
    }

    /// Add an image cell: extends the previous run if it continues it.
    fn push_image(
        &mut self,
        images: &ImageStore,
        id: u32,
        line: usize,
        column: usize,
        image_line: usize,
        image_column: usize,
    ) {
        let Some(image) = images.get(id) else {
            return;
        };
        if let Some(last) = self.images.last_mut()
            && last.line == line
            && last.column + last.columns == column
            && Arc::ptr_eq(&last.image, image)
            && last.image_line == image_line
            && last.image_column + last.columns == image_column
        {
            last.columns += 1;
            return;
        }
        self.images.push(ImagePiece {
            line,
            column,
            columns: 1,
            image: image.clone(),
            image_column,
            image_line,
        });
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
    use alacritty_terminal::event::{VoidListener, WindowSize};
    use alacritty_terminal::term::Config;
    use alacritty_terminal::vte::ansi::Processor;

    use crate::image::{CellGeometry, ImageKey, TermImage, place};
    use crate::image_scan::ImageRequest;

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
        let mut snapshot = Snapshot::default();
        snapshot.refresh(
            &term,
            &Palette::default(),
            &ImageStore::default(),
            &[],
            None,
        );
        snapshot
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

    #[test]
    fn refresh_reuses_the_buffer() {
        let size = |columns, lines| crate::TermSize {
            columns,
            lines,
            cell_width: 1,
            cell_height: 1,
        };
        let big = size(10, 2);
        let mut term = Term::new(Config::default(), &big, VoidListener);
        let mut parser: Processor = Processor::new();
        parser.advance(&mut term, b"hello");
        let mut s = Snapshot::default();
        s.refresh(
            &term,
            &Palette::default(),
            &ImageStore::default(),
            &[],
            None,
        );
        let (ptr, capacity) = (s.cells.as_ptr(), s.cells.capacity());
        assert_eq!(s.cell(0, 0).c, 'h');

        parser.advance(&mut term, b"\r\nworld");
        s.refresh(
            &term,
            &Palette::default(),
            &ImageStore::default(),
            &[],
            None,
        );
        assert_eq!((s.cells.as_ptr(), s.cells.capacity()), (ptr, capacity));
        assert_eq!(s.cell(0, 1).c, 'w');

        let small = size(5, 1);
        let term = Term::new(Config::default(), &small, VoidListener);
        s.refresh(
            &term,
            &Palette::default(),
            &ImageStore::default(),
            &[],
            None,
        );
        assert_eq!(s.columns * s.lines, s.cells.len());
        assert_eq!(s.cells.len(), 5);
    }

    /// A 10×3 terminal with "ab" and a 3×2 image after it, stored if
    /// `stored`.
    fn with_image(stored: bool) -> (Term<VoidListener>, Processor, ImageStore) {
        let size = crate::TermSize {
            columns: 10,
            lines: 3,
            cell_width: 1,
            cell_height: 1,
        };
        let mut term = Term::new(Config::default(), &size, VoidListener);
        let mut parser: Processor = Processor::new();
        parser.advance(&mut term, b"ab");
        let image = || TermImage {
            uid: 0,
            columns: 3,
            lines: 2,
            width: 3,
            height: 2,
            rgba: vec![0; 24].into_boxed_slice(),
        };
        let mut store = ImageStore::default();
        let id = if stored {
            let key = ImageKey::new(
                &ImageRequest {
                    args: Vec::new(),
                    payload: Vec::new(),
                },
                CellGeometry::from(WindowSize {
                    num_lines: 3,
                    num_cols: 10,
                    cell_width: 1,
                    cell_height: 1,
                }),
            );
            store.insert(key, image())
        } else {
            5
        };
        place(&mut term, id, &image());
        (term, parser, store)
    }

    /// `(line, column, columns, image_line, image_column)` of each run.
    fn runs(s: &Snapshot) -> Vec<(usize, usize, usize, usize, usize)> {
        s.images
            .iter()
            .map(|p| (p.line, p.column, p.columns, p.image_line, p.image_column))
            .collect()
    }

    #[test]
    fn image_runs() {
        let (mut term, mut parser, store) = with_image(true);
        let mut s = Snapshot::default();
        s.refresh(&term, &Palette::default(), &store, &[], None);
        assert_eq!(runs(&s), [(0, 2, 3, 0, 0), (1, 2, 3, 1, 0)]);
        assert!((2..5).all(|c| s.cell(c, 0).is_blank() && s.cell(c, 1).is_blank()));
        assert_eq!(s.cell(1, 0).c, 'b');

        parser.advance(&mut term, b"\x1b[1;4HX");
        s.refresh(&term, &Palette::default(), &store, &[], None);
        assert_eq!(
            runs(&s),
            [(0, 2, 1, 0, 0), (0, 4, 1, 0, 2), (1, 2, 3, 1, 0)]
        );
        assert_eq!(s.cell(3, 0).c, 'X');
    }

    #[test]
    fn unknown_image_has_no_runs() {
        let (term, _, store) = with_image(false);
        let mut s = Snapshot::default();
        s.refresh(&term, &Palette::default(), &store, &[], None);
        assert!(s.images.is_empty());
        assert!(s.cell(2, 0).is_blank());
    }
}
