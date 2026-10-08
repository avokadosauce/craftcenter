//! Verify, from a state file on disk to the exit code a script reads.
//!
//! The per-format behaviour is tested where it lives, in `craftcenter-install`. What this adds
//! is the whole pipe: a record and a manifest written the way an install writes them, read back
//! by a separate process, printed, and turned into one of the three exit codes. Nothing here
//! touches the network — `verify` never does.

use std::path::Path;
use std::process::Command;

use craftcenter_verify::manifest::Manifest;

/// Exit code and output together: a verdict is both, and a test that checked only one of them
/// would pass on a program that printed the right words and returned the wrong answer.
fn cli(root: &Path, args: &[&str]) -> (Option<i32>, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_craftcenter-cli")).env("CRAFTCENTER_ROOT", root).args(args).output().expect("the command line binary runs");
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    (output.status.code(), text)
}

/// Write the two files an install leaves behind for one app: the per-file manifest, and the
/// state entry that names it by digest.
fn pretend_photocraft_is_installed(root: &Path) -> std::path::PathBuf {
    let dir = root.join("apps/photocraft/0.3.0");
    std::fs::create_dir_all(&dir).expect("create the install");
    std::fs::write(dir.join("ai.storyteller.photocraft.AppImage"), "the app").expect("write the app");

    let text = Manifest::of_tree(&dir).expect("walked").to_json().expect("serialised");
    let manifests = root.join("state/manifests");
    std::fs::create_dir_all(&manifests).expect("create");
    std::fs::write(manifests.join("photocraft.json"), &text).expect("write the manifest");
    let digest = craftcenter_verify::hex(&craftcenter_verify::sha256_reader(text.as_bytes()).expect("hash"));

    // The three paths go in TOML *literal* strings, in single quotes, because a Windows path is
    // full of backslashes and TOML reads `\U` in a quoted string as the start of an escape.
    let state = format!(
        "[installed.photocraft]\n\
         version = \"0.3.0\"\n\
         tag = \"v0.3.0\"\n\
         asset = \"photocraft-0.3.0-linux-x86_64.AppImage\"\n\
         sha256 = \"{asset}\"\n\
         format = \"app-image\"\n\
         dir = '{dir}'\n\
         root = '{home}'\n\
         manifest = \"{digest}\"\n\
         launcher = '{launcher}'\n\
         installed_at = 1700000000\n",
        asset = "0".repeat(64),
        dir = dir.display(),
        home = root.join("apps/photocraft").display(),
        launcher = root.join("bin/photocraft").display(),
    );
    std::fs::write(root.join("state/state.toml"), state).expect("write the state");
    dir
}

#[test]
fn an_untouched_install_verifies_and_exits_zero() {
    let root = tempfile::tempdir().expect("temp dir");
    pretend_photocraft_is_installed(root.path());

    let (code, text) = cli(root.path(), &["verify", "photocraft"]);
    assert_eq!(code, Some(0), "{text}");
    assert!(text.contains("match what was installed"), "{text}");
}

#[test]
fn a_changed_file_is_named_and_exits_one() {
    let root = tempfile::tempdir().expect("temp dir");
    let dir = pretend_photocraft_is_installed(root.path());
    std::fs::write(dir.join("ai.storyteller.photocraft.AppImage"), "the bpp").expect("tamper");

    let (code, text) = cli(root.path(), &["verify", "photocraft"]);
    assert_eq!(code, Some(1), "{text}");
    assert!(text.contains("changed since it was installed"), "{text}");
    assert!(text.contains("ai.storyteller.photocraft.AppImage"), "the path is named: {text}");
}

/// The third answer. A script asking whether an app is intact must be able to tell "no" from
/// "I cannot say", so an install with nothing to check against exits 2 rather than 0 or 1.
#[test]
fn nothing_to_check_against_exits_two() {
    let root = tempfile::tempdir().expect("temp dir");
    pretend_photocraft_is_installed(root.path());
    std::fs::remove_file(root.path().join("state/manifests/photocraft.json")).expect("remove the manifest");

    let (code, text) = cli(root.path(), &["verify", "photocraft"]);
    assert_eq!(code, Some(2), "{text}");
    assert!(text.contains("reinstall"), "it says what to do about it: {text}");
}

#[test]
fn verifying_something_that_is_not_installed_says_so() {
    let root = tempfile::tempdir().expect("temp dir");
    let (code, text) = cli(root.path(), &["verify", "photocraft"]);
    assert_eq!(code, Some(1), "{text}");
    assert!(text.contains("not installed"), "{text}");
}
