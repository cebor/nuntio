//! The wire format between nuntio (Windows) and the helper (Linux, inside WSL).
//!
//! nuntio -> helper, on the helper's stdin, is a sequence of frames:
//! `[kind: u8][len: u32 LE][payload]`. [`DATA`] frames carry bytes for the shell's input,
//! [`RESIZE`] frames carry a [`Size`].
//!
//! helper -> nuntio, on the helper's stdout, is the raw output of the PTY, preceded by a
//! single [`READY`] marker once the shell is running. Reports about the PTY ([`Marker`]) are
//! mixed into the output as OSC sequences, which the [`MarkerFilter`] removes again. stderr
//! carries the helper's own messages.

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

/// Starts every in-band report of the helper.
const MARKER_PREFIX: &[u8] = b"\x1b]nuntio-wsl;";
/// Ends a report.
const BEL: u8 = 0x07;
/// The longest body of a report: `cwd;` and a path of PATH_MAX bytes. A longer one is not ours
/// and passes through unchanged.
const MAX_BODY: usize = "cwd;".len() + 4096;
/// The longest process name of a foreground report.
const MAX_NAME: usize = 64;

/// A report of the helper, found by the [`MarkerFilter`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Marker {
    /// The shell has started; the same as [`READY`].
    Ready,
    /// The foreground process group of the PTY changed.
    Foreground {
        /// The `comm` name of the process group leader.
        name: String,
        /// Whether it is the shell itself.
        shell: bool,
        /// The process group, i.e. its leader's pid.
        pgid: u32,
    },
    /// The working directory of the foreground process group changed: a Linux path, `None` if
    /// the helper can't tell (unreadable, not UTF-8, a control character, too long).
    Directory(Option<String>),
}

/// The report [`Marker::Foreground`] as bytes for the helper's stdout: `fg;<0|1>;<pgid>;<name>`,
/// with the shell flag before the pgid so that the name may contain `;`. Escape and bell
/// characters are removed from `name`, and it is cut to 64 bytes.
pub fn foreground_marker(name: &str, shell: bool, pgid: u32) -> Vec<u8> {
    let mut out = MARKER_PREFIX.to_vec();
    out.extend_from_slice(b"fg;");
    out.push(if shell { b'1' } else { b'0' });
    out.push(b';');
    out.extend_from_slice(pgid.to_string().as_bytes());
    out.push(b';');
    let mut len = 0;
    for c in name.chars().filter(|&c| c != '\x07' && c != '\x1b') {
        len += c.len_utf8();
        if len > MAX_NAME {
            break;
        }
        let mut utf8 = [0; 4];
        out.extend_from_slice(c.encode_utf8(&mut utf8).as_bytes());
    }
    out.push(BEL);
    out
}

/// The report [`Marker::Directory`] as bytes for the helper's stdout. A `path` that is not
/// UTF-8, holds a control character or doesn't fit a report is sent as unknown: `cwd;` with
/// nothing after it.
pub fn directory_marker(path: &[u8]) -> Vec<u8> {
    let usable = "cwd;".len() + path.len() <= MAX_BODY
        && std::str::from_utf8(path).is_ok_and(|p| !p.chars().any(char::is_control));
    let mut out = MARKER_PREFIX.to_vec();
    out.extend_from_slice(b"cwd;");
    if usable {
        out.extend_from_slice(path);
    }
    out.push(BEL);
    out
}

/// Removes the helper's reports from its output: every OSC that starts with
/// `ESC ] nuntio-wsl ;` and ends with BEL. Everything else passes unchanged, including such an
/// OSC with a body longer than `MAX_BODY`.
#[derive(Debug, Default)]
pub struct MarkerFilter {
    ready: bool,
    /// A partial prefix at the end of the previous chunk.
    held: Vec<u8>,
    /// The body read so far of a report that has not ended yet.
    body: Option<Vec<u8>>,
}

impl MarkerFilter {
    /// Whether the [`Marker::Ready`] has been seen.
    pub fn is_ready(&self) -> bool {
        self.ready
    }

    /// Feeds a chunk of output; `out` receives the bytes to forward and `marker` the reports,
    /// in the order they appeared.
    pub fn feed(
        &mut self,
        bytes: &[u8],
        mut out: impl FnMut(&[u8]),
        mut marker: impl FnMut(Marker),
    ) {
        if self.held.is_empty() {
            self.scan(bytes, &mut out, &mut marker);
        } else {
            let mut data = std::mem::take(&mut self.held);
            data.extend_from_slice(bytes);
            self.scan(&data, &mut out, &mut marker);
        }
    }

    /// Flushes held bytes at the end of the stream.
    pub fn finish(&mut self, mut out: impl FnMut(&[u8])) {
        if !self.held.is_empty() {
            out(&self.held);
            self.held.clear();
        }
        if let Some(body) = self.body.take() {
            out(MARKER_PREFIX);
            out(&body);
        }
    }

    fn scan(
        &mut self,
        mut data: &[u8],
        out: &mut impl FnMut(&[u8]),
        marker: &mut impl FnMut(Marker),
    ) {
        while !data.is_empty() {
            let Some(body) = self.body.as_mut() else {
                match data
                    .windows(MARKER_PREFIX.len())
                    .position(|w| w == MARKER_PREFIX)
                {
                    Some(at) => {
                        if at > 0 {
                            out(&data[..at]);
                        }
                        self.body = Some(Vec::new());
                        data = &data[at + MARKER_PREFIX.len()..];
                    }
                    None => {
                        // The longest tail that starts the prefix: it may complete in the
                        // next chunk.
                        let max = (MARKER_PREFIX.len() - 1).min(data.len());
                        let keep = (1..=max)
                            .rev()
                            .find(|&k| data.ends_with(&MARKER_PREFIX[..k]))
                            .unwrap_or(0);
                        let send = data.len() - keep;
                        if send > 0 {
                            out(&data[..send]);
                        }
                        self.held.extend_from_slice(&data[send..]);
                        return;
                    }
                }
                continue;
            };
            let end = data.iter().position(|&b| b == BEL);
            let fits = body.len() + end.unwrap_or(data.len()) <= MAX_BODY;
            match (end, fits) {
                (Some(end), true) => {
                    body.extend_from_slice(&data[..end]);
                    data = &data[end + 1..];
                    let body = self.body.take().unwrap_or_default();
                    if let Some(found) = self.interpret(&body) {
                        marker(found);
                    }
                }
                (None, true) => {
                    body.extend_from_slice(data);
                    return;
                }
                // Too long to be ours: give it back and carry on with the text.
                (_, false) => {
                    out(MARKER_PREFIX);
                    out(body);
                    self.body = None;
                }
            }
        }
    }

    /// The report in `body`, if it is one this filter knows.
    fn interpret(&mut self, body: &[u8]) -> Option<Marker> {
        let body = String::from_utf8_lossy(body);
        if body == "ready" {
            self.ready = true;
            return Some(Marker::Ready);
        }
        if let Some(dir) = body.strip_prefix("cwd;") {
            return Some(Marker::Directory((!dir.is_empty()).then(|| dir.to_owned())));
        }
        let rest = body.strip_prefix("fg;")?;
        let mut fields = rest.splitn(3, ';');
        let shell = match fields.next()? {
            "0" => false,
            "1" => true,
            _ => return None,
        };
        let pgid = fields
            .next()?
            .parse::<u32>()
            .ok()
            .filter(|&pgid| pgid > 0)?;
        let name = fields.next()?;
        Some(Marker::Foreground {
            name: name.to_owned(),
            shell,
            pgid,
        })
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

    /// Output, reports and the ready flag after feeding `chunks`.
    fn filter(chunks: &[&[u8]]) -> (Vec<u8>, Vec<Marker>, bool) {
        let mut filter = MarkerFilter::default();
        let mut out = Vec::new();
        let mut markers = Vec::new();
        for chunk in chunks {
            filter.feed(chunk, |b| out.extend_from_slice(b), |m| markers.push(m));
        }
        filter.finish(|b| out.extend_from_slice(b));
        (out, markers, filter.is_ready())
    }

    fn fg(name: &str, shell: bool, pgid: u32) -> Marker {
        Marker::Foreground {
            name: name.to_owned(),
            shell,
            pgid,
        }
    }

    #[test]
    fn marker_filter_strips_ready_split_across_chunks() {
        let (out, markers, ready) = filter(&[b"ab\x1b]nuntio", b"-wsl;rea", b"dy\x07cd"]);
        assert_eq!(out, b"abcd");
        assert_eq!(markers, [Marker::Ready]);
        assert!(ready);
    }

    #[test]
    fn marker_filter_strips_ready_split_byte_by_byte() {
        let mut wire = b"before".to_vec();
        wire.extend_from_slice(READY);
        wire.extend_from_slice(b"after");
        let chunks: Vec<&[u8]> = wire.chunks(1).collect();
        let (out, markers, ready) = filter(&chunks);
        assert_eq!(out, b"beforeafter");
        assert_eq!(markers, [Marker::Ready]);
        assert!(ready);
    }

    #[test]
    fn marker_filter_forwards_text_before_ready_unchanged() {
        let mut first = b"hello\r\n".to_vec();
        first.extend_from_slice(READY);
        let (out, _, ready) = filter(&[&first]);
        assert_eq!(out, b"hello\r\n");
        assert!(ready);
    }

    #[test]
    fn marker_filter_strips_every_marker() {
        let mut wire = READY.to_vec();
        wire.extend_from_slice(b"x");
        wire.extend_from_slice(READY);
        let (out, markers, ready) = filter(&[&wire]);
        assert_eq!(out, b"x");
        assert_eq!(markers, [Marker::Ready, Marker::Ready]);
        assert!(ready);
    }

    #[test]
    fn marker_filter_flushes_partial_match_at_end() {
        let (out, markers, ready) = filter(&[b"hi\x1b]nunt"]);
        assert_eq!(out, b"hi\x1b]nunt");
        assert!(markers.is_empty());
        assert!(!ready);
    }

    #[test]
    fn marker_filter_flushes_unfinished_report_at_end() {
        let (out, markers, _) = filter(&[b"hi\x1b]nuntio-wsl;fg;1;zs"]);
        assert_eq!(out, b"hi\x1b]nuntio-wsl;fg;1;zs");
        assert!(markers.is_empty());
    }

    #[test]
    fn marker_filter_releases_false_start() {
        let (out, markers, ready) = filter(&[b"\x1b]", b"0;title\x07"]);
        assert_eq!(out, b"\x1b]0;title\x07");
        assert!(markers.is_empty());
        assert!(!ready);
    }

    #[test]
    fn marker_filter_splits_foreground_reports_at_every_position() {
        let mut wire = b"ab".to_vec();
        wire.extend_from_slice(&foreground_marker("zsh", true, 7));
        wire.extend_from_slice(b"cd");
        wire.extend_from_slice(&foreground_marker("tmux: client", false, 42));
        wire.extend_from_slice(b"ef");
        let expected = [fg("zsh", true, 7), fg("tmux: client", false, 42)];

        for at in 0..=wire.len() {
            let (out, markers, ready) = filter(&[&wire[..at], &wire[at..]]);
            assert_eq!(out, b"abcdef", "split at {at}");
            assert_eq!(markers, expected, "split at {at}");
            assert!(!ready);
        }
        let chunks: Vec<&[u8]> = wire.chunks(1).collect();
        let (out, markers, _) = filter(&chunks);
        assert_eq!(out, b"abcdef");
        assert_eq!(markers, expected);
    }

    #[test]
    fn marker_filter_passes_long_bodies_through() {
        let mut wire = b"a".to_vec();
        wire.extend_from_slice(MARKER_PREFIX);
        wire.extend_from_slice(&[b'x'; MAX_BODY + 1]);
        wire.push(BEL);
        wire.extend_from_slice(b"b");
        for at in 0..=wire.len() {
            let (out, markers, _) = filter(&[&wire[..at], &wire[at..]]);
            assert_eq!(out, wire, "split at {at}");
            assert!(markers.is_empty(), "split at {at}");
        }

        // The longest body still is a report, an unknown one here.
        let mut wire = MARKER_PREFIX.to_vec();
        wire.extend_from_slice(&[b'x'; MAX_BODY]);
        wire.push(BEL);
        let (out, markers, _) = filter(&[&wire]);
        assert!(out.is_empty());
        assert!(markers.is_empty());
    }

    #[test]
    fn marker_filter_ignores_unknown_bodies() {
        let (out, markers, _) = filter(&[
            b"\x1b]nuntio-wsl;fg;2;7;x\x07\x1b]nuntio-wsl;fg;1;0;x\x07\x1b]nuntio-wsl;fg;1;x;y\x07\x1b]nuntio-wsl;what\x07z",
        ]);
        assert_eq!(out, b"z");
        assert!(markers.is_empty());
    }

    #[test]
    fn foreground_marker_cleans_and_cuts_the_name() {
        assert_eq!(
            foreground_marker("a\x07b\x1bc", false, 7),
            b"\x1b]nuntio-wsl;fg;0;7;abc\x07"
        );
        let marker = foreground_marker(&"é".repeat(100), true, 7);
        let body = &marker[MARKER_PREFIX.len()..marker.len() - 1];
        assert_eq!(body.len(), "fg;1;7;".len() + MAX_NAME);
        let (_, markers, _) = filter(&[&marker]);
        assert_eq!(markers, [fg(&"é".repeat(32), true, 7)]);
    }

    #[test]
    fn directory_marker_round_trips() {
        let marker = directory_marker(b"/home/me/a b");
        assert_eq!(marker, b"\x1b]nuntio-wsl;cwd;/home/me/a b\x07");
        let (out, markers, _) = filter(&[&marker]);
        assert!(out.is_empty());
        assert_eq!(markers, [Marker::Directory(Some("/home/me/a b".into()))]);

        let (_, markers, _) = filter(&[&directory_marker(&[b'/'; 4096])]);
        assert_eq!(markers, [Marker::Directory(Some("/".repeat(4096)))]);
    }

    #[test]
    fn directory_marker_sends_unusable_paths_as_unknown() {
        for path in [&b"/x\x07y"[..], &[b'/', 0xff], &[b'a'; 4097]] {
            let (out, markers, _) = filter(&[&directory_marker(path)]);
            assert!(out.is_empty());
            assert_eq!(markers, [Marker::Directory(None)]);
        }
    }
}
