//! Keyboard input to PTY bytes. Covers the basics for M1; modifier
//! combinations on special keys, keypad and macOS Option-as-Meta follow in M2.

use nuntio_term::TermMode;
use winit::event::KeyEvent;
use winit::keyboard::{Key, ModifiersState, NamedKey};

pub fn encode_key(event: &KeyEvent, mods: ModifiersState, mode: TermMode) -> Option<Vec<u8>> {
    if let Key::Named(named) = &event.logical_key
        && let Some(bytes) = encode_named(*named, mode)
    {
        return Some(prefix_alt(bytes.to_vec(), mods));
    }

    if mods.control_key()
        && let Key::Character(s) = &event.logical_key
        && let Some(byte) = control_byte(s)
    {
        return Some(prefix_alt(vec![byte], mods));
    }

    let text = event.text.as_ref()?;
    if text.is_empty() {
        return None;
    }
    Some(prefix_alt(text.as_bytes().to_vec(), mods))
}

fn prefix_alt(mut bytes: Vec<u8>, mods: ModifiersState) -> Vec<u8> {
    // Alt sends ESC first, like xterm with metaSendsEscape.
    if mods.alt_key() && !cfg!(target_os = "macos") {
        bytes.insert(0, 0x1b);
    }
    bytes
}

fn encode_named(key: NamedKey, mode: TermMode) -> Option<&'static [u8]> {
    let app_cursor = mode.contains(TermMode::APP_CURSOR);
    let bytes: &[u8] = match key {
        NamedKey::Enter => b"\r",
        NamedKey::Backspace => b"\x7f",
        NamedKey::Tab => b"\t",
        NamedKey::Escape => b"\x1b",
        NamedKey::ArrowUp if app_cursor => b"\x1bOA",
        NamedKey::ArrowDown if app_cursor => b"\x1bOB",
        NamedKey::ArrowRight if app_cursor => b"\x1bOC",
        NamedKey::ArrowLeft if app_cursor => b"\x1bOD",
        NamedKey::Home if app_cursor => b"\x1bOH",
        NamedKey::End if app_cursor => b"\x1bOF",
        NamedKey::ArrowUp => b"\x1b[A",
        NamedKey::ArrowDown => b"\x1b[B",
        NamedKey::ArrowRight => b"\x1b[C",
        NamedKey::ArrowLeft => b"\x1b[D",
        NamedKey::Home => b"\x1b[H",
        NamedKey::End => b"\x1b[F",
        NamedKey::Insert => b"\x1b[2~",
        NamedKey::Delete => b"\x1b[3~",
        NamedKey::PageUp => b"\x1b[5~",
        NamedKey::PageDown => b"\x1b[6~",
        NamedKey::F1 => b"\x1bOP",
        NamedKey::F2 => b"\x1bOQ",
        NamedKey::F3 => b"\x1bOR",
        NamedKey::F4 => b"\x1bOS",
        NamedKey::F5 => b"\x1b[15~",
        NamedKey::F6 => b"\x1b[17~",
        NamedKey::F7 => b"\x1b[18~",
        NamedKey::F8 => b"\x1b[19~",
        NamedKey::F9 => b"\x1b[20~",
        NamedKey::F10 => b"\x1b[21~",
        NamedKey::F11 => b"\x1b[23~",
        NamedKey::F12 => b"\x1b[24~",
        _ => return None,
    };
    Some(bytes)
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
        '@' | ' ' | '2' => Some(0x00),
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

    #[test]
    fn control_letters() {
        assert_eq!(control_byte("c"), Some(0x03));
        assert_eq!(control_byte("C"), Some(0x03));
        assert_eq!(control_byte("d"), Some(0x04));
        assert_eq!(control_byte("["), Some(0x1b));
        assert_eq!(control_byte("é"), None);
        assert_eq!(control_byte("ab"), None);
    }

    #[test]
    fn cursor_keys_follow_decckm() {
        let normal = TermMode::empty();
        assert_eq!(
            encode_named(NamedKey::ArrowUp, normal),
            Some(&b"\x1b[A"[..])
        );
        assert_eq!(
            encode_named(NamedKey::ArrowUp, TermMode::APP_CURSOR),
            Some(&b"\x1bOA"[..])
        );
    }
}
