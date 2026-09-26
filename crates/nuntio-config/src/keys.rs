//! Key combinations and action names of `[[keybindings]]`, independent of
//! the windowing library so that editors can validate them too.

use std::fmt;

use crate::schema::Variant;

/// A key that has a name rather than a character.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NamedKey {
    Enter,
    Tab,
    Escape,
    Space,
    Backspace,
    Delete,
    Insert,
    Home,
    End,
    PageUp,
    PageDown,
    Up,
    Down,
    Left,
    Right,
    /// F1 to F12.
    F(u8),
}

impl NamedKey {
    fn name(self) -> String {
        match self {
            NamedKey::Enter => "Enter".into(),
            NamedKey::Tab => "Tab".into(),
            NamedKey::Escape => "Escape".into(),
            NamedKey::Space => "Space".into(),
            NamedKey::Backspace => "Backspace".into(),
            NamedKey::Delete => "Delete".into(),
            NamedKey::Insert => "Insert".into(),
            NamedKey::Home => "Home".into(),
            NamedKey::End => "End".into(),
            NamedKey::PageUp => "PageUp".into(),
            NamedKey::PageDown => "PageDown".into(),
            NamedKey::Up => "Up".into(),
            NamedKey::Down => "Down".into(),
            NamedKey::Left => "Left".into(),
            NamedKey::Right => "Right".into(),
            NamedKey::F(n) => format!("F{n}"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyName {
    /// Lowercase; compared case-insensitively.
    Char(char),
    Named(NamedKey),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Mods {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    /// Cmd on macOS, the Windows key elsewhere.
    pub super_key: bool,
}

/// Why a key combination could not be parsed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum KeyComboError {
    #[error("missing key")]
    MissingKey,
    #[error("empty modifier")]
    EmptyModifier,
    #[error("unknown modifier `{0}`")]
    UnknownModifier(String),
    #[error("modifier `{0}` appears twice")]
    DuplicateModifier(String),
    #[error("unknown key `{0}`")]
    UnknownKey(String),
}

/// A parsed key combination like `Ctrl+Shift+T`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KeyCombo {
    pub mods: Mods,
    pub key: KeyName,
}

impl KeyCombo {
    /// Parse a combination like `"Ctrl+Shift+T"` or `"Cmd+PageUp"`.
    /// Case and spaces around `+` don't matter.
    pub fn parse(combo: &str) -> Result<Self, KeyComboError> {
        let parts: Vec<&str> = combo.split('+').map(str::trim).collect();
        let (key, modifiers) = parts.split_last().ok_or(KeyComboError::MissingKey)?;
        let mut mods = Mods::default();
        for m in modifiers {
            let flag = match m.to_lowercase().as_str() {
                "ctrl" | "control" => &mut mods.ctrl,
                "shift" => &mut mods.shift,
                "alt" | "opt" | "option" => &mut mods.alt,
                "cmd" | "command" | "super" | "win" | "meta" => &mut mods.super_key,
                "" => return Err(KeyComboError::EmptyModifier),
                _ => return Err(KeyComboError::UnknownModifier(m.to_string())),
            };
            if *flag {
                return Err(KeyComboError::DuplicateModifier(m.to_string()));
            }
            *flag = true;
        }
        Ok(Self {
            mods,
            key: parse_key(key)?,
        })
    }
}

fn parse_key(key: &str) -> Result<KeyName, KeyComboError> {
    let mut chars = key.chars();
    match (chars.next(), chars.next()) {
        (None, _) => return Err(KeyComboError::MissingKey),
        (Some(c), None) => return Ok(KeyName::Char(c.to_lowercase().next().unwrap_or(c))),
        _ => {}
    }
    let lower = key.to_lowercase();
    let named = match lower.as_str() {
        "plus" => return Ok(KeyName::Char('+')),
        "minus" => return Ok(KeyName::Char('-')),
        "enter" | "return" => NamedKey::Enter,
        "tab" => NamedKey::Tab,
        "escape" | "esc" => NamedKey::Escape,
        "space" => NamedKey::Space,
        "backspace" => NamedKey::Backspace,
        "delete" | "del" => NamedKey::Delete,
        "insert" | "ins" => NamedKey::Insert,
        "home" => NamedKey::Home,
        "end" => NamedKey::End,
        "pageup" | "pgup" => NamedKey::PageUp,
        "pagedown" | "pgdn" => NamedKey::PageDown,
        "up" => NamedKey::Up,
        "down" => NamedKey::Down,
        "left" => NamedKey::Left,
        "right" => NamedKey::Right,
        _ => match lower.strip_prefix('f').and_then(|n| n.parse::<u8>().ok()) {
            Some(n @ 1..=12) => NamedKey::F(n),
            _ => return Err(KeyComboError::UnknownKey(key.to_string())),
        },
    };
    Ok(KeyName::Named(named))
}

/// Canonical spelling, e.g. `Ctrl+Shift+T`.
impl fmt::Display for KeyCombo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Mods {
            ctrl,
            shift,
            alt,
            super_key,
        } = self.mods;
        for (on, name) in [
            (ctrl, "Ctrl"),
            (alt, "Alt"),
            (super_key, "Cmd"),
            (shift, "Shift"),
        ] {
            if on {
                write!(f, "{name}+")?;
            }
        }
        match self.key {
            KeyName::Char('+') => f.write_str("Plus"),
            KeyName::Char('-') => f.write_str("Minus"),
            KeyName::Char(c) => write!(f, "{}", c.to_uppercase()),
            KeyName::Named(named) => f.write_str(&named.name()),
        }
    }
}

const fn action(value: &'static str, help: &'static str) -> Variant {
    Variant { value, help }
}

/// Every action name `[[keybindings]]` accepts.
pub static ACTIONS: &[Variant] = &[
    action("copy", "Copy the selection"),
    action("paste", "Paste from the clipboard"),
    action("new_tab", "Open a new tab in the focused pane's directory"),
    action("close_tab", "Close the current tab with all its panes"),
    action(
        "close_pane",
        "Close the focused pane (and the tab, if it was the last pane)",
    ),
    action("next_tab", "Switch to the next tab"),
    action("previous_tab", "Switch to the previous tab"),
    action("select_tab_1", "Go to tab 1"),
    action("select_tab_2", "Go to tab 2"),
    action("select_tab_3", "Go to tab 3"),
    action("select_tab_4", "Go to tab 4"),
    action("select_tab_5", "Go to tab 5"),
    action("select_tab_6", "Go to tab 6"),
    action("select_tab_7", "Go to tab 7"),
    action("select_tab_8", "Go to tab 8"),
    action("select_tab_9", "Go to tab 9"),
    action("split_vertical", "Split the focused pane side by side"),
    action("split_horizontal", "Split the focused pane top and bottom"),
    action("focus_pane_left", "Move focus to the pane on the left"),
    action("focus_pane_right", "Move focus to the pane on the right"),
    action("focus_pane_up", "Move focus to the pane above"),
    action("focus_pane_down", "Move focus to the pane below"),
    action("resize_pane_left", "Move the focused pane's divider left"),
    action("resize_pane_right", "Move the focused pane's divider right"),
    action("resize_pane_up", "Move the focused pane's divider up"),
    action("resize_pane_down", "Move the focused pane's divider down"),
    action(
        "zoom_pane",
        "Toggle between the focused pane filling the tab and the split layout",
    ),
    action("search", "Open the find bar"),
    action("toggle_fullscreen", "Enter or leave full screen"),
    action("scroll_page_up", "Scroll up by a page"),
    action("scroll_page_down", "Scroll down by a page"),
    action("scroll_line_up", "Scroll up by a line"),
    action("scroll_line_down", "Scroll down by a line"),
    action(
        "increase_font_size",
        "Increase the font size for this session",
    ),
    action(
        "decrease_font_size",
        "Decrease the font size for this session",
    ),
    action(
        "reset_font_size",
        "Reset the font size to the configured one",
    ),
    action("clear_scrollback", "Clear the history of the focused pane"),
    action("reload_config", "Reload the config file"),
    action("open_settings", "Open nuntio-config in a new tab"),
    action(
        "none",
        "Unbind the key combination; it goes to the terminal",
    ),
];

#[cfg(test)]
mod tests {
    use super::*;

    fn combo(s: &str) -> KeyCombo {
        KeyCombo::parse(s).unwrap()
    }

    #[test]
    fn parses_combos() {
        let t = combo("Ctrl+Shift+T");
        assert!(t.mods.ctrl && t.mods.shift && !t.mods.alt && !t.mods.super_key);
        assert_eq!(t.key, KeyName::Char('t'));
        let pgup = combo("cmd + pageup");
        assert!(pgup.mods.super_key);
        assert_eq!(pgup.key, KeyName::Named(NamedKey::PageUp));
        assert_eq!(combo("Alt+Plus").key, KeyName::Char('+'));
        assert_eq!(combo("F5").key, KeyName::Named(NamedKey::F(5)));
    }

    #[test]
    fn rejects_bad_combos() {
        for bad in [
            "",
            "Hyper+T",
            "Ctrl+Foo",
            "F13",
            "F0",
            "Ctrl+",
            "Ctrl++Shift+T",
            "Ctrl+Ctrl+T",
            "Cmd+Super+T",
        ] {
            assert!(KeyCombo::parse(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn canonical_spelling() {
        assert_eq!(combo("shift + ctrl + t").to_string(), "Ctrl+Shift+T");
        assert_eq!(combo("Cmd+Opt+pgdn").to_string(), "Alt+Cmd+PageDown");
        assert_eq!(combo("ctrl+plus").to_string(), "Ctrl+Plus");
        assert_eq!(combo("f12").to_string(), "F12");
        assert_eq!(combo(&combo("Ctrl+Alt+,").to_string()), combo("Ctrl+Alt+,"));
    }

    #[test]
    fn action_names_are_unique() {
        let mut names: Vec<_> = ACTIONS.iter().map(|a| a.value).collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), ACTIONS.len());
    }
}
