//! Paths of files dropped on the window, as a shell in a pane reads them.

/// How the shell in a pane reads a dropped file's path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathSyntax {
    /// Backslash escapes (sh, bash, zsh, fish).
    Posix,
    /// Single quotes, in which PowerShell expands nothing.
    #[cfg_attr(not(windows), allow(dead_code))]
    PowerShell,
    /// Double quotes, with `%` outside them so it can be escaped.
    #[cfg_attr(not(windows), allow(dead_code))]
    Cmd,
    /// A Windows path, seen from inside WSL.
    #[cfg_attr(not(windows), allow(dead_code))]
    Wsl,
}

impl PathSyntax {
    /// The syntax of a pane whose foreground program is `process`; `wsl`
    /// if its shell runs in WSL.
    pub fn of(wsl: bool, process: &str) -> Self {
        if !cfg!(windows) {
            Self::Posix
        } else if wsl {
            Self::Wsl
        } else if process.eq_ignore_ascii_case("cmd") {
            Self::Cmd
        } else {
            Self::PowerShell
        }
    }
}

/// A dropped file's path as a shell word, with a space after it like in
/// Terminal.app, so several files line up as arguments.
pub fn dropped_path(path: &str, syntax: PathSyntax) -> String {
    // Windows paths can't contain `"`.
    let plain = || {
        path.chars()
            .all(|c| c.is_alphanumeric() || "\\/:._-".contains(c))
    };
    let mut word = match syntax {
        PathSyntax::Posix => posix_word(path),
        PathSyntax::PowerShell if plain() => path.to_owned(),
        PathSyntax::PowerShell => powershell_word(path),
        PathSyntax::Cmd if plain() => path.to_owned(),
        // cmd expands `%VAR%` even in quotes, where `^` is no escape.
        PathSyntax::Cmd => format!("\"{}\"", path.replace('%', "\"^%\"")),
        PathSyntax::Wsl => posix_word(&crate::wsl::wsl_path(path)),
    };
    word.push(' ');
    word
}

/// `s` in single quotes. PowerShell also closes single quotes with the
/// typographic ones; each is escaped by doubling it.
fn powershell_word(s: &str) -> String {
    let mut word = String::from("'");
    for c in s.chars() {
        if matches!(c, '\'' | '\u{2018}' | '\u{2019}' | '\u{201a}' | '\u{201b}') {
            word.push(c);
        }
        word.push(c);
    }
    word.push('\'');
    word
}

/// `s` with a backslash before each character a POSIX shell treats
/// specially. A line break after a backslash would continue the line
/// instead, so names with control characters are single-quoted.
fn posix_word(s: &str) -> String {
    if s.chars().any(char::is_control) {
        return format!("'{}'", s.replace('\'', r"'\''"));
    }
    let mut word = String::new();
    for c in s.chars() {
        let plain = c.is_alphanumeric() || "/._-+,:@%=".contains(c);
        if !plain {
            word.push('\\');
        }
        word.push(c);
    }
    word
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dropped_paths_are_shell_words() {
        let posix = |path| dropped_path(path, PathSyntax::Posix);
        assert_eq!(posix("/tmp/a.txt"), "/tmp/a.txt ");
        assert_eq!(
            posix("/Users/me/My Files/it's (1).pdf"),
            "/Users/me/My\\ Files/it\\'s\\ \\(1\\).pdf "
        );
        assert_eq!(posix("/tmp/Übung $x"), "/tmp/Übung\\ \\$x ");
        assert_eq!(posix("/tmp/a\nb's"), "'/tmp/a\nb'\\''s' ");
    }

    #[test]
    fn dropped_windows_paths_are_quoted_when_needed() {
        let powershell = |path| dropped_path(path, PathSyntax::PowerShell);
        assert_eq!(powershell(r"C:\tmp\a.txt"), r"C:\tmp\a.txt ");
        assert_eq!(
            powershell(r"C:\Users\me\My Files\a&b.txt"),
            r"'C:\Users\me\My Files\a&b.txt' "
        );
        assert_eq!(
            powershell(r"C:\tmp\a$(calc)`n.txt"),
            r"'C:\tmp\a$(calc)`n.txt' "
        );
        assert_eq!(powershell(r"C:\it's ‘x’"), r"'C:\it''s ‘‘x’’' ");

        let cmd = |path| dropped_path(path, PathSyntax::Cmd);
        assert_eq!(cmd(r"C:\tmp\a.txt"), r"C:\tmp\a.txt ");
        assert_eq!(cmd(r"C:\My Files\a&b.txt"), r#""C:\My Files\a&b.txt" "#);
        assert_eq!(cmd(r"C:\100%PATH%.txt"), r#""C:\100"^%"PATH"^%".txt" "#);
    }

    #[test]
    fn dropped_paths_in_wsl_are_translated() {
        let wsl = |path| dropped_path(path, PathSyntax::Wsl);
        assert_eq!(
            wsl(r"C:\Users\me\My Files\a.txt"),
            "/mnt/c/Users/me/My\\ Files/a.txt "
        );
        assert_eq!(wsl(r"D:\"), "/mnt/d/ ");
        assert_eq!(wsl(r"\\wsl.localhost\Ubuntu\home\me\x"), "/home/me/x ");
        assert_eq!(wsl(r"\\wsl$\Debian\etc"), "/etc ");
        assert_eq!(wsl(r"\\?\C:\a"), "/mnt/c/a ");
    }

    #[test]
    fn only_windows_distinguishes_shells() {
        if cfg!(windows) {
            assert_eq!(PathSyntax::of(true, "bash"), PathSyntax::Wsl);
            assert_eq!(PathSyntax::of(false, "CMD"), PathSyntax::Cmd);
            assert_eq!(PathSyntax::of(false, "pwsh"), PathSyntax::PowerShell);
        } else {
            assert_eq!(PathSyntax::of(false, "cmd"), PathSyntax::Posix);
        }
    }
}
