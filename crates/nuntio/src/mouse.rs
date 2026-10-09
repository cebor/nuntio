//! Mouse reporting (X10/normal, SGR 1006, UTF-8 1005) and click counting.

use std::time::{Duration, Instant};

use nuntio_term::TermMode;

/// Clicks closer together than this count as double/triple clicks.
pub const MULTI_CLICK_INTERVAL: Duration = Duration::from_millis(400);

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

/// The buttons currently held down and reported to the application.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HeldButtons(u8);

impl HeldButtons {
    fn bit(button: Button) -> u8 {
        match button {
            Button::Left => 1,
            Button::Middle => 2,
            Button::Right => 4,
            Button::WheelUp | Button::WheelDown => 0,
        }
    }

    /// Wheel buttons are ignored.
    pub fn insert(&mut self, button: Button) {
        self.0 |= Self::bit(button);
    }

    /// Returns whether the button was held.
    pub fn remove(&mut self, button: Button) -> bool {
        let bit = Self::bit(button);
        let held = self.0 & bit != 0;
        self.0 &= !bit;
        held
    }

    /// The held button that motion is reported for: Left, Middle, then Right.
    pub fn first(self) -> Option<Button> {
        [Button::Left, Button::Middle, Button::Right]
            .into_iter()
            .find(|&b| self.0 & Self::bit(b) != 0)
    }

    pub fn clear(&mut self) {
        self.0 = 0;
    }
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
    // Coordinates beyond the range of the encoding are clamped to its last
    // value, as xterm does, so a release far out is still reported.
    let limit = if mode.contains(TermMode::UTF8_MOUSE) {
        2015
    } else {
        223
    };
    for v in [x.min(limit), y.min(limit)] {
        if mode.contains(TermMode::UTF8_MOUSE) {
            // At most U+07FF: two bytes.
            let c = char::from_u32(32 + v)?;
            let mut buf = [0; 4];
            out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
        } else {
            out.push((32 + v) as u8);
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
    fn utf8_encoding_switches_to_two_bytes_and_ends_at_2015() {
        let mode = TermMode::MOUSE_REPORT_CLICK | TermMode::UTF8_MOUSE;
        let left = Some(Button::Left);
        let at = |column| encode_report(left, MouseAction::Press, NONE, column, 0, mode);
        // x = column + 1; the byte is 32 + x, y = 1 -> '!'.
        assert_eq!(at(94).unwrap(), [0x1b, b'[', b'M', 32, 0x7f, 33]);
        assert_eq!(at(95).unwrap(), [0x1b, b'[', b'M', 32, 0xC2, 0x80, 33]);
        assert_eq!(at(2014).unwrap(), [0x1b, b'[', b'M', 32, 0xDF, 0xBF, 33]);
        // Past the end of the range the coordinate is clamped, as in xterm.
        assert_eq!(at(2015), at(2014));
        assert_eq!(at(5000), at(2014));
        // The line is limited the same way.
        assert_eq!(
            encode_report(left, MouseAction::Press, NONE, 0, 2015, mode).unwrap(),
            [0x1b, b'[', b'M', 32, 33, 0xDF, 0xBF]
        );
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
        // Coordinates past 223 are clamped, so releases are never lost.
        let far = |x, y| encode_report(right, MouseAction::Press, NONE, x, y, mode).unwrap();
        assert_eq!(far(300, 0), [0x1b, b'[', b'M', 32 + 2, 255, 32 + 1]);
        assert_eq!(
            encode_report(right, MouseAction::Release, NONE, 300, 300, mode).unwrap(),
            [0x1b, b'[', b'M', 32 + 3, 255, 255]
        );
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
