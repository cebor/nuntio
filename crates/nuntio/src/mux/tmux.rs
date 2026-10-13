//! tmux's output, as the commands the service runs print it.

/// The `-F` format of `list-sessions` that [`parse_sessions`] reads.
pub const LIST_FORMAT: &str =
    "#{session_name}\t#{session_windows}\t#{session_attached}\t#{session_path}";

/// A tmux session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TmuxSession {
    pub name: String,
    pub windows: u32,
    /// At least one client is attached.
    pub attached: bool,
    /// The directory the session started in.
    pub path: String,
}

/// The output of `list-sessions -F` [`LIST_FORMAT`]. A line that doesn't
/// have the four fields is skipped.
pub fn parse_sessions(out: &str) -> Vec<TmuxSession> {
    out.lines()
        .filter_map(|line| {
            let mut fields = line.split('\t');
            let name = fields.next().filter(|n| !n.is_empty())?;
            let windows = fields.next()?.trim().parse().ok()?;
            let attached: u32 = fields.next()?.trim().parse().ok()?;
            let path = fields.next()?;
            Some(TmuxSession {
                name: name.to_owned(),
                windows,
                attached: attached != 0,
                path: path.to_owned(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sessions() {
        let out = "main\t3\t1\t/home/felix\nwork\t1\t0\t/home/felix/code/nuntio\n";
        assert_eq!(
            parse_sessions(out),
            [
                TmuxSession {
                    name: "main".into(),
                    windows: 3,
                    attached: true,
                    path: "/home/felix".into(),
                },
                TmuxSession {
                    name: "work".into(),
                    windows: 1,
                    attached: false,
                    path: "/home/felix/code/nuntio".into(),
                },
            ]
        );
    }

    #[test]
    fn several_clients_and_bad_lines() {
        let out = "a\t2\t2\t/a\nbroken\nb\tx\t0\t/b\n\nc\t1\t0\t\n";
        let sessions = parse_sessions(out);
        assert_eq!(sessions.len(), 2);
        assert!(sessions[0].attached);
        assert_eq!(sessions[1].name, "c");
        assert_eq!(sessions[1].path, "");
        assert!(parse_sessions("").is_empty());
    }

    #[test]
    fn format_has_real_tabs() {
        assert_eq!(LIST_FORMAT.matches('\t').count(), 3);
    }
}
