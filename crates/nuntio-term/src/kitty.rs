//! The Kitty graphics protocol (`ESC _ G <control> ; <base64> ESC \`).
//!
//! A placement becomes ordinary image cells in the grid, like an iTerm2
//! image (see `image.rs`), as WezTerm does it. The transmitted pixels are
//! kept as a [`Source`] under the program's image id; every placement renders
//! its own cropped and scaled [`TermImage`] from it. Images replace the text
//! in their cells, so z-order against text isn't drawn and `z` only selects
//! placements for deletion.
//!
//! Not supported, and answered with `EINVAL`: Unicode placeholders (`U=1`),
//! relative placements (`P`, `Q`, `H`, `V`), animation (`a=f|a|c`) and any
//! transmission medium but direct data (`t=d`).
//!
//! A command takes three steps so that decoding and scaling run without
//! the term lock: [`plan`] reads the store, [`prepare`] does the heavy work
//! on any thread, [`apply`] changes the grid and the store.

use std::collections::HashSet;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use alacritty_terminal::event::EventListener;
use alacritty_terminal::term::{Term, TermMode};
use image::RgbaImage;
use image::imageops;

use crate::image::{
    CellGeometry, ImageError, ImageStore, KittyPlacement, MAX_BITMAP_SIDE, MAX_COLUMNS, MAX_LINES,
    MAX_SCREENS, TermImage, decode_base64, decode_rgba, erase_image, next_uid, place, place_fixed,
    rows_above_screen, scale, visible_image_ids,
};
use crate::image_scan::MAX_PAYLOAD;

/// Kitty source images a pane keeps; the oldest are dropped beyond it.
pub(crate) const KITTY_SOURCE_LIMIT: usize = 128 << 20;
/// First id the terminal assigns to images the program didn't name.
pub(crate) const FIRST_TERMINAL_ID: u32 = 1 << 31;
/// Longest side of a source image.
const MAX_SIDE: u32 = 16384;
/// Most bytes an `o=z` payload may inflate to.
const INFLATE_LIMIT: usize = 256 << 20;

const BAD_DATA: &str = "EINVAL:bad image data";
const SIZE_MISMATCH: &str = "EINVAL:size mismatch";
const TOO_BIG: &str = "EFBIG:image too large";
const BAD_MEDIUM: &str = "EINVAL:unsupported transmission medium";
const UNSUPPORTED: &str = "EINVAL:unsupported";
const NO_IMAGE: &str = "ENOENT:no such image";
const NO_CELL_SIZE: &str = "EINVAL:the cell size is unknown";
const EMPTY_RECT: &str = "EINVAL:empty source rectangle";
const NO_MEMORY: &str = "ENOMEM:image too large";

static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);

/// The keys of one command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Command {
    /// `a`: `t` transmit, `T` transmit and place, `p` place, `d` delete,
    /// `q` query.
    action: u8,
    /// `q`: 1 suppresses `OK`, 2 suppresses errors too.
    quiet: u8,
    /// `f`: 24 or 32 for raw pixels, 100 for PNG.
    format: u32,
    /// `t`: `d` for direct data.
    medium: u8,
    /// `o=z`.
    compressed: bool,
    /// `s` and `v`: the pixel size of raw data.
    width: u32,
    height: u32,
    /// `i`.
    id: Option<u32>,
    /// `I`.
    number: Option<u32>,
    /// `p`.
    placement: Option<u32>,
    /// `m=1`: more chunks follow.
    more: bool,
    /// `x`, `y`, `w`, `h`: the part of the image to show; 0 for `w`/`h` is
    /// the rest. For deletes `x` and `y` select by position or range.
    src: (u32, u32, u32, u32),
    /// `X`, `Y`: pixel offset inside the first cell.
    offset: (u32, u32),
    /// `c`, `r`: the cells to stretch the image over; 0 is unset.
    cells: (u32, u32),
    /// `C=1`: leave the cursor where it is.
    keep_cursor: bool,
    /// `z`.
    z: i32,
    /// `d`: what to delete.
    delete: u8,
    /// Uses something this terminal doesn't implement.
    unsupported: bool,
}

impl Default for Command {
    fn default() -> Self {
        Self {
            action: b't',
            quiet: 0,
            format: 32,
            medium: b'd',
            compressed: false,
            width: 0,
            height: 0,
            id: None,
            number: None,
            placement: None,
            more: false,
            src: (0, 0, 0, 0),
            offset: (0, 0),
            cells: (0, 0),
            keep_cursor: false,
            z: 0,
            delete: b'a',
            unsupported: false,
        }
    }
}

/// The command in `control`, the `key=value,…` list. Unknown keys and
/// malformed values are ignored.
pub(crate) fn parse(control: &[u8]) -> Command {
    let mut cmd = Command::default();
    for pair in control.split(|&b| b == b',') {
        let Some(eq) = pair.iter().position(|&b| b == b'=') else {
            continue;
        };
        let ([key], value) = (&pair[..eq], &pair[eq + 1..]) else {
            continue;
        };
        let text = std::str::from_utf8(value).unwrap_or("");
        let ch = value.first().copied();
        let number = text.parse::<u32>().unwrap_or(0);
        let nonzero = (number != 0).then_some(number);
        match key {
            b'a' => cmd.action = ch.unwrap_or(cmd.action),
            b'q' => cmd.quiet = number.min(2) as u8,
            b'f' => cmd.format = number,
            b't' => cmd.medium = ch.unwrap_or(0),
            b'o' => cmd.compressed = ch == Some(b'z'),
            b's' => cmd.width = number,
            b'v' => cmd.height = number,
            b'i' => cmd.id = nonzero,
            b'I' => cmd.number = nonzero,
            b'p' => cmd.placement = nonzero,
            b'm' => cmd.more = number == 1,
            b'x' => cmd.src.0 = number,
            b'y' => cmd.src.1 = number,
            b'w' => cmd.src.2 = number,
            b'h' => cmd.src.3 = number,
            b'X' => cmd.offset.0 = number,
            b'Y' => cmd.offset.1 = number,
            b'c' => cmd.cells.0 = number,
            b'r' => cmd.cells.1 = number,
            b'C' => cmd.keep_cursor = number == 1,
            b'z' => cmd.z = text.parse().unwrap_or(0),
            b'd' => cmd.delete = ch.unwrap_or(cmd.delete),
            b'U' => cmd.unsupported |= number != 0,
            b'P' | b'Q' | b'H' | b'V' => cmd.unsupported = true,
            _ => {}
        }
    }
    if !matches!(cmd.action, b't' | b'T' | b'p' | b'd' | b'q') {
        cmd.unsupported = true;
    }
    cmd
}

/// A command with all its chunks.
#[derive(Debug)]
pub(crate) struct Transfer {
    command: Command,
    /// The base64 data of all chunks.
    payload: Vec<u8>,
}

/// What a chunk did to the pending transfer.
#[derive(Debug)]
pub(crate) enum Accepted {
    /// More chunks are due.
    Pending,
    Complete(Transfer),
    /// The transfer was dropped; the reply for the program, if any.
    Rejected(Option<String>),
}

/// Collects the chunks (`m=1`) of a transfer.
pub(crate) struct Transfers {
    pending: Option<Transfer>,
    /// A rejected transfer is swallowed up to its last chunk.
    discarding: bool,
    limit: usize,
}

impl Default for Transfers {
    fn default() -> Self {
        Self {
            pending: None,
            discarding: false,
            limit: MAX_PAYLOAD,
        }
    }
}

impl Transfers {
    /// Take one chunk. Later chunks only carry `m` (and `q`); the first
    /// chunk's command applies to the whole transfer.
    pub(crate) fn accept(&mut self, control: &[u8], payload: Vec<u8>) -> Accepted {
        let chunk = parse(control);
        if self.discarding {
            self.discarding = chunk.more;
            return Accepted::Pending;
        }
        let mut transfer = match self.pending.take() {
            Some(mut transfer) => {
                if transfer.payload.len() + payload.len() > self.limit {
                    tracing::warn!("graphics transfer larger than the limit ignored");
                    self.discarding = chunk.more;
                    return Accepted::Rejected(reply(&transfer.command, None, Err(NO_MEMORY)));
                }
                transfer.payload.extend_from_slice(&payload);
                transfer
            }
            None => Transfer {
                command: chunk.clone(),
                payload,
            },
        };
        if chunk.more {
            self.pending = Some(transfer);
            Accepted::Pending
        } else {
            // The last chunk may repeat nothing but `m=0`.
            transfer.command.more = false;
            Accepted::Complete(transfer)
        }
    }
}

/// The pixels of a transmitted image: straight (not premultiplied) RGBA.
#[derive(Debug, Clone)]
pub(crate) struct Source {
    pub(crate) rgba: Arc<Vec<u8>>,
    pub(crate) width: u32,
    pub(crate) height: u32,
    /// Differs for every transmission, also under the same image id.
    generation: u64,
}

impl Source {
    fn new(width: u32, height: u32, rgba: Vec<u8>) -> Self {
        Self {
            rgba: Arc::new(rgba),
            width,
            height,
            generation: NEXT_GENERATION.fetch_add(1, Ordering::Relaxed),
        }
    }
}

/// The decoded pixels of the transmitted `base64` data.
pub(crate) fn decode_source(cmd: &Command, base64: &[u8]) -> Result<Source, &'static str> {
    if cmd.medium != b'd' {
        return Err(BAD_MEDIUM);
    }
    let mut bytes = decode_base64(base64).map_err(|_| BAD_DATA)?;
    if cmd.compressed {
        bytes = miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(&bytes, INFLATE_LIMIT)
            .map_err(|err| {
                if err.status == miniz_oxide::inflate::TINFLStatus::HasMoreOutput {
                    TOO_BIG
                } else {
                    BAD_DATA
                }
            })?;
    }
    match cmd.format {
        100 => {
            let (rgba, _) = decode_rgba(bytes).map_err(|err| match err {
                ImageError::Image(image::ImageError::Limits(_)) => TOO_BIG,
                _ => BAD_DATA,
            })?;
            Ok(Source::new(rgba.width(), rgba.height(), rgba.into_raw()))
        }
        24 | 32 => {
            let (width, height) = (cmd.width, cmd.height);
            if width == 0 || height == 0 {
                return Err(SIZE_MISMATCH);
            }
            if width > MAX_SIDE || height > MAX_SIDE {
                return Err(TOO_BIG);
            }
            let pixels = width as usize * height as usize;
            let rgba = if cmd.format == 32 {
                if bytes.len() < pixels * 4 {
                    return Err(SIZE_MISMATCH);
                }
                bytes.truncate(pixels * 4);
                bytes
            } else {
                if bytes.len() < pixels * 3 {
                    return Err(SIZE_MISMATCH);
                }
                let mut rgba = Vec::with_capacity(pixels * 4);
                for pixel in bytes[..pixels * 3].as_chunks::<3>().0 {
                    rgba.extend_from_slice(pixel);
                    rgba.push(255);
                }
                rgba
            };
            Ok(Source::new(width, height, rgba))
        }
        _ => Err(BAD_DATA),
    }
}

/// What a rendering depends on: the source, the part shown, where and how
/// big. Two placements with the same key look the same.
fn placement_key(source: &Source, cmd: &Command, geometry: CellGeometry) -> u64 {
    let mut hasher = DefaultHasher::new();
    source.generation.hash(&mut hasher);
    cmd.src.hash(&mut hasher);
    cmd.offset.hash(&mut hasher);
    cmd.cells.hash(&mut hasher);
    geometry.hash(&mut hasher);
    hasher.finish()
}

/// The part of `source` that `cmd` shows, as a new image.
fn crop(source: &Source, (x, y, w, h): (u32, u32, u32, u32)) -> RgbaImage {
    let mut out = Vec::with_capacity(w as usize * h as usize * 4);
    for row in y..y + h {
        let start = (row as usize * source.width as usize + x as usize) * 4;
        out.extend_from_slice(&source.rgba[start..start + w as usize * 4]);
    }
    RgbaImage::from_raw(w, h, out).expect("the buffer has w×h pixels")
}

/// Render the placement `cmd` describes: the source rectangle at the
/// offset, stretched over the cells if `c` or `r` is given and at its pixel
/// size otherwise, on a canvas of whole cells.
fn render(
    source: &Source,
    cmd: &Command,
    geometry: CellGeometry,
) -> Result<TermImage, &'static str> {
    let (cw, ch) = (
        u64::from(geometry.cell_width),
        u64::from(geometry.cell_height),
    );
    if cw == 0 || ch == 0 {
        return Err(NO_CELL_SIZE);
    }
    let x = cmd.src.0.min(source.width);
    let y = cmd.src.1.min(source.height);
    let rest = |wanted: u32, available: u32| {
        if wanted == 0 {
            available
        } else {
            wanted.min(available)
        }
    };
    let w = rest(cmd.src.2, source.width - x);
    let h = rest(cmd.src.3, source.height - y);
    if w == 0 || h == 0 {
        return Err(EMPTY_RECT);
    }
    let (w64, h64) = (u64::from(w), u64::from(h));
    let (ox, oy) = (u64::from(cmd.offset.0), u64::from(cmd.offset.1));
    let (c, r) = (u64::from(cmd.cells.0), u64::from(cmd.cells.1));
    let (columns, lines) = match (c, r) {
        (0, 0) => ((ox + w64).div_ceil(cw), (oy + h64).div_ceil(ch)),
        (c, 0) => (c, (c * cw * h64).div_ceil(w64 * ch)),
        (0, r) => ((r * ch * w64).div_ceil(h64 * cw), r),
        (c, r) => (c, r),
    };
    let max_lines = (geometry.lines as u64 * MAX_SCREENS).clamp(1, MAX_LINES);
    let columns = columns.clamp(1, MAX_COLUMNS);
    let lines = lines.clamp(1, max_lines);
    let canvas = (columns * cw, lines * ch);
    let longest = canvas.0.max(canvas.1);
    // Past the bitmap limit everything shrinks by the same factor.
    let fit = |n: u64| {
        if longest > MAX_BITMAP_SIDE {
            n * MAX_BITMAP_SIDE / longest
        } else {
            n
        }
    };
    let canvas = (fit(canvas.0).max(1), fit(canvas.1).max(1));
    let origin = (fit(ox), fit(oy));
    let room = (
        canvas.0.saturating_sub(origin.0),
        canvas.1.saturating_sub(origin.1),
    );
    let drawn = if c != 0 || r != 0 {
        room
    } else {
        (fit(w64).max(1).min(room.0), fit(h64).max(1).min(room.1))
    };
    let mut bitmap = RgbaImage::new(canvas.0 as u32, canvas.1 as u32);
    if drawn.0 > 0 && drawn.1 > 0 {
        let scaled = scale(crop(source, (x, y, w, h)), (drawn.0 as u32, drawn.1 as u32));
        imageops::replace(&mut bitmap, &scaled, origin.0 as i64, origin.1 as i64);
    }
    Ok(TermImage {
        uid: next_uid(),
        columns: columns as usize,
        lines: lines as usize,
        width: bitmap.width(),
        height: bitmap.height(),
        rgba: bitmap.into_raw().into_boxed_slice(),
    })
}

/// A command and what planning took from the store for it.
pub(crate) struct Job {
    transfer: Transfer,
    geometry: CellGeometry,
    /// For `p`: the Kitty image id, its source, and the key of the
    /// placement it already has under the same placement id.
    resolved: Option<(u32, Source, Option<u64>)>,
}

impl Job {
    /// Whether [`prepare`] decodes or scales, so it is worth running
    /// without the term lock.
    pub(crate) fn needs_work(&self) -> bool {
        let cmd = &self.transfer.command;
        !cmd.unsupported && matches!(cmd.action, b't' | b'T' | b'p' | b'q')
    }
}

/// Look up what `transfer` refers to in `store`.
pub(crate) fn plan(store: &ImageStore, transfer: Transfer, geometry: CellGeometry) -> Job {
    let cmd = &transfer.command;
    let resolved = if cmd.action == b'p' && !cmd.unsupported {
        cmd.id
            .or_else(|| cmd.number.and_then(|n| store.kitty_number(n)))
            .and_then(|id| {
                let source = store.kitty_source(id)?.clone();
                let existing = cmd
                    .placement
                    .and_then(|p| store.find_kitty(id, p))
                    .and_then(|stored| store.kitty_placement(stored))
                    .map(|k| k.key);
                Some((id, source, existing))
            })
    } else {
        None
    };
    Job {
        transfer,
        geometry,
        resolved,
    }
}

/// A placement rendered, or known to look like the one it replaces.
struct Render {
    key: u64,
    /// `None`: the placement it replaces already looks like this.
    image: Option<TermImage>,
}

type Rendered = Result<Render, &'static str>;

fn render_placement(
    source: &Source,
    cmd: &Command,
    geometry: CellGeometry,
    existing: Option<u64>,
) -> Rendered {
    let key = placement_key(source, cmd, geometry);
    if existing == Some(key) {
        return Ok(Render { key, image: None });
    }
    render(source, cmd, geometry).map(|image| Render {
        key,
        image: Some(image),
    })
}

enum Kind {
    Unsupported,
    Delete,
    Query(Result<(), &'static str>),
    /// `t`, or `T` with the rendering of its placement.
    Transmit {
        source: Result<Source, &'static str>,
        place: Option<Rendered>,
    },
    /// `p`: the Kitty image id and the rendering.
    Place(Result<(u32, Render), &'static str>),
}

/// A command with the decoding and scaling done.
pub(crate) struct Prepared {
    command: Command,
    kind: Kind,
}

/// Do the heavy work of a command. Touches neither the grid nor the store.
pub(crate) fn prepare(job: Job) -> Prepared {
    let Job {
        transfer: Transfer { command, payload },
        geometry,
        resolved,
    } = job;
    let kind = if command.unsupported {
        Kind::Unsupported
    } else {
        match command.action {
            b'd' => Kind::Delete,
            b'q' => Kind::Query(decode_source(&command, &payload).map(drop)),
            b't' => Kind::Transmit {
                source: decode_source(&command, &payload),
                place: None,
            },
            b'T' => {
                let source = decode_source(&command, &payload);
                let place = source
                    .as_ref()
                    .ok()
                    .map(|source| render_placement(source, &command, geometry, None));
                Kind::Transmit { source, place }
            }
            _ => Kind::Place(match resolved {
                None => Err(NO_IMAGE),
                Some((id, source, existing)) => {
                    render_placement(&source, &command, geometry, existing)
                        .map(|render| (id, render))
                }
            }),
        }
    };
    Prepared { command, kind }
}

/// The reply for the program: `ESC _ G i=<id>[,I=<n>][,p=<p>] ; OK|error ESC \`.
/// None unless the command named an image, and as `q` allows. `image` is
/// the id the terminal chose, if the program gave a number only.
fn reply(cmd: &Command, image: Option<u32>, result: Result<(), &str>) -> Option<String> {
    if cmd.id.is_none() && cmd.number.is_none() {
        return None;
    }
    let text = match result {
        Ok(()) if cmd.quiet == 0 => "OK",
        Err(err) if cmd.quiet < 2 => err,
        _ => return None,
    };
    let mut keys = Vec::new();
    if let Some(id) = image.or(cmd.id) {
        keys.push(format!("i={id}"));
    }
    if let Some(number) = cmd.number {
        keys.push(format!("I={number}"));
    }
    if let Some(placement) = cmd.placement {
        keys.push(format!("p={placement}"));
    }
    Some(format!("\x1b_G{};{text}\x1b\\", keys.join(",")))
}

/// Change the grid and the store as `prepared` says. Returns the reply to
/// write to the program, if any. `cursor_up` is as for iTerm2 images.
pub(crate) fn apply<T: EventListener>(
    prepared: Prepared,
    term: &mut Term<T>,
    store: &mut ImageStore,
    cursor_up: Option<u32>,
) -> Option<String> {
    let Prepared { command: cmd, kind } = prepared;
    match kind {
        Kind::Unsupported => reply(&cmd, None, Err(UNSUPPORTED)),
        Kind::Query(result) => reply(&cmd, None, result),
        Kind::Delete => {
            delete(&cmd, term, store);
            None
        }
        Kind::Transmit { source, place } => {
            let source = match source {
                Ok(source) => source,
                Err(err) => return reply(&cmd, None, Err(err)),
            };
            let image = match (cmd.id, cmd.number) {
                (Some(id), _) => id,
                (None, Some(number)) => {
                    let id = store.new_kitty_id();
                    store.set_kitty_number(number, id);
                    id
                }
                (None, None) => store.new_kitty_id(),
            };
            // New data under an id replaces the image and its placements.
            let old = store.kitty_ids(|k| k.image == image);
            erase_image(term, &old.iter().copied().collect());
            for id in old {
                store.remove(id);
            }
            store.put_kitty_source(image, source);
            let result = match place {
                Some(Ok(render)) => place_image(&cmd, image, render, term, store, cursor_up),
                Some(Err(err)) => Err(err),
                None => Ok(()),
            };
            reply(&cmd, Some(image), result)
        }
        Kind::Place(target) => match target {
            Ok((image, render)) => {
                let result = place_image(&cmd, image, render, term, store, cursor_up);
                reply(&cmd, Some(image), result)
            }
            Err(err) => reply(&cmd, None, Err(err)),
        },
    }
}

/// Put a placement of Kitty image `image` into the grid at the cursor,
/// replacing the placement with the same placement id.
fn place_image<T: EventListener>(
    cmd: &Command,
    image: u32,
    render: Render,
    term: &mut Term<T>,
    store: &mut ImageStore,
    cursor_up: Option<u32>,
) -> Result<(), &'static str> {
    let Render { key, image: drawn } = render;
    let existing = cmd.placement.and_then(|p| store.find_kitty(image, p));
    let placement = cmd.placement.unwrap_or_else(|| store.fresh_placement());
    if let Some(old) = existing {
        erase_image(term, &HashSet::from([old]));
    }
    let (id, picture, inserted) = match (existing, drawn) {
        (Some(old), None) if store.kitty_placement(old).is_some_and(|k| k.key == key) => {
            if let Some(k) = store.kitty_placement_mut(old) {
                k.z = cmd.z;
            }
            let picture = store.get(old).expect("the placement exists").clone();
            (old, picture, false)
        }
        // Planned against a placement that is gone.
        (_, None) => return Err(NO_IMAGE),
        (old, Some(picture)) => {
            if let Some(old) = old {
                store.remove(old);
            }
            let id = store.insert_kitty(
                picture,
                KittyPlacement {
                    image,
                    placement,
                    z: cmd.z,
                    key,
                },
            );
            let picture = store.get(id).expect("just inserted").clone();
            (id, picture, true)
        }
    };
    let hidden = rows_above_screen(term, cursor_up);
    if cmd.keep_cursor {
        place_fixed(term, id, &picture, hidden);
    } else {
        place(term, id, &picture, hidden);
    }
    store.placed(id, term.mode().contains(TermMode::ALT_SCREEN));
    // Memory only grows with new images.
    if inserted {
        store.evict(term);
    }
    Ok(())
}

/// `a=d`: remove the placements `cmd.delete` selects. Uppercase also frees
/// the data of images left without placements.
fn delete<T: EventListener>(cmd: &Command, term: &mut Term<T>, store: &mut ImageStore) {
    let upper = cmd.delete.is_ascii_uppercase();
    let what = cmd.delete.to_ascii_lowercase();
    // Screen position and range, as the program counts: from 1.
    let (x, y) = (cmd.src.0, cmd.src.1);
    let z = cmd.z;
    // The image `i` and `n` name.
    let named = match what {
        b'i' => cmd.id,
        b'n' => cmd.number.and_then(|n| store.kitty_number(n)),
        _ => None,
    };
    let matched = match what {
        b'i' | b'n' => named.map_or_else(Vec::new, |image| {
            store.kitty_ids(|k| image == k.image && cmd.placement.is_none_or(|p| p == k.placement))
        }),
        _ => {
            let cursor = term.grid().cursor.point;
            let cursor = (cursor.line.0 as usize, cursor.column.0);
            let on_screen: Box<dyn Fn(usize, usize) -> bool> = match what {
                b'a' | b'z' | b'r' => Box::new(|_, _| true),
                b'c' => Box::new(move |line, column| (line, column) == cursor),
                b'p' | b'q' => {
                    Box::new(move |line, column| line + 1 == y as usize && column + 1 == x as usize)
                }
                b'x' => Box::new(move |_, column| column + 1 == x as usize),
                b'y' => Box::new(move |line, _| line + 1 == y as usize),
                _ => return,
            };
            let visible = visible_image_ids(term, on_screen);
            let selects = |k: &KittyPlacement| match what {
                b'z' | b'q' => k.z == z,
                b'r' => (x..=y).contains(&k.image),
                _ => true,
            };
            store
                .kitty_ids(selects)
                .into_iter()
                .filter(|id| visible.contains(id))
                .collect()
        }
    };
    let images: HashSet<u32> = if upper {
        matched
            .iter()
            .filter_map(|&id| store.kitty_placement(id).map(|k| k.image))
            .chain(named)
            .collect()
    } else {
        HashSet::new()
    };
    erase_image(term, &matched.iter().copied().collect());
    for id in matched {
        store.remove(id);
    }
    for image in images {
        if store.kitty_ids(|k| k.image == image).is_empty() {
            store.drop_kitty_source(image);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use alacritty_terminal::event::VoidListener;
    use alacritty_terminal::index::{Column, Line};
    use alacritty_terminal::term::Config;
    use alacritty_terminal::vte::ansi::Processor;
    use base64::Engine;

    use super::*;
    use crate::image::image_cell_ref;

    const GEOMETRY: CellGeometry = CellGeometry {
        columns: 80,
        lines: 24,
        cell_width: 10,
        cell_height: 20,
    };

    fn term() -> Term<VoidListener> {
        let size = crate::TermSize {
            columns: 80,
            lines: 24,
            cell_width: 10,
            cell_height: 20,
        };
        Term::new(Config::default(), &size, VoidListener)
    }

    fn b64(bytes: &[u8]) -> Vec<u8> {
        base64::engine::general_purpose::STANDARD
            .encode(bytes)
            .into_bytes()
    }

    /// `width`×`height` red pixels, `channels` bytes each.
    fn raw(width: u32, height: u32, channels: usize) -> Vec<u8> {
        [255, 0, 0, 255][..channels].repeat((width * height) as usize)
    }

    fn png(width: u32, height: u32) -> Vec<u8> {
        let image = RgbaImage::from_pixel(width, height, image::Rgba([0, 255, 0, 255]));
        let mut bytes = Cursor::new(Vec::new());
        image.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
        bytes.into_inner()
    }

    fn finish(
        term: &mut Term<VoidListener>,
        store: &mut ImageStore,
        transfer: Transfer,
    ) -> Option<String> {
        let job = plan(store, transfer, GEOMETRY);
        apply(prepare(job), term, store, None)
    }

    /// Send one command with its whole payload.
    fn send(
        term: &mut Term<VoidListener>,
        store: &mut ImageStore,
        control: &str,
        payload: &[u8],
    ) -> Option<String> {
        let mut transfers = Transfers::default();
        match transfers.accept(control.as_bytes(), payload.to_vec()) {
            Accepted::Complete(transfer) => finish(term, store, transfer),
            other => panic!("not complete: {other:?}"),
        }
    }

    fn image_at(
        term: &Term<VoidListener>,
        line: i32,
        column: usize,
    ) -> Option<(u32, usize, usize)> {
        image_cell_ref(&term.grid()[Line(line)][Column(column)])
    }

    fn cursor(term: &Term<VoidListener>) -> (i32, usize) {
        let point = term.grid().cursor.point;
        (point.line.0, point.column.0)
    }

    fn move_to(term: &mut Term<VoidListener>, line: usize, column: usize) {
        let mut parser: Processor = Processor::new();
        parser.advance(
            term,
            format!("\x1b[{};{}H", line + 1, column + 1).as_bytes(),
        );
    }

    #[test]
    fn parse_reads_the_control_data() {
        let cmd = parse(b"a=T,f=100,i=7,I=9,p=3,c=20,r=10,C=1,z=-2,q=2,m=1,o=z,X=4,Y=5,x=1,y=2,w=3,h=4,bogus=1,s");
        assert_eq!(cmd.action, b'T');
        assert_eq!(cmd.format, 100);
        assert_eq!(
            (cmd.id, cmd.number, cmd.placement),
            (Some(7), Some(9), Some(3))
        );
        assert_eq!(cmd.cells, (20, 10));
        assert!(cmd.keep_cursor && cmd.more && cmd.compressed);
        assert_eq!((cmd.z, cmd.quiet), (-2, 2));
        assert_eq!(cmd.offset, (4, 5));
        assert_eq!(cmd.src, (1, 2, 3, 4));
        assert!(!cmd.unsupported);

        let cmd = parse(b"");
        assert_eq!(
            (cmd.action, cmd.format, cmd.medium, cmd.delete),
            (b't', 32, b'd', b'a')
        );
        assert_eq!(cmd.id, None);
        for control in [&b"a=T,U=1"[..], b"a=p,P=1,Q=2", b"a=f", b"a=a", b"a=p,H=1"] {
            assert!(parse(control).unsupported, "{control:?}");
        }
        assert!(!parse(b"a=p,U=0").unsupported);
    }

    #[test]
    fn transmit_and_place_writes_the_cells() {
        let mut term = term();
        let mut store = ImageStore::default();
        let reply = send(
            &mut term,
            &mut store,
            "a=T,f=32,s=4,v=4,i=1,c=3,r=2",
            &b64(&raw(4, 4, 4)),
        );
        assert_eq!(reply.as_deref(), Some("\x1b_Gi=1;OK\x1b\\"));
        let id = image_at(&term, 0, 0).expect("an image cell").0;
        for (line, row) in [(0, 0), (1, 1)] {
            for column in 0..3 {
                assert_eq!(image_at(&term, line, column), Some((id, row, column)));
            }
            assert_eq!(image_at(&term, line, 3), None);
        }
        assert_eq!(image_at(&term, 2, 0), None);
        // Like iTerm2 images: after the bottom-right corner.
        assert_eq!(cursor(&term), (1, 3));
        let image = store.get(id).unwrap();
        assert_eq!((image.columns, image.lines), (3, 2));
        assert_eq!((image.width, image.height), (30, 40));
        assert!(store.kitty_source(1).is_some());
    }

    #[test]
    fn fixed_placement_leaves_the_cursor() {
        let mut term = term();
        let mut store = ImageStore::default();
        move_to(&mut term, 2, 5);
        let reply = send(
            &mut term,
            &mut store,
            "a=T,f=24,s=2,v=2,i=1,c=3,r=2,C=1,q=1",
            &b64(&raw(2, 2, 3)),
        );
        assert_eq!(reply, None);
        assert_eq!(cursor(&term), (2, 5));
        assert!(image_at(&term, 2, 5).is_some());
        assert!(image_at(&term, 3, 7).is_some());
        assert!(image_at(&term, 3, 8).is_none());
        assert!(image_at(&term, 4, 5).is_none());
    }

    #[test]
    fn fixed_placement_is_cropped_at_the_screen_edges() {
        let mut term = term();
        let mut store = ImageStore::default();
        move_to(&mut term, 22, 78);
        send(
            &mut term,
            &mut store,
            "a=T,f=32,s=1,v=1,i=1,c=5,r=5,C=1,q=2",
            &b64(&raw(1, 1, 4)),
        );
        assert_eq!(cursor(&term), (22, 78));
        assert!(image_at(&term, 23, 79).is_some());
        assert_eq!(
            term.grid()[Line(23)][Column(79)].c,
            crate::image::PLACEHOLDER
        );
    }

    #[test]
    fn placing_again_moves_the_placement() {
        let mut term = term();
        let mut store = ImageStore::default();
        send(
            &mut term,
            &mut store,
            "a=T,f=32,s=1,v=1,i=1,p=2,c=3,r=2,C=1",
            &b64(&raw(1, 1, 4)),
        );
        let old = store.find_kitty(1, 2).expect("the placement is stored");
        assert_eq!(image_at(&term, 0, 0).map(|c| c.0), Some(old));

        move_to(&mut term, 5, 0);
        let reply = send(&mut term, &mut store, "a=p,i=1,p=2,c=3,r=2,C=1", b"");
        assert_eq!(reply.as_deref(), Some("\x1b_Gi=1,p=2;OK\x1b\\"));
        assert_eq!(image_at(&term, 0, 0), None, "the old cells are erased");
        assert_eq!(image_at(&term, 1, 2), None);
        // Same look: the rendering is reused.
        assert_eq!(store.find_kitty(1, 2), Some(old));
        assert_eq!(image_at(&term, 5, 0).map(|c| c.0), Some(old));
        assert!(image_at(&term, 6, 2).is_some());

        // A different size renders again.
        move_to(&mut term, 10, 0);
        send(&mut term, &mut store, "a=p,i=1,p=2,c=4,r=1,C=1", b"");
        let new = store.find_kitty(1, 2).unwrap();
        assert_ne!(new, old);
        assert!(store.get(old).is_none());
        assert_eq!(image_at(&term, 5, 0), None);
        assert!(image_at(&term, 10, 3).is_some());
        assert_eq!(image_at(&term, 10, 4), None);
    }

    #[test]
    fn placements_without_a_number_are_separate() {
        let mut term = term();
        let mut store = ImageStore::default();
        send(
            &mut term,
            &mut store,
            "a=T,f=32,s=1,v=1,i=1,c=1,r=1,C=1",
            &b64(&raw(1, 1, 4)),
        );
        move_to(&mut term, 3, 0);
        send(&mut term, &mut store, "a=p,i=1,c=1,r=1,C=1", b"");
        assert!(image_at(&term, 0, 0).is_some());
        assert!(image_at(&term, 3, 0).is_some());
        assert_eq!(store.kitty_ids(|k| k.image == 1).len(), 2);
    }

    #[test]
    fn delete_by_image_id_erases_and_frees_the_data() {
        let mut term = term();
        let mut store = ImageStore::default();
        let data = b64(&raw(1, 1, 4));
        send(
            &mut term,
            &mut store,
            "a=T,f=32,s=1,v=1,i=1,c=2,r=2,C=1,q=2",
            &data,
        );
        move_to(&mut term, 4, 0);
        send(
            &mut term,
            &mut store,
            "a=T,f=32,s=1,v=1,i=2,c=2,r=2,C=1,q=2",
            &data,
        );

        // Lowercase keeps the data.
        send(&mut term, &mut store, "a=d,d=i,i=1", b"");
        assert_eq!(image_at(&term, 0, 0), None);
        assert_eq!(image_at(&term, 1, 1), None);
        assert!(image_at(&term, 4, 0).is_some());
        assert!(store.kitty_source(1).is_some());
        assert!(store.kitty_ids(|k| k.image == 1).is_empty());

        send(&mut term, &mut store, "a=d,d=I,i=1", b"");
        assert!(store.kitty_source(1).is_none());
        assert!(store.kitty_source(2).is_some());

        send(&mut term, &mut store, "a=d,d=I,i=2,q=2", b"");
        assert_eq!(image_at(&term, 4, 0), None);
        assert!(store.kitty_source(2).is_none());
        assert!(store.is_empty());
        // The erased cells are blank.
        assert_eq!(term.grid()[Line(4)][Column(0)].c, ' ');
    }

    #[test]
    fn delete_selects_placements() {
        let data = b64(&raw(1, 1, 4));
        let setup = |term: &mut Term<VoidListener>, store: &mut ImageStore| {
            // Image 1 at (line 0, column 0) z=0, image 2 at (line 3, column 10) z=5,
            // image 3 at (line 6, column 20) z=5.
            for (line, column, image, z) in [(0, 0, 1, 0), (3, 10, 2, 5), (6, 20, 3, 5)] {
                move_to(term, line, column);
                let control = format!("a=T,f=32,s=1,v=1,i={image},c=2,r=2,z={z},C=1,q=2");
                send(term, store, &control, &data);
            }
            move_to(term, 3, 11);
        };
        let remaining = |term: &Term<VoidListener>| -> Vec<bool> {
            [(0, 0), (3, 10), (6, 20)]
                .iter()
                .map(|&(line, column)| image_at(term, line, column).is_some())
                .collect()
        };
        let cases: [(&str, [bool; 3]); 11] = [
            ("a=d,d=a", [false, false, false]),
            ("a=d,d=c", [true, false, true]),
            ("a=d,d=p,x=21,y=7", [true, true, false]),
            ("a=d,d=q,x=11,y=4,z=5", [true, false, true]),
            ("a=d,d=q,x=11,y=4,z=0", [true, true, true]),
            ("a=d,d=x,x=1", [false, true, true]),
            ("a=d,d=y,y=4", [true, false, true]),
            ("a=d,d=z,z=5", [true, false, false]),
            ("a=d,d=r,x=2,y=3", [true, false, false]),
            ("a=d,d=i,i=2,p=9", [true, true, true]),
            ("a=d,d=n,I=4", [true, true, true]),
        ];
        for (control, expected) in cases {
            let mut term = term();
            let mut store = ImageStore::default();
            setup(&mut term, &mut store);
            assert_eq!(send(&mut term, &mut store, control, b""), None);
            assert_eq!(remaining(&term), expected, "{control}");
        }
    }

    #[test]
    fn uppercase_delete_keeps_data_of_images_with_other_placements() {
        let mut term = term();
        let mut store = ImageStore::default();
        let data = b64(&raw(1, 1, 4));
        send(
            &mut term,
            &mut store,
            "a=T,f=32,s=1,v=1,i=1,p=1,c=1,r=1,C=1,q=2",
            &data,
        );
        move_to(&mut term, 3, 0);
        send(&mut term, &mut store, "a=p,i=1,p=2,c=1,r=1,C=1", b"");
        send(&mut term, &mut store, "a=d,d=P,x=1,y=1", b"");
        assert!(store.kitty_source(1).is_some(), "placement 2 is left");
        move_to(&mut term, 3, 0);
        send(&mut term, &mut store, "a=d,d=C", b"");
        assert!(store.kitty_source(1).is_none());
    }

    #[test]
    fn delete_by_number() {
        let mut term = term();
        let mut store = ImageStore::default();
        let reply = send(
            &mut term,
            &mut store,
            "a=T,f=32,s=1,v=1,I=13,c=1,r=1,C=1",
            &b64(&raw(1, 1, 4)),
        );
        let assigned = store.kitty_number(13).expect("the number is recorded");
        assert!(assigned >= FIRST_TERMINAL_ID);
        assert_eq!(
            reply,
            Some(format!("\x1b_Gi={assigned},I=13;OK\x1b\\")),
            "the terminal tells the id it chose"
        );
        send(&mut term, &mut store, "a=d,d=N,I=13", b"");
        assert_eq!(image_at(&term, 0, 0), None);
        assert!(store.kitty_source(assigned).is_none());
        assert_eq!(store.kitty_number(13), None);
    }

    #[test]
    fn transmitting_again_replaces_the_placements() {
        let mut term = term();
        let mut store = ImageStore::default();
        let data = b64(&raw(1, 1, 4));
        send(
            &mut term,
            &mut store,
            "a=T,f=32,s=1,v=1,i=1,c=2,r=2,C=1,q=2",
            &data,
        );
        send(&mut term, &mut store, "a=t,f=32,s=1,v=1,i=1,q=2", &data);
        assert_eq!(image_at(&term, 0, 0), None);
        assert!(store.kitty_ids(|k| k.image == 1).is_empty());
        assert!(store.kitty_source(1).is_some());
    }

    #[test]
    fn placing_a_missing_image_is_an_error() {
        let mut term = term();
        let mut store = ImageStore::default();
        let reply = send(&mut term, &mut store, "a=p,i=5", b"");
        assert_eq!(
            reply.as_deref(),
            Some("\x1b_Gi=5;ENOENT:no such image\x1b\\")
        );
        assert_eq!(send(&mut term, &mut store, "a=p", b""), None);
        assert!(store.is_empty());
    }

    #[test]
    fn decodes_raw_png_and_compressed_data() {
        let cmd = |control: &str| parse(control.as_bytes());

        let source = decode_source(&cmd("f=32,s=2,v=1"), &b64(&[1, 2, 3, 4, 5, 6, 7, 8])).unwrap();
        assert_eq!((source.width, source.height), (2, 1));
        assert_eq!(source.rgba.as_slice(), [1, 2, 3, 4, 5, 6, 7, 8]);

        let source = decode_source(&cmd("f=24,s=2,v=1"), &b64(&[1, 2, 3, 4, 5, 6])).unwrap();
        assert_eq!(source.rgba.as_slice(), [1, 2, 3, 255, 4, 5, 6, 255]);

        let source = decode_source(&cmd("f=100"), &b64(&png(3, 2))).unwrap();
        assert_eq!((source.width, source.height), (3, 2));
        assert_eq!(&source.rgba[..4], [0, 255, 0, 255]);

        let compressed = miniz_oxide::deflate::compress_to_vec_zlib(&raw(4, 4, 4), 6);
        let source = decode_source(&cmd("f=32,s=4,v=4,o=z"), &b64(&compressed)).unwrap();
        assert_eq!(source.rgba.len(), 64);
        let compressed = miniz_oxide::deflate::compress_to_vec_zlib(&png(2, 2), 6);
        let source = decode_source(&cmd("f=100,o=z"), &b64(&compressed)).unwrap();
        assert_eq!((source.width, source.height), (2, 2));

        // Without padding, as some programs send it.
        let data = b64(&[
            1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20,
        ]);
        let trimmed = data.strip_suffix(b"=").unwrap_or(&data);
        assert!(decode_source(&cmd("f=32,s=5,v=1"), trimmed).is_ok());
    }

    #[test]
    fn decoding_errors() {
        let cmd = |control: &str| parse(control.as_bytes());
        let err =
            |control: &str, payload: &[u8]| decode_source(&cmd(control), payload).unwrap_err();

        assert_eq!(err("f=32,s=2,v=2", &b64(&[0; 8])), "EINVAL:size mismatch");
        assert_eq!(err("f=24,s=2,v=2", &b64(&[0; 8])), "EINVAL:size mismatch");
        assert_eq!(err("f=32", &b64(&[0; 8])), "EINVAL:size mismatch");
        assert_eq!(err("f=100", &b64(b"not a png")), "EINVAL:bad image data");
        assert_eq!(err("f=32,s=1,v=1", b"!!!!"), "EINVAL:bad image data");
        assert_eq!(
            err("f=32,s=1,v=1,o=z", &b64(b"junk")),
            "EINVAL:bad image data"
        );
        assert_eq!(err("f=7,s=1,v=1", &b64(&[0; 4])), "EINVAL:bad image data");
        assert_eq!(
            err("f=32,s=1,v=1,t=f", &b64(&[0; 4])),
            "EINVAL:unsupported transmission medium"
        );
        assert_eq!(
            err("f=32,s=16385,v=1", &b64(&[0; 4])),
            "EFBIG:image too large"
        );
    }

    #[test]
    fn replies_and_quiet_levels() {
        let mut term = term();
        let mut store = ImageStore::default();
        let good = b64(&raw(1, 1, 4));
        let mut run = |control: &str, payload: &[u8]| send(&mut term, &mut store, control, payload);

        assert_eq!(
            run("a=t,f=32,s=1,v=1,i=3", &good).as_deref(),
            Some("\x1b_Gi=3;OK\x1b\\")
        );
        assert_eq!(run("a=t,f=32,s=1,v=1,i=3,q=1", &good), None);
        assert_eq!(
            run("a=t,f=32,s=1,v=1,i=3,p=4,q=1", b"!!!").as_deref(),
            Some("\x1b_Gi=3,p=4;EINVAL:bad image data\x1b\\")
        );
        assert_eq!(run("a=t,f=32,s=1,v=1,i=3,q=2", b"!!!"), None);
        // Without an id there is nobody to answer.
        assert_eq!(run("a=t,f=32,s=1,v=1", &good), None);
        assert_eq!(run("a=t,f=32,s=1,v=1", b"!!!"), None);
        // Queries answer without storing.
        assert_eq!(
            run("a=q,f=32,s=1,v=1,i=31", &good).as_deref(),
            Some("\x1b_Gi=31;OK\x1b\\")
        );
        assert_eq!(
            run("a=q,f=32,s=2,v=2,i=31", &good).as_deref(),
            Some("\x1b_Gi=31;EINVAL:size mismatch\x1b\\")
        );
        // Unsupported features.
        assert_eq!(
            run("a=T,U=1,f=32,s=1,v=1,i=8", &good).as_deref(),
            Some("\x1b_Gi=8;EINVAL:unsupported\x1b\\")
        );
        assert_eq!(
            run("a=f,i=9", b"").as_deref(),
            Some("\x1b_Gi=9;EINVAL:unsupported\x1b\\")
        );
        assert_eq!(
            run("a=t,f=32,s=1,v=1,i=10,t=f", &good).as_deref(),
            Some("\x1b_Gi=10;EINVAL:unsupported transmission medium\x1b\\")
        );
        drop(run);
        assert!(store.kitty_source(31).is_none());
        assert!(store.kitty_source(8).is_none());
    }

    #[test]
    fn chunks_make_one_transfer() {
        let data = b64(&raw(4, 4, 4));
        let (first, rest) = data.split_at(40);
        let (second, third) = rest.split_at(40);
        let mut transfers = Transfers::default();
        assert!(matches!(
            transfers.accept(b"a=T,f=32,s=4,v=4,i=1,c=2,r=1,m=1", first.to_vec()),
            Accepted::Pending
        ));
        assert!(matches!(
            transfers.accept(b"m=1", second.to_vec()),
            Accepted::Pending
        ));
        let Accepted::Complete(transfer) = transfers.accept(b"m=0", third.to_vec()) else {
            panic!("the last chunk completes");
        };
        assert_eq!(transfer.payload, data);
        assert_eq!(transfer.command.action, b'T');
        assert!(!transfer.command.more);
        let mut term = term();
        let mut store = ImageStore::default();
        let reply = finish(&mut term, &mut store, transfer);
        assert_eq!(reply.as_deref(), Some("\x1b_Gi=1;OK\x1b\\"));
        assert!(image_at(&term, 0, 1).is_some());

        // A command without chunks after a finished transfer is a new one.
        assert!(matches!(
            transfers.accept(b"a=d", Vec::new()),
            Accepted::Complete(_)
        ));
    }

    #[test]
    fn oversized_transfers_are_rejected_to_their_end() {
        let mut transfers = Transfers {
            limit: 8,
            ..Transfers::default()
        };
        assert!(matches!(
            transfers.accept(b"a=t,i=1,m=1,q=0", b"AAAAAAAA".to_vec()),
            Accepted::Pending
        ));
        let Accepted::Rejected(reply) = transfers.accept(b"m=1", b"AAAA".to_vec()) else {
            panic!("over the limit");
        };
        assert_eq!(
            reply.as_deref(),
            Some("\x1b_Gi=1;ENOMEM:image too large\x1b\\")
        );
        assert!(matches!(
            transfers.accept(b"m=1", b"AAAA".to_vec()),
            Accepted::Pending
        ));
        assert!(matches!(
            transfers.accept(b"m=0", b"AAAA".to_vec()),
            Accepted::Pending
        ));
        // The next command is taken again.
        assert!(matches!(
            transfers.accept(b"a=d", Vec::new()),
            Accepted::Complete(_)
        ));
    }

    #[test]
    fn sources_beyond_the_limit_are_dropped_oldest_first() {
        let mut store = ImageStore::default().with_kitty_limit(100);
        let source = |bytes: usize| Source::new(bytes as u32 / 4, 1, vec![0; bytes]);
        store.put_kitty_source(1, source(40));
        store.put_kitty_source(2, source(40));
        store.set_kitty_number(7, 1);
        store.put_kitty_source(3, source(40));
        assert!(store.kitty_source(1).is_none());
        assert_eq!(store.kitty_number(7), None);
        assert!(store.kitty_source(2).is_some());
        assert!(store.kitty_source(3).is_some());
        // Replacing counts the new data only.
        store.put_kitty_source(2, source(40));
        assert!(store.kitty_source(3).is_some());
        store.put_kitty_source(4, source(40));
        assert!(store.kitty_source(3).is_none());
        // A single source over the limit stays.
        store.put_kitty_source(5, source(400));
        assert!(store.kitty_source(5).is_some());
        assert!(store.kitty_source(4).is_none());
    }

    #[test]
    fn size_follows_cells_and_aspect_ratio() {
        // 20×10 px source at 10×20 px cells.
        let source = Source::new(20, 10, vec![255; 20 * 10 * 4]);
        let render = |control: &str| render(&source, &parse(control.as_bytes()), GEOMETRY).unwrap();

        // Both: stretched.
        let image = render("c=4,r=3");
        assert_eq!((image.columns, image.lines), (4, 3));
        assert_eq!((image.width, image.height), (40, 60));
        assert_eq!(image.rgba[3], 255);
        assert_eq!(image.rgba[image.rgba.len() - 1], 255);
        // Columns only: 4 columns = 40 px wide, 20 px tall, one line.
        let image = render("c=4");
        assert_eq!((image.columns, image.lines), (4, 1));
        // Rows only: 2 rows = 40 px tall, 80 px wide.
        let image = render("r=2");
        assert_eq!((image.columns, image.lines), (8, 2));
        // Neither: native size.
        let image = render("");
        assert_eq!((image.columns, image.lines), (2, 1));
        assert_eq!((image.width, image.height), (20, 20));
        assert_eq!(image.rgba[3], 255);
        assert_eq!(image.rgba[(19 * 20 * 4) + 3], 0, "below the 10 px image");
        // Offset moves the image inside its cells.
        let image = render("X=5,Y=10");
        assert_eq!((image.columns, image.lines), (3, 1));
        assert_eq!(image.rgba[3], 0);
        assert_eq!(image.rgba[((10 * image.width + 5) * 4 + 3) as usize], 255);
        // A source rectangle.
        let image = render("x=10,w=5,h=4");
        assert_eq!((image.columns, image.lines), (1, 1));
        assert!(render_error(&source, "x=30").is_some());
    }

    fn render_error(source: &Source, control: &str) -> Option<&'static str> {
        render(source, &parse(control.as_bytes()), GEOMETRY).err()
    }

    #[test]
    fn huge_placements_are_limited() {
        let source = Source::new(1, 1, vec![255; 4]);
        let image = render(&source, &parse(b"c=500,r=500"), GEOMETRY).unwrap();
        assert_eq!(image.columns, 500);
        assert!(image.width <= MAX_BITMAP_SIDE as u32 && image.height <= MAX_BITMAP_SIDE as u32);
        let image = render(&source, &parse(b"c=1,r=100000"), GEOMETRY).unwrap();
        assert_eq!(image.lines, 384);
    }

    #[test]
    fn unknown_cell_size_is_an_error() {
        let source = Source::new(1, 1, vec![255; 4]);
        let geometry = CellGeometry {
            cell_width: 0,
            ..GEOMETRY
        };
        assert!(render(&source, &Command::default(), geometry).is_err());
    }
}
