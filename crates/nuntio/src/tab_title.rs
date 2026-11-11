//! What a tab shows as its title: the directory, the running program or the
//! application's own title, depending on `tabs.title`.

use std::path::{Path, PathBuf};

use nuntio_config::TabTitle;

/// What is known about a pane when its title is chosen.
#[derive(Debug, Clone, Default)]
pub struct TitleInfo {
    /// Title set by the application (OSC 0/2).
    pub application: Option<String>,
    /// Whether `application` was set while a program, not the shell at its
    /// prompt, was in the foreground: `auto` shows it instead of the process.
    pub program_title: bool,
    /// Name of the foreground process.
    pub process: String,
    /// Working directory of the foreground process.
    pub directory: Option<PathBuf>,
    /// Whether the shell waits at its prompt; `None` where unknown.
    pub shell_idle: Option<bool>,
}

/// The title of a tab in `mode`; `local_host` is this machine's host name,
/// so that `auto` keeps the host of a title from another machine.
pub fn title(mode: TabTitle, info: TitleInfo, home: Option<&Path>, local_host: &str) -> String {
    let directory = || {
        info.directory
            .as_deref()
            .map(|dir| nuntio_config::tilde_path(dir, home))
    };
    let application = || {
        info.application
            .as_deref()
            .map(|t| auto_application(t, local_host))
    };
    let title = match mode {
        TabTitle::Application => info.application.clone(),
        TabTitle::Process => None,
        TabTitle::Path => directory(),
        TabTitle::Auto => match info.shell_idle {
            Some(true) => directory().or_else(application),
            Some(false) => info.program_title.then(application).flatten(),
            None => application(),
        },
    };
    title
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| info.process.clone())
}

/// Drop the `user@host:` prefix that shells put in front of the directory.
pub fn strip_user_host(title: &str) -> String {
    split_user_host(title)
        .map_or(title, |(_, rest)| rest)
        .to_owned()
}

/// The `user@host` prefix and the rest of a title like `felix@box: ~/code`,
/// the rest without leading spaces. `None` without such a prefix.
fn split_user_host(title: &str) -> Option<(&str, &str)> {
    let (prefix, rest) = title.split_once(':')?;
    (prefix.contains('@') && !prefix.contains(char::is_whitespace))
        .then(|| (prefix, rest.trim_start()))
}

/// Whether `host` names this machine: empty, `localhost` or `local_host`,
/// compared by the part before the first `.` and ignoring case
/// (`Box.local` is `box`).
fn is_local_host(host: &str, local_host: &str) -> bool {
    let short = |name: &str| name.split('.').next().unwrap_or_default().to_owned();
    let host = short(host);
    host.is_empty()
        || host.eq_ignore_ascii_case("localhost")
        || host.eq_ignore_ascii_case(&short(local_host))
}

/// An application title as `auto` shows it: without the `user@host:`
/// prefix shells put in front of the directory, but with the host kept
/// (`server: ~/code`) when it is another machine, as in a shell over ssh.
fn auto_application(title: &str, local_host: &str) -> String {
    let Some((prefix, rest)) = split_user_host(title) else {
        return title.to_owned();
    };
    let host = prefix.rsplit_once('@').map_or(prefix, |(_, host)| host);
    if is_local_host(host, local_host) {
        rest.to_owned()
    } else if rest.is_empty() {
        host.to_owned()
    } else {
        format!("{host}: {rest}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `~` and `rest` joined as `nuntio_config::tilde_path` does on this platform.
    fn tilde(rest: &str) -> String {
        format!("~{}{rest}", std::path::MAIN_SEPARATOR)
    }

    #[test]
    fn user_and_host_are_stripped() {
        assert_eq!(strip_user_host("felix@box: ~/code"), "~/code");
        assert_eq!(strip_user_host("felix@box:~"), "~");
        assert_eq!(strip_user_host("vim foo.rs"), "vim foo.rs");
        assert_eq!(strip_user_host("foo:bar"), "foo:bar", "no @");
        assert_eq!(strip_user_host("mail a@b: x"), "mail a@b: x", "space");
    }

    fn info(shell_idle: Option<bool>) -> TitleInfo {
        TitleInfo {
            application: Some("felix@box: ~/code".into()),
            program_title: false,
            process: "htop".into(),
            directory: Some("/home/felix/src".into()),
            shell_idle,
        }
    }

    #[test]
    fn auto_shows_the_directory_at_the_prompt_and_the_program_otherwise() {
        let home = Some(Path::new("/home/felix"));
        assert_eq!(
            title(TabTitle::Auto, info(Some(true)), home, "box"),
            tilde("src")
        );
        assert_eq!(
            title(TabTitle::Auto, info(Some(false)), home, "box"),
            "htop"
        );
        // Without process information, fall back to the cleaned-up OSC title.
        assert_eq!(title(TabTitle::Auto, info(None), home, "box"), "~/code");
        let bare = TitleInfo {
            process: "zsh".into(),
            ..TitleInfo::default()
        };
        assert_eq!(title(TabTitle::Auto, bare, home, "box"), "zsh");
    }

    #[test]
    fn auto_shows_the_title_the_running_program_set() {
        let home = Some(Path::new("/home/felix"));
        let program = |shell_idle, application: Option<&str>| TitleInfo {
            application: application.map(Into::into),
            program_title: true,
            ..info(shell_idle)
        };
        assert_eq!(
            title(
                TabTitle::Auto,
                program(Some(false), Some("felix@box: ~/code")),
                home,
                "box"
            ),
            "~/code"
        );
        // A program without a title of its own shows its name.
        assert_eq!(
            title(TabTitle::Auto, program(Some(false), None), home, "box"),
            "htop"
        );
        // At the prompt the directory wins.
        assert_eq!(
            title(
                TabTitle::Auto,
                program(Some(true), Some("vim")),
                home,
                "box"
            ),
            tilde("src")
        );
    }

    #[test]
    fn auto_keeps_the_host_of_another_machine() {
        let ssh = |application: &str, local_host: &str| {
            let info = TitleInfo {
                application: Some(application.into()),
                program_title: true,
                process: "ssh".into(),
                directory: Some("/home/felix".into()),
                shell_idle: Some(false),
            };
            title(TabTitle::Auto, info, None, local_host)
        };
        assert_eq!(ssh("felix@server: ~/code", "box"), "server: ~/code");
        assert_eq!(ssh("felix@server:~/code", "box"), "server: ~/code");
        // This machine, by its short name and ignoring case.
        assert_eq!(ssh("felix@Box.local: ~/code", "box"), "~/code");
        assert_eq!(ssh("felix@box: ~/code", "BOX.fritz.box"), "~/code");
        assert_eq!(ssh("root@localhost: /tmp", "box"), "/tmp");
        assert_eq!(ssh("felix@server:", "box"), "server");
        let raw = TitleInfo {
            application: Some("felix@server: ~/code".into()),
            ..info(Some(false))
        };
        assert_eq!(
            title(TabTitle::Application, raw, None, "box"),
            "felix@server: ~/code"
        );
    }

    #[test]
    fn fixed_modes() {
        let home = Some(Path::new("/home/felix"));
        assert_eq!(
            title(TabTitle::Path, info(Some(false)), home, "box"),
            tilde("src")
        );
        assert_eq!(
            title(TabTitle::Process, info(Some(true)), home, "box"),
            "htop"
        );
        assert_eq!(
            title(TabTitle::Application, info(Some(true)), home, "box"),
            "felix@box: ~/code"
        );
    }
}
