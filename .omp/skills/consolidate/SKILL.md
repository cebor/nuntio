---
name: consolidate
description: Review the nuntio repository (or the crate or path given as argument) as if it were built from scratch today, pull duplicated code and state into one place, finish half-done migrations, align rival conventions, remove dead weight and stale docs, and land each consolidation as its own gated commit on main, pushed to origin main, together with a run log in docs/consolidations/; changes to observable behavior, configuration and other external interfaces, stored files or the WSL protocol only after the user approves them.
---

# consolidate

Goal: bring nuntio back to the shape it would have if it were written today. For every area the question is "how would we structure this today, knowing what nuntio has become?", not "what is the smallest diff?". Duplicated code moves into shared modules, rival conventions become one, dead weight goes.

Consolidation preserves behavior. It is not a feature run and not a bug-fix run: what nuntio does before and after a consolidation commit is the same, unless the user approved a specific behavior or compatibility change (step 5). A bug found during the review is a `behavior` finding.

This skill is in English; messages to the user are in the language the user writes in.

`AGENTS.md` applies in full: its conventions, the `cargo xtask drive` workflow and the rule that `SPEC.md` is never added to git. Two consequences matter most here:

- A security problem found during the review is reported to the user (step 8), never fixed on the side. nuntio is public; `SECURITY.md` describes private reporting.
- The local, untracked planning files of the main checkout (`SPEC.md`, `TODO-engines.md`, `docs/proposals/`) are read, never edited, deleted, moved or added to git. A finding about them goes only into the chat report.

## 1. Arguments and target repository

- Run `git rev-parse --show-toplevel` in the current directory. The main checkout is the first `worktree` entry of `git worktree list --porcelain`. If it contains `crates/nuntio-term/`, it is the target repository; otherwise use `~/code/nuntio`. Below, `<repo>` is that main checkout.
- Optional argument: a crate name (`nuntio-term`) or a repository-relative path (`crates/nuntio/src/tab_bar.rs`, `xtask`).
- Without an argument the scope is the whole repository: every crate under `crates/`, plus `xtask/`, `site/` (without the generated files listed in `.gitignore`), `assets/`, `.github/`, `docs/` (tracked files only) and the tracked root files (`*.md`, `Cargo.toml`, `deny.toml`, `about.toml`, `about.hbs`, `.cargo/config.toml`, `rust-toolchain.toml`).
- With an argument, duplicates across crates are still searched, but a finding is only consolidated if at least one side lies in the scope.
- Unknown crate or nonexistent path: tell the user that the repository has no crate or path `<arg>`, and stop.

## 2. Workspace

- `git -C <repo> fetch -q origin`.
- If `git -C <repo> status --porcelain --untracked-files=no` prints anything, or `<repo>` is not on `main`, work in a worktree: `git -C <repo> worktree add <repo>/../nuntio-consolidate -b consolidate-<YYYY-MM-DD> origin/main` (append `-2`, `-3` and so on to path and branch if they exist). The worktree has its own `target/`, so the first build is cold; do not point `CARGO_TARGET_DIR` at the main checkout's `target/`.
- Otherwise work in `<repo>` itself, on a new branch `consolidate-<YYYY-MM-DD>` from `origin/main`.
- The branch stays local. It lands on `main` by rebase in step 7 and is never pushed itself. Push only to `origin`, never to another remote such as `gitlab`.

## 3. Orientation

Read before reviewing:

- `AGENTS.md`, `CONTRIBUTING.md`, `ARCHITECTURE.md` (completely, in particular "Workspace and crate boundaries", "Invariants" and "Where to change what"), `README.md` and the root `Cargo.toml`.
- The planned direction, from `<repo>` (these files are untracked and do not exist in a worktree): sections 1, 2 and 11 of `SPEC.md` (vision, goals and non-goals, open points; in German), the headings of `TODO-engines.md`, and title, status line and first paragraph of every `docs/proposals/*.md`. A consolidation must not fight a planned feature. A plan only protects code while it is still planned: one the code already implements, or whose direction the code has since left, protects nothing and is mentioned in the chat report (step 8); the file itself stays untouched.
- `git log --oneline -40`: what moved recently.
- Every earlier run log, `docs/consolidations/*.md`: what was consolidated, dropped or declined before. A finding declined in an earlier run is not proposed again unless something it depended on has changed since; then name that change in the `ask`.

## 4. Review (parallel, read-only)

Spawn `scout` subagents in one batch. Slices for the whole repository:

1. `nuntio` core: `app.rs`, `app_input.rs`, `app_panes.rs`, `debug_server.rs`, `event.rs`, `main.rs`, `window.rs`, `window_pointer.rs`, `window_frame.rs`, `tabs.rs`, `pane_tree.rs`, `ime.rs`, `bin/`
2. `nuntio` UI and input: `tab_bar.rs`, `tab_title.rs`, `status_bar.rs`, `search_bar.rs`, `banner.rs`, `style.rs`, `actions.rs`, `menu.rs`, `macos_menu.rs`, `input.rs`, `kitty_keys.rs`, `mouse.rs`, `link.rs`
3. Services and platform glue: `update.rs`, `sysmon.rs`, `shells.rs`, `shell_words.rs`, `pane_env.rs`, `pasted_images.rs`, `wsl.rs` and `build.rs` of `nuntio`; the `nuntio-wsl` crate; `detect.rs` of `nuntio-config`
4. `nuntio-term`, including `tests/`
5. `nuntio-render`, including `quad.wgsl`
6. `nuntio-config` (without `detect.rs`, with `themes/`) and `nuntio-config-tui`
7. `nuntio-apps`
8. Tooling and packaging: `xtask`, `.github/`, `assets/`, `site/` (without generated files), root `Cargo.toml`, `deny.toml`, `about.toml`, `about.hbs`, `.cargo/config.toml`, `rust-toolchain.toml`
9. Cross-cutting: the same helper, type, constant, environment variable name, error type or pattern in more than one crate; a crate pinning its own version where `[workspace.dependencies]` has the dependency; duplicate dependency versions (`cargo tree -d`); dependencies a crate's `Cargo.toml` names but its code never uses (grep the crate for the dependency's `use` path, mind renamed and `features`-only dependencies); crate boundaries that do not match the "Owns" and "Must not know about" columns in `ARCHITECTURE.md`; the same platform split (`cfg(windows)`, `cfg(target_os = "macos")`, `cfg(unix)`) written differently in several places.
10. Documentation and configuration: `ARCHITECTURE.md` (including "Invariants" and "Where to change what", checked against the code), `docs/architecture.svg`, `README.md`, `CONTRIBUTING.md`, `AGENTS.md` (crate table and commands), `PRIVACY.md` (against what nuntio actually sends), `SECURITY.md`, `docs/config.md` against `schema.rs` and the `Config` types, `site/content/` and `assets/nuntio.metainfo.xml` against `README.md`. Findings: documents describing an older architecture or a removed path, crate tables that disagree with each other or with `Cargo.toml`, settings whose defaults, validation, example and documentation disagree, and settings nothing reads.

Files and crates added since this list was written join the slice they fit best. With an argument: one scout per top-level module of the scope (at most 8), plus the cross-cutting and the documentation scout, both limited to findings that touch the scope.

Every scout gets the same checklist:

- **Duplicates:** functions, types, constants, UI layout and color math, path handling (display, `~` expansion, WSL path translation), temp-file and atomic-write code, TUI terminal setup and key normalization, retry and backoff, size limits that exist twice or more, including near-duplicates that differ only in names or a parameter. Not a finding: code that looks alike but serves concepts that will change independently, and duplication documented as deliberate (the `KIND_*` constants in `renderer.rs` and `quad.wgsl`).
- **Duplicate state:** the same fact held in two places (a field that another field or a lookup already determines, a cache with no invalidation reason), repeated conversions between near-identical types, adapters between two abstractions that should be one. Name the single source of truth.
- **Half-finished migrations:** an old and a new way both still in use, where the code shows a move from one to the other that stopped part-way. The newer, deliberate direction wins; every remaining caller moves and the old way is deleted.
- **Rival conventions:** two ways of doing the same thing (two error styles, two config readers, two overlay drawing patterns, two logging styles). Name the one that should win and why.
- **Bolted-on features:** a feature handled as a special case beside the core model (its own arm in `Core::run_action`, `App::user_event` or the frame assembly, its own flag threaded through unrelated code) where it should be one case of the model, state and lifecycle that already exist.
- **Misplaced code:** logic in the wrong crate or module, for example window-free logic in `nuntio` that belongs in `nuntio-term` or `nuntio-config`, or GPU and window types in pure logic (an invariant in `ARCHITECTURE.md`).
- **Stale names:** types, functions, modules, fields, messages whose name describes earlier behavior and now misleads. Only where the meaning has drifted; a name that is merely not pretty is not a finding.
- **Speculative abstractions:** traits, generics, builders, extension points with one implementation and no credible second one. Keep abstractions that enforce a real boundary or invariant (the `Store` traits that keep the logic of `nuntio-config-tui` and `nuntio-apps` testable without a terminal); do not collapse useful boundaries to save files.
- **Error handling:** errors swallowed without reason (`let _ =`, `.ok()`, empty `Err(_) => {}`), vague messages without context, `anyhow` in a library crate or ad-hoc error types in the binary, `println!`/`eprintln!` where `tracing` belongs (usage and CLI output of `xtask` and the helper binaries are fine). Surfacing a swallowed error changes behavior: such findings take `compat: behavior`.
- **Dead weight:** unused code, compatibility shims, aliases, re-exports kept for old callers, stale features and `cfg` branches nothing enables, `#[allow(dead_code)]` hiding dead items, dead files and scripts, old sample configs, temporary debug code.
- **Comments and TODOs:** comments that narrate history ("previously", "now", "new") or explain a workaround whose cause is gone. Every `TODO`/`FIXME`/`XXX` in the slice is classified: still valid and actionable (stays, rewritten if vague), already done another way, obsolete, or too vague to act on (the last three are deleted). Comments on invariants, constraints, platform quirks and trade-offs stay.
- **Tests:** tests that pin obsolete internals, intermediate states of earlier development or private structure instead of behavior; tests that duplicate each other; tests that break the conventions (tests next to the code, temp dirs from `std::env::temp_dir()` plus the process id, no dev-dependencies, no hard-coded `/` path separators).
- **Greenfield deviations:** module or crate boundaries, types that should own an invariant, oversized files or functions that mix concerns.

Each scout returns its findings as one JSON object with an array `findings`, each item with

- `title` (string)
- `kind`: `duplicate` | `state` | `migration` | `convention` | `bolted` | `misplaced` | `naming` | `abstraction` | `error` | `dead` | `comment` | `test` | `docs` | `structure`
- `locations`: array of `path:line` strings
- `proposal` (string): target module or crate, new symbol names and signatures
- `compat`: `internal` | `behavior` | `wire` | `stored` | `interface`
- `size`: `small` (< 150 changed lines) | `medium` (< 800) | `large`

The scout justifies the `compat` class of every finding:

- `wire`: changes the framed protocol between nuntio and `nuntio-wsl` (`crates/nuntio-wsl/src/proto.rs`, its use in `crates/nuntio-wsl/src/relay.rs` and `crates/nuntio-term/src/wsl_pipe.rs`): frame layout, tags, the ready handshake.
- `stored`: changes files nuntio or its helpers write and read back, other than the config: the update-check state (`update.toml` in the cache directory, `crates/nuntio/src/update.rs`), the notes files and their todo syntax (`crates/nuntio-apps/src/notes/store.rs`), the log file location (`crates/nuntio/src/main.rs`), pasted-image files (`crates/nuntio/src/pasted_images.rs`).
- `interface`: changes something users, scripts or programs outside the workspace rely on: config keys, value syntax, defaults and validation (`crates/nuntio-config/src/schema.rs`, `docs/config.md`), the theme file format, action names and key-combo syntax (`crates/nuntio-config/src/keys.rs`), default shortcuts, CLI flags of `nuntio`, `nuntio-config` and `nuntio-apps`, environment variables nuntio sets in panes (`crates/nuntio-term/src/pane.rs`, `crates/nuntio/src/pane_env.rs`), what programs in a pane see (key and mouse encodings in `input.rs`, `kitty_keys.rs`, `mouse.rs`; escape sequences nuntio intercepts in `image_scan.rs` and `osc_cwd.rs`), package contents and install paths (`xtask/src/main.rs`, `assets/`), and `cargo xtask` commands and flags that CI, docs or `AGENTS.md` use.
- `behavior`: none of the above, but what a user or log reader observes changes: a pixel in a frame, UI text, a banner or log message, a default, a result in an edge case, idle CPU or redraw frequency, a fixed bug.
- `internal`: none of the above. The debug-server protocol (`crates/nuntio/src/debug_server.rs` and `xtask/src/drive.rs`) is internal as long as both sides change in the same commit.

## 5. Triage

- Merge the findings and drop duplicates across slices. Verify every finding by reading the cited locations yourself: scouts can be wrong.
- Before deciding, write a target picture per reviewed area: three to five sentences on how that area would be built today, knowing what nuntio has become (ownership, boundaries, the one representation of each core concept). Findings are judged against it: one that moves the area toward it is kept, one that only moves code around is dropped. The target picture goes into the run log (step 8).
- Drop findings that only rename for taste (a `naming` finding needs a name that now misleads), findings that would rewrite a working subsystem only for stylistic uniformity, findings that would break an invariant in `ARCHITECTURE.md` (main thread owns window, renderer and panes; short term-lock holds; idle CPU ~0; pure logic free of GPU and window types), findings that would fight a plan that is still current (step 3), and findings declined in an earlier run log without a changed reason (step 3).
- `internal` findings: carry out all of them (step 6). A `docs` finding about a stale section of a tracked document rewrites or deletes that section; nothing is archived in a second place.
- `behavior`, `wire`, `stored` and `interface` findings: never carry out without approval. Ask the user with one `ask` call (`multi: true`), one option per finding: title, what changes or breaks for users, and what else must change with it (for a setting: `schema.rs`, the `Config` type and `docs/config.md`; for an action: the places in "Where to change what"; for `stored`: how existing files are read after the change). Approved ones join step 6; the others go into the run log as declined (step 8).
- Findings that need a new external dependency or a new `deny.toml` exception are not carried out here: they go into the run log (step 8) as the user's decision.
- Findings that change `.github/workflows/release.yml` or `.github/workflows/pages.yml` are not carried out here, because only a tag push or a site deploy would exercise them: they go into the run log with a recommendation. `ci.yml` may change; CI checks it after the push.
- A new workspace crate is allowed when shared code is used by two or more crates and has no natural home among the existing ones. It takes the workspace `version`, `edition`, `license` and lints, joins `[workspace.dependencies]`, and gets a row in the crate tables of `ARCHITECTURE.md` (and its crate diagram), `AGENTS.md` and `CONTRIBUTING.md`. A new binary that nuntio starts in panes also joins `HELPERS` in `xtask/src/main.rs` and the package lists in `assets/`.

## 6. Implement (sequential)

- Order: `dead`, `comment` and `docs` first, then by size `small`, `medium`, `large`. Within that, dependencies first: a shared module lands before its callers move to it, a migration completes before code built on the winning side is consolidated.
- One finding, one commit.
- Implementation runs in the main agent or in one `task` subagent at a time, never in parallel: findings share files and the cargo target lock.

Per finding:

- Behavior stays identical. An approved compat finding changes only what was approved.
- No half states: a commit completes its migration. If a finding cannot be finished within its commit, it is dropped (last point), never landed half-way.
- Use `xd://lsp` references before moving or renaming an exported symbol, and `xd://ast_edit` for mechanical codemods.
- Clean cutover: every caller moves. No re-exports, aliases, deprecated wrappers or "moved to" comments remain.
- Tests move with their code; duplicated tests merge into one; tests that pin obsolete internals are deleted, not rewritten to pin the new ones. No new tests, unless the shared module now carries an invariant no existing test covers. Never weaken a behavior test to make a refactor pass.
- Update every document that names a moved path or symbol: `ARCHITECTURE.md`, `AGENTS.md`, `CONTRIBUTING.md`, `README.md`, `docs/config.md`, doc comments.
- Check: `cargo fmt --all`, then for each touched crate `cargo clippy -p <crate> --all-targets -- -D warnings` and `cargo test -p <crate>`. Touched `nuntio`: also `cargo clippy -p nuntio --all-targets --features debug-server -- -D warnings`. Touched `nuntio-render`: also `cargo clippy -p nuntio-render --all-targets --features capture -- -D warnings`. Moved or renamed a documented item: `RUSTDOCFLAGS="-D warnings" cargo doc -p <crate> --no-deps`.
- Windows: if the commit touches `crates/nuntio-wsl/`, `crates/nuntio/build.rs`, or a `.rs` file containing `cfg(windows)`, `cfg(not(windows))` or `target_os = "windows"`, and `cargo.exe` is on `PATH` (WSL with a Windows Rust toolchain), check the committed state on Windows: `win="$(wslpath "$(cmd.exe /c 'echo %LOCALAPPDATA%' 2>/dev/null | tr -d '\r')")/Temp/nuntio-consolidate"`, `mkdir -p "$win"`, `find "$win" -mindepth 1 -maxdepth 1 ! -name target -exec rm -rf {} +`, `git archive HEAD | tar -x -C "$win"`, then with `$win` as working directory `cargo.exe clippy --workspace --all-targets -- -D warnings` and `cargo.exe test --workspace`. On failure fix and `git commit --amend --no-edit`, or drop the finding (last point, with `git reset --hard HEAD~1`). Without `cargo.exe` the Windows build is only checked by CI after the push, without being waited for. Never use the user's own Windows checkout for this.
- macOS: code under `cfg(target_os = "macos")` cannot be compiled here (dependencies need the macOS SDK); CI checks it after the push, without being waited for. Note such commits under remaining risks in the run log.
- UI-visible code (`nuntio-render`, the overlays and window files of `nuntio`, `ui.rs` and `widgets.rs` of `nuntio-config-tui` and `nuntio-apps`): compare screenshots with `cargo xtask drive` (headless, as in `AGENTS.md`). Before editing, on the parent commit: `cargo xtask drive start --size 1280x800 --config /tmp/nuntio-consolidate/config.toml`, `cargo xtask drive wait-idle`, open the affected surface (`action new_tab`, `action split_vertical`, `action search`, `action open_actions_menu`, `action open_settings`, `action open_notes`, `type`, `key`, `mouse`), `wait-idle`, `shot`, copy the screenshot (path in `AGENTS.md`) to `/tmp/nuntio-consolidate/shots/<finding #>-<surface>-before.png`, `cargo xtask drive stop`. After editing, the same commands into `-after.png`. Look at both with `read`: they must look the same. `/tmp/nuntio-consolidate/config.toml` contains only `[notes]` with `directory = "/tmp/nuntio-consolidate/notes"`, so the notes app never touches the user's notes.
- `xtask` changes: `cargo test -p xtask` and run the changed subcommand before and after with the same input (for `changelog`: `cargo xtask changelog <previous tag>..<last tag>`, compare the output; for `package`: `cargo xtask package`, compare the file lists in `dist/`; for `site`: `cargo xtask site`, and if `zola` is not on `PATH`, revert the change and record the finding as dropped, "not verifiable without zola").
- Commit per `CONTRIBUTING.md`: imperative subject of at most 72 characters without prefix, a blank line, and a body that explains why (which duplicates or conventions were unified). Stage paths explicitly, never with `git add -A` or `git add .`. Internal refactors carry no `Changelog` trailer. Approved compat changes carry `Changelog: changed`, or `Changelog: removed` when a setting, action or flag goes away, and their subject is written as a release note users understand.
- If a finding turns out wrong or not behavior-preserving while implementing it, go back to the last commit (`git reset --hard HEAD`), record the finding as dropped with the reason, and continue with the next one.

## 7. Land

- Run the full gate:
  - `cargo fmt --all --check`
  - `cargo clippy --workspace --all-targets -- -D warnings` and `cargo clippy --workspace --all-targets --features nuntio/debug-server -- -D warnings`
  - `cargo test --workspace` and `cargo test --workspace --features nuntio/debug-server`
  - `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps`
  - MSRV: `msrv=$(sed -n 's/^rust-version = "\(.*\)"/\1/p' Cargo.toml)`, then `cargo +$msrv check --workspace --all-targets --locked` and the same with `--features nuntio/debug-server`; install a missing toolchain with `rustup toolchain install $msrv --profile minimal`.
  - `cargo deny --all-features check`
  - With `cargo.exe` on `PATH`: the Windows check from step 6 on `HEAD`, with the clippy run also repeated with `--features nuntio/debug-server`.
- `git pull --rebase origin main`. If that brought new commits, run the full gate again.
- Gate not green and not fixable within the consolidation commits: ask the user before going on.
- Write the run log (step 8) now, after the rebase, so it names the final commit hashes, and commit it on its own: subject `Log the consolidation run of <YYYY-MM-DD>`, no `Changelog` trailer. It is documentation only and needs no second gate.
- Before pushing, apply the red-`main` rule of `AGENTS.md` (one look at the newest run, never wait). `git push origin HEAD:main`. If the push is rejected because `main` moved, rebase again, run the full gate again, correct the hashes in the log (`git commit --amend`) and push.
- Do not wait for CI: the user follows the run of the pushed commit.
- Afterwards remove the worktree (`git worktree remove`) if one was used and delete the local branch (`git branch -D`). If the run worked in `<repo>` itself, switch it back to `main` and `git pull --ff-only origin main` first. Remove `/tmp/nuntio-consolidate` and the Windows check directory.

## 8. Run log and report

The run log is the lasting record of a run; the next run reads it in step 3. File: `docs/consolidations/<YYYY-MM-DD>.md` (append `-2`, `-3` and so on if it exists; create the directory if needed), in English like the rest of the repository's documentation, with:

- Title, date, scope (whole repository or the argument), the `origin/main` commit the review started from.
- Target picture per reviewed area (step 5).
- Overview table `# | Finding | Kind | Compat | Result`, where `Result` is one of ✅ done (with commit hash), ❌ dropped, 🚫 declined, ⏸ open.
- Carried out: per commit, hash, subject and in one or two sentences what was unified and where it lives now.
- Dropped findings, each with its reason (wrong, not behavior-preserving, not finishable in one commit, not verifiable locally, taste, stylistic rewrite, breaks an invariant, fights a plan).
- Declined compat findings, with what they would break, so the next run does not ask again without a new reason.
- Not carried out here: new dependencies, `deny.toml` exceptions and `release.yml`/`pages.yml` changes, each with the next step.
- Greenfield observations too large or too open to act on, for example a different crate split, each with a one-line recommendation.
- Remaining debt and risks: what the run left deliberately, and where a consolidation commit carries a risk the checks may not have covered (macOS-only code, platform paths CI does not exercise, UI states no screenshot showed).
- Starting points for the next development phase: where the consolidated structure now makes the next feature cheap.

The run log is public. It never quotes the local planning files (`SPEC.md`, `TODO-engines.md`, `docs/proposals/`); it may name a plan by its title as the reason a finding was dropped. Security observations never go into the run log or any other file in the repository: an unfixed weakness does not belong in a public document. They go only into the chat report.

A run with no finding carried out still writes a log, so that dropped and declined findings are recorded.

Report to the user in the chat: path of the run log, the target picture in brief, the pushed commits (hash and subject) grouped by `kind`, the open points, remaining risks and starting points briefly, local plans the code has already implemented or left (step 3), and the security observations in their own section (never fixed here).
