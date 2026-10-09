//! Paths for shells in WSL, which nuntio on Windows starts through
//! `wsl.exe` and whose processes it can't see.

use std::collections::HashMap;
use std::io::{self, Read};
use std::process::{Command, Output, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

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

/// A WSL distribution and the user a shell runs as in it.
#[derive(Debug, Clone, PartialEq, Eq)]
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
    for host in [r"\\wsl.localhost\", r"\\wsl$\"] {
        if let Some(rest) = path
            .get(..host.len())
            .filter(|prefix| prefix.eq_ignore_ascii_case(host))
            .map(|_| &path[host.len()..])
        {
            let name = rest.split('\\').next().unwrap_or(rest);
            return name.eq_ignore_ascii_case(distro).then(|| wsl_path(path));
        }
    }
    (!path.starts_with(r"\\")).then(|| wsl_path(path))
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
pub struct Homes(HashMap<(String, Option<String>), String>);

impl Homes {
    pub fn get(&mut self, distro: &str, user: Option<&str>) -> Option<String> {
        self.get_with(distro, user, query_home)
    }

    fn get_with(
        &mut self,
        distro: &str,
        user: Option<&str>,
        query: impl FnOnce(&str, Option<&str>) -> Option<String>,
    ) -> Option<String> {
        let key = (distro.to_owned(), user.map(str::to_owned));
        if let Some(home) = self.0.get(&key) {
            return Some(home.clone());
        }
        let home = query(distro, user)?;
        self.0.insert(key, home.clone());
        Some(home)
    }
}

/// How long `wsl.exe` may take to name a home directory.
#[cfg(windows)]
const HOME_LOOKUP_TIMEOUT: Duration = Duration::from_secs(3);

/// `Command::output`, but a command that runs longer than `limit` is
/// killed and gives `None`. Stderr isn't captured.
#[cfg_attr(not(windows), allow(dead_code))]
fn output_within(command: &mut Command, limit: Duration) -> io::Result<Option<Output>> {
    let mut child = command.stdout(Stdio::piped()).spawn()?;
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("no stdout"))?;
    // Read on the side, so that a full pipe doesn't stall the command.
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stdout.read_to_end(&mut bytes);
        let _ = sender.send(bytes);
    });
    let deadline = Instant::now() + limit;
    loop {
        if let Some(status) = child.try_wait()? {
            let stdout = receiver
                .recv_timeout(Duration::from_secs(1))
                .unwrap_or_default();
            return Ok(Some(Output {
                status,
                stdout,
                stderr: Vec::new(),
            }));
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Ok(None);
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[cfg(windows)]
fn query_home(distro: &str, user: Option<&str>) -> Option<String> {
    use std::os::windows::process::CommandExt;

    // No console window flashing up.
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let mut command = Command::new("wsl.exe");
    command.args(["-d", distro]);
    if let Some(user) = user {
        command.args(["-u", user]);
    }
    command
        .args(["--exec", "sh", "-c", r#"printf %s "$HOME""#])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW);
    let output = match output_within(&mut command, HOME_LOOKUP_TIMEOUT) {
        Ok(Some(output)) => output,
        Ok(None) => {
            tracing::warn!(distro, "wsl.exe did not answer in time");
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
        tracing::warn!(distro, "found no home directory in WSL");
    }
    found.then_some(home)
}

#[cfg(not(windows))]
fn query_home(_distro: &str, _user: Option<&str>) -> Option<String> {
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
        let mut ask = |answer: Option<&str>| {
            homes.get_with("Ubuntu", None, |_, _| {
                asked += 1;
                answer.map(str::to_owned)
            })
        };
        assert_eq!(ask(None), None);
        assert_eq!(ask(Some("/home/me")), Some("/home/me".into()));
        assert_eq!(ask(None), Some("/home/me".into()), "cached");
        assert_eq!(asked, 2);

        let other = homes.get_with("Ubuntu", Some("root"), |_, user| {
            assert_eq!(user, Some("root"));
            Some("/root".into())
        });
        assert_eq!(other, Some("/root".into()), "another user asks again");
    }

    #[cfg(unix)]
    #[test]
    fn slow_commands_are_killed() {
        use std::process::Command;

        let started = Instant::now();
        let slow = output_within(
            Command::new("sh").args(["-c", "sleep 5"]),
            Duration::from_millis(100),
        )
        .unwrap();
        assert!(slow.is_none());
        assert!(started.elapsed() < Duration::from_secs(2));

        let quick = output_within(
            Command::new("sh").args(["-c", "printf %s hi"]),
            Duration::from_secs(5),
        )
        .unwrap()
        .unwrap();
        assert_eq!(quick.stdout, b"hi");
    }
}
