//! The project of a pane's directory as a multiplexer session name.

use super::tmux::TmuxSession;

/// The last component of `dir`, which may use `/` or `\`. `"project"` if
/// there is none.
pub fn project_name(dir: &str) -> String {
    dir.rsplit(['/', '\\'])
        .find(|part| !part.is_empty())
        .unwrap_or("project")
        .to_owned()
}

/// [`project_name`] as a tmux session name: tmux replaces `.` and `:` in
/// names itself, so they are replaced here to keep the name stable.
pub fn tmux_name(dir: &str) -> String {
    project_name(dir).replace(['.', ':'], "_")
}

/// The tmux session for a project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TmuxProjectSession {
    /// A session started in the project's directory.
    Existing(String),
    /// A session to create; its name is free.
    New(String),
}

impl TmuxProjectSession {
    pub fn name(&self) -> &str {
        match self {
            TmuxProjectSession::Existing(name) | TmuxProjectSession::New(name) => name,
        }
    }
}

/// `dir` without trailing separators, for comparing paths.
fn trimmed(dir: &str) -> &str {
    let trimmed = dir.trim_end_matches(['/', '\\']);
    if trimmed.is_empty() { dir } else { trimmed }
}

/// The session of the project in `dir`: one that started there, otherwise
/// a new one named after the directory. If another project's session has
/// that name, the parent directory's name goes in front, then a number
/// after it.
pub fn tmux_project(sessions: &[TmuxSession], dir: &str) -> TmuxProjectSession {
    let dir = trimmed(dir);
    if let Some(session) = sessions.iter().find(|s| trimmed(&s.path) == dir) {
        return TmuxProjectSession::Existing(session.name.clone());
    }
    let taken = |name: &str| sessions.iter().any(|s| s.name == name);
    let name = tmux_name(dir);
    if !taken(&name) {
        return TmuxProjectSession::New(name);
    }
    let parent = dir
        .rsplit_once(['/', '\\'])
        .map_or("", |(parent, _)| parent);
    let qualified = if parent.is_empty() {
        name
    } else {
        format!("{}-{name}", tmux_name(parent))
    };
    if !taken(&qualified) {
        return TmuxProjectSession::New(qualified);
    }
    let free = (2..)
        .map(|n| format!("{qualified}-{n}"))
        .find(|candidate| !taken(candidate))
        .expect("an unbounded range has a free name");
    TmuxProjectSession::New(free)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(name: &str, path: &str) -> TmuxSession {
        TmuxSession {
            name: name.into(),
            windows: 1,
            attached: false,
            path: path.into(),
        }
    }

    #[test]
    fn names() {
        assert_eq!(project_name("/home/felix/code/nuntio"), "nuntio");
        assert_eq!(project_name("/home/felix/code/nuntio/"), "nuntio");
        assert_eq!(project_name(r"C:\Users\felix\app"), "app");
        assert_eq!(project_name("/"), "project");
        assert_eq!(project_name(""), "project");
        assert_eq!(project_name("/a/v1.2"), "v1.2");
        assert_eq!(tmux_name("/a/v1.2:x"), "v1_2_x");
    }

    #[test]
    fn an_existing_session_in_the_directory_is_used() {
        let sessions = [session("other", "/x"), session("mine", "/a/proj")];
        assert_eq!(
            tmux_project(&sessions, "/a/proj/"),
            TmuxProjectSession::Existing("mine".into())
        );
    }

    #[test]
    fn a_new_session_takes_the_directory_name() {
        let sessions = [session("other", "/x")];
        let project = tmux_project(&sessions, "/a/proj");
        assert_eq!(project, TmuxProjectSession::New("proj".into()));
        assert_eq!(project.name(), "proj");
    }

    #[test]
    fn name_collisions_use_the_parent_and_then_a_number() {
        let sessions = [session("proj", "/b/proj")];
        assert_eq!(
            tmux_project(&sessions, "/a/proj"),
            TmuxProjectSession::New("a-proj".into())
        );
        let sessions = [session("proj", "/b/proj"), session("a-proj", "/c/a/proj-x")];
        assert_eq!(
            tmux_project(&sessions, "/a/proj"),
            TmuxProjectSession::New("a-proj-2".into())
        );
        let sessions = [
            session("proj", "/b/proj"),
            session("a-proj", "/c"),
            session("a-proj-2", "/d"),
        ];
        assert_eq!(
            tmux_project(&sessions, "/a/proj"),
            TmuxProjectSession::New("a-proj-3".into())
        );
        // No parent to qualify with.
        let sessions = [session("proj", "/b")];
        assert_eq!(
            tmux_project(&sessions, "/proj"),
            TmuxProjectSession::New("proj-2".into())
        );
    }
}
