//! Built-in shortcuts. Config-defined bindings replace/extend these in M4.

use winit::keyboard::{Key, ModifiersState, NamedKey};

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
    NextTab,
    PreviousTab,
    /// Activate the tab at this index (0-based).
    SelectTab(usize),
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
    action: Action,
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
            action,
        };
        let named = |key, mods, action| Binding {
            key: BindKey::Named(key),
            mods,
            action,
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
            char('w', cmd_shift, CloseTab),
        ];
        if cfg!(target_os = "macos") {
            let cmd = ModifiersState::SUPER;
            bindings.push(char(']', cmd | shift, NextTab));
            bindings.push(char('[', cmd | shift, PreviousTab));
        } else {
            let ctrl = ModifiersState::CONTROL;
            bindings.push(named(NamedKey::Tab, ctrl, NextTab));
            bindings.push(named(NamedKey::Tab, ctrl | shift, PreviousTab));
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
            .map(|b| b.action)
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
            b.lookup(&ch("3"), ModifiersState::ALT),
            Some(Action::SelectTab(2))
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
    }
}
