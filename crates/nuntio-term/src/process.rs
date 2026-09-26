//! Information about the process running in a pane: the foreground job's
//! name and working directory (tab titles, and where new tabs start).
//!
//! Implemented via `/proc` on Linux and `proc_pidinfo` on macOS. Windows
//! returns `None` for now, so callers fall back to the shell name and the
//! default directory.

use std::path::PathBuf;

/// The foreground process group of the shell's terminal, falling back to
/// the shell itself.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn foreground_pid(shell_pid: u32) -> u32 {
    sys::tpgid(shell_pid).unwrap_or(shell_pid)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub fn foreground_name(shell_pid: u32) -> Option<String> {
    sys::name(foreground_pid(shell_pid)).filter(|s| !s.is_empty())
}

/// Whether the shell itself is in the foreground, i.e. waiting at its
/// prompt rather than running a program.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub fn foreground_is_shell(shell_pid: u32) -> Option<bool> {
    sys::tpgid(shell_pid).map(|pid| pid == shell_pid)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub fn working_directory(shell_pid: u32) -> Option<PathBuf> {
    sys::cwd(foreground_pid(shell_pid)).or_else(|| sys::cwd(shell_pid))
}

/// The shell that `login` (see `pane::login_command`) started as its
/// child, once it exists.
#[cfg(target_os = "macos")]
pub fn login_child(login_pid: u32) -> Option<u32> {
    sys::children(login_pid).into_iter().next()
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn foreground_name(_shell_pid: u32) -> Option<String> {
    None
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn foreground_is_shell(_shell_pid: u32) -> Option<bool> {
    None
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn working_directory(_shell_pid: u32) -> Option<PathBuf> {
    None
}

#[cfg(target_os = "linux")]
mod sys {
    use std::path::PathBuf;

    /// The foreground process group of `pid`'s terminal.
    pub fn tpgid(pid: u32) -> Option<u32> {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        super::parse_tpgid(&stat)
    }

    pub fn name(pid: u32) -> Option<String> {
        let comm = std::fs::read_to_string(format!("/proc/{pid}/comm")).ok()?;
        Some(comm.trim_end().to_owned())
    }

    pub fn cwd(pid: u32) -> Option<PathBuf> {
        std::fs::read_link(format!("/proc/{pid}/cwd")).ok()
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

/// Field 8 (`tpgid`) of `/proc/<pid>/stat`: the terminal's foreground
/// process group. The command name in field 2 may contain spaces and
/// parentheses, so fields are counted after its closing parenthesis.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn parse_tpgid(stat: &str) -> Option<u32> {
    let after_comm = &stat[stat.rfind(')')? + 1..];
    // state ppid pgrp session tty_nr tpgid
    let tpgid: i64 = after_comm.split_whitespace().nth(5)?.parse().ok()?;
    u32::try_from(tpgid).ok().filter(|&pid| pid > 0)
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
    fn no_controlling_terminal() {
        let stat = "42 (daemon) S 1 42 42 0 -1 0";
        assert_eq!(parse_tpgid(stat), None);
    }

    #[test]
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn own_process() {
        // The foreground group may be another process (cargo's, in a
        // terminal), so look at this one directly.
        let pid = std::process::id();
        let cwd = std::env::current_dir().unwrap().canonicalize().unwrap();
        let found = sys::cwd(pid).map(|p| p.canonicalize().unwrap());
        assert_eq!(found, Some(cwd));
        assert!(sys::name(pid).is_some_and(|name| !name.is_empty()));
        assert!(working_directory(pid).is_some());
        assert!(foreground_name(pid).is_some());
    }
}
