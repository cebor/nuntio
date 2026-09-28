//! `cargo xtask drive <command>`: start nuntio with its debug server
//! (feature `debug-server`) and remote-control it, to look at it and try
//! things out without touching the keyboard. See `drive help`.

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::Command;
#[cfg(unix)]
use std::process::Stdio;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use tiny_skia::{IntSize, Paint, Pixmap, PixmapPaint, Rect, Transform};

use crate::{root, run};

const USAGE: &str = "\
usage: cargo xtask drive <command>

  start [--visible] [--release] [--size WxH] [--config <file>] [-- <command>...]
                          build nuntio with the debug server and start it in
                          the background (with an empty config by default);
                          headless unless --visible: off-screen on Windows and
                          macOS, under Xvfb on Linux (needs xvfb-run)
  stop                    quit it
  state                   window, tabs, panes, banner, idle time as JSON
  text [--pane N]         the visible lines of a pane
  cells --line L [--from C] [--to C] [--pane N]
                          colors and attributes of the cells in a line
  shot [<file.png>] [--pane N]
                          screenshot (default target/nuntio-debug/shot.png)
  key <combo>...          e.g. `key ctrl+shift+t`, `key up up enter`
  type <text>             type text; \\r \\n \\t \\e \\\\ and \\xNN are escapes
  paste <text>            paste text (bracketed, may ask for confirmation)
  action <name>           run an action like split_vertical or new_tab
  mouse <move|down|up|click|scroll> [<x> <y> | --cell <column> <line>]
        [--pane N] [--button left|middle|right] [--count N] [--lines N]
        [--mods shift,ctrl,alt,super]
                          mouse input at window pixels or a cell's center
  resize <width> <height> inner size in physical pixels
  window <minimize|maximize|restore>
                          change the window state as the window manager would;
                          restore un-minimizes, or else un-maximizes
  wait-idle [--ms 300] [--timeout 10]
                          wait until no pane had output for --ms
  wait-text <text> [--pane N] [--timeout 10]
                          wait until the text is on the screen
  record start [--max N]  capture every frame drawn from now on (60 by default)
  record stop [--dir <dir>] [--sheet] [--video <file.mp4>]
                          save the frames as PNGs plus frames.json; --sheet
                          adds a contact sheet, --video an MP4 (needs ffmpeg)
  log [-n 50]             the end of the log
";

pub fn main(args: &[String]) -> Result<()> {
    let Some((command, args)) = args.split_first() else {
        eprint!("{USAGE}");
        std::process::exit(2);
    };
    let mut args = Args::new(args);
    match command.as_str() {
        "start" => start(&mut args),
        "stop" => stop(),
        "state" => {
            let reply = request(json!({ "cmd": "state" }))?;
            println!("{}", serde_json::to_string_pretty(&reply.json)?);
            Ok(())
        }
        "text" => {
            let pane = args.option_u64("--pane")?;
            let reply = request(json!({ "cmd": "text", "pane": pane }))?;
            print_text(&reply.json);
            Ok(())
        }
        "cells" => {
            let line = args.option_u64("--line")?.context("--line is required")?;
            let request_json = json!({
                "cmd": "cells",
                "line": line,
                "from": args.option_u64("--from")?.unwrap_or(0),
                "to": args.option_u64("--to")?,
                "pane": args.option_u64("--pane")?,
            });
            let reply = request(request_json)?;
            for cell in reply.json["cells"].as_array().into_iter().flatten() {
                println!("{cell}");
            }
            Ok(())
        }
        "shot" => {
            let pane = args.option_u64("--pane")?;
            let path = args
                .positional()
                .map_or_else(|| dir().join("shot.png"), PathBuf::from);
            let reply = request(json!({ "cmd": "screenshot", "pane": pane }))?;
            let (width, height) = size(&reply.json);
            save_png(&path, width, height, reply.binary)?;
            println!("{} ({width}x{height})", path.display());
            Ok(())
        }
        "key" => {
            let keys = args.rest();
            ensure!(!keys.is_empty(), "key: name at least one key");
            request(json!({ "cmd": "key", "keys": keys }))?;
            Ok(())
        }
        "type" => {
            let text = unescape(&args.rest().join(" "))?;
            request(json!({ "cmd": "type", "text": text }))?;
            Ok(())
        }
        "paste" => {
            let text = unescape(&args.rest().join(" "))?;
            request(json!({ "cmd": "paste", "text": text }))?;
            Ok(())
        }
        "action" => {
            let name = args.positional().context("action: name the action")?;
            request(json!({ "cmd": "action", "name": name }))?;
            Ok(())
        }
        "mouse" => mouse(&mut args),
        "resize" => {
            let width: u32 = args.positional().context("resize: width")?.parse()?;
            let height: u32 = args.positional().context("resize: height")?.parse()?;
            request(json!({ "cmd": "resize", "width": width, "height": height }))?;
            Ok(())
        }
        "window" => {
            let request_json = match args.positional().as_deref() {
                Some("minimize") => json!({ "cmd": "window", "minimized": true }),
                Some("maximize") => json!({ "cmd": "window", "maximized": true }),
                // Like the OS: a minimized window comes back as it was,
                // maximized or not.
                Some("restore") => {
                    let state = request(json!({ "cmd": "state" }))?;
                    if state.json["window"]["minimized"] == true {
                        json!({ "cmd": "window", "minimized": false })
                    } else {
                        json!({ "cmd": "window", "maximized": false })
                    }
                }
                _ => bail!("window: minimize, maximize or restore"),
            };
            request(request_json)?;
            Ok(())
        }
        "wait-idle" => {
            let ms = args.option_u64("--ms")?.unwrap_or(300);
            let timeout = timeout(&mut args)?;
            wait(timeout, "output to stop", || {
                let reply = request(json!({ "cmd": "state" }))?;
                // No output yet counts as idle.
                Ok(reply.json["idle_ms"].as_u64().is_none_or(|idle| idle >= ms))
            })
        }
        "wait-text" => {
            let pane = args.option_u64("--pane")?;
            let timeout = timeout(&mut args)?;
            let text = args.rest().join(" ");
            ensure!(!text.is_empty(), "wait-text: name the text");
            wait(timeout, &format!("{text:?}"), || {
                let reply = request(json!({ "cmd": "text", "pane": pane }))?;
                Ok(lines(&reply.json).any(|line| line.contains(&text)))
            })
        }
        "record" => match args.positional().as_deref() {
            Some("start") => {
                let max = args.option_u64("--max")?;
                request(json!({ "cmd": "record_start", "max_frames": max }))?;
                Ok(())
            }
            Some("stop") => record_stop(&mut args),
            _ => bail!("record: start or stop"),
        },
        "log" => {
            let n = args.option_u64("-n")?.unwrap_or(50) as usize;
            let log = fs::read_to_string(dir().join("nuntio.log")).context("no log yet")?;
            let lines: Vec<&str> = log.lines().collect();
            for line in &lines[lines.len().saturating_sub(n)..] {
                println!("{line}");
            }
            Ok(())
        }
        "help" | "--help" | "-h" => {
            print!("{USAGE}");
            Ok(())
        }
        other => bail!("unknown drive command `{other}`\n\n{USAGE}"),
    }
}

/// Where the debug instance keeps its files.
fn dir() -> PathBuf {
    root().join("target").join("nuntio-debug")
}

fn state_file() -> PathBuf {
    dir().join("state.json")
}

/// Command line arguments, taken out as they are used.
struct Args(Vec<String>);

impl Args {
    fn new(args: &[String]) -> Self {
        Self(args.to_vec())
    }

    fn flag(&mut self, name: &str) -> bool {
        let found = self.0.iter().position(|a| a == name);
        found.map(|i| self.0.remove(i)).is_some()
    }

    fn option(&mut self, name: &str) -> Result<Option<String>> {
        let Some(i) = self.0.iter().position(|a| a == name) else {
            return Ok(None);
        };
        ensure!(i + 1 < self.0.len(), "{name} needs a value");
        self.0.remove(i);
        Ok(Some(self.0.remove(i)))
    }

    fn option_u64(&mut self, name: &str) -> Result<Option<u64>> {
        self.option(name)?
            .map(|v| v.parse().with_context(|| format!("{name}: not a number")))
            .transpose()
    }

    fn positional(&mut self) -> Option<String> {
        (!self.0.is_empty()).then(|| self.0.remove(0))
    }

    fn rest(&mut self) -> Vec<String> {
        std::mem::take(&mut self.0)
    }
}

fn timeout(args: &mut Args) -> Result<Duration> {
    Ok(Duration::from_secs(
        args.option_u64("--timeout")?.unwrap_or(10),
    ))
}

struct Reply {
    json: Value,
    binary: Vec<u8>,
}

/// Send one request to the running instance and read its reply.
fn request(mut request: Value) -> Result<Reply> {
    let info = fs::read_to_string(state_file())
        .context("nuntio is not running; start it with `cargo xtask drive start`")?;
    let info: Value = serde_json::from_str(&info)?;
    let port = info["port"].as_u64().context("no port in state.json")?;
    request["token"] = info["token"].clone();

    let stream = TcpStream::connect(("127.0.0.1", port as u16))
        .context("nuntio does not answer; is it still running?")?;
    let mut writer = stream.try_clone()?;
    writer.write_all(format!("{request}\n").as_bytes())?;
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    ensure!(!line.is_empty(), "nuntio closed the connection");
    let json: Value = serde_json::from_str(&line)?;
    if json["ok"] != true {
        bail!("{}", json["error"].as_str().unwrap_or("request failed"));
    }
    let mut binary = vec![0; json["bytes"].as_u64().unwrap_or(0) as usize];
    reader.read_exact(&mut binary)?;
    Ok(Reply { json, binary })
}

/// Poll `done` until it is true.
fn wait(timeout: Duration, what: &str, mut done: impl FnMut() -> Result<bool>) -> Result<()> {
    let start = Instant::now();
    loop {
        if done()? {
            return Ok(());
        }
        ensure!(
            start.elapsed() < timeout,
            "timed out after {timeout:?} waiting for {what}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn start(args: &mut Args) -> Result<()> {
    let release = args.flag("--release");
    let visible = args.flag("--visible");
    let size = args.option("--size")?;
    let config = args.option("--config")?;
    let command = match args.0.iter().position(|a| a == "--") {
        Some(i) => args.0.split_off(i).split_off(1),
        None => Vec::new(),
    };
    ensure!(args.0.is_empty(), "start: unexpected {:?}", args.0);
    if request(json!({ "cmd": "state" })).is_ok() {
        bail!("nuntio is already running; `cargo xtask drive stop` first");
    }

    let mut build = Command::new(env!("CARGO"));
    build.current_dir(root()).args([
        "build",
        "-p",
        "nuntio",
        "--bin",
        "nuntio",
        "--features",
        "debug-server",
    ]);
    if release {
        build.arg("--release");
    }
    run(&mut build)?;

    let dir = dir();
    fs::create_dir_all(&dir)?;
    let config = match config {
        Some(path) => PathBuf::from(path),
        None => {
            // Defaults, whatever the user's own config says.
            let path = dir.join("config.toml");
            fs::write(&path, "")?;
            path
        }
    };
    let _ = fs::remove_file(state_file());
    let profile = if release { "release" } else { "debug" };
    let binary = root()
        .join("target")
        .join(profile)
        .join(format!("nuntio{}", std::env::consts::EXE_SUFFIX));
    // nuntio writes its log next to the state file itself.
    let argv = vec![
        "--debug-server".to_owned(),
        state_file().to_string_lossy().into_owned(),
        "--config".to_owned(),
        config.to_string_lossy().into_owned(),
    ];
    let mut launch = launch(binary, argv, !visible);
    if !command.is_empty() {
        launch.args.push("-e".to_owned());
        launch.args.extend(command);
    }
    let headless = launch.headless;
    let mut child = Detached::spawn(&launch).context("failed to start nuntio")?;

    let started = Instant::now();
    loop {
        if let Some(status) = child.exit_status() {
            bail!("nuntio exited with {status}; see `cargo xtask drive log`");
        }
        if request(json!({ "cmd": "state" })).is_ok() {
            break;
        }
        ensure!(
            started.elapsed() < Duration::from_secs(60),
            "nuntio did not answer within a minute; see `cargo xtask drive log`"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    if let Some(size) = size {
        let (width, height) = size.split_once('x').context("--size: WIDTHxHEIGHT")?;
        request(json!({
            "cmd": "resize",
            "width": width.parse::<u32>()?,
            "height": height.parse::<u32>()?,
        }))?;
    }
    let mode = if headless { "headless" } else { "visible" };
    println!("nuntio is running {mode} (pid {})", child.pid());
    Ok(())
}

/// How nuntio is started.
struct Launch {
    program: PathBuf,
    args: Vec<String>,
    /// Environment variables the program must not see (Linux only).
    #[cfg_attr(not(unix), allow(dead_code))]
    env_remove: &'static [&'static str],
    headless: bool,
}

/// Start `nuntio` with `args`, headless if `headless` and possible. On
/// Linux that means an X server without a screen (Xvfb): Wayland windows
/// can be neither hidden nor moved away, so Wayland is kept out of it.
/// Elsewhere nuntio keeps its window off-screen itself.
fn launch(nuntio: PathBuf, mut args: Vec<String>, headless: bool) -> Launch {
    let plain = |args| Launch {
        program: nuntio.clone(),
        args,
        env_remove: &[],
        headless: false,
    };
    if !headless {
        return plain(args);
    }
    if cfg!(target_os = "linux") {
        let Some(xvfb_run) = on_path("xvfb-run") else {
            eprintln!("xvfb-run is not installed (package xvfb): starting a visible window");
            return plain(args);
        };
        let mut wrapped = vec![
            "-a".to_owned(),
            "-s".to_owned(),
            "-screen 0 1920x1080x24".to_owned(),
            nuntio.to_string_lossy().into_owned(),
        ];
        wrapped.append(&mut args);
        return Launch {
            program: xvfb_run,
            args: wrapped,
            env_remove: &["WAYLAND_DISPLAY", "WAYLAND_SOCKET"],
            headless: true,
        };
    }
    args.push("--headless".to_owned());
    Launch {
        headless: true,
        ..plain(args)
    }
}

/// Where a program is on the `PATH`.
fn on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
}

/// nuntio started so that it outlives the driver and doesn't hold on to
/// anything of it: whoever reads the driver's output through a pipe would
/// otherwise wait for nuntio to exit too.
///
/// On Windows it is created without inheriting any handle (`std` passes
/// on every inheritable one, including copies of the caller's pipes that
/// cargo handed down), without a console window, in its own process group
/// (no Ctrl+C from the driver's shell) and, where the job allows it,
/// outside the job object of that shell: tools that run commands in a job
/// end everything in it when the command ends.
struct Detached {
    #[cfg(windows)]
    pid: u32,
    #[cfg(windows)]
    process: windows_sys::Win32::Foundation::HANDLE,
    #[cfg(unix)]
    child: std::process::Child,
}

#[cfg(windows)]
impl Detached {
    /// `launch.env_remove` is only used on Linux.
    fn spawn(launch: &Launch) -> std::io::Result<Self> {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::Threading::{
            CREATE_BREAKAWAY_FROM_JOB, CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW, CreateProcessW,
            PROCESS_INFORMATION, STARTUPINFOW,
        };

        let mut line = quote(&launch.program.to_string_lossy());
        for arg in &launch.args {
            line.push(' ');
            line.push_str(&quote(arg));
        }
        let wide = |s: &std::ffi::OsStr| -> Vec<u16> { s.encode_wide().chain([0]).collect() };
        let directory = wide(root().as_os_str());
        let flags = CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW;
        // A job without JOB_OBJECT_LIMIT_BREAKAWAY_OK refuses to let go.
        for flags in [flags | CREATE_BREAKAWAY_FROM_JOB, flags] {
            // CreateProcessW may write to the command line.
            let mut command_line = wide(line.as_ref());
            // SAFETY: plain C structs, all zero is their empty value.
            let mut startup: STARTUPINFOW = unsafe { std::mem::zeroed() };
            startup.cb = size_of::<STARTUPINFOW>() as u32;
            let mut info: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
            // SAFETY: the strings are NUL-terminated and outlive the call.
            let created = unsafe {
                CreateProcessW(
                    std::ptr::null(),
                    command_line.as_mut_ptr(),
                    std::ptr::null(),
                    std::ptr::null(),
                    0,
                    flags,
                    std::ptr::null(),
                    directory.as_ptr(),
                    &startup,
                    &mut info,
                )
            };
            if created != 0 {
                // SAFETY: a handle CreateProcessW gave us and nothing else uses.
                unsafe { CloseHandle(info.hThread) };
                return Ok(Self {
                    pid: info.dwProcessId,
                    process: info.hProcess,
                });
            }
        }
        Err(std::io::Error::last_os_error())
    }

    fn pid(&self) -> u32 {
        self.pid
    }

    /// The exit code, once nuntio has exited.
    fn exit_status(&mut self) -> Option<String> {
        use windows_sys::Win32::Foundation::WAIT_OBJECT_0;
        use windows_sys::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject};

        // SAFETY: the process handle stays open for our lifetime.
        unsafe {
            if WaitForSingleObject(self.process, 0) != WAIT_OBJECT_0 {
                return None;
            }
            let mut code = 0;
            GetExitCodeProcess(self.process, &mut code);
            Some(format!("exit code {code:#x}"))
        }
    }
}

#[cfg(windows)]
impl Drop for Detached {
    fn drop(&mut self) {
        // SAFETY: our own handle; closing it doesn't end the process.
        unsafe { windows_sys::Win32::Foundation::CloseHandle(self.process) };
    }
}

#[cfg(unix)]
impl Detached {
    fn spawn(launch: &Launch) -> std::io::Result<Self> {
        use std::os::unix::process::CommandExt;
        let mut command = Command::new(&launch.program);
        for name in launch.env_remove {
            command.env_remove(name);
        }
        let child = command
            .args(&launch.args)
            .current_dir(root())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()?;
        Ok(Self { child })
    }

    fn pid(&self) -> u32 {
        self.child.id()
    }

    fn exit_status(&mut self) -> Option<String> {
        self.child.try_wait().ok().flatten().map(|s| s.to_string())
    }
}

/// An argument for a Windows command line, quoted the way programs split
/// it again (backslashes only escape before a quote).
#[cfg_attr(not(windows), allow(dead_code))]
fn quote(arg: &str) -> String {
    if !arg.is_empty() && !arg.contains([' ', '\t', '"']) {
        return arg.to_owned();
    }
    let mut out = String::from('"');
    let mut backslashes = 0;
    for c in arg.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                out.push_str(&"\\".repeat(backslashes * 2 + 1));
                out.push('"');
                backslashes = 0;
            }
            _ => {
                out.push_str(&"\\".repeat(backslashes));
                out.push(c);
                backslashes = 0;
            }
        }
    }
    out.push_str(&"\\".repeat(backslashes * 2));
    out.push('"');
    out
}

fn stop() -> Result<()> {
    request(json!({ "cmd": "quit" }))?;
    let started = Instant::now();
    while request(json!({ "cmd": "state" })).is_ok() {
        ensure!(
            started.elapsed() < Duration::from_secs(10),
            "nuntio is still running"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = fs::remove_file(state_file());
    println!("nuntio stopped");
    Ok(())
}

fn mouse(args: &mut Args) -> Result<()> {
    let kind = args
        .positional()
        .context("mouse: move, down, up, click or scroll")?;
    let mut request_json = json!({ "cmd": "mouse", "kind": kind });
    if let Some(i) = args.0.iter().position(|a| a == "--cell") {
        ensure!(i + 2 < args.0.len(), "--cell <column> <line>");
        let cell: Vec<String> = args.0.drain(i..i + 3).collect();
        request_json["column"] = cell[1].parse::<u64>()?.into();
        request_json["line"] = cell[2].parse::<u64>()?.into();
    }
    if let Some(pane) = args.option_u64("--pane")? {
        request_json["pane"] = pane.into();
    }
    if let Some(button) = args.option("--button")? {
        request_json["button"] = button.into();
    }
    if let Some(count) = args.option_u64("--count")? {
        request_json["count"] = count.into();
    }
    if let Some(lines) = args.option("--lines")? {
        request_json["lines"] = lines.parse::<f64>()?.into();
    }
    if let Some(mods) = args.option("--mods")? {
        request_json["mods"] = mods.split(',').collect::<Vec<_>>().into();
    }
    if let Some(x) = args.positional() {
        let y = args.positional().context("mouse: x needs a y")?;
        request_json["x"] = x.parse::<f64>()?.into();
        request_json["y"] = y.parse::<f64>()?.into();
    }
    ensure!(args.0.is_empty(), "mouse: unexpected {:?}", args.0);
    request(request_json)?;
    Ok(())
}

fn record_stop(args: &mut Args) -> Result<()> {
    let out = args
        .option("--dir")?
        .map_or_else(|| dir().join("recording"), PathBuf::from);
    let sheet = args.flag("--sheet");
    let video = args.option("--video")?;
    let reply = request(json!({ "cmd": "record_stop" }))?;

    // A new recording replaces the old one's frames.
    if out.is_dir() {
        for entry in fs::read_dir(&out)? {
            let path = entry?.path();
            if path
                .file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("frame-"))
            {
                fs::remove_file(path)?;
            }
        }
    }
    fs::create_dir_all(&out)?;
    let frames = reply.json["frames"].as_array().cloned().unwrap_or_default();
    let mut offset = 0;
    let mut images = Vec::new();
    let mut index = Vec::new();
    for (i, frame) in frames.iter().enumerate() {
        let (width, height) = size(frame);
        let bytes = frame["bytes"].as_u64().unwrap_or(0) as usize;
        let mut entry = json!({
            "frame": i,
            "ms": frame["ms"],
            "status": frame["status"],
        });
        if bytes > 0 {
            let rgba = reply.binary[offset..offset + bytes].to_vec();
            offset += bytes;
            let name = format!("frame-{i:03}.png");
            let pixmap = pixmap(width, height, rgba)?;
            pixmap.save_png(out.join(&name))?;
            entry["file"] = name.into();
            images.push((i, frame["ms"].as_f64().unwrap_or(0.0), pixmap));
        }
        index.push(entry);
    }
    fs::write(
        out.join("frames.json"),
        serde_json::to_string_pretty(&index)?,
    )?;
    println!(
        "{} frames, {} captured, in {}",
        frames.len(),
        images.len(),
        out.display()
    );
    if sheet && !images.is_empty() {
        let path = out.join("sheet.png");
        contact_sheet(&images)?.save_png(&path)?;
        println!("{}", path.display());
    }
    if let Some(video) = video {
        ensure!(!images.is_empty(), "no frames to make a video of");
        make_video(&out, &images, Path::new(&video))?;
        println!("{video}");
    }
    Ok(())
}

/// All frames scaled down in a grid, each with its number in the corner.
fn contact_sheet(images: &[(usize, f64, Pixmap)]) -> Result<Pixmap> {
    const THUMB_WIDTH: f32 = 480.0;
    const GAP: u32 = 8;
    let columns = (images.len() as f64).sqrt().ceil() as u32;
    let rows = (images.len() as u32).div_ceil(columns);
    let first = &images[0].2;
    let scale = (THUMB_WIDTH / first.width() as f32).min(1.0);
    let thumb_w = (first.width() as f32 * scale).ceil() as u32;
    let thumb_h = (first.height() as f32 * scale).ceil() as u32;
    let mut sheet = Pixmap::new(
        columns * (thumb_w + GAP) + GAP,
        rows * (thumb_h + GAP) + GAP,
    )
    .context("contact sheet too large")?;
    sheet.fill(tiny_skia::Color::from_rgba8(40, 40, 40, 255));
    for (n, (number, _, image)) in images.iter().enumerate() {
        let (column, row) = (n as u32 % columns, n as u32 / columns);
        let x = GAP + column * (thumb_w + GAP);
        let y = GAP + row * (thumb_h + GAP);
        let transform = Transform::from_row(scale, 0.0, 0.0, scale, x as f32, y as f32);
        let paint = PixmapPaint {
            quality: tiny_skia::FilterQuality::Bilinear,
            ..PixmapPaint::default()
        };
        sheet.draw_pixmap(0, 0, image.as_ref(), &paint, transform, None);
        draw_number(&mut sheet, x, y, *number);
    }
    Ok(sheet)
}

/// Digits of a 3x5 pixel font, one row of three bits per entry.
const DIGITS: [[u8; 5]; 10] = [
    [0b111, 0b101, 0b101, 0b101, 0b111],
    [0b010, 0b110, 0b010, 0b010, 0b111],
    [0b111, 0b001, 0b111, 0b100, 0b111],
    [0b111, 0b001, 0b111, 0b001, 0b111],
    [0b101, 0b101, 0b111, 0b001, 0b001],
    [0b111, 0b100, 0b111, 0b001, 0b111],
    [0b111, 0b100, 0b111, 0b101, 0b111],
    [0b111, 0b001, 0b010, 0b010, 0b010],
    [0b111, 0b101, 0b111, 0b101, 0b111],
    [0b111, 0b101, 0b111, 0b001, 0b111],
];

fn draw_number(pixmap: &mut Pixmap, x: u32, y: u32, number: usize) {
    const DOT: f32 = 3.0;
    let digits: Vec<usize> = number
        .to_string()
        .bytes()
        .map(|b| (b - b'0') as usize)
        .collect();
    let (x, y) = (x as f32, y as f32);
    let width = digits.len() as f32 * 4.0 * DOT + DOT;
    let mut paint = Paint::default();
    paint.set_color_rgba8(0, 0, 0, 200);
    if let Some(rect) = Rect::from_xywh(x, y, width, 7.0 * DOT) {
        pixmap.fill_rect(rect, &paint, Transform::identity(), None);
    }
    paint.set_color_rgba8(255, 220, 0, 255);
    for (i, digit) in digits.iter().enumerate() {
        for (row, bits) in DIGITS[*digit].iter().enumerate() {
            for column in 0..3 {
                if bits & (0b100 >> column) != 0 {
                    let dx = x + DOT + (i as f32 * 4.0 + column as f32) * DOT;
                    let dy = y + DOT + row as f32 * DOT;
                    if let Some(rect) = Rect::from_xywh(dx, dy, DOT, DOT) {
                        pixmap.fill_rect(rect, &paint, Transform::identity(), None);
                    }
                }
            }
        }
    }
}

/// An MP4 of the frames with their real timing, made by ffmpeg.
fn make_video(dir: &Path, images: &[(usize, f64, Pixmap)], out: &Path) -> Result<()> {
    ensure!(
        crate::available("ffmpeg"),
        "ffmpeg is not on the PATH; the frames are in {}",
        dir.display()
    );
    // The concat demuxer shows each file for its duration; the last one
    // must be listed twice for its duration to count.
    let mut list = String::new();
    for (n, (number, ms, _)) in images.iter().enumerate() {
        let next = images.get(n + 1).map_or(ms + 100.0, |(_, next, _)| *next);
        list.push_str(&format!(
            "file 'frame-{number:03}.png'\nduration {:.4}\n",
            ((next - ms) / 1000.0).max(0.001)
        ));
    }
    let (last, ..) = images[images.len() - 1];
    list.push_str(&format!("file 'frame-{last:03}.png'\n"));
    let list_path = dir.join("frames.txt");
    fs::write(&list_path, list)?;
    let first = &images[0].2;
    // x264 wants even sizes.
    let scale = format!("scale={}:{}", first.width() / 2 * 2, first.height() / 2 * 2);
    let out = std::path::absolute(out)?;
    run(Command::new("ffmpeg")
        .current_dir(dir)
        .args([
            "-y",
            "-loglevel",
            "error",
            "-f",
            "concat",
            "-safe",
            "0",
            "-i",
        ])
        .arg("frames.txt")
        .args([
            "-vf",
            &scale,
            "-fps_mode",
            "vfr",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
        ])
        .arg(&out))
}

fn size(json: &Value) -> (u32, u32) {
    (
        json["width"].as_u64().unwrap_or(0) as u32,
        json["height"].as_u64().unwrap_or(0) as u32,
    )
}

fn pixmap(width: u32, height: u32, rgba: Vec<u8>) -> Result<Pixmap> {
    let size = IntSize::from_wh(width, height).context("empty image")?;
    Pixmap::from_vec(rgba, size).context("image data does not match its size")
}

fn save_png(path: &Path, width: u32, height: u32, rgba: Vec<u8>) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    pixmap(width, height, rgba)?.save_png(path)?;
    Ok(())
}

fn lines(json: &Value) -> impl Iterator<Item = &str> {
    json["text"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
}

fn print_text(json: &Value) {
    let cursor = match json["cursor"].as_array() {
        Some(c) => format!("cursor {},{}", c[0], c[1]),
        None => "no cursor".into(),
    };
    println!("# {}x{}, {cursor}", json["columns"], json["lines"]);
    for line in lines(json) {
        println!("{line}");
    }
}

/// Backslash escapes for control characters in `type` and `paste`.
fn unescape(text: &str) -> Result<String> {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('r') => out.push('\r'),
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('e') => out.push('\u{1b}'),
            Some('\\') => out.push('\\'),
            Some('x') => {
                let hex: String = chars.by_ref().take(2).collect();
                let byte =
                    u8::from_str_radix(&hex, 16).with_context(|| format!("bad escape \\x{hex}"))?;
                out.push(char::from(byte));
            }
            other => bail!(
                "unknown escape \\{}",
                other.map_or(String::new(), String::from)
            ),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_become_control_characters() {
        assert_eq!(unescape(r"ls\r").unwrap(), "ls\r");
        assert_eq!(unescape(r"\e[A\x03\\").unwrap(), "\u{1b}[A\u{3}\\");
        assert!(unescape(r"\q").is_err());
    }

    #[test]
    fn windows_arguments_are_quoted() {
        assert_eq!(quote(r"C:\target\nuntio.exe"), r"C:\target\nuntio.exe");
        assert_eq!(quote(r"C:\my dir\"), r#""C:\my dir\\""#);
        assert_eq!(quote(r#"say "hi""#), r#""say \"hi\"""#);
        assert_eq!(quote(""), r#""""#);
    }

    #[test]
    fn args_are_taken_out() {
        let mut args = Args::new(&["a".into(), "--pane".into(), "3".into(), "b".into()]);
        assert_eq!(args.option_u64("--pane").unwrap(), Some(3));
        assert_eq!(args.positional().as_deref(), Some("a"));
        assert_eq!(args.rest(), ["b"]);
    }
}
