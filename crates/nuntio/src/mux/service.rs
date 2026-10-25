//! The background thread that asks herdr and tmux what runs: sessions,
//! saved machines, key bindings and the project of the focused pane. It
//! owns the agent monitors and stops, with them, when dropped.

use std::collections::HashMap;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

use nuntio_config::Config;
use winit::event_loop::EventLoopProxy;

use super::agents::{Endpoint, Monitor, SessionKey};
use super::herdr::{self, Agent, HerdrSession, Machine};
use super::keys::{self, Bindings};
use super::launch;
use super::project::{TmuxProjectSession, project_name, tmux_project};
use super::resolve::resolve;
use super::run::run;
use super::tmux::{self, TmuxSession};
use super::{Target, Tool};
use crate::event::UserEvent;
use crate::wsl::Distro;

/// How often the sessions are listed while nothing asks.
const POLL: Duration = Duration::from_secs(60);
/// How long the programs found stay valid.
const RESOLVE_AGE: Duration = Duration::from_secs(60);
/// How long a multiplexer command may take.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(5);
/// How long `git` may take to find the repository.
const GIT_TIMEOUT: Duration = Duration::from_secs(2);

/// What the service looks for; `None` turns a tool off.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MuxSettings {
    pub herdr: Option<String>,
    pub tmux: Option<String>,
}

impl MuxSettings {
    /// The settings `config` asks for; `None` when both tools are off.
    pub fn of(config: &Config) -> Option<Self> {
        let program = |tool: Tool| {
            let tool = tool.config(config);
            tool.enabled.then(|| tool.program.trim().to_owned())
        };
        let settings = Self {
            herdr: program(Tool::Herdr),
            tmux: program(Tool::Tmux),
        };
        (settings.herdr.is_some() || settings.tmux.is_some()).then_some(settings)
    }
}

/// A directory to find the project of.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectDir {
    pub target: Target,
    pub dir: String,
}

/// A herdr workspace to focus or create, then attach to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectEntry {
    pub target: Target,
    pub program: String,
    pub dir: String,
    pub name: String,
    pub workspace: Option<String>,
}

/// An agent to focus in its herdr session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentRef {
    pub key: SessionKey,
    pub program: String,
    pub pane_id: String,
}

#[derive(Debug)]
pub enum Request {
    /// List again; with a project dir (the shell menu opened), also find
    /// its project and ask the saved machines for their agents.
    Refresh {
        project: Option<ProjectDir>,
    },
    OpenProject(ProjectEntry),
    FocusAgent(AgentRef),
}

/// A client to open in a new tab, or the tab to switch to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MuxOpen {
    pub tool: Tool,
    pub target: Target,
    pub session: Option<String>,
    pub argv: Vec<String>,
    pub dir: Option<String>,
}

/// What the service found, for the shell menu and forwarding.
#[derive(Debug, Clone, Default)]
pub struct MuxSnapshot {
    pub targets: Vec<TargetState>,
    pub project: Option<Project>,
}

impl MuxSnapshot {
    pub fn target(&self, target: &Target) -> Option<&TargetState> {
        self.targets.iter().find(|t| &t.target == target)
    }

    /// The key bindings of `tool` in `target`, its defaults until they are
    /// known.
    pub fn bindings(&self, tool: Tool, target: &Target) -> Bindings {
        let state = self.target(target);
        let found = match tool {
            Tool::Herdr => state.and_then(|s| s.herdr.as_ref()).map(|h| &h.bindings),
            Tool::Tmux => state.and_then(|s| s.tmux.as_ref()).map(|t| &t.bindings),
        };
        match (found, tool) {
            (Some(bindings), _) => bindings.clone(),
            (None, Tool::Herdr) => Bindings::herdr_defaults(),
            (None, Tool::Tmux) => Bindings::tmux_defaults(),
        }
    }

    /// The program found for `tool` in `target`.
    pub fn program(&self, tool: Tool, target: &Target) -> Option<&str> {
        let state = self.target(target)?;
        match tool {
            Tool::Herdr => state.herdr.as_ref().map(|h| h.program.as_str()),
            Tool::Tmux => state.tmux.as_ref().map(|t| t.program.as_str()),
        }
    }
}

#[derive(Debug, Clone)]
pub struct TargetState {
    pub target: Target,
    pub herdr: Option<HerdrState>,
    pub tmux: Option<TmuxState>,
}

#[derive(Debug, Clone)]
pub struct HerdrState {
    pub program: String,
    pub sessions: Vec<HerdrSession>,
    pub machines: Vec<Machine>,
    /// The agents of each saved machine, by machine id.
    pub machine_agents: HashMap<String, Vec<Agent>>,
    pub bindings: Bindings,
}

#[derive(Debug, Clone)]
pub struct TmuxState {
    pub program: String,
    pub sessions: Vec<TmuxSession>,
    pub bindings: Bindings,
}

/// The project of the focused pane: its Git repository, or its directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Project {
    pub target: Target,
    pub dir: String,
    pub name: String,
    /// The herdr workspace already open there.
    pub herdr_workspace: Option<String>,
    /// The tmux session for it, if tmux is on.
    pub tmux_session: Option<TmuxProjectSession>,
}

/// The service thread; it stops, with the agent monitors, when dropped.
pub struct MuxService {
    settings: MuxSettings,
    requests: mpsc::Sender<Request>,
}

impl MuxService {
    pub fn start(settings: MuxSettings, proxy: EventLoopProxy<UserEvent>) -> Self {
        let (requests, received) = mpsc::channel();
        let worker_settings = settings.clone();
        std::thread::Builder::new()
            .name("multiplexers".into())
            .spawn(move || Worker::new(worker_settings, proxy).serve(&received))
            .expect("spawning a thread");
        Self { settings, requests }
    }

    pub fn settings(&self) -> &MuxSettings {
        &self.settings
    }

    pub fn send(&self, request: Request) {
        let _ = self.requests.send(request);
    }
}

struct Worker {
    settings: MuxSettings,
    proxy: EventLoopProxy<UserEvent>,
    targets: Vec<Target>,
    programs: HashMap<Target, (Instant, HashMap<Tool, String>)>,
    snapshot: MuxSnapshot,
    monitors: HashMap<SessionKey, Monitor>,
}

impl Worker {
    fn new(settings: MuxSettings, proxy: EventLoopProxy<UserEvent>) -> Self {
        let mut targets = vec![Target::Local];
        targets.extend(
            nuntio_config::detect::wsl_distributions()
                .into_iter()
                .map(|d| {
                    Target::Wsl(Distro {
                        name: d.value,
                        user: None,
                    })
                }),
        );
        Self {
            settings,
            proxy,
            targets,
            programs: HashMap::new(),
            snapshot: MuxSnapshot::default(),
            monitors: HashMap::new(),
        }
    }

    fn serve(mut self, requests: &mpsc::Receiver<Request>) {
        self.refresh(None);
        loop {
            match requests.recv_timeout(POLL) {
                Ok(Request::Refresh { mut project }) => {
                    // Menus opened in a row: only the last one matters.
                    while let Ok(next) = requests.try_recv() {
                        match next {
                            Request::Refresh { project: p } => project = p,
                            other => self.handle(other),
                        }
                    }
                    self.refresh(project);
                }
                Ok(other) => self.handle(other),
                Err(RecvTimeoutError::Timeout) => self.refresh(None),
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
    }

    fn handle(&mut self, request: Request) {
        match request {
            Request::Refresh { project } => self.refresh(project),
            Request::OpenProject(entry) => self.open_project(entry),
            Request::FocusAgent(agent) => {
                let argv = launch::herdr_focus_agent(
                    &agent.program,
                    agent.key.session.as_deref(),
                    &agent.pane_id,
                );
                run(&agent.key.target, argv[0], &argv[1..], COMMAND_TIMEOUT);
            }
        }
    }

    /// The programs `target` has, found again once they are old.
    fn programs(&mut self, target: &Target) -> HashMap<Tool, String> {
        if let Some((found, programs)) = self.programs.get(target)
            && found.elapsed() < RESOLVE_AGE
        {
            return programs.clone();
        }
        let mut tools = Vec::new();
        if let Some(program) = &self.settings.herdr {
            tools.push((Tool::Herdr, program.as_str()));
        }
        if let Some(program) = &self.settings.tmux {
            tools.push((Tool::Tmux, program.as_str()));
        }
        let programs = resolve(target, &tools);
        tracing::debug!(?target, ?programs, "multiplexers found");
        self.programs
            .insert(target.clone(), (Instant::now(), programs.clone()));
        programs
    }

    fn refresh(&mut self, project: Option<ProjectDir>) {
        let menu = project.is_some();
        let mut targets = Vec::new();
        for target in self.targets.clone() {
            let programs = self.programs(&target);
            let previous = self.snapshot.target(&target);
            let herdr = programs.get(&Tool::Herdr).map(|program| {
                herdr_state(
                    &target,
                    program,
                    menu,
                    previous.and_then(|p| p.herdr.as_ref()),
                )
            });
            let tmux = programs.get(&Tool::Tmux).map(|program| {
                tmux_state(&target, program, previous.and_then(|p| p.tmux.as_ref()))
            });
            if herdr.is_some() || tmux.is_some() {
                targets.push(TargetState {
                    target,
                    herdr,
                    tmux,
                });
            }
        }
        self.snapshot.targets = targets;
        if let Some(dir) = project {
            self.snapshot.project = Some(self.project(&dir));
        }
        let _ = self
            .proxy
            .send_event(UserEvent::Mux(Box::new(self.snapshot.clone())));
        self.sync_monitors();
    }

    fn project(&self, wanted: &ProjectDir) -> Project {
        let target = &wanted.target;
        let dir = run(
            target,
            "git",
            &["-C", &wanted.dir, "rev-parse", "--show-toplevel"],
            GIT_TIMEOUT,
        )
        .map(|out| out.trim().to_owned())
        .filter(|out| !out.is_empty())
        .unwrap_or_else(|| wanted.dir.clone());
        let name = project_name(&dir);
        let state = self.snapshot.target(target);
        let herdr_workspace = state
            .and_then(|s| s.herdr.as_ref())
            .filter(|h| h.sessions.iter().any(|s| s.default && s.running))
            .and_then(|h| herdr_workspace(target, &h.program, &dir, &name));
        let tmux_session = state
            .and_then(|s| s.tmux.as_ref())
            .map(|t| tmux_project(&t.sessions, &dir));
        Project {
            target: target.clone(),
            dir,
            name,
            herdr_workspace,
            tmux_session,
        }
    }

    fn open_project(&mut self, entry: ProjectEntry) {
        let program = entry.program.as_str();
        match &entry.workspace {
            Some(id) => {
                run(
                    &entry.target,
                    program,
                    &["workspace", "focus", id],
                    COMMAND_TIMEOUT,
                );
            }
            None => {
                let created = run(
                    &entry.target,
                    program,
                    &[
                        "workspace",
                        "create",
                        "--cwd",
                        &entry.dir,
                        "--label",
                        &entry.name,
                        "--focus",
                    ],
                    COMMAND_TIMEOUT,
                );
                tracing::debug!(
                    workspace = ?created.as_deref().and_then(herdr::parse_created_workspace),
                    "created herdr workspace"
                );
            }
        }
        let _ = self.proxy.send_event(UserEvent::MuxOpen(MuxOpen {
            tool: Tool::Herdr,
            target: entry.target,
            session: None,
            argv: vec![entry.program],
            dir: Some(entry.dir),
        }));
    }

    /// One monitor per running herdr session, while herdr is on.
    fn sync_monitors(&mut self) {
        if self.settings.herdr.is_none() {
            self.monitors.clear();
            return;
        }
        let mut running = Vec::new();
        for state in &self.snapshot.targets {
            let Some(herdr) = &state.herdr else { continue };
            for session in herdr.sessions.iter().filter(|s| s.running) {
                let key = SessionKey {
                    target: state.target.clone(),
                    session: (!session.default).then(|| session.name.clone()),
                };
                running.push((key, Endpoint::of(&state.target, &session.socket)));
            }
        }
        self.monitors
            .retain(|key, monitor| monitor.alive() && running.iter().any(|(k, _)| k == key));
        for (key, endpoint) in running {
            if !self.monitors.contains_key(&key) {
                let monitor = Monitor::start(key.clone(), endpoint, self.proxy.clone());
                self.monitors.insert(key, monitor);
            }
        }
    }
}

fn herdr_state(
    target: &Target,
    program: &str,
    menu: bool,
    previous: Option<&HerdrState>,
) -> HerdrState {
    let sessions = run(
        target,
        program,
        &["session", "list", "--json"],
        COMMAND_TIMEOUT,
    )
    .map(|out| herdr::parse_sessions(&out))
    .unwrap_or_default();
    let machines = if *target == Target::Local {
        run(
            target,
            program,
            &["machine", "list", "--json"],
            COMMAND_TIMEOUT,
        )
        .map(|out| herdr::parse_machines(&out))
        .unwrap_or_default()
    } else {
        Vec::new()
    };
    let machine_agents = if menu {
        machine_agents(target, program, &machines)
    } else {
        previous
            .map(|p| p.machine_agents.clone())
            .unwrap_or_default()
    };
    let bindings = herdr_config(target)
        .map(|text| keys::herdr_bindings(&text))
        .unwrap_or_else(Bindings::herdr_defaults);
    HerdrState {
        program: program.to_owned(),
        sessions,
        machines,
        machine_agents,
        bindings,
    }
}

/// The agents of each saved machine, asked in parallel.
fn machine_agents(
    target: &Target,
    program: &str,
    machines: &[Machine],
) -> HashMap<String, Vec<Agent>> {
    std::thread::scope(|scope| {
        let asks: Vec<_> = machines
            .iter()
            .map(|machine| {
                let id = machine.id.clone();
                scope.spawn(move || {
                    let agents = run(
                        target,
                        program,
                        &["--machine", &id, "agent", "list"],
                        COMMAND_TIMEOUT,
                    )
                    .map(|out| herdr::parse_agents(&out))
                    .unwrap_or_default();
                    (id, agents)
                })
            })
            .collect();
        asks.into_iter().filter_map(|a| a.join().ok()).collect()
    })
}

/// The text of herdr's config file in `target`.
fn herdr_config(target: &Target) -> Option<String> {
    match target {
        Target::Local => {
            let path = if cfg!(windows) {
                dirs::config_dir()?.join("herdr").join("config.toml")
            } else {
                dirs::home_dir()?
                    .join(".config")
                    .join("herdr")
                    .join("config.toml")
            };
            std::fs::read_to_string(path).ok()
        }
        Target::Wsl(_) => run(
            target,
            "sh",
            &["-c", r#"cat "$HOME/.config/herdr/config.toml""#],
            COMMAND_TIMEOUT,
        ),
    }
}

/// The workspace of the default session that works in `dir`: one with a
/// pane there, otherwise one labeled `name`.
fn herdr_workspace(target: &Target, program: &str, dir: &str, name: &str) -> Option<String> {
    let inside = |cwd: &str| {
        let dir = dir.trim_end_matches('/');
        cwd == dir
            || cwd
                .strip_prefix(dir)
                .is_some_and(|rest| rest.starts_with('/'))
    };
    if let Some(out) = run(target, program, &["pane", "list"], COMMAND_TIMEOUT)
        && let Some((id, _)) = herdr::parse_pane_workspaces(&out)
            .into_iter()
            .find(|(_, cwd)| inside(cwd))
    {
        return Some(id);
    }
    let out = run(target, program, &["workspace", "list"], COMMAND_TIMEOUT)?;
    herdr::parse_workspaces(&out)
        .into_iter()
        .find(|(_, label)| label == name)
        .map(|(id, _)| id)
}

fn tmux_state(target: &Target, program: &str, previous: Option<&TmuxState>) -> TmuxState {
    let listed = run(
        target,
        program,
        &["list-sessions", "-F", tmux::LIST_FORMAT],
        COMMAND_TIMEOUT,
    );
    let Some(listed) = listed else {
        // No server: no sessions, and nothing to forward keys to.
        return TmuxState {
            program: program.to_owned(),
            sessions: Vec::new(),
            bindings: previous
                .map(|p| p.bindings.clone())
                .unwrap_or_else(Bindings::tmux_defaults),
        };
    };
    let prefix = run(
        target,
        program,
        &["show-options", "-gv", "prefix"],
        COMMAND_TIMEOUT,
    );
    let list_keys = run(
        target,
        program,
        &["list-keys", "-T", "prefix"],
        COMMAND_TIMEOUT,
    );
    let bindings = match (prefix, list_keys) {
        (Some(prefix), Some(list_keys)) => keys::tmux_bindings(prefix.trim(), &list_keys),
        _ => Bindings::tmux_defaults(),
    };
    TmuxState {
        program: program.to_owned(),
        sessions: tmux::parse_sessions(&listed),
        bindings,
    }
}
