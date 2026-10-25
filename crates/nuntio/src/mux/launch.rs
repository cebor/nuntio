//! The command lines of herdr and tmux clients: argv with the program
//! first, as `Launch::Command` and `run::run` take them.

use super::Tool;

/// The client of `tool` for `session`: herdr starts or attaches to it
/// (`None`: the default session), tmux attaches to it (`None`: the
/// most recent one).
pub fn attach<'a>(tool: Tool, program: &'a str, session: Option<&'a str>) -> Vec<&'a str> {
    match tool {
        Tool::Herdr => herdr(program, session),
        Tool::Tmux => {
            let mut argv = vec![program, "attach-session"];
            if let Some(session) = session {
                argv.extend(["-t", session]);
            }
            argv
        }
    }
}

/// herdr in `session`; `None` is the default session.
pub fn herdr<'a>(program: &'a str, session: Option<&'a str>) -> Vec<&'a str> {
    let mut argv = vec![program];
    if let Some(session) = session {
        argv.extend(["--session", session]);
    }
    argv
}

/// herdr as the client of the machine `target`, in its session `session`
/// (the default one when empty or `"default"`).
pub fn herdr_remote<'a>(program: &'a str, target: &'a str, session: &'a str) -> Vec<&'a str> {
    let mut argv = vec![program, "--remote", target];
    if !session.is_empty() && session != "default" {
        argv.extend(["--session", session]);
    }
    argv
}

/// Focus the pane `pane_id` in herdr's `session`.
pub fn herdr_focus_agent<'a>(
    program: &'a str,
    session: Option<&'a str>,
    pane_id: &'a str,
) -> Vec<&'a str> {
    let mut argv = herdr(program, session);
    argv.extend(["agent", "focus", pane_id]);
    argv
}

/// tmux creating the session `name`, or attaching to it if it exists,
/// started in `dir` when given.
pub fn tmux_new_session<'a>(program: &'a str, name: &'a str, dir: Option<&'a str>) -> Vec<&'a str> {
    let mut argv = vec![program, "new-session", "-A", "-s", name];
    if let Some(dir) = dir {
        argv.extend(["-c", dir]);
    }
    argv
}

/// `argv` as owned strings.
pub fn owned(argv: Vec<&str>) -> Vec<String> {
    argv.into_iter().map(str::to_owned).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn herdr_clients() {
        assert_eq!(herdr("herdr", None), ["herdr"]);
        assert_eq!(herdr("herdr", Some("nv")), ["herdr", "--session", "nv"]);
        assert_eq!(
            herdr_remote("herdr", "me@host", "default"),
            ["herdr", "--remote", "me@host"]
        );
        assert_eq!(
            herdr_remote("herdr", "me@host", "work"),
            ["herdr", "--remote", "me@host", "--session", "work"]
        );
        assert_eq!(
            herdr_focus_agent("herdr", Some("nv"), "w1:p1"),
            ["herdr", "--session", "nv", "agent", "focus", "w1:p1"]
        );
    }

    #[test]
    fn tmux_clients() {
        assert_eq!(attach(Tool::Tmux, "tmux", None), ["tmux", "attach-session"]);
        assert_eq!(
            attach(Tool::Tmux, "tmux", Some("a")),
            ["tmux", "attach-session", "-t", "a"]
        );
        assert_eq!(
            tmux_new_session("tmux", "main", None),
            ["tmux", "new-session", "-A", "-s", "main"]
        );
        assert_eq!(
            tmux_new_session("tmux", "p", Some("/x")),
            ["tmux", "new-session", "-A", "-s", "p", "-c", "/x"]
        );
    }
}
