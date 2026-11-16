//! `nuntio-apps touch-id-sudo`: lets `sudo` ask for Touch ID instead of the
//! password by adding `pam_tid.so` to `/etc/pam.d/sudo_local` (macOS 14 or
//! later). The file is written through `sudo`, so the user authenticates in
//! the pane; the pane stays open until Enter.

// Only `enable` on macOS uses the paths and the PAM logic.
#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

use std::io::{BufRead, Write};

use anyhow::Result;

const SUDO: &str = "/etc/pam.d/sudo";
const SUDO_LOCAL: &str = "/etc/pam.d/sudo_local";
const TEMPLATE: &str = "/etc/pam.d/sudo_local.template";
const PAM_LINE: &str = "auth       sufficient     pam_tid.so";
const HELP: &str = "Usage: nuntio-apps touch-id-sudo\n\nLets sudo ask for Touch ID instead of the password (macOS 14 or later):\nadds `auth sufficient pam_tid.so` to /etc/pam.d/sudo_local through sudo.\n\nOptions:\n  -h, --help  Print this help\n";

/// What `/etc/pam.d/sudo_local` needs for Touch ID.
#[derive(Debug, PartialEq, Eq)]
enum Plan {
    /// A line that isn't commented out already loads `pam_tid.so`.
    AlreadyEnabled,
    /// Write this as the new `sudo_local`.
    Write(String),
}

/// Whether a line that isn't commented out loads `pam_tid.so`.
fn is_enabled(text: &str) -> bool {
    text.lines()
        .any(|line| !line.trim_start().starts_with('#') && line.contains("pam_tid.so"))
}

/// The new `sudo_local` from the current one (`local`) or, without it, the
/// system's `template`: the commented-out `pam_tid.so` line uncommented, or
/// [`PAM_LINE`] appended, so lines before it (`pam_reattach.so`) stay first.
fn plan(local: Option<&str>, template: Option<&str>) -> Plan {
    if local.is_some_and(is_enabled) {
        return Plan::AlreadyEnabled;
    }
    let source = local.or(template).unwrap_or("");
    let mut replaced = false;
    let mut lines: Vec<&str> = source
        .lines()
        .map(|line| {
            let trimmed = line.trim_start();
            let commented_auth = trimmed.starts_with('#')
                && trimmed
                    .trim_start_matches('#')
                    .trim_start()
                    .starts_with("auth");
            if commented_auth && line.contains("pam_tid.so") {
                replaced = true;
                PAM_LINE
            } else {
                line
            }
        })
        .collect();
    if !replaced {
        lines.push(PAM_LINE);
    }
    let mut text = lines.join("\n").trim_end_matches('\n').to_owned();
    text.push('\n');
    Plan::Write(text)
}

/// Whether `/etc/pam.d/sudo` includes `sudo_local` (macOS 14 or later).
fn includes_local(sudo: &str) -> bool {
    sudo.lines().any(|line| {
        !line.trim_start().starts_with('#')
            && line.contains("include")
            && line.contains("sudo_local")
    })
}

pub fn run(mut parser: lexopt::Parser) -> Result<()> {
    use lexopt::prelude::*;
    if let Some(arg) = parser.next()? {
        match arg {
            Short('h') | Long("help") => {
                print!("{HELP}");
                return Ok(());
            }
            _ => Err(arg.unexpected())?,
        }
    }
    match enable() {
        Ok(message) => println!("{message}"),
        Err(err) => println!("\x1b[31mError: {err:#}\x1b[0m"),
    }
    print!("\nPress Enter to close.");
    std::io::stdout().flush()?;
    // EOF closes as well.
    let _ = std::io::stdin().lock().read_line(&mut String::new());
    Ok(())
}

/// Enable Touch ID for sudo; `Ok` is the message for the user.
#[cfg(not(target_os = "macos"))]
fn enable() -> Result<String> {
    anyhow::bail!("Touch ID for sudo is only available on macOS")
}

/// Enable Touch ID for sudo; `Ok` is the message for the user.
#[cfg(target_os = "macos")]
fn enable() -> Result<String> {
    use std::fs;

    use anyhow::Context;

    if !fs::read_to_string(SUDO).is_ok_and(|sudo| includes_local(&sudo)) {
        anyhow::bail!(
            "this macOS has no /etc/pam.d/sudo_local; Touch ID for sudo needs macOS 14 or later"
        );
    }
    let local = fs::read_to_string(SUDO_LOCAL).ok();
    let template = fs::read_to_string(TEMPLATE).ok();
    let text = match plan(local.as_deref(), template.as_deref()) {
        Plan::AlreadyEnabled => {
            return Ok("Touch ID for sudo is already enabled (/etc/pam.d/sudo_local).".to_owned());
        }
        Plan::Write(text) => text,
    };
    println!("Enabling Touch ID for sudo: nuntio writes /etc/pam.d/sudo_local through sudo.");
    println!("New contents:\n");
    for line in text.lines() {
        println!("  {line}");
    }
    println!();

    let tmp = std::env::temp_dir().join(format!("nuntio-sudo_local-{}", std::process::id()));
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp)
        .and_then(|mut file| file.write_all(text.as_bytes()))
        .with_context(|| format!("could not write {}", tmp.display()))?;
    let status = std::process::Command::new("/usr/bin/sudo")
        .args(["/usr/bin/install", "-m", "644", "-o", "root", "-g", "wheel"])
        .arg(&tmp)
        .arg(SUDO_LOCAL)
        .status();
    let _ = fs::remove_file(&tmp);
    if !status.context("could not run sudo")?.success() {
        anyhow::bail!("sudo failed; /etc/pam.d/sudo_local is unchanged");
    }
    Ok("Touch ID for sudo is enabled. Try it with: sudo -k && sudo -v\nWithout a Touch ID sensor (or with the lid closed) sudo asks for the password as before. Inside tmux or herdr, Touch ID also needs pam_reattach.".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEMPLATE_TEXT: &str = "# sudo_local: local config file which survives system update and is included for sudo\n# uncomment following line to enable Touch ID for sudo\n#auth       sufficient     pam_tid.so\n";

    #[test]
    fn uncomments_the_template_line() {
        let expected = format!(
            "# sudo_local: local config file which survives system update and is included for sudo\n# uncomment following line to enable Touch ID for sudo\n{PAM_LINE}\n"
        );
        assert_eq!(plan(None, Some(TEMPLATE_TEXT)), Plan::Write(expected));
    }

    #[test]
    fn an_active_line_is_already_enabled() {
        let local = "auth       sufficient     pam_tid.so\n";
        assert_eq!(plan(Some(local), Some(TEMPLATE_TEXT)), Plan::AlreadyEnabled);
        assert_eq!(plan(Some(local), None), Plan::AlreadyEnabled);
    }

    #[test]
    fn appends_after_pam_reattach() {
        let local = "auth optional /opt/homebrew/lib/pam/pam_reattach.so\n";
        assert_eq!(
            plan(Some(local), None),
            Plan::Write(format!(
                "auth optional /opt/homebrew/lib/pam/pam_reattach.so\n{PAM_LINE}\n"
            ))
        );
    }

    #[test]
    fn without_any_file_writes_only_the_line() {
        assert_eq!(plan(None, None), Plan::Write(format!("{PAM_LINE}\n")));
    }

    #[test]
    fn finds_the_sudo_local_include() {
        let sudo = "# sudo: auth account password session\nauth       include        sudo_local\nauth       sufficient     pam_smartcard.so\nauth       required       pam_opendirectory.so\n";
        assert!(includes_local(sudo));
        assert!(!includes_local(
            &sudo.replace("auth       include", "#auth       include")
        ));
        assert!(!includes_local(
            "auth       required       pam_opendirectory.so\n"
        ));
    }
}
