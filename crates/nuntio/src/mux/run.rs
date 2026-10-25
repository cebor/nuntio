//! Running a multiplexer's command line, on this system or in WSL.

use std::process::Command;
use std::time::Duration;

use super::Target;
use crate::command::{no_window, output_within};

/// `program` with `args` in `target`: here, or in a WSL distribution
/// through `wsl.exe --exec`.
pub fn command(target: &Target, program: &str, args: &[&str]) -> Command {
    let mut command = match target {
        Target::Local => Command::new(program),
        Target::Wsl(distro) => {
            let mut command = Command::new("wsl.exe");
            command.args(["-d", &distro.name]);
            if let Some(user) = &distro.user {
                command.args(["-u", user]);
            }
            command.args(["--exec", program]);
            command
        }
    };
    command.args(args);
    no_window(&mut command);
    command
}

/// The output of `program args` in `target`, if it succeeds within
/// `timeout`.
pub fn run(target: &Target, program: &str, args: &[&str], timeout: Duration) -> Option<String> {
    let mut command = command(target, program, args);
    match output_within(&mut command, timeout) {
        Ok(Some(output)) if output.status.success() => {
            Some(String::from_utf8_lossy(&output.stdout).into_owned())
        }
        Ok(Some(output)) => {
            tracing::debug!(program, ?args, status = %output.status, "multiplexer command failed");
            None
        }
        Ok(None) => {
            tracing::debug!(program, ?args, "multiplexer command timed out");
            None
        }
        Err(err) => {
            tracing::debug!(program, ?args, "can't run multiplexer command: {err}");
            None
        }
    }
}
