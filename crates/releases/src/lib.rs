//! Finding the latest release, and fetching its assets.
//!
//! # Why this does not use the REST API first
//!
//! The reflex design is one `api.github.com/repos/<repo>/releases/latest` call per app, with ETags
//! to stay inside the unauthenticated budget. Measured against the live API, that budget is spent
//! either way: a conditional request answered with `304 Not Modified` still increments
//! `x-ratelimit-used`. ETags save bandwidth, not quota, and twelve apps is a fifth of an hour's
//! unauthenticated allowance per check — of a limit that is shared per source IP.
//!
//! `github.com` offers a path that spends none of it. A request for
//! `/<repo>/releases/latest/download/SHA256SUMS.txt` answers `302` with a `location` naming the
//! concrete release, and that file lists every asset of the release with its SHA-256. So two plain
//! `github.com` requests — one for the redirect, one for the manifest — yield the version, the
//! complete asset list and every digest, without touching the API's rate limit at all.
//!
//! The REST API stays as the fallback for a repository that publishes no manifest, and for a probe
//! that fails for any other reason. CraftCenter never sends a token: the primary path needs none,
//! and asking a user for one would be asking for a credential the program has no use for.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable, clippy::indexing_slicing)]

pub mod cache;
mod http;

use std::io::{self, Read, Write};
use std::path::Path;

use craftcenter_catalogue::App;
use craftcenter_verify::Sums;
use serde::{Deserialize, Serialize};

pub use cache::{Cache, Cached};
pub use http::Ureq;

/// The user agent every request carries. Deliberately generic: it names the program and version
/// and nothing about the machine or the person running it.
pub const USER_AGENT: &str = concat!("craftcenter/", env!("CARGO_PKG_VERSION"), " (+https://github.com/avokadosauce/craftcenter)");

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{url}: {message}")]
    Transport { url: String, message: String },
    #[error("{url}: HTTP {status}")]
    Status { url: String, status: u16 },
    #[error("{url}: a redirect without a location header")]
    NoLocation { url: String },
    #[error("{url}: location {location:?} does not name a release tag")]
    NoTag { url: String, location: String },
    #[error("checksum manifest for {repo}: {source}")]
    Sums { repo: String, source: craftcenter_verify::Error },
    #[error("release metadata for {repo}: {source}")]
    Json { repo: String, source: serde_json::Error },
    #[error("release metadata for {repo} has no tag_name")]
    NoTagName { repo: String },
    #[error("GitHub's API rate limit is spent{}", match reset { Some(t) => format!(" until {t}"), None => String::new() })]
    RateLimited { reset: Option<String> },
    #[error("{path}: {source}")]
    Io { path: String, source: io::Error },
}

/// Which of the two paths produced a release.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Source {
    /// The `releases/latest/download/SHA256SUMS.txt` redirect. Costs no API rate limit.
    Manifest,
    /// The REST API, used only when the manifest path does not answer.
    RestApi,
}

/// One release of one app, as much as CraftCenter needs to know about it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Release {
    /// The git tag, for example `v0.3.0`.
    pub tag: String,
    /// The tag without its leading `v`, which is what asset filenames carry.
    pub version: String,
    /// Every asset name in the release.
    pub assets: Vec<String>,
    /// The checksum manifest, when the release published one. Installing requires it.
    #[serde(skip)]
    pub sums: Option<Sums>,
    pub source: Source,
}

impl Release {
    /// The version without its leading `v`, as asset filenames spell it.
    fn strip_v(tag: &str) -> String {
        tag.strip_prefix('v').unwrap_or(tag).to_owned()
    }

    /// Build a release from the redirect target and the manifest body.
    pub fn from_manifest(tag: &str, repo: &str, manifest: &str) -> Result<Self, Error> {
        let sums = Sums::parse(manifest).map_err(|source| Error::Sums { repo: repo.to_owned(), source })?;
        let mut assets = sums.asset_names();
        // The manifest is not an asset of interest to an installer.
        assets.retain(|name| name != "SHA256SUMS.txt");
        Ok(Self { tag: tag.to_owned(), version: Self::strip_v(tag), assets, sums: Some(sums), source: Source::Manifest })
    }

    /// Build a release from `GET /repos/<repo>/releases/latest`.
    pub fn from_api(repo: &str, json: &str) -> Result<Self, Error> {
        #[derive(Deserialize)]
        struct ApiAsset {
            name: String,
        }
        #[derive(Deserialize)]
        struct ApiRelease {
            tag_name: Option<String>,
            #[serde(default)]
            assets: Vec<ApiAsset>,
        }
        let parsed: ApiRelease = serde_json::from_str(json).map_err(|source| Error::Json { repo: repo.to_owned(), source })?;
        let tag = parsed.tag_name.ok_or_else(|| Error::NoTagName { repo: repo.to_owned() })?;
        let assets = parsed.assets.into_iter().map(|a| a.name).filter(|name| name != "SHA256SUMS.txt").collect();
        Ok(Self { version: Self::strip_v(&tag), tag, assets, sums: None, source: Source::RestApi })
    }
}

/// Pull the tag out of a `releases/latest/download/<asset>` redirect target.
///
/// `https://github.com/o/r/releases/download/v0.3.0/SHA256SUMS.txt` → `v0.3.0`.
pub fn tag_from_location(location: &str) -> Option<&str> {
    let (_, after) = location.split_once("/releases/download/")?;
    let tag = after.split('/').next()?;
    if tag.is_empty() { None } else { Some(tag) }
}

/// One HTTP response, reduced to what this crate uses.
pub struct Response {
    pub status: u16,
    pub location: Option<String>,
    pub body: Vec<u8>,
}

/// The HTTP surface, behind a trait so every parsing path is testable with no network.
pub trait Fetch {
    /// `GET url`, following redirects only when asked. A 3xx or 4xx is a [`Response`], not an
    /// error: "no release yet" is a state this program renders, not a failure.
    fn get(&self, url: &str, follow_redirects: bool) -> Result<Response, Error>;

    /// `GET url`, streaming the body into `sink` and reporting progress as
    /// `(bytes so far, total if the server declared one)`.
    fn get_to(&self, url: &str, sink: &mut dyn Write, progress: &mut dyn FnMut(u64, Option<u64>)) -> Result<u16, Error>;
}

/// Release discovery over some [`Fetch`].
pub struct Client<F: Fetch> {
    fetch: F,
}

impl<F: Fetch> Client<F> {
    pub fn new(fetch: F) -> Self {
        Self { fetch }
    }

    /// The latest release of `app`, or `None` when the app has published none.
    ///
    /// Tries the manifest redirect first (no API rate limit), then the REST API.
    pub fn latest(&self, app: &App) -> Result<Option<Release>, Error> {
        match self.latest_from_manifest(app) {
            Ok(Some(release)) => return Ok(Some(release)),
            Ok(None) => {}
            // A transport problem on the cheap path is worth one attempt on the API.
            Err(Error::Transport { .. }) => {}
            Err(other) => return Err(other),
        }
        self.latest_from_api(app)
    }

    /// The manifest path: a redirect that names the tag, then the manifest itself.
    pub fn latest_from_manifest(&self, app: &App) -> Result<Option<Release>, Error> {
        let url = app.sums_url();
        let probe = self.fetch.get(&url, false)?;
        match probe.status {
            301 | 302 | 303 | 307 | 308 => {}
            404 => return Ok(None),
            status => return Err(Error::Status { url, status }),
        }
        let location = probe.location.ok_or_else(|| Error::NoLocation { url: url.clone() })?;
        let tag = tag_from_location(&location).ok_or_else(|| Error::NoTag { url: url.clone(), location: location.clone() })?.to_owned();

        let manifest = self.fetch.get(&location, true)?;
        if manifest.status == 404 {
            return Ok(None);
        }
        if manifest.status != 200 {
            return Err(Error::Status { url: location, status: manifest.status });
        }
        let text = String::from_utf8_lossy(&manifest.body);
        Release::from_manifest(&tag, &app.repo, &text).map(Some)
    }

    /// The REST fallback. Unauthenticated, so it is rate limited per source IP; a spent budget is
    /// reported as [`Error::RateLimited`] rather than as a failure on every row.
    pub fn latest_from_api(&self, app: &App) -> Result<Option<Release>, Error> {
        let url = app.api_url();
        let response = self.fetch.get(&url, true)?;
        match response.status {
            200 => {}
            404 => return Ok(None),
            403 | 429 => return Err(Error::RateLimited { reset: None }),
            status => return Err(Error::Status { url, status }),
        }
        let text = String::from_utf8_lossy(&response.body);
        Release::from_api(&app.repo, &text).map(Some)
    }

    /// Download `url` to `dest`, atomically: the bytes go to `dest.part` and are renamed into
    /// place only once the whole body has been written, so an interrupted download never leaves
    /// something that looks finished.
    pub fn download(&self, url: &str, dest: &Path, progress: &mut dyn FnMut(u64, Option<u64>)) -> Result<(), Error> {
        let io_err = |path: &Path| {
            let path = path.display().to_string();
            move |source: io::Error| Error::Io { path: path.clone(), source }
        };
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(io_err(parent))?;
        }
        let part = dest.with_extension("part");
        let mut file = std::fs::File::create(&part).map_err(io_err(&part))?;
        let status = self.fetch.get_to(url, &mut file, progress)?;
        if status != 200 {
            let _ = std::fs::remove_file(&part);
            return Err(Error::Status { url: url.to_owned(), status });
        }
        file.sync_all().map_err(io_err(&part))?;
        drop(file);
        std::fs::rename(&part, dest).map_err(io_err(dest))?;
        Ok(())
    }
}

/// Read a whole body with a ceiling, so a hostile or broken server cannot exhaust memory.
pub(crate) fn read_capped<R: Read>(mut reader: R, cap: usize) -> io::Result<Vec<u8>> {
    let mut out = Vec::new();
    let read = reader.by_ref().take(cap as u64 + 1).read_to_end(&mut out)?;
    if read > cap {
        return Err(io::Error::new(io::ErrorKind::InvalidData, format!("response is larger than {cap} bytes")));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    /// A [`Fetch`] backed by recorded responses, so every path is exercised with no network.
    #[derive(Default)]
    struct Recorded {
        responses: HashMap<String, (u16, Option<String>, Vec<u8>)>,
    }

    impl Recorded {
        fn with(mut self, url: &str, status: u16, location: Option<&str>, body: &str) -> Self {
            self.responses.insert(url.to_owned(), (status, location.map(str::to_owned), body.as_bytes().to_vec()));
            self
        }
    }

    impl Fetch for Recorded {
        fn get(&self, url: &str, _follow_redirects: bool) -> Result<Response, Error> {
            match self.responses.get(url) {
                Some((status, location, body)) => Ok(Response { status: *status, location: location.clone(), body: body.clone() }),
                None => Err(Error::Transport { url: url.to_owned(), message: "not recorded".to_owned() }),
            }
        }

        fn get_to(&self, url: &str, sink: &mut dyn Write, progress: &mut dyn FnMut(u64, Option<u64>)) -> Result<u16, Error> {
            let response = self.get(url, true)?;
            sink.write_all(&response.body).map_err(|source| Error::Io { path: url.to_owned(), source })?;
            let len = response.body.len() as u64;
            progress(len, Some(len));
            Ok(response.status)
        }
    }

    fn app(slug: &str) -> App {
        let catalogue = craftcenter_catalogue::Catalogue::embedded().expect("catalogue parses");
        catalogue.get(slug).cloned().unwrap_or_else(|| panic!("{slug} is in the catalogue"))
    }

    macro_rules! fixture {
        ($slug:literal, $file:literal) => {
            include_str!(concat!("../fixtures/", $slug, "/", $file))
        };
    }

    #[test]
    fn a_tag_is_read_out_of_the_redirect() {
        assert_eq!(tag_from_location("https://github.com/storytold/photocraft/releases/download/v0.3.0/SHA256SUMS.txt"), Some("v0.3.0"));
        assert_eq!(tag_from_location("https://github.com/o/r/releases/tag/v1.0.0"), None);
        assert_eq!(tag_from_location("nonsense"), None);
    }

    #[test]
    fn the_manifest_path_yields_version_assets_and_digests_in_one_release() {
        let app = app("photocraft");
        let fetch = Recorded::default().with(&app.sums_url(), 302, Some(fixture!("photocraft", "redirect.txt").trim()), "").with(
            fixture!("photocraft", "redirect.txt").trim(),
            200,
            None,
            fixture!("photocraft", "SHA256SUMS.txt"),
        );
        let release = Client::new(fetch).latest(&app).expect("fetched").expect("photocraft has a release");

        assert_eq!(release.tag, "v0.3.0");
        assert_eq!(release.version, "0.3.0");
        assert_eq!(release.source, Source::Manifest);
        assert!(release.assets.contains(&"photocraft-0.3.0-linux-x86_64.AppImage".to_owned()));
        assert!(!release.assets.contains(&"SHA256SUMS.txt".to_owned()), "the manifest is not an installable asset");
        let sums = release.sums.as_ref().expect("the manifest path always carries digests");
        assert!(sums.digest("photocraft-0.3.0-macos-universal.dmg").is_some());
    }

    #[test]
    fn an_app_with_no_release_is_none_not_an_error() {
        let app = app("soundcraft");
        // The real response: github.com 404s the manifest, and the API 404s the release.
        let fetch = Recorded::default().with(&app.sums_url(), 404, None, "").with(&app.api_url(), 404, None, "");
        assert!(Client::new(fetch).latest(&app).expect("no error").is_none());
    }

    #[test]
    fn the_api_fallback_parses_a_real_release() {
        let app = app("gridcraft");
        let fetch = Recorded::default().with(&app.api_url(), 200, None, fixture!("gridcraft", "latest.json"));
        let release = Client::new(fetch).latest_from_api(&app).expect("fetched").expect("has a release");
        assert_eq!(release.tag, "v0.1.0");
        assert_eq!(release.source, Source::RestApi);
        assert!(release.assets.iter().any(|a| a.ends_with("-linux-x86_64.AppImage")));
    }

    #[test]
    fn the_api_is_only_tried_when_the_manifest_path_does_not_answer() {
        let app = app("photocraft");
        // No API response is recorded, so reaching for it would be a Transport error.
        let fetch = Recorded::default().with(&app.sums_url(), 302, Some(fixture!("photocraft", "redirect.txt").trim()), "").with(
            fixture!("photocraft", "redirect.txt").trim(),
            200,
            None,
            fixture!("photocraft", "SHA256SUMS.txt"),
        );
        assert!(Client::new(fetch).latest(&app).expect("fetched").is_some());
    }

    #[test]
    fn a_spent_rate_limit_is_its_own_error() {
        let app = app("photocraft");
        let fetch = Recorded::default().with(&app.sums_url(), 500, None, "").with(&app.api_url(), 403, None, "");
        assert!(matches!(Client::new(fetch).latest_from_api(&app), Err(Error::RateLimited { .. })));
    }

    #[test]
    fn a_redirect_without_a_location_is_an_error() {
        let app = app("photocraft");
        let fetch = Recorded::default().with(&app.sums_url(), 302, None, "");
        assert!(matches!(Client::new(fetch).latest_from_manifest(&app), Err(Error::NoLocation { .. })));
    }

    #[test]
    fn a_malformed_manifest_is_an_error_not_a_panic() {
        let app = app("photocraft");
        let location = "https://github.com/storytold/photocraft/releases/download/v9.9.9/SHA256SUMS.txt";
        let fetch = Recorded::default().with(&app.sums_url(), 302, Some(location), "").with(location, 200, None, "this is not a checksum manifest");
        assert!(matches!(Client::new(fetch).latest_from_manifest(&app), Err(Error::Sums { .. })));
    }

    #[test]
    fn every_recorded_release_parses_and_names_its_tag() {
        // One case per app with a published release, from the manifests recorded off the live site.
        let cases: &[(&str, &str, &str)] = &[
            ("photocraft", fixture!("photocraft", "redirect.txt"), fixture!("photocraft", "SHA256SUMS.txt")),
            ("vectorcraft", fixture!("vectorcraft", "redirect.txt"), fixture!("vectorcraft", "SHA256SUMS.txt")),
            ("filmcraft", fixture!("filmcraft", "redirect.txt"), fixture!("filmcraft", "SHA256SUMS.txt")),
            ("lightcraft", fixture!("lightcraft", "redirect.txt"), fixture!("lightcraft", "SHA256SUMS.txt")),
            ("pdfcraft", fixture!("pdfcraft", "redirect.txt"), fixture!("pdfcraft", "SHA256SUMS.txt")),
            ("effectcraft", fixture!("effectcraft", "redirect.txt"), fixture!("effectcraft", "SHA256SUMS.txt")),
            ("designcraft", fixture!("designcraft", "redirect.txt"), fixture!("designcraft", "SHA256SUMS.txt")),
            ("wordcraft", fixture!("wordcraft", "redirect.txt"), fixture!("wordcraft", "SHA256SUMS.txt")),
            ("cadcraft", fixture!("cadcraft", "redirect.txt"), fixture!("cadcraft", "SHA256SUMS.txt")),
            ("gridcraft", fixture!("gridcraft", "redirect.txt"), fixture!("gridcraft", "SHA256SUMS.txt")),
            ("deckcraft", fixture!("deckcraft", "redirect.txt"), fixture!("deckcraft", "SHA256SUMS.txt")),
        ];
        for (slug, redirect, manifest) in cases {
            let tag = tag_from_location(redirect.trim()).unwrap_or_else(|| panic!("{slug}: no tag in {redirect:?}"));
            let release = Release::from_manifest(tag, slug, manifest).unwrap_or_else(|e| panic!("{slug}: {e}"));
            assert!(release.tag.starts_with('v'), "{slug}: {}", release.tag);
            assert!(!release.assets.is_empty(), "{slug}");
            assert!(release.sums.is_some(), "{slug}");
        }
    }

    #[test]
    fn download_writes_atomically() {
        let dir = tempfile::tempdir().expect("temp dir");
        let dest = dir.path().join("asset.bin");
        let fetch = Recorded::default().with("https://github.com/x/y/releases/download/v1/asset.bin", 200, None, "payload");
        let mut seen = Vec::new();
        Client::new(fetch)
            .download("https://github.com/x/y/releases/download/v1/asset.bin", &dest, &mut |done, total| seen.push((done, total)))
            .expect("downloaded");
        assert_eq!(std::fs::read_to_string(&dest).expect("read"), "payload");
        assert!(!dest.with_extension("part").exists(), "the partial file is renamed, not left behind");
        assert_eq!(seen, vec![(7, Some(7))]);
    }

    #[test]
    fn a_failed_download_leaves_nothing_that_looks_finished() {
        let dir = tempfile::tempdir().expect("temp dir");
        let dest = dir.path().join("asset.bin");
        let fetch = Recorded::default().with("https://github.com/x/y/releases/download/v1/asset.bin", 404, None, "not found");
        let result = Client::new(fetch).download("https://github.com/x/y/releases/download/v1/asset.bin", &dest, &mut |_, _| {});
        assert!(matches!(result, Err(Error::Status { status: 404, .. })));
        assert!(!dest.exists());
        assert!(!dest.with_extension("part").exists());
    }

    #[test]
    fn an_oversized_body_is_refused_rather_than_buffered() {
        assert!(read_capped(&b"0123456789"[..], 4).is_err());
        assert_eq!(read_capped(&b"0123"[..], 4).expect("fits"), b"0123");
    }
}
