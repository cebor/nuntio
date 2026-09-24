//! Mouse reporting (X10/normal, SGR 1006, UTF-8 1005) and click counting.

use std::time::{Duration, Instant};

use nuntio_term::TermMode;

/// Clicks closer together than this count as double/triple clicks.
const MULTI_CLICK_INTERVAL: Duration = Duration::from_millis(400);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Button {
    Left,
    Middle,
    Right,
    WheelUp,
    WheelDown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseAction {
    Press,
    Release,
    /// Pointer moved; `button` is the one held down, if any.
    Motion,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MouseMods {
    pub shift: bool,
    pub alt: bool,
    pub ctrl: bool,
}

/// The application wants mouse events instead of local selection.
pub fn reporting_enabled(mode: TermMode) -> bool {
    mode.intersects(TermMode::MOUSE_MODE)
}

/// Encode a mouse event for the PTY, or `None` if the current mode doesn't
/// want it. `column`/`line` are 0-based viewport coordinates.
pub fn encode_report(
    button: Option<Button>,
    action: MouseAction,
    mods: MouseMods,
    column: usize,
    line: usize,
    mode: TermMode,
) -> Option<Vec<u8>> {
    if !reporting_enabled(mode) {
        return None;
    }
    let is_wheel = matches!(button, Some(Button::WheelUp | Button::WheelDown));
    match action {
        MouseAction::Motion => {
            let wanted = mode.contains(TermMode::MOUSE_MOTION)
                || (button.is_some() && mode.contains(TermMode::MOUSE_DRAG));
            if !wanted {
                return None;
            }
        }
        MouseAction::Release if is_wheel => return None,
        _ => {}
    }

    let mut code: u32 = match button {
        Some(Button::Left) => 0,
        Some(Button::Middle) => 1,
        Some(Button::Right) => 2,
        Some(Button::WheelUp) => 64,
        Some(Button::WheelDown) => 65,
        None => 3,
    };
    if action == MouseAction::Motion {
        code += 32;
    }
    if mods.shift {
        code += 4;
    }
    if mods.alt {
        code += 8;
    }
    if mods.ctrl {
        code += 16;
    }

    let (x, y) = (column as u32 + 1, line as u32 + 1);
    if mode.contains(TermMode::SGR_MOUSE) {
        let suffix = if action == MouseAction::Release {
            'm'
        } else {
            'M'
        };
        return Some(format!("\x1b[<{code};{x};{y}{suffix}").into_bytes());
    }

    // Legacy encodings can't tell which button was released.
    if action == MouseAction::Release {
        code = (code & !0b11) | 3;
    }
    let mut out = b"\x1b[M".to_vec();
    out.push((32 + code) as u8);
    if mode.contains(TermMode::UTF8_MOUSE) {
        for v in [x, y] {
            let c = char::from_u32(32 + v).filter(|_| v <= 2015)?;
            let mut buf = [0; 4];
            out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
        }
    } else {
        for v in [x, y] {
            out.push(u8::try_from(32 + v).ok()?);
        }
    }
    Some(out)
}

/// Counts consecutive clicks on the same cell: 1, 2, 3, then wraps.
#[derive(Debug, Default)]
pub struct ClickCounter {
    last: Option<(Instant, usize, usize)>,
    count: u8,
}

impl ClickCounter {
    pub fn click(&mut self, now: Instant, column: usize, line: usize) -> u8 {
        let repeated = self.last.is_some_and(|(at, c, l)| {
            now.duration_since(at) < MULTI_CLICK_INTERVAL && (c, l) == (column, line)
        });
        self.count = if repeated { self.count % 3 + 1 } else { 1 };
        self.last = Some((now, column, line));
        self.count
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NONE: MouseMods = MouseMods {
        shift: false,
        alt: false,
        ctrl: false,
    };

    fn report(button: Option<Button>, action: MouseAction, mode: TermMode) -> Option<Vec<u8>> {
        encode_report(button, action, NONE, 4, 9, mode)
    }

    #[test]
    fn nothing_without_mouse_mode() {
        let left = Some(Button::Left);
        assert_eq!(report(left, MouseAction::Press, TermMode::empty()), None);
    }

    #[test]
    fn sgr_press_and_release() {
        let mode = TermMode::MOUSE_REPORT_CLICK | TermMode::SGR_MOUSE;
        let left = Some(Button::Left);
        assert_eq!(
            report(left, MouseAction::Press, mode).unwrap(),
            b"\x1b[<0;5;10M"
        );
        assert_eq!(
            report(left, MouseAction::Release, mode).unwrap(),
            b"\x1b[<0;5;10m"
        );
    }

    #[test]
    fn modifiers_and_wheel() {
        let mode = TermMode::MOUSE_REPORT_CLICK | TermMode::SGR_MOUSE;
        let mods = MouseMods {
            shift: true,
            alt: false,
            ctrl: true,
        };
        let wheel = Some(Button::WheelDown);
        assert_eq!(
            encode_report(wheel, MouseAction::Press, mods, 0, 0, mode).unwrap(),
            b"\x1b[<85;1;1M"
        );
        assert_eq!(report(wheel, MouseAction::Release, mode), None);
    }

    #[test]
    fn motion_depends_on_mode() {
        let left = Some(Button::Left);
        let click = TermMode::MOUSE_REPORT_CLICK | TermMode::SGR_MOUSE;
        assert_eq!(report(left, MouseAction::Motion, click), None);

        let drag = TermMode::MOUSE_DRAG | TermMode::SGR_MOUSE;
        assert_eq!(
            report(left, MouseAction::Motion, drag).unwrap(),
            b"\x1b[<32;5;10M"
        );
        assert_eq!(report(None, MouseAction::Motion, drag), None);

        let motion = TermMode::MOUSE_MOTION | TermMode::SGR_MOUSE;
        assert_eq!(
            report(None, MouseAction::Motion, motion).unwrap(),
            b"\x1b[<35;5;10M"
        );
    }

    #[test]
    fn legacy_encoding() {
        let mode = TermMode::MOUSE_REPORT_CLICK;
        let right = Some(Button::Right);
        assert_eq!(
            report(right, MouseAction::Press, mode).unwrap(),
            [0x1b, b'[', b'M', 32 + 2, 32 + 5, 32 + 10]
        );
        assert_eq!(
            report(right, MouseAction::Release, mode).unwrap(),
            [0x1b, b'[', b'M', 32 + 3, 32 + 5, 32 + 10]
        );
        // Out of range for single-byte coordinates.
        let far = encode_report(right, MouseAction::Press, NONE, 300, 0, mode);
        assert_eq!(far, None);
    }

    #[test]
    fn click_counter() {
        let mut counter = ClickCounter::default();
        let t = Instant::now();
        let ms = Duration::from_millis;
        assert_eq!(counter.click(t, 1, 1), 1);
        assert_eq!(counter.click(t + ms(100), 1, 1), 2);
        assert_eq!(counter.click(t + ms(200), 1, 1), 3);
        assert_eq!(counter.click(t + ms(300), 1, 1), 1);
        assert_eq!(counter.click(t + ms(350), 2, 1), 1);
        assert_eq!(counter.click(t + ms(2000), 2, 1), 1);
    }
}
