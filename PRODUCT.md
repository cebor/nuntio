# Product

<!-- impeccable:product-schema 1 -->

## Platform

web

## Users

Linux power users and developers who run AI coding agents (Claude Code, omp, Codex) in the terminal all day, often several at once in herdr or tmux, and Windows developers who live in WSL. They evaluate a terminal by what it does for that workflow:

- getting screenshots into an agent's prompt
- seeing images the agent renders
- keeping notes and todos next to the agents
- configuring without leaving the terminal

They read code, know escape sequences and protocol names, and distrust marketing claims.

## Product Purpose

nuntio is a free, open-source (MIT or Apache-2.0), GPU-rendered terminal emulator in Rust for Linux, macOS and Windows: one window, tabs, split panes, one TOML config with hot reload.

The website does two jobs: it makes the AI-agent workflow case, and it hands out the current release for the visitor's OS. Success: within one viewport, a visitor understands that nuntio is the terminal built for working with AI agents, and then downloads the current release.

## Positioning

nuntio is the terminal for AI coding agents:

- **Image paste.** Paste a screenshot and nuntio saves it as a PNG and pastes its path, which Claude Code and omp attach to the prompt.
- **Inline images.** Images that agents show arrive inline via the Kitty graphics protocol and iTerm2's OSC 1337. They live in the grid and scroll like text, also in WSL.
- **Agent status.** herdr agent status (working, blocked, done) shows in the status bar, as a tab dot, and as a banner when an agent blocks out of view.
- **Notes and Settings.** Notes (Markdown + todos) and Settings run as terminal apps in a pane.
- **WSL.** On Windows, WSL panes get a real Linux PTY through the nuntio-wsl helper instead of ConPTY, byte for byte.

## Operating Context

nuntio is used for hours in tiling setups, beside editors and agents, often with dark themes.

Visitors evaluate it by:

- reading the README and the protocol support
- trying a download: AppImage, .deb, .tar.gz; universal .dmg; .exe installer / .zip
- checking escape-sequence compatibility

Packages are not signed yet: macOS needs Open Anyway, and Windows SmartScreen needs Run anyway.

## Capabilities and Constraints

- **Image paste:** clipboard image → `<tmp>/nuntio-paste-<pid>-<n>/image-N.png`, path pasted (Ctrl+Shift+V, Cmd+V on macOS); folder removed at exit.
- **Inline images:**
  - iTerm2 OSC 1337: PNG, JPEG, GIF first frame, WebP, BMP.
  - Kitty graphics: transmit/place/delete/query, RGB/RGBA/PNG, zlib, chunked.
  - Not supported: Sixel, Kitty unicode placeholders, relative placements, animation, file/shm transfer. Never claim these.
- **Protocols:**
  - Supported: Kitty keyboard protocol (all five flags), OSC 7, OSC 9;9, OSC 8 hyperlinks, synchronized output (DEC 2026), focus events, SGR mouse, true colour, curly/dotted/dashed underlines, colour queries.
  - OSC 52 is write-only; reading is denied.
  - No desktop notifications.
- **Notes:** `nuntio-apps notes`, Ctrl+Shift+N (tab) / Ctrl+Alt+Shift+N (split); plain `.md` files, live Markdown preview, `- [ ]` todos with per-note progress, todo overview across notes, autosave.
- **Settings:** `nuntio-config` TUI (Ctrl+,).
  - It offers only valid values and previews themes and fonts.
  - It writes the TOML and keeps comments; hot reload is the preview.
  - Errors show a banner and keep the previous config.
  - 20 built-in themes, plus custom TOML or .itermcolors.
- **WSL:** `shell = { wsl = "Ubuntu" }`, distro picker, nuntio-wsl relay, foreground-process tab titles, herdr/tmux found inside distros, ConPTY fallback with a banner.
- **herdr/tmux:** sessions in the shell menu, key forwarding for splits/focus/zoom, herdr agent monitor.
- **Performance:** idle window ~0% CPU; wgpu rendering with CPU fallback.
- **Website:** Zola 0.23 static site on GitHub Pages; release data from the GitHub API at build time; no cookies, analytics or third-party requests (PRIVACY.md).

## Brand Commitments

- The name is "nuntio", always lowercase.
- Keep the existing icon `assets/icon.svg` (rounded square, chevron + cursor bar).
- Write plain, technical English; no hype.

## Evidence on Hand

- Real product screenshots under `assets/screenshots/` (images, themes, nuntio-config, find-bar, status-bar, notes, settings) and `assets/screenshot.png`.
- New screenshots can be captured headless with `cargo xtask drive`.
- There are no testimonials, user counts, benchmarks or press: never invent them.

## Product Principles

1. Show the real thing: product claims are backed by real screenshots or exact protocol facts.
2. Honest compatibility: state what is not supported as plainly as what is.
3. The terminal is the workspace: features that keep the visitor in the terminal lead.
4. Respect the visitor: static, fast, no tracking, works without JavaScript.
