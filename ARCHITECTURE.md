# Architecture

This document describes how nuntio's processes, threads and crates fit together and how data moves between them. It is written for contributors who want to change nuntio and need the big picture first.

<p align="center"><img src="docs/architecture.svg" alt="nuntio system architecture: the main thread owning window, renderer and panes; one PTY reader thread per pane; service threads; shells, ConPTY, WSL via nuntio-wsl and the helper apps nuntio-config (settings TUI) and nuntio-apps notes as child processes; config files, the drive tool, CI and the GitHub releases API" width="100%"></p>

- [README.md](README.md): features and usage
- [CONTRIBUTING.md](CONTRIBUTING.md): building, testing, commit conventions
- [docs/config.md](docs/config.md): every setting
- [AGENTS.md](AGENTS.md): the `cargo xtask drive` workflow for driving a running nuntio

Contents:
[System overview](#system-overview) ·
[Workspace and crate boundaries](#workspace-and-crate-boundaries) ·
[Threads and events](#threads-and-events) ·
[Main-thread state](#main-thread-state) ·
[Output path](#output-path-pty-to-pixels) ·
[Input path](#input-path) ·
[Rendering](#rendering) ·
[Idle and timers](#idle-and-timers) ·
[Pane lifecycle](#pane-lifecycle) ·
[Inline images](#inline-images) ·
[herdr and tmux](#herdr-and-tmux) ·
[Configuration and hot reload](#configuration-and-hot-reload) ·
[nuntio-config](#nuntio-config-settings-tui) ·
[nuntio-apps](#nuntio-apps-notes) ·
[WSL panes](#wsl-panes-on-windows) ·
[Platform differences](#platform-differences) ·
[Background services](#background-services) ·
[Debug server](#debug-server-and-drive) ·
[Build and release](#build-packaging-and-release) ·
[Invariants](#invariants) ·
[Where to change what](#where-to-change-what)

## System overview

nuntio is a single-window terminal emulator with tabs and split panes. Terminal emulation (parsing, grid, scrollback) is done by `alacritty_terminal`; nuntio adds the PTY handling, inline images, the window, the UI around the panes and a GPU renderer built on wgpu. Configuration is a TOML file that is reloaded while nuntio runs.

```mermaid
flowchart LR
    user([User]) -->|keys, mouse, IME| win[OS window system / winit]
    subgraph proc[nuntio process]
        main[Main thread<br/>App = Core + WindowState]
        pty[PTY reader thread<br/>one per pane]
        svc[Service threads<br/>config watcher, sysmon, clipboard, links,<br/>update check and install,<br/>multiplexers, herdr agents]
    end
    win <--> main
    main -->|Frame| gpu[(GPU via wgpu)]
    pty <-->|bytes| shell[Shell or program<br/>in a PTY / ConPTY]
    pty -->|UserEvent::Term| main
    svc -->|UserEvent| main
    cfg[(config.toml, themes/)] -.->|notify| svc
    tui[nuntio-config TUI<br/>runs inside a pane] -->|atomic write| cfg
    notes[nuntio-apps notes<br/>runs inside a pane] -->|autosave| nf[(notes folder, .md)]
    svc -.->|opt-in| gh[(GitHub releases API)]
    pty <-->|framed pipes, Windows only| wsl[wsl.exe → nuntio-wsl<br/>Linux PTY]
    drive[cargo xtask drive] -.->|TCP JSON, debug-server feature| main
```

## Workspace and crate boundaries

```mermaid
flowchart TD
    nuntio["nuntio<br/>bins: nuntio, nuntio-config, nuntio-apps"] --> render[nuntio-render]
    nuntio --> term[nuntio-term]
    nuntio --> config[nuntio-config]
    nuntio --> tui[nuntio-config-tui]
    nuntio --> apps[nuntio-apps]
    apps --> config
    apps --> termui[nuntio-tui]
    render --> term
    tui --> config
    tui --> termui
    term -.->|"cfg(windows): proto only"| wsl[nuntio-wsl]
    term --> alacritty[alacritty_terminal]
    render --> wgpu[wgpu + cosmic-text]
    nuntio --> winit[winit]
    tui --> ratatui[ratatui]
    apps --> ratatui
    termui --> ratatui
```

|Crate|Owns|Must not know about|
|---|---|---|
|[`nuntio`](crates/nuntio)|The winit loop, `App`/`Core`/`WindowState`, tabs, `PaneTree`, input encoding, actions, overlays (tab bar, status bar, find bar, banner, menus), background services, the debug server. No lib target: `app_input.rs`, `app_panes.rs`, `app_mux.rs` and `debug_server.rs` are child modules of `app.rs` via `#[path]`, so they reach private `Core` state; likewise `update_install.rs` is the child module `update::install` of `update.rs`.|—|
|[`nuntio-term`](crates/nuntio-term)|One PTY and IO thread per pane, ownership of the `Term`, image scanning, decoding and storage, `Snapshot`, palette, search, URLs (`url::link_at` also joins links a program broke across rows, judging each panel between box-drawing separators, U+2500..259F, on its own), cwd and foreground process.|winit, wgpu|
|[`nuntio-render`](crates/nuntio-render)|wgpu, fonts, glyph and image atlases. Input is only a `Frame` and `nuntio_term::Snapshot`.|config, tabs, winit beyond raw window handles|
|[`nuntio-config`](crates/nuntio-config)|Config types, `schema.rs`, loading and validation, `edit.rs`, themes, `keys.rs`, the file watcher, shell and WSL detection (`detect.rs`).|any other nuntio crate|
|[`nuntio-config-tui`](crates/nuntio-config-tui)|The ratatui settings editor: its logic in `state.rs` (behind the `Store` trait) and `state_keys.rs` (key handling), the state of its choice list and the key mapping of its text input in `widgets.rs`, the splitting of program arguments in `args.rs`; `ui.rs` draws.|the running nuntio|
|[`nuntio-tui`](crates/nuntio-tui)|What both terminal apps share: terminal setup and restore (`terminal.rs`: raw mode, alternate screen, bracketed paste, optional mouse and focus events, panic hook), key normalization (releases dropped, AltGr as text on Windows) and the OSC 52 clipboard; the single-line `TextInput` (`text_input.rs`, each app maps its keys to `Edit`); the bordered popup (`popup.rs`); `ACCENT`, `Tone` and the small drawing pieces they share (`style.rs`: `heading`, `panel`, `center`, `too_small`, `hints`).|any other nuntio crate|
|[`nuntio-apps`](crates/nuntio-apps)|Terminal apps that nuntio opens in panes, one subcommand each: `notes` (Markdown notes). Terminal I/O only in `notes/mod.rs`; `notes/ui.rs` draws. The rest (`store`, `buffer`, `markdown`, `edit`, `layout`, `state`, `widgets`) takes crossterm events and ratatui styles but never touches the terminal.|the running nuntio|
|[`nuntio-wsl`](crates/nuntio-wsl)|The Linux-only relay binary and `proto`, which nuntio-term shares on Windows.|nuntio internals|
|[`xtask`](xtask)|Icons, packaging, changelog, website, `drive`.|nuntio crates: it talks via processes, files and TCP|

Binaries: `nuntio` is a GUI-subsystem binary in release builds on Windows. `nuntio-config` ([`crates/nuntio/src/bin/nuntio-config.rs`](crates/nuntio/src/bin/nuntio-config.rs)) and `nuntio-apps` ([`crates/nuntio/src/bin/nuntio-apps.rs`](crates/nuntio/src/bin/nuntio-apps.rs)) are shims that call `nuntio_config_tui::main()` and `nuntio_apps::main()`; both are console programs on every OS. `nuntio-wsl` is built for Linux only.

Design rule: pure logic (tabs, pane tree, tab bar layout, key and mouse encoding, config parsing) stays free of GPU and window types, so it can be unit-tested.

## Threads and events

Only the main thread touches the window, the `Renderer` (which holds `Rc` and is not `Send`), tabs and panes. Every other thread reaches it only through `EventLoopProxy<UserEvent>::send_event`.

|Name|Spawned in|Job|Ends when|
|---|---|---|---|
|main|`main::run` → `run_app`|runs winit|process exit|
|`PTY reader`|`nuntio-term` `IoLoop::spawn`|one per pane: polls the backend, parses into `Term`, writes input|`Msg::Shutdown` or child exit|
|unnamed scoped worker|`IoLoop` `work_serving`|decodes one image while the PTY thread keeps serving|the decode finishes|
|`nuntio-wsl input/stdout/stderr/waiter`|`wsl_pipe.rs` (Windows)|the pipe transport of WSL panes|the pane closes|
|`config watcher`|`nuntio-config` `watch.rs`|debounces notify events, calls `on_change`|`ConfigWatcher` dropped (it holds only a `Weak`)|
|`sysmon`|`sysmon.rs` `SystemMonitor::start` (a `ticker.rs` `Ticker`)|samples for the status bar|`SystemMonitor` dropped|
|`update-check`|`update.rs` `Checker::start` (a `ticker.rs` `Ticker`)|the periodic release check|`Checker` dropped|
|`update-check-now`|`update::check_now`|one manual check|after one check|
|`update-install`|`update::install::start`|downloads, checks and installs a release package update, reports `UserEvent::UpdateInstalled`|after one install|
|`clipboard-read`|`Core::request_paste`|reads the clipboard off the main thread|after one read|
|`link-open`|`Core::open_url`|runs `link::check` and opens the link|after one link|
|`multiplexers`|`mux/service.rs` `MuxService::start`|finds herdr/tmux (also in WSL), lists sessions, machines, key bindings and the project every 60 s and when the shell menu opens; owns the agent monitors|`MuxService` dropped|
|`herdr agents`|`mux/agents.rs` `Monitor::start`|one per running herdr session: `agent.list` and `events.subscribe` on its socket|`Monitor` dropped or the session ends|
|`font preload`|`nuntio_render::preload_fonts`|builds the `FontSystem` while the window and GPU start|joined by `Fonts::new`|
|`debug-server` + one unnamed thread per connection|`debug_server::start` (feature)|the TCP accept loop / `serve`|process exit / connection close|

`UserEvent` ([`crates/nuntio/src/event.rs`](crates/nuntio/src/event.rs)):

|Variant|Sender|Handler|
|---|---|---|
|`Term(PaneId, TermEvent)`|PTY thread, via the callback passed to `TermHandle::spawn`|`Core::term_event`|
|`ConfigChanged`|config watcher|`reload_config`|
|`SystemStats(Sample)`|sysmon|status bar; redraw only if `status_bar_changed`|
|`Update(Checked)`|update threads|`update_checked`|
|`UpdateInstalled(Installed)`|`update-install`|`update_installed`|
|`Menu(MenuCommand)`|macOS menu target|`run_action` / `request_close`|
|`Debug(DebugCall)` (feature `debug-server`)|debug connections|`debug_call`|
|`Paste(ClipboardPaste)`|clipboard-read|`clipboard_pasted`|
|`LinkFailed(String)`|link-open|banner|
|`Mux(Box<MuxSnapshot>)`|multiplexers|`mux_snapshot`: kept for the shell menu and forwarding|
|`Agents(AgentsUpdate)`|herdr agents|`agents_update`: status bar item, tab markers, banner|
|`MuxOpen(MuxOpen)`|multiplexers|`open_mux`: switch to the session's tab or open one|

`TermEvent` handling on the main thread:

- `Wakeup`: redraw, or set the `activity` flag on a background tab.
- `TitleChanged`: `take_title`.
- `Bell`: `ack_bell` and a window attention request.
- `Exit`: `close_pane`.
- `ClipboardStore`: OSC 52 → clipboard.
- `ColorQuery`: `answer_color_queries`.
- `HelperFailed` (Windows): banner.

## Main-thread state

`App { core: Core, state: Option<WindowState> }` ([`crates/nuntio/src/app.rs`](crates/nuntio/src/app.rs)). `WindowState` is created in `resumed()` by `Core::create_window`.

The split exists so handlers can borrow both halves at once. `Core` holds everything that isn't window state: config, themes, palette, bindings, banner, services, clipboard, pending close and paste, the pane id counter and the startup args. `WindowState` ([`crates/nuntio/src/window.rs`](crates/nuntio/src/window.rs)) holds the window, the renderer, `Tabs<TabContent>`, chrome, mouse/IME/menu/find-bar state, timers and the `shot_pool`.

Ownership: `Tabs<TabContent>` → `TabContent { tree: PaneTree, panes: Vec<Pane>, focused }` → `Pane { id, term: TermHandle, … }`.

`PaneTree` ([`crates/nuntio/src/pane_tree.rs`](crates/nuntio/src/pane_tree.rs)):

- `Node::Leaf(PaneId) | Node::Split { axis, ratio, first, second }`, plus `zoomed: Option<PaneId>`.
- `layout(area, gap)` returns the pane rects and dividers.
- `neighbor` finds the next pane in a direction by geometry.

`Tabs::close` refuses to close the last tab; the app sets `Core::exit` (`Exit::Quit`) instead.

## Output path: PTY to pixels

```mermaid
sequenceDiagram
    participant S as Shell
    participant IO as PTY reader (IoLoop)
    participant T as Term (FairMutex)
    participant M as Main thread (Core)
    participant R as Renderer
    S->>IO: output bytes
    IO->>IO: CwdScanner, then ImageScanner
    IO->>T: vte Processor::advance (lease + lock, at most MAX_LOCKED_READ per pass)
    IO-->>M: UserEvent::Term(id, Wakeup), coalesced
    M->>M: request_redraw
    M->>T: snapshot_into (lock held only for the copy)
    M->>R: render(&Frame)
    R-->>M: FrameStatus
```

- Read pipeline ([`crates/nuntio-term/src/io_loop.rs`](crates/nuntio-term/src/io_loop.rs)): `CwdScanner` (OSC 7 / OSC 9;9 → `ReportedDir`) → `ImageScanner` (takes out OSC 1337 `File=`, Kitty APC graphics and `CSI 16 t`) → the `alacritty_terminal` vte parser.
- Term lock: `term.lease()` reserves the next turn of the `FairMutex`. The lock is released after `MAX_LOCKED_READ` bytes, so the main thread can draw during floods.
- Wakeups are coalesced: `wakeup_pending` is cleared by the next snapshot. DEC 2026 synchronized updates hold the wakeup until the update ends or `sync_timeout` fires.
- No damage tracking: each frame copies the whole visible grid into a pooled `Snapshot` (`WindowState::shot_pool`); `Snapshot::refresh` resolves colors, selection and search highlights.
- Frame assembly is in [`window_frame.rs`](crates/nuntio/src/window_frame.rs) (`with_frame`): pane snapshots; an overlay layer in the order tab bar, dividers, status bar, find bar, link hint, banner; a popup layer for menus. The result is `Frame { background, background_opacity, panes, rects, texts, popup_rects, popup_texts, corner_radius }`.
- Reveal: a new tab or split keeps showing the old frame until the shell's output pauses or `REVEAL_MAX` passes (`reveal_holds`), so the user doesn't see a half-drawn prompt.

## Input path

```mermaid
flowchart TD
    key[WindowEvent::KeyboardInput] --> kp[KeyPress::from]
    kp --> ki[Core::keyboard_input]
    ki -->|menu or find bar open| ui[menu_key / search_key]
    ki -->|Bindings::lookup hit| act[Core::run_action]
    ki -->|otherwise| enc[input::encode_key<br/>kitty_keys or xterm style]
    enc --> w[TermHandle::write]
    w --> q[Msg::Input via LoopSender<br/>mpsc + Poller::notify]
    q --> io[IoLoop write queue] --> pty[PTY]
```

- `Bindings::from_config` puts config entries before `platform_defaults` (Cmd on macOS, Ctrl/Ctrl+Shift elsewhere); the first match wins.
- Key and action names are parsed in `nuntio-config` (`KeyCombo`, `ACTIONS`); `nuntio::actions` maps them to `Action`.
- `run_action` is the single dispatcher for keys, menus, the macOS menu bar and the debug server.
- Mouse: `mouse_press` tries, in order:
  1. the open menu,
  2. the banner,
  3. window chrome (tab bar, resize edges, drag),
  4. status items,
  5. links,
  6. dividers,
  7. pane focus.

  Then it either reports to the program (`mouse::encode_report`: X10, SGR 1006, UTF-8 1005; reporting is bypassed while Shift is held) or starts a selection.
- Paste:
  1. The `clipboard-read` thread reads the clipboard and sends `UserEvent::Paste`.
  2. `confirm_paste`: multi-line text without bracketed paste needs a second paste to confirm.
  3. `TermHandle::paste` sends it.
  4. Images go to `PastedImages`, which writes a PNG and pastes its path, quoted by `shell_words::path_word` (as are dropped files).
- Terminal replies (DA, `CSI 16 t`, OSC color queries) go through the `Listener` reply queue, capped by `ReplyBudget`. Color queries are answered on the main thread (`answer_color_queries`); later replies queue behind them to keep the order.

## Rendering

`Renderer` ([`crates/nuntio-render`](crates/nuntio-render)):

- `new`, `render(&Frame) -> FrameStatus`, `resize`.
- `set_font_family` / `set_font_size` clear the glyph caches.
- `release_surface` / `restore_surface` let a successor renderer take over the window (software fallback when `window.gpu_acceleration` changes).
- `capture` (feature `capture`) renders offscreen.

Pipeline:

- One instanced-quad shader, `quad.wgsl`. An `Instance` holds position, size, uv, color and kind. Kinds: `KIND_SOLID`, `KIND_MASK`, `KIND_COLOR`, `KIND_ROUNDED`, `KIND_IMAGE`, `KIND_IMAGE_SCALED`.
- Three pipelines: alpha blend; `fill` (REPLACE, for a pane's own background); `cutout` (transparent rounded window corners).
- One render pass. Each pane is scissored to its area. Draw order inside a pane: backgrounds → images → cursor → underline/strikeout → glyphs → dim veil. Then the UI, popups and cutouts.

Atlases (`atlas.rs`, etagere): mask (R8), color (RGBA, emoji) and image (RGBA). When full they grow, then clear, then skip missing glyphs. Glyph and cluster sprites are cached in `HashMap`s; images are cached by `TermImage::uid` and uploaded once. `ImageCache` (`images.rs`) owns the image atlas with the regions of the uploaded images and forgets them whenever the atlas is cleared, grown or replaced.

Glyphs come from cosmic-text (shaping, fallback, emoji vs text presentation). Box drawing, Powerline and media symbols are drawn procedurally (`box_drawing.rs`), as are curly, dotted and dashed underlines (`decoration.rs`).

Color: the surface format is forced to non-sRGB, so theme sRGB values reach the screen unchanged. On macOS the `CAMetalLayer` colorspace is set to sRGB.

GPU selection honors `WGPU_BACKEND`, `WGPU_ADAPTER_NAME` and `WGPU_POWER_PREF`; adapters that can't present to the surface are skipped. On Windows, opaque windows prefer DX12 and present through DirectComposition (`dcomp.rs`); transparent windows stay on Vulkan.

`FrameStatus` handling: `RenderHealth::on_frame` (`render_health.rs`, one per window) turns each status into a `HealthAction` that `Core::redraw_requested` carries out:

- `Skipped`: retry, up to `MAX_FRAME_RETRIES`.
- `Paused` (occluded): wait for the next event.
- `Lost`: `rebuild_renderer`, up to `MAX_RENDERER_REBUILDS`, then a "GPU error" banner.

## Idle and timers

The loop runs with `ControlFlow::Wait`. In `about_to_wait`, `WindowState::run_timers` returns the earliest deadline (`WaitUntil`) of:

- the reveal,
- the cursor blink, only if the program asked for a blinking cursor,
- the tab-title refresh,
- autoscroll during a selection drag.

The status bar redraws only when its content changes, and `sysmon` pauses while the window is occluded. Invariant: an idle window uses ~0% CPU.

## Pane lifecycle

`Core::spawn_pane(dir, launch, size)` ([`crates/nuntio/src/app_panes.rs`](crates/nuntio/src/app_panes.rs)):

- `Launch::{Shell(ShellChoice), Command(argv), Helper { program, args, name }}`.
- Builds `SpawnOptions` with the `Program` (`Default`; `Login(shell)` for the user's shell; `Command(shell)` for commands and helpers), the `Transport` (`Pty`, or `WslHelper { fallback }` on Windows), `term_options`, the palette and the `pane_env` (`NUNTIO_CONFIG`, `PATH` with the helper dir, the macOS locale; empty for WSL).
- The event callback is `proxy.send_event(UserEvent::Term(id, ev))`.

`nuntio-term` sets `TERM=xterm-256color`, `COLORTERM`, `TERM_PROGRAM`, `PI_FORCE_IMAGE_PROTOCOL=kitty` (not on native ConPTY) and `WINDOWID`. On macOS the shell is wrapped in `login(1)`.

Working directory for new tabs and splits, first hit wins:

1. the focused pane's cwd: from process info (`/proc` on Linux, `proc_pidinfo` on macOS), else the OSC-reported dir; translated between Windows and WSL paths;
2. `configured_dir`;
3. home.

Splits always inherit; tabs inherit only with `tabs.inherit_directory`.

Close:

1. `TermEvent::Exit` → `close_pane` → `PaneTree::remove` → `close_tab` when the tree is empty → exit on the last tab.
2. Dropping a `TermHandle` sends `Msg::Shutdown` and doesn't join the thread.
3. In `IoLoop`, `Backend` is the last field, because dropping the PTY can block until the child exits; the grid and images are freed first.
4. `ExitGuard` closes the pane even when the PTY thread panics.

`confirm_close` asks before closing when `foreground_is_shell() == Some(false)`. It doesn't apply to native Windows panes, where the foreground process isn't visible.

## Inline images

1. `ImageScanner` ([`image_scan.rs`](crates/nuntio-term/src/image_scan.rs)) cuts OSC 1337 `File=` sequences (and Kitty APC graphics, below) out of the stream before the parser sees them.
2. `parse_args` → `decode` runs on a scoped worker thread: base64, the `image` crate, EXIF orientation, scaling to cells, premultiplied RGBA.
3. The per-pane `ImageStore` dedupes by `ImageKey` and evicts by `IMAGE_MEMORY_BUDGET` / `IMAGE_MEMORY_LIMIT`.
4. `image::place` ([`image.rs`](crates/nuntio-term/src/image.rs)) writes placeholder cells: char `PLACEHOLDER` (U+10EEEE), fg = the 24-bit image id, and two zero-width marks for row and column. Images therefore scroll, reflow, clear and get overwritten like text. If `ESC 7 CSI n A` comes right before the image (omp draws images upward from their last row), rows that would lie above line 0 are left out (`rows_above_screen`) instead of shifting the whole image down.
5. `Snapshot.images` (runs of `ImagePiece`) feeds the renderer's image atlas.
6. `CSI 16 t` is answered with the cell size. Copy (`strip_image_text`) and URL detection skip placeholder cells.

Kitty graphics ([`kitty.rs`](crates/nuntio-term/src/kitty.rs)), which herdr sends to its outer terminal:

- `ImageScanner` also cuts out APC `ESC _ G … ESC \`. `Transfers` joins `m=1` chunks; `plan` → `prepare` (decoding on the worker) → `apply` (under the term lock).
- Transmitted images are kept per Kitty id in the `ImageStore` (`KITTY_SOURCE_LIMIT`, oldest dropped first). Each placement is rendered (source rect, offset, `c`×`r` cells) into its own `TermImage` and written as ordinary placeholder cells, `C=1` with `place_fixed` (cursor unchanged). Images replace the text in their cells; `z` only matters for deletes.
- Placing the same image and placement id again erases the old cells first (`erase_image`); `a=d` erases by the predicates of the visible cells.
- Not supported (answered `EINVAL`): Unicode placeholders, relative placements, animation, media other than direct (`t=d`).

## herdr and tmux

Opt-in (`[herdr]`, `[tmux]`); the code is in [`crates/nuntio/src/mux/`](crates/nuntio/src/mux/) and [`app_mux.rs`](crates/nuntio/src/app_mux.rs).

- `mux::scrub_inherited_env`, which `main` calls before any thread starts, removes `HERDR_*`, `TMUX` and `TMUX_PANE` from nuntio's own environment: panes inherit it, and herdr refuses to start "nested".
- A pane runs a multiplexer if nuntio started it there from the shell menu (`Pane::mux`) or its foreground process is called `herdr…`/`tmux…` (`mux::pane_mux`; in WSL through the helper's `fg` markers).
- `run_action` first offers the action to `forward_to_mux`: new tab, split, close pane, focus, resize and zoom become the tool's own keys (herdr's `[keys]` config, tmux's `list-keys -T prefix`), encoded for the pane's keyboard mode by `input::chord_bytes`. Tab switching and closing tabs stay nuntio's. Unbound actions fall through to nuntio; a nuntio split of such a pane runs the default shell, not a second client.
- The service thread resolves programs like a login shell would (`mux/resolve.rs`), on Windows also in every WSL distribution, and runs the tools' CLIs with a timeout (`command::output_within`). The shell menu shows its last snapshot and asks for a new one, which replaces the menu's sections in place.
- Agents: one monitor per running herdr session lists them over the socket and re-lists on pushed events. The main thread keeps them per `SessionKey` for the `agents` item, the tab markers (`WindowState::agent_status`) and, with `herdr.agent_banner` on, the banner when one becomes blocked out of view.
- Images: herdr passes only Kitty graphics on to its outer terminal and gives its panes the environment of its server. Every nuntio pane therefore gets `PI_FORCE_IMAGE_PROTOCOL=kitty`, so a herdr server started in one hands it to omp in its panes. A server that still runs with an older value (`iterm2`) has to be restarted.

## Configuration and hot reload

Loading ([`crates/nuntio-config/src/load.rs`](crates/nuntio-config/src/load.rs)):

- `--config`, else `locate_config`: `$XDG_CONFIG_HOME/nuntio/config.toml` or `~/.config/nuntio/config.toml`, else `~/.nuntio.toml`. Same paths on every OS.
- Unknown keys are warnings (`serde_ignored`).
- `Config::validate` uses the range constants from `schema.rs`.
- Errors keep the previous config and show a banner.

`SETTINGS` in [`schema.rs`](crates/nuntio-config/src/schema.rs) is the single source for every setting (kind, allowed values, help, platform). It drives the TUI, and tests keep it in sync with `Config::default()` and [docs/config.md](docs/config.md).

```mermaid
sequenceDiagram
    participant E as Editor or nuntio-config
    participant FS as File system
    participant W as config watcher thread
    participant M as Main thread
    E->>FS: save (nuntio-config: write_atomically = temp file + rename)
    FS-->>W: notify event on the parent directory
    W->>W: 150 ms debounce, re-resolve symlinks
    W-->>M: UserEvent::ConfigChanged
    M->>M: reload_config: load + validate
    alt invalid
        M->>M: error banner, previous Config stays
    else valid
        M->>M: themes, Bindings, palette to every pane
        M->>M: sync sysmon and update checker
        M->>M: apply_config_to_window (diff old vs new)
    end
```

- The watcher watches directories, not files, because editors replace files atomically. It also watches the themes dir, symlink targets, and the closest existing ancestor of directories that don't exist yet.
- `apply_config_to_window` diffs old against new: a renderer rebuild on `gpu_acceleration`, font family and size, `set_options` for all panes on `term_options` changes, `resize_terms`.
- `restart_warnings` covers what can't change live: decorations, the macOS titlebar, opacity on a window created opaque.
- Never re-created on reload: the window and existing panes. Shell and profile settings affect only new panes. Everything read per frame (keybindings, status bar, `dim_inactive`) applies on the next frame.
- Themes: 20 built-ins via `include_str!` from [`crates/nuntio-config/themes/`](crates/nuntio-config/themes); user `themes/*.toml` and `*.itermcolors` override them by name; `ThemeSelection::Auto { light, dark }` follows winit's `ThemeChanged` (`Core::os_dark`).

## nuntio-config (settings TUI)

`nuntio-config` runs as a separate process inside a nuntio tab and is not linked into nuntio's event loop.

- `Action::OpenSettings` → `Launch::Helper { program, .. }` with `pane_env::helper(CONFIG_HELPER)`. The helpers live in `pane_env::helper_dir` (next to the exe or in `../lib/nuntio`). They are never on the global PATH, only prepended to the PATH of nuntio's panes.
- Config path precedence: `--config`, then `NUNTIO_CONFIG`, then `locate_config` (`nuntio_config::config_path`, shared with `nuntio-apps`; nuntio itself sets `nuntio_config::CONFIG_ENV` in its panes).
- The `App` sits behind the `Store` trait (`FileStore` in production, in-memory in tests).
- Every valid edit re-reads the file, applies the change via `ConfigDoc` (toml_edit, keeps comments), validates with `nuntio_config::parse` and writes immediately with `write_atomically`. It is atomic: temp file + rename, follows a symlink to its target, fsyncs the directory on Unix. The notes app and the update check state use it too.
- There is no IPC: nuntio's watcher picks up the write, so hot reload is the live preview.

## nuntio-apps (notes)

`nuntio-apps notes` is another helper process: `Action::OpenNotes` opens it in a new tab, `Action::OpenNotesSplit` through `Core::split_with` next to the focused pane, both as `Launch::Helper` with `pane_env::helper(APPS_HELPER)`.

- Notes are flat `.md` files in `notes.directory` (default `<data dir>/nuntio/notes`); `--dir` overrides it, the config path follows the same precedence as `nuntio-config`. Deleted notes move to `.trash/`.
- `App` (`notes/state.rs`) sits behind the `Store` trait (`DirStore` in production, in-memory in tests). Autosave 1 s after the last edit via `write_atomically`; a file stamp (mtime + length) detects changes by other programs, which reload a clean note and ask on a dirty one.
- Live preview: `markdown.rs` renders the whole note with pulldown-cmark into per-line pieces (rendered and raw), `layout.rs` wraps them; the cursor line is shown raw.
- It blocks in `event::read` while idle; it only polls with a timeout while an autosave is pending. Copy goes out as OSC 52.

## WSL panes on Windows

ConPTY moves inline images to the wrong place, so WSL panes get a real Linux PTY through a helper running inside WSL.

```mermaid
flowchart LR
    subgraph winhost[Windows: nuntio.exe]
        io[IoLoop] <--> pipe[WslPipe<br/>threads: input, stdout, stderr, waiter]
    end
    pipe -->|stdin: DATA / RESIZE frames| wslexe[wsl.exe]
    wslexe -->|stdout: READY marker, then raw PTY bytes| pipe
    subgraph linux[WSL distribution]
        helper[nuntio-wsl relay<br/>single poll loop] <--> ptm[Linux PTY] <--> sh[Shell]
    end
    wslexe <--> helper
```

Launch: `nuntio_config::Shell::command` builds

```text
wsl.exe -d <distro> [-u <user>] --cd <dir> --exec /bin/sh -c 'exec "$(wslpath -u "$0")" "$@"' <helper path> [-- <program> <args>]
```

Wire protocol ([`crates/nuntio-wsl/src/proto.rs`](crates/nuntio-wsl/src/proto.rs)):

- nuntio → helper (stdin): frames `[kind: u8][len: u32 LE][payload]`. `DATA = 0` carries input bytes; `RESIZE = 1` carries `columns, lines, cell_width, cell_height`, each u16 LE. A payload is at most `MAX_FRAME` (1 MiB). The first frame must be `RESIZE`.
- helper → nuntio (stdout): raw PTY bytes with in-band markers `ESC ] nuntio-wsl;<body> BEL`: `ready` once at the start, then `fg;…` foreground reports. `MarkerFilter` strips them; `wsl_pipe.rs` stores the foreground in `TermHandle`'s `remote_foreground`, which `foreground` and `foreground_is_shell` fall back to. stderr carries helper messages.

Relay ([`crates/nuntio-wsl/src/relay.rs`](crates/nuntio-wsl/src/relay.rs)):

- `openpty`, the shell as session leader (`setsid` + `TIOCSCTTY`).
- One `poll` loop over stdin, the PTY master and a SIGCHLD self-pipe.
- Back-pressure on input; stdin EOF hangs up the shell.
- Exit code 127 for a missing program, 2 for data before the first size.
- After each PTY read and SIGCHLD (and polling for a second after input or output), `tcgetpgrp` on the master: a new foreground group is reported as `ESC ] nuntio-wsl;fg;<0|1>;<comm> BEL` (1: the shell itself) before the bytes of that read.

Windows pipes can't be polled, so `WslPipe` ([`wsl_pipe.rs`](crates/nuntio-term/src/wsl_pipe.rs)) posts IOCP completion packets to the `Poller`. Output is buffered with a cap (back-pressure).

Fallback: if wsl.exe exits before READY, `BackendEvent::HelperFailed` → `IoLoop::fall_back` starts ConPTY with the same shell → `TermEvent::HelperFailed` shows a banner. Without a helper binary, WSL panes use plain ConPTY from the start.

`nuntio-wsl socket <path>` is a second mode: it copies between its standard streams and a Unix socket inside the distribution, so that nuntio on Windows can talk to a herdr server in WSL (`mux/agents.rs`).

Build: [`crates/nuntio/build.rs`](crates/nuntio/build.rs) cross-builds `nuntio-wsl` for `x86_64-unknown-linux-musl` (rust-lld) next to `nuntio.exe`. Without that target installed it only prints a cargo warning.

## Platform differences

|Concern|Linux|macOS|Windows|
|---|---|---|---|
|PTY|forkpty (alacritty `tty`)|forkpty + `login(1)`|ConPTY; WSL via `nuntio-wsl`|
|Default shell|`$SHELL` / passwd|`$SHELL` / passwd|`powershell`|
|Window chrome|System; undecorated with own rounded corners (transparent window) for `decorations = "custom"` or under WSLg, whose Weston crashes on winit's client-side decorations|`Chrome::TitlebarInset` or native titlebar; menu bar in `macos_menu.rs`|Undecorated custom chrome with DWM rounded corners, or system|
|Foreground process / cwd|`/proc`|`proc_pidinfo`|none for native panes (OSC 7 / 9;9 only); WSL panes through the helper get the foreground's name from `fg` markers|
|Clipboard|arboard + primary selection (middle click)|arboard|arboard|
|GPU|Vulkan, GL only as fallback (avoids EGL noise on WSLg)|Metal, sRGB layer|DX12 + DirectComposition when opaque, else Vulkan|
|Startup errors|log only|NSAlert|MessageBoxW; `restrict_dll_search` against a foreign `conpty.dll`|
|Shortcut modifier|Ctrl+Shift|Cmd|Ctrl+Shift|

## Background services

- Status bar `sysmon`: started only if a placed `StatusItem` needs sampling (`sysmon::is_sampled`); synced on every reload.
- Update check ([`update.rs`](crates/nuntio/src/update.rs)):
  - opt-in: `[updates] check = true` plus at least one indicator;
  - GET `https://api.github.com/repos/cebor/nuntio/releases/latest`, using only `tag_name` and the asset names, with conditional ETag requests;
  - state in the cache dir (`nuntio/update.toml`);
  - source builds only inform (banner, tab-bar badge, status item); the wording depends on `NUNTIO_BUILD=release`, which `cargo xtask package` sets.
- Self-update of release packages (`update_install.rs`, child module `update::install`):
  - `install::detect` at startup classifies the canonical exe path: tarball (`<prefix>/bin` + `<prefix>/lib/nuntio`), AppImage (`$APPIMAGE` with the exe inside `$APPDIR`), .deb (`/usr/bin/nuntio` + dpkg list), macOS bundle, Windows directory; anything else falls back to informing;
  - offer (banner, badge and status item open the update menu) or install right away (`updates.install`); `Core::update_progress` tracks Installing/Installed;
  - the thread takes `nuntio/update.lock` in the cache dir, downloads `SHA256SUMS` and the asset from `releases/download/<tag>/`, checks the SHA-256 (`ring`) and swaps it in: renames within the install's filesystem (staged as `.nuntio-update-<pid>*`, leftovers removed at the next start), `pkexec apt-get install` for the .deb, `hdiutil` + `ditto` for the .dmg, system `tar` for .tar.gz and .zip;
  - restart: `CloseTarget::Restart` (confirmed like quitting) or `updates.restart` polling every 5 s in `about_to_wait` until every pane's foreground is its shell; `App::finish` returns the `Install` and `main` starts it detached after the panes are gone, passing only `--config` and `--log-level`.
- Pasted images: a private temp dir per process; stale dirs from earlier runs are swept, the current one is removed in `exiting`.

## Debug server and drive

Feature `debug-server` (enables `nuntio-render/capture`), never in packages.

- Transport: `TcpListener` on 127.0.0.1 with a random port. The state file `{port, token, pid}` (mode 0600 on Unix) is written to the path given by `--debug-server <file>`. One JSON line per request (with the token), one JSON line per reply, plus raw RGBA bytes for screenshots.
- Each request becomes `UserEvent::Debug` and is handled on the main thread (`Core::debug_request`), so input goes through the real paths (`keyboard_input`, `mouse_input`, `run_action`).
- Screenshots use `Renderer::capture` offscreen, so they work while the window is covered. While the server is on, the window acts as focused.
- Headless: Xvfb via `xvfb-run` on Linux and WSL; `--headless` (off-screen, out of the taskbar) on Windows; a transparent window with the Accessory activation policy on macOS.
- The client is [`xtask/src/drive.rs`](xtask/src/drive.rs); [AGENTS.md](AGENTS.md) lists the commands.

## Build, packaging and release

Details in [CONTRIBUTING.md](CONTRIBUTING.md).

|OS|nuntio|nuntio-config, nuntio-apps|nuntio-wsl|
|---|---|---|---|
|Linux (tar.gz, deb, AppImage)|`bin/nuntio` (`/usr/bin`)|`lib/nuntio/`|—|
|macOS|`nuntio.app/Contents/MacOS/nuntio`|same directory|—|
|Windows (zip, installer)|`nuntio.exe`|`.exe` next to it|`nuntio-wsl` (musl ELF) next to it|

- CI ([`ci.yml`](.github/workflows/ci.yml)): fmt on Linux; clippy and tests on all three OSes, with and without `nuntio/debug-server`; MSRV and packages on all three; docs, cargo-deny and site on Linux.
- [`release.yml`](.github/workflows/release.yml) on `v*` tags: version check → the whole `ci.yml` on the tagged commit and packages → GitHub release with notes from `cargo xtask changelog` (`Changelog:` trailers) → Pages rebuild.

## Invariants

- Only the main thread owns the window, the renderer and the panes; other threads send `UserEvent`.
- The term lock is held only to parse a bounded chunk or to copy a snapshot.
- Lock order: term first, then images/replies.
- Idle CPU stays at ~0: redraw only on events or timer deadlines.
- Pure logic stays free of GPU and window types and is unit-tested.
- Every setting has a `schema.rs` entry and a `docs/config.md` row (enforced by tests).
- The helper binaries (`nuntio-config`, `nuntio-apps`) are never on the global PATH.
- An invalid config never replaces a valid one.
- The reusable library crates (`nuntio-config`, `nuntio-term`, `nuntio-render`) use `thiserror`; the binary and the app crates and their support library (`nuntio-config-tui`, `nuntio-apps`, `nuntio-tui`) use `anyhow`; logging uses `tracing`.

## Where to change what

|Task|Touch|
|---|---|
|New action|`Action`, `NAMES` and `Action::label` (`actions.rs`; the menus take their titles from it), `Bindings::platform_defaults`, `Core::run_action`, `menu::MENU`, `nuntio_config::ACTIONS`, the actions table in the docs|
|New setting|the `Config` type, `SETTINGS` in `schema.rs`, `docs/config.md`; apply it in `reload_config` / `apply_config_to_window` or read it per frame|
|New status item|`nuntio_config::StatusItem`, `status_bar.rs`, `sysmon::is_sampled` / `Sample` if it samples, the click in `mouse_press`|
|New background source|a `UserEvent` variant in `event.rs`, an arm in `App::user_event`, send through a cloned proxy|
|New quad kind|a `KIND_*` constant in both `renderer.rs` and `quad.wgsl` (duplicated on purpose; keep them equal)|
|New procedural glyph|`box_drawing::rasterize`|
|New underline style|`decoration.rs` + `push_underline`|
|New escape sequence nuntio intercepts|`image_scan.rs` (before the parser) or `osc_cwd.rs`; everything else belongs to `alacritty_terminal`|
|New multiplexer action to forward|`MuxAction` and the binding parsers in `mux/keys.rs`, `mux_action` in `app_mux.rs`|
|New debug command|`debug_server::Request`, `Core::debug_request`, `xtask/src/drive.rs`|
|New built-in theme|a file in `crates/nuntio-config/themes/`, the `BUILTIN` array in `theme.rs`, the theme list in the docs|
