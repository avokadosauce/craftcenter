//! Where things go.
//!
//! Everything is under the user's own directories and nothing needs administrator rights. The
//! platform conventions differ, so the paths do; the shape — a versioned directory per app, a
//! stable pointer to the current one, and state beside them — does not.

use std::path::{Path, PathBuf};

use crate::Error;

/// The directory set CraftCenter writes to.
#[derive(Clone, Debug)]
pub struct Paths {
    /// One directory per app, one directory per version inside it.
    pub apps: PathBuf,
    /// Where the stable pointer to the current version goes (`~/.local/bin` on Linux).
    pub bin: PathBuf,
    /// The XDG data root, for `.desktop` entries and hicolor icons (Linux only).
    pub data: PathBuf,
    /// The installed-apps record.
    pub state: PathBuf,
    /// Release-check cache and part-downloaded assets.
    pub cache: PathBuf,
}

impl Paths {
    /// The real per-user locations for this platform.
    pub fn for_user() -> Result<Self, Error> {
        let missing = |what: &'static str| Error::NoHome { what };
        let home = dirs::home_dir().ok_or_else(|| missing("home directory"))?;

        if cfg!(target_os = "macos") {
            let support = dirs::data_dir().ok_or_else(|| missing("application support directory"))?;
            return Ok(Self {
                apps: home.join("Applications"),
                bin: home.join(".local/bin"),
                data: support.join("CraftCenter"),
                state: support.join("CraftCenter/state.toml"),
                cache: dirs::cache_dir().ok_or_else(|| missing("cache directory"))?.join("CraftCenter"),
            });
        }

        if cfg!(target_os = "windows") {
            let local = dirs::data_local_dir().ok_or_else(|| missing("local app data directory"))?;
            let roaming = dirs::data_dir().ok_or_else(|| missing("app data directory"))?;
            return Ok(Self {
                apps: local.join("Programs/CraftCenter"),
                bin: local.join("Programs/CraftCenter/bin"),
                data: roaming.join("Microsoft/Windows/Start Menu/Programs"),
                state: roaming.join("CraftCenter/state.toml"),
                cache: local.join("CraftCenter/cache"),
            });
        }

        // Linux and anything else XDG-shaped.
        let data = dirs::data_dir().ok_or_else(|| missing("XDG data directory"))?;
        let state_root = std::env::var_os("XDG_STATE_HOME").map(PathBuf::from).unwrap_or_else(|| home.join(".local/state"));
        Ok(Self {
            apps: data.join("craftcenter/apps"),
            bin: home.join(".local/bin"),
            data,
            state: state_root.join("craftcenter/state.toml"),
            cache: dirs::cache_dir().ok_or_else(|| missing("XDG cache directory"))?.join("craftcenter"),
        })
    }

    /// Every path under one root. This is what the tests install into, and what
    /// `CRAFTCENTER_ROOT` gives a user who wants a self-contained install.
    pub fn rooted(root: &Path) -> Self {
        Self { apps: root.join("apps"), bin: root.join("bin"), data: root.join("share"), state: root.join("state/state.toml"), cache: root.join("cache") }
    }

    /// `CRAFTCENTER_ROOT` when it is set, the platform locations otherwise.
    pub fn from_env() -> Result<Self, Error> {
        match std::env::var_os("CRAFTCENTER_ROOT") {
            Some(root) if !root.is_empty() => Ok(Self::rooted(Path::new(&root))),
            _ => Self::for_user(),
        }
    }

    /// The same layout with the apps kept somewhere else.
    ///
    /// Only the apps move. State, cache and launchers stay where the platform puts them, because
    /// they are CraftCenter's own bookkeeping rather than anything the user chose a place for.
    pub fn with_apps(self, apps: PathBuf) -> Self {
        Self { apps, ..self }
    }

    pub fn app_dir(&self, slug: &str) -> PathBuf {
        self.apps.join(slug)
    }

    pub fn version_dir(&self, slug: &str, version: &str) -> PathBuf {
        self.apps.join(slug).join(version)
    }

    pub fn downloads(&self) -> PathBuf {
        self.cache.join("downloads")
    }

    pub fn releases_cache(&self) -> PathBuf {
        self.cache.join("releases")
    }

    /// Create the directories an install needs. Done up front so a failure happens before
    /// anything is downloaded.
    pub fn create(&self) -> Result<(), Error> {
        for dir in [&self.apps, &self.bin, &self.cache] {
            crate::fs::mkdir_p(dir)?;
        }
        if let Some(parent) = self.state.parent() {
            crate::fs::mkdir_p(parent)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rooted_layout_stays_under_its_root() {
        let root = Path::new("/tmp/example-root");
        let paths = Paths::rooted(root);
        for path in [&paths.apps, &paths.bin, &paths.data, &paths.state, &paths.cache] {
            assert!(path.starts_with(root), "{} escaped {}", path.display(), root.display());
        }
        assert_eq!(paths.version_dir("photocraft", "0.3.0"), root.join("apps/photocraft/0.3.0"));
    }

    #[test]
    fn choosing_where_apps_go_moves_only_the_apps() {
        let root = Path::new("/tmp/example-root");
        let chosen = PathBuf::from("/tmp/somewhere-else");
        let default = Paths::rooted(root);
        let paths = Paths::rooted(root).with_apps(chosen.clone());
        assert_eq!(paths.apps, chosen);
        assert_eq!(paths.app_dir("photocraft"), chosen.join("photocraft"));
        // The bookkeeping is CraftCenter's own and does not follow the apps around.
        assert_eq!(paths.state, default.state);
        assert_eq!(paths.cache, default.cache);
        assert_eq!(paths.bin, default.bin);
        assert_eq!(paths.data, default.data);
    }

    #[test]
    fn the_platform_layout_never_leaves_the_users_own_directories() {
        let Ok(paths) = Paths::for_user() else {
            return; // No home directory in this environment; nothing to assert.
        };
        let Some(home) = dirs::home_dir() else { return };
        for path in [&paths.apps, &paths.bin, &paths.state, &paths.cache] {
            assert!(path.starts_with(&home), "{} is outside {}", path.display(), home.display());
        }
    }
}
