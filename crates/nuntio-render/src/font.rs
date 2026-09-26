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
    /// Shapes the smaller UI text (see [`SMALL_TEXT_SCALE`]).
    small_buffer: Buffer,
    family: Option<String>,
    px_size: f32,
    metrics: CellMetrics,
    small_metrics: CellMetrics,
}

/// The font database being loaded by `preload_fonts`.
static PRELOADED: std::sync::Mutex<Option<std::thread::JoinHandle<FontSystem>>> =
    std::sync::Mutex::new(None);

/// Start loading the installed fonts on another thread, so that it runs
/// while the window and the GPU are set up. Scanning the fonts takes
/// tens of milliseconds (hundreds of faces on macOS).
pub fn preload_fonts() {
    let spawned = std::thread::Builder::new()
        .name("font preload".into())
        .spawn(FontSystem::new);
    match spawned {
        Ok(handle) => *PRELOADED.lock().unwrap_or_else(|e| e.into_inner()) = Some(handle),
        Err(err) => tracing::debug!("fonts load on the main thread: {err}"),
    }
}

/// The preloaded font database, or a new one.
fn font_system() -> FontSystem {
    let preloaded = PRELOADED.lock().unwrap_or_else(|e| e.into_inner()).take();
    preloaded
        .and_then(|handle| handle.join().ok())
        .unwrap_or_else(FontSystem::new)
}

/// Size of small UI text (the status bar) relative to the terminal font.
pub const SMALL_TEXT_SCALE: f32 = 0.9;

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

/// Result of looking up a configured font family among installed fonts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FamilyMatch {
    /// Found, ignoring case and spacing; carries the installed name.
    Exact(String),
    /// Same words in a different order ("Nerd Font Hack Mono").
    Reordered(String),
    NotFound {
        suggestions: Vec<String>,
    },
}

fn normalized(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn words(name: &str) -> Vec<String> {
    let mut words: Vec<String> = name.split_whitespace().map(str::to_lowercase).collect();
    words.sort();
    words
}

/// Find `requested` among installed family names.
pub fn resolve_family<'a>(
    installed: impl IntoIterator<Item = &'a str>,
    requested: &str,
) -> FamilyMatch {
    let mut families: Vec<&str> = installed.into_iter().collect();
    families.sort_unstable();
    families.dedup();

    let wanted = normalized(requested);
    if let Some(name) = families.iter().find(|f| normalized(f) == wanted) {
        return FamilyMatch::Exact(name.to_string());
    }
    let wanted_words = words(requested);
    if let Some(name) = families.iter().find(|f| words(f) == wanted_words) {
        return FamilyMatch::Reordered(name.to_string());
    }

    // Suggest the families sharing the most words with the request.
    let score = |family: &str| {
        let family_words = words(family);
        wanted_words
            .iter()
            .filter(|w| family_words.contains(w))
            .count()
    };
    // Only suggest families sharing more than half of the requested words.
    let best = families.iter().map(|f| score(f)).max().unwrap_or(0);
    let suggestions = if best * 2 <= wanted_words.len() {
        Vec::new()
    } else {
        families
            .iter()
            .filter(|f| score(f) == best)
            .take(3)
            .map(|f| f.to_string())
            .collect()
    };
    FamilyMatch::NotFound { suggestions }
}

/// Monospace families to use when no family is configured, in order of
/// preference. cosmic-text defaults to "Noto Sans Mono", and when that isn't
/// installed it takes whichever monospace face comes first, which on macOS is
/// an italic one.
const DEFAULT_MONOSPACE: &[&str] = if cfg!(target_os = "macos") {
    &["Menlo", "SF Mono", "Monaco"]
} else if cfg!(windows) {
    &["Cascadia Mono", "Consolas", "Courier New"]
} else {
    &[
        "Noto Sans Mono",
        "DejaVu Sans Mono",
        "Liberation Mono",
        "Ubuntu Mono",
    ]
};

/// The first of `candidates` that is installed.
fn default_monospace<'a>(
    installed: impl IntoIterator<Item = &'a str>,
    candidates: &[&'static str],
) -> Option<&'static str> {
    let installed: Vec<&str> = installed.into_iter().collect();
    candidates
        .iter()
        .find(|c| installed.iter().any(|f| f.eq_ignore_ascii_case(c)))
        .copied()
}

impl Fonts {
    /// Load fonts. Returns a warning if the requested family isn't installed.
    pub fn new(
        family: Option<String>,
        size_points: f32,
        scale_factor: f64,
    ) -> (Self, Option<String>) {
        let mut system = font_system();
        let installed = system
            .db()
            .faces()
            .flat_map(|face| face.families.iter().map(|(name, _)| name.as_str()));
        if let Some(name) = default_monospace(installed, DEFAULT_MONOSPACE) {
            system.db_mut().set_monospace_family(name);
        }
        // cosmic-text finds no match for fontdb's default sans-serif family
        // on macOS and logs a miss on every fallback lookup.
        if cfg!(target_os = "macos") {
            system.db_mut().set_sans_serif_family("Helvetica");
        }
        let px_size = points_to_pixels(size_points, scale_factor);
        let buffer = new_buffer(&mut system, px_size);
        let small_buffer = new_buffer(&mut system, px_size * SMALL_TEXT_SCALE);
        let placeholder = CellMetrics {
            width: 1,
            height: 1,
            baseline: 1,
            underline_y: 1,
            stroke: 1,
            strikeout_y: 1,
        };
        let mut fonts = Self {
            system,
            swash: SwashCache::new(),
            buffer,
            small_buffer,
            family: None,
            px_size,
            metrics: placeholder,
            small_metrics: placeholder,
        };
        let warning = fonts.set_family(family);
        (fonts, warning)
    }

    /// Switch the font family (`None` = system monospace). Returns a warning
    /// if the family isn't installed; the system monospace font is used then.
    pub fn set_family(&mut self, family: Option<String>) -> Option<String> {
        let (resolved, warning) = match family {
            None => (None, None),
            Some(requested) => {
                let installed = self
                    .system
                    .db()
                    .faces()
                    .flat_map(|face| face.families.iter().map(|(name, _)| name.as_str()));
                match resolve_family(installed, &requested) {
                    FamilyMatch::Exact(name) => (Some(name), None),
                    FamilyMatch::Reordered(name) => {
                        let warning =
                            format!("font \"{requested}\" found as \"{name}\"; use that name");
                        (Some(name), Some(warning))
                    }
                    FamilyMatch::NotFound { suggestions } => {
                        let mut warning = format!(
                            "font \"{requested}\" is not installed, using the default monospace font"
                        );
                        if !suggestions.is_empty() {
                            warning +=
                                &format!(" (did you mean \"{}\"?)", suggestions.join("\", \""));
                        }
                        (None, Some(warning))
                    }
                }
            }
        };
        self.family = resolved;
        self.remeasure();
        tracing::info!(family = ?self.family, px_size = self.px_size, metrics = ?self.metrics, "font loaded");
        warning
    }

    /// Change font size or scale factor. Returns the new cell metrics.
    pub fn set_size(&mut self, size_points: f32, scale_factor: f64) -> CellMetrics {
        self.px_size = points_to_pixels(size_points, scale_factor);
        self.remeasure();
        self.metrics
    }

    /// Rebuild both buffers and their metrics after a size or family change.
    fn remeasure(&mut self) {
        self.buffer = new_buffer(&mut self.system, self.px_size);
        self.small_buffer = new_buffer(&mut self.system, self.px_size * SMALL_TEXT_SCALE);
        self.metrics = self.measure(false);
        self.small_metrics = self.measure(true);
    }

    pub fn metrics(&self) -> CellMetrics {
        self.metrics
    }

    /// Cell metrics of small UI text.
    pub fn small_metrics(&self) -> CellMetrics {
        self.small_metrics
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

    fn buffer(&self, small: bool) -> &Buffer {
        if small {
            &self.small_buffer
        } else {
            &self.buffer
        }
    }

    fn shape(&mut self, text: &str, style: FaceStyle, small: bool) {
        let attrs = self.attrs(style);
        // `attrs` borrows `self.family`; clone it out so the buffer can be borrowed mutably.
        let attrs = cosmic_text::AttrsOwned::new(&attrs);
        let buffer = if small {
            &mut self.small_buffer
        } else {
            &mut self.buffer
        };
        buffer.set_text(text, &attrs.as_attrs(), Shaping::Advanced, None);
        buffer.shape_until_scroll(&mut self.system, false);
    }

    fn measure(&mut self, small: bool) -> CellMetrics {
        self.shape(
            "M",
            FaceStyle {
                bold: false,
                italic: false,
            },
            small,
        );
        let px_size = if small {
            self.px_size * SMALL_TEXT_SCALE
        } else {
            self.px_size
        };
        let run = self.buffer(small).layout_runs().next();
        let glyph = run.and_then(|run| run.glyphs.first());
        let (advance, font_id) = match glyph {
            Some(g) => (g.w, Some(g.font_id)),
            None => (px_size * 0.6, None),
        };

        let font = font_id.and_then(|id| self.system.get_font(id, Weight::NORMAL));
        let (ascent, descent, leading, underline, strikeout) = match &font {
            Some(font) => {
                let m = font.metrics();
                let scale = px_size / m.units_per_em as f32;
                (
                    m.ascent * scale,
                    -m.descent * scale,
                    m.leading * scale,
                    m.underline
                        .map(|d| (-d.offset * scale, d.thickness * scale)),
                    m.strikeout.map(|d| (d.offset * scale, d.thickness * scale)),
                )
            }
            None => (px_size * 0.8, px_size * 0.2, 0.0, None, None),
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
    pub fn rasterize(&mut self, text: &str, style: FaceStyle, small: bool) -> Vec<RasterGlyph> {
        self.shape(text, style, small);
        let buffer = if small {
            &self.small_buffer
        } else {
            &self.buffer
        };
        let Some(run) = buffer.layout_runs().next() else {
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

#[cfg(test)]
mod tests {
    use super::*;

    const INSTALLED: [&str; 5] = [
        "DejaVu Sans Mono",
        "Hack Nerd Font",
        "Hack Nerd Font Mono",
        "Hack Nerd Font Propo",
        "Liberation Mono",
    ];

    #[test]
    fn default_monospace_takes_the_first_installed_candidate() {
        let candidates = ["Menlo", "DejaVu Sans Mono", "Liberation Mono"];
        assert_eq!(
            default_monospace(INSTALLED, &candidates),
            Some("DejaVu Sans Mono")
        );
        assert_eq!(default_monospace(INSTALLED, &["Menlo"]), None);
    }

    #[test]
    fn exact_ignores_case_and_spacing() {
        assert_eq!(
            resolve_family(INSTALLED, "hack nerd font mono"),
            FamilyMatch::Exact("Hack Nerd Font Mono".into())
        );
        assert_eq!(
            resolve_family(INSTALLED, "DejaVuSansMono"),
            FamilyMatch::Exact("DejaVu Sans Mono".into())
        );
    }

    #[test]
    fn reordered_words() {
        assert_eq!(
            resolve_family(INSTALLED, "Nerd Font Hack Mono"),
            FamilyMatch::Reordered("Hack Nerd Font Mono".into())
        );
    }

    #[test]
    fn suggestions_for_unknown_families() {
        assert_eq!(
            resolve_family(INSTALLED, "Hack Mono"),
            FamilyMatch::NotFound {
                suggestions: vec!["Hack Nerd Font Mono".into()]
            }
        );
        assert_eq!(
            resolve_family(INSTALLED, "Comic Sans"),
            FamilyMatch::NotFound {
                suggestions: vec![]
            }
        );
    }
}
