//! Terminal setup shared by the apps: raw mode and the alternate screen
//! (ratatui), plus bracketed paste, mouse capture and focus events.

use std::io::{self, Write};

use anyhow::{Context, Result};
use base64::Engine;
use ratatui::crossterm::event::{
    DisableBracketedPaste, DisableFocusChange, DisableMouseCapture, EnableBracketedPaste,
    EnableFocusChange, EnableMouseCapture, KeyCode, KeyEvent, KeyEventKind, KeyModifiers,
};
use ratatui::crossterm::execute;

/// Set up the terminal. Fails with an error instead of a panic when there
/// is no terminal. The extra modes are switched off again if the program
/// panics: the panic message goes to a terminal that keeps them on
/// otherwise.
pub fn init() -> Result<ratatui::DefaultTerminal> {
    static HOOK: std::sync::Once = std::sync::Once::new();
    let terminal = ratatui::try_init().context("cannot set up the terminal")?;
    HOOK.call_once(|| {
        // After ratatui's hook (installed by `try_init`), which restores the
        // screen: this one runs first.
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            restore_modes();
            previous(info);
        }));
    });
    execute!(
        io::stdout(),
        EnableBracketedPaste,
        EnableMouseCapture,
        EnableFocusChange
    )?;
    Ok(terminal)
}

/// Undo [`init`].
pub fn restore() {
    restore_modes();
    ratatui::restore();
}

fn restore_modes() {
    let _ = execute!(
        io::stdout(),
        DisableBracketedPaste,
        DisableMouseCapture,
        DisableFocusChange
    );
}

/// The key as the apps see it: no releases, AltGr characters (Ctrl+Alt on
/// Windows) as plain text, Ctrl+letters lower-case.
pub fn normalize(mut event: KeyEvent) -> Option<KeyEvent> {
    if event.kind == KeyEventKind::Release {
        return None;
    }
    let both = KeyModifiers::CONTROL | KeyModifiers::ALT;
    if let KeyCode::Char(c) = event.code {
        if cfg!(windows) && event.modifiers.contains(both) {
            event.modifiers.remove(both);
        } else if event.modifiers.contains(KeyModifiers::CONTROL) {
            event.code = KeyCode::Char(c.to_ascii_lowercase());
        }
    }
    Some(event)
}

/// Put `text` on the clipboard with OSC 52 (nuntio honours it with
/// `clipboard_write = true`, the default).
pub fn copy_to_clipboard(text: &str) -> io::Result<()> {
    let encoded = base64::engine::general_purpose::STANDARD.encode(text);
    let mut out = io::stdout();
    write!(out, "\x1b]52;c;{encoded}\x07")?;
    out.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn altgr_characters_are_text_not_ctrl() {
        let ev = |c, m| KeyEvent::new(KeyCode::Char(c), m);
        let key = |c, m| Some(ev(c, m));
        if cfg!(windows) {
            assert_eq!(
                normalize(ev('@', KeyModifiers::CONTROL | KeyModifiers::ALT)),
                key('@', KeyModifiers::NONE)
            );
        } else {
            assert_eq!(
                normalize(ev('D', KeyModifiers::CONTROL | KeyModifiers::ALT)),
                key('d', KeyModifiers::CONTROL | KeyModifiers::ALT)
            );
        }
        assert_eq!(
            normalize(ev('N', KeyModifiers::CONTROL | KeyModifiers::SHIFT)),
            key('n', KeyModifiers::CONTROL | KeyModifiers::SHIFT)
        );
        let mut release = ev('a', KeyModifiers::NONE);
        release.kind = KeyEventKind::Release;
        assert_eq!(normalize(release), None);
    }
}
