//! Config schema, loading, validation, themes and file watching.

mod color;
mod load;
mod theme;
mod watch;

use serde::Deserialize;

pub use color::Color;
pub use load::{ConfigError, Loaded, config_dir, default_config_path, load, parse};
pub use theme::{DEFAULT_THEME, Theme, ThemeSet, builtin_themes, parse_itermcolors};
pub use watch::ConfigWatcher;

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct Config {
    pub shell: Option<Shell>,
    pub scrollback: usize,
    pub font: Font,
    pub window: Window,
    pub tabs: Tabs,
    pub panes: Panes,
    pub theme: ThemeSelection,
    pub mouse: Mouse,
    pub macos: MacOs,
    pub keybindings: Vec<Keybinding>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            shell: None,
            scrollback: 10_000,
            font: Font::default(),
            window: Window::default(),
            tabs: Tabs::default(),
            panes: Panes::default(),
            theme: ThemeSelection::default(),
            mouse: Mouse::default(),
            macos: MacOs::default(),
            keybindings: Vec::new(),
        }
    }
}

impl Config {
    pub fn from_toml(s: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(s)
    }
}

#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(default)]
pub struct Shell {
    /// With `wsl`, the program to run inside the distro instead of the
    /// user's login shell.
    pub program: Option<String>,
    pub args: Vec<String>,
    /// WSL distribution to start the shell in (Windows).
    pub wsl: Option<String>,
    /// User in the WSL distribution; the distro's default user if unset.
    pub wsl_user: Option<String>,
}

impl Shell {
    /// Program and arguments to spawn.
    pub fn command(&self) -> (String, Vec<String>) {
        let Some(distro) = &self.wsl else {
            let program = self.program.clone().unwrap_or_default();
            return (program, self.args.clone());
        };
        let mut args = vec!["-d".to_owned(), distro.clone()];
        if let Some(user) = &self.wsl_user {
            args.extend(["-u".to_owned(), user.clone()]);
        }
        args.extend(["--cd".to_owned(), "~".to_owned()]);
        if let Some(program) = &self.program {
            args.extend(["--exec".to_owned(), program.clone()]);
            args.extend(self.args.iter().cloned());
        }
        ("wsl.exe".to_owned(), args)
    }

    pub fn is_wsl(&self) -> bool {
        self.wsl.is_some()
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct Font {
    pub family: Option<String>,
    pub size: f32,
}

impl Default for Font {
    fn default() -> Self {
        Self {
            family: None,
            size: 13.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(default)]
pub struct Padding {
    pub x: u16,
    pub y: u16,
}

impl Default for Padding {
    fn default() -> Self {
        Self { x: 8, y: 6 }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MacosTitlebar {
    #[default]
    Native,
    Transparent,
    None,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct Window {
    pub padding: Padding,
    pub opacity: f32,
    pub macos_titlebar: MacosTitlebar,
}

impl Default for Window {
    fn default() -> Self {
        Self {
            padding: Padding::default(),
            opacity: 1.0,
            macos_titlebar: MacosTitlebar::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TabPosition {
    #[default]
    Top,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct Tabs {
    pub hide_when_single: bool,
    pub position: TabPosition,
}

impl Default for Tabs {
    fn default() -> Self {
        Self {
            hide_when_single: true,
            position: TabPosition::Top,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct Panes {
    pub dim_inactive: f32,
}

impl Default for Panes {
    fn default() -> Self {
        Self { dim_inactive: 0.15 }
    }
}

/// Either a single theme name or a light/dark pair following the OS theme.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(untagged)]
pub enum ThemeSelection {
    Single(String),
    Auto { light: String, dark: String },
}

impl Default for ThemeSelection {
    fn default() -> Self {
        Self::Single("iTerm2 Default".into())
    }
}

#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(default)]
pub struct Mouse {
    pub copy_on_select: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OptionAsMeta {
    #[default]
    None,
    Left,
    Right,
    Both,
}

#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(default)]
pub struct MacOs {
    pub option_as_meta: OptionAsMeta,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Keybinding {
    pub key: String,
    pub action: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_file_gives_defaults() {
        assert_eq!(Config::from_toml("").unwrap(), Config::default());
    }

    #[test]
    fn full_example_parses() {
        let cfg = Config::from_toml(
            r#"
shell = { program = "/bin/zsh", args = ["-l"] }
scrollback = 10000

[font]
family = "JetBrains Mono"
size = 13.0

[window]
padding = { x = 8, y = 6 }
opacity = 1.0
macos_titlebar = "transparent"

[tabs]
hide_when_single = true
position = "top"

[panes]
dim_inactive = 0.15

[theme]
light = "Solarized Light"
dark = "Tokyo Night"

[mouse]
copy_on_select = false

[macos]
option_as_meta = "left"

[[keybindings]]
key = "Cmd+Shift+D"
action = "split_horizontal"
"#,
        )
        .unwrap();

        let shell = cfg.shell.unwrap();
        assert_eq!(shell.program.as_deref(), Some("/bin/zsh"));
        assert_eq!(shell.args, ["-l"]);
        assert_eq!(cfg.font.family.as_deref(), Some("JetBrains Mono"));
        assert_eq!(cfg.window.macos_titlebar, MacosTitlebar::Transparent);
        assert_eq!(
            cfg.theme,
            ThemeSelection::Auto {
                light: "Solarized Light".into(),
                dark: "Tokyo Night".into()
            }
        );
        assert_eq!(cfg.macos.option_as_meta, OptionAsMeta::Left);
        assert_eq!(cfg.keybindings.len(), 1);
    }

    fn shell(toml: &str) -> Shell {
        Config::from_toml(toml).unwrap().shell.unwrap()
    }

    #[test]
    fn shell_command() {
        assert_eq!(
            shell(r#"shell = { program = "fish", args = ["-l"] }"#).command(),
            ("fish".to_owned(), vec!["-l".to_owned()])
        );
        assert_eq!(
            shell(r#"shell = { wsl = "Ubuntu" }"#).command(),
            (
                "wsl.exe".to_owned(),
                ["-d", "Ubuntu", "--cd", "~"].map(String::from).to_vec()
            )
        );
        assert_eq!(
            shell(r#"shell = { wsl = "Ubuntu", wsl_user = "root" }"#)
                .command()
                .1,
            ["-d", "Ubuntu", "-u", "root", "--cd", "~"]
        );
        assert_eq!(
            shell(r#"shell = { wsl = "Debian", program = "fish", args = ["-l"] }"#)
                .command()
                .1,
            ["-d", "Debian", "--cd", "~", "--exec", "fish", "-l"]
        );
    }

    #[test]
    fn single_theme_name() {
        let cfg = Config::from_toml(r#"theme = "Dracula""#).unwrap();
        assert_eq!(cfg.theme, ThemeSelection::Single("Dracula".into()));
    }
}
