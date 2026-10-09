//! Telling a program from an archive by its first bytes.
//!
//! This exists because of a real failure. Until 0.2.1 a self-update renamed the *downloaded
//! asset* over the running program, so a Mac that pressed Update ended up with a disk image at
//! `CraftCenter.app/Contents/MacOS/craftcenter` and a window that would never open again — the
//! system's answer was "this application is not supported on this Mac". Each format is now
//! unpacked before anything is swapped, and this module is the guard that would have caught the
//! mistake anyway: four bytes read from whatever is about to be renamed over the running build,
//! refused unless they are the program image this machine runs.
//!
//! It is a sanity check and not a signature. It says "this is shaped like a program for this
//! platform", which is exactly what the old code got wrong; *who* built it is the business of
//! `SHA256SUMS.txt` and of the platform's own notarisation.

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use crate::{Error, fs};

/// How much of the head is needed: a PE's own offset field sits at 0x3c.
const HEAD: u64 = 64;

/// A UDIF disk image is identified by the last 512 bytes of the file.
const TRAILER: u64 = 512;

/// The Mach-O magics: 32- and 64-bit, universal ("fat"), each in both byte orders, because the
/// magic is written in the order the target reads it in.
const MACH_O: [[u8; 4]; 8] = [
    [0xfe, 0xed, 0xfa, 0xce],
    [0xfe, 0xed, 0xfa, 0xcf],
    [0xce, 0xfa, 0xed, 0xfe],
    [0xcf, 0xfa, 0xed, 0xfe],
    [0xca, 0xfe, 0xba, 0xbe],
    [0xca, 0xfe, 0xba, 0xbf],
    [0xbe, 0xba, 0xfe, 0xca],
    [0xbf, 0xba, 0xfe, 0xca],
];

/// What a file's bytes say it is — only the shapes this program has to tell apart.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Shape {
    /// A macOS program, thin or universal.
    MachO,
    /// A Linux program. An AppImage is one of these with a filesystem appended, which is why the
    /// AppImage path needs no special case.
    Elf,
    /// A Windows program.
    Pe,
    /// A macOS disk image: what a release publishes, and what 0.2.0 wrote over the program.
    Dmg,
    /// A zip: the Windows portable build, and what 0.2.0 wrote over `craftcenter.exe`.
    Zip,
    /// A `.tar.gz`.
    Gzip,
    Unrecognised,
}

impl Shape {
    /// How to name it in a sentence someone has to act on.
    fn label(self) -> &'static str {
        match self {
            Shape::MachO => "a macOS program",
            Shape::Elf => "a Linux program",
            Shape::Pe => "a Windows program",
            Shape::Dmg => "a disk image",
            Shape::Zip => "a zip archive",
            Shape::Gzip => "a compressed archive",
            Shape::Unrecognised => "not anything CraftCenter recognises",
        }
    }
}

/// The shape a program this machine can run has.
pub(crate) const fn host() -> Shape {
    if cfg!(target_os = "macos") {
        Shape::MachO
    } else if cfg!(target_os = "windows") {
        Shape::Pe
    } else {
        Shape::Elf
    }
}

/// Refuse `path` unless it is a program image this machine can run.
///
/// Run before every rename over the running program, whatever format it came out of.
pub fn check_is_program(path: &Path) -> Result<(), Error> {
    check_shape(path, host())
}

/// The same check against a stated expectation, so a test can exercise every magic rather than
/// only the one belonging to the machine it happens to run on.
pub(crate) fn check_shape(path: &Path, wanted: Shape) -> Result<(), Error> {
    let found = shape_of(path)?;
    if found == wanted {
        return Ok(());
    }
    Err(Error::NotAProgram { path: path.display().to_string(), found: found.label() })
}

pub(crate) fn shape_of(path: &Path) -> Result<Shape, Error> {
    let mut file = std::fs::File::open(path).map_err(fs::io_err(path))?;
    let len = file.metadata().map_err(fs::io_err(path))?.len();

    let mut head = Vec::new();
    file.by_ref().take(HEAD).read_to_end(&mut head).map_err(fs::io_err(path))?;

    // The trailer is read first because what a disk image *starts* with is compressed data and
    // could be anything at all, while a UDIF image always ends in a 512-byte block beginning
    // `koly`. `hdiutil convert -format UDZO`, which is how the release's DMG is made, writes one.
    if len >= TRAILER && trailer_is_koly(&mut file, len) {
        return Ok(Shape::Dmg);
    }
    if head.starts_with(&[0x7f, b'E', b'L', b'F']) {
        return Ok(Shape::Elf);
    }
    if MACH_O.iter().any(|magic| head.starts_with(magic)) {
        return Ok(Shape::MachO);
    }
    // A bare `MZ` is a 16-bit DOS program, which no build of this could be; the DOS header's
    // last field says where the real signature is.
    if head.starts_with(b"MZ") && pe_signature(&mut file, &head) {
        return Ok(Shape::Pe);
    }
    if head.starts_with(b"PK") {
        return Ok(Shape::Zip);
    }
    if head.starts_with(&[0x1f, 0x8b]) {
        return Ok(Shape::Gzip);
    }
    Ok(Shape::Unrecognised)
}

fn trailer_is_koly(file: &mut std::fs::File, len: u64) -> bool {
    if file.seek(SeekFrom::Start(len.saturating_sub(TRAILER))).is_err() {
        return false;
    }
    let mut trailer = [0u8; 4];
    file.read_exact(&mut trailer).is_ok() && &trailer == b"koly"
}

fn pe_signature(file: &mut std::fs::File, head: &[u8]) -> bool {
    let Some(field) = head.get(0x3c..0x40).and_then(|bytes| bytes.first_chunk::<4>()).copied() else {
        return false;
    };
    if file.seek(SeekFrom::Start(u32::from_le_bytes(field).into())).is_err() {
        return false;
    }
    let mut signature = [0u8; 4];
    file.read_exact(&mut signature).is_ok() && &signature == b"PE\0\0"
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn written(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, bytes).expect("write");
        path
    }

    /// A DOS header whose `e_lfanew` field points at `entry`.
    fn dos_header(entry: u32) -> Vec<u8> {
        let mut bytes = vec![0u8; 64];
        bytes[0..2].copy_from_slice(b"MZ");
        bytes[0x3c..0x40].copy_from_slice(&entry.to_le_bytes());
        bytes
    }

    fn windows_program() -> Vec<u8> {
        let mut bytes = dos_header(64);
        bytes.extend_from_slice(b"PE\0\0");
        bytes
    }

    /// A file ending in the `koly` trailer `hdiutil` writes.
    fn disk_image() -> Vec<u8> {
        let mut bytes = vec![0u8; 1024];
        bytes[512..516].copy_from_slice(b"koly");
        bytes
    }

    #[test]
    fn every_program_magic_is_recognised() {
        let dir = tempfile::tempdir().expect("temp dir");
        for (index, magic) in MACH_O.iter().enumerate() {
            let path = written(dir.path(), &format!("macho-{index}"), magic);
            assert_eq!(shape_of(&path).expect("read"), Shape::MachO, "{magic:02x?} was not read as a Mach-O");
        }
        let elf = written(dir.path(), "elf", b"\x7fELF\x02\x01\x01\x00");
        assert_eq!(shape_of(&elf).expect("read"), Shape::Elf);
        let pe = written(dir.path(), "pe", &windows_program());
        assert_eq!(shape_of(&pe).expect("read"), Shape::Pe);
    }

    #[test]
    fn an_archive_is_never_read_as_a_program() {
        let dir = tempfile::tempdir().expect("temp dir");
        let cases = [
            ("craftcenter-0.2.0-macos-universal.dmg", disk_image(), Shape::Dmg),
            ("craftcenter-0.2.0-windows-x64-portable.zip", b"PK\x03\x04\x14\x00\x00\x00".to_vec(), Shape::Zip),
            ("craftcenter-0.2.0-linux-x86_64.tar.gz", b"\x1f\x8b\x08\x00".to_vec(), Shape::Gzip),
            ("notes.txt", b"CraftCenter - portable build\n".to_vec(), Shape::Unrecognised),
            ("script", b"#!/bin/sh\nexec craftcenter\n".to_vec(), Shape::Unrecognised),
            ("empty", Vec::new(), Shape::Unrecognised),
            // `MZ` with nothing a loader could use: a DOS program, not a Windows one.
            ("dos", dos_header(0), Shape::Unrecognised),
        ];
        for (name, bytes, expected) in cases {
            let path = written(dir.path(), name, &bytes);
            assert_eq!(shape_of(&path).expect("read"), expected, "{name} was read wrongly");
        }
    }

    #[test]
    fn a_program_for_another_platform_is_refused_as_plainly_as_an_archive_is() {
        let dir = tempfile::tempdir().expect("temp dir");
        let elf = written(dir.path(), "craftcenter", b"\x7fELF\x02\x01\x01\x00");
        check_shape(&elf, Shape::Elf).expect("a Linux program on Linux");
        let error = check_shape(&elf, Shape::MachO).expect_err("a Linux program is not a macOS one");
        assert!(error.to_string().contains("a Linux program"), "{error}");

        // The failure this release fixes: the downloaded asset, presented as the program.
        let dmg = written(dir.path(), "craftcenter-0.2.0-macos-universal.dmg", &disk_image());
        for wanted in [Shape::MachO, Shape::Elf, Shape::Pe] {
            let error = check_shape(&dmg, wanted).expect_err("a disk image is not a program anywhere");
            assert!(matches!(error, Error::NotAProgram { .. }), "{error:?}");
            assert!(error.to_string().contains("a disk image"), "{error}");
        }
    }

    #[test]
    fn a_file_that_is_not_there_is_an_io_error_and_not_a_verdict() {
        let dir = tempfile::tempdir().expect("temp dir");
        let error = check_is_program(&dir.path().join("absent")).expect_err("nothing to read");
        assert!(matches!(error, Error::Io { .. }), "{error:?}");
    }
}
