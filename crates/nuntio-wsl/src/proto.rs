//! The wire format between nuntio (Windows) and the helper (Linux, inside WSL).
//!
//! nuntio -> helper, on the helper's stdin, is a sequence of frames:
//! `[kind: u8][len: u32 LE][payload]`. [`DATA`] frames carry bytes for the shell's input,
//! [`RESIZE`] frames carry a [`Size`].
//!
//! helper -> nuntio, on the helper's stdout, is the raw output of the PTY, preceded by a
//! single [`READY`] marker once the shell is running. stderr carries the helper's own messages.

use std::fmt;

/// Written once to stdout when the shell has started. An unknown OSC, so it is harmless if it
/// ever reaches a terminal parser.
pub const READY: &[u8] = b"\x1b]nuntio-wsl;ready\x07";

/// A frame with bytes for the shell's input.
pub const DATA: u8 = 0;
/// A frame with a new window [`Size`].
pub const RESIZE: u8 = 1;
/// The largest payload of a frame.
pub const MAX_FRAME: usize = 1 << 20;

const HEADER: usize = 5;
const SIZE_BYTES: usize = 8;

/// The window size of the PTY.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Size {
    pub columns: u16,
    pub lines: u16,
    pub cell_width: u16,
    pub cell_height: u16,
}

/// A decoded frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Frame<'a> {
    Data(&'a [u8]),
    Resize(Size),
}

/// A frame the decoder can't accept. The stream is not usable afterwards.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameError {
    /// The announced payload is larger than [`MAX_FRAME`].
    TooLong(usize),
    /// A resize payload that is not 8 bytes.
    BadResize(usize),
}

impl fmt::Display for FrameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLong(len) => {
                write!(f, "frame of {len} bytes exceeds the limit of {MAX_FRAME}")
            }
            Self::BadResize(len) => {
                write!(f, "resize frame of {len} bytes, expected {SIZE_BYTES}")
            }
        }
    }
}

impl std::error::Error for FrameError {}

/// Appends `bytes` as data frames of at most [`MAX_FRAME`] bytes each. Nothing for no bytes.
pub fn encode_data(bytes: &[u8], out: &mut Vec<u8>) {
    for chunk in bytes.chunks(MAX_FRAME) {
        out.push(DATA);
        // MAX_FRAME fits in u32.
        out.extend_from_slice(&(chunk.len() as u32).to_le_bytes());
        out.extend_from_slice(chunk);
    }
}

/// Appends a resize frame.
pub fn encode_resize(size: Size, out: &mut Vec<u8>) {
    out.push(RESIZE);
    out.extend_from_slice(&(SIZE_BYTES as u32).to_le_bytes());
    for value in [size.columns, size.lines, size.cell_width, size.cell_height] {
        out.extend_from_slice(&value.to_le_bytes());
    }
}

/// Splits a byte stream into frames. Partial frames are buffered between calls.
#[derive(Debug, Default)]
pub struct Decoder {
    buf: Vec<u8>,
}

impl Decoder {
    /// Feeds `bytes` and calls `on_frame` for every complete frame. Frames of an unknown
    /// kind are skipped.
    pub fn feed(
        &mut self,
        bytes: &[u8],
        mut on_frame: impl FnMut(Frame<'_>),
    ) -> Result<(), FrameError> {
        if self.buf.is_empty() {
            let used = parse(bytes, &mut on_frame)?;
            self.buf.extend_from_slice(&bytes[used..]);
        } else {
            self.buf.extend_from_slice(bytes);
            let used = parse(&self.buf, &mut on_frame)?;
            self.buf.drain(..used);
        }
        Ok(())
    }
}

/// Parses the complete frames at the start of `data` and returns how many bytes they take.
fn parse(data: &[u8], on_frame: &mut impl FnMut(Frame<'_>)) -> Result<usize, FrameError> {
    let mut at = 0;
    while data.len() - at >= HEADER {
        let kind = data[at];
        let len =
            u32::from_le_bytes([data[at + 1], data[at + 2], data[at + 3], data[at + 4]]) as usize;
        if len > MAX_FRAME {
            return Err(FrameError::TooLong(len));
        }
        if kind == RESIZE && len != SIZE_BYTES {
            return Err(FrameError::BadResize(len));
        }
        let end = at + HEADER + len;
        if data.len() < end {
            break;
        }
        let payload = &data[at + HEADER..end];
        match kind {
            DATA => on_frame(Frame::Data(payload)),
            RESIZE => {
                let value = |i: usize| u16::from_le_bytes([payload[i], payload[i + 1]]);
                on_frame(Frame::Resize(Size {
                    columns: value(0),
                    lines: value(2),
                    cell_width: value(4),
                    cell_height: value(6),
                }));
            }
            _ => {}
        }
        at = end;
    }
    Ok(at)
}

/// Removes the first [`READY`] from the helper's output. Everything else passes unchanged,
/// including a second READY.
#[derive(Debug, Default)]
pub struct ReadyFilter {
    ready: bool,
    /// A partial match at the end of the previous chunk.
    held: Vec<u8>,
}

impl ReadyFilter {
    /// Whether the marker has been seen.
    pub fn is_ready(&self) -> bool {
        self.ready
    }

    /// Feeds a chunk of output; `out` receives the bytes to forward.
    pub fn feed(&mut self, bytes: &[u8], mut out: impl FnMut(&[u8])) {
        if self.ready {
            if !bytes.is_empty() {
                out(bytes);
            }
        } else if self.held.is_empty() {
            self.scan(bytes, &mut out);
        } else {
            let mut data = std::mem::take(&mut self.held);
            data.extend_from_slice(bytes);
            self.scan(&data, &mut out);
        }
    }

    /// Flushes held bytes at the end of the stream.
    pub fn finish(&mut self, mut out: impl FnMut(&[u8])) {
        if !self.held.is_empty() {
            out(&self.held);
            self.held.clear();
        }
    }

    /// Looks for the marker in `data`, with nothing held.
    fn scan(&mut self, data: &[u8], out: &mut impl FnMut(&[u8])) {
        if let Some(at) = data.windows(READY.len()).position(|w| w == READY) {
            self.ready = true;
            if at > 0 {
                out(&data[..at]);
            }
            let rest = &data[at + READY.len()..];
            if !rest.is_empty() {
                out(rest);
            }
            return;
        }
        // The longest tail that starts the marker: it may complete in the next chunk.
        let max = (READY.len() - 1).min(data.len());
        let keep = (1..=max)
            .rev()
            .find(|&k| data.ends_with(&READY[..k]))
            .unwrap_or(0);
        let send = data.len() - keep;
        if send > 0 {
            out(&data[..send]);
        }
        self.held.extend_from_slice(&data[send..]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, PartialEq, Eq)]
    enum Owned {
        Data(Vec<u8>),
        Resize(Size),
    }

    fn collect(
        decoder: &mut Decoder,
        bytes: &[u8],
        into: &mut Vec<Owned>,
    ) -> Result<(), FrameError> {
        decoder.feed(bytes, |frame| {
            into.push(match frame {
                Frame::Data(d) => Owned::Data(d.to_vec()),
                Frame::Resize(s) => Owned::Resize(s),
            })
        })
    }

    const SIZE: Size = Size {
        columns: 120,
        lines: 40,
        cell_width: 9,
        cell_height: 18,
    };

    #[test]
    fn decoder_handles_frames_split_byte_by_byte() {
        let mut wire = Vec::new();
        encode_resize(SIZE, &mut wire);
        encode_data(b"ls\r", &mut wire);
        encode_data(b"", &mut wire);
        encode_data(b"x", &mut wire);

        let mut decoder = Decoder::default();
        let mut frames = Vec::new();
        for byte in &wire {
            collect(&mut decoder, std::slice::from_ref(byte), &mut frames).unwrap();
        }
        assert_eq!(
            frames,
            [
                Owned::Resize(SIZE),
                Owned::Data(b"ls\r".to_vec()),
                Owned::Data(b"x".to_vec()),
            ]
        );
    }

    #[test]
    fn decoder_handles_many_frames_in_one_chunk() {
        let mut wire = Vec::new();
        encode_data(b"a", &mut wire);
        encode_resize(SIZE, &mut wire);
        encode_data(b"b", &mut wire);
        // Ends in the middle of a frame.
        wire.extend_from_slice(&[DATA, 3, 0, 0, 0, b'c']);

        let mut decoder = Decoder::default();
        let mut frames = Vec::new();
        collect(&mut decoder, &wire, &mut frames).unwrap();
        assert_eq!(frames.len(), 3);
        collect(&mut decoder, b"de", &mut frames).unwrap();
        assert_eq!(frames.last(), Some(&Owned::Data(b"cde".to_vec())));
    }

    #[test]
    fn decoder_skips_unknown_kinds() {
        let mut wire = vec![9, 2, 0, 0, 0, 0xaa, 0xbb];
        encode_data(b"ok", &mut wire);
        let mut decoder = Decoder::default();
        let mut frames = Vec::new();
        collect(&mut decoder, &wire, &mut frames).unwrap();
        assert_eq!(frames, [Owned::Data(b"ok".to_vec())]);
    }

    #[test]
    fn encode_data_splits_large_payloads() {
        let payload = vec![7u8; MAX_FRAME * 2 + 10];
        let mut wire = Vec::new();
        encode_data(&payload, &mut wire);
        assert_eq!(wire.len(), payload.len() + 3 * HEADER);

        let mut decoder = Decoder::default();
        let mut sizes = Vec::new();
        let mut total = 0;
        decoder
            .feed(&wire, |frame| match frame {
                Frame::Data(d) => {
                    sizes.push(d.len());
                    total += d.len();
                }
                Frame::Resize(_) => panic!("unexpected resize"),
            })
            .unwrap();
        assert_eq!(sizes, [MAX_FRAME, MAX_FRAME, 10]);
        assert_eq!(total, payload.len());
    }

    #[test]
    fn decoder_rejects_too_long_frames() {
        let len = (MAX_FRAME as u32 + 1).to_le_bytes();
        let wire = [DATA, len[0], len[1], len[2], len[3]];
        let mut decoder = Decoder::default();
        let error = decoder.feed(&wire, |_| {}).unwrap_err();
        assert_eq!(error, FrameError::TooLong(MAX_FRAME + 1));
        assert!(error.to_string().contains("exceeds"));
    }

    #[test]
    fn decoder_rejects_bad_resize() {
        let wire = [RESIZE, 2, 0, 0, 0, 1, 2];
        let mut decoder = Decoder::default();
        assert_eq!(
            decoder.feed(&wire, |_| {}).unwrap_err(),
            FrameError::BadResize(2)
        );
    }

    fn filter(chunks: &[&[u8]]) -> (Vec<u8>, bool) {
        let mut filter = ReadyFilter::default();
        let mut out = Vec::new();
        for chunk in chunks {
            filter.feed(chunk, |b| out.extend_from_slice(b));
        }
        filter.finish(|b| out.extend_from_slice(b));
        (out, filter.is_ready())
    }

    #[test]
    fn ready_filter_strips_marker_split_across_chunks() {
        let (out, ready) = filter(&[b"ab\x1b]nuntio", b"-wsl;rea", b"dy\x07cd"]);
        assert_eq!(out, b"abcd");
        assert!(ready);
    }

    #[test]
    fn ready_filter_strips_marker_split_byte_by_byte() {
        let mut wire = b"before".to_vec();
        wire.extend_from_slice(READY);
        wire.extend_from_slice(b"after");
        let chunks: Vec<&[u8]> = wire.chunks(1).collect();
        let (out, ready) = filter(&chunks);
        assert_eq!(out, b"beforeafter");
        assert!(ready);
    }

    #[test]
    fn ready_filter_forwards_text_before_marker_unchanged() {
        let mut first = b"hello\r\n".to_vec();
        first.extend_from_slice(READY);
        let (out, ready) = filter(&[&first]);
        assert_eq!(out, b"hello\r\n");
        assert!(ready);
    }

    #[test]
    fn ready_filter_passes_second_marker_through() {
        let mut wire = READY.to_vec();
        wire.extend_from_slice(b"x");
        wire.extend_from_slice(READY);
        let (out, ready) = filter(&[&wire]);
        let mut expected = b"x".to_vec();
        expected.extend_from_slice(READY);
        assert_eq!(out, expected);
        assert!(ready);
    }

    #[test]
    fn ready_filter_flushes_partial_match_at_end() {
        let (out, ready) = filter(&[b"hi\x1b]nunt"]);
        assert_eq!(out, b"hi\x1b]nunt");
        assert!(!ready);
    }

    #[test]
    fn ready_filter_releases_false_start() {
        let (out, ready) = filter(&[b"\x1b]", b"0;title\x07"]);
        assert_eq!(out, b"\x1b]0;title\x07");
        assert!(!ready);
    }
}
