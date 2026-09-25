//! Keyboard input in the kitty keyboard protocol
//! (<https://sw.kovidgoyal.net/kitty/keyboard-protocol/>), used while a
//! program has turned on at least one of its flags. alacritty_terminal
//! keeps the flag stacks; this only encodes keys:
//! `CSI code[:shifted[:base]] [; mods[:event] [; text]] u`, or the
//! `CSI 1;mods A`/`CSI n;mods ~` forms for keys that have them.

use nuntio_term::TermMode;
use winit::keyboard::{Key, KeyCode, KeyLocation, NamedKey, PhysicalKey};

use crate::input::{KeyEventKind, KeyInput, altgr_text, printable, us_char};

const SHIFT: u8 = 1;
const ALT: u8 = 2;
const CTRL: u8 = 4;
const SUPER: u8 = 8;

/// A key with no text of its own: its number and the final byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Functional {
    number: u32,
    final_byte: u8,
    /// Shift, Ctrl, Alt, Super, AltGr and the lock keys: reported only
    /// with "report all keys as escape codes".
    modifier: Option<u8>,
}

impl Functional {
    const fn u(number: u32) -> Self {
        Self {
            number,
            final_byte: b'u',
            modifier: None,
        }
    }

    const fn tilde(number: u32) -> Self {
        Self {
            number,
            final_byte: b'~',
            modifier: None,
        }
    }

    const fn letter(final_byte: u8) -> Self {
        Self {
            number: 1,
            final_byte,
            modifier: None,
        }
    }

    const fn modifier(number: u32, bit: u8) -> Self {
        Self {
            number,
            final_byte: b'u',
            modifier: Some(bit),
        }
    }
}

pub fn encode(input: &KeyInput, mode: TermMode) -> Option<Vec<u8>> {
    let report_events = mode.contains(TermMode::REPORT_EVENT_TYPES);
    let all_keys = mode.contains(TermMode::REPORT_ALL_KEYS_AS_ESC);
    let event = match input.event {
        KeyEventKind::Release if !report_events => return None,
        KeyEventKind::Repeat if !report_events => KeyEventKind::Press,
        event => event,
    };

    // AltGr, which Windows reports as Ctrl+Alt, types text; it isn't a
    // Ctrl+Alt combination.
    let altgr = altgr_text(input);
    let (ctrl, alt) = if altgr.is_some() {
        (false, false)
    } else {
        (input.ctrl, input.meta)
    };
    let mut mods = [
        (input.shift, SHIFT),
        (alt, ALT),
        (ctrl, CTRL),
        (input.super_key, SUPER),
    ]
    .into_iter()
    .filter(|&(held, _)| held)
    .fold(0, |mods, (_, bit)| mods | bit);
    let text = altgr.or_else(|| printable(input).filter(|_| !ctrl && !alt));

    // Keys that type text send it, unless a program wants all keys as
    // escape codes. Shift only changes the text.
    if !all_keys
        && event != KeyEventKind::Release
        && let Some(text) = text
        && mods & !SHIFT == 0
    {
        return Some(text.as_bytes().to_vec());
    }

    if let Some(key) = functional(input) {
        if let Some(bit) = key.modifier {
            if !all_keys {
                return None;
            }
            // A modifier key's own event carries its new state, whichever
            // order the platform reports the key and the modifier change in.
            match event {
                KeyEventKind::Release => mods &= !bit,
                _ => mods |= bit,
            }
        }
        if !all_keys {
            let legacy = match input.key {
                Key::Named(NamedKey::Enter) => Some(b'\r'),
                Key::Named(NamedKey::Tab) => Some(b'\t'),
                Key::Named(NamedKey::Backspace) => Some(0x7f),
                _ => None,
            };
            // Enter, Tab and Backspace keep their bytes so a shell stays
            // usable after a program exits without resetting the mode.
            if input.location != KeyLocation::Numpad
                && let Some(byte) = legacy
            {
                return match event {
                    KeyEventKind::Release => None,
                    _ if mods == 0 => Some(vec![byte]),
                    _ => Some(csi(key.number, &[], mods, event, None, key.final_byte)),
                };
            }
        }
        let text = (key.final_byte == b'u' && event != KeyEventKind::Release)
            .then_some(text)
            .flatten()
            .filter(|_| all_keys && mode.contains(TermMode::REPORT_ASSOCIATED_TEXT));
        return Some(csi(key.number, &[], mods, event, text, key.final_byte));
    }

    let code = text_key_code(input.unmodified)?;

    let mut alternates = Vec::new();
    if mode.contains(TermMode::REPORT_ALTERNATE_KEYS) {
        let shifted = match input.key {
            Key::Character(s) if input.shift => single_char(s).map(u32::from),
            _ => None,
        }
        .filter(|&c| c != code);
        let base = match input.physical {
            PhysicalKey::Code(physical) => us_char(physical).map(u32::from),
            PhysicalKey::Unidentified(_) => None,
        }
        .filter(|&c| c != code);
        if shifted.is_some() || base.is_some() {
            alternates.push(shifted);
        }
        if base.is_some() {
            alternates.push(base);
        }
    }
    let text = text
        .filter(|_| event != KeyEventKind::Release)
        .filter(|_| all_keys && mode.contains(TermMode::REPORT_ASSOCIATED_TEXT));
    Some(csi(code, &alternates, mods, event, text, b'u'))
}

/// `CSI number[:alternates] [; mods[:event]] [; text] final`, leaving out
/// fields at their defaults.
fn csi(
    number: u32,
    alternates: &[Option<u32>],
    mods: u8,
    event: KeyEventKind,
    text: Option<&str>,
    final_byte: u8,
) -> Vec<u8> {
    let mut out = String::from("\x1b[");
    let event_number = match event {
        KeyEventKind::Press => None,
        KeyEventKind::Repeat => Some(2),
        KeyEventKind::Release => Some(3),
    };
    let plain = mods == 0 && event_number.is_none() && text.is_none();
    // `CSI A` rather than `CSI 1A`.
    if !(plain && number == 1 && final_byte != b'u' && final_byte != b'~') {
        out.push_str(&number.to_string());
    }
    for alternate in alternates {
        out.push(':');
        if let Some(c) = alternate {
            out.push_str(&c.to_string());
        }
    }
    if !plain {
        out.push(';');
        if mods != 0 || event_number.is_some() {
            out.push_str(&(1 + mods).to_string());
        }
        if let Some(event) = event_number {
            out.push_str(&format!(":{event}"));
        }
    }
    if let Some(text) = text {
        let codes: Vec<String> = text.chars().map(|c| u32::from(c).to_string()).collect();
        out.push(';');
        out.push_str(&codes.join(":"));
    }
    out.push(final_byte as char);
    out.into_bytes()
}

fn single_char(s: &str) -> Option<char> {
    let mut chars = s.chars();
    let c = chars.next()?;
    chars.next().is_none().then_some(c)
}

/// The code of a key that types text: its character without Shift, in
/// lower case.
fn text_key_code(unmodified: &Key) -> Option<u32> {
    let c = match unmodified {
        Key::Character(s) => single_char(s)?,
        Key::Named(NamedKey::Space) => ' ',
        _ => return None,
    };
    let mut lower = c.to_lowercase();
    let c = match (lower.next(), lower.next()) {
        (Some(lower), None) => lower,
        _ => c,
    };
    Some(c.into())
}

fn functional(input: &KeyInput) -> Option<Functional> {
    if input.location == KeyLocation::Numpad
        && let Some(key) = keypad(input)
    {
        return Some(key);
    }
    let Key::Named(key) = input.key else {
        return None;
    };
    let right = input.location == KeyLocation::Right;
    let side = |left: u32| if right { left + 6 } else { left };
    use NamedKey::*;
    Some(match key {
        Escape => Functional::u(27),
        Enter => Functional::u(13),
        Tab => Functional::u(9),
        Backspace => Functional::u(127),
        Insert => Functional::tilde(2),
        Delete => Functional::tilde(3),
        ArrowLeft => Functional::letter(b'D'),
        ArrowRight => Functional::letter(b'C'),
        ArrowUp => Functional::letter(b'A'),
        ArrowDown => Functional::letter(b'B'),
        PageUp => Functional::tilde(5),
        PageDown => Functional::tilde(6),
        Home => Functional::letter(b'H'),
        End => Functional::letter(b'F'),
        CapsLock => Functional::modifier(57358, 0),
        ScrollLock => Functional::modifier(57359, 0),
        NumLock => Functional::modifier(57360, 0),
        PrintScreen => Functional::u(57361),
        Pause => Functional::u(57362),
        ContextMenu => Functional::u(57363),
        F1 => Functional::letter(b'P'),
        F2 => Functional::letter(b'Q'),
        // Not `CSI R`, which is also the cursor position report.
        F3 => Functional::tilde(13),
        F4 => Functional::letter(b'S'),
        F5 => Functional::tilde(15),
        F6 => Functional::tilde(17),
        F7 => Functional::tilde(18),
        F8 => Functional::tilde(19),
        F9 => Functional::tilde(20),
        F10 => Functional::tilde(21),
        F11 => Functional::tilde(23),
        F12 => Functional::tilde(24),
        F13 | F14 | F15 | F16 | F17 | F18 | F19 | F20 | F21 | F22 | F23 | F24 | F25 | F26 | F27
        | F28 | F29 | F30 | F31 | F32 | F33 | F34 | F35 => {
            let f = [
                F13, F14, F15, F16, F17, F18, F19, F20, F21, F22, F23, F24, F25, F26, F27, F28,
                F29, F30, F31, F32, F33, F34, F35,
            ];
            let i = f.iter().position(|k| k == key)? as u32;
            Functional::u(57376 + i)
        }
        MediaPlay => Functional::u(57428),
        MediaPause => Functional::u(57429),
        MediaPlayPause => Functional::u(57430),
        MediaStop => Functional::u(57432),
        MediaFastForward => Functional::u(57433),
        MediaRewind => Functional::u(57434),
        MediaTrackNext => Functional::u(57435),
        MediaTrackPrevious => Functional::u(57436),
        MediaRecord => Functional::u(57437),
        AudioVolumeDown => Functional::u(57438),
        AudioVolumeUp => Functional::u(57439),
        AudioVolumeMute => Functional::u(57440),
        Shift => Functional::modifier(side(57441), SHIFT),
        Control => Functional::modifier(side(57442), CTRL),
        Alt => Functional::modifier(side(57443), ALT),
        Super => Functional::modifier(side(57444), SUPER),
        Hyper => Functional::modifier(side(57445), 0),
        Meta => Functional::modifier(side(57446), 0),
        AltGraph => Functional::modifier(57453, 0),
        _ => return None,
    })
}

/// Keypad keys have their own numbers, apart from the keys they type or
/// move like.
fn keypad(input: &KeyInput) -> Option<Functional> {
    // Without Num Lock the digits move the cursor.
    if let Key::Named(key) = input.key {
        let number = match key {
            NamedKey::Enter => 57414,
            NamedKey::ArrowLeft => 57417,
            NamedKey::ArrowRight => 57418,
            NamedKey::ArrowUp => 57419,
            NamedKey::ArrowDown => 57420,
            NamedKey::PageUp => 57421,
            NamedKey::PageDown => 57422,
            NamedKey::Home => 57423,
            NamedKey::End => 57424,
            NamedKey::Insert => 57425,
            NamedKey::Delete => 57426,
            NamedKey::Clear => 57427,
            _ => return None,
        };
        return Some(Functional::u(number));
    }
    let PhysicalKey::Code(code) = input.physical else {
        return None;
    };
    use KeyCode::*;
    let digits = [
        Numpad0, Numpad1, Numpad2, Numpad3, Numpad4, Numpad5, Numpad6, Numpad7, Numpad8, Numpad9,
    ];
    if let Some(i) = digits.iter().position(|&k| k == code) {
        return Some(Functional::u(57399 + i as u32));
    }
    let number = match code {
        NumpadDecimal => 57409,
        NumpadDivide => 57410,
        NumpadMultiply => 57411,
        NumpadSubtract => 57412,
        NumpadAdd => 57413,
        NumpadEnter => 57414,
        NumpadEqual => 57415,
        NumpadComma => 57416,
        _ => return None,
    };
    Some(Functional::u(number))
}

#[cfg(test)]
mod tests {
    use super::*;
    use winit::keyboard::NativeKeyCode;

    const DISAMBIGUATE: TermMode = TermMode::DISAMBIGUATE_ESC_CODES;
    const EVENTS: TermMode = TermMode::REPORT_EVENT_TYPES;
    const ALTERNATES: TermMode = TermMode::REPORT_ALTERNATE_KEYS;
    const ALL_KEYS: TermMode = TermMode::REPORT_ALL_KEYS_AS_ESC;
    const TEXT: TermMode = TermMode::REPORT_ASSOCIATED_TEXT;

    /// A key press, changed with the builder methods.
    struct Press {
        key: Key,
        unmodified: Key,
        text: Option<&'static str>,
        location: KeyLocation,
        physical: PhysicalKey,
        event: KeyEventKind,
        shift: bool,
        ctrl: bool,
        meta: bool,
        super_key: bool,
    }

    fn named(key: NamedKey) -> Press {
        let key = Key::Named(key);
        let text = match key {
            Key::Named(NamedKey::Space) => Some(" "),
            _ => None,
        };
        Press {
            unmodified: key.clone(),
            key,
            text,
            location: KeyLocation::Standard,
            physical: PhysicalKey::Unidentified(NativeKeyCode::Unidentified),
            event: KeyEventKind::Press,
            shift: false,
            ctrl: false,
            meta: false,
            super_key: false,
        }
    }

    fn ch(c: &'static str) -> Press {
        Press {
            key: Key::Character(c.into()),
            unmodified: Key::Character(c.into()),
            text: Some(c),
            ..named(NamedKey::Escape)
        }
    }

    impl Press {
        fn shift(mut self, shifted: &'static str) -> Self {
            self.shift = true;
            self.key = Key::Character(shifted.into());
            self.text = Some(shifted);
            self
        }
        fn shift_held(mut self) -> Self {
            self.shift = true;
            self
        }
        fn ctrl(mut self) -> Self {
            self.ctrl = true;
            self
        }
        fn meta(mut self) -> Self {
            self.meta = true;
            self
        }
        fn at(mut self, location: KeyLocation, code: KeyCode) -> Self {
            self.location = location;
            self.physical = PhysicalKey::Code(code);
            self
        }
        fn physical(mut self, code: KeyCode) -> Self {
            self.physical = PhysicalKey::Code(code);
            self
        }
        fn event(mut self, event: KeyEventKind) -> Self {
            self.event = event;
            self
        }
        fn encode(&self, mode: TermMode) -> Option<String> {
            let input = KeyInput {
                key: &self.key,
                unmodified: &self.unmodified,
                text: self.text,
                location: self.location,
                physical: self.physical,
                event: self.event,
                shift: self.shift,
                ctrl: self.ctrl,
                meta: self.meta,
                super_key: self.super_key,
            };
            encode(&input, mode).map(|bytes| String::from_utf8(bytes).unwrap())
        }
        fn sends(&self, mode: TermMode) -> String {
            self.encode(mode).expect("key sends nothing")
        }
    }

    #[test]
    fn disambiguate() {
        let m = DISAMBIGUATE;
        assert_eq!(named(NamedKey::Escape).sends(m), "\x1b[27u");
        assert_eq!(ch("a").ctrl().sends(m), "\x1b[97;5u");
        assert_eq!(ch("i").ctrl().sends(m), "\x1b[105;5u");
        assert_eq!(ch("b").meta().sends(m), "\x1b[98;3u");
        assert_eq!(ch("a").ctrl().shift("A").sends(m), "\x1b[97;6u");
        assert_eq!(named(NamedKey::Space).ctrl().sends(m), "\x1b[32;5u");
        assert_eq!(named(NamedKey::Tab).shift_held().sends(m), "\x1b[9;2u");
        assert_eq!(named(NamedKey::Enter).shift_held().sends(m), "\x1b[13;2u");
        // Text keys, and Enter, Tab and Backspace on their own, stay as
        // they were.
        assert_eq!(ch("a").sends(m), "a");
        assert_eq!(ch("a").shift("A").sends(m), "A");
        assert_eq!(ch("é").sends(m), "é");
        assert_eq!(named(NamedKey::Space).sends(m), " ");
        assert_eq!(named(NamedKey::Enter).sends(m), "\r");
        assert_eq!(named(NamedKey::Tab).sends(m), "\t");
        assert_eq!(named(NamedKey::Backspace).sends(m), "\x7f");
    }

    #[test]
    fn functional_keys() {
        let m = DISAMBIGUATE;
        let up = named(NamedKey::ArrowUp);
        assert_eq!(up.sends(m), "\x1b[A");
        // Application cursor mode no longer applies.
        assert_eq!(up.sends(m | TermMode::APP_CURSOR), "\x1b[A");
        let all = named(NamedKey::ArrowUp).shift_held().ctrl().meta();
        assert_eq!(all.sends(m), "\x1b[1;8A");
        assert_eq!(named(NamedKey::F1).sends(m), "\x1b[P");
        assert_eq!(named(NamedKey::F3).sends(m), "\x1b[13~");
        assert_eq!(named(NamedKey::F5).ctrl().sends(m), "\x1b[15;5~");
        assert_eq!(named(NamedKey::Delete).sends(m), "\x1b[3~");
        assert_eq!(named(NamedKey::F13).sends(m), "\x1b[57376u");
        assert_eq!(named(NamedKey::ContextMenu).sends(m), "\x1b[57363u");
    }

    #[test]
    fn keypad() {
        let m = DISAMBIGUATE;
        let five = ch("5").at(KeyLocation::Numpad, KeyCode::Numpad5);
        assert_eq!(five.sends(m), "5");
        assert_eq!(five.ctrl().sends(m), "\x1b[57404;5u");
        assert_eq!(
            ch("5")
                .at(KeyLocation::Numpad, KeyCode::Numpad5)
                .sends(m | ALL_KEYS),
            "\x1b[57404u"
        );
        let enter = named(NamedKey::Enter).at(KeyLocation::Numpad, KeyCode::NumpadEnter);
        assert_eq!(enter.sends(m), "\x1b[57414u");
        // Num Lock off: the key moves the cursor.
        let up = named(NamedKey::ArrowUp).at(KeyLocation::Numpad, KeyCode::Numpad8);
        assert_eq!(up.sends(m), "\x1b[57419u");
    }

    #[test]
    fn event_types() {
        let m = DISAMBIGUATE | EVENTS;
        let a = || ch("a").physical(KeyCode::KeyA);
        assert_eq!(a().event(KeyEventKind::Repeat).sends(m), "a");
        assert_eq!(a().event(KeyEventKind::Release).sends(m), "\x1b[97;1:3u");
        assert_eq!(
            a().ctrl().event(KeyEventKind::Repeat).sends(m),
            "\x1b[97;5:2u"
        );
        assert_eq!(
            named(NamedKey::ArrowUp)
                .event(KeyEventKind::Release)
                .sends(m),
            "\x1b[1;1:3A"
        );
        // Enter, Tab and Backspace releases need all keys as escape codes.
        let enter = || named(NamedKey::Enter).event(KeyEventKind::Release);
        assert_eq!(enter().encode(m), None);
        assert_eq!(enter().sends(m | ALL_KEYS), "\x1b[13;1:3u");
        // Without the flag, repeats are presses and releases send nothing.
        assert_eq!(
            a().ctrl().event(KeyEventKind::Repeat).sends(DISAMBIGUATE),
            "\x1b[97;5u"
        );
        assert_eq!(a().event(KeyEventKind::Release).encode(DISAMBIGUATE), None);
    }

    #[test]
    fn all_keys_as_escape_codes() {
        let m = ALL_KEYS;
        assert_eq!(ch("a").sends(m), "\x1b[97u");
        assert_eq!(ch("a").shift("A").sends(m), "\x1b[97;2u");
        assert_eq!(named(NamedKey::Enter).sends(m), "\x1b[13u");
        assert_eq!(ch("a").sends(m | TEXT), "\x1b[97;;97u");
        assert_eq!(ch("a").shift("A").sends(m | TEXT), "\x1b[97;2;65u");
        // Ctrl types no text.
        assert_eq!(ch("a").ctrl().sends(m | TEXT), "\x1b[97;5u");
        // Text alone does nothing without all keys as escape codes.
        assert_eq!(ch("a").sends(DISAMBIGUATE | TEXT), "a");
    }

    #[test]
    fn modifier_keys() {
        let shift = || named(NamedKey::Shift).at(KeyLocation::Left, KeyCode::ShiftLeft);
        assert_eq!(shift().encode(DISAMBIGUATE), None);
        assert_eq!(shift().sends(ALL_KEYS), "\x1b[57441;2u");
        // The modifier state may or may not include the key yet.
        let held = shift().shift_held().event(KeyEventKind::Release);
        assert_eq!(held.sends(ALL_KEYS | EVENTS), "\x1b[57441;1:3u");
        let right_ctrl = named(NamedKey::Control).at(KeyLocation::Right, KeyCode::ControlRight);
        assert_eq!(right_ctrl.sends(ALL_KEYS), "\x1b[57448;5u");
    }

    #[test]
    fn alternate_keys() {
        let m = ALL_KEYS | ALTERNATES;
        let a = || ch("a").physical(KeyCode::KeyA);
        assert_eq!(a().sends(m), "\x1b[97u");
        assert_eq!(a().shift("A").sends(m), "\x1b[97:65;2u");
        // Russian: the key C types "с", its base layout key is "c".
        let es = ch("с").physical(KeyCode::KeyC);
        assert_eq!(es.sends(m), "\x1b[1089::99u");
        assert_eq!(
            es.ctrl().sends(DISAMBIGUATE | ALTERNATES),
            "\x1b[1089::99;5u"
        );
        // German: Shift+ß types "?".
        let sz = ch("ß").physical(KeyCode::Minus).shift("?");
        assert_eq!(sz.sends(m), "\x1b[223:63:45;2u");
    }

    #[test]
    fn altgr_types_text() {
        // AltGr+Q on a German layout, reported as Ctrl+Alt on Windows.
        let at = || {
            let mut key = ch("q").ctrl().meta();
            key.key = Key::Character("@".into());
            key.text = Some("@");
            key
        };
        assert_eq!(at().sends(DISAMBIGUATE), "@");
        assert_eq!(at().sends(ALL_KEYS | TEXT), "\x1b[113;;64u");
    }
}
