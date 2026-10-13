//! Keyboard input to PTY bytes, following xterm's encoding: modifier
//! parameters on special keys (`CSI 1;<m> A`), SS3/CSI function keys,
//! application cursor (DECCKM) and keypad (DECKPAM) modes, and Alt as an
//! ESC prefix. Programs that turn on the kitty keyboard protocol get
//! `kitty_keys` instead.

use nuntio_config::{KeyCombo, KeyName};
use nuntio_term::TermMode;

use crate::actions::named_key;
use crate::kitty_keys;
use winit::event::{ElementState, KeyEvent};
use winit::keyboard::{
    Key, KeyCode, KeyLocation, ModifiersState, NamedKey, NativeKeyCode, PhysicalKey,
};
use winit::platform::modifier_supplement::KeyEventExtModifierSupplement;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyEventKind {
    Press,
    Repeat,
    Release,
}

/// A key event as nuntio handles it: winit's `KeyEvent` can't be built
/// outside winit, and the debug server makes up its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyPress {
    pub logical: Key,
    /// The key with no modifiers applied (e.g. `c` for Ctrl+Shift+C).
    pub unmodified: Key,
    pub text: Option<String>,
    pub location: KeyLocation,
    pub physical: PhysicalKey,
    pub kind: KeyEventKind,
}

impl From<&KeyEvent> for KeyPress {
    fn from(event: &KeyEvent) -> Self {
        Self {
            logical: event.logical_key.clone(),
            unmodified: event.key_without_modifiers(),
            text: event.text.as_ref().map(ToString::to_string),
            location: event.location,
            physical: event.physical_key,
            kind: match (event.state, event.repeat) {
                (ElementState::Released, _) => KeyEventKind::Release,
                (ElementState::Pressed, false) => KeyEventKind::Press,
                (ElementState::Pressed, true) => KeyEventKind::Repeat,
            },
        }
    }
}

/// A key event with the state of the modifiers when it happened, as the
/// encoders need it.
#[derive(Debug, Clone)]
pub struct KeyInput<'a> {
    pub press: &'a KeyPress,
    pub shift: bool,
    pub ctrl: bool,
    /// Alt acts as Meta. On macOS only when Option-as-Meta is enabled for
    /// the pressed side; otherwise Option composes characters.
    pub meta: bool,
    /// Option is held on macOS without acting as Meta.
    pub option: bool,
    /// Only the kitty protocol reports it, on the modifier keys' own
    /// events; other Super combinations are nuntio's shortcuts.
    pub super_key: bool,
}

impl<'a> KeyInput<'a> {
    /// `press` with the modifiers held while it happened; `meta` tells
    /// whether Alt acts as Meta (see `KeyInput::meta`).
    pub fn new(press: &'a KeyPress, mods: ModifiersState, meta: bool) -> Self {
        Self {
            press,
            shift: mods.shift_key(),
            ctrl: mods.control_key(),
            meta,
            option: cfg!(target_os = "macos") && mods.alt_key() && !meta,
            super_key: mods.super_key(),
        }
    }

    /// xterm modifier parameter: 1 + Shift(1) + Alt(2) + Ctrl(4).
    fn modifier_param(&self) -> u8 {
        1 + self.shift as u8 + 2 * self.meta as u8 + 4 * self.ctrl as u8
    }
}

pub fn encode_key(input: &KeyInput, mode: TermMode) -> Option<Vec<u8>> {
    if mode.intersects(TermMode::KITTY_KEYBOARD_PROTOCOL) {
        return kitty_keys::encode(input, mode);
    }
    if input.press.kind == KeyEventKind::Release {
        return None;
    }
    if input.press.location == KeyLocation::Numpad
        && mode.contains(TermMode::APP_KEYPAD)
        && let Some(bytes) = encode_app_keypad(input)
    {
        return Some(bytes);
    }
    if let Key::Named(named) = &input.press.logical
        && let Some(bytes) = encode_named(*named, input, mode)
    {
        return Some(bytes);
    }

    if let Some(text) = altgr_text(input) {
        return Some(text.as_bytes().to_vec());
    }

    // Ctrl+С on a Cyrillic layout is Ctrl+C. Without Shift the typed
    // character comes first: it includes AltGr (and Option), so Ctrl+AltGr+8
    // is Ctrl+[ on a German layout, not Ctrl+8. With Shift the base key
    // decides (Ctrl+Shift+/ stays 0x1f).
    if input.ctrl {
        let latin = latin_key(&input.press.unmodified, input.press.physical);
        let base = latin.as_ref().unwrap_or(&input.press.unmodified);
        let typed = (!input.shift).then_some(&input.press.logical);
        if let Some(byte) = typed.into_iter().chain([base]).find_map(key_control_byte) {
            return Some(with_meta(vec![byte], input.meta));
        }
    }

    if input.meta {
        // Meta sends ESC plus the unmodified character (so Option+f on macOS
        // becomes ESC f instead of ƒ).
        let text = match &input.press.unmodified {
            Key::Character(s) if !input.shift => s.as_str(),
            _ => input.press.text.as_deref()?,
        };
        return Some(with_meta(text.as_bytes().to_vec(), true));
    }

    let text = input.press.text.as_deref().filter(|t| !t.is_empty())?;
    Some(text.as_bytes().to_vec())
}

/// Printable text the key produces, if any.
pub(crate) fn printable<'a>(input: &KeyInput<'a>) -> Option<&'a str> {
    input
        .press
        .text
        .as_deref()
        .filter(|text| !text.is_empty() && !text.chars().any(char::is_control))
}

/// Windows treats Ctrl+Alt as AltGr (and reports a physical AltGr that way
/// when winit can't tell): Ctrl+Alt+Q on a German layout must type "@", not
/// Ctrl+Meta+Q. A real Ctrl+Alt combination yields no other printable text
/// than the key itself. Elsewhere AltGr is never reported as Ctrl+Alt, so a
/// Ctrl+Alt+Shift+2 stays a shortcut.
pub(crate) fn altgr_text<'a>(input: &KeyInput<'a>) -> Option<&'a str> {
    if !cfg!(windows) {
        return None;
    }
    let text = printable(input).filter(|_| input.ctrl && input.meta)?;
    let same_key =
        matches!(&input.press.unmodified, Key::Character(s) if s.eq_ignore_ascii_case(text));
    (!same_key).then_some(text)
}

fn key_control_byte(key: &Key) -> Option<u8> {
    match key {
        Key::Character(s) => control_byte(s),
        _ => None,
    }
}

/// The text a key types into one of nuntio's own input fields (the find
/// bar), or `None` for a shortcut or a key without text. Option on macOS
/// composes characters like "@" unless it acts as Meta, and AltGr types
/// them on Windows; Ctrl, Meta and Cmd/Super combinations are shortcuts.
pub fn field_text<'a>(input: &KeyInput<'a>) -> Option<&'a str> {
    if input.super_key {
        return None;
    }
    if let Some(text) = altgr_text(input) {
        return Some(text);
    }
    printable(input).filter(|_| !input.ctrl && !input.meta)
}

fn with_meta(mut bytes: Vec<u8>, meta: bool) -> Vec<u8> {
    if meta {
        bytes.insert(0, 0x1b);
    }
    bytes
}

/// F13–F35 as xterm's terminfo defines them: F13–F24 are Shift+F1–F12 and
/// F25–F35 are Ctrl+F1–F11. Returns the base key and whether Ctrl (not
/// Shift) is added.
fn high_function_key(key: NamedKey) -> Option<(NamedKey, bool)> {
    let i = function_number(key)?.checked_sub(13)?;
    Some((function_key(i % 12 + 1)?, i >= 12))
}

fn encode_named(key: NamedKey, input: &KeyInput, mode: TermMode) -> Option<Vec<u8>> {
    if let Some((base, ctrl)) = high_function_key(key) {
        let input = KeyInput {
            shift: input.shift || !ctrl,
            ctrl: input.ctrl || ctrl,
            ..input.clone()
        };
        return encode_named(base, &input, mode);
    }
    // Option+←/→/⌫ work on words, as in Terminal.app: the Emacs keys that
    // readline, zsh and fish know without any setup.
    if input.option && !input.ctrl && !input.shift {
        match key {
            NamedKey::ArrowLeft => return Some(b"\x1bb".to_vec()),
            NamedKey::ArrowRight => return Some(b"\x1bf".to_vec()),
            NamedKey::Backspace => return Some(b"\x1b\x7f".to_vec()),
            _ => {}
        }
    }
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
        } else {
            plain_cursor_key(f, mode).to_vec()
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
    let f = match &input.press.logical {
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

/// The key as it is labeled on a US layout, for layouts without Latin
/// letters: on a Russian layout the key C gives "с", yet Ctrl+С must work
/// as Ctrl+C, for shortcuts and for the shell. `None` on Latin layouts,
/// whose own letters count (German QWERTZ's Z stays Z, and Ctrl+Ü is not
/// Ctrl+[), and for keys without a US character.
pub fn latin_key(unmodified: &Key, physical: PhysicalKey) -> Option<Key> {
    let Key::Character(text) = unmodified else {
        return None;
    };
    if text.is_empty() || text.chars().any(is_latin) {
        return None;
    }
    let PhysicalKey::Code(code) = physical else {
        return None;
    };
    let c = us_char(code)?;
    Some(Key::Character(c.to_string().into()))
}

/// ASCII and the Latin blocks of Unicode: letters of Latin layouts.
fn is_latin(c: char) -> bool {
    matches!(
        c as u32,
        0..0x0250 | 0x1E00..0x1F00 | 0x2C60..0x2C80 | 0xA720..0xA800 | 0xAB30..0xAB70
    )
}

const FUNCTION_KEYS: [NamedKey; 35] = {
    use NamedKey::*;
    [
        F1, F2, F3, F4, F5, F6, F7, F8, F9, F10, F11, F12, F13, F14, F15, F16, F17, F18, F19, F20,
        F21, F22, F23, F24, F25, F26, F27, F28, F29, F30, F31, F32, F33, F34, F35,
    ]
};

/// The function key F`n`, for `n` from 1 to 35.
pub fn function_key(n: u8) -> Option<NamedKey> {
    FUNCTION_KEYS.get(usize::from(n).checked_sub(1)?).copied()
}

/// The `n` of a function key F`n`.
pub fn function_number(key: NamedKey) -> Option<u8> {
    let index = FUNCTION_KEYS.iter().position(|&k| k == key)?;
    u8::try_from(index + 1).ok()
}

/// A cursor key without modifiers: `final_byte` after SS3 in application
/// cursor mode, after CSI otherwise.
fn plain_cursor_key(final_byte: u8, mode: TermMode) -> [u8; 3] {
    let intro = if mode.contains(TermMode::APP_CURSOR) {
        b'O'
    } else {
        b'['
    };
    [0x1b, intro, final_byte]
}

/// What a wheel notch sends to a full-screen program without mouse
/// support (alternate scroll): the arrow key up or down.
pub fn wheel_arrow(up: bool, mode: TermMode) -> [u8; 3] {
    plain_cursor_key(if up { b'A' } else { b'B' }, mode)
}

/// The report that the window gained or lost focus (`TermMode::FOCUS_IN_OUT`).
pub fn focus_report(focused: bool) -> &'static [u8] {
    if focused { b"\x1b[I" } else { b"\x1b[O" }
}

/// Shift, Ctrl, Alt, Super and the like: keys that only modify others.
pub fn is_modifier(key: &Key) -> bool {
    use NamedKey::*;
    matches!(
        key,
        Key::Named(Shift | Control | Alt | AltGraph | Super | Meta | Hyper)
    )
}

/// The key event a combination stands for, and the modifiers it needs:
/// what the debug server and multiplexer bindings press on a pane's behalf.
pub fn synthetic_press(combo: &KeyCombo, kind: KeyEventKind) -> (KeyPress, ModifiersState) {
    let m = combo.mods;
    let mut mods = ModifiersState::empty();
    for (on, flag) in [
        (m.ctrl, ModifiersState::CONTROL),
        (m.shift, ModifiersState::SHIFT),
        (m.alt, ModifiersState::ALT),
        (m.super_key, ModifiersState::SUPER),
    ] {
        if on {
            mods |= flag;
        }
    }
    let press = match combo.key {
        KeyName::Char(c) => {
            let shifted = if m.shift {
                c.to_uppercase().next().unwrap_or(c)
            } else {
                c
            };
            let physical = char_code(c).map_or(
                PhysicalKey::Unidentified(NativeKeyCode::Unidentified),
                PhysicalKey::Code,
            );
            KeyPress {
                logical: Key::Character(shifted.to_string().into()),
                unmodified: Key::Character(c.to_string().into()),
                // Like winit: Ctrl and Cmd combinations type nothing.
                text: (!m.ctrl && !m.super_key).then(|| shifted.to_string()),
                location: KeyLocation::Standard,
                physical,
                kind,
            }
        }
        KeyName::Named(name) => {
            let named = named_key(name);
            let text = match named {
                NamedKey::Enter => Some("\r"),
                NamedKey::Tab => Some("\t"),
                NamedKey::Space => Some(" "),
                NamedKey::Backspace => Some("\u{8}"),
                NamedKey::Escape => Some("\u{1b}"),
                NamedKey::Delete => Some("\u{7f}"),
                _ => None,
            };
            KeyPress {
                logical: Key::Named(named),
                unmodified: Key::Named(named),
                text: text.map(str::to_owned),
                location: KeyLocation::Standard,
                physical: named_code(named).map_or(
                    PhysicalKey::Unidentified(NativeKeyCode::Unidentified),
                    PhysicalKey::Code,
                ),
                kind,
            }
        }
    };
    (press, mods)
}

/// What a multiplexer receives when nuntio forwards `combo`: the key press
/// and, in the kitty protocol's event-type mode, its release. Under the
/// kitty protocol every key, text keys included, goes out as an escape code:
/// herdr ignores a text key that arrives in the same read as its prefix.
pub fn chord_bytes(combo: &KeyCombo, mode: TermMode) -> Vec<u8> {
    let mode = if mode.intersects(TermMode::KITTY_KEYBOARD_PROTOCOL) {
        mode | TermMode::REPORT_ALL_KEYS_AS_ESC
    } else {
        mode
    };
    let mut bytes = Vec::new();
    for kind in [KeyEventKind::Press, KeyEventKind::Release] {
        let (press, mods) = synthetic_press(combo, kind);
        let input = KeyInput::new(&press, mods, combo.mods.alt);
        if let Some(encoded) = encode_key(&input, mode) {
            bytes.extend_from_slice(&encoded);
        }
    }
    bytes
}

/// The US-layout key for a letter or digit.
fn char_code(c: char) -> Option<KeyCode> {
    use KeyCode::*;
    const LETTERS: [KeyCode; 26] = [
        KeyA, KeyB, KeyC, KeyD, KeyE, KeyF, KeyG, KeyH, KeyI, KeyJ, KeyK, KeyL, KeyM, KeyN, KeyO,
        KeyP, KeyQ, KeyR, KeyS, KeyT, KeyU, KeyV, KeyW, KeyX, KeyY, KeyZ,
    ];
    const DIGITS: [KeyCode; 10] = [
        Digit0, Digit1, Digit2, Digit3, Digit4, Digit5, Digit6, Digit7, Digit8, Digit9,
    ];
    match c {
        'a'..='z' => Some(LETTERS[(c as u8 - b'a') as usize]),
        '0'..='9' => Some(DIGITS[(c as u8 - b'0') as usize]),
        _ => None,
    }
}

fn named_code(key: NamedKey) -> Option<KeyCode> {
    let code = match key {
        NamedKey::Enter => KeyCode::Enter,
        NamedKey::Tab => KeyCode::Tab,
        NamedKey::Escape => KeyCode::Escape,
        NamedKey::Space => KeyCode::Space,
        NamedKey::Backspace => KeyCode::Backspace,
        NamedKey::Delete => KeyCode::Delete,
        NamedKey::Insert => KeyCode::Insert,
        NamedKey::Home => KeyCode::Home,
        NamedKey::End => KeyCode::End,
        NamedKey::PageUp => KeyCode::PageUp,
        NamedKey::PageDown => KeyCode::PageDown,
        NamedKey::ArrowUp => KeyCode::ArrowUp,
        NamedKey::ArrowDown => KeyCode::ArrowDown,
        NamedKey::ArrowLeft => KeyCode::ArrowLeft,
        NamedKey::ArrowRight => KeyCode::ArrowRight,
        NamedKey::F1 => KeyCode::F1,
        NamedKey::F2 => KeyCode::F2,
        NamedKey::F3 => KeyCode::F3,
        NamedKey::F4 => KeyCode::F4,
        NamedKey::F5 => KeyCode::F5,
        NamedKey::F6 => KeyCode::F6,
        NamedKey::F7 => KeyCode::F7,
        NamedKey::F8 => KeyCode::F8,
        NamedKey::F9 => KeyCode::F9,
        NamedKey::F10 => KeyCode::F10,
        NamedKey::F11 => KeyCode::F11,
        NamedKey::F12 => KeyCode::F12,
        _ => return None,
    };
    Some(code)
}

/// The unshifted character of a key on a US layout.
pub(crate) fn us_char(code: KeyCode) -> Option<char> {
    use KeyCode::*;
    let letters = [
        KeyA, KeyB, KeyC, KeyD, KeyE, KeyF, KeyG, KeyH, KeyI, KeyJ, KeyK, KeyL, KeyM, KeyN, KeyO,
        KeyP, KeyQ, KeyR, KeyS, KeyT, KeyU, KeyV, KeyW, KeyX, KeyY, KeyZ,
    ];
    let digits = [
        Digit0, Digit1, Digit2, Digit3, Digit4, Digit5, Digit6, Digit7, Digit8, Digit9,
    ];
    if let Some(i) = letters.iter().position(|&k| k == code) {
        return Some((b'a' + i as u8) as char);
    }
    if let Some(i) = digits.iter().position(|&k| k == code) {
        return Some((b'0' + i as u8) as char);
    }
    Some(match code {
        Minus => '-',
        Equal => '=',
        BracketLeft => '[',
        BracketRight => ']',
        Backslash => '\\',
        Semicolon => ';',
        Quote => '\'',
        Backquote => '`',
        Comma => ',',
        Period => '.',
        Slash => '/',
        _ => return None,
    })
}

/// `Ctrl+<key>` as a C0 control character.
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
        '_' | '-' | '/' | '7' => Some(0x1f),
        '?' | '8' => Some(0x7f),
        _ => None,
    }
}

/// Key events for the tests of the encoders.
#[cfg(test)]
pub(crate) mod test_keys {
    use super::*;

    /// A key press with the modifiers held, changed with the builder methods.
    #[derive(Clone)]
    pub struct Press {
        pub press: KeyPress,
        pub shift: bool,
        pub ctrl: bool,
        pub meta: bool,
        pub option: bool,
        pub super_key: bool,
    }

    /// A key without text of its own, except Space.
    pub fn named(key: NamedKey) -> Press {
        let key = Key::Named(key);
        let text = (key == Key::Named(NamedKey::Space)).then(|| " ".to_owned());
        Press::new(key.clone(), key, text)
    }

    /// A character key that types `c`.
    pub fn ch(c: &str) -> Press {
        let key = Key::Character(c.into());
        Press::new(key.clone(), key, Some(c.to_owned()))
    }

    impl Press {
        fn new(logical: Key, unmodified: Key, text: Option<String>) -> Self {
            Self {
                press: KeyPress {
                    logical,
                    unmodified,
                    text,
                    location: KeyLocation::Standard,
                    physical: PhysicalKey::Unidentified(NativeKeyCode::Unidentified),
                    kind: KeyEventKind::Press,
                },
                shift: false,
                ctrl: false,
                meta: false,
                option: false,
                super_key: false,
            }
        }

        pub fn input(&self) -> KeyInput<'_> {
            KeyInput {
                press: &self.press,
                shift: self.shift,
                ctrl: self.ctrl,
                meta: self.meta,
                option: self.option,
                super_key: self.super_key,
            }
        }

        /// Shift held, typing `shifted` instead.
        pub fn shift(mut self, shifted: &str) -> Self {
            self.shift = true;
            self.press.logical = Key::Character(shifted.into());
            self.press.text = Some(shifted.to_owned());
            self
        }

        pub fn shift_held(mut self) -> Self {
            self.shift = true;
            self
        }

        pub fn ctrl(mut self) -> Self {
            self.ctrl = true;
            self
        }

        pub fn meta(mut self) -> Self {
            self.meta = true;
            self
        }

        pub fn option(mut self) -> Self {
            self.option = true;
            self
        }

        /// The key types `typed` (a composed or dead character) instead of
        /// its own.
        pub fn typing(mut self, typed: &str) -> Self {
            self.press.logical = Key::Character(typed.into());
            self.press.text = Some(typed.to_owned());
            self
        }

        pub fn logical(mut self, key: Key) -> Self {
            self.press.logical = key;
            self
        }

        pub fn text(mut self, text: Option<&str>) -> Self {
            self.press.text = text.map(str::to_owned);
            self
        }

        pub fn location(mut self, location: KeyLocation) -> Self {
            self.press.location = location;
            self
        }

        pub fn at(self, location: KeyLocation, code: KeyCode) -> Self {
            self.location(location).physical(code)
        }

        pub fn physical(mut self, code: KeyCode) -> Self {
            self.press.physical = PhysicalKey::Code(code);
            self
        }

        pub fn event(mut self, kind: KeyEventKind) -> Self {
            self.press.kind = kind;
            self
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_keys::{self, Press};
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

    /// `press` with `mods` held.
    fn encode_with(press: Press, mods: Mods, mode: TermMode) -> Option<Vec<u8>> {
        let press = Press {
            shift: mods.shift,
            ctrl: mods.ctrl,
            meta: mods.meta,
            ..press
        };
        encode_key(&press.input(), mode)
    }

    fn named(key: NamedKey, mods: Mods, mode: TermMode) -> Vec<u8> {
        encode_with(test_keys::named(key), mods, mode).unwrap()
    }

    fn char_key(c: &str, text: Option<&str>, mods: Mods) -> Option<Vec<u8>> {
        encode_with(test_keys::ch(c).text(text), mods, TermMode::empty())
    }

    #[test]
    fn high_function_keys_use_shift_and_ctrl_f1_to_f12() {
        let none = TermMode::empty();
        assert_eq!(named(NamedKey::F13, NONE, none), b"\x1b[1;2P");
        assert_eq!(named(NamedKey::F17, NONE, none), b"\x1b[15;2~");
        assert_eq!(named(NamedKey::F24, NONE, none), b"\x1b[24;2~");
        assert_eq!(named(NamedKey::F25, NONE, none), b"\x1b[1;5P");
        assert_eq!(named(NamedKey::F13, SHIFT, none), b"\x1b[1;2P");
        assert_eq!(named(NamedKey::F35, NONE, none), b"\x1b[23;5~");
    }

    #[test]
    fn function_keys_are_numbered_from_one_to_35() {
        for n in 1..=35 {
            assert_eq!(function_key(n).and_then(function_number), Some(n));
        }
        assert_eq!(function_key(0), None);
        assert_eq!(function_key(36), None);
        assert_eq!(function_number(NamedKey::Enter), None);
    }

    #[test]
    fn ctrl_slash_is_unit_separator() {
        assert_eq!(char_key("/", Some("/"), CTRL), Some(vec![0x1f]));
    }

    #[test]
    fn wheel_arrows_and_focus_reports() {
        assert_eq!(wheel_arrow(true, TermMode::empty()), *b"\x1b[A");
        assert_eq!(wheel_arrow(true, TermMode::APP_CURSOR), *b"\x1bOA");
        assert_eq!(wheel_arrow(false, TermMode::empty()), *b"\x1b[B");
        assert_eq!(wheel_arrow(false, TermMode::APP_CURSOR), *b"\x1bOB");
        assert_eq!(focus_report(true), b"\x1b[I");
        assert_eq!(focus_report(false), b"\x1b[O");
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
        let dead = encode_with(
            test_keys::ch("^").logical(Key::Dead(Some('^'))).text(None),
            NONE,
            TermMode::empty(),
        );
        assert_eq!(dead, None);
    }

    #[test]
    #[cfg(windows)]
    fn altgr_types_its_character() {
        let ctrl_alt = Mods {
            ctrl: true,
            meta: true,
            shift: false,
        };
        let key = |c: &str, unmodified: &str, text: Option<&str>| {
            let press = test_keys::ch(unmodified).logical(ch(c)).text(text);
            encode_with(press, ctrl_alt, TermMode::empty())
        };
        // AltGr+Q and AltGr+7 on a German layout (Windows).
        assert_eq!(key("@", "q", Some("@")).unwrap(), b"@");
        assert_eq!(key("{", "7", Some("{")).unwrap(), b"{");
        // Real Ctrl+Alt combinations stay control characters with ESC.
        assert_eq!(key("a", "a", Some("\u{1}")).unwrap(), b"\x1b\x01");
        assert_eq!(key("a", "a", Some("a")).unwrap(), b"\x1b\x01");
        assert_eq!(key("a", "a", None).unwrap(), b"\x1b\x01");
        let all = Mods {
            shift: true,
            ctrl: true,
            meta: true,
        };
        let shifted = encode_with(
            test_keys::ch("2").logical(ch("@")).text(Some("@")),
            all,
            TermMode::empty(),
        );
        assert_eq!(shifted.unwrap(), b"@");
    }

    #[cfg(not(windows))]
    #[test]
    fn ctrl_alt_symbols_are_not_altgr_outside_windows() {
        let all = Mods {
            shift: true,
            ctrl: true,
            meta: true,
        };
        let out = encode_with(
            test_keys::ch("2").logical(ch("@")).text(Some("@")),
            all,
            TermMode::empty(),
        );
        assert_eq!(out.unwrap(), b"\x1b\x00");
    }

    #[test]
    fn ctrl_uses_the_typed_character_unless_shift_is_held() {
        let ctrl_shift = Mods {
            shift: true,
            ctrl: true,
            meta: false,
        };
        let key = |c: &str, unmodified: &str, mods| {
            let press = test_keys::ch(unmodified).logical(ch(c)).text(Some(c));
            encode_with(press, mods, TermMode::empty())
        };
        // German Linux: Ctrl+AltGr+8/9/ß are Ctrl+[ / ] / \ (AltGr isn't Alt there).
        assert_eq!(key("[", "8", CTRL), Some(vec![0x1b]));
        assert_eq!(key("]", "9", CTRL), Some(vec![0x1d]));
        assert_eq!(key("\\", "ß", CTRL), Some(vec![0x1c]));
        // With Shift the base key decides, as before.
        assert_eq!(key("?", "/", ctrl_shift), Some(vec![0x1f]));
        assert_eq!(key("@", "2", ctrl_shift), Some(vec![0x00]));
        assert_eq!(key("A", "a", ctrl_shift), Some(vec![0x01]));
    }

    #[test]
    fn option_as_meta_uses_unmodified_key() {
        // macOS: Option+f produces "ƒ", but as Meta it must send ESC f.
        let out = encode_with(test_keys::ch("f").typing("ƒ"), META, TermMode::empty());
        assert_eq!(out.unwrap(), b"\x1bf");
    }

    #[test]
    fn text_for_input_fields() {
        let field = |c: &str, unmodified: &str, text: Option<&str>, mods: Mods, super_key| {
            let press = Press {
                shift: mods.shift,
                ctrl: mods.ctrl,
                meta: mods.meta,
                super_key,
                ..test_keys::ch(unmodified).logical(ch(c)).text(text)
            };
            field_text(&press.input()).map(str::to_owned)
        };
        let ctrl_alt = Mods {
            ctrl: true,
            meta: true,
            shift: false,
        };
        assert_eq!(
            field("a", "a", Some("a"), NONE, false).as_deref(),
            Some("a")
        );
        assert_eq!(
            field("A", "a", Some("A"), SHIFT, false).as_deref(),
            Some("A")
        );
        // Option composes "@" on a German Mac keyboard when it isn't Meta.
        assert_eq!(
            field("@", "l", Some("@"), NONE, false).as_deref(),
            Some("@")
        );
        // AltGr+Q on Windows.
        let altgr = if cfg!(windows) { Some("@") } else { None };
        assert_eq!(
            field("@", "q", Some("@"), ctrl_alt, false).as_deref(),
            altgr
        );
        // Shortcuts and control characters type nothing.
        assert_eq!(field("w", "w", Some("\u{17}"), CTRL, false), None);
        assert_eq!(field("w", "w", Some("w"), CTRL, false), None);
        assert_eq!(field("b", "b", Some("b"), META, false), None);
        assert_eq!(field("a", "a", Some("a"), ctrl_alt, false), None);
        assert_eq!(field("v", "v", Some("v"), NONE, true), None);
        assert_eq!(field("a", "a", None, NONE, false), None);
    }

    #[test]
    fn latin_keys_for_other_scripts() {
        let latin = |text: &str, code| latin_key(&ch(text), PhysicalKey::Code(code));
        // Russian: the keys C, Ю (period) and 1.
        assert_eq!(latin("с", KeyCode::KeyC), Some(ch("c")));
        assert_eq!(latin("ю", KeyCode::Period), Some(ch(".")));
        // Greek.
        assert_eq!(latin("ψ", KeyCode::KeyC), Some(ch("c")));
        // Latin layouts keep their own letters: QWERTZ's Z is where Y is.
        assert_eq!(latin("z", KeyCode::KeyY), None);
        assert_eq!(latin("1", KeyCode::Digit1), None);
        assert_eq!(latin("ü", KeyCode::BracketLeft), None);
        assert_eq!(latin("ß", KeyCode::Minus), None);
        // Hebrew.
        assert_eq!(latin("ב", KeyCode::KeyC), Some(ch("c")));
        assert_eq!(latin("с", KeyCode::F1), None);
        assert_eq!(
            latin_key(
                &Key::Named(NamedKey::Enter),
                PhysicalKey::Code(KeyCode::Enter)
            ),
            None
        );
    }

    fn ch(s: &str) -> Key {
        Key::Character(s.into())
    }

    #[test]
    fn application_keypad() {
        let keypad = |c: &str, mode| {
            let press = test_keys::ch(c).location(KeyLocation::Numpad);
            encode_with(press, NONE, mode).unwrap()
        };
        assert_eq!(keypad("5", TermMode::APP_KEYPAD), b"\x1bOu");
        assert_eq!(keypad("+", TermMode::APP_KEYPAD), b"\x1bOk");
        assert_eq!(keypad("5", TermMode::empty()), b"5");
    }

    #[test]
    fn option_arrows_move_by_word() {
        let option = |key: NamedKey, shift: bool| {
            let press = Press {
                shift,
                ..test_keys::named(key).option()
            };
            encode_key(&press.input(), TermMode::APP_CURSOR).unwrap()
        };
        assert_eq!(option(NamedKey::ArrowLeft, false), b"\x1bb");
        assert_eq!(option(NamedKey::ArrowRight, false), b"\x1bf");
        assert_eq!(option(NamedKey::Backspace, false), b"\x1b\x7f");
        // Other keys and combinations keep their encoding.
        assert_eq!(option(NamedKey::ArrowUp, false), b"\x1bOA");
        assert_eq!(option(NamedKey::ArrowLeft, true), b"\x1b[1;2D");
    }

    #[test]
    fn modifier_keys() {
        assert!(is_modifier(&Key::Named(NamedKey::Super)));
        assert!(is_modifier(&Key::Named(NamedKey::Shift)));
        assert!(!is_modifier(&Key::Named(NamedKey::Enter)));
        assert!(!is_modifier(&ch("a")));
    }

    #[test]
    fn legacy_keys_send_nothing_on_release() {
        let press = test_keys::ch("a")
            .physical(KeyCode::KeyA)
            .event(KeyEventKind::Release);
        assert_eq!(encode_key(&press.input(), TermMode::empty()), None);
    }

    #[test]
    fn ctrl_on_other_scripts_sends_the_latin_control_character() {
        let press = test_keys::ch("с").text(None).physical(KeyCode::KeyC).ctrl();
        assert_eq!(
            encode_key(&press.input(), TermMode::empty()).unwrap(),
            [0x03]
        );
    }

    #[test]
    fn combos_become_key_presses() {
        let press_of = |combo: &str, kind| synthetic_press(&KeyCombo::parse(combo).unwrap(), kind);
        let (press, mods) = press_of("ctrl+c", KeyEventKind::Press);
        assert_eq!(mods, ModifiersState::CONTROL);
        assert_eq!(press.unmodified, Key::Character("c".into()));
        assert_eq!(press.physical, PhysicalKey::Code(KeyCode::KeyC));
        assert_eq!(press.text, None);

        let (press, mods) = press_of("shift+a", KeyEventKind::Press);
        assert_eq!(mods, ModifiersState::SHIFT);
        assert_eq!(press.logical, Key::Character("A".into()));
        assert_eq!(press.unmodified, Key::Character("a".into()));
        assert_eq!(press.text.as_deref(), Some("A"));

        let (press, mods) = press_of("enter", KeyEventKind::Release);
        assert_eq!(mods, ModifiersState::empty());
        assert_eq!(press.logical, Key::Named(NamedKey::Enter));
        assert_eq!(press.physical, PhysicalKey::Code(KeyCode::Enter));
        assert_eq!(press.text.as_deref(), Some("\r"));
        assert_eq!(press.kind, KeyEventKind::Release);
    }

    #[test]
    fn chords_under_the_kitty_protocol_are_escape_codes() {
        let ctrl_b = KeyCombo::parse("ctrl+b").unwrap();
        assert_eq!(chord_bytes(&ctrl_b, TermMode::empty()), [0x02]);
        assert_eq!(
            chord_bytes(&ctrl_b, TermMode::DISAMBIGUATE_ESC_CODES),
            b"\x1b[98;5u"
        );
        let minus = KeyCombo::parse("minus").unwrap();
        assert_eq!(chord_bytes(&minus, TermMode::empty()), b"-");
        assert_eq!(
            chord_bytes(&minus, TermMode::DISAMBIGUATE_ESC_CODES),
            b"\x1b[45u"
        );
    }

    #[test]
    fn chords_in_event_mode_end_with_the_release() {
        let ctrl_b = KeyCombo::parse("ctrl+b").unwrap();
        let mode = TermMode::DISAMBIGUATE_ESC_CODES | TermMode::REPORT_EVENT_TYPES;
        assert_eq!(chord_bytes(&ctrl_b, mode), b"\x1b[98;5u\x1b[98;5:3u");
    }

    #[test]
    fn herdr_prefix_and_key_arrive_as_escape_codes() {
        let mode = TermMode::DISAMBIGUATE_ESC_CODES
            | TermMode::REPORT_EVENT_TYPES
            | TermMode::REPORT_ALTERNATE_KEYS;
        let mut bytes = chord_bytes(&KeyCombo::parse("ctrl+b").unwrap(), mode);
        bytes.extend(chord_bytes(&KeyCombo::parse("v").unwrap(), mode));
        assert_eq!(bytes, b"\x1b[98;5u\x1b[98;5:3u\x1b[118u\x1b[118;1:3u");
    }
}
