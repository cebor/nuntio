//! herdr and tmux: finding them, listing their sessions for the shell menu,
//! forwarding pane shortcuts into them and following herdr's agents.

pub mod agents;
pub mod entries;
pub mod herdr;
pub mod keys;
pub mod project;
pub mod resolve;
pub mod run;
pub mod service;
pub mod tmux;

use nuntio_config::Config;
use nuntio_term::Rgb;

use self::herdr::AgentStatus;
use crate::style::{AGENT_BLOCKED, AGENT_DONE, AGENT_WORKING};
use crate::window::Pane;
use crate::wsl::Distro;

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

/// Whether the config turns `tool` on.
pub fn enabled(config: &Config, tool: Tool) -> bool {
    match tool {
        Tool::Herdr => config.herdr.enabled,
        Tool::Tmux => config.tmux.enabled,
    }
}

/// Whether shortcuts go into `tool` running in a pane.
pub fn forwards(config: &Config, tool: Tool) -> bool {
    match tool {
        Tool::Herdr => config.herdr.forward_actions,
        Tool::Tmux => config.tmux.forward_actions,
    }
}

/// The multiplexer the first pane starts in: herdr before tmux, each only
/// while it is enabled.
pub fn startup_tool(config: &Config) -> Option<Tool> {
    [Tool::Herdr, Tool::Tmux].into_iter().find(|&tool| {
        enabled(config, tool)
            && match tool {
                Tool::Herdr => config.herdr.on_startup,
                Tool::Tmux => config.tmux.on_startup,
            }
    })
}

/// The multiplexer running in `pane`, if its tool is enabled: the session
/// nuntio started there, or one found by the foreground process's name.
pub fn pane_mux(pane: &Pane, config: &Config) -> Option<MuxPane> {
    if let Some(mux) = &pane.mux
        && enabled(config, mux.tool)
    {
        return Some(mux.clone());
    }
    let tool = Tool::from_process(&pane.term.process_name())?;
    enabled(config, tool).then(|| MuxPane {
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
