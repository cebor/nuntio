//! herdr and tmux in the app: the service that watches them, shortcuts
//! forwarded into them, their entries in the shell menu, and herdr's agents
//! in the status bar, the tab bar, a menu and banners.

use std::collections::HashMap;
use std::path::PathBuf;

use nuntio_config::Shell;

use super::Core;
use super::app_panes::StartDir;
use crate::actions::Action;
use crate::banner::{Banner, Severity, Topic};
use crate::input;
use crate::menu::{MenuEntry, MenuKind, MenuSection};
use crate::mux::agents::{AgentsUpdate, SessionKey};
use crate::mux::entries::{self, MuxEntry, remote_session};
use crate::mux::herdr::{Agent, AgentStatus, summary};
use crate::mux::keys::MuxAction;
use crate::mux::resolve::resolve;
use crate::mux::service::{
    AgentRef, MuxOpen, MuxService, MuxSettings, MuxSnapshot, ProjectDir, Request,
};
use crate::mux::{MuxPane, Target, Tool, agent_color, pane_mux};
use crate::shells::{Launch, ShellChoice};
use crate::window::{self, WindowState};

/// What the app knows about the multiplexers.
#[derive(Default)]
pub(super) struct MuxState {
    /// Runs while herdr or tmux is enabled.
    service: Option<MuxService>,
    snapshot: MuxSnapshot,
    agents: HashMap<SessionKey, Vec<Agent>>,
    /// The multiplexer entries of the open shell menu;
    /// `Action::OpenMuxEntry` indexes them.
    entries: Vec<MuxEntry>,
    /// The agents of the open agents menu; `Action::FocusAgent` indexes
    /// them.
    agent_choices: Vec<AgentRef>,
}

/// The tmux session the first pane attaches to (or creates) with
/// `tmux.on_startup`.
const STARTUP_TMUX_SESSION: &str = "main";

/// What a pane runs to start the client `argv` (never empty) of `tool` in
/// `target`: the command here, or through the WSL shell machinery.
fn mux_launch(tool: Tool, target: &Target, argv: &[String]) -> Option<Launch> {
    let (program, args) = argv.split_first()?;
    Some(match target {
        Target::Local => Launch::Command(argv.to_vec()),
        Target::Wsl(distro) => Launch::Shell(ShellChoice {
            name: tool.name().to_owned(),
            shell: Some(Shell {
                program: Some(program.clone()),
                args: args.to_vec(),
                wsl: Some(distro.name.clone()),
                wsl_user: distro.user.clone(),
            }),
        }),
    })
}

/// The multiplexer action that `action` becomes in a herdr or tmux pane.
/// Switching and closing nuntio's tabs stays with nuntio, so the
/// multiplexer's tab can always be left.
fn mux_action(action: Action) -> Option<MuxAction> {
    Some(match action {
        Action::NewTab => MuxAction::NewTab,
        Action::SplitVertical => MuxAction::SplitVertical,
        Action::SplitHorizontal => MuxAction::SplitHorizontal,
        Action::ClosePane => MuxAction::ClosePane,
        Action::FocusPane(d) => MuxAction::Focus(d),
        Action::ResizePane(d) => MuxAction::Resize(d),
        Action::ZoomPane => MuxAction::Zoom,
        _ => return None,
    })
}

/// The last component of a path, Linux or Windows.
fn basename(path: &str) -> &str {
    let path = path.trim_end_matches(['/', '\\']);
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

impl Core {
    /// Start, restart or stop the multiplexer service to match the config.
    pub(super) fn sync_mux(&mut self, state: Option<&mut WindowState>) {
        let wanted = MuxSettings::of(&self.config);
        if wanted.as_ref() == self.mux.service.as_ref().map(MuxService::settings) {
            return;
        }
        self.mux = MuxState::default();
        if let Some(settings) = wanted {
            tracing::debug!(?settings, "starting multiplexer service");
            self.mux.service = Some(MuxService::start(settings, self.proxy.clone()));
        }
        self.show_agents(state);
    }

    /// Whether the multiplexer service runs; events of a stopped one are
    /// late and ignored.
    pub(super) fn mux_running(&self) -> bool {
        self.mux.service.is_some()
    }

    /// Send `action` to herdr or tmux in the focused pane as that tool's
    /// own keys. `false` when the pane runs neither, forwarding is off or
    /// the tool has no key for it: nuntio does the action then.
    pub(super) fn forward_to_mux(&mut self, state: &mut WindowState, action: Action) -> bool {
        let Some(mux_action) = mux_action(action) else {
            return false;
        };
        let pane = state.content().focused_pane();
        let Some(mux) = pane_mux(pane, &self.config) else {
            return false;
        };
        if !mux.tool.config(&self.config).forward_actions {
            return false;
        }
        let bindings = self.mux.snapshot.bindings(mux.tool, &mux.target);
        let Some(combos) = bindings.get(mux_action) else {
            return false;
        };
        let mode = pane.term.mode();
        let bytes: Vec<u8> = combos
            .iter()
            .flat_map(|combo| input::chord_bytes(combo, mode))
            .collect();
        tracing::debug!(?action, tool = mux.tool.name(), "forwarding to multiplexer");
        state.type_bytes(bytes);
        true
    }

    /// The multiplexer sections of the shell menu from what the service
    /// last found; remembers their entries.
    pub(super) fn mux_sections(&mut self, state: &WindowState) -> Vec<MenuSection> {
        if self.mux.service.is_none() {
            self.mux.entries.clear();
            return Vec::new();
        }
        let open: Vec<MuxPane> = state
            .panes()
            .filter_map(|p| pane_mux(p, &self.config))
            .collect();
        let (sections, found) =
            entries::entries(&self.mux.snapshot, &self.mux.agents, &open, &self.config);
        self.mux.entries = found;
        sections
    }

    /// Ask the service to look again, with the focused pane's project.
    pub(super) fn refresh_mux(&mut self, state: &WindowState) {
        if self.mux.service.is_none() {
            return;
        }
        let pane = state.content().focused_pane();
        let target = Target::of(pane.wsl.as_ref());
        let distro = pane.wsl.clone();
        let dir = match self.focused_dir(state, distro.as_ref()) {
            Some(StartDir::Local(path)) => Some(path.to_string_lossy().into_owned()),
            Some(StartDir::Wsl(dir)) if dir.starts_with('~') => {
                distro.and_then(|d| self.wsl_absolute(&d, &dir))
            }
            Some(StartDir::Wsl(dir)) => Some(dir),
            None => None,
        };
        let project = dir.map(|dir| ProjectDir { target, dir });
        if let Some(service) = &self.mux.service {
            service.send(Request::Refresh { project });
        }
    }

    /// The service found something new.
    pub(super) fn mux_snapshot(&mut self, state: Option<&mut WindowState>, snapshot: MuxSnapshot) {
        if self.mux.service.is_none() {
            return;
        }
        self.mux.snapshot = snapshot;
        let Some(state) = state else { return };
        if state.menu_kind() == Some(MenuKind::Shells) {
            let sections = self.mux_sections(state);
            let names: Vec<String> = self.shell_choices.iter().map(|c| c.name.clone()).collect();
            state.refresh_shell_menu(&self.config, &names, &sections, &self.bindings);
        }
    }

    /// Open entry `index` of the shell menu's multiplexer sections.
    pub(super) fn open_mux_entry(&mut self, state: &mut WindowState, index: usize) {
        let Some(entry) = self.mux.entries.get(index).cloned() else {
            return;
        };
        let snapshot = &self.mux.snapshot;
        let open = match entry {
            MuxEntry::Attach {
                tool,
                target,
                session,
            } => {
                let Some(program) = snapshot.program(tool, &target) else {
                    return;
                };
                let mut argv = vec![program.to_owned()];
                match (tool, &session) {
                    (Tool::Herdr, Some(s)) => argv.extend(["--session".into(), s.clone()]),
                    (Tool::Herdr, None) => {}
                    (Tool::Tmux, Some(s)) => {
                        argv.extend(["attach-session".into(), "-t".into(), s.clone()]);
                    }
                    (Tool::Tmux, None) => argv.push("attach-session".into()),
                }
                MuxOpen {
                    tool,
                    target,
                    session,
                    argv,
                    dir: None,
                }
            }
            MuxEntry::Remote { machine } => {
                let Some(program) = snapshot.program(Tool::Herdr, &Target::Local) else {
                    return;
                };
                let mut argv = vec![
                    program.to_owned(),
                    "--remote".into(),
                    machine.target.clone(),
                ];
                if !machine.session.is_empty() && machine.session != "default" {
                    argv.extend(["--session".into(), machine.session.clone()]);
                }
                MuxOpen {
                    tool: Tool::Herdr,
                    target: Target::Local,
                    session: Some(remote_session(&machine)),
                    argv,
                    dir: None,
                }
            }
            MuxEntry::TmuxProject { target, name, dir } => {
                let Some(program) = snapshot.program(Tool::Tmux, &target) else {
                    return;
                };
                MuxOpen {
                    tool: Tool::Tmux,
                    argv: [program, "new-session", "-A", "-s", &name, "-c", &dir]
                        .map(str::to_owned)
                        .to_vec(),
                    target,
                    session: Some(name),
                    dir: Some(dir),
                }
            }
            MuxEntry::HerdrProject(project) => {
                let running = snapshot
                    .target(&project.target)
                    .and_then(|t| t.herdr.as_ref())
                    .is_some_and(|h| h.sessions.iter().any(|s| s.default && s.running));
                if running && let Some(service) = &self.mux.service {
                    // The tab opens once the workspace is there (`MuxOpen`).
                    service.send(Request::OpenProject(project));
                    state.close_menu();
                    return;
                }
                // herdr makes its first workspace from the directory.
                MuxOpen {
                    tool: Tool::Herdr,
                    target: project.target,
                    session: None,
                    argv: vec![project.program],
                    dir: Some(project.dir),
                }
            }
        };
        self.open_mux(state, open);
    }

    /// Switch to the tab that shows `open`'s session, or open a tab with
    /// its client.
    pub(super) fn open_mux(&mut self, state: &mut WindowState, open: MuxOpen) {
        let MuxOpen {
            tool,
            target,
            session,
            argv,
            dir,
        } = open;
        let wanted = MuxPane {
            tool,
            target,
            session,
        };
        let config = &self.config;
        if let Some(index) = state.tabs.position(|content| {
            content
                .panes
                .iter()
                .any(|p| pane_mux(p, config).as_ref() == Some(&wanted))
        }) {
            state.select_tab(index);
            state.close_menu();
            return;
        }
        let Some(launch) = mux_launch(tool, &wanted.target, &argv) else {
            return;
        };
        let distro = launch.distro();
        let dir = match dir {
            Some(dir) if distro.is_some() => Some(StartDir::Wsl(dir)),
            Some(dir) => Some(StartDir::Local(PathBuf::from(dir))),
            None => self.start_dir(state, distro.as_ref(), false),
        };
        state.close_menu();
        let placement = self.shell_menu_placement;
        if let Some(id) = self.new_tab_in(state, launch, placement, dir)
            && let Some(pane) = state.content_mut().pane_mut(id)
        {
            pane.mux = Some(wanted);
        }
    }

    /// The client the first pane runs instead of the shell (`on_startup`),
    /// and the session it counts as. `None` when no tool asks for it, or
    /// with a banner when the program isn't found.
    pub(super) fn startup_mux(&mut self) -> Option<(Launch, MuxPane)> {
        let tool = crate::mux::startup_tool(&self.config)?;
        let target = Target::of(self.default_launch().distro().as_ref());
        let configured = tool.config(&self.config).program;
        let Some(program) = resolve(&target, &[(tool, configured)]).remove(&tool) else {
            self.notify(Banner::new(
                Severity::Warning,
                Topic::Shell,
                vec![format!(
                    "{} not found; started the shell instead",
                    tool.name()
                )],
            ));
            return None;
        };
        let (argv, session) = match tool {
            Tool::Herdr => (vec![program], None),
            Tool::Tmux => (
                [
                    program.as_str(),
                    "new-session",
                    "-A",
                    "-s",
                    STARTUP_TMUX_SESSION,
                ]
                .map(str::to_owned)
                .to_vec(),
                Some(STARTUP_TMUX_SESSION.to_owned()),
            ),
        };
        let launch = mux_launch(tool, &target, &argv)?;
        Some((
            launch,
            MuxPane {
                tool,
                target,
                session,
            },
        ))
    }

    /// The agents of a herdr session changed.
    pub(super) fn agents_update(
        &mut self,
        mut state: Option<&mut WindowState>,
        update: AgentsUpdate,
    ) {
        if self.mux.service.is_none() {
            return;
        }
        let AgentsUpdate { key, agents } = update;
        let before = if agents.is_empty() {
            self.mux.agents.remove(&key)
        } else {
            self.mux.agents.insert(key.clone(), agents.clone())
        }
        .unwrap_or_default();
        if let Some(state) = state.as_deref_mut()
            && self.config.herdr.agent_banner
        {
            let newly_blocked: Vec<&Agent> = agents
                .iter()
                .filter(|a| a.status == AgentStatus::Blocked)
                .filter(|a| {
                    !before
                        .iter()
                        .any(|b| b.pane_id == a.pane_id && b.status == AgentStatus::Blocked)
                })
                .collect();
            if !newly_blocked.is_empty() {
                let config = &self.config;
                let tab = state.tabs.position(|content| {
                    content.panes.iter().any(|p| {
                        pane_mux(p, config).is_some_and(|m| {
                            m.tool == Tool::Herdr
                                && m.target == key.target
                                && m.session == key.session
                        })
                    })
                });
                let tab_active = tab == Some(state.tabs.active_index());
                if window::wants_attention(tab_active, state.focused, false) {
                    let messages = newly_blocked
                        .iter()
                        .map(|a| format!("{} in {} needs input", a.agent, basename(&a.cwd)))
                        .collect();
                    self.notify(Banner::new(Severity::Info, Topic::Agents, messages));
                }
            }
        }
        self.show_agents(state);
    }

    /// Show the agents in the status bar item and the tab markers.
    fn show_agents(&mut self, state: Option<&mut WindowState>) {
        let on = self.mux.service.is_some() && self.config.herdr.enabled;
        let all: Vec<Agent> = if on {
            self.mux.agents.values().flatten().cloned().collect()
        } else {
            Vec::new()
        };
        let most_urgent = all.iter().map(|a| a.status).max();
        let item = summary(&all).zip(most_urgent.and_then(agent_color));
        self.stats.set_agents(item);
        if let Some(state) = state {
            state.agent_status = if on {
                self.mux
                    .agents
                    .iter()
                    .filter_map(|(key, agents)| {
                        Some((key.clone(), agents.iter().map(|a| a.status).max()?))
                    })
                    .collect()
            } else {
                HashMap::new()
            };
            state.window.request_redraw();
        }
    }

    /// Open, or close if it is open, the list of herdr agents.
    pub(super) fn open_agents_menu(&mut self, state: &mut WindowState) {
        if state.menu_kind() == Some(MenuKind::Agents) {
            state.close_menu();
            return;
        }
        let mut agents: Vec<(&SessionKey, &Agent)> = self
            .mux
            .agents
            .iter()
            .flat_map(|(key, agents)| agents.iter().map(move |a| (key, a)))
            .collect();
        agents.sort_by(|a, b| b.1.status.cmp(&a.1.status).then(a.1.agent.cmp(&b.1.agent)));
        let mut entries = Vec::new();
        self.mux.agent_choices.clear();
        for (key, agent) in agents {
            let mut detail = agent.status.word().to_owned();
            if let Target::Wsl(distro) = &key.target {
                detail.push_str(&format!(" · {}", distro.name));
            }
            entries.push(MenuEntry {
                label: format!("{} · {}", agent.agent, basename(&agent.cwd)),
                detail: Some(detail),
                current: false,
                action: Action::FocusAgent(self.mux.agent_choices.len()),
            });
            self.mux.agent_choices.push(AgentRef {
                key: key.clone(),
                program: self
                    .mux
                    .snapshot
                    .program(Tool::Herdr, &key.target)
                    .unwrap_or("herdr")
                    .to_owned(),
                pane_id: agent.pane_id.clone(),
            });
        }
        state.open_agents_menu(&self.config, &self.stats, &entries);
    }

    /// Switch to (or open) the tab of agent `index` of the agents menu and
    /// focus it in herdr.
    pub(super) fn focus_agent(&mut self, state: &mut WindowState, index: usize) {
        let Some(agent) = self.mux.agent_choices.get(index).cloned() else {
            return;
        };
        let mut argv = vec![agent.program.clone()];
        if let Some(session) = &agent.key.session {
            argv.extend(["--session".to_owned(), session.clone()]);
        }
        self.open_mux(
            state,
            MuxOpen {
                tool: Tool::Herdr,
                target: agent.key.target.clone(),
                session: agent.key.session.clone(),
                argv,
                dir: None,
            },
        );
        if let Some(service) = &self.mux.service {
            service.send(Request::FocusAgent(agent));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basenames_of_linux_and_windows_paths() {
        assert_eq!(basename("/home/me/code/nuntio"), "nuntio");
        assert_eq!(basename("/home/me/code/nuntio/"), "nuntio");
        assert_eq!(basename(r"C:\Users\me\x"), "x");
        assert_eq!(basename("x"), "x");
    }

    #[test]
    fn tab_switching_stays_with_nuntio() {
        use crate::pane_tree::Direction;
        assert_eq!(mux_action(Action::NewTab), Some(MuxAction::NewTab));
        assert_eq!(
            mux_action(Action::FocusPane(Direction::Left)),
            Some(MuxAction::Focus(Direction::Left))
        );
        for action in [
            Action::NextTab,
            Action::PreviousTab,
            Action::SelectTab(0),
            Action::CloseTab,
        ] {
            assert_eq!(mux_action(action), None);
        }
    }
}
