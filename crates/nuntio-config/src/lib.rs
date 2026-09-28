//! Config schema, loading, validation, themes and file watching.

mod color;
mod edit;
mod keys;
mod load;
pub mod schema;
mod theme;
mod watch;

use std::borrow::Cow;

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
    /// Programs may copy to the clipboard (OSC 52).
    pub clipboard_write: bool,
    /// Ask before pasting line breaks that would run commands at once.
    pub confirm_paste: bool,
    /// Programs may turn on the kitty keyboard protocol.
    pub kitty_keyboard: bool,
    pub font: Font,
    pub window: Window,
    pub tabs: Tabs,
    pub panes: Panes,
    pub status_bar: StatusBar,
    pub updates: Updates,
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
            clipboard_write: true,
            confirm_paste: true,
            kitty_keyboard: true,
            font: Font::default(),
            window: Window::default(),
            tabs: Tabs::default(),
            panes: Panes::default(),
            status_bar: StatusBar::default(),
            updates: Updates::default(),
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

    /// At least one of the three update indicators is switched on.
    pub fn update_indicators(&self) -> bool {
        self.updates.banner || self.updates.tab_bar || self.updates.status_bar
    }

    /// The `update` status bar item is switched on and placed in the items.
    pub fn update_in_status_bar(&self) -> bool {
        self.updates.status_bar && self.status_bar.items.contains(&StatusItem::Update)
    }

    /// Look for a newer release on GitHub once a day: while the check is
    /// on and at least one indicator could show what it finds.
    pub fn update_check(&self) -> bool {
        self.updates.check
            && (self.updates.banner
                || self.updates.tab_bar
                || (self.status_bar.enabled && self.update_in_status_bar()))
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
    /// Program and arguments to spawn. With `wsl`, the shell starts in
    /// `wsl_dir` (`~` or an absolute Linux path), at home without it.
    pub fn command(&self, wsl_dir: Option<&str>) -> (String, Vec<String>) {
        let Some(distro) = &self.wsl else {
            let program = self.program.clone().unwrap_or_default();
            return (program, self.args.clone());
        };
        let mut args = vec!["-d".to_owned(), distro.clone()];
        if let Some(user) = &self.wsl_user {
            args.extend(["-u".to_owned(), user.clone()]);
        }
        args.extend(["--cd".to_owned(), wsl_dir.unwrap_or("~").to_owned()]);
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
    /// Grid size of a new window, in cells.
    pub columns: u16,
    pub lines: u16,
    pub padding: Padding,
    pub opacity: f32,
    pub decorations: Decorations,
    /// Overrides `decorations` on macOS when set.
    pub macos_titlebar: Option<MacosTitlebar>,
    /// Ask before closing a window, tab or pane in which a program runs.
    pub confirm_close: bool,
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
            columns: 100,
            lines: 30,
            padding: Padding::default(),
            opacity: 1.0,
            decorations: Decorations::default(),
            macos_titlebar: None,
            confirm_close: true,
        }
    }
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
    pub title: TabTitle,
    /// New tabs start in the focused pane's directory instead of at home.
    pub inherit_directory: bool,
}

impl Default for Tabs {
    fn default() -> Self {
        Self {
            hide_when_single: true,
            title: TabTitle::default(),
            inherit_directory: false,
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
    /// A newer nuntio release; hidden while there is none.
    Update,
    /// A flexible gap; springs share the free space evenly.
    #[serde(rename = "<->")]
    Spring,
}

impl StatusItem {
    /// How the item is spelled in the config file.
    pub fn name(self) -> &'static str {
        match self {
            Self::Cpu => "cpu",
            Self::Memory => "memory",
            Self::Network => "network",
            Self::Battery => "battery",
            Self::Datetime => "datetime",
            Self::Update => "update",
            Self::Spring => schema::SPRING,
        }
    }
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
    /// Each item in its own color, spread around the color wheel.
    pub rainbow: bool,
}

impl StatusBar {
    /// Enabled and with something to show.
    pub fn visible(&self) -> bool {
        self.enabled && self.items.iter().any(|&item| item != StatusItem::Spring)
    }

    /// The items as laid out: without a spring, one before the last item,
    /// so it sits at the right edge.
    pub fn arranged_items(&self) -> Cow<'_, [StatusItem]> {
        arranged(&self.items)
    }
}

/// `items` with a spring before the last one, if it has none.
pub fn arranged(items: &[StatusItem]) -> Cow<'_, [StatusItem]> {
    if items.is_empty() || items.contains(&StatusItem::Spring) {
        return Cow::Borrowed(items);
    }
    let mut out = items.to_vec();
    out.insert(items.len() - 1, StatusItem::Spring);
    Cow::Owned(out)
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
            rainbow: false,
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

/// The update check and where a newer release is announced.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub struct Updates {
    /// Look for a newer release on GitHub once a day.
    pub check: bool,
    /// An info banner at the bottom of the window.
    pub banner: bool,
    /// An arrow in the tab bar.
    pub tab_bar: bool,
    /// The `update` status bar item, where `status_bar.items` places it.
    pub status_bar: bool,
}

impl Default for Updates {
    fn default() -> Self {
        Self {
            check: false,
            banner: true,
            tab_bar: true,
            status_bar: false,
        }
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
    fn without_a_spring_the_last_item_is_pushed_right() {
        use StatusItem::*;
        assert_eq!(
            arranged(&[Cpu, Memory, Datetime])[..],
            [Cpu, Memory, Spring, Datetime]
        );
        assert_eq!(arranged(&[Cpu, Spring, Memory])[..], [Cpu, Spring, Memory]);
        assert_eq!(
            arranged(&[Cpu, Datetime, Spring])[..],
            [Cpu, Datetime, Spring]
        );
        assert_eq!(arranged(&[Datetime])[..], [Spring, Datetime]);
        assert_eq!(arranged(&[])[..], []);
        let bar = StatusBar {
            enabled: true,
            items: vec![Spring, Spring],
            ..StatusBar::default()
        };
        assert!(!bar.visible(), "springs alone show nothing");
    }

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
columns = 120
lines = 40
padding = { x = 8, y = 6 }
opacity = 1.0
decorations = "system"
macos_titlebar = "none"

[tabs]
hide_when_single = true
title = "process"
inherit_directory = true

[panes]
dim_inactive = 0.15

[status_bar]
enabled = true
position = "top"
items = ["datetime", "cpu"]
datetime_format = "%H:%M:%S"
rainbow = true

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
        assert!(cfg.tabs.inherit_directory);
        assert!(cfg.status_bar.visible());
        assert_eq!(cfg.status_bar.position, StatusBarPosition::Top);
        assert_eq!(
            cfg.status_bar.items,
            [StatusItem::Datetime, StatusItem::Cpu]
        );
        assert_eq!(cfg.status_bar.datetime_format, "%H:%M:%S");
        assert!(cfg.status_bar.rainbow);
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
    fn update_check_needs_an_indicator() {
        let check = |toml: &str| Config::from_toml(toml).unwrap().update_check();
        assert!(!check(""), "off by default");
        assert!(check("[updates]\ncheck = true"));
        assert!(!check("[updates]\nbanner = true\ntab_bar = true"));
        let none = "[updates]\ncheck = true\nbanner = false\ntab_bar = false";
        let bar = format!("{none}\nstatus_bar = true");
        assert!(!check(&bar), "the item isn't placed");
        let placed = "[status_bar]\nenabled = true\nitems = [\"update\"]";
        assert!(check(&format!("{bar}\n{placed}")));
        assert!(
            !check(&format!("{none}\n{placed}")),
            "the item is switched off"
        );
        assert!(
            !check(&format!("{bar}\n[status_bar]\nitems = [\"update\"]")),
            "the status bar is off"
        );
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
            shell(r#"shell = { program = "fish", args = ["-l"] }"#).command(None),
            ("fish".to_owned(), vec!["-l".to_owned()])
        );
        assert_eq!(
            shell(r#"shell = { wsl = "Ubuntu" }"#).command(None),
            (
                "wsl.exe".to_owned(),
                ["-d", "Ubuntu", "--cd", "~"].map(String::from).to_vec()
            )
        );
        assert_eq!(
            shell(r#"shell = { wsl = "Ubuntu", wsl_user = "root" }"#)
                .command(None)
                .1,
            ["-d", "Ubuntu", "-u", "root", "--cd", "~"]
        );
        assert_eq!(
            shell(r#"shell = { wsl = "Debian", program = "fish", args = ["-l"] }"#)
                .command(None)
                .1,
            ["-d", "Debian", "--cd", "~", "--exec", "fish", "-l"]
        );
        assert_eq!(
            shell(r#"shell = { wsl = "Ubuntu" }"#)
                .command(Some("/home/me/My Files"))
                .1,
            ["-d", "Ubuntu", "--cd", "/home/me/My Files"]
        );
    }

    #[test]
    fn single_theme_name() {
        let cfg = Config::from_toml(r#"theme = "Dracula""#).unwrap();
        assert_eq!(cfg.theme, ThemeSelection::Single("Dracula".into()));
    }
}
