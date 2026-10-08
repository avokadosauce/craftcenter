//! CraftCenter, the desktop app.
//!
//! A thin binary: open the core, build the window, hand both to the shell. Everything the user
//! can do lives in `craftcenter-core`, and everything the window looks like lives in
//! `craftcenter-ui-egui`.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable, clippy::indexing_slicing)]
// A GUI binary on Windows should not also open a console window.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::process::ExitCode;

use craftcenter_core::Center;
use craftcenter_ui_egui::{CraftCenterApp, titlebar};

/// The windowing framework's half of the app: it owns the event loop and hands the shell a `Ui`
/// to draw into once per frame.
struct Shell {
    app: CraftCenterApp,
}

impl eframe::App for Shell {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.app.ui(ui);
    }
}

/// Printed by `craftcenter --version` and `--help`, so the desktop binary answers the two
/// questions a terminal user will ask of it before launching a window.
const USAGE: &str = "\
craftcenter — install and update the Crafting Apps

USAGE
    craftcenter [--version] [--help]

With no arguments, opens the CraftCenter window. For a headless machine, use craftcenter-cli.

ENVIRONMENT
    CRAFTCENTER_ROOT             install everything under this directory instead of the
                                 per-user locations
    CRAFTCENTER_OS_DECORATIONS=1 use the system's window decorations instead of the app's
                                 own title bar
";

fn main() -> ExitCode {
    // The window takes no arguments; the two flags below are the only ones worth answering from a
    // terminal, and each of them is the whole invocation.
    if let Some(argument) = std::env::args().nth(1) {
        match argument.as_str() {
            "--version" | "-V" => {
                println!("craftcenter {}", env!("CARGO_PKG_VERSION"));
                return ExitCode::SUCCESS;
            }
            "--help" | "-h" => {
                print!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            other => {
                eprintln!("craftcenter: unknown argument {other:?}\n");
                print!("{USAGE}");
                return ExitCode::FAILURE;
            }
        }
    }

    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();

    // A previous run may have replaced this program with a newer build and left the old image
    // behind, where the platform would not let it be deleted while it was still mapped.
    if let Ok(exe) = std::env::current_exe() {
        craftcenter_core::clean_after_self_update(&exe);
    }

    let center = match Center::open() {
        Ok(center) => center,
        Err(error) => {
            eprintln!("craftcenter: {error}");
            return ExitCode::FAILURE;
        }
    };

    let mut viewport = egui::ViewportBuilder::default()
        .with_title("CraftCenter")
        .with_app_id("io.github.avokadosauce.craftcenter")
        .with_inner_size([940.0, 680.0])
        .with_min_inner_size([520.0, 420.0])
        .with_decorations(titlebar::decorations_wanted());
    if let Some(icon) = window_icon() {
        viewport = viewport.with_icon(icon);
    }

    let options = eframe::NativeOptions { viewport, ..Default::default() };
    match eframe::run_native(
        "CraftCenter",
        options,
        Box::new(|cc| Ok(Box::new(Shell { app: CraftCenterApp::new(&cc.egui_ctx, center).with_folder_picker(Box::new(folder_picker)) }))),
    ) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("craftcenter: the window could not be opened: {error}");
            eprintln!("craftcenter: on a machine with no display, use craftcenter-cli instead");
            ExitCode::FAILURE
        }
    }
}

/// The platform's own folder dialog, which is the only part of the window the shell crate does
/// not draw itself. It blocks while it is open, as a modal dialog should.
fn folder_picker(start: &std::path::Path) -> Option<std::path::PathBuf> {
    rfd::FileDialog::new().set_title("Where should the Crafting Apps be installed?").set_directory(start).pick_folder()
}

/// CraftCenter's own window icon. Absent until the project draws one, and a missing icon is not
/// worth failing a launch over.
fn window_icon() -> Option<std::sync::Arc<egui::IconData>> {
    let bytes = include_bytes!("../../../assets/app-icon/craftcenter-64.png");
    let decoded = image::load_from_memory(bytes).ok()?.to_rgba8();
    let (width, height) = (decoded.width(), decoded.height());
    Some(std::sync::Arc::new(egui::IconData { rgba: decoded.into_raw(), width, height }))
}
