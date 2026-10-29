//! The kitty clipboard protocol (OSC 5522) as far as paste events (mode
//! 5522) need it: a paste announces its MIME type with a one-time password,
//! and the program reads the data with that password. Other reads and all
//! writes are refused: programs may not read the clipboard on their own.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;

use crate::sync::lock;

/// Where a paste reads from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PasteSource {
    Clipboard,
    /// The primary selection, which only Linux has.
    Primary,
}

pub(crate) enum PasteData {
    Text(String),
    Png(Vec<u8>),
}

/// A paste the program may read once with its password.
pub(crate) struct PasteOffer {
    /// Base64 of 32 lowercase hex digits (16 random bytes): the `pw` value
    /// as sent and as the program sends it back.
    password: String,
    source: PasteSource,
    data: PasteData,
    expires: Instant,
}

/// How long a paste's password stays valid.
const OFFER_LIFETIME: Duration = Duration::from_secs(30);
/// Data bytes per DATA packet, before base64 (the protocol's maximum).
const CHUNK: usize = 4096;
/// Longest request body kept (metadata + payload); longer ones are dropped.
pub(crate) const MAX_REQUEST: usize = 8192;

impl PasteOffer {
    pub(crate) fn new(
        data: PasteData,
        source: PasteSource,
        now: Instant,
    ) -> Result<Self, getrandom::Error> {
        let mut random = [0u8; 16];
        getrandom::fill(&mut random)?;
        let hex: String = random.iter().map(|b| format!("{b:02x}")).collect();
        Ok(Self {
            password: STANDARD.encode(hex),
            source,
            data,
            expires: now + OFFER_LIFETIME,
        })
    }

    fn mime(&self) -> &'static str {
        match self.data {
            PasteData::Text(_) => "text/plain",
            PasteData::Png(_) => "image/png",
        }
    }

    fn bytes(&self) -> &[u8] {
        match &self.data {
            PasteData::Text(text) => text.as_bytes(),
            PasteData::Png(png) => png,
        }
    }

    /// What a paste sends instead of the data: the password and the MIME
    /// type on offer.
    pub(crate) fn announcement(&self) -> Vec<u8> {
        let mut out = Vec::new();
        let loc = match self.source {
            PasteSource::Clipboard => "",
            PasteSource::Primary => ":loc=primary",
        };
        packet(
            &mut out,
            &format!("type=read:status=OK{loc}:pw={}", self.password),
            None,
        );
        list(&mut out, self.mime(), "");
        out
    }
}

/// Mode 5522 as the program set it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PasteEvents {
    Off,
    /// Turned on while `group` was the terminal's foreground process group (`None`: unknown).
    On {
        group: Option<u32>,
    },
}

impl PasteEvents {
    /// Whether a paste goes out as paste events while `foreground` is the terminal's foreground
    /// process group. They belong to the group that turned them on: one that ended without
    /// turning them off (a crash) leaves them to nobody, and a suspended one gets them back.
    /// Unknown groups don't count against them.
    pub(crate) fn applies_to(self, foreground: Option<u32>) -> bool {
        match self {
            Self::Off => false,
            Self::On { group: Some(owner) } => foreground.is_none_or(|group| group == owner),
            Self::On { group: None } => true,
        }
    }
}

/// The answer to `CSI ? 5522 $ p` (DECRQM).
pub(crate) fn mode_report(supported: bool, on: bool) -> String {
    let ps = match (supported, on) {
        (false, _) => 0,
        (true, true) => 1,
        (true, false) => 2,
    };
    format!("\x1b[?5522;{ps}$y")
}

/// The answer to the request `body` (between `ESC ] 5522 ;` and the
/// terminator), or `None` if it is ignored. A read of the offer's MIME type
/// with its password takes the offer.
pub(crate) fn respond(
    body: &[u8],
    slot: &Mutex<Option<PasteOffer>>,
    now: Instant,
) -> Option<Vec<u8>> {
    let (metadata, payload) = match body.iter().position(|&b| b == b';') {
        Some(at) => (&body[..at], &body[at + 1..]),
        None => (body, &[][..]),
    };
    let metadata = std::str::from_utf8(metadata).ok()?;
    let mut kind = None;
    let mut id = String::new();
    let mut loc = None;
    let mut password = None;
    let mut name = None;
    for (key, value) in metadata.split(':').filter_map(|e| e.split_once('=')) {
        match key {
            "type" => kind = Some(value),
            "id" => {
                id = value
                    .chars()
                    .filter(|c| c.is_ascii_alphanumeric() || "-_+.".contains(*c))
                    .collect();
            }
            "loc" => loc = Some(value),
            "pw" => password = Some(value),
            "name" => name = Some(value),
            _ => {}
        }
    }
    let id = if id.is_empty() {
        String::new()
    } else {
        format!(":id={id}")
    };
    let mut out = Vec::new();
    match kind? {
        "write" => {
            packet(&mut out, &format!("type=write:status=EPERM{id}"), None);
            return Some(out);
        }
        "read" => {}
        _ => return None,
    }
    let source = match loc {
        None => PasteSource::Clipboard,
        Some("primary") => PasteSource::Primary,
        Some(_) => return None,
    };
    let payload = STANDARD.decode(payload).ok()?;
    let requested = std::str::from_utf8(&payload).ok()?;
    let mut offer = lock(slot);
    if offer.as_ref().is_some_and(|o| now >= o.expires) {
        *offer = None;
    }
    // A password without a name counts as no password.
    let authorized = match (password, name, offer.as_ref()) {
        (Some(pw), Some(name), Some(o)) => {
            !name.is_empty() && pw == o.password && source == o.source
        }
        _ => false,
    };
    let Some(current) = offer.as_ref().filter(|_| authorized) else {
        packet(&mut out, &format!("type=read:status=EPERM{id}"), None);
        return Some(out);
    };
    let mut wanted = requested.split_ascii_whitespace();
    if wanted.clone().any(|m| m == ".") {
        packet(&mut out, &format!("type=read:status=OK{id}"), None);
        list(&mut out, current.mime(), &id);
    } else if wanted.any(|m| m == current.mime()) {
        let taken = offer.take().expect("checked above");
        drop(offer);
        packet(&mut out, &format!("type=read:status=OK{id}"), None);
        let metadata = format!(
            "type=read:status=DATA:mime={}{id}",
            STANDARD.encode(taken.mime())
        );
        for chunk in taken.bytes().chunks(CHUNK) {
            packet(&mut out, &metadata, Some(&STANDARD.encode(chunk)));
        }
        packet(&mut out, &format!("type=read:status=DONE{id}"), None);
    } else {
        packet(&mut out, &format!("type=read:status=OK{id}"), None);
        packet(&mut out, &format!("type=read:status=DONE{id}"), None);
    }
    Some(out)
}

/// The MIME list (`mime=.`) with `mime` as its only entry, and DONE.
fn list(out: &mut Vec<u8>, mime: &str, id: &str) {
    packet(
        out,
        &format!("type=read:status=DATA:mime=Lg=={id}"),
        Some(&STANDARD.encode(mime)),
    );
    packet(out, &format!("type=read:status=DONE{id}"), None);
}

/// `ESC ] 5522 ; metadata [; payload] ESC \`, terminated with ST as kitty
/// does.
fn packet(out: &mut Vec<u8>, metadata: &str, payload: Option<&str>) {
    out.extend_from_slice(b"\x1b]5522;");
    out.extend_from_slice(metadata.as_bytes());
    if let Some(payload) = payload {
        out.push(b';');
        out.extend_from_slice(payload.as_bytes());
    }
    out.extend_from_slice(b"\x1b\\");
}

#[cfg(test)]
mod tests {
    use super::*;

    const PW: &str = "cHc=";
    const NAME: &str = "name=UGFzdGUgZXZlbnQ=";

    fn slot(data: PasteData, source: PasteSource, now: Instant) -> Mutex<Option<PasteOffer>> {
        Mutex::new(Some(PasteOffer {
            password: PW.into(),
            source,
            data,
            expires: now + OFFER_LIFETIME,
        }))
    }

    fn text_slot(now: Instant) -> Mutex<Option<PasteOffer>> {
        slot(PasteData::Text("hi".into()), PasteSource::Clipboard, now)
    }

    fn read(metadata: &str, mime: &str) -> Vec<u8> {
        format!("type=read:{metadata};{}", STANDARD.encode(mime)).into_bytes()
    }

    fn packets(out: &[u8]) -> Vec<String> {
        let text = std::str::from_utf8(out).unwrap();
        text.strip_prefix("\x1b]5522;")
            .unwrap()
            .strip_suffix("\x1b\\")
            .unwrap()
            .split("\x1b\\\x1b]5522;")
            .map(str::to_owned)
            .collect()
    }

    const EPERM: &[u8] = b"\x1b]5522;type=read:status=EPERM\x1b\\";

    #[test]
    fn text_read_is_single_use() {
        let now = Instant::now();
        let slot = text_slot(now);
        let request = read(&format!("pw={PW}:{NAME}"), "text/plain");
        let expected: &[u8] = b"\x1b]5522;type=read:status=OK\x1b\\\
            \x1b]5522;type=read:status=DATA:mime=dGV4dC9wbGFpbg==;aGk=\x1b\\\
            \x1b]5522;type=read:status=DONE\x1b\\";
        assert_eq!(respond(&request, &slot, now).unwrap(), expected);
        assert_eq!(respond(&request, &slot, now).unwrap(), EPERM);
    }

    #[test]
    fn png_is_sent_in_chunks() {
        let now = Instant::now();
        let png: Vec<u8> = (0..10_000u32).map(|i| (i % 251) as u8).collect();
        let slot = slot(PasteData::Png(png.clone()), PasteSource::Clipboard, now);
        let out = respond(&read(&format!("pw={PW}:{NAME}"), "image/png"), &slot, now).unwrap();
        let packets = packets(&out);
        assert_eq!(packets.first().unwrap(), "type=read:status=OK");
        assert_eq!(packets.last().unwrap(), "type=read:status=DONE");
        let chunks: Vec<Vec<u8>> = packets[1..packets.len() - 1]
            .iter()
            .map(|p| {
                let data = p
                    .strip_prefix("type=read:status=DATA:mime=aW1hZ2UvcG5n;")
                    .unwrap();
                STANDARD.decode(data).unwrap()
            })
            .collect();
        assert_eq!(
            chunks.iter().map(Vec::len).collect::<Vec<_>>(),
            [4096, 4096, 1808]
        );
        assert_eq!(chunks.concat(), png);
    }

    #[test]
    fn unauthorized_reads_are_refused() {
        let now = Instant::now();
        let text = "text/plain";
        for request in [
            read(&format!("pw=eHg=:{NAME}"), text),
            read(&format!("pw={PW}"), text),
            read(&format!("pw={PW}:name="), text),
            read(&format!("loc=primary:pw={PW}:{NAME}"), text),
            read("", "."),
        ] {
            let slot = text_slot(now);
            assert_eq!(respond(&request, &slot, now).unwrap(), EPERM);
            assert!(lock(&slot).is_some());
        }
        let slot = text_slot(now);
        let request = read(&format!("pw={PW}:{NAME}"), text);
        assert_eq!(
            respond(&request, &slot, now + OFFER_LIFETIME).unwrap(),
            EPERM
        );
        assert!(lock(&slot).is_none());
    }

    #[test]
    fn list_keeps_the_offer() {
        let now = Instant::now();
        let slot = slot(PasteData::Text("hi".into()), PasteSource::Primary, now);
        let out = respond(
            &read(&format!("loc=primary:pw={PW}:{NAME}"), "."),
            &slot,
            now,
        )
        .unwrap();
        assert_eq!(
            packets(&out),
            [
                "type=read:status=OK",
                "type=read:status=DATA:mime=Lg==;dGV4dC9wbGFpbg==",
                "type=read:status=DONE"
            ]
        );
        assert!(lock(&slot).is_some());
        let out = respond(
            &read(&format!("loc=primary:pw={PW}:{NAME}"), "image/png"),
            &slot,
            now,
        )
        .unwrap();
        assert_eq!(
            packets(&out),
            ["type=read:status=OK", "type=read:status=DONE"]
        );
        assert!(lock(&slot).is_some());
    }

    #[test]
    fn id_is_sanitized_and_echoed() {
        let now = Instant::now();
        let slot = text_slot(now);
        let out = respond(
            &read(&format!("id=a/b_1:pw={PW}:{NAME}"), "text/plain"),
            &slot,
            now,
        )
        .unwrap();
        let packets = packets(&out);
        assert_eq!(packets.len(), 3);
        assert!(
            packets.iter().all(|p| p.contains(":id=ab_1")),
            "{packets:?}"
        );
    }

    #[test]
    fn invalid_base64_is_ignored() {
        let now = Instant::now();
        let slot = text_slot(now);
        for payload in ["dGV4dC9wbGFpbg", "dGV4dC9w\nbGFpbg=="] {
            let request = format!("type=read:pw={PW}:{NAME};{payload}");
            assert_eq!(respond(request.as_bytes(), &slot, now), None);
        }
    }

    #[test]
    fn writes_are_refused() {
        let now = Instant::now();
        let slot = text_slot(now);
        assert_eq!(
            respond(b"type=write", &slot, now).unwrap(),
            b"\x1b]5522;type=write:status=EPERM\x1b\\"
        );
        assert_eq!(respond(b"type=wdata;aGk=", &slot, now), None);
    }

    #[test]
    fn primary_announcement() {
        let offer = PasteOffer::new(
            PasteData::Text("hi".into()),
            PasteSource::Primary,
            Instant::now(),
        )
        .unwrap();
        let announcement = offer.announcement();
        let text = std::str::from_utf8(&announcement).unwrap();
        assert!(text.contains(":loc=primary:pw="), "{text:?}");
        assert_eq!(announcement.len(), 76 + 12 + 57 + 30);
    }

    #[test]
    fn paste_events_apply_only_to_their_group() {
        assert!(!PasteEvents::Off.applies_to(Some(5)));
        assert!(!PasteEvents::Off.applies_to(None));
        assert!(PasteEvents::On { group: None }.applies_to(Some(5)));
        let owned = PasteEvents::On { group: Some(5) };
        assert!(owned.applies_to(Some(5)));
        assert!(owned.applies_to(None));
        assert!(!owned.applies_to(Some(6)));
    }
}
