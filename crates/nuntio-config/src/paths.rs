//! `~` in paths: expanding it in configured directories and shortening the
//! home directory for display; and nuntio's cache directory.

use std::path::{Path, PathBuf};

/// nuntio's own folder in the user's cache directory (log, update state and
/// lock); it is not created here.
pub fn cache_dir() -> Option<PathBuf> {
    Some(dirs::cache_dir()?.join("nuntio"))
}

/// Whether `dir` starts at the home directory: exactly `~`, or `~`
/// followed by `/` or `\`. Not `~name`: nuntio has no other users' homes.
pub fn is_home_relative(dir: &str) -> bool {
    dir.strip_prefix('~')
        .is_some_and(|rest| rest.is_empty() || rest.starts_with(['/', '\\']))
}

/// `dir` as a path: a directory that [`is_home_relative`] starts at `home`,
/// anything else (`~name` included) is taken as it is. `None` if `dir`
/// starts at home and there is no home.
pub fn expand_home(dir: &str, home: Option<&Path>) -> Option<PathBuf> {
    match dir.strip_prefix('~').filter(|_| is_home_relative(dir)) {
        Some(rest) => Some(home?.join(rest.trim_start_matches(['/', '\\']))),
        None => Some(PathBuf::from(dir)),
    }
}

/// `path` with the home directory shortened to `~`. A root as home stays
/// unshortened: `~` would go in front of every path.
pub fn tilde_path(path: &Path, home: Option<&Path>) -> String {
    let home = home.filter(|home| home.parent().is_some());
    match home.and_then(|home| path.strip_prefix(home).ok()) {
        Some(rest) if rest.as_os_str().is_empty() => "~".to_owned(),
        Some(rest) => format!("~{}{}", std::path::MAIN_SEPARATOR, rest.display()),
        None => path.display().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `~` and `rest` joined as `tilde_path` does on this platform.
    fn tilde(rest: &str) -> String {
        format!("~{}{rest}", std::path::MAIN_SEPARATOR)
    }

    #[test]
    fn home_is_expanded_in_every_spelling() {
        let home = Some(Path::new("/h"));
        assert_eq!(expand_home("~", home), Some(PathBuf::from("/h")));
        assert_eq!(expand_home("~/n", home), Some(PathBuf::from("/h/n")));
        assert_eq!(expand_home("~\\n", home), Some(PathBuf::from("/h/n")));
        assert_eq!(expand_home("/abs/n", home), Some(PathBuf::from("/abs/n")));
        assert_eq!(expand_home("/abs/n", None), Some(PathBuf::from("/abs/n")));
        assert_eq!(expand_home("~/n", None), None);
        // Another user's home is not supported: `~me` is no home path.
        assert_eq!(expand_home("~me", home), Some(PathBuf::from("~me")));
        assert_eq!(expand_home("~me", None), Some(PathBuf::from("~me")));
        for (dir, relative) in [
            ("~", true),
            ("~/n", true),
            ("~\\n", true),
            ("~me", false),
            ("n/~", false),
            ("", false),
        ] {
            assert_eq!(is_home_relative(dir), relative, "{dir}");
        }
    }

    #[test]
    fn home_becomes_tilde() {
        let home = Some(Path::new("/home/felix"));
        assert_eq!(tilde_path(Path::new("/home/felix"), home), "~");
        assert_eq!(
            tilde_path(Path::new("/home/felix/code/nuntio"), home),
            tilde("code/nuntio")
        );
        assert_eq!(tilde_path(Path::new("/home/felixx"), home), "/home/felixx");
        assert_eq!(tilde_path(Path::new("/etc"), None), "/etc");
        // A root as home would put `~` in front of every path.
        assert_eq!(tilde_path(Path::new("/etc"), Some(Path::new("/"))), "/etc");
        assert_eq!(tilde_path(Path::new("/"), Some(Path::new("/"))), "/");
    }

    #[test]
    #[cfg(windows)]
    fn home_becomes_tilde_on_windows() {
        assert_eq!(
            tilde_path(
                Path::new(r"C:\Users\f\code"),
                Some(Path::new(r"C:\Users\f"))
            ),
            "~\\code"
        );
    }
}
