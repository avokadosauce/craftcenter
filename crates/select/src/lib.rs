//! Asset selection.
//!
//! The upstream names release assets `<stem>-<version>-<os>-<arch>.<ext>`, but a template is the
//! wrong way to find one, because the published reality drifts from the pattern in three ways:
//!
//! 1. **The stem is not the repository name.** Every published `pdfcraft` release is named
//!    `printcraft-*`, while the packaging scripts on its `main` branch already emit `pdfcraft-*`.
//! 2. **Variants come and go.** Of the eleven apps with a release, five publish no Windows ARM64
//!    build, four no FreeBSD build, ten no Flatpak and eleven no `.zsync`.
//! 3. **An app can have no release at all.**
//!
//! So selection ranks a release's *real* asset list and reports a miss as "not available for this
//! platform", with the reason, rather than guessing at a filename and 404ing later.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable, clippy::indexing_slicing)]

use craftcenter_catalogue::App;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Os {
    Linux,
    #[serde(rename = "macos")]
    MacOs,
    Windows,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Arch {
    #[serde(rename = "x86_64")]
    X86_64,
    #[serde(rename = "aarch64")]
    Aarch64,
    #[serde(rename = "x86")]
    X86,
}

/// An install target: the pair that decides which assets are candidates.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Platform {
    pub os: Os,
    pub arch: Arch,
}

impl Platform {
    pub const fn new(os: Os, arch: Arch) -> Self {
        Self { os, arch }
    }

    /// The platform this binary was built for, or `None` on a target CraftCenter does not install
    /// for (it never guesses: an unknown target gets an honest "unsupported").
    pub fn host() -> Option<Self> {
        let os = if cfg!(target_os = "linux") {
            Os::Linux
        } else if cfg!(target_os = "macos") {
            Os::MacOs
        } else if cfg!(target_os = "windows") {
            Os::Windows
        } else {
            return None;
        };
        let arch = if cfg!(target_arch = "x86_64") {
            Arch::X86_64
        } else if cfg!(target_arch = "aarch64") {
            Arch::Aarch64
        } else if cfg!(target_arch = "x86") {
            Arch::X86
        } else {
            return None;
        };
        Some(Self { os, arch })
    }

    /// `"linux-x86_64"`, `"macos-aarch64"`, `"windows-x64"` …; the spelling used in CLI output.
    pub fn label(self) -> String {
        format!("{}-{}", self.os_tag(), self.arch_tag())
    }

    fn os_tag(self) -> &'static str {
        match self.os {
            Os::Linux => "linux",
            Os::MacOs => "macos",
            Os::Windows => "windows",
        }
    }

    /// The arch spelling the upstream uses in asset names — which differs by OS: Linux assets say
    /// `x86_64`/`aarch64`, Windows assets say `x64`/`arm64`/`x86`.
    fn arch_tag(self) -> &'static str {
        match (self.os, self.arch) {
            (Os::Windows, Arch::X86_64) => "x64",
            (Os::Windows, Arch::Aarch64) => "arm64",
            (Os::Windows, Arch::X86) => "x86",
            (_, Arch::X86_64) => "x86_64",
            (_, Arch::Aarch64) => "aarch64",
            (_, Arch::X86) => "i686",
        }
    }

    /// Parse a label such as `"windows-arm64"`. Used by the CLI's `--platform` and by `xtask`.
    pub fn parse(text: &str) -> Option<Self> {
        let (os, arch) = text.split_once('-')?;
        let os = match os {
            "linux" => Os::Linux,
            "macos" => Os::MacOs,
            "windows" => Os::Windows,
            _ => return None,
        };
        let arch = match arch {
            "x86_64" | "x64" | "amd64" => Arch::X86_64,
            "aarch64" | "arm64" => Arch::Aarch64,
            "x86" | "i686" => Arch::X86,
            _ => return None,
        };
        Some(Self { os, arch })
    }

    /// Every platform CraftCenter installs for, for table-driven tests and `xtask catalogue`.
    pub const ALL: [Self; 7] = [
        Self::new(Os::Linux, Arch::X86_64),
        Self::new(Os::Linux, Arch::Aarch64),
        Self::new(Os::MacOs, Arch::X86_64),
        Self::new(Os::MacOs, Arch::Aarch64),
        Self::new(Os::Windows, Arch::X86_64),
        Self::new(Os::Windows, Arch::X86),
        Self::new(Os::Windows, Arch::Aarch64),
    ];
}

/// The kinds of asset CraftCenter can install from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Format {
    /// A single self-contained executable. Its own `AppRun` installs the app's desktop entry and
    /// icons into `$XDG_DATA_HOME` on first run, so this is the cheapest Linux install there is.
    AppImage,
    /// An FHS tree (`bin/`, `share/…`) under one top-level directory, which relocates into
    /// `~/.local` unchanged.
    TarGz,
    /// A disk image holding `<App>.app`, notarised and stapled.
    Dmg,
    /// A zip that unpacks to a directory and needs no installer.
    PortableZip,
    /// A per-machine installer. Always elevates, so CraftCenter does not use it by default.
    Msi,
    /// The headless companion binary, published for macOS only.
    CliZip,
}

impl Format {
    /// Whether installing from this asset requires administrator rights.
    pub fn needs_elevation(self) -> bool {
        matches!(self, Format::Msi)
    }

    pub fn label(self) -> &'static str {
        match self {
            Format::AppImage => "AppImage",
            Format::TarGz => "tar.gz",
            Format::Dmg => "dmg",
            Format::PortableZip => "portable zip",
            Format::Msi => "msi",
            Format::CliZip => "cli zip",
        }
    }
}

/// Something true about a choice that the user should be told.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Note {
    /// No asset was published for the host architecture, so one for `used` was chosen instead. On
    /// Windows the x64 build runs on ARM64 under emulation.
    ArchFallback { wanted: Arch, used: Arch },
}

impl std::fmt::Display for Note {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Note::ArchFallback { wanted, used } => {
                write!(f, "no {wanted:?} build is published; using the {used:?} build (it runs under emulation)")
            }
        }
    }
}

/// The asset to download, and why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Choice {
    pub asset: String,
    pub format: Format,
    pub note: Option<Note>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum Unavailable {
    #[error("{app} publishes nothing for {platform}")]
    NoAsset { app: String, platform: String },
    #[error("{app} publishes only a per-machine installer for {platform}, which needs administrator rights")]
    ElevationOnly { app: String, platform: String },
}

/// How to choose when several assets would do.
#[derive(Clone, Copy, Debug, Default)]
pub struct Preference {
    /// Allow an asset that requires administrator rights — in practice the Windows `.msi`, which
    /// the upstream builds `Scope="perMachine"`. CraftCenter installs per-user and never elevates,
    /// so this defaults to `false` and nothing in the program sets it.
    pub allow_elevation: bool,
}

/// Does `name` look like `<stem>-<version><suffix>`?
///
/// The version check is what keeps `photocraft-cli-0.3.0-macos-universal.zip` from matching the
/// app's own `photocraft-<version>-macos-universal.*`: after the stem is stripped the remainder
/// would be `cli-0.3.0`, which does not start with a digit.
fn matches(name: &str, stem: &str, suffix: &str) -> bool {
    let Some(rest) = name.strip_prefix(stem).and_then(|r| r.strip_prefix('-')) else {
        return false;
    };
    let Some(version) = rest.strip_suffix(suffix) else {
        return false;
    };
    is_version(version)
}

fn is_version(text: &str) -> bool {
    let mut chars = text.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    first.is_ascii_digit() && text.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+'))
}

fn find<'a>(assets: &'a [String], stems: &[String], suffix: &str) -> Option<&'a str> {
    // Stems in catalogue order, so the preferred spelling wins when a release carries both.
    stems.iter().find_map(|stem| assets.iter().find(|name| matches(name, stem, suffix)).map(String::as_str))
}

/// The ranked candidates for a platform: `(filename suffix, format)`, best first.
fn candidates(platform: Platform) -> Vec<(String, Format)> {
    let arch = platform.arch_tag();
    match platform.os {
        Os::Linux => vec![(format!("-linux-{arch}.AppImage"), Format::AppImage), (format!("-linux-{arch}.tar.gz"), Format::TarGz)],
        // Every macOS asset is a universal binary, so both architectures take the same one.
        Os::MacOs => vec![("-macos-universal.dmg".to_owned(), Format::Dmg)],
        Os::Windows => vec![(format!("-windows-{arch}-portable.zip"), Format::PortableZip), (format!("-windows-{arch}.msi"), Format::Msi)],
    }
}

/// Choose the asset to install for `platform` from the asset names a release actually has.
pub fn select(app: &App, platform: Platform, assets: &[String], pref: Preference) -> Result<Choice, Unavailable> {
    let mut elevation_only = false;

    for (wanted, note) in fallbacks(platform) {
        for (suffix, format) in candidates(wanted) {
            if let Some(asset) = find(assets, &app.asset_stems, &suffix) {
                if format.needs_elevation() && !pref.allow_elevation {
                    elevation_only = true;
                    continue;
                }
                return Ok(Choice { asset: asset.to_owned(), format, note });
            }
        }
    }

    let platform_label = platform.label();
    if elevation_only {
        Err(Unavailable::ElevationOnly { app: app.name.clone(), platform: platform_label })
    } else {
        Err(Unavailable::NoAsset { app: app.name.clone(), platform: platform_label })
    }
}

/// The platforms to try, in order: the host, then any documented substitute.
fn fallbacks(platform: Platform) -> Vec<(Platform, Option<Note>)> {
    let mut out = vec![(platform, None)];
    // Five of the eleven released apps publish no Windows ARM64 asset. Windows on ARM runs x64
    // binaries under emulation, so that is a real install rather than a failure — but the user is
    // told which build they got.
    if platform.os == Os::Windows && platform.arch == Arch::Aarch64 {
        out.push((Platform::new(Os::Windows, Arch::X86_64), Some(Note::ArchFallback { wanted: Arch::Aarch64, used: Arch::X86_64 })));
    }
    out
}

/// The headless companion binary, which the upstream publishes for macOS only.
pub fn select_cli(app: &App, platform: Platform, assets: &[String]) -> Option<Choice> {
    if platform.os != Os::MacOs || app.cli.is_none() {
        return None;
    }
    let stems: Vec<String> = app.asset_stems.iter().map(|s| format!("{s}-cli")).collect();
    let asset = find(assets, &stems, "-macos-universal.zip")?;
    Some(Choice { asset: asset.to_owned(), format: Format::CliZip, note: None })
}

#[cfg(test)]
mod tests {
    use craftcenter_catalogue::Catalogue;

    use super::*;

    fn app(slug: &str) -> App {
        let catalogue = Catalogue::embedded().expect("catalogue parses");
        catalogue.get(slug).cloned().unwrap_or_else(|| panic!("{slug} is in the catalogue"))
    }

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
    }

    /// The real asset list of photocraft v0.3.0.
    fn photocraft_assets() -> Vec<String> {
        names(&[
            "photocraft-0.3.0-freebsd-x86_64.tar.gz",
            "photocraft-0.3.0-linux-aarch64.AppImage",
            "photocraft-0.3.0-linux-aarch64.AppImage.zsync",
            "photocraft-0.3.0-linux-aarch64.deb",
            "photocraft-0.3.0-linux-aarch64.flatpak",
            "photocraft-0.3.0-linux-aarch64.rpm",
            "photocraft-0.3.0-linux-aarch64.tar.gz",
            "photocraft-0.3.0-linux-x86_64.AppImage",
            "photocraft-0.3.0-linux-x86_64.AppImage.zsync",
            "photocraft-0.3.0-linux-x86_64.deb",
            "photocraft-0.3.0-linux-x86_64.flatpak",
            "photocraft-0.3.0-linux-x86_64.rpm",
            "photocraft-0.3.0-linux-x86_64.tar.gz",
            "photocraft-0.3.0-macos-universal.dmg",
            "photocraft-0.3.0-windows-arm64-portable.zip",
            "photocraft-0.3.0-windows-arm64.msi",
            "photocraft-0.3.0-windows-x64-portable.zip",
            "photocraft-0.3.0-windows-x64.msi",
            "photocraft-0.3.0-windows-x86-portable.zip",
            "photocraft-0.3.0-windows-x86.msi",
            "photocraft-cli-0.3.0-macos-universal.zip",
            "photocraft-web-0.3.0.zip",
            "SHA256SUMS.txt",
        ])
    }

    /// The real asset list of pdfcraft v0.2.1 — published under the *printcraft* stem.
    fn pdfcraft_assets() -> Vec<String> {
        names(&[
            "printcraft-0.2.1-freebsd-x86_64.tar.gz",
            "printcraft-0.2.1-linux-aarch64.AppImage",
            "printcraft-0.2.1-linux-x86_64.AppImage",
            "printcraft-0.2.1-linux-x86_64.deb",
            "printcraft-0.2.1-linux-x86_64.rpm",
            "printcraft-0.2.1-linux-x86_64.tar.gz",
            "printcraft-0.2.1-macos-universal.dmg",
            "printcraft-0.2.1-windows-x64-portable.zip",
            "printcraft-0.2.1-windows-x64.msi",
            "printcraft-0.2.1-windows-x86-portable.zip",
            "printcraft-0.2.1-windows-x86.msi",
            "printcraft-cli-0.2.1-macos-universal.zip",
            "printcraft-web-0.2.1.zip",
            "SHA256SUMS.txt",
        ])
    }

    #[test]
    fn linux_prefers_the_appimage() {
        let choice = select(&app("photocraft"), Platform::new(Os::Linux, Arch::X86_64), &photocraft_assets(), Preference::default()).expect("found");
        assert_eq!(choice.asset, "photocraft-0.3.0-linux-x86_64.AppImage");
        assert_eq!(choice.format, Format::AppImage);
        assert!(choice.note.is_none());
    }

    #[test]
    fn the_zsync_index_is_never_chosen_as_the_app() {
        for platform in Platform::ALL {
            if let Ok(choice) = select(&app("photocraft"), platform, &photocraft_assets(), Preference::default()) {
                assert!(!choice.asset.ends_with(".zsync"), "{} is an update index, not an app", choice.asset);
            }
        }
    }

    #[test]
    fn linux_falls_back_to_the_tarball_when_there_is_no_appimage() {
        let assets = names(&["photocraft-0.3.0-linux-x86_64.tar.gz", "photocraft-0.3.0-linux-x86_64.deb"]);
        let choice = select(&app("photocraft"), Platform::new(Os::Linux, Arch::X86_64), &assets, Preference::default()).expect("found");
        assert_eq!(choice.format, Format::TarGz);
    }

    #[test]
    fn deb_and_rpm_and_flatpak_are_never_chosen() {
        let assets = names(&["photocraft-0.3.0-linux-x86_64.deb", "photocraft-0.3.0-linux-x86_64.rpm", "photocraft-0.3.0-linux-x86_64.flatpak"]);
        // They install to /usr and need root, so a per-user installer has no use for them.
        assert!(select(&app("photocraft"), Platform::new(Os::Linux, Arch::X86_64), &assets, Preference::default()).is_err());
    }

    #[test]
    fn macos_takes_the_universal_dmg_on_both_architectures() {
        for arch in [Arch::X86_64, Arch::Aarch64] {
            let choice = select(&app("photocraft"), Platform::new(Os::MacOs, arch), &photocraft_assets(), Preference::default()).expect("found");
            assert_eq!(choice.asset, "photocraft-0.3.0-macos-universal.dmg");
        }
    }

    #[test]
    fn the_cli_zip_is_not_mistaken_for_the_app() {
        let assets = names(&["photocraft-cli-0.3.0-macos-universal.zip"]);
        assert!(select(&app("photocraft"), Platform::new(Os::MacOs, Arch::Aarch64), &assets, Preference::default()).is_err());
    }

    #[test]
    fn the_cli_is_found_separately_and_only_on_macos() {
        let cli = select_cli(&app("photocraft"), Platform::new(Os::MacOs, Arch::Aarch64), &photocraft_assets()).expect("published");
        assert_eq!(cli.asset, "photocraft-cli-0.3.0-macos-universal.zip");
        assert!(select_cli(&app("photocraft"), Platform::new(Os::Linux, Arch::X86_64), &photocraft_assets()).is_none());
    }

    #[test]
    fn windows_prefers_the_portable_zip_over_the_msi() {
        let choice = select(&app("photocraft"), Platform::new(Os::Windows, Arch::X86_64), &photocraft_assets(), Preference::default()).expect("found");
        assert_eq!(choice.asset, "photocraft-0.3.0-windows-x64-portable.zip");
        assert_eq!(choice.format, Format::PortableZip);
    }

    #[test]
    fn windows_arm64_takes_the_arm64_zip_when_there_is_one() {
        let choice = select(&app("photocraft"), Platform::new(Os::Windows, Arch::Aarch64), &photocraft_assets(), Preference::default()).expect("found");
        assert_eq!(choice.asset, "photocraft-0.3.0-windows-arm64-portable.zip");
        assert!(choice.note.is_none());
    }

    #[test]
    fn windows_arm64_falls_back_to_x64_when_there_is_not() {
        // filmcraft, lightcraft, designcraft, pdfcraft and deckcraft publish no ARM64 asset.
        let choice = select(&app("pdfcraft"), Platform::new(Os::Windows, Arch::Aarch64), &pdfcraft_assets(), Preference::default()).expect("found");
        assert_eq!(choice.asset, "printcraft-0.2.1-windows-x64-portable.zip");
        assert_eq!(choice.note, Some(Note::ArchFallback { wanted: Arch::Aarch64, used: Arch::X86_64 }));
    }

    #[test]
    fn the_printcraft_stem_is_found_through_the_catalogue_row() {
        // The repository is pdfcraft; every published asset says printcraft.
        let choice = select(&app("pdfcraft"), Platform::new(Os::Linux, Arch::X86_64), &pdfcraft_assets(), Preference::default()).expect("found");
        assert_eq!(choice.asset, "printcraft-0.2.1-linux-x86_64.AppImage");
    }

    #[test]
    fn the_preferred_stem_wins_when_a_release_carries_both() {
        let mut assets = pdfcraft_assets();
        assets.push("pdfcraft-0.3.0-linux-x86_64.AppImage".to_owned());
        let choice = select(&app("pdfcraft"), Platform::new(Os::Linux, Arch::X86_64), &assets, Preference::default()).expect("found");
        assert_eq!(choice.asset, "pdfcraft-0.3.0-linux-x86_64.AppImage", "catalogue order puts pdfcraft first");
    }

    #[test]
    fn the_msi_is_refused_by_default_and_named_as_the_reason() {
        let assets = names(&["photocraft-0.3.0-windows-x64.msi"]);
        let err = select(&app("photocraft"), Platform::new(Os::Windows, Arch::X86_64), &assets, Preference::default()).expect_err("elevates");
        assert!(matches!(err, Unavailable::ElevationOnly { .. }), "{err}");
        let allowed = select(&app("photocraft"), Platform::new(Os::Windows, Arch::X86_64), &assets, Preference { allow_elevation: true }).expect("found");
        assert_eq!(allowed.format, Format::Msi);
    }

    #[test]
    fn an_app_with_no_release_is_unavailable_not_a_panic() {
        let err = select(&app("soundcraft"), Platform::new(Os::Linux, Arch::X86_64), &[], Preference::default()).expect_err("no assets");
        assert!(matches!(err, Unavailable::NoAsset { .. }));
    }

    #[test]
    fn freebsd_and_web_assets_are_never_chosen() {
        for platform in Platform::ALL {
            if let Ok(choice) = select(&app("photocraft"), platform, &photocraft_assets(), Preference::default()) {
                assert!(!choice.asset.contains("freebsd"), "{}", choice.asset);
                assert!(!choice.asset.contains("-web-"), "{}", choice.asset);
            }
        }
    }

    #[test]
    fn every_catalogued_app_resolves_on_every_platform_given_the_photocraft_shape() {
        // The release shape is shared, so a full asset list resolves for every app once the stem
        // is substituted. This is the guard against a catalogue row with an unusable stem.
        let catalogue = Catalogue::embedded().expect("parses");
        for app in catalogue.installable() {
            let stem = app.asset_stems.first().map(String::as_str).unwrap_or_default();
            let assets: Vec<String> = photocraft_assets().iter().map(|a| a.replace("photocraft", stem)).collect();
            for platform in Platform::ALL {
                let choice = select(app, platform, &assets, Preference::default());
                assert!(choice.is_ok(), "{} on {}: {:?}", app.slug, platform.label(), choice);
            }
        }
    }

    #[test]
    fn a_version_is_required_between_stem_and_suffix() {
        assert!(is_version("0.3.0"));
        assert!(is_version("1.2.3-rc.1"));
        assert!(!is_version("cli-0.3.0"));
        assert!(!is_version(""));
        assert!(!is_version("web"));
    }

    #[test]
    fn platform_labels_round_trip() {
        for platform in Platform::ALL {
            assert_eq!(Platform::parse(&platform.label()), Some(platform), "{}", platform.label());
        }
        assert_eq!(Platform::parse("plan9-risc"), None);
    }
}
