//! The shells a new tab can run: the default one, `[[profiles]]` and the
//! ones installed on this system, and how menus and the status bar name them.

use nuntio_config::detect::{self, Found};
use nuntio_config::{Config, Shell};

use crate::wsl::Distro;

/// A shell the shell menu offers.
#[derive(Debug, Clone, PartialEq)]
pub struct ShellChoice {
    pub name: String,
    /// `None`: the system's default shell.
    pub shell: Option<Shell>,
}

/// What a new pane runs.
#[derive(Debug, Clone, PartialEq)]
pub enum Launch {
    Shell(ShellChoice),
    /// argv, never empty.
    Command(Vec<String>),
}

impl Launch {
    /// The name the `shell` status bar item shows.
    pub fn name(&self) -> String {
        match self {
            Self::Shell(choice) => choice.name.clone(),
            Self::Command(argv) => argv
                .first()
                .map(|program| detect::program_stem(program).to_owned())
                .unwrap_or_default(),
        }
    }

    /// The WSL distribution it runs in.
    pub fn distro(&self) -> Option<Distro> {
        let Self::Shell(ShellChoice {
            shell: Some(shell), ..
        }) = self
        else {
            return None;
        };
        let name = shell.wsl.clone()?;
        Some(Distro {
            name,
            user: shell.wsl_user.clone(),
        })
    }
}

/// How menus and the status bar name a shell.
pub fn shell_name(shell: Option<&Shell>) -> String {
    let program_name = |program: &str| {
        detect::known_shell_name(program)
            .map_or_else(|| detect::program_stem(program).to_owned(), str::to_owned)
    };
    match shell {
        Some(Shell {
            wsl: Some(distro),
            wsl_user,
            ..
        }) => match wsl_user {
            Some(user) => format!("{distro} ({user})"),
            None => distro.clone(),
        },
        Some(Shell {
            program: Some(program),
            ..
        }) => program_name(program),
        _ => program_name(&nuntio_term::default_shell_name()),
    }
}

/// The default shell: `shell` from the config, or the system's.
pub fn default_choice(config: &Config) -> ShellChoice {
    ShellChoice {
        name: shell_name(config.shell.as_ref()),
        shell: config.shell.clone(),
    }
}

/// The shell menu's entries: the default shell, `[[profiles]]` in order,
/// then the installed shells and WSL distributions that no earlier entry
/// already names or runs.
pub fn choices(
    config: &Config,
    installed: Vec<Found>,
    distributions: Vec<Found>,
) -> Vec<ShellChoice> {
    let mut choices = vec![default_choice(config)];
    choices.extend(config.profiles.iter().map(|profile| ShellChoice {
        name: profile.name.clone(),
        shell: Some(profile.shell()),
    }));
    let detected = installed
        .into_iter()
        .map(|found| Shell {
            program: Some(found.value),
            ..Shell::default()
        })
        .chain(distributions.into_iter().map(|found| Shell {
            wsl: Some(found.value),
            ..Shell::default()
        }));
    for shell in detected {
        let name = shell_name(Some(&shell));
        let taken = choices
            .iter()
            .any(|choice| choice.name == name || resolved(choice) == shell);
        if !taken {
            choices.push(ShellChoice {
                name,
                shell: Some(shell),
            });
        }
    }
    choices
}

/// The shell a choice runs, with the system's default spelled out.
fn resolved(choice: &ShellChoice) -> Shell {
    choice.shell.clone().unwrap_or_else(|| Shell {
        program: Some(nuntio_term::default_shell_name()),
        ..Shell::default()
    })
}

#[cfg(test)]
mod tests {
    use nuntio_config::Profile;

    use super::*;

    fn found(values: &[&str]) -> Vec<Found> {
        values
            .iter()
            .map(|value| Found {
                value: (*value).to_owned(),
                help: String::new(),
            })
            .collect()
    }

    fn program(program: &str) -> Shell {
        Shell {
            program: Some(program.to_owned()),
            ..Shell::default()
        }
    }

    fn names(choices: &[ShellChoice]) -> Vec<&str> {
        choices.iter().map(|choice| choice.name.as_str()).collect()
    }

    #[test]
    fn detected_shells_already_listed_are_left_out() {
        let mut config = Config {
            shell: Some(Shell {
                wsl: Some("Ubuntu".into()),
                ..Shell::default()
            }),
            ..Config::default()
        };
        config.profiles.push(Profile {
            name: "PowerShell".into(),
            program: Some("pwsh".into()),
            ..Profile::default()
        });
        let choices = choices(
            &config,
            found(&["pwsh", "cmd"]),
            found(&["Ubuntu", "Debian"]),
        );
        assert_eq!(
            names(&choices),
            ["Ubuntu", "PowerShell", "Command Prompt", "Debian"]
        );
        assert_eq!(
            choices[3].shell.as_ref().unwrap().wsl.as_deref(),
            Some("Debian")
        );
    }

    #[test]
    fn shells_with_the_same_name_are_listed_once() {
        let config = Config {
            shell: Some(program("fish")),
            ..Config::default()
        };
        let choices = choices(&config, found(&["/bin/bash", "/usr/bin/bash"]), Vec::new());
        assert_eq!(names(&choices), ["fish", "bash"]);
    }

    #[test]
    fn shells_are_named_by_distribution_or_program() {
        let root = Shell {
            wsl: Some("Ubuntu".into()),
            wsl_user: Some("root".into()),
            ..Shell::default()
        };
        assert_eq!(shell_name(Some(&root)), "Ubuntu (root)");
        assert_eq!(
            shell_name(Some(&program(r"C:\Program Files\PowerShell\7\pwsh.exe"))),
            "PowerShell 7"
        );
        assert_eq!(shell_name(Some(&program("/usr/bin/fish"))), "fish");
    }
}
