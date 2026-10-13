//! Where herdr and tmux are: the absolute path of each program, looked up
//! the way a login shell would find it, since nuntio started from a
//! desktop launcher may have a much shorter `PATH` than the user's shell.

use std::collections::HashMap;
use std::time::Duration;

use super::{Target, Tool};

/// How long the lookup may take, login shell included.
const TIMEOUT: Duration = Duration::from_secs(5);

/// Prints `name<TAB>path` for each argument, the path empty when it isn't
/// found: on `PATH`, then on the login shell's `PATH`, then in the usual
/// install directories. Only absolute paths count.
const SCRIPT: &str = r#"for p in "$@"; do
  f=$(command -v "$p" 2>/dev/null)
  if [ -z "$f" ] && [ -n "$SHELL" ]; then f=$("$SHELL" -lc "command -v $p" 2>/dev/null | tail -n 1); fi
  if [ -z "$f" ]; then for d in "$HOME/.local/bin" "$HOME/.cargo/bin" "$HOME/.nix-profile/bin" /opt/homebrew/bin /usr/local/bin /usr/bin; do [ -x "$d/$p" ] && { f="$d/$p"; break; }; done; fi
  case "$f" in /*) ;; *) f= ;; esac
  printf '%s\t%s\n' "$p" "$f"
done"#;

/// The absolute path of each tool in `tools` (with the program the config
/// names) that `target` has. WSL distributions always look for the bare
/// tool names.
pub fn resolve(target: &Target, tools: &[(Tool, &str)]) -> HashMap<Tool, String> {
    let mut found = HashMap::new();
    let mut names: Vec<(Tool, &str)> = Vec::new();
    for &(tool, program) in tools {
        let program = match target {
            Target::Local => program.trim(),
            Target::Wsl { .. } => tool.name(),
        };
        if matches!(target, Target::Local) && std::path::Path::new(program).is_absolute() {
            if std::path::Path::new(program).is_file() {
                found.insert(tool, program.to_owned());
            }
        } else if safe_name(program) {
            names.push((tool, program));
        }
    }
    if names.is_empty() {
        return found;
    }
    #[cfg(windows)]
    if matches!(target, Target::Local) {
        for (tool, name) in names {
            if let Some(path) = windows_program(tool, name) {
                found.insert(tool, path);
            }
        }
        return found;
    }
    let mut args = vec!["-c", SCRIPT, "sh"];
    args.extend(names.iter().map(|(_, name)| *name));
    let Some(output) = super::run::run(target, "sh", &args, TIMEOUT) else {
        return found;
    };
    for (name, path) in parse_resolved(&output) {
        let (Some(path), Some(&(tool, _))) = (path, names.iter().find(|(_, n)| *n == name)) else {
            continue;
        };
        found.insert(tool, path);
    }
    found
}

/// A program name that is safe to put into the script's `-lc` command.
fn safe_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// The lines of the script's output: each name and its path, if found.
pub fn parse_resolved(output: &str) -> Vec<(String, Option<String>)> {
    output
        .lines()
        .filter_map(|line| {
            let (name, path) = line.split_once('\t')?;
            let path = path.trim();
            Some((
                name.to_owned(),
                path.starts_with('/').then(|| path.to_owned()),
            ))
        })
        .collect()
}

/// herdr on native Windows: on `PATH` or where its installer puts it.
/// There is no native tmux.
#[cfg(windows)]
fn windows_program(tool: Tool, name: &str) -> Option<String> {
    if tool != Tool::Herdr {
        return None;
    }
    let file = if name.to_ascii_lowercase().ends_with(".exe") {
        name.to_owned()
    } else {
        format!("{name}.exe")
    };
    let path = std::env::var_os("PATH")?;
    // `symlink_metadata`: app execution aliases are reparse points that
    // `metadata` can't follow.
    std::env::split_paths(&path)
        .map(|dir| dir.join(&file))
        .find(|p| std::fs::symlink_metadata(p).is_ok())
        .or_else(|| {
            let local = std::env::var_os("LOCALAPPDATA")?;
            let p = std::path::Path::new(&local).join(r"Programs\Herdr\bin\herdr.exe");
            p.is_file().then_some(p)
        })
        .map(|p| p.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolved_paths_are_parsed() {
        let out = "herdr\t/home/me/.local/bin/herdr\ntmux\t\nodd line\nx\trelative/x\n";
        assert_eq!(
            parse_resolved(out),
            [
                (
                    "herdr".to_owned(),
                    Some("/home/me/.local/bin/herdr".to_owned())
                ),
                ("tmux".to_owned(), None),
                ("x".to_owned(), None),
            ]
        );
    }

    #[test]
    fn only_plain_names_reach_the_shell() {
        assert!(safe_name("herdr"));
        assert!(safe_name("tmux-3.4"));
        assert!(!safe_name("tmux; rm -rf ~"));
        assert!(!safe_name("$(x)"));
        assert!(!safe_name(""));
    }

    #[cfg(unix)]
    #[test]
    fn programs_are_found_through_the_script() {
        let found = resolve(&Target::Local, &[(Tool::Herdr, "sh")]);
        assert!(found.get(&Tool::Herdr).is_some_and(|p| p.starts_with('/')));
        let missing = resolve(&Target::Local, &[(Tool::Tmux, "no-such-program-nuntio")]);
        assert!(missing.is_empty());
    }
}
