//! Config schema, loading, validation, themes and file watching.

mod color;
mod edit;
mod keys;
mod load;
pub mod schema;
mod theme;
mod watch;

use serde::{Deserialize, Serialize};

pub use color::{Color, ColorError};
pub use edit::{ConfigDoc, write_config};
pub use keys::{ACTIONS, KeyCombo, KeyComboError, KeyName, Mods, NamedKey};
pub use load::{
    ConfigError, ConfigLocation, Loaded, config_dir, load, locate_config, parse, themes_dir,
};
pub use theme::{DEFAULT_THEME, Theme, ThemeSet, builtin_themes, parse_itermcolors};
pub use toml_edit;
pub use watch::ConfigWatcher;

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub struct Config {
    pub shell: Option<Shell>,
    pub scrollback: usize,
    pub font: Font,
    pub window: Window,
    pub tabs: Tabs,
    pub panes: Panes,
    pub status_bar: StatusBar,
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
            status_bar: StatusBar::default(),
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

#[derive(Debug, Clone, PartialEq, Default, Deserialize, Serialize)]
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

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
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

#[derive(Debug, Clone, Copy, PartialEq, Deserialize, Serialize)]
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

/// Who draws the window frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Decorations {
    /// nuntio's own header: the tab bar with window buttons (on macOS, the
    /// native traffic lights in a transparent title bar).
    #[default]
    Custom,
    /// The system's title bar and frame.
    System,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum MacosTitlebar {
    Native,
    Transparent,
    None,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub struct Window {
    pub padding: Padding,
    pub opacity: f32,
    pub decorations: Decorations,
    /// Overrides `decorations` on macOS when set.
    pub macos_titlebar: Option<MacosTitlebar>,
}

impl Window {
    /// The macOS title bar style: `macos_titlebar` if set, otherwise derived
    /// from `decorations`.
    pub fn effective_macos_titlebar(&self) -> MacosTitlebar {
        self.macos_titlebar.unwrap_or(match self.decorations {
            Decorations::Custom => MacosTitlebar::Transparent,
            Decorations::System => MacosTitlebar::Native,
        })
    }
}

impl Default for Window {
    fn default() -> Self {
        Self {
            padding: Padding::default(),
            opacity: 1.0,
            decorations: Decorations::default(),
            macos_titlebar: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TabPosition {
    #[default]
    Top,
}

/// What a tab shows as its title.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TabTitle {
    /// The directory while the shell waits at its prompt, otherwise the
    /// running program.
    #[default]
    Auto,
    /// The working directory.
    Path,
    /// The name of the running program.
    Process,
    /// The title the application sets (OSC 0/2), unchanged.
    Application,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub struct Tabs {
    pub hide_when_single: bool,
    pub position: TabPosition,
    pub title: TabTitle,
}

impl Default for Tabs {
    fn default() -> Self {
        Self {
            hide_when_single: true,
            position: TabPosition::Top,
            title: TabTitle::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub struct Panes {
    pub dim_inactive: f32,
}

impl Default for Panes {
    fn default() -> Self {
        Self { dim_inactive: 0.15 }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum StatusBarPosition {
    #[default]
    Bottom,
    /// Below the tab bar.
    Top,
}

/// Something the status bar shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum StatusItem {
    Cpu,
    Memory,
    Network,
    Battery,
    Datetime,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub struct StatusBar {
    pub enabled: bool,
    pub position: StatusBarPosition,
    /// Shown in this order; the last one sits at the right edge.
    pub items: Vec<StatusItem>,
    /// strftime format of the date and time.
    pub datetime_format: String,
}

impl StatusBar {
    /// Enabled and with something to show.
    pub fn visible(&self) -> bool {
        self.enabled && !self.items.is_empty()
    }
}

impl Default for StatusBar {
    fn default() -> Self {
        Self {
            enabled: false,
            position: StatusBarPosition::default(),
            items: vec![
                StatusItem::Cpu,
                StatusItem::Memory,
                StatusItem::Network,
                StatusItem::Battery,
                StatusItem::Datetime,
            ],
            datetime_format: "%a %d %b %H:%M".into(),
        }
    }
}

/// Either a single theme name or a light/dark pair following the OS theme.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub enum ThemeSelection {
    Single(String),
    Auto { light: String, dark: String },
}

/// By hand instead of `untagged`, whose error ("data did not match any
/// variant") doesn't say what is wrong.
impl<'de> Deserialize<'de> for ThemeSelection {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;

        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = ThemeSelection;

            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a theme name or a table with `light` and `dark`")
            }

            fn visit_str<E: serde::de::Error>(self, name: &str) -> Result<Self::Value, E> {
                Ok(ThemeSelection::Single(name.to_owned()))
            }

            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> Result<Self::Value, A::Error> {
                use serde::de::Error;

                let (mut light, mut dark) = (None, None);
                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "light" => light = Some(map.next_value()?),
                        "dark" => dark = Some(map.next_value()?),
                        other => {
                            return Err(A::Error::unknown_field(other, &["light", "dark"]));
                        }
                    }
                }
                match (light, dark) {
                    (Some(light), Some(dark)) => Ok(ThemeSelection::Auto { light, dark }),
                    _ => Err(A::Error::custom(
                        "`theme` needs both `light` and `dark` to follow the OS appearance",
                    )),
                }
            }
        }

        deserializer.deserialize_any(Visitor)
    }
}

impl Default for ThemeSelection {
    fn default() -> Self {
        Self::Single("iTerm2 Default".into())
    }
}

#[derive(Debug, Clone, PartialEq, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct Mouse {
    pub copy_on_select: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum OptionAsMeta {
    #[default]
    None,
    Left,
    Right,
    Both,
}

#[derive(Debug, Clone, PartialEq, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct MacOs {
    pub option_as_meta: OptionAsMeta,
}

/// Missing fields are empty, so one incomplete entry is skipped with a
/// warning instead of rejecting the whole config.
#[derive(Debug, Clone, PartialEq, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct Keybinding {
    pub key: String,
    pub action: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn incomplete_keybinding_does_not_reject_the_config() {
        let cfg = Config::from_toml(
            r#"
[[keybindings]]
key = "Ctrl+Shift+X"

[[keybindings]]
key = "F5"
action = "new_tab"
"#,
        )
        .unwrap();
        assert_eq!(cfg.keybindings.len(), 2);
        assert_eq!(cfg.keybindings[0].action, "");
    }

    #[test]
    fn theme_selection_errors_say_what_is_missing() {
        let pair = Config::from_toml("theme = { light = \"a\", dark = \"b\" }").unwrap();
        assert_eq!(
            pair.theme,
            ThemeSelection::Auto {
                light: "a".into(),
                dark: "b".into()
            }
        );
        let err = Config::from_toml("theme = { light = \"a\" }").unwrap_err();
        assert!(err.message().contains("both `light` and `dark`"), "{err}");
        let err = Config::from_toml("theme = 3").unwrap_err();
        assert!(err.message().contains("a theme name"), "{err}");
        let err = Config::from_toml("theme = { light = \"a\", dark = \"b\", x = 1 }").unwrap_err();
        assert!(err.message().contains("unknown field `x`"), "{err}");
    }

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
decorations = "system"
macos_titlebar = "none"

[tabs]
hide_when_single = true
position = "top"
title = "process"

[panes]
dim_inactive = 0.15

[status_bar]
enabled = true
position = "top"
items = ["datetime", "cpu"]
datetime_format = "%H:%M:%S"

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
        assert_eq!(cfg.window.decorations, Decorations::System);
        assert_eq!(cfg.window.effective_macos_titlebar(), MacosTitlebar::None);
        assert_eq!(cfg.tabs.title, TabTitle::Process);
        assert!(cfg.status_bar.visible());
        assert_eq!(cfg.status_bar.position, StatusBarPosition::Top);
        assert_eq!(
            cfg.status_bar.items,
            [StatusItem::Datetime, StatusItem::Cpu]
        );
        assert_eq!(cfg.status_bar.datetime_format, "%H:%M:%S");
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

    #[test]
    fn status_bar_is_off_by_default() {
        let bar = Config::default().status_bar;
        assert!(!bar.visible());
        assert_eq!(bar.position, StatusBarPosition::Bottom);
        assert_eq!(bar.items.len(), 5);
        assert_eq!(bar.items.last(), Some(&StatusItem::Datetime));

        let bar = Config::from_toml("[status_bar]\nenabled = true\nitems = []")
            .unwrap()
            .status_bar;
        assert!(!bar.visible(), "nothing to show");
    }

    #[test]
    fn macos_titlebar_follows_decorations_unless_set() {
        let window = |toml: &str| Config::from_toml(toml).unwrap().window;
        assert_eq!(
            window("").effective_macos_titlebar(),
            MacosTitlebar::Transparent
        );
        assert_eq!(
            window("[window]\ndecorations = \"system\"").effective_macos_titlebar(),
            MacosTitlebar::Native
        );
        assert_eq!(
            window("[window]\nmacos_titlebar = \"native\"").effective_macos_titlebar(),
            MacosTitlebar::Native
        );
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
