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
cargo xtask changelog <from>..HEAD
cargo xtask site [serve] # build the website in site/ with Zola (needs `zola` on PATH)
```

Always pass `--workspace` to clippy and tests: `default-members` contains only `crates/nuntio`, so plain `cargo test` skips the other crates. CI (`.github/workflows/ci.yml`) runs fmt, clippy and tests on all three OSes, also with `--features nuntio/debug-server`; `release.yml` builds packages when a `v*` tag is pushed.

## Seeing and driving nuntio (`cargo xtask drive`)

To look at the running terminal or try out a change, use the debug server instead of asking the user for screenshots:

```sh
cargo xtask drive start                  # build with --features debug-server, start headless in the background (empty config)
cargo xtask drive wait-idle              # until no pane had output for 300 ms
cargo xtask drive shot                   # target/nuntio-debug/shot.png (%LOCALAPPDATA%\nuntio\debug\shot.png on Windows), look at it with Read
cargo xtask drive text                   # visible lines of the focused pane; `state` for tabs/panes/banner as JSON
cargo xtask drive type 'ls\r'            # also: key ctrl+shift+t, action split_vertical, mouse click 100 17 / --cell 5 3
cargo xtask drive record start           # then act, then: record stop --sheet (contact sheet of every drawn frame)
cargo xtask drive stop                   # `drive help` lists everything; `drive log` shows nuntio's log
```

The server (`crates/nuntio/src/debug_server.rs`, a child module of `app.rs`) is only compiled with the `debug-server` feature, never in packages. Input goes through the real paths (`keyboard_input`, `mouse_input`, `run_action`); screenshots are rendered offscreen (`Renderer::capture`), so they work while the window is covered. The window opens without taking focus and acts as focused.

Always test headless, the default: nothing shows up on the user's desktop. Use `drive start --visible` only when the user wants to watch. Headless means off-screen and out of the taskbar on Windows (`--headless`; a hidden window would get no redraws there), a fully transparent window without Dock icon that ignores the mouse on macOS (AppKit pulls windows moved off-screen back onto the screen), and an Xvfb X server on Linux and WSL (needs `xvfb-run` and Mesa's Vulkan driver; there `state` reports `headless: false`, since the whole display is invisible).

## Architecture

| Crate | Role |
|---|---|
| `crates/nuntio` | Binaries `nuntio` and `nuntio-config` (`src/bin/`, a console program even on Windows). winit event loop (`app.rs`: `App` = `Core` + `Option<WindowState>`; handlers in the child modules `app_input.rs`, `app_panes.rs`), window state and layout (`window.rs`, with `window_pointer.rs` and `window_frame.rs`), tabs (`tabs.rs`), split tree (`pane_tree.rs`), key and mouse encoding (`input.rs`, `mouse.rs`), shortcuts (`actions.rs`), UI overlays (`tab_bar.rs`, `status_bar.rs`, `search_bar.rs`, `banner.rs`) with shared colors and `UiMetrics` (`style.rs`), the macOS menu bar (`macos_menu.rs`) |
| `crates/nuntio-term` | Wrapper around `alacritty_terminal`: PTY per pane (`pane.rs`) with nuntio's own IO thread (`io_loop.rs`) over a `Backend` (`backend.rs`: the PTY or ConPTY, or on Windows the pipes to `nuntio-wsl`, `wsl_pipe.rs`), which takes inline images (iTerm2 OSC 1337, `image_scan.rs`) and `CSI 16 t` out of the stream before the parser; images live in the grid as placeholder cells (`image.rs`). `Snapshot` of the visible screen for rendering, palette, search, URL detection, foreground process info |
| `crates/nuntio-wsl` | `nuntio-wsl`, a Linux binary that nuntio on Windows starts in WSL through `wsl.exe` with plain pipes: it owns a Linux PTY for the shell (`relay.rs`) and speaks a small framed protocol (`proto.rs`, also used by nuntio-term) so the output arrives byte for byte, without ConPTY |
| `crates/nuntio-render` | wgpu renderer: one instanced-quad pipeline for backgrounds, glyphs (cosmic-text, R8 + RGBA atlases), inline images (RGBA image atlas) and UI; box-drawing characters are drawn procedurally |
| `crates/nuntio-config` | Config types, `schema.rs` (every setting with kind, allowed values, help; tests keep it in sync with `Config` and `docs/config.md`), loading/validation, `edit.rs` (toml_edit, keeps comments), key-combo syntax and action names (`keys.rs`), themes, file watcher |
| `crates/nuntio-config-tui` | The `nuntio-config` TUI (ratatui). Logic in `state.rs` behind a `Store` trait, tested without a terminal; `ui.rs` only draws. Every valid change is written at once, and hot reload is the preview |
| `xtask` | Icons, packaging, changelog, website build (`site`), debug driver (`drive`) |
| `site` | Zola website on GitHub Pages (`pages.yml`, also run by `release.yml`). The download section reads the latest release from the GitHub API at build time; `docs/config.md` is copied in by `cargo xtask site`. Zola 0.23 uses Tera 2: components instead of macros |

Data flow: PTY threads send `UserEvent::Term(PaneId, TermEvent)` via the winit proxy. The main thread takes a `Snapshot` per visible pane (the term lock is only held for the copy) and hands a `Frame` (panes + `UiRect`s + `UiText`s) to the renderer. It only redraws on damage: idle CPU must stay at ~0 (`ControlFlow::Wait`; cursor blinking uses `WaitUntil`).

`nuntio-config` isn't on the global PATH: packages put it in `lib/nuntio/` (Linux) or next to the executable, and `pane_env.rs` prepends that directory to `PATH` and sets `NUNTIO_CONFIG` only for nuntio's own panes. New settings need an entry in `schema.rs`.

Keep pure logic (tabs, pane tree, tab bar layout, key/mouse encoding, config parsing) free of GPU/window types so it stays unit-testable. That's where most tests live; `crates/nuntio-term/tests/spawn.rs` runs a real `/bin/sh`.

## Conventions

- Code, comments, commit messages and repo docs in English. The user writes to you in German.
- Commit messages: imperative subject ≤ 72 chars. For user-visible changes add a `Changelog: added|changed|deprecated|removed|fixed|security|performance` trailer; the subject becomes the release note. See CONTRIBUTING.md.
- Errors: `thiserror` in library crates, `anyhow` in the binary. Log with `tracing`.
- `SPEC.md` (the German product spec) stays local and uncommitted. Never add it to git.
- Text files are LF everywhere (`.gitattributes`): tests and `include_str!` rely on it.

## Environment gotchas

- The dev machine is WSL2/WSLg. wgpu only gets llvmpipe (software Vulkan), so don't trust local performance numbers.
- WSLg's Weston crashes on winit's client-side decorations. nuntio detects WSLg and runs undecorated; the tab bar then provides window dragging, resize edges and min/max/close buttons.
- If nuntio "crashes" under WSLg, check `/mnt/wslg/stderr.log` for a Weston segfault first.
- Started without a terminal, logs go to `~/.cache/nuntio/nuntio.log` (`~/Library/Caches/nuntio/nuntio.log` on macOS).
- On Windows, WSL panes run the Linux helper `nuntio-wsl` (crate `crates/nuntio-wsl`) through `wsl.exe` with plain pipes, so they get a real Linux PTY instead of ConPTY, whose built-in version puts inline images at the wrong place. Windows packages ship it next to the exe; on Windows, `cargo build` (also check/clippy/test, so `drive start` too) cross-builds it from `crates/nuntio/build.rs` into `target/<profile>` when `rustup target add x86_64-unknown-linux-musl` is installed, and otherwise only prints a cargo warning. Without it, or if it doesn't start, WSL panes fall back to ConPTY. Native panes (cmd, PowerShell) always use ConPTY and get no `PI_FORCE_IMAGE_PROTOCOL`.
