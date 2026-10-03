//! Looks for a newer nuntio release on GitHub. It only tells: nothing is
//! downloaded or installed. The last answer is cached, so the check runs
//! at most once a day, even across restarts.

use std::cmp::Ordering;
use std::fs;
use std::path::PathBuf;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
use serde::{Deserialize, Serialize};
use winit::event_loop::EventLoopProxy;

use crate::event::UserEvent;

const REPOSITORY: &str = env!("CARGO_PKG_REPOSITORY");
/// Tag characters kept as they are in a URL path.
const TAG: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'.')
    .remove(b'-')
    .remove(b'_')
    .remove(b'+');

/// Title of the banner that announces an update.
pub const BANNER: &str = "Update";
const CURRENT: &str = env!("CARGO_PKG_VERSION");
const INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
/// After a failed check (offline, rate limit), try again sooner.
const RETRY: Duration = Duration::from_secs(60 * 60);
const TIMEOUT: Duration = Duration::from_secs(20);

/// How this binary was built. `cargo xtask package` sets `NUNTIO_BUILD`;
/// anything else is a build from a source checkout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Build {
    Release,
    Source,
}

impl Build {
    pub fn current() -> Self {
        Self::from_env(option_env!("NUNTIO_BUILD"))
    }

    fn from_env(value: Option<&str>) -> Self {
        match value {
            Some("release") => Self::Release,
            _ => Self::Source,
        }
    }
}

/// A version like `0.1.6`; a `v` prefix and pre-release or build suffixes
/// are ignored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Version(u64, u64, u64);

impl Version {
    fn parse(s: &str) -> Option<Self> {
        let s = s.trim().trim_start_matches('v');
        let core = s.split(['-', '+']).next()?;
        let mut parts = core.split('.').map(|p| p.parse::<u64>().ok());
        let version = Self(parts.next()??, parts.next()??, parts.next()??);
        parts.next().is_none().then_some(version)
    }
}

/// The latest release.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct Release {
    pub tag: String,
    /// The release page, with the notes and downloads.
    pub url: String,
}

impl Release {
    /// The page to open: `url` if it is on this repository's releases (it comes from GitHub or a
    /// cache file), else the page of the tag, built here.
    fn page_url(&self) -> String {
        let releases = format!("{}/releases/", REPOSITORY.trim_end_matches('/'));
        if self.url.starts_with(&releases) {
            return self.url.clone();
        }
        format!("{releases}tag/{}", utf8_percent_encode(&self.tag, TAG))
    }
}

/// The fields of GitHub's answer we need; its `url` is the API's own.
#[derive(Deserialize)]
struct GitHubRelease {
    tag_name: String,
    html_url: String,
}

impl From<GitHubRelease> for Release {
    fn from(release: GitHubRelease) -> Self {
        Self {
            tag: release.tag_name,
            url: release.html_url,
        }
    }
}

/// A release newer than this binary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Update {
    /// Without the `v`, e.g. `0.1.6`.
    pub version: String,
    pub url: String,
    pub build: Build,
}

impl Update {
    fn against(release: &Release, current: &str, build: Build) -> Option<Self> {
        let latest = Version::parse(&release.tag)?;
        let current = Version::parse(current)?;
        (latest.cmp(&current) == Ordering::Greater).then(|| Self {
            version: release.tag.trim_start_matches('v').to_owned(),
            url: release.page_url(),
            build,
        })
    }

    fn message_for(version: &str, current: &str, build: Build) -> String {
        match build {
            Build::Release => format!(
                "nuntio {version} is available (you have {current}). Click for the release notes"
            ),
            Build::Source => format!(
                "nuntio {version} is available; your checkout is at {current}: git pull and rebuild"
            ),
        }
    }
}

/// The message for a manual check that found nothing newer.
pub fn up_to_date() -> String {
    format!("nuntio {CURRENT} is the latest version")
}

/// The result of a check, for the main thread.
#[derive(Debug)]
pub struct Checked {
    pub result: Result<Release, String>,
    /// Asked for by the user (`check_for_updates`), not the daily check.
    pub manual: bool,
    /// The user closed the banner for this release before.
    pub dismissed: bool,
}

/// What the main thread does with a check's answer.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// A late answer of the daily check that was just turned off.
    Ignore,
    /// A failed daily check without the banner indicator: only logged.
    Silent { error: String },
    /// A failed check: a warning banner (replacing the current banner if `manual`).
    Failed { error: String, manual: bool },
    /// A release was found: show `notice` (if any) and remember `update`
    /// (badge, status bar item). `manual` banners replace the current one.
    Found {
        update: Option<Update>,
        notice: Option<Notice>,
        manual: bool,
    },
}

/// The text of an update banner.
#[derive(Debug, PartialEq, Eq)]
pub struct Notice {
    pub message: String,
    pub url: Option<String>,
}

pub fn decide(
    checked: Checked,
    checker_running: bool,
    banner_enabled: bool,
    build: Build,
) -> Outcome {
    decide_against(checked, checker_running, banner_enabled, build, CURRENT)
}

fn decide_against(
    checked: Checked,
    checker_running: bool,
    banner_enabled: bool,
    build: Build,
    current: &str,
) -> Outcome {
    if !checked.manual && !checker_running {
        return Outcome::Ignore;
    }
    // Without the banner indicator, only a manual check speaks up.
    let show_banner = checked.manual || banner_enabled;
    let release = match checked.result {
        Ok(release) => release,
        Err(error) if show_banner => {
            return Outcome::Failed {
                error,
                manual: checked.manual,
            };
        }
        Err(error) => return Outcome::Silent { error },
    };
    let update = Update::against(&release, current, build);
    let notice = match &update {
        Some(u) if checked.manual || (show_banner && !checked.dismissed) => Some(Notice {
            message: Update::message_for(&u.version, current, u.build),
            url: Some(u.url.clone()),
        }),
        None if checked.manual => Some(Notice {
            message: up_to_date(),
            url: None,
        }),
        _ => None,
    };
    Outcome::Found {
        update,
        notice,
        manual: checked.manual,
    }
}

/// What is kept between runs, in the cache directory.
#[derive(Debug, Default, Clone, PartialEq, Deserialize, Serialize)]
#[serde(default)]
struct State {
    /// Unix time of the last successful check.
    last_check: u64,
    /// Lets GitHub answer "not modified", which doesn't count against
    /// its rate limit.
    etag: Option<String>,
    /// Version of the release whose banner was closed.
    dismissed: Option<String>,
    latest: Option<Release>,
}

impl State {
    fn path() -> Option<PathBuf> {
        Some(dirs::cache_dir()?.join("nuntio").join("update.toml"))
    }

    fn load() -> Self {
        Self::path()
            .and_then(|path| fs::read_to_string(path).ok())
            .and_then(|text| toml::from_str(&text).ok())
            .unwrap_or_default()
    }

    fn save(&self) {
        let saved = Self::path().context("no cache directory").and_then(|path| {
            if let Some(dir) = path.parent() {
                fs::create_dir_all(dir)?;
            }
            fs::write(&path, toml::to_string(self)?)?;
            Ok(())
        });
        if let Err(err) = saved {
            tracing::warn!("cannot save the update check state: {err:#}");
        }
    }

    /// Time until the next check is due; zero if it is.
    fn wait(&self, now: u64) -> Duration {
        let next = self.last_check.saturating_add(INTERVAL.as_secs());
        if self.latest.is_none() || self.last_check > now {
            return Duration::ZERO;
        }
        Duration::from_secs(next.saturating_sub(now))
    }

    fn is_dismissed(&self, release: &Release) -> bool {
        let dismissed = self.dismissed.as_deref().and_then(Version::parse);
        dismissed.is_some() && dismissed == Version::parse(&release.tag)
    }

    /// `other` if it was checked later: the state this thread remembers
    /// when the file couldn't be saved.
    fn newer(self, other: Option<&State>) -> State {
        match other {
            Some(other) if other.last_check > self.last_check => other.clone(),
            _ => self,
        }
    }

    /// Ask GitHub, and remember the answer.
    fn refresh(&mut self, now: u64) -> Result<Release> {
        // An ETag without the release it belongs to is useless.
        let etag = self.latest.as_ref().and(self.etag.as_deref());
        let release = match fetch(etag)? {
            Fetched::NotModified => self.latest.clone().context("no cached release")?,
            Fetched::Release(release, etag) => {
                self.etag = etag;
                release
            }
        };
        self.latest = Some(release.clone());
        self.last_check = now;
        Ok(release)
    }
}

/// Don't show the banner for the release `version` again.
pub fn dismiss(version: &str) {
    let mut state = State::load();
    state.dismissed = Some(version.to_owned());
    state.save();
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

enum Fetched {
    NotModified,
    Release(Release, Option<String>),
}

/// `https://api.github.com/repos/<owner>/<repo>/releases/latest`.
fn api_url() -> String {
    let repo = env!("CARGO_PKG_REPOSITORY")
        .trim_start_matches("https://github.com/")
        .trim_end_matches('/');
    format!("https://api.github.com/repos/{repo}/releases/latest")
}

fn fetch(etag: Option<&str>) -> Result<Fetched> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(TIMEOUT))
        .http_status_as_error(false)
        .build()
        .into();
    let mut request = agent
        .get(api_url())
        .header("User-Agent", format!("nuntio/{CURRENT}"))
        .header("Accept", "application/vnd.github+json");
    if let Some(etag) = etag {
        request = request.header("If-None-Match", etag);
    }
    let mut response = request.call().context("cannot reach GitHub")?;
    match response.status().as_u16() {
        200 => {
            let etag = response
                .headers()
                .get("etag")
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned);
            let release = response
                .body_mut()
                .read_json::<GitHubRelease>()
                .context("unexpected answer from GitHub")?;
            Ok(Fetched::Release(release.into(), etag))
        }
        304 => Ok(Fetched::NotModified),
        404 => bail!("no release found on GitHub"),
        403 | 429 => bail!("GitHub's rate limit is reached, try again later"),
        status => bail!("GitHub answered with status {status}"),
    }
}

/// What the check thread tells the main thread: a fresh answer, else the
/// cached release if it wasn't reported yet (offline start, or another
/// instance found a newer one).
fn to_report(
    fresh: Option<Release>,
    cached: Option<&Release>,
    reported: Option<&Release>,
) -> Option<Release> {
    fresh.or_else(|| cached.filter(|c| Some(*c) != reported).cloned())
}

/// How long the thread sleeps after looking at the clock: at most `RETRY`,
/// since the monotonic timeout doesn't count time in suspend, and a full
/// `RETRY` after a check (which failed or has just succeeded).
fn sleep_after(wait: Duration) -> Duration {
    if wait.is_zero() {
        RETRY
    } else {
        wait.min(RETRY)
    }
}

/// The daily check; it stops when this is dropped.
pub struct Checker {
    _stop: mpsc::Sender<()>,
}

impl Checker {
    /// Check now if the last check is a day old (else report the cached
    /// release), then once a day, by the wall clock (checked at least hourly).
    pub fn start(proxy: EventLoopProxy<UserEvent>) -> Self {
        let (stop, stopped) = mpsc::channel();
        let spawned = thread::Builder::new()
            .name("update-check".into())
            .spawn(move || {
                // The release the main thread was last told about.
                let mut reported: Option<Release> = None;
                // The last successful check, in case the file can't be saved.
                let mut remembered: Option<State> = None;
                loop {
                    let mut state = State::load().newer(remembered.as_ref());
                    let now = now();
                    let wait = state.wait(now);
                    let fresh = if wait.is_zero() {
                        match state.refresh(now) {
                            Ok(release) => {
                                state.save();
                                remembered = Some(state.clone());
                                Some(release)
                            }
                            Err(err) => {
                                tracing::warn!("update check failed: {err:#}");
                                None
                            }
                        }
                    } else {
                        None
                    };
                    if let Some(release) =
                        to_report(fresh, state.latest.as_ref(), reported.as_ref())
                    {
                        tracing::debug!(tag = release.tag, "latest release");
                        let checked = Checked {
                            dismissed: state.is_dismissed(&release),
                            result: Ok(release.clone()),
                            manual: false,
                        };
                        if proxy.send_event(UserEvent::Update(checked)).is_err() {
                            return;
                        }
                        reported = Some(release);
                    }
                    match stopped.recv_timeout(sleep_after(wait)) {
                        Err(RecvTimeoutError::Timeout) => {}
                        _ => break,
                    }
                }
                tracing::debug!("update check stopped");
            });
        if let Err(err) = spawned {
            tracing::warn!("cannot start the update check: {err}");
        }
        Self { _stop: stop }
    }
}

/// Check once, right away, and report the result either way.
pub fn check_now(proxy: EventLoopProxy<UserEvent>) {
    let spawned = thread::Builder::new()
        .name("update-check-now".into())
        .spawn(move || {
            let mut state = State::load();
            let result = state.refresh(now());
            if result.is_ok() {
                state.save();
            }
            let dismissed = result.as_ref().is_ok_and(|r| state.is_dismissed(r));
            let checked = Checked {
                result: result.map_err(|err| format!("{err:#}")),
                manual: true,
                dismissed,
            };
            let _ = proxy.send_event(UserEvent::Update(checked));
        });
    if let Err(err) = spawned {
        tracing::warn!("cannot start the update check: {err}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release(tag: &str) -> Release {
        Release {
            tag: tag.into(),
            url: format!("https://github.com/cebor/nuntio/releases/tag/{tag}"),
        }
    }

    #[test]
    fn parses_versions() {
        assert_eq!(Version::parse("v0.1.6"), Some(Version(0, 1, 6)));
        assert_eq!(Version::parse("1.20.3"), Some(Version(1, 20, 3)));
        assert_eq!(Version::parse("v1.0.0-rc.1"), Some(Version(1, 0, 0)));
        assert_eq!(Version::parse("1.2"), None);
        assert_eq!(Version::parse("1.2.3.4"), None);
        assert_eq!(Version::parse("nightly"), None);
        assert!(Version(0, 10, 0) > Version(0, 9, 9), "numeric, not text");
    }

    #[test]
    fn only_newer_releases_are_updates() {
        let newer = Update::against(&release("v0.1.6"), "0.1.5", Build::Release).unwrap();
        assert_eq!(newer.version, "0.1.6");
        assert_eq!(newer.url, release("v0.1.6").url);
        assert_eq!(
            Update::against(&release("v0.1.5"), "0.1.5", Build::Release),
            None
        );
        assert_eq!(
            Update::against(&release("v0.1.4"), "0.1.5", Build::Source),
            None
        );
        assert_eq!(
            Update::against(&release("junk"), "0.1.5", Build::Source),
            None
        );
    }

    #[test]
    fn foreign_release_urls_are_replaced() {
        let page = "https://github.com/cebor/nuntio/releases/tag/v0.1.6";
        for url in [
            "ms-msdt:/x",
            "file:///C:/x.exe",
            "https://github.com.evil.example/cebor/nuntio/releases/x",
            "https://github.com/other/repo/releases/tag/v0.1.6",
            "",
        ] {
            let r = Release {
                tag: "v0.1.6".into(),
                url: url.into(),
            };
            let update = Update::against(&r, "0.1.5", Build::Release).unwrap();
            assert_eq!(update.url, page, "{url}");
        }
        let r = Release {
            tag: "v0.1.6-../../x?y".into(),
            url: "file:///x".into(),
        };
        assert_eq!(
            r.page_url(),
            "https://github.com/cebor/nuntio/releases/tag/v0.1.6-..%2F..%2Fx%3Fy"
        );
    }

    #[test]
    fn source_builds_suggest_pulling() {
        let release = Update::message_for("0.1.6", "0.1.5", Build::Release);
        assert!(release.contains("release notes"), "{release}");
        let source = Update::message_for("0.1.6", "0.1.3", Build::Source);
        assert!(source.contains("git pull"), "{source}");
        assert!(source.contains("0.1.3"));
    }

    #[test]
    fn build_kind_comes_from_the_environment() {
        assert_eq!(Build::from_env(Some("release")), Build::Release);
        assert_eq!(Build::from_env(Some("debug")), Build::Source);
        assert_eq!(Build::from_env(None), Build::Source);
    }

    #[test]
    fn reads_github_answers() {
        let json = r#"{
            "url": "https://api.github.com/repos/cebor/nuntio/releases/1",
            "html_url": "https://github.com/cebor/nuntio/releases/tag/v0.1.6",
            "tag_name": "v0.1.6",
            "draft": false
        }"#;
        let parsed: GitHubRelease = serde_json::from_str(json).unwrap();
        assert_eq!(Release::from(parsed), release("v0.1.6"));
    }

    #[test]
    fn api_url_points_at_the_repository() {
        assert_eq!(
            api_url(),
            "https://api.github.com/repos/cebor/nuntio/releases/latest"
        );
    }

    #[test]
    fn checks_are_due_once_a_day() {
        let day = INTERVAL.as_secs();
        let mut state = State::default();
        assert_eq!(state.wait(1000), Duration::ZERO, "never checked");
        state.last_check = 1000;
        state.latest = Some(release("v0.1.6"));
        assert_eq!(state.wait(1000 + 60), Duration::from_secs(day - 60));
        assert_eq!(state.wait(1000 + day), Duration::ZERO);
        assert_eq!(state.wait(10), Duration::ZERO, "clock went back");
        state.latest = None;
        assert_eq!(state.wait(1000 + 60), Duration::ZERO, "nothing cached");
    }

    #[test]
    fn state_survives_a_round_trip() {
        let state = State {
            last_check: 1_700_000_000,
            etag: Some("W/\"abc\"".into()),
            dismissed: Some("0.1.6".into()),
            latest: Some(release("v0.1.6")),
        };
        let text = toml::to_string(&state).unwrap();
        assert_eq!(toml::from_str::<State>(&text).unwrap(), state);
        assert!(state.is_dismissed(&release("v0.1.6")));
        assert!(!state.is_dismissed(&release("v0.1.7")));
        assert!(!State::default().is_dismissed(&release("v0.1.7")));
        assert_eq!(toml::from_str::<State>("").unwrap(), State::default());
    }

    #[test]
    fn reports_the_cached_release_once() {
        let (v1, v2) = (release("v0.1.6"), release("v0.1.7"));
        // A due check that fresh-succeeds always reports.
        assert_eq!(
            to_report(Some(v2.clone()), Some(&v1), Some(&v1)),
            Some(v2.clone())
        );
        // Offline start with a cached newer release: report it.
        assert_eq!(to_report(None, Some(&v1), None), Some(v1.clone()));
        // The hourly wake-ups don't repeat it.
        assert_eq!(to_report(None, Some(&v1), Some(&v1)), None);
        // Another instance cached a newer one.
        assert_eq!(to_report(None, Some(&v2), Some(&v1)), Some(v2));
        assert_eq!(to_report(None, None, None), None);
    }

    #[test]
    fn the_check_thread_wakes_at_least_hourly() {
        let minutes = |m: u64| Duration::from_secs(m * 60);
        assert_eq!(sleep_after(Duration::ZERO), RETRY); // due: retry in an hour
        assert_eq!(sleep_after(INTERVAL), RETRY);
        assert_eq!(sleep_after(minutes(10)), minutes(10));
        assert_eq!(sleep_after(RETRY), RETRY);
        // The wall clock decides when a check is due, whatever the sleeps add up to.
        let state = State {
            last_check: 1_000,
            latest: Some(release("v0.1.6")),
            ..State::default()
        };
        assert_eq!(state.wait(1_000 + INTERVAL.as_secs() + 5), Duration::ZERO);
    }

    #[test]
    fn a_check_that_could_not_be_saved_is_remembered() {
        let now = 1_000_000;
        let checked = State {
            last_check: now,
            latest: Some(release("v0.1.6")),
            ..State::default()
        };
        assert!(State::default().newer(Some(&checked)).wait(now) > Duration::ZERO);
        let older = State {
            last_check: now - 10,
            ..checked.clone()
        };
        assert_eq!(checked.clone().newer(Some(&older)), checked);
        assert_eq!(checked.clone().newer(None), checked);
    }

    fn answer(result: Result<Release, String>, manual: bool, dismissed: bool) -> Checked {
        Checked {
            result,
            manual,
            dismissed,
        }
    }

    const RUNNING: bool = true;

    #[test]
    fn late_daily_answers_are_ignored_but_manual_ones_answer() {
        let found = || answer(Ok(release("v0.1.6")), false, false);
        assert_eq!(
            decide_against(found(), !RUNNING, true, Build::Release, "0.1.5"),
            Outcome::Ignore
        );
        let manual = answer(Ok(release("v0.1.5")), true, false);
        let outcome = decide_against(manual, !RUNNING, false, Build::Release, "0.1.5");
        assert!(matches!(
            outcome,
            Outcome::Found {
                notice: Some(Notice { url: None, .. }),
                update: None,
                manual: true
            }
        ));
    }

    #[test]
    fn a_dismissed_release_never_comes_back_by_itself() {
        let daily = decide_against(
            answer(Ok(release("v0.1.6")), false, true),
            RUNNING,
            true,
            Build::Release,
            "0.1.5",
        );
        // Still known (badge, status item), but no banner.
        assert!(matches!(
            daily,
            Outcome::Found {
                update: Some(_),
                notice: None,
                manual: false
            }
        ));
        // A manual check shows it anyway.
        let manual = decide_against(
            answer(Ok(release("v0.1.6")), true, true),
            RUNNING,
            false,
            Build::Release,
            "0.1.5",
        );
        assert!(matches!(
            manual,
            Outcome::Found {
                notice: Some(Notice { url: Some(_), .. }),
                manual: true,
                ..
            }
        ));
    }

    #[test]
    fn the_daily_banner_follows_the_banner_setting_and_newness() {
        let daily = |tag: &str, banner| {
            decide_against(
                answer(Ok(release(tag)), false, false),
                RUNNING,
                banner,
                Build::Release,
                "0.1.5",
            )
        };
        assert!(matches!(
            daily("v0.1.6", true),
            Outcome::Found {
                update: Some(_),
                notice: Some(_),
                ..
            }
        ));
        assert!(matches!(
            daily("v0.1.6", false),
            Outcome::Found {
                update: Some(_),
                notice: None,
                ..
            }
        ));
        assert!(matches!(
            daily("v0.1.5", true),
            Outcome::Found {
                update: None,
                notice: None,
                ..
            }
        ));
    }

    #[test]
    fn errors_are_quiet_without_the_banner_indicator() {
        let fail = |manual, banner| {
            decide_against(
                answer(Err("offline".into()), manual, false),
                RUNNING,
                banner,
                Build::Release,
                "0.1.5",
            )
        };
        assert_eq!(
            fail(false, false),
            Outcome::Silent {
                error: "offline".into()
            }
        );
        assert_eq!(
            fail(false, true),
            Outcome::Failed {
                error: "offline".into(),
                manual: false
            }
        );
        assert_eq!(
            fail(true, false),
            Outcome::Failed {
                error: "offline".into(),
                manual: true
            }
        );
    }
}
