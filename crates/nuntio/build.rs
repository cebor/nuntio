//! Embeds the application icon and file properties into the Windows
//! executable, and on Windows cross-builds the Linux helper `nuntio-wsl`
//! next to it.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn main() {
    println!("cargo:rerun-if-changed=../../assets/icons/nuntio.ico");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        embed_resources();
        println!("cargo:rerun-if-changed=../nuntio-wsl/src");
        println!("cargo:rerun-if-changed=../nuntio-wsl/Cargo.toml");
        println!("cargo:rerun-if-changed=../../Cargo.lock");
        if let Err(e) = build_wsl_helper() {
            println!("cargo:warning=nuntio-wsl not built, WSL panes will use ConPTY: {e}");
        }
    }
}

fn embed_resources() {
    let mut resource = winresource::WindowsResource::new();
    resource.set_icon("../../assets/icons/nuntio.ico");
    resource.set("ProductName", "nuntio");
    // Task Manager shows this as the process name.
    resource.set("FileDescription", "nuntio");
    resource.set(
        "LegalCopyright",
        "© 2026 Felix Itzenplitz. MIT or Apache-2.0.",
    );
    resource
        .compile()
        .expect("failed to embed the Windows resources");
}

/// Cross-build `nuntio-wsl` for the Linux of WSL (static musl, linked with
/// the `rust-lld` that ships with Rust) and copy it next to `nuntio.exe`.
/// WSL panes run it to get a Linux PTY instead of Windows' ConPTY.
fn build_wsl_helper() -> Result<(), String> {
    let arch = env::var("CARGO_CFG_TARGET_ARCH").map_err(|e| e.to_string())?;
    let triple = match arch.as_str() {
        "x86_64" => "x86_64-unknown-linux-musl",
        "aarch64" => "aarch64-unknown-linux-musl",
        arch => return Err(format!("no nuntio-wsl build for {arch}")),
    };
    // Without rustup (or if it fails) just try the build.
    if let Ok(installed) = Command::new("rustup")
        .args(["target", "list", "--installed"])
        .output()
        && installed.status.success()
        && !String::from_utf8_lossy(&installed.stdout)
            .lines()
            .any(|line| line.trim() == triple)
    {
        return Err(format!(
            "the Rust target {triple} is missing: rustup target add {triple}"
        ));
    }

    // OUT_DIR is <target>/[<triple>/]<profile>/build/nuntio-<hash>/out.
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").ok_or("OUT_DIR is not set")?);
    let exe_dir = out_dir
        .ancestors()
        .nth(3)
        .ok_or("unexpected OUT_DIR layout")?;
    // A separate target dir: the outer cargo holds the lock on this one.
    let target_dir = exe_dir.join("nuntio-wsl-build");
    let release = env::var("PROFILE").as_deref() == Ok("release");
    let manifest_dir = env::var_os("CARGO_MANIFEST_DIR").ok_or("CARGO_MANIFEST_DIR is not set")?;
    let cargo = env::var_os("CARGO").ok_or("CARGO is not set")?;

    let mut command = Command::new(cargo);
    command
        .current_dir(Path::new(&manifest_dir).join("../.."))
        .env(
            format!(
                "CARGO_TARGET_{}_LINKER",
                triple.to_uppercase().replace('-', "_")
            ),
            "rust-lld",
        )
        // The outer flags are for the Windows target.
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        // Keeps `cargo clippy` from linting the nested build.
        .env_remove("RUSTC_WORKSPACE_WRAPPER")
        .args([
            "build",
            "-p",
            "nuntio-wsl",
            "--bin",
            "nuntio-wsl",
            "--target",
            triple,
            "--target-dir",
        ])
        .arg(&target_dir)
        // Nothing nested may be read as a `cargo:` directive.
        .stdout(Stdio::null());
    if release {
        command.arg("--release");
    }
    let status = command.status().map_err(|e| e.to_string())?;
    if !status.success() {
        return Err(format!(
            "`cargo build -p nuntio-wsl --target {triple}` failed"
        ));
    }
    fs::copy(
        target_dir
            .join(triple)
            .join(if release { "release" } else { "debug" })
            .join("nuntio-wsl"),
        exe_dir.join("nuntio-wsl"),
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}
