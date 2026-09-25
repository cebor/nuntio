//! Watching the config file and theme directory for changes.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError, mpsc};
use std::time::Duration;

use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};

/// Editors write files in several steps; wait for them to settle.
const DEBOUNCE: Duration = Duration::from_millis(150);

/// Keeps watching as long as it's alive.
pub struct ConfigWatcher {
    _watcher: Arc<Mutex<RecommendedWatcher>>,
}

enum Signal {
    /// A watched file changed.
    Changed,
    /// A directory on the way to a watched directory, or one of those
    /// directories itself, appeared or disappeared.
    Rescan,
}

impl ConfigWatcher {
    /// Call `on_change` (from a watcher thread) when `config_path` or a file
    /// in `themes_dir` changes. Directories are watched rather than files,
    /// because editors often replace files atomically on save. Directories
    /// that don't exist yet are picked up once they are created.
    pub fn new(
        config_path: &Path,
        themes_dir: Option<&Path>,
        on_change: impl Fn() + Send + 'static,
    ) -> notify::Result<Self> {
        // Event paths may be canonical (macOS FSEvents resolves symlinks such
        // as /var -> /private/var), and the config may itself be a symlink
        // into a dotfiles repository: match and watch every variant.
        let config_files = path_variants(config_path);
        let theme_dirs: Vec<PathBuf> = themes_dir.map(path_variants).unwrap_or_default();
        let mut wanted: Vec<PathBuf> = config_files
            .iter()
            .filter_map(|p| p.parent())
            .map(Path::to_owned)
            .chain(theme_dirs.iter().cloned())
            .collect();
        wanted.sort();
        wanted.dedup();

        let (tx, rx) = mpsc::channel::<Signal>();
        let watched_themes = theme_dirs;
        let wanted_dirs = wanted.clone();
        let watcher = notify::recommended_watcher(move |result: notify::Result<Event>| {
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
                config_files.contains(path)
                    || watched_themes.iter().any(|dir| path.starts_with(dir))
            });
            let on_the_way = event
                .paths
                .iter()
                .any(|path| wanted_dirs.iter().any(|dir| dir.starts_with(path)));
            // Deleting a watched themes directory is both.
            if relevant {
                let _ = tx.send(Signal::Changed);
            }
            if on_the_way {
                let _ = tx.send(Signal::Rescan);
            }
        })?;
        let watcher = Arc::new(Mutex::new(watcher));
        let mut watched = Vec::new();
        watch_existing(&mut watcher.lock().unwrap(), &wanted, &mut watched)?;

        // Collapse bursts of events into one notification. The thread only
        // holds a weak reference: dropping `ConfigWatcher` drops the watcher
        // and with it `tx`, which ends the thread.
        let thread_watcher = Arc::downgrade(&watcher);
        std::thread::Builder::new()
            .name("config watcher".into())
            .spawn(move || {
                while let Ok(first) = rx.recv() {
                    let mut changed = matches!(first, Signal::Changed);
                    let mut rescan = !changed;
                    while let Ok(signal) = rx.recv_timeout(DEBOUNCE) {
                        match signal {
                            Signal::Changed => changed = true,
                            Signal::Rescan => rescan = true,
                        }
                    }
                    if rescan {
                        let Some(watcher) = thread_watcher.upgrade() else {
                            break;
                        };
                        let mut watcher = watcher.lock().unwrap_or_else(PoisonError::into_inner);
                        match watch_existing(&mut watcher, &wanted, &mut watched) {
                            // A new directory may already contain the config.
                            Ok(added) => changed |= added,
                            Err(err) => tracing::warn!("config watcher: {err}"),
                        }
                    }
                    if changed {
                        on_change();
                    }
                }
                tracing::debug!("config watcher stopped");
            })?;
        Ok(Self { _watcher: watcher })
    }
}

/// Watch each wanted directory, or its closest existing ancestor while it
/// doesn't exist, skipping what is already watched. Directories deleted
/// since are dropped first, so that their ancestor takes over and sees
/// them come back. Returns whether a directory was added.
fn watch_existing(
    watcher: &mut RecommendedWatcher,
    wanted: &[PathBuf],
    watched: &mut Vec<PathBuf>,
) -> notify::Result<bool> {
    watched.retain(|dir| {
        let exists = dir.is_dir();
        if !exists {
            // The OS usually ended the watch already.
            let _ = watcher.unwatch(dir);
        }
        exists
    });
    let mut added = false;
    for dir in wanted {
        let Some(existing) = dir.ancestors().find(|d| d.is_dir()) else {
            continue;
        };
        if watched.iter().any(|w| w == existing) {
            continue;
        }
        watcher.watch(existing, RecursiveMode::NonRecursive)?;
        tracing::debug!(dir = %existing.display(), "watching for config changes");
        watched.push(existing.to_owned());
        added = true;
    }
    Ok(added)
}

/// A path as given, with its closest existing ancestor resolved (the rest may
/// not exist yet), and fully resolved (following a symlinked file to its
/// target).
fn path_variants(path: &Path) -> Vec<PathBuf> {
    let mut variants = vec![path.to_owned()];
    if let Some(parent) = path.parent()
        && let Some((existing, resolved)) = parent
            .ancestors()
            .find_map(|dir| Some((dir, dir.canonicalize().ok()?)))
        && let Ok(rest) = path.strip_prefix(existing)
    {
        variants.push(resolved.join(rest));
    }
    if let Ok(resolved) = path.canonicalize() {
        variants.push(resolved);
    }
    variants.dedup();
    variants
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

        // FSEvents may still deliver the write above, made just before the
        // watcher started.
        std::thread::sleep(Duration::from_millis(500));
        while rx.try_recv().is_ok() {}

        // Unrelated files and reading the config are ignored.
        std::fs::write(dir.join("other.txt"), "x").unwrap();
        std::fs::read_to_string(&path).unwrap();
        assert!(quiet(&rx), "reported an irrelevant event");

        // So are changes to the watched directory itself.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::Permissions::from_mode(0o700);
            std::fs::set_permissions(&dir, mode).unwrap();
            assert!(quiet(&rx), "reported a change to the directory");
        }

        // Several writes in a burst are reported once.
        std::fs::write(&path, "scrollback = 5").unwrap();
        std::fs::write(&path, "scrollback = 6").unwrap();
        let changed = rx.recv_timeout(Duration::from_secs(3));
        let settled = quiet(&rx);
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(changed.is_ok(), "no change reported");
        assert!(settled, "burst reported more than once");
    }

    #[test]
    fn dropping_the_watcher_stops_its_thread() {
        let dir = std::env::temp_dir().join(format!("nuntio-watch-drop-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (tx, rx) = mpsc::channel::<()>();
        let watcher = ConfigWatcher::new(&dir.join("config.toml"), None, move || {
            let _ = tx.send(());
        })
        .unwrap();
        drop(watcher);
        // The thread owns `on_change`; once it ends, the sender is gone.
        let result = rx.recv_timeout(Duration::from_secs(3));
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(result, Err(mpsc::RecvTimeoutError::Disconnected));
    }

    #[test]
    fn notices_a_config_directory_created_later() {
        let base = std::env::temp_dir().join(format!("nuntio-watch-new-{}", std::process::id()));
        std::fs::create_dir_all(&base).unwrap();
        let dir = base.join("config").join("nuntio");
        let path = dir.join("config.toml");

        let (tx, rx) = mpsc::channel();
        let _watcher = ConfigWatcher::new(&path, None, move || {
            let _ = tx.send(());
        })
        .unwrap();

        // Two levels are created before the file appears.
        std::fs::create_dir_all(&dir).unwrap();
        std::thread::sleep(Duration::from_millis(500));
        while rx.try_recv().is_ok() {}
        std::fs::write(&path, "scrollback = 5").unwrap();
        let changed = rx.recv_timeout(Duration::from_secs(3));
        std::fs::remove_dir_all(&base).unwrap();
        assert!(changed.is_ok(), "config in a new directory not noticed");
    }

    #[test]
    // Windows keeps a watched directory until the watch ends, so the test
    // can't delete and recreate it.
    #[cfg(unix)]
    fn notices_a_config_directory_deleted_and_created_again() {
        let base = std::env::temp_dir().join(format!("nuntio-watch-again-{}", std::process::id()));
        let dir = base.join("nuntio");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(&path, "").unwrap();

        let (tx, rx) = mpsc::channel();
        let _watcher = ConfigWatcher::new(&path, None, move || {
            let _ = tx.send(());
        })
        .unwrap();

        // A dotfiles tool replaces the whole directory.
        std::fs::remove_dir_all(&dir).unwrap();
        std::thread::sleep(Duration::from_millis(500));
        std::fs::create_dir_all(&dir).unwrap();
        std::thread::sleep(Duration::from_millis(500));
        while rx.try_recv().is_ok() {}
        std::fs::write(&path, "scrollback = 5").unwrap();
        let changed = rx.recv_timeout(Duration::from_secs(3));
        std::fs::remove_dir_all(&base).unwrap();
        assert!(
            changed.is_ok(),
            "config in a recreated directory not noticed"
        );
    }

    #[test]
    #[cfg(unix)]
    fn resolves_a_symlinked_ancestor_of_a_missing_path() {
        let base = std::env::temp_dir().join(format!("nuntio-watch-vars-{}", std::process::id()));
        let real = base.join("real");
        std::fs::create_dir_all(&real).unwrap();
        let link = base.join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        let path = link.join("nuntio").join("config.toml");
        let variants = path_variants(&path);
        let expected = real.canonicalize().unwrap().join("nuntio/config.toml");
        std::fs::remove_dir_all(&base).unwrap();
        assert!(variants.contains(&path));
        assert!(variants.contains(&expected), "{variants:?}");
    }

    #[test]
    #[cfg(unix)]
    fn follows_a_symlinked_config_file() {
        let base = std::env::temp_dir().join(format!("nuntio-watch-link-{}", std::process::id()));
        let (config_dir, dotfiles) = (base.join("config"), base.join("dotfiles"));
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::create_dir_all(&dotfiles).unwrap();
        let target = dotfiles.join("nuntio.toml");
        std::fs::write(&target, "").unwrap();
        let link = config_dir.join("config.toml");
        std::os::unix::fs::symlink(&target, &link).unwrap();

        let (tx, rx) = mpsc::channel();
        let _watcher = ConfigWatcher::new(&link, None, move || {
            let _ = tx.send(());
        })
        .unwrap();

        // Editing the file in the dotfiles directory is noticed.
        std::fs::write(&target, "scrollback = 5").unwrap();
        let changed = rx.recv_timeout(Duration::from_secs(3));
        std::fs::remove_dir_all(&base).unwrap();
        assert!(changed.is_ok(), "change behind the symlink not reported");
    }
}
