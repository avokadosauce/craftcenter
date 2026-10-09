# AGENTS.md: guide for contributors and AI agents

CraftCenter is an installer and updater for the [Crafting Apps](https://getartcraft.com/apps),
written in **Rust only**. No Tauri, Electron or webview shell: the desktop app is native
egui/eframe on wgpu, and the command-line front end is the same core without a window. The product
name is written **CraftCenter** in user-facing text; machine names stay lowercase (`craftcenter`,
the crates `craftcenter-*`, the id `io.github.avokadosauce.craftcenter`).

Read this file first, then `docs/design.md`, which records what was measured about how the
upstream builds and ships and why this program is shaped the way it is.

## 1. What this program is, and is not

It downloads each app's **official release assets** from GitHub, verifies them against that
release's `SHA256SUMS.txt`, and installs them for the current user. It is **unofficial**: not made,
sponsored or endorsed by the ArtCraft team.

- It **never vendors or rebuilds their code.** The only upstream material in this repository is
  each app's own icon, which its repository licenses MIT OR Apache-2.0 (see `ATTRIBUTION.md`).
- It **never uses the ArtCraft name, wordmark or mark.** Those are trademarks and are not open
  source. Do not add them, and do not name anything after them.
- It **never elevates.** Every install is per-user. The upstream `.msi` is `Scope="perMachine"`
  and always asks for administrator rights, so this program does not use it.
- It **has no telemetry** of any kind, and **asks for no credential.** The update check costs no
  API rate limit, so there is nothing a token would buy.
- It **talks only to GitHub.** `crates/releases/src/http.rs` holds the allow-list; a request to
  any other host is refused there, and there is no setting that widens it.

## 2. Workspace map

```text
crates/
  catalogue   L0  catalogue/apps.toml: parsing and validation
  verify      L0  SHA256SUMS.txt parsing, streaming SHA-256, the per-file manifest of a tree
  select      L1  which release asset to install, per platform
  releases    L2  release discovery over github.com, the REST fallback, the on-disk cache
  install     L3  install / launch / remove per platform, the state file, the atomic swap
  core        L4  the facade both front ends drive
  ui-egui     L5  the egui shell: theme tokens, widgets, the catalogue window
apps/
  craftcenter       the desktop app (eframe + wgpu); owns the event loop
  craftcenter-cli   the headless front end
xtask/              cargo xtask layers | catalogue
packaging/          the scripts that build the release assets
```

**Layering is enforced** by `cargo xtask layers`: a crate may depend only on crates of a strictly
lower layer, and nothing below `ui-egui` may depend on egui, eframe, winit or wgpu. A new crate
gets a row in `xtask/src/main.rs`. `ui-egui` does not depend on eframe either — it draws into a
`Ui` and the binary owns the windowing — so the shell stays testable without a display.

## 3. Golden rules

### Never crash

A person is trusting this program to replace software on their machine. A malformed manifest, a
truncated download, a hostile archive entry, a full disk or an unreadable settings file must
produce an error they can act on, never a panic.

- **Non-test code never panics.** No `unwrap()`, `expect()`, `panic!`, `unreachable!`, `todo!` or
  indexing with `[i]`. Return the crate's error type and propagate with `?`. Every crate carries
  `#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, …, clippy::indexing_slicing)]`;
  `clippy.toml` allows those in tests only.
- **No `unsafe`.** The workspace sets `unsafe_code = "forbid"`. There is no exception.
- **Input from the network is hostile.** Asset names, archive entry names, redirect targets and
  manifest lines are all attacker-controlled. Entry names go through `fs::safe_join`, which refuses
  anything that would escape the destination; hosts go through the allow-list; bodies are read with
  a ceiling.
- **Prove it.** A crash fix comes with the small test that reproduced it.

### Verification is not optional

Nothing is installed that has not been hashed and matched against the release's manifest. An asset
the manifest does not list is refused. A mismatched download is deleted, not kept. There is no
setting to turn this off, and adding one would need a very good reason.

An install also records a manifest of its own — the SHA-256 and size of every file it wrote,
relative to the install directory — and `verify` checks the tree against that. It has to: the
release's digest is the digest of an *archive* for three of the four formats, so re-hashing an
installed file against it can only ever match for an AppImage. Anything that writes files into an
install writes the manifest with them, and a format added later must do the same. A self-update
records the program it put in place by the same rule — the file the swap landed on, not the archive
it came out of and not the `.app` directory around it.

Be honest about what it proves: the manifest is unsigned in every upstream repository, so a
verified digest shows the bytes are what that release published — not who published it. The
per-file manifest is CraftCenter's own, in the user's own directory, so it shows that a file has
not changed since it was installed and nothing more — not who installed it. Where the platform
offers a real identity check (a notarised DMG, an Authenticode signature), use it as well.

### Nothing is written over something that might be running

A new version goes into its own versioned directory and a pointer is flipped with an atomic
`rename`. The replaced version survives until the new one has been launched once.

### The catalogue is data

Adding a Crafting App is one `[[app]]` row in `catalogue/apps.toml` and nothing else. If a change
would need code to add an app, it is in the wrong place. The upstream drifts — an asset stem can
change between releases, a platform variant can appear or vanish, an app can go from no release to
a release — so selection ranks a release's **real asset list** and never builds a filename from a
template.

### The shell is thin

Every action is a method on `craftcenter_core::Center`, which both front ends call. The window
draws state and dispatches; it decides nothing the CLI would decide differently. Colours and radii
come from `theme::Tokens`, never hard-coded.

## 4. Before you finish a task

```sh
cargo fmt --all -- --check
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo xtask layers
cargo xtask catalogue --check-offline
cargo xtask catalogue --check        # needs the network; run it when you touch the catalogue
```

The last one reports drift against the live releases. It is deliberately **not** in CI: an upstream
project shipping a release must not turn this repository red.

## 5. Tests

CI runs with no network. Release manifests recorded from the real site live in
`crates/releases/fixtures/<app>/`, and the HTTP layer is a trait so every parsing and selection
path is exercised against them. When you add a behaviour that depends on what a release looks like,
add the fixture rather than mocking a shape you imagine.

What a test suite cannot reach here is the window and the two platforms this was not written on.
`README.md` says which steps have never been run by a person; keep that list honest.
