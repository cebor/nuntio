use alacritty_terminal::term::color::{COUNT, Colors};
use alacritty_terminal::vte::ansi::{Color, NamedColor, Rgb};

/// Full color table: 256 indexed colors plus the named special colors
/// (foreground, background, cursor, dim variants), indexed like
/// `alacritty_terminal::term::color::Colors`.
#[derive(Debug, Clone)]
pub struct Palette {
    colors: [Rgb; COUNT],
    pub selection_background: Rgb,
    pub selection_foreground: Rgb,
}

const fn rgb(hex: u32) -> Rgb {
    Rgb {
        r: (hex >> 16) as u8,
        g: (hex >> 8) as u8,
        b: hex as u8,
    }
}

/// iTerm2 "Default" preset, used until themes land.
const ITERM2_ANSI: [u32; 16] = [
    0x000000, 0xc91b00, 0x00c200, 0xc7c400, 0x0225c7, 0xc930c7, 0x00c5c7, 0xc7c7c7, //
    0x686868, 0xff6e67, 0x5ffa68, 0xfffc67, 0x6871ff, 0xff77ff, 0x60fdff, 0xffffff,
];
const ITERM2_FG: u32 = 0xc7c7c7;
const ITERM2_BG: u32 = 0x000000;
const ITERM2_SELECTION_BG: u32 = 0xb5d5ff;
const ITERM2_SELECTION_FG: u32 = 0x000000;

impl Default for Palette {
    fn default() -> Self {
        let ansi = ITERM2_ANSI.map(rgb);
        Self::new(&ansi, rgb(ITERM2_FG), rgb(ITERM2_BG), rgb(ITERM2_FG))
    }
}

impl Palette {
    pub fn new(ansi: &[Rgb; 16], foreground: Rgb, background: Rgb, cursor: Rgb) -> Self {
        let mut colors = [Rgb::default(); COUNT];
        colors[..16].copy_from_slice(ansi);

        // 6x6x6 color cube.
        let level = |v: usize| if v == 0 { 0 } else { (v * 40 + 55) as u8 };
        for i in 0..216 {
            colors[16 + i] = Rgb {
                r: level(i / 36),
                g: level(i / 6 % 6),
                b: level(i % 6),
            };
        }
        // Grayscale ramp.
        for i in 0..24 {
            let v = (i * 10 + 8) as u8;
            colors[232 + i] = Rgb { r: v, g: v, b: v };
        }

        colors[NamedColor::Foreground as usize] = foreground;
        colors[NamedColor::Background as usize] = background;
        colors[NamedColor::Cursor as usize] = cursor;
        colors[NamedColor::BrightForeground as usize] = foreground;
        colors[NamedColor::DimForeground as usize] = dim(foreground);
        for i in 0..8 {
            colors[NamedColor::DimBlack as usize + i] = dim(ansi[i]);
        }

        Self {
            colors,
            selection_background: rgb(ITERM2_SELECTION_BG),
            selection_foreground: rgb(ITERM2_SELECTION_FG),
        }
    }

    pub fn get(&self, index: usize) -> Rgb {
        self.colors[index]
    }

    pub fn foreground(&self) -> Rgb {
        self.colors[NamedColor::Foreground as usize]
    }

    pub fn background(&self) -> Rgb {
        self.colors[NamedColor::Background as usize]
    }

    pub fn cursor(&self) -> Rgb {
        self.colors[NamedColor::Cursor as usize]
    }

    /// Resolve a cell color, honoring runtime overrides (OSC 4/10/11/12).
    pub fn resolve(&self, color: Color, overrides: &Colors) -> Rgb {
        let index = match color {
            Color::Spec(rgb) => return rgb,
            Color::Named(named) => named as usize,
            Color::Indexed(i) => i as usize,
        };
        overrides[index].unwrap_or(self.colors[index])
    }
}

pub(crate) fn dim(c: Rgb) -> Rgb {
    let f = |v: u8| (v as u16 * 2 / 3) as u8;
    Rgb {
        r: f(c.r),
        g: f(c.g),
        b: f(c.b),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cube_and_grayscale_match_xterm() {
        let p = Palette::default();
        assert_eq!(p.get(16), rgb(0x000000));
        assert_eq!(p.get(21), rgb(0x0000ff));
        assert_eq!(p.get(196), rgb(0xff0000));
        assert_eq!(p.get(231), rgb(0xffffff));
        assert_eq!(p.get(232), rgb(0x080808));
        assert_eq!(p.get(255), rgb(0xeeeeee));
    }

    #[test]
    fn overrides_take_precedence() {
        let p = Palette::default();
        let mut overrides = Colors::default();
        overrides[1] = Some(rgb(0x123456));
        assert_eq!(p.resolve(Color::Indexed(1), &overrides), rgb(0x123456));
        assert_eq!(
            p.resolve(Color::Named(NamedColor::Red), &overrides),
            rgb(0x123456)
        );
        assert_eq!(p.resolve(Color::Indexed(2), &overrides), rgb(0x00c200));
        assert_eq!(
            p.resolve(Color::Spec(rgb(0xabcdef)), &overrides),
            rgb(0xabcdef)
        );
    }
}
