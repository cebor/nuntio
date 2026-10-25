//! The update flow in the app: the daily check, the newer release it
//! found, the download and install of a release package, their banners, the
//! update menu and the restart into the installed update.

use std::time::{Duration, Instant};

use super::app_panes::CloseTarget;
use super::{Core, Exit};
use crate::actions::Action;
use crate::banner::{Banner, Severity, Topic};
use crate::menu::{MenuKind, UpdateStage};
use crate::update::install::{self, Install, InstallResult, Installed};
use crate::update::{self, Build, Checker, Progress, Reaction, Update};
use crate::window::WindowState;
use nuntio_config::Config;

/// How often `updates.restart` looks whether nothing runs any more.
const RESTART_POLL: Duration = Duration::from_secs(5);

/// What the app knows about updates.
#[derive(Default)]
pub(super) struct UpdateState {
    /// The daily update check, while an update indicator is on.
    checker: Option<Checker>,
    /// A newer release, once a check found one.
    available: Option<Update>,
    /// How this release package is installed, if it can update itself.
    install: Option<Install>,
    /// A download and install of `available`, running or done.
    progress: Option<Progress>,
    /// When `updates.restart` next looks whether nothing runs.
    restart_check: Option<Instant>,
}

impl UpdateState {
    /// Release builds find out how they are installed.
    pub(super) fn detect() -> Self {
        let install = (Build::current() == Build::Release)
            .then(install::detect)
            .flatten();
        tracing::debug!(?install, "update installation");
        Self {
            install,
            ..Self::default()
        }
    }

    /// The installation to start after nuntio quit, if it was to restart
    /// into the update.
    pub(super) fn into_install(self) -> Option<Install> {
        self.install
    }

    /// The newer release a check found.
    pub(super) fn available(&self) -> Option<&Update> {
        self.available.as_ref()
    }

    /// When `updates.restart` next looks whether nothing runs.
    pub(super) fn restart_check(&self) -> Option<Instant> {
        self.restart_check
    }

    /// `updates.restart`: look at once whether nothing runs, if an update
    /// is installed; otherwise stop looking.
    fn schedule_restart_check(&mut self, restart: bool) {
        self.restart_check = self
            .progress
            .as_ref()
            .filter(|p| p.is_installed() && restart)
            .map(|_| Instant::now());
    }

    /// Whether to restart now: a check is due and `idle` says nothing
    /// runs. A busy answer postpones the next check.
    fn restart_poll(&mut self, now: Instant, idle: impl FnOnce() -> bool) -> bool {
        if self.restart_check.is_none_or(|at| now < at) {
            return false;
        }
        if idle() {
            return true;
        }
        self.restart_check = Some(now + RESTART_POLL);
        false
    }
}

/// Close the update menu if it is open: what it offers has changed.
fn close_update_menu(state: &mut WindowState) {
    if state.menu_kind() == Some(MenuKind::Update) {
        state.close_menu();
    }
}

impl Core {
    /// The tab bar shows a badge for a newer release.
    pub(super) fn update_badge(&self) -> bool {
        self.update.available.is_some() && self.config.updates.tab_bar
    }

    /// Open the page of the newer release.
    pub(super) fn open_update(&mut self) {
        if let Some(url) = self.update.available.as_ref().map(|u| u.url.clone()) {
            self.open_url(&url);
        }
    }

    /// Open, or close if it is open, the update menu, at the tab bar's
    /// badge with `at_badge`, otherwise at the `update` status bar item.
    /// nuntio that can't update itself opens the release page instead.
    pub(super) fn open_update_menu(&mut self, state: &mut WindowState, at_badge: bool) {
        if self.update.install.is_none() {
            self.open_update();
            return;
        }
        let Some(update) = &self.update.available else {
            return;
        };
        if state.menu_kind() == Some(MenuKind::Update) {
            state.close_menu();
            return;
        }
        let stage = match &self.update.progress {
            Some(p) if p.is_installing() => UpdateStage::Installing,
            Some(p) if p.is_installed_for(&update.version) => UpdateStage::Installed,
            _ => UpdateStage::Available,
        };
        state.open_update_menu(&self.config, &self.stats, &update.version, stage, at_badge);
    }

    /// Apply the update settings that changed from `old`.
    pub(super) fn sync_update_settings(
        &mut self,
        old: &Config,
        mut state: Option<&mut WindowState>,
    ) {
        self.sync_update_checker(state.as_deref_mut());
        if !old.updates.install && self.config.updates.install {
            self.start_install(state.as_deref_mut(), false);
        }
        if old.updates.restart != self.config.updates.restart {
            self.update
                .schedule_restart_check(self.config.updates.restart);
            // The banner on screen says whether nuntio restarts by itself.
            if let Some(manual) = self
                .update
                .progress
                .as_ref()
                .filter(|p| p.is_installed())
                .map(Progress::manual)
                && self
                    .banner
                    .as_ref()
                    .is_some_and(|b| b.topic == Topic::Update)
            {
                self.show_progress_banner(manual);
            }
        }
        if old.updates.banner && !self.config.updates.banner {
            self.dismiss_banner(Topic::Update);
        }
        if old.updates.status_bar != self.config.updates.status_bar {
            self.set_update(state, self.update.available.clone());
        }
    }

    /// Start or stop the daily update check to match the update
    /// indicators, and show or hide the tab bar badge.
    pub(super) fn sync_update_checker(&mut self, mut state: Option<&mut WindowState>) {
        match (self.config.update_check(), self.update.checker.is_some()) {
            (true, false) => {
                tracing::debug!("starting update check");
                self.update.checker = Some(Checker::start(self.proxy.clone()));
            }
            (false, true) => {
                // Dropping it stops the thread; forget what it found.
                self.update.checker = None;
                self.set_update(state.as_deref_mut(), None);
                self.dismiss_banner(Topic::Update);
            }
            _ => {}
        }
        if let Some(state) = state
            && state.update_badge != self.update_badge()
        {
            state.update_badge = self.update_badge();
            state.window.request_redraw();
        }
    }

    /// Show what an update check found.
    pub(super) fn update_checked(
        &mut self,
        mut state: Option<&mut WindowState>,
        checked: update::Checked,
    ) {
        let reaction = match &self.update.install {
            Some(_) if self.config.updates.install => Reaction::Install,
            Some(_) => Reaction::Offer,
            None => Reaction::Tell,
        };
        let outcome = update::decide(
            checked,
            self.update.checker.is_some(),
            self.config.updates.banner,
            Build::current(),
            reaction,
        );
        match outcome {
            update::Outcome::Ignore => {}
            update::Outcome::Silent { error } => tracing::warn!("update check: {error}"),
            update::Outcome::Failed { error, manual } => {
                let banner = Banner::new(Severity::Warning, Topic::UpdateCheck, vec![error]);
                self.show_update_banner(banner, manual);
            }
            update::Outcome::Found {
                update,
                notice,
                manual,
                install,
            } => {
                let found = update.as_ref().map(|u| u.version.as_str());
                // Installing or installed already: its own banner speaks.
                let underway = self
                    .update
                    .progress
                    .as_ref()
                    .is_some_and(|p| Some(p.version()) == found);
                if underway {
                    if manual {
                        self.show_progress_banner(true);
                    }
                } else {
                    // A repeated answer replaces the banner instead of adding to it.
                    self.dismiss_banner(Topic::Update);
                    if let Some(update::Notice { message, click }) = notice {
                        let banner = Banner::new(Severity::Info, Topic::Update, vec![message]);
                        let banner = banner.map(|b| match click {
                            update::Click::Open(url) => b.with_url(url),
                            update::Click::Install => b.with_action(Action::InstallUpdate),
                            update::Click::None => b,
                        });
                        self.show_update_banner(banner, manual);
                    }
                }
                self.set_update(state.as_deref_mut(), update);
                if install && !underway {
                    self.start_install(state, manual);
                }
            }
        }
    }

    /// Download and install the newer release, unless that runs or is
    /// done. `manual`: asked for by the user, so it reports with banners
    /// even without `updates.banner`.
    pub(super) fn start_install(&mut self, state: Option<&mut WindowState>, manual: bool) {
        let (Some(install), Some(update)) = (&self.update.install, &self.update.available) else {
            return;
        };
        if let Some(progress) = &self.update.progress
            && (progress.is_installing() || progress.is_installed_for(&update.version))
        {
            return;
        }
        let (install, release) = (install.clone(), update.release.clone());
        self.update.progress = Some(Progress::Installing {
            version: update.version.clone(),
            manual,
        });
        if let Some(state) = state {
            close_update_menu(state);
        }
        self.show_progress_banner(manual);
        install::start(self.proxy.clone(), install, release);
        self.request_redraw();
    }

    /// The banner for a running or finished install, if the user asked for
    /// it or `updates.banner` is on.
    fn show_progress_banner(&mut self, manual: bool) {
        if !manual && !self.config.updates.banner {
            return;
        }
        let Some(progress) = &self.update.progress else {
            return;
        };
        let version = progress.version();
        let message = if progress.is_installing() {
            format!("Downloading and installing nuntio {version}…")
        } else if self.config.updates.restart {
            format!(
                "nuntio {version} is installed and restarts when no program is running. Click to restart now"
            )
        } else {
            format!("nuntio {version} is installed. Click to restart")
        };
        let restart = progress.is_installed();
        self.dismiss_banner(Topic::Update);
        let banner = Banner::new(Severity::Info, Topic::Update, vec![message]);
        let banner = banner.map(|b| {
            if restart {
                b.with_action(Action::RestartForUpdate)
            } else {
                b
            }
        });
        self.show_update_banner(banner, manual);
    }

    /// An install thread finished.
    pub(super) fn update_installed(
        &mut self,
        state: Option<&mut WindowState>,
        installed: Installed,
    ) {
        let manual = match &self.update.progress {
            Some(p) if p.is_installing() && p.version() == installed.version => p.manual(),
            _ => return,
        };
        let version = installed.version;
        match installed.result {
            InstallResult::Installed => {
                self.update.progress = Some(Progress::Installed { version, manual });
                if let Some(state) = state {
                    close_update_menu(state);
                }
                self.show_progress_banner(manual);
                self.update
                    .schedule_restart_check(self.config.updates.restart);
            }
            InstallResult::Busy => {
                self.update.progress = None;
                self.dismiss_banner(Topic::Update);
                if manual {
                    let message = "another nuntio window is installing an update".to_owned();
                    let banner =
                        Banner::new(Severity::Warning, Topic::UpdateInstall, vec![message]);
                    self.show_update_banner(banner, true);
                } else {
                    tracing::info!("another nuntio process is installing the update");
                }
            }
            InstallResult::Failed(err) => {
                tracing::warn!("installing nuntio {version} failed: {err}");
                self.update.progress = None;
                self.dismiss_banner(Topic::Update);
                let url = self.update.available.as_ref().map(|u| u.url.clone());
                let message = match url {
                    Some(_) => format!("nuntio {version}: {err}. Click for the release page"),
                    None => format!("nuntio {version}: {err}"),
                };
                let banner = Banner::new(Severity::Warning, Topic::UpdateInstall, vec![message]);
                let banner = banner.map(|b| match url {
                    Some(url) => b.with_url(url),
                    None => b,
                });
                self.show_update_banner(banner, manual);
            }
        }
        self.request_redraw();
    }

    /// `updates.restart`: restart into the installed update once every pane
    /// runs only its shell. A pane whose process can't be seen (Windows)
    /// counts as busy.
    pub(super) fn restart_due(&mut self, state: &WindowState, now: Instant) -> bool {
        let due = self.update.restart_poll(now, || {
            state
                .panes()
                .all(|p| p.term.foreground_is_shell() == Some(true))
        });
        if due {
            tracing::info!("restarting into the installed update");
            self.quit(Exit::Restart);
        }
        due
    }

    /// Quit and start the installed update, once confirmed like quitting.
    pub(super) fn restart_for_update(&mut self, state: &mut WindowState) {
        if !self
            .update
            .progress
            .as_ref()
            .is_some_and(Progress::is_installed)
        {
            tracing::debug!("no installed update to restart into");
            return;
        }
        self.request_close(state, CloseTarget::Restart);
    }

    /// A manual check's banner replaces the current one; a daily one is queued.
    fn show_update_banner(&mut self, banner: Option<Banner>, manual: bool) {
        if manual {
            self.replace_banner(banner);
        } else {
            self.notify(banner);
        }
    }

    fn set_update(&mut self, state: Option<&mut WindowState>, update: Option<Update>) {
        self.stats.set_update(
            update
                .as_ref()
                .filter(|_| self.config.updates.status_bar)
                .map(|u| u.version.clone()),
        );
        self.update.available = update;
        if let Some(state) = state {
            state.update_badge = self.update_badge();
            state.status_bar_changed(&self.config, &self.stats);
            state.window.request_redraw();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn installed() -> UpdateState {
        UpdateState {
            progress: Some(Progress::Installed {
                version: "0.9.0".into(),
                manual: false,
            }),
            ..UpdateState::default()
        }
    }

    #[test]
    fn restart_is_checked_only_for_an_installed_update_with_the_setting_on() {
        let mut state = installed();
        state.schedule_restart_check(true);
        assert!(state.restart_check().is_some());
        // Turning the setting off stops looking.
        state.schedule_restart_check(false);
        assert!(state.restart_check().is_none());

        let mut installing = UpdateState {
            progress: Some(Progress::Installing {
                version: "0.9.0".into(),
                manual: false,
            }),
            ..UpdateState::default()
        };
        installing.schedule_restart_check(true);
        assert!(installing.restart_check().is_none());
        let mut nothing = UpdateState::default();
        nothing.schedule_restart_check(true);
        assert!(nothing.restart_check().is_none());
    }

    #[test]
    fn a_busy_pane_postpones_the_restart() {
        let now = Instant::now();
        let mut state = UpdateState {
            restart_check: Some(now),
            ..UpdateState::default()
        };
        assert!(!state.restart_poll(now, || false));
        assert_eq!(state.restart_check(), Some(now + RESTART_POLL));
        // Not due yet: the panes aren't even looked at.
        assert!(!state.restart_poll(now, || unreachable!()));
        assert!(state.restart_poll(now + RESTART_POLL, || true));
        // Nothing scheduled: never due.
        assert!(!UpdateState::default().restart_poll(now, || true));
    }
}
