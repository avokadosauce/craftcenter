//! Checksums.
//!
//! Every Crafting App release publishes a `SHA256SUMS.txt` produced by `sha256sum -- *` over the
//! collected artifacts, so it is plain `<64 hex><two spaces><bare filename>` lines, one per asset.
//! That file is the only machine-readable index a release has: it names every asset *and* carries
//! its digest, which is why CraftCenter uses it both to discover a release and to verify it.
//!
//! What it does not do: the sums file is **unsigned** in every upstream repository — no detached
//! signature, no minisign, no cosign. So the trust root is TLS plus GitHub's control of the
//! release, not a publisher key. A verified digest proves the bytes are the ones that release
//! published; it is not a publisher-identity check. Where the platform offers one — a notarised
//! and stapled DMG, an Authenticode signature — the installer checks that too.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable, clippy::indexing_slicing)]

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{self, BufReader, Read};
use std::path::Path;

use sha2::{Digest, Sha256};

/// A SHA-256 digest.
pub type Sha256Digest = [u8; 32];

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("line {line}: expected \"<64 hex>  <filename>\", found {text:?}")]
    MalformedLine { line: usize, text: String },
    #[error("checksum manifest lists no files")]
    Empty,
    #[error("line {line}: {name:?} is listed twice with different digests")]
    Conflict { line: usize, name: String },
    #[error("{name:?} is not listed in the checksum manifest")]
    NotListed { name: String },
    #[error("{name}: checksum mismatch (expected {expected}, got {actual})")]
    Mismatch { name: String, expected: String, actual: String },
    #[error("reading {path}: {source}")]
    Io { path: String, source: io::Error },
}

/// Lowercase hex of a digest.
pub fn hex(digest: &Sha256Digest) -> String {
    let mut s = String::with_capacity(64);
    for byte in digest {
        // Two hex digits per byte; `write!` to a String cannot fail, so push the nibbles directly.
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let hi = HEX.get(usize::from(byte >> 4)).copied().unwrap_or(b'?');
        let lo = HEX.get(usize::from(byte & 0x0f)).copied().unwrap_or(b'?');
        s.push(char::from(hi));
        s.push(char::from(lo));
    }
    s
}

fn parse_hex(text: &str) -> Option<Sha256Digest> {
    if text.len() != 64 || !text.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let bytes = text.as_bytes();
    let mut out = [0u8; 32];
    for (i, slot) in out.iter_mut().enumerate() {
        let hi = char::from(*bytes.get(i * 2)?).to_digit(16)?;
        let lo = char::from(*bytes.get(i * 2 + 1)?).to_digit(16)?;
        *slot = u8::try_from(hi * 16 + lo).ok()?;
    }
    Some(out)
}

/// A parsed `SHA256SUMS.txt`: the asset list of one release, with a digest for each entry.
#[derive(Clone, Debug, Default)]
pub struct Sums {
    entries: BTreeMap<String, Sha256Digest>,
}

impl Sums {
    /// Parse `sha256sum` output.
    ///
    /// Accepts both `sha256sum` modes — two spaces for text mode and ` *` for binary mode — and
    /// ignores blank lines. Anything else is a [`Error::MalformedLine`] naming the line, because a
    /// manifest we cannot read in full is a manifest we must not verify against.
    pub fn parse(text: &str) -> Result<Self, Error> {
        let mut entries: BTreeMap<String, Sha256Digest> = BTreeMap::new();
        for (index, raw) in text.lines().enumerate() {
            let line = index + 1;
            let trimmed = raw.trim_end_matches(['\r', '\n']);
            if trimmed.trim().is_empty() {
                continue;
            }
            let malformed = || Error::MalformedLine { line, text: trimmed.to_owned() };
            let (digest_text, rest) = trimmed.split_once(' ').ok_or_else(malformed)?;
            let digest = parse_hex(digest_text).ok_or_else(malformed)?;
            // sha256sum writes "<digest>  <name>" (text) or "<digest> *<name>" (binary).
            let name = rest.strip_prefix(' ').or_else(|| rest.strip_prefix('*')).unwrap_or(rest);
            if name.is_empty() {
                return Err(malformed());
            }
            if entries.get(name).is_some_and(|existing| existing != &digest) {
                return Err(Error::Conflict { line, name: name.to_owned() });
            }
            entries.insert(name.to_owned(), digest);
        }
        if entries.is_empty() {
            return Err(Error::Empty);
        }
        Ok(Self { entries })
    }

    /// Every asset name in the manifest, sorted.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.entries.keys().map(String::as_str)
    }

    /// The asset names as an owned list — the input to asset selection.
    pub fn asset_names(&self) -> Vec<String> {
        self.entries.keys().cloned().collect()
    }

    pub fn digest(&self, name: &str) -> Option<&Sha256Digest> {
        self.entries.get(name)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Stream a reader through SHA-256 without holding it in memory.
pub fn sha256_reader<R: Read>(reader: R) -> io::Result<Sha256Digest> {
    let mut reader = BufReader::new(reader);
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        match buffer.get(..read) {
            Some(chunk) => hasher.update(chunk),
            None => break,
        }
    }
    Ok(hasher.finalize().into())
}

/// Hash a file on disk.
pub fn sha256_file(path: &Path) -> Result<Sha256Digest, Error> {
    let file = File::open(path).map_err(|source| Error::Io { path: path.display().to_string(), source })?;
    sha256_reader(file).map_err(|source| Error::Io { path: path.display().to_string(), source })
}

/// Hash `path` and compare it with the manifest entry for `name`.
///
/// An asset absent from the manifest is [`Error::NotListed`], never a silent pass: a download we
/// cannot check is a download we do not install.
pub fn verify_file(sums: &Sums, name: &str, path: &Path) -> Result<(), Error> {
    let expected = sums.digest(name).ok_or_else(|| Error::NotListed { name: name.to_owned() })?;
    let actual = sha256_file(path)?;
    if &actual == expected { Ok(()) } else { Err(Error::Mismatch { name: name.to_owned(), expected: hex(expected), actual: hex(&actual) }) }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    /// Three lines of the real photocraft v0.3.0 manifest.
    const REAL: &str = "\
29e3011f49a52ea25c8fe404258a6c5fadb02094dbb40a884d69e6ba808e6136  photocraft-0.3.0-linux-x86_64.AppImage
c0b0223cddb18dd7f5607fb4d6cc6a925a62f5b5b47457997d61ad0f72aa5911  photocraft-0.3.0-macos-universal.dmg
2b3e1bfdacfb597c1cab783c9ed14ed59854cc4bf11f333973d1c823d77e9db6  photocraft-0.3.0-windows-x64.msi
";

    #[test]
    fn parses_real_sha256sum_output() {
        let sums = Sums::parse(REAL).expect("parses");
        assert_eq!(sums.len(), 3);
        assert_eq!(
            sums.digest("photocraft-0.3.0-macos-universal.dmg").map(hex).as_deref(),
            Some("c0b0223cddb18dd7f5607fb4d6cc6a925a62f5b5b47457997d61ad0f72aa5911")
        );
    }

    #[test]
    fn accepts_binary_mode_and_blank_lines() {
        let text = format!("\n{}\n\n", REAL.replace("  photocraft", " *photocraft"));
        let sums = Sums::parse(&text).expect("parses");
        assert_eq!(sums.len(), 3);
    }

    #[test]
    fn rejects_a_truncated_digest() {
        let err = Sums::parse("abc  file.bin").expect_err("too short");
        assert!(matches!(err, Error::MalformedLine { line: 1, .. }));
    }

    #[test]
    fn rejects_a_line_with_no_filename() {
        let digest = "0".repeat(64);
        assert!(Sums::parse(&format!("{digest}  ")).is_err());
    }

    #[test]
    fn rejects_an_empty_manifest() {
        assert!(matches!(Sums::parse("\n\n"), Err(Error::Empty)));
    }

    #[test]
    fn rejects_the_same_name_with_two_digests() {
        let a = "a".repeat(64);
        let b = "b".repeat(64);
        let text = format!("{a}  x.bin\n{b}  x.bin\n");
        assert!(matches!(Sums::parse(&text), Err(Error::Conflict { .. })));
    }

    #[test]
    fn hashes_a_known_value() {
        // The SHA-256 of the empty input, and of "abc".
        assert_eq!(hex(&sha256_reader(&b""[..]).expect("hash")), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
        assert_eq!(hex(&sha256_reader(&b"abc"[..]).expect("hash")), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }

    fn fixture(bytes: &[u8]) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("asset.bin");
        let mut file = File::create(&path).expect("create");
        file.write_all(bytes).expect("write");
        (dir, path)
    }

    #[test]
    fn verifies_a_good_file() {
        let (_dir, path) = fixture(b"abc");
        let sums = Sums::parse("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad  asset.bin").expect("parses");
        assert!(verify_file(&sums, "asset.bin", &path).is_ok());
    }

    #[test]
    fn rejects_one_corrupted_byte() {
        let (_dir, path) = fixture(b"abd");
        let sums = Sums::parse("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad  asset.bin").expect("parses");
        assert!(matches!(verify_file(&sums, "asset.bin", &path), Err(Error::Mismatch { .. })));
    }

    #[test]
    fn rejects_a_truncated_file() {
        let (_dir, path) = fixture(b"ab");
        let sums = Sums::parse("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad  asset.bin").expect("parses");
        assert!(matches!(verify_file(&sums, "asset.bin", &path), Err(Error::Mismatch { .. })));
    }

    #[test]
    fn refuses_a_name_the_manifest_does_not_list() {
        let (_dir, path) = fixture(b"abc");
        let sums = Sums::parse(REAL).expect("parses");
        assert!(matches!(verify_file(&sums, "asset.bin", &path), Err(Error::NotListed { .. })));
    }

    #[test]
    fn a_missing_file_is_an_error_not_a_panic() {
        let sums = Sums::parse(REAL).expect("parses");
        let err = verify_file(&sums, "photocraft-0.3.0-windows-x64.msi", Path::new("/definitely/not/here"));
        assert!(matches!(err, Err(Error::Io { .. })));
    }
}
