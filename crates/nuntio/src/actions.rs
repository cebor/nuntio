//! Keyboard shortcuts: platform defaults plus `[[keybindings]]` from the config.

use nuntio_config::{KeyCombo, KeyName, Mods, NamedKey as Named};
use winit::keyboard::{Key, KeyCode, ModifiersState, NamedKey};

use crate::input;
use crate::pane_tree::Direction;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Copy,
    Paste,
    ScrollPageUp,
    ScrollPageDown,
    ScrollLineUp,
    ScrollLineDown,
    FontIncrease,
    FontDecrease,
    FontReset,
    ClearScrollback,
    NewTab,
    CloseTab,
    /// Close the focused pane (the tab, if it's the last pane).
    ClosePane,
    NextTab,
    PreviousTab,
    /// Activate the tab at this index (0-based).
    SelectTab(usize),
    ReloadConfig,
    /// Split the focused pane side by side.
    SplitVertical,
    /// Split the focused pane top and bottom.
    SplitHorizontal,
    FocusPane(Direction),
    ResizePane(Direction),
    ZoomPane,
    /// Open the find bar.
    Search,
    ToggleFullscreen,
    /// Open `nuntio-config` in a new tab.
    OpenSettings,
    /// Open the notes in a new tab.
    OpenNotes,
    /// Open the notes in a split next to the focused pane.
    OpenNotesSplit,
    /// Look for a newer release now.
    CheckForUpdates,
    /// Open the menu of all actions.
    OpenActionsMenu,
    /// Open the menu of shells for a new tab.
    OpenShellMenu,
    /// Open a new tab with entry N of the open shell menu.
    NewTabWithShell(usize),
    /// Open, or switch to, entry N of the multiplexer sections of the
    /// open shell menu.
    OpenMuxEntry(usize),
    /// Open the list of herdr agents.
    OpenAgentsMenu,
    /// Switch to agent N of the open agents menu.
    FocusAgent(usize),
    /// Open the update menu (tab bar badge, `update` status bar item).
    OpenUpdateMenu,
    /// Download and install the newer release.
    InstallUpdate,
    /// Quit and start the installed update.
    RestartForUpdate,
    /// Open the newer release's page.
    OpenReleaseNotes,
}

/// Every action by its name in the config (`nuntio_config::ACTIONS`
/// documents them).
pub(crate) const NAMES: &[(&str, Action)] = {
    use Action::*;
    use Direction::{Down, Left, Right, Up};
    &[
        ("copy", Copy),
        ("paste", Paste),
        ("scroll_page_up", ScrollPageUp),
        ("scroll_page_down", ScrollPageDown),
        ("scroll_line_up", ScrollLineUp),
        ("scroll_line_down", ScrollLineDown),
        ("increase_font_size", FontIncrease),
        ("decrease_font_size", FontDecrease),
        ("reset_font_size", FontReset),
        ("clear_scrollback", ClearScrollback),
        ("new_tab", NewTab),
        ("close_tab", CloseTab),
        ("next_tab", NextTab),
        ("previous_tab", PreviousTab),
        ("select_tab_1", SelectTab(0)),
        ("select_tab_2", SelectTab(1)),
        ("select_tab_3", SelectTab(2)),
        ("select_tab_4", SelectTab(3)),
        ("select_tab_5", SelectTab(4)),
        ("select_tab_6", SelectTab(5)),
        ("select_tab_7", SelectTab(6)),
        ("select_tab_8", SelectTab(7)),
        ("select_tab_9", SelectTab(8)),
        ("reload_config", ReloadConfig),
        ("open_settings", OpenSettings),
        ("open_notes", OpenNotes),
        ("open_notes_split", OpenNotesSplit),
        ("check_for_updates", CheckForUpdates),
        ("open_actions_menu", OpenActionsMenu),
        ("open_shell_menu", OpenShellMenu),
        ("open_agents_menu", OpenAgentsMenu),
        ("close_pane", ClosePane),
        ("split_vertical", SplitVertical),
        ("split_horizontal", SplitHorizontal),
        ("zoom_pane", ZoomPane),
        ("search", Search),
        ("toggle_fullscreen", ToggleFullscreen),
        ("focus_pane_left", FocusPane(Left)),
        ("focus_pane_right", FocusPane(Right)),
        ("focus_pane_up", FocusPane(Up)),
        ("focus_pane_down", FocusPane(Down)),
        ("resize_pane_left", ResizePane(Left)),
        ("resize_pane_right", ResizePane(Right)),
        ("resize_pane_up", ResizePane(Up)),
        ("resize_pane_down", ResizePane(Down)),
    ]
};

impl Action {
    /// Parse an action name from the config; `"none"` gives `None`.
    pub(crate) fn from_name(name: &str) -> Result<Option<Self>, String> {
        if name == "none" {
            return Ok(None);
        }
        NAMES
            .iter()
            .find(|(n, _)| *n == name)
            .map(|&(_, action)| Some(action))
            .ok_or_else(|| format!("unknown action `{name}`"))
    }

    /// The title menus show for the action; `None` for the actions they
    /// leave out: `select_tab_N` (the tab bar has the tabs), the menu's own
    /// action and the entries of the shell menu.
    pub(crate) fn label(self) -> Option<&'static str> {
        use Direction::{Down, Left, Right, Up};
        Some(match self {
            Self::NewTab => "New Tab",
            Self::OpenShellMenu => "New Tab With…",
            Self::CloseTab => "Close Tab",
            Self::NextTab => "Next Tab",
            Self::PreviousTab => "Previous Tab",
            Self::SplitVertical => "Split Side by Side",
            Self::SplitHorizontal => "Split Top and Bottom",
            Self::ClosePane => "Close Pane",
            Self::ZoomPane => "Zoom Pane",
            Self::FocusPane(Left) => "Focus Pane Left",
            Self::FocusPane(Right) => "Focus Pane Right",
            Self::FocusPane(Up) => "Focus Pane Up",
            Self::FocusPane(Down) => "Focus Pane Down",
            Self::ResizePane(Left) => "Resize Pane Left",
            Self::ResizePane(Right) => "Resize Pane Right",
            Self::ResizePane(Up) => "Resize Pane Up",
            Self::ResizePane(Down) => "Resize Pane Down",
            Self::Copy => "Copy",
            Self::Paste => "Paste",
            Self::Search => "Find…",
            Self::ClearScrollback => "Clear Scrollback",
            Self::FontIncrease => "Bigger",
            Self::FontDecrease => "Smaller",
            Self::FontReset => "Actual Size",
            Self::ToggleFullscreen => "Toggle Full Screen",
            Self::ScrollPageUp => "Scroll Page Up",
            Self::ScrollPageDown => "Scroll Page Down",
            Self::ScrollLineUp => "Scroll Line Up",
            Self::ScrollLineDown => "Scroll Line Down",
            Self::OpenNotes => "Open Notes",
            Self::OpenNotesSplit => "Open Notes in Split",
            Self::OpenSettings => "Settings",
            Self::ReloadConfig => "Reload Config",
            Self::CheckForUpdates => "Check for Updates…",
            Self::OpenAgentsMenu => "Agents…",
            Self::SelectTab(_)
            | Self::OpenActionsMenu
            | Self::NewTabWithShell(_)
            | Self::OpenMuxEntry(_)
            | Self::FocusAgent(_)
            | Self::OpenUpdateMenu
            | Self::InstallUpdate
            | Self::RestartForUpdate
            | Self::OpenReleaseNotes => return None,
        })
    }
}

#[derive(Debug, Clone)]
struct Binding {
    combo: KeyCombo,
    /// `None` disables the key combination (it goes to the terminal).
    action: Option<Action>,
}

#[derive(Debug, Clone)]
pub struct Bindings(Vec<Binding>);

impl Bindings {
    pub fn platform_defaults() -> Self {
        use Action::*;

        let shift = ModifiersState::SHIFT;
        // The "command" modifier: Cmd on macOS, Ctrl (+Shift where it would
        // clash with terminal control keys) elsewhere.
        let (cmd, cmd_shift) = if cfg!(target_os = "macos") {
            (ModifiersState::SUPER, ModifiersState::SUPER)
        } else {
            (
                ModifiersState::CONTROL,
                ModifiersState::CONTROL | ModifiersState::SHIFT,
            )
        };
        let char = |c, mods, action| Binding {
            combo: KeyCombo {
                mods: mods_of(mods),
                key: KeyName::Char(c),
            },
            action: Some(action),
        };
        let named = |key, mods, action| Binding {
            combo: KeyCombo {
                mods: mods_of(mods),
                key: KeyName::Named(key),
            },
            action: Some(action),
        };

        let mut bindings = vec![
            char('c', cmd_shift, Copy),
            char('v', cmd_shift, Paste),
            named(Named::Insert, shift, Paste),
            char('k', cmd_shift, ClearScrollback),
            char('=', cmd, FontIncrease),
            char('+', cmd, FontIncrease),
            char('=', cmd | shift, FontIncrease),
            char('-', cmd, FontDecrease),
            char('0', cmd, FontReset),
            named(Named::PageUp, shift, ScrollPageUp),
            named(Named::PageDown, shift, ScrollPageDown),
            named(Named::Up, cmd_shift, ScrollLineUp),
            named(Named::Down, cmd_shift, ScrollLineDown),
            char('t', cmd_shift, NewTab),
            char('t', cmd_shift | ModifiersState::ALT, OpenShellMenu),
            char('w', cmd_shift, ClosePane),
            char('f', cmd_shift, Search),
            char(',', cmd_shift | shift, ReloadConfig),
            char('n', cmd_shift | shift, OpenNotes),
            char('n', cmd_shift | shift | ModifiersState::ALT, OpenNotesSplit),
        ];
        let (focus_mods, resize_mods) = if cfg!(target_os = "macos") {
            let cmd = ModifiersState::SUPER;
            bindings.push(char(']', cmd | shift, NextTab));
            bindings.push(char('[', cmd | shift, PreviousTab));
            bindings.push(named(Named::Right, cmd, NextTab));
            bindings.push(named(Named::Left, cmd, PreviousTab));
            bindings.push(char('d', cmd, SplitVertical));
            bindings.push(char('d', cmd | shift, SplitHorizontal));
            bindings.push(named(Named::Enter, cmd | shift, ZoomPane));
            // Like iTerm2; the menu also has macOS's Ctrl+Cmd+F.
            bindings.push(named(Named::Enter, cmd, ToggleFullscreen));
            // The settings shortcut of macOS apps.
            bindings.push(char(',', cmd, OpenSettings));
            // Like iTerm2: Cmd+Opt+Arrow focuses, Cmd+Ctrl+Arrow resizes.
            (cmd | ModifiersState::ALT, cmd | ModifiersState::CONTROL)
        } else {
            let ctrl = ModifiersState::CONTROL;
            bindings.push(named(Named::Tab, ctrl, NextTab));
            bindings.push(named(Named::Tab, ctrl | shift, PreviousTab));
            bindings.push(named(Named::PageDown, ctrl, NextTab));
            bindings.push(named(Named::PageUp, ctrl, PreviousTab));
            bindings.push(char('d', ctrl | shift, SplitVertical));
            bindings.push(char('e', ctrl | shift, SplitHorizontal));
            bindings.push(named(Named::Enter, ctrl | shift, ZoomPane));
            bindings.push(named(
                Named::F(11),
                ModifiersState::empty(),
                ToggleFullscreen,
            ));
            // The settings shortcut of Windows Terminal and VS Code.
            bindings.push(char(',', ctrl, OpenSettings));
            let ctrl_alt = ctrl | ModifiersState::ALT;
            (ctrl_alt, ctrl_alt | shift)
        };
        for (key, direction) in [
            (Named::Left, Direction::Left),
            (Named::Right, Direction::Right),
            (Named::Up, Direction::Up),
            (Named::Down, Direction::Down),
        ] {
            bindings.push(named(key, focus_mods, FocusPane(direction)));
            bindings.push(named(key, resize_mods, ResizePane(direction)));
        }
        // Cmd+1…9 on macOS, Alt+1…9 elsewhere.
        let select_mods = if cfg!(target_os = "macos") {
            ModifiersState::SUPER
        } else {
            ModifiersState::ALT
        };
        for (i, digit) in ('1'..='9').enumerate() {
            bindings.push(char(digit, select_mods, SelectTab(i)));
        }
        Self(bindings)
    }

    /// Platform defaults, overridden by the config's `[[keybindings]]`.
    /// Invalid entries are skipped and reported as warnings.
    pub fn from_config(entries: &[nuntio_config::Keybinding]) -> (Self, Vec<String>) {
        let mut warnings = Vec::new();
        let mut custom = Vec::new();
        for entry in entries {
            if entry.key.is_empty() {
                warnings.push(format!("keybinding for \"{}\" has no `key`", entry.action));
                continue;
            }
            if entry.action.is_empty() {
                warnings.push(format!("keybinding \"{}\" has no `action`", entry.key));
                continue;
            }
            let parsed = entry.check().map_err(|e| e.to_string()).and_then(|combo| {
                let action = Action::from_name(&entry.action)?;
                Ok(Binding { combo, action })
            });
            match parsed {
                Ok(binding) => custom.push(binding),
                Err(err) => warnings.push(format!("keybinding \"{}\": {err}", entry.key)),
            }
        }
        // Earlier entries win in `lookup`, so config bindings go first.
        custom.extend(Self::platform_defaults().0);
        (Self(custom), warnings)
    }

    /// The shortcut that runs `action`, spelled like in the config, e.g.
    /// `Ctrl+Shift+T`. Combinations another binding takes first (or that
    /// the config unbinds) don't count.
    pub fn shortcut(&self, action: Action) -> Option<String> {
        self.0.iter().enumerate().find_map(|(i, b)| {
            let shadowed = self.0[..i].iter().any(|earlier| earlier.combo == b.combo);
            if b.action != Some(action) || shadowed {
                return None;
            }
            Some(b.combo.to_string())
        })
    }

    /// `key` should be the key without modifiers applied (so Ctrl+Shift+C
    /// arrives as `c`, not `C` or a control character).
    pub fn lookup(&self, key: &Key, mods: ModifiersState) -> Option<Action> {
        let key = match key {
            Key::Named(named) => KeyName::Named(config_named_key(*named)?),
            Key::Character(s) => {
                let mut chars = s.chars().flat_map(char::to_lowercase);
                let c = chars.next()?;
                chars.next().is_none().then_some(KeyName::Char(c))?
            }
            _ => return None,
        };
        let mods = mods_of(mods);
        self.0
            .iter()
            .find(|b| b.combo.key == key && b.combo.mods == mods)
            .and_then(|b| b.action)
    }
}

/// The modifiers of `mods` that key combinations can name.
fn mods_of(mods: ModifiersState) -> Mods {
    Mods {
        ctrl: mods.control_key(),
        shift: mods.shift_key(),
        alt: mods.alt_key(),
        super_key: mods.super_key(),
    }
}

/// The modifier state that stands for `mods`: the inverse of `mods_of`.
pub(crate) fn modifiers_state(mods: Mods) -> ModifiersState {
    let mut state = ModifiersState::empty();
    for (on, flag) in [
        (mods.ctrl, ModifiersState::CONTROL),
        (mods.shift, ModifiersState::SHIFT),
        (mods.alt, ModifiersState::ALT),
        (mods.super_key, ModifiersState::SUPER),
    ] {
        if on {
            state |= flag;
        }
    }
    state
}

/// The named keys other than F1 to F12 (see `input::function_key`): the
/// config's name, winit's key and the physical key on a US layout.
pub(crate) const NAMED_KEYS: [(Named, NamedKey, KeyCode); 15] = [
    (Named::Enter, NamedKey::Enter, KeyCode::Enter),
    (Named::Tab, NamedKey::Tab, KeyCode::Tab),
    (Named::Escape, NamedKey::Escape, KeyCode::Escape),
    (Named::Space, NamedKey::Space, KeyCode::Space),
    (Named::Backspace, NamedKey::Backspace, KeyCode::Backspace),
    (Named::Delete, NamedKey::Delete, KeyCode::Delete),
    (Named::Insert, NamedKey::Insert, KeyCode::Insert),
    (Named::Home, NamedKey::Home, KeyCode::Home),
    (Named::End, NamedKey::End, KeyCode::End),
    (Named::PageUp, NamedKey::PageUp, KeyCode::PageUp),
    (Named::PageDown, NamedKey::PageDown, KeyCode::PageDown),
    (Named::Up, NamedKey::ArrowUp, KeyCode::ArrowUp),
    (Named::Down, NamedKey::ArrowDown, KeyCode::ArrowDown),
    (Named::Left, NamedKey::ArrowLeft, KeyCode::ArrowLeft),
    (Named::Right, NamedKey::ArrowRight, KeyCode::ArrowRight),
];

/// The config's name for `key`, if it has one.
fn config_named_key(key: NamedKey) -> Option<Named> {
    match NAMED_KEYS.iter().find(|(_, winit, _)| *winit == key) {
        Some(&(config, ..)) => Some(config),
        // The config knows F1 to F12.
        None => Some(Named::F(input::function_number(key).filter(|n| *n <= 12)?)),
    }
}

/// The key a debug server request or a multiplexer binding names, as winit
/// reports it.
pub(crate) fn named_key(key: Named) -> NamedKey {
    match key {
        Named::F(n) => input::function_key(n.clamp(1, 12)).expect("F1 to F12 exist"),
        _ => {
            let (_, winit, _) = NAMED_KEYS
                .iter()
                .find(|(config, ..)| *config == key)
                .expect("every other named key is in NAMED_KEYS");
            *winit
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ch(s: &str) -> Key {
        Key::Character(s.into())
    }

    fn binding(key: &str, action: &str) -> nuntio_config::Keybinding {
        nuntio_config::Keybinding {
            key: key.into(),
            action: action.into(),
        }
    }

    #[test]
    fn action_names() {
        assert_eq!(Action::from_name("new_tab"), Ok(Some(Action::NewTab)));
        assert_eq!(
            Action::from_name("select_tab_3"),
            Ok(Some(Action::SelectTab(2)))
        );
        assert_eq!(Action::from_name("none"), Ok(None));
        assert_eq!(Action::from_name("open_notes"), Ok(Some(Action::OpenNotes)));
        assert_eq!(
            Action::from_name("open_notes_split"),
            Ok(Some(Action::OpenNotesSplit))
        );
        assert_eq!(
            Action::from_name("split_horizontal"),
            Ok(Some(Action::SplitHorizontal))
        );
        assert_eq!(
            Action::from_name("focus_pane_up"),
            Ok(Some(Action::FocusPane(Direction::Up)))
        );
        assert!(Action::from_name("select_tab_0").is_err());
        assert!(Action::from_name("split_sideways").is_err());
    }

    #[test]
    fn the_config_documents_exactly_these_actions() {
        let mut documented: Vec<&str> = nuntio_config::ACTIONS.iter().map(|a| a.value).collect();
        let mut known: Vec<&str> = NAMES.iter().map(|&(name, _)| name).collect();
        known.push("none");
        documented.sort_unstable();
        known.sort_unstable();
        assert_eq!(documented, known);
    }

    #[test]
    fn active_combo_matches_from_config() {
        for (key, action, expected) in [
            ("Ctrl+T", "new_tab", true),
            ("Ctrl+T", "none", true),
            ("Ctrl+T", "nwe_tab", false),
            ("Ctrl+Nope", "new_tab", false),
            ("", "new_tab", false),
            ("Ctrl+T", "", false),
        ] {
            let entry = nuntio_config::Keybinding {
                key: key.into(),
                action: action.into(),
            };
            assert_eq!(
                Bindings::from_config(std::slice::from_ref(&entry))
                    .1
                    .is_empty(),
                entry.active_combo().is_some(),
                "{key:?} {action:?}"
            );
            assert_eq!(entry.active_combo().is_some(), expected);
        }
    }

    #[test]
    fn shortcuts_follow_the_bindings() {
        // The platform's default of new_tab, which the config takes over.
        let new_tab = if cfg!(target_os = "macos") {
            "Cmd+T"
        } else {
            "Ctrl+Shift+T"
        };
        assert_eq!(
            Bindings::platform_defaults()
                .shortcut(Action::NewTab)
                .as_deref(),
            Some(new_tab)
        );
        let (b, _) = Bindings::from_config(&[
            binding(new_tab, "close_tab"),
            binding("F12", "open_actions_menu"),
            binding("Ctrl+Alt+PageDown", "next_tab"),
        ]);
        assert_eq!(b.shortcut(Action::OpenActionsMenu).as_deref(), Some("F12"));
        assert_eq!(b.shortcut(Action::CloseTab).as_deref(), Some(new_tab));
        // Its default combination is taken by close_tab now.
        assert_eq!(b.shortcut(Action::NewTab), None);
        // The config's binding comes before the defaults.
        assert_eq!(
            b.shortcut(Action::NextTab).as_deref(),
            Some("Ctrl+Alt+PageDown")
        );
        assert!(b.shortcut(Action::Search).is_some());
        // Not bound by default.
        let (b, _) = Bindings::from_config(&[]);
        assert_eq!(b.shortcut(Action::OpenActionsMenu), None);
    }

    #[test]
    fn config_overrides_and_disables_defaults() {
        let ctrl_shift = ModifiersState::CONTROL | ModifiersState::SHIFT;
        let (b, warnings) = Bindings::from_config(&[
            binding("Ctrl+Shift+T", "close_tab"),
            binding("Ctrl+Shift+C", "none"),
            binding("F12", "new_tab"),
            binding("Ctrl+Nope", "copy"),
            binding("F11", "fly"),
            binding("", "copy"),
            binding("F10", ""),
        ]);
        assert_eq!(b.lookup(&ch("t"), ctrl_shift), Some(Action::CloseTab));
        assert_eq!(b.lookup(&ch("c"), ctrl_shift), None);
        assert_eq!(
            b.lookup(&Key::Named(NamedKey::F12), ModifiersState::empty()),
            Some(Action::NewTab)
        );
        // Untouched defaults remain.
        assert!(b.lookup(&ch("v"), ctrl_shift).is_some() || cfg!(target_os = "macos"));
        assert_eq!(warnings.len(), 4, "{warnings:?}");
        assert!(warnings[2].contains("no `key`"), "{warnings:?}");
        assert!(warnings[3].contains("no `action`"), "{warnings:?}");
    }

    #[test]
    #[cfg(not(target_os = "macos"))]
    fn linux_windows_defaults() {
        let b = Bindings::platform_defaults();
        let ctrl = ModifiersState::CONTROL;
        let ctrl_shift = ctrl | ModifiersState::SHIFT;
        assert_eq!(b.lookup(&ch("c"), ctrl_shift), Some(Action::Copy));
        assert_eq!(b.lookup(&ch("C"), ctrl_shift), Some(Action::Copy));
        // Plain Ctrl+C must reach the shell.
        assert_eq!(b.lookup(&ch("c"), ctrl), None);
        assert_eq!(b.lookup(&ch("="), ctrl), Some(Action::FontIncrease));
        // Ctrl++ on a US layout: `=` with Shift.
        assert_eq!(b.lookup(&ch("="), ctrl_shift), Some(Action::FontIncrease));
        assert_eq!(
            b.lookup(&Key::Named(NamedKey::Insert), ModifiersState::SHIFT),
            Some(Action::Paste)
        );
        assert_eq!(b.lookup(&ch("t"), ctrl_shift), Some(Action::NewTab));
        assert_eq!(
            b.lookup(&Key::Named(NamedKey::Tab), ctrl | ModifiersState::SHIFT),
            Some(Action::PreviousTab)
        );
        assert_eq!(
            b.lookup(&Key::Named(NamedKey::PageDown), ctrl),
            Some(Action::NextTab)
        );
        assert_eq!(
            b.lookup(&Key::Named(NamedKey::PageUp), ctrl),
            Some(Action::PreviousTab)
        );
        // Shift+PageUp still scrolls.
        assert_eq!(
            b.lookup(&Key::Named(NamedKey::PageUp), ModifiersState::SHIFT),
            Some(Action::ScrollPageUp)
        );
        assert_eq!(
            b.lookup(&ch("3"), ModifiersState::ALT),
            Some(Action::SelectTab(2))
        );
        assert_eq!(b.lookup(&ch("d"), ctrl_shift), Some(Action::SplitVertical));
        assert_eq!(
            b.lookup(&ch("e"), ctrl_shift),
            Some(Action::SplitHorizontal)
        );
        assert_eq!(b.lookup(&ch("w"), ctrl_shift), Some(Action::ClosePane));
        assert_eq!(b.lookup(&ch(","), ctrl), Some(Action::OpenSettings));
        assert_eq!(b.lookup(&ch(","), ctrl_shift), Some(Action::ReloadConfig));
        assert_eq!(b.lookup(&ch("n"), ctrl_shift), Some(Action::OpenNotes));
        assert_eq!(
            b.lookup(&ch("n"), ctrl_shift | ModifiersState::ALT),
            Some(Action::OpenNotesSplit)
        );
        let ctrl_alt = ctrl | ModifiersState::ALT;
        assert_eq!(
            b.lookup(&Key::Named(NamedKey::ArrowLeft), ctrl_alt),
            Some(Action::FocusPane(Direction::Left))
        );
        assert_eq!(
            b.lookup(
                &Key::Named(NamedKey::ArrowDown),
                ctrl_alt | ModifiersState::SHIFT
            ),
            Some(Action::ResizePane(Direction::Down))
        );
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn macos_defaults() {
        let b = Bindings::platform_defaults();
        let cmd = ModifiersState::SUPER;
        assert_eq!(b.lookup(&ch("c"), cmd), Some(Action::Copy));
        assert_eq!(b.lookup(&ch("k"), cmd), Some(Action::ClearScrollback));
        assert_eq!(b.lookup(&ch("c"), ModifiersState::CONTROL), None);
        assert_eq!(b.lookup(&ch("t"), cmd), Some(Action::NewTab));
        assert_eq!(b.lookup(&ch("1"), cmd), Some(Action::SelectTab(0)));
        assert_eq!(
            b.lookup(&Key::Named(NamedKey::ArrowRight), cmd),
            Some(Action::NextTab)
        );
        assert_eq!(
            b.lookup(&Key::Named(NamedKey::ArrowLeft), cmd),
            Some(Action::PreviousTab)
        );
        assert_eq!(
            b.lookup(&Key::Named(NamedKey::ArrowLeft), cmd | ModifiersState::ALT),
            Some(Action::FocusPane(Direction::Left))
        );
        assert_eq!(
            b.lookup(&Key::Named(NamedKey::Enter), cmd),
            Some(Action::ToggleFullscreen)
        );
        assert_eq!(b.lookup(&ch(","), cmd), Some(Action::OpenSettings));
        assert_eq!(
            b.lookup(&ch("n"), cmd | ModifiersState::SHIFT),
            Some(Action::OpenNotes)
        );
        assert_eq!(
            b.lookup(&ch(","), cmd | ModifiersState::SHIFT),
            Some(Action::ReloadConfig)
        );
    }
}
