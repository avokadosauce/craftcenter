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
            launcher: "/home/example/.local/bin/photocraft".to_owned(),
            installed_at: 1_700_000_000,
            previous: None,
        }
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
