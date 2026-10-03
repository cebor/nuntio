//! Inline images (iTerm2's OSC 1337 `File=`): decoding, the per-pane store
//! and placement in the grid.
//!
//! Images live in alacritty's grid as ordinary cells, so scrolling, the
//! scrollback, clearing and overwriting with text treat them like text.
//! Every image cell is a [`PLACEHOLDER`] with the image id as its 24-bit
//! foreground and exactly two combining marks, `MARK_BASE + row` and
//! `MARK_BASE + column`. Programs can't produce such cells: these marks
//! have width 1, so alacritty never attaches them to a cell.

use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;
use std::hash::{Hash, Hasher};
use std::io::Cursor;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use alacritty_terminal::event::{EventListener, WindowSize};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::{Cell, Flags};
use alacritty_terminal::term::{Term, TermMode};
use alacritty_terminal::vte::ansi::{Color, Handler, Rgb};
use base64::Engine;
use base64::alphabet;
use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};
use image::imageops::{self, FilterType};
use image::{ImageReader, Limits, RgbaImage};
use thiserror::Error;

use crate::image_scan::ImageRequest;

/// The character of every image cell.
pub(crate) const PLACEHOLDER: char = '\u{10EEEE}';
/// First of the marks encoding an image cell's row and column.
const MARK_BASE: u32 = 0xF0000;
/// Number of marks: rows and columns up to this are encodable.
const MARK_COUNT: u32 = 0xFFFE;
/// Image memory per pane before images no longer on screen are dropped.
const IMAGE_MEMORY_BUDGET: usize = 256 << 20;
/// Longest side of an image bitmap; larger ones are scaled down.
const MAX_BITMAP_SIDE: u64 = 4096;
/// Most rows an image may cover.
const MAX_LINES: u64 = 10_000;
/// Most columns an image may cover; more than any terminal is wide.
const MAX_COLUMNS: u64 = 10_000;
/// Largest image id: ids fill the 24 bits of an RGB color.
const MAX_ID: u32 = 0xFF_FFFF;

static NEXT_UID: AtomicU64 = AtomicU64::new(1);

/// A decoded image, scaled to the cells it covers.
pub struct TermImage {
    /// Unique across panes, for the renderer's cache.
    pub uid: u64,
    /// Cells the image covers.
    pub columns: usize,
    pub lines: usize,
    /// Bitmap size in pixels: the cells at the cell size when the image
    /// arrived, unless scaled down.
    pub width: u32,
    pub height: u32,
    /// Straight (not premultiplied) RGBA, row by row.
    pub rgba: Box<[u8]>,
}

impl fmt::Debug for TermImage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TermImage")
            .field("uid", &self.uid)
            .field("columns", &self.columns)
            .field("lines", &self.lines)
            .field("width", &self.width)
            .field("height", &self.height)
            .finish_non_exhaustive()
    }
}

/// A run of image cells on one row of the visible screen.
#[derive(Debug, Clone)]
pub struct ImagePiece {
    /// Screen position of the first cell.
    pub line: usize,
    pub column: usize,
    /// Cells in the run.
    pub columns: usize,
    pub image: Arc<TermImage>,
    /// Cell of the image the run starts with.
    pub image_column: usize,
    pub image_line: usize,
}

fn mark(index: usize) -> char {
    char::from_u32(MARK_BASE + index.min(MARK_COUNT as usize - 1) as u32)
        .expect("marks are valid characters")
}

fn image_cell(id: u32, line: usize, column: usize, bg: Color) -> Cell {
    let mut cell = Cell {
        c: PLACEHOLDER,
        fg: Color::Spec(Rgb {
            r: (id >> 16) as u8,
            g: (id >> 8) as u8,
            b: id as u8,
        }),
        bg,
        ..Cell::default()
    };
    cell.push_zerowidth(mark(line));
    cell.push_zerowidth(mark(column));
    cell
}

/// The image id, row and column of an image cell.
pub(crate) fn image_cell_ref(cell: &Cell) -> Option<(u32, usize, usize)> {
    if cell.c != PLACEHOLDER {
        return None;
    }
    let Color::Spec(rgb) = cell.fg else {
        return None;
    };
    let [line, column] = cell.zerowidth()? else {
        return None;
    };
    let index = |c: char| {
        let n = (c as u32).checked_sub(MARK_BASE)?;
        (n < MARK_COUNT).then_some(n as usize)
    };
    let id = ((rgb.r as u32) << 16) | ((rgb.g as u32) << 8) | rgb.b as u32;
    Some((id, index(*line)?, index(*column)?))
}

fn is_mark(c: char) -> bool {
    (MARK_BASE..MARK_BASE + MARK_COUNT).contains(&(c as u32))
}

/// `text` without image cells, for copying a selection.
pub(crate) fn strip_image_text(text: String) -> String {
    if !text.contains(PLACEHOLDER) {
        return text;
    }
    let mut out = String::with_capacity(text.len());
    let mut in_image = false;
    for c in text.chars() {
        if c == PLACEHOLDER {
            in_image = true;
        } else if !(in_image && is_mark(c)) {
            in_image = false;
            out.push(c);
        }
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Dimension {
    Auto,
    Cells(u32),
    Pixels(u32),
    Percent(u32),
}

impl Dimension {
    fn parse(value: &str) -> Self {
        let (number, make): (_, fn(u32) -> Self) = if let Some(n) = value.strip_suffix("px") {
            (n, Self::Pixels)
        } else if let Some(n) = value.strip_suffix('%') {
            (n, Self::Percent)
        } else {
            (value, Self::Cells)
        };
        number.parse().map_or(Self::Auto, make)
    }

    /// Size in pixels, given the cell size and the terminal size along the
    /// same axis.
    fn pixels(self, cell: u64, terminal: u64) -> Option<u64> {
        match self {
            Self::Auto => None,
            Self::Cells(n) => Some(n as u64 * cell),
            Self::Pixels(n) => Some(n as u64),
            Self::Percent(p) => Some(p as u64 * terminal / 100),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct ImageArgs {
    pub(crate) inline: bool,
    width: Dimension,
    height: Dimension,
    preserve_aspect: bool,
}

/// The `key=value;…` arguments of `File=`. Unknown keys (`name`, `size`)
/// are ignored.
pub(crate) fn parse_args(args: &[u8]) -> ImageArgs {
    let mut parsed = ImageArgs {
        inline: false,
        width: Dimension::Auto,
        height: Dimension::Auto,
        preserve_aspect: true,
    };
    for pair in args.split(|&b| b == b';') {
        let Ok(pair) = std::str::from_utf8(pair) else {
            continue;
        };
        let Some((key, value)) = pair.split_once('=') else {
            continue;
        };
        match key {
            "inline" => parsed.inline = value == "1",
            "width" => parsed.width = Dimension::parse(value),
            "height" => parsed.height = Dimension::parse(value),
            "preserveAspectRatio" => parsed.preserve_aspect = value != "0",
            _ => {}
        }
    }
    parsed
}

/// The terminal's size in cells and its cell size in pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct CellGeometry {
    columns: usize,
    lines: usize,
    cell_width: u32,
    cell_height: u32,
}

impl From<WindowSize> for CellGeometry {
    fn from(size: WindowSize) -> Self {
        Self {
            columns: size.num_cols as usize,
            lines: size.num_lines as usize,
            cell_width: size.cell_width as u32,
            cell_height: size.cell_height as u32,
        }
    }
}

#[derive(Debug, Error)]
pub(crate) enum ImageError {
    #[error("the cell size is unknown")]
    NoCellSize,
    #[error("invalid base64: {0}")]
    Base64(#[from] base64::DecodeError),
    #[error(transparent)]
    Image(#[from] image::ImageError),
}

/// `(w, h)` scaled to `width`, keeping the aspect ratio.
fn scale_to_width((w, h): (u64, u64), width: u64) -> (u64, u64) {
    (width, h * width / w.max(1))
}

/// `(w, h)` scaled to `height`, keeping the aspect ratio.
fn scale_to_height((w, h): (u64, u64), height: u64) -> (u64, u64) {
    (w * height / h.max(1), height)
}

/// Decode an image and scale it to the cells it will cover.
pub(crate) fn decode(
    request: &ImageRequest,
    args: &ImageArgs,
    geometry: CellGeometry,
) -> Result<TermImage, ImageError> {
    let (cw, ch) = (geometry.cell_width as u64, geometry.cell_height as u64);
    if cw == 0 || ch == 0 {
        return Err(ImageError::NoCellSize);
    }
    let engine = GeneralPurpose::new(
        &alphabet::STANDARD,
        GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent),
    );
    let bytes = engine.decode(&request.payload)?;
    let mut reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(image::ImageError::IoError)?;
    let mut limits = Limits::default();
    limits.max_image_width = Some(16384);
    limits.max_image_height = Some(16384);
    limits.max_alloc = Some(256 << 20);
    reader.limits(limits);
    // The first frame of animations.
    let decoded = reader.decode()?.to_rgba8();

    let inherent = (decoded.width() as u64, decoded.height() as u64);
    let terminal = (geometry.columns as u64 * cw, geometry.lines as u64 * ch);
    let width = args.width.pixels(cw, terminal.0).map(|w| w.max(1));
    let height = args.height.pixels(ch, terminal.1).map(|h| h.max(1));
    // `drawn` is the image's size on screen, `cells` the area it covers.
    let (mut drawn, mut cells) = match (width, height) {
        (None, None) => (inherent, inherent),
        (Some(w), None) => {
            let size = scale_to_width(inherent, w);
            (size, size)
        }
        (None, Some(h)) => {
            let size = scale_to_height(inherent, h);
            (size, size)
        }
        (Some(w), Some(h)) => {
            let drawn = if !args.preserve_aspect {
                (w, h)
            } else if inherent.0 * h <= inherent.1 * w {
                scale_to_height(inherent, h)
            } else {
                scale_to_width(inherent, w)
            };
            (drawn, (w, h))
        }
    };
    if width.is_none() && drawn.0 > terminal.0 {
        drawn = scale_to_width(drawn, terminal.0);
        cells = drawn;
    }
    drawn = (drawn.0.max(1), drawn.1.max(1));

    let columns = cells.0.div_ceil(cw).clamp(1, MAX_COLUMNS);
    let lines = cells.1.div_ceil(ch).clamp(1, MAX_LINES);
    let mut bitmap = (columns * cw, lines * ch);
    let longest = bitmap.0.max(bitmap.1);
    if longest > MAX_BITMAP_SIDE {
        let scale = |n: u64| (n * MAX_BITMAP_SIDE / longest).max(1);
        bitmap = (scale(bitmap.0), scale(bitmap.1));
        drawn = (scale(drawn.0), scale(drawn.1));
    }
    // Only past the row and column limits is the image larger than its cells.
    drawn = (drawn.0.min(bitmap.0), drawn.1.min(bitmap.1));

    let mut canvas = RgbaImage::new(bitmap.0 as u32, bitmap.1 as u32);
    let resized = imageops::resize(
        &decoded,
        drawn.0 as u32,
        drawn.1 as u32,
        FilterType::Triangle,
    );
    imageops::overlay(&mut canvas, &resized, 0, 0);

    Ok(TermImage {
        uid: NEXT_UID.fetch_add(1, Ordering::Relaxed),
        columns: columns as usize,
        lines: lines as usize,
        width: canvas.width(),
        height: canvas.height(),
        rgba: canvas.into_raw().into_boxed_slice(),
    })
}

/// Identifies a request at a geometry, so an image sent again (omp redraws
/// its output) is decoded only once.
#[derive(Debug, Hash, PartialEq, Eq, Clone, Copy)]
pub(crate) struct ImageKey {
    hash: u64,
    geometry: CellGeometry,
}

impl ImageKey {
    pub(crate) fn new(request: &ImageRequest, geometry: CellGeometry) -> Self {
        let mut hasher = DefaultHasher::new();
        request.args.hash(&mut hasher);
        request.payload.hash(&mut hasher);
        Self {
            hash: hasher.finish(),
            geometry,
        }
    }
}

/// The images of one pane, by the id their cells carry.
pub(crate) struct ImageStore {
    next_id: u32,
    images: HashMap<u32, Arc<TermImage>>,
    /// Ids, oldest first.
    order: VecDeque<u32>,
    recent: HashMap<ImageKey, u32>,
    bytes: usize,
    budget: usize,
}

impl Default for ImageStore {
    fn default() -> Self {
        Self::with_budget(IMAGE_MEMORY_BUDGET)
    }
}

impl ImageStore {
    pub(crate) fn with_budget(budget: usize) -> Self {
        Self {
            next_id: 1,
            images: HashMap::new(),
            order: VecDeque::new(),
            recent: HashMap::new(),
            bytes: 0,
            budget,
        }
    }

    pub(crate) fn get(&self, id: u32) -> Option<&Arc<TermImage>> {
        self.images.get(&id)
    }

    /// The id of an image decoded from the same request at the same
    /// geometry, if it is still stored.
    pub(crate) fn cached(&self, key: ImageKey) -> Option<u32> {
        self.recent
            .get(&key)
            .copied()
            .filter(|id| self.images.contains_key(id))
    }

    pub(crate) fn insert(&mut self, key: ImageKey, image: TermImage) -> u32 {
        let mut id = self.next_id;
        while self.images.contains_key(&id) {
            id = if id >= MAX_ID { 1 } else { id + 1 };
        }
        self.next_id = if id >= MAX_ID { 1 } else { id + 1 };
        self.bytes += image.rgba.len();
        self.images.insert(id, Arc::new(image));
        self.order.push_back(id);
        self.recent.insert(key, id);
        id
    }

    /// Over budget, drop the oldest images no cell refers to anymore.
    pub(crate) fn evict<T: EventListener>(&mut self, term: &Term<T>) {
        if self.bytes <= self.budget {
            return;
        }
        // Only the active screen is reachable through alacritty's API, so
        // the images of the inactive one may go as well.
        let grid = term.grid();
        let mut referenced = HashSet::new();
        for line in grid.topmost_line().0..=grid.bottommost_line().0 {
            let row = &grid[Line(line)];
            for column in 0..grid.columns() {
                if let Some((id, ..)) = image_cell_ref(&row[Column(column)]) {
                    referenced.insert(id);
                }
            }
        }
        let mut kept = VecDeque::with_capacity(self.order.len());
        while let Some(id) = self.order.pop_front() {
            if self.bytes > self.budget && !referenced.contains(&id) {
                if let Some(image) = self.images.remove(&id) {
                    self.bytes -= image.rgba.len();
                }
            } else {
                kept.push_back(id);
            }
        }
        self.order = kept;
        let images = &self.images;
        self.recent.retain(|_, id| images.contains_key(id));
    }
}

/// Write the cells of image `id` at the cursor, scrolling as needed, and
/// leave the cursor after its bottom-right corner, as iTerm2 does.
pub(crate) fn place<T: EventListener>(term: &mut Term<T>, id: u32, image: &TermImage) {
    if term.grid().cursor.input_needs_wrap && term.mode().contains(TermMode::LINE_WRAP) {
        term.carriage_return();
        term.linefeed();
    }
    let columns = term.columns();
    let x = term.grid().cursor.point.column.0;
    let visible = image.columns.min(columns - x);
    let bg = term.grid().cursor.template.bg;
    let blank = Cell {
        bg,
        ..Cell::default()
    };
    for row in 0..image.lines {
        if row > 0 {
            term.linefeed();
        }
        let line = term.grid().cursor.point.line;
        let cells = &mut term.grid_mut()[line];
        // Don't leave half of a wide character behind.
        if x > 0 && cells[Column(x)].flags.contains(Flags::WIDE_CHAR_SPACER) {
            cells[Column(x - 1)] = blank.clone();
        }
        if x + visible < columns
            && cells[Column(x + visible - 1)]
                .flags
                .contains(Flags::WIDE_CHAR)
        {
            cells[Column(x + visible)] = blank.clone();
        }
        for col in 0..visible {
            cells[Column(x + col)] = image_cell(id, row, col, bg);
        }
    }
    let end = x + image.columns;
    let cursor = &mut term.grid_mut().cursor;
    if end < columns {
        cursor.point.column = Column(end);
        cursor.input_needs_wrap = false;
    } else {
        cursor.point.column = Column(columns - 1);
        cursor.input_needs_wrap = true;
    }
}

#[cfg(test)]
mod tests {
    use alacritty_terminal::event::VoidListener;
    use alacritty_terminal::term::Config;
    use alacritty_terminal::vte::ansi::Processor;
    use image::ImageFormat;

    use super::*;

    const GEOMETRY: CellGeometry = CellGeometry {
        columns: 80,
        lines: 24,
        cell_width: 10,
        cell_height: 20,
    };

    fn png(width: u32, height: u32) -> Vec<u8> {
        let image = RgbaImage::from_pixel(width, height, image::Rgba([255, 0, 0, 255]));
        let mut bytes = Cursor::new(Vec::new());
        image.write_to(&mut bytes, ImageFormat::Png).unwrap();
        base64::engine::general_purpose::STANDARD
            .encode(bytes.into_inner())
            .into_bytes()
    }

    fn request(args: &str, payload: Vec<u8>) -> ImageRequest {
        ImageRequest {
            args: args.as_bytes().to_vec(),
            payload,
        }
    }

    fn decode_with(args: &str, payload: Vec<u8>, geometry: CellGeometry) -> TermImage {
        let request = request(args, payload);
        decode(&request, &parse_args(&request.args), geometry).unwrap()
    }

    fn alpha(image: &TermImage, x: u32, y: u32) -> u8 {
        image.rgba[((y * image.width + x) * 4 + 3) as usize]
    }

    fn term(columns: u16, lines: u16) -> Term<VoidListener> {
        let size = crate::TermSize {
            columns,
            lines,
            cell_width: 1,
            cell_height: 1,
        };
        Term::new(Config::default(), &size, VoidListener)
    }

    fn test_image(columns: usize, lines: usize) -> TermImage {
        TermImage {
            uid: 0,
            columns,
            lines,
            width: columns as u32,
            height: lines as u32,
            rgba: vec![0; columns * lines * 4].into_boxed_slice(),
        }
    }

    fn cell_at<T: EventListener>(term: &Term<T>, line: i32, column: usize) -> &Cell {
        &term.grid()[Line(line)][Column(column)]
    }

    #[test]
    fn args() {
        let a = parse_args(b"name=eA==;inline=1;width=10;height=20px;preserveAspectRatio=0");
        assert!(a.inline);
        assert_eq!(a.width, Dimension::Cells(10));
        assert_eq!(a.height, Dimension::Pixels(20));
        assert!(!a.preserve_aspect);

        let a = parse_args(b"inline=0;width=50%;height=x");
        assert!(!a.inline);
        assert_eq!(a.width, Dimension::Percent(50));
        assert_eq!(a.height, Dimension::Auto);
        assert!(a.preserve_aspect);

        let a = parse_args(b"");
        assert!(!a.inline);
        assert_eq!((a.width, a.height), (Dimension::Auto, Dimension::Auto));
    }

    #[test]
    fn width_in_cells_keeps_the_aspect_ratio() {
        // 40×40 px at 2 columns: 20×20 px, one 20 px line.
        let image = decode_with("inline=1;width=2", png(40, 40), GEOMETRY);
        assert_eq!((image.columns, image.lines), (2, 1));
        assert_eq!((image.width, image.height), (20, 20));
        assert_eq!(image.rgba.len(), 20 * 20 * 4);
    }

    #[test]
    fn box_with_preserved_aspect_ratio_is_fitted() {
        // A square in a 4×2 cell box (40×40 px): fits exactly.
        let image = decode_with("inline=1;width=6;height=2", png(10, 10), GEOMETRY);
        assert_eq!((image.columns, image.lines), (6, 2));
        assert_eq!((image.width, image.height), (60, 40));
        assert_eq!(alpha(&image, 39, 39), 255);
        assert_eq!(alpha(&image, 40, 0), 0, "right of the fitted image");
    }

    #[test]
    fn box_without_preserved_aspect_ratio_is_stretched() {
        let image = decode_with(
            "inline=1;width=6;height=2;preserveAspectRatio=0",
            png(10, 10),
            GEOMETRY,
        );
        assert_eq!((image.width, image.height), (60, 40));
        assert_eq!(alpha(&image, 59, 39), 255);
    }

    #[test]
    fn auto_width_is_limited_to_the_terminal() {
        let image = decode_with("inline=1", png(2000, 100), GEOMETRY);
        assert_eq!(image.columns, GEOMETRY.columns);
        // 800×40 px after scaling down.
        assert_eq!((image.width, image.lines), (800, 2));
    }

    #[test]
    fn unknown_cell_size() {
        let request = request("inline=1", png(1, 1));
        let geometry = CellGeometry {
            cell_width: 0,
            ..GEOMETRY
        };
        assert!(matches!(
            decode(&request, &parse_args(&request.args), geometry),
            Err(ImageError::NoCellSize)
        ));
    }

    #[test]
    fn place_writes_cells_and_moves_the_cursor() {
        let mut term = term(10, 5);
        let mut parser: Processor = Processor::new();
        parser.advance(&mut term, b"ab");
        place(&mut term, 7, &test_image(3, 2));
        for (row, col) in [(0, 0), (0, 2), (1, 1)] {
            assert_eq!(
                image_cell_ref(cell_at(&term, row as i32, 2 + col)),
                Some((7, row, col))
            );
        }
        assert_eq!(cell_at(&term, 0, 1).c, 'b');
        assert_eq!(image_cell_ref(cell_at(&term, 0, 5)), None);
        let cursor = term.grid().cursor.point;
        assert_eq!((cursor.line.0, cursor.column.0), (1, 5));
    }

    #[test]
    fn place_at_the_bottom_scrolls() {
        let mut term = term(10, 3);
        let mut parser: Processor = Processor::new();
        parser.advance(&mut term, b"\r\n\r\n");
        place(&mut term, 1, &test_image(2, 2));
        let cursor = term.grid().cursor.point;
        assert_eq!(cursor.line.0, 2);
        assert_eq!(image_cell_ref(cell_at(&term, 1, 0)), Some((1, 0, 0)));
        assert_eq!(image_cell_ref(cell_at(&term, 2, 0)), Some((1, 1, 0)));
        assert_eq!(term.grid().history_size(), 1);
    }

    #[test]
    fn place_crops_at_the_right_edge() {
        let mut term = term(5, 3);
        let mut parser: Processor = Processor::new();
        parser.advance(&mut term, b"abc");
        place(&mut term, 1, &test_image(4, 1));
        assert_eq!(image_cell_ref(cell_at(&term, 0, 4)), Some((1, 0, 1)));
        assert!(term.grid().cursor.input_needs_wrap);
        assert_eq!(term.grid().cursor.point.column.0, 4);
        parser.advance(&mut term, b"x");
        assert_eq!(cell_at(&term, 1, 0).c, 'x');
    }

    #[test]
    fn place_clears_split_wide_characters() {
        let mut term = term(6, 2);
        let mut parser: Processor = Processor::new();
        parser.advance(&mut term, "a界b界".as_bytes());
        // Over the spacer of the first 界 and the first half of the second.
        parser.advance(&mut term, b"\x1b[1;3H");
        place(&mut term, 1, &test_image(3, 1));
        assert_eq!(cell_at(&term, 0, 1).c, ' ');
        assert!(!cell_at(&term, 0, 1).flags.contains(Flags::WIDE_CHAR));
        assert_eq!(cell_at(&term, 0, 5).c, ' ');
        assert!(!cell_at(&term, 0, 5).flags.contains(Flags::WIDE_CHAR_SPACER));
    }

    #[test]
    fn pending_wrap_starts_on_the_next_line() {
        let mut term = term(3, 3);
        let mut parser: Processor = Processor::new();
        parser.advance(&mut term, b"abc");
        assert!(term.grid().cursor.input_needs_wrap);
        place(&mut term, 1, &test_image(1, 1));
        assert_eq!(cell_at(&term, 0, 2).c, 'c');
        assert_eq!(image_cell_ref(cell_at(&term, 1, 0)), Some((1, 0, 0)));
    }

    #[test]
    fn eviction_keeps_images_on_screen() {
        let mut term = term(4, 2);
        let mut parser: Processor = Processor::new();
        let key = |n| ImageKey {
            hash: n,
            geometry: GEOMETRY,
        };
        // Each test image is 4 cells × 4 bytes.
        let mut store = ImageStore::with_budget(20);
        let old = store.insert(key(1), test_image(2, 2));
        place(&mut term, old, &test_image(2, 2));
        parser.advance(&mut term, b"\x1b[Hxx\r\nxx");
        let kept = store.insert(key(2), test_image(2, 2));
        place(&mut term, kept, &test_image(2, 2));
        store.evict(&term);
        assert!(store.get(old).is_none());
        assert_eq!(store.cached(key(1)), None);
        assert!(store.get(kept).is_some());
        assert_eq!(store.cached(key(2)), Some(kept));
    }

    #[test]
    fn strip() {
        let image = format!("{PLACEHOLDER}{}{}", mark(0), mark(1));
        assert_eq!(
            strip_image_text(format!("a{image}{image}b\n{image}c")),
            "ab\nc"
        );
        assert_eq!(strip_image_text("plain".into()), "plain");
    }
}
