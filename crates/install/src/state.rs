//! What is installed, as a file.

use std::collections::BTreeMap;
use std::path::Path;

use craftcenter_select::Format;
use serde::{Deserialize, Serialize};

use crate::Error;

/// One installed app.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Installed {
    /// The version as the asset filename spelled it, which is also the directory name.
    pub version: String,
    /// The release tag it came from.
    pub tag: String,
    /// The asset that was installed.
    pub asset: String,
    /// The verified SHA-256 of that asset, in hex. Recorded so `verify` can re-check later.
    pub sha256: String,
    pub format: Format,
    /// The directory holding this version.
    pub dir: String,
    /// The directory holding *every* version of this app — the app's own home under whichever
    /// install location was current when it was first installed.
    ///
    /// Recorded per app so that changing the install location leaves what is already installed
    /// exactly where it is: an update goes back into this directory, and a remove deletes this
    /// one, whatever the setting says today. Absent in records written before the location could
    /// be chosen, where today's layout is a safe guess.
    #[serde(default)]
    pub root: Option<String>,
    /// The SHA-256, in hex, of the per-file manifest recorded beside this entry.
    ///
    /// Its presence is what separates an install that can be checked file by file from one
    /// recorded before CraftCenter wrote manifests at all — which is "not verifiable" rather
    /// than broken. Both files are the user's own and a determined hand can rewrite either; what
    /// this digest catches is a manifest removed or swapped without the record being touched.
    #[serde(default)]
    pub manifest: Option<String>,
    /// What to run.
    pub launcher: String,
    /// Seconds since the Unix epoch.
    pub installed_at: u64,
    /// The previous version's directory, kept until this one has launched once.
    #[serde(default)]
    pub previous: Option<String>,
}

/// The installed-apps record.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct State {
    #[serde(default)]
    pub installed: BTreeMap<String, Installed>,
}

impl State {
    /// Read the record. A missing file is an empty record; an unreadable or malformed one is an
    /// error, because silently forgetting what is installed would orphan it.
    pub fn load(path: &Path) -> Result<Self, Error> {
        match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text).map_err(|source| Error::State { path: path.display().to_string(), message: source.to_string() }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(source) => Err(Error::Io { path: path.display().to_string(), source }),
        }
    }

    /// Write the record atomically.
    pub fn save(&self, path: &Path) -> Result<(), Error> {
        let text = toml::to_string_pretty(self).map_err(|source| Error::State { path: path.display().to_string(), message: source.to_string() })?;
        crate::fs::write_atomic(path, text.as_bytes())
    }

    pub fn get(&self, slug: &str) -> Option<&Installed> {
        self.installed.get(slug)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry() -> Installed {
        Installed {
            version: "0.3.0".to_owned(),
            tag: "v0.3.0".to_owned(),
            asset: "photocraft-0.3.0-linux-x86_64.AppImage".to_owned(),
            sha256: "29e3011f49a52ea25c8fe404258a6c5fadb02094dbb40a884d69e6ba808e6136".to_owned(),
            format: Format::AppImage,
            dir: "/home/example/.local/share/craftcenter/apps/photocraft/0.3.0".to_owned(),
            root: Some("/home/example/.local/share/craftcenter/apps/photocraft".to_owned()),
            manifest: Some("f".repeat(64)),
            launcher: "/home/example/.local/bin/photocraft".to_owned(),
            installed_at: 1_700_000_000,
            previous: None,
        }
    }

    /// A record written before the install location could be chosen still reads, and simply has
    /// no home of its own recorded. Forgetting what is installed would orphan it.
    #[test]
    fn a_record_from_before_the_install_location_was_a_setting_still_loads() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("state.toml");
        let mut state = State::default();
        state.installed.insert("photocraft".to_owned(), Installed { root: None, manifest: None, ..entry() });
        state.save(&path).expect("saved");

        let text = std::fs::read_to_string(&path).expect("read back");
        assert!(!text.contains("root"), "an absent home is not written out");
        assert!(!text.contains("manifest"), "neither is an absent manifest");
        let read = State::load(&path).expect("loaded");
        assert_eq!(read.get("photocraft").and_then(|installed| installed.root.clone()), None);
        assert_eq!(read.get("photocraft").and_then(|installed| installed.manifest.clone()), None, "an older record is not verifiable, not broken");
        assert_eq!(read.get("photocraft").map(|installed| installed.version.clone()).as_deref(), Some("0.3.0"));
    }

    /// The home is what a change of install location must not disturb, so it is written out.
    #[test]
    fn the_home_is_part_of_the_record() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("state.toml");
        let mut state = State::default();
        state.installed.insert("photocraft".to_owned(), entry());
        state.save(&path).expect("saved");
        let read = State::load(&path).expect("loaded");
        assert_eq!(read.get("photocraft"), Some(&entry()));
        assert!(read.get("photocraft").and_then(|installed| installed.root.clone()).is_some());
    }

    #[test]
    fn round_trips() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("state.toml");
        let mut state = State::default();
        state.installed.insert("photocraft".to_owned(), entry());
        state.save(&path).expect("saved");
        let read = State::load(&path).expect("loaded");
        assert_eq!(read.get("photocraft"), Some(&entry()));
    }

    #[test]
    fn a_missing_file_is_an_empty_record() {
        let dir = tempfile::tempdir().expect("temp dir");
        let state = State::load(&dir.path().join("absent.toml")).expect("loaded");
        assert!(state.installed.is_empty());
    }

    #[test]
    fn a_malformed_record_is_an_error_rather_than_a_silent_reset() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("state.toml");
        std::fs::write(&path, "this is not toml {{{").expect("write");
        assert!(matches!(State::load(&path), Err(Error::State { .. })));
    }
}
