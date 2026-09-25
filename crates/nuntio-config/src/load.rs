//! Locating, reading and validating the config file.

use std::fmt;
use std::path::{Path, PathBuf};

use crate::schema::{DIM_INACTIVE, FONT_SIZE, MAX_SCROLLBACK, OPACITY};
use crate::{Config, Shell, StatusBar};

/// `$XDG_CONFIG_HOME/nuntio`, else `~/.config/nuntio` — on every platform,
/// so dotfiles work the same on Linux, macOS and Windows.
pub fn config_dir() -> Option<PathBuf> {
    let xdg = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute());
    let base = xdg.or_else(|| dirs::home_dir().map(|home| home.join(".config")))?;
    Some(base.join("nuntio"))
}

/// File name of the alternative config in the home directory.
const HOME_CONFIG: &str = ".nuntio.toml";

/// The config file to use when none is given on the command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigLocation {
    pub path: PathBuf,
    /// `~/.nuntio.toml`, if it exists but is ignored because
    /// `<config dir>/config.toml` exists too.
    pub shadowed: Option<PathBuf>,
}

/// `<config dir>/config.toml` if it exists, else `~/.nuntio.toml` if that
/// exists. With neither, the former, so creating it later is picked up by
/// hot reload.
pub fn locate_config() -> Option<ConfigLocation> {
    choose_config(
        config_dir().map(|dir| dir.join("config.toml")),
        dirs::home_dir().map(|home| home.join(HOME_CONFIG)),
        |path| path.is_file(),
    )
}

fn choose_config(
    primary: Option<PathBuf>,
    home: Option<PathBuf>,
    exists: impl Fn(&Path) -> bool,
) -> Option<ConfigLocation> {
    let home = home.filter(|path| exists(path));
    let location = match (primary, home) {
        (Some(primary), home) if exists(&primary) => ConfigLocation {
            path: primary,
            shadowed: home,
        },
        (_, Some(home)) => ConfigLocation {
            path: home,
            shadowed: None,
        },
        (primary, None) => ConfigLocation {
            path: primary?,
            shadowed: None,
        },
    };
    Some(location)
}

/// Directory with user themes for the config at `config_path`: `themes/`
/// next to it, except for `~/.nuntio.toml`, whose themes stay in
/// `<config dir>/themes` rather than cluttering the home directory.
pub fn themes_dir(config_path: &Path) -> Option<PathBuf> {
    if config_path.file_name() == Some(HOME_CONFIG.as_ref()) {
        return config_dir().map(|dir| dir.join("themes"));
    }
    config_path.parent().map(|dir| dir.join("themes"))
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
            let line = source.as_bytes()[..span.start.min(source.len())]
                .iter()
                .filter(|&&b| b == b'\n')
                .count()
                + 1;
            format!("line {line}: {message}")
        }
        None => message.to_owned(),
    }
}

impl Config {
    fn validate(&self) -> Result<(), String> {
        let in_range = |name: &str, value: f32, (min, max): (f64, f64)| {
            if (min..=max).contains(&f64::from(value)) {
                Ok(())
            } else {
                Err(format!(
                    "`{name}` must be between {min} and {max}, got {value}"
                ))
            }
        };
        in_range("font.size", self.font.size, FONT_SIZE)?;
        in_range("window.opacity", self.window.opacity, OPACITY)?;
        in_range("panes.dim_inactive", self.panes.dim_inactive, DIM_INACTIVE)?;
        if self.scrollback as u64 > MAX_SCROLLBACK as u64 {
            return Err(format!(
                "`scrollback` must be at most {MAX_SCROLLBACK}, got {}",
                self.scrollback
            ));
        }
        if let Some(shell) = &self.shell {
            shell.validate()?;
        }
        self.status_bar.validate()?;
        Ok(())
    }
}

impl Shell {
    fn validate(&self) -> Result<(), String> {
        let not_empty = |name: &str, value: &Option<String>| match value {
            Some(v) if v.trim().is_empty() => Err(format!("`{name}` must not be empty")),
            _ => Ok(()),
        };
        not_empty("shell.program", &self.program)?;
        not_empty("shell.wsl", &self.wsl)?;
        not_empty("shell.wsl_user", &self.wsl_user)?;
        if self.program.is_none() && self.wsl.is_none() {
            return Err("`shell` needs `program` or `wsl`".into());
        }
        if self.wsl_user.is_some() && self.wsl.is_none() {
            return Err("`shell.wsl_user` needs `shell.wsl`".into());
        }
        if !self.args.is_empty() && self.program.is_none() {
            return Err("`shell.args` needs `shell.program`".into());
        }
        Ok(())
    }
}

impl StatusBar {
    fn validate(&self) -> Result<(), String> {
        for (i, item) in self.items.iter().enumerate() {
            if self.items[..i].contains(item) {
                let name = format!("{item:?}").to_lowercase();
                return Err(format!("`status_bar.items` lists \"{name}\" twice"));
            }
        }
        let invalid = chrono::format::StrftimeItems::new(&self.datetime_format)
            .any(|item| item == chrono::format::Item::Error);
        if invalid {
            return Err(format!(
                "`status_bar.datetime_format` is not a valid strftime format: \"{}\"",
                self.datetime_format
            ));
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
    fn errors_at_the_start_of_a_line_name_that_line() {
        let err = parse("scrollback = 1\nscrollback = 2").unwrap_err();
        assert!(err.starts_with("line 2:"), "{err}");
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
    fn invalid_shells_are_errors() {
        for (source, expected) in [
            ("shell = {}", "`shell` needs"),
            ("shell = { wsl = \" \" }", "`shell.wsl` must not be empty"),
            (
                "shell = { program = \"sh\", wsl_user = \"root\" }",
                "shell.wsl_user",
            ),
            (
                "shell = { wsl = \"Ubuntu\", args = [\"-l\"] }",
                "shell.args",
            ),
        ] {
            let err = parse(source).unwrap_err();
            assert!(err.contains(expected), "{source}: {err}");
        }
        assert!(parse("shell = { wsl = \"Ubuntu\", wsl_user = \"root\" }").is_ok());
        assert!(parse("shell = { wsl = \"Ubuntu\", program = \"fish\" }").is_ok());
    }

    #[test]
    fn invalid_status_bars_are_errors() {
        let err = parse("[status_bar]\nitems = [\"cpu\", \"gpu\"]").unwrap_err();
        assert!(err.starts_with("line 2:"), "{err}");
        let err = parse("[status_bar]\nitems = [\"cpu\", \"datetime\", \"cpu\"]").unwrap_err();
        assert!(err.contains("\"cpu\" twice"), "{err}");
        let err = parse("[status_bar]\ndatetime_format = \"%H:%\"").unwrap_err();
        assert!(err.contains("status_bar.datetime_format"), "{err}");
        assert!(parse("[status_bar]\ndatetime_format = \"%a %d.%m. %H:%M\"").is_ok());
    }

    #[test]
    fn config_location() {
        let primary = PathBuf::from("/home/u/.config/nuntio/config.toml");
        let home = PathBuf::from("/home/u/.nuntio.toml");
        let choose = |existing: &[&PathBuf]| {
            choose_config(Some(primary.clone()), Some(home.clone()), |p| {
                existing.iter().any(|e| e.as_path() == p)
            })
            .unwrap()
        };
        let at = |path: &PathBuf, shadowed: Option<&PathBuf>| ConfigLocation {
            path: path.clone(),
            shadowed: shadowed.cloned(),
        };
        assert_eq!(choose(&[]), at(&primary, None));
        assert_eq!(choose(&[&primary]), at(&primary, None));
        assert_eq!(choose(&[&home]), at(&home, None));
        assert_eq!(choose(&[&primary, &home]), at(&primary, Some(&home)));
        assert_eq!(choose_config(None, None, |_| true), None);
    }

    #[test]
    fn themes_next_to_config_except_in_home() {
        assert_eq!(
            themes_dir(Path::new("/etc/nuntio/custom.toml")).unwrap(),
            Path::new("/etc/nuntio/themes")
        );
        assert_eq!(
            themes_dir(Path::new("/home/u/.nuntio.toml")),
            config_dir().map(|dir| dir.join("themes"))
        );
    }

    #[test]
    fn config_dir_honors_xdg() {
        // Only checks the shape; the environment is process-global.
        let dir = config_dir().unwrap();
        assert!(dir.ends_with("nuntio"));
    }
}
