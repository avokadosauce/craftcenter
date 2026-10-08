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
mod state;

use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use craftcenter_catalogue::App;
use craftcenter_select::{Choice, Format};

pub use paths::Paths;
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

    // Keep the version that was there until the new one has been launched once.
    let installed = Installed { previous: previous.as_ref().map(|p| p.dir.clone()).filter(|d| d != &installed.dir), ..installed };

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
    let _ = std::fs::remove_dir_all(&mount);
    fs::mkdir_p(&mount)?;

    run("hdiutil", &["attach", "-nobrowse", "-readonly", "-noautoopen", "-mountpoint", &mount.display().to_string(), &request.archive.display().to_string()])?;

    let result = (|| {
        let bundle =
            first_bundle(&mount)?.ok_or_else(|| Error::NotInArchive { asset: request.choice.asset.clone(), what: "application bundle (.app)".to_owned() })?;
        let name = bundle.file_name().ok_or_else(|| Error::NotInArchive { asset: request.choice.asset.clone(), what: "named bundle".to_owned() })?;
        // A bundle that is already installed is replaced where it stands, wherever that is.
        let destination = previous.and_then(|installed| installed.root.clone()).map(PathBuf::from).unwrap_or_else(|| paths.apps.join(name));
        let home = destination.parent().unwrap_or(&paths.apps).to_path_buf();
        fs::mkdir_p(&home)?;
        let staged = home.join(format!(".{}.new", name.to_string_lossy()));
        let _ = std::fs::remove_dir_all(&staged);
        copy_tree(&bundle, &staged)?;

        let retired = destination.with_file_name(format!(".{}.old", name.to_string_lossy()));
        let _ = std::fs::remove_dir_all(&retired);
        if destination.exists() {
            std::fs::rename(&destination, &retired).map_err(fs::io_err(&destination))?;
        }
        std::fs::rename(&staged, &destination).map_err(fs::io_err(&destination))?;
        let _ = std::fs::remove_dir_all(&retired);
        Ok(record(request, &destination, &destination, &destination))
    })();

    let _ = run("hdiutil", &["detach", &mount.display().to_string()]);
    let _ = std::fs::remove_dir_all(&mount);
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
    let total = tree_size(&from)?;
    let mut copied = 0u64;
    let result = copy_tree_verified(&from, &staged, &mut copied, total, progress);
    if let Err(error) = result {
        let _ = std::fs::remove_dir_all(&staged);
        return Err(error);
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

/// Copy a tree, hashing every file on both sides and reporting progress in bytes.
///
/// Verifying the copy is the point of doing it this way: a move is the one thing in this program
/// that can lose the only copy of something, so nothing is believed until it has been read back.
fn copy_tree_verified(from: &Path, to: &Path, copied: &mut u64, total: u64, progress: Progress<'_>) -> Result<(), Error> {
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
        return std::os::unix::fs::symlink(&target, to).map_err(fs::io_err(to));
    }

    if meta.is_dir() {
        fs::mkdir_p(to)?;
        for entry in std::fs::read_dir(from).map_err(fs::io_err(from))? {
            let entry = entry.map_err(fs::io_err(from))?;
            copy_tree_verified(&entry.path(), &to.join(entry.file_name()), copied, total, progress)?;
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
    *copied = copied.saturating_add(meta.len());
    progress(*copied, Some(total));
    Ok(())
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

/// Replace the running program with a newer build of itself.
///
/// The swap is two renames: the running file is moved aside and the new one takes its place. That
/// order is what makes it work on all three platforms — Unix lets a running image be renamed or
/// unlinked because the kernel holds the inode, and Windows refuses to *delete* a running image
/// but allows it to be *renamed*. The moved-aside file is deleted immediately where that is
/// permitted, and by [`clean_after_self_update`] on the next start where it is not.
///
/// The caller restarts the program; this function does not, because only the caller knows whether
/// it is safe to.
pub fn self_replace(current_exe: &Path, staged: &Path) -> Result<(), Error> {
    fs::make_executable(staged)?;
    let retired = retired_path(current_exe);
    let _ = std::fs::remove_file(&retired);
    std::fs::rename(current_exe, &retired).map_err(fs::io_err(current_exe))?;

    if let Err(error) = std::fs::rename(staged, current_exe) {
        // Put the running program back rather than leaving the user with nothing to run.
        let _ = std::fs::rename(&retired, current_exe);
        return Err(Error::Io { path: current_exe.display().to_string(), source: error });
    }
    // Succeeds on Unix; on Windows it fails while the old image is still mapped, and the next
    // start clears it.
    let _ = std::fs::remove_file(&retired);
    Ok(())
}

fn retired_path(current_exe: &Path) -> PathBuf {
    let name = current_exe.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "craftcenter".to_owned());
    current_exe.with_file_name(format!(".{name}.old"))
}

/// Delete the previous build left behind by [`self_replace`]. Call it once at startup; it does
/// nothing when there is nothing to clean.
pub fn clean_after_self_update(current_exe: &Path) {
    let _ = std::fs::remove_file(retired_path(current_exe));
}

#[cfg(test)]
mod tests {
    use craftcenter_catalogue::Catalogue;
    use craftcenter_select::Note;

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

    #[test]
    fn self_replace_swaps_the_running_file_and_cleans_up() {
        let dir = tempfile::tempdir().expect("temp dir");
        let exe = dir.path().join("craftcenter");
        let staged = dir.path().join("craftcenter.new");
        std::fs::write(&exe, "old build").expect("write");
        std::fs::write(&staged, "new build").expect("write");

        self_replace(&exe, &staged).expect("replaced");
        assert_eq!(std::fs::read_to_string(&exe).expect("read"), "new build");
        assert!(!staged.exists());
        clean_after_self_update(&exe);
        assert!(!retired_path(&exe).exists());
    }

    #[test]
    fn a_failed_self_replace_puts_the_old_program_back() {
        let dir = tempfile::tempdir().expect("temp dir");
        let exe = dir.path().join("craftcenter");
        std::fs::write(&exe, "old build").expect("write");
        // Nothing was staged, so the second rename fails.
        let result = self_replace(&exe, &dir.path().join("absent"));
        assert!(result.is_err());
        assert_eq!(std::fs::read_to_string(&exe).expect("still runnable"), "old build");
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
        let from = Path::new("/one/apps/photocraft");
        let to = Path::new("/two/photocraft");
        assert_eq!(repoint("/one/apps/photocraft/0.3.0", from, to), "/two/photocraft/0.3.0");
        assert_eq!(repoint("/one/apps/photocraft", from, to), "/two/photocraft");
        // A launcher in the user's bin is not inside the tree and must not be rewritten.
        assert_eq!(repoint("/home/example/.local/bin/photocraft", from, to), "/home/example/.local/bin/photocraft");
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
}
