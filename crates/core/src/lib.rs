//! The core: one type that the CLI and the desktop app both drive.
//!
//! Everything the user can ask for is a method here, so the two front ends cannot drift apart in
//! behaviour — only in presentation.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable, clippy::indexing_slicing)]

pub mod icons;
mod settings;

use std::path::{Path, PathBuf};

use craftcenter_install::State;
use craftcenter_releases::{Cache, Cached, Client, Release, cache::now_secs};
use craftcenter_select::{Choice, Platform, Preference, Unavailable, select};
use craftcenter_verify::{Sums, hex};

// The three types `Center::with` and `Row` are built out of. They were always part of this
// facade's surface — there is no way to open a `Center` without naming a catalogue and a layout —
// so they are re-exported rather than left to be fetched from a crate the caller should not have
// to depend on.
pub use craftcenter_catalogue::{App, Catalogue, Kind};
pub use craftcenter_install::Error as InstallError;
pub use craftcenter_install::clean_after_self_update;
pub use craftcenter_install::{Installed, Paths};
pub use craftcenter_releases::Error as ReleaseError;
// The transport is part of this facade's surface, not an implementation detail of it: `Center`
// is generic over it so that a test — or the desktop shell's own frame test — can drive the
// whole program against recorded responses rather than the network.
pub use craftcenter_releases::{Fetch, Response, Ureq};
pub use craftcenter_select::{Arch, Format, Note, Os};
// What Verify now answers with, for both front ends to print.
pub use craftcenter_verify::manifest::{Level, Verification};
pub use settings::Settings;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("the catalogue could not be read: {0}")]
    Catalogue(#[from] craftcenter_catalogue::Error),
    #[error(transparent)]
    Install(#[from] craftcenter_install::Error),
    #[error(transparent)]
    Release(#[from] craftcenter_releases::Error),
    #[error(transparent)]
    Verify(#[from] craftcenter_verify::Error),
    #[error("{0}")]
    Unavailable(#[from] Unavailable),
    #[error("no app called {slug:?}; `craftcenter list` shows the catalogue")]
    UnknownApp { slug: String },
    #[error("{app} has published no release yet")]
    NoRelease { app: String },
    #[error("{app}'s release publishes no SHA256SUMS.txt, so the download cannot be verified; CraftCenter will not install it")]
    NoChecksums { app: String },
    #[error("this build runs on a platform CraftCenter does not install for")]
    UnknownPlatform,
    #[error("settings: {message}")]
    Settings { message: String },
    #[error("the running program's path is unknown, so it cannot replace itself")]
    NoCurrentExe,
}

/// Where an app stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    /// Checked, and the app publishes nothing yet.
    NoRelease,
    /// Not installed, and installable.
    Available,
    /// Installed, and the newest release.
    UpToDate,
    /// Installed, and a newer release exists.
    UpdateAvailable { from: String, to: String },
    /// A release exists but nothing in it fits this platform.
    Unavailable { reason: String },
    /// No check has been made yet.
    Unchecked,
}

impl Status {
    pub fn label(&self) -> String {
        match self {
            Status::NoRelease => "no release yet".to_owned(),
            Status::Available => "not installed".to_owned(),
            Status::UpToDate => "up to date".to_owned(),
            Status::UpdateAvailable { from, to } => format!("update {from} \u{2192} {to}"),
            Status::Unavailable { reason } => reason.clone(),
            Status::Unchecked => "not checked yet".to_owned(),
        }
    }

    pub fn is_actionable(&self) -> bool {
        matches!(self, Status::Available | Status::UpdateAvailable { .. })
    }
}

/// One line of the catalogue, as the user sees it.
#[derive(Clone, Debug)]
pub struct Row {
    pub app: App,
    pub installed: Option<Installed>,
    pub latest: Option<Release>,
    pub status: Status,
    /// What would be downloaded, when something would.
    pub choice: Option<Choice>,
    /// When the release was last looked up, in seconds since the Unix epoch.
    pub checked_at: Option<u64>,
}

/// Progress of a download: bytes so far, and the total if the server declared one.
pub type Progress<'a> = &'a mut dyn FnMut(u64, Option<u64>);

/// The result of replacing CraftCenter with a newer build of itself.
#[derive(Clone, Debug)]
pub struct SelfUpdate {
    pub from: String,
    pub to: String,
    /// The program must be restarted to run the new build.
    pub restart_required: bool,
}

/// Everything CraftCenter can do.
pub struct Center<F: Fetch = Ureq> {
    catalogue: Catalogue,
    paths: Paths,
    /// Where apps would go if the user had not chosen anywhere: what "Reset" restores.
    default_apps: PathBuf,
    settings: Settings,
    client: Client<F>,
    cache: Cache,
    platform: Option<Platform>,
}

impl Center<Ureq> {
    /// Open with the real network and the real per-user directories.
    pub fn open() -> Result<Self, Error> {
        let paths = Paths::from_env()?;
        Ok(Self::with(Catalogue::embedded()?, paths, Ureq::new()))
    }
}

impl<F: Fetch> Center<F> {
    /// Open with an explicit catalogue, layout and transport. This is how the tests run, with
    /// recorded responses and a temporary directory.
    pub fn with(catalogue: Catalogue, paths: Paths, fetch: F) -> Self {
        let settings = Settings::load(&Self::settings_path(&paths));
        let cache = Cache::new(paths.releases_cache());
        let default_apps = paths.apps.clone();
        // A chosen location is adopted without being checked here: a folder that has gone
        // missing since it was chosen must not stop CraftCenter from opening, and the install
        // that needs it will say so plainly when the time comes.
        let paths = match &settings.install_dir {
            Some(dir) => paths.with_apps(PathBuf::from(dir)),
            None => paths,
        };
        Self { catalogue, paths, default_apps, settings, client: Client::new(fetch), cache, platform: Platform::host() }
    }

    fn settings_path(paths: &Paths) -> PathBuf {
        paths.state.with_file_name("settings.toml")
    }

    /// Install into `dir` for this run only, without recording it.
    ///
    /// What the command line's `--install-dir` is: one invocation into a folder of your
    /// choosing, with nothing changed about where the next one will go. The folder is checked
    /// the same way a saved choice is, so an unusable one is refused before anything downloads.
    pub fn with_install_dir(mut self, dir: &Path) -> Result<Self, Error> {
        craftcenter_install::check_install_dir(dir)?;
        self.paths = self.paths.with_apps(dir.to_path_buf());
        Ok(self)
    }

    /// Install for a platform other than the host's. Used by `xtask` and the tests to check every
    /// (app × platform) pair without cross-compiling.
    pub fn with_platform(mut self, platform: Platform) -> Self {
        self.platform = Some(platform);
        self
    }

    pub fn catalogue(&self) -> &Catalogue {
        &self.catalogue
    }

    pub fn paths(&self) -> &Paths {
        &self.paths
    }

    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    /// Save the settings and adopt them. A chosen install location is checked first — and
    /// refused with a plain sentence if this user cannot write to it — so the failure happens
    /// here rather than part-way through the next install.
    pub fn set_settings(&mut self, settings: Settings) -> Result<(), Error> {
        let apps = match &settings.install_dir {
            Some(dir) => {
                let dir = PathBuf::from(dir);
                craftcenter_install::check_install_dir(&dir)?;
                dir
            }
            None => self.default_apps.clone(),
        };
        settings.save(&Self::settings_path(&self.paths))?;
        self.paths = self.paths.clone().with_apps(apps);
        self.settings = settings;
        Ok(())
    }

    /// Where apps are installed now.
    pub fn install_dir(&self) -> &Path {
        &self.paths.apps
    }

    /// Where apps would be installed if nobody had chosen: the per-user default for this
    /// platform, or whatever `CRAFTCENTER_ROOT` names.
    pub fn default_install_dir(&self) -> &Path {
        &self.default_apps
    }

    /// Choose where apps are installed from now on, or pass `None` to go back to the default.
    ///
    /// Nothing already installed moves — see [`Self::move_app`], which is the explicit action
    /// for that. Everything installed from here on goes to the new place.
    pub fn set_install_dir(&mut self, dir: Option<&Path>) -> Result<(), Error> {
        let install_dir = dir.map(|dir| dir.display().to_string());
        self.set_settings(Settings { install_dir, ..self.settings.clone() })
    }

    /// The installed apps that are not in the current install location, in catalogue order.
    pub fn misplaced(&self) -> Vec<String> {
        let Ok(state) = State::load(&self.paths.state) else {
            return Vec::new();
        };
        self.catalogue
            .installable()
            .filter(|app| {
                state
                    .get(&app.slug)
                    .is_some_and(|installed| craftcenter_install::app_home(&self.paths, app, installed).parent() != Some(self.paths.apps.as_path()))
            })
            .map(|app| app.slug.clone())
            .collect()
    }

    /// Move one installed app into the current install location.
    ///
    /// Copy, verify, swap, delete, in that order and one app at a time. An app that appears to
    /// be running is refused rather than moved out from under itself.
    pub fn move_app(&self, slug: &str, progress: Progress<'_>) -> Result<Installed, Error> {
        let app = self.app(slug)?;
        let apps = self.paths.apps.clone();
        craftcenter_install::move_app(&self.paths, app, &apps, progress).map_err(Error::from)
    }

    pub fn platform(&self) -> Result<Platform, Error> {
        self.platform.ok_or(Error::UnknownPlatform)
    }

    fn app(&self, slug: &str) -> Result<&App, Error> {
        self.catalogue.get(slug).ok_or_else(|| Error::UnknownApp { slug: slug.to_owned() })
    }

    /// The catalogue as it stands, from what has already been checked. Never touches the network,
    /// so a front end can draw a window before any request has been made.
    pub fn rows(&self) -> Vec<Row> {
        self.catalogue.installable().map(|app| self.row(app)).collect()
    }

    /// One row, including CraftCenter's own.
    pub fn row_for(&self, slug: &str) -> Result<Row, Error> {
        Ok(self.row(self.app(slug)?))
    }

    fn row(&self, app: &App) -> Row {
        let installed = State::load(&self.paths.state).ok().and_then(|s| s.get(&app.slug).cloned());
        let cached = self.cache.get(&app.slug);
        let checked_at = cached.as_ref().map(|c| c.checked_at);
        let latest = cached.as_ref().and_then(|c| c.release.clone());

        let (status, choice) = match (&cached, &latest) {
            (None, _) => (Status::Unchecked, None),
            (Some(_), None) => (Status::NoRelease, None),
            (Some(_), Some(release)) => match self.choose(app, release) {
                Ok(choice) => {
                    let status = match &installed {
                        Some(current) if current.version == release.version => Status::UpToDate,
                        Some(current) => Status::UpdateAvailable { from: current.version.clone(), to: release.version.clone() },
                        None => Status::Available,
                    };
                    (status, Some(choice))
                }
                Err(unavailable) => (Status::Unavailable { reason: unavailable.to_string() }, None),
            },
        };

        Row { app: app.clone(), installed, latest, status, choice, checked_at }
    }

    fn choose(&self, app: &App, release: &Release) -> Result<Choice, Unavailable> {
        let platform = self.platform.ok_or_else(|| Unavailable::NoAsset { app: app.name.clone(), platform: "this platform".to_owned() })?;
        select(app, platform, &release.assets, Preference::default())
    }

    /// Is the recorded check still fresh?
    pub fn is_fresh(&self, slug: &str) -> bool {
        let interval = self.settings.check_interval_secs();
        self.cache.get(slug).is_some_and(|entry| entry.age_secs(now_secs()) < interval)
    }

    /// Look up the latest release of one app and record it.
    ///
    /// Two `github.com` requests and no REST API call in the normal case, so checking every app in
    /// the catalogue costs none of the API's unauthenticated rate limit.
    pub fn check(&self, slug: &str, force: bool) -> Result<Row, Error> {
        let app = self.app(slug)?;
        if !force && self.is_fresh(slug) {
            return Ok(self.row(app));
        }
        let release = self.client.latest(app)?;
        let manifest = release.as_ref().and_then(|r| r.sums.as_ref()).map(manifest_text);
        let entry = Cached { checked_at: now_secs(), release, manifest };
        let _ = self.cache.put(slug, &entry);
        Ok(self.row(app))
    }

    /// Check every installable app. Each result is reported on its own, so one app's rate limit or
    /// network error does not hide the others.
    pub fn check_all(&self, force: bool) -> Vec<(String, Result<Row, Error>)> {
        self.catalogue.installable().map(|app| (app.slug.clone(), self.check(&app.slug, force))).collect()
    }

    /// Download, verify and install one app.
    ///
    /// Verification is not optional: an asset absent from the release's `SHA256SUMS.txt`, or a
    /// release with no manifest at all, stops the install.
    pub fn install(&self, slug: &str, progress: Progress<'_>) -> Result<Installed, Error> {
        let app = self.app(slug)?;
        let row = self.check(slug, false)?;
        let release = row.latest.ok_or_else(|| Error::NoRelease { app: app.name.clone() })?;
        let choice = row.choice.ok_or_else(|| Error::NoRelease { app: app.name.clone() })?;
        let sums = self.sums_for(slug)?.ok_or_else(|| Error::NoChecksums { app: app.name.clone() })?;

        let (archive, digest) = self.fetch_and_verify(app, &release, &choice.asset, &sums, progress)?;
        let request = craftcenter_install::Request {
            app,
            version: &release.version,
            tag: &release.tag,
            choice: &choice,
            archive: &archive,
            sha256: &digest,
            icon_png: app.icon.as_deref().and_then(icons::icon),
        };
        let installed = craftcenter_install::install(&self.paths, &request)?;
        let _ = std::fs::remove_file(&archive);
        if !self.settings.keep_previous {
            craftcenter_install::prune_previous(&self.paths, app)?;
        }
        Ok(installed)
    }

    fn fetch_and_verify(&self, app: &App, release: &Release, asset: &str, sums: &Sums, progress: Progress<'_>) -> Result<(PathBuf, String), Error> {
        let url = app.asset_url(&release.tag, asset);
        let destination = self.paths.downloads().join(asset);
        self.client.download(&url, &destination, progress)?;
        match craftcenter_verify::verify_file(sums, asset, &destination) {
            Ok(()) => {}
            Err(error) => {
                // A download that does not match is deleted, not left for a later run to find.
                let _ = std::fs::remove_file(&destination);
                return Err(error.into());
            }
        }
        let digest = hex(&craftcenter_verify::sha256_file(&destination)?);
        Ok((destination, digest))
    }

    /// The checksum manifest for an app's latest release, from the cache, re-fetched if the cached
    /// entry does not carry it.
    fn sums_for(&self, slug: &str) -> Result<Option<Sums>, Error> {
        if let Some(text) = self.cache.get(slug).and_then(|c| c.manifest) {
            return Ok(Sums::parse(&text).ok());
        }
        let app = self.app(slug)?;
        Ok(self.client.latest_from_manifest(app)?.and_then(|r| r.sums))
    }

    /// Update everything that has an update, newest-first in catalogue order.
    pub fn update_all(&self, progress: Progress<'_>) -> Vec<(String, Result<Installed, Error>)> {
        self.rows()
            .into_iter()
            .filter(|row| matches!(row.status, Status::UpdateAvailable { .. }))
            .map(|row| (row.app.slug.clone(), self.install(&row.app.slug, progress)))
            .collect()
    }

    pub fn launch(&self, slug: &str) -> Result<(), Error> {
        let app = self.app(slug)?;
        craftcenter_install::launch(&self.paths, app)?;
        // The new version has now run, so the one it replaced can go.
        let _ = craftcenter_install::prune_previous(&self.paths, app);
        Ok(())
    }

    pub fn remove(&self, slug: &str) -> Result<(), Error> {
        let app = self.app(slug)?;
        craftcenter_install::remove(&self.paths, app).map_err(Error::from)
    }

    /// Show an installed app's folder in the system's file manager.
    pub fn open_install_dir(&self, slug: &str) -> Result<(), Error> {
        let app = self.app(slug)?;
        let state = State::load(&self.paths.state)?;
        let installed = state.get(slug).ok_or_else(|| InstallError::NotInstalled { app: app.name.clone() })?;
        craftcenter_install::open_externally(&installed.dir).map_err(Error::from)
    }

    /// Where this app's releases are published, as specific a page as is known: the exact tag
    /// when a release has been looked up or installed, the releases index otherwise.
    pub fn release_notes_url(&self, slug: &str) -> Result<String, Error> {
        let app = self.app(slug)?;
        let tag = self
            .cache
            .get(slug)
            .and_then(|entry| entry.release)
            .map(|release| release.tag)
            .or_else(|| State::load(&self.paths.state).ok().and_then(|state| state.get(slug).map(|installed| installed.tag.clone())));
        Ok(match tag {
            Some(tag) => format!("https://github.com/{}/releases/tag/{tag}", app.repo),
            None => format!("https://github.com/{}/releases", app.repo),
        })
    }

    /// Open [`Self::release_notes_url`] in the user's browser.
    pub fn open_release_notes(&self, slug: &str) -> Result<(), Error> {
        craftcenter_install::open_externally(&self.release_notes_url(slug)?).map_err(Error::from)
    }

    /// Check what is on disk against the record of what was installed.
    ///
    /// Four answers, not two: everything matches, something has changed, something is gone, or
    /// there is nothing to check against — an app installed before CraftCenter recorded a digest
    /// for every file is honestly not verifiable rather than either sound or broken.
    ///
    /// Asked of CraftCenter itself, this checks the running program against what its own last
    /// self-update recorded. A build that arrived any other way has recorded nothing, and says so.
    pub fn verify(&self, slug: &str) -> Result<Verification, Error> {
        let app = self.app(slug)?;
        if app.is_self() {
            let exe = std::env::current_exe().map_err(|_| Error::NoCurrentExe)?;
            return Ok(craftcenter_install::verify_self(&self.paths, &app.slug, &exe)?);
        }
        let state = State::load(&self.paths.state)?;
        let installed = state.get(slug).ok_or_else(|| InstallError::NotInstalled { app: app.name.clone() })?;
        Ok(craftcenter_install::verify_installed(&self.paths, app, installed)?)
    }

    /// Replace CraftCenter with a newer build of itself.
    ///
    /// The same download-and-verify path as any other app, and then the same per-format unpack:
    /// the macOS asset is a disk image and the Windows one a zip, so what is swapped is the
    /// `.app` bundle or the extracted executable, never the downloaded asset itself. The build
    /// that is replaced is kept until [`clean_after_self_update`], which the front end calls
    /// once the new one has started. The caller restarts: this returns once it is in place.
    pub fn self_update(&self, progress: Progress<'_>) -> Result<SelfUpdate, Error> {
        let app = self.catalogue.zelf().ok_or_else(|| Error::UnknownApp { slug: "craftcenter".to_owned() })?;
        let current = env!("CARGO_PKG_VERSION").to_owned();
        let release = self.client.latest(app)?.ok_or_else(|| Error::NoRelease { app: app.name.clone() })?;
        if release.version == current {
            return Ok(SelfUpdate { from: current.clone(), to: current, restart_required: false });
        }
        let sums = release.sums.clone().ok_or_else(|| Error::NoChecksums { app: app.name.clone() })?;
        let platform = self.platform()?;
        let choice = select(app, platform, &release.assets, Preference::default())?;

        let (archive, _) = self.fetch_and_verify(app, &release, &choice.asset, &sums, progress)?;
        let exe = std::env::current_exe().map_err(|_| Error::NoCurrentExe)?;
        let replaced = craftcenter_install::self_update(&craftcenter_install::SelfUpdateRequest {
            current_exe: &exe,
            format: choice.format,
            asset: &choice.asset,
            archive: &archive,
            staging: &self.paths.self_update_staging(),
        })?;
        // The new build's own program, recorded so that Verify can be asked of the installer
        // too — whichever file the swap actually put in place: the executable inside the `.app`
        // on macOS, the exe on Windows, the binary on Linux.
        //
        // Best-effort on purpose: the swap has already happened, and failing here would report a
        // successful update as a failure over a record that only costs a later check.
        let _ = craftcenter_install::record_self(&self.paths, &app.slug, &replaced.program);
        Ok(SelfUpdate { from: current, to: release.version, restart_required: true })
    }
}

fn manifest_text(sums: &Sums) -> String {
    let mut text = String::new();
    for name in sums.names() {
        if let Some(digest) = sums.digest(name) {
            text.push_str(&hex(digest));
            text.push_str("  ");
            text.push_str(name);
            text.push('\n');
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::io::Write;

    use craftcenter_releases::Response;

    use super::*;

    const PHOTOCRAFT_REDIRECT: &str = include_str!("../../releases/fixtures/photocraft/redirect.txt");
    const PHOTOCRAFT_SUMS: &str = include_str!("../../releases/fixtures/photocraft/SHA256SUMS.txt");

    #[derive(Default)]
    struct Recorded {
        responses: HashMap<String, (u16, Option<String>, Vec<u8>)>,
    }

    impl Recorded {
        fn with(mut self, url: &str, status: u16, location: Option<&str>, body: &[u8]) -> Self {
            self.responses.insert(url.to_owned(), (status, location.map(str::to_owned), body.to_vec()));
            self
        }
    }

    impl Fetch for Recorded {
        fn get(&self, url: &str, _follow: bool) -> Result<Response, ReleaseError> {
            match self.responses.get(url) {
                Some((status, location, body)) => Ok(Response { status: *status, location: location.clone(), body: body.clone() }),
                None => Ok(Response { status: 404, location: None, body: Vec::new() }),
            }
        }

        fn get_to(&self, url: &str, sink: &mut dyn Write, progress: &mut dyn FnMut(u64, Option<u64>)) -> Result<u16, ReleaseError> {
            let response = self.get(url, true)?;
            if response.status == 200 {
                sink.write_all(&response.body).map_err(|source| ReleaseError::Io { path: url.to_owned(), source })?;
                let len = response.body.len() as u64;
                progress(len, Some(len));
            }
            Ok(response.status)
        }
    }

    /// The asset a Linux x86_64 install of photocraft v0.3.0 resolves to.
    const ASSET: &str = "photocraft-0.3.0-linux-x86_64.AppImage";
    /// Stand-in bytes for it; the recorded manifest's digest for that one line is rewritten to
    /// match, so verification succeeds with no network and no 62 MB fixture.
    const PAYLOAD: &[u8] = b"appimage";

    /// The real photocraft v0.3.0 release — its real redirect and its real asset list — with the
    /// digest of the one asset the test actually serves swapped for the digest of [`PAYLOAD`].
    fn manifest_with_payload_digest() -> String {
        let digest = hex(&craftcenter_verify::sha256_reader(PAYLOAD).expect("hash"));
        PHOTOCRAFT_SUMS.lines().map(|line| if line.ends_with(ASSET) { format!("{digest}  {ASSET}") } else { line.to_owned() }).collect::<Vec<_>>().join("\n")
    }

    fn center(root: &std::path::Path) -> Center<Recorded> {
        let catalogue = Catalogue::embedded().expect("catalogue parses");
        let app = catalogue.get("photocraft").cloned().expect("in catalogue");
        let location = PHOTOCRAFT_REDIRECT.trim();

        let fetch = Recorded::default()
            .with(&app.sums_url(), 302, Some(location), b"")
            .with(location, 200, None, manifest_with_payload_digest().as_bytes())
            .with(&app.asset_url("v0.3.0", ASSET), 200, None, PAYLOAD);

        Center::with(catalogue, Paths::rooted(root), fetch).with_platform(Platform::new(Os::Linux, Arch::X86_64))
    }

    #[cfg(unix)]
    #[test]
    fn a_chosen_install_location_takes_new_installs_and_leaves_old_ones_alone() {
        let root = tempfile::tempdir().expect("temp dir");
        let elsewhere = tempfile::tempdir().expect("temp dir");
        let mut center = center(root.path());
        let mut nothing = |_: u64, _: Option<u64>| {};

        // Installed where the platform would put it.
        let first = center.install("photocraft", &mut nothing).expect("installed");
        assert!(PathBuf::from(&first.dir).starts_with(center.default_install_dir()));
        assert!(center.misplaced().is_empty(), "nothing is out of place yet");

        center.set_install_dir(Some(elsewhere.path())).expect("the new location is usable");
        assert_eq!(center.install_dir(), elsewhere.path());
        assert_eq!(center.settings().install_dir.as_deref(), Some(elsewhere.path().display().to_string().as_str()));

        // The app that is already installed has not moved, and says so.
        assert_eq!(center.misplaced(), vec!["photocraft".to_owned()]);
        assert!(PathBuf::from(&first.dir).is_dir(), "the installed app is untouched");

        // Moving it is the separate, explicit action.
        let moved = center.move_app("photocraft", &mut nothing).expect("moved");
        assert!(PathBuf::from(&moved.dir).starts_with(elsewhere.path()), "{}", moved.dir);
        assert!(!PathBuf::from(&first.dir).exists(), "the old copy is gone");
        assert!(center.misplaced().is_empty(), "and nothing is out of place any more");

        // Resetting puts the default back without moving anything a second time.
        center.set_install_dir(None).expect("reset");
        assert_eq!(center.install_dir(), center.default_install_dir());
        assert_eq!(center.settings().install_dir, None);
        assert_eq!(center.misplaced(), vec!["photocraft".to_owned()]);
    }

    #[test]
    fn an_install_location_that_is_a_file_is_refused_with_a_sentence() {
        let root = tempfile::tempdir().expect("temp dir");
        let mut center = center(root.path());
        let file = root.path().join("not-a-folder.txt");
        std::fs::write(&file, b"x").expect("write");

        let error = center.set_install_dir(Some(&file)).expect_err("a file is not a location");
        assert!(error.to_string().contains("file, not a folder"), "{error}");
        // And the setting is unchanged, so nothing is left half-applied.
        assert_eq!(center.settings().install_dir, None);
        assert_eq!(center.install_dir(), center.default_install_dir());
    }

    #[test]
    fn a_chosen_location_survives_a_restart() {
        let root = tempfile::tempdir().expect("temp dir");
        let elsewhere = tempfile::tempdir().expect("temp dir");
        center(root.path()).set_install_dir(Some(elsewhere.path())).expect("chosen");

        let reopened = center(root.path());
        assert_eq!(reopened.install_dir(), elsewhere.path());
        assert_eq!(reopened.default_install_dir(), Paths::rooted(root.path()).apps);
    }

    #[test]
    fn rows_draw_before_anything_has_been_checked() {
        let root = tempfile::tempdir().expect("temp dir");
        let center = center(root.path());
        let rows = center.rows();
        assert_eq!(rows.len(), center.catalogue().installable().count());
        assert!(rows.iter().all(|r| r.status == Status::Unchecked));
        assert!(rows.iter().all(|r| r.installed.is_none()));
    }

    #[test]
    fn craftcenter_itself_is_not_a_row_in_the_catalogue_list() {
        let root = tempfile::tempdir().expect("temp dir");
        let center = center(root.path());
        assert!(center.rows().iter().all(|r| !r.app.is_self()));
        assert!(center.row_for("craftcenter").is_ok(), "but it is still addressable");
    }

    #[test]
    fn a_check_finds_the_release_and_the_row_becomes_available() {
        let root = tempfile::tempdir().expect("temp dir");
        let center = center(root.path());
        let row = center.check("photocraft", true).expect("checked");
        assert_eq!(row.status, Status::Available);
        assert_eq!(row.latest.as_ref().map(|r| r.tag.clone()), Some("v0.3.0".to_owned()));
        assert_eq!(row.choice.as_ref().map(|c| c.asset.clone()), Some(ASSET.to_owned()));
    }

    #[test]
    fn an_app_with_no_release_reads_as_no_release_not_as_an_error() {
        let root = tempfile::tempdir().expect("temp dir");
        let center = center(root.path());
        // Nothing is recorded for soundcraft, so both paths answer 404.
        let row = center.check("soundcraft", true).expect("checked");
        assert_eq!(row.status, Status::NoRelease);
    }

    #[test]
    fn install_verifies_then_installs_then_reads_as_up_to_date() {
        let root = tempfile::tempdir().expect("temp dir");
        let center = center(root.path());
        center.check("photocraft", true).expect("checked");

        let mut seen = Vec::new();
        let installed = center.install("photocraft", &mut |done, total| seen.push((done, total))).expect("installed");
        assert_eq!(installed.version, "0.3.0");
        assert!(!seen.is_empty(), "progress was reported");

        assert_eq!(center.row_for("photocraft").expect("row").status, Status::UpToDate);
        let report = center.verify("photocraft").expect("verified");
        assert_eq!(report.level(), Level::Ok, "{}", report.summary());
    }

    /// The question Verify is for, asked through the facade both front ends call: an installed
    /// file that has changed since it was installed is named, and the verdict is not a pass.
    #[test]
    fn an_installed_file_that_has_changed_since_is_reported_as_modified() {
        let root = tempfile::tempdir().expect("temp dir");
        let center = center(root.path());
        center.check("photocraft", true).expect("checked");
        let installed = center.install("photocraft", &mut |_, _| {}).expect("installed");

        let on_disk = std::path::PathBuf::from(&installed.dir).join("ai.storyteller.photocraft.AppImage");
        let was = std::fs::read(&on_disk).expect("read");
        std::fs::write(&on_disk, b"something else entirely").expect("tamper");

        let report = center.verify("photocraft").expect("verified");
        assert_eq!(report.level(), Level::Modified, "{}", report.summary());
        assert_eq!(report.modified, ["ai.storyteller.photocraft.AppImage"]);

        std::fs::write(&on_disk, &was).expect("put it back");
        assert_eq!(center.verify("photocraft").expect("verified").level(), Level::Ok);
    }

    /// Asked of CraftCenter itself in a build that did not come from CraftCenter's own update —
    /// a test binary, or a packaged one — there is nothing recorded to check against, and that
    /// is an honest answer rather than an error or a pass.
    #[test]
    fn craftcenter_itself_has_nothing_recorded_until_it_has_updated_itself() {
        let root = tempfile::tempdir().expect("temp dir");
        let center = center(root.path());
        let report = center.verify("craftcenter").expect("asked");
        assert_eq!(report.level(), Level::NotVerifiable);
        assert!(report.summary().contains("own update"), "{}", report.summary());
    }

    #[test]
    fn a_tampered_download_is_refused_and_deleted() {
        let root = tempfile::tempdir().expect("temp dir");
        let catalogue = Catalogue::embedded().expect("parses");
        let app = catalogue.get("photocraft").cloned().expect("in catalogue");
        // The manifest promises the digest of PAYLOAD; the server sends something else.
        let location = PHOTOCRAFT_REDIRECT.trim();
        let fetch = Recorded::default()
            .with(&app.sums_url(), 302, Some(location), b"")
            .with(location, 200, None, manifest_with_payload_digest().as_bytes())
            .with(&app.asset_url("v0.3.0", ASSET), 200, None, b"tampered");

        let center = Center::with(catalogue, Paths::rooted(root.path()), fetch).with_platform(Platform::new(Os::Linux, Arch::X86_64));
        center.check("photocraft", true).expect("checked");
        let result = center.install("photocraft", &mut |_, _| {});
        assert!(matches!(result, Err(Error::Verify(craftcenter_verify::Error::Mismatch { .. }))), "{result:?}");
        assert!(!center.paths().downloads().join(ASSET).exists(), "the bad download is not kept");
        assert_eq!(center.row_for("photocraft").expect("row").status, Status::Available, "nothing was installed");
    }

    #[test]
    fn an_update_is_offered_and_then_applied() {
        let root = tempfile::tempdir().expect("temp dir");
        let center = center(root.path());
        center.check("photocraft", true).expect("checked");
        center.install("photocraft", &mut |_, _| {}).expect("installed");

        // Pretend an older version was installed, so the cached release now looks newer.
        let state_path = center.paths().state.clone();
        let mut state = State::load(&state_path).expect("state");
        if let Some(entry) = state.installed.get_mut("photocraft") {
            entry.version = "0.1.0".to_owned();
        }
        state.save(&state_path).expect("saved");

        let row = center.row_for("photocraft").expect("row");
        assert_eq!(row.status, Status::UpdateAvailable { from: "0.1.0".to_owned(), to: "0.3.0".to_owned() });
        let results = center.update_all(&mut |_, _| {});
        assert_eq!(results.len(), 1);
        assert!(results.first().is_some_and(|(_, r)| r.is_ok()));
        assert_eq!(center.row_for("photocraft").expect("row").status, Status::UpToDate);
    }

    #[test]
    fn a_fresh_check_is_not_repeated_unless_forced() {
        let root = tempfile::tempdir().expect("temp dir");
        let center = center(root.path());
        center.check("photocraft", true).expect("checked");
        assert!(center.is_fresh("photocraft"));
        // Nothing to assert about request counts here; what matters is the decision itself.
        assert!(center.check("photocraft", false).is_ok());
    }

    #[test]
    fn an_unknown_app_is_named_in_the_error() {
        let root = tempfile::tempdir().expect("temp dir");
        let center = center(root.path());
        let err = center.install("artcraft", &mut |_, _| {}).expect_err("not in the catalogue");
        assert!(matches!(err, Error::UnknownApp { .. }), "{err}");
        assert!(err.to_string().contains("artcraft"));
    }

    #[test]
    fn remove_undoes_an_install() {
        let root = tempfile::tempdir().expect("temp dir");
        let center = center(root.path());
        center.check("photocraft", true).expect("checked");
        center.install("photocraft", &mut |_, _| {}).expect("installed");
        center.remove("photocraft").expect("removed");
        assert_eq!(center.row_for("photocraft").expect("row").status, Status::Available);
    }

    #[test]
    fn settings_persist_and_never_hold_a_token() {
        let root = tempfile::tempdir().expect("temp dir");
        let mut center = center(root.path());
        center.set_settings(Settings { check_interval_hours: 1, keep_previous: false, theme: "pro".to_owned(), install_dir: None }).expect("saved");
        assert_eq!(center.settings().check_interval_hours, 1);
        let reopened = Center::with(Catalogue::embedded().expect("parses"), Paths::rooted(root.path()), Recorded::default());
        assert_eq!(reopened.settings().check_interval_hours, 1);
    }
}
