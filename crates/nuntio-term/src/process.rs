//! Information about the process running in a pane: the foreground job's
//! name and working directory (tab titles, and where new tabs start).
//!
//! Implemented via `/proc` on Linux and `proc_pidinfo` on macOS. It is not
//! implemented on other platforms, where nothing is found and callers fall
//! back to the shell name and the default directory.

use std::path::PathBuf;

/// The foreground process group of the shell's terminal, falling back to
/// the shell itself.
fn foreground_pid(shell_pid: u32) -> u32 {
    let Some(pgid) = sys::tpgid(shell_pid) else {
        return shell_pid;
    };
    // A dead or zombie leader (`ls | less`) is replaced by a member of its
    // group; an unreadable cwd (other user, setuid) doesn't make it dead.
    if sys::is_alive(pgid) {
        return pgid;
    }
    sys::group_member(pgid).unwrap_or(pgid)
}

/// What is running in the foreground of a pane.
pub(crate) struct Foreground {
    pub(crate) name: Option<String>,
    pub(crate) cwd: Option<PathBuf>,
}

/// Name and, if wanted, working directory of the foreground process, from
/// one lookup of its pid.
pub(crate) fn foreground(shell_pid: u32, want_cwd: bool) -> Foreground {
    let pid = foreground_pid(shell_pid);
    Foreground {
        name: sys::name(pid).filter(|s| !s.is_empty()),
        cwd: if want_cwd {
            sys::cwd(pid).or_else(|| sys::cwd(shell_pid))
        } else {
            None
        },
    }
}

/// Whether the shell itself is in the foreground, i.e. waiting at its
/// prompt rather than running a program.
pub(crate) fn foreground_is_shell(shell_pid: u32) -> Option<bool> {
    sys::tpgid(shell_pid).map(|pid| pid == shell_pid)
}

/// The foreground process group of `pid`'s controlling terminal.
pub(crate) fn foreground_group(pid: u32) -> Option<u32> {
    sys::tpgid(pid)
}

/// The shell that `login` (see `pane::login_command`) started as its
/// child, once it exists.
#[cfg(target_os = "macos")]
pub(crate) fn login_child(login_pid: u32) -> Option<u32> {
    sys::children(login_pid).into_iter().next()
}

#[cfg(target_os = "linux")]
mod sys {
    use std::path::PathBuf;

    /// `/proc/<pid>/stat`.
    fn stat(pid: u32) -> Option<String> {
        std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()
    }

    /// Field `n` of `/proc/<pid>/stat`, numbered as in proc(5): 3 is
    /// `state`, 5 `pgrp`, 8 `tpgid`. The command name in field 2 may contain
    /// spaces and parentheses, so fields are counted after its closing
    /// parenthesis.
    fn stat_field(stat: &str, n: usize) -> Option<&str> {
        stat[stat.rfind(')')? + 1..]
            .split_whitespace()
            .nth(n.checked_sub(3)?)
    }

    /// The terminal's foreground process group (field 8).
    fn parse_tpgid(stat: &str) -> Option<u32> {
        let tpgid: i64 = stat_field(stat, 8)?.parse().ok()?;
        u32::try_from(tpgid).ok().filter(|&pid| pid > 0)
    }

    /// The process state (field 3).
    fn parse_state(stat: &str) -> Option<char> {
        stat_field(stat, 3)?.chars().next()
    }

    /// The process group (field 5).
    fn parse_pgrp(stat: &str) -> Option<u32> {
        stat_field(stat, 5)?.parse().ok()
    }

    /// The foreground process group of `pid`'s terminal.
    pub fn tpgid(pid: u32) -> Option<u32> {
        parse_tpgid(&stat(pid)?)
    }

    pub fn name(pid: u32) -> Option<String> {
        let comm = std::fs::read_to_string(format!("/proc/{pid}/comm")).ok()?;
        Some(comm.trim_end().to_owned())
    }

    pub fn cwd(pid: u32) -> Option<PathBuf> {
        std::fs::read_link(format!("/proc/{pid}/cwd")).ok()
    }

    /// Whether `pid` exists and isn't a zombie.
    pub fn is_alive(pid: u32) -> bool {
        stat(pid)
            .and_then(|stat| parse_state(&stat))
            .is_some_and(|state| !matches!(state, 'Z' | 'X'))
    }

    /// The highest pid in process group `pgid`.
    pub fn group_member(pgid: u32) -> Option<u32> {
        std::fs::read_dir("/proc")
            .ok()?
            .filter_map(|entry| entry.ok()?.file_name().to_str()?.parse::<u32>().ok())
            .filter(|&pid| stat(pid).and_then(|stat| parse_pgrp(&stat)) == Some(pgid))
            .max()
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn tpgid_from_stat() {
            let stat = "1234 (zsh) S 1 1234 1234 34816 5678 4194304 0 0";
            assert_eq!(parse_tpgid(stat), Some(5678));
        }

        #[test]
        fn command_names_with_spaces_and_parens() {
            let stat = "42 (my (weird) cmd) R 1 42 42 34816 99 0";
            assert_eq!(parse_tpgid(stat), Some(99));
        }

        #[test]
        fn pgrp_from_stat() {
            let stat = "1234 (zsh) S 1 1234 1234 34816 5678 4194304 0 0";
            assert_eq!(parse_pgrp(stat), Some(1234));
            assert_eq!(parse_pgrp("42 (my (weird) cmd) R 1 77 42 0 -1"), Some(77));
        }

        #[test]
        fn no_controlling_terminal() {
            let stat = "42 (daemon) S 1 42 42 0 -1 0";
            assert_eq!(parse_tpgid(stat), None);
        }

        #[test]
        fn state_from_stat() {
            assert_eq!(parse_state("42 (my (weird) cmd) Z 1 42"), Some('Z'));
            assert_eq!(parse_state("1 (zsh) S 0"), Some('S'));
        }

        #[test]
        fn unreadable_cwd_is_not_a_dead_leader() {
            // Root's cwd is readable when running as root.
            if cwd(1).is_some() {
                return;
            }
            assert!(is_alive(1));
        }

        #[test]
        fn zombies_are_not_alive() {
            let mut child = std::process::Command::new("true").spawn().unwrap();
            let id = child.id();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
            loop {
                if stat(id).and_then(|stat| parse_state(&stat)) == Some('Z') {
                    break;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "never became a zombie"
                );
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            assert!(!is_alive(id));
            child.wait().unwrap();
            assert!(!is_alive(id));
        }
    }
}

#[cfg(target_os = "macos")]
mod sys {
    use std::ffi::{OsStr, c_int};
    use std::os::unix::ffi::OsStrExt;
    use std::path::PathBuf;

    /// Fill a `T` with `proc_pidinfo`'s `flavor` for `pid`.
    fn pidinfo<T>(pid: u32, flavor: c_int) -> Option<T> {
        let pid = c_int::try_from(pid).ok()?;
        let size = c_int::try_from(size_of::<T>()).ok()?;
        let mut info = std::mem::MaybeUninit::<T>::zeroed();
        // SAFETY: the buffer is a `T` of `size` bytes; the flavors used
        // here fill exactly that struct.
        let written = unsafe { libc::proc_pidinfo(pid, flavor, 0, info.as_mut_ptr().cast(), size) };
        // SAFETY: the kernel filled all `size` bytes, and the libc structs
        // are plain data for which any bytes are valid.
        (written == size).then(|| unsafe { info.assume_init() })
    }

    /// The foreground process group of `pid`'s terminal.
    pub fn tpgid(pid: u32) -> Option<u32> {
        let info: libc::proc_bsdinfo = pidinfo(pid, libc::PROC_PIDTBSDINFO)?;
        Some(info.e_tpgid).filter(|&pid| pid > 0)
    }

    pub fn name(pid: u32) -> Option<String> {
        let pid = c_int::try_from(pid).ok()?;
        let mut buffer = [0u8; 2 * libc::MAXCOMLEN + 1];
        // SAFETY: the pointer and size describe `buffer`.
        let len = unsafe { libc::proc_name(pid, buffer.as_mut_ptr().cast(), buffer.len() as u32) };
        let len = usize::try_from(len).ok().filter(|&len| len > 0)?;
        Some(String::from_utf8_lossy(&buffer[..len.min(buffer.len())]).into_owned())
    }

    /// The processes whose parent is `pid`.
    pub fn children(pid: u32) -> Vec<u32> {
        let Ok(pid) = c_int::try_from(pid) else {
            return Vec::new();
        };
        let mut pids = [0 as libc::pid_t; 16];
        let size = c_int::try_from(size_of_val(&pids)).expect("small buffer");
        // SAFETY: the pointer and size describe `pids`.
        let written = unsafe { libc::proc_listchildpids(pid, pids.as_mut_ptr().cast(), size) };
        // Returns the number of pids, not bytes (unlike `proc_listpids`).
        let count = usize::try_from(written).unwrap_or(0).min(pids.len());
        pids[..count]
            .iter()
            .filter_map(|&pid| u32::try_from(pid).ok().filter(|&pid| pid > 0))
            .collect()
    }

    /// Whether `pid` exists and isn't a zombie.
    pub fn is_alive(pid: u32) -> bool {
        pidinfo::<libc::proc_bsdinfo>(pid, libc::PROC_PIDTBSDINFO)
            .is_some_and(|info| info.pbi_status != libc::SZOMB)
    }

    /// The highest pid in process group `pgid`.
    pub fn group_member(pgid: u32) -> Option<u32> {
        // From <sys/proc_info.h>; the libc crate doesn't export it.
        const PROC_PGRP_ONLY: u32 = 2;
        let mut pids = [0 as libc::pid_t; 64];
        let size = c_int::try_from(size_of_val(&pids)).expect("small buffer");
        // SAFETY: the pointer and size describe `pids`.
        let written =
            unsafe { libc::proc_listpids(PROC_PGRP_ONLY, pgid, pids.as_mut_ptr().cast(), size) };
        // Returns bytes, unlike `proc_listchildpids`.
        let count = usize::try_from(written).unwrap_or(0) / size_of::<libc::pid_t>();
        pids[..count.min(pids.len())]
            .iter()
            .filter_map(|&pid| u32::try_from(pid).ok().filter(|&pid| pid > 0))
            .max()
    }

    pub fn cwd(pid: u32) -> Option<PathBuf> {
        let info: libc::proc_vnodepathinfo = pidinfo(pid, libc::PROC_PIDVNODEPATHINFO)?;
        let path = info.pvi_cdir.vip_path.as_flattened();
        let bytes: Vec<u8> = path
            .iter()
            .map(|&c| c as u8)
            .take_while(|&b| b != 0)
            .collect();
        (!bytes.is_empty()).then(|| PathBuf::from(OsStr::from_bytes(&bytes)))
    }
}

/// Nothing is found on platforms without an implementation.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod sys {
    use std::path::PathBuf;

    pub fn tpgid(_pid: u32) -> Option<u32> {
        None
    }

    pub fn name(_pid: u32) -> Option<String> {
        None
    }

    pub fn cwd(_pid: u32) -> Option<PathBuf> {
        None
    }

    pub fn is_alive(_pid: u32) -> bool {
        false
    }

    pub fn group_member(_pgid: u32) -> Option<u32> {
        None
    }
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod tests {
    use super::*;

    #[test]
    fn own_process() {
        // The foreground group may be another process (cargo's, in a
        // terminal), so look at this one directly.
        let pid = std::process::id();
        let cwd = std::env::current_dir().unwrap().canonicalize().unwrap();
        let found = sys::cwd(pid).map(|p| p.canonicalize().unwrap());
        assert_eq!(found, Some(cwd));
        assert!(sys::name(pid).is_some_and(|name| !name.is_empty()));
        let fg = foreground(pid, true);
        assert!(fg.cwd.is_some());
        assert!(fg.name.is_some());
    }
}
