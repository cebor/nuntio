//! herdr and tmux: finding them, listing their sessions for the shell menu,
//! forwarding pane shortcuts into them and following herdr's agents.

pub mod agents;
pub mod entries;
pub mod herdr;
pub mod keys;
pub mod launch;
pub mod project;
pub mod resolve;
pub mod run;
pub mod service;
pub mod tmux;

use std::time::Duration;

use nuntio_config::Config;
use nuntio_term::Rgb;

use self::agents::SessionKey;
use self::herdr::AgentStatus;
use crate::style::{AGENT_BLOCKED, AGENT_DONE, AGENT_WORKING};
use crate::window::Pane;
use crate::wsl::Distro;

/// How long asking a multiplexer for its focused pane's directory may take.
const DIR_TIMEOUT: Duration = Duration::from_secs(2);

/// Variables that tell herdr and tmux they run inside themselves.
const INHERITED_ENV: [&str; 9] = [
    "HERDR_ENV",
    "HERDR_SOCKET_PATH",
    "HERDR_BIN_PATH",
    "HERDR_PANE_ID",
    "HERDR_TAB_ID",
    "HERDR_WORKSPACE_ID",
    "HERDR_SESSION",
    "TMUX",
    "TMUX_PANE",
];

/// Remove the variables of an enclosing herdr or tmux from nuntio's own
/// environment. nuntio started from a herdr or tmux pane would hand them to
/// its own panes, where herdr then refuses to start ("nested herdr is
/// disabled"). Call it first thing in `main`, before any thread exists.
pub fn scrub_inherited_env() {
    for name in INHERITED_ENV {
        // SAFETY: no other thread exists yet.
        unsafe { std::env::remove_var(name) };
    }
}

/// The color that marks agents in `status`; none for idle ones.
pub fn agent_color(status: AgentStatus) -> Option<Rgb> {
    match status {
        AgentStatus::Blocked => Some(AGENT_BLOCKED),
        AgentStatus::Done => Some(AGENT_DONE),
        AgentStatus::Working => Some(AGENT_WORKING),
        AgentStatus::Idle | AgentStatus::Unknown => None,
    }
}

/// A terminal multiplexer nuntio works with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tool {
    Herdr,
    Tmux,
}

impl Tool {
    /// The tool whose client runs as `name` (`/proc/<pid>/comm`, the
    /// executable's name on other systems).
    pub fn from_process(name: &str) -> Option<Tool> {
        if name.starts_with("herdr") {
            Some(Tool::Herdr)
        } else if name.starts_with("tmux") {
            Some(Tool::Tmux)
        } else {
            None
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Tool::Herdr => "herdr",
            Tool::Tmux => "tmux",
        }
    }

    /// What `config` says about this tool.
    pub fn config(self, config: &Config) -> ToolConfig<'_> {
        match self {
            Tool::Herdr => ToolConfig {
                enabled: config.herdr.enabled,
                on_startup: config.herdr.on_startup,
                program: &config.herdr.program,
                forward_actions: config.herdr.forward_actions,
            },
            Tool::Tmux => ToolConfig {
                enabled: config.tmux.enabled,
                on_startup: config.tmux.on_startup,
                program: &config.tmux.program,
                forward_actions: config.tmux.forward_actions,
            },
        }
    }
}

/// The settings herdr and tmux have in common (`[herdr]`, `[tmux]`).
#[derive(Debug, Clone, Copy)]
pub struct ToolConfig<'a> {
    /// The tool is on: its sessions are listed and its panes known.
    pub enabled: bool,
    /// The first pane at startup runs the tool.
    pub on_startup: bool,
    /// A program name searched for, or an absolute path.
    pub program: &'a str,
    /// Shortcuts go into the tool running in a pane.
    pub forward_actions: bool,
}

/// Where a tool runs: on this system or in a WSL distribution.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Target {
    Local,
    Wsl(Distro),
}

impl Target {
    /// Where the programs of a shell run: in its WSL distribution, if it
    /// has one.
    pub fn of(wsl: Option<&Distro>) -> Self {
        wsl.map_or(Target::Local, |distro| Target::Wsl(distro.clone()))
    }
}

/// A multiplexer session in a pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MuxPane {
    pub tool: Tool,
    pub target: Target,
    /// `None`: herdr's default session, or a tmux session nuntio doesn't
    /// know.
    pub session: Option<String>,
}

impl MuxPane {
    /// The herdr session this pane shows; `None` for other tools.
    pub fn agent_key(&self) -> Option<SessionKey> {
        (self.tool == Tool::Herdr).then(|| SessionKey {
            target: self.target.clone(),
            session: self.session.clone(),
        })
    }

    /// The working directory of the session's focused pane, a path in
    /// `target`: of herdr's foreground process there, tmux's
    /// `pane_current_path`. Runs `program`, the tool's client, so it blocks.
    pub fn focused_dir(&self, program: &str) -> Option<String> {
        let session = self.session.as_deref();
        let argv = match self.tool {
            Tool::Herdr => {
                let mut argv = launch::herdr(program, session);
                argv.extend(["pane", "list"]);
                argv
            }
            Tool::Tmux => {
                let mut argv = vec![program, "display-message", "-p"];
                if let Some(session) = session {
                    argv.extend(["-t", session]);
                }
                argv.push("#{pane_current_path}");
                argv
            }
        };
        let out = run::run(&self.target, argv[0], &argv[1..], DIR_TIMEOUT)?;
        match self.tool {
            Tool::Herdr => herdr::parse_focused_dir(&out),
            Tool::Tmux => Some(out.trim().to_owned()).filter(|dir| !dir.is_empty()),
        }
    }
}

/// The multiplexer the first pane starts in: herdr before tmux, each only
/// while it is enabled.
pub fn startup_tool(config: &Config) -> Option<Tool> {
    [Tool::Herdr, Tool::Tmux].into_iter().find(|&tool| {
        let tool = tool.config(config);
        tool.enabled && tool.on_startup
    })
}

/// The multiplexer running in `pane`, if its tool is enabled: the session
/// nuntio started there, or one found by the foreground process's name.
pub fn pane_mux(pane: &Pane, config: &Config) -> Option<MuxPane> {
    if let Some(mux) = &pane.mux
        && mux.tool.config(config).enabled
    {
        return Some(mux.clone());
    }
    let tool = Tool::from_process(&pane.term.process_name())?;
    tool.config(config).enabled.then(|| MuxPane {
        tool,
        target: Target::of(pane.wsl.as_ref()),
        session: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_tool_prefers_herdr_and_needs_enabled() {
        let mut config = Config::default();
        assert_eq!(startup_tool(&config), None);

        config.tmux.on_startup = true;
        assert_eq!(startup_tool(&config), None, "tmux not enabled");
        config.tmux.enabled = true;
        assert_eq!(startup_tool(&config), Some(Tool::Tmux));

        config.herdr.on_startup = true;
        assert_eq!(startup_tool(&config), Some(Tool::Tmux), "herdr not enabled");
        config.herdr.enabled = true;
        assert_eq!(startup_tool(&config), Some(Tool::Herdr));
    }
}
