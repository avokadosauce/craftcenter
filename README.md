<p align="center">
  <img alt="" src="assets/app-icon/craftcenter-64.png" width="64" height="64">
</p>

<h1 align="center">CraftCenter</h1>

<p align="center">
  <b>One place to install and update the Crafting Apps.</b><br>
  Downloads each app's official release from GitHub, checks it against that release's<br>
  <code>SHA256SUMS.txt</code>, and installs it for you — no administrator password, no telemetry.
</p>

<p align="center">
  <img alt="100% Rust" src="https://img.shields.io/badge/100%25-Rust-b7410e?style=flat-square&logo=rust">
  <img alt="Linux, macOS and Windows" src="https://img.shields.io/badge/Linux%20%C2%B7%20macOS%20%C2%B7%20Windows-native-2f7bf5?style=flat-square">
  <img alt="License: PolyForm Noncommercial 1.0.0" src="https://img.shields.io/badge/license-PolyForm%20Noncommercial-3a3a3a?style=flat-square">
  <img alt="Status: early" src="https://img.shields.io/badge/status-early-d69e2e?style=flat-square">
</p>

<p align="center">
  <a href="#what-it-does">What it does</a> ·
  <a href="#the-apps">The apps</a> ·
  <a href="#install">Install</a> ·
  <a href="#the-command-line">Command line</a> ·
  <a href="#how-it-checks-for-updates">How updates work</a> ·
  <a href="#what-has-and-has-not-been-run-by-a-person">What has been run</a> ·
  <a href="#building">Building</a>
</p>

> [!NOTE]
> **CraftCenter is unofficial.** It is not made, sponsored, endorsed or certified by the ArtCraft
> team. It contains none of their code: it downloads the releases they publish, verifies them, and
> installs them. The ArtCraft name, wordmark and mark are their trademarks and are not used here.

---

## What it does

The [Crafting Apps](https://getartcraft.com/apps) each publish their own GitHub releases, and none
of them checks whether a newer version exists. CraftCenter is the piece that was missing:

- **A catalogue** of every crafting app, with its tagline, the version you have, and the version
  that is out.
- **Install, Update, Launch and Remove** per app, and **Update all**.
- **Checksum verification, always.** Every download is hashed and compared with the release's own
  `SHA256SUMS.txt` before anything is installed. An asset the manifest does not list is refused; a
  download that does not match is deleted.
- **Per-user installs.** Nothing is written outside your own directories and you are never asked
  for an administrator password.
- **No telemetry, no account, no token.** The only hosts it contacts are `github.com` and
  `api.github.com`.
- **A command line** with the same core, for machines without a display.

## The apps

| App | What it is for |
|---|---|
| **PhotoCraft** | Image editing: layers, masks, type and real PSD files |
| **VectorCraft** | Vector illustration |
| **FilmCraft** | Video editing, color and sound |
| **LightCraft** | Photo library and raw development |
| **PdfCraft** | Reading, organizing and protecting PDFs |
| **EffectCraft** | Motion graphics and visual effects |
| **DesignCraft** | Page layout and publishing |
| **WordCraft** | Word processing and long documents |
| **CADCraft** | Computer-aided design and drafting |
| **GridCraft** | Spreadsheets and calculation |
| **DeckCraft** | Presentations and slide shows |
| **SoundCraft** | Audio production and mixing |

Adding one is a single row in [`catalogue/apps.toml`](catalogue/apps.toml) — no code change.

`artcraft`, the engine, is deliberately not in the catalogue: it is not one of the creative-suite
apps, its releases have a different shape, and it publishes no checksum manifest, so a download of
it could not be verified.

## Install

CraftCenter has not cut its own release yet. Until it does, build it (see [Building](#building))
and run `target/release/craftcenter`.

Where things go, once it has:

| | Apps | Launchers | State |
|---|---|---|---|
| **Linux** | `~/.local/share/craftcenter/apps/` | `~/.local/bin/` | `~/.local/state/craftcenter/` |
| **macOS** | `~/Applications/` | `~/.local/bin/` | `~/Library/Application Support/CraftCenter/` |
| **Windows** | `%LOCALAPPDATA%\Programs\CraftCenter\` | the same folder | `%APPDATA%\CraftCenter\` |

Set `CRAFTCENTER_ROOT=/some/directory` to put everything under one directory instead — useful for
trying it out, and what the tests use.

On Linux, make sure `~/.local/bin` is on your `PATH`; the desktop entry CraftCenter writes uses an
absolute path and does not depend on it.

## The command line

```console
$ craftcenter-cli list
APP           WHAT IT IS FOR                     INSTALLED  LATEST     STATUS
photocraft    Image editing: layers, masks, typ…  -          0.5.0      not installed
gridcraft     Spreadsheets and calculation        0.3.0      0.3.0      up to date

$ craftcenter-cli install gridcraft
installed gridcraft 0.3.0 from gridcraft-0.3.0-linux-x86_64.AppImage
run it with: /home/someone/.local/bin/gridcraft

$ craftcenter-cli verify gridcraft
gridcraft: matches the digest recorded at install time
```

`list`, `check`, `install`, `update`, `launch`, `remove`, `verify`, `self-update`, `paths`.
`--json` for `list`, `--force` to ignore the cached check, `--platform windows-arm64` to ask what
*would* be installed somewhere else.

## How it checks for updates

The obvious design is one `api.github.com/repos/<repo>/releases/latest` call per app, with ETags to
stay inside GitHub's unauthenticated budget of 60 requests an hour. Measured against the live API,
that does not work the way people assume: **a conditional request answered `304 Not Modified` still
spends a unit of the budget.** ETags save bandwidth, not quota — and that budget is shared by
everyone behind your IP address.

So CraftCenter takes a different route. `github.com` answers a request for
`/<repo>/releases/latest/download/SHA256SUMS.txt` with a `302` whose target names the release, and
that file lists every asset of the release with its SHA-256. Two plain `github.com` requests
therefore yield the version, the complete asset list **and** every digest, and cost **nothing** of
the API's rate limit. That is why there is no setting for an access token: there is nothing one
would buy. The REST API is kept only as a fallback for a repository that publishes no manifest.

Asset selection ranks the release's *real* asset list rather than building a filename from a
template, because the published reality drifts from the pattern: an asset stem can change between
releases (every `pdfcraft` release up to v0.2.1 was published as `printcraft-*`), platform variants
come and go, and an app can have no release at all. The full reasoning, with the measurements, is
in [`docs/design.md`](docs/design.md).

## What has and has not been run by a person

Stated plainly, because an installer that overclaims is worse than one that admits its gaps.

**Run, against the live upstream, on Linux x86_64:** the catalogue and release check for all twelve
apps; downloading, verifying, installing, updating, verifying again and removing a real app; the
desktop entry and icon; the launcher symlink; the `--json` output; the error paths for an unknown
app and an unknown command.

**Compiled and unit-tested on all three platforms in CI, but never run by a person:**

- the window itself, on any platform — including the app-drawn title bar, its drag and
  double-click-to-maximize behaviour, and the caption buttons. If it misbehaves, set
  `CRAFTCENTER_OS_DECORATIONS=1` to get the system's own decorations back;
- the macOS install path (mount the DMG, copy the bundle into `~/Applications`, detach);
- the Windows install path (unpack the portable zip, write a Start Menu shortcut);
- replacing CraftCenter with a newer build of itself, on any platform. The file swap itself is
  covered by tests; doing it to a genuinely running program is not.

## Building

```sh
cargo build --release          # or: cargo run -p craftcenter-cli -- list
```

Rust 1.90 or newer. On Linux the window needs the usual egui/winit development packages:
`libxkbcommon-dev libwayland-dev libx11-dev libxrandr-dev libxi-dev libgl1-mesa-dev` (or your
distribution's equivalents). The command-line binary needs none of them.

Before sending a change:

```sh
cargo fmt --all -- --check
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo xtask layers
cargo xtask catalogue --check-offline
```

[`AGENTS.md`](AGENTS.md) is the guide for contributors and agents: the rules this code holds itself
to, and why.

## License

[PolyForm Noncommercial License 1.0.0](LICENSE.md). Any noncommercial purpose is permitted —
personal use, hobby projects, study, charities, schools, public bodies. Selling the software, or
the code it is built from, is not. The software comes as is, without any warranty or condition, and
the authors are not liable for anything arising from it.

Third-party material, with its source and licence, is listed in
[`ATTRIBUTION.md`](ATTRIBUTION.md): the app icons (each app's own artwork, under the MIT or
Apache-2.0 licence its repository grants), and Inter and JetBrains Mono (SIL OFL 1.1).
