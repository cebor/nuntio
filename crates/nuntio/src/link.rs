//! Links under the pointer: the hint that shows where one leads, and the
//! check before opening it. OSC 8 links can show any text, so the text
//! doesn't tell where a click goes; and a `file://` link to a program
//! would run it on macOS and Windows.

use std::borrow::Cow;
use std::path::{Path, PathBuf};

use nuntio_render::{Rect, UiRect, UiText};
use nuntio_term::Rgb;
use unicode_width::UnicodeWidthStr;

use crate::style::{UiMetrics, framed_box, hairline, truncate};

/// Distance from the pane's bottom corner, in logical pixels.
const MARGIN: f64 = 6.0;
/// Space between the border and the text, in logical pixels.
const PADDING: f64 = 3.0;

/// Extensions of files that the OS runs, installs or mounts when they are
/// opened, on any platform: a link could come from a remote machine.
const PROGRAM_EXTENSIONS: &[&str] = &[
    // Windows
    "appinstaller",
    "application",
    "appref-ms",
    "appx",
    "bat",
    "chm",
    "cmd",
    "com",
    "cpl",
    "diagcab",
    "exe",
    "gadget",
    "hta",
    "inf",
    "jnlp",
    "jse",
    "library-ms",
    "lnk",
    "msc",
    "msi",
    "msix",
    "msp",
    "mst",
    "msu",
    "pif",
    "ps1",
    "ps1xml",
    "psc1",
    "psm1",
    "rdp",
    "reg",
    "scf",
    "scr",
    "sct",
    "searchconnector-ms",
    "settingcontent-ms",
    "theme",
    "themepack",
    "url",
    "vbe",
    "vbs",
    "vsto",
    "website",
    "ws",
    "wsb",
    "wsc",
    "wsf",
    "wsh",
    "xbap",
    "xll",
    // macOS
    "app",
    "applescript",
    "command",
    "dmg",
    "fileloc",
    "inetloc",
    "mpkg",
    "pkg",
    "prefpane",
    "saver",
    "scpt",
    "terminal",
    "tool",
    "webloc",
    "workflow",
    // Linux
    "appimage",
    "desktop",
    // Disk images, which the OS mounts
    "img",
    "iso",
    "vhd",
    "vhdx",
    // Anywhere
    "jar",
    "js",
    "py",
    "pyw",
    "pyz",
    "sh",
];

/// Why a link must not be opened, if it mustn't.
pub fn check(url: &str) -> Result<(), String> {
    let Some(path) = file_path(url, &local_host())? else {
        return Ok(());
    };
    // What the path really names: `Evil.app/Contents/..` or a symlink can
    // hide a program.
    let path = match std::fs::canonicalize(&path) {
        Ok(path) => path,
        // A missing file can't run; the opener reports it.
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(_) => path,
    };
    let Ok(metadata) = std::fs::metadata(&path) else {
        return Ok(());
    };
    if is_program(&path, &metadata) {
        return Err("it could run a program".into());
    }
    Ok(())
}

fn local_host() -> String {
    gethostname::gethostname().to_string_lossy().into_owned()
}

/// The local path of a `file://` URL, `None` for other schemes. Hosts
/// other than this machine are refused: on Windows they are network
/// shares.
fn file_path(url: &str, local_host: &str) -> Result<Option<PathBuf>, String> {
    let scheme = "file://";
    let is_file = url
        .get(..scheme.len())
        .is_some_and(|s| s.eq_ignore_ascii_case(scheme));
    if !is_file {
        return Ok(None);
    }
    // The opener drops a query and fragment, so the check must too:
    // `Calc.app#x` would otherwise pass as a missing file.
    let rest = &url[scheme.len()..];
    let rest = &rest[..rest.find(['?', '#']).unwrap_or(rest.len())];
    let (host, path) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
    let local = host.is_empty()
        || host.eq_ignore_ascii_case("localhost")
        || host.eq_ignore_ascii_case(local_host);
    if !local {
        return Err(format!("it points to another computer ({host})"));
    }
    let path = percent_encoding::percent_decode_str(path)
        .decode_utf8()
        .map_err(|_| "its path is not valid UTF-8".to_owned())?;
    // `file:///C:/Users` is `C:/Users` on Windows.
    let path = match path.strip_prefix('/') {
        Some(rest) if cfg!(windows) && rest.get(1..2) == Some(":") => rest,
        _ => &path,
    };
    // `file:////evil/share` names no host, yet Windows reads `//evil/share`
    // as a network share (and `\\?\…` as a device path).
    if path.starts_with(['/', '\\']) && path[1..].starts_with(['/', '\\']) {
        return Err("it points to another computer".into());
    }
    Ok(Some(PathBuf::from(path)))
}

/// Opening it would start something: a known program type (including
/// macOS app bundles, which are directories) or an executable file.
fn is_program(path: &Path, metadata: &std::fs::Metadata) -> bool {
    let name = path.file_name().map(|n| n.to_string_lossy());
    let extension = name.as_deref().and_then(program_extension).unwrap_or("");
    if PROGRAM_EXTENSIONS
        .iter()
        .any(|p| p.eq_ignore_ascii_case(extension))
    {
        return true;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.is_file() && metadata.permissions().mode() & 0o111 != 0 {
            return true;
        }
    }
    #[cfg(not(unix))]
    let _ = metadata;
    false
}

/// The extension of the file Windows opens for `name`: it drops trailing
/// dots and spaces (`setup.exe.` is `setup.exe`), and `:` starts an
/// alternate data stream (`setup.exe::$DATA`).
fn program_extension(name: &str) -> Option<&str> {
    let name = name.split(':').next().unwrap_or(name);
    let name = name.trim_end_matches(['.', ' ']);
    Path::new(name).extension()?.to_str()
}

/// Area of the hint for `columns` cells of text: the bottom-left corner of
/// the pane, or the bottom-right one while the pointer is in the way.
fn hint_bounds(columns: usize, pane: Rect, pointer: (f32, f32), metrics: UiMetrics) -> Rect {
    let cell = metrics.small;
    let margin = metrics.logical(MARGIN);
    let padding = metrics.logical(PADDING);
    let width = (columns as f32 * cell.width as f32 + 2.0 * padding)
        .min(pane.width - 2.0 * margin)
        .max(0.0);
    let height = cell.height as f32 + 2.0 * padding;
    let left = Rect {
        x: pane.x + margin,
        y: pane.y + pane.height - margin - height,
        width,
        height,
    };
    if left.contains(pointer.0, pointer.1) {
        Rect {
            x: pane.x + pane.width - margin - width,
            ..left
        }
    } else {
        left
    }
}

/// `url` without the userinfo of its authority (`user@`), which can make
/// it look like it leads to another host.
fn without_userinfo(url: &str) -> Cow<'_, str> {
    let Some(scheme_end) = url.find("://").map(|i| i + 3) else {
        return Cow::Borrowed(url);
    };
    let rest = &url[scheme_end..];
    let authority = &rest[..rest.find(['/', '?', '#']).unwrap_or(rest.len())];
    match authority.rfind('@') {
        Some(at) => Cow::Owned(format!("{}{}", &url[..scheme_end], &rest[at + 1..])),
        None => Cow::Borrowed(url),
    }
}

/// Draw where the link `url` leads, in `pane`, in the small UI font.
pub fn draw_hint(
    url: &str,
    pane: Rect,
    pointer: (f32, f32),
    metrics: UiMetrics,
    background: Rgb,
    foreground: Rgb,
) -> (Vec<UiRect>, Vec<UiText>) {
    let padding = metrics.logical(PADDING);
    let margin = metrics.logical(MARGIN);
    let fits =
        ((pane.width - 2.0 * (margin + padding)) / metrics.small.width as f32).max(0.0) as usize;
    // The start names the scheme and host (userinfo dropped, it can fake a
    // host); cut the end.
    let text = truncate(&without_userinfo(url), fits);
    let bounds = hint_bounds(text.width(), pane, pointer, metrics);
    let rects = framed_box(bounds, hairline(metrics.scale), background, foreground);
    let texts = vec![UiText::new(bounds.x + padding, bounds.y + padding, text, foreground).small()];
    (rects, texts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::style::test_metrics::*;

    const PANE: Rect = Rect {
        x: 0.0,
        y: 30.0,
        width: 1000.0,
        height: 600.0,
    };

    #[test]
    fn file_urls_become_local_paths() {
        let path = |url| file_path(url, "box").unwrap();
        assert_eq!(path("https://example.com"), None);
        assert_eq!(path("mailto:me@example.com"), None);
        assert_eq!(path("file:///tmp/a%20b"), Some(PathBuf::from("/tmp/a b")));
        assert_eq!(path("FILE:///tmp"), Some(PathBuf::from("/tmp")));
        assert_eq!(path("file:///tmp/a#x"), Some(PathBuf::from("/tmp/a")));
        assert_eq!(path("file:///tmp/a?q=1#x"), Some(PathBuf::from("/tmp/a")));
        assert_eq!(path("file:///tmp/a%23b"), Some(PathBuf::from("/tmp/a#b")));
        // `ls --hyperlink` names the host.
        assert_eq!(path("file://box/etc"), Some(PathBuf::from("/etc")));
        assert_eq!(path("file://localhost/etc"), Some(PathBuf::from("/etc")));
        if cfg!(windows) {
            assert_eq!(path("file:///C:/x"), Some(PathBuf::from("C:/x")));
        }
    }

    #[test]
    fn other_hosts_are_refused() {
        let err = file_path("file://evil.example/share/x.exe", "box").unwrap_err();
        assert!(err.contains("evil.example"), "{err}");
        assert!(file_path("file:///%FF", "box").is_err());
        // No host, but a network share on Windows.
        assert!(file_path("file:////evil/share/x.txt", "box").is_err());
        assert!(file_path("file:///%5C%5Cevil%5Cshare%5Cx", "box").is_err());
        assert!(file_path("file:///%5C%5C?%5CUNC%5Cevil%5Cx", "box").is_err());
    }

    #[test]
    fn extensions_are_read_as_windows_does() {
        assert_eq!(program_extension("setup.exe"), Some("exe"));
        assert_eq!(program_extension("setup.exe."), Some("exe"));
        assert_eq!(program_extension("setup.exe . ."), Some("exe"));
        assert_eq!(program_extension("setup.exe::$DATA"), Some("exe"));
        assert_eq!(program_extension("notes"), None);
    }

    #[test]
    fn programs_are_not_opened() {
        let dir = std::env::temp_dir().join(format!("nuntio-link-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("Evil.app").join("Contents")).unwrap();
        let (text, exe) = (dir.join("notes.txt"), dir.join("setup.EXE"));
        std::fs::write(&text, "").unwrap();
        std::fs::write(&exe, "").unwrap();
        // `file:///C:/…` on Windows.
        let url = |p: &Path| {
            let path = p.display().to_string().replace('\\', "/");
            format!("file:///{}", path.trim_start_matches('/'))
        };
        let results = [
            check(&url(&dir)),
            check(&url(&text)),
            check(&url(&exe)),
            check(&url(&dir.join("Evil.app"))),
            check(&url(&dir.join("missing.exe"))),
            check(&format!("{}#x", url(&dir.join("Evil.app")))),
            check(&format!("{}?x", url(&exe))),
            check(&url(&dir.join("Evil.app").join("Contents").join(".."))),
        ];
        #[cfg(unix)]
        let symlink = {
            let link = dir.join("notes2.txt");
            std::os::unix::fs::symlink(dir.join("Evil.app"), &link).unwrap();
            check(&url(&link))
        };
        #[cfg(unix)]
        let script = {
            use std::os::unix::fs::PermissionsExt;
            let script = dir.join("run");
            std::fs::write(&script, "").unwrap();
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
            check(&url(&script))
        };
        std::fs::remove_dir_all(&dir).unwrap();

        assert!(results[0].is_ok(), "directory");
        assert!(results[1].is_ok(), "text file");
        assert!(results[2].is_err(), "exe");
        assert!(results[3].is_err(), "app bundle");
        assert!(results[4].is_ok(), "missing file");
        assert!(results[5].is_err(), "app bundle with a fragment");
        assert!(results[6].is_err(), "exe with a query");
        assert!(results[7].is_err(), "app bundle behind `..`");
        #[cfg(unix)]
        assert!(script.is_err(), "executable");
        #[cfg(unix)]
        assert!(symlink.is_err(), "symlink to an app bundle");
        assert!(check("https://example.com/x.exe").is_ok());
    }

    #[test]
    fn hint_hides_userinfo() {
        assert_eq!(
            without_userinfo("https://github.com.x@evil.example/p"),
            "https://evil.example/p"
        );
        assert_eq!(
            without_userinfo("https://a.example/x@y"),
            "https://a.example/x@y"
        );
    }

    #[test]
    fn hint_sits_bottom_left_and_dodges_the_pointer() {
        let bounds = |pointer| hint_bounds(20, PANE, pointer, METRICS);
        let left = bounds((500.0, 100.0));
        assert_eq!((left.x, left.y + left.height), (6.0, 624.0));
        assert_eq!(left.width, 206.0);
        let right = bounds((10.0, 620.0));
        assert_eq!(right.x + right.width, 994.0);
        assert_eq!(right.y, left.y);
    }

    #[test]
    fn long_urls_are_cut_at_the_end() {
        let narrow = Rect {
            width: 120.0,
            ..PANE
        };
        let url = "https://example.com/a/very/long/path";
        let (_, texts) = draw_hint(
            url,
            narrow,
            (0.0, 0.0),
            METRICS,
            Rgb::default(),
            Rgb::default(),
        );
        assert_eq!(texts[0].text, "https://e…");
    }
}
