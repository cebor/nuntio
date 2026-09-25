//! Links under the pointer: the hint that shows where one leads, and the
//! check before opening it. OSC 8 links can show any text, so the text
//! doesn't tell where a click goes; and a `file://` link to a program
//! would run it on macOS and Windows.

use std::path::{Path, PathBuf};

use nuntio_render::{CellMetrics, UiRect, UiText};
use nuntio_term::Rgb;
use unicode_width::UnicodeWidthStr;

use crate::pane_tree::Rect;
use crate::tab_bar::{mix, truncate};

/// Distance from the pane's bottom corner, in logical pixels.
const MARGIN: f64 = 6.0;
/// Space between the border and the text, in logical pixels.
const PADDING: f64 = 3.0;

/// Extensions of files that the OS runs, installs or mounts when they are
/// opened, on any platform: a link could come from a remote machine.
const PROGRAM_EXTENSIONS: &[&str] = &[
    // Windows
    "appref-ms",
    "application",
    "bat",
    "cmd",
    "com",
    "cpl",
    "exe",
    "hta",
    "inf",
    "jse",
    "lnk",
    "msc",
    "msi",
    "msp",
    "pif",
    "ps1",
    "reg",
    "scf",
    "scr",
    "url",
    "vbe",
    "vbs",
    "wsf",
    "wsh",
    // macOS
    "app",
    "applescript",
    "command",
    "dmg",
    "mpkg",
    "pkg",
    "scpt",
    "terminal",
    "tool",
    "workflow",
    // Linux
    "appimage",
    "desktop",
    // Anywhere
    "jar",
    "js",
];

/// Why a link must not be opened, if it mustn't.
pub fn check(url: &str) -> Result<(), String> {
    let Some(path) = file_path(url, &local_host())? else {
        return Ok(());
    };
    // A missing file can't run; the opener reports it.
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
    let rest = &url[scheme.len()..];
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
    Ok(Some(PathBuf::from(path)))
}

/// Opening it would start something: a known program type (including
/// macOS app bundles, which are directories) or an executable file.
fn is_program(path: &Path, metadata: &std::fs::Metadata) -> bool {
    let extension = path.extension().and_then(|e| e.to_str()).unwrap_or("");
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

/// Area of the hint for `columns` cells of text: the bottom-left corner of
/// the pane, or the bottom-right one while the pointer is in the way.
fn hint_bounds(
    columns: usize,
    pane: Rect,
    pointer: (f32, f32),
    cell: CellMetrics,
    scale: f64,
) -> Rect {
    let margin = (MARGIN * scale).round() as f32;
    let padding = (PADDING * scale).round() as f32;
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

/// Draw where the link `url` leads, in `pane`. `cell` is the size of the
/// small UI font.
pub fn draw_hint(
    url: &str,
    pane: Rect,
    pointer: (f32, f32),
    cell: CellMetrics,
    scale: f64,
    background: Rgb,
    foreground: Rgb,
) -> (Vec<UiRect>, Vec<UiText>) {
    let padding = (PADDING * scale).round() as f32;
    let margin = (MARGIN * scale).round() as f32;
    let fits = ((pane.width - 2.0 * (margin + padding)) / cell.width as f32).max(0.0) as usize;
    // The start names the host and scheme; cut the end.
    let text = truncate(url, fits);
    let bounds = hint_bounds(text.width(), pane, pointer, cell, scale);
    let border = scale.round().max(1.0) as f32;
    let rect = |r: Rect, color| UiRect {
        x: r.x,
        y: r.y,
        width: r.width,
        height: r.height,
        color,
        radius: 0.0,
    };
    let inner = Rect {
        x: bounds.x + border,
        y: bounds.y + border,
        width: bounds.width - 2.0 * border,
        height: bounds.height - 2.0 * border,
    };
    let rects = vec![
        rect(bounds, mix(background, foreground, 0.35)),
        rect(inner, mix(background, foreground, 0.12)),
    ];
    let texts = vec![UiText {
        x: bounds.x + padding,
        y: bounds.y + padding,
        text,
        color: foreground,
        bold: false,
        small: true,
    }];
    (rects, texts)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CELL: CellMetrics = CellMetrics {
        width: 10,
        height: 20,
        baseline: 15,
        underline_y: 17,
        stroke: 1,
        strikeout_y: 10,
    };
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
    }

    #[test]
    fn programs_are_not_opened() {
        let dir = std::env::temp_dir().join(format!("nuntio-link-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("Evil.app")).unwrap();
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
        ];
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
        #[cfg(unix)]
        assert!(script.is_err(), "executable");
        assert!(check("https://example.com/x.exe").is_ok());
    }

    #[test]
    fn hint_sits_bottom_left_and_dodges_the_pointer() {
        let bounds = |pointer| hint_bounds(20, PANE, pointer, CELL, 1.0);
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
            CELL,
            1.0,
            Rgb::default(),
            Rgb::default(),
        );
        assert_eq!(texts[0].text, "https://e…");
    }
}
