//! The CraftCenter desktop shell.
//!
//! One window: a catalogue of the Crafting Apps with what is installed, what is available, and a
//! button per row. The shell is thin — every action is a call into `craftcenter-core`, the same
//! calls the command-line front end makes, so the two cannot drift apart in behaviour.
//!
//! Nothing blocks the interface. A check or a download runs on a worker thread and reports back
//! over a channel; the window stays interactive and shows per-row progress.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable, clippy::indexing_slicing)]

pub mod theme;
pub mod titlebar;
pub mod widgets;

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};

use craftcenter_core::{Center, Row, Settings, Status};
use egui::{RichText, TextureHandle};

use theme::{ThemeKind, Tokens, medium, semibold};
use widgets::Tone;

/// Which screen is showing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum View {
    #[default]
    Catalogue,
    Settings,
    About,
}

/// What a worker thread has to say.
enum Event {
    Progress { slug: String, done: u64, total: Option<u64> },
    Checked { slug: String },
    Installed { slug: String, version: String },
    Removed { slug: String },
    Failed { slug: String, message: String },
    SelfUpdated { to: String },
}

/// What a row is doing right now.
#[derive(Clone, Debug, Default)]
struct Activity {
    busy: bool,
    done: u64,
    total: Option<u64>,
}

/// The application state.
pub struct CraftCenterApp {
    center: Arc<Center>,
    rows: Vec<Row>,
    view: View,
    kind: ThemeKind,
    activity: HashMap<String, Activity>,
    icons: HashMap<String, TextureHandle>,
    message: Option<(String, Tone)>,
    draft: Settings,
    events: Receiver<Event>,
    sender: Sender<Event>,
    /// True once a first check has been kicked off, so it happens exactly once per launch.
    checked_on_start: bool,
    restart_needed: bool,
}

impl CraftCenterApp {
    /// Build the app around an already-opened core.
    pub fn new(ctx: &egui::Context, center: Center) -> Self {
        theme::install_fonts(ctx);
        let center = Arc::new(center);
        let kind = ThemeKind::from_id(&center.settings().theme).unwrap_or_default();
        theme::apply(ctx, kind);
        let (sender, events) = channel();
        let draft = center.settings().clone();
        let rows = center.rows();
        Self {
            center,
            rows,
            view: View::default(),
            kind,
            activity: HashMap::new(),
            icons: HashMap::new(),
            message: None,
            draft,
            events,
            sender,
            checked_on_start: false,
            restart_needed: false,
        }
    }

    fn refresh_rows(&mut self) {
        self.rows = self.center.rows();
    }

    fn any_busy(&self) -> bool {
        self.activity.values().any(|a| a.busy)
    }

    fn start(&mut self, slug: &str) {
        self.activity.insert(slug.to_owned(), Activity { busy: true, done: 0, total: None });
    }

    /// Run `job` on a worker thread, repainting when it reports.
    fn spawn<J>(&mut self, ctx: &egui::Context, slug: &str, job: J)
    where
        J: FnOnce(&Center, &Sender<Event>, &egui::Context) + Send + 'static,
    {
        self.start(slug);
        let center = Arc::clone(&self.center);
        let sender = self.sender.clone();
        let ctx = ctx.clone();
        // A detached thread: the work is self-contained and reports over the channel, and the
        // window must stay responsive while it runs.
        std::thread::spawn(move || {
            job(&center, &sender, &ctx);
            ctx.request_repaint();
        });
    }

    fn check_all(&mut self, ctx: &egui::Context, force: bool) {
        for row in self.rows.clone() {
            let slug = row.app.slug.clone();
            let key = slug.clone();
            self.spawn(ctx, &key, move |center, sender, ctx| {
                let event = match center.check(&slug, force) {
                    Ok(_) => Event::Checked { slug: slug.clone() },
                    Err(error) => Event::Failed { slug: slug.clone(), message: error.to_string() },
                };
                let _ = sender.send(event);
                ctx.request_repaint();
            });
        }
    }

    fn install(&mut self, ctx: &egui::Context, slug: &str) {
        let key = slug.to_owned();
        let slug = slug.to_owned();
        self.spawn(ctx, &key, move |center, sender, ctx| {
            let reporter = |done: u64, total: Option<u64>| {
                let _ = sender.send(Event::Progress { slug: slug.clone(), done, total });
                ctx.request_repaint();
            };
            let mut reporter = reporter;
            let event = match center.install(&slug, &mut reporter) {
                Ok(installed) => Event::Installed { slug: slug.clone(), version: installed.version },
                Err(error) => Event::Failed { slug: slug.clone(), message: error.to_string() },
            };
            let _ = sender.send(event);
        });
    }

    fn remove(&mut self, ctx: &egui::Context, slug: &str) {
        let key = slug.to_owned();
        let slug = slug.to_owned();
        self.spawn(ctx, &key, move |center, sender, _ctx| {
            let event = match center.remove(&slug) {
                Ok(()) => Event::Removed { slug: slug.clone() },
                Err(error) => Event::Failed { slug: slug.clone(), message: error.to_string() },
            };
            let _ = sender.send(event);
        });
    }

    fn launch(&mut self, slug: &str) {
        match self.center.launch(slug) {
            Ok(()) => self.message = Some((format!("Started {slug}"), Tone::Good)),
            Err(error) => self.message = Some((error.to_string(), Tone::Danger)),
        }
    }

    fn self_update(&mut self, ctx: &egui::Context) {
        self.spawn(ctx, "craftcenter", move |center, sender, ctx| {
            let reporter = |done: u64, total: Option<u64>| {
                let _ = sender.send(Event::Progress { slug: "craftcenter".to_owned(), done, total });
                ctx.request_repaint();
            };
            let mut reporter = reporter;
            let event = match center.self_update(&mut reporter) {
                Ok(update) if update.restart_required => Event::SelfUpdated { to: update.to },
                Ok(update) => Event::Failed { slug: "craftcenter".to_owned(), message: format!("Already running the newest build ({})", update.from) },
                Err(error) => Event::Failed { slug: "craftcenter".to_owned(), message: error.to_string() },
            };
            let _ = sender.send(event);
        });
    }

    fn drain_events(&mut self) {
        let mut changed = false;
        while let Ok(event) = self.events.try_recv() {
            match event {
                Event::Progress { slug, done, total } => {
                    let activity = self.activity.entry(slug).or_default();
                    activity.busy = true;
                    activity.done = done;
                    activity.total = total;
                }
                Event::Checked { slug } => {
                    self.activity.remove(&slug);
                    changed = true;
                }
                Event::Installed { slug, version } => {
                    self.activity.remove(&slug);
                    self.message = Some((format!("Installed {slug} {version}"), Tone::Good));
                    changed = true;
                }
                Event::Removed { slug } => {
                    self.activity.remove(&slug);
                    self.message = Some((format!("Removed {slug}"), Tone::Neutral));
                    changed = true;
                }
                Event::Failed { slug, message } => {
                    self.activity.remove(&slug);
                    self.message = Some((message, Tone::Danger));
                    changed = true;
                }
                Event::SelfUpdated { to } => {
                    self.activity.remove("craftcenter");
                    self.restart_needed = true;
                    self.message = Some((format!("CraftCenter {to} is installed. Restart to run it."), Tone::Accent));
                }
            }
        }
        if changed {
            self.refresh_rows();
        }
    }

    fn icon(&mut self, ctx: &egui::Context, row: &Row) -> Option<TextureHandle> {
        let name = row.app.icon.clone()?;
        if let Some(handle) = self.icons.get(&name) {
            return Some(handle.clone());
        }
        let bytes = craftcenter_core::icons::icon(&name)?;
        let decoded = image::load_from_memory(bytes).ok()?.to_rgba8();
        let size = [decoded.width() as usize, decoded.height() as usize];
        let image = egui::ColorImage::from_rgba_unmultiplied(size, decoded.as_raw());
        let handle = ctx.load_texture(&name, image, egui::TextureOptions::LINEAR);
        self.icons.insert(name, handle.clone());
        Some(handle)
    }

    /// The window's own top bar: the brand mark, the screens, and the caption buttons.
    fn top_bar(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let ctx = &ctx;
        let tokens = Tokens::get(ctx);
        egui::Panel::top("titlebar").exact_size(38.0).frame(egui::Frame::NONE.fill(tokens.chrome).inner_margin(egui::Margin::symmetric(10, 0))).show(
            ui,
            |ui| {
                let bar = ui.max_rect();
                // What the tabs end at and what the caption buttons begin at: between them is the
                // bare strip that drags the window, claimed as a widget so no pixel is contested.
                let (content_right, caption_left) = ui
                    .horizontal_centered(|ui| {
                        ui.label(RichText::new("CraftCenter").font(semibold(13.0)).color(tokens.text));
                        ui.add_space(14.0);
                        for (view, label) in [(View::Catalogue, "Apps"), (View::Settings, "Settings"), (View::About, "About")] {
                            let selected = self.view == view;
                            let text = RichText::new(label).font(medium(12.0)).color(if selected { tokens.text } else { tokens.text_faint });
                            if ui.selectable_label(selected, text).clicked() {
                                self.view = view;
                            }
                        }
                        let content_right = ui.cursor().left();
                        (content_right, titlebar::caption_buttons(ui, ctx).left())
                    })
                    .inner;
                titlebar::window_drag_strip(ui, ctx, bar, content_right, caption_left);
            },
        );
    }

    fn catalogue(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let tokens = Tokens::get(ctx);
        let updates = self.rows.iter().filter(|r| matches!(r.status, Status::UpdateAvailable { .. })).count();

        ui.horizontal(|ui| {
            widgets::section_title(ui, "The Crafting Apps");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if updates > 0 {
                    let label = if updates == 1 { "Update all (1)".to_owned() } else { format!("Update all ({updates})") };
                    if widgets::primary_button(ui, &label).clicked() && !self.any_busy() {
                        for row in self.rows.clone() {
                            if matches!(row.status, Status::UpdateAvailable { .. }) {
                                self.install(ctx, &row.app.slug);
                            }
                        }
                    }
                }
                if widgets::secondary_button(ui, "Check now").clicked() {
                    self.check_all(ctx, true);
                }
            });
        });
        ui.add_space(6.0);

        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            for row in self.rows.clone() {
                self.app_row(ui, ctx, &row);
                ui.add_space(6.0);
            }
            ui.add_space(4.0);
            ui.label(
                RichText::new(
                    "CraftCenter is unofficial. The apps are downloaded from their publisher's own \
                     GitHub releases and checked against each release's SHA256SUMS.txt before anything is installed.",
                )
                .small()
                .color(tokens.text_faint),
            );
        });
    }

    fn app_row(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, row: &Row) {
        let tokens = Tokens::get(ctx);
        let icon = self.icon(ctx, row);
        let activity = self.activity.get(&row.app.slug).cloned().unwrap_or_default();

        widgets::card(ui, |ui| {
            ui.horizontal(|ui| {
                match &icon {
                    Some(handle) => {
                        ui.add(egui::Image::new(handle).fit_to_exact_size(egui::vec2(36.0, 36.0)));
                    }
                    None => ui.add_space(36.0),
                }
                ui.add_space(4.0);

                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    ui.label(RichText::new(&row.app.name).font(medium(13.0)).color(tokens.text));
                    ui.label(RichText::new(&row.app.tagline).color(tokens.text_dim));
                });

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if activity.busy {
                        widgets::progress(ui, activity.done, activity.total);
                        return;
                    }
                    self.row_buttons(ui, ctx, row);
                    ui.add_space(8.0);
                    widgets::pill(ui, &row.status.label(), tone_for(&row.status));
                    if let Some(installed) = &row.installed {
                        widgets::numeric(ui, &installed.version);
                    } else if let Some(latest) = &row.latest {
                        widgets::numeric(ui, latest.version.as_str());
                    }
                });
            });
        });
    }

    fn row_buttons(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, row: &Row) {
        let slug = row.app.slug.clone();
        match &row.status {
            Status::Available => {
                if widgets::primary_button(ui, "Install").clicked() {
                    self.install(ctx, &slug);
                }
            }
            Status::UpdateAvailable { .. } => {
                if widgets::primary_button(ui, "Update").clicked() {
                    self.install(ctx, &slug);
                }
                if widgets::secondary_button(ui, "Launch").clicked() {
                    self.launch(&slug);
                }
            }
            Status::UpToDate => {
                if widgets::primary_button(ui, "Launch").clicked() {
                    self.launch(&slug);
                }
                if widgets::danger_button(ui, "Remove").clicked() {
                    self.remove(ctx, &slug);
                }
            }
            Status::Unchecked | Status::NoRelease | Status::Unavailable { .. } => {
                if row.installed.is_some() && widgets::danger_button(ui, "Remove").clicked() {
                    self.remove(ctx, &slug);
                }
            }
        }
    }

    fn settings(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let tokens = Tokens::get(ctx);
        widgets::section_title(ui, "Settings");
        ui.add_space(8.0);

        widgets::card(ui, |ui| {
            egui::Grid::new("settings").num_columns(2).spacing([16.0, 10.0]).show(ui, |ui| {
                ui.label(RichText::new("Theme").color(tokens.text_dim));
                egui::ComboBox::from_id_salt("theme").selected_text(self.kind.label()).show_ui(ui, |ui| {
                    for kind in ThemeKind::ALL {
                        if ui.selectable_label(self.kind == kind, kind.label()).clicked() {
                            self.kind = kind;
                            self.draft.theme = kind.id().to_owned();
                            theme::apply(ctx, kind);
                        }
                    }
                });
                ui.end_row();

                ui.label(RichText::new("Check for updates every").color(tokens.text_dim));
                ui.add(egui::DragValue::new(&mut self.draft.check_interval_hours).range(1..=720).suffix(" hours"));
                ui.end_row();

                ui.label(RichText::new("Keep the previous version").color(tokens.text_dim));
                ui.checkbox(&mut self.draft.keep_previous, "until the new one has been launched once");
                ui.end_row();

                ui.label(RichText::new("Channel").color(tokens.text_dim));
                ui.label(RichText::new("latest published release").color(tokens.text_faint));
                ui.end_row();

                ui.label(RichText::new("Install location").color(tokens.text_dim));
                ui.label(RichText::new(self.center.paths().apps.display().to_string()).monospace().color(tokens.text_faint));
                ui.end_row();
            });
        });

        ui.add_space(10.0);
        ui.horizontal(|ui| {
            let dirty = &self.draft != self.center.settings();
            if ui.add_enabled(dirty, egui::Button::new("Apply")).clicked() {
                self.apply_settings();
            }
            if widgets::secondary_button(ui, "Revert").clicked() {
                self.draft = self.center.settings().clone();
                self.kind = ThemeKind::from_id(&self.draft.theme).unwrap_or_default();
                theme::apply(ctx, self.kind);
            }
        });

        ui.add_space(14.0);
        widgets::dim(
            ui,
            "There is no telemetry to turn off and no access token to enter. The update check uses \
             github.com's release redirect, which costs none of the API's rate limit, so no \
             credential is needed.",
        );
    }

    fn apply_settings(&mut self) {
        // `Arc::get_mut` would fail while a worker thread holds a clone, so settings are written
        // through a short-lived exclusive borrow only when nothing is in flight.
        match Arc::get_mut(&mut self.center) {
            Some(center) => match center.set_settings(self.draft.clone()) {
                Ok(()) => self.message = Some(("Settings applied".to_owned(), Tone::Good)),
                Err(error) => self.message = Some((error.to_string(), Tone::Danger)),
            },
            None => self.message = Some(("Settings will be saved once the current download finishes".to_owned(), Tone::Warning)),
        }
    }

    fn about(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let tokens = Tokens::get(ctx);
        widgets::section_title(ui, "About CraftCenter");
        ui.add_space(8.0);
        widgets::card(ui, |ui| {
            ui.label(RichText::new(format!("CraftCenter {}", env!("CARGO_PKG_VERSION"))).font(medium(13.0)).color(tokens.text));
            ui.add_space(4.0);
            widgets::dim(ui, "An installer and updater for the Crafting Apps.");
            ui.add_space(10.0);
            widgets::hairline(ui);
            ui.add_space(10.0);
            widgets::dim(
                ui,
                "CraftCenter is unofficial. It is not made, sponsored or endorsed by the ArtCraft \
                 team. It downloads each app's official release assets from GitHub, verifies them \
                 against that release's SHA256SUMS.txt, and installs them for the current user \
                 without ever asking for administrator rights.",
            );
            ui.add_space(8.0);
            widgets::dim(
                ui,
                "The app icons are each app's own artwork, used under the MIT or Apache-2.0 licence \
                 their repositories grant. The ArtCraft name, wordmark and mark are trademarks and \
                 are not used here. Inter and JetBrains Mono are used under the SIL Open Font \
                 License 1.1. Full details are in ATTRIBUTION.md.",
            );
            ui.add_space(10.0);
            widgets::hairline(ui);
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if widgets::secondary_button(ui, "Check for an update").clicked() {
                    self.self_update(ctx);
                }
                if self.restart_needed {
                    widgets::pill(ui, "restart to run the new build", Tone::Accent);
                }
            });
        });
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        let Some((message, tone)) = self.message.clone() else {
            return;
        };
        let tokens = Tokens::get(ui.ctx());
        egui::Panel::bottom("status").exact_size(34.0).frame(egui::Frame::NONE.fill(tokens.chrome).inner_margin(egui::Margin::symmetric(10, 6))).show(
            ui,
            |ui| {
                ui.horizontal(|ui| {
                    widgets::pill(ui, &message, tone);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("Dismiss").clicked() {
                            self.message = None;
                        }
                    });
                });
            },
        );
    }
}

fn tone_for(status: &Status) -> Tone {
    match status {
        Status::UpToDate => Tone::Good,
        Status::UpdateAvailable { .. } => Tone::Accent,
        Status::Available => Tone::Neutral,
        Status::NoRelease | Status::Unchecked => Tone::Neutral,
        Status::Unavailable { .. } => Tone::Warning,
    }
}

impl CraftCenterApp {
    /// Draw one frame.
    ///
    /// Takes a `Ui` rather than implementing a windowing framework's trait, so this crate never
    /// depends on eframe, winit or a renderer: the binary owns that wiring, and this crate stays
    /// a pure egui shell that a test harness can drive.
    pub fn ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let ctx = &ctx;
        self.drain_events();

        // One check per launch, started after the first frame so the window appears immediately
        // rather than after the network answers.
        if !self.checked_on_start {
            self.checked_on_start = true;
            let stale: Vec<String> = self.rows.iter().filter(|row| !self.center.is_fresh(&row.app.slug)).map(|row| row.app.slug.clone()).collect();
            for slug in stale {
                let owned = slug.clone();
                self.spawn(ctx, &slug.clone(), move |center, sender, ctx| {
                    let event = match center.check(&owned, false) {
                        Ok(_) => Event::Checked { slug: owned.clone() },
                        Err(error) => Event::Failed { slug: owned.clone(), message: error.to_string() },
                    };
                    let _ = sender.send(event);
                    ctx.request_repaint();
                });
            }
        }

        self.top_bar(ui);
        self.status_bar(ui);

        let tokens = Tokens::get(ctx);
        egui::CentralPanel::default_margins().frame(egui::Frame::NONE.fill(tokens.dock).inner_margin(egui::Margin::same(12))).show(ui, |ui| match self.view {
            View::Catalogue => self.catalogue(ui, ctx),
            View::Settings => self.settings(ui, ctx),
            View::About => self.about(ui, ctx),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_tones_separate_what_needs_attention_from_what_does_not() {
        assert_eq!(tone_for(&Status::UpToDate), Tone::Good);
        assert_eq!(tone_for(&Status::UpdateAvailable { from: "1".into(), to: "2".into() }), Tone::Accent);
        assert_eq!(tone_for(&Status::Unavailable { reason: "x".into() }), Tone::Warning);
        assert_eq!(tone_for(&Status::Available), Tone::Neutral);
    }

    #[test]
    fn every_status_has_a_tone_and_a_label() {
        let statuses = [
            Status::NoRelease,
            Status::Available,
            Status::UpToDate,
            Status::UpdateAvailable { from: "0.1.0".into(), to: "0.2.0".into() },
            Status::Unavailable { reason: "nothing for this platform".into() },
            Status::Unchecked,
        ];
        for status in statuses {
            assert!(!status.label().is_empty(), "{status:?} has no label");
            let _ = tone_for(&status);
        }
    }

    #[test]
    fn only_actionable_rows_offer_a_primary_action() {
        assert!(Status::Available.is_actionable());
        assert!(Status::UpdateAvailable { from: "1".into(), to: "2".into() }.is_actionable());
        assert!(!Status::NoRelease.is_actionable());
        assert!(!Status::Unchecked.is_actionable());
    }
}
