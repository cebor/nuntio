//! Watching the config file and theme directory for changes.

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};

/// Editors write files in several steps; wait for them to settle.
const DEBOUNCE: Duration = Duration::from_millis(150);

/// Keeps watching as long as it's alive.
pub struct ConfigWatcher {
    _watcher: RecommendedWatcher,
}

impl ConfigWatcher {
    /// Call `on_change` (from a watcher thread) when `config_path` or a file
    /// in `themes_dir` changes. Directories are watched rather than files,
    /// because editors often replace files atomically on save.
    pub fn new(
        config_path: &Path,
        themes_dir: Option<&Path>,
        on_change: impl Fn() + Send + 'static,
    ) -> notify::Result<Self> {
        let config_file = config_path.to_owned();
        let watched_themes = themes_dir.map(Path::to_owned);
        let (tx, rx) = mpsc::channel::<()>();

        let mut watcher = notify::recommended_watcher(move |result: notify::Result<Event>| {
            let event = match result {
                Ok(event) => event,
                Err(err) => {
                    tracing::warn!("config watcher: {err}");
                    return;
                }
            };
            // Reading the files (as a reload does) must not trigger a reload.
            if matches!(event.kind, EventKind::Access(_)) {
                return;
            }
            let relevant = event.paths.iter().any(|path| {
                *path == config_file
                    || watched_themes
                        .as_deref()
                        .is_some_and(|dir| path.starts_with(dir))
            });
            if relevant {
                let _ = tx.send(());
            }
        })?;

        // Collapse bursts of events into one notification.
        std::thread::Builder::new()
            .name("config watcher".into())
            .spawn(move || {
                while rx.recv().is_ok() {
                    while rx.recv_timeout(DEBOUNCE).is_ok() {}
                    on_change();
                }
            })?;

        let mut dirs: Vec<PathBuf> = config_path
            .parent()
            .filter(|p| p.is_dir())
            .map(Path::to_owned)
            .into_iter()
            .collect();
        dirs.extend(themes_dir.filter(|d| d.is_dir()).map(Path::to_owned));
        for dir in &dirs {
            watcher.watch(dir, RecursiveMode::NonRecursive)?;
            tracing::debug!(dir = %dir.display(), "watching for config changes");
        }
        Ok(Self { _watcher: watcher })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_changes_to_the_config_file_only() {
        let dir = std::env::temp_dir().join(format!("nuntio-watch-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(&path, "").unwrap();

        let (tx, rx) = mpsc::channel();
        let _watcher = ConfigWatcher::new(&path, None, move || {
            let _ = tx.send(());
        })
        .unwrap();
        let quiet = |rx: &mpsc::Receiver<()>| rx.recv_timeout(Duration::from_millis(500)).is_err();

        // Unrelated files and reading the config are ignored.
        std::fs::write(dir.join("other.txt"), "x").unwrap();
        std::fs::read_to_string(&path).unwrap();
        assert!(quiet(&rx), "reported an irrelevant event");

        // Several writes in a burst are reported once.
        std::fs::write(&path, "scrollback = 5").unwrap();
        std::fs::write(&path, "scrollback = 6").unwrap();
        let changed = rx.recv_timeout(Duration::from_secs(3));
        let settled = quiet(&rx);
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(changed.is_ok(), "no change reported");
        assert!(settled, "burst reported more than once");
    }
}
