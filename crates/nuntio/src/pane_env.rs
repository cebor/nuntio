//! Environment for the shells in nuntio's panes: the config file in use,
//! and `nuntio-config` on the PATH. The helper isn't installed on the
//! global PATH, so the command only exists inside nuntio.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// Tells `nuntio-config` which file to edit, e.g. with `--config`.
pub const CONFIG_ENV: &str = "NUNTIO_CONFIG";

fn helper_name() -> String {
    format!("nuntio-config{}", std::env::consts::EXE_SUFFIX)
}

/// Where `nuntio-config` is installed relative to nuntio's executable:
/// next to it (development builds, macOS app bundle, Windows zip) or in
/// `../lib/nuntio/` (Linux packages, so that it stays off the PATH).
fn helper_dir(exe: &Path, exists: impl Fn(&Path) -> bool) -> Option<PathBuf> {
    let exe_dir = exe.parent()?;
    [exe_dir.to_owned(), exe_dir.join("../lib/nuntio")]
        .into_iter()
        .find(|dir| exists(&dir.join(helper_name())))
}

/// `dir` in front of `existing`.
fn prepend_path(dir: &Path, existing: Option<OsString>) -> Option<String> {
    let rest = existing.map(|p| std::env::split_paths(&p).collect::<Vec<_>>());
    // An empty entry would mean the current directory.
    let rest = rest
        .into_iter()
        .flatten()
        .filter(|p| !p.as_os_str().is_empty());
    let paths = std::iter::once(dir.to_owned()).chain(rest);
    std::env::join_paths(paths).ok()?.into_string().ok()
}

/// Variables to add for new panes. Shells in WSL get none: Windows paths
/// mean nothing there.
pub fn pane_env(config_path: Option<&Path>, wsl: bool) -> Vec<(String, String)> {
    if wsl {
        return Vec::new();
    }
    let mut env = Vec::new();
    if let Some(path) = config_path.and_then(Path::to_str) {
        env.push((CONFIG_ENV.to_owned(), path.to_owned()));
    }
    let dir = std::env::current_exe()
        .ok()
        .and_then(|exe| helper_dir(&exe, Path::is_file));
    match dir {
        Some(dir) => {
            let dir = dir.canonicalize().unwrap_or(dir);
            if let Some(path) = prepend_path(&dir, std::env::var_os("PATH")) {
                env.push(("PATH".to_owned(), path));
            }
        }
        None => tracing::debug!("nuntio-config not found next to the executable"),
    }
    env
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_helper_next_to_the_executable_or_in_lib() {
        let exe = Path::new("/opt/nuntio/bin/nuntio");
        let next_to = Path::new("/opt/nuntio/bin").join(helper_name());
        let in_lib = Path::new("/opt/nuntio/bin/../lib/nuntio").join(helper_name());
        assert_eq!(
            helper_dir(exe, |p| p == next_to),
            Some(PathBuf::from("/opt/nuntio/bin"))
        );
        assert_eq!(
            helper_dir(exe, |p| p == in_lib),
            Some(PathBuf::from("/opt/nuntio/bin/../lib/nuntio"))
        );
        assert_eq!(helper_dir(exe, |_| false), None);
    }

    #[test]
    fn prepends_to_the_path() {
        let dir = Path::new("/usr/lib/nuntio");
        let sep = if cfg!(windows) { ";" } else { ":" };
        let existing = std::env::join_paths(["/usr/bin", "/bin"]).unwrap();
        assert_eq!(
            prepend_path(dir, Some(existing)).unwrap(),
            format!("/usr/lib/nuntio{sep}/usr/bin{sep}/bin")
        );
        assert_eq!(prepend_path(dir, None).unwrap(), "/usr/lib/nuntio");
        assert_eq!(
            prepend_path(dir, Some("".into())).unwrap(),
            "/usr/lib/nuntio"
        );
    }

    #[test]
    fn nothing_for_wsl_shells() {
        assert!(pane_env(Some(Path::new("/c.toml")), true).is_empty());
        let env = pane_env(Some(Path::new("/c.toml")), false);
        assert!(env.contains(&(CONFIG_ENV.to_owned(), "/c.toml".to_owned())));
    }
}
