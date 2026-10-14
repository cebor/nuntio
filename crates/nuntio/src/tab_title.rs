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

pub fn title(mode: TabTitle, info: TitleInfo, home: Option<&Path>) -> String {
    let directory = || {
        info.directory
            .as_deref()
            .map(|dir| nuntio_config::tilde_path(dir, home))
    };
    let application = || info.application.as_deref().map(strip_user_host);
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
    match title.split_once(':') {
        Some((prefix, rest)) if prefix.contains('@') && !prefix.contains(char::is_whitespace) => {
            rest.trim_start().to_owned()
        }
        _ => title.to_owned(),
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
        assert_eq!(title(TabTitle::Auto, info(Some(true)), home), tilde("src"));
        assert_eq!(title(TabTitle::Auto, info(Some(false)), home), "htop");
        // Without process information, fall back to the cleaned-up OSC title.
        assert_eq!(title(TabTitle::Auto, info(None), home), "~/code");
        let bare = TitleInfo {
            process: "zsh".into(),
            ..TitleInfo::default()
        };
        assert_eq!(title(TabTitle::Auto, bare, home), "zsh");
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
                home
            ),
            "~/code"
        );
        // A program without a title of its own shows its name.
        assert_eq!(
            title(TabTitle::Auto, program(Some(false), None), home),
            "htop"
        );
        // At the prompt the directory wins.
        assert_eq!(
            title(TabTitle::Auto, program(Some(true), Some("vim")), home),
            tilde("src")
        );
    }

    #[test]
    fn fixed_modes() {
        let home = Some(Path::new("/home/felix"));
        assert_eq!(title(TabTitle::Path, info(Some(false)), home), tilde("src"));
        assert_eq!(title(TabTitle::Process, info(Some(true)), home), "htop");
        assert_eq!(
            title(TabTitle::Application, info(Some(true)), home),
            "felix@box: ~/code"
        );
    }
}
