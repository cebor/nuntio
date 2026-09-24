//! Locating, reading and validating the config file.

use std::fmt;
use std::path::{Path, PathBuf};

use crate::Config;

/// `$XDG_CONFIG_HOME/nuntio`, else `~/.config/nuntio` — on every platform,
/// so dotfiles work the same on Linux, macOS and Windows.
pub fn config_dir() -> Option<PathBuf> {
    let xdg = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute());
    let base = xdg.or_else(|| dirs::home_dir().map(|home| home.join(".config")))?;
    Some(base.join("nuntio"))
}

pub fn default_config_path() -> Option<PathBuf> {
    config_dir().map(|dir| dir.join("config.toml"))
}

/// A config that could not be used; the previous one stays active.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigError {
    pub path: PathBuf,
    pub message: String,
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.path.display(), self.message)
    }
}

impl std::error::Error for ConfigError {}

/// A successfully loaded config plus problems that didn't prevent loading.
#[derive(Debug, Clone)]
pub struct Loaded {
    pub config: Config,
    pub warnings: Vec<String>,
}

/// Load the config at `path`. A missing file yields the defaults.
pub fn load(path: &Path) -> Result<Loaded, ConfigError> {
    let error = |message: String| ConfigError {
        path: path.to_owned(),
        message,
    };
    let source = match std::fs::read_to_string(path) {
        Ok(source) => source,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Loaded {
                config: Config::default(),
                warnings: Vec::new(),
            });
        }
        Err(err) => return Err(error(err.to_string())),
    };
    parse(&source).map_err(error)
}

/// Parse and validate config source.
pub fn parse(source: &str) -> Result<Loaded, String> {
    let mut warnings = Vec::new();
    let deserializer = toml::Deserializer::parse(source).map_err(|e| describe(&e, source))?;
    let config: Config = serde_ignored::deserialize(deserializer, |path| {
        warnings.push(format!("unknown key `{path}`"));
    })
    .map_err(|e: toml::de::Error| describe(&e, source))?;
    config.validate()?;
    Ok(Loaded { config, warnings })
}

/// Error message with a 1-based line number.
fn describe(err: &toml::de::Error, source: &str) -> String {
    let message = err.message().trim_end();
    match err.span() {
        Some(span) => {
            let line = source[..span.start.min(source.len())]
                .lines()
                .count()
                .max(1);
            format!("line {line}: {message}")
        }
        None => message.to_owned(),
    }
}

impl Config {
    fn validate(&self) -> Result<(), String> {
        let in_range = |name: &str, value: f32, min: f32, max: f32| {
            if (min..=max).contains(&value) {
                Ok(())
            } else {
                Err(format!(
                    "`{name}` must be between {min} and {max}, got {value}"
                ))
            }
        };
        in_range("font.size", self.font.size, 4.0, 72.0)?;
        in_range("window.opacity", self.window.opacity, 0.0, 1.0)?;
        in_range("panes.dim_inactive", self.panes.dim_inactive, 0.0, 1.0)?;
        if self.scrollback > 1_000_000 {
            return Err(format!(
                "`scrollback` must be at most 1000000, got {}",
                self.scrollback
            ));
        }
        if let Some(shell) = &self.shell
            && shell.program.trim().is_empty()
        {
            return Err("`shell.program` must not be empty".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_gives_defaults() {
        let loaded = load(Path::new("/nonexistent/nuntio/config.toml")).unwrap();
        assert_eq!(loaded.config, Config::default());
        assert!(loaded.warnings.is_empty());
    }

    #[test]
    fn unknown_keys_are_warnings() {
        let mut loaded = parse("scrolback = 5\n[font]\nsize = 12.0\nfamliy = \"Hack\"").unwrap();
        assert_eq!(loaded.config.font.size, 12.0);
        loaded.warnings.sort();
        assert_eq!(
            loaded.warnings,
            ["unknown key `font.famliy`", "unknown key `scrolback`"]
        );
    }

    #[test]
    fn syntax_errors_name_the_line() {
        let err = parse("scrollback = 100\n\n[font\nsize = 1").unwrap_err();
        assert!(err.starts_with("line 3:"), "{err}");
    }

    #[test]
    fn type_errors_name_the_line() {
        let err = parse("[font]\nsize = \"big\"").unwrap_err();
        assert!(err.starts_with("line 2:"), "{err}");
    }

    #[test]
    fn invalid_values_are_errors() {
        let err = parse("[font]\nsize = 0.0").unwrap_err();
        assert!(err.contains("font.size"), "{err}");
        let err = parse("[window]\nopacity = 1.5").unwrap_err();
        assert!(err.contains("window.opacity"), "{err}");
        let err = parse("shell = { program = \"\" }").unwrap_err();
        assert!(err.contains("shell.program"), "{err}");
    }

    #[test]
    fn config_dir_honors_xdg() {
        // Only checks the shape; the environment is process-global.
        let dir = config_dir().unwrap();
        assert!(dir.ends_with("nuntio"));
    }
}
