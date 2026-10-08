//! Repository tasks. Run them through the alias in `.cargo/config.toml`: `cargo xtask <task>`.
//!
//!   layers              check that no crate depends on a layer above it
//!   catalogue --check-offline   check the catalogue against the recorded fixtures (no network)
//!   catalogue --check           check the catalogue against the live releases (needs the network)
//!
//! `layers` and `catalogue --check-offline` run in CI. The live check does not, because CI must
//! not fail when an upstream project ships a release.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable, clippy::indexing_slicing)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use craftcenter_catalogue::Catalogue;
use craftcenter_select::{Platform, Preference, select};

/// The layering. A crate may depend only on crates of a strictly lower layer, which is what keeps
/// the parsing and selection logic free of the shell and of the network.
const LAYERS: &[(&str, u8)] = &[
    ("craftcenter-catalogue", 0),
    ("craftcenter-verify", 0),
    ("craftcenter-select", 1),
    ("craftcenter-releases", 2),
    ("craftcenter-install", 3),
    ("craftcenter-core", 4),
    ("craftcenter-ui-egui", 5),
];

/// Crates that must not mention the user interface toolkit at all.
const NO_UI_BELOW: u8 = 5;
const UI_CRATES: &[&str] = &["egui", "eframe", "winit", "wgpu", "egui_extras"];

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let task = args.first().map(String::as_str).unwrap_or("help");
    let flags: Vec<&str> = args.iter().skip(1).map(String::as_str).collect();

    let result = match task {
        "layers" => layers(),
        "catalogue" => catalogue(&flags),
        "help" | "--help" | "-h" => {
            print!("{}", include_str!("usage.txt"));
            return ExitCode::SUCCESS;
        }
        other => Err(format!("unknown task {other:?}; `cargo xtask help` lists them")),
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("xtask: {message}");
            ExitCode::FAILURE
        }
    }
}

fn workspace_root() -> PathBuf {
    // CARGO_MANIFEST_DIR is `<root>/xtask` when cargo runs this.
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."))
}

/// Check that the dependency graph respects the declared layering.
fn layers() -> Result<(), String> {
    let root = workspace_root();
    let levels: BTreeMap<&str, u8> = LAYERS.iter().copied().collect();
    let mut problems = Vec::new();
    let mut checked = 0usize;

    for (name, level) in LAYERS {
        let dir = name.strip_prefix("craftcenter-").unwrap_or(name);
        let manifest = root.join("crates").join(dir).join("Cargo.toml");
        let text = std::fs::read_to_string(&manifest).map_err(|e| format!("{}: {e}", manifest.display()))?;
        let parsed: toml::Value = toml::from_str(&text).map_err(|e| format!("{}: {e}", manifest.display()))?;
        checked += 1;

        let dependencies = parsed.get("dependencies").and_then(toml::Value::as_table);
        for dependency in dependencies.into_iter().flat_map(|t| t.keys()) {
            if let Some(other) = levels.get(dependency.as_str()).filter(|other| *other >= level) {
                problems.push(format!("{name} (layer {level}) depends on {dependency} (layer {other})"));
            }
            if *level < NO_UI_BELOW && UI_CRATES.contains(&dependency.as_str()) {
                problems.push(format!("{name} (layer {level}) depends on {dependency}; only the shell may"));
            }
        }
    }

    if problems.is_empty() {
        println!("layers: {checked} crates, no violations");
        return Ok(());
    }
    for problem in &problems {
        eprintln!("  {problem}");
    }
    Err(format!("{} layering violation(s)", problems.len()))
}

fn catalogue(flags: &[&str]) -> Result<(), String> {
    let catalogue = Catalogue::embedded().map_err(|e| e.to_string())?;
    println!("catalogue: {} rows ({} installable)", catalogue.apps().len(), catalogue.installable().count());

    if flags.contains(&"--check-offline") {
        return check_offline(&catalogue);
    }
    if flags.contains(&"--check") {
        return check_live(&catalogue);
    }
    for app in catalogue.apps() {
        println!("  {:<13} {:<26} {}", app.slug, app.repo, app.asset_stems.join(", "));
    }
    Ok(())
}

/// Resolve every (app × platform) against the recorded release manifests. No network.
fn check_offline(catalogue: &Catalogue) -> Result<(), String> {
    let root = workspace_root().join("crates/releases/fixtures");
    let mut problems = Vec::new();
    let mut resolved = 0usize;
    let mut skipped = Vec::new();

    for app in catalogue.installable() {
        let manifest = root.join(&app.slug).join("SHA256SUMS.txt");
        let Ok(text) = std::fs::read_to_string(&manifest) else {
            // An app with no recorded release is a legitimate state, not a failure: soundcraft had
            // none when these fixtures were recorded.
            skipped.push(app.slug.clone());
            continue;
        };
        let sums = craftcenter_verify::Sums::parse(&text).map_err(|e| format!("{}: {e}", manifest.display()))?;
        let assets = sums.asset_names();
        for platform in Platform::ALL {
            match select(app, platform, &assets, Preference::default()) {
                Ok(_) => resolved += 1,
                Err(error) => problems.push(format!("{} on {}: {error}", app.slug, platform.label())),
            }
        }
    }

    if !skipped.is_empty() {
        println!("  no recorded release: {}", skipped.join(", "));
    }
    if problems.is_empty() {
        println!("catalogue: {resolved} (app x platform) pairs resolve against the recorded releases");
        return Ok(());
    }
    for problem in &problems {
        eprintln!("  {problem}");
    }
    Err(format!("{} unresolved (app x platform) pair(s)", problems.len()))
}

/// Look up every app's current release and report drift from what the catalogue expects: a new
/// asset stem, a variant that has gone, or a first release for an app that had none.
///
/// Reports rather than fails, except when an app resolves on no platform at all — that is a
/// catalogue row that would not work for anyone.
fn check_live(catalogue: &Catalogue) -> Result<(), String> {
    let client = craftcenter_releases::Client::new(craftcenter_releases::Ureq::new());
    let mut broken = Vec::new();

    for app in catalogue.apps() {
        let release = match client.latest(app) {
            Ok(Some(release)) => release,
            Ok(None) => {
                println!("  {:<13} no release yet", app.slug);
                continue;
            }
            Err(error) => {
                println!("  {:<13} could not be checked: {error}", app.slug);
                continue;
            }
        };

        let mut resolved = Vec::new();
        let mut missing = Vec::new();
        for platform in Platform::ALL {
            match select(app, platform, &release.assets, Preference::default()) {
                Ok(choice) => resolved.push((platform.label(), choice)),
                Err(_) => missing.push(platform.label()),
            }
        }

        // Which stem the release actually used, so a rename is visible the day it happens.
        let stem = resolved
            .first()
            .and_then(|(_, choice)| app.asset_stems.iter().find(|stem| choice.asset.starts_with(stem.as_str())))
            .cloned()
            .unwrap_or_else(|| "?".to_owned());

        println!(
            "  {:<13} {:<8} stem={:<12} {} of {} platforms{}",
            app.slug,
            release.tag,
            stem,
            resolved.len(),
            Platform::ALL.len(),
            if missing.is_empty() { String::new() } else { format!("  (none for {})", missing.join(", ")) }
        );

        if app.asset_stems.first().map(String::as_str) != Some(stem.as_str()) && stem != "?" {
            println!("                 note: the release uses {stem:?}, which is not the first stem in the catalogue row");
        }
        if resolved.is_empty() {
            broken.push(app.slug.clone());
        }
    }

    if broken.is_empty() {
        return Ok(());
    }
    Err(format!("these rows resolve on no platform at all: {}", broken.join(", ")))
}
