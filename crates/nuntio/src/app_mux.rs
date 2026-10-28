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
use crate::event::{PaneId, UserEvent};
use crate::input;
use crate::menu::{MenuEntry, MenuKind, MenuSection};
use crate::mux::agents::{AgentsUpdate, SessionKey};
use crate::mux::entries::{self, MuxEntry, remote_session};
use crate::mux::herdr::{self, Agent, summary};
use crate::mux::keys::MuxAction;
use crate::mux::launch;
use crate::mux::project::last_component;
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
                let argv = launch::owned(launch::attach(tool, program, session.as_deref()));
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
                let argv = launch::owned(launch::herdr_remote(
                    program,
                    &machine.target,
                    &machine.session,
                ));
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
                let argv = launch::owned(launch::tmux_new_session(program, &name, Some(&dir)));
                MuxOpen {
                    tool: Tool::Tmux,
                    argv,
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

    /// The tab with a pane that runs the multiplexer session `wanted`.
    fn mux_tab(&self, state: &WindowState, wanted: &MuxPane) -> Option<usize> {
        state.tabs.position(|content| {
            content
                .panes
                .iter()
                .any(|p| pane_mux(p, &self.config).as_ref() == Some(wanted))
        })
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
        if let Some(index) = self.mux_tab(state, &wanted) {
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
            Tool::Herdr => (launch::herdr(&program, None), None),
            Tool::Tmux => (
                launch::tmux_new_session(&program, STARTUP_TMUX_SESSION, None),
                Some(STARTUP_TMUX_SESSION.to_owned()),
            ),
        };
        let launch = mux_launch(tool, &target, &launch::owned(argv))?;
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
            let newly_blocked = herdr::newly_blocked(&before, &agents);
            if !newly_blocked.is_empty() {
                let tab = self.mux_tab(state, &key.pane());
                let tab_active = tab == Some(state.tabs.active_index());
                if window::wants_attention(tab_active, state.focused, false) {
                    let messages = herdr::needs_input_messages(&newly_blocked);
                    self.notify(Banner::new(Severity::Info, Topic::Agents, messages));
                }
            }
        }
        self.show_agents(state);
    }

    /// Ask `mux`, running in `pane`, for its focused pane's directory on a
    /// worker thread: it runs the tool's client, in WSL through `wsl.exe`.
    pub(super) fn look_up_mux_dir(&self, pane: PaneId, mux: MuxPane) {
        let program = self
            .mux
            .snapshot
            .program(mux.tool, &mux.target)
            .map(str::to_owned);
        let proxy = self.proxy.clone();
        let spawned = std::thread::Builder::new()
            .name("mux-directory".into())
            .spawn(move || {
                let dir = program.and_then(|program| mux.focused_dir(&program));
                let _ = proxy.send_event(UserEvent::MuxDir(pane, dir));
            });
        if let Err(err) = spawned {
            tracing::warn!("can't ask the multiplexer for its directory: {err}");
        }
    }

    /// The multiplexer in `pane` answered with its focused pane's directory.
    pub(super) fn mux_dir_found(
        &mut self,
        state: Option<&mut WindowState>,
        pane: PaneId,
        dir: Option<String>,
    ) {
        if let Some(state) = state
            && state.mouse.mux_dir_found(pane, dir)
        {
            self.update_hover_link(state);
        }
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
                label: format!(
                    "{} · {}",
                    agent.agent,
                    last_component(&agent.cwd).unwrap_or_default()
                ),
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
        let argv = launch::owned(launch::herdr(&agent.program, agent.key.session.as_deref()));
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
