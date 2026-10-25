//! Downloads the release asset for this installation from GitHub, checks it
//! against the release's `SHA256SUMS`, and puts it in place. Only release
//! packages do this: what an update replaces depends on how nuntio was
//! installed ([`Install`]).

use std::ffi::{OsStr, OsString};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use winit::event_loop::EventLoopProxy;

use super::{Release, agent_builder, download_url};
use crate::command;
use crate::event::UserEvent;

/// What every staged file and directory next to the installation starts
/// with, followed by the process id.
const STAGING: &str = ".nuntio-update-";
/// An install lock this old was left by a crashed nuntio.
const LOCK_STALE: Duration = Duration::from_secs(30 * 60);
/// Staged files this old were left by an interrupted install.
const LEFTOVER_AGE: Duration = Duration::from_secs(60 * 60);
/// Time to download one asset.
const DOWNLOAD_LIMIT: Duration = Duration::from_secs(15 * 60);
/// Unpacking, mounting.
const UNPACK_LIMIT: Duration = Duration::from_secs(2 * 60);
/// Copying the app out of the disk image.
const COPY_LIMIT: Duration = Duration::from_secs(5 * 60);
/// apt-get, including the time to type the password.
const APT_LIMIT: Duration = Duration::from_secs(15 * 60);

/// Where the .deb puts nuntio, and the list of files dpkg keeps for it.
const DEB_PROGRAM: &str = "/usr/bin/nuntio";
const DPKG_LIST: &str = "/var/lib/dpkg/info/nuntio.list";

/// How this release package is installed: what an update replaces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Install {
    /// `<prefix>/bin/nuntio` next to `<prefix>/lib/nuntio/` (the .tar.gz).
    Tarball { prefix: PathBuf },
    /// The AppImage file (`$APPIMAGE`).
    AppImage { file: PathBuf },
    /// `/usr/bin/nuntio` from the .deb.
    Deb,
    /// `<bundle>.app/Contents/MacOS/nuntio` (the .dmg).
    MacApp { bundle: PathBuf },
    /// The directory of nuntio.exe (setup.exe or .zip).
    Windows { dir: PathBuf },
}

/// What an install thread reports.
#[derive(Debug)]
pub struct Installed {
    /// Without the `v`, e.g. `0.3.2`.
    pub version: String,
    pub result: InstallResult,
}

#[derive(Debug, PartialEq, Eq)]
pub enum InstallResult {
    Installed,
    /// Another nuntio process holds the install lock.
    Busy,
    Failed(String),
}

/// How this nuntio is installed, if it can update itself; removes what an
/// interrupted update left behind. Runs at startup, before an update
/// replaces the executable: on Linux, its path then reads "(deleted)".
pub fn detect() -> Option<Install> {
    let exe = match std::env::current_exe().and_then(fs::canonicalize) {
        Ok(exe) => exe,
        Err(err) => {
            tracing::warn!("cannot find the nuntio executable, so it can't update itself: {err}");
            return None;
        }
    };
    let install = classify(
        &exe,
        std::env::consts::OS,
        std::env::var_os("APPIMAGE"),
        std::env::var_os("APPDIR"),
    )?;
    install.remove_leftovers();
    Some(install)
}

/// The installation `exe` belongs to on `os`. An AppImage sets `APPIMAGE`
/// and `APPDIR`; `exe` must lie in `APPDIR`, since programs started in a
/// pane of an AppImage nuntio inherit both.
fn classify(
    exe: &Path,
    os: &str,
    appimage: Option<OsString>,
    appdir: Option<OsString>,
) -> Option<Install> {
    let parent = exe.parent()?;
    match os {
        "linux" => {
            if let (Some(file), Some(dir)) = (appimage, appdir) {
                let (file, dir) = (PathBuf::from(file), PathBuf::from(dir));
                let dir = fs::canonicalize(&dir).unwrap_or(dir);
                if exe.starts_with(&dir) && file.is_file() {
                    return Some(Install::AppImage { file });
                }
            }
            if exe == Path::new(DEB_PROGRAM) && Path::new(DPKG_LIST).exists() {
                return Some(Install::Deb);
            }
            let prefix = parent.parent()?;
            (parent.file_name() == Some(OsStr::new("bin")) && prefix.join("lib/nuntio").is_dir())
                .then(|| Install::Tarball {
                    prefix: prefix.to_owned(),
                })
        }
        "macos" => {
            let bundle = parent.parent()?.parent()?;
            (parent.ends_with("Contents/MacOS") && bundle.extension() == Some(OsStr::new("app")))
                .then(|| Install::MacApp {
                    bundle: bundle.to_owned(),
                })
        }
        "windows" => Some(Install::Windows {
            dir: parent.to_owned(),
        }),
        _ => None,
    }
}

impl Install {
    /// The asset of `release` that updates this installation.
    fn asset(&self, release: &Release) -> Result<String> {
        self.asset_for(release, std::env::consts::ARCH)
    }

    fn asset_for(&self, release: &Release, arch: &str) -> Result<String> {
        let v = release.tag.trim_start_matches('v');
        let tag = &release.tag;
        let exact = |name: String| {
            if release.assets.contains(&name) {
                Ok(name)
            } else {
                bail!("release {tag} has no {name} for this system")
            }
        };
        match self {
            Self::Tarball { .. } => exact(format!("nuntio-{v}-{arch}-linux.tar.gz")),
            Self::AppImage { .. } => exact(format!("nuntio-{v}-{arch}-linux.AppImage")),
            Self::MacApp { .. } => exact(format!("nuntio-{v}-macos-universal.dmg")),
            // setup.exe installs update from the zip too: the installer
            // can't replace a running nuntio.exe and would close nuntio.
            Self::Windows { .. } => exact(format!("nuntio-{v}-{arch}-windows.zip")),
            Self::Deb => {
                let deb_arch = match arch {
                    "x86_64" => "amd64",
                    "aarch64" => "arm64",
                    other => other,
                };
                let (start, end) = (format!("nuntio_{v}-"), format!("_{deb_arch}.deb"));
                release
                    .assets
                    .iter()
                    .find(|a| a.starts_with(&start) && a.ends_with(&end))
                    .cloned()
                    .with_context(|| {
                        format!("release {tag} has no nuntio_{v}-*_{deb_arch}.deb for this system")
                    })
            }
        }
    }

    /// The nuntio executable of this installation.
    pub fn program(&self) -> PathBuf {
        match self {
            Self::Tarball { prefix } => prefix.join("bin").join("nuntio"),
            Self::AppImage { file } => file.clone(),
            Self::Deb => PathBuf::from(DEB_PROGRAM),
            Self::MacApp { bundle } => bundle.join("Contents").join("MacOS").join("nuntio"),
            Self::Windows { dir } => dir.join("nuntio.exe"),
        }
    }

    /// Start the installed nuntio with `args`, detached from this process.
    pub fn relaunch(&self, args: &[OsString]) -> io::Result<()> {
        let mut command = match self {
            Self::MacApp { bundle } => {
                let mut command = Command::new("open");
                command.arg("-n").arg(bundle);
                if !args.is_empty() {
                    command.arg("--args").args(args);
                }
                command
            }
            _ => {
                let mut command = Command::new(self.program());
                command.args(args);
                command
            }
        };
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            // SAFETY: the closure only calls `setsid`, which is
            // async-signal-safe.
            unsafe {
                command.pre_exec(|| {
                    libc::setsid();
                    Ok(())
                });
            }
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            use windows_sys::Win32::System::Threading::{
                CREATE_NEW_PROCESS_GROUP, DETACHED_PROCESS,
            };
            command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
        }
        command.spawn().map(drop)
    }

    /// The directory where updates are staged next to the installation.
    fn staging_parent(&self) -> Option<&Path> {
        match self {
            Self::Tarball { prefix } => Some(prefix),
            Self::AppImage { file } => file.parent(),
            Self::Deb => None,
            Self::MacApp { bundle } => bundle.parent(),
            Self::Windows { dir } => Some(dir),
        }
    }

    /// Remove what an interrupted update staged, and old files a finished
    /// one moved aside (on Windows, they can't go while they run).
    fn remove_leftovers(&self) {
        let Some(entries) = self.staging_parent().and_then(|dir| fs::read_dir(dir).ok()) else {
            return;
        };
        for entry in entries.flatten() {
            if !entry.file_name().to_string_lossy().starts_with(STAGING) {
                continue;
            }
            let old = entry
                .metadata()
                .and_then(|m| m.modified())
                .is_ok_and(|t| t.elapsed().is_ok_and(|age| age > LEFTOVER_AGE));
            if !old {
                continue;
            }
            let path = entry.path();
            let removed = if entry.file_type().is_ok_and(|t| t.is_dir()) {
                fs::remove_dir_all(&path)
            } else {
                fs::remove_file(&path)
            };
            match removed {
                Ok(()) => tracing::debug!("removed {}", path.display()),
                Err(err) => tracing::debug!("cannot remove {} yet: {err}", path.display()),
            }
        }
    }
}

/// Download and install `release` on a thread; it reports with
/// [`UserEvent::UpdateInstalled`].
pub fn start(proxy: EventLoopProxy<UserEvent>, install: Install, release: Release) {
    let version = release.tag.trim_start_matches('v').to_owned();
    let report = proxy.clone();
    let thread_version = version.clone();
    let spawned = thread::Builder::new()
        .name("update-install".into())
        .spawn(move || {
            let version = thread_version;
            let result = match take_lock() {
                Locking::Busy => InstallResult::Busy,
                locking => {
                    let _lock = locking;
                    tracing::info!(version, "downloading and installing an update");
                    match apply(&install, &release) {
                        Ok(()) => {
                            tracing::info!(version, "update installed");
                            InstallResult::Installed
                        }
                        Err(err) => InstallResult::Failed(format!("{err:#}")),
                    }
                }
            };
            let _ = proxy.send_event(UserEvent::UpdateInstalled(Installed { version, result }));
        });
    if let Err(err) = spawned {
        tracing::warn!("cannot start the update install: {err}");
        let result = InstallResult::Failed(format!("cannot start the install: {err}"));
        let _ = report.send_event(UserEvent::UpdateInstalled(Installed { version, result }));
    }
}

/// The install lock file; removed when dropped.
struct Lock(PathBuf);

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

enum Locking {
    Taken(#[allow(dead_code, reason = "held until dropped")] Lock),
    /// Another nuntio process installs.
    Busy,
    /// No lock file can be made: go on without.
    Unavailable,
}

/// `update.lock` in the cache directory, so that two nuntio processes
/// don't install at the same time.
fn take_lock() -> Locking {
    let Some(dir) = dirs::cache_dir().map(|d| d.join("nuntio")) else {
        return Locking::Unavailable;
    };
    let _ = fs::create_dir_all(&dir);
    let path = dir.join("update.lock");
    for _ in 0..2 {
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut file) => {
                let _ = write!(file, "{}", std::process::id());
                return Locking::Taken(Lock(path));
            }
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
                let fresh = fs::metadata(&path)
                    .and_then(|m| m.modified())
                    .is_ok_and(|t| t.elapsed().is_ok_and(|age| age < LOCK_STALE));
                if fresh {
                    return Locking::Busy;
                }
                tracing::info!("removing a stale update lock");
                let _ = fs::remove_file(&path);
            }
            Err(err) => {
                tracing::warn!("cannot take the update lock: {err}");
                return Locking::Unavailable;
            }
        }
    }
    Locking::Busy
}

/// `<parent>/.nuntio-update-<pid><suffix>`.
fn staged(parent: &Path, suffix: &str) -> PathBuf {
    parent.join(format!("{STAGING}{}{suffix}", std::process::id()))
}

/// A fresh private directory in the temporary directory.
fn temp_staging() -> Result<PathBuf> {
    let dir = std::env::temp_dir().join(format!("nuntio-update-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir(&dir).with_context(|| format!("cannot create {}", dir.display()))?;
    Ok(dir)
}

fn apply(install: &Install, release: &Release) -> Result<()> {
    let name = install.asset(release)?;
    let tag = &release.tag;
    let sums = fetch_text(&download_url(tag, "SHA256SUMS"), "SHA256SUMS")?;
    let expected = checksum_for(&sums, &name)
        .with_context(|| format!("SHA256SUMS of {tag} lists no {name}"))?;
    let asset = Asset {
        url: download_url(tag, &name),
        name,
        sha256: expected,
    };
    match install {
        Install::Tarball { prefix } => {
            let staging = staged(prefix, "");
            fs::create_dir(&staging)
                .with_context(|| format!("cannot write to {}", prefix.display()))?;
            let result = install_tarball(&asset, prefix, &staging);
            let _ = fs::remove_dir_all(&staging);
            result
        }
        Install::AppImage { file } => {
            let dir = file.parent().context("the AppImage has no directory")?;
            let new = staged(dir, ".AppImage");
            let result = asset.download_to(&new).and_then(|()| {
                set_mode(&new, 0o755)?;
                fs::rename(&new, file).with_context(|| format!("cannot replace {}", file.display()))
            });
            if result.is_err() {
                let _ = fs::remove_file(&new);
            }
            result
        }
        Install::Deb => {
            let staging = temp_staging()?;
            let result = install_deb(&asset, &staging);
            let _ = fs::remove_dir_all(&staging);
            result
        }
        Install::MacApp { bundle } => {
            let staging = temp_staging()?;
            let result = install_mac_app(&asset, bundle, &staging);
            let _ = fs::remove_dir_all(&staging);
            result
        }
        Install::Windows { dir } => {
            let staging = staged(dir, "");
            fs::create_dir(&staging)
                .with_context(|| format!("cannot write to {}", dir.display()))?;
            let result = install_windows(&asset, dir, &staging);
            let _ = fs::remove_dir_all(&staging);
            result
        }
    }
}

/// The new version in a .tar.gz: every file moves to the same place under
/// `prefix`, `bin/nuntio` last. Files the new version no longer ships stay.
fn install_tarball(asset: &Asset, prefix: &Path, staging: &Path) -> Result<()> {
    let archive = staging.join(&asset.name);
    asset.download_to(&archive)?;
    let unpacked = staging.join("unpacked");
    fs::create_dir(&unpacked)?;
    run(
        Command::new("tar")
            .arg("-xzf")
            .arg(&archive)
            .arg("-C")
            .arg(&unpacked),
        UNPACK_LIMIT,
    )?;
    let root = only_dir(&unpacked, &asset.name)?;
    let mut files = files_under(&root)?;
    let program = Path::new("bin").join("nuntio");
    files.sort_by_key(|rel| *rel == program);
    for rel in files {
        let dest = prefix.join(&rel);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("cannot create {}", parent.display()))?;
        }
        fs::rename(root.join(&rel), &dest)
            .with_context(|| format!("cannot replace {}", dest.display()))?;
    }
    Ok(())
}

/// The .deb through apt-get, as root through pkexec. apt-get reads the
/// file as its own user, so the directory and file are world-readable.
fn install_deb(asset: &Asset, staging: &Path) -> Result<()> {
    set_mode(staging, 0o755)?;
    let deb = staging.join(&asset.name);
    asset.download_to(&deb)?;
    set_mode(&deb, 0o644)?;
    let mut apt = Command::new("pkexec");
    apt.arg("/usr/bin/apt-get")
        .args(["install", "-y"])
        .arg(&deb);
    let output = match command::output_within(command::no_window(&mut apt), APT_LIMIT) {
        Ok(Some(output)) => output,
        Ok(None) => bail!("apt-get took longer than 15 minutes"),
        Err(err) if err.kind() == io::ErrorKind::NotFound => bail!("pkexec is not installed"),
        Err(err) => return Err(err).context("cannot run pkexec"),
    };
    match output.status.code() {
        Some(0) => Ok(()),
        Some(126) => bail!("the password prompt was dismissed"),
        Some(127) => bail!("pkexec could not ask for the password (no polkit agent?)"),
        _ => bail!("apt-get failed ({})", output.status),
    }
}

/// The app from the .dmg is copied next to `bundle`, then swapped in.
fn install_mac_app(asset: &Asset, bundle: &Path, staging: &Path) -> Result<()> {
    let parent = bundle.parent().context("the app has no directory")?;
    let dmg = staging.join(&asset.name);
    asset.download_to(&dmg)?;
    let mount = staging.join("mnt");
    run(
        Command::new("hdiutil")
            .args([
                "attach",
                "-nobrowse",
                "-readonly",
                "-noautoopen",
                "-mountpoint",
            ])
            .arg(&mount)
            .arg(&dmg),
        UNPACK_LIMIT,
    )?;
    let new = staged(parent, ".app");
    let copied = run(
        Command::new("ditto")
            .arg(mount.join("nuntio.app"))
            .arg(&new),
        COPY_LIMIT,
    );
    if let Err(err) = run(
        Command::new("hdiutil")
            .arg("detach")
            .arg(&mount)
            .arg("-force"),
        UNPACK_LIMIT,
    ) {
        tracing::warn!("{err:#}");
    }
    if let Err(err) = copied {
        let _ = fs::remove_dir_all(&new);
        return Err(err);
    }
    let old = staged(parent, "-old.app");
    if let Err(err) = fs::rename(bundle, &old) {
        let _ = fs::remove_dir_all(&new);
        return Err(err).with_context(|| format!("cannot replace {}", bundle.display()));
    }
    if let Err(err) = fs::rename(&new, bundle) {
        let _ = fs::rename(&old, bundle);
        let _ = fs::remove_dir_all(&new);
        return Err(err).with_context(|| format!("cannot replace {}", bundle.display()));
    }
    let _ = fs::remove_dir_all(&old);
    Ok(())
}

/// The files of the .zip replace those in `dir`. A running exe can be
/// renamed but not deleted, so each old file moves aside first; on an
/// error everything is moved back.
fn install_windows(asset: &Asset, dir: &Path, staging: &Path) -> Result<()> {
    let zip = staging.join(&asset.name);
    asset.download_to(&zip)?;
    let unpacked = staging.join("unpacked");
    fs::create_dir(&unpacked)?;
    run(
        Command::new(system_tar())
            .arg("-xf")
            .arg(&zip)
            .arg("-C")
            .arg(&unpacked),
        UNPACK_LIMIT,
    )?;
    let root = only_dir(&unpacked, &asset.name)?;
    // (file, its old version moved aside)
    let mut moved: Vec<(PathBuf, Option<PathBuf>)> = Vec::new();
    for rel in files_under(&root)? {
        match replace_file(&root, dir, &rel) {
            Ok(old) => moved.push((rel, old)),
            Err(err) => {
                for (rel, old) in moved.iter().rev() {
                    let dest = dir.join(rel);
                    let _ = fs::rename(&dest, root.join(rel));
                    if let Some(old) = old {
                        let _ = fs::rename(old, &dest);
                    }
                }
                return Err(err);
            }
        }
    }
    // What isn't running goes now; the rest at the next start.
    for old in moved.iter().filter_map(|(_, old)| old.as_ref()) {
        let _ = fs::remove_file(old);
    }
    Ok(())
}

/// Move `root/rel` to `dir/rel`, the file there aside first; returns where
/// that went.
fn replace_file(root: &Path, dir: &Path, rel: &Path) -> Result<Option<PathBuf>> {
    let dest = dir.join(rel);
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("cannot create {}", parent.display()))?;
    }
    let old = if dest.exists() {
        let flat = rel.to_string_lossy().replace(['/', '\\'], "-");
        let old = staged(dir, &format!("-old-{flat}"));
        fs::rename(&dest, &old).with_context(|| format!("cannot move {}", dest.display()))?;
        Some(old)
    } else {
        None
    };
    if let Err(err) = fs::rename(root.join(rel), &dest) {
        if let Some(old) = &old {
            let _ = fs::rename(old, &dest);
        }
        return Err(err).with_context(|| format!("cannot replace {}", dest.display()));
    }
    Ok(old)
}

/// Windows' own bsdtar, which unpacks zip files.
fn system_tar() -> PathBuf {
    std::env::var_os("SystemRoot")
        .map(|root| PathBuf::from(root).join("System32").join("tar.exe"))
        .filter(|tar| tar.is_file())
        .unwrap_or_else(|| PathBuf::from("tar.exe"))
}

/// The single directory an archive unpacked into `dir`.
fn only_dir(dir: &Path, archive: &str) -> Result<PathBuf> {
    let mut dirs = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            dirs.push(entry.path());
        }
    }
    match <[PathBuf; 1]>::try_from(dirs) {
        Ok([root]) => Ok(root),
        Err(_) => bail!("{archive} doesn't hold a single directory"),
    }
}

/// Every file below `root`, relative to it.
fn files_under(root: &Path) -> Result<Vec<PathBuf>> {
    fn walk(root: &Path, rel: &Path, files: &mut Vec<PathBuf>) -> io::Result<()> {
        for entry in fs::read_dir(root.join(rel))? {
            let entry = entry?;
            let rel = rel.join(entry.file_name());
            if entry.file_type()?.is_dir() {
                walk(root, &rel, files)?;
            } else {
                files.push(rel);
            }
        }
        Ok(())
    }
    let mut files = Vec::new();
    walk(root, Path::new(""), &mut files)
        .with_context(|| format!("cannot read {}", root.display()))?;
    Ok(files)
}

/// Run a helper program; anything but success is an error.
fn run(command: &mut Command, limit: Duration) -> Result<()> {
    let program = command.get_program().to_string_lossy().into_owned();
    match command::output_within(command::no_window(command), limit) {
        Ok(Some(output)) if output.status.success() => Ok(()),
        Ok(Some(output)) => bail!("{program} failed ({})", output.status),
        Ok(None) => bail!("{program} took longer than {} seconds", limit.as_secs()),
        Err(err) => Err(err).with_context(|| format!("cannot run {program}")),
    }
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
        .with_context(|| format!("cannot set the permissions of {}", path.display()))
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: u32) -> Result<()> {
    Ok(())
}

/// A release asset to download, with the checksum SHA256SUMS lists.
struct Asset {
    name: String,
    url: String,
    sha256: String,
}

impl Asset {
    /// Download to `dest` and check it.
    fn download_to(&self, dest: &Path) -> Result<()> {
        if download(&self.url, &self.name, dest)? != self.sha256 {
            bail!("{} is damaged: checksum mismatch", self.name);
        }
        Ok(())
    }
}

fn agent() -> ureq::Agent {
    agent_builder()
        .timeout_connect(Some(Duration::from_secs(20)))
        .timeout_recv_response(Some(Duration::from_secs(30)))
        .timeout_recv_body(Some(DOWNLOAD_LIMIT))
        .build()
        .into()
}

/// GET `url` (GitHub's download URLs redirect to its CDN).
fn get(url: &str, name: &str) -> Result<ureq::http::Response<ureq::Body>> {
    let response = agent()
        .get(url)
        .call()
        .with_context(|| format!("cannot download {name}"))?;
    match response.status().as_u16() {
        200 => Ok(response),
        status => bail!("GitHub answered with status {status} for {name}"),
    }
}

fn fetch_text(url: &str, name: &str) -> Result<String> {
    get(url, name)?
        .body_mut()
        .read_to_string()
        .with_context(|| format!("cannot download {name}"))
}

/// Stream `url` into `dest`; returns its SHA-256 in lowercase hex. Not
/// `read_to_vec`, which stops at 10 MB.
fn download(url: &str, name: &str, dest: &Path) -> Result<String> {
    let mut body = get(url, name)?.into_body().into_reader();
    let mut file =
        File::create(dest).with_context(|| format!("cannot write {}", dest.display()))?;
    let mut digest = ring::digest::Context::new(&ring::digest::SHA256);
    let mut chunk = vec![0; 64 * 1024];
    loop {
        let read = match body.read(&mut chunk) {
            Ok(0) => break,
            Ok(read) => read,
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(err).with_context(|| format!("cannot download {name}")),
        };
        digest.update(&chunk[..read]);
        file.write_all(&chunk[..read])
            .with_context(|| format!("cannot write {}", dest.display()))?;
    }
    file.sync_all()
        .with_context(|| format!("cannot write {}", dest.display()))?;
    Ok(hex(digest.finish().as_ref()))
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    bytes
        .iter()
        .flat_map(|b| [DIGITS[usize::from(b >> 4)], DIGITS[usize::from(b & 0xf)]])
        .map(char::from)
        .collect()
}

/// The lowercase SHA-256 that `sums` (`sha256sum` output: `<hex>  <name>`,
/// or `<hex> *<name>` for binary mode) lists for `name`.
fn checksum_for(sums: &str, name: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let (hash, file) = line.trim_end().split_once(' ')?;
        let file = file.strip_prefix(' ').or_else(|| file.strip_prefix('*'))?;
        (file == name && hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit()))
            .then(|| hash.to_ascii_lowercase())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release(tag: &str, assets: &[&str]) -> Release {
        Release {
            tag: tag.into(),
            assets: assets.iter().map(|a| a.to_string()).collect(),
        }
    }

    #[test]
    fn each_installation_takes_its_own_asset() {
        let all = release(
            "v0.3.2",
            &[
                "nuntio-0.3.2-x86_64-linux.tar.gz",
                "nuntio-0.3.2-x86_64-linux.AppImage",
                "nuntio_0.3.2-1_amd64.deb",
                "nuntio_0.3.2-1_arm64.deb",
                "nuntio-0.3.2-macos-universal.dmg",
                "nuntio-0.3.2-x86_64-windows.zip",
                "nuntio-0.3.2-x86_64-windows-setup.exe",
                "SHA256SUMS",
            ],
        );
        let asset = |install: Install, arch| install.asset_for(&all, arch).unwrap();
        let path = PathBuf::new;
        assert_eq!(
            asset(Install::Tarball { prefix: path() }, "x86_64"),
            "nuntio-0.3.2-x86_64-linux.tar.gz"
        );
        assert_eq!(
            asset(Install::AppImage { file: path() }, "x86_64"),
            "nuntio-0.3.2-x86_64-linux.AppImage"
        );
        assert_eq!(asset(Install::Deb, "x86_64"), "nuntio_0.3.2-1_amd64.deb");
        assert_eq!(asset(Install::Deb, "aarch64"), "nuntio_0.3.2-1_arm64.deb");
        assert_eq!(
            asset(Install::MacApp { bundle: path() }, "aarch64"),
            "nuntio-0.3.2-macos-universal.dmg"
        );
        assert_eq!(
            asset(Install::Windows { dir: path() }, "x86_64"),
            "nuntio-0.3.2-x86_64-windows.zip"
        );

        let missing = Install::Tarball { prefix: path() }
            .asset_for(&all, "aarch64")
            .unwrap_err()
            .to_string();
        assert!(
            missing.contains("nuntio-0.3.2-aarch64-linux.tar.gz"),
            "{missing}"
        );
        let no_deb = Install::Deb
            .asset_for(&release("v0.3.2", &["nuntio_0.3.1-1_amd64.deb"]), "x86_64")
            .unwrap_err()
            .to_string();
        assert!(no_deb.contains("_amd64.deb"), "{no_deb}");
    }

    #[test]
    fn checksums_are_read_from_sha256sums() {
        let a = "a".repeat(64);
        let b = "B".repeat(64);
        let sums = format!("{a}  nuntio.tar.gz\n{b} *nuntio.zip\n{a}  nuntio.tar.gz.sig\n");
        assert_eq!(checksum_for(&sums, "nuntio.tar.gz"), Some(a.clone()));
        assert_eq!(checksum_for(&sums, "nuntio.zip"), Some("b".repeat(64)));
        assert_eq!(checksum_for(&sums, "nuntio.dmg"), None);
        assert_eq!(checksum_for("abc  nuntio.zip", "nuntio.zip"), None);
    }

    #[test]
    fn hashes_are_lowercase_hex() {
        assert_eq!(hex(&[0x00, 0x9f, 0xa0, 0xff]), "009fa0ff");
    }

    #[test]
    fn installations_are_told_apart_by_their_paths() {
        assert_eq!(
            classify(Path::new("/opt/nuntio/nuntio.exe"), "windows", None, None),
            Some(Install::Windows {
                dir: PathBuf::from("/opt/nuntio")
            })
        );
        assert_eq!(
            classify(
                Path::new("/Applications/nuntio.app/Contents/MacOS/nuntio"),
                "macos",
                None,
                None
            ),
            Some(Install::MacApp {
                bundle: PathBuf::from("/Applications/nuntio.app")
            })
        );
        assert_eq!(
            classify(Path::new("/usr/local/bin/nuntio"), "macos", None, None),
            None
        );

        let root = std::env::temp_dir().join(format!("nuntio-classify-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("prefix/bin")).unwrap();
        let exe = root.join("prefix/bin/nuntio");
        assert_eq!(
            classify(&exe, "linux", None, None),
            None,
            "no lib/nuntio next to bin"
        );
        fs::create_dir_all(root.join("prefix/lib/nuntio")).unwrap();
        let tarball = Some(Install::Tarball {
            prefix: root.join("prefix"),
        });
        assert_eq!(classify(&exe, "linux", None, None), tarball);
        assert_eq!(classify(&exe, "freebsd", None, None), None);

        // An AppImage's variables, inherited by a nuntio started in its pane.
        let appimage = root.join("nuntio.AppImage");
        fs::write(&appimage, "").unwrap();
        let mount = root.join("mount");
        let env = || {
            (
                Some(appimage.clone().into_os_string()),
                Some(mount.clone().into_os_string()),
            )
        };
        let (file, dir) = env();
        assert_eq!(classify(&exe, "linux", file, dir), tarball);
        let (file, dir) = env();
        assert_eq!(
            classify(&mount.join("usr/bin/nuntio"), "linux", file, dir),
            Some(Install::AppImage { file: appimage })
        );
        fs::remove_dir_all(&root).unwrap();
    }
}
