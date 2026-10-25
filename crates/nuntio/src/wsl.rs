//! Paths for shells in WSL, which nuntio on Windows starts through
//! `wsl.exe` and whose processes it can't see.

use std::collections::HashMap;
#[cfg(windows)]
use std::process::Command;
#[cfg(windows)]
use std::time::Duration;

use nuntio_term::ReportedDir;

use crate::tab_title::strip_user_host;

/// Whether this is a Linux build running inside WSL, where the opener hands
/// `file:///C:/…` to Windows. Same test as the `open` crate's: `WSL_DISTRO_NAME` or a
/// Microsoft kernel.
pub fn running_in_wsl() -> bool {
    static WSL: std::sync::LazyLock<bool> = std::sync::LazyLock::new(|| {
        cfg!(target_os = "linux")
            && (std::env::var_os("WSL_DISTRO_NAME").is_some()
                || ["/proc/sys/kernel/osrelease", "/proc/version"]
                    .iter()
                    .any(|p| {
                        std::fs::read_to_string(p)
                            .is_ok_and(|s| s.to_lowercase().contains("microsoft"))
                    }))
    });
    *WSL
}

/// A path on a WSL share (`\\wsl.localhost\Ubuntu\home\x`, also `\\wsl$\`):
/// the distribution (`Ubuntu`) and the path inside it (`\home\x`, empty for
/// the root). `None` for any other path.
fn wsl_share(path: &str) -> Option<(&str, &str)> {
    [r"\\wsl.localhost\", r"\\wsl$\"]
        .into_iter()
        .find_map(|host| {
            let rest = path
                .get(..host.len())
                .filter(|prefix| prefix.eq_ignore_ascii_case(host))
                .map(|_| &path[host.len()..])?;
            Some(rest.split_at(rest.find('\\').unwrap_or(rest.len())))
        })
}

/// Where WSL sees the file `rest` (backslashes allowed) on the Windows
/// drive `letter`: `/mnt/c/x`.
pub fn drive_path(letter: char, rest: &str) -> String {
    format!(
        "/mnt/{}{}",
        letter.to_ascii_lowercase(),
        rest.replace('\\', "/")
    )
}

/// Where WSL sees a Windows path: `C:\x` is `/mnt/c/x`, and a file in a
/// distribution (`\\wsl.localhost\Ubuntu\home\x`) is `/home/x`.
pub fn wsl_path(path: &str) -> String {
    let path = path.strip_prefix(r"\\?\").unwrap_or(path);
    let slashed = |rest: &str| rest.replace('\\', "/");
    let mut chars = path.chars();
    if let (Some(drive), Some(':')) = (chars.next(), chars.next())
        && drive.is_ascii_alphabetic()
    {
        return drive_path(drive, chars.as_str());
    }
    if let Some((_, inside)) = wsl_share(path) {
        return if inside.is_empty() {
            "/".into()
        } else {
            slashed(inside)
        };
    }
    slashed(path)
}

/// A WSL distribution and the user a shell runs as in it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Distro {
    pub name: String,
    pub user: Option<String>,
}

/// Where Windows sees `dir` (absolute) of a WSL distribution, if it is on a
/// Windows drive: `/mnt/c/x` is `C:\x`. Directories inside the distribution
/// (`\\wsl.localhost\…`) give `None`: a Windows shell is no use there.
pub fn windows_path(dir: &str) -> Option<String> {
    let rest = dir.strip_prefix("/mnt/")?;
    let (drive, rest) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
    let mut chars = drive.chars();
    let (Some(letter), None) = (chars.next(), chars.next()) else {
        return None;
    };
    if !letter.is_ascii_alphabetic() {
        return None;
    }
    let rest = rest.strip_prefix('/').unwrap_or(rest);
    Some(format!(
        "{}:\\{}",
        letter.to_ascii_uppercase(),
        rest.replace('/', "\\")
    ))
}

/// A Windows directory as a shell in `distro` sees it (see `wsl_path`);
/// `None` inside another distribution or on a network share.
pub fn linux_path_in(path: &str, distro: &str) -> Option<String> {
    let path = path.strip_prefix(r"\\?\").unwrap_or(path);
    match wsl_share(path) {
        Some((name, _)) => name.eq_ignore_ascii_case(distro).then(|| wsl_path(path)),
        None => (!path.starts_with(r"\\")).then(|| wsl_path(path)),
    }
}

/// The directory of a shell in WSL, as `/abs/path`, `~` or `~/path`: the
/// one it reported (OSC 7, OSC 9;9), otherwise the one its title shows,
/// as in the default prompt of Ubuntu and Debian (`user@host: ~/code`).
pub fn linux_dir(reported: Option<ReportedDir>, title: Option<&str>) -> Option<String> {
    match reported {
        Some(ReportedDir::Posix(path)) => return Some(path),
        Some(ReportedDir::Windows(path)) => return Some(wsl_path(&path)),
        None => {}
    }
    let dir = strip_user_host(title?);
    let dir = dir.trim();
    let is_dir = dir == "~" || dir.starts_with("~/") || dir.starts_with('/');
    is_dir.then(|| dir.to_owned())
}

/// The argument for `wsl.exe --cd`, which takes only `~` and absolute
/// paths: `~/x` needs the home directory.
pub fn cd_arg(dir: &str, home: impl FnOnce() -> Option<String>) -> Option<String> {
    if dir == "~" || dir.starts_with('/') {
        return Some(dir.to_owned());
    }
    let rest = dir.strip_prefix("~/")?;
    let home = home()?;
    Some(format!("{}/{rest}", home.trim_end_matches('/')))
}

/// Home directories in WSL distributions, by distribution and user. Asking
/// `wsl.exe` takes a moment, so an answer is kept; a failure is asked again
/// next time, as `wsl.exe` may just have been slow or the distribution
/// not started.
#[derive(Default)]
pub struct Homes(HashMap<Distro, String>);

impl Homes {
    pub fn get(&mut self, distro: &Distro) -> Option<String> {
        self.get_with(distro, query_home)
    }

    fn get_with(
        &mut self,
        distro: &Distro,
        query: impl FnOnce(&Distro) -> Option<String>,
    ) -> Option<String> {
        if let Some(home) = self.0.get(distro) {
            return Some(home.clone());
        }
        let home = query(distro)?;
        self.0.insert(distro.clone(), home.clone());
        Some(home)
    }
}

/// How long `wsl.exe` may take to name a home directory.
#[cfg(windows)]
const HOME_LOOKUP_TIMEOUT: Duration = Duration::from_secs(3);

#[cfg(windows)]
fn query_home(distro: &Distro) -> Option<String> {
    let mut command = Command::new("wsl.exe");
    command.args(["-d", &distro.name]);
    if let Some(user) = &distro.user {
        command.args(["-u", user]);
    }
    command.args(["--exec", "sh", "-c", r#"printf %s "$HOME""#]);
    let output = match crate::command::output_within(
        crate::command::no_window(&mut command),
        HOME_LOOKUP_TIMEOUT,
    ) {
        Ok(Some(output)) => output,
        Ok(None) => {
            tracing::warn!(distro = distro.name, "wsl.exe did not answer in time");
            return None;
        }
        Err(err) => {
            tracing::warn!("failed to run wsl.exe: {err}");
            return None;
        }
    };
    let home = String::from_utf8(output.stdout).ok()?;
    let found = output.status.success() && home.starts_with('/');
    if !found {
        tracing::warn!(distro = distro.name, "found no home directory in WSL");
    }
    found.then_some(home)
}

#[cfg(not(windows))]
fn query_home(_distro: &Distro) -> Option<String> {
    None
}

/// nuntio-wsl, the Linux helper that gives WSL panes their own Linux PTY,
/// if it sits next to the executable and WSL can run it. Not canonicalized:
/// `wslpath` can't read `\\?\` paths.
#[cfg(windows)]
pub fn helper() -> Option<std::path::PathBuf> {
    if !helper_runs_natively() {
        tracing::debug!(
            "nuntio-wsl is built for x86_64, but this machine isn't one: WSL panes use ConPTY"
        );
        return None;
    }
    let helper = std::env::current_exe().ok()?.parent()?.join("nuntio-wsl");
    helper.is_file().then_some(helper)
}

/// Whether WSL's Linux can run the helper, which is built for nuntio's
/// architecture: x86_64 nuntio also runs emulated on Windows on Arm, whose
/// WSL is aarch64.
#[cfg(all(windows, target_arch = "x86_64"))]
fn helper_runs_natively() -> bool {
    use windows_sys::Win32::System::SystemInformation::IMAGE_FILE_MACHINE_AMD64;
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, IsWow64Process2};
    let (mut process, mut native) = (0, 0);
    // SAFETY: this process's pseudo handle and valid out-pointers.
    let ok = unsafe { IsWow64Process2(GetCurrentProcess(), &mut process, &mut native) } != 0;
    !ok || native == IMAGE_FILE_MACHINE_AMD64
}

#[cfg(all(windows, not(target_arch = "x86_64")))]
fn helper_runs_natively() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_paths_are_translated() {
        assert_eq!(wsl_path(r"C:\Users\me"), "/mnt/c/Users/me");
        assert_eq!(wsl_path(r"D:\"), "/mnt/d/");
        assert_eq!(wsl_path(r"\\wsl.localhost\Ubuntu\home\me\x"), "/home/me/x");
        assert_eq!(wsl_path(r"\\wsl$\Debian\etc"), "/etc");
        assert_eq!(wsl_path(r"\\wsl$\Debian"), "/");
        assert_eq!(wsl_path(r"\\?\C:\a"), "/mnt/c/a");
    }

    #[test]
    fn only_drive_paths_are_seen_from_windows() {
        assert_eq!(windows_path("/mnt/c/Users/me"), Some(r"C:\Users\me".into()));
        assert_eq!(windows_path("/mnt/c"), Some(r"C:\".into()));
        assert_eq!(windows_path("/home/me"), None);
        assert_eq!(windows_path("~/code"), None);
        assert_eq!(windows_path("/"), None);
        assert_eq!(windows_path("/mnt/wsl/x"), None);
    }

    #[test]
    fn windows_paths_are_seen_from_one_distribution() {
        assert_eq!(linux_path_in(r"C:\x", "Ubuntu"), Some("/mnt/c/x".into()));
        assert_eq!(
            linux_path_in(r"\\wsl.localhost\Ubuntu\home\me", "ubuntu"),
            Some("/home/me".into())
        );
        assert_eq!(
            linux_path_in(r"\\wsl.localhost\Ubuntu\home\me", "Debian"),
            None
        );
        assert_eq!(
            linux_path_in(r"\\wsl$\Ubuntu\x", "Ubuntu"),
            Some("/x".into())
        );
        assert_eq!(linux_path_in(r"\\server\share", "Ubuntu"), None);
        assert_eq!(
            linux_path_in(r"\\?\C:\x", "Ubuntu"),
            Some("/mnt/c/x".into())
        );
    }

    #[test]
    fn a_reported_directory_comes_before_the_title() {
        let title = Some("me@box: ~/code");
        assert_eq!(
            linux_dir(Some(ReportedDir::Posix("/srv".into())), title),
            Some("/srv".into())
        );
        assert_eq!(
            linux_dir(
                Some(ReportedDir::Windows(r"\\wsl.localhost\Ubuntu\srv".into())),
                title
            ),
            Some("/srv".into())
        );
        assert_eq!(linux_dir(None, title), Some("~/code".into()));
    }

    #[test]
    fn titles_that_show_no_directory_are_ignored() {
        assert_eq!(linux_dir(None, Some("me@box: /etc")), Some("/etc".into()));
        assert_eq!(linux_dir(None, Some("me@box:~")), Some("~".into()));
        assert_eq!(linux_dir(None, Some("/tmp")), Some("/tmp".into()));
        assert_eq!(linux_dir(None, Some("vim notes.txt")), None);
        assert_eq!(linux_dir(None, Some("~foo")), None);
        assert_eq!(linux_dir(None, None), None);
    }

    #[test]
    fn wsl_cd_takes_absolute_paths() {
        let home = || Some("/home/me/".to_owned());
        assert_eq!(cd_arg("~", || unreachable!()), Some("~".into()));
        assert_eq!(cd_arg("/etc", || unreachable!()), Some("/etc".into()));
        assert_eq!(cd_arg("~/code", home), Some("/home/me/code".into()));
        assert_eq!(cd_arg("~/code", || None), None);
        assert_eq!(cd_arg("code", home), None);
    }

    #[test]
    fn failed_home_lookups_are_not_cached() {
        let mut homes = Homes::default();
        let mut asked = 0;
        let ubuntu = Distro {
            name: "Ubuntu".into(),
            user: None,
        };
        let mut ask = |answer: Option<&str>| {
            homes.get_with(&ubuntu, |_| {
                asked += 1;
                answer.map(str::to_owned)
            })
        };
        assert_eq!(ask(None), None);
        assert_eq!(ask(Some("/home/me")), Some("/home/me".into()));
        assert_eq!(ask(None), Some("/home/me".into()), "cached");
        assert_eq!(asked, 2);

        let root = Distro {
            name: "Ubuntu".into(),
            user: Some("root".into()),
        };
        let other = homes.get_with(&root, |distro| {
            assert_eq!(distro, &root);
            Some("/root".into())
        });
        assert_eq!(other, Some("/root".into()), "another user asks again");
    }
}
