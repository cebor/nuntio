//! The keys that make herdr and tmux act: each action's key sequence as the
//! tool is configured, so nuntio's shortcuts can type them into a pane.

use std::collections::HashMap;

use nuntio_config::{KeyCombo, KeyName, Mods, NamedKey};

use crate::pane_tree::Direction;

/// Something herdr and tmux both do with panes and tabs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MuxAction {
    NewTab,
    /// Side by side.
    SplitVertical,
    /// Stacked.
    SplitHorizontal,
    ClosePane,
    Focus(Direction),
    Resize(Direction),
    Zoom,
}

/// The key sequence of each action. An action without one is unbound.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Bindings(HashMap<MuxAction, Vec<KeyCombo>>);

impl Bindings {
    /// herdr as it comes.
    pub fn herdr_defaults() -> Self {
        herdr_bindings("")
    }

    /// tmux as it comes.
    pub fn tmux_defaults() -> Self {
        let prefix = KeyCombo::parse("ctrl+b").expect("a valid combo");
        let plain = |key: &str| {
            let key = KeyCombo::parse(key).expect("a valid combo");
            vec![prefix, key]
        };
        let char_key = |c: char| {
            vec![
                prefix,
                KeyCombo {
                    mods: Mods::default(),
                    key: KeyName::Char(c),
                },
            ]
        };
        let mut map = HashMap::new();
        map.insert(MuxAction::NewTab, char_key('c'));
        map.insert(MuxAction::SplitVertical, char_key('%'));
        map.insert(MuxAction::SplitHorizontal, char_key('"'));
        map.insert(MuxAction::ClosePane, char_key('x'));
        map.insert(MuxAction::Zoom, char_key('z'));
        for (direction, key) in [
            (Direction::Up, "up"),
            (Direction::Down, "down"),
            (Direction::Left, "left"),
            (Direction::Right, "right"),
        ] {
            map.insert(MuxAction::Focus(direction), plain(key));
        }
        Bindings(map)
    }

    /// The keys to press for `action`.
    pub fn get(&self, action: MuxAction) -> Option<&[KeyCombo]> {
        self.0.get(&action).map(Vec::as_slice)
    }
}

/// herdr's names of the keys and their defaults, as in its `[keys]` table.
const HERDR_KEYS: [(&str, MuxAction, &str); 12] = [
    ("new_tab", MuxAction::NewTab, "prefix+c"),
    ("split_vertical", MuxAction::SplitVertical, "prefix+v"),
    (
        "split_horizontal",
        MuxAction::SplitHorizontal,
        "prefix+minus",
    ),
    ("close_pane", MuxAction::ClosePane, "prefix+x"),
    (
        "focus_pane_left",
        MuxAction::Focus(Direction::Left),
        "prefix+h",
    ),
    (
        "focus_pane_right",
        MuxAction::Focus(Direction::Right),
        "prefix+l",
    ),
    ("focus_pane_up", MuxAction::Focus(Direction::Up), "prefix+k"),
    (
        "focus_pane_down",
        MuxAction::Focus(Direction::Down),
        "prefix+j",
    ),
    ("resize_pane_left", MuxAction::Resize(Direction::Left), ""),
    ("resize_pane_right", MuxAction::Resize(Direction::Right), ""),
    ("resize_pane_up", MuxAction::Resize(Direction::Up), ""),
    ("resize_pane_down", MuxAction::Resize(Direction::Down), ""),
];

const HERDR_ZOOM: (&str, MuxAction, &str) = ("zoom", MuxAction::Zoom, "prefix+z");

/// A `[keys]` value: a string, or the first string of an array.
fn herdr_value(value: &toml::Value) -> Option<&str> {
    match value {
        toml::Value::String(s) => Some(s),
        toml::Value::Array(items) => items.first()?.as_str(),
        _ => None,
    }
}

/// herdr's names of punctuation, as the config syntax knows them.
fn herdr_key_name(name: &str) -> &str {
    match name {
        "comma" => ",",
        "period" => ".",
        "ampersand" => "&",
        "backtick" => "`",
        "semicolon" => ";",
        "slash" => "/",
        "backslash" => "\\",
        "quote" => "'",
        "doublequote" => "\"",
        "bracketleft" => "[",
        "bracketright" => "]",
        other => other,
    }
}

/// A herdr key such as `ctrl+b` or `shift+comma`.
fn herdr_combo(keys: &str) -> Option<KeyCombo> {
    let translated: Vec<&str> = keys.split('+').map(herdr_key_name).collect();
    KeyCombo::parse(&translated.join("+")).ok()
}

/// The keys of a herdr binding: `prefix+x` is the prefix, then `x`.
fn herdr_sequence(value: &str, prefix: Option<KeyCombo>) -> Option<Vec<KeyCombo>> {
    match value.trim().strip_prefix("prefix+") {
        Some(rest) => Some(vec![prefix?, herdr_combo(rest)?]),
        None => Some(vec![herdr_combo(value.trim())?]),
    }
}

/// The bindings of herdr's `config.toml`: its `[keys]` table over the
/// defaults. Invalid TOML gives the defaults; a value that isn't a key
/// leaves the action unbound.
pub fn herdr_bindings(config_toml: &str) -> Bindings {
    let config: toml::Table = config_toml.parse().unwrap_or_default();
    let keys = config.get("keys").and_then(toml::Value::as_table);
    let value = |name: &str, default: &str| {
        keys.and_then(|keys| keys.get(name))
            .map_or(Some(default), herdr_value)
            .map(str::to_owned)
    };
    let prefix = value("prefix", "ctrl+b").and_then(|p| herdr_combo(&p));
    let mut map = HashMap::new();
    for (name, action, default) in HERDR_KEYS.into_iter().chain([HERDR_ZOOM]) {
        let sequence = value(name, default)
            .filter(|v| !v.trim().is_empty())
            .and_then(|v| herdr_sequence(&v, prefix));
        if let Some(sequence) = sequence {
            map.insert(action, sequence);
        }
    }
    Bindings(map)
}

/// tmux's name of a key: `C-b`, `M-Up`, `%`, `F5`.
fn tmux_key(name: &str) -> Option<KeyCombo> {
    let mut mods = Mods::default();
    let mut rest = name;
    while rest.len() > 2 {
        let flag = match &rest[..2] {
            "C-" => &mut mods.ctrl,
            "M-" => &mut mods.alt,
            "S-" => &mut mods.shift,
            _ => break,
        };
        *flag = true;
        rest = &rest[2..];
    }
    let named = match rest {
        "Up" => Some(NamedKey::Up),
        "Down" => Some(NamedKey::Down),
        "Left" => Some(NamedKey::Left),
        "Right" => Some(NamedKey::Right),
        "Space" => Some(NamedKey::Space),
        "Enter" => Some(NamedKey::Enter),
        "Tab" => Some(NamedKey::Tab),
        "Escape" => Some(NamedKey::Escape),
        "BSpace" => Some(NamedKey::Backspace),
        "PPage" => Some(NamedKey::PageUp),
        "NPage" => Some(NamedKey::PageDown),
        "Home" => Some(NamedKey::Home),
        "End" => Some(NamedKey::End),
        "IC" => Some(NamedKey::Insert),
        "DC" => Some(NamedKey::Delete),
        _ => rest
            .strip_prefix('F')
            .and_then(|n| n.parse::<u8>().ok())
            .filter(|n| (1..=12).contains(n))
            .map(NamedKey::F),
    };
    if let Some(named) = named {
        return Some(KeyCombo {
            mods,
            key: KeyName::Named(named),
        });
    }
    if rest == "BTab" {
        mods.shift = true;
        return Some(KeyCombo {
            mods,
            key: KeyName::Named(NamedKey::Tab),
        });
    }
    let mut chars = rest.chars();
    let c = chars.next()?;
    if chars.next().is_some() {
        return None;
    }
    // tmux writes Shift+a as `A`.
    if c.is_uppercase() {
        mods.shift = true;
    }
    Some(KeyCombo {
        mods,
        key: KeyName::Char(c.to_lowercase().next().unwrap_or(c)),
    })
}

/// Split a line like a shell would, but keep a backslash outside quotes
/// (with the character it escapes): tmux writes the key `"` as `\"`.
fn tokenize(line: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut token = String::new();
    let mut started = false;
    let mut quote: Option<char> = None;
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match quote {
            Some(q) if c == q => quote = None,
            Some('"') if c == '\\' => token.extend(chars.next()),
            Some(_) => token.push(c),
            None if c.is_whitespace() => {
                if started {
                    tokens.push(std::mem::take(&mut token));
                    started = false;
                }
            }
            None if c == '"' || c == '\'' => {
                quote = Some(c);
                started = true;
            }
            None if c == '\\' => {
                token.push(c);
                token.extend(chars.next());
                started = true;
            }
            None => {
                token.push(c);
                started = true;
            }
        }
    }
    if started {
        tokens.push(token);
    }
    tokens
}

/// Whether `command` has the flag `flag` (`-h`), alone or among others.
fn has_flag(command: &[String], flag: char) -> bool {
    command.iter().skip(1).any(|t| {
        t.strip_prefix('-')
            .is_some_and(|f| !f.is_empty() && f.chars().all(|c| c.is_ascii_alphabetic()))
            && t.contains(flag)
    })
}

/// The direction a pane command's `-U`, `-D`, `-L` or `-R` names.
fn direction_flag(command: &[String]) -> Option<Direction> {
    command.iter().skip(1).find_map(|t| match t.as_str() {
        "-U" => Some(Direction::Up),
        "-D" => Some(Direction::Down),
        "-L" => Some(Direction::Left),
        "-R" => Some(Direction::Right),
        _ => None,
    })
}

/// The action a tmux command stands for. Commands that only mention
/// others (menus, prompts) stand for none.
fn tmux_action(command: &[String]) -> Option<MuxAction> {
    match command.first()?.as_str() {
        "new-window" => Some(MuxAction::NewTab),
        "split-window" if has_flag(command, 'h') => Some(MuxAction::SplitVertical),
        "split-window" => Some(MuxAction::SplitHorizontal),
        "kill-pane" => Some(MuxAction::ClosePane),
        // `confirm-before -p "kill-pane #P? (y/n)" kill-pane`
        "confirm-before" => tmux_action(&tokenize(command.last()?)),
        "select-pane" => direction_flag(command).map(MuxAction::Focus),
        "resize-pane" if has_flag(command, 'Z') => Some(MuxAction::Zoom),
        "resize-pane" => direction_flag(command).map(MuxAction::Resize),
        _ => None,
    }
}

/// The bindings of a tmux server: `prefix` is the output of `show-options
/// -gv prefix` and `list_keys` that of `list-keys -T prefix`. Without a
/// prefix key, nothing is bound.
pub fn tmux_bindings(prefix: &str, list_keys: &str) -> Bindings {
    let Some(prefix) = tmux_key(prefix.trim()) else {
        return Bindings::default();
    };
    // Whether the binding found for an action repeats.
    let mut map: HashMap<MuxAction, (Vec<KeyCombo>, bool)> = HashMap::new();
    for line in list_keys.lines() {
        let tokens = tokenize(line);
        let mut tokens = tokens.iter().map(String::as_str);
        if tokens.next() != Some("bind-key") {
            continue;
        }
        let mut repeat = false;
        let mut key = None;
        while let Some(token) = tokens.next() {
            match token {
                "-r" => repeat = true,
                "-T" | "-N" => {
                    tokens.next();
                }
                flag if flag.starts_with('-') && flag.len() > 1 => {}
                _ => {
                    key = Some(token);
                    break;
                }
            }
        }
        let Some(key) = key else { continue };
        let command: Vec<String> = tokens.map(str::to_owned).collect();
        let Some(action) = tmux_action(&command) else {
            continue;
        };
        let name = key
            .strip_prefix('\\')
            .filter(|k| !k.is_empty())
            .unwrap_or(key);
        let Some(key) = tmux_key(name) else { continue };
        // The first binding wins, but resizing prefers one that repeats.
        let better = match map.get(&action) {
            None => true,
            Some((_, found_repeat)) => {
                matches!(action, MuxAction::Resize(_)) && repeat && !found_repeat
            }
        };
        if better {
            map.insert(action, (vec![prefix, key], repeat));
        }
    }
    Bindings(map.into_iter().map(|(a, (keys, _))| (a, keys)).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn combo(s: &str) -> KeyCombo {
        KeyCombo::parse(s).unwrap()
    }

    fn chars(c: char) -> KeyCombo {
        KeyCombo {
            mods: Mods::default(),
            key: KeyName::Char(c),
        }
    }

    fn keys(b: &Bindings, a: MuxAction) -> Vec<KeyCombo> {
        b.get(a).expect("bound").to_vec()
    }

    #[test]
    fn herdr_defaults_follow_its_documentation() {
        let b = Bindings::herdr_defaults();
        let ctrl_b = combo("ctrl+b");
        assert_eq!(keys(&b, MuxAction::NewTab), [ctrl_b, chars('c')]);
        assert_eq!(keys(&b, MuxAction::SplitVertical), [ctrl_b, chars('v')]);
        assert_eq!(keys(&b, MuxAction::SplitHorizontal), [ctrl_b, chars('-')]);
        assert_eq!(keys(&b, MuxAction::ClosePane), [ctrl_b, chars('x')]);
        assert_eq!(keys(&b, MuxAction::Zoom), [ctrl_b, chars('z')]);
        assert_eq!(
            keys(&b, MuxAction::Focus(Direction::Left)),
            [ctrl_b, chars('h')]
        );
        assert_eq!(
            keys(&b, MuxAction::Focus(Direction::Down)),
            [ctrl_b, chars('j')]
        );
        assert_eq!(
            keys(&b, MuxAction::Focus(Direction::Up)),
            [ctrl_b, chars('k')]
        );
        assert_eq!(
            keys(&b, MuxAction::Focus(Direction::Right)),
            [ctrl_b, chars('l')]
        );
        assert_eq!(b.get(MuxAction::Resize(Direction::Left)), None);
        assert_eq!(
            b,
            herdr_bindings("onboarding = false\n[ui.toast]\ndelivery = \"off\"\n")
        );
        assert_eq!(b, herdr_bindings("this is [not toml"));
    }

    #[test]
    fn herdr_config_overrides_the_defaults() {
        let b = herdr_bindings(
            r#"
[keys]
prefix = ["ctrl+a", "f12"]
split_horizontal = "prefix+minus"
close_pane = ""
new_tab = ["prefix+shift+t", "ctrl+t"]
split_vertical = "ctrl+backslash"
resize_pane_left = "prefix+comma"
focus_pane_up = 5
zoom = "prefix+nonsense"
"#,
        );
        let ctrl_a = combo("ctrl+a");
        assert_eq!(b.get(MuxAction::ClosePane), None);
        assert_eq!(keys(&b, MuxAction::SplitHorizontal), [ctrl_a, chars('-')]);
        assert_eq!(keys(&b, MuxAction::NewTab), [ctrl_a, combo("shift+t")]);
        assert_eq!(keys(&b, MuxAction::SplitVertical), [combo("ctrl+\\")]);
        assert_eq!(
            keys(&b, MuxAction::Resize(Direction::Left)),
            [ctrl_a, chars(',')]
        );
        assert_eq!(b.get(MuxAction::Focus(Direction::Up)), None);
        assert_eq!(b.get(MuxAction::Zoom), None);
        assert_eq!(
            keys(&b, MuxAction::Focus(Direction::Left)),
            [ctrl_a, chars('h')]
        );
    }

    #[test]
    fn an_unusable_herdr_prefix_unbinds_prefixed_actions() {
        let b = herdr_bindings("[keys]\nprefix = \"bogus+\"\nclose_pane = \"ctrl+q\"\n");
        assert_eq!(b.get(MuxAction::NewTab), None);
        assert_eq!(keys(&b, MuxAction::ClosePane), [combo("ctrl+q")]);
    }

    /// `tmux list-keys -T prefix` of tmux 3.6 (the lines that matter).
    const LIST_KEYS: &str = r##"bind-key    -T prefix Space   next-layout
bind-key    -T prefix !       break-pane
bind-key    -T prefix \"      split-window
bind-key    -T prefix \#      list-buffers
bind-key    -T prefix \$      command-prompt -I "#S" { rename-session "%%" }
bind-key    -T prefix \%      split-window -h
bind-key    -T prefix &       confirm-before -p "kill-window #W? (y/n)" kill-window
bind-key    -T prefix <       display-menu -T "#[align=centre]#{window_index}:#{window_name}" -x W -y W Kill X { kill-window } "New At End" W { new-window } Split | { split-window -h } Close x { kill-pane }
bind-key    -T prefix c       new-window
bind-key    -T prefix x       confirm-before -p "kill-pane #P? (y/n)" kill-pane
bind-key    -T prefix z       resize-pane -Z
bind-key    -T prefix \{      swap-pane -U
bind-key -r -T prefix Up      select-pane -U
bind-key -r -T prefix Down    select-pane -D
bind-key -r -T prefix Left    select-pane -L
bind-key -r -T prefix Right   select-pane -R
bind-key -r -T prefix M-Up    resize-pane -U 5
bind-key -r -T prefix M-Down  resize-pane -D 5
bind-key -r -T prefix M-Left  resize-pane -L 5
bind-key -r -T prefix M-Right resize-pane -R 5
bind-key -r -T prefix C-Up    resize-pane -U
bind-key -r -T prefix C-Down  resize-pane -D
bind-key -r -T prefix S-Up    refresh-client -U 10
"##;

    #[test]
    fn tmux_defaults_come_out_of_list_keys() {
        let b = tmux_bindings("C-b\n", LIST_KEYS);
        let ctrl_b = combo("ctrl+b");
        assert_eq!(keys(&b, MuxAction::NewTab), [ctrl_b, chars('c')]);
        assert_eq!(keys(&b, MuxAction::SplitVertical), [ctrl_b, chars('%')]);
        assert_eq!(keys(&b, MuxAction::SplitHorizontal), [ctrl_b, chars('"')]);
        assert_eq!(keys(&b, MuxAction::ClosePane), [ctrl_b, chars('x')]);
        assert_eq!(keys(&b, MuxAction::Zoom), [ctrl_b, chars('z')]);
        assert_eq!(
            keys(&b, MuxAction::Focus(Direction::Up)),
            [ctrl_b, combo("up")]
        );
        assert_eq!(
            keys(&b, MuxAction::Focus(Direction::Right)),
            [ctrl_b, combo("right")]
        );
        // The first of two repeating bindings.
        assert_eq!(
            keys(&b, MuxAction::Resize(Direction::Up)),
            [ctrl_b, combo("alt+up")]
        );
        assert_eq!(
            keys(&b, MuxAction::Resize(Direction::Left)),
            [ctrl_b, combo("alt+left")]
        );
        // The same bindings the defaults name.
        let defaults = Bindings::tmux_defaults();
        for action in [
            MuxAction::NewTab,
            MuxAction::SplitVertical,
            MuxAction::SplitHorizontal,
            MuxAction::ClosePane,
            MuxAction::Zoom,
            MuxAction::Focus(Direction::Left),
        ] {
            assert_eq!(b.get(action), defaults.get(action));
        }
    }

    #[test]
    fn tmux_prefix_and_custom_keys() {
        let list = "bind-key -T prefix | split-window -h -c \"#{pane_current_path}\"\n\
                    bind-key -T prefix - split-window -v\n\
                    bind-key -T prefix C-t new-window\n\
                    bind-key -T prefix h select-pane -L\n\
                    bind-key -T prefix \\\\ kill-pane\n\
                    bind-key -T prefix R resize-pane -R 2\n\
                    bind-key -r -T prefix F5 resize-pane -R 9\n";
        let b = tmux_bindings("C-a", list);
        let ctrl_a = combo("ctrl+a");
        assert_eq!(keys(&b, MuxAction::SplitVertical), [ctrl_a, chars('|')]);
        assert_eq!(keys(&b, MuxAction::SplitHorizontal), [ctrl_a, chars('-')]);
        assert_eq!(keys(&b, MuxAction::NewTab), [ctrl_a, combo("ctrl+t")]);
        assert_eq!(
            keys(&b, MuxAction::Focus(Direction::Left)),
            [ctrl_a, chars('h')]
        );
        assert_eq!(keys(&b, MuxAction::ClosePane), [ctrl_a, chars('\\')]);
        // A repeating binding beats the first.
        assert_eq!(
            keys(&b, MuxAction::Resize(Direction::Right)),
            [ctrl_a, combo("f5")]
        );
    }

    #[test]
    fn tmux_without_a_usable_prefix_binds_nothing() {
        assert_eq!(tmux_bindings("None", LIST_KEYS), Bindings::default());
        assert_eq!(tmux_bindings("", LIST_KEYS), Bindings::default());
        assert_eq!(tmux_bindings("C-b", ""), Bindings::default());
    }

    #[test]
    fn tmux_key_names() {
        assert_eq!(tmux_key("C-b"), Some(combo("ctrl+b")));
        assert_eq!(tmux_key("C-M-x"), Some(combo("ctrl+alt+x")));
        assert_eq!(tmux_key("S-Up"), Some(combo("shift+up")));
        assert_eq!(tmux_key("PPage"), Some(combo("pageup")));
        assert_eq!(tmux_key("NPage"), Some(combo("pagedown")));
        assert_eq!(tmux_key("BSpace"), Some(combo("backspace")));
        assert_eq!(tmux_key("F12"), Some(combo("f12")));
        assert_eq!(tmux_key("F13"), None);
        assert_eq!(tmux_key("C--"), Some(combo("ctrl+minus")));
        assert_eq!(tmux_key("A"), Some(combo("shift+a")));
        assert_eq!(tmux_key("%"), Some(chars('%')));
        assert_eq!(tmux_key("Bogus"), None);
        assert_eq!(tmux_key(""), None);
    }

    #[test]
    fn tokens_keep_escaped_keys() {
        assert_eq!(
            tokenize(r##"bind-key -T prefix \" split-window -c "#{a b}""##),
            [
                "bind-key",
                "-T",
                "prefix",
                "\\\"",
                "split-window",
                "-c",
                "#{a b}"
            ]
        );
        assert_eq!(tokenize("a 'b c' \"d\\\"e\""), ["a", "b c", "d\"e"]);
    }
}
