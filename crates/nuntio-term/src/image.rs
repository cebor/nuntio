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
use image::{ColorType, DynamicImage, ImageDecoder, ImageReader, Limits, RgbaImage};
use thiserror::Error;

use crate::image_scan::ImageRequest;

/// The character of every image cell.
pub(crate) const PLACEHOLDER: char = '\u{10EEEE}';
/// First of the marks encoding an image cell's row and column.
const MARK_BASE: u32 = 0xF0000;
/// Number of marks: rows and columns up to this are encodable.
const MARK_COUNT: u32 = 0xFFFE;
/// Image memory per pane above which images no longer on the active screen
/// are dropped.
const IMAGE_MEMORY_BUDGET: usize = 256 << 20;
/// Image memory per pane above which even images in the scrollback and on
/// the inactive screen are dropped, as far as they aren't visible.
const IMAGE_MEMORY_LIMIT: usize = 512 << 20;
/// Longest side of an image bitmap; larger ones are scaled down. One less
/// than the 4096 texels of the smallest GPU limit, so the atlas's 1-texel
/// padding still fits.
const MAX_BITMAP_SIDE: u64 = 4095;
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
    let [line, column, ..] = cell.zerowidth()? else {
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
    // An image cell is the placeholder and two marks; anything after them
    // is text, even if it lies in the marks' range.
    let mut marks_left = 0u8;
    for c in text.chars() {
        if c == PLACEHOLDER {
            marks_left = 2;
        } else if marks_left > 0 && is_mark(c) {
            marks_left -= 1;
        } else {
            marks_left = 0;
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
    let mut budget = limits.clone();
    reader.limits(limits);
    // The first frame of animations, upright as the file's EXIF says.
    let mut decoder = reader.into_decoder()?;
    // `into_decoder` only checks the dimensions; reserve what decoding and
    // the RGBA copy allocate.
    budget.reserve(decoder.total_bytes())?;
    if decoder.color_type() != ColorType::Rgba8 {
        let (w, h) = decoder.dimensions();
        budget.reserve(u64::from(w) * u64::from(h) * 4)?;
    }
    let orientation = decoder.orientation()?;
    let mut img = DynamicImage::from_decoder(decoder)?;
    img.apply_orientation(orientation);
    let decoded = img.into_rgba8();

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
    let scaled = scale(decoded, (drawn.0 as u32, drawn.1 as u32));
    imageops::replace(&mut canvas, &scaled, 0, 0);

    Ok(TermImage {
        uid: NEXT_UID.fetch_add(1, Ordering::Relaxed),
        columns: columns as usize,
        lines: lines as usize,
        width: canvas.width(),
        height: canvas.height(),
        rgba: canvas.into_raw().into_boxed_slice(),
    })
}

/// `image` scaled to `size`. Transparent pixels are averaged premultiplied,
/// so the color of fully transparent ones doesn't bleed into the edges.
fn scale(mut image: RgbaImage, size: (u32, u32)) -> RgbaImage {
    let translucent = image.pixels().any(|p| p.0[3] < 255);
    if translucent {
        premultiply(&mut image);
    }
    // Shrinking by area averaging is cheap and doesn't alias; only the
    // axes that get smaller are shrunk.
    let shrunk = (image.width().min(size.0), image.height().min(size.1));
    if shrunk != image.dimensions() {
        image = imageops::thumbnail(&image, shrunk.0, shrunk.1);
    }
    // What is left is enlarging, from an image no larger than `size`.
    if image.dimensions() != size {
        image = imageops::resize(&image, size.0, size.1, FilterType::Triangle);
    }
    if translucent {
        unpremultiply(&mut image);
    }
    image
}

/// Multiply the colors with the alpha channel, rounding.
fn premultiply(image: &mut RgbaImage) {
    for pixel in image.pixels_mut() {
        let a = pixel.0[3] as u32;
        for c in &mut pixel.0[..3] {
            *c = ((*c as u32 * a + 127) / 255) as u8;
        }
    }
}

/// Undo [`premultiply`], rounding.
fn unpremultiply(image: &mut RgbaImage) {
    for pixel in image.pixels_mut() {
        let a = pixel.0[3] as u32;
        if a == 0 || a == 255 {
            continue;
        }
        for c in &mut pixel.0[..3] {
            *c = ((*c as u32 * 255 + a / 2) / a).min(255) as u8;
        }
    }
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

/// A stored image and whether it was placed on the primary screen.
struct StoredImage {
    image: Arc<TermImage>,
    on_primary: bool,
}

/// The images of one pane, by the id their cells carry.
pub(crate) struct ImageStore {
    next_id: u32,
    images: HashMap<u32, StoredImage>,
    /// Ids, oldest first.
    order: VecDeque<u32>,
    recent: HashMap<ImageKey, u32>,
    bytes: usize,
    /// Above this, images no cell of the active grid refers to are dropped.
    budget: usize,
    /// Above this, images not visible are dropped.
    limit: usize,
}

impl Default for ImageStore {
    fn default() -> Self {
        Self::with_limits(IMAGE_MEMORY_BUDGET, IMAGE_MEMORY_LIMIT)
    }
}

impl ImageStore {
    /// A store whose hard limit is twice the `budget`.
    #[cfg(test)]
    pub(crate) fn with_budget(budget: usize) -> Self {
        Self::with_limits(budget, budget.saturating_mul(2))
    }

    fn with_limits(budget: usize, limit: usize) -> Self {
        Self {
            next_id: 1,
            images: HashMap::new(),
            order: VecDeque::new(),
            recent: HashMap::new(),
            bytes: 0,
            budget,
            limit,
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.images.is_empty()
    }

    pub(crate) fn get(&self, id: u32) -> Option<&Arc<TermImage>> {
        self.images.get(&id).map(|stored| &stored.image)
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
        self.images.insert(
            id,
            StoredImage {
                image: Arc::new(image),
                on_primary: false,
            },
        );
        self.order.push_back(id);
        self.recent.insert(key, id);
        id
    }

    /// Note that image `id` was placed on the alternate screen (`alt`) or
    /// the primary one. Only the primary screen is remembered: alacritty
    /// clears the alternate one whenever it is entered.
    pub(crate) fn placed(&mut self, id: u32, alt: bool) {
        if !alt && let Some(stored) = self.images.get_mut(&id) {
            stored.on_primary = true;
        }
    }

    /// Over budget, drop the oldest images no cell of the active screen
    /// refers to, unless they were placed on the primary screen while the
    /// alternate one is active: its grid isn't reachable. Images placed
    /// only on the alternate screen aren't protected, as it is cleared
    /// whenever it is entered. Still over the hard limit, drop the oldest
    /// ones that aren't visible, wherever they were placed, and then any.
    pub(crate) fn evict<T: EventListener>(&mut self, term: &Term<T>) {
        if self.bytes <= self.budget {
            return;
        }
        let alt = term.mode().contains(TermMode::ALT_SCREEN);
        let grid = term.grid();
        let referenced = referenced_ids(term, grid.topmost_line().0..=grid.bottommost_line().0);
        self.drop_oldest(self.budget, |id, stored| {
            let on_inactive = alt && stored.on_primary;
            !referenced.contains(&id) && !on_inactive
        });
        if self.bytes > self.limit {
            let screen_lines = grid.screen_lines() as i32;
            let offset = grid.display_offset() as i32;
            let mut visible = referenced_ids(term, 0..=screen_lines - 1);
            visible.extend(referenced_ids(term, -offset..=screen_lines - 1 - offset));
            self.drop_oldest(self.budget, |id, _| !visible.contains(&id));
        }
        // Overlapping images can keep any number of them visible: the limit
        // holds anyway.
        if self.bytes > self.limit {
            self.drop_oldest(self.limit, |_, _| true);
        }
        let images = &self.images;
        self.recent.retain(|_, id| images.contains_key(id));
    }

    /// Drop the oldest images `droppable` accepts until at most `target`
    /// bytes are left.
    fn drop_oldest(&mut self, target: usize, mut droppable: impl FnMut(u32, &StoredImage) -> bool) {
        let mut kept = VecDeque::with_capacity(self.order.len());
        while let Some(id) = self.order.pop_front() {
            let stored = self.images.get(&id);
            if self.bytes > target && stored.is_some_and(|s| droppable(id, s)) {
                if let Some(stored) = self.images.remove(&id) {
                    self.bytes -= stored.image.rgba.len();
                }
            } else {
                kept.push_back(id);
            }
        }
        self.order = kept;
    }
}

/// The ids of the images on `lines` of the active grid.
fn referenced_ids<T: EventListener>(
    term: &Term<T>,
    lines: std::ops::RangeInclusive<i32>,
) -> HashSet<u32> {
    let grid = term.grid();
    let mut ids = HashSet::new();
    for line in lines {
        let row = &grid[Line(line)];
        for column in 0..grid.columns() {
            if let Some((id, ..)) = image_cell_ref(&row[Column(column)]) {
                ids.insert(id);
            }
        }
    }
    ids
}

/// Remove the image cells from `columns` on, in every line of the active
/// grid, before the terminal is narrowed to `columns`. The cells would
/// otherwise be reflowed onto new lines, which cuts an image into strips.
/// Default cells are empty, so shrinking drops them.
///
/// Only the active grid is reachable: alacritty keeps the inactive one
/// private. Image cells there are reflowed when it becomes active again.
pub(crate) fn crop_columns<T: EventListener>(term: &mut Term<T>, columns: usize) {
    let (top, bottom, width) = {
        let grid = term.grid();
        (
            grid.topmost_line().0,
            grid.bottommost_line().0,
            grid.columns(),
        )
    };
    let grid = term.grid_mut();
    for line in top..=bottom {
        let row = &mut grid[Line(line)];
        for column in columns..width {
            if image_cell_ref(&row[Column(column)]).is_some() {
                row[Column(column)] = Cell::default();
            }
        }
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
            let before = term.grid().cursor.point.line;
            term.linefeed();
            let line = term.grid().cursor.point.line;
            // A cursor below the scroll region can't move down; it would
            // overwrite the previous row. Crop the image there.
            if line == before
                && image_cell_ref(&term.grid()[line][Column(x)]) == Some((id, row - 1, 0))
            {
                break;
            }
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
    use alacritty_terminal::grid::Scroll;
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
    fn a_decompression_bomb_is_rejected() {
        // A GIF with a 16384×16384 screen and a 1×1 frame: 35 bytes that
        // would decode to 1 GiB.
        let gif = [
            0x47, 0x49, 0x46, 0x38, 0x39, 0x61, 0x00, 0x40, 0x00, 0x40, 0x80, 0x00, 0x00, 0xFF,
            0xFF, 0xFF, 0x00, 0x00, 0x00, 0x2C, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x01, 0x00,
            0x00, 0x02, 0x02, 0x44, 0x01, 0x00, 0x3B,
        ];
        let payload = base64::engine::general_purpose::STANDARD.encode(gif);
        let request = request("inline=1", payload.into_bytes());
        let result = decode(&request, &parse_args(&request.args), GEOMETRY);
        assert!(
            matches!(result, Err(ImageError::Image(image::ImageError::Limits(_)))),
            "{result:?}"
        );
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

    fn test_key(n: u64) -> ImageKey {
        ImageKey {
            hash: n,
            geometry: GEOMETRY,
        }
    }

    /// Three 16-byte images in a 4×2 terminal: the first two in the
    /// scrollback, the third visible. Returns their ids.
    fn scrolled_images(store: &mut ImageStore, term: &mut Term<VoidListener>) -> [u32; 3] {
        let mut parser: Processor = Processor::new();
        [1, 2, 3].map(|n| {
            let id = store.insert(test_key(n), test_image(2, 2));
            place(term, id, &test_image(2, 2));
            if n < 3 {
                parser.advance(term, b"\r\n\r\n\r\n");
            }
            id
        })
    }

    #[test]
    fn cropping_before_narrowing_keeps_images_in_one_piece_per_row() {
        let mut term = term(8, 4);
        place(&mut term, 1, &test_image(6, 2));
        // The cursor below the image, as after a prompt: a cursor right of
        // the new width would reflow its own line.
        let mut parser: Processor = Processor::new();
        parser.advance(&mut term, b"\x1b[4;1H");
        crop_columns(&mut term, 4);
        let size = crate::TermSize {
            columns: 4,
            lines: 4,
            cell_width: 1,
            cell_height: 1,
        };
        term.resize(size);
        for (line, row) in [(0, 0), (1, 1)] {
            for column in 0..4 {
                assert_eq!(
                    image_cell_ref(cell_at(&term, line, column)),
                    Some((1, row, column))
                );
            }
        }
        for line in 2..4 {
            assert!((0..4).all(|c| image_cell_ref(cell_at(&term, line, c)).is_none()));
        }
        assert_eq!(term.grid().history_size(), 0);
    }

    #[test]
    fn place_stops_below_the_scroll_region() {
        let mut term = term(10, 5);
        let mut parser: Processor = Processor::new();
        // Lines 1 to 3 scroll; the cursor goes to the last line, below them.
        parser.advance(&mut term, b"\x1b[1;3r\x1b[5;1H");
        place(&mut term, 1, &test_image(2, 3));
        assert_eq!(image_cell_ref(cell_at(&term, 4, 0)), Some((1, 0, 0)));
        for line in 0..4 {
            assert!(
                (0..10).all(|c| image_cell_ref(cell_at(&term, line, c)).is_none()),
                "line {line}"
            );
        }
        assert_eq!(term.grid().cursor.point.line.0, 4);
        assert_eq!(term.grid().cursor.point.column.0, 2);
    }

    #[test]
    fn the_hard_limit_drops_images_in_the_scrollback() {
        let mut term = term(4, 2);
        // Budget 16 bytes, limit 32: three images are over the limit.
        let mut store = ImageStore::with_budget(16);
        let [first, second, third] = scrolled_images(&mut store, &mut term);
        store.evict(&term);
        assert!(store.get(first).is_none());
        assert!(store.get(second).is_none());
        assert!(store.get(third).is_some());
        assert_eq!(store.cached(test_key(1)), None);
        assert_eq!(store.cached(test_key(3)), Some(third));
    }

    #[test]
    fn the_hard_limit_keeps_images_in_the_viewport() {
        let mut term = term(4, 2);
        let mut store = ImageStore::with_budget(16);
        let [first, second, third] = scrolled_images(&mut store, &mut term);
        term.scroll_display(Scroll::Top);
        store.evict(&term);
        assert!(store.get(first).is_some(), "shown by the scrolled viewport");
        assert!(store.get(second).is_none());
        assert!(store.get(third).is_some());
    }

    #[test]
    fn images_of_the_primary_screen_survive_while_the_alt_screen_is_active() {
        let mut term = term(4, 2);
        let mut parser: Processor = Processor::new();
        let mut store = ImageStore::with_budget(16);
        let primary = store.insert(test_key(1), test_image(2, 2));
        place(&mut term, primary, &test_image(2, 2));
        store.placed(primary, false);
        parser.advance(&mut term, b"\x1b[?1049h");
        let alt = store.insert(test_key(2), test_image(2, 2));
        place(&mut term, alt, &test_image(2, 2));
        store.placed(alt, true);
        // Never placed anywhere, so nothing protects it.
        let unplaced = store.insert(test_key(3), test_image(2, 2));
        store.evict(&term);
        assert!(store.get(primary).is_some());
        assert!(store.get(alt).is_some());
        assert!(store.get(unplaced).is_none());
    }

    #[test]
    fn images_of_the_alt_screen_are_dropped_after_leaving_it() {
        let mut term = term(4, 2);
        let mut parser: Processor = Processor::new();
        let mut store = ImageStore::with_budget(16);
        parser.advance(&mut term, b"\x1b[?1049h");
        let alt = store.insert(test_key(1), test_image(2, 2));
        place(&mut term, alt, &test_image(2, 2));
        store.placed(alt, true);
        // alacritty clears the alternate screen whenever it is entered.
        parser.advance(&mut term, b"\x1b[?1049l");
        let primary = store.insert(test_key(2), test_image(2, 2));
        place(&mut term, primary, &test_image(2, 2));
        store.placed(primary, false);
        store.evict(&term);
        assert!(store.get(alt).is_none());
        assert!(store.get(primary).is_some());
    }

    #[test]
    fn the_hard_limit_holds_when_every_image_is_visible() {
        let mut term = term(4, 2);
        let mut parser: Processor = Processor::new();
        // Budget 16 bytes, limit 32.
        let mut store = ImageStore::with_budget(16);
        // Each image leaves one column of the one before visible.
        let [first, second, third] = [1, 2, 3].map(|n| {
            parser.advance(&mut term, format!("\x1b[1;{n}H").as_bytes());
            let id = store.insert(test_key(n), test_image(2, 2));
            place(&mut term, id, &test_image(2, 2));
            id
        });
        store.evict(&term);
        assert!(store.get(first).is_none());
        assert!(store.get(second).is_some());
        assert!(store.get(third).is_some());
    }

    #[test]
    fn an_image_cell_with_a_combining_mark_is_still_one() {
        let mut cell = image_cell(1, 0, 0, Cell::default().bg);
        cell.push_zerowidth('\u{301}');
        assert_eq!(image_cell_ref(&cell), Some((1, 0, 0)));
    }

    #[test]
    fn translucent_pixels_are_scaled_premultiplied() {
        let mut source = RgbaImage::new(2, 1);
        source.put_pixel(0, 0, image::Rgba([255, 0, 0, 255]));
        source.put_pixel(1, 0, image::Rgba([0, 0, 0, 0]));
        let mut bytes = Cursor::new(Vec::new());
        source.write_to(&mut bytes, ImageFormat::Png).unwrap();
        let payload = base64::engine::general_purpose::STANDARD
            .encode(bytes.into_inner())
            .into_bytes();
        let image = decode_with(
            "inline=1;width=1px;height=1px;preserveAspectRatio=0",
            payload,
            GEOMETRY,
        );
        let pixel = &image.rgba[..4];
        assert!(pixel[0] >= 250, "red survived: {pixel:?}");
        assert!(
            (127..=129).contains(&pixel[3]),
            "half transparent: {pixel:?}"
        );
    }

    #[test]
    fn strip() {
        let image = format!("{PLACEHOLDER}{}{}", mark(0), mark(1));
        assert_eq!(
            strip_image_text(format!("a{image}{image}b\n{image}c")),
            "ab\nc"
        );
        assert_eq!(strip_image_text("plain".into()), "plain");
        // A Nerd Font icon after an image is text.
        assert_eq!(strip_image_text(format!("{image}\u{F0001}x")), "\u{F0001}x");
    }
}
