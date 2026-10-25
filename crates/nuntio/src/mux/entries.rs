//! The multiplexer sections of the shell menu, from what the service found.

use std::collections::HashMap;

use nuntio_config::Config;

use super::agents::SessionKey;
use super::herdr::{Agent, Machine, summary};
use super::project::TmuxProjectSession;
use super::service::{MuxSnapshot, ProjectEntry, TargetState};
use super::{MuxPane, Target, Tool};
use crate::actions::Action;
use crate::menu::{MenuEntry, MenuSection};

/// What an entry of a multiplexer section opens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MuxEntry {
    /// A running session; `None` is herdr's default one.
    Attach {
        tool: Tool,
        target: Target,
        session: Option<String>,
    },
    /// A herdr workspace for the project.
    HerdrProject(ProjectEntry),
    /// The tmux session for the project, created if needed.
    TmuxProject {
        target: Target,
        name: String,
        dir: String,
    },
    /// herdr on a saved machine, over SSH.
    Remote { machine: Machine },
}

/// The session a remote entry's pane counts as.
pub fn remote_session(machine: &Machine) -> String {
    format!("remote:{}", machine.id)
}

/// The sections for the shell menu and the entries that
/// `Action::OpenMuxEntry(i)` indexes. `open` are the multiplexer sessions
/// in nuntio's panes; their entries are marked.
pub fn entries(
    snapshot: &MuxSnapshot,
    agents: &HashMap<SessionKey, Vec<Agent>>,
    open: &[MuxPane],
    config: &Config,
) -> (Vec<MenuSection>, Vec<MuxEntry>) {
    let mut sections = Vec::new();
    let mut found = Vec::new();
    let push = |found: &mut Vec<MuxEntry>, entry: MuxEntry, label, detail, current| {
        let action = Action::OpenMuxEntry(found.len());
        found.push(entry);
        MenuEntry {
            label,
            detail,
            current,
            action,
        }
    };
    let is_open = |tool, target: &Target, session: Option<&str>| {
        open.iter()
            .any(|p| p.tool == tool && &p.target == target && p.session.as_deref() == session)
    };
    for tool in [Tool::Herdr, Tool::Tmux] {
        if !tool.config(config).enabled {
            continue;
        }
        for state in &snapshot.targets {
            let target = &state.target;
            let project = snapshot.project.as_ref().filter(|p| &p.target == target);
            let mut items = Vec::new();
            match tool {
                Tool::Herdr => {
                    let Some(herdr) = &state.herdr else { continue };
                    let mut sessions: Vec<_> =
                        herdr.sessions.iter().filter(|s| s.running).collect();
                    sessions.sort_by_key(|s| !s.default);
                    for session in &sessions {
                        let name = (!session.default).then(|| session.name.clone());
                        let key = SessionKey {
                            target: target.clone(),
                            session: name.clone(),
                        };
                        let detail = agents
                            .get(&key)
                            .and_then(|a| summary(a))
                            .unwrap_or_else(|| "running".into());
                        let current = is_open(Tool::Herdr, target, name.as_deref());
                        items.push(push(
                            &mut found,
                            MuxEntry::Attach {
                                tool,
                                target: target.clone(),
                                session: name,
                            },
                            session.name.clone(),
                            Some(detail),
                            current,
                        ));
                    }
                    if let Some(project) = project {
                        let detail = if project.herdr_workspace.is_some() {
                            "workspace"
                        } else {
                            "new workspace"
                        };
                        items.push(push(
                            &mut found,
                            MuxEntry::HerdrProject(ProjectEntry {
                                target: target.clone(),
                                program: herdr.program.clone(),
                                dir: project.dir.clone(),
                                name: project.name.clone(),
                                workspace: project.herdr_workspace.clone(),
                            }),
                            project.name.clone(),
                            Some(detail.into()),
                            false,
                        ));
                    }
                    if *target == Target::Local {
                        for machine in &herdr.machines {
                            let detail = match herdr
                                .machine_agents
                                .get(&machine.id)
                                .and_then(|a| summary(a))
                            {
                                Some(agents) => format!("remote · {agents}"),
                                None => "remote".into(),
                            };
                            let current = is_open(
                                Tool::Herdr,
                                &Target::Local,
                                Some(&remote_session(machine)),
                            );
                            items.push(push(
                                &mut found,
                                MuxEntry::Remote {
                                    machine: machine.clone(),
                                },
                                machine.label.clone(),
                                Some(detail),
                                current,
                            ));
                        }
                    }
                    if sessions.is_empty() && project.is_none() {
                        items.push(push(
                            &mut found,
                            MuxEntry::Attach {
                                tool,
                                target: target.clone(),
                                session: None,
                            },
                            "herdr".into(),
                            Some("start".into()),
                            false,
                        ));
                    }
                }
                Tool::Tmux => {
                    let Some(tmux) = &state.tmux else { continue };
                    for session in &tmux.sessions {
                        let mut detail = format!(
                            "{} window{}",
                            session.windows,
                            if session.windows == 1 { "" } else { "s" }
                        );
                        if session.attached {
                            detail.push_str(" · attached");
                        }
                        let current = is_open(Tool::Tmux, target, Some(&session.name));
                        items.push(push(
                            &mut found,
                            MuxEntry::Attach {
                                tool,
                                target: target.clone(),
                                session: Some(session.name.clone()),
                            },
                            session.name.clone(),
                            Some(detail),
                            current,
                        ));
                    }
                    if let Some(project) = project
                        && let Some(session) = &project.tmux_session
                    {
                        let name = session.name().to_owned();
                        let detail = match session {
                            TmuxProjectSession::Existing(_) => "session",
                            TmuxProjectSession::New(_) => "new session",
                        };
                        let current = is_open(Tool::Tmux, target, Some(&name));
                        items.push(push(
                            &mut found,
                            MuxEntry::TmuxProject {
                                target: target.clone(),
                                name: name.clone(),
                                dir: project.dir.clone(),
                            },
                            name,
                            Some(detail.into()),
                            current,
                        ));
                    }
                }
            }
            if !items.is_empty() {
                sections.push(MenuSection {
                    heading: heading(tool, state),
                    entries: items,
                });
            }
        }
    }
    (sections, found)
}

fn heading(tool: Tool, state: &TargetState) -> String {
    match &state.target {
        Target::Local => tool.name().to_owned(),
        Target::Wsl(distro) => format!("{} · {}", tool.name(), distro.name),
    }
}
