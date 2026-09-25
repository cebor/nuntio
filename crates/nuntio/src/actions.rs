//! Keyboard shortcuts: platform defaults plus `[[keybindings]]` from the config.

use winit::keyboard::{Key, ModifiersState, NamedKey};

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
}

impl Action {
    /// Parse an action name from the config; `"none"` gives `None`.
    fn from_name(name: &str) -> Result<Option<Self>, String> {
        use Action::*;
        let action = match name {
            "none" => return Ok(None),
            "copy" => Copy,
            "paste" => Paste,
            "scroll_page_up" => ScrollPageUp,
            "scroll_page_down" => ScrollPageDown,
            "scroll_line_up" => ScrollLineUp,
            "scroll_line_down" => ScrollLineDown,
            "increase_font_size" => FontIncrease,
            "decrease_font_size" => FontDecrease,
            "reset_font_size" => FontReset,
            "clear_scrollback" => ClearScrollback,
            "new_tab" => NewTab,
            "close_tab" => CloseTab,
            "next_tab" => NextTab,
            "previous_tab" => PreviousTab,
            "reload_config" => ReloadConfig,
            "close_pane" => ClosePane,
            "split_vertical" => SplitVertical,
            "split_horizontal" => SplitHorizontal,
            "zoom_pane" => ZoomPane,
            "search" => Search,
            "focus_pane_left" => FocusPane(Direction::Left),
            "focus_pane_right" => FocusPane(Direction::Right),
            "focus_pane_up" => FocusPane(Direction::Up),
            "focus_pane_down" => FocusPane(Direction::Down),
            "resize_pane_left" => ResizePane(Direction::Left),
            "resize_pane_right" => ResizePane(Direction::Right),
            "resize_pane_up" => ResizePane(Direction::Up),
            "resize_pane_down" => ResizePane(Direction::Down),
            _ => {
                let tab = name
                    .strip_prefix("select_tab_")
                    .and_then(|n| n.parse::<usize>().ok())
                    .filter(|n| (1..=9).contains(n));
                match tab {
                    Some(n) => SelectTab(n - 1),
                    None => return Err(format!("unknown action `{name}`")),
                }
            }
        };
        Ok(Some(action))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum BindKey {
    /// Compared case-insensitively against the key without modifiers.
    Char(char),
    Named(NamedKey),
}

#[derive(Debug, Clone)]
struct Binding {
    key: BindKey,
    mods: ModifiersState,
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
            key: BindKey::Char(c),
            mods,
            action: Some(action),
        };
        let named = |key, mods, action| Binding {
            key: BindKey::Named(key),
            mods,
            action: Some(action),
        };

        let mut bindings = vec![
            char('c', cmd_shift, Copy),
            char('v', cmd_shift, Paste),
            named(NamedKey::Insert, shift, Paste),
            char('k', cmd_shift, ClearScrollback),
            char('=', cmd, FontIncrease),
            char('+', cmd, FontIncrease),
            char('+', cmd | shift, FontIncrease),
            char('-', cmd, FontDecrease),
            char('0', cmd, FontReset),
            named(NamedKey::PageUp, shift, ScrollPageUp),
            named(NamedKey::PageDown, shift, ScrollPageDown),
            named(NamedKey::ArrowUp, cmd_shift, ScrollLineUp),
            named(NamedKey::ArrowDown, cmd_shift, ScrollLineDown),
            char('t', cmd_shift, NewTab),
            char('w', cmd_shift, ClosePane),
            char('f', cmd_shift, Search),
            char(',', cmd_shift | shift, ReloadConfig),
        ];
        let (focus_mods, resize_mods) = if cfg!(target_os = "macos") {
            let cmd = ModifiersState::SUPER;
            bindings.push(char(']', cmd | shift, NextTab));
            bindings.push(char('[', cmd | shift, PreviousTab));
            bindings.push(named(NamedKey::ArrowRight, cmd, NextTab));
            bindings.push(named(NamedKey::ArrowLeft, cmd, PreviousTab));
            bindings.push(char('d', cmd, SplitVertical));
            bindings.push(char('d', cmd | shift, SplitHorizontal));
            bindings.push(named(NamedKey::Enter, cmd | shift, ZoomPane));
            // Like iTerm2: Cmd+Opt+Arrow focuses, Cmd+Ctrl+Arrow resizes.
            (cmd | ModifiersState::ALT, cmd | ModifiersState::CONTROL)
        } else {
            let ctrl = ModifiersState::CONTROL;
            bindings.push(named(NamedKey::Tab, ctrl, NextTab));
            bindings.push(named(NamedKey::Tab, ctrl | shift, PreviousTab));
            bindings.push(named(NamedKey::PageDown, ctrl, NextTab));
            bindings.push(named(NamedKey::PageUp, ctrl, PreviousTab));
            bindings.push(char('d', ctrl | shift, SplitVertical));
            bindings.push(char('e', ctrl | shift, SplitHorizontal));
            bindings.push(named(NamedKey::Enter, ctrl | shift, ZoomPane));
            let ctrl_alt = ctrl | ModifiersState::ALT;
            (ctrl_alt, ctrl_alt | shift)
        };
        for (key, direction) in [
            (NamedKey::ArrowLeft, Direction::Left),
            (NamedKey::ArrowRight, Direction::Right),
            (NamedKey::ArrowUp, Direction::Up),
            (NamedKey::ArrowDown, Direction::Down),
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
            let parsed = parse_combo(&entry.key).and_then(|(key, mods)| {
                let action = Action::from_name(&entry.action)?;
                Ok(Binding { key, mods, action })
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

    /// `key` should be the key without modifiers applied (so Ctrl+Shift+C
    /// arrives as `c`, not `C` or a control character).
    pub fn lookup(&self, key: &Key, mods: ModifiersState) -> Option<Action> {
        let mods = mods & relevant_mods();
        self.0
            .iter()
            .find(|b| {
                b.mods == mods
                    && match (&b.key, key) {
                        (BindKey::Named(n), Key::Named(k)) => n == k,
                        (BindKey::Char(c), Key::Character(s)) => {
                            let mut chars = s.chars().flat_map(char::to_lowercase);
                            chars.next() == Some(*c) && chars.next().is_none()
                        }
                        _ => false,
                    }
            })
            .and_then(|b| b.action)
    }
}

/// Parse a key combination like `"Ctrl+Shift+T"` or `"Cmd+PageUp"`.
fn parse_combo(combo: &str) -> Result<(BindKey, ModifiersState), String> {
    use nuntio_config::KeyName;

    let combo = nuntio_config::KeyCombo::parse(combo)?;
    let mut mods = ModifiersState::empty();
    for (on, flag) in [
        (combo.mods.ctrl, ModifiersState::CONTROL),
        (combo.mods.shift, ModifiersState::SHIFT),
        (combo.mods.alt, ModifiersState::ALT),
        (combo.mods.super_key, ModifiersState::SUPER),
    ] {
        if on {
            mods |= flag;
        }
    }
    let key = match combo.key {
        KeyName::Char(c) => BindKey::Char(c),
        KeyName::Named(named) => BindKey::Named(named_key(named)),
    };
    Ok((key, mods))
}

fn named_key(key: nuntio_config::NamedKey) -> NamedKey {
    use nuntio_config::NamedKey as N;
    match key {
        N::Enter => NamedKey::Enter,
        N::Tab => NamedKey::Tab,
        N::Escape => NamedKey::Escape,
        N::Space => NamedKey::Space,
        N::Backspace => NamedKey::Backspace,
        N::Delete => NamedKey::Delete,
        N::Insert => NamedKey::Insert,
        N::Home => NamedKey::Home,
        N::End => NamedKey::End,
        N::PageUp => NamedKey::PageUp,
        N::PageDown => NamedKey::PageDown,
        N::Up => NamedKey::ArrowUp,
        N::Down => NamedKey::ArrowDown,
        N::Left => NamedKey::ArrowLeft,
        N::Right => NamedKey::ArrowRight,
        N::F(n) => [
            NamedKey::F1,
            NamedKey::F2,
            NamedKey::F3,
            NamedKey::F4,
            NamedKey::F5,
            NamedKey::F6,
            NamedKey::F7,
            NamedKey::F8,
            NamedKey::F9,
            NamedKey::F10,
            NamedKey::F11,
            NamedKey::F12,
        ][usize::from(n.clamp(1, 12)) - 1],
    }
}

fn relevant_mods() -> ModifiersState {
    ModifiersState::SHIFT | ModifiersState::CONTROL | ModifiersState::ALT | ModifiersState::SUPER
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
    fn combos() {
        let ctrl_shift = ModifiersState::CONTROL | ModifiersState::SHIFT;
        assert_eq!(
            parse_combo("Ctrl+Shift+T").unwrap(),
            (BindKey::Char('t'), ctrl_shift)
        );
        assert_eq!(
            parse_combo("cmd + pageup").unwrap(),
            (BindKey::Named(NamedKey::PageUp), ModifiersState::SUPER)
        );
        assert_eq!(
            parse_combo("Alt+Plus").unwrap(),
            (BindKey::Char('+'), ModifiersState::ALT)
        );
        assert_eq!(parse_combo("F5").unwrap().0, BindKey::Named(NamedKey::F5));
        assert!(parse_combo("Hyper+T").is_err());
        assert!(parse_combo("Ctrl+Foo").is_err());
    }

    #[test]
    fn action_names() {
        assert_eq!(Action::from_name("new_tab"), Ok(Some(Action::NewTab)));
        assert_eq!(
            Action::from_name("select_tab_3"),
            Ok(Some(Action::SelectTab(2)))
        );
        assert_eq!(Action::from_name("none"), Ok(None));
        assert_eq!(
            Action::from_name("split_horizontal"),
            Ok(Some(Action::SplitHorizontal))
        );
        assert_eq!(
            Action::from_name("focus_pane_up"),
            Ok(Some(Action::FocusPane(Direction::Up)))
        );
        assert!(Action::from_name("select_tab_0").is_err());
        for action in nuntio_config::ACTIONS {
            assert!(Action::from_name(action.value).is_ok(), "{}", action.value);
        }
        assert!(Action::from_name("split_sideways").is_err());
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
        ]);
        assert_eq!(b.lookup(&ch("t"), ctrl_shift), Some(Action::CloseTab));
        assert_eq!(b.lookup(&ch("c"), ctrl_shift), None);
        assert_eq!(
            b.lookup(&Key::Named(NamedKey::F12), ModifiersState::empty()),
            Some(Action::NewTab)
        );
        // Untouched defaults remain.
        assert!(b.lookup(&ch("v"), ctrl_shift).is_some() || cfg!(target_os = "macos"));
        assert_eq!(warnings.len(), 2, "{warnings:?}");
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
    }
}
