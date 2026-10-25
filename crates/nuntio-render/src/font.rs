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

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub(crate) struct FaceStyle {
    pub(crate) bold: bool,
    pub(crate) italic: bool,
}

impl FaceStyle {
    /// Number of different styles, the range of [`slot`](Self::slot).
    pub(crate) const COUNT: usize = 4;

    /// Index into the per-style state of the glyph caches.
    pub(crate) const fn slot(self) -> usize {
        self.bold as usize * 2 + self.italic as usize
    }
}

/// A rasterized glyph, positioned relative to the cell's baseline origin.
pub(crate) struct RasterGlyph {
    pub(crate) left: i32,
    /// Distance from the baseline up to the bitmap's top edge.
    pub(crate) top: i32,
    pub(crate) width: u32,
    pub(crate) height: u32,
    /// Color bitmap (emoji): RGBA8. Otherwise an 8-bit coverage mask.
    pub(crate) color: bool,
    pub(crate) data: Vec<u8>,
}

/// Which of the two font sizes text is drawn in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum TextSize {
    /// The terminal font.
    Terminal,
    /// The smaller UI text (see [`SMALL_TEXT_SCALE`]).
    Small,
}

impl TextSize {
    pub(crate) const ALL: [Self; 2] = [Self::Terminal, Self::Small];
    pub(crate) const COUNT: usize = Self::ALL.len();

    /// Index into the per-size state of [`Fonts`] and the glyph caches.
    pub(crate) const fn slot(self) -> usize {
        match self {
            Self::Terminal => 0,
            Self::Small => 1,
        }
    }

    fn scale(self) -> f32 {
        match self {
            Self::Terminal => 1.0,
            Self::Small => SMALL_TEXT_SCALE,
        }
    }
}

/// Shaping state and cell metrics of one [`TextSize`].
struct Face {
    buffer: Buffer,
    metrics: CellMetrics,
}

/// Font discovery, fallback, shaping and rasterization.
pub(crate) struct Fonts {
    system: FontSystem,
    swash: SwashCache,
    /// Indexed by [`TextSize::slot`].
    faces: [Face; TextSize::COUNT],
    family: Option<String>,
    /// Monochrome symbol font for text-presentation emoji (see
    /// [`prefers_text_presentation`]).
    symbol_family: Option<&'static str>,
    px_size: f32,
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
    match preloaded.map(std::thread::JoinHandle::join) {
        Some(Ok(system)) => system,
        Some(Err(_)) => {
            tracing::warn!("font preload thread panicked, loading fonts again");
            FontSystem::new()
        }
        None => FontSystem::new(),
    }
}

/// Size of small UI text (the status bar) relative to the terminal font.
pub(crate) const SMALL_TEXT_SCALE: f32 = 0.9;

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
pub(crate) enum FamilyMatch {
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
pub(crate) fn resolve_family<'a>(
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

/// Monochrome fonts with symbols that the platform's fallback list would
/// otherwise take from the color emoji font, like ✳ (U+2733) from Segoe UI
/// Emoji on Windows.
const TEXT_SYMBOL_FAMILIES: &[&str] = if cfg!(target_os = "macos") {
    &["Apple Symbols"]
} else if cfg!(windows) {
    &["Segoe UI Symbol"]
} else {
    &["DejaVu Sans", "Noto Sans Symbols 2", "Noto Sans Symbols"]
};

/// Characters below U+1F000 that default to emoji presentation
/// (`Emoji_Presentation=Yes` in Unicode's emoji-data.txt).
const EMOJI_PRESENTATION: &[(char, char)] = &[
    ('\u{231A}', '\u{231B}'),
    ('\u{23E9}', '\u{23EC}'),
    ('\u{23F0}', '\u{23F0}'),
    ('\u{23F3}', '\u{23F3}'),
    ('\u{25FD}', '\u{25FE}'),
    ('\u{2614}', '\u{2615}'),
    ('\u{2648}', '\u{2653}'),
    ('\u{267F}', '\u{267F}'),
    ('\u{2693}', '\u{2693}'),
    ('\u{26A1}', '\u{26A1}'),
    ('\u{26AA}', '\u{26AB}'),
    ('\u{26BD}', '\u{26BE}'),
    ('\u{26C4}', '\u{26C5}'),
    ('\u{26CE}', '\u{26CE}'),
    ('\u{26D4}', '\u{26D4}'),
    ('\u{26EA}', '\u{26EA}'),
    ('\u{26F2}', '\u{26F3}'),
    ('\u{26F5}', '\u{26F5}'),
    ('\u{26FA}', '\u{26FA}'),
    ('\u{26FD}', '\u{26FD}'),
    ('\u{2705}', '\u{2705}'),
    ('\u{270A}', '\u{270B}'),
    ('\u{2728}', '\u{2728}'),
    ('\u{274C}', '\u{274C}'),
    ('\u{274E}', '\u{274E}'),
    ('\u{2753}', '\u{2755}'),
    ('\u{2757}', '\u{2757}'),
    ('\u{2795}', '\u{2797}'),
    ('\u{27B0}', '\u{27B0}'),
    ('\u{27BF}', '\u{27BF}'),
    ('\u{2B1B}', '\u{2B1C}'),
    ('\u{2B50}', '\u{2B50}'),
    ('\u{2B55}', '\u{2B55}'),
];

/// Whether a grapheme should be drawn as a monochrome symbol rather than a
/// color emoji. Symbols like ✳ default to text presentation unless followed
/// by VS16 (U+FE0F). Everything from U+1F000 up is treated as emoji.
fn prefers_text_presentation(grapheme: &str) -> bool {
    if grapheme.contains('\u{FE0E}') {
        return true;
    }
    if grapheme.contains('\u{FE0F}') {
        return false;
    }
    let Some(c) = grapheme.chars().next() else {
        return true;
    };
    if c >= '\u{1F000}' {
        return false;
    }
    EMOJI_PRESENTATION
        .binary_search_by(|&(lo, hi)| {
            if hi < c {
                std::cmp::Ordering::Less
            } else if lo > c {
                std::cmp::Ordering::Greater
            } else {
                std::cmp::Ordering::Equal
            }
        })
        .is_err()
}

/// The first of `candidates` that is installed.
fn first_installed<'a>(
    installed: impl IntoIterator<Item = &'a str>,
    candidates: &[&'static str],
) -> Option<&'static str> {
    let installed: Vec<&str> = installed.into_iter().collect();
    candidates
        .iter()
        .find(|c| installed.iter().any(|f| f.eq_ignore_ascii_case(c)))
        .copied()
}

/// The system has no fonts at all.
#[derive(Debug, thiserror::Error)]
#[error("no fonts installed: install a font package such as fonts-dejavu")]
pub struct NoFontsError;

impl Fonts {
    /// Load fonts. Returns a warning if the requested family isn't installed.
    pub(crate) fn new(
        family: Option<String>,
        size_points: f32,
        scale_factor: f64,
    ) -> Result<(Self, Option<String>), NoFontsError> {
        Self::with_system(font_system(), family, size_points, scale_factor)
    }

    fn with_system(
        mut system: FontSystem,
        family: Option<String>,
        size_points: f32,
        scale_factor: f64,
    ) -> Result<(Self, Option<String>), NoFontsError> {
        if system.db().faces().next().is_none() {
            return Err(NoFontsError);
        }
        if let Some(name) = first_installed(family_names(&system), DEFAULT_MONOSPACE) {
            system.db_mut().set_monospace_family(name);
        }
        let symbol_family = first_installed(family_names(&system), TEXT_SYMBOL_FAMILIES);
        // cosmic-text finds no match for fontdb's default sans-serif family
        // on macOS and logs a miss on every fallback lookup.
        if cfg!(target_os = "macos") {
            system.db_mut().set_sans_serif_family("Helvetica");
        }
        let px_size = points_to_pixels(size_points, scale_factor);
        let placeholder = CellMetrics {
            width: 1,
            height: 1,
            baseline: 1,
            underline_y: 1,
            stroke: 1,
            strikeout_y: 1,
        };
        let faces = TextSize::ALL.map(|size| Face {
            buffer: new_buffer(&mut system, px_size * size.scale()),
            metrics: placeholder,
        });
        let mut fonts = Self {
            system,
            swash: SwashCache::new(),
            faces,
            family: None,
            symbol_family,
            px_size,
        };
        let warning = fonts.set_family(family);
        Ok((fonts, warning))
    }

    /// Switch the font family (`None` = system monospace). Returns a warning
    /// if the family isn't installed; the system monospace font is used then.
    pub(crate) fn set_family(&mut self, family: Option<String>) -> Option<String> {
        let (resolved, warning) = match family {
            None => (None, None),
            Some(requested) => match resolve_family(family_names(&self.system), &requested) {
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
                        warning += &format!(" (did you mean \"{}\"?)", suggestions.join("\", \""));
                    }
                    (None, Some(warning))
                }
            },
        };
        self.family = resolved;
        self.remeasure();
        tracing::info!(family = ?self.family, px_size = self.px_size, metrics = ?self.metrics(TextSize::Terminal), "font loaded");
        warning
    }

    /// Change font size or scale factor. Returns the new terminal cell metrics.
    pub(crate) fn set_size(&mut self, size_points: f32, scale_factor: f64) -> CellMetrics {
        self.px_size = points_to_pixels(size_points, scale_factor);
        self.remeasure();
        self.metrics(TextSize::Terminal)
    }

    /// Rebuild the buffers and metrics of both sizes after a size or family change.
    fn remeasure(&mut self) {
        for size in TextSize::ALL {
            self.faces[size.slot()].buffer =
                new_buffer(&mut self.system, self.px_size * size.scale());
        }
        for size in TextSize::ALL {
            self.faces[size.slot()].metrics = self.measure(size);
        }
    }

    pub(crate) fn metrics(&self, size: TextSize) -> CellMetrics {
        self.faces[size.slot()].metrics
    }

    fn attrs(&self, style: FaceStyle, symbols: bool) -> Attrs<'_> {
        let family = match (&self.family, self.symbol_family) {
            (_, Some(name)) if symbols => Family::Name(name),
            (Some(name), _) => Family::Name(name),
            (None, _) => Family::Monospace,
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

    /// Shape `text`; `symbols` puts the monochrome symbol font first.
    fn shape(&mut self, text: &str, style: FaceStyle, size: TextSize, symbols: bool) {
        let attrs = self.attrs(style, symbols);
        // `attrs` borrows `self.family`; clone it out so the buffer can be borrowed mutably.
        let attrs = cosmic_text::AttrsOwned::new(&attrs);
        let buffer = &mut self.faces[size.slot()].buffer;
        buffer.set_text(text, &attrs.as_attrs(), Shaping::Advanced, None);
        buffer.shape_until_scroll(&mut self.system, false);
    }

    fn measure(&mut self, size: TextSize) -> CellMetrics {
        self.shape("M", FaceStyle::default(), size, false);
        let px_size = self.px_size * size.scale();
        let run = self.faces[size.slot()].buffer.layout_runs().next();
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
            underline_y: ((baseline + underline_offset).round() as u32)
                .min((height as u32).saturating_sub(stroke as u32)),
            stroke: stroke as u32,
            strikeout_y: ((baseline - strikeout_offset).round().max(0.0) as u32)
                .min((height as u32).saturating_sub(stroke as u32)),
        }
    }

    /// Shape one grapheme (with fallback) and rasterize its glyphs.
    pub(crate) fn rasterize(
        &mut self,
        text: &str,
        style: FaceStyle,
        size: TextSize,
    ) -> Vec<RasterGlyph> {
        let glyphs = self.rasterize_with(text, style, size, false);
        // The fallback list may prefer the color emoji font for symbols
        // that default to text presentation; take them from a symbol font.
        if glyphs.iter().any(|g| g.color)
            && self.symbol_family.is_some()
            && prefers_text_presentation(text)
        {
            let symbols = self.rasterize_with(text, style, size, true);
            if !symbols.is_empty() && !symbols.iter().any(|g| g.color) {
                return symbols;
            }
        }
        glyphs
    }

    fn rasterize_with(
        &mut self,
        text: &str,
        style: FaceStyle,
        size: TextSize,
        symbols: bool,
    ) -> Vec<RasterGlyph> {
        self.shape(text, style, size, symbols);
        let buffer = &self.faces[size.slot()].buffer;
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

/// The names of the installed font families; a family repeats for each of its faces.
fn family_names(system: &FontSystem) -> impl Iterator<Item = &str> {
    system
        .db()
        .faces()
        .flat_map(|face| face.families.iter().map(|(name, _)| name.as_str()))
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
    fn an_empty_font_database_is_an_error() {
        let system = FontSystem::new_with_locale_and_db(
            "en-US".into(),
            cosmic_text::fontdb::Database::new(),
        );
        let Err(err) = Fonts::with_system(system, None, 12.0, 1.0) else {
            panic!("expected an error");
        };
        assert_eq!(
            err.to_string(),
            "no fonts installed: install a font package such as fonts-dejavu"
        );
    }

    #[test]
    fn first_installed_takes_the_first_installed_candidate() {
        let candidates = ["Menlo", "DejaVu Sans Mono", "Liberation Mono"];
        assert_eq!(
            first_installed(INSTALLED, &candidates),
            Some("DejaVu Sans Mono")
        );
        assert_eq!(first_installed(INSTALLED, &["Menlo"]), None);
    }

    #[test]
    fn text_presentation() {
        assert!(prefers_text_presentation("\u{2733}"));
        assert!(prefers_text_presentation("\u{2764}"));
        assert!(prefers_text_presentation("\u{2705}\u{FE0E}"));
        assert!(prefers_text_presentation("a"));
        assert!(!prefers_text_presentation("\u{2733}\u{FE0F}"));
        assert!(!prefers_text_presentation("\u{2705}"));
        assert!(!prefers_text_presentation("\u{2B50}"));
        assert!(!prefers_text_presentation("\u{231A}"));
        assert!(!prefers_text_presentation("\u{1F600}"));
    }

    #[test]
    fn emoji_presentation_ranges_are_sorted() {
        assert!(EMOJI_PRESENTATION.iter().all(|(lo, hi)| lo <= hi));
        assert!(EMOJI_PRESENTATION.windows(2).all(|w| w[0].1 < w[1].0));
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
