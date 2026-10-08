//! Installing an app for the person running CraftCenter, and nobody else.
//!
//! Three rules shape everything here.
//!
//! **Never elevate.** Every path writes only inside the user's own directories. The upstream's
//! `.msi` is `Scope="perMachine"` and always asks for administrator rights, so CraftCenter does
//! not use it; their portable zip, AppImage, tarball and DMG all install without a prompt.
//!
//! **Never write over something that might be running.** A new version is unpacked into its own
//! versioned directory and a stable pointer is flipped with an atomic `rename`, so an interrupted
//! install leaves either the old version or the new one, never a half-replaced binary.
//!
//! **Treat archive contents as hostile.** Entry names come off the network; one that would escape
//! the destination directory aborts the install instead of being written.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable, clippy::indexing_slicing)]

pub mod fs;
mod paths;
mod program;
mod state;

use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use craftcenter_catalogue::App;
use craftcenter_select::{Choice, Format};
use craftcenter_verify::manifest::{FileEntry, LinkEntry, Manifest, Verification, ignorable, manifest_path};

pub use paths::Paths;
pub use program::check_is_program;
pub use state::{Installed, State};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{path}: {source}")]
    Io { path: String, source: io::Error },
    #[error("cannot find the user's {what}")]
    NoHome { what: &'static str },
    #[error("{archive}: entry {entry:?} would be written outside the install directory")]
    UnsafeEntry { archive: String, entry: String },
    #[error("{archive}: {message}")]
    Archive { archive: String, message: String },
    #[error("{path}: {message}")]
    State { path: String, message: String },
    #[error("installing {app} from a {format} needs administrator rights, which CraftCenter never asks for")]
    NeedsElevation { app: String, format: &'static str },
    #[error("{asset}: no {what} inside the downloaded archive")]
    NotInArchive { asset: String, what: String },
    #[error("{app} is not installed")]
    NotInstalled { app: String },
    #[error("{command} failed: {message}")]
    Command { command: String, message: String },
    #[error("{path}: {reason}")]
    UnusableLocation { path: String, reason: &'static str },
    #[error("{app} is running; close it and move it again")]
    Running { app: String },
    #[error("{path}: the copy does not match the original, so nothing was moved")]
    CopyMismatch { path: String },
    #[error("{path} already exists; nothing was moved")]
    DestinationExists { path: String },
    #[error("{path}: {found}, not a program this machine can run; nothing was replaced")]
    NotAProgram { path: String, found: &'static str },
    #[error("{app} on disk is not the install that was recorded ({what}); nothing was moved")]
    NotAsInstalled { app: String, what: String },
    #[error(transparent)]
    Verify(#[from] craftcenter_verify::Error),
}

/// Bytes done, and the total when one is known. The same shape the download reporter uses, so a
/// front end draws a move with the bar it already has.
pub type Progress<'a> = &'a mut dyn FnMut(u64, Option<u64>);

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Everything an install needs, once the asset has been downloaded and verified.
pub struct Request<'a> {
    pub app: &'a App,
    /// The version as the asset filename spells it; also the directory name.
    pub version: &'a str,
    pub tag: &'a str,
    pub choice: &'a Choice,
    /// The downloaded asset, already checked against the release's `SHA256SUMS.txt`.
    pub archive: &'a Path,
    /// Its verified digest, in hex, recorded so `verify` can re-check later.
    pub sha256: &'a str,
    /// The app's icon, for the Linux desktop entry. Optional: a missing icon costs the entry its
    /// picture and nothing else.
    pub icon_png: Option<&'a [u8]>,
}

/// Install `request`, replacing any version already installed.
///
/// The record is written last, so a failure part-way leaves CraftCenter believing the previous
/// version is still the installed one — which it is.
pub fn install(paths: &Paths, request: &Request<'_>) -> Result<Installed, Error> {
    paths.create()?;
    let previous = State::load(&paths.state)?.get(&request.app.slug).cloned();
    // An app keeps the home it was first installed into. That is what lets the install location
    // be changed without stranding anything: an update goes back where the app already is, and
    // only an app being installed for the first time goes to the new place.
    let home = previous.as_ref().and_then(home_of).unwrap_or_else(|| paths.app_dir(&request.app.slug));

    let installed = match request.choice.format {
        Format::AppImage => install_appimage(paths, request, &home)?,
        Format::TarGz => install_tarball(paths, request, &home)?,
        Format::Dmg => install_dmg(paths, request, previous.as_ref())?,
        Format::PortableZip => install_portable_zip(paths, request, &home)?,
        Format::Msi => {
            return Err(Error::NeedsElevation { app: request.app.name.clone(), format: "per-machine installer" });
        }
        Format::CliZip => {
            return Err(Error::NotInArchive { asset: request.choice.asset.clone(), what: "application (this is the headless CLI)".to_owned() });
        }
    };

    // Everything that went on disk, file by file. Written before the state entry that names it,
    // so an interruption between the two leaves a manifest nothing points at — which the next
    // install overwrites — rather than a record pointing at a manifest that is not there.
    let manifest = write_manifest(paths, &request.app.slug, Path::new(&installed.dir))?;

    // Keep the version that was there until the new one has been launched once.
    let installed = Installed { previous: previous.as_ref().map(|p| p.dir.clone()).filter(|d| d != &installed.dir), manifest: Some(manifest), ..installed };

    let mut state = State::load(&paths.state)?;
    state.installed.insert(request.app.slug.clone(), installed.clone());
    state.save(&paths.state)?;
    Ok(installed)
}

/// The directory a recorded install owns, as the record itself says. `None` for a record
/// written before the install location could be chosen and for a macOS bundle, neither of which
/// has a per-app directory of versions.
fn home_of(installed: &Installed) -> Option<PathBuf> {
    if let Some(root) = &installed.root {
        return Some(PathBuf::from(root));
    }
    if installed.format == Format::Dmg {
        return None;
    }
    // `dir` is `<home>/<version>` in every versioned layout, so the home is its parent.
    Path::new(&installed.dir).parent().map(Path::to_path_buf)
}

fn record(request: &Request<'_>, root: &Path, dir: &Path, launcher: &Path) -> Installed {
    Installed {
        root: Some(root.display().to_string()),
        // Filled in by `install` once the tree it describes is complete.
        manifest: None,
        version: request.version.to_owned(),
        tag: request.tag.to_owned(),
        asset: request.choice.asset.clone(),
        sha256: request.sha256.to_owned(),
        format: request.choice.format,
        dir: dir.display().to_string(),
        launcher: launcher.display().to_string(),
        installed_at: now_secs(),
        previous: None,
    }
}

/// An AppImage is one executable file. Its own `AppRun` installs the app's desktop entry and
/// icons into `$XDG_DATA_HOME` on first run and rewrites `Exec=` to wherever the file sits, so
/// CraftCenter writes the same entry eagerly — the app appears in the launcher before it is first
/// started, and `AppRun` finds the entry identical and leaves it alone.
fn install_appimage(paths: &Paths, request: &Request<'_>, home: &Path) -> Result<Installed, Error> {
    let dir = home.join(request.version);
    fs::mkdir_p(&dir)?;
    let target = dir.join(format!("{}.AppImage", request.app.app_id));
    std::fs::copy(request.archive, &target).map_err(fs::io_err(&target))?;
    fs::make_executable(&target)?;

    let launcher = paths.bin.join(&request.app.binary);
    fs::flip_symlink(&launcher, &target)?;
    write_desktop_integration(paths, request, &target)?;
    Ok(record(request, home, &dir, &launcher))
}

/// The tarball is a plain FHS tree (`bin/`, `share/…`) under one top-level directory, so it
/// relocates into the user's data directory unchanged.
fn install_tarball(paths: &Paths, request: &Request<'_>, home: &Path) -> Result<Installed, Error> {
    let dir = home.join(request.version);
    let _ = std::fs::remove_dir_all(&dir);
    fs::mkdir_p(&dir)?;
    fs::unpack_tar_gz(request.archive, &dir)?;

    let root = single_child_dir(&dir)?.unwrap_or(dir.clone());
    let binary = root.join("bin").join(&request.app.binary);
    if !binary.is_file() {
        return Err(Error::NotInArchive { asset: request.choice.asset.clone(), what: format!("bin/{}", request.app.binary) });
    }
    fs::make_executable(&binary)?;
    if let Some(cli) = &request.app.cli {
        let cli_path = root.join("bin").join(cli);
        if cli_path.is_file() {
            fs::make_executable(&cli_path)?;
            fs::flip_symlink(&paths.bin.join(cli), &cli_path)?;
        }
    }

    let launcher = paths.bin.join(&request.app.binary);
    fs::flip_symlink(&launcher, &binary)?;
    write_desktop_integration(paths, request, &binary)?;
    Ok(record(request, home, &dir, &launcher))
}

/// A DMG holds `<App>.app`, notarised and stapled. Mounted read-only and without a Finder window,
/// the bundle is copied into the user's own `~/Applications` — no administrator prompt, and no
/// quarantine attribute, because the bytes were written by this program rather than handed to
/// LaunchServices by a browser.
fn install_dmg(paths: &Paths, request: &Request<'_>, previous: Option<&Installed>) -> Result<Installed, Error> {
    let mount = paths.cache.join("mnt").join(&request.app.slug);
    with_mounted_bundle(request.archive, &mount, &request.choice.asset, |bundle| {
        let name = bundle.file_name().ok_or_else(|| Error::NotInArchive { asset: request.choice.asset.clone(), what: "named bundle".to_owned() })?;
        // A bundle that is already installed is replaced where it stands, wherever that is.
        let destination = previous.and_then(|installed| installed.root.clone()).map(PathBuf::from).unwrap_or_else(|| paths.apps.join(name));
        let home = destination.parent().unwrap_or(&paths.apps).to_path_buf();
        fs::mkdir_p(&home)?;
        let staged = stage_bundle(bundle, &destination)?;
        let retired = swap_tree(&destination, &staged)?;
        // An app's own previous version is kept as a version directory and recorded in the state
        // file, so there is nothing to gain from holding this hidden copy back as well.
        let _ = std::fs::remove_dir_all(&retired);
        Ok(record(request, &destination, &destination, &destination))
    })
}

/// Mount `archive` read-only, hand the `.app` inside it to `with`, and detach whatever happens.
///
/// `-nobrowse -noautoopen` keeps the Finder out of it and the mount point is CraftCenter's own,
/// so nothing is left on the desktop for the user to eject. Both an app install and a
/// self-update need the mounted bundle, and neither should own the mounting.
fn with_mounted_bundle<T>(archive: &Path, mount: &Path, asset: &str, with: impl FnOnce(&Path) -> Result<T, Error>) -> Result<T, Error> {
    let _ = std::fs::remove_dir_all(mount);
    fs::mkdir_p(mount)?;
    run("hdiutil", &["attach", "-nobrowse", "-readonly", "-noautoopen", "-mountpoint", &mount.display().to_string(), &archive.display().to_string()])?;

    let result = (|| {
        let bundle = first_bundle(mount)?.ok_or_else(|| Error::NotInArchive { asset: asset.to_owned(), what: "application bundle (.app)".to_owned() })?;
        with(&bundle)
    })();

    let _ = run("hdiutil", &["detach", &mount.display().to_string()]);
    let _ = std::fs::remove_dir_all(mount);
    result
}

/// The portable zip unpacks to a directory and needs no installer, which is exactly why
/// CraftCenter prefers it to the `.msi`.
fn install_portable_zip(paths: &Paths, request: &Request<'_>, home: &Path) -> Result<Installed, Error> {
    let dir = home.join(request.version);
    let _ = std::fs::remove_dir_all(&dir);
    fs::mkdir_p(&dir)?;
    fs::unpack_zip(request.archive, &dir)?;

    let root = single_child_dir(&dir)?.unwrap_or(dir.clone());
    let exe = root.join(format!("{}.exe", request.app.binary));
    let exe = if exe.is_file() { exe } else { root.join(&request.app.binary) };
    if !exe.is_file() {
        return Err(Error::NotInArchive { asset: request.choice.asset.clone(), what: format!("{}.exe", request.app.binary) });
    }
    fs::make_executable(&exe)?;
    if cfg!(target_os = "windows") {
        write_start_menu_shortcut(paths, &request.app.name, &exe);
    }
    Ok(record(request, home, &dir, &exe))
}

/// When an archive unpacked to exactly one directory, that directory is the real root.
fn single_child_dir(dir: &Path) -> Result<Option<PathBuf>, Error> {
    let mut found = None;
    for entry in std::fs::read_dir(dir).map_err(fs::io_err(dir))? {
        let entry = entry.map_err(fs::io_err(dir))?;
        if found.is_some() {
            return Ok(None);
        }
        if entry.path().is_dir() {
            found = Some(entry.path());
        } else {
            return Ok(None);
        }
    }
    Ok(found)
}

fn first_bundle(dir: &Path) -> Result<Option<PathBuf>, Error> {
    for entry in std::fs::read_dir(dir).map_err(fs::io_err(dir))? {
        let path = entry.map_err(fs::io_err(dir))?.path();
        if path.extension().is_some_and(|e| e == "app") {
            return Ok(Some(path));
        }
    }
    Ok(None)
}

fn copy_tree(from: &Path, to: &Path) -> Result<(), Error> {
    if from.is_dir() {
        fs::mkdir_p(to)?;
        for entry in std::fs::read_dir(from).map_err(fs::io_err(from))? {
            let entry = entry.map_err(fs::io_err(from))?;
            copy_tree(&entry.path(), &to.join(entry.file_name()))?;
        }
        return Ok(());
    }
    if let Some(parent) = to.parent() {
        fs::mkdir_p(parent)?;
    }
    std::fs::copy(from, to).map_err(fs::io_err(to))?;
    Ok(())
}

/// A path safe to put after `Exec=` in a desktop entry. The desktop file format gives `"`, `` ` ``,
/// `$`, `\` and `%` meaning, and a newline would add a line, so a path containing any of them is
/// not written rather than written wrongly — the same decision the upstream's own `AppRun` makes.
fn exec_safe(path: &Path) -> Option<&str> {
    let text = path.to_str()?;
    if text.contains(['"', '`', '$', '\\', '%', '\n', '\r']) { None } else { Some(text) }
}

/// Write the `.desktop` entry and icon so the app appears in the launcher. Linux only, and
/// best-effort: a failure here never fails the install, because the app itself is installed and
/// runnable either way.
fn write_desktop_integration(paths: &Paths, request: &Request<'_>, target: &Path) -> Result<(), Error> {
    write_desktop_entry(paths, request.app, target);
    if let Some(png) = request.icon_png {
        let icon = paths.data.join("icons/hicolor/64x64/apps").join(format!("{}.png", request.app.app_id));
        let _ = fs::write_atomic(&icon, png);
    }
    Ok(())
}

/// The `.desktop` entry alone, pointed at `target`. Written on install and rewritten on a move,
/// because an `Exec=` naming a directory that no longer exists is worse than no entry at all.
fn write_desktop_entry(paths: &Paths, app: &App, target: &Path) {
    if !cfg!(target_os = "linux") {
        return;
    }
    let Some(exec) = exec_safe(target) else {
        return;
    };
    let entry = format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name={name}\n\
         Comment={tagline}\n\
         Exec=\"{exec}\" %F\n\
         Icon={app_id}\n\
         Terminal=false\n\
         StartupNotify=true\n\
         StartupWMClass={binary}\n\
         Categories=Graphics;AudioVideo;Office;\n\
         X-CraftCenter-Installed=true\n",
        name = app.name,
        tagline = app.tagline,
        app_id = app.app_id,
        binary = app.binary,
    );
    let desktop = paths.data.join("applications").join(format!("{}.desktop", app.app_id));
    let _ = fs::write_atomic(&desktop, entry.as_bytes());
}

/// Windows has no `.desktop` file; the Start Menu entry is a shortcut, which the shell creates
/// through PowerShell rather than through COM interop this workspace is not allowed to write.
/// Best-effort: without the shortcut the app is still installed and launchable.
fn write_start_menu_shortcut(paths: &Paths, app_name: &str, exe: &Path) {
    let folder = paths.data.join("CraftCenter");
    if fs::mkdir_p(&folder).is_err() {
        return;
    }
    let link = folder.join(format!("{app_name}.lnk"));
    let (Some(link), Some(exe), Some(dir)) = (link.to_str(), exe.to_str(), exe.parent().and_then(Path::to_str)) else {
        return;
    };
    if [link, exe, dir].iter().any(|p| p.contains(['"', '`', '$', '\n', '\r'])) {
        return;
    }
    let script = format!(
        "$s = (New-Object -ComObject WScript.Shell).CreateShortcut(\"{link}\"); \
         $s.TargetPath = \"{exe}\"; $s.WorkingDirectory = \"{dir}\"; $s.Save()"
    );
    let _ = run("powershell", &["-NoProfile", "-NonInteractive", "-Command", &script]);
}

fn run(program: &str, args: &[&str]) -> Result<(), Error> {
    let output = std::process::Command::new(program)
        .args(args)
        .output()
        .map_err(|source| Error::Command { command: program.to_owned(), message: source.to_string() })?;
    if output.status.success() {
        return Ok(());
    }
    let message = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    Err(Error::Command { command: program.to_owned(), message: if message.is_empty() { format!("exited with {}", output.status) } else { message } })
}

/// Start an installed app, detached, so CraftCenter can be closed without taking it down.
pub fn launch(paths: &Paths, app: &App) -> Result<(), Error> {
    let state = State::load(&paths.state)?;
    let installed = state.get(&app.slug).ok_or_else(|| Error::NotInstalled { app: app.name.clone() })?;
    let launcher = PathBuf::from(&installed.launcher);

    if cfg!(target_os = "macos") && launcher.extension().is_some_and(|e| e == "app") {
        return run("open", &["-a", &launcher.display().to_string()]);
    }
    std::process::Command::new(&launcher).spawn().map(|_| ()).map_err(|source| Error::Io { path: installed.launcher.clone(), source })
}

/// Hand a folder to the system's file manager, or a URL to the user's browser.
///
/// The program is started with the target as a single argument and no shell in between, so a
/// path out of the state file cannot become a command. Nothing is waited for: `xdg-open` stays
/// alive as long as the window it opened, and a failure after the handler has been started is
/// the handler's to report, not CraftCenter's.
pub fn open_externally(target: &str) -> Result<(), Error> {
    let program = if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(target_os = "windows") {
        // `explorer` takes both a folder and a URL, and unlike `cmd /c start` it is not a shell.
        "explorer"
    } else {
        "xdg-open"
    };
    std::process::Command::new(program)
        .arg(target)
        .spawn()
        .map(|_| ())
        .map_err(|source| Error::Command { command: program.to_owned(), message: source.to_string() })
}

/// Remove an app: every version directory, the launcher, and the desktop entry and icon this
/// program wrote. Anything the app itself created in the user's home is left alone.
pub fn remove(paths: &Paths, app: &App) -> Result<(), Error> {
    let mut state = State::load(&paths.state)?;
    let installed = state.installed.remove(&app.slug).ok_or_else(|| Error::NotInstalled { app: app.name.clone() })?;

    let _ = std::fs::remove_dir_all(app_home(paths, app, &installed));
    if let Some(previous) = &installed.previous {
        let _ = std::fs::remove_dir_all(previous);
    }
    let _ = std::fs::remove_file(paths.bin.join(&app.binary));
    if let Some(cli) = &app.cli {
        let _ = std::fs::remove_file(paths.bin.join(cli));
    }
    let _ = std::fs::remove_file(paths.data.join("applications").join(format!("{}.desktop", app.app_id)));
    let _ = std::fs::remove_file(paths.data.join("icons/hicolor/64x64/apps").join(format!("{}.png", app.app_id)));
    let _ = std::fs::remove_file(paths.data.join("CraftCenter").join(format!("{}.lnk", app.name)));
    let _ = std::fs::remove_file(paths.manifest(&app.slug));

    state.save(&paths.state)
}

/// Everything this app owns on disk: the home the record names, or — for a record written
/// before the install location could be chosen — the one today's layout implies.
pub fn app_home(paths: &Paths, app: &App, installed: &Installed) -> PathBuf {
    home_of(installed).unwrap_or_else(|| {
        // A bundle is its own root; anything else keeps its versions in a directory per app.
        if installed.format == Format::Dmg { PathBuf::from(&installed.dir) } else { paths.app_dir(&app.slug) }
    })
}

/// Record every file of an installed tree, beside the state entry that will name it.
///
/// Returns the digest of the file that was written, which goes into that entry.
fn write_manifest(paths: &Paths, slug: &str, dir: &Path) -> Result<String, Error> {
    let text = Manifest::of_tree(dir)?.to_json()?;
    fs::write_atomic(&paths.manifest(slug), text.as_bytes())?;
    digest_of(&text)
}

/// The digest of a record's own bytes.
fn digest_of(text: &str) -> Result<String, Error> {
    let digest = craftcenter_verify::sha256_reader(text.as_bytes()).map_err(|source| Error::Io { path: "the file record".to_owned(), source })?;
    Ok(craftcenter_verify::hex(&digest))
}

/// The three ways there can be nothing to check an install against. They are printed to the
/// person who asked, so each says what to do about it and not only what is wrong.
const INSTALLED_BEFORE: &str = "installed before CraftCenter recorded a digest for every file; reinstall it to enable a full check";
const RECORD_GONE: &str = "the record of this install's files is missing, so there is nothing to check against; reinstall it to write a new one";
const RECORD_REPLACED: &str = "the record of this install's files is not the one written when it was installed; reinstall it to write a new one";
const NO_SELF_RECORD: &str =
    "this build has not recorded its own files, so there is nothing to check against; it was not installed by CraftCenter's own update";

/// What there is to check an install against.
enum Recorded {
    Manifest(Manifest),
    /// Nothing, and the sentence that says why.
    Nothing(&'static str),
}

/// Read the manifest a record names — and only the one it names.
///
/// The digest in the record is not a signature: both files are the user's own, and a hand that
/// can rewrite the tree can rewrite both. What it does separate is an install made before
/// CraftCenter recorded manifests, which cannot be checked, from one whose manifest has since
/// been removed or swapped, which is worth saying out loud.
fn recorded(paths: &Paths, slug: &str, installed: &Installed) -> Result<Recorded, Error> {
    let Some(expected) = &installed.manifest else {
        return Ok(Recorded::Nothing(INSTALLED_BEFORE));
    };
    let path = paths.manifest(slug);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Recorded::Nothing(RECORD_GONE)),
        Err(source) => return Err(Error::Io { path: path.display().to_string(), source }),
    };
    if !digest_of(&text)?.eq_ignore_ascii_case(expected) {
        return Ok(Recorded::Nothing(RECORD_REPLACED));
    }
    Ok(Recorded::Manifest(Manifest::parse(&text)?))
}

/// Check what is on disk against the record of what was installed.
///
/// This is what Verify means. The digest of the downloaded asset stays in the record and stays
/// the answer to "where did these bytes come from", but for three of the four formats it is the
/// digest of an *archive* — and the question a person asks of an installed app is whether what is
/// on disk is still what was put there.
pub fn verify_installed(paths: &Paths, app: &App, installed: &Installed) -> Result<Verification, Error> {
    let manifest = match recorded(paths, &app.slug, installed)? {
        Recorded::Nothing(reason) => return Ok(Verification::not_verifiable(reason)),
        Recorded::Manifest(manifest) => manifest,
    };
    let dir = Path::new(&installed.dir);
    if !dir.exists() {
        // The whole tree is gone. That is an answer — every recorded file is missing — rather
        // than a failure to go and look.
        return Ok(manifest.compare(&Manifest::new(Vec::new(), Vec::new())));
    }
    Ok(manifest.verify_tree(dir)?)
}

/// Record CraftCenter's own executable, after a self-update has put a new one in place.
///
/// The same question asked of the installer as of anything it installs, with two differences.
/// The program is one file in a directory full of files nobody here installed, so the record
/// names that file and the check does not go looking for anything else. And there is no state
/// entry to carry the record's digest, so a build with no record reads as one that did not come
/// from CraftCenter's own update — which is what a packaged build is — rather than as tampering.
pub fn record_self(paths: &Paths, slug: &str, exe: &Path) -> Result<(), Error> {
    let text = Manifest::of_file(exe)?.to_json()?;
    fs::write_atomic(&paths.manifest(slug), text.as_bytes())
}

/// Check CraftCenter's own executable against what [`record_self`] wrote.
pub fn verify_self(paths: &Paths, slug: &str, exe: &Path) -> Result<Verification, Error> {
    let path = paths.manifest(slug);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Verification::not_verifiable(NO_SELF_RECORD)),
        Err(source) => return Err(Error::Io { path: path.display().to_string(), source }),
    };
    let Some(dir) = exe.parent() else {
        return Ok(Verification::not_verifiable(NO_SELF_RECORD));
    };
    Ok(Manifest::parse(&text)?.verify_listed(dir)?)
}

/// Can apps be installed into this directory?
///
/// The check ends in a real write, because a permission bit is not the whole story on any of the
/// three platforms: a read-only mount, a full disk and a Windows ACL all pass a `metadata()`
/// test and then fail the first install. Per-user is a ruling, not a default — CraftCenter never
/// elevates — so a location this user cannot write to is refused in a plain sentence instead of
/// being retried with a prompt.
pub fn check_install_dir(dir: &Path) -> Result<(), Error> {
    let unusable = |reason: &'static str| Error::UnusableLocation { path: dir.display().to_string(), reason };
    if dir.as_os_str().is_empty() {
        return Err(unusable("that is not a folder"));
    }
    if dir.exists() && !dir.is_dir() {
        return Err(unusable("that is a file, not a folder"));
    }
    if let Some(program) = std::env::current_exe().ok().and_then(|exe| exe.parent().map(Path::to_path_buf))
        && dir.starts_with(&program)
    {
        return Err(unusable("that is inside CraftCenter's own folder; choose somewhere else, so updating CraftCenter cannot disturb the apps it installed"));
    }
    fs::mkdir_p(dir).map_err(|_| unusable("that folder does not exist and cannot be created without administrator rights"))?;

    let probe = dir.join(".craftcenter-write-test");
    std::fs::write(&probe, b"").map_err(|_| unusable("that folder cannot be written to without administrator rights, and CraftCenter never asks for them"))?;
    let _ = std::fs::remove_file(&probe);
    Ok(())
}

/// Does this install look like it is running right now?
///
/// Each platform has to be asked in its own way and none of the answers is airtight — a process
/// can start a moment after the question. It is here to catch the ordinary mistake of moving an
/// app while using it; [`move_app`] is written so that a missed detection still costs nothing,
/// because nothing is deleted until the copy has been made and read back.
pub fn appears_to_be_running(root: &Path, launcher: &Path) -> bool {
    running(root, launcher)
}

#[cfg(target_os = "linux")]
fn running(root: &Path, _launcher: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return false;
    };
    entries.flatten().any(|entry| {
        let is_pid = entry.file_name().to_str().is_some_and(|name| !name.is_empty() && name.bytes().all(|byte| byte.is_ascii_digit()));
        is_pid && std::fs::read_link(entry.path().join("exe")).is_ok_and(|exe| exe.starts_with(root))
    })
}

#[cfg(target_os = "windows")]
fn running(_root: &Path, launcher: &Path) -> bool {
    // Windows refuses write access to the image of a running program, which is the question.
    // Any other failure — a missing file, an ordinary permission problem — is not an answer and
    // is not read as one.
    match std::fs::OpenOptions::new().write(true).open(launcher) {
        Ok(_) => false,
        // 5 is ERROR_ACCESS_DENIED, 32 ERROR_SHARING_VIOLATION.
        Err(error) => matches!(error.raw_os_error(), Some(5 | 32)),
    }
}

#[cfg(target_os = "macos")]
fn running(root: &Path, _launcher: &Path) -> bool {
    // No `/proc` here. `pgrep` is part of the base system; it reads its pattern as a regular
    // expression, so this errs towards saying yes, which is the safe direction.
    let Some(pattern) = root.to_str() else {
        return false;
    };
    std::process::Command::new("pgrep").arg("-f").arg(pattern).output().is_ok_and(|output| output.status.success())
}

#[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
fn running(_root: &Path, _launcher: &Path) -> bool {
    false
}

/// Rewrite a recorded path that used to live under `from` so that it lives under `to`. A path
/// outside `from` — a launcher symlink in the user's `bin`, say — is left alone.
fn repoint(recorded: &str, from: &Path, to: &Path) -> String {
    match Path::new(recorded).strip_prefix(from) {
        // `join("")` would leave a trailing separator on the root of the tree itself.
        Ok(rest) if rest.as_os_str().is_empty() => to.display().to_string(),
        Ok(rest) => to.join(rest).display().to_string(),
        Err(_) => recorded.to_owned(),
    }
}

/// Move one installed app into `destination`, which becomes its new home.
///
/// Copy, verify, swap, delete — in that order, so an interruption at any point leaves the app
/// working exactly where it already was. The old copy goes only once every file of the new one
/// has been read back and hashed to the same digest, and only once the launcher, the desktop
/// entry and the Start Menu shortcut name the new location.
pub fn move_app(paths: &Paths, app: &App, destination: &Path, progress: Progress<'_>) -> Result<Installed, Error> {
    let mut state = State::load(&paths.state)?;
    let installed = state.get(&app.slug).cloned().ok_or_else(|| Error::NotInstalled { app: app.name.clone() })?;
    let from = app_home(paths, app, &installed);

    // A bundle keeps its own name in the new folder; everything else is a directory per app.
    let leaf = match installed.format {
        Format::Dmg => from.file_name().map(std::ffi::OsString::from).unwrap_or_else(|| app.slug.clone().into()),
        _ => app.slug.clone().into(),
    };
    let to = destination.join(&leaf);
    if to == from {
        return Ok(installed);
    }
    check_install_dir(destination)?;
    if appears_to_be_running(&from, Path::new(&installed.launcher)) {
        return Err(Error::Running { app: app.name.clone() });
    }
    if to.exists() {
        return Err(Error::DestinationExists { path: to.display().to_string() });
    }

    // Staged beside its destination, so the rename into place stays on one filesystem.
    let staged = destination.join(format!(".{}.moving", app.slug));
    let _ = std::fs::remove_dir_all(&staged);
    let mut copier = Copier { root: &from, observed: Observed::default(), copied: 0, total: tree_size(&from)? };
    if let Err(error) = copier.tree(&from, &staged, progress) {
        let _ = std::fs::remove_dir_all(&staged);
        return Err(error);
    }

    // Reading the copy back proves it is byte for byte what was there. It proves nothing about
    // whether what was there is what was installed — and a move that carried a tampered tree to
    // a new folder, re-aimed the launcher at it and left the record saying all was well would
    // launder it. So the copy is checked against the record of the install as well, using the
    // digests the copy has just computed rather than reading the whole tree a third time.
    if let Recorded::Manifest(manifest) = recorded(paths, &app.slug, &installed)? {
        let report = match manifest_path(&from, Path::new(&installed.dir)) {
            Some(prefix) => manifest.compare(&copier.observed.under(&prefix)),
            // A record whose version directory is not inside the home it names: read the tree
            // itself rather than let the question go unanswered.
            None => manifest.verify_tree(Path::new(&installed.dir))?,
        };
        if !report.is_intact() {
            let _ = std::fs::remove_dir_all(&staged);
            return Err(Error::NotAsInstalled { app: app.name.clone(), what: report.summary() });
        }
    }

    std::fs::rename(&staged, &to).map_err(fs::io_err(&to))?;

    let moved = Installed {
        dir: repoint(&installed.dir, &from, &to),
        launcher: repoint(&installed.launcher, &from, &to),
        root: Some(to.display().to_string()),
        previous: installed.previous.as_deref().map(|previous| repoint(previous, &from, &to)),
        ..installed
    };

    // A launcher outside the tree is a pointer into it, so it has to be re-aimed by hand. Where
    // there is one, what it points at — not the pointer — is what the desktop entry should run,
    // the same file the install wrote there.
    let mut exec = PathBuf::from(&moved.launcher);
    for name in std::iter::once(app.binary.clone()).chain(app.cli.clone()) {
        let link = paths.bin.join(&name);
        let Ok(target) = std::fs::read_link(&link) else {
            continue;
        };
        let retargeted = repoint(&target.display().to_string(), &from, &to);
        if name == app.binary {
            exec = PathBuf::from(&retargeted);
        }
        let _ = fs::flip_symlink(&link, Path::new(&retargeted));
    }
    write_desktop_entry(paths, app, &exec);
    if cfg!(target_os = "windows") {
        write_start_menu_shortcut(paths, &app.name, &exec);
    }

    state.installed.insert(app.slug.clone(), moved.clone());
    state.save(&paths.state)?;
    let _ = std::fs::remove_dir_all(&from);
    Ok(moved)
}

/// How many bytes of file content a tree holds. Symlinks count as nothing: they are recreated,
/// not copied.
fn tree_size(root: &Path) -> Result<u64, Error> {
    let meta = std::fs::symlink_metadata(root).map_err(fs::io_err(root))?;
    if meta.file_type().is_symlink() {
        return Ok(0);
    }
    if !meta.is_dir() {
        return Ok(meta.len());
    }
    let mut total = 0;
    for entry in std::fs::read_dir(root).map_err(fs::io_err(root))? {
        total += tree_size(&entry.map_err(fs::io_err(root))?.path())?;
    }
    Ok(total)
}

/// What a copy saw while it was making it, in the shape a manifest is in.
///
/// Kept rather than thrown away because the copy hashes every file on both sides anyway: a move
/// can then check what it has copied against the record of the install for free.
#[derive(Default)]
struct Observed {
    files: Vec<FileEntry>,
    links: Vec<LinkEntry>,
}

impl Observed {
    /// The part of what was copied that lies under `prefix`, with the prefix taken off.
    ///
    /// A manifest's paths are relative to the version directory. A move copies the app's whole
    /// home, which holds that directory and, until it is pruned, the version it replaced. This is
    /// what lines the two up.
    fn under(&self, prefix: &str) -> Manifest {
        let strip = |path: &str| -> Option<String> {
            if prefix.is_empty() {
                return Some(path.to_owned());
            }
            path.strip_prefix(prefix)?.strip_prefix('/').map(str::to_owned)
        };
        Manifest::new(
            self.files.iter().filter_map(|file| strip(&file.path).map(|path| FileEntry { path, sha256: file.sha256.clone(), size: file.size })).collect(),
            self.links.iter().filter_map(|link| strip(&link.path).map(|path| LinkEntry { path, target: link.target.clone() })).collect(),
        )
    }
}

/// A verified copy of a tree, in progress.
struct Copier<'a> {
    /// The tree being copied, so that every file can be recorded by its path relative to it.
    root: &'a Path,
    observed: Observed,
    copied: u64,
    total: u64,
}

impl Copier<'_> {
    /// Copy a tree, hashing every file on both sides and reporting progress in bytes.
    ///
    /// Verifying the copy is the point of doing it this way: a move is the one thing in this
    /// program that can lose the only copy of something, so nothing is believed until it has
    /// been read back.
    fn tree(&mut self, from: &Path, to: &Path, progress: Progress<'_>) -> Result<(), Error> {
        let meta = std::fs::symlink_metadata(from).map_err(fs::io_err(from))?;

        #[cfg(unix)]
        if meta.file_type().is_symlink() {
            // Followed rather than recreated, a symlink inside a macOS bundle would turn into a
            // second copy of whatever it points at.
            let target = std::fs::read_link(from).map_err(fs::io_err(from))?;
            let _ = std::fs::remove_file(to);
            if let Some(parent) = to.parent() {
                fs::mkdir_p(parent)?;
            }
            std::os::unix::fs::symlink(&target, to).map_err(fs::io_err(to))?;
            if let Some(path) = self.recordable(from) {
                self.observed.links.push(LinkEntry { path, target: target.to_string_lossy().into_owned() });
            }
            return Ok(());
        }

        if meta.is_dir() {
            fs::mkdir_p(to)?;
            for entry in std::fs::read_dir(from).map_err(fs::io_err(from))? {
                let entry = entry.map_err(fs::io_err(from))?;
                self.tree(&entry.path(), &to.join(entry.file_name()), progress)?;
            }
            return Ok(());
        }

        if let Some(parent) = to.parent() {
            fs::mkdir_p(parent)?;
        }
        std::fs::copy(from, to).map_err(fs::io_err(to))?;
        let mismatch = || Error::CopyMismatch { path: from.display().to_string() };
        let original = craftcenter_verify::sha256_file(from).map_err(|_| mismatch())?;
        if craftcenter_verify::sha256_file(to).map_err(|_| mismatch())? != original {
            return Err(mismatch());
        }
        if let Some(path) = self.recordable(from) {
            self.observed.files.push(FileEntry { path, sha256: craftcenter_verify::hex(&original), size: meta.len() });
        }
        self.copied = self.copied.saturating_add(meta.len());
        progress(self.copied, Some(self.total));
        Ok(())
    }

    /// How a manifest would spell `path`, for the entries that belong in one. A name a manifest
    /// never records is copied like anything else and then left out of the comparison, because
    /// the two sets have to be drawn up by the same rule.
    fn recordable(&self, path: &Path) -> Option<String> {
        if path.file_name().is_some_and(|name| ignorable(&name.to_string_lossy())) {
            return None;
        }
        manifest_path(self.root, path)
    }
}

/// Drop the version that was replaced, once the new one has run.
pub fn prune_previous(paths: &Paths, app: &App) -> Result<(), Error> {
    let mut state = State::load(&paths.state)?;
    let Some(installed) = state.installed.get_mut(&app.slug) else {
        return Ok(());
    };
    if let Some(previous) = installed.previous.take() {
        let _ = std::fs::remove_dir_all(&previous);
    }
    state.save(&paths.state)
}

/// A new build of CraftCenter, downloaded and verified, ready to take the running one's place.
pub struct SelfUpdateRequest<'a> {
    /// The running program, as `std::env::current_exe()` reports it.
    pub current_exe: &'a Path,
    /// Which kind of asset was downloaded. The swap is per-format, exactly as an install is.
    pub format: Format,
    /// The asset's name, for the error when it turns out to hold no program.
    pub asset: &'a str,
    /// The downloaded asset, already checked against the release's `SHA256SUMS.txt`.
    pub archive: &'a Path,
    /// Scratch space this call may empty and fill: where the archive is unpacked.
    pub staging: &'a Path,
}

/// What a self-update left on disk.
#[derive(Debug)]
pub struct Replaced {
    /// What now holds the new build: the program file, or the `.app` bundle around it.
    pub target: PathBuf,
    /// The build that was replaced, kept until the new one has started once.
    pub previous: PathBuf,
}

/// Replace the running CraftCenter with a newer build of itself.
///
/// **The asset is not the program.** A release's macOS asset is a disk image and its Windows
/// asset is a zip; only the Linux AppImage is itself the thing that runs. Until 0.2.1 this
/// renamed the downloaded asset straight over `current_exe`, which left a disk image where the
/// Mach-O had been — the program never started again, and because the retired copy was deleted
/// at once there was nothing to go back to. So each format is now unpacked the way the matching
/// install unpacks it, whatever comes out is checked to be a program this machine can run
/// ([`check_is_program`]), and the build it replaces is kept until [`clean_after_self_update`].
pub fn self_update(request: &SelfUpdateRequest<'_>) -> Result<Replaced, Error> {
    let _ = std::fs::remove_dir_all(request.staging);
    fs::mkdir_p(request.staging)?;

    let result = match request.format {
        // Here the download really is the program: an AppImage is an ELF with a filesystem
        // appended. It still goes through the same check as everything else.
        Format::AppImage => replace_program(request, request.archive),
        Format::TarGz => {
            fs::unpack_tar_gz(request.archive, request.staging)?;
            let root = unpacked_root(request.staging)?;
            let program = program_in(&root, request.current_exe).ok_or_else(|| no_program_in(request))?;
            replace_program(request, &program)
        }
        Format::PortableZip => {
            fs::unpack_zip(request.archive, request.staging)?;
            let root = unpacked_root(request.staging)?;
            let program = program_in(&root, request.current_exe).ok_or_else(|| no_program_in(request))?;
            let replaced = replace_program(request, &program)?;
            // The portable build is a folder rather than a lone executable: the command line
            // binary and the notices ship beside the program and belong beside the new one too.
            copy_companions(&root, &program, &replaced.target);
            Ok(replaced)
        }
        Format::Dmg => {
            let mount = request.staging.join("mnt");
            with_mounted_bundle(request.archive, &mount, request.asset, |bundle| replace_from_bundle(request, bundle))
        }
        Format::Msi => Err(Error::NeedsElevation { app: "CraftCenter".to_owned(), format: "per-machine installer" }),
        Format::CliZip => Err(Error::NotInArchive { asset: request.asset.to_owned(), what: "application (this is the headless CLI)".to_owned() }),
    };

    let _ = std::fs::remove_dir_all(request.staging);
    result
}

fn no_program_in(request: &SelfUpdateRequest<'_>) -> Error {
    Error::NotInArchive { asset: request.asset.to_owned(), what: "CraftCenter program".to_owned() }
}

/// Copy `program` beside the running one and rename it into place.
fn replace_program(request: &SelfUpdateRequest<'_>, program: &Path) -> Result<Replaced, Error> {
    let staged = stage_sibling(request.current_exe, program)?;
    match self_replace(request.current_exe, &staged) {
        Ok(previous) => Ok(Replaced { target: request.current_exe.to_path_buf(), previous }),
        Err(error) => {
            let _ = std::fs::remove_file(&staged);
            Err(error)
        }
    }
}

/// The half of a DMG self-update that needs no disk image: a `.app` tree has been found, and now
/// it takes the place of the one the program is running from.
///
/// A program started from a bundle is replaced *as a bundle* — that is what the Finder, the dock
/// and the code signature all treat as the program, and macOS allows a running bundle to be
/// renamed out from under itself. A program built from source is in no bundle at all, so the
/// Mach-O is taken out of the one in the disk image and the file itself is replaced.
fn replace_from_bundle(request: &SelfUpdateRequest<'_>, bundle: &Path) -> Result<Replaced, Error> {
    let Some(destination) = enclosing_bundle(request.current_exe) else {
        let program = bundle_program(bundle, request.current_exe).ok_or_else(|| no_program_in(request))?;
        return replace_program(request, &program);
    };

    let staged = stage_bundle(bundle, &destination)?;
    // Check the bundle's own program before the bundle is renamed into place: a `.app` holding
    // the wrong thing would be just as unopenable as the disk image 0.2.0 left behind.
    let checked = bundle_program(&staged, request.current_exe).ok_or_else(|| no_program_in(request)).and_then(|program| check_is_program(&program));
    if let Err(error) = checked {
        let _ = std::fs::remove_dir_all(&staged);
        return Err(error);
    }

    let previous = swap_tree(&destination, &staged)?;
    Ok(Replaced { target: destination, previous })
}

/// The `.app` the running program sits inside, if it sits inside one. A build from source does
/// not: `cargo build` produces a bare Mach-O with no bundle around it.
fn enclosing_bundle(program: &Path) -> Option<PathBuf> {
    program.ancestors().find(|ancestor| ancestor.extension().is_some_and(|e| e == "app")).map(Path::to_path_buf)
}

/// The executable inside a bundle.
fn bundle_program(bundle: &Path, running: &Path) -> Option<PathBuf> {
    let macos = bundle.join("Contents/MacOS");
    for name in names_to_try(running) {
        let candidate = macos.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    // A later release could rename the binary; one executable in `Contents/MacOS` is unambiguous.
    let mut files = std::fs::read_dir(&macos).ok()?.flatten().map(|entry| entry.path()).filter(|path| path.is_file());
    let only = files.next()?;
    files.next().is_none().then_some(only)
}

/// The program inside an unpacked archive, looked for at its root and in `bin/`.
fn program_in(root: &Path, running: &Path) -> Option<PathBuf> {
    for dir in [root.to_path_buf(), root.join("bin")] {
        for name in names_to_try(running) {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// What the new build's program might be called: whatever the running one is called first, so a
/// portable copy someone renamed still updates itself, then the names a release publishes.
fn names_to_try(running: &Path) -> Vec<String> {
    let mut names = vec![file_name_of(running)];
    for name in ["craftcenter.exe", "craftcenter"] {
        if !names.iter().any(|existing| existing == name) {
            names.push(name.to_owned());
        }
    }
    names
}

/// When an archive unpacked to exactly one directory, that directory is the real root.
fn unpacked_root(dir: &Path) -> Result<PathBuf, Error> {
    Ok(single_child_dir(dir)?.unwrap_or_else(|| dir.to_path_buf()))
}

/// Copy `program` to a hidden file beside `target`, so the rename that swaps them happens within
/// one directory and is therefore atomic. A downloaded asset lives in the cache, which on some
/// machines is a different filesystem, where a rename would fail outright.
fn stage_sibling(target: &Path, program: &Path) -> Result<PathBuf, Error> {
    let staged = staged_path(target);
    if let Some(parent) = staged.parent() {
        fs::mkdir_p(parent)?;
    }
    let _ = std::fs::remove_file(&staged);
    std::fs::copy(program, &staged).map_err(fs::io_err(&staged))?;
    Ok(staged)
}

/// The same, for the `.app` tree a macOS build lives in.
fn stage_bundle(bundle: &Path, destination: &Path) -> Result<PathBuf, Error> {
    let staged = staged_path(destination);
    let _ = std::fs::remove_dir_all(&staged);
    copy_tree(bundle, &staged)?;
    Ok(staged)
}

fn staged_path(target: &Path) -> PathBuf {
    target.with_file_name(format!(".{}.new", file_name_of(target)))
}

fn retired_path(target: &Path) -> PathBuf {
    target.with_file_name(format!(".{}.old", file_name_of(target)))
}

fn file_name_of(path: &Path) -> String {
    path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_else(|| "craftcenter".to_owned())
}

/// Replace the running program file with `staged`, which is already beside it.
///
/// The swap is two renames: the running file is moved aside and the new one takes its place.
/// That order is what makes it work on all three platforms — Unix lets a running image be
/// renamed or unlinked because the kernel holds the inode, and Windows refuses to *delete* a
/// running image but allows it to be *renamed*. Returns where the previous build was moved to;
/// it stays there until [`clean_after_self_update`].
///
/// The caller restarts the program; this function does not, because only the caller knows
/// whether it is safe to.
pub fn self_replace(current_exe: &Path, staged: &Path) -> Result<PathBuf, Error> {
    check_is_program(staged)?;
    fs::make_executable(staged)?;
    swap_file(current_exe, staged)
}

/// Rename `staged` over `target`, keeping whatever was there as `.<name>.old`. A second rename
/// that fails puts the original back, so the worst case is the build that was already running.
fn swap_file(target: &Path, staged: &Path) -> Result<PathBuf, Error> {
    let retired = retired_path(target);
    let _ = std::fs::remove_file(&retired);
    let existed = target.exists();
    if existed {
        std::fs::rename(target, &retired).map_err(fs::io_err(target))?;
    }
    if let Err(error) = std::fs::rename(staged, target) {
        if existed {
            let _ = std::fs::rename(&retired, target);
        }
        return Err(Error::Io { path: target.display().to_string(), source: error });
    }
    Ok(retired)
}

/// The same swap for a directory, which is what a macOS bundle is.
fn swap_tree(destination: &Path, staged: &Path) -> Result<PathBuf, Error> {
    let retired = retired_path(destination);
    let _ = std::fs::remove_dir_all(&retired);
    let existed = destination.exists();
    if existed {
        std::fs::rename(destination, &retired).map_err(fs::io_err(destination))?;
    }
    if let Err(error) = std::fs::rename(staged, destination) {
        if existed {
            let _ = std::fs::rename(&retired, destination);
        }
        let _ = std::fs::remove_dir_all(staged);
        return Err(Error::Io { path: destination.display().to_string(), source: error });
    }
    Ok(retired)
}

/// Everything else the archive held goes beside the new program: the command line binary, the
/// licence, the portable build's own notice.
///
/// Best-effort on purpose. The update is the program; a companion that cannot be replaced
/// because it is itself running leaves the copy already there, which still works, and the next
/// update takes another go at it.
fn copy_companions(root: &Path, program: &Path, target: &Path) {
    let Some(destination) = target.parent() else { return };
    let Ok(entries) = std::fs::read_dir(root) else { return };
    for entry in entries.flatten() {
        let from = entry.path();
        if from == program {
            continue;
        }
        let Some(name) = from.file_name() else { continue };
        let to = destination.join(name);
        if from.is_dir() {
            let _ = copy_tree(&from, &to);
            continue;
        }
        // Renamed into place rather than written over, because Windows refuses to write over an
        // image that is mapped but will let it be moved aside.
        let Ok(staged) = stage_sibling(&to, &from) else { continue };
        match swap_file(&to, &staged) {
            Ok(retired) => {
                let _ = std::fs::remove_file(&retired);
            }
            Err(_) => {
                let _ = std::fs::remove_file(&staged);
            }
        }
    }
}

/// Delete the build a self-update replaced.
///
/// Call it once the new build has proved it runs — the window has drawn a frame, or the command
/// line has printed its version — and not before: until then the previous build is the only
/// thing there is to go back to. It does nothing when there is nothing to clean.
pub fn clean_after_self_update(current_exe: &Path) {
    let _ = std::fs::remove_file(retired_path(current_exe));
    if let Some(bundle) = enclosing_bundle(current_exe) {
        let _ = std::fs::remove_dir_all(retired_path(&bundle));
    }
}

#[cfg(test)]
mod tests {
    use craftcenter_catalogue::Catalogue;
    use craftcenter_select::Note;
    use craftcenter_verify::manifest::Level;

    use super::*;

    fn app(slug: &str) -> App {
        let catalogue = Catalogue::embedded().expect("catalogue parses");
        catalogue.get(slug).cloned().unwrap_or_else(|| panic!("{slug} is in the catalogue"))
    }

    fn appimage(dir: &Path, body: &str) -> PathBuf {
        let path = dir.join("photocraft-0.3.0-linux-x86_64.AppImage");
        std::fs::write(&path, body).expect("write");
        path
    }

    fn request<'a>(app: &'a App, version: &'a str, choice: &'a Choice, archive: &'a Path) -> Request<'a> {
        Request { app, version, tag: "v0.3.0", choice, archive, sha256: "0".repeat(64).leak(), icon_png: Some(b"PNG") }
    }

    fn choice(asset: &str, format: Format) -> Choice {
        Choice { asset: asset.to_owned(), format, note: None }
    }

    /// A `.tar.gz` with one entry per `(name, body)`, written the way a real release asset is: a
    /// single top-level directory holding an FHS tree. Shared by every test below that needs a
    /// tarball fixture, so the archive-building boilerplate exists in one place.
    fn write_tar_gz(path: &Path, entries: &[(&str, &[u8])]) {
        let file = std::fs::File::create(path).expect("create");
        let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::fast());
        let mut builder = tar::Builder::new(encoder);
        for (name, body) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(body.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            builder.append_data(&mut header, *name, *body).expect("append");
        }
        builder.into_inner().expect("finish").finish().expect("flush");
    }

    /// A portable `.zip` with one entry per `(name, body)`, built the same way `fs::tests` builds
    /// one: the `zip` crate is already a dependency of this crate, so no new one is needed to test
    /// the format it unpacks.
    fn write_zip(path: &Path, entries: &[(&str, &[u8])]) {
        let file = std::fs::File::create(path).expect("create");
        let mut writer = zip::ZipWriter::new(file);
        for (name, body) in entries {
            let options: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default();
            writer.start_file(*name, options).expect("entry");
            std::io::Write::write_all(&mut writer, body).expect("write");
        }
        writer.finish().expect("finish");
    }

    #[test]
    fn installs_an_appimage_with_a_launcher_and_a_desktop_entry() {
        let root = tempfile::tempdir().expect("temp dir");
        let paths = Paths::rooted(root.path());
        let source = tempfile::tempdir().expect("temp dir");
        let archive = appimage(source.path(), "version one");
        let app = app("photocraft");
        let choice = choice("photocraft-0.3.0-linux-x86_64.AppImage", Format::AppImage);

        let installed = install(&paths, &request(&app, "0.3.0", &choice, &archive)).expect("installed");

        assert_eq!(installed.version, "0.3.0");
        let target = paths.version_dir("photocraft", "0.3.0").join("ai.storyteller.photocraft.AppImage");
        assert_eq!(std::fs::read_to_string(&target).expect("read"), "version one");
        assert_eq!(std::fs::read_to_string(paths.bin.join("photocraft")).expect("through the launcher"), "version one");
        if cfg!(target_os = "linux") {
            let entry = std::fs::read_to_string(paths.data.join("applications/ai.storyteller.photocraft.desktop")).expect("entry");
            assert!(entry.contains(&format!("Exec=\"{}\" %F", target.display())), "{entry}");
            assert!(entry.contains("Name=PhotoCraft"));
            assert!(paths.data.join("icons/hicolor/64x64/apps/ai.storyteller.photocraft.png").is_file());
        }
        assert_eq!(State::load(&paths.state).expect("state").get("photocraft").map(|i| i.version.clone()), Some("0.3.0".to_owned()));
    }

    #[cfg(unix)]
    #[test]
    fn an_upgrade_flips_the_launcher_and_keeps_the_old_version_until_it_is_pruned() {
        let root = tempfile::tempdir().expect("temp dir");
        let paths = Paths::rooted(root.path());
        let source = tempfile::tempdir().expect("temp dir");
        let app = app("photocraft");

        let first = appimage(source.path(), "version one");
        let c1 = choice("photocraft-0.3.0-linux-x86_64.AppImage", Format::AppImage);
        install(&paths, &request(&app, "0.3.0", &c1, &first)).expect("first install");

        let second = source.path().join("photocraft-0.4.0-linux-x86_64.AppImage");
        std::fs::write(&second, "version two").expect("write");
        let c2 = choice("photocraft-0.4.0-linux-x86_64.AppImage", Format::AppImage);
        let upgraded = install(&paths, &request(&app, "0.4.0", &c2, &second)).expect("upgrade");

        assert_eq!(std::fs::read_to_string(paths.bin.join("photocraft")).expect("read"), "version two");
        let previous = upgraded.previous.clone().expect("the replaced version is remembered");
        assert!(Path::new(&previous).is_dir(), "the old version survives the upgrade");

        prune_previous(&paths, &app).expect("pruned");
        assert!(!Path::new(&previous).exists());
        assert!(State::load(&paths.state).expect("state").get("photocraft").and_then(|i| i.previous.clone()).is_none());
    }

    #[test]
    fn installs_a_relocatable_tarball() {
        let root = tempfile::tempdir().expect("temp dir");
        let paths = Paths::rooted(root.path());
        let source = tempfile::tempdir().expect("temp dir");
        let archive = source.path().join("photocraft-0.3.0-linux-x86_64.tar.gz");

        let file = std::fs::File::create(&archive).expect("create");
        let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::fast());
        let mut builder = tar::Builder::new(encoder);
        for (name, body) in [("photocraft-0.3.0-linux-x86_64/bin/photocraft", "the app"), ("photocraft-0.3.0-linux-x86_64/bin/photocraft-cli", "the cli")] {
            let mut header = tar::Header::new_gnu();
            header.set_size(body.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            builder.append_data(&mut header, name, body.as_bytes()).expect("append");
        }
        builder.into_inner().expect("finish").finish().expect("flush");

        let app = app("photocraft");
        let choice = choice("photocraft-0.3.0-linux-x86_64.tar.gz", Format::TarGz);
        install(&paths, &request(&app, "0.3.0", &choice, &archive)).expect("installed");

        assert_eq!(std::fs::read_to_string(paths.bin.join("photocraft")).expect("app"), "the app");
        assert_eq!(std::fs::read_to_string(paths.bin.join("photocraft-cli")).expect("cli"), "the cli");
    }

    #[test]
    fn a_tarball_without_the_expected_binary_is_refused() {
        let root = tempfile::tempdir().expect("temp dir");
        let paths = Paths::rooted(root.path());
        let source = tempfile::tempdir().expect("temp dir");
        let archive = source.path().join("photocraft-0.3.0-linux-x86_64.tar.gz");
        let file = std::fs::File::create(&archive).expect("create");
        let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::fast());
        let mut builder = tar::Builder::new(encoder);
        let mut header = tar::Header::new_gnu();
        header.set_size(4);
        header.set_mode(0o644);
        header.set_cksum();
        builder.append_data(&mut header, "photocraft-0.3.0-linux-x86_64/README", &b"docs"[..]).expect("append");
        builder.into_inner().expect("finish").finish().expect("flush");

        let app = app("photocraft");
        let choice = choice("photocraft-0.3.0-linux-x86_64.tar.gz", Format::TarGz);
        let result = install(&paths, &request(&app, "0.3.0", &choice, &archive));
        assert!(matches!(result, Err(Error::NotInArchive { .. })), "{result:?}");
        assert!(State::load(&paths.state).expect("state").get("photocraft").is_none(), "a failed install records nothing");
    }

    #[test]
    fn the_msi_is_refused_because_it_would_elevate() {
        let root = tempfile::tempdir().expect("temp dir");
        let paths = Paths::rooted(root.path());
        let source = tempfile::tempdir().expect("temp dir");
        let archive = appimage(source.path(), "irrelevant");
        let app = app("photocraft");
        let choice = choice("photocraft-0.3.0-windows-x64.msi", Format::Msi);
        assert!(matches!(install(&paths, &request(&app, "0.3.0", &choice, &archive)), Err(Error::NeedsElevation { .. })));
    }

    #[test]
    fn remove_takes_away_everything_it_wrote() {
        let root = tempfile::tempdir().expect("temp dir");
        let paths = Paths::rooted(root.path());
        let source = tempfile::tempdir().expect("temp dir");
        let archive = appimage(source.path(), "version one");
        let app = app("photocraft");
        let choice = choice("photocraft-0.3.0-linux-x86_64.AppImage", Format::AppImage);
        install(&paths, &request(&app, "0.3.0", &choice, &archive)).expect("installed");

        remove(&paths, &app).expect("removed");
        assert!(!paths.app_dir("photocraft").exists());
        assert!(!paths.bin.join("photocraft").exists());
        assert!(!paths.data.join("applications/ai.storyteller.photocraft.desktop").exists());
        assert!(State::load(&paths.state).expect("state").get("photocraft").is_none());
    }

    #[test]
    fn removing_something_that_is_not_installed_says_so() {
        let root = tempfile::tempdir().expect("temp dir");
        let paths = Paths::rooted(root.path());
        assert!(matches!(remove(&paths, &app("photocraft")), Err(Error::NotInstalled { .. })));
    }

    /// Bytes that pass the program check on whichever platform the tests are running on, with
    /// `mark` at the end so a swap can be told from no swap.
    fn program_bytes(mark: &str) -> Vec<u8> {
        let mut bytes = vec![0u8; 64];
        if cfg!(target_os = "macos") {
            bytes[0..4].copy_from_slice(&[0xcf, 0xfa, 0xed, 0xfe]);
        } else if cfg!(target_os = "windows") {
            bytes[0..2].copy_from_slice(b"MZ");
            bytes[0x3c..0x40].copy_from_slice(&64u32.to_le_bytes());
            bytes.extend_from_slice(b"PE\0\0");
        } else {
            bytes[0..4].copy_from_slice(&[0x7f, b'E', b'L', b'F']);
        }
        bytes.extend_from_slice(mark.as_bytes());
        bytes
    }

    fn write_program(path: &Path, mark: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create");
        }
        std::fs::write(path, program_bytes(mark)).expect("write");
    }

    /// Which build a file holds, read back from the mark `program_bytes` appended.
    fn mark_of(path: &Path) -> String {
        let bytes = std::fs::read(path).expect("read");
        let header = program_bytes("").len();
        String::from_utf8_lossy(bytes.get(header..).unwrap_or_default()).into_owned()
    }

    /// A file ending in the 512-byte `koly` trailer `hdiutil` writes: a disk image, as far as
    /// anything reading its bytes is concerned.
    fn disk_image(path: &Path) -> PathBuf {
        let mut bytes = vec![0u8; 1024];
        bytes[512..516].copy_from_slice(b"koly");
        std::fs::write(path, bytes).expect("write");
        path.to_path_buf()
    }

    /// A `.app` tree, which is what a mounted disk image holds and what CI has no `hdiutil` to
    /// make. Everything after the mount is the same code either way.
    fn bundle_tree(parent: &Path, name: &str, mark: &str) -> PathBuf {
        let bundle = parent.join(name);
        write_program(&bundle.join("Contents/MacOS/craftcenter"), mark);
        std::fs::create_dir_all(bundle.join("Contents/Resources")).expect("create");
        std::fs::write(bundle.join("Contents/Info.plist"), "<plist/>").expect("write");
        bundle
    }

    fn portable_zip(dir: &Path, entries: &[(&str, Vec<u8>)]) -> PathBuf {
        let path = dir.join("craftcenter-0.2.1-windows-x64-portable.zip");
        let file = std::fs::File::create(&path).expect("create");
        let mut writer = zip::ZipWriter::new(file);
        for (name, bytes) in entries {
            let options: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default();
            writer.start_file(*name, options).expect("entry");
            std::io::Write::write_all(&mut writer, bytes).expect("write");
        }
        writer.finish().expect("finish");
        path
    }

    fn tarball(dir: &Path, mark: &str) -> PathBuf {
        let path = dir.join("craftcenter-0.2.1-linux-x86_64.tar.gz");
        let file = std::fs::File::create(&path).expect("create");
        let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::fast());
        let mut builder = tar::Builder::new(encoder);
        let bytes = program_bytes(mark);
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        builder.append_data(&mut header, "craftcenter-0.2.1-linux-x86_64/bin/craftcenter", bytes.as_slice()).expect("append");
        builder.into_inner().expect("finish").finish().expect("flush");
        path
    }

    fn self_request<'a>(current_exe: &'a Path, format: Format, asset: &'a str, archive: &'a Path, staging: &'a Path) -> SelfUpdateRequest<'a> {
        SelfUpdateRequest { current_exe, format, asset, archive, staging }
    }

    #[test]
    fn self_replace_swaps_the_running_file_and_keeps_the_build_it_replaced() {
        let dir = tempfile::tempdir().expect("temp dir");
        let exe = dir.path().join("craftcenter");
        write_program(&exe, "old build");
        let staged = staged_path(&exe);
        write_program(&staged, "new build");

        let previous = self_replace(&exe, &staged).expect("replaced");
        assert_eq!(mark_of(&exe), "new build");
        assert!(!staged.exists());
        // The point of 0.2.1: the retired build is still there to go back to.
        assert_eq!(mark_of(&previous), "old build");

        clean_after_self_update(&exe);
        assert!(!previous.exists(), "the previous build goes once the new one has run");
    }

    #[test]
    fn nothing_but_a_program_is_renamed_over_the_running_build() {
        // The 0.2.0 bug, as a test: the downloaded asset is not the program, and presenting it as
        // one must fail before anything is moved.
        let dir = tempfile::tempdir().expect("temp dir");
        let exe = dir.path().join("craftcenter");
        write_program(&exe, "old build");

        let dmg = disk_image(&dir.path().join("craftcenter-0.2.0-macos-universal.dmg"));
        let zip = portable_zip(dir.path(), &[("craftcenter-0.2.1-windows-x64-portable/craftcenter.exe", program_bytes("new build"))]);
        for (archive, expected) in [(&dmg, "a disk image"), (&zip, "a zip archive")] {
            let error = self_replace(&exe, archive).expect_err("refused");
            assert!(matches!(error, Error::NotAProgram { .. }), "{error:?}");
            assert!(error.to_string().contains(expected), "{error}");
            assert_eq!(mark_of(&exe), "old build", "the running program was replaced anyway");
            assert!(!retired_path(&exe).exists(), "the running program was moved aside anyway");
        }
    }

    #[test]
    fn a_rename_that_fails_puts_the_running_program_back() {
        let dir = tempfile::tempdir().expect("temp dir");
        let exe = dir.path().join("craftcenter");
        write_program(&exe, "old build");

        // Nothing is staged, so the second rename fails after the first has already moved the
        // running program out of the way.
        let error = swap_file(&exe, &staged_path(&exe)).expect_err("failed");
        assert!(matches!(error, Error::Io { .. }), "{error:?}");
        assert_eq!(mark_of(&exe), "old build");
        assert!(!retired_path(&exe).exists(), "the moved-aside copy was left behind");
    }

    #[test]
    fn an_appimage_self_update_is_the_download_itself() {
        let dir = tempfile::tempdir().expect("temp dir");
        let exe = dir.path().join("bin/craftcenter");
        write_program(&exe, "old build");
        let archive = dir.path().join("downloads/craftcenter-0.2.1-linux-x86_64.AppImage");
        write_program(&archive, "new build");
        let staging = dir.path().join("staging");

        let replaced = self_update(&self_request(&exe, Format::AppImage, "craftcenter-0.2.1-linux-x86_64.AppImage", &archive, &staging)).expect("updated");

        assert_eq!(replaced.target, exe);
        assert_eq!(mark_of(&exe), "new build");
        assert_eq!(mark_of(&replaced.previous), "old build");
        // Staged as a copy, so the verified download is still in the cache for a retry.
        assert_eq!(mark_of(&archive), "new build");
    }

    #[test]
    fn a_tarball_self_update_swaps_the_binary_out_of_the_tree() {
        let dir = tempfile::tempdir().expect("temp dir");
        let exe = dir.path().join("bin/craftcenter");
        write_program(&exe, "old build");
        let archive = tarball(dir.path(), "new build");
        let staging = dir.path().join("staging");

        let replaced = self_update(&self_request(&exe, Format::TarGz, "craftcenter-0.2.1-linux-x86_64.tar.gz", &archive, &staging)).expect("updated");

        assert_eq!(mark_of(&exe), "new build");
        assert_eq!(mark_of(&replaced.previous), "old build");
        assert!(!staging.exists(), "the unpacked tree is cleaned up");
    }

    #[test]
    fn a_portable_zip_self_update_swaps_the_exe_and_what_ships_beside_it() {
        let dir = tempfile::tempdir().expect("temp dir");
        let folder = dir.path().join("CraftCenter");
        let exe = folder.join("craftcenter.exe");
        write_program(&exe, "old build");
        write_program(&folder.join("craftcenter-cli.exe"), "old cli");
        let archive = portable_zip(
            dir.path(),
            &[
                ("craftcenter-0.2.1-windows-x64-portable/craftcenter.exe", program_bytes("new build")),
                ("craftcenter-0.2.1-windows-x64-portable/craftcenter-cli.exe", program_bytes("new cli")),
                ("craftcenter-0.2.1-windows-x64-portable/portable.txt", b"CraftCenter - portable build\n".to_vec()),
            ],
        );
        let staging = dir.path().join("staging");

        let replaced =
            self_update(&self_request(&exe, Format::PortableZip, "craftcenter-0.2.1-windows-x64-portable.zip", &archive, &staging)).expect("updated");

        assert_eq!(mark_of(&exe), "new build");
        assert_eq!(mark_of(&replaced.previous), "old build");
        assert_eq!(mark_of(&folder.join("craftcenter-cli.exe")), "new cli", "the command line binary came along");
        assert_eq!(std::fs::read_to_string(folder.join("portable.txt")).expect("read"), "CraftCenter - portable build\n");
        assert!(!staged_path(&exe).exists(), "a staging copy was left behind");
        assert!(!staging.exists(), "the unpacked folder is cleaned up");
    }

    #[test]
    fn a_disk_image_self_update_swaps_the_bundle_and_keeps_the_old_one() {
        // No `hdiutil` here, so the `.app` a mounted image would hold is built by hand; the swap
        // this exercises is the half that runs after the mount, on every platform.
        let dir = tempfile::tempdir().expect("temp dir");
        let applications = dir.path().join("Applications");
        let installed = bundle_tree(&applications, "CraftCenter.app", "old build");
        let exe = installed.join("Contents/MacOS/craftcenter");
        let fresh = bundle_tree(&dir.path().join("mnt"), "CraftCenter.app", "new build");
        let staging = dir.path().join("staging");
        let archive = disk_image(&dir.path().join("craftcenter-0.2.1-macos-universal.dmg"));

        let request = self_request(&exe, Format::Dmg, "craftcenter-0.2.1-macos-universal.dmg", &archive, &staging);
        let replaced = replace_from_bundle(&request, &fresh).expect("updated");

        assert_eq!(replaced.target, installed, "the bundle the program runs from is what was swapped");
        assert_eq!(mark_of(&exe), "new build");
        assert_eq!(replaced.previous, applications.join(".CraftCenter.app.old"));
        assert_eq!(mark_of(&replaced.previous.join("Contents/MacOS/craftcenter")), "old build");
        assert!(!staged_path(&installed).exists(), "the staged bundle was renamed, not copied");

        clean_after_self_update(&exe);
        assert!(!replaced.previous.exists(), "the previous bundle goes once the new build has run");
    }

    #[test]
    fn a_build_from_source_has_no_bundle_so_the_program_itself_is_replaced() {
        let dir = tempfile::tempdir().expect("temp dir");
        let exe = dir.path().join("target/release/craftcenter");
        write_program(&exe, "old build");
        let fresh = bundle_tree(&dir.path().join("mnt"), "CraftCenter.app", "new build");
        let staging = dir.path().join("staging");
        let archive = disk_image(&dir.path().join("craftcenter-0.2.1-macos-universal.dmg"));

        let request = self_request(&exe, Format::Dmg, "craftcenter-0.2.1-macos-universal.dmg", &archive, &staging);
        let replaced = replace_from_bundle(&request, &fresh).expect("updated");

        assert_eq!(replaced.target, exe);
        assert_eq!(mark_of(&exe), "new build", "the Mach-O came out of the bundle in the image");
        assert_eq!(mark_of(&replaced.previous), "old build");
    }

    #[test]
    fn an_archive_holding_no_program_is_refused_and_changes_nothing() {
        let dir = tempfile::tempdir().expect("temp dir");
        let exe = dir.path().join("CraftCenter/craftcenter.exe");
        write_program(&exe, "old build");
        let archive = portable_zip(dir.path(), &[("portable.txt", b"CraftCenter - portable build\n".to_vec())]);
        let staging = dir.path().join("staging");

        let error =
            self_update(&self_request(&exe, Format::PortableZip, "craftcenter-0.2.1-windows-x64-portable.zip", &archive, &staging)).expect_err("refused");

        assert!(matches!(error, Error::NotInArchive { .. }), "{error:?}");
        assert_eq!(mark_of(&exe), "old build");
    }

    #[test]
    fn craftcenter_never_updates_itself_through_the_per_machine_installer() {
        let dir = tempfile::tempdir().expect("temp dir");
        let exe = dir.path().join("craftcenter.exe");
        write_program(&exe, "old build");
        let archive = dir.path().join("craftcenter-0.2.1-windows-x64.msi");
        std::fs::write(&archive, b"not used").expect("write");
        let staging = dir.path().join("staging");

        let error = self_update(&self_request(&exe, Format::Msi, "craftcenter-0.2.1-windows-x64.msi", &archive, &staging)).expect_err("refused");

        assert!(matches!(error, Error::NeedsElevation { .. }), "{error:?}");
        assert_eq!(mark_of(&exe), "old build");
    }

    #[test]
    fn a_desktop_exec_path_with_shell_metacharacters_is_not_written() {
        assert!(exec_safe(Path::new("/home/someone/.local/share/craftcenter/apps/x/1.0/a.AppImage")).is_some());
        for hostile in ["/tmp/a\"b", "/tmp/a$b", "/tmp/a%b", "/tmp/a\\b", "/tmp/a\nb"] {
            assert!(exec_safe(Path::new(hostile)).is_none(), "{hostile} was accepted");
        }
    }

    #[test]
    fn an_arch_fallback_note_survives_into_the_choice() {
        // Selection records why an x64 build was chosen for an ARM64 host; installation copies
        // the asset name through unchanged, so the note stays available to the caller.
        let choice = Choice {
            asset: "printcraft-0.2.1-windows-x64-portable.zip".to_owned(),
            format: Format::PortableZip,
            note: Some(Note::ArchFallback { wanted: craftcenter_select::Arch::Aarch64, used: craftcenter_select::Arch::X86_64 }),
        };
        assert!(choice.note.is_some());
    }

    #[test]
    fn a_usable_install_location_is_a_folder_this_user_can_write_to() {
        let root = tempfile::tempdir().expect("temp dir");

        let chosen = root.path().join("Crafting Apps");
        check_install_dir(&chosen).expect("a folder that does not exist yet is created");
        assert!(chosen.is_dir());
        check_install_dir(&chosen).expect("and is usable the second time too");
        assert!(!chosen.join(".craftcenter-write-test").exists(), "the write probe is cleaned up");
    }

    #[test]
    fn a_file_is_not_an_install_location() {
        let root = tempfile::tempdir().expect("temp dir");
        let file = root.path().join("notes.txt");
        std::fs::write(&file, b"not a folder").expect("write");
        let error = check_install_dir(&file).expect_err("a file is refused");
        assert!(error.to_string().contains("file, not a folder"), "{error}");
    }

    #[test]
    fn craftcenters_own_folder_is_not_an_install_location() {
        let Ok(exe) = std::env::current_exe() else {
            return; // Nothing to compare against on a platform that will not say.
        };
        let Some(program) = exe.parent() else { return };
        let error = check_install_dir(&program.join("apps")).expect_err("inside the program's own folder is refused");
        assert!(error.to_string().contains("CraftCenter's own folder"), "{error}");
    }

    #[cfg(unix)]
    #[test]
    fn a_folder_this_user_cannot_write_to_is_refused_rather_than_elevated() {
        use std::os::unix::fs::PermissionsExt;
        if nix_is_root() {
            return; // root can write anywhere, so there is nothing to refuse.
        }
        let root = tempfile::tempdir().expect("temp dir");
        let locked = root.path().join("locked");
        std::fs::create_dir(&locked).expect("create");
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o500)).expect("read-only");

        let error = check_install_dir(&locked).expect_err("a read-only folder is refused");
        assert!(error.to_string().contains("administrator rights"), "{error}");

        let _ = std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700));
    }

    #[cfg(unix)]
    fn nix_is_root() -> bool {
        std::fs::metadata("/proc/self").is_ok() && std::env::var("USER").is_ok_and(|user| user == "root")
    }

    #[test]
    fn a_recorded_path_follows_the_app_to_its_new_home() {
        // Built with `join` rather than written out, because the separator differs by platform
        // and what is being tested is the prefix swap, not how a path is spelled.
        let from = Path::new("one").join("apps").join("photocraft");
        let to = Path::new("two").join("photocraft");
        let inside = from.join("0.3.0");
        let elsewhere = Path::new("home").join("bin").join("photocraft");

        assert_eq!(repoint(&inside.display().to_string(), &from, &to), to.join("0.3.0").display().to_string());
        // The root of the tree itself, with no trailing separator left behind.
        assert_eq!(repoint(&from.display().to_string(), &from, &to), to.display().to_string());
        // A launcher in the user's bin is not inside the tree and must not be rewritten.
        assert_eq!(repoint(&elsewhere.display().to_string(), &from, &to), elsewhere.display().to_string());
    }

    #[cfg(unix)]
    #[test]
    fn moving_an_app_copies_verifies_relinks_and_only_then_deletes() {
        let root = tempfile::tempdir().expect("temp dir");
        let paths = Paths::rooted(root.path());
        let source = tempfile::tempdir().expect("temp dir");
        let archive = appimage(source.path(), "version one");
        let app = app("photocraft");
        let choice = choice("photocraft-0.3.0-linux-x86_64.AppImage", Format::AppImage);
        let installed = install(&paths, &request(&app, "0.3.0", &choice, &archive)).expect("installed");
        let was = PathBuf::from(&installed.dir);
        assert!(was.starts_with(&paths.apps));

        let elsewhere = tempfile::tempdir().expect("temp dir");
        let mut seen: Vec<(u64, Option<u64>)> = Vec::new();
        let mut progress = |done: u64, total: Option<u64>| seen.push((done, total));
        let moved = move_app(&paths, &app, elsewhere.path(), &mut progress).expect("moved");

        let now = PathBuf::from(&moved.dir);
        assert!(now.starts_with(elsewhere.path()), "{} is not under the new home", now.display());
        assert_eq!(moved.root.as_deref(), Some(elsewhere.path().join("photocraft").display().to_string().as_str()));
        assert_eq!(std::fs::read_to_string(now.join("ai.storyteller.photocraft.AppImage")).expect("read"), "version one");
        assert!(!was.exists(), "the old copy is gone");
        assert!(!paths.app_dir("photocraft").exists(), "and so is the directory that held it");

        // The launcher still runs the app from its new home.
        let target = std::fs::read_link(paths.bin.join("photocraft")).expect("launcher is a symlink");
        assert!(target.starts_with(elsewhere.path()), "{} still points at the old home", target.display());
        assert_eq!(std::fs::read_to_string(paths.bin.join("photocraft")).expect("through the launcher"), "version one");
        if cfg!(target_os = "linux") {
            let entry = std::fs::read_to_string(paths.data.join("applications/ai.storyteller.photocraft.desktop")).expect("entry");
            assert!(entry.contains(&format!("Exec=\"{}\"", target.display())), "{entry}");
        }
        assert!(!seen.is_empty(), "the move reported progress");
        assert_eq!(State::load(&paths.state).expect("state").get("photocraft").and_then(|i| i.root.clone()), moved.root);
    }

    #[cfg(unix)]
    #[test]
    fn an_app_that_has_been_moved_updates_in_place_and_removes_cleanly() {
        let root = tempfile::tempdir().expect("temp dir");
        let paths = Paths::rooted(root.path());
        let source = tempfile::tempdir().expect("temp dir");
        let app = app("photocraft");

        let first = appimage(source.path(), "version one");
        let c1 = choice("photocraft-0.3.0-linux-x86_64.AppImage", Format::AppImage);
        install(&paths, &request(&app, "0.3.0", &c1, &first)).expect("installed");

        let elsewhere = tempfile::tempdir().expect("temp dir");
        let mut progress = |_: u64, _: Option<u64>| {};
        let moved = move_app(&paths, &app, elsewhere.path(), &mut progress).expect("moved");
        let home = PathBuf::from(moved.root.clone().expect("a home"));

        // The install location is changed afterwards; the app that was moved keeps its own home.
        let changed = Paths::rooted(root.path()).with_apps(root.path().join("new-apps"));
        std::fs::write(source.path().join("photocraft-0.4.0-linux-x86_64.AppImage"), "version two").expect("write");
        let second = source.path().join("photocraft-0.4.0-linux-x86_64.AppImage");
        let c2 = choice("photocraft-0.4.0-linux-x86_64.AppImage", Format::AppImage);
        let updated = install(&changed, &Request { version: "0.4.0", ..request(&app, "0.4.0", &c2, &second) }).expect("updated");

        assert_eq!(updated.root.as_deref(), moved.root.as_deref(), "an update does not relocate an app");
        assert!(PathBuf::from(&updated.dir).starts_with(&home), "{} left its home", updated.dir);
        assert!(!PathBuf::from(&updated.dir).starts_with(&changed.apps), "the new location is for new apps only");

        remove(&changed, &app).expect("removed");
        assert!(!home.exists(), "remove deletes the home the app actually had");
    }

    #[test]
    fn an_appimage_round_trip_is_recorded_and_a_tampered_file_is_caught() {
        let root = tempfile::tempdir().expect("temp dir");
        let paths = Paths::rooted(root.path());
        let source = tempfile::tempdir().expect("temp dir");
        let archive = appimage(source.path(), "version one");
        let app = app("photocraft");
        let choice = choice("photocraft-0.3.0-linux-x86_64.AppImage", Format::AppImage);

        let installed = install(&paths, &request(&app, "0.3.0", &choice, &archive)).expect("installed");
        assert!(installed.manifest.is_some(), "the state record carries the manifest's own digest");
        assert!(paths.manifest("photocraft").is_file());

        let report = verify_installed(&paths, &app, &installed).expect("verified");
        assert_eq!(report.level(), Level::Ok);
        assert_eq!(report.listed, 1);

        let target = paths.version_dir("photocraft", "0.3.0").join("ai.storyteller.photocraft.AppImage");
        std::fs::write(&target, "version TWO").expect("tamper, same length as the original");
        let report = verify_installed(&paths, &app, &installed).expect("verified");
        assert_eq!(report.level(), Level::Modified);
        assert_eq!(report.modified, ["ai.storyteller.photocraft.AppImage"]);
    }

    /// For an AppImage, re-hashing the one installed file against the asset's own digest would
    /// have worked, because the install is a byte copy. For a tarball it never could: the bytes on
    /// disk are the unpacked tree, nothing like the archive they came out of, so this is the case
    /// a per-file manifest exists to cover.
    #[test]
    fn a_tarball_install_verifies_file_by_file() {
        let root = tempfile::tempdir().expect("temp dir");
        let paths = Paths::rooted(root.path());
        let source = tempfile::tempdir().expect("temp dir");
        let archive = source.path().join("photocraft-0.3.0-linux-x86_64.tar.gz");
        write_tar_gz(
            &archive,
            &[("photocraft-0.3.0-linux-x86_64/bin/photocraft", b"the app"), ("photocraft-0.3.0-linux-x86_64/bin/photocraft-cli", b"the cli")],
        );
        let app = app("photocraft");
        let choice = choice("photocraft-0.3.0-linux-x86_64.tar.gz", Format::TarGz);
        let installed = install(&paths, &request(&app, "0.3.0", &choice, &archive)).expect("installed");

        let report = verify_installed(&paths, &app, &installed).expect("verified");
        assert_eq!(report.level(), Level::Ok);
        assert_eq!(report.listed, 2, "every file the archive put in the version directory is accounted for");

        let binary = Path::new(&installed.dir).join("photocraft-0.3.0-linux-x86_64/bin/photocraft");
        std::fs::write(&binary, "the bpp").expect("tamper, same length as the original");
        let report = verify_installed(&paths, &app, &installed).expect("verified");
        assert_eq!(report.level(), Level::Modified);
        assert_eq!(report.modified, ["photocraft-0.3.0-linux-x86_64/bin/photocraft"]);
    }

    #[test]
    fn a_file_missing_from_a_tarball_install_is_incomplete_not_modified() {
        let root = tempfile::tempdir().expect("temp dir");
        let paths = Paths::rooted(root.path());
        let source = tempfile::tempdir().expect("temp dir");
        let archive = source.path().join("photocraft-0.3.0-linux-x86_64.tar.gz");
        write_tar_gz(
            &archive,
            &[("photocraft-0.3.0-linux-x86_64/bin/photocraft", b"the app"), ("photocraft-0.3.0-linux-x86_64/bin/photocraft-cli", b"the cli")],
        );
        let app = app("photocraft");
        let choice = choice("photocraft-0.3.0-linux-x86_64.tar.gz", Format::TarGz);
        let installed = install(&paths, &request(&app, "0.3.0", &choice, &archive)).expect("installed");

        let cli = Path::new(&installed.dir).join("photocraft-0.3.0-linux-x86_64/bin/photocraft-cli");
        std::fs::remove_file(&cli).expect("remove");

        let report = verify_installed(&paths, &app, &installed).expect("verified");
        assert_eq!(report.level(), Level::Incomplete);
        assert_eq!(report.missing, ["photocraft-0.3.0-linux-x86_64/bin/photocraft-cli"]);
        assert!(report.modified.is_empty());
    }

    /// `install_portable_zip` only writes a Start Menu shortcut on Windows, so the rest of it,
    /// including this check, runs here too.
    #[test]
    fn a_portable_zip_install_verifies() {
        let root = tempfile::tempdir().expect("temp dir");
        let paths = Paths::rooted(root.path());
        let source = tempfile::tempdir().expect("temp dir");
        let archive = source.path().join("photocraft-0.3.0-windows-x64-portable.zip");
        write_zip(&archive, &[("photocraft-0.3.0-windows-x64/photocraft.exe", b"MZ"), ("photocraft-0.3.0-windows-x64/resources/strings.json", b"{}")]);
        let app = app("photocraft");
        let choice = choice("photocraft-0.3.0-windows-x64-portable.zip", Format::PortableZip);
        let installed = install(&paths, &request(&app, "0.3.0", &choice, &archive)).expect("installed");
        assert_eq!(verify_installed(&paths, &app, &installed).expect("verified").level(), Level::Ok);

        let exe = Path::new(&installed.dir).join("photocraft-0.3.0-windows-x64/photocraft.exe");
        std::fs::write(&exe, "NO").expect("tamper, same length as the original");
        assert_eq!(verify_installed(&paths, &app, &installed).expect("verified").level(), Level::Modified);
    }

    /// `install_dmg` runs `hdiutil`, which this sandbox does not have, so the bundle an install
    /// would have produced is built by hand and recorded the same way `install` records one:
    /// `write_manifest` over the bundle, then an `Installed` whose `dir` and `root` both name it.
    #[test]
    fn a_hand_built_bundle_verifies_the_way_a_dmg_install_would() {
        let root = tempfile::tempdir().expect("temp dir");
        let paths = Paths::rooted(root.path());
        let bundle_root = tempfile::tempdir().expect("temp dir");
        let bundle = bundle_root.path().join("PhotoCraft.app");
        fs::mkdir_p(&bundle.join("Contents/MacOS")).expect("create");
        std::fs::write(bundle.join("Contents/MacOS/PhotoCraft"), "the app").expect("write");
        std::fs::write(bundle.join("Contents/Info.plist"), "<plist/>").expect("write");
        #[cfg(unix)]
        {
            fs::mkdir_p(&bundle.join("Contents/Frameworks")).expect("create");
            std::os::unix::fs::symlink("Versions/A", bundle.join("Contents/Frameworks/Current")).expect("link");
        }

        let digest = write_manifest(&paths, "photocraft", &bundle).expect("manifest written");
        let installed = Installed {
            version: "0.3.0".to_owned(),
            tag: "v0.3.0".to_owned(),
            asset: "PhotoCraft-0.3.0.dmg".to_owned(),
            sha256: "0".repeat(64),
            format: Format::Dmg,
            dir: bundle.display().to_string(),
            root: Some(bundle.display().to_string()),
            manifest: Some(digest),
            launcher: bundle.display().to_string(),
            installed_at: 0,
            previous: None,
        };
        let app = app("photocraft");

        assert_eq!(verify_installed(&paths, &app, &installed).expect("verified").level(), Level::Ok);

        std::fs::write(bundle.join("Contents/MacOS/PhotoCraft"), "the bpp").expect("tamper, same length as the original");
        let report = verify_installed(&paths, &app, &installed).expect("verified");
        assert_eq!(report.level(), Level::Modified);
        assert_eq!(report.modified, ["Contents/MacOS/PhotoCraft"]);
    }

    #[test]
    fn an_extra_file_is_reported_and_file_manager_noise_is_not() {
        let root = tempfile::tempdir().expect("temp dir");
        let paths = Paths::rooted(root.path());
        let source = tempfile::tempdir().expect("temp dir");
        let archive = source.path().join("photocraft-0.3.0-linux-x86_64.tar.gz");
        write_tar_gz(
            &archive,
            &[("photocraft-0.3.0-linux-x86_64/bin/photocraft", b"the app"), ("photocraft-0.3.0-linux-x86_64/bin/photocraft-cli", b"the cli")],
        );
        let app = app("photocraft");
        let choice = choice("photocraft-0.3.0-linux-x86_64.tar.gz", Format::TarGz);
        let installed = install(&paths, &request(&app, "0.3.0", &choice, &archive)).expect("installed");

        std::fs::write(Path::new(&installed.dir).join("photocraft.log"), "yesterday").expect("write");
        std::fs::write(Path::new(&installed.dir).join(".DS_Store"), "noise").expect("write");

        let report = verify_installed(&paths, &app, &installed).expect("verified");
        assert_eq!(report.level(), Level::Ok, "an extra file is reported, not failed");
        assert_eq!(report.extra, ["photocraft.log"]);
        assert!(!report.extra.iter().any(|path| path.contains("DS_Store")), "{:?}", report.extra);
    }

    /// A record from before CraftCenter wrote manifests at all has no digest to check against —
    /// that reads as "cannot tell", never as a tampered install.
    #[test]
    fn a_pre_manifest_record_is_not_verifiable() {
        let root = tempfile::tempdir().expect("temp dir");
        let paths = Paths::rooted(root.path());
        let source = tempfile::tempdir().expect("temp dir");
        let archive = appimage(source.path(), "version one");
        let app = app("photocraft");
        let choice = choice("photocraft-0.3.0-linux-x86_64.AppImage", Format::AppImage);
        install(&paths, &request(&app, "0.3.0", &choice, &archive)).expect("installed");

        let mut state = State::load(&paths.state).expect("state");
        state.installed.get_mut("photocraft").expect("entry").manifest = None;
        state.save(&paths.state).expect("saved");
        let installed = State::load(&paths.state).expect("state").get("photocraft").cloned().expect("entry");

        let report = verify_installed(&paths, &app, &installed).expect("verified");
        assert_eq!(report.level(), Level::NotVerifiable);
        assert!(report.summary().contains("reinstall"), "{}", report.summary());
    }

    #[test]
    fn a_deleted_manifest_is_not_verifiable() {
        let root = tempfile::tempdir().expect("temp dir");
        let paths = Paths::rooted(root.path());
        let source = tempfile::tempdir().expect("temp dir");
        let archive = appimage(source.path(), "version one");
        let app = app("photocraft");
        let choice = choice("photocraft-0.3.0-linux-x86_64.AppImage", Format::AppImage);
        let installed = install(&paths, &request(&app, "0.3.0", &choice, &archive)).expect("installed");

        std::fs::remove_file(paths.manifest("photocraft")).expect("remove");

        let report = verify_installed(&paths, &app, &installed).expect("verified");
        assert_eq!(report.level(), Level::NotVerifiable);
        assert!(report.summary().contains("missing"), "{}", report.summary());
    }

    /// The most important test in this file. The digest the state record carries of the manifest
    /// is what catches a manifest swapped for one that matches a tampered tree; without it, a hand
    /// that can rewrite the installed files could rewrite the manifest beside them to match and
    /// Verify would say everything was fine. Be plain about what this is and is not: it is not a
    /// signature — both files are the user's own and the same hand can rewrite either — it only
    /// checks that the two still agree with each other.
    #[test]
    fn a_manifest_rewritten_to_match_a_tampered_tree_is_still_not_accepted() {
        let root = tempfile::tempdir().expect("temp dir");
        let paths = Paths::rooted(root.path());
        let source = tempfile::tempdir().expect("temp dir");
        let archive = appimage(source.path(), "version one");
        let app = app("photocraft");
        let choice = choice("photocraft-0.3.0-linux-x86_64.AppImage", Format::AppImage);
        let installed = install(&paths, &request(&app, "0.3.0", &choice, &archive)).expect("installed");

        let target = Path::new(&installed.dir).join("ai.storyteller.photocraft.AppImage");
        std::fs::write(&target, "version TWO").expect("tamper, same length as the original");
        let forged = Manifest::of_tree(Path::new(&installed.dir)).expect("walked").to_json().expect("serialised");
        std::fs::write(paths.manifest("photocraft"), forged).expect("write the forged manifest over the real one");

        let report = verify_installed(&paths, &app, &installed).expect("verified");
        assert_eq!(report.level(), Level::NotVerifiable);
        assert!(report.summary().contains("not the one written when it was installed"), "{}", report.summary());
    }

    #[test]
    fn an_update_rewrites_the_manifest_for_the_new_version() {
        let root = tempfile::tempdir().expect("temp dir");
        let paths = Paths::rooted(root.path());
        let source = tempfile::tempdir().expect("temp dir");
        let app = app("photocraft");

        let first = appimage(source.path(), "version one");
        let c1 = choice("photocraft-0.3.0-linux-x86_64.AppImage", Format::AppImage);
        install(&paths, &request(&app, "0.3.0", &c1, &first)).expect("first install");

        let second = source.path().join("photocraft-0.4.0-linux-x86_64.AppImage");
        std::fs::write(&second, "version two").expect("write");
        let c2 = choice("photocraft-0.4.0-linux-x86_64.AppImage", Format::AppImage);
        let updated = install(&paths, &request(&app, "0.4.0", &c2, &second)).expect("upgrade");

        assert_eq!(verify_installed(&paths, &app, &updated).expect("verified").level(), Level::Ok);
        let text = std::fs::read_to_string(paths.manifest("photocraft")).expect("read");
        assert!(!text.contains("0.3.0"), "the old version directory left nothing behind: {text}");
    }

    #[test]
    fn remove_takes_the_manifest_away_too() {
        let root = tempfile::tempdir().expect("temp dir");
        let paths = Paths::rooted(root.path());
        let source = tempfile::tempdir().expect("temp dir");
        let archive = appimage(source.path(), "version one");
        let app = app("photocraft");
        let choice = choice("photocraft-0.3.0-linux-x86_64.AppImage", Format::AppImage);
        install(&paths, &request(&app, "0.3.0", &choice, &archive)).expect("installed");
        assert!(paths.manifest("photocraft").is_file());

        remove(&paths, &app).expect("removed");
        assert!(!paths.manifest("photocraft").exists());
    }

    /// The whole tree being gone is an answer the manifest can give on its own, with no special
    /// case: every path it lists simply is not there.
    #[test]
    fn a_deleted_install_directory_reports_everything_as_missing() {
        let root = tempfile::tempdir().expect("temp dir");
        let paths = Paths::rooted(root.path());
        let source = tempfile::tempdir().expect("temp dir");
        let archive = appimage(source.path(), "version one");
        let app = app("photocraft");
        let choice = choice("photocraft-0.3.0-linux-x86_64.AppImage", Format::AppImage);
        let installed = install(&paths, &request(&app, "0.3.0", &choice, &archive)).expect("installed");

        std::fs::remove_dir_all(Path::new(&installed.dir)).expect("remove");

        let report = verify_installed(&paths, &app, &installed).expect("verified");
        assert_eq!(report.level(), Level::Incomplete);
        assert_eq!(report.missing, ["ai.storyteller.photocraft.AppImage"]);
    }

    #[cfg(unix)]
    #[test]
    fn a_move_refuses_to_carry_a_tampered_tree() {
        let root = tempfile::tempdir().expect("temp dir");
        let paths = Paths::rooted(root.path());
        let source = tempfile::tempdir().expect("temp dir");
        let archive = appimage(source.path(), "version one");
        let app = app("photocraft");
        let choice = choice("photocraft-0.3.0-linux-x86_64.AppImage", Format::AppImage);
        let installed = install(&paths, &request(&app, "0.3.0", &choice, &archive)).expect("installed");
        let home = PathBuf::from(installed.root.clone().expect("a home"));

        let target = Path::new(&installed.dir).join("ai.storyteller.photocraft.AppImage");
        std::fs::write(&target, "version TWO").expect("tamper, same length as the original");

        let elsewhere = tempfile::tempdir().expect("temp dir");
        let mut progress = |_: u64, _: Option<u64>| {};
        let result = move_app(&paths, &app, elsewhere.path(), &mut progress);
        assert!(matches!(result, Err(Error::NotAsInstalled { .. })), "{result:?}");

        assert!(home.exists(), "the original tree is still there");
        assert!(!elsewhere.path().join("photocraft").exists(), "nothing was moved to the destination");
        assert_eq!(
            State::load(&paths.state).expect("state").get("photocraft").and_then(|i| i.root.clone()),
            Some(home.display().to_string()),
            "the record still points at the old home"
        );
    }

    /// The manifest's paths are relative to the installed tree, which is what lets a move carry
    /// the record's meaning along with it: an intact tree verifies just as well after the move as
    /// before it.
    #[cfg(unix)]
    #[test]
    fn moving_an_intact_tree_still_verifies_afterwards() {
        let root = tempfile::tempdir().expect("temp dir");
        let paths = Paths::rooted(root.path());
        let source = tempfile::tempdir().expect("temp dir");
        let archive = appimage(source.path(), "version one");
        let app = app("photocraft");
        let choice = choice("photocraft-0.3.0-linux-x86_64.AppImage", Format::AppImage);
        install(&paths, &request(&app, "0.3.0", &choice, &archive)).expect("installed");

        let elsewhere = tempfile::tempdir().expect("temp dir");
        let mut progress = |_: u64, _: Option<u64>| {};
        let moved = move_app(&paths, &app, elsewhere.path(), &mut progress).expect("moved");

        assert_eq!(verify_installed(&paths, &app, &moved).expect("verified").level(), Level::Ok);
    }

    #[test]
    fn craftcenters_own_build_records_and_checks_one_file() {
        let root = tempfile::tempdir().expect("temp dir");
        let paths = Paths::rooted(root.path());
        let dir = tempfile::tempdir().expect("temp dir");
        let exe = dir.path().join("craftcenter");
        std::fs::write(&exe, "the program").expect("write");

        record_self(&paths, "craftcenter", &exe).expect("recorded");
        assert_eq!(verify_self(&paths, "craftcenter", &exe).expect("verified").level(), Level::Ok);

        std::fs::write(dir.path().join("unrelated"), "not ours").expect("write");
        assert!(verify_self(&paths, "craftcenter", &exe).expect("verified").extra.is_empty(), "a listed-only check does not go looking for extras");

        std::fs::write(&exe, "tampered").expect("tamper");
        assert_eq!(verify_self(&paths, "craftcenter", &exe).expect("verified").level(), Level::Modified);

        assert_eq!(
            verify_self(&paths, "someone-else", &exe).expect("verified").level(),
            Level::NotVerifiable,
            "no record at all reads as nothing to check, not as tampering"
        );
    }
}
