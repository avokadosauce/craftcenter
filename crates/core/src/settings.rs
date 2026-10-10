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
    /// Where apps are installed, when the user has chosen somewhere other than the per-user
    /// default for their platform. `None` means the default, which is what "Reset" restores.
    ///
    /// Only new installs go here: an app already installed keeps the home recorded for it, and
    /// moving it is a separate, explicit action.
    #[serde(default)]
    pub install_dir: Option<String>,
    /// The CraftCenter version the official-launcher notice has already been shown and
    /// dismissed for. `None` means it has never been shown. Compared against
    /// `CARGO_PKG_VERSION` so the notice reappears exactly once per new version, the same way a
    /// release note would.
    #[serde(default)]
    pub launcher_notice_shown_for: Option<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self { check_interval_hours: 24, keep_previous: true, theme: "proMedium".to_owned(), install_dir: None, launcher_notice_shown_for: None }
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
        let settings = Settings {
            check_interval_hours: 6,
            keep_previous: false,
            theme: "studio".to_owned(),
            install_dir: None,
            launcher_notice_shown_for: Some("0.3.0".to_owned()),
        };
        settings.save(&path).expect("saved");
        assert_eq!(Settings::load(&path), settings);
    }

    #[test]
    fn a_chosen_install_location_round_trips_and_the_default_writes_nothing() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("settings.toml");

        Settings::default().save(&path).expect("saved");
        let text = std::fs::read_to_string(&path).expect("read back");
        assert!(!text.contains("install_dir"), "the default location is not written out: {text}");

        let chosen = Settings { install_dir: Some("/srv/crafting apps".to_owned()), ..Settings::default() };
        chosen.save(&path).expect("saved");
        assert_eq!(Settings::load(&path), chosen);

        // And resetting puts it back to the default.
        Settings { install_dir: None, ..chosen }.save(&path).expect("saved");
        assert_eq!(Settings::load(&path).install_dir, None);
    }

    #[test]
    fn the_shown_for_version_round_trips_and_defaults_to_nothing() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("settings.toml");

        assert_eq!(Settings::default().launcher_notice_shown_for, None);

        let shown = Settings { launcher_notice_shown_for: Some("0.4.0".to_owned()), ..Settings::default() };
        shown.save(&path).expect("saved");
        assert_eq!(Settings::load(&path), shown);
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
