//! Program arguments as one line of text: split on spaces, with quotes
//! for arguments that contain spaces.

/// Split like a shell would, but without escapes or variables: `"…"` and
/// `'…'` group, everything else splits on whitespace.
pub fn split(line: &str) -> Result<Vec<String>, String> {
    let mut args = Vec::new();
    let mut current: Option<String> = None;
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' | '\'' => {
                let arg = current.get_or_insert_with(String::new);
                loop {
                    match chars.next() {
                        Some(q) if q == c => break,
                        Some(other) => arg.push(other),
                        None => return Err(format!("missing closing {c}")),
                    }
                }
            }
            c if c.is_whitespace() => args.extend(current.take()),
            c => current.get_or_insert_with(String::new).push(c),
        }
    }
    args.extend(current);
    Ok(args)
}

/// The inverse of [`split`].
pub fn join(args: &[String]) -> String {
    args.iter()
        .map(|arg| {
            if !arg.is_empty()
                && !arg.contains(|c: char| c.is_whitespace() || c == '"' || c == '\'')
            {
                arg.clone()
            } else if arg.contains('"') {
                format!("'{arg}'")
            } else {
                format!("\"{arg}\"")
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn splits_on_whitespace_and_quotes() {
        assert_eq!(
            split("-l  -c 'echo hi'").unwrap(),
            strings(&["-l", "-c", "echo hi"])
        );
        assert_eq!(split(r#"a"b c"d ''"#).unwrap(), strings(&["ab cd", ""]));
        assert_eq!(split("  ").unwrap(), Vec::<String>::new());
        assert!(split("\"open").is_err());
    }

    #[test]
    fn join_round_trips() {
        for args in [
            strings(&["-l"]),
            strings(&["-c", "echo hi", ""]),
            strings(&["say \"hi\"", "it's"]),
        ] {
            assert_eq!(split(&join(&args)).unwrap(), args, "{}", join(&args));
        }
    }
}
