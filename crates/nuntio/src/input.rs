//! Keyboard input to PTY bytes, following xterm's encoding: modifier
//! parameters on special keys (`CSI 1;<m> A`), SS3/CSI function keys,
//! application cursor (DECCKM) and keypad (DECKPAM) modes, and Alt as an
//! ESC prefix.

use nuntio_term::TermMode;
use winit::keyboard::{Key, KeyLocation, NamedKey};

/// The parts of a key press the encoder needs.
#[derive(Debug, Clone)]
pub struct KeyInput<'a> {
    pub key: &'a Key,
    /// The key with no modifiers applied (e.g. `c` for Ctrl+Shift+C).
    pub unmodified: &'a Key,
    /// Text produced by the key, if any.
    pub text: Option<&'a str>,
    pub location: KeyLocation,
    pub shift: bool,
    pub ctrl: bool,
    /// Alt acts as Meta. On macOS only when Option-as-Meta is enabled for
    /// the pressed side; otherwise Option composes characters.
    pub meta: bool,
}

impl KeyInput<'_> {
    /// xterm modifier parameter: 1 + Shift(1) + Alt(2) + Ctrl(4).
    fn modifier_param(&self) -> u8 {
        1 + self.shift as u8 + 2 * self.meta as u8 + 4 * self.ctrl as u8
    }
}

pub fn encode_key(input: &KeyInput, mode: TermMode) -> Option<Vec<u8>> {
    if input.location == KeyLocation::Numpad
        && mode.contains(TermMode::APP_KEYPAD)
        && let Some(bytes) = encode_app_keypad(input)
    {
        return Some(bytes);
    }
    if let Key::Named(named) = input.key
        && let Some(bytes) = encode_named(*named, input, mode)
    {
        return Some(bytes);
    }

    if input.ctrl
        && let Key::Character(s) = input.unmodified
        && let Some(byte) = control_byte(s)
    {
        return Some(with_meta(vec![byte], input.meta));
    }

    if input.meta {
        // Meta sends ESC plus the unmodified character (so Option+f on macOS
        // becomes ESC f instead of ƒ).
        let text = match input.unmodified {
            Key::Character(s) if !input.shift => s.as_str(),
            _ => input.text?,
        };
        return Some(with_meta(text.as_bytes().to_vec(), true));
    }

    let text = input.text.filter(|t| !t.is_empty())?;
    Some(text.as_bytes().to_vec())
}

fn with_meta(mut bytes: Vec<u8>, meta: bool) -> Vec<u8> {
    if meta {
        bytes.insert(0, 0x1b);
    }
    bytes
}

fn encode_named(key: NamedKey, input: &KeyInput, mode: TermMode) -> Option<Vec<u8>> {
    let m = input.modifier_param();
    let plain = m == 1;

    // Cursor keys: SS3 in application mode, CSI otherwise; CSI 1;m with modifiers.
    let cursor_final = match key {
        NamedKey::ArrowUp => Some(b'A'),
        NamedKey::ArrowDown => Some(b'B'),
        NamedKey::ArrowRight => Some(b'C'),
        NamedKey::ArrowLeft => Some(b'D'),
        NamedKey::Home => Some(b'H'),
        NamedKey::End => Some(b'F'),
        _ => None,
    };
    if let Some(f) = cursor_final {
        return Some(if !plain {
            format!("\x1b[1;{m}{}", f as char).into_bytes()
        } else if mode.contains(TermMode::APP_CURSOR) {
            vec![0x1b, b'O', f]
        } else {
            vec![0x1b, b'[', f]
        });
    }

    // F1-F4: SS3 P-S, or CSI 1;m P-S with modifiers.
    let ss3_final = match key {
        NamedKey::F1 => Some(b'P'),
        NamedKey::F2 => Some(b'Q'),
        NamedKey::F3 => Some(b'R'),
        NamedKey::F4 => Some(b'S'),
        _ => None,
    };
    if let Some(f) = ss3_final {
        return Some(if plain {
            vec![0x1b, b'O', f]
        } else {
            format!("\x1b[1;{m}{}", f as char).into_bytes()
        });
    }

    // Keys encoded as CSI n ~ / CSI n;m ~.
    let tilde = match key {
        NamedKey::Insert => Some(2),
        NamedKey::Delete => Some(3),
        NamedKey::PageUp => Some(5),
        NamedKey::PageDown => Some(6),
        NamedKey::F5 => Some(15),
        NamedKey::F6 => Some(17),
        NamedKey::F7 => Some(18),
        NamedKey::F8 => Some(19),
        NamedKey::F9 => Some(20),
        NamedKey::F10 => Some(21),
        NamedKey::F11 => Some(23),
        NamedKey::F12 => Some(24),
        _ => None,
    };
    if let Some(n) = tilde {
        return Some(if plain {
            format!("\x1b[{n}~").into_bytes()
        } else {
            format!("\x1b[{n};{m}~").into_bytes()
        });
    }

    let meta = input.meta;
    let bytes = match key {
        NamedKey::Enter => with_meta(b"\r".to_vec(), meta),
        NamedKey::Tab if input.shift => b"\x1b[Z".to_vec(),
        NamedKey::Tab => with_meta(b"\t".to_vec(), meta),
        NamedKey::Backspace if input.ctrl => with_meta(vec![0x08], meta),
        NamedKey::Backspace => with_meta(vec![0x7f], meta),
        NamedKey::Escape => with_meta(vec![0x1b], meta),
        NamedKey::Space if input.ctrl => with_meta(vec![0x00], meta),
        NamedKey::Space => with_meta(b" ".to_vec(), meta),
        _ => return None,
    };
    Some(bytes)
}

/// Keypad keys in application keypad mode (DECKPAM) send SS3 sequences.
fn encode_app_keypad(input: &KeyInput) -> Option<Vec<u8>> {
    let f = match input.key {
        Key::Named(NamedKey::Enter) => b'M',
        Key::Character(s) => match s.as_str() {
            c @ ("0" | "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9") => {
                b'p' + (c.as_bytes()[0] - b'0')
            }
            "." | "," => b'n',
            "+" => b'k',
            "-" => b'm',
            "*" => b'j',
            "/" => b'o',
            "=" => b'X',
            _ => return None,
        },
        _ => return None,
    };
    Some(vec![0x1b, b'O', f])
}

/// Ctrl+<key> as a C0 control character.
fn control_byte(s: &str) -> Option<u8> {
    let mut chars = s.chars();
    let c = chars.next()?;
    if chars.next().is_some() {
        return None;
    }
    match c.to_ascii_lowercase() {
        c @ 'a'..='z' => Some(c as u8 - b'a' + 1),
        '@' | '2' => Some(0x00),
        '[' | '3' => Some(0x1b),
        '\\' | '4' => Some(0x1c),
        ']' | '5' => Some(0x1d),
        '^' | '6' => Some(0x1e),
        '_' | '-' | '7' => Some(0x1f),
        '?' | '8' => Some(0x7f),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Copy, Default)]
    struct Mods {
        shift: bool,
        ctrl: bool,
        meta: bool,
    }

    const NONE: Mods = Mods {
        shift: false,
        ctrl: false,
        meta: false,
    };
    const SHIFT: Mods = Mods {
        shift: true,
        ..NONE
    };
    const CTRL: Mods = Mods { ctrl: true, ..NONE };
    const META: Mods = Mods { meta: true, ..NONE };

    fn encode_with(
        key: Key,
        unmodified: Key,
        text: Option<&str>,
        location: KeyLocation,
        mods: Mods,
        mode: TermMode,
    ) -> Option<Vec<u8>> {
        let input = KeyInput {
            key: &key,
            unmodified: &unmodified,
            text,
            location,
            shift: mods.shift,
            ctrl: mods.ctrl,
            meta: mods.meta,
        };
        encode_key(&input, mode)
    }

    fn named(key: NamedKey, mods: Mods, mode: TermMode) -> Vec<u8> {
        let key = Key::Named(key);
        encode_with(key.clone(), key, None, KeyLocation::Standard, mods, mode).unwrap()
    }

    fn char_key(c: &str, text: Option<&str>, mods: Mods) -> Option<Vec<u8>> {
        let key = Key::Character(c.into());
        encode_with(
            key.clone(),
            key,
            text,
            KeyLocation::Standard,
            mods,
            TermMode::empty(),
        )
    }

    #[test]
    fn cursor_keys() {
        let normal = TermMode::empty();
        let app = TermMode::APP_CURSOR;
        let table: &[(NamedKey, Mods, TermMode, &[u8])] = &[
            (NamedKey::ArrowUp, NONE, normal, b"\x1b[A"),
            (NamedKey::ArrowUp, NONE, app, b"\x1bOA"),
            (NamedKey::ArrowLeft, CTRL, normal, b"\x1b[1;5D"),
            (NamedKey::ArrowLeft, CTRL, app, b"\x1b[1;5D"),
            (NamedKey::ArrowRight, SHIFT, normal, b"\x1b[1;2C"),
            (NamedKey::End, META, normal, b"\x1b[1;3F"),
            (NamedKey::Home, NONE, app, b"\x1bOH"),
        ];
        for &(key, mods, mode, expected) in table {
            assert_eq!(named(key, mods, mode), expected, "{key:?}");
        }
    }

    #[test]
    fn function_and_editing_keys() {
        let mode = TermMode::empty();
        let shift_ctrl = Mods {
            shift: true,
            ctrl: true,
            meta: false,
        };
        let table: &[(NamedKey, Mods, &[u8])] = &[
            (NamedKey::F1, NONE, b"\x1bOP"),
            (NamedKey::F1, SHIFT, b"\x1b[1;2P"),
            (NamedKey::F5, NONE, b"\x1b[15~"),
            (NamedKey::F12, CTRL, b"\x1b[24;5~"),
            (NamedKey::Delete, NONE, b"\x1b[3~"),
            (NamedKey::PageUp, shift_ctrl, b"\x1b[5;6~"),
            (NamedKey::Tab, SHIFT, b"\x1b[Z"),
            (NamedKey::Backspace, NONE, b"\x7f"),
            (NamedKey::Backspace, CTRL, b"\x08"),
            (NamedKey::Backspace, META, b"\x1b\x7f"),
            (NamedKey::Enter, META, b"\x1b\r"),
            (NamedKey::Space, CTRL, b"\x00"),
        ];
        for &(key, mods, expected) in table {
            assert_eq!(named(key, mods, mode), expected, "{key:?}");
        }
    }

    #[test]
    fn characters() {
        assert_eq!(char_key("a", Some("a"), NONE).unwrap(), b"a");
        assert_eq!(char_key("c", Some("\u{3}"), CTRL).unwrap(), [0x03]);
        assert_eq!(char_key("[", None, CTRL).unwrap(), [0x1b]);
        assert_eq!(char_key("b", Some("b"), META).unwrap(), b"\x1bb");
        let ctrl_meta = Mods {
            ctrl: true,
            meta: true,
            shift: false,
        };
        assert_eq!(char_key("x", None, ctrl_meta).unwrap(), b"\x1b\x18");
        assert_eq!(char_key("é", Some("é"), NONE).unwrap(), "é".as_bytes());
        assert_eq!(char_key("a", None, NONE), None);
    }

    #[test]
    fn option_as_meta_uses_unmodified_key() {
        // macOS: Option+f produces "ƒ", but as Meta it must send ESC f.
        let out = encode_with(
            Key::Character("ƒ".into()),
            Key::Character("f".into()),
            Some("ƒ"),
            KeyLocation::Standard,
            META,
            TermMode::empty(),
        );
        assert_eq!(out.unwrap(), b"\x1bf");
    }

    #[test]
    fn application_keypad() {
        let keypad = |c: &str, mode| {
            let key = Key::Character(c.into());
            encode_with(key.clone(), key, Some(c), KeyLocation::Numpad, NONE, mode).unwrap()
        };
        assert_eq!(keypad("5", TermMode::APP_KEYPAD), b"\x1bOu");
        assert_eq!(keypad("+", TermMode::APP_KEYPAD), b"\x1bOk");
        assert_eq!(keypad("5", TermMode::empty()), b"5");
    }
}
