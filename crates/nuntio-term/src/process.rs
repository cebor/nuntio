//! Information about the process running in a pane: the foreground job's
//! name (tab title fallback) and working directory (for new tabs).
//!
//! Implemented via `/proc` on Linux; other platforms return `None` for now,
//! so callers fall back to the shell name and the default directory.

use std::path::PathBuf;

/// The foreground process group of the shell's terminal, falling back to
/// the shell itself.
#[cfg(target_os = "linux")]
fn foreground_pid(shell_pid: u32) -> u32 {
    std::fs::read_to_string(format!("/proc/{shell_pid}/stat"))
        .ok()
        .and_then(|stat| parse_tpgid(&stat))
        .unwrap_or(shell_pid)
}

#[cfg(target_os = "linux")]
pub fn foreground_name(shell_pid: u32) -> Option<String> {
    let pid = foreground_pid(shell_pid);
    let comm = std::fs::read_to_string(format!("/proc/{pid}/comm")).ok()?;
    Some(comm.trim_end().to_owned()).filter(|s| !s.is_empty())
}

#[cfg(target_os = "linux")]
pub fn working_directory(shell_pid: u32) -> Option<PathBuf> {
    let pid = foreground_pid(shell_pid);
    std::fs::read_link(format!("/proc/{pid}/cwd"))
        .or_else(|_| std::fs::read_link(format!("/proc/{shell_pid}/cwd")))
        .ok()
}

#[cfg(not(target_os = "linux"))]
pub fn foreground_name(_shell_pid: u32) -> Option<String> {
    None
}

#[cfg(not(target_os = "linux"))]
pub fn working_directory(_shell_pid: u32) -> Option<PathBuf> {
    None
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
    #[cfg(target_os = "linux")]
    fn own_process() {
        let pid = std::process::id();
        assert!(working_directory(pid).is_some());
        assert!(foreground_name(pid).is_some());
    }
}
