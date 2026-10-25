# AGENTS.md

nuntio is a terminal emulator in Rust for Linux, macOS and Windows: one window, tabs, split panes, GPU rendering, TOML config with hot reload.

## Commands

```sh
cargo run -r                                         # start nuntio (default workspace member)
cargo run -r -- --config <path> --log-level debug    # custom config / verbose logs
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo xtask package      # release build + packages for the host OS into dist/ (needs cargo-about)
cargo xtask icons        # re-render assets/icons/ from assets/icon.svg
cargo xtask changelog <from>..HEAD   # release notes from the Changelog: trailers (range defaults to HEAD)
cargo xtask site [serve] # build the website in site/ with Zola (needs `zola` on PATH)
```

Always pass `--workspace` to clippy and tests: `default-members` contains only `crates/nuntio`, so plain `cargo test` skips the other crates and `xtask`. CI (`.github/workflows/ci.yml`, on pushes to `main` and on pull requests, with `RUSTFLAGS=-D warnings`) runs fmt on Linux; clippy and tests on Linux, macOS and Windows, with and without `--features nuntio/debug-server`; the MSRV check (`rust-version` in `Cargo.toml`) and `cargo xtask package` on all three; docs, `cargo deny` and the website build on Linux. `release.yml` runs when a `v*` tag is pushed: version check, the whole `ci.yml` on the tagged commit and the packages, GitHub release with notes from `cargo xtask changelog` only once all of them passed, then the Pages rebuild (`pages.yml`).

## Gate

Every change that touches code passes the gate before it is committed:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo clippy --workspace --all-targets --features nuntio/debug-server -- -D warnings
cargo test --workspace
cargo test --workspace --features nuntio/debug-server
cargo deny --all-features check
```

A change to doc comments also runs `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps`. A UI change is checked the way a user meets it: driven headless with `cargo xtask drive` and screenshotted, not only compiled. The minimum Rust version, macOS, Windows, packaging and the website are checked by CI. Missing tools: `cargo install --locked cargo-deny` (gate), `cargo install --locked cargo-about` (`cargo xtask package`).

## Finishing a change

A change is finished when its gate passed and it is committed and pushed to `origin main`. A change without code, such as documentation only, skips the gate but is still committed and pushed. The agent does not wait for CI.

- Stage paths explicitly, never `git add -A` or `git add .`: the checkout may hold the user's uncommitted work, which is left alone. `SPEC.md` and other local planning files are never committed.
- Before pushing, `git pull --rebase origin main`; if that brought new commits, run the gate again. Push only to `origin` (GitHub), never to another remote such as `gitlab`.
- A red `main` comes first, whoever's commit broke it: before pushing, look once at the newest run (`gh run list --workflow ci.yml --branch main --limit 1`). If it failed and the cause is within reach (`gh run view <id> --log-failed`), fix it first in its own commit through the gate; otherwise push anyway and tell the user that `main` is red. A queued or running run is not waited for, and neither is the run of the own push.

## Seeing and driving nuntio (`cargo xtask drive`)

To look at the running terminal or try out a change, use the debug server instead of asking the user for screenshots:

```sh
cargo xtask drive start                  # build nuntio, nuntio-apps and nuntio-config with --features debug-server, start headless in the background (empty config); also --size WxH, --config <file>, -- <command>
cargo xtask drive wait-idle              # until no pane had output for 300 ms; `wait-text <text>` waits for text on screen
cargo xtask drive shot                   # target/nuntio-debug/shot.png (%LOCALAPPDATA%\nuntio\debug\shot.png on Windows), look at it with Read; --pane N for one pane
cargo xtask drive text                   # visible lines of the focused pane (--pane N); `cells --line L` for colors/attributes; `state` for window/tabs/panes/banner as JSON
cargo xtask drive type 'ls\r'            # also: paste <text>, key ctrl+shift+t, action split_vertical, mouse click 100 17 / --cell 5 3, resize 1280 800, window maximize
cargo xtask drive record start           # then act, then: record stop --sheet (contact sheet of every drawn frame; --video <file.mp4> needs ffmpeg)
cargo xtask drive stop                   # `drive help` lists everything; `drive log` shows this run's log (target/nuntio-debug/nuntio.log)
```

The server (`crates/nuntio/src/debug_server.rs`, a child module of `app.rs`) is only compiled with the `debug-server` feature, never in packages. Input goes through the real paths (`keyboard_input`, `mouse_input`, `run_action`); screenshots are rendered offscreen (`Renderer::capture`), so they work while the window is covered. The window opens without taking focus and acts as focused.

Always test headless, the default: nothing shows up on the user's desktop. Use `drive start --visible` only when the user wants to watch. Headless means off-screen and out of the taskbar on Windows (`--headless`; a hidden window would get no redraws there), a fully transparent window without Dock icon that ignores the mouse on macOS (AppKit pulls windows moved off-screen back onto the screen), and an Xvfb X server on Linux and WSL (needs `xvfb-run` and Mesa's Vulkan driver; without `xvfb-run`, `drive start` falls back to a visible window; there `state` reports `headless: false`, since the whole display is invisible).

## Architecture

ARCHITECTURE.md has the full picture (threads, UserEvent flow, output/input paths, rendering and GPU loss, hot reload, WSL protocol, invariants, "Where to change what"); read it before cross-crate changes.

| Crate | Role |
|---|---|
| `crates/nuntio` | Binary `nuntio` (`src/main.rs`: CLI flags, logging) and the thin wrappers `nuntio-config` and `nuntio-apps` (`src/bin/`), console programs even on Windows. winit event loop (`app.rs`: `App` = `Core` + `Option<WindowState>`, renderer rebuild with CPU fallback and GPU re-probe; handlers in the child modules `app_input.rs`, `app_panes.rs`, `app_mux.rs`; `UserEvent` in `event.rs`), window state and layout (`window.rs`, with `window_pointer.rs` and `window_frame.rs`), renderer health (`render_health.rs`: skipped and lost frames, GPU re-probe timing), tabs (`tabs.rs`, `tab_title.rs`), split tree (`pane_tree.rs`), key and mouse encoding (`input.rs`, `kitty_keys.rs`, `mouse.rs`), shortcuts (`actions.rs`) and the menus (`menu.rs`: actions, shells, tabs, agents), UI overlays (`tab_bar.rs`, `status_bar.rs`, `search_bar.rs`, `banner.rs`) with shared colors and `UiMetrics` (`style.rs`), background services (`update.rs` release check, `update_install.rs` self-update of release packages, `sysmon.rs` status-bar samples, `mux/` herdr and tmux: finding them, sessions, key bindings, agent monitors), helper commands with a timeout (`command.rs`), the macOS menu bar (`macos_menu.rs`) |
| `crates/nuntio-term` | Wrapper around `alacritty_terminal`: PTY per pane (`pane.rs`) with nuntio's own IO thread (`io_loop.rs`) over a `Backend` (`backend.rs`: the PTY or ConPTY, or on Windows the pipes to `nuntio-wsl`, `wsl_pipe.rs`), which takes inline images (iTerm2 OSC 1337 and Kitty APC graphics, `image_scan.rs`, `kitty.rs`) and `CSI 16 t` out of the stream before the parser; images live in the grid as placeholder cells (`image.rs`). `Snapshot` of the visible screen for rendering, palette, search, URL detection, working directory from OSC 7 / OSC 9;9 (`osc_cwd.rs`), foreground process info (`process.rs`) |
| `crates/nuntio-wsl` | `nuntio-wsl`, a Linux binary that nuntio on Windows starts in WSL through `wsl.exe` with plain pipes: it owns a Linux PTY for the shell (`relay.rs`) and speaks a small framed protocol (`proto.rs`, also used by nuntio-term) so the output arrives byte for byte, without ConPTY, with in-band reports of the foreground process. `nuntio-wsl socket <path>` bridges a Unix socket (herdr's) to its standard streams |
| `crates/nuntio-render` | wgpu renderer: one instanced-quad pipeline for backgrounds, glyphs (cosmic-text, R8 + RGBA atlases), inline images (RGBA image atlas) and UI; `Frame` and its parts in `frame.rs`; adapter choice, the software (CPU) adapter and device loss in `gpu.rs`; box-drawing, Powerline and media symbols are drawn procedurally (`box_drawing.rs`) |
| `crates/nuntio-config` | Config types, `schema.rs` (every setting with kind, allowed values, help; tests keep it in sync with `Config` and `docs/config.md`), loading/validation (`load.rs`), `edit.rs` (toml_edit, keeps comments), key-combo syntax and action names (`keys.rs`), themes, file watcher, installed shells and WSL distros for pickers (`detect.rs`) |
| `crates/nuntio-tui` | What both terminal apps share (`terminal.rs`): terminal setup and restore (raw mode, alternate screen, bracketed paste, optional mouse and focus events), key normalization, OSC 52 clipboard; `text_input.rs`: the single-line `TextInput` (each app maps its keys to `Edit`); `popup.rs`: the bordered popup; `style.rs`: `ACCENT`, `Tone`, `heading`, `panel`, `center`, `too_small`, `hints`. A library, not a helper binary |
| `crates/nuntio-config-tui` | The `nuntio-config` TUI (ratatui). Logic in `state.rs` behind a `Store` trait, tested without a terminal; `ui.rs` only draws. Every valid change is written at once, and hot reload is the preview |
| `crates/nuntio-apps` | `nuntio-apps <command>`: terminal apps that nuntio opens in panes. `notes` (actions `open_notes`, `open_notes_split`): Markdown notes as `.md` files in `notes.directory`, live preview (pulldown-cmark, `markdown.rs` + `layout.rs`), todos. Logic in `notes/state.rs` behind a `Store` trait (`notes/store.rs`); `ui.rs` only draws |
| `xtask` | Icons, packaging (`HELPERS` lists the helper binaries packages ship), changelog, website build (`site`) in `main.rs`; debug driver (`drive`) in `drive.rs`; `args.rs` parses every command's arguments and rejects leftovers |
| `site` | Zola website on GitHub Pages (`pages.yml`, also run by `release.yml`). The download section reads the latest release from the GitHub API at build time; `docs/config.md` is copied in by `cargo xtask site`. Zola 0.23 uses Tera 2: components instead of macros |

Data flow: PTY threads send `UserEvent::Term(PaneId, TermEvent)` via the winit proxy. The main thread takes a `Snapshot` per visible pane (the term lock is only held for the copy) and hands a `Frame` (panes + `UiRect`s + `UiText`s) to the renderer. It redraws only on events or timer deadlines (no damage tracking): idle CPU must stay at ~0 (`ControlFlow::Wait`; cursor blinking uses `WaitUntil`).

`nuntio-config` and `nuntio-apps` aren't on the global PATH: packages put them in `lib/nuntio/` (Linux) or next to the executable, and `pane_env.rs` prepends that directory to `PATH` and sets `NUNTIO_CONFIG` only for nuntio's own panes. New settings need an entry in `schema.rs` and a row in `docs/config.md` (tests enforce both).

Keep pure logic (tabs, pane tree, tab bar layout, key/mouse encoding, config parsing) free of GPU/window types so it stays unit-testable. That's where most tests live; `crates/nuntio-term/tests/` runs real shells: `spawn.rs` and `hangup.rs` a `/bin/sh` (Linux/Unix), `spawn_windows.rs` the Windows backends.

## Conventions

- Code, comments, commit messages and repo docs in English. The user writes to you in German.
- Commit messages: imperative subject ≤ 72 chars. For user-visible changes add a `Changelog: added|changed|deprecated|removed|fixed|security|performance` trailer; the subject becomes the release note. See CONTRIBUTING.md.
- Errors: `thiserror` in the reusable library crates (`nuntio-config`, `nuntio-term`, `nuntio-render`), `anyhow` in the binary and the app crates and their support library (`nuntio-config-tui`, `nuntio-apps`, `nuntio-tui`). Log with `tracing`.
- `SPEC.md` (the German product spec), `TODO-engines.md` and `docs/proposals/` are local planning files: they stay untracked, never add them to git.
- Text files are LF everywhere (`.gitattributes`): tests and `include_str!` rely on it.

## Environment gotchas

- The dev machine is WSL2/WSLg. wgpu only gets llvmpipe (software Vulkan), so don't trust local performance numbers. Elsewhere, `window.gpu_acceleration = false` prefers the software adapter and `WGPU_ADAPTER_NAME` picks one by name; if a renderer can't be created nuntio falls back to the other kind, and after a device loss it draws on the CPU until a GPU is back.
- WSLg's Weston crashes on winit's client-side decorations. nuntio detects WSLg and runs undecorated; the tab bar then provides window dragging, resize edges and min/max/close buttons.
- If nuntio "crashes" under WSLg, check `/mnt/wslg/stderr.log` for a Weston segfault first.
- Started without a terminal, logs go to `~/.cache/nuntio/nuntio.log` (`~/Library/Caches/nuntio/nuntio.log` on macOS), the previous run's to `nuntio.old.log` next to it; with a terminal they go to stderr. `cargo xtask drive` runs log to `target/nuntio-debug/nuntio.log` (`drive log`).
- On Windows, WSL panes run the Linux helper `nuntio-wsl` (crate `crates/nuntio-wsl`) through `wsl.exe` with plain pipes, so they get a real Linux PTY instead of ConPTY, whose built-in version puts inline images at the wrong place. Windows packages ship it next to the exe; on Windows, `cargo build` (also check/clippy/test, so `drive start` too) cross-builds it from `crates/nuntio/build.rs` into `target/<profile>` when `rustup target add x86_64-unknown-linux-musl` is installed, and otherwise only prints a cargo warning. Without it, or if it doesn't start, WSL panes fall back to ConPTY. Native panes (cmd, PowerShell) always use ConPTY and get no `PI_FORCE_IMAGE_PROTOCOL`.
