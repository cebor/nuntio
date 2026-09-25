use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer};

/// An sRGB color, written as `"#rrggbb"` in config and theme files.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Color {
    pub const fn from_hex(hex: u32) -> Self {
        Self {
            r: (hex >> 16) as u8,
            g: (hex >> 8) as u8,
            b: hex as u8,
        }
    }

    /// Perceived brightness, 0.0 (black) to 1.0 (white).
    pub fn luminance(self) -> f32 {
        (0.2126 * self.r as f32 + 0.7152 * self.g as f32 + 0.0722 * self.b as f32) / 255.0
    }
}

/// A color that isn't written as `"#rrggbb"`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid color {0:?}, expected \"#rrggbb\"")]
pub struct ColorError(pub String);

impl FromStr for Color {
    type Err = ColorError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let invalid = || ColorError(s.to_owned());
        let hex = s
            .strip_prefix('#')
            .filter(|h| h.len() == 6 && h.chars().all(|c| c.is_ascii_hexdigit()))
            .ok_or_else(invalid)?;
        let value = u32::from_str_radix(hex, 16).map_err(|_| invalid())?;
        Ok(Self::from_hex(value))
    }
}

impl fmt::Display for Color {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
    }
}

impl<'de> Deserialize<'de> for Color {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_display() {
        let c: Color = "#1A2b3c".parse().unwrap();
        assert_eq!(c, Color::from_hex(0x1a2b3c));
        assert_eq!(c.to_string(), "#1a2b3c");
        assert!("1a2b3c".parse::<Color>().is_err());
        assert!("#12345".parse::<Color>().is_err());
        assert!("#12345g".parse::<Color>().is_err());
    }
}
