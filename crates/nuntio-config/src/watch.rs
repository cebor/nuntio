//! Watching the config file and theme directory for changes.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError, mpsc};
use std::time::Duration;

use notify::event::ModifyKind;
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};

/// Editors write files in several steps; wait for them to settle.
const DEBOUNCE: Duration = Duration::from_millis(150);

/// Keeps watching as long as it's alive.
pub struct ConfigWatcher {
    _watcher: Arc<Mutex<RecommendedWatcher>>,
}

enum Signal {
    /// A watched file changed, or a watched directory was moved or deleted.
    Changed,
    /// A directory on the way to a watched directory, or one of those
    /// directories itself, appeared or disappeared.
    Rescan,
}

/// What events are matched against and which directories are needed.
/// Resolved again after changes: symlinks may point elsewhere by then.
#[derive(PartialEq, Eq)]
struct Targets {
    config_files: Vec<PathBuf>,
    theme_dirs: Vec<PathBuf>,
    /// Parents of `config_files` and `theme_dirs`; sorted, no duplicates.
    wanted: Vec<PathBuf>,
}

impl Targets {
    fn resolve(config_path: &Path, themes_dir: Option<&Path>) -> Self {
        let config_files = path_variants(config_path);
        let theme_dirs: Vec<PathBuf> = themes_dir.map(path_variants).unwrap_or_default();
        let mut wanted: Vec<PathBuf> = config_files
            .iter()
            .filter_map(|p| p.parent())
            .map(Path::to_owned)
            .chain(theme_dirs.iter().cloned())
            .collect();
        // A symlinked directory on the way may be retargeted: the directory
        // holding the link sees that.
        for path in [Some(config_path), themes_dir].into_iter().flatten() {
            for ancestor in path.ancestors() {
                if ancestor
                    .symlink_metadata()
                    .is_ok_and(|m| m.file_type().is_symlink())
                    && let Some(parent) = ancestor.parent()
                    && !parent.as_os_str().is_empty()
                {
                    wanted.push(parent.to_owned());
                }
            }
        }
        wanted.sort();
        wanted.dedup();
        Self {
            config_files,
            theme_dirs,
            wanted,
        }
    }
}

/// Whether a theme file in `dir` (or `dir` itself) changed: only what
/// `ThemeSet::load` reads counts, not an editor's swap and backup files.
/// Emacs lock files (`.#x.toml`, dangling links) end in `.toml` too.
fn is_theme_path(path: &Path, dir: &Path) -> bool {
    path == dir
        || (path.parent() == Some(dir)
            && !path
                .file_name()
                .is_some_and(|name| name.as_encoded_bytes().starts_with(b".#"))
            && matches!(
                path.extension().and_then(|e| e.to_str()),
                Some("toml" | "itermcolors")
            ))
}

/// What an event means for the config.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Verdict {
    /// The config may differ now.
    changed: bool,
    /// The watched directories may have to change.
    rescan: bool,
}

fn classify(event: &Event, t: &Targets) -> Verdict {
    // Reading the files (as a reload does) must not trigger a reload.
    if matches!(event.kind, EventKind::Access(_)) {
        return Verdict::default();
    }
    // Events were lost: anything may have happened.
    if event.need_rescan() {
        return Verdict {
            changed: true,
            rescan: true,
        };
    }
    let relevant = event.paths.iter().any(|path| {
        t.config_files.contains(path) || t.theme_dirs.iter().any(|dir| is_theme_path(path, dir))
    });
    // A watched directory that is moved or deleted: what it holds is
    // no longer what the config was loaded from.
    let dir_gone = matches!(
        event.kind,
        EventKind::Remove(_) | EventKind::Modify(ModifyKind::Name(_))
    ) && event.paths.iter().any(|path| t.wanted.contains(path));
    let on_the_way = event
        .paths
        .iter()
        .any(|path| t.wanted.iter().any(|dir| dir.starts_with(path)));
    // Deleting a watched themes directory is both.
    Verdict {
        changed: relevant || dir_gone,
        rescan: on_the_way,
    }
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
        let targets = Arc::new(Mutex::new(Targets::resolve(config_path, themes_dir)));
        let initial = targets
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .wanted
            .clone();

        let (tx, rx) = mpsc::channel::<Signal>();
        let callback_targets = Arc::clone(&targets);
        let watcher = notify::recommended_watcher(move |result: notify::Result<Event>| {
            let event = match result {
                Ok(event) => event,
                Err(err) => {
                    tracing::warn!("config watcher: {err}");
                    return;
                }
            };
            let verdict = {
                let t = callback_targets
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                classify(&event, &t)
            };
            if verdict.changed {
                let _ = tx.send(Signal::Changed);
            }
            if verdict.rescan {
                let _ = tx.send(Signal::Rescan);
            }
        })?;
        let watcher = Arc::new(Mutex::new(watcher));
        let mut watched = Vec::new();
        let watching = watch_existing(
            &mut watcher.lock().unwrap_or_else(PoisonError::into_inner),
            &initial,
            &mut watched,
        );
        // Hot reload works as long as something is watched.
        if watched.is_empty()
            && let Some(err) = watching.failure
        {
            return Err(err);
        }

        // Collapse bursts of events into one notification. The thread only
        // holds a weak reference: dropping `ConfigWatcher` drops the watcher
        // and with it `tx`, which ends the thread.
        let thread_watcher = Arc::downgrade(&watcher);
        let config_path = config_path.to_owned();
        let themes_dir = themes_dir.map(Path::to_owned);
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
                    // Symlinks may point elsewhere by now. Never hold the
                    // lock across `watch_existing`: notify's `watch` waits for
                    // the event thread, which may wait for this lock.
                    let fresh = Targets::resolve(&config_path, themes_dir.as_deref());
                    let (moved, wanted) = {
                        let mut current = targets.lock().unwrap_or_else(PoisonError::into_inner);
                        let moved = *current != fresh;
                        if moved {
                            *current = fresh;
                        }
                        (moved, current.wanted.clone())
                    };
                    if rescan || moved {
                        let Some(watcher) = thread_watcher.upgrade() else {
                            break;
                        };
                        let mut watcher = watcher.lock().unwrap_or_else(PoisonError::into_inner);
                        changed |= watch_existing(&mut watcher, &wanted, &mut watched).added;
                    }
                    // A symlink now points elsewhere: the config may differ.
                    changed |= moved;
                    if changed {
                        on_change();
                    }
                }
                tracing::debug!("config watcher stopped");
            })?;
        Ok(Self { _watcher: watcher })
    }
}

/// What `watch_existing` did.
struct Watching {
    /// A directory was newly watched.
    added: bool,
    /// The first directory that could not be watched (others are logged).
    failure: Option<notify::Error>,
}

/// The directories to watch for `wanted` ones: each one itself, or its
/// closest existing ancestor while it doesn't exist. Sorted, no duplicates.
fn needed_dirs(wanted: &[PathBuf]) -> Vec<PathBuf> {
    let mut needed: Vec<PathBuf> = wanted
        .iter()
        .filter_map(|dir| dir.ancestors().find(|d| d.is_dir()))
        .map(Path::to_owned)
        .collect();
    needed.sort();
    needed.dedup();
    needed
}

/// Watch each wanted directory, or its closest existing ancestor while it
/// doesn't exist, skipping what is already watched. Watches no longer needed
/// (an ancestor stand-in whose directory exists now) and directories deleted
/// since are dropped first, so that their ancestor takes over and sees
/// them come back. Kept directories are watched anew: the OS ends the
/// watch on deletion even if the directory is back by now. A directory that
/// can't be watched is logged and skipped, to be tried again on the next
/// rescan; the others are still watched.
fn watch_existing(
    watcher: &mut RecommendedWatcher,
    wanted: &[PathBuf],
    watched: &mut Vec<PathBuf>,
) -> Watching {
    let needed = needed_dirs(wanted);
    let mut failure = None;
    let mut fail = |dir: &Path, err: notify::Error| {
        tracing::warn!(dir = %dir.display(), "config watcher: {err}");
        failure.get_or_insert(err);
    };
    watched.retain(|dir| {
        // The OS usually ended the watch already.
        let _ = watcher.unwatch(dir);
        if !needed.contains(dir) || !dir.is_dir() {
            return false;
        }
        match watcher.watch(dir, RecursiveMode::NonRecursive) {
            Ok(()) => true,
            Err(err) => {
                fail(dir, err);
                false
            }
        }
    });
    let mut added = false;
    for dir in needed {
        if watched.contains(&dir) {
            continue;
        }
        match watcher.watch(&dir, RecursiveMode::NonRecursive) {
            Ok(()) => {
                tracing::debug!(dir = %dir.display(), "watching for config changes");
                watched.push(dir);
                added = true;
            }
            Err(err) => fail(&dir, err),
        }
    }
    Watching { added, failure }
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
    fn notices_a_directory_recreated_within_the_debounce() {
        let base = std::env::temp_dir().join(format!("nuntio-watch-quick-{}", std::process::id()));
        let dir = base.join("nuntio");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(&path, "").unwrap();

        let (tx, rx) = mpsc::channel();
        let _watcher = ConfigWatcher::new(&path, None, move || {
            let _ = tx.send(());
        })
        .unwrap();

        std::fs::remove_dir_all(&dir).unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        std::thread::sleep(Duration::from_millis(500));
        while rx.try_recv().is_ok() {}
        std::fs::write(&path, "scrollback = 5").unwrap();
        let changed = rx.recv_timeout(Duration::from_secs(3));
        std::fs::remove_dir_all(&base).unwrap();
        assert!(
            changed.is_ok(),
            "config in a quickly recreated directory not noticed"
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

    #[test]
    #[cfg(unix)]
    fn notices_a_directory_replaced_by_rename() {
        let base = std::env::temp_dir().join(format!("nuntio-watch-swap-{}", std::process::id()));
        let (dir, new) = (base.join("nuntio"), base.join("new"));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::create_dir_all(&new).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(&path, "").unwrap();
        std::fs::write(new.join("config.toml"), "scrollback = 5").unwrap();

        let (tx, rx) = mpsc::channel();
        let _watcher = ConfigWatcher::new(&path, None, move || {
            let _ = tx.send(());
        })
        .unwrap();

        std::fs::rename(&dir, base.join("old")).unwrap();
        std::fs::rename(&new, &dir).unwrap();
        let swapped = rx.recv_timeout(Duration::from_secs(3));
        std::thread::sleep(Duration::from_millis(500));
        while rx.try_recv().is_ok() {}
        std::fs::write(&path, "scrollback = 6").unwrap();
        let changed = rx.recv_timeout(Duration::from_secs(3));
        std::fs::remove_dir_all(&base).unwrap();
        assert!(swapped.is_ok(), "replaced directory not noticed");
        assert!(changed.is_ok(), "change in the new directory not noticed");
    }

    #[test]
    #[cfg(unix)]
    fn follows_a_symlink_retargeted_to_another_directory() {
        let base =
            std::env::temp_dir().join(format!("nuntio-watch-retarget-{}", std::process::id()));
        let config_dir = base.join("config");
        let (a, b) = (base.join("dots/a"), base.join("dots/b"));
        for dir in [&config_dir, &a, &b] {
            std::fs::create_dir_all(dir).unwrap();
        }
        std::fs::write(a.join("nuntio.toml"), "").unwrap();
        std::fs::write(b.join("nuntio.toml"), "").unwrap();
        let link = config_dir.join("config.toml");
        std::os::unix::fs::symlink(a.join("nuntio.toml"), &link).unwrap();

        let (tx, rx) = mpsc::channel();
        let _watcher = ConfigWatcher::new(&link, None, move || {
            let _ = tx.send(());
        })
        .unwrap();

        let tmp = config_dir.join("tmp");
        std::os::unix::fs::symlink(b.join("nuntio.toml"), &tmp).unwrap();
        std::fs::rename(&tmp, &link).unwrap();
        let retargeted = rx.recv_timeout(Duration::from_secs(3));
        std::thread::sleep(Duration::from_millis(500));
        while rx.try_recv().is_ok() {}
        std::fs::write(b.join("nuntio.toml"), "scrollback = 5").unwrap();
        let changed = rx.recv_timeout(Duration::from_secs(3));
        std::fs::remove_dir_all(&base).unwrap();
        assert!(retargeted.is_ok(), "retargeted symlink not noticed");
        assert!(changed.is_ok(), "change in the new target not noticed");
    }

    #[test]
    fn notices_a_themes_directory_created_later() {
        let base = std::env::temp_dir().join(format!("nuntio-watch-themes-{}", std::process::id()));
        std::fs::create_dir_all(&base).unwrap();
        let path = base.join("config.toml");
        std::fs::write(&path, "").unwrap();
        let themes = base.join("themes");

        let (tx, rx) = mpsc::channel();
        let _watcher = ConfigWatcher::new(&path, Some(&themes), move || {
            let _ = tx.send(());
        })
        .unwrap();

        std::fs::create_dir_all(&themes).unwrap();
        std::thread::sleep(Duration::from_millis(500));
        while rx.try_recv().is_ok() {}
        std::fs::write(themes.join("x.toml"), "").unwrap();
        let changed = rx.recv_timeout(Duration::from_secs(3));
        std::fs::remove_dir_all(&base).unwrap();
        assert!(changed.is_ok(), "theme in a new directory not noticed");
    }

    #[test]
    // Windows keeps a watched directory until the watch ends, so the test
    // can't delete and recreate it.
    #[cfg(unix)]
    fn notices_a_themes_directory_deleted_and_created_again() {
        let base =
            std::env::temp_dir().join(format!("nuntio-watch-themes-again-{}", std::process::id()));
        let themes = base.join("themes");
        std::fs::create_dir_all(&themes).unwrap();
        let path = base.join("config.toml");
        std::fs::write(&path, "").unwrap();
        std::fs::write(themes.join("x.toml"), "").unwrap();

        let (tx, rx) = mpsc::channel();
        let _watcher = ConfigWatcher::new(&path, Some(&themes), move || {
            let _ = tx.send(());
        })
        .unwrap();

        std::fs::remove_dir_all(&themes).unwrap();
        let deleted = rx.recv_timeout(Duration::from_secs(3));
        std::thread::sleep(Duration::from_millis(500));
        while rx.try_recv().is_ok() {}
        std::fs::create_dir_all(&themes).unwrap();
        std::thread::sleep(Duration::from_millis(500));
        while rx.try_recv().is_ok() {}
        std::fs::write(themes.join("x.toml"), "").unwrap();
        let changed = rx.recv_timeout(Duration::from_secs(3));
        std::fs::remove_dir_all(&base).unwrap();
        assert!(
            deleted.is_ok(),
            "deleting the themes directory not reported"
        );
        assert!(
            changed.is_ok(),
            "theme in a recreated directory not noticed"
        );
    }

    fn event(kind: EventKind, paths: &[&Path]) -> Event {
        paths.iter().fold(Event::new(kind), |event, path| {
            event.add_path(path.to_path_buf())
        })
    }

    #[test]
    fn only_theme_files_count_in_the_themes_directory() {
        let themes = Path::new("/c/themes");
        let t = Targets::resolve(Path::new("/c/config.toml"), Some(themes));
        let changed = |kind, path: &str| classify(&event(kind, &[Path::new(path)]), &t).changed;
        let modify = EventKind::Modify(ModifyKind::Any);
        for editor_file in [
            "/c/themes/.x.toml.swp",
            "/c/themes/x.toml~",
            "/c/themes/4913",
            "/c/themes/#x.toml#",
            "/c/themes/.#x.toml",
            "/c/themes/.DS_Store",
            "/c/themes/sub/x.toml",
        ] {
            assert!(!changed(modify, editor_file), "{editor_file}");
        }
        assert!(changed(modify, "/c/themes/x.toml"));
        assert!(changed(modify, "/c/themes/y.itermcolors"));
        assert!(changed(
            EventKind::Remove(notify::event::RemoveKind::Folder),
            "/c/themes"
        ));
    }

    #[test]
    fn lost_events_mean_a_rescan_and_a_change() {
        let t = Targets::resolve(Path::new("/c/config.toml"), None);
        let lost = Event::new(EventKind::Other).set_flag(notify::event::Flag::Rescan);
        assert_eq!(
            classify(&lost, &t),
            Verdict {
                changed: true,
                rescan: true
            }
        );
        let plain = Event::new(EventKind::Other);
        assert_eq!(classify(&plain, &t), Verdict::default());
        let read = event(
            EventKind::Access(notify::event::AccessKind::Any),
            &[Path::new("/c/config.toml")],
        );
        assert_eq!(classify(&read, &t), Verdict::default());
    }

    #[test]
    fn ignores_editor_files_in_the_themes_directory() {
        let base =
            std::env::temp_dir().join(format!("nuntio-watch-swap-th-{}", std::process::id()));
        let themes = base.join("themes");
        std::fs::create_dir_all(&themes).unwrap();
        let path = base.join("config.toml");
        std::fs::write(&path, "").unwrap();

        let (tx, rx) = mpsc::channel();
        let _watcher = ConfigWatcher::new(&path, Some(&themes), move || {
            let _ = tx.send(());
        })
        .unwrap();
        std::thread::sleep(Duration::from_millis(500));
        while rx.try_recv().is_ok() {}

        std::fs::write(themes.join(".x.toml.swp"), "x").unwrap();
        std::fs::write(themes.join("x.toml~"), "x").unwrap();
        std::fs::write(themes.join("4913"), "x").unwrap();
        let quiet = rx.recv_timeout(Duration::from_millis(500)).is_err();
        std::fs::write(themes.join("x.toml"), "").unwrap();
        let changed = rx.recv_timeout(Duration::from_secs(3));
        std::fs::remove_dir_all(&base).unwrap();
        assert!(quiet, "editor files reported");
        assert!(changed.is_ok(), "theme file not reported");
    }

    #[test]
    #[cfg(unix)]
    fn keeps_watching_when_a_themes_directory_is_unreadable() {
        use std::os::unix::fs::PermissionsExt;
        let base =
            std::env::temp_dir().join(format!("nuntio-watch-unreadable-{}", std::process::id()));
        let themes = base.join("themes");
        std::fs::create_dir_all(&themes).unwrap();
        let path = base.join("config.toml");
        std::fs::write(&path, "").unwrap();
        std::fs::set_permissions(&themes, std::fs::Permissions::from_mode(0o000)).unwrap();

        let (tx, rx) = mpsc::channel();
        let watcher = ConfigWatcher::new(&path, Some(&themes), move || {
            let _ = tx.send(());
        });
        let created = watcher.is_ok();
        std::thread::sleep(Duration::from_millis(500));
        while rx.try_recv().is_ok() {}
        std::fs::write(&path, "scrollback = 5").unwrap();
        let changed = rx.recv_timeout(Duration::from_secs(3));
        std::fs::set_permissions(&themes, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::remove_dir_all(&base).unwrap();
        assert!(created, "one failing directory disabled the watcher");
        assert!(changed.is_ok(), "config change not reported");
    }

    #[test]
    #[cfg(unix)]
    fn follows_a_symlinked_directory_retargeted() {
        let base =
            std::env::temp_dir().join(format!("nuntio-watch-dirlink-{}", std::process::id()));
        let cfg = base.join("cfg");
        let (a, b) = (base.join("dots/a/nuntio"), base.join("dots/b/nuntio"));
        for dir in [&cfg, &a, &b] {
            std::fs::create_dir_all(dir).unwrap();
        }
        std::fs::write(a.join("config.toml"), "").unwrap();
        std::fs::write(b.join("config.toml"), "").unwrap();
        let link = cfg.join("nuntio");
        std::os::unix::fs::symlink(&a, &link).unwrap();
        let path = link.join("config.toml");

        // The directory holding the link is watched too.
        assert!(Targets::resolve(&path, None).wanted.contains(&cfg));

        let (tx, rx) = mpsc::channel();
        let _watcher = ConfigWatcher::new(&path, None, move || {
            let _ = tx.send(());
        })
        .unwrap();

        let tmp = cfg.join("tmp");
        std::os::unix::fs::symlink(&b, &tmp).unwrap();
        std::fs::rename(&tmp, &link).unwrap();
        let retargeted = rx.recv_timeout(Duration::from_secs(3));
        std::thread::sleep(Duration::from_millis(500));
        while rx.try_recv().is_ok() {}
        std::fs::write(b.join("config.toml"), "scrollback = 5").unwrap();
        let changed = rx.recv_timeout(Duration::from_secs(3));
        std::fs::remove_dir_all(&base).unwrap();
        assert!(retargeted.is_ok(), "retargeted directory link not noticed");
        assert!(changed.is_ok(), "change in the new target not noticed");
    }

    #[test]
    fn needed_dirs_prefers_the_directory_itself() {
        let base = std::env::temp_dir().join(format!("nuntio-watch-needed-{}", std::process::id()));
        let sub = base.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        let missing = base.join("missing/deeper");
        let needed = needed_dirs(&[sub.join("nope"), sub.clone(), missing, sub.clone()]);
        std::fs::remove_dir_all(&base).unwrap();
        assert_eq!(needed, vec![base, sub]);
    }

    #[test]
    fn drops_the_stand_in_once_the_directory_exists() {
        let base = std::env::temp_dir().join(format!("nuntio-watch-stand-{}", std::process::id()));
        std::fs::create_dir_all(&base).unwrap();
        let dir = base.join("later");
        let mut watcher = notify::recommended_watcher(|_: notify::Result<Event>| {}).unwrap();
        let mut watched = Vec::new();
        let wanted = [dir.clone()];

        let first = watch_existing(&mut watcher, &wanted, &mut watched);
        assert!(first.added);
        assert_eq!(watched, vec![base.clone()]);

        std::fs::create_dir_all(&dir).unwrap();
        let second = watch_existing(&mut watcher, &wanted, &mut watched);
        std::fs::remove_dir_all(&base).unwrap();
        assert!(second.added);
        assert!(second.failure.is_none());
        assert_eq!(watched, vec![dir]);
    }
}
