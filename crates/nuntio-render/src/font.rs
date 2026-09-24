use cosmic_text::{
    Attrs, Buffer, Family, FontSystem, Metrics, Shaping, Style, SwashCache, SwashContent, Weight,
    Wrap,
};

/// Terminal cell geometry in physical pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CellMetrics {
    pub width: u32,
    pub height: u32,
    /// Distance from the cell top to the baseline.
    pub baseline: u32,
    /// Underline top, relative to the cell top.
    pub underline_y: u32,
    pub stroke: u32,
    /// Strikeout top, relative to the cell top.
    pub strikeout_y: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FaceStyle {
    pub bold: bool,
    pub italic: bool,
}

/// A rasterized glyph, positioned relative to the cell's baseline origin.
pub struct RasterGlyph {
    pub left: i32,
    /// Distance from the baseline up to the bitmap's top edge.
    pub top: i32,
    pub width: u32,
    pub height: u32,
    /// Color bitmap (emoji): RGBA8. Otherwise an 8-bit coverage mask.
    pub color: bool,
    pub data: Vec<u8>,
}

/// Font discovery, fallback, shaping and rasterization.
pub struct Fonts {
    system: FontSystem,
    swash: SwashCache,
    buffer: Buffer,
    family: Option<String>,
    px_size: f32,
    metrics: CellMetrics,
}

/// Font sizes are given in points. macOS treats 1pt as one logical pixel,
/// elsewhere the usual 96 DPI convention applies.
fn points_to_pixels(points: f32, scale_factor: f64) -> f32 {
    let dpi_factor = if cfg!(target_os = "macos") {
        1.0
    } else {
        96.0 / 72.0
    };
    points * dpi_factor * scale_factor as f32
}

impl Fonts {
    pub fn new(family: Option<String>, size_points: f32, scale_factor: f64) -> Self {
        let mut system = FontSystem::new();
        let px_size = points_to_pixels(size_points, scale_factor);
        let buffer = new_buffer(&mut system, px_size);
        let mut fonts = Self {
            system,
            swash: SwashCache::new(),
            buffer,
            family,
            px_size,
            metrics: CellMetrics {
                width: 1,
                height: 1,
                baseline: 1,
                underline_y: 1,
                stroke: 1,
                strikeout_y: 1,
            },
        };
        fonts.metrics = fonts.measure();
        tracing::info!(px_size, metrics = ?fonts.metrics, "font loaded");
        fonts
    }

    /// Change font size or scale factor. Returns the new cell metrics.
    pub fn set_size(&mut self, size_points: f32, scale_factor: f64) -> CellMetrics {
        self.px_size = points_to_pixels(size_points, scale_factor);
        self.buffer = new_buffer(&mut self.system, self.px_size);
        self.metrics = self.measure();
        self.metrics
    }

    pub fn metrics(&self) -> CellMetrics {
        self.metrics
    }

    fn attrs(&self, style: FaceStyle) -> Attrs<'_> {
        let family = match &self.family {
            Some(name) => Family::Name(name),
            None => Family::Monospace,
        };
        Attrs::new()
            .family(family)
            .weight(if style.bold {
                Weight::BOLD
            } else {
                Weight::NORMAL
            })
            .style(if style.italic {
                Style::Italic
            } else {
                Style::Normal
            })
    }

    fn shape(&mut self, text: &str, style: FaceStyle) {
        let attrs = self.attrs(style);
        // `attrs` borrows `self.family`; clone it out so the buffer can be borrowed mutably.
        let attrs = cosmic_text::AttrsOwned::new(&attrs);
        self.buffer
            .set_text(text, &attrs.as_attrs(), Shaping::Advanced, None);
        self.buffer.shape_until_scroll(&mut self.system, false);
    }

    fn measure(&mut self) -> CellMetrics {
        self.shape(
            "M",
            FaceStyle {
                bold: false,
                italic: false,
            },
        );
        let run = self.buffer.layout_runs().next();
        let glyph = run.and_then(|run| run.glyphs.first());
        let (advance, font_id) = match glyph {
            Some(g) => (g.w, Some(g.font_id)),
            None => (self.px_size * 0.6, None),
        };

        let font = font_id.and_then(|id| self.system.get_font(id, Weight::NORMAL));
        let (ascent, descent, leading, underline, strikeout) = match &font {
            Some(font) => {
                let m = font.metrics();
                let scale = self.px_size / m.units_per_em as f32;
                (
                    m.ascent * scale,
                    -m.descent * scale,
                    m.leading * scale,
                    m.underline
                        .map(|d| (-d.offset * scale, d.thickness * scale)),
                    m.strikeout.map(|d| (d.offset * scale, d.thickness * scale)),
                )
            }
            None => (self.px_size * 0.8, self.px_size * 0.2, 0.0, None, None),
        };

        let baseline = (ascent + leading / 2.0).round().max(1.0);
        let height = (ascent + descent + leading).round().max(1.0);
        let (underline_offset, stroke) = underline.unwrap_or((descent / 2.0, 1.0));
        let (strikeout_offset, _) = strikeout.unwrap_or((ascent / 3.0, stroke));
        let stroke = stroke.round().max(1.0);

        CellMetrics {
            width: advance.round().max(1.0) as u32,
            height: height as u32,
            baseline: baseline as u32,
            underline_y: ((baseline + underline_offset).round() as u32).min(height as u32 - 1),
            stroke: stroke as u32,
            strikeout_y: (baseline - strikeout_offset).round().max(0.0) as u32,
        }
    }

    /// Shape one grapheme (with fallback) and rasterize its glyphs.
    pub fn rasterize(&mut self, text: &str, style: FaceStyle) -> Vec<RasterGlyph> {
        self.shape(text, style);
        let Some(run) = self.buffer.layout_runs().next() else {
            return Vec::new();
        };

        let mut glyphs = Vec::with_capacity(run.glyphs.len());
        for glyph in run.glyphs {
            let physical = glyph.physical((0.0, 0.0), 1.0);
            let Some(image) = self
                .swash
                .get_image_uncached(&mut self.system, physical.cache_key)
            else {
                continue;
            };
            let (width, height) = (image.placement.width, image.placement.height);
            if width == 0 || height == 0 {
                continue;
            }
            let (color, data) = match image.content {
                SwashContent::Mask => (false, image.data),
                SwashContent::Color => (true, image.data),
                // Subpixel masks are RGBA coverage; collapse to grayscale.
                SwashContent::SubpixelMask => (
                    false,
                    image
                        .data
                        .as_chunks::<4>()
                        .0
                        .iter()
                        .map(|px| px[1])
                        .collect(),
                ),
            };
            glyphs.push(RasterGlyph {
                left: physical.x + image.placement.left,
                top: image.placement.top - physical.y,
                width,
                height,
                color,
                data,
            });
        }
        glyphs
    }
}

fn new_buffer(system: &mut FontSystem, px_size: f32) -> Buffer {
    let mut buffer = Buffer::new(system, Metrics::new(px_size, px_size * 1.5));
    buffer.set_wrap(Wrap::None);
    buffer
}
