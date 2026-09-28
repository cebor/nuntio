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
use serde::{Deserialize, Serialize};
use winit::event_loop::EventLoopProxy;

use crate::event::UserEvent;

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
    /// `release`, if it is newer than this binary.
    pub fn new(release: &Release, build: Build) -> Option<Self> {
        Self::against(release, CURRENT, build)
    }

    fn against(release: &Release, current: &str, build: Build) -> Option<Self> {
        let latest = Version::parse(&release.tag)?;
        let current = Version::parse(current)?;
        (latest.cmp(&current) == Ordering::Greater).then(|| Self {
            version: release.tag.trim_start_matches('v').to_owned(),
            url: release.url.clone(),
            build,
        })
    }

    pub fn message(&self) -> String {
        Self::message_for(&self.version, CURRENT, self.build)
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

/// The daily check; it stops when this is dropped.
pub struct Checker {
    _stop: mpsc::Sender<()>,
}

impl Checker {
    /// Check now if the last check is a day old (else report the cached
    /// release), then once a day.
    pub fn start(proxy: EventLoopProxy<UserEvent>) -> Self {
        let (stop, stopped) = mpsc::channel();
        let spawned = thread::Builder::new()
            .name("update-check".into())
            .spawn(move || {
                loop {
                    let mut state = State::load();
                    let now = now();
                    let wait = state.wait(now);
                    let (release, next) = if wait.is_zero() {
                        match state.refresh(now) {
                            Ok(release) => {
                                state.save();
                                (Some(release), INTERVAL)
                            }
                            Err(err) => {
                                tracing::warn!("update check failed: {err:#}");
                                (None, RETRY)
                            }
                        }
                    } else {
                        (state.latest.clone(), wait)
                    };
                    if let Some(release) = release {
                        tracing::debug!(tag = release.tag, "latest release");
                        let checked = Checked {
                            dismissed: state.is_dismissed(&release),
                            result: Ok(release),
                            manual: false,
                        };
                        if proxy.send_event(UserEvent::Update(checked)).is_err() {
                            return;
                        }
                    }
                    match stopped.recv_timeout(next) {
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
}
