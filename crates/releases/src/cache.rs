//! The on-disk release cache.
//!
//! An update check is cheap but not free, so what a check found is written down with the time it
//! happened, and a check is only repeated once the entry is older than the configured interval.
//! The cache is advisory: a corrupt or unreadable entry is treated as a miss, never as an error
//! the user has to deal with.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::Release;

/// What a check found, and when.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Cached {
    /// Seconds since the Unix epoch.
    pub checked_at: u64,
    /// `None` records "this app has published no release", which is worth caching too.
    pub release: Option<Release>,
    /// The checksum manifest text, kept verbatim so an install can verify without re-fetching.
    #[serde(default)]
    pub manifest: Option<String>,
}

impl Cached {
    pub fn age_secs(&self, now: u64) -> u64 {
        now.saturating_sub(self.checked_at)
    }
}

/// Seconds since the Unix epoch, or 0 if the clock is before it.
pub fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// One JSON file per app under a directory.
#[derive(Clone, Debug)]
pub struct Cache {
    dir: PathBuf,
}

impl Cache {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn path(&self, slug: &str) -> PathBuf {
        self.dir.join(format!("{slug}.json"))
    }

    /// The recorded check for `slug`, or `None` when there is none, it cannot be read, or it is
    /// not the shape this build writes.
    pub fn get(&self, slug: &str) -> Option<Cached> {
        let text = std::fs::read_to_string(self.path(slug)).ok()?;
        serde_json::from_str(&text).ok()
    }

    /// Record a check. Written to a temporary file and renamed, so a crash mid-write cannot leave
    /// a half-written entry; a failure to write is returned but is never fatal to a check.
    pub fn put(&self, slug: &str, entry: &Cached) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        let text = serde_json::to_string_pretty(entry).map_err(std::io::Error::other)?;
        let temp = self.path(slug).with_extension("json.new");
        std::fs::write(&temp, text)?;
        std::fs::rename(&temp, self.path(slug))
    }

    pub fn forget(&self, slug: &str) {
        let _ = std::fs::remove_file(self.path(slug));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release() -> Release {
        Release {
            tag: "v0.3.0".to_owned(),
            version: "0.3.0".to_owned(),
            assets: vec!["photocraft-0.3.0-linux-x86_64.AppImage".to_owned()],
            sums: None,
            source: crate::Source::Manifest,
        }
    }

    #[test]
    fn round_trips_an_entry() {
        let dir = tempfile::tempdir().expect("temp dir");
        let cache = Cache::new(dir.path());
        let entry = Cached { checked_at: 1_700_000_000, release: Some(release()), manifest: Some("sums".to_owned()) };
        cache.put("photocraft", &entry).expect("written");
        let read = cache.get("photocraft").expect("present");
        assert_eq!(read.checked_at, 1_700_000_000);
        assert_eq!(read.release.as_ref().map(|r| r.tag.as_str()), Some("v0.3.0"));
        assert_eq!(read.manifest.as_deref(), Some("sums"));
    }

    #[test]
    fn caches_the_absence_of_a_release() {
        let dir = tempfile::tempdir().expect("temp dir");
        let cache = Cache::new(dir.path());
        cache.put("soundcraft", &Cached { checked_at: 1, release: None, manifest: None }).expect("written");
        let read = cache.get("soundcraft").expect("present");
        assert!(read.release.is_none());
    }

    #[test]
    fn a_corrupt_entry_is_a_miss_not_an_error() {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::write(dir.path().join("photocraft.json"), "{{{ not json").expect("write");
        assert!(Cache::new(dir.path()).get("photocraft").is_none());
    }

    #[test]
    fn a_missing_entry_is_a_miss() {
        let dir = tempfile::tempdir().expect("temp dir");
        assert!(Cache::new(dir.path()).get("nothing-here").is_none());
    }

    #[test]
    fn age_never_goes_backwards() {
        let entry = Cached { checked_at: 100, release: None, manifest: None };
        assert_eq!(entry.age_secs(160), 60);
        assert_eq!(entry.age_secs(10), 0, "a clock that went backwards must not look like a fresh check");
    }

    #[test]
    fn forget_removes_an_entry() {
        let dir = tempfile::tempdir().expect("temp dir");
        let cache = Cache::new(dir.path());
        cache.put("x", &Cached { checked_at: 1, release: None, manifest: None }).expect("written");
        cache.forget("x");
        assert!(cache.get("x").is_none());
    }
}
