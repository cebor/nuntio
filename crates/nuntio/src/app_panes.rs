//! Starting panes, opening tabs and splits, and closing them, with the
//! confirmation while programs still run.

use std::path::PathBuf;
use std::time::Instant;

use anyhow::Result;
use nuntio_term::{Shell, SpawnOptions, TermHandle, TermSize, Transport};

use super::{CONFIRM_REPEAT, Core, INITIAL_GRID, Startup, term_options};
use crate::banner::{Banner, Severity};
use crate::event::{PaneId, UserEvent};
use crate::pane_tree::Axis;
use crate::shells::{self, Launch, ShellChoice};
use crate::tabs::Placement;
use crate::window::{Pane, TabContent, WindowState};
use crate::wsl::Distro;

/// The Windows path of nuntio-wsl for a pane that runs `shell` in WSL, if
/// the helper is there: the pane then gets a Linux PTY instead of ConPTY.
#[cfg(windows)]
fn wsl_helper(shell: &nuntio_config::Shell) -> Option<String> {
    shell.wsl.as_ref()?;
    let Some(helper) = crate::wsl::helper() else {
        tracing::debug!("nuntio-wsl not found next to the executable, WSL panes use ConPTY");
        return None;
    };
    helper.to_str().map(str::to_owned)
}

#[cfg(not(windows))]
fn wsl_helper(_shell: &nuntio_config::Shell) -> Option<String> {
    None
}

/// Title of the banner that asks to confirm a close.
const CLOSE_BANNER: &str = "Close";

/// What a close request ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseTarget {
    Window,
    /// The tab holding this pane.
    Tab(PaneId),
    Pane(PaneId),
}

/// A close that named running programs and waits to be repeated.
#[derive(Debug, Clone, Copy)]
pub struct PendingClose {
    target: CloseTarget,
    until: Instant,
}

/// Where a new pane starts.
pub enum StartDir {
    Local(PathBuf),
    /// For a shell in WSL: `~` or an absolute Linux path.
    Wsl(String),
}

/// Where the first pane starts: `dir`, or home if a directory was asked for
/// but rejected. Without a request the shell keeps the process's directory.
fn first_pane_dir(asked: bool, dir: Option<StartDir>, home: Option<PathBuf>) -> Option<StartDir> {
    dir.or_else(|| home.filter(|_| asked).map(StartDir::Local))
}

/// Asks to confirm closing `target`, in which the programs `running` run
/// (one name per pane).
fn close_message(running: &[String], target: CloseTarget) -> String {
    let mut names: Vec<String> = Vec::new();
    for name in running {
        let count = running.iter().filter(|n| *n == name).count();
        let name = if count > 1 {
            format!("{name} ({count})")
        } else {
            name.clone()
        };
        if !names.contains(&name) {
            names.push(name);
        }
    }
    let (verb, pronoun) = if running.len() == 1 {
        ("is", "it")
    } else {
        ("are", "them")
    };
    let again = match target {
        CloseTarget::Window => "quit",
        CloseTarget::Tab(_) => "close the tab",
        CloseTarget::Pane(_) => "close the pane",
    };
    format!(
        "{} {verb} still running; {again} again to end {pronoun}",
        names.join(", ")
    )
}

impl Core {
    // ----- Starting panes ----------------------------------------------------

    /// Start `launch` in a new pane of `size`, in `dir` if it suits the
    /// shell.
    fn spawn_pane(
        &mut self,
        dir: Option<StartDir>,
        launch: Launch,
        size: TermSize,
    ) -> Result<Pane> {
        let id = PaneId(self.next_pane_id);
        self.next_pane_id += 1;
        let proxy = self.proxy.clone();
        let distro = launch.distro();
        let login_shell = matches!(launch, Launch::Shell(_));
        let name = launch.name();
        let own_shell = match &launch {
            Launch::Shell(choice) if *choice != shells::default_choice(&self.config) => {
                Some(choice.clone())
            }
            _ => None,
        };
        let mut transport = Transport::Pty;
        let shell = match launch {
            Launch::Command(mut argv) => Some(Shell {
                program: argv.remove(0),
                args: argv,
            }),
            Launch::Settings(program) => Some(Shell {
                program,
                args: Vec::new(),
            }),
            Launch::Shell(ShellChoice { shell: None, .. }) => None,
            Launch::Shell(ShellChoice { shell: Some(s), .. }) => {
                let wsl_dir = match &dir {
                    Some(StartDir::Wsl(dir)) => Some(dir.as_str()),
                    _ => None,
                };
                let (program, args) = s.command(wsl_dir, None);
                let conpty = Shell { program, args };
                match wsl_helper(&s) {
                    Some(helper) => {
                        let (program, args) = s.command(wsl_dir, Some(&helper));
                        transport = Transport::WslHelper { fallback: conpty };
                        Some(Shell { program, args })
                    }
                    None => Some(conpty),
                }
            }
        };
        let options = SpawnOptions {
            shell,
            login_shell,
            // `wsl.exe --cd` picks the directory; a Windows one would be ignored.
            working_directory: match dir {
                Some(StartDir::Local(dir)) if distro.is_none() => Some(dir),
                _ => None,
            },
            term: term_options(&self.config),
            palette: self.palette.clone(),
            env: crate::pane_env::pane_env(self.config_path.as_deref(), distro.is_some()),
            window_id: self.window_id,
            transport,
        };
        let term = TermHandle::spawn(options, size, move |event| {
            let _ = proxy.send_event(UserEvent::Term(id, event));
        })?;
        Ok(Pane::new(id, term, name, distro, own_shell))
    }

    /// The first pane, as the command line asks for it.
    pub(super) fn spawn_first_pane(&mut self) -> Result<Pane> {
        let Startup {
            command,
            working_directory,
        } = std::mem::take(&mut self.startup);
        let launch = command.map_or_else(|| self.default_launch(), Launch::Command);
        let asked = working_directory.is_some() || self.config.working_directory.is_some();
        let dir = match working_directory {
            Some(dir) => self.existing_dir(dir).map(StartDir::Local),
            None => self.configured_dir(launch.distro().as_ref()),
        };
        let dir = first_pane_dir(asked, dir, dirs::home_dir());
        let may_fall_back = matches!(&launch, Launch::Shell(ShellChoice { shell: Some(_), .. }));
        let err = match self.spawn_pane(dir, launch, INITIAL_GRID) {
            Ok(pane) => return Ok(pane),
            Err(err) => err,
        };
        if !may_fall_back {
            return Err(err);
        }
        // A broken `shell` setting shouldn't keep the window from opening.
        let system = Launch::Shell(ShellChoice {
            name: shells::shell_name(None),
            shell: None,
        });
        let fallback = self.spawn_pane(dirs::home_dir().map(StartDir::Local), system, INITIAL_GRID);
        let Ok(pane) = fallback else {
            return Err(err);
        };
        self.notify(Banner::new(
            Severity::Error,
            "Shell",
            vec![format!(
                "failed to start: {err}; started the default shell instead"
            )],
        ));
        Ok(pane)
    }

    /// What a new tab or split runs by default: the configured shell.
    pub(super) fn default_launch(&self) -> Launch {
        Launch::Shell(shells::default_choice(&self.config))
    }

    /// Start `launch` for a new tab or split: in the focused pane's
    /// directory with `inherit`, otherwise (or if it's unknown) in
    /// `working_directory` or at home. Failures are shown in a banner.
    fn spawn_for_focused(
        &mut self,
        state: &WindowState,
        launch: Launch,
        inherit: bool,
    ) -> Option<Pane> {
        let target = launch.distro();
        let dir = inherit
            .then(|| self.focused_dir(state, target.as_ref()))
            .flatten()
            .or_else(|| self.configured_dir(target.as_ref()))
            .or_else(|| dirs::home_dir().map(StartDir::Local));
        self.spawn_pane(dir, launch, state.grid())
            .inspect_err(|err| {
                self.notify(Banner::new(
                    Severity::Error,
                    "Shell",
                    vec![format!("failed to start: {err:#}")],
                ));
            })
            .ok()
    }

    /// `dir` if it is a directory; otherwise a warning.
    fn existing_dir(&mut self, dir: PathBuf) -> Option<PathBuf> {
        if dir.is_dir() {
            return Some(dir);
        }
        self.notify(Banner::new(
            Severity::Warning,
            "Working directory",
            vec![format!("{} is not a directory", dir.display())],
        ));
        None
    }

    /// The configured `working_directory` for a new pane, whose shell runs
    /// in `target` (WSL) or on the host.
    fn configured_dir(&mut self, target: Option<&Distro>) -> Option<StartDir> {
        let dir = self.config.working_directory.clone()?;
        if let Some(distro) = target {
            return self.wsl_cd(distro, &dir).map(StartDir::Wsl);
        }
        let path = match dir.strip_prefix('~') {
            Some(rest) => {
                let rest = rest.trim_start_matches(['/', '\\']);
                dirs::home_dir()?.join(rest)
            }
            None => PathBuf::from(dir),
        };
        self.existing_dir(path).map(StartDir::Local)
    }

    /// `dir` in `distro` as `wsl.exe --cd` takes it: `~/…` becomes absolute.
    fn wsl_cd(&mut self, distro: &Distro, dir: &str) -> Option<String> {
        crate::wsl::cd_arg(dir, || {
            self.wsl_homes.get(&distro.name, distro.user.as_deref())
        })
    }

    /// `dir` in `distro` as an absolute path: `~` too becomes absolute.
    fn wsl_absolute(&mut self, distro: &Distro, dir: &str) -> Option<String> {
        if dir == "~" {
            return self.wsl_homes.get(&distro.name, distro.user.as_deref());
        }
        self.wsl_cd(distro, dir)
    }

    /// The directory of the focused pane, for a new pane whose shell runs in
    /// `target` (WSL) or on the host. Translated between WSL and Windows;
    /// `None` from one distribution to another.
    fn focused_dir(&mut self, state: &WindowState, target: Option<&Distro>) -> Option<StartDir> {
        let pane = state.content().focused_pane();
        let Some(source) = pane.wsl.clone() else {
            let path = pane.term.working_directory()?;
            let Some(distro) = target else {
                return Some(StartDir::Local(path));
            };
            let dir = crate::wsl::linux_path_in(&path.to_string_lossy(), &distro.name)?;
            return self.wsl_cd(distro, &dir).map(StartDir::Wsl);
        };
        let dir = crate::wsl::linux_dir(pane.term.reported_directory(), pane.title.as_deref())?;
        match target {
            Some(distro) if distro.name.eq_ignore_ascii_case(&source.name) => {
                // `~` means another home for another user.
                let dir = if distro.user == source.user {
                    self.wsl_cd(&source, &dir)
                } else {
                    self.wsl_absolute(&source, &dir)
                };
                dir.map(StartDir::Wsl)
            }
            Some(_) => None,
            // Only directories on a Windows drive; inside the distribution
            // the new shell starts in `working_directory` or at home.
            None => {
                let path = PathBuf::from(crate::wsl::windows_path(&dir)?);
                path.is_dir().then_some(StartDir::Local(path))
            }
        }
    }

    // ----- Tabs and splits ---------------------------------------------------

    /// Open a tab that runs `launch` at `placement`.
    pub(super) fn new_tab(
        &mut self,
        state: &mut WindowState,
        launch: Launch,
        placement: Placement,
    ) {
        let inherit = self.config.tabs.inherit_directory;
        let Some(pane) = self.spawn_for_focused(state, launch, inherit) else {
            return;
        };
        let id = pane.id;
        state.send_focus(false);
        state.tabs.open(TabContent::new(pane), placement);
        state.close_tab_menu();
        state.attention_requested = false;
        state.hold_reveal(id, Instant::now());
        state.reset_focus_state();
        // The first extra tab may show the tab bar and shrink the grid.
        state.resize_terms(&self.config);
        state.window.request_redraw();
    }

    /// Split the focused pane; the new pane runs the focused pane's shell in
    /// the same directory.
    pub(super) fn split(&mut self, state: &mut WindowState, axis: Axis) {
        let launch = state
            .content()
            .focused_pane()
            .shell
            .clone()
            .map_or_else(|| self.default_launch(), Launch::Shell);
        let Some(pane) = self.spawn_for_focused(state, launch, true) else {
            return;
        };
        let new = pane.id;
        let content = state.content_mut();
        let target = content.focused;
        if !content.tree.split(target, new, axis) {
            // Dropping the pane ends its shell.
            tracing::error!("focused pane {target:?} is not in the split tree");
            return;
        }
        content.panes.push(pane);
        state.focus_pane(new);
        state.hold_reveal(new, Instant::now());
        state.resize_terms(&self.config);
    }

    // ----- Closing -------------------------------------------------------------

    /// Close a pane; closing a tab's last pane closes the tab.
    pub(super) fn close_pane(&mut self, state: &mut WindowState, id: PaneId) {
        let Some(index) = state.tabs.position(|c| c.contains(id)) else {
            return;
        };
        let active = index == state.tabs.active_index();
        let Some(tab) = state.tabs.get_mut(index) else {
            return;
        };
        let content = &mut tab.content;
        let Some(next) = content.tree.remove(id) else {
            // The tab's last pane.
            self.close_tab(state, index);
            return;
        };
        let was_focused = content.focused == id;
        content.panes.retain(|p| p.id != id);
        if was_focused {
            content.focused = next;
            if active {
                state.send_focus(true);
                state.reset_focus_state();
            }
        }
        state.mouse.divider_drag = None;
        state.resize_terms(&self.config);
        state.window.request_redraw();
    }

    /// Close a tab; closing the last one quits.
    fn close_tab(&mut self, state: &mut WindowState, index: usize) {
        if state.tabs.len() == 1 {
            self.exit_requested = true;
            return;
        }
        let was_active = index == state.tabs.active_index();
        state.tabs.close(index);
        state.close_tab_menu();
        if was_active {
            state.attention_requested = false;
            state.send_focus(true);
            state.reset_focus_state();
        }
        state.mouse.hovered_bar = None;
        state.mouse.tab_drag = None;
        state.mouse.divider_drag = None;
        state.resize_terms(&self.config);
        state.window.request_redraw();
    }

    /// Close what the user asked to close, unless programs run there and
    /// this isn't the confirming repetition.
    pub(super) fn request_close(&mut self, state: &mut WindowState, target: CloseTarget) {
        if !self.confirm_close(state, target) {
            return;
        }
        match target {
            CloseTarget::Window => self.exit_requested = true,
            CloseTarget::Tab(id) => {
                if let Some(index) = state.tabs.position(|c| c.contains(id)) {
                    self.close_tab(state, index);
                }
            }
            CloseTarget::Pane(id) => self.close_pane(state, id),
        }
    }

    /// Close the tab at `index`, as `request_close` does.
    pub(super) fn request_close_tab(&mut self, state: &mut WindowState, index: usize) {
        let id = state.tabs.iter().nth(index).map(|tab| tab.content.focused);
        if let Some(id) = id {
            self.request_close(state, CloseTarget::Tab(id));
        }
    }

    /// Whether `target` may close now. If programs other than the shell
    /// run in it, the first request names them in a banner, and only the
    /// same request again within `CONFIRM_REPEAT` closes it.
    fn confirm_close(&mut self, state: &WindowState, target: CloseTarget) -> bool {
        let now = Instant::now();
        let pending = self.pending_close.take();
        if !self.config.window.confirm_close {
            return true;
        }
        let tabs = state.tabs.iter().map(|t| &t.content);
        let panes: Vec<&Pane> = match target {
            CloseTarget::Window => state.panes().collect(),
            CloseTarget::Tab(id) => tabs
                .filter(|c| c.contains(id))
                .flat_map(|c| &c.panes)
                .collect(),
            CloseTarget::Pane(id) => tabs.filter_map(|c| c.pane(id)).collect(),
        };
        // Where the process can't be seen (Windows, WSL), don't ask.
        let running: Vec<String> = panes
            .iter()
            .filter(|p| p.term.foreground_is_shell() == Some(false))
            .map(|p| p.term.process_name())
            .collect();
        self.dismiss_banner(CLOSE_BANNER);
        if running.is_empty() || pending.is_some_and(|p| p.target == target && now < p.until) {
            return true;
        }
        self.pending_close = Some(PendingClose {
            target,
            until: now + CONFIRM_REPEAT,
        });
        self.ask_to_repeat(CLOSE_BANNER, close_message(&running, target));
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_pane_falls_back_to_home_only_when_a_directory_was_asked_for() {
        use std::path::Path;
        let home = || Some(PathBuf::from("/home/u"));
        assert!(
            matches!(first_pane_dir(true, None, home()), Some(StartDir::Local(p)) if p == Path::new("/home/u"))
        );
        assert!(first_pane_dir(false, None, home()).is_none());
        assert!(
            matches!(first_pane_dir(true, Some(StartDir::Local("/srv".into())), home()), Some(StartDir::Local(p)) if p == Path::new("/srv"))
        );
        assert!(first_pane_dir(true, None, None).is_none());
    }

    #[test]
    fn close_messages_name_the_programs() {
        let names = |list: &[&str]| list.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            close_message(&names(&["vim"]), CloseTarget::Pane(PaneId(0))),
            "vim is still running; close the pane again to end it"
        );
        assert_eq!(
            close_message(&names(&["ssh", "vim", "ssh"]), CloseTarget::Window),
            "ssh (2), vim are still running; quit again to end them"
        );
        assert_eq!(
            close_message(&names(&["htop", "top"]), CloseTarget::Tab(PaneId(1))),
            "htop, top are still running; close the tab again to end them"
        );
    }
}
