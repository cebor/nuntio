//! The working directory a shell reports in its output: OSC 7
//! (`file://host/path`) and OSC 9;9 (ConEmu, Windows Terminal). Where nuntio
//! can't read the shell's directory itself (Windows, shells inside WSL),
//! this is the only source.

use std::borrow::Cow;

use percent_encoding::percent_decode_str;

/// Longest OSC payload that is kept; longer ones are no directory.
const MAX_LEN: usize = 4096;

/// A directory the shell reported, in the syntax it used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReportedDir {
    /// `/home/me`: a Unix path, possibly inside WSL.
    Posix(String),
    /// `C:\Users\me` or `\\wsl.localhost\Ubuntu\home\me`.
    Windows(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Ground,
    Esc,
    Osc,
    /// ESC inside an OSC: `\` ends it (ST).
    OscEsc,
}

/// Finds directory reports in PTY output, also when a sequence is split
/// across reads.
#[derive(Debug)]
pub struct CwdScanner {
    state: State,
    payload: Vec<u8>,
    overflow: bool,
}

impl Default for CwdScanner {
    fn default() -> Self {
        Self {
            state: State::Ground,
            payload: Vec::new(),
            overflow: false,
        }
    }
}

impl CwdScanner {
    /// Scan more output. Returns the last directory it reports, if any.
    pub fn feed(&mut self, bytes: &[u8]) -> Option<ReportedDir> {
        let mut found = None;
        for &byte in bytes {
            self.state = match (self.state, byte) {
                (State::Ground, 0x1b) => State::Esc,
                (State::Ground, _) => State::Ground,
                (State::Esc, b']') => {
                    self.payload.clear();
                    self.overflow = false;
                    State::Osc
                }
                (State::Esc, 0x1b) => State::Esc,
                (State::Esc, _) => State::Ground,
                (State::Osc, 0x07) | (State::OscEsc, b'\\') => {
                    if !self.overflow
                        && let Some(dir) = parse(&self.payload)
                    {
                        found = Some(dir);
                    }
                    State::Ground
                }
                (State::Osc, 0x1b) => State::OscEsc,
                // Like xterm: CAN and SUB abort the sequence.
                (State::Osc, 0x18 | 0x1a) => State::Ground,
                (State::Osc, _) => {
                    if self.payload.len() < MAX_LEN {
                        self.payload.push(byte);
                    } else {
                        self.overflow = true;
                    }
                    State::Osc
                }
                // Another escape sequence starts; the OSC is over.
                (State::OscEsc, b']') => {
                    self.payload.clear();
                    self.overflow = false;
                    State::Osc
                }
                (State::OscEsc, 0x1b) => State::Esc,
                (State::OscEsc, _) => State::Ground,
            };
        }
        found
    }
}

fn parse(payload: &[u8]) -> Option<ReportedDir> {
    let payload = std::str::from_utf8(payload).ok()?;
    if let Some(url) = payload.strip_prefix("7;") {
        return parse_file_url(url);
    }
    let path = payload.strip_prefix("9;9;")?;
    let path = path
        .strip_prefix('"')
        .and_then(|p| p.strip_suffix('"'))
        .unwrap_or(path);
    if path.is_empty() {
        None
    } else if path.starts_with('/') {
        Some(ReportedDir::Posix(path.to_owned()))
    } else {
        Some(ReportedDir::Windows(path.to_owned()))
    }
}

/// `file://host/path`, with %-escapes. `file:///C:/x` is a Windows path.
fn parse_file_url(url: &str) -> Option<ReportedDir> {
    let rest = url.strip_prefix("file://")?;
    let path = &rest[rest.find('/')?..];
    let path: Cow<str> = percent_decode_str(path).decode_utf8().ok()?;
    let bytes = path.as_bytes();
    let drive = bytes.len() >= 3
        && bytes[1].is_ascii_alphabetic()
        && bytes[2] == b':'
        && bytes.get(3).is_none_or(|&b| b == b'/');
    if drive {
        let windows = path[1..].replace('/', "\\");
        // `C:` alone is the drive's current directory, not its root.
        let windows = if windows.len() == 2 {
            windows + "\\"
        } else {
            windows
        };
        return Some(ReportedDir::Windows(windows));
    }
    Some(ReportedDir::Posix(path.into_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan(bytes: &[u8]) -> Option<ReportedDir> {
        CwdScanner::default().feed(bytes)
    }

    fn posix(path: &str) -> Option<ReportedDir> {
        Some(ReportedDir::Posix(path.into()))
    }

    fn windows(path: &str) -> Option<ReportedDir> {
        Some(ReportedDir::Windows(path.into()))
    }

    #[test]
    fn osc7_with_bel_or_st() {
        assert_eq!(scan(b"\x1b]7;file://box/home/me\x07"), posix("/home/me"));
        assert_eq!(scan(b"x\x1b]7;file://box/tmp\x1b\\y"), posix("/tmp"));
        assert_eq!(scan(b"\x1b]7;file:///srv\x07"), posix("/srv"), "no host");
    }

    #[test]
    fn osc7_is_percent_decoded() {
        assert_eq!(
            scan(b"\x1b]7;file://box/home/me/My%20Files\x07"),
            posix("/home/me/My Files")
        );
    }

    #[test]
    fn osc7_with_a_drive_is_a_windows_path() {
        assert_eq!(
            scan(b"\x1b]7;file:///C:/Users/me\x07"),
            windows(r"C:\Users\me")
        );
        assert_eq!(scan(b"\x1b]7;file://host/D:\x07"), windows(r"D:\"));
    }

    #[test]
    fn osc9_9() {
        assert_eq!(
            scan(b"\x1b]9;9;\"\\\\wsl.localhost\\Ubuntu\\home\\me\"\x1b\\"),
            windows(r"\\wsl.localhost\Ubuntu\home\me")
        );
        assert_eq!(scan(b"\x1b]9;9;C:\\x\x07"), windows(r"C:\x"));
        assert_eq!(scan(b"\x1b]9;9;/home/me\x07"), posix("/home/me"));
    }

    #[test]
    fn sequences_may_be_split_across_reads() {
        let mut scanner = CwdScanner::default();
        assert_eq!(scanner.feed(b"ls\r\n\x1b"), None);
        assert_eq!(scanner.feed(b"]7;file://box/ho"), None);
        assert_eq!(scanner.feed(b"me\x1b"), None);
        assert_eq!(scanner.feed(b"\\$ "), posix("/home"));
    }

    #[test]
    fn the_last_report_wins() {
        assert_eq!(
            scan(b"\x1b]7;file://h/a\x07\x1b]7;file://h/b\x07"),
            posix("/b")
        );
    }

    #[test]
    fn other_sequences_are_ignored() {
        assert_eq!(scan(b"\x1b]0;me@box: ~\x07"), None, "title");
        assert_eq!(scan(b"\x1b]9;hello\x07"), None, "notification");
        assert_eq!(scan(b"\x1b]7;https://x/y\x07"), None);
        assert_eq!(scan(b"\x1b]7;file://box\x07"), None, "no path");
        assert_eq!(scan(b"\x1b]9;9;\x07"), None);
        assert_eq!(scan(b"\x1b[31m7;file://h/x\x07"), None, "no OSC");
        assert_eq!(scan(b"\x1b]7;file://h/x\x18\x07"), None, "cancelled");
    }

    #[test]
    fn overlong_payloads_are_dropped() {
        let mut bytes = b"\x1b]7;file://h/".to_vec();
        bytes.resize(MAX_LEN + 100, b'a');
        bytes.push(0x07);
        assert_eq!(scan(&bytes), None);
        // And the scanner recovers.
        let mut scanner = CwdScanner::default();
        scanner.feed(&bytes);
        assert_eq!(scanner.feed(b"\x1b]7;file://h/ok\x07"), posix("/ok"));
    }
}
