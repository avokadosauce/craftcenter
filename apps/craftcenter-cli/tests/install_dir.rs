//! The install location, through the command line and back out again.
//!
//! An end-to-end test rather than a unit one: what matters is that a location typed at a
//! terminal is written to the settings file, read back by the next invocation, and restored by
//! `--default`. Nothing here touches the network — `config` and `paths` never do — so it runs
//! wherever the rest of the suite does.

use std::path::Path;
use std::process::Command;

fn cli(root: &Path, args: &[&str]) -> (bool, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_craftcenter-cli")).env("CRAFTCENTER_ROOT", root).args(args).output().expect("the command line binary runs");
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    (output.status.success(), text)
}

#[test]
fn a_chosen_install_location_round_trips_through_the_command_line() {
    let root = tempfile::tempdir().expect("temp dir");
    let chosen = root.path().join("Crafting Apps");
    let default = root.path().join("apps");

    let (ok, before) = cli(root.path(), &["config"]);
    assert!(ok, "{before}");
    assert!(before.contains(&default.display().to_string()), "the default is what is in force to begin with: {before}");

    let (ok, set) = cli(root.path(), &["config", "install-dir", &chosen.display().to_string()]);
    assert!(ok, "{set}");
    assert!(set.contains(&chosen.display().to_string()), "{set}");
    assert!(chosen.is_dir(), "the chosen folder is created");

    // A second invocation reads it back from the settings file, and still knows the default.
    let (ok, after) = cli(root.path(), &["config"]);
    assert!(ok, "{after}");
    assert!(after.contains(&chosen.display().to_string()), "{after}");
    assert!(after.contains(&default.display().to_string()), "the default is still named, to reset to: {after}");
    let (ok, paths) = cli(root.path(), &["paths"]);
    assert!(ok, "{paths}");
    assert!(paths.contains(&chosen.display().to_string()), "`paths` reports where apps actually go: {paths}");

    let (ok, reset) = cli(root.path(), &["config", "install-dir", "--default"]);
    assert!(ok, "{reset}");
    assert!(reset.contains(&default.display().to_string()), "{reset}");
    let (_, finally) = cli(root.path(), &["config"]);
    assert!(!finally.contains(&chosen.display().to_string()), "the choice is gone: {finally}");
}

#[test]
fn a_location_that_cannot_be_used_is_refused_in_a_sentence_and_changes_nothing() {
    let root = tempfile::tempdir().expect("temp dir");
    let file = root.path().join("not-a-folder.txt");
    std::fs::write(&file, b"x").expect("write");

    let (ok, message) = cli(root.path(), &["config", "install-dir", &file.display().to_string()]);
    assert!(!ok, "a file is not an install location: {message}");
    assert!(message.contains("file, not a folder"), "{message}");

    let (_, after) = cli(root.path(), &["config"]);
    assert!(after.contains(&root.path().join("apps").display().to_string()), "nothing was changed: {after}");
}

#[test]
fn install_dir_applies_to_one_run_without_being_kept() {
    let root = tempfile::tempdir().expect("temp dir");
    let once = root.path().join("just-this-once");

    let (ok, paths) = cli(root.path(), &["--install-dir", &once.display().to_string(), "paths"]);
    assert!(ok, "{paths}");
    assert!(paths.contains(&once.display().to_string()), "{paths}");

    let (_, after) = cli(root.path(), &["config"]);
    assert!(!after.contains(&once.display().to_string()), "a one-run location is not saved: {after}");
}

#[test]
fn there_is_nothing_to_move_when_nothing_is_installed() {
    let root = tempfile::tempdir().expect("temp dir");
    let (ok, message) = cli(root.path(), &["move"]);
    assert!(ok, "{message}");
    assert!(message.contains("nothing to move"), "{message}");
}
