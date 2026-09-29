//! The modes wezterm-term doesn't track, read from the PTY output before
//! it reaches the terminal: alternate scroll (1007) and synchronized
//! output (2026).

/// Synchronized updates start and end with exactly these bytes, as in
/// vte, which alacritty uses.
const SYNC_PREFIX: &[u8] = b"\x1b[?2026";

/// Where a synchronized update starts or ends: the offset right after
/// its `CSI ? 2026 h` or `CSI ? 2026 l` within the bytes fed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SyncEvent {
    Begin(usize),
    End(usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Ground,
    Esc,
    /// Parameters of a CSI sequence.
    Csi,
}

/// Follows escape sequences across reads.
#[derive(Debug)]
pub(crate) struct ModeScanner {
    state: State,
    /// The CSI started with `?`.
    private: bool,
    /// The CSI has intermediate bytes (like `$` in DECRQM).
    intermediate: bool,
    /// The parameter being read, saturating.
    param: u32,
    /// Some parameter of the CSI is 1007.
    alt_scroll_param: bool,
    /// The CSI has a byte besides the marker, parameters and intermediates.
    invalid: bool,
    /// Bytes of `SYNC_PREFIX` matched so far.
    sync_matched: usize,
    alt_scroll: bool,
}

impl Default for ModeScanner {
    fn default() -> Self {
        Self {
            state: State::Ground,
            private: false,
            intermediate: false,
            param: 0,
            alt_scroll_param: false,
            invalid: false,
            sync_matched: 0,
            // Like alacritty's default mode.
            alt_scroll: true,
        }
    }
}

impl ModeScanner {
    /// Alternate scroll (DECSET 1007): the wheel sends arrow keys on the
    /// alternate screen.
    pub(crate) fn alt_scroll(&self) -> bool {
        self.alt_scroll
    }

    /// Scan more output. Returns where synchronized updates start and end.
    pub(crate) fn feed(&mut self, bytes: &[u8]) -> Vec<SyncEvent> {
        let mut events = Vec::new();
        for (i, &byte) in bytes.iter().enumerate() {
            self.match_sync(byte, i + 1, &mut events);
            self.state = match (self.state, byte) {
                (_, 0x1b) => State::Esc,
                // Like xterm: CAN and SUB abort a sequence.
                (_, 0x18 | 0x1a) => State::Ground,
                (State::Ground, _) => State::Ground,
                (State::Esc, b'[') => {
                    self.private = false;
                    self.intermediate = false;
                    self.param = 0;
                    self.alt_scroll_param = false;
                    self.invalid = false;
                    State::Csi
                }
                // RIS, a full reset.
                (State::Esc, b'c') => {
                    self.alt_scroll = true;
                    State::Ground
                }
                (State::Esc, _) => State::Ground,
                (State::Csi, b'0'..=b'9') => {
                    if self.intermediate {
                        self.invalid = true;
                    }
                    self.param = self
                        .param
                        .saturating_mul(10)
                        .saturating_add(u32::from(byte - b'0'));
                    State::Csi
                }
                (State::Csi, b';' | b':') => {
                    self.end_param();
                    State::Csi
                }
                (State::Csi, b'?') => {
                    // The marker only counts as the first byte.
                    if self.private || self.param != 0 || self.alt_scroll_param {
                        self.invalid = true;
                    }
                    self.private = true;
                    State::Csi
                }
                (State::Csi, 0x20..=0x2f) => {
                    self.intermediate = true;
                    State::Csi
                }
                (State::Csi, 0x40..=0x7e) => {
                    self.end_param();
                    let set = match byte {
                        b'h' => Some(true),
                        b'l' => Some(false),
                        _ => None,
                    };
                    if let Some(set) = set
                        && self.private
                        && !self.intermediate
                        && !self.invalid
                        && self.alt_scroll_param
                    {
                        self.alt_scroll = set;
                    }
                    State::Ground
                }
                // Other C0 controls run inside a sequence without ending it.
                (State::Csi, 0x00..=0x1f) => State::Csi,
                (State::Csi, _) => {
                    self.invalid = true;
                    State::Csi
                }
            };
        }
        events
    }

    fn end_param(&mut self) {
        if self.param == 1007 {
            self.alt_scroll_param = true;
        }
        self.param = 0;
    }

    /// Match the exact BSU/ESU bytes; `end` is the offset after `byte`.
    fn match_sync(&mut self, byte: u8, end: usize, events: &mut Vec<SyncEvent>) {
        if self.sync_matched == SYNC_PREFIX.len() {
            match byte {
                b'h' => events.push(SyncEvent::Begin(end)),
                b'l' => events.push(SyncEvent::End(end)),
                _ => (),
            }
            self.sync_matched = 0;
        } else if byte == SYNC_PREFIX[self.sync_matched] {
            self.sync_matched += 1;
            return;
        } else {
            self.sync_matched = 0;
        }
        // Only ESC starts the prefix; it appears nowhere else in it.
        if byte == 0x1b {
            self.sync_matched = 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alternate_scroll_follows_decset() {
        let mut scanner = ModeScanner::default();
        assert!(scanner.alt_scroll());
        scanner.feed(b"x\x1b[?1007l");
        assert!(!scanner.alt_scroll());
        scanner.feed(b"\x1b[?1049;1007h");
        assert!(scanner.alt_scroll());
        // Other modes, and 1007 without `?`, leave it alone.
        scanner.feed(b"\x1b[?1049l\x1b[1007l\x1b[?10070l");
        assert!(scanner.alt_scroll());
    }

    #[test]
    fn a_sequence_may_span_reads() {
        let mut scanner = ModeScanner::default();
        scanner.feed(b"\x1b[?10");
        assert!(scanner.alt_scroll());
        scanner.feed(b"07l");
        assert!(!scanner.alt_scroll());
    }

    #[test]
    fn full_reset_turns_alternate_scroll_on() {
        let mut scanner = ModeScanner::default();
        scanner.feed(b"\x1b[?1007l");
        scanner.feed(b"\x1bc");
        assert!(scanner.alt_scroll());
    }

    #[test]
    fn synchronized_updates_are_located() {
        let mut scanner = ModeScanner::default();
        assert_eq!(
            scanner.feed(b"ab\x1b[?2026hcd\x1b[?2026le"),
            [SyncEvent::Begin(10), SyncEvent::End(20)]
        );
        // Split across reads, the offset is within the read that ends it.
        assert_eq!(scanner.feed(b"x\x1b[?20"), []);
        assert_eq!(scanner.feed(b"26hy"), [SyncEvent::Begin(3)]);
        // ESC restarts the match.
        assert_eq!(scanner.feed(b"\x1b[\x1b[?2026l"), [SyncEvent::End(10)]);
    }

    #[test]
    fn a_mode_query_is_no_synchronized_update() {
        let mut scanner = ModeScanner::default();
        assert_eq!(scanner.feed(b"\x1b[?2026$p"), []);
        assert_eq!(scanner.feed(b"\x1b[?2026;1h"), []);
    }
}
