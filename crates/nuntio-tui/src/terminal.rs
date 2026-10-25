//! Terminal setup for the apps: raw mode and the alternate screen (ratatui),
//! bracketed paste, and the modes an app asks for.

use std::io::{self, Write};
use std::time::Duration;

use anyhow::{Context, Result};
use base64::Engine;
use ratatui::crossterm::event::{
    self, DisableBracketedPaste, DisableFocusChange, DisableMouseCapture, EnableBracketedPaste,
    EnableFocusChange, EnableMouseCapture, KeyCode, KeyEvent, KeyEventKind, KeyModifiers,
};
use ratatui::crossterm::execute;

/// The input modes an app switches on besides bracketed paste, which every
/// app uses.
#[derive(Debug, Clone, Copy, Default)]
pub struct Modes {
    pub mouse: bool,
    pub focus: bool,
}

/// Set up the terminal. Fails with an error instead of a panic when there
/// is no terminal, and leaves it as it found it when a mode cannot be
/// switched on. The modes are switched off again if the program panics: the
/// panic message goes to a terminal that keeps them on otherwise.
pub fn init(modes: Modes) -> Result<ratatui::DefaultTerminal> {
    // crossterm turns colours off under `NO_COLOR` but still writes `ESC[;m`
    // for them, which resets bold, strikethrough and every other attribute.
    // These apps are nuntio's own UI and their theme previews need colour,
    // so colour output is forced.
    ratatui::crossterm::style::force_color_output(true);
    static HOOK: std::sync::Once = std::sync::Once::new();
    let terminal = ratatui::try_init().context("cannot set up the terminal")?;
    HOOK.call_once(|| {
        // After ratatui's hook (installed by `try_init`), which restores the
        // screen: this one runs first.
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            restore_modes(modes);
            previous(info);
        }));
    });
    if let Err(err) = enable_modes(modes) {
        restore(modes);
        return Err(err.into());
    }
    Ok(terminal)
}

/// Undo [`init`].
pub fn restore(modes: Modes) {
    restore_modes(modes);
    ratatui::restore();
}

/// Run `app` on a set-up terminal and restore the terminal afterwards, also
/// when `app` fails. `app` may [`restore`] and [`init`] the terminal itself
/// (to run another program on the screen) and replace the terminal it is
/// given with the new one.
pub fn run(
    modes: Modes,
    app: impl FnOnce(&mut ratatui::DefaultTerminal) -> Result<()>,
) -> Result<()> {
    let mut terminal = init(modes)?;
    // Starts crossterm's event reader, which only then listens for
    // resizes: nuntio resizes a new split's pane right after starting it.
    let result = event::poll(Duration::ZERO)
        .map_err(Into::into)
        .and_then(|_| app(&mut terminal));
    restore(modes);
    result
}

fn enable_modes(modes: Modes) -> io::Result<()> {
    let mut out = io::stdout();
    execute!(out, EnableBracketedPaste)?;
    if modes.mouse {
        execute!(out, EnableMouseCapture)?;
    }
    if modes.focus {
        execute!(out, EnableFocusChange)?;
    }
    Ok(())
}

fn restore_modes(modes: Modes) {
    let mut out = io::stdout();
    let _ = execute!(out, DisableBracketedPaste);
    if modes.mouse {
        let _ = execute!(out, DisableMouseCapture);
    }
    if modes.focus {
        let _ = execute!(out, DisableFocusChange);
    }
}

/// The key as the apps see it: no releases, AltGr characters (Ctrl+Alt on
/// Windows) as plain text, Ctrl+letters lower-case.
pub fn normalize_key(mut event: KeyEvent) -> Option<KeyEvent> {
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
                normalize_key(ev('@', KeyModifiers::CONTROL | KeyModifiers::ALT)),
                key('@', KeyModifiers::NONE)
            );
        } else {
            assert_eq!(
                normalize_key(ev('D', KeyModifiers::CONTROL | KeyModifiers::ALT)),
                key('d', KeyModifiers::CONTROL | KeyModifiers::ALT)
            );
        }
        assert_eq!(
            normalize_key(ev('N', KeyModifiers::CONTROL | KeyModifiers::SHIFT)),
            key('n', KeyModifiers::CONTROL | KeyModifiers::SHIFT)
        );
        let mut release = ev('a', KeyModifiers::NONE);
        release.kind = KeyEventKind::Release;
        assert_eq!(normalize_key(release), None);
    }
}
