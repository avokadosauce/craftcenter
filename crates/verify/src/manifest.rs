//! What an install put on disk, file by file, so that checking it later means something.
//!
//! A release's `SHA256SUMS.txt` is a digest of the *asset* that was downloaded. For an AppImage
//! that is also the digest of the installed file, because the install is a byte copy — but a
//! tarball, a portable zip and a DMG are all unpacked, and the bytes on disk are then nothing
//! like the archive they came out of. Re-hashing one installed file against the asset's digest
//! can only ever succeed for the one format where the two are the same thing.
//!
//! So an install records a manifest of its own: every regular file it wrote, with that file's
//! SHA-256 and size, and every symlink with its target. Paths are relative to the installed
//! tree, which is what lets the app be moved to another folder without invalidating the record,
//! and are always spelled with `/` so the file reads the same on every platform.
//!
//! "The installed tree" is the directory the record names, and nothing below it is privileged:
//! an unpacked tarball or portable zip wraps its contents in a directory of its own, so its
//! paths read `photocraft-0.3.0-linux-x86_64/bin/photocraft` rather than `bin/photocraft`. That
//! is deliberate. The manifest covers exactly what the install directory holds, which is what
//! Open folder shows and what a person comparing the two would see, and it needs nothing in the
//! record beyond the directory itself to be checked again later.
//!
//! Be honest about what this proves. The manifest is written by this program, into the user's own
//! directory, in plain JSON — anyone who can rewrite an installed binary can rewrite the manifest
//! beside it. It catches a file that has changed since it was installed: a half-finished update, a
//! corrupted disk, an editor that saved over a library, a background process that replaced one
//! file. It is not a signature, and nothing here claims it is. The release's asset digest stays
//! recorded alongside it, because that is the part that says where the bytes came from.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::{Error, hex, sha256_file};

/// The shape of the file on disk. Bumped if an older CraftCenter could no longer read it; a
/// manifest from a *newer* version is refused rather than half-understood.
pub const MANIFEST_VERSION: u32 = 1;

/// Names that are never recorded and never reported as unexpected.
///
/// All four are written by a file manager or by this program rather than by the app's publisher,
/// and all four come and go on their own. Reporting them as tampering would teach the user that
/// Verify cries wolf, which costs more than the two bytes of honesty it buys.
pub const IGNORED: &[&str] = &[
    ".DS_Store",               // Finder, in any folder it has shown
    "Thumbs.db",               // Windows Explorer
    "desktop.ini",             // Windows Explorer
    ".craftcenter-write-test", // the probe `check_install_dir` writes and removes
];

/// Is this a name from [`IGNORED`], or one of the two families that go with them: `._name`, the
/// other half of an AppleDouble pair, and `name.tmp-new`, a half-written atomic write that a
/// crash left behind?
///
/// Public because a move records what it copies as it copies it, and the two sets of entries
/// have to be drawn up by the same rule or the comparison between them is not one.
pub fn ignorable(name: &str) -> bool {
    IGNORED.contains(&name) || name.starts_with("._") || name.ends_with(".tmp-new")
}

/// One regular file, as it was when it was installed.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct FileEntry {
    /// Relative to the installed tree, `/`-separated.
    pub path: String,
    /// Lowercase hex.
    pub sha256: String,
    pub size: u64,
}

/// One symlink. Recorded by its target and never followed: a macOS bundle is full of links into
/// its own `Versions/`, and following them would record the same bytes several times over and
/// then report a correct bundle as broken.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct LinkEntry {
    pub path: String,
    pub target: String,
}

/// Every file and link of one installed tree.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Manifest {
    pub version: u32,
    /// Sorted by path, so two manifests of the same tree are the same bytes.
    pub files: Vec<FileEntry>,
    #[serde(default)]
    pub links: Vec<LinkEntry>,
}

impl Manifest {
    /// Collect `files` and `links` into a manifest, sorted.
    pub fn new(mut files: Vec<FileEntry>, mut links: Vec<LinkEntry>) -> Self {
        files.sort_by(|a, b| a.path.cmp(&b.path));
        links.sort_by(|a, b| a.path.cmp(&b.path));
        Self { version: MANIFEST_VERSION, files, links }
    }

    /// Walk `root` and hash everything under it.
    ///
    /// Directories are not entries of their own — an empty directory holds nothing that can be
    /// tampered with — and anything that is neither a regular file nor a symlink (a socket, a
    /// device node) is skipped, because an app install contains none and a manifest should not
    /// invite one.
    pub fn of_tree(root: &Path) -> Result<Self, Error> {
        let mut files = Vec::new();
        let mut links = Vec::new();
        walk(root, &mut Vec::new(), &mut files, &mut links)?;
        Ok(Self::new(files, links))
    }

    /// A manifest of exactly one file, keyed by its own name.
    ///
    /// What CraftCenter's own self-update records: the program is a single executable that shares
    /// a directory with files nobody here installed, so its tree cannot be walked.
    pub fn of_file(path: &Path) -> Result<Self, Error> {
        let name = path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
        Ok(Self::new(vec![entry_for(name, path)?], Vec::new()))
    }

    /// How many paths this manifest accounts for.
    pub fn len(&self) -> usize {
        self.files.len() + self.links.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn to_json(&self) -> Result<String, Error> {
        serde_json::to_string_pretty(self).map_err(|source| Error::Manifest { message: source.to_string() })
    }

    /// Read a manifest written by [`Self::to_json`].
    pub fn parse(text: &str) -> Result<Self, Error> {
        let manifest: Self = serde_json::from_str(text).map_err(|source| Error::Manifest { message: source.to_string() })?;
        if manifest.version > MANIFEST_VERSION {
            return Err(Error::ManifestVersion { found: manifest.version });
        }
        Ok(manifest)
    }

    /// Check an installed tree against this manifest.
    pub fn verify_tree(&self, root: &Path) -> Result<Verification, Error> {
        Ok(self.compare(&Self::of_tree(root)?))
    }

    /// Check only the paths this manifest lists, without looking for anything else.
    ///
    /// For a tree CraftCenter does not own in full — the directory CraftCenter's own executable
    /// happens to sit in — where everything else present is somebody else's business.
    pub fn verify_listed(&self, root: &Path) -> Result<Verification, Error> {
        let mut found_files = Vec::new();
        let mut found_links = Vec::new();
        for file in &self.files {
            let path = root.join(file.path.replace('/', std::path::MAIN_SEPARATOR_STR));
            if std::fs::symlink_metadata(&path).is_ok() {
                found_files.push(entry_for(file.path.clone(), &path)?);
            }
        }
        for link in &self.links {
            let path = root.join(link.path.replace('/', std::path::MAIN_SEPARATOR_STR));
            if let Ok(target) = std::fs::read_link(&path) {
                found_links.push(LinkEntry { path: link.path.clone(), target: target.to_string_lossy().into_owned() });
            }
        }
        Ok(self.compare(&Self::new(found_files, found_links)))
    }

    /// Compare this manifest with what is actually there.
    ///
    /// Taken apart from the walk so that a move, which hashes every file it copies anyway, can
    /// check the copy it has just made against the manifest without reading it all a third time.
    pub fn compare(&self, observed: &Self) -> Verification {
        let seen_files: BTreeMap<&str, &FileEntry> = observed.files.iter().map(|file| (file.path.as_str(), file)).collect();
        let seen_links: BTreeMap<&str, &LinkEntry> = observed.links.iter().map(|link| (link.path.as_str(), link)).collect();

        let mut report = Verification { listed: self.len(), ..Verification::default() };
        for file in &self.files {
            match seen_files.get(file.path.as_str()) {
                Some(found) if found.size == file.size && found.sha256.eq_ignore_ascii_case(&file.sha256) => {}
                // A file replaced by a symlink is a change, not an absence.
                Some(_) => report.modified.push(file.path.clone()),
                None if seen_links.contains_key(file.path.as_str()) => report.modified.push(file.path.clone()),
                None => report.missing.push(file.path.clone()),
            }
        }
        for link in &self.links {
            match seen_links.get(link.path.as_str()) {
                Some(found) if found.target == link.target => {}
                Some(_) => report.modified.push(link.path.clone()),
                None if seen_files.contains_key(link.path.as_str()) => report.modified.push(link.path.clone()),
                None => report.missing.push(link.path.clone()),
            }
        }

        let recorded: BTreeMap<&str, ()> = self.files.iter().map(|f| (f.path.as_str(), ())).chain(self.links.iter().map(|l| (l.path.as_str(), ()))).collect();
        for path in seen_files.keys().chain(seen_links.keys()) {
            if !recorded.contains_key(path) {
                report.extra.push((*path).to_owned());
            }
        }
        report.modified.sort();
        report.missing.sort();
        report.extra.sort();
        report
    }
}

/// Hash one file into an entry under `path`.
fn entry_for(path: String, on_disk: &Path) -> Result<FileEntry, Error> {
    let size = std::fs::symlink_metadata(on_disk).map_err(|source| Error::Io { path: on_disk.display().to_string(), source })?.len();
    Ok(FileEntry { path, sha256: hex(&sha256_file(on_disk)?), size })
}

/// `parts` joined with `/`: how every path in a manifest is spelled, whatever the platform
/// separator is, so that a record written on one does not read as a change on another.
fn joined(parts: &[String]) -> String {
    parts.join("/")
}

/// How a manifest would spell `path`, which lies inside the tree rooted at `root`.
///
/// `None` when it does not lie inside it. For callers that already hold an absolute path — a
/// move, which records what it copies as it copies it — rather than walking a tree themselves.
pub fn manifest_path(root: &Path, path: &Path) -> Option<String> {
    let rest = path.strip_prefix(root).ok()?;
    let parts: Vec<String> = rest.components().map(|part| part.as_os_str().to_string_lossy().into_owned()).collect();
    Some(joined(&parts))
}

fn walk(dir: &Path, parts: &mut Vec<String>, files: &mut Vec<FileEntry>, links: &mut Vec<LinkEntry>) -> Result<(), Error> {
    let io = |path: &Path| {
        let path = path.display().to_string();
        move |source| Error::Io { path, source }
    };
    let entries = std::fs::read_dir(dir).map_err(io(dir))?;
    for entry in entries {
        let entry = entry.map_err(io(dir))?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if ignorable(&name) {
            continue;
        }
        let kind = std::fs::symlink_metadata(&path).map_err(io(&path))?.file_type();
        parts.push(name);
        if kind.is_symlink() {
            let target = std::fs::read_link(&path).map_err(io(&path))?;
            links.push(LinkEntry { path: joined(parts), target: target.to_string_lossy().into_owned() });
        } else if kind.is_dir() {
            walk(&path, parts, files, links)?;
        } else if kind.is_file() {
            files.push(entry_for(joined(parts), &path)?);
        }
        parts.pop();
    }
    Ok(())
}

/// What Verify found, in the four states it can be in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    /// Everything recorded is present and unchanged.
    Ok,
    /// At least one recorded file is there and is not what was installed. The worst of the four,
    /// and the one that takes precedence when files are both changed and gone.
    Modified,
    /// Nothing changed, but something recorded is no longer there.
    Incomplete,
    /// There was nothing to check against.
    NotVerifiable,
}

/// The result of checking an installed tree against its manifest.
///
/// Extra files are *reported*, never failed: a `.DS_Store` is not tampering, and neither is a
/// log file an app wrote next to itself. What fails is a recorded file that has changed or gone.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Verification {
    /// How many paths the manifest accounted for.
    pub listed: usize,
    /// Recorded paths whose contents are not what was recorded. Relative, `/`-separated.
    pub modified: Vec<String>,
    /// Recorded paths that are no longer there.
    pub missing: Vec<String>,
    /// Paths present that the manifest does not list, minus the ones in [`IGNORED`].
    pub extra: Vec<String>,
    /// Why there was nothing to check, when there was nothing to check.
    pub not_verifiable: Option<&'static str>,
}

impl Verification {
    /// A verdict of "cannot tell", with the sentence that says why.
    pub fn not_verifiable(reason: &'static str) -> Self {
        Self { not_verifiable: Some(reason), ..Self::default() }
    }

    pub fn level(&self) -> Level {
        if self.not_verifiable.is_some() {
            Level::NotVerifiable
        } else if !self.modified.is_empty() {
            Level::Modified
        } else if !self.missing.is_empty() {
            Level::Incomplete
        } else {
            Level::Ok
        }
    }

    /// True only when the tree is exactly what was installed.
    pub fn is_intact(&self) -> bool {
        self.level() == Level::Ok
    }

    /// One line a person can read, in either front end. The paths themselves are in the three
    /// lists, for a caller with room to show them.
    pub fn summary(&self) -> String {
        if let Some(reason) = self.not_verifiable {
            return reason.to_owned();
        }
        let files = |count: usize| if count == 1 { "1 file".to_owned() } else { format!("{count} files") };
        let mut text = match (self.modified.len(), self.missing.len()) {
            (0, 0) => format!("{} match what was installed", files(self.listed)),
            (0, gone) => format!("{} of {} missing", files(gone), self.listed),
            (changed, 0) => format!("{} of {} changed since it was installed", files(changed), self.listed),
            (changed, gone) => format!("{} of {} changed and {} missing", files(changed), self.listed, files(gone)),
        };
        if !self.extra.is_empty() {
            text.push_str(&format!("; {} present that CraftCenter did not install", files(self.extra.len())));
        }
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A small tree with a nested directory, as every unpacked format produces.
    fn tree() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::create_dir_all(dir.path().join("bin")).expect("create");
        std::fs::create_dir_all(dir.path().join("share/icons")).expect("create");
        std::fs::write(dir.path().join("bin/photocraft"), "the app").expect("write");
        std::fs::write(dir.path().join("share/icons/photocraft.png"), "PNG").expect("write");
        std::fs::write(dir.path().join("README"), "docs").expect("write");
        dir
    }

    #[test]
    fn a_tree_records_every_file_with_a_relative_slash_separated_path() {
        let dir = tree();
        let manifest = Manifest::of_tree(dir.path()).expect("walked");
        let paths: Vec<&str> = manifest.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, ["README", "bin/photocraft", "share/icons/photocraft.png"]);
        assert!(manifest.files.iter().all(|f| !f.path.contains('\\')), "paths are spelled with forward slashes");
        let app = manifest.files.iter().find(|f| f.path == "bin/photocraft").expect("the binary");
        assert_eq!(app.size, 7);
        assert_eq!(app.sha256, hex(&crate::sha256_reader(&b"the app"[..]).expect("hash")));
    }

    #[test]
    fn an_unchanged_tree_verifies() {
        let dir = tree();
        let manifest = Manifest::of_tree(dir.path()).expect("walked");
        let report = manifest.verify_tree(dir.path()).expect("verified");
        assert_eq!(report.level(), Level::Ok);
        assert_eq!(report.listed, 3);
        assert!(report.summary().contains("3 files match"), "{}", report.summary());
    }

    #[test]
    fn one_changed_byte_is_caught_and_the_path_is_named() {
        let dir = tree();
        let manifest = Manifest::of_tree(dir.path()).expect("walked");
        std::fs::write(dir.path().join("bin/photocraft"), "the bpp").expect("tamper");
        let report = manifest.verify_tree(dir.path()).expect("verified");
        assert_eq!(report.level(), Level::Modified);
        assert_eq!(report.modified, ["bin/photocraft"]);
        assert!(report.missing.is_empty());
    }

    /// Same digest, different length, is still the same question: a truncation is a change.
    #[test]
    fn a_truncated_file_is_caught() {
        let dir = tree();
        let manifest = Manifest::of_tree(dir.path()).expect("walked");
        std::fs::write(dir.path().join("README"), "").expect("truncate");
        assert_eq!(manifest.verify_tree(dir.path()).expect("verified").modified, ["README"]);
    }

    #[test]
    fn a_missing_file_is_incomplete_rather_than_modified() {
        let dir = tree();
        let manifest = Manifest::of_tree(dir.path()).expect("walked");
        std::fs::remove_file(dir.path().join("share/icons/photocraft.png")).expect("remove");
        let report = manifest.verify_tree(dir.path()).expect("verified");
        assert_eq!(report.level(), Level::Incomplete);
        assert_eq!(report.missing, ["share/icons/photocraft.png"]);
        assert!(report.summary().contains("missing"), "{}", report.summary());
    }

    /// Both at once reads as the worse of the two, and says so in one line.
    #[test]
    fn a_tree_both_changed_and_short_reports_as_modified() {
        let dir = tree();
        let manifest = Manifest::of_tree(dir.path()).expect("walked");
        std::fs::write(dir.path().join("README"), "edited").expect("tamper");
        std::fs::remove_file(dir.path().join("bin/photocraft")).expect("remove");
        let report = manifest.verify_tree(dir.path()).expect("verified");
        assert_eq!(report.level(), Level::Modified);
        assert_eq!(report.modified, ["README"]);
        assert_eq!(report.missing, ["bin/photocraft"]);
        assert!(report.summary().contains("changed and 1 file missing"), "{}", report.summary());
    }

    #[test]
    fn a_file_the_install_did_not_write_is_reported_and_does_not_fail() {
        let dir = tree();
        let manifest = Manifest::of_tree(dir.path()).expect("walked");
        std::fs::write(dir.path().join("photocraft.log"), "yesterday").expect("write");
        let report = manifest.verify_tree(dir.path()).expect("verified");
        assert_eq!(report.level(), Level::Ok, "an extra file is not tampering");
        assert_eq!(report.extra, ["photocraft.log"]);
        assert!(report.summary().contains("did not install"), "{}", report.summary());
    }

    #[test]
    fn the_noise_a_file_manager_leaves_is_neither_recorded_nor_reported() {
        let dir = tree();
        for noise in [".DS_Store", "Thumbs.db", "desktop.ini", "._photocraft", "bin.tmp-new", ".craftcenter-write-test"] {
            std::fs::write(dir.path().join(noise), "noise").expect("write");
        }
        let manifest = Manifest::of_tree(dir.path()).expect("walked");
        assert_eq!(manifest.len(), 3, "only the three real files are recorded");
        let report = manifest.verify_tree(dir.path()).expect("verified");
        assert_eq!(report.level(), Level::Ok);
        assert!(report.extra.is_empty(), "{:?} should have been ignored", report.extra);
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_is_recorded_by_its_target_and_not_followed() {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::create_dir_all(dir.path().join("Versions/A")).expect("create");
        std::fs::write(dir.path().join("Versions/A/PhotoCraft"), "the app").expect("write");
        std::os::unix::fs::symlink("Versions/A", dir.path().join("Current")).expect("link");

        let manifest = Manifest::of_tree(dir.path()).expect("walked");
        assert_eq!(manifest.files.len(), 1, "the link is not a second copy of the binary");
        assert_eq!(manifest.links, [LinkEntry { path: "Current".to_owned(), target: "Versions/A".to_owned() }]);
        assert_eq!(manifest.verify_tree(dir.path()).expect("verified").level(), Level::Ok);
    }

    #[cfg(unix)]
    #[test]
    fn a_relinked_symlink_is_a_modification() {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::create_dir_all(dir.path().join("Versions/A")).expect("create");
        std::fs::create_dir_all(dir.path().join("Versions/B")).expect("create");
        std::os::unix::fs::symlink("Versions/A", dir.path().join("Current")).expect("link");
        let manifest = Manifest::of_tree(dir.path()).expect("walked");

        std::fs::remove_file(dir.path().join("Current")).expect("unlink");
        std::os::unix::fs::symlink("Versions/B", dir.path().join("Current")).expect("relink");
        let report = manifest.verify_tree(dir.path()).expect("verified");
        assert_eq!(report.modified, ["Current"], "a link pointing somewhere new is a change");
    }

    #[cfg(unix)]
    #[test]
    fn a_file_swapped_for_a_symlink_is_a_modification_not_an_absence() {
        let dir = tree();
        let manifest = Manifest::of_tree(dir.path()).expect("walked");
        std::fs::remove_file(dir.path().join("bin/photocraft")).expect("remove");
        std::os::unix::fs::symlink("/bin/sh", dir.path().join("bin/photocraft")).expect("link");
        let report = manifest.verify_tree(dir.path()).expect("verified");
        assert_eq!(report.modified, ["bin/photocraft"]);
        assert!(report.missing.is_empty(), "the path is still occupied, so it is not missing");
    }

    #[test]
    fn a_manifest_round_trips_through_json() {
        let dir = tree();
        let manifest = Manifest::of_tree(dir.path()).expect("walked");
        let text = manifest.to_json().expect("serialised");
        assert_eq!(Manifest::parse(&text).expect("parsed"), manifest);
        // Deterministic: the same tree is the same bytes, so a diff of two manifests is readable.
        assert_eq!(Manifest::of_tree(dir.path()).expect("walked").to_json().expect("serialised"), text);
    }

    #[test]
    fn a_manifest_from_a_newer_craftcenter_is_refused_rather_than_half_understood() {
        let text = format!("{{\"version\":{},\"files\":[],\"links\":[]}}", MANIFEST_VERSION + 1);
        assert!(matches!(Manifest::parse(&text), Err(Error::ManifestVersion { .. })));
    }

    #[test]
    fn a_manifest_that_is_not_json_is_an_error_not_a_panic() {
        assert!(matches!(Manifest::parse("{{{ not json"), Err(Error::Manifest { .. })));
        // An older manifest with no links at all still reads.
        let text = "{\"version\":1,\"files\":[]}";
        assert_eq!(Manifest::parse(text).expect("parsed").links, []);
    }

    #[test]
    fn one_file_can_be_a_manifest_of_its_own() {
        let dir = tempfile::tempdir().expect("temp dir");
        let exe = dir.path().join("craftcenter");
        std::fs::write(&exe, "the program").expect("write");
        let manifest = Manifest::of_file(&exe).expect("hashed");
        assert_eq!(manifest.files.len(), 1);
        assert_eq!(manifest.files.first().map(|f| f.path.as_str()), Some("craftcenter"));

        // Everything else in that directory is somebody else's, so it is not looked at.
        std::fs::write(dir.path().join("unrelated"), "not ours").expect("write");
        let report = manifest.verify_listed(dir.path()).expect("verified");
        assert_eq!(report.level(), Level::Ok);
        assert!(report.extra.is_empty(), "a listed-only check does not go looking for extras");

        std::fs::write(&exe, "tampered").expect("tamper");
        assert_eq!(manifest.verify_listed(dir.path()).expect("verified").modified, ["craftcenter"]);
    }

    #[test]
    fn a_listed_only_check_still_notices_the_file_is_gone() {
        let dir = tempfile::tempdir().expect("temp dir");
        let exe = dir.path().join("craftcenter");
        std::fs::write(&exe, "the program").expect("write");
        let manifest = Manifest::of_file(&exe).expect("hashed");
        std::fs::remove_file(&exe).expect("remove");
        assert_eq!(manifest.verify_listed(dir.path()).expect("verified").level(), Level::Incomplete);
    }

    #[test]
    fn not_verifiable_carries_the_reason_and_is_not_intact() {
        let report = Verification::not_verifiable("installed before CraftCenter recorded manifests");
        assert_eq!(report.level(), Level::NotVerifiable);
        assert!(!report.is_intact());
        assert_eq!(report.summary(), "installed before CraftCenter recorded manifests");
    }

    #[test]
    fn a_path_inside_a_tree_is_spelled_the_way_the_manifest_spells_it() {
        let root = Path::new("one").join("photocraft");
        assert_eq!(manifest_path(&root, &root.join("0.3.0").join("bin").join("photocraft")).as_deref(), Some("0.3.0/bin/photocraft"));
        // The root of the tree itself, and something that is not inside it at all.
        assert_eq!(manifest_path(&root, &root).as_deref(), Some(""));
        assert_eq!(manifest_path(&root, Path::new("elsewhere")), None);
    }

    #[test]
    fn a_tree_that_is_not_there_is_an_error_a_caller_can_report() {
        let err = Manifest::of_tree(Path::new("/definitely/not/here"));
        assert!(matches!(err, Err(Error::Io { .. })));
    }
}
