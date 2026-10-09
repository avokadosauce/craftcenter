//! One real frame of the shell, driven without a window.
//!
//! Everything else in this crate is tested as arithmetic: how many columns fit, which card an
//! arrow key lands on, what a footer says. None of that is the same as asking whether the window
//! *draws* — the gap round 2 closed last was a drag region that laid out correctly and never
//! received a press, and the gap it could not close was that no test had ever opened a frame.
//!
//! So this does. An `egui::Context` needs no display: given a screen rectangle and a list of
//! input events it produces the shapes it would paint, which can then be tessellated into the
//! triangles a renderer would be handed. A NaN rectangle — the usual way a layout bug shows up
//! before anyone sees it — reaches a vertex position and is caught here, two layers before a
//! graphics driver would have silently drawn nothing.
//!
//! The whole program runs behind it, against the release manifests recorded in
//! `crates/releases/fixtures`: no network, no display, and a temporary directory for a home.

use std::collections::HashMap;
use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use craftcenter_core::{Catalogue, Center, Fetch, Paths, ReleaseError, Response};
use craftcenter_select::{Arch, Os, Platform};

use super::*;

const REDIRECT: &str = include_str!("../../releases/fixtures/photocraft/redirect.txt");
const SUMS: &str = include_str!("../../releases/fixtures/photocraft/SHA256SUMS.txt");
/// The asset a Linux x86_64 install of photocraft v0.3.0 resolves to.
const ASSET: &str = "photocraft-0.3.0-linux-x86_64.AppImage";
/// Stand-in bytes for it. The recorded manifest's line for that one asset is rewritten to their
/// digest, so the install verifies with no network and no 62 MB fixture.
const PAYLOAD: &[u8] = b"appimage";
/// The window the desktop app opens at, for the frames where only the width is under test.
const HEIGHT: f32 = 680.0;

/// The recorded release of one app, served to the program under test.
///
/// photocraft answers with its real redirect and its real asset list; every other app answers
/// 404, which is a state this program renders rather than an error. That is also the more
/// interesting grid to lay out: one card with something to install and eleven with nothing.
struct Fixtures {
    responses: HashMap<String, (u16, Option<String>, Vec<u8>)>,
    /// How many asset downloads have been started. The install path is the only thing that
    /// downloads an asset, so this is how a test says "once".
    downloads: Arc<AtomicUsize>,
}

impl Fetch for Fixtures {
    fn get(&self, url: &str, _follow: bool) -> Result<Response, ReleaseError> {
        match self.responses.get(url) {
            Some((status, location, body)) => Ok(Response { status: *status, location: location.clone(), body: body.clone() }),
            None => Ok(Response { status: 404, location: None, body: Vec::new() }),
        }
    }

    fn get_to(&self, url: &str, sink: &mut dyn Write, progress: &mut dyn FnMut(u64, Option<u64>)) -> Result<u16, ReleaseError> {
        if url.ends_with(ASSET) {
            self.downloads.fetch_add(1, Ordering::SeqCst);
        }
        let response = self.get(url, true)?;
        if response.status == 200 {
            sink.write_all(&response.body).map_err(|source| ReleaseError::Io { path: url.to_owned(), source })?;
            let len = response.body.len() as u64;
            progress(len, Some(len));
        }
        Ok(response.status)
    }
}

/// The real photocraft v0.3.0 asset list, with the digest of the one asset actually served
/// swapped for the digest of [`PAYLOAD`].
fn manifest_with_payload_digest() -> String {
    let digest = craftcenter_verify::hex(&craftcenter_verify::sha256_reader(PAYLOAD).expect("hash"));
    SUMS.lines().map(|line| if line.ends_with(ASSET) { format!("{digest}  {ASSET}") } else { line.to_owned() }).collect::<Vec<_>>().join("\n")
}

/// A shell with a context, a temporary home, and photocraft's release already looked up.
///
/// The lookup happens before the first frame on purpose: a card can only be Enter-ed into an
/// install once its release is known, and doing it here makes the frame under test deterministic
/// instead of racing eleven worker threads.
fn harness(root: &std::path::Path) -> (egui::Context, CraftCenterApp<Fixtures>, Arc<AtomicUsize>) {
    let catalogue = Catalogue::embedded().expect("catalogue parses");
    let app = catalogue.get("photocraft").cloned().expect("photocraft is in the catalogue");
    let location = REDIRECT.trim();
    let downloads = Arc::new(AtomicUsize::new(0));

    let mut responses = HashMap::new();
    responses.insert(app.sums_url(), (302, Some(location.to_owned()), Vec::new()));
    responses.insert(location.to_owned(), (200, None, manifest_with_payload_digest().into_bytes()));
    responses.insert(app.asset_url("v0.3.0", ASSET), (200, None, PAYLOAD.to_vec()));

    let fetch = Fixtures { responses, downloads: Arc::clone(&downloads) };
    let center = Center::with(catalogue, Paths::rooted(root), fetch).with_platform(Platform::new(Os::Linux, Arch::X86_64));
    center.check("photocraft", true).expect("the recorded release is found");

    let ctx = egui::Context::default();
    let shell = CraftCenterApp::new(&ctx, center);
    (ctx, shell, downloads)
}

/// Draw one frame `width` points wide and hand back the triangles a renderer would be given.
///
/// Tessellating rather than stopping at the shapes is the point: it is the step that turns a
/// rectangle into vertex positions, so a coordinate that is not a number has nowhere left to hide.
fn frame(ctx: &egui::Context, shell: &mut CraftCenterApp<Fixtures>, width: f32, events: Vec<egui::Event>) -> Vec<egui::ClippedPrimitive> {
    let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width, HEIGHT))), events, ..Default::default() };
    let mut output = ctx.run_ui(input, |ui| shell.ui(ui));
    let shapes = std::mem::take(&mut output.shapes);
    let pixels_per_point = output.pixels_per_point;
    // Nothing here uploads textures to a renderer, so the deltas are dropped deliberately
    // rather than left for egui to complain about.
    output.drop_without_applying_deltas();
    ctx.tessellate(shapes, pixels_per_point)
}

/// Every coordinate of a frame must be a real number.
///
/// Clip rectangles are allowed to be infinite — that is how egui spells "do not clip" — so they
/// are only checked for NaN. A vertex position has no such excuse.
fn every_coordinate_is_a_number(primitives: &[egui::ClippedPrimitive], what: &str) {
    assert!(!primitives.is_empty(), "{what}: the frame painted nothing at all");
    for clipped in primitives {
        let clip = clipped.clip_rect;
        for value in [clip.min.x, clip.min.y, clip.max.x, clip.max.y] {
            assert!(!value.is_nan(), "{what}: a clip rectangle of {clip:?}");
        }
        if let egui::epaint::Primitive::Mesh(mesh) = &clipped.primitive {
            for vertex in &mesh.vertices {
                assert!(vertex.pos.x.is_finite() && vertex.pos.y.is_finite(), "{what}: a vertex at {:?}", vertex.pos);
            }
        }
    }
}

/// Keep drawing frames until `done`, or give up. Nothing here sleeps for a fixed time: the work
/// happens on worker threads, and the frames are what collect their results.
fn frames_until(
    ctx: &egui::Context,
    shell: &mut CraftCenterApp<Fixtures>,
    what: &str,
    mut done: impl FnMut(&CraftCenterApp<Fixtures>) -> bool,
) -> Vec<egui::ClippedPrimitive> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let painted = frame(ctx, shell, 940.0, Vec::new());
        if done(shell) {
            return painted;
        }
        assert!(Instant::now() < deadline, "{what} never happened");
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn key(key: egui::Key) -> Vec<egui::Event> {
    vec![egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE }]
}

/// The four widths the grid is built around: the minimum window, the width it opens at, a
/// three-column desktop, and a wide monitor. One context throughout, so this is also four
/// resizes rather than four fresh windows.
#[test]
fn the_grid_draws_at_every_window_width_without_a_coordinate_that_is_not_a_number() {
    let root = tempfile::tempdir().expect("temp dir");
    let (ctx, mut shell, _) = harness(root.path());

    for width in [520.0, 940.0, 1300.0, 2560.0] {
        // Twice at each width: the first frame lays out, the second draws against the layout the
        // first one settled, which is when a rectangle derived from a previous frame goes wrong.
        frame(&ctx, &mut shell, width, Vec::new());
        let painted = frame(&ctx, &mut shell, width, Vec::new());
        every_coordinate_is_a_number(&painted, &format!("{width} points wide"));
    }
}

#[test]
fn the_arrow_keys_move_the_selection_from_card_to_card() {
    let root = tempfile::tempdir().expect("temp dir");
    let (ctx, mut shell, _) = harness(root.path());
    frame(&ctx, &mut shell, 940.0, Vec::new());
    assert!(shell.focus.is_none(), "nothing is selected before a key is pressed");

    // Nothing is installed, so the grid is the catalogue in order and the first arrow key
    // selects its first card rather than moving from nowhere.
    let expected: Vec<String> = shell.rows.iter().take(2).map(|row| row.app.slug.clone()).collect();
    frame(&ctx, &mut shell, 940.0, key(egui::Key::ArrowRight));
    assert_eq!(shell.focus.as_deref(), expected.first().map(String::as_str));

    frame(&ctx, &mut shell, 940.0, key(egui::Key::ArrowRight));
    assert_eq!(shell.focus.as_deref(), expected.get(1).map(String::as_str), "a second press moves on");

    frame(&ctx, &mut shell, 940.0, key(egui::Key::ArrowLeft));
    assert_eq!(shell.focus.as_deref(), expected.first().map(String::as_str), "and back");

    // The frame that answered the key must still be a frame that can be drawn.
    let painted = frame(&ctx, &mut shell, 940.0, Vec::new());
    every_coordinate_is_a_number(&painted, "with a card selected");
}

/// Enter does what the card's own button does, and one download is enough: the key reaches both
/// this shell and egui's own "activate the focused widget", and a card can be double-tapped.
#[test]
fn enter_on_an_installable_card_installs_it_once() {
    let root = tempfile::tempdir().expect("temp dir");
    let (ctx, mut shell, downloads) = harness(root.path());
    frame(&ctx, &mut shell, 940.0, Vec::new());

    shell.focus = Some("photocraft".to_owned());
    frame(&ctx, &mut shell, 940.0, key(egui::Key::Enter));
    // Enter again while it is in flight. The guard is "this app is busy", so this is the press
    // that would cost a second download if it were not.
    frame(&ctx, &mut shell, 940.0, key(egui::Key::Enter));

    frames_until(&ctx, &mut shell, "the install", |shell| shell.rows.iter().any(|row| row.app.slug == "photocraft" && row.installed.is_some()));
    assert_eq!(downloads.load(Ordering::SeqCst), 1, "one install, one download");
    assert_eq!(shell.center.row_for("photocraft").expect("row").status, craftcenter_core::Status::UpToDate);
}

/// Verify, through the frame that offers it: a sound install says so on its own card, and a
/// tampered one opens the sheet, because a list of paths does not fit on a card.
#[test]
fn a_verdict_lands_on_the_card_and_a_list_of_paths_opens_a_sheet() {
    let root = tempfile::tempdir().expect("temp dir");
    let (ctx, mut shell, _) = harness(root.path());
    frame(&ctx, &mut shell, 940.0, Vec::new());
    shell.focus = Some("photocraft".to_owned());
    frame(&ctx, &mut shell, 940.0, key(egui::Key::Enter));
    frames_until(&ctx, &mut shell, "the install", |shell| shell.rows.iter().any(|row| row.app.slug == "photocraft" && row.installed.is_some()));

    shell.verify(&ctx, "photocraft");
    let painted = frames_until(&ctx, &mut shell, "the first verdict", |shell| shell.verdicts.contains_key("photocraft"));
    assert_eq!(shell.verdicts.get("photocraft").map(|verdict| verdict.report.level()), Some(Level::Ok));
    assert!(shell.sheet.is_none(), "a clean answer needs no sheet");
    every_coordinate_is_a_number(&painted, "with a verdict on a card");

    let installed = shell.rows.iter().find_map(|row| row.installed.clone()).expect("the install is recorded");
    let on_disk = std::path::PathBuf::from(&installed.dir).join("ai.storyteller.photocraft.AppImage");
    std::fs::write(&on_disk, b"tampered").expect("tamper");

    shell.verify(&ctx, "photocraft");
    let painted = frames_until(&ctx, &mut shell, "the second verdict", |shell| {
        shell.verdicts.get("photocraft").is_some_and(|verdict| verdict.report.level() == Level::Modified)
    });
    let sheet = shell.sheet.as_ref().expect("a list of paths opens a sheet");
    assert_eq!(sheet.report.modified, ["ai.storyteller.photocraft.AppImage"]);
    every_coordinate_is_a_number(&painted, "with the sheet open");
}
