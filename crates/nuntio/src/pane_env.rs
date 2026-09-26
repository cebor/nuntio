//! Environment for the shells in nuntio's panes: the config file in use,
//! and `nuntio-config` on the PATH. The helper isn't installed on the
//! global PATH, so the command only exists inside nuntio. On macOS also a
//! UTF-8 locale, which apps started from the Dock don't get.

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

/// The locale variable for a pane when none of `LANG`, `LC_ALL` and
/// `LC_CTYPE` is set: `LANG` for the system's language and region (a
/// BCP 47 tag like `de-DE`) if that locale is installed, else only UTF-8
/// characters (`LC_CTYPE=UTF-8`), so shells don't show `<00e4>` for "ä".
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn locale_env(
    inherited: bool,
    system: Option<&str>,
    installed: impl Fn(&str) -> bool,
) -> Option<(String, String)> {
    if inherited {
        return None;
    }
    let lang = system.map(|tag| {
        // `de-DE`, `zh-Hans-CN` or `en-US-u-mu-celsius`: language and region.
        let mut parts = tag.split(['-', '_']);
        let language = parts.next().unwrap_or_default();
        let region = parts.find(|p| p.len() == 2 && p.chars().all(|c| c.is_ascii_alphabetic()));
        match region {
            Some(region) => format!("{language}_{}.UTF-8", region.to_ascii_uppercase()),
            None => format!("{language}.UTF-8"),
        }
    });
    Some(match lang.filter(|lang| installed(lang)) {
        Some(lang) => ("LANG".to_owned(), lang),
        None => ("LC_CTYPE".to_owned(), "UTF-8".to_owned()),
    })
}

#[cfg(target_os = "macos")]
fn macos_locale() -> Option<(String, String)> {
    let inherited = ["LANG", "LC_ALL", "LC_CTYPE"]
        .iter()
        .any(|var| std::env::var_os(var).is_some_and(|v| !v.is_empty()));
    locale_env(inherited, sys_locale::get_locale().as_deref(), |lang| {
        Path::new("/usr/share/locale").join(lang).is_dir()
    })
}

/// Variables to add for new panes. Shells in WSL get none: Windows paths
/// mean nothing there.
pub fn pane_env(config_path: Option<&Path>, wsl: bool) -> Vec<(String, String)> {
    if wsl {
        return Vec::new();
    }
    let mut env = Vec::new();
    #[cfg(target_os = "macos")]
    env.extend(macos_locale());
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
    fn locale_from_the_system_language() {
        let installed = |lang: &str| ["de_DE.UTF-8", "zh_CN.UTF-8"].contains(&lang);
        let env = |system| locale_env(false, system, installed);
        let pair = |k: &str, v: &str| Some((k.to_owned(), v.to_owned()));
        assert_eq!(env(Some("de-DE")), pair("LANG", "de_DE.UTF-8"));
        assert_eq!(env(Some("zh-Hans-CN")), pair("LANG", "zh_CN.UTF-8"));
        // English with a German region isn't installed.
        assert_eq!(env(Some("en-DE")), pair("LC_CTYPE", "UTF-8"));
        assert_eq!(env(None), pair("LC_CTYPE", "UTF-8"));
        assert_eq!(locale_env(true, Some("de-DE"), installed), None);
    }

    #[test]
    fn nothing_for_wsl_shells() {
        assert!(pane_env(Some(Path::new("/c.toml")), true).is_empty());
        let env = pane_env(Some(Path::new("/c.toml")), false);
        assert!(env.contains(&(CONFIG_ENV.to_owned(), "/c.toml".to_owned())));
    }
}
