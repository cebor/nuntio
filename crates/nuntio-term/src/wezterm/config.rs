//! The settings wezterm-term reads from its embedder, changeable while
//! the pane runs.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{PoisonError, RwLock};

use alacritty_terminal::vte::ansi::Rgb;
use wezterm_term::color::{ColorPalette, SrgbaTuple};
use wezterm_term::{TerminalConfiguration, UnicodeVersion};

use crate::palette::Palette;
use crate::pane::TermOptions;

#[derive(Debug)]
pub(crate) struct Config {
    palette: RwLock<ColorPalette>,
    scrollback: AtomicUsize,
    kitty_keyboard: AtomicBool,
    /// Bumped with every change, so wezterm-term drops its caches.
    generation: AtomicUsize,
}

impl Config {
    pub(crate) fn new(options: TermOptions, palette: &Palette) -> Self {
        Self {
            palette: RwLock::new(color_palette_from(palette)),
            scrollback: AtomicUsize::new(options.scrollback),
            kitty_keyboard: AtomicBool::new(options.kitty_keyboard),
            generation: AtomicUsize::new(0),
        }
    }

    pub(crate) fn update(&self, options: TermOptions) {
        self.scrollback.store(options.scrollback, Ordering::Relaxed);
        self.kitty_keyboard
            .store(options.kitty_keyboard, Ordering::Relaxed);
        self.generation.fetch_add(1, Ordering::AcqRel);
    }

    pub(crate) fn set_palette(&self, palette: &Palette) {
        *self.palette.write().unwrap_or_else(PoisonError::into_inner) = color_palette_from(palette);
        self.generation.fetch_add(1, Ordering::AcqRel);
    }

    pub(crate) fn kitty_keyboard(&self) -> bool {
        self.kitty_keyboard.load(Ordering::Relaxed)
    }
}

impl TerminalConfiguration for Config {
    fn generation(&self) -> usize {
        self.generation.load(Ordering::Acquire)
    }

    fn scrollback_size(&self) -> usize {
        self.scrollback.load(Ordering::Relaxed)
    }

    fn color_palette(&self) -> ColorPalette {
        self.palette
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn enable_kitty_keyboard(&self) -> bool {
        self.kitty_keyboard()
    }

    /// Current widths, as alacritty takes them from `unicode-width`, rather
    /// than wezterm's default of Unicode 9.
    fn unicode_version(&self) -> UnicodeVersion {
        UnicodeVersion {
            version: 14,
            ambiguous_are_wide: false,
            cell_widths: None,
        }
    }
}

/// nuntio's palette in wezterm's terms.
fn color_palette_from(palette: &Palette) -> ColorPalette {
    let color = |c: Rgb| SrgbaTuple::from((c.r, c.g, c.b));
    let defaults = ColorPalette::default();
    let mut colors = defaults.colors.clone();
    for (i, slot) in colors.0.iter_mut().enumerate() {
        *slot = color(palette.get(i));
    }
    ColorPalette {
        colors,
        foreground: color(palette.foreground()),
        background: color(palette.background()),
        cursor_fg: color(palette.background()),
        cursor_bg: color(palette.cursor()),
        cursor_border: color(palette.cursor()),
        selection_fg: color(palette.selection_foreground),
        selection_bg: color(palette.selection_background),
        scrollbar_thumb: defaults.scrollbar_thumb,
        split: defaults.split,
    }
}

/// A wezterm color as 8-bit RGB. Rounded: `SrgbaTuple::to_srgb_u8`
/// truncates, which can turn a round-tripped 8-bit value into its
/// neighbour.
pub(crate) fn rgb(c: SrgbaTuple) -> Rgb {
    let channel = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    Rgb {
        r: channel(c.0),
        g: channel(c.1),
        b: channel(c.2),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_8_bit_value_survives_the_round_trip() {
        for v in 0..=255u8 {
            let back = rgb(SrgbaTuple::from((v, v, v)));
            assert_eq!(back, Rgb { r: v, g: v, b: v });
        }
    }
}
