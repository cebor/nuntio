//! Color themes: built-in ones, user TOML files and iTerm2 `.itermcolors`
//! files from `<config dir>/themes/`.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Deserialize;

use crate::color::Color;

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Theme {
    /// Display name; user theme files default to their file name.
    #[serde(default)]
    pub name: String,
    pub foreground: Color,
    pub background: Color,
    pub cursor: Color,
    pub selection_foreground: Color,
    pub selection_background: Color,
    /// ANSI colors 0–7: black, red, green, yellow, blue, magenta, cyan, white.
    pub normal: [Color; 8],
    /// ANSI colors 8–15, the bright variants.
    pub bright: [Color; 8],
}

impl Theme {
    /// All 16 ANSI colors.
    pub fn ansi(&self) -> [Color; 16] {
        let mut ansi = [Color::default(); 16];
        ansi[..8].copy_from_slice(&self.normal);
        ansi[8..].copy_from_slice(&self.bright);
        ansi
    }

    pub fn is_dark(&self) -> bool {
        self.background.luminance() < 0.5
    }
}

const BUILTIN: [&str; 5] = [
    include_str!("../themes/iterm2-default.toml"),
    include_str!("../themes/solarized-dark.toml"),
    include_str!("../themes/solarized-light.toml"),
    include_str!("../themes/dracula.toml"),
    include_str!("../themes/tokyo-night.toml"),
];

pub const DEFAULT_THEME: &str = "iTerm2 Default";

pub fn builtin_themes() -> Vec<Theme> {
    BUILTIN
        .iter()
        .map(|src| toml::from_str(src).expect("built-in themes are valid"))
        .collect()
}

/// Built-in and user themes, looked up by name (case-insensitive).
#[derive(Debug, Clone)]
pub struct ThemeSet {
    themes: BTreeMap<String, Theme>,
}

impl ThemeSet {
    /// Built-in themes plus the files in `dir`; user themes override
    /// built-in ones of the same name. Problems are returned as warnings.
    pub fn load(dir: Option<&Path>) -> (Self, Vec<String>) {
        let mut set = Self {
            themes: BTreeMap::new(),
        };
        for theme in builtin_themes() {
            set.insert(theme);
        }
        let mut warnings = Vec::new();
        let Some(entries) = dir.and_then(|d| std::fs::read_dir(d).ok()) else {
            return (set, warnings);
        };

        let mut paths: Vec<_> = entries.flatten().map(|e| e.path()).collect();
        paths.sort();
        for path in paths {
            let result = match path.extension().and_then(|e| e.to_str()) {
                Some("toml") => load_toml(&path),
                Some("itermcolors") => load_itermcolors(&path),
                _ => continue,
            };
            match result {
                Ok(theme) => set.insert(theme),
                Err(err) => warnings.push(format!("theme {}: {err}", path.display())),
            }
        }
        (set, warnings)
    }

    fn insert(&mut self, theme: Theme) {
        self.themes.insert(theme.name.to_lowercase(), theme);
    }

    pub fn get(&self, name: &str) -> Option<&Theme> {
        self.themes.get(&name.to_lowercase())
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.themes.values().map(|t| t.name.as_str())
    }
}

fn file_stem(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn load_toml(path: &Path) -> Result<Theme, String> {
    let src = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let mut theme: Theme = toml::from_str(&src).map_err(|e| e.message().to_owned())?;
    if theme.name.is_empty() {
        theme.name = file_stem(path);
    }
    Ok(theme)
}

fn load_itermcolors(path: &Path) -> Result<Theme, String> {
    let plist = plist::Value::from_file(path).map_err(|e| e.to_string())?;
    let mut theme = parse_itermcolors(&plist)?;
    theme.name = file_stem(path);
    Ok(theme)
}

/// Convert an iTerm2 color preset. Components are floats in 0..1; color
/// spaces other than sRGB (e.g. Display P3) are taken as sRGB, which is
/// close enough for terminal palettes.
pub fn parse_itermcolors(plist: &plist::Value) -> Result<Theme, String> {
    let dict = plist
        .as_dictionary()
        .ok_or("not an iTerm2 color preset (expected a dictionary)")?;
    let color = |key: &str| -> Result<Color, String> {
        let entry = dict
            .get(key)
            .and_then(|v| v.as_dictionary())
            .ok_or_else(|| format!("missing {key:?}"))?;
        let component = |name: &str| -> Result<u8, String> {
            let value = entry
                .get(name)
                .and_then(|v| v.as_real())
                .ok_or_else(|| format!("{key:?} has no {name:?}"))?;
            Ok((value.clamp(0.0, 1.0) * 255.0).round() as u8)
        };
        Ok(Color {
            r: component("Red Component")?,
            g: component("Green Component")?,
            b: component("Blue Component")?,
        })
    };
    let ansi = |start: usize| -> Result<[Color; 8], String> {
        let mut colors = [Color::default(); 8];
        for (i, slot) in colors.iter_mut().enumerate() {
            *slot = color(&format!("Ansi {} Color", start + i))?;
        }
        Ok(colors)
    };

    let foreground = color("Foreground Color")?;
    let background = color("Background Color")?;
    Ok(Theme {
        name: String::new(),
        foreground,
        background,
        cursor: color("Cursor Color").unwrap_or(foreground),
        selection_foreground: color("Selected Text Color").unwrap_or(foreground),
        selection_background: color("Selection Color").unwrap_or(foreground),
        normal: ansi(0)?,
        bright: ansi(8)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLIST_TEMPLATE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  COLORS
</dict>
</plist>"#;

    fn entry(key: &str, r: f64, g: f64, b: f64) -> String {
        format!(
            "<key>{key}</key><dict>\
             <key>Color Space</key><string>sRGB</string>\
             <key>Red Component</key><real>{r}</real>\
             <key>Green Component</key><real>{g}</real>\
             <key>Blue Component</key><real>{b}</real></dict>"
        )
    }

    fn itermcolors(with_cursor: bool) -> plist::Value {
        let mut colors = String::new();
        for i in 0..16 {
            colors += &entry(&format!("Ansi {i} Color"), i as f64 / 15.0, 0.0, 1.0);
        }
        colors += &entry("Foreground Color", 1.0, 1.0, 1.0);
        colors += &entry("Background Color", 0.0, 0.0, 0.0);
        if with_cursor {
            colors += &entry("Cursor Color", 1.0, 0.0, 0.0);
        }
        colors += &entry("Selection Color", 0.2, 0.2, 0.2);
        let xml = PLIST_TEMPLATE.replace("COLORS", &colors);
        plist::Value::from_reader_xml(xml.as_bytes()).unwrap()
    }

    #[test]
    fn builtin_themes_parse() {
        let themes = builtin_themes();
        assert_eq!(themes.len(), 5);
        let (set, warnings) = ThemeSet::load(None);
        assert!(warnings.is_empty());
        assert!(set.get("tokyo night").is_some());
        assert!(set.get(DEFAULT_THEME).is_some());
        assert!(set.get("Solarized Light").is_some_and(|t| !t.is_dark()));
        assert!(set.get("dracula").is_some_and(|t| t.is_dark()));
    }

    #[test]
    fn itermcolors_import() {
        let theme = parse_itermcolors(&itermcolors(true)).unwrap();
        assert_eq!(theme.foreground, Color::from_hex(0xffffff));
        assert_eq!(theme.cursor, Color::from_hex(0xff0000));
        assert_eq!(theme.selection_background, Color::from_hex(0x333333));
        assert_eq!(theme.normal[0], Color::from_hex(0x0000ff));
        assert_eq!(theme.bright[7], Color::from_hex(0xff00ff));
        // No "Selected Text Color": falls back to the foreground.
        assert_eq!(theme.selection_foreground, theme.foreground);
    }

    #[test]
    fn itermcolors_optional_cursor() {
        let theme = parse_itermcolors(&itermcolors(false)).unwrap();
        assert_eq!(theme.cursor, theme.foreground);
    }

    #[test]
    fn user_themes_override_and_report_errors() {
        let dir = std::env::temp_dir().join(format!("nuntio-themes-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let dracula = include_str!("../themes/dracula.toml").replace("#282a36", "#000001");
        std::fs::write(dir.join("dracula.toml"), dracula).unwrap();
        let mine = include_str!("../themes/dracula.toml").replace("name = \"Dracula\"\n", "");
        std::fs::write(dir.join("Mine.toml"), mine).unwrap();
        std::fs::write(dir.join("broken.toml"), "foreground = 1").unwrap();

        let (set, warnings) = ThemeSet::load(Some(&dir));
        std::fs::remove_dir_all(&dir).unwrap();

        assert_eq!(set.get("Dracula").unwrap().background, Color::from_hex(1));
        assert_eq!(set.get("mine").unwrap().name, "Mine");
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("broken.toml"), "{warnings:?}");
    }
}
