//! Filesystem helpers: unpacking that cannot escape its destination, and pointer flips that are
//! atomic.

use std::io;
use std::path::{Component, Path, PathBuf};

use crate::Error;

pub(crate) fn io_err(path: &Path) -> impl Fn(io::Error) -> Error {
    let path = path.display().to_string();
    move |source| Error::Io { path: path.clone(), source }
}

pub fn mkdir_p(dir: &Path) -> Result<(), Error> {
    std::fs::create_dir_all(dir).map_err(io_err(dir))
}

/// Join `base` and a path that came out of an archive, refusing anything that would escape.
///
/// Archive entry names are attacker-controlled input: `../../.bashrc`, an absolute path, or a
/// Windows drive letter must not be able to write outside the destination. Returns `None` for an
/// entry that is not safe to extract, and the caller skips it.
pub fn safe_join(base: &Path, entry: &Path) -> Option<PathBuf> {
    let mut out = base.to_path_buf();
    for component in entry.components() {
        match component {
            Component::Normal(part) => {
                // A name containing a separator would already have been split into components, so
                // what remains to reject is only the traversal and root forms below.
                out.push(part);
            }
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    // Belt and braces: the result must still be under base.
    if out.starts_with(base) { Some(out) } else { None }
}

/// Point `link` at `target`, replacing whatever was there, without a window in which `link` does
/// not exist.
///
/// A symlink cannot be overwritten in place, so the new one is created under a temporary name and
/// renamed over the old. `rename` is atomic on every platform CraftCenter installs on, so a
/// crash leaves either the old pointer or the new one and never neither.
#[cfg(unix)]
pub fn flip_symlink(link: &Path, target: &Path) -> Result<(), Error> {
    if let Some(parent) = link.parent() {
        mkdir_p(parent)?;
    }
    let staging = link.with_file_name(format!(".{}.new", link.file_name().and_then(|n| n.to_str()).unwrap_or("link")));
    let _ = std::fs::remove_file(&staging);
    std::os::unix::fs::symlink(target, &staging).map_err(io_err(&staging))?;
    std::fs::rename(&staging, link).map_err(io_err(link))
}

/// Windows has no dependable unprivileged symlink, so the pointer is a copy of the launcher.
#[cfg(not(unix))]
pub fn flip_symlink(link: &Path, target: &Path) -> Result<(), Error> {
    if let Some(parent) = link.parent() {
        mkdir_p(parent)?;
    }
    let staging = link.with_extension("new");
    std::fs::copy(target, &staging).map_err(io_err(&staging))?;
    std::fs::rename(&staging, link).map_err(io_err(link))
}

/// Make a file executable by its owner. A no-op off Unix.
#[cfg(unix)]
pub fn make_executable(path: &Path) -> Result<(), Error> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path).map_err(io_err(path))?.permissions();
    perms.set_mode(perms.mode() | 0o755);
    std::fs::set_permissions(path, perms).map_err(io_err(path))
}

#[cfg(not(unix))]
pub fn make_executable(_path: &Path) -> Result<(), Error> {
    Ok(())
}

/// Write a file by writing a neighbour and renaming it into place.
pub fn write_atomic(path: &Path, contents: &[u8]) -> Result<(), Error> {
    if let Some(parent) = path.parent() {
        mkdir_p(parent)?;
    }
    let temp = path.with_extension("tmp-new");
    std::fs::write(&temp, contents).map_err(io_err(&temp))?;
    std::fs::rename(&temp, path).map_err(io_err(path))
}

/// Unpack a `.tar.gz` into `dest`, skipping anything that would escape it.
pub fn unpack_tar_gz(archive: &Path, dest: &Path) -> Result<Vec<PathBuf>, Error> {
    let file = std::fs::File::open(archive).map_err(io_err(archive))?;
    let decoder = flate2::read::GzDecoder::new(std::io::BufReader::new(file));
    let mut tar = tar::Archive::new(decoder);
    mkdir_p(dest)?;
    let mut written = Vec::new();
    for entry in tar.entries().map_err(io_err(archive))? {
        let mut entry = entry.map_err(io_err(archive))?;
        let path = entry.path().map_err(io_err(archive))?.to_path_buf();
        let Some(out) = safe_join(dest, &path) else {
            return Err(Error::UnsafeEntry { archive: archive.display().to_string(), entry: path.display().to_string() });
        };
        // A tar stream need not carry a directory entry before the files inside it.
        if let Some(parent) = out.parent() {
            mkdir_p(parent)?;
        }
        entry.unpack(&out).map_err(io_err(&out))?;
        written.push(out);
    }
    Ok(written)
}

/// Unpack a `.zip` into `dest`, skipping anything that would escape it.
pub fn unpack_zip(archive: &Path, dest: &Path) -> Result<Vec<PathBuf>, Error> {
    let file = std::fs::File::open(archive).map_err(io_err(archive))?;
    let mut zip =
        zip::ZipArchive::new(std::io::BufReader::new(file)).map_err(|e| Error::Archive { archive: archive.display().to_string(), message: e.to_string() })?;
    mkdir_p(dest)?;
    let mut written = Vec::new();
    for index in 0..zip.len() {
        let mut entry = zip.by_index(index).map_err(|e| Error::Archive { archive: archive.display().to_string(), message: e.to_string() })?;
        let name = entry.name().to_owned();
        let Some(out) = safe_join(dest, Path::new(&name)) else {
            return Err(Error::UnsafeEntry { archive: archive.display().to_string(), entry: name });
        };
        if entry.is_dir() {
            mkdir_p(&out)?;
            continue;
        }
        if let Some(parent) = out.parent() {
            mkdir_p(parent)?;
        }
        let mut sink = std::fs::File::create(&out).map_err(io_err(&out))?;
        std::io::copy(&mut entry, &mut sink).map_err(io_err(&out))?;
        #[cfg(unix)]
        if let Some(mode) = entry.unix_mode() {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&out, std::fs::Permissions::from_mode(mode));
        }
        written.push(out);
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_join_accepts_ordinary_entries() {
        let base = Path::new("/dest");
        assert_eq!(safe_join(base, Path::new("bin/photocraft")), Some(PathBuf::from("/dest/bin/photocraft")));
        assert_eq!(safe_join(base, Path::new("./share/x")), Some(PathBuf::from("/dest/share/x")));
    }

    #[test]
    fn safe_join_refuses_everything_that_would_escape() {
        let base = Path::new("/dest");
        for hostile in ["../outside", "a/../../outside", "/etc/passwd", "/", "../"] {
            assert_eq!(safe_join(base, Path::new(hostile)), None, "{hostile} was not refused");
        }
    }

    #[test]
    fn write_atomic_leaves_no_temporary_behind() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("sub/file.txt");
        write_atomic(&path, b"hello").expect("written");
        assert_eq!(std::fs::read_to_string(&path).expect("read"), "hello");
        assert!(!path.with_extension("tmp-new").exists());
    }

    #[cfg(unix)]
    #[test]
    fn flipping_a_symlink_never_leaves_it_missing() {
        let dir = tempfile::tempdir().expect("temp dir");
        let old = dir.path().join("v1");
        let new = dir.path().join("v2");
        std::fs::write(&old, "one").expect("write");
        std::fs::write(&new, "two").expect("write");
        let link = dir.path().join("bin/app");

        flip_symlink(&link, &old).expect("first");
        assert_eq!(std::fs::read_to_string(&link).expect("read"), "one");
        flip_symlink(&link, &new).expect("flip");
        assert_eq!(std::fs::read_to_string(&link).expect("read"), "two");
        // No staging file is left in the directory.
        let leftovers: Vec<_> = std::fs::read_dir(link.parent().expect("parent"))
            .expect("read dir")
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().starts_with('.'))
            .collect();
        assert!(leftovers.is_empty(), "staging file left behind");
    }

    fn tar_gz_with(entries: &[(&str, &[u8])]) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("archive.tar.gz");
        let file = std::fs::File::create(&path).expect("create");
        let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::fast());
        let mut builder = tar::Builder::new(encoder);
        for (name, bytes) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(bytes.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder.append_data(&mut header, name, *bytes).expect("append");
        }
        builder.into_inner().expect("finish").finish().expect("flush");
        (dir, path)
    }

    #[test]
    fn unpacks_a_relocatable_fhs_tree() {
        // The shape of a real `<app>-<version>-linux-<arch>.tar.gz`: one top-level directory
        // holding the contents of usr/.
        let (_dir, archive) = tar_gz_with(&[
            ("photocraft-0.3.0-linux-x86_64/bin/photocraft", b"ELF"),
            ("photocraft-0.3.0-linux-x86_64/share/applications/ai.storyteller.photocraft.desktop", b"[Desktop Entry]"),
        ]);
        let dest = tempfile::tempdir().expect("temp dir");
        unpack_tar_gz(&archive, dest.path()).expect("unpacked");
        assert!(dest.path().join("photocraft-0.3.0-linux-x86_64/bin/photocraft").is_file());
        assert!(dest.path().join("photocraft-0.3.0-linux-x86_64/share/applications/ai.storyteller.photocraft.desktop").is_file());
    }

    #[test]
    fn an_archive_entry_that_escapes_is_refused_and_writes_nothing() {
        // Written as a zip because the `tar` crate's builder refuses to put `..` in an archive at
        // all; a zip can carry any name, which is exactly the hostile input to guard against.
        let dir = tempfile::tempdir().expect("temp dir");
        let archive = dir.path().join("hostile.zip");
        {
            let file = std::fs::File::create(&archive).expect("create");
            let mut writer = zip::ZipWriter::new(file);
            let options: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default();
            writer.start_file("../escaped", options).expect("entry");
            std::io::Write::write_all(&mut writer, b"nope").expect("write");
            writer.finish().expect("finish");
        }
        let dest = tempfile::tempdir().expect("temp dir");
        let result = unpack_zip(&archive, dest.path());
        assert!(matches!(result, Err(Error::UnsafeEntry { .. })), "{result:?}");
        assert!(!dest.path().join("../escaped").exists(), "the entry was written outside the destination");
    }

    #[test]
    fn unpacks_an_ordinary_zip() {
        let dir = tempfile::tempdir().expect("temp dir");
        let archive = dir.path().join("portable.zip");
        {
            let file = std::fs::File::create(&archive).expect("create");
            let mut writer = zip::ZipWriter::new(file);
            let options: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default();
            writer.start_file("photocraft/photocraft.exe", options).expect("entry");
            std::io::Write::write_all(&mut writer, b"MZ").expect("write");
            writer.finish().expect("finish");
        }
        let dest = tempfile::tempdir().expect("temp dir");
        unpack_zip(&archive, dest.path()).expect("unpacked");
        assert_eq!(std::fs::read_to_string(dest.path().join("photocraft/photocraft.exe")).expect("read"), "MZ");
    }
}
