# CraftCenter — design

> **Status: proposal.** This document records what was measured about how the Crafting Apps are
> built and shipped, and the design that follows from it. Nothing here is implemented yet.
> Every claim below was checked against the upstream repositories and their published releases
> on 2026-10-08; the file path or URL is given so each can be rechecked.

CraftCenter is an **unaffiliated, third-party** installer and updater for the
[Crafting Apps](https://getartcraft.com/apps) — the clean-room Rust creative tools published by the
ArtCraft team at [github.com/storytold](https://github.com/storytold). It downloads the publishers'
own official release assets from GitHub, verifies them against the `SHA256SUMS.txt` those releases
publish, installs them for the current user, and tells you when a new version is out.

It redistributes nothing. It vendors none of their code. It is not made, sponsored or endorsed by
the ArtCraft team.

---

## 1. What the upstream does today (measured)

### 1.1 One repository shape, twelve times

Every crafting app is a Cargo workspace with the same skeleton. Checked in full on
[`storytold/photocraft`](https://github.com/storytold/photocraft) and spot-checked on `filmcraft`,
`gridcraft`, `deckcraft` and `pdfcraft`:

```
Cargo.toml          [workspace] resolver = "3", members = ["crates/*", "apps/*", "xtask"]
                    [workspace.package] edition = "2024", license = "MIT OR Apache-2.0",
                                        rust-version = "1.90".."1.95"
                    [workspace.lints.rust] unsafe_code = "forbid"
rustfmt.toml        max_width = 160, use_small_heuristics = "Max"
clippy.toml         allow-{unwrap,expect,panic,indexing-slicing}-in-tests = true
.cargo/config.toml  [alias] xtask = "run -p xtask --"
AGENTS.md CLAUDE.md ATTRIBUTION.md NOTICE README.md ROADMAP.md SECURITY.md
LICENSE-MIT LICENSE-APACHE
apps/<app>  apps/<app>-cli  apps/<app>-web
crates/*    xtask/    packaging/    assets/{app-icon,fonts,icons}
.github/workflows/  ci.yml release.yml packaging-lint.yml freebsd.yml windows-arm64.yml
```

The UI is **egui + eframe `0.36` on wgpu**, pinned identically across every app
(`photocraft/Cargo.toml`, `filmcraft/Cargo.toml`, `gridcraft/Cargo.toml`, `deckcraft/Cargo.toml`).
`AGENTS.md` forbids Tauri, Electron and any webview or JS UI framework. `unsafe` is forbidden
workspace-wide, with one isolated, audited exception per repo where platform interop demands it.

CI (`.github/workflows/ci.yml`) is `cargo fmt --all -- --check`,
`cargo test --workspace --locked`, `cargo clippy --workspace --all-targets --locked -- -D warnings`,
plus repo-specific `xtask` gates, across an `ubuntu-latest`/`macos-latest`/`windows-latest` matrix
that is narrowed to Linux on PRs which touch no platform-sensitive path.

### 1.2 How a release is cut

A push to the `release` branch builds every platform and creates or updates a **draft** GitHub
Release titled `<AppName> v<version>`, tagged `v<version>`; a human publishes the draft
(`photocraft/docs/releasing.md`, `photocraft/.github/workflows/release.yml`). The version lives in
exactly one place, `[workspace.package] version`.

Signing, per `release.yml` and `packaging/`:

| Platform | What ships | Signed? |
| --- | --- | --- |
| macOS | `.app` on a drag-to-`/Applications` DMG, plus a CLI zip | Developer ID, hardened runtime, **notarised and stapled**; `packaging/macos/verify.sh` fails the build if Gatekeeper disagrees |
| Windows | per-machine `.msi` (WiX 5, `Scope="perMachine"`) and a portable `.zip` | Authenticode, via a PFX or Azure Trusted Signing |
| Linux | AppImage, deb, rpm, tar.gz, (sometimes) flatpak | **unsigned** — no GPG, no minisign, no cosign |
| FreeBSD | tar.gz | unsigned |

Every signing secret is optional: a missing one yields an unsigned artifact and a warning, never a
failed build.

**There is no machine-readable release manifest.** The only metadata asset is `SHA256SUMS.txt`,
produced by `sha256sum -- *` over the collected artifacts, so it is plain
`<64 hex>␠␠<bare filename>` lines — one per asset in that release. **It is not itself signed**, in
any of the twelve repositories.

### 1.3 Asset naming, and where it drifts

The pattern is `<stem>-<version>-<os>-<arch>.<ext>`, with `-cli` and `-web` variants:

```
photocraft-0.3.0-linux-x86_64.AppImage        photocraft-0.3.0-linux-x86_64.AppImage.zsync
photocraft-0.3.0-linux-aarch64.{deb,rpm,tar.gz,flatpak}
photocraft-0.3.0-macos-universal.dmg          photocraft-cli-0.3.0-macos-universal.zip
photocraft-0.3.0-windows-{x64,x86,arm64}.msi  photocraft-0.3.0-windows-x64-portable.zip
photocraft-0.3.0-freebsd-x86_64.tar.gz        photocraft-web-0.3.0.zip
SHA256SUMS.txt
```

What is actually present varies per app. Latest release of each, read from the GitHub API on
2026-10-08:

| App | Latest | AppImage · deb · rpm · tar.gz · dmg · msi+zip (x64, x86) | win arm64 | flatpak | `.zsync` | FreeBSD |
| --- | --- | --- | --- | --- | --- | --- |
| photocraft | v0.3.0 | ✓ | ✓ | ✓ | ✓ | ✓ |
| vectorcraft | v0.5.0 | ✓ | ✓ | — | — | ✓ |
| effectcraft | v0.4.0 | ✓ | ✓ | — | — | — |
| filmcraft | v0.2.1 | ✓ | — | — | — | — |
| lightcraft | v0.2.1 | ✓ | — | — | — | — |
| designcraft | v0.2.1 | ✓ | — | — | — | — |
| pdfcraft | v0.2.1 | ✓ | — | — | — | ✓ |
| wordcraft | v0.1.0 | ✓ | ✓ | — | — | ✓ |
| cadcraft | v0.1.0 | ✓ | ✓ | — | — | ✓ |
| gridcraft | v0.1.0 | ✓ | ✓ | — | — | ✓ |
| deckcraft | v0.1.0 | ✓ | — | ✓ (x86_64) | — | ✓ |
| soundcraft | — | **no releases at all** | | | | |

Three pieces of drift matter to an installer:

1. **The asset stem is not the repository name.** `storytold/pdfcraft` has published four releases,
   all titled *PrintCraft* with assets named `printcraft-0.2.1-…`, while `main`'s packaging scripts
   now emit `pdfcraft-…` (`pdfcraft/packaging/linux/package.sh`: `BASENAME="pdfcraft-$VERSION-…"`).
   The rename is mid-flight, so the next release changes the filenames.
2. **Variants come and go.** Windows ARM64 exists for six apps and not for five. FreeBSD for seven.
   Flatpak for two. `.zsync` for one.
3. **An app can have no release.** `soundcraft` has none today.

So asset selection must be a **ranked match against the release's real asset list**, never a
filename built from a template. That is the single most important consequence of this research.

### 1.4 Their visual and interaction language

From `photocraft/docs/ui-design.md` and `photocraft/crates/ui-egui/src/theme.rs`:

- **Five themes**, named and switchable at runtime. `ProMedium` is the shipped default
  (`#[derive(Default)]` on `ThemeKind::ProMedium`), while `docs/ui-design.md` still calls `Pro` the
  default — a documentation lag worth knowing about.

  | Theme | id | Character |
  | --- | --- | --- |
  | Pro (Dark) | `pro` | Spectrum-dark charcoal: chrome `#323232`, dock `#1e1e1e`, canvas `#282828`, accent `#378ef0`, radii 3/4/6 |
  | Pro (Medium Gray) | `proMedium` | *default* — chrome `#535353`, dock `#424242`, same grammar |
  | Studio (Dark) | `studio` | near-black `#141415`, rounded cards, violet accent `#8b7cf6`, radii 6/8/12 |
  | Studio (Light) | `studioLight` | the same system on light surfaces |
  | Classic | `classic` | Windows-2000 bevels, square corners, navy selection |

- **Tokens, not literals.** A `Tokens` struct carries ~30 named colours (`chrome`, `dock`, `card`,
  `field`, `hover`, `text_dim`, `accent_soft`, `separator`, `row_selected`, `caption_close`, …),
  three radii and two shape flags (`bevel`, `pro`). Widgets read `Tokens::get(ctx)`; the house rule
  is that no widget ever hard-codes a colour.
- **Typography.** Inter (Regular/Medium/SemiBold) for UI, JetBrains Mono for numbers, both SIL
  OFL-1.1 and both committed to each repo under `assets/fonts/` with their licence texts. Body text
  is 12.0 px in the Pro themes and 12.5 px otherwise; headings are SemiBold 15 px; `Small` is
  10.5 px.
- **Icons.** Lucide SVGs under `assets/icons/` (ISC), tinted at runtime from a generated
  `include_bytes!` table.
- **Window chrome.** On Windows and Linux the window has **no OS decorations**: the app's own top
  bar is the title bar, with Minimize / Maximize / Close flush in the top-right (`caption_close`
  turns Windows red on hover), a draggable gap, and invisible 5 pt resize edges. On macOS the
  traffic lights sit over an integrated title strip.
- **Shared widget set** in `widgets.rs`: `card`, `value_field`, `slider_row`, `toggle`, `checkbox`,
  `primary_button`, `secondary_button`, `dropdown`, `hairline`.
- **Preferences** has Apply / OK / Cancel, with Apply disabled while the draft matches what is
  saved.
- **About** shows contributors and the AI models that helped, compiled into the binary at build
  time (`crates/ui-egui/src/credits.rs`).
- Ten UI languages, as one TSV catalogue per language.

### 1.5 What the apps do *not* do

- **No app checks for its own updates.** A code search over `storytold/photocraft` for
  `api.github.com/repos` returns nothing.
- **No app knows about its siblings.** `vectorcraft` appears in `photocraft` only in `README.md`
  and `AGENTS.md` — prose, not code.
- The one update mechanism that exists is external: the Linux AppImage embeds AppImage update
  information, `gh-releases-zsync|storytold|photocraft|latest|photocraft-*-linux-<arch>.AppImage.zsync`
  (`packaging/linux/package.sh`), so `AppImageUpdate` can delta-update it. Only `photocraft`
  publishes the matching `.zsync`.

That gap is what CraftCenter fills.

### 1.6 A useful accident: the AppImage integrates itself

`packaging/linux/AppRun` is a shell script, not the usual symlink. On first run — and whenever the
AppImage's path changes — it copies the app's `.desktop` entry and hicolor icons into
`$XDG_DATA_HOME`, rewriting `Exec=` to the AppImage's absolute path and dropping `TryExec=`, so
Wayland compositors can resolve the window's `app_id` to a real icon. It compares before writing,
and any failure leaves the launch untouched.

Consequence: an installer that drops an AppImage somewhere stable and marks it executable gets
launcher integration for free. If CraftCenter writes the same `ai.storyteller.<app>.desktop` file
with the same `Exec=`, `AppRun`'s own `cmp -s` finds it identical and leaves it alone.

### 1.7 Licensing, and the one hard boundary

App code and original app assets are **MIT OR Apache-2.0**. Each app's own icon is the app owner's
original work under that same dual licence — verified in `photocraft/ATTRIBUTION.md`
(`assets/app-icon/` → "MIT OR Apache-2.0") and `filmcraft/ATTRIBUTION.md`.

The exception is `docs/brand/`: the **ArtCraft name, wordmark and mark are trademarks and are not
open source** (`photocraft/docs/brand/LICENSE-brand.txt`). The permission granted there covers use
"as part of copies of this repository" and "within PhotoCraft itself"; it states that neither the
marks nor the ArtCraft name may appear "in the name, logo, icon, domain, social media handle,
app-store listing, advertising or marketing of anything other than PhotoCraft".

CraftCenter therefore:

- **never** ships or displays the ArtCraft wordmark or mark, and is not named after ArtCraft;
- names the apps descriptively, as any package manager does;
- carries a visible "unofficial, not affiliated with or endorsed by the ArtCraft team" statement;
- ships each app's own permissively licensed icon with an `ATTRIBUTION.md` row per file.

`storytold/artcraft`, the engine, is separate again: a non-OSI "fair source" licence
(`LICENSE.md`) that permits private use and forbids commercial resale or building a competing
product. Downloading a publisher's own official build for a user implicates none of that — but see
D3 for why it is still a special case.

---

## 2. Platform realities for a user-space installer

### 2.1 Where things can go without asking for a password

| | Default, no elevation | Why |
| --- | --- | --- |
| **Linux** | AppImage (or the relocated `tar.gz` tree) under `~/.local/share/craftcenter/apps/<slug>/<version>/`; a symlink in `~/.local/bin`; `.desktop` + icons in `~/.local/share` | `deb`/`rpm` write `/usr/bin` and need root; `flatpak` needs flatpak. The `tar.gz` is a plain FHS tree (`bin/`, `share/applications`, `share/icons/hicolor`, `share/metainfo`, `share/mime/packages`) under one top-level directory, so it relocates into `~/.local` unchanged |
| **macOS** | mount the DMG read-only (`hdiutil attach -nobrowse`), copy `<App>.app` into `~/Applications`, detach | `/Applications` needs an admin prompt. The app is notarised and stapled, and a file written by our own HTTP client carries no `com.apple.quarantine` attribute, so there is no Gatekeeper prompt and nothing to strip |
| **Windows** | extract the **portable** `.zip` into `%LOCALAPPDATA%\Programs\CraftCenter\<slug>\<version>\`, with a Start-Menu `.lnk` | their `.msi` is `Scope="perMachine"` (`packaging/windows/photocraft.wxs`) and always elevates. Their portable zip additionally carries a `portable.txt` marker that keeps the app's own settings beside the binary |

### 2.2 Update checks without spending GitHub's rate limit

The obvious design — `GET https://api.github.com/repos/<owner>/<repo>/releases/latest` per app — is
the wrong one. Measured on 2026-10-08, unauthenticated:

```
GET  api.github.com/repos/storytold/gridcraft/releases/latest   200   x-ratelimit-used: 1  (limit 60)
GET  same, with If-None-Match: <etag>                           304   x-ratelimit-used: 2
GET  same, unconditional                                        200   x-ratelimit-used: 3
```

**A conditional request that returns 304 still costs a unit of the unauthenticated 60/hour budget**,
which is shared per source IP. ETags save bandwidth here; they do not save quota.

There is a better path that costs no API quota at all. GitHub serves
`https://github.com/<owner>/<repo>/releases/latest/download/<asset>` as a 302 to the concrete
versioned URL:

```
GET github.com/storytold/photocraft/releases/latest/download/SHA256SUMS.txt
 → 302  location: .../releases/download/v0.3.0/SHA256SUMS.txt
GET github.com/storytold/gridcraft/releases/latest/download/SHA256SUMS.txt
 → 302  location: .../releases/download/v0.1.0/SHA256SUMS.txt
GET github.com/storytold/soundcraft/releases/latest/download/SHA256SUMS.txt
 → 404  (no release)
after all of the above:  api.github.com rate limit x-ratelimit-used unchanged
```

One request per app therefore yields **all three things an installer needs**: the latest version
(from the redirect target), the complete asset list for that release, and the SHA-256 of every
asset. Download URLs are then built from the resolved tag and need no API call either.

So:

- **Primary check:** one `GET` of `releases/latest/download/SHA256SUMS.txt` per app. No API quota.
- **Fallback:** `api.github.com/repos/<repo>/releases/latest` when a repo publishes no sums file, or
  when the probe fails. This costs quota, so it is cached and rate-limit-aware: on `403`/`429` the
  UI says "rate limited until &lt;x-ratelimit-reset&gt;" instead of showing an error per row.
- **404 is a state, not a failure:** "no release yet" renders as a greyed row.
- Checks are scheduled (default: daily) and cached on disk. Never per frame, never per repaint.
- An optional personal access token in settings raises the fallback budget to 5000/hour. Off by
  default; most users will never need it, because the primary path spends nothing.

### 2.3 Verification, honestly described

`SHA256SUMS.txt` is fetched over TLS from `github.com` and every downloaded asset is stream-hashed
and compared before anything is installed. That is mandatory and not configurable.

What it does *not* give you: the sums file is **unsigned** in all twelve repositories, so the trust
root is TLS plus GitHub's control of the release, not a publisher key. The hash protects against a
truncated or corrupted download and against fetching the wrong asset from the right release; it is
not a substitute for a signature over the manifest.

Where the platform offers a real publisher-identity check, CraftCenter uses it after install:
`codesign --verify --strict` plus `spctl --assess` on macOS, and the Authenticode signature of the
extracted `.exe` on Windows. On Linux there is nothing to check — their Linux artifacts are
unsigned.

### 2.4 Replacing a version atomically

Install into a *new* versioned directory, then flip a pointer:

- **Linux / macOS:** create the new symlink under a temporary name and `rename(2)` it over the old
  one — atomic on POSIX. The previous version stays on disk until the new one has launched once,
  then it is pruned (`keep_previous = 1`).
- **Windows:** a running `.exe` cannot be deleted, but its directory *can* be renamed. Stage, flip
  the shortcut, rename the old directory aside, and prune it on the next start.

Nothing ever writes over a binary that might be running.

---

## 3. The design

### 3.1 Repository shape

Indistinguishable in shape from one of theirs, because the conventions are theirs:

```
Cargo.toml              [workspace] resolver = "3", members = ["crates/*", "apps/*", "xtask"]
                        edition 2024 · license "MIT OR Apache-2.0" · unsafe_code = "forbid"
rustfmt.toml            max_width = 160, use_small_heuristics = "Max"
clippy.toml             allow-{unwrap,expect,panic,indexing-slicing}-in-tests
.cargo/config.toml      [alias] xtask = "run -p xtask --"
AGENTS.md CLAUDE.md ATTRIBUTION.md NOTICE README.md ROADMAP.md SECURITY.md
LICENSE-MIT LICENSE-APACHE
catalogue/apps.toml     the data file — one row per app
crates/
  catalogue   L0  parse and validate catalogue/apps.toml (embedded with include_str!)
  verify      L0  SHA256SUMS.txt parsing, streaming SHA-256
  releases    L1  release discovery: the redirect probe, the API fallback, the on-disk cache
  select      L1  asset selection: rank a real asset list for (os, arch, preference)
  install     L2  per-platform install / launch / remove, state file, atomic flip
  core        L3  the facade the CLI and GUI share: catalogue + state + update plan
  ui-egui     L4  the egui shell and its theme tokens
apps/craftcenter        the desktop app (eframe + wgpu)
apps/craftcenter-cli    the headless CLI
xtask/                  layers | catalogue | fixtures | ci
packaging/              env.sh · linux/ · macos/ · windows/
.github/workflows/      ci.yml · release.yml · packaging-lint.yml
assets/                 app-icon/ · fonts/ · icons/
```

Layering is enforced by `cargo xtask layers`, as theirs is: a crate may depend only on lower
layers, and nothing below `ui-egui` may mention egui, eframe or winit. Their *never crash* rule is
adopted verbatim — non-test code returns errors and never panics; `clippy.toml` permits `unwrap`
only in tests; a bad download, a truncated archive, a full disk and a hostile filename all have to
produce a message, not a crash. No code is copied from their repositories: their own `AGENTS.md`
says learnings are shared and code is not, so the theme tokens are re-implemented from the values
documented above rather than taken from `photocraft-ui-egui` — which in any case sits on top of
their engine and is not a standalone toolkit.

### 3.2 The catalogue is data

Adding a new crafting app is one row, no code:

```toml
schema = 1

[[app]]
slug        = "photocraft"
name        = "PhotoCraft"
tagline     = "Image editing: layers, masks, type and real PSD files"
repo        = "storytold/photocraft"
app_id      = "ai.storyteller.photocraft"
binary      = "photocraft"
cli         = "photocraft-cli"
asset_stems = ["photocraft"]          # candidates, newest first
icon        = "assets/app-icon/photocraft-64.png"
site        = "https://getartcraft.com/apps/photocraft"

[[app]]
slug        = "pdfcraft"
name        = "PdfCraft"
tagline     = "Reading, organizing and protecting PDFs"
repo        = "storytold/pdfcraft"
app_id      = "ai.storyteller.pdfcraft"
binary      = "pdfcraft"
asset_stems = ["pdfcraft", "printcraft"]   # the rename of §1.3, handled as data
```

`cargo xtask catalogue --check` re-reads every row against the live releases and reports drift —
a new stem, a vanished variant, a first release for an app that had none. `--icons` refreshes the
committed icons from each repo's `assets/app-icon/hicolor/64x64/apps/<app_id>.png` and rewrites the
`ATTRIBUTION.md` rows.

### 3.3 Asset selection

A ranked preference list per `(os, arch)`, matched against the asset names the release actually has:

| Target | Preference order |
| --- | --- |
| `linux-x86_64`, `linux-aarch64` | `.AppImage` → `.tar.gz` → (`.deb`/`.rpm`/`.flatpak` only if a system install is explicitly chosen) |
| `macos` (any arch) | `-macos-universal.dmg`; the CLI from `<stem>-cli-<ver>-macos-universal.zip` |
| `windows-x64`, `windows-x86` | `-portable.zip` → `.msi` (elevates) |
| `windows-arm64` | arm64 `-portable.zip` → **fall back to x64** (emulation) → `.msi` |

The arm64 fallback is not hypothetical: five of the eleven released apps have no Windows ARM64
asset. Every miss is reported as "not available for this platform" with the reason, never as a
failure.

### 3.4 The GUI

One window, in their grammar: egui `0.36` + eframe on wgpu, their five theme names, Inter and
JetBrains Mono, Lucide icons, a custom title bar with flush caption buttons on Windows and Linux
and an integrated strip on macOS, colours read from a `Tokens` struct and never hard-coded.

- **Catalogue** — a row per app: icon, name, tagline, installed version, latest version, download
  size, and per-row **Install** / **Update** / **Launch** / **Remove** with inline progress.
  **Update all** in the header.
- **Settings** — install location, channel (latest published release only, for now), check
  frequency, optional GitHub token, "keep previous version".
- **About** — version and build commit, the licence, the attribution list, and the statement that
  CraftCenter is unofficial.

### 3.5 The CLI

The same core crate, mirroring their `<app>-cli` pattern — and the only part that a machine without
a display can prove:

```
craftcenter list [--json]          catalogue, installed version, latest version
craftcenter check [<app>]          refresh the release cache
craftcenter install <app>[@ver]    download, verify, install for this user
craftcenter update [<app> | --all]
craftcenter launch <app>
craftcenter remove <app>
craftcenter verify <app>           re-hash what is installed
```

### 3.6 Tests, with no network in CI

Release JSON and `SHA256SUMS.txt` are recorded from the real API into
`crates/releases/fixtures/<app>/`, a couple of kilobytes each, and committed. Then:

- **selection** — a table test over every (app × os × arch) in the catalogue against the recorded
  assets, asserting the chosen asset or a documented "unavailable". `pdfcraft`'s `printcraft-`
  stem and the missing Windows ARM64 builds are fixtures, so the drift of §1.3 is a test, not a
  surprise;
- **verify** — a correct file, a corrupted byte, a truncated file, a name absent from the sums, a
  malformed sums line;
- **install** — against a temporary directory: fresh install, upgrade, atomic flip, rollback when
  the download fails halfway, remove, and a filename from a release that tries to escape the
  install root;
- **catalogue** — every row parses, slugs and app ids are unique, every referenced icon exists;
- **`xtask catalogue --check`** runs on a schedule, not in PR CI, because it needs the network.

### 3.7 CraftCenter's own releases

Its own `release.yml` produces assets in their naming —
`craftcenter-<ver>-linux-{x86_64,aarch64}.{AppImage,tar.gz}`,
`craftcenter-<ver>-macos-universal.dmg`, `craftcenter-<ver>-windows-x64-portable.zip`, and
`SHA256SUMS.txt` — so CraftCenter can be installed, and can check itself, through exactly the same
code path as everything else in the catalogue. Signing secrets are optional in the same way: absent
ones produce unsigned artifacts and a warning.

### 3.8 Rules it holds itself to

- No telemetry, no analytics, no crash reporting, no phone-home of any kind.
- Network access only to `github.com`, `api.github.com`, `objects.githubusercontent.com` and
  `raw.githubusercontent.com`. Any other host is a bug.
- No elevation in the default path, on any platform.
- Checksum verification is mandatory and not configurable away.
- Nothing is installed outside the user's own directories.

---

## 4. Decisions

Each needs a ruling before implementation starts; the recommendation is what will be built absent
one.

| | Decision | Options | Recommendation |
| --- | --- | --- | --- |
| **D1** | Platforms for v1 | all three · Linux only | **All three.** Their assets cover all three, and the selection logic is per-platform data either way. Be precise about what is *proven*: the CI matrix proves it compiles and that the unit and integration tests pass on Linux, macOS and Windows; the CLI is exercised end to end on Linux. GUI behaviour on macOS and Windows stays unverified until a person runs it |
| **D2** | GUI toolkit | egui 0.36 + eframe, their tokens re-implemented · reuse `photocraft-ui-egui` · iced / gpui | **egui 0.36 + eframe + wgpu**, pinned to the version every app uses, with the tokens re-implemented from their published values. Their `AGENTS.md` says repos share learnings and never code, and their UI crate sits on top of their engine — it is not a reusable toolkit |
| **D3** | Is `artcraft` in the catalogue? | listed but not installable · full support behind an "allow unverified" switch · omitted | **Listed, not installable in v1.** Its releases are a different shape entirely (`ArtCraft_0.41.0_x64-setup.exe`, `_x64_en-US.msi`, `ArtCraft_universal.app.tar.gz`, tag `artcraft-v0.41.0`), there is **no `SHA256SUMS.txt`**, there is no Linux build, and the licence is non-OSI. A row with "Download from GitHub ↗" is honest and ships no unverifiable install path |
| **D4** | Install scope | per-user only · also offer system install | **Per-user only in v1**, never elevating. The `.msi` and `deb`/`rpm`/`flatpak` paths stay a v2 item behind an explicit choice |
| **D5** | Update strategy | full download · zsync deltas | **Full download.** Only `photocraft` publishes a `.zsync`, and zsync means implementing rsync rolling checksums. The format table already records `.zsync`, so a delta path can be added later for the apps that have one |
| **D6** | How CraftCenter updates itself | notify only · self-replace | **Notify, with a one-click open of the release page, in v1.** A self-replacing updater across three platforms is where installers break, and it cannot be verified on a machine with no display. v2, once the same verified-download core has run in anger |
| **D7** | Licence | MIT OR Apache-2.0 · one of them · something else | **MIT OR Apache-2.0**, matching theirs, with `LICENSE-MIT`, `LICENSE-APACHE`, `NOTICE` and `ATTRIBUTION.md` |
| **D8** | Name and branding | CraftCenter · a `<x>craft` name · something else | **CraftCenter.** The trademark terms forbid the ArtCraft name in the name, logo, icon or domain of anything else, so "ArtCraft Center" is out; their app-naming grammar is `{Function}Craft`, which "CraftCenter" deliberately inverts to read as a hub rather than an app. Needs a ruling — it is a naming call, not a technical one |
| **D9** | Per-app icons | commit them (attributed) · fetch at runtime · none | **Commit them.** Each app's icon is its owner's original work under MIT OR Apache-2.0, so redistribution with attribution is permitted; `xtask catalogue --icons` fetches and attributes them. The ArtCraft wordmark and mark are never used |
| **D10** | Ask upstream? | open one issue · just read releases | **Both:** build against the releases, and open one upstream issue asking whether they would bless a third-party installer, publish a catalogue endpoint, and add `SHA256SUMS.txt` to `artcraft`. Public and attributable, so it needs a ruling first |
| **D11** | GitHub token | optional, off by default · required · never | **Optional, off by default**, stored in the OS keyring where one exists. The primary check path costs no API quota, so the token only widens the fallback |
| **D12** | A `-web` build | no · yes | **No.** Every crafting app ships one, but a page in a browser cannot install a desktop application. The README says so, so the omission reads as deliberate |

---

## 5. Open questions

1. **The name** (D8). "CraftCenter" is the working name; the trademark terms rule out anything built
   on "ArtCraft".
2. **Upstream contact** (D10). Opening an issue on their repository is a public act and needs a
   decision before it happens.
3. **`artcraft`** (D3): listed-but-not-installable, or full support behind an explicit
   "allow unverified downloads" switch?
4. **Self-update** (D6): notify-only in v1?
5. **Windows** (D4): portable zip only, or also drive the per-machine `.msi` with its elevation
   prompt?

Nothing else is needed to build v1: no accounts, no secrets, no credentials. Signing CraftCenter's
own releases later would need an Apple Developer ID and a Windows code-signing certificate; until
then its artifacts are unsigned and say so, exactly as the upstream pipeline tolerates.
