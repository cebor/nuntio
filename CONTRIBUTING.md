# Contributing to nuntio

Thanks for your interest! This document explains how to build and test nuntio and how to submit changes.

## Prerequisites

- Current stable Rust. The toolchain is pinned in `rust-toolchain.toml`; `rustup` installs it automatically.
- Linux: development packages for windowing and input, e.g. on Debian/Ubuntu:
  ```sh
  sudo apt-get install libxkbcommon-dev libwayland-dev libx11-dev libxcursor-dev libxrandr-dev libxi-dev
  ```
- macOS and Windows need nothing extra.

## Build & run

```sh
cargo run                         # debug build
cargo run --release               # for performance testing
cargo run -- --log-level debug    # verbose logging (or use RUST_LOG)
```

## Before opening a pull request

CI runs these checks on Linux, macOS and Windows:

```sh
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

For changes to rendering or terminal emulation, please also check the affected cases manually, e.g. `vim`, `htop`, `tmux`, `less`, colored `git log`, emoji/CJK output and box drawing (`tree`, TUI borders).

## Layout

| Crate | Contents |
|---|---|
| `crates/nuntio` | Binary: event loop, app state, input |
| `crates/nuntio-config` | Config schema, loading, themes |
| `crates/nuntio-render` | wgpu renderer |
| `crates/nuntio-term` | Wrapper around `alacritty_terminal` and the PTY |

Conventions:
- Errors: `thiserror` in library crates, `anyhow` in the binary.
- Log with `tracing`, not `println!`.
- Keep pure logic (config, pane layout, key encoding) free of GPU/window dependencies so it can be unit tested.

## Commits & changelog

The changelog is generated from **commit trailers**. There is no hand-maintained CHANGELOG file.

### Commit message

```
Short summary in imperative mood (≤ 72 chars)

Optional body: what and why, not how.
Wrap lines at ~72 characters.

Changelog: fixed
```

- The **subject line becomes the changelog entry**. Write it so users can understand it.
- The `Changelog:` trailer goes in the last paragraph of the message, separated by a blank line.
- At most one `Changelog:` trailer per commit.

### Categories

| Trailer | Use for |
|---|---|
| `Changelog: added` | New feature |
| `Changelog: changed` | Changed behavior of an existing feature |
| `Changelog: deprecated` | Feature scheduled for removal |
| `Changelog: removed` | Feature removed |
| `Changelog: fixed` | Bug fix |
| `Changelog: security` | Security-relevant fix |
| `Changelog: performance` | Noticeable performance improvement |

**No trailer** means the commit is left out of the changelog. That's right for refactorings, CI, tests, internal docs and anything users won't notice.

### Examples

```
Add split panes with keyboard focus navigation

Changelog: added
```

```
Fix cursor drawn one cell off after font size change

Changelog: fixed
```

```
Extract glyph atlas packing into its own module
```

### Listing changelog entries

```sh
git log --format='%(trailers:key=Changelog,valueonly,separator=)%x09%s' <last-tag>..HEAD \
  | awk -F'\t' '$1 != ""'
```

### Pull requests

- Keep a PR to one topic. Multiple commits are fine as long as each one makes sense on its own.
- If a PR is squash-merged, the `Changelog:` trailer must be kept in the final squash message.
- For larger changes, please open an issue first to discuss the approach.

## License

Contributions are licensed under MIT **or** Apache-2.0, at the user's option; see [LICENSE-MIT](LICENSE-MIT) and [LICENSE-APACHE](LICENSE-APACHE). By submitting a contribution you agree to this dual licensing.
