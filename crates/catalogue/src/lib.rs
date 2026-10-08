//! The catalogue: which apps CraftCenter knows about, read from `catalogue/apps.toml`.
//!
//! The file is embedded at build time, so a release binary carries the catalogue it shipped with
//! and needs no network to list apps. Adding a Crafting App is one `[[app]]` row.
//!
//! Nothing here panics: a malformed catalogue is an [`Error`], and the embedded one is covered by
//! a test so a bad edit fails `cargo test` rather than a user's launch.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable, clippy::indexing_slicing)]

use std::collections::BTreeSet;

use serde::Deserialize;

/// The catalogue as shipped, before parsing. Exposed so `xtask` can lint the same text.
pub const EMBEDDED: &str = include_str!("../../../catalogue/apps.toml");

/// The schema version this build understands.
pub const SCHEMA: u32 = 1;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("catalogue is not valid TOML: {0}")]
    Toml(#[from] toml::de::Error),
    #[error("catalogue schema {found} is not supported (this build reads schema {SCHEMA})")]
    Schema { found: u32 },
    #[error("catalogue is empty")]
    Empty,
    #[error("app {slug}: {problem}")]
    App { slug: String, problem: String },
    #[error("duplicate {field} {value:?}")]
    Duplicate { field: &'static str, value: String },
    #[error("exactly one app must have kind = \"self\"; found {found}")]
    SelfRows { found: usize },
}

/// What a row is for. CraftCenter itself is a row so that it updates through the same code path as
/// everything else it installs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// An app CraftCenter installs for the user.
    #[default]
    App,
    /// CraftCenter itself.
    #[serde(rename = "self")]
    Zelf,
}

/// One catalogue row.
#[derive(Clone, Debug, Deserialize)]
pub struct App {
    pub slug: String,
    pub name: String,
    pub tagline: String,
    /// `<owner>/<repo>` on github.com.
    pub repo: String,
    /// The upstream's reverse-DNS id; names the Linux `.desktop` file.
    pub app_id: String,
    /// The executable inside the release asset.
    pub binary: String,
    /// The headless binary, where the release ships one.
    #[serde(default)]
    pub cli: Option<String>,
    /// Release-asset filename stems to look for, newest first.
    pub asset_stems: Vec<String>,
    /// File name under `assets/app-icon/`.
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default)]
    pub site: Option<String>,
    #[serde(default)]
    pub kind: Kind,
}

impl App {
    /// The `<owner>` part of [`App::repo`].
    pub fn owner(&self) -> &str {
        self.repo.split('/').next().unwrap_or(&self.repo)
    }

    /// The `<repo>` part of [`App::repo`].
    pub fn repo_name(&self) -> &str {
        self.repo.split('/').nth(1).unwrap_or(&self.repo)
    }

    /// Where the latest release's checksum manifest lives. A `GET` of this answers with a 302 whose
    /// target names the tag, and a body listing every asset of that release with its SHA-256 — the
    /// whole update check in one request, and it spends none of the REST API's rate limit.
    pub fn sums_url(&self) -> String {
        format!("https://github.com/{}/releases/latest/download/SHA256SUMS.txt", self.repo)
    }

    /// The REST endpoint used only when [`App::sums_url`] does not answer.
    pub fn api_url(&self) -> String {
        format!("https://api.github.com/repos/{}/releases/latest", self.repo)
    }

    /// The download URL of one asset of one release.
    pub fn asset_url(&self, tag: &str, asset: &str) -> String {
        format!("https://github.com/{}/releases/download/{tag}/{asset}", self.repo)
    }

    pub fn is_self(&self) -> bool {
        self.kind == Kind::Zelf
    }
}

#[derive(Debug, Deserialize)]
struct Raw {
    schema: u32,
    #[serde(default, rename = "app")]
    apps: Vec<App>,
}

/// Every app this build knows about, in catalogue order.
#[derive(Clone, Debug)]
pub struct Catalogue {
    apps: Vec<App>,
}

impl Catalogue {
    /// Parse and validate the catalogue compiled into this binary.
    pub fn embedded() -> Result<Self, Error> {
        Self::parse(EMBEDDED)
    }

    /// Parse and validate catalogue text.
    pub fn parse(text: &str) -> Result<Self, Error> {
        let raw: Raw = toml::from_str(text)?;
        if raw.schema != SCHEMA {
            return Err(Error::Schema { found: raw.schema });
        }
        if raw.apps.is_empty() {
            return Err(Error::Empty);
        }

        let mut slugs = BTreeSet::new();
        let mut ids = BTreeSet::new();
        let mut selves = 0usize;
        for app in &raw.apps {
            let bad = |problem: &str| Error::App { slug: app.slug.clone(), problem: problem.to_owned() };
            if app.slug.is_empty() || !app.slug.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') {
                return Err(bad("slug must be lowercase ASCII, digits and dashes"));
            }
            if app.name.trim().is_empty() {
                return Err(bad("name is empty"));
            }
            if app.tagline.trim().is_empty() {
                return Err(bad("tagline is empty"));
            }
            if app.repo.split('/').count() != 2 || app.repo.split('/').any(str::is_empty) {
                return Err(bad("repo must be \"<owner>/<repo>\""));
            }
            if app.app_id.split('.').count() < 3 {
                return Err(bad("app_id must be reverse-DNS"));
            }
            if app.binary.trim().is_empty() {
                return Err(bad("binary is empty"));
            }
            if app.asset_stems.is_empty() || app.asset_stems.iter().any(|s| s.trim().is_empty()) {
                return Err(bad("asset_stems must list at least one non-empty stem"));
            }
            if !slugs.insert(app.slug.as_str()) {
                return Err(Error::Duplicate { field: "slug", value: app.slug.clone() });
            }
            if !ids.insert(app.app_id.as_str()) {
                return Err(Error::Duplicate { field: "app_id", value: app.app_id.clone() });
            }
            if app.is_self() {
                selves += 1;
            }
        }
        if selves != 1 {
            return Err(Error::SelfRows { found: selves });
        }
        Ok(Self { apps: raw.apps })
    }

    /// Every row, catalogue order.
    pub fn apps(&self) -> &[App] {
        &self.apps
    }

    /// The installable apps — everything except CraftCenter's own row.
    pub fn installable(&self) -> impl Iterator<Item = &App> {
        self.apps.iter().filter(|a| !a.is_self())
    }

    /// CraftCenter's own row.
    pub fn zelf(&self) -> Option<&App> {
        self.apps.iter().find(|a| a.is_self())
    }

    pub fn get(&self, slug: &str) -> Option<&App> {
        self.apps.iter().find(|a| a.slug == slug)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shipped_catalogue_is_valid() {
        let c = Catalogue::embedded().expect("embedded catalogue parses");
        assert!(c.apps().len() >= 13, "expected the twelve craft apps plus CraftCenter, got {}", c.apps().len());
        assert_eq!(c.installable().count(), c.apps().len() - 1);
        assert!(c.zelf().is_some());
    }

    #[test]
    fn artcraft_is_not_in_the_catalogue() {
        let c = Catalogue::embedded().expect("parses");
        assert!(c.get("artcraft").is_none(), "artcraft is the engine, not one of the creative suite apps");
        assert!(c.apps().iter().all(|a| a.repo != "storytold/artcraft"));
    }

    #[test]
    fn pdfcraft_carries_both_asset_stems() {
        let c = Catalogue::embedded().expect("parses");
        let app = c.get("pdfcraft").expect("pdfcraft is in the catalogue");
        assert!(app.asset_stems.contains(&"printcraft".to_owned()), "every published pdfcraft release is named printcraft-*");
        assert!(app.asset_stems.contains(&"pdfcraft".to_owned()), "main's packaging scripts already emit pdfcraft-*");
    }

    #[test]
    fn urls_are_built_from_the_repo() {
        let c = Catalogue::embedded().expect("parses");
        let app = c.get("photocraft").expect("photocraft is in the catalogue");
        assert_eq!(app.owner(), "storytold");
        assert_eq!(app.repo_name(), "photocraft");
        assert_eq!(app.sums_url(), "https://github.com/storytold/photocraft/releases/latest/download/SHA256SUMS.txt");
        assert_eq!(
            app.asset_url("v0.3.0", "photocraft-0.3.0-linux-x86_64.AppImage"),
            "https://github.com/storytold/photocraft/releases/download/v0.3.0/photocraft-0.3.0-linux-x86_64.AppImage"
        );
    }

    #[test]
    fn only_github_urls_are_produced() {
        let c = Catalogue::embedded().expect("parses");
        for app in c.apps() {
            for url in [app.sums_url(), app.api_url(), app.asset_url("v1", "x")] {
                assert!(url.starts_with("https://github.com/") || url.starts_with("https://api.github.com/"), "{url} is not a github.com URL");
            }
        }
    }

    #[test]
    fn a_duplicate_slug_is_rejected() {
        let text = r#"
schema = 1
[[app]]
slug = "a"
name = "A"
tagline = "t"
repo = "o/a"
app_id = "x.y.a"
binary = "a"
asset_stems = ["a"]
kind = "self"
[[app]]
slug = "a"
name = "A2"
tagline = "t"
repo = "o/a2"
app_id = "x.y.a2"
binary = "a2"
asset_stems = ["a2"]
"#;
        assert!(matches!(Catalogue::parse(text), Err(Error::Duplicate { field: "slug", .. })));
    }

    #[test]
    fn a_catalogue_without_exactly_one_self_row_is_rejected() {
        let text = r#"
schema = 1
[[app]]
slug = "a"
name = "A"
tagline = "t"
repo = "o/a"
app_id = "x.y.a"
binary = "a"
asset_stems = ["a"]
"#;
        assert!(matches!(Catalogue::parse(text), Err(Error::SelfRows { found: 0 })));
    }

    #[test]
    fn a_future_schema_is_rejected_rather_than_half_read() {
        assert!(matches!(Catalogue::parse("schema = 99"), Err(Error::Schema { found: 99 })));
    }

    #[test]
    fn garbage_is_an_error_not_a_panic() {
        assert!(Catalogue::parse("this is not toml {{{").is_err());
    }
}
