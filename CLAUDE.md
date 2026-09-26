# CLAUDE.md

nuntio is a terminal emulator in Rust for Linux, macOS and Windows, modeled on iTerm2: one window, tabs, split panes, GPU rendering, TOML config with hot reload.

## Commands

```sh
cargo run -r                                         # start nuntio (default workspace member)
cargo run -r -- --config <path> --log-level debug    # custom config / verbose logs
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo xtask package      # release build + packages for the host OS into dist/
cargo xtask icons        # re-render assets/icons/ from assets/icon.svg
cargo xtask changelog <from>..HEAD
cargo xtask site [serve] # build the website in site/ with Zola (needs `zola` on PATH)
```

Always pass `--workspace` to clippy and tests: `default-members` contains only `crates/nuntio`, so plain `cargo test` skips the other crates. CI (`.github/workflows/ci.yml`) runs fmt, clippy and tests on all three OSes; `release.yml` builds packages when a `v*` tag is pushed.

## Architecture

| Crate | Role |
|---|---|
| `crates/nuntio` | Binaries `nuntio` and `nuntio-config` (`src/bin/`, a console program even on Windows). winit event loop (`app.rs`), window state and layout (`window.rs`), tabs (`tabs.rs`), split tree (`pane_tree.rs`), key and mouse encoding (`input.rs`, `mouse.rs`), shortcuts (`actions.rs`), UI overlays (`tab_bar.rs`, `search_bar.rs`, `banner.rs`), the macOS menu bar (`macos_menu.rs`) |
| `crates/nuntio-term` | Wrapper around `alacritty_terminal`: PTY and IO thread per pane (`pane.rs`), `Snapshot` of the visible screen for rendering, palette, search, URL detection, foreground process info |
| `crates/nuntio-render` | wgpu renderer: one instanced-quad pipeline for backgrounds, glyphs (cosmic-text, R8 + RGBA atlases) and UI; box-drawing characters are drawn procedurally |
| `crates/nuntio-config` | Config types, `schema.rs` (every setting with kind, allowed values, help; tests keep it in sync with `Config` and `docs/config.md`), loading/validation, `edit.rs` (toml_edit, keeps comments), key-combo syntax and action names (`keys.rs`), themes, file watcher |
| `crates/nuntio-config-tui` | The `nuntio-config` TUI (ratatui). Logic in `state.rs` behind a `Store` trait, tested without a terminal; `ui.rs` only draws. Every valid change is written at once, and hot reload is the preview |
| `xtask` | Icons, packaging, changelog |
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
- macOS and Windows code paths can't be run locally; CI is the only check for them.
