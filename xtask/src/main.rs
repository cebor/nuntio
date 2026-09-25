//! Project automation: `cargo xtask <command>`.
//!
//! - `icons`: render `assets/icon.svg` into the PNG/ICO/ICNS icons that are
//!   committed under `assets/icons/`.
//! - `package`: build a release and package it for the host platform into
//!   `dist/`: tar.gz, .deb and AppImage on Linux, a universal .app in a .dmg
//!   on macOS, a .zip on Windows.
//! - `changelog [<range>]`: release notes in Markdown from the `Changelog:`
//!   commit trailers (see CONTRIBUTING.md), e.g. `v0.1.0..HEAD`.
//! - `site [serve]`: generate the website's derived files and build it with
//!   Zola into `site/public/`, or serve it locally with live reload.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail, ensure};

const NAME: &str = "nuntio";
/// The config editor, built from the same package. It must not land on
/// the global PATH: nuntio adds it only inside its panes.
const HELPER: &str = "nuntio-config";
const PNG_SIZES: [u32; 9] = [16, 24, 32, 48, 64, 128, 256, 512, 1024];
const ICO_SIZES: [u32; 7] = [16, 24, 32, 48, 64, 128, 256];
const ICNS_SIZES: [u32; 7] = [16, 32, 64, 128, 256, 512, 1024];
const LINUX_ICON_SIZES: [u32; 7] = [16, 32, 48, 64, 128, 256, 512];
/// AppStream component id, as in `assets/nuntio.metainfo.xml`.
const METAINFO_ID: &str = "io.github.cebor.nuntio";

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("icons") => icons(),
        Some("package") => package(),
        Some("site") => site(args.get(1).is_some_and(|a| a == "serve")),
        Some("changelog") => {
            print!("{}", changelog(args.get(1).map_or("HEAD", String::as_str))?);
            Ok(())
        }
        _ => {
            eprintln!("usage: cargo xtask <icons|package|changelog [<range>]|site [serve]>");
            std::process::exit(2);
        }
    }
}

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask lives in the workspace root")
        .to_owned()
}

fn run(command: &mut Command) -> Result<()> {
    eprintln!("$ {command:?}");
    let status = command
        .status()
        .with_context(|| format!("failed to run {:?}", command.get_program()))?;
    ensure!(
        status.success(),
        "{:?} failed with {status}",
        command.get_program()
    );
    Ok(())
}

fn available(program: &str) -> bool {
    Command::new(program)
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

fn version() -> Result<String> {
    let manifest = fs::read_to_string(root().join("Cargo.toml"))?;
    manifest
        .lines()
        .find_map(|line| line.strip_prefix("version = \""))
        .and_then(|rest| rest.strip_suffix('"'))
        .map(str::to_owned)
        .context("no workspace version in Cargo.toml")
}

// ------------------------------------------------------------ changelog

/// Categories of the `Changelog:` trailer and their headings, in order.
const CHANGELOG_KINDS: [(&str, &str); 7] = [
    ("added", "Added"),
    ("changed", "Changed"),
    ("deprecated", "Deprecated"),
    ("removed", "Removed"),
    ("fixed", "Fixed"),
    ("security", "Security"),
    ("performance", "Performance"),
];

fn changelog(range: &str) -> Result<String> {
    let output = Command::new("git")
        .current_dir(root())
        .args([
            "log",
            "--reverse",
            "--format=%(trailers:key=Changelog,valueonly,separator=)%x09%s",
            range,
        ])
        .output()
        .context("failed to run git")?;
    ensure!(
        output.status.success(),
        "git log failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(format_changelog(&String::from_utf8(output.stdout)?))
}

/// Group `kind<TAB>subject` lines into Markdown sections.
fn format_changelog(log: &str) -> String {
    let entries: Vec<(String, &str)> = log
        .lines()
        .filter_map(|line| line.split_once('\t'))
        .filter(|(kind, _)| !kind.trim().is_empty())
        .map(|(kind, subject)| (kind.trim().to_lowercase(), subject))
        .collect();
    let mut out = String::new();
    for (kind, heading) in CHANGELOG_KINDS {
        let items: Vec<&str> = entries
            .iter()
            .filter(|(k, _)| k == kind)
            .map(|(_, subject)| *subject)
            .collect();
        if items.is_empty() {
            continue;
        }
        out += &format!("### {heading}\n\n");
        for item in items {
            out += &format!("- {item}\n");
        }
        out += "\n";
    }
    let unknown: Vec<&str> = entries
        .iter()
        .filter(|(k, _)| !CHANGELOG_KINDS.iter().any(|(kind, _)| kind == k))
        .map(|(k, _)| k.as_str())
        .collect();
    if !unknown.is_empty() {
        eprintln!(
            "warning: unknown changelog categories: {}",
            unknown.join(", ")
        );
    }
    out
}

// ----------------------------------------------------------------- site

fn site(serve: bool) -> Result<()> {
    let root = root();
    let site = root.join("site");
    let config = fs::read_to_string(root.join("docs/config.md"))?;
    fs::write(
        site.join("content/docs/config.md"),
        site_config_page(&config)?,
    )?;
    copy(&root.join("assets/icon.svg"), &site.join("static/icon.svg"))?;
    copy(
        &root.join("assets/icons/png/32.png"),
        &site.join("static/favicon.png"),
    )?;

    ensure!(
        available("zola"),
        "zola not found on PATH, see https://www.getzola.org/documentation/getting-started/installation/"
    );
    run(Command::new("zola")
        .current_dir(&site)
        .arg(if serve { "serve" } else { "build" }))
}

/// Turn `docs/config.md` into a Zola page: front matter instead of the H1,
/// and links into the repo pointed at the site's own pages.
fn site_config_page(markdown: &str) -> Result<String> {
    let body = markdown
        .strip_prefix("# Configuration reference\n")
        .context("docs/config.md must start with `# Configuration reference`")?;
    let body = body.replace(
        "](../README.md#keyboard-shortcuts)",
        "](@/docs/getting-started.md#keyboard-shortcuts)",
    );
    ensure!(
        !body.contains("](../"),
        "docs/config.md links to a repo file the site doesn't have"
    );
    Ok(format!(
        "+++\ntitle = \"Configuration reference\"\nweight = 2\n+++\n{body}"
    ))
}

// ---------------------------------------------------------------- icons

/// Render the SVG at `size`×`size` as straight (non-premultiplied) RGBA.
fn render_rgba(tree: &resvg::usvg::Tree, size: u32) -> Result<Vec<u8>> {
    let mut pixmap = resvg::tiny_skia::Pixmap::new(size, size).context("pixmap")?;
    let scale = size as f32 / tree.size().width();
    resvg::render(
        tree,
        resvg::tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    Ok(pixmap
        .pixels()
        .iter()
        .flat_map(|p| {
            let c = p.demultiply();
            [c.red(), c.green(), c.blue(), c.alpha()]
        })
        .collect())
}

fn icons() -> Result<()> {
    let assets = root().join("assets");
    let svg = fs::read(assets.join("icon.svg"))?;
    let tree = resvg::usvg::Tree::from_data(&svg, &resvg::usvg::Options::default())?;
    let out = assets.join("icons");
    fs::create_dir_all(out.join("png"))?;

    let mut ico = ico::IconDir::new(ico::ResourceType::Icon);
    let mut icns = icns::IconFamily::new();
    for size in PNG_SIZES {
        let rgba = render_rgba(&tree, size)?;
        let image = ico::IconImage::from_rgba_data(size, size, rgba.clone());
        image.write_png(fs::File::create(out.join(format!("png/{size}.png")))?)?;
        if ICO_SIZES.contains(&size) {
            ico.add_entry(ico::IconDirEntry::encode(&image)?);
        }
        if ICNS_SIZES.contains(&size) {
            let image = icns::Image::from_data(icns::PixelFormat::RGBA, size, size, rgba)?;
            icns.add_icon(&image)?;
        }
    }
    ico.write(fs::File::create(out.join(format!("{NAME}.ico")))?)?;
    icns.write(fs::File::create(out.join(format!("{NAME}.icns")))?)?;
    eprintln!("icons written to {}", out.display());
    Ok(())
}

// -------------------------------------------------------------- package

/// `nuntio-config` next to the built `nuntio`.
fn helper_of(binary: &Path) -> PathBuf {
    binary.with_file_name(format!("{HELPER}{}", std::env::consts::EXE_SUFFIX))
}

/// Builds both binaries of the package; returns the path of `nuntio`.
fn cargo_build(target: Option<&str>) -> Result<PathBuf> {
    let mut command = Command::new(env!("CARGO"));
    command
        .current_dir(root())
        .args(["build", "--release", "--locked", "-p", NAME]);
    if let Some(target) = target {
        command.args(["--target", target]);
    }
    run(&mut command)?;
    let mut dir = root().join("target");
    if let Some(target) = target {
        dir.push(target);
    }
    Ok(dir
        .join("release")
        .join(format!("{NAME}{}", std::env::consts::EXE_SUFFIX)))
}

fn fresh_dir(path: &Path) -> Result<()> {
    if path.exists() {
        fs::remove_dir_all(path)?;
    }
    fs::create_dir_all(path)?;
    Ok(())
}

fn copy(from: &Path, to: &Path) -> Result<()> {
    if let Some(parent) = to.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::copy(from, to).with_context(|| format!("copy {} -> {}", from.display(), to.display()))?;
    Ok(())
}

fn package() -> Result<()> {
    let dist = root().join("dist");
    fs::create_dir_all(&dist)?;
    let version = version()?;
    match std::env::consts::OS {
        "linux" => package_linux(&dist, &version),
        "macos" => package_macos(&dist, &version),
        "windows" => package_windows(&dist, &version),
        os => bail!("packaging for {os} is not supported"),
    }
}

/// Files shipped in every archive next to the binary.
fn copy_docs(to: &Path) -> Result<()> {
    for file in ["LICENSE-MIT", "LICENSE-APACHE"] {
        copy(&root().join(file), &to.join(file))?;
    }
    Ok(())
}

fn package_linux(dist: &Path, version: &str) -> Result<()> {
    let binary = cargo_build(None)?;
    let arch = std::env::consts::ARCH;
    let assets = root().join("assets");
    let stage = root().join("target/package");
    let base = format!("{NAME}-{version}-{arch}-linux");

    // tar.gz with an FHS-like layout: bin/, lib/nuntio/, share/applications,
    // share/icons.
    let tree = stage.join(&base);
    fresh_dir(&tree)?;
    copy(&binary, &tree.join("bin").join(NAME))?;
    copy(
        &helper_of(&binary),
        &tree.join("lib").join(NAME).join(HELPER),
    )?;
    install_desktop_files(&assets, &tree.join("share"))?;
    copy_docs(&tree)?;
    let tarball = dist.join(format!("{base}.tar.gz"));
    run(Command::new("tar")
        .arg("-czf")
        .arg(&tarball)
        .arg("-C")
        .arg(&stage)
        .arg(&base))?;

    // .deb, if cargo-deb is installed (metadata in crates/nuntio/Cargo.toml).
    if available("cargo-deb") {
        run(Command::new(env!("CARGO"))
            .current_dir(root())
            .args(["deb", "-p", NAME, "--no-build", "--output"])
            .arg(dist))?;
    } else {
        eprintln!("cargo-deb not found, skipping .deb (cargo install cargo-deb)");
    }

    // AppImage, if appimagetool is available (or $APPIMAGETOOL points to it).
    let appimagetool = std::env::var("APPIMAGETOOL").unwrap_or_else(|_| "appimagetool".into());
    if available(&appimagetool) {
        let appdir = stage.join("AppDir");
        fresh_dir(&appdir)?;
        copy(&binary, &appdir.join("usr/bin").join(NAME))?;
        copy(
            &helper_of(&binary),
            &appdir.join("usr/lib").join(NAME).join(HELPER),
        )?;
        install_desktop_files(&assets, &appdir.join("usr/share"))?;
        copy(
            &assets.join("nuntio.desktop"),
            &appdir.join("nuntio.desktop"),
        )?;
        copy(
            &assets.join("icons/png/256.png"),
            &appdir.join("nuntio.png"),
        )?;
        let apprun = appdir.join("AppRun");
        fs::write(
            &apprun,
            "#!/bin/sh\nexec \"$(dirname \"$(readlink -f \"$0\")\")/usr/bin/nuntio\" \"$@\"\n",
        )?;
        make_executable(&apprun)?;
        run(Command::new(&appimagetool)
            .env("ARCH", arch)
            .arg(&appdir)
            .arg(dist.join(format!("{base}.AppImage"))))?;
    } else {
        eprintln!("appimagetool not found, skipping AppImage");
    }
    eprintln!("packages written to {}", dist.display());
    Ok(())
}

fn install_desktop_files(assets: &Path, share: &Path) -> Result<()> {
    copy(
        &assets.join("nuntio.desktop"),
        &share.join("applications/nuntio.desktop"),
    )?;
    copy(
        &assets.join("nuntio.metainfo.xml"),
        &share.join(format!("metainfo/{METAINFO_ID}.metainfo.xml")),
    )?;
    copy(
        &assets.join("icon.svg"),
        &share.join(format!("icons/hicolor/scalable/apps/{NAME}.svg")),
    )?;
    for size in LINUX_ICON_SIZES {
        copy(
            &assets.join(format!("icons/png/{size}.png")),
            &share.join(format!("icons/hicolor/{size}x{size}/apps/{NAME}.png")),
        )?;
    }
    Ok(())
}

#[cfg(unix)]
fn make_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o755))?;
    Ok(())
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) -> Result<()> {
    Ok(())
}

fn package_macos(dist: &Path, version: &str) -> Result<()> {
    // Universal binary for Intel and Apple Silicon.
    let intel = cargo_build(Some("x86_64-apple-darwin"))?;
    let arm = cargo_build(Some("aarch64-apple-darwin"))?;
    let stage = root().join("target/package");
    fresh_dir(&stage)?;

    let app = stage.join("nuntio.app/Contents");
    fs::create_dir_all(app.join("MacOS"))?;
    for (intel, arm, name) in [
        (intel.clone(), arm.clone(), NAME),
        (helper_of(&intel), helper_of(&arm), HELPER),
    ] {
        run(Command::new("lipo")
            .arg("-create")
            .arg(&intel)
            .arg(&arm)
            .arg("-output")
            .arg(app.join("MacOS").join(name)))?;
    }
    copy(
        &root().join("assets/icons/nuntio.icns"),
        &app.join("Resources/nuntio.icns"),
    )?;
    let plist = fs::read_to_string(root().join("assets/Info.plist"))?.replace("@VERSION@", version);
    fs::write(app.join("Info.plist"), plist)?;
    // Ad-hoc signature (no identity): binds Info.plist and resources to the
    // bundle. Unsigned universal bundles are reported as "damaged" on Apple
    // Silicon instead of just "from an unidentified developer". The helper
    // is signed first, as a nested executable.
    run(Command::new("codesign")
        .args(["--force", "--sign", "-"])
        .arg(app.join("MacOS").join(HELPER)))?;
    run(Command::new("codesign")
        .args(["--force", "--sign", "-"])
        .arg(stage.join("nuntio.app")))?;
    copy_docs(&stage)?;

    // Disk image with the app and a link to /Applications for drag-install.
    #[cfg(unix)]
    std::os::unix::fs::symlink("/Applications", stage.join("Applications"))?;
    let dmg = dist.join(format!("{NAME}-{version}-macos-universal.dmg"));
    run(Command::new("hdiutil")
        .args([
            "create",
            "-volname",
            NAME,
            "-format",
            "UDZO",
            "-ov",
            "-srcfolder",
        ])
        .arg(&stage)
        .arg(&dmg))?;
    eprintln!("package written to {}", dmg.display());
    Ok(())
}

fn package_windows(dist: &Path, version: &str) -> Result<()> {
    let binary = cargo_build(None)?;
    let stage = root()
        .join("target/package")
        .join(format!("{NAME}-{version}"));
    fresh_dir(&stage)?;
    copy(&binary, &stage.join(format!("{NAME}.exe")))?;
    copy(&helper_of(&binary), &stage.join(format!("{HELPER}.exe")))?;
    copy_docs(&stage)?;
    let zip = dist.join(format!(
        "{NAME}-{version}-{}-windows.zip",
        std::env::consts::ARCH
    ));
    // Windows' bsdtar writes zip archives with `-a`.
    run(Command::new("tar")
        .arg("-a")
        .arg("-cf")
        .arg(&zip)
        .arg("-C")
        .arg(stage.parent().expect("stage has a parent"))
        .arg(stage.file_name().expect("stage has a name")))?;
    eprintln!("package written to {}", zip.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_entries_by_category() {
        let log = "added\tAdd tabs\n\tRefactor internals\nfixed\tFix crash\nadded\tAdd splits\n";
        assert_eq!(
            format_changelog(log),
            "### Added\n\n- Add tabs\n- Add splits\n\n### Fixed\n\n- Fix crash\n\n"
        );
    }

    #[test]
    fn config_page_gets_front_matter_and_site_links() {
        let page = site_config_page(
            "# Configuration reference\n\nSee the [defaults](../README.md#keyboard-shortcuts).\n",
        )
        .unwrap();
        assert_eq!(
            page,
            "+++\ntitle = \"Configuration reference\"\nweight = 2\n+++\n\n\
             See the [defaults](@/docs/getting-started.md#keyboard-shortcuts).\n"
        );
    }

    #[test]
    fn config_page_rejects_unknown_repo_links() {
        assert!(site_config_page("# Configuration reference\n[x](../CONTRIBUTING.md)\n").is_err());
    }
}
