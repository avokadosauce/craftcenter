//! The core: one type that the CLI and the desktop app both drive.
//!
//! Everything the user can ask for is a method here, so the two front ends cannot drift apart in
//! behaviour — only in presentation.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable, clippy::indexing_slicing)]

pub mod icons;
mod settings;

use std::path::PathBuf;

use craftcenter_catalogue::{App, Catalogue};
use craftcenter_install::{Installed, Paths, State};
use craftcenter_releases::{Cache, Cached, Client, Fetch, Release, Ureq, cache::now_secs};
use craftcenter_select::{Choice, Platform, Preference, Unavailable, select};
use craftcenter_verify::{Sums, hex};

pub use craftcenter_catalogue::Kind;
pub use craftcenter_install::Error as InstallError;
pub use craftcenter_install::{clean_after_self_update, self_replace};
pub use craftcenter_releases::Error as ReleaseError;
pub use craftcenter_select::{Arch, Format, Note, Os};
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
        Self { catalogue, paths, settings, client: Client::new(fetch), cache, platform: Platform::host() }
    }

    fn settings_path(paths: &Paths) -> PathBuf {
        paths.state.with_file_name("settings.toml")
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

    pub fn set_settings(&mut self, settings: Settings) -> Result<(), Error> {
        settings.save(&Self::settings_path(&self.paths))?;
        self.settings = settings;
        Ok(())
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

    /// Re-hash what is installed and compare it with the digest recorded at install time.
    pub fn verify(&self, slug: &str) -> Result<(), Error> {
        let app = self.app(slug)?;
        let state = State::load(&self.paths.state)?;
        let installed = state.get(slug).ok_or_else(|| InstallError::NotInstalled { app: app.name.clone() })?;

        let path = match installed.format {
            Format::AppImage => PathBuf::from(&installed.dir).join(format!("{}.AppImage", app.app_id)),
            _ => PathBuf::from(&installed.launcher),
        };
        let actual = hex(&craftcenter_verify::sha256_file(&path)?);
        if actual == installed.sha256 {
            Ok(())
        } else {
            Err(craftcenter_verify::Error::Mismatch { name: installed.asset.clone(), expected: installed.sha256.clone(), actual }.into())
        }
    }

    /// Replace CraftCenter with a newer build of itself.
    ///
    /// The same download-and-verify path as any other app, then an atomic swap of the running
    /// executable. The caller restarts: this returns once the new build is in place.
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
        craftcenter_install::self_replace(&exe, &archive)?;
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
        assert!(center.verify("photocraft").is_ok(), "what is on disk matches what was recorded");
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
        center.set_settings(Settings { check_interval_hours: 1, keep_previous: false, theme: "pro".to_owned() }).expect("saved");
        assert_eq!(center.settings().check_interval_hours, 1);
        let reopened = Center::with(Catalogue::embedded().expect("parses"), Paths::rooted(root.path()), Recorded::default());
        assert_eq!(reopened.settings().check_interval_hours, 1);
    }
}
