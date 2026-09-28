//! Paths for shells in WSL, which nuntio on Windows starts through
//! `wsl.exe` and whose processes it can't see.

use std::collections::HashMap;

use nuntio_term::ReportedDir;

use crate::tab_title::strip_user_host;

/// Where WSL sees a Windows path: `C:\x` is `/mnt/c/x`, and a file in a
/// distribution (`\\wsl.localhost\Ubuntu\home\x`) is `/home/x`.
pub fn wsl_path(path: &str) -> String {
    let path = path.strip_prefix(r"\\?\").unwrap_or(path);
    let slashed = |rest: &str| rest.replace('\\', "/");
    let mut chars = path.chars();
    if let (Some(drive), Some(':')) = (chars.next(), chars.next())
        && drive.is_ascii_alphabetic()
    {
        let rest = slashed(chars.as_str());
        return format!("/mnt/{}{rest}", drive.to_ascii_lowercase());
    }
    for host in [r"\\wsl.localhost\", r"\\wsl$\"] {
        if let Some(rest) = path
            .get(..host.len())
            .filter(|prefix| prefix.eq_ignore_ascii_case(host))
            .map(|_| &path[host.len()..])
        {
            // Skip the distribution's name.
            let inside = rest.find('\\').map_or("", |i| &rest[i..]);
            return if inside.is_empty() {
                "/".into()
            } else {
                slashed(inside)
            };
        }
    }
    slashed(path)
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
/// `wsl.exe` takes a moment, so each is asked once.
#[derive(Default)]
pub struct Homes(HashMap<(String, Option<String>), Option<String>>);

impl Homes {
    pub fn get(&mut self, distro: &str, user: Option<&str>) -> Option<String> {
        self.0
            .entry((distro.to_owned(), user.map(str::to_owned)))
            .or_insert_with(|| query_home(distro, user))
            .clone()
    }
}

#[cfg(windows)]
fn query_home(distro: &str, user: Option<&str>) -> Option<String> {
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};

    // No console window flashing up.
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let mut command = Command::new("wsl.exe");
    command.args(["-d", distro]);
    if let Some(user) = user {
        command.args(["-u", user]);
    }
    let output = command
        .args(["--exec", "sh", "-c", r#"printf %s "$HOME""#])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .inspect_err(|err| tracing::warn!("failed to run wsl.exe: {err}"))
        .ok()?;
    let home = String::from_utf8(output.stdout).ok()?;
    let found = output.status.success() && home.starts_with('/');
    if !found {
        tracing::warn!(distro, "found no home directory in WSL");
    }
    found.then_some(home)
}

#[cfg(not(windows))]
fn query_home(_distro: &str, _user: Option<&str>) -> Option<String> {
    None
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
}
