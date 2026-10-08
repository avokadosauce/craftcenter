//! The settings file.
//!
//! Short on purpose. There is no telemetry switch because there is no telemetry, and no access
//! token field because the update check that matters spends no API rate limit and so needs no
//! credential.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::Error;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    /// How long a recorded check stays fresh.
    pub check_interval_hours: u64,
    /// Keep the replaced version on disk until the new one has been launched once.
    pub keep_previous: bool,
    /// The theme the desktop app opens in; one of the ids in `craftcenter-ui-egui`.
    pub theme: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self { check_interval_hours: 24, keep_previous: true, theme: "proMedium".to_owned() }
    }
}

impl Settings {
    /// Read the settings. A missing file gives the defaults; a malformed one gives the defaults
    /// too, because a typo in a preferences file must not stop the program from running.
    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path).ok().and_then(|text| toml::from_str(&text).ok()).unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> Result<(), Error> {
        let text = toml::to_string_pretty(self).map_err(|e| Error::Settings { message: e.to_string() })?;
        craftcenter_install::fs::write_atomic(path, text.as_bytes()).map_err(Error::from)
    }

    pub fn check_interval_secs(&self) -> u64 {
        self.check_interval_hours.saturating_mul(3600)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_a_daily_check_that_keeps_one_old_version() {
        let settings = Settings::default();
        assert_eq!(settings.check_interval_hours, 24);
        assert_eq!(settings.check_interval_secs(), 86_400);
        assert!(settings.keep_previous);
    }

    #[test]
    fn round_trips() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("settings.toml");
        let settings = Settings { check_interval_hours: 6, keep_previous: false, theme: "studio".to_owned() };
        settings.save(&path).expect("saved");
        assert_eq!(Settings::load(&path), settings);
    }

    #[test]
    fn a_malformed_file_falls_back_to_the_defaults_rather_than_failing_to_start() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("settings.toml");
        std::fs::write(&path, "check_interval_hours = \"every so often\"").expect("write");
        assert_eq!(Settings::load(&path), Settings::default());
    }

    #[test]
    fn there_is_no_token_setting() {
        let text = toml::to_string_pretty(&Settings::default()).expect("serialises");
        assert!(!text.contains("token"), "CraftCenter never asks for a GitHub token");
    }
}
