//! Splits PTY output into text for the parser, inline images (iTerm2's
//! `ESC ] 1337 ; File=<args> : <base64> BEL|ST`) and cell size queries
//! (`CSI 16 t`), also when a sequence is split across reads.

/// Start of an inline image.
const OSC_FILE: &[u8] = b"\x1b]1337;File=";
/// The cell size query, which alacritty leaves unanswered.
const CSI_CELL_SIZE: &[u8] = b"\x1b[16t";
/// Longest argument list that is kept; longer sequences are dropped.
const MAX_ARGS: usize = 4096;
/// Largest base64 payload that is kept; larger images are dropped.
const MAX_PAYLOAD: usize = 64 << 20;

const BEL: u8 = 0x07;
const ESC: u8 = 0x1b;
const CAN: u8 = 0x18;
const SUB: u8 = 0x1a;

pub(crate) enum ScanEvent<'a> {
    /// Output for the parser, unchanged.
    Text(&'a [u8]),
    Image(ImageRequest),
    CellSizeQuery,
}

/// An inline image as the program sent it.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ImageRequest {
    /// The `key=value` list between `File=` and `:`.
    pub args: Vec<u8>,
    /// The base64 data, without whitespace.
    pub payload: Vec<u8>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum State {
    #[default]
    Ground,
    /// `held` starts with ESC and is a prefix of a recognized sequence.
    Prefix,
    Args,
    /// ESC in the arguments: ends the sequence.
    ArgsEsc,
    Payload,
    /// ESC in the payload: ends the sequence, which is dispatched.
    PayloadEsc,
    /// A sequence over a limit, dropped up to its end.
    Discard,
    DiscardEsc,
}

#[derive(Default)]
pub(crate) struct ImageScanner {
    state: State,
    /// Bytes held back while they may start a recognized sequence. Passing
    /// them on early would leave the parser inside an OSC once the rest is
    /// stripped.
    held: Vec<u8>,
    args: Vec<u8>,
    payload: Vec<u8>,
}

impl ImageScanner {
    pub(crate) fn feed(&mut self, input: &[u8], on_event: &mut impl FnMut(ScanEvent<'_>)) {
        let mut i = 0;
        while i < input.len() {
            if self.state == State::Ground {
                let end = input[i..]
                    .iter()
                    .position(|&b| b == ESC)
                    .map_or(input.len(), |p| i + p);
                if end > i {
                    on_event(ScanEvent::Text(&input[i..end]));
                }
                if end == input.len() {
                    break;
                }
                self.held.push(ESC);
                self.state = State::Prefix;
                i = end + 1;
            } else {
                self.byte(input[i], on_event);
                i += 1;
            }
        }
    }

    fn byte(&mut self, b: u8, on_event: &mut impl FnMut(ScanEvent<'_>)) {
        match self.state {
            State::Ground => unreachable!("ground bytes are handled in runs"),
            State::Prefix => self.prefix(b, on_event),
            State::Args => match b {
                b':' => self.state = State::Payload,
                BEL | CAN | SUB => self.reset(),
                ESC => self.state = State::ArgsEsc,
                0..0x20 => {}
                _ => {
                    self.args.push(b);
                    if self.args.len() > MAX_ARGS {
                        self.args = Vec::new();
                        self.state = State::Discard;
                    }
                }
            },
            State::ArgsEsc => {
                self.reset();
                self.after_esc(b, on_event);
            }
            State::Payload => match b {
                b' ' | b'\t' | b'\r' | b'\n' => {}
                BEL => self.dispatch(on_event),
                ESC => self.state = State::PayloadEsc,
                CAN | SUB => self.reset(),
                0..0x20 => {}
                // Checked before the push, which could double the buffer.
                _ if self.payload.len() == MAX_PAYLOAD => {
                    tracing::warn!("inline image larger than 64 MiB ignored");
                    self.reset();
                    self.state = State::Discard;
                }
                _ => self.payload.push(b),
            },
            State::PayloadEsc => {
                self.dispatch(on_event);
                self.after_esc(b, on_event);
            }
            State::Discard => match b {
                BEL | CAN | SUB => self.reset(),
                ESC => self.state = State::DiscardEsc,
                _ => {}
            },
            State::DiscardEsc => {
                self.reset();
                self.after_esc(b, on_event);
            }
        }
    }

    /// A byte while `held` may still become a recognized sequence.
    fn prefix(&mut self, b: u8, on_event: &mut impl FnMut(ScanEvent<'_>)) {
        self.held.push(b);
        if self.held == CSI_CELL_SIZE {
            self.held.clear();
            self.state = State::Ground;
            on_event(ScanEvent::CellSizeQuery);
        } else if self.held == OSC_FILE {
            self.held.clear();
            self.state = State::Args;
        } else if !(OSC_FILE.starts_with(&self.held) || CSI_CELL_SIZE.starts_with(&self.held)) {
            // No match: pass the bytes on. A new ESC may start a sequence.
            if b == ESC {
                self.held.pop();
                on_event(ScanEvent::Text(&self.held));
                self.held.clear();
                self.held.push(ESC);
            } else {
                on_event(ScanEvent::Text(&self.held));
                self.held.clear();
                self.state = State::Ground;
            }
        }
    }

    /// The byte after an ESC that ended a sequence: `\` completes ST, any
    /// other byte starts a new sequence with that ESC, as in vte.
    fn after_esc(&mut self, b: u8, on_event: &mut impl FnMut(ScanEvent<'_>)) {
        if b != b'\\' {
            self.held.push(ESC);
            self.state = State::Prefix;
            self.prefix(b, on_event);
        }
    }

    fn dispatch(&mut self, on_event: &mut impl FnMut(ScanEvent<'_>)) {
        let request = ImageRequest {
            args: std::mem::take(&mut self.args),
            payload: std::mem::take(&mut self.payload),
        };
        self.state = State::Ground;
        on_event(ScanEvent::Image(request));
    }

    /// Drop the sequence and go back to ground.
    fn reset(&mut self) {
        self.args = Vec::new();
        self.payload = Vec::new();
        self.state = State::Ground;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, PartialEq, Eq)]
    enum Owned {
        Text(Vec<u8>),
        Image(ImageRequest),
        CellSizeQuery,
    }

    /// The events of feeding `chunks` one after another, with adjacent
    /// text merged.
    fn scan(chunks: &[&[u8]]) -> Vec<Owned> {
        let mut scanner = ImageScanner::default();
        let mut events = Vec::new();
        for chunk in chunks {
            scanner.feed(chunk, &mut |event| match event {
                ScanEvent::Text(text) => match events.last_mut() {
                    Some(Owned::Text(prev)) => prev.extend_from_slice(text),
                    _ => events.push(Owned::Text(text.to_vec())),
                },
                ScanEvent::Image(request) => events.push(Owned::Image(request)),
                ScanEvent::CellSizeQuery => events.push(Owned::CellSizeQuery),
            });
        }
        events
    }

    fn text(s: &[u8]) -> Owned {
        Owned::Text(s.to_vec())
    }

    fn image(args: &[u8], payload: &[u8]) -> Owned {
        Owned::Image(ImageRequest {
            args: args.to_vec(),
            payload: payload.to_vec(),
        })
    }

    #[test]
    fn other_output_passes_unchanged() {
        let input: &[u8] = b"a\x1b]0;t\x07b\x1b[1;2Hc\x1b[16;2t\x1b\x1b]133;A\x07";
        assert_eq!(scan(&[input]), [text(input)]);
        for split in 1..input.len() {
            let (head, tail) = input.split_at(split);
            assert_eq!(scan(&[head, tail]), [text(input)], "split at {split}");
        }
        assert_eq!(scan(&[b"\x1b]13", b"38;x\x07"]), [text(b"\x1b]1338;x\x07")]);
    }

    #[test]
    fn split_image_with_bel_and_st() {
        for end in [&b"\x07"[..], b"\x1b\\"] {
            let tail = [&b"WFla\nWQ="[..], end, b"after"].concat();
            assert_eq!(
                scan(&[
                    b"before\x1b]1337;Fi",
                    b"le=inline=1;width=2:YW Jj\r\n",
                    &tail
                ]),
                [
                    text(b"before"),
                    image(b"inline=1;width=2", b"YWJjWFlaWQ="),
                    text(b"after"),
                ]
            );
        }
    }

    #[test]
    fn esc_in_payload_ends_the_image() {
        assert_eq!(
            scan(&[b"\x1b]1337;File=inline=1:AAAA\x1b[1mX"]),
            [image(b"inline=1", b"AAAA"), text(b"\x1b[1mX")]
        );
    }

    #[test]
    fn can_aborts() {
        assert_eq!(
            scan(&[b"\x1b]1337;File=inline=1:AAAA\x18next"]),
            [text(b"next")]
        );
    }

    #[test]
    fn oversized_payload_is_dropped() {
        let payload = vec![b'A'; MAX_PAYLOAD + 10];
        assert_eq!(
            scan(&[b"\x1b]1337;File=inline=1:", &payload, b"\x07next"]),
            [text(b"next")]
        );
    }

    #[test]
    fn cell_size_query() {
        assert_eq!(
            scan(&[b"a\x1b[1", b"6tb"]),
            [text(b"a"), Owned::CellSizeQuery, text(b"b")]
        );
        assert_eq!(scan(&[b"\x1b[16;1t"]), [text(b"\x1b[16;1t")]);
    }
}
