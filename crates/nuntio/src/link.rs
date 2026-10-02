//! Links under the pointer: the hint that shows where one leads, and the
//! check before opening it. OSC 8 links can show any text, so the text
//! doesn't tell where a click goes; and a `file://` link to a program
//! would run it on macOS and Windows.

use std::path::{Path, PathBuf};

use nuntio_render::{Rect, UiRect, UiText};
use nuntio_term::Rgb;
use unicode_width::UnicodeWidthStr;

use crate::style::{UiMetrics, framed_box, hairline, truncate, truncate_start};

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

const PROGRAM: &str = "it could run a program";

/// Why a link must not be opened, if it mustn't.
pub fn check(url: &str) -> Result<(), String> {
    let Some(path) = file_path(url, &local_host(), Drives::current())? else {
        return Ok(());
    };
    // By name first: a missing file with a program extension is refused too.
    if has_program_extension(&path) {
        return Err(PROGRAM.into());
    }
    // What the path really names: `Evil.app/Contents/..` or a symlink can
    // hide a program.
    let path = match std::fs::canonicalize(&path) {
        Ok(path) => path,
        // A missing file can't run; the opener reports it.
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(format!("it can't be checked ({err})")),
    };
    let metadata =
        std::fs::metadata(&path).map_err(|err| format!("it can't be checked ({err})"))?;
    if is_program(&path, &metadata) {
        return Err(PROGRAM.into());
    }
    Ok(())
}

fn local_host() -> String {
    gethostname::gethostname().to_string_lossy().into_owned()
}

/// What `/C:/…` in a `file://` URL means here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Drives {
    Windows,
    Wsl,
    Unsupported,
}

impl Drives {
    fn current() -> Self {
        if cfg!(windows) {
            Self::Windows
        } else if crate::wsl::running_in_wsl() {
            Self::Wsl
        } else {
            Self::Unsupported
        }
    }
}

/// The local path of a `file://` URL, `None` for other schemes. Hosts
/// other than this machine are refused: on Windows they are network
/// shares.
fn file_path(url: &str, local_host: &str, drives: Drives) -> Result<Option<PathBuf>, String> {
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
    if path.contains('\0') {
        return Err("its path contains a NUL character".into());
    }
    // `file:///C:/Users` is a drive: what it means depends on the system.
    let rest = path.strip_prefix('/').unwrap_or(&path);
    let bytes = rest.as_bytes();
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && matches!(bytes[1], b':' | b'|') {
        // `C:foo` is relative to the current directory on that drive.
        if bytes.len() > 2 && !matches!(bytes[2], b'/' | b'\\') {
            return Err("it names a Windows drive".into());
        }
        let mapped = match drives {
            Drives::Windows => format!("{}:{}", &rest[..1], &rest[2..]),
            Drives::Wsl => format!(
                "/mnt/{}{}",
                rest[..1].to_ascii_lowercase(),
                rest[2..].replace('\\', "/")
            ),
            Drives::Unsupported => return Err("it names a Windows drive".into()),
        };
        return Ok(Some(PathBuf::from(mapped)));
    }
    // `file:////evil/share` names no host, yet Windows reads `//evil/share`
    // as a network share (and `\\?\…` as a device path).
    if path.starts_with(['/', '\\']) && path[1..].starts_with(['/', '\\']) {
        return Err("it points to another computer".into());
    }
    Ok(Some(PathBuf::from(&*path)))
}

/// Opening it would start something: a known program type (including
/// macOS app bundles, which are directories) or an executable file.
fn is_program(path: &Path, metadata: &std::fs::Metadata) -> bool {
    if has_program_extension(path) {
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

/// Whether the name of `path` ends in a program extension.
fn has_program_extension(path: &Path) -> bool {
    let name = path.file_name().map(|n| n.to_string_lossy());
    let extension = name.as_deref().and_then(program_extension).unwrap_or("");
    PROGRAM_EXTENSIONS
        .iter()
        .any(|p| p.eq_ignore_ascii_case(extension))
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

/// `url` up to the end of its authority, without userinfo (`user@`, which can fake a host),
/// and the rest. The authority also ends at `\`, where browsers end it for http(s).
fn split_host(url: &str) -> (String, &str) {
    let Some(scheme_end) = url.find("://").map(|i| i + 3) else {
        return (String::new(), url);
    };
    let rest = &url[scheme_end..];
    let end = rest.find(['/', '?', '#', '\\']).unwrap_or(rest.len());
    let authority = &rest[..end];
    let host = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    (format!("{}{host}", &url[..scheme_end]), &rest[end..])
}

/// The hint text in `cells` columns: the host is never cut, only the rest is; a host too wide
/// shows its end (the registrable domain).
fn hint_text(url: &str, cells: usize) -> String {
    let (start, rest) = split_host(url);
    let used = start.width();
    if used > cells {
        return truncate_start(&start, cells);
    }
    format!("{start}{}", truncate(rest, cells - used))
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
    let text = hint_text(url, fits);
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
        let path = |url| file_path(url, "box", Drives::Unsupported).unwrap();
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
    }

    #[test]
    fn other_hosts_are_refused() {
        let path = |url| file_path(url, "box", Drives::Unsupported);
        let err = path("file://evil.example/share/x.exe").unwrap_err();
        assert!(err.contains("evil.example"), "{err}");
        assert!(path("file:///%FF").is_err());
        // No host, but a network share on Windows.
        assert!(path("file:////evil/share/x.txt").is_err());
        assert!(path("file:///%5C%5Cevil%5Cshare%5Cx").is_err());
        assert!(path("file:///%5C%5C?%5CUNC%5Cevil%5Cx").is_err());
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
            check(&url(&dir.join("missing.txt"))),
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
        assert!(results[4].is_err(), "missing exe");
        assert!(results[5].is_err(), "app bundle with a fragment");
        assert!(results[6].is_err(), "exe with a query");
        assert!(results[7].is_err(), "app bundle behind `..`");
        assert!(results[8].is_ok(), "missing text file");
        #[cfg(unix)]
        assert!(script.is_err(), "executable");
        #[cfg(unix)]
        assert!(symlink.is_err(), "symlink to an app bundle");
        assert!(check("https://example.com/x.exe").is_ok());
    }

    #[test]
    fn listed_extensions_are_refused() {
        for ext in [
            "py",
            "pyw",
            "pyz",
            "sh",
            "rdp",
            "jnlp",
            "wsb",
            "ws",
            "wsc",
            "sct",
            "msu",
            "mst",
            "ps1xml",
            "psc1",
            "theme",
            "themepack",
            "xbap",
            "vsto",
            "gadget",
        ] {
            assert!(
                check(&format!("file:///nonexistent/x.{ext}")).is_err(),
                "{ext}"
            );
            let upper = ext.to_uppercase();
            assert!(
                check(&format!("file:///nonexistent/X.{upper}")).is_err(),
                "{upper}"
            );
        }
    }

    #[test]
    fn drive_letters_depend_on_the_system() {
        use Drives::*;
        let p = |u, d| file_path(u, "box", d);
        let some = |s: &str| Ok(Some(PathBuf::from(s)));
        assert_eq!(p("file:///C:/x", Windows), some("C:/x"));
        assert_eq!(p("file:///C%7C/x", Windows), some("C:/x"));
        assert_eq!(p("file:///C|/x", Windows), some("C:/x"));
        assert_eq!(p("file:///C:/x.bat", Wsl), some("/mnt/c/x.bat"));
        assert_eq!(p("file:///D:%5Cx", Wsl), some("/mnt/d/x"));
        assert!(p("file:///C:/x.bat", Unsupported).is_err());
        assert!(p("file:///C|/x", Unsupported).is_err());
        assert!(p("file:///C:x", Wsl).is_err());
        assert_eq!(p("file:///C", Unsupported), some("/C"));
        assert_eq!(p("file:///tmp/x", Unsupported), some("/tmp/x"));
    }

    #[test]
    fn nul_is_refused() {
        assert!(file_path("file:///tmp/a%00.txt", "box", Drives::Unsupported).is_err());
        assert!(check("file:///tmp/x%00.exe").is_err());
    }

    #[test]
    fn missing_programs_are_refused_and_missing_files_are_not() {
        assert!(check("file:///nonexistent-dir/setup.exe").is_err());
        assert!(check("file:///nonexistent-dir/setup.exe::$DATA").is_err());
        assert!(check("file:///nonexistent-dir/notes.txt").is_ok());
        assert!(check("file:///C:/Windows/System32/calc.exe").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn unresolvable_paths_are_refused() {
        let dir = std::env::temp_dir().join(format!("nuntio-link-enotdir-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.txt");
        std::fs::write(&file, "").unwrap();
        let result = check(&format!("file://{}/x.txt", file.display()));
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(result.is_err());
    }

    #[test]
    fn hint_shows_the_real_host() {
        assert_eq!(
            hint_text("https://github.com.x@evil.example/p", 100),
            "https://evil.example/p"
        );
        assert_eq!(
            hint_text("https://a.example/x@y", 100),
            "https://a.example/x@y"
        );
        // Browsers end the authority of http(s) at a backslash.
        assert_eq!(
            hint_text("https://evil.example\\@bank.example/login", 100),
            "https://evil.example\\@bank.example/login"
        );
        assert_eq!(hint_text("https://a@b\\@c/", 100), "https://b\\@c/");
        assert_eq!(
            hint_text("mailto:me@example.com", 100),
            "mailto:me@example.com"
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
        let url = "https://example.com/a/very/long/path";
        let hint = |width| {
            let pane = Rect { width, ..PANE };
            let (_, texts) = draw_hint(
                url,
                pane,
                (0.0, 0.0),
                METRICS,
                Rgb::default(),
                Rgb::default(),
            );
            texts[0].text.clone()
        };
        // The host is kept whole, the rest is cut.
        assert_eq!(hint(300.0), "https://example.com/a/very/…");
        // A host too wide shows its end.
        assert_eq!(hint(120.0), "…ample.com");
    }

    #[test]
    fn long_hosts_keep_their_end() {
        assert_eq!(
            hint_text("https://accounts.google.com.evil.io/x", 10),
            "…m.evil.io"
        );
    }
}
