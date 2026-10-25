//! Fixtures the unit tests of this crate share: terminals, bytes fed to
//! them, and encoded images.

use std::io::Cursor;

use alacritty_terminal::event::VoidListener;
use alacritty_terminal::term::{Config, Term};
use alacritty_terminal::vte::ansi::Processor;
use base64::Engine;
use image::{ImageFormat, Rgba, RgbaImage};

use crate::TermSize;
use crate::image::CellGeometry;

/// A cell grid of 80×24 cells of 10×20 pixels.
pub(crate) const GEOMETRY: CellGeometry = CellGeometry {
    columns: 80,
    lines: 24,
    cell_width: 10,
    cell_height: 20,
};

/// An empty terminal of `columns`×`lines` cells.
pub(crate) fn term(columns: u16, lines: u16) -> Term<VoidListener> {
    let size = TermSize {
        columns,
        lines,
        cell_width: 1,
        cell_height: 1,
    };
    Term::new(Config::default(), &size, VoidListener)
}

/// What a program writing `bytes` to the terminal does to it. Escape
/// sequences must be complete within `bytes`.
pub(crate) fn feed(term: &mut Term<VoidListener>, bytes: impl AsRef<[u8]>) {
    let mut parser: Processor = Processor::new();
    parser.advance(term, bytes.as_ref());
}

/// `image` as a PNG file.
pub(crate) fn png_of(image: &RgbaImage) -> Vec<u8> {
    let mut bytes = Cursor::new(Vec::new());
    image.write_to(&mut bytes, ImageFormat::Png).unwrap();
    bytes.into_inner()
}

/// A `width`×`height` PNG file of green pixels.
pub(crate) fn png(width: u32, height: u32) -> Vec<u8> {
    png_of(&RgbaImage::from_pixel(
        width,
        height,
        Rgba([0, 255, 0, 255]),
    ))
}

/// `bytes` as the base64 payload of an image command.
pub(crate) fn b64(bytes: &[u8]) -> Vec<u8> {
    base64::engine::general_purpose::STANDARD
        .encode(bytes)
        .into_bytes()
}
