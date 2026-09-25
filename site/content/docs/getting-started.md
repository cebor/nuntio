+++
title = "Getting started"
description = "Install nuntio on Linux, macOS or Windows, set up the config file and learn the keyboard shortcuts."
weight = 1
+++

## Install

Download the package for your platform from the [home page](@/_index.md) or the [latest GitHub release](https://github.com/cebor/nuntio/releases/latest).

| Platform | Package | Install |
|---|---|---|
| macOS (Intel and Apple Silicon) | `nuntio-<version>-macos-universal.dmg` | Open the image and drag nuntio to Applications |
| Linux (Debian / Ubuntu) | `nuntio_<version>-1_amd64.deb` | `sudo apt install ./nuntio_<version>-1_amd64.deb` |
| Linux (any distribution) | `nuntio-<version>-x86_64-linux.AppImage` | `chmod +x` it and run it |
| Linux (any distribution) | `nuntio-<version>-x86_64-linux.tar.gz` | Unpack it; it contains `bin/`, a desktop file and icons |
| Windows | `nuntio-<version>-x86_64-windows.zip` | Unpack it and run `nuntio.exe` |

### First launch

The packages are not signed yet.

- **macOS**: the first start is blocked. Allow it in **System Settings** → **Privacy & Security** → **Open Anyway**, or run `xattr -dr com.apple.quarantine /Applications/nuntio.app`. (Right-click → **Open** no longer works since macOS 15.)
- **Windows**: SmartScreen may ask you to confirm with **More info** → **Run anyway**.

### Build from source

You need current stable Rust; `rust-toolchain.toml` selects the stable channel with rustfmt and clippy, and `rustup` installs it automatically. On Linux you also need the windowing development packages:

```sh
# Debian / Ubuntu
sudo apt-get install libxkbcommon-dev libwayland-dev libx11-dev libxcursor-dev libxrandr-dev libxi-dev
```

Then:

```sh
git clone https://github.com/cebor/nuntio.git
cd nuntio
cargo run -r
```

## Configure

nuntio runs with sensible defaults and needs no config file. To change something, create `~/.config/nuntio/config.toml` (or `~/.nuntio.toml`). The paths are the same on Linux, macOS and Windows, so one dotfiles repo works everywhere.

```toml
[font]
family = "JetBrains Mono"
size = 14.0

# Follow the OS appearance
[theme]
light = "Solarized Light"
dark = "Tokyo Night"

[panes]
dim_inactive = 0.2

[[keybindings]]
key = "Ctrl+Shift+N"
action = "new_tab"
```

Changes apply as soon as you save the file. If something is wrong, a banner in the window shows the error and the previous settings stay active. You can also run `nuntio-config` in a nuntio tab to change settings in a terminal UI ([Editing in the terminal](@/docs/config.md#editing-in-the-terminal)). The [configuration reference](@/docs/config.md) covers every option with its default, custom themes, and all keybinding actions.

## Keyboard shortcuts

On macOS, nuntio uses iTerm2's shortcuts. On Linux and Windows, most shortcuts add <kbd>Shift</kbd> so that plain <kbd>Ctrl</kbd> combinations still reach the shell. All shortcuts can be changed or disabled in the [config](@/docs/config.md#keybindings).

| Action | Linux / Windows | macOS |
|---|---|---|
| Copy / paste | <kbd>Ctrl</kbd><kbd>Shift</kbd><kbd>C</kbd> / <kbd>Ctrl</kbd><kbd>Shift</kbd><kbd>V</kbd> | <kbd>Cmd</kbd><kbd>C</kbd> / <kbd>Cmd</kbd><kbd>V</kbd> |
| Paste | <kbd>Shift</kbd><kbd>Insert</kbd> | <kbd>Shift</kbd><kbd>Insert</kbd> |
| New tab | <kbd>Ctrl</kbd><kbd>Shift</kbd><kbd>T</kbd> | <kbd>Cmd</kbd><kbd>T</kbd> |
| Close pane (or tab) | <kbd>Ctrl</kbd><kbd>Shift</kbd><kbd>W</kbd> | <kbd>Cmd</kbd><kbd>W</kbd> |
| Next / previous tab | <kbd>Ctrl</kbd><kbd>Tab</kbd> / <kbd>Ctrl</kbd><kbd>Shift</kbd><kbd>Tab</kbd> | <kbd>Cmd</kbd><kbd>Shift</kbd><kbd>]</kbd> / <kbd>Cmd</kbd><kbd>Shift</kbd><kbd>[</kbd> |
| Go to tab 1–9 | <kbd>Alt</kbd><kbd>1</kbd> … <kbd>Alt</kbd><kbd>9</kbd> | <kbd>Cmd</kbd><kbd>1</kbd> … <kbd>Cmd</kbd><kbd>9</kbd> |
| Split side by side | <kbd>Ctrl</kbd><kbd>Shift</kbd><kbd>D</kbd> | <kbd>Cmd</kbd><kbd>D</kbd> |
| Split top / bottom | <kbd>Ctrl</kbd><kbd>Shift</kbd><kbd>E</kbd> | <kbd>Cmd</kbd><kbd>Shift</kbd><kbd>D</kbd> |
| Focus pane | <kbd>Ctrl</kbd><kbd>Alt</kbd><kbd>←↑→↓</kbd> | <kbd>Cmd</kbd><kbd>Opt</kbd><kbd>←↑→↓</kbd> |
| Resize pane | <kbd>Ctrl</kbd><kbd>Alt</kbd><kbd>Shift</kbd><kbd>←↑→↓</kbd> | <kbd>Cmd</kbd><kbd>Ctrl</kbd><kbd>←↑→↓</kbd> |
| Zoom pane | <kbd>Ctrl</kbd><kbd>Shift</kbd><kbd>Enter</kbd> | <kbd>Cmd</kbd><kbd>Shift</kbd><kbd>Enter</kbd> |
| Find | <kbd>Ctrl</kbd><kbd>Shift</kbd><kbd>F</kbd> | <kbd>Cmd</kbd><kbd>F</kbd> |
| Font size bigger / smaller / reset | <kbd>Ctrl</kbd><kbd>+</kbd> / <kbd>Ctrl</kbd><kbd>-</kbd> / <kbd>Ctrl</kbd><kbd>0</kbd> | <kbd>Cmd</kbd><kbd>+</kbd> / <kbd>Cmd</kbd><kbd>-</kbd> / <kbd>Cmd</kbd><kbd>0</kbd> |
| Clear scrollback | <kbd>Ctrl</kbd><kbd>Shift</kbd><kbd>K</kbd> | <kbd>Cmd</kbd><kbd>K</kbd> |
| Scroll a page | <kbd>Shift</kbd><kbd>PgUp</kbd> / <kbd>Shift</kbd><kbd>PgDn</kbd> | <kbd>Shift</kbd><kbd>PgUp</kbd> / <kbd>Shift</kbd><kbd>PgDn</kbd> |
| Scroll a line | <kbd>Ctrl</kbd><kbd>Shift</kbd><kbd>↑</kbd> / <kbd>↓</kbd> | <kbd>Cmd</kbd><kbd>↑</kbd> / <kbd>↓</kbd> |
| Reload config | <kbd>Ctrl</kbd><kbd>Shift</kbd><kbd>,</kbd> | <kbd>Cmd</kbd><kbd>Shift</kbd><kbd>,</kbd> |

In the find bar, <kbd>Enter</kbd> jumps to the next match, <kbd>Shift</kbd><kbd>Enter</kbd> to the previous one, <kbd>Alt</kbd><kbd>R</kbd> toggles regex mode and <kbd>Esc</kbd> closes the bar.

## Command line

```text
nuntio [--config <path>] [--log-level <level>]
```

| Flag | Description |
|---|---|
| `--config <path>` | Use this config file instead of the default location |
| `--log-level <level>` | Log filter such as `debug` or `nuntio=trace` (`RUST_LOG` also works) |
| `-V`, `--version` | Print the version |
| `-h`, `--help` | Print usage |

When nuntio is started without a terminal (from a desktop launcher, for example), it logs to `nuntio/nuntio.log` in the platform's cache directory, such as `~/.cache/nuntio/nuntio.log` on Linux (the previous run's log is kept as `nuntio.old.log`). If something goes wrong, that's the first place to look, and the log is welcome in a [bug report](https://github.com/cebor/nuntio/issues).
