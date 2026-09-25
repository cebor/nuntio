//! Description of every setting, for editors: where it lives in the file,
//! what values it takes and what it does. Defaults and current values come
//! from serializing a [`Config`](crate::Config), not from here.

/// Limits shared by validation and the schema.
pub const FONT_SIZE: (f64, f64) = (4.0, 72.0);
pub const OPACITY: (f64, f64) = (0.0, 1.0);
pub const DIM_INACTIVE: (f64, f64) = (0.0, 1.0);
pub const MAX_SCROLLBACK: i64 = 1_000_000;
/// Enough for any sensible margin; more would leave no room for text.
pub const MAX_PADDING: u16 = 200;
/// Allowed `window.columns` and `window.lines`.
pub const WINDOW_COLUMNS: (u16, u16) = (10, 1000);
pub const WINDOW_LINES: (u16, u16) = (4, 500);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    General,
    Font,
    Window,
    Tabs,
    Panes,
    StatusBar,
    Theme,
    Mouse,
    MacOs,
    Shell,
    Keybindings,
}

impl Section {
    pub const ALL: [Section; 11] = [
        Section::General,
        Section::Theme,
        Section::Font,
        Section::Window,
        Section::Tabs,
        Section::Panes,
        Section::StatusBar,
        Section::Mouse,
        Section::Shell,
        Section::Keybindings,
        Section::MacOs,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Section::General => "General",
            Section::Font => "Font",
            Section::Window => "Window",
            Section::Tabs => "Tabs",
            Section::Panes => "Panes",
            Section::StatusBar => "Status bar",
            Section::Theme => "Theme",
            Section::Mouse => "Mouse",
            Section::MacOs => "macOS",
            Section::Shell => "Shell",
            Section::Keybindings => "Keybindings",
        }
    }
}

/// The only platform a setting has an effect on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    MacOs,
    Windows,
}

impl Platform {
    pub fn label(self) -> &'static str {
        match self {
            Platform::MacOs => "macOS",
            Platform::Windows => "Windows",
        }
    }

    /// Whether this is the platform the program runs on.
    pub fn is_current(self) -> bool {
        match self {
            Platform::MacOs => cfg!(target_os = "macos"),
            Platform::Windows => cfg!(windows),
        }
    }
}

/// One allowed value of a string setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Variant {
    pub value: &'static str,
    pub help: &'static str,
}

const fn variant(value: &'static str, help: &'static str) -> Variant {
    Variant { value, help }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Kind {
    Bool,
    Int {
        min: i64,
        max: i64,
        step: i64,
    },
    Float {
        min: f64,
        max: f64,
        step: f64,
    },
    /// One of a fixed set of strings.
    Choice(&'static [Variant]),
    /// An installed font family, or any other name.
    Font,
    Text,
    /// A program to run; the installed shells are offered.
    Program,
    /// A WSL distribution; the installed ones are offered.
    WslDistribution,
    /// A list of strings (program arguments).
    StringList,
    /// A subset of the variants in a chosen order, each at most once
    /// except [`SPRING`], which may repeat.
    OrderedSet(&'static [Variant]),
    /// A theme name, or a light/dark pair of them.
    Theme,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Setting {
    /// Dotted path of the key, e.g. `font.size`.
    pub path: &'static str,
    pub section: Section,
    pub label: &'static str,
    pub help: &'static str,
    pub kind: Kind,
    /// For settings that may be left out: what nuntio does then.
    pub unset: Option<&'static str>,
    pub platform: Option<Platform>,
}

impl Setting {
    pub fn keys(&self) -> impl Iterator<Item = &'static str> {
        self.path.split('.')
    }
}

pub static DECORATIONS: &[Variant] = &[
    variant(
        "custom",
        "nuntio draws its own header: the tab bar with the window buttons.",
    ),
    variant("system", "The system's title bar and frame."),
];

pub static MACOS_TITLEBAR: &[Variant] = &[
    variant("native", "Normal title bar."),
    variant(
        "transparent",
        "The tab bar moves into the title bar, next to the traffic lights.",
    ),
    variant("none", "No title bar and no buttons."),
];

pub static TAB_TITLE: &[Variant] = &[
    variant(
        "auto",
        "The directory while the shell waits at its prompt, otherwise the running program.",
    ),
    variant("path", "Always the working directory."),
    variant("process", "Always the running program."),
    variant(
        "application",
        "The title the shell or program sets, unchanged.",
    ),
];

pub static STATUS_BAR_POSITION: &[Variant] = &[
    variant("bottom", "At the bottom edge of the window."),
    variant("top", "Right below the tab bar."),
];

/// The status bar item that is a flexible gap instead of content.
pub const SPRING: &str = "<->";

pub static STATUS_ITEMS: &[Variant] = &[
    variant("cpu", "Usage of all cores over the last minute."),
    variant("memory", "Used memory over the last minute."),
    variant("network", "Download and upload throughput."),
    variant("battery", "Charge level; hidden without a battery."),
    variant("datetime", "The local date and time."),
    variant(
        SPRING,
        "A flexible gap: springs share the free space, pushing the items apart.",
    ),
];

pub static OPTION_AS_META: &[Variant] = &[
    variant("none", "Both Option keys type special characters."),
    variant("left", "The left Option key acts as Meta."),
    variant("right", "The right Option key acts as Meta."),
    variant("both", "Both Option keys act as Meta."),
];

const fn setting(
    path: &'static str,
    section: Section,
    label: &'static str,
    kind: Kind,
    help: &'static str,
) -> Setting {
    Setting {
        path,
        section,
        label,
        help,
        kind,
        unset: None,
        platform: None,
    }
}

const fn optional(mut setting: Setting, unset: &'static str) -> Setting {
    setting.unset = Some(unset);
    setting
}

const fn only_on(mut setting: Setting, platform: Platform) -> Setting {
    setting.platform = Some(platform);
    setting
}

const PADDING: Kind = Kind::Int {
    min: 0,
    max: MAX_PADDING as i64,
    step: 1,
};

/// Every setting except `[[keybindings]]`, in display order.
pub static SETTINGS: &[Setting] = &[
    setting(
        "scrollback",
        Section::General,
        "Scrollback lines",
        Kind::Int {
            min: 0,
            max: MAX_SCROLLBACK,
            step: 1000,
        },
        "Lines of history per pane. Applies to open panes too; a smaller value drops the oldest lines.",
    ),
    setting(
        "clipboard_write",
        Section::General,
        "Clipboard write",
        Kind::Bool,
        "Let programs copy to the clipboard with OSC 52, e.g. vim or tmux over ssh. Programs can never read it.",
    ),
    setting(
        "confirm_paste",
        Section::General,
        "Confirm paste",
        Kind::Bool,
        "Ask before pasting text with line breaks where the shell would run each line at once: paste it again within a few seconds to confirm.",
    ),
    setting(
        "theme",
        Section::Theme,
        "Theme",
        Kind::Theme,
        "A theme, or a light/dark pair that follows the OS appearance. \
         Custom themes go in the themes/ directory next to the config file.",
    ),
    optional(
        setting(
            "font.family",
            Section::Font,
            "Family",
            Kind::Font,
            "Any installed font. Missing glyphs fall back to other installed fonts.",
        ),
        "the system's monospace font",
    ),
    setting(
        "font.size",
        Section::Font,
        "Size",
        Kind::Float {
            min: FONT_SIZE.0,
            max: FONT_SIZE.1,
            step: 0.5,
        },
        "Font size in points.",
    ),
    setting(
        "window.columns",
        Section::Window,
        "Columns",
        Kind::Int {
            min: WINDOW_COLUMNS.0 as i64,
            max: WINDOW_COLUMNS.1 as i64,
            step: 10,
        },
        "Width of a new window, in cells. Applies at the next start.",
    ),
    setting(
        "window.lines",
        Section::Window,
        "Lines",
        Kind::Int {
            min: WINDOW_LINES.0 as i64,
            max: WINDOW_LINES.1 as i64,
            step: 5,
        },
        "Height of a new window, in lines. Applies at the next start.",
    ),
    setting(
        "window.padding.x",
        Section::Window,
        "Horizontal padding",
        PADDING,
        "Space in pixels between the left and right window edges and the text.",
    ),
    setting(
        "window.padding.y",
        Section::Window,
        "Vertical padding",
        PADDING,
        "Space in pixels between the top and bottom window edges and the text.",
    ),
    setting(
        "window.decorations",
        Section::Window,
        "Decorations",
        Kind::Choice(DECORATIONS),
        "Who draws the window frame. Takes effect on the next start.",
    ),
    only_on(
        optional(
            setting(
                "window.macos_titlebar",
                Section::Window,
                "macOS title bar",
                Kind::Choice(MACOS_TITLEBAR),
                "Overrides decorations on macOS.",
            ),
            "derived from decorations",
        ),
        Platform::MacOs,
    ),
    setting(
        "window.opacity",
        Section::Window,
        "Opacity",
        Kind::Float {
            min: OPACITY.0,
            max: OPACITY.1,
            step: 0.05,
        },
        "Opacity of the terminal background, 0.0 (clear) to 1.0 (opaque). Text, colored backgrounds and the bars stay opaque. Going below 1.0 needs a restart unless the window already is transparent.",
    ),
    setting(
        "window.confirm_close",
        Section::Window,
        "Confirm close",
        Kind::Bool,
        "Ask before closing a window, tab or pane in which a program still runs: close it again within a few seconds to confirm.",
    ),
    setting(
        "tabs.hide_when_single",
        Section::Tabs,
        "Hide when single",
        Kind::Bool,
        "Hide the tab bar while only one tab is open. It is always shown when \
         nuntio draws its own header.",
    ),
    setting(
        "tabs.title",
        Section::Tabs,
        "Title",
        Kind::Choice(TAB_TITLE),
        "What a tab shows as its title.",
    ),
    setting(
        "panes.dim_inactive",
        Section::Panes,
        "Dim inactive",
        Kind::Float {
            min: DIM_INACTIVE.0,
            max: DIM_INACTIVE.1,
            step: 0.05,
        },
        "How much to dim panes that don't have focus, 0 is off.",
    ),
    setting(
        "status_bar.enabled",
        Section::StatusBar,
        "Enabled",
        Kind::Bool,
        "Show a bar with live system graphs and the date and time.",
    ),
    setting(
        "status_bar.position",
        Section::StatusBar,
        "Position",
        Kind::Choice(STATUS_BAR_POSITION),
        "Where the status bar sits.",
    ),
    setting(
        "status_bar.items",
        Section::StatusBar,
        "Items",
        Kind::OrderedSet(STATUS_ITEMS),
        "What the bar shows, in this order. Springs (<->) push items apart; without one, the last item sits at the right edge.",
    ),
    setting(
        "status_bar.datetime_format",
        Section::StatusBar,
        "Date format",
        Kind::Text,
        "Format of the date and time in strftime syntax, e.g. \"%d.%m.%Y %H:%M\".",
    ),
    setting(
        "mouse.copy_on_select",
        Section::Mouse,
        "Copy on select",
        Kind::Bool,
        "Copy selected text to the clipboard as soon as you release the mouse button.",
    ),
    only_on(
        setting(
            "macos.option_as_meta",
            Section::MacOs,
            "Option as Meta",
            Kind::Choice(OPTION_AS_META),
            "Which Option keys send Esc + key instead of typing special characters.",
        ),
        Platform::MacOs,
    ),
    optional(
        setting(
            "shell.program",
            Section::Shell,
            "Program",
            Kind::Program,
            "Program to run in new panes. With a WSL distribution, it runs inside it \
             instead of the login shell.",
        ),
        "$SHELL on Linux and macOS, PowerShell on Windows",
    ),
    optional(
        setting(
            "shell.args",
            Section::Shell,
            "Arguments",
            Kind::StringList,
            "Arguments for the program, separated by spaces. Use quotes for \
             arguments with spaces.",
        ),
        "none",
    ),
    only_on(
        optional(
            setting(
                "shell.wsl",
                Section::Shell,
                "WSL distribution",
                Kind::WslDistribution,
                "Start new panes in this WSL distribution, as listed by `wsl -l -v`.",
            ),
            "none",
        ),
        Platform::Windows,
    ),
    only_on(
        optional(
            setting(
                "shell.wsl_user",
                Section::Shell,
                "WSL user",
                Kind::Text,
                "User in the WSL distribution.",
            ),
            "the distribution's default user",
        ),
        Platform::Windows,
    ),
];

pub fn setting_at(path: &str) -> Option<&'static Setting> {
    SETTINGS.iter().find(|s| s.path == path)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::fmt::Debug;

    use serde::Serialize;
    use serde::de::DeserializeOwned;

    use super::*;
    use crate::{
        Config, Decorations, MacosTitlebar, OptionAsMeta, StatusBarPosition, StatusItem, TabTitle,
    };

    fn leaf_paths(value: &toml::Value, prefix: &str, out: &mut BTreeSet<String>) {
        match value {
            toml::Value::Table(table) => {
                for (key, value) in table {
                    let path = if prefix.is_empty() {
                        key.clone()
                    } else {
                        format!("{prefix}.{key}")
                    };
                    leaf_paths(value, &path, out);
                }
            }
            _ => {
                out.insert(prefix.to_owned());
            }
        }
    }

    #[test]
    fn schema_covers_every_default_key() {
        let mut defaults = BTreeSet::new();
        leaf_paths(
            &toml::Value::try_from(Config::default()).unwrap(),
            "",
            &mut defaults,
        );
        defaults.remove("keybindings");
        let required: BTreeSet<String> = SETTINGS
            .iter()
            .filter(|s| s.unset.is_none())
            .map(|s| s.path.to_owned())
            .collect();
        assert_eq!(defaults, required);
    }

    #[test]
    fn optional_settings_are_accepted() {
        let shell = r#"shell = { program = "fish", args = ["-l"] }"#;
        let loaded = crate::parse(&format!(
            "{shell}\n[font]\nfamily = \"Hack\"\n[window]\nmacos_titlebar = \"none\""
        ))
        .unwrap();
        assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);
        let wsl = crate::parse(r#"shell = { wsl = "Ubuntu", wsl_user = "root" }"#).unwrap();
        assert!(wsl.warnings.is_empty());
        for s in SETTINGS.iter().filter(|s| s.unset.is_some()) {
            assert!(
                ["font.family", "window.macos_titlebar"].contains(&s.path)
                    || s.path.starts_with("shell."),
                "untested optional setting {}",
                s.path
            );
        }
    }

    #[test]
    fn paths_are_unique() {
        let paths: BTreeSet<_> = SETTINGS.iter().map(|s| s.path).collect();
        assert_eq!(paths.len(), SETTINGS.len());
    }

    #[test]
    fn every_setting_is_documented() {
        let docs = include_str!("../../../docs/config.md");
        for s in SETTINGS {
            let key = s.path.rsplit('.').next().unwrap();
            let key = if s.path.starts_with("window.padding") {
                "padding"
            } else {
                key
            };
            assert!(
                docs.contains(&format!("`{key}`")),
                "{} is undocumented",
                s.path
            );
        }
    }

    /// The documented default of `path`: the third column of its row in
    /// the table under its section heading.
    fn documented_default<'a>(docs: &'a str, path: &str) -> Option<&'a str> {
        let (heading, key) = match path.split_once('.') {
            Some((section, key)) => (format!("### `[{section}]`"), key),
            None => ("### Top level".to_owned(), path),
        };
        let section = docs.split(&heading).nth(1)?;
        let section = section.split("\n### ").next()?;
        let row = section
            .lines()
            .find(|line| line.starts_with(&format!("| `{key}` |")))?;
        row.split('|').nth(3).map(str::trim)
    }

    #[test]
    fn documented_defaults_match() {
        let docs = include_str!("../../../docs/config.md");
        let defaults = toml::Value::try_from(Config::default()).unwrap();
        let mut checked = 0;
        for s in SETTINGS.iter().filter(|s| s.unset.is_none()) {
            // Documented as one `padding` table.
            if s.path.starts_with("window.padding") {
                continue;
            }
            let value = s
                .path
                .split('.')
                .try_fold(&defaults, |v, key| v.get(key))
                .unwrap();
            let documented = documented_default(docs, s.path);
            match value {
                // Settings are f32, which serialize with f64 noise.
                toml::Value::Float(f) => {
                    let parsed = documented.and_then(|d| d.trim_matches('`').parse::<f32>().ok());
                    assert_eq!(parsed, Some(*f as f32), "{}: {documented:?}", s.path);
                }
                _ => {
                    let expected = format!("`{value}`");
                    assert_eq!(documented, Some(expected.as_str()), "{}", s.path);
                }
            }
            checked += 1;
        }
        assert!(checked > 10);
    }

    fn check<T: Serialize + DeserializeOwned + PartialEq + Debug>(variants: &[Variant], all: &[T]) {
        assert_eq!(variants.len(), all.len());
        for (variant, value) in variants.iter().zip(all) {
            let parsed: T = toml::Value::String(variant.value.into())
                .try_into()
                .unwrap();
            assert_eq!(&parsed, value);
        }
    }

    #[test]
    fn variants_match_the_enums() {
        // The matches fail to compile when a variant is added, as a reminder
        // to list it here and in the schema.
        let _ = |d: Decorations| match d {
            Decorations::Custom | Decorations::System => (),
        };
        check(DECORATIONS, &[Decorations::Custom, Decorations::System]);
        let _ = |t: MacosTitlebar| match t {
            MacosTitlebar::Native | MacosTitlebar::Transparent | MacosTitlebar::None => (),
        };
        check(
            MACOS_TITLEBAR,
            &[
                MacosTitlebar::Native,
                MacosTitlebar::Transparent,
                MacosTitlebar::None,
            ],
        );
        let _ = |t: TabTitle| match t {
            TabTitle::Auto | TabTitle::Path | TabTitle::Process | TabTitle::Application => (),
        };
        check(
            TAB_TITLE,
            &[
                TabTitle::Auto,
                TabTitle::Path,
                TabTitle::Process,
                TabTitle::Application,
            ],
        );
        let _ = |p: StatusBarPosition| match p {
            StatusBarPosition::Bottom | StatusBarPosition::Top => (),
        };
        check(
            STATUS_BAR_POSITION,
            &[StatusBarPosition::Bottom, StatusBarPosition::Top],
        );
        let _ = |i: StatusItem| match i {
            StatusItem::Cpu
            | StatusItem::Memory
            | StatusItem::Network
            | StatusItem::Battery
            | StatusItem::Datetime
            | StatusItem::Spring => (),
        };
        check(
            STATUS_ITEMS,
            &[
                StatusItem::Cpu,
                StatusItem::Memory,
                StatusItem::Network,
                StatusItem::Battery,
                StatusItem::Datetime,
                StatusItem::Spring,
            ],
        );
        let _ = |o: OptionAsMeta| match o {
            OptionAsMeta::None | OptionAsMeta::Left | OptionAsMeta::Right | OptionAsMeta::Both => {}
        };
        check(
            OPTION_AS_META,
            &[
                OptionAsMeta::None,
                OptionAsMeta::Left,
                OptionAsMeta::Right,
                OptionAsMeta::Both,
            ],
        );
    }

    #[test]
    fn every_section_has_settings() {
        for section in Section::ALL {
            let has = SETTINGS.iter().any(|s| s.section == section);
            assert!(has || section == Section::Keybindings, "{section:?}");
        }
    }
}
