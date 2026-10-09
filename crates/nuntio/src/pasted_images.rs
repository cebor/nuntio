//! Clipboard images saved as PNG files, so a paste can hand a program
//! their path (AI CLIs like Claude Code attach pasted image paths).

use std::fs::{DirBuilder, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Context;
use image::ImageEncoder;
use image::codecs::png::PngEncoder;

pub struct PastedImages {
    /// Created on the first save; removed by `remove_all`.
    dir: PathBuf,
    created: bool,
    /// Directories of earlier runs were looked for.
    swept: bool,
    count: u32,
}

impl PastedImages {
    pub fn new() -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        Self {
            // Pid and time make the name unique to this nuntio.
            dir: std::env::temp_dir().join(format!("nuntio-paste-{}-{nanos}", std::process::id())),
            created: false,
            swept: false,
            count: 0,
        }
    }

    /// Save RGBA8 pixels as the next PNG; returns its path.
    pub fn save(&mut self, image: &arboard::ImageData) -> anyhow::Result<PathBuf> {
        let size = u32::try_from(image.width)
            .ok()
            .zip(u32::try_from(image.height).ok())
            .filter(|_| {
                image
                    .width
                    .checked_mul(image.height)
                    .and_then(|pixels| pixels.checked_mul(4))
                    == Some(image.bytes.len())
            });
        let Some((width, height)) = size else {
            anyhow::bail!(
                "clipboard image has {} bytes for {}x{} pixels",
                image.bytes.len(),
                image.width,
                image.height
            );
        };
        if !self.swept {
            self.swept = true;
            sweep_stale(&std::env::temp_dir(), SystemTime::now(), process_alive);
        }
        if !self.created {
            create_private_dir(&self.dir)
                .with_context(|| format!("failed to create {}", self.dir.display()))?;
            self.created = true;
        }
        self.count += 1;
        let path = self.dir.join(format!("image-{}.png", self.count));
        let file = File::create_new(&path)
            .with_context(|| format!("failed to create {}", path.display()))?;
        let mut writer = BufWriter::new(file);
        PngEncoder::new(&mut writer)
            .write_image(&image.bytes, width, height, image::ExtendedColorType::Rgba8)
            .map_err(anyhow::Error::from)
            .and_then(|()| writer.flush().map_err(anyhow::Error::from))
            .with_context(|| format!("failed to write {}", path.display()))?;
        Ok(path)
    }

    /// Delete the directory with every saved image (at exit).
    pub fn remove_all(&mut self) {
        if !self.created {
            return;
        }
        if let Err(err) = std::fs::remove_dir_all(&self.dir) {
            tracing::debug!("failed to remove {}: {err}", self.dir.display());
        }
        self.created = false;
    }
}

/// Directories of a nuntio that is gone without a clean exit stay in the
/// temp directory; those of an owner nobody can ask about are removed
/// after this long.
const STALE_AFTER: Duration = Duration::from_secs(24 * 60 * 60);

/// The process id in the name of a `PastedImages` directory.
fn dir_pid(name: &str) -> Option<u32> {
    let (pid, nanos) = name.strip_prefix("nuntio-paste-")?.split_once('-')?;
    if nanos.is_empty() || !nanos.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    pid.parse().ok().filter(|&pid| pid > 0)
}

/// Remove the directories under `root` that earlier runs left behind:
/// those of a process that no longer runs, or, where that can't be told
/// (`alive` returns `None`), older than `STALE_AFTER`.
fn sweep_stale(root: &Path, now: SystemTime, alive: impl Fn(u32) -> Option<bool>) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        // Not a symlink to a directory: only what nuntio itself made.
        if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        let Some(pid) = entry.file_name().to_str().and_then(dir_pid) else {
            continue;
        };
        let stale = match alive(pid) {
            Some(running) => !running,
            None => entry
                .metadata()
                .and_then(|metadata| metadata.modified())
                .is_ok_and(|modified| {
                    now.duration_since(modified).unwrap_or_default() >= STALE_AFTER
                }),
        };
        if stale && let Err(err) = std::fs::remove_dir_all(entry.path()) {
            tracing::debug!("failed to remove {}: {err}", entry.path().display());
        }
    }
}

/// Whether the process `pid` runs; `None` if that can't be told.
#[cfg(unix)]
fn process_alive(pid: u32) -> Option<bool> {
    let pid = libc::pid_t::try_from(pid).ok()?;
    // SAFETY: signal 0 only checks that the process exists.
    if unsafe { libc::kill(pid, 0) } == 0 {
        return Some(true);
    }
    // EPERM: it exists, but belongs to someone else.
    Some(std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM))
}

#[cfg(windows)]
fn process_alive(pid: u32) -> Option<bool> {
    use windows_sys::Win32::Foundation::{
        CloseHandle, ERROR_ACCESS_DENIED, ERROR_INVALID_PARAMETER, STILL_ACTIVE,
    };
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    // SAFETY: no pointers; returns a handle we close below, or null.
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        return match std::io::Error::last_os_error().raw_os_error() {
            Some(code) if code == ERROR_INVALID_PARAMETER as i32 => Some(false),
            Some(code) if code == ERROR_ACCESS_DENIED as i32 => Some(true),
            _ => None,
        };
    }
    let mut code = 0;
    // SAFETY: `handle` is a valid process handle and `code` outlives the call.
    let queried = unsafe { GetExitCodeProcess(handle, &mut code) } != 0;
    // SAFETY: `handle` was opened above and isn't used afterwards.
    unsafe { CloseHandle(handle) };
    queried.then_some(code == STILL_ACTIVE as u32)
}

#[cfg(not(any(unix, windows)))]
fn process_alive(_pid: u32) -> Option<bool> {
    None
}

/// Create only the leaf, readable by this user alone where that's
/// possible: screenshots shouldn't be visible to others in a shared `/tmp`.
#[cfg(unix)]
fn create_private_dir(dir: &std::path::Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    DirBuilder::new().mode(0o700).create(dir)
}

#[cfg(not(unix))]
fn create_private_dir(dir: &std::path::Path) -> std::io::Result<()> {
    DirBuilder::new().create(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(bytes: Vec<u8>) -> arboard::ImageData<'static> {
        arboard::ImageData {
            width: 2,
            height: 1,
            bytes: bytes.into(),
        }
    }

    #[test]
    fn saved_images_are_pngs_and_removed_at_exit() -> anyhow::Result<()> {
        let pixels = vec![255, 0, 0, 255, 0, 0, 255, 128];
        let mut images = PastedImages::new();
        let first = images.save(&image(pixels.clone()))?;
        let second = images.save(&image(pixels.clone()))?;
        assert_ne!(first, second);
        for path in [&first, &second] {
            assert_eq!(path.extension().and_then(|e| e.to_str()), Some("png"));
        }
        let decoded = image::open(&first)?.to_rgba8();
        assert_eq!(decoded.dimensions(), (2, 1));
        assert_eq!(decoded.into_raw(), pixels);

        assert!(images.save(&image(vec![0; 4])).is_err());

        images.remove_all();
        assert!(!first.parent().unwrap().exists());
        Ok(())
    }

    #[test]
    fn stale_directories_are_swept() -> anyhow::Result<()> {
        let root = std::env::temp_dir().join(format!("nuntio-sweep-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root)?;
        for name in [
            "nuntio-paste-1-10",
            "nuntio-paste-2-10",
            "nuntio-paste-3-10",
            "other",
        ] {
            std::fs::create_dir(root.join(name))?;
        }
        std::fs::write(root.join("nuntio-paste-4-5"), b"")?;
        let alive = |pid| match pid {
            1 => Some(false),
            2 => Some(true),
            _ => None,
        };

        let later = SystemTime::now() + STALE_AFTER * 2;
        sweep_stale(&root, later, alive);
        assert!(!root.join("nuntio-paste-1-10").exists(), "dead owner");
        assert!(root.join("nuntio-paste-2-10").exists(), "live owner");
        assert!(!root.join("nuntio-paste-3-10").exists(), "unknown and old");
        assert!(root.join("other").exists(), "not ours");
        assert!(root.join("nuntio-paste-4-5").exists(), "a file");

        std::fs::create_dir(root.join("nuntio-paste-3-11"))?;
        sweep_stale(&root, SystemTime::now(), alive);
        assert!(root.join("nuntio-paste-3-11").exists(), "unknown and fresh");

        assert_eq!(dir_pid("nuntio-paste-12-345"), Some(12));
        assert_eq!(dir_pid("nuntio-paste-0-1"), None);
        assert_eq!(dir_pid("nuntio-paste-x-1"), None);
        assert_eq!(dir_pid("nuntio-paste-12"), None);
        assert_eq!(dir_pid("other"), None);
        std::fs::remove_dir_all(&root)?;
        Ok(())
    }
}
