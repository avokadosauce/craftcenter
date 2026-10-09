//! The CraftCenter desktop shell.
//!
//! One window: a catalogue of the Crafting Apps as a grid of cards, grouped into what is
//! installed and what is available. The shell is thin — every action is a call into
//! `craftcenter-core`, the same calls the command-line front end makes, so the two cannot drift
//! apart in behaviour.
//!
//! Nothing blocks the interface. A check or a download runs on a worker thread and reports back
//! over a channel; the window stays interactive and shows progress on the card it belongs to.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable, clippy::indexing_slicing)]

pub mod theme;
pub mod titlebar;
pub mod widgets;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};

use craftcenter_core::{Center, Row, Settings, Status};
use craftcenter_select::{Os, Platform, Preference, select};
use egui::{RichText, TextureHandle};

use theme::{ThemeKind, Tokens, medium, mono, semibold};
use widgets::Tone;

/// Ask the user to choose a folder, starting at the one given.
///
/// Supplied by the binary. This crate never talks to the window system — that is what lets it be
/// tested without a display — so the platform's own folder dialog sits on the other side of this.
pub type FolderPicker = Box<dyn Fn(&Path) -> Option<PathBuf>>;

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
    Moved { slug: String },
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
    /// Which platforms each app's latest release publishes for, recomputed when the rows are.
    platforms: HashMap<String, Vec<Os>>,
    /// The selected card, by slug. Kept as a slug rather than an index so it survives an app
    /// moving between the two grids when it is installed or removed.
    focus: Option<String>,
    /// Installed apps that are not in the current install location, so the settings screen can
    /// offer to move them without reading the state file on every frame.
    misplaced: Vec<String>,
    picker: Option<FolderPicker>,
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
        let platforms = rows.iter().map(|row| (row.app.slug.clone(), published_for(row))).collect();
        let misplaced = center.misplaced();
        Self {
            center,
            rows,
            view: View::default(),
            kind,
            activity: HashMap::new(),
            icons: HashMap::new(),
            platforms,
            focus: None,
            misplaced,
            picker: None,
            message: None,
            draft,
            events,
            sender,
            checked_on_start: false,
            restart_needed: false,
        }
    }

    /// Hand the shell the platform's folder dialog. Without one, the install location can still
    /// be read and reset; it just cannot be chosen from this window.
    pub fn with_folder_picker(mut self, picker: FolderPicker) -> Self {
        self.picker = Some(picker);
        self
    }

    fn refresh_rows(&mut self) {
        self.rows = self.center.rows();
        self.platforms = self.rows.iter().map(|row| (row.app.slug.clone(), published_for(row))).collect();
        self.misplaced = self.center.misplaced();
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
                Event::Moved { slug } => {
                    self.activity.remove(&slug);
                    self.message = Some((format!("Moved {slug}"), Tone::Good));
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
                    self.message =
                        Some((format!("CraftCenter {to} is installed. Restart to run it; the previous build is kept until the next start."), Tone::Accent));
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
        ui.add_space(8.0);

        // Two grids rather than one: with a dozen apps, "what have I got, and does any of it need
        // updating" should be answerable without reading every card. The state is still written on
        // each card, because a card has to make sense on its own once it is the only one you are
        // looking at.
        let (installed, available): (Vec<Row>, Vec<Row>) = self.rows.clone().into_iter().partition(|row| row.installed.is_some());

        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            let columns = grid_columns(ui.available_width());
            let width = tile_width(ui.available_width(), columns);
            let slots = grid_slots(&slugs(&installed), &slugs(&available), columns);
            self.handle_grid_keys(ctx, &slots, columns);

            if !installed.is_empty() {
                widgets::section_title(ui, "Installed");
                ui.add_space(8.0);
                self.tile_grid(ui, ctx, &installed, columns, width);
                ui.add_space(16.0);
            }
            if !available.is_empty() {
                widgets::section_title(ui, "Available");
                ui.add_space(8.0);
                self.tile_grid(ui, ctx, &available, columns, width);
            }

            ui.add_space(10.0);
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

    /// One section's cards, `columns` across, in reading order.
    fn tile_grid(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, rows: &[Row], columns: usize, width: f32) {
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(TILE_GAP, TILE_GAP);
            for line in rows.chunks(columns.max(1)) {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = TILE_GAP;
                    for row in line {
                        self.app_tile(ui, ctx, row, width);
                    }
                });
            }
        });
    }

    /// Arrow keys move the selection through the grid; Enter does what the card's own button
    /// does.
    ///
    /// Not "while nothing has keyboard focus": clicking a card, or any button on one, gives that
    /// widget egui's focus, and the grid would then stop answering the arrow keys for the rest
    /// of the session. The two things that genuinely own the keyboard are a field being typed
    /// into and an open menu, and those are what stand aside for.
    fn handle_grid_keys(&mut self, ctx: &egui::Context, slots: &[Option<String>], columns: usize) {
        if ctx.text_edit_focused() || ctx.any_popup_open() {
            return;
        }
        let (left, right, up, down, enter) = ctx.input(|i| {
            (
                i.key_pressed(egui::Key::ArrowLeft),
                i.key_pressed(egui::Key::ArrowRight),
                i.key_pressed(egui::Key::ArrowUp),
                i.key_pressed(egui::Key::ArrowDown),
                i.key_pressed(egui::Key::Enter),
            )
        });
        let step = match (left, right, up, down) {
            (true, _, _, _) => Some(Step::Left),
            (_, true, _, _) => Some(Step::Right),
            (_, _, true, _) => Some(Step::Up),
            (_, _, _, true) => Some(Step::Down),
            _ => None,
        };
        if let Some(step) = step {
            let current = self.focus.as_deref().and_then(|slug| slots.iter().position(|slot| slot.as_deref() == Some(slug)));
            self.focus = match current {
                // Nothing selected yet: the first arrow key selects the first card.
                None => slots.iter().flatten().next().cloned(),
                Some(current) => slots.get(step_focus(slots, current, step, columns)).cloned().flatten(),
            };
        }
        if enter && let Some(row) = self.focus.as_deref().and_then(|slug| self.rows.iter().find(|row| row.app.slug == slug)).cloned() {
            self.primary_action(ctx, &row);
        }
    }

    /// One app, as a card of the grid.
    fn app_tile(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, row: &Row, width: f32) {
        let tokens = Tokens::get(ctx);
        let slug = row.app.slug.clone();
        let focused = self.focus.as_deref() == Some(slug.as_str());
        let activity = self.activity.get(&slug).cloned().unwrap_or_default();
        let icon = self.icon(ctx, row);
        let platforms = self.platforms.get(&slug).cloned().unwrap_or_default();

        let tile = widgets::tile(ui, egui::vec2(width, TILE_HEIGHT), focused);
        if tile.response.clicked() {
            self.focus = Some(slug.clone());
        }

        let mut body = ui.new_child(egui::UiBuilder::new().max_rect(tile.inner).layout(egui::Layout::top_down(egui::Align::Min)));
        let body = &mut body;
        body.spacing_mut().item_spacing.y = 0.0;

        // Identity: what it is, and where it runs.
        body.horizontal(|ui| {
            match &icon {
                Some(handle) => {
                    ui.add(egui::Image::new(handle).fit_to_exact_size(egui::vec2(26.0, 26.0)));
                }
                None => ui.add_space(26.0),
            }
            ui.add_space(8.0);
            ui.label(RichText::new(&row.app.name).font(semibold(13.0)).color(tokens.text));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| platform_tags(ui, &platforms));
        });
        body.add_space(7.0);
        body.add(egui::Label::new(RichText::new(&row.app.tagline).color(tokens.text_dim)).truncate());

        // The footer sits on the bottom edge of a card whose height never changes, so the grid
        // stays a grid however long a tagline or a status reason is.
        let footer_height = 24.0;
        let reserved = 1.0 + 8.0 + footer_height;
        body.add_space((body.available_height() - reserved).max(0.0));
        widgets::hairline(body);
        body.add_space(8.0);
        body.allocate_ui_with_layout(egui::vec2(body.available_width(), footer_height), egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            self.overflow_menu(ui, ctx, row);
            if let Some(label) = row_action(row)
                && widgets::pill_button(ui, label).clicked()
            {
                self.focus = Some(slug.clone());
                self.primary_action(ctx, row);
            }
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| footer_state(ui, row, &activity));
        });

        if activity.busy {
            widgets::card_progress(ui, tile.rect, activity.done, activity.total);
        }
    }

    /// Everything a card can do that is not its one lead action.
    fn overflow_menu(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, row: &Row) {
        let tokens = Tokens::get(ui.ctx());
        let slug = row.app.slug.clone();
        let installed = row.installed.is_some();
        let button = egui::Button::new(RichText::new("\u{2026}").font(medium(14.0)).color(tokens.text_dim))
            .fill(tokens.field)
            .corner_radius(tokens.radius_sm())
            .stroke(egui::Stroke::new(1.0, tokens.field_border))
            .min_size(egui::vec2(28.0, 24.0));

        egui::containers::menu::MenuButton::from_button(button).ui(ui, |ui| {
            // Launch is the lead action only when there is nothing to install; everywhere else an
            // installed app can still be started from here.
            if installed && row_action(row) != Some("Launch") && ui.button("Launch").clicked() {
                self.launch(&slug);
                ui.close();
            }
            if ui.add_enabled(installed, egui::Button::new("Open folder")).clicked() {
                if let Err(error) = self.center.open_install_dir(&slug) {
                    self.message = Some((error.to_string(), Tone::Danger));
                }
                ui.close();
            }
            // Verify is hidden here, not removed: `Center::verify` only re-hashes the AppImage or
            // launcher file, so on a DMG-installed .app or an unpacked Windows build it checks one
            // file out of many and calls that "verified". It returns once an install-time manifest
            // covers every file a format writes. The CLI's `verify` is unaffected.
            if ui.button("Release notes").clicked() {
                if let Err(error) = self.center.open_release_notes(&slug) {
                    self.message = Some((error.to_string(), Tone::Danger));
                }
                ui.close();
            }
            ui.separator();
            if ui.add_enabled(installed, egui::Button::new("Remove")).clicked() {
                self.remove(ctx, &slug);
                ui.close();
            }
        });
    }

    /// Do what the card's lead button does. Shared with Enter, so the keyboard and the pointer
    /// cannot come to different conclusions about what a card is for.
    fn primary_action(&mut self, ctx: &egui::Context, row: &Row) {
        let slug = row.app.slug.clone();
        // Enter reaches both this and egui's own "activate the focused button", and a card can
        // be double-clicked; either way one download is enough.
        if self.activity.get(&slug).is_some_and(|activity| activity.busy) {
            return;
        }
        match row_action(row) {
            Some("Install" | "Update") => self.install(ctx, &slug),
            Some("Launch") => self.launch(&slug),
            _ => {}
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
                self.install_location(ui, ctx);
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

    /// Where apps go, and the two things that can be done about it.
    ///
    /// Unlike the rest of this screen, choosing a location takes effect at once rather than on
    /// Apply: it is the answer to a dialog the user has just dismissed, and leaving it pending
    /// behind a second button would read as though nothing had happened.
    fn install_location(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let tokens = Tokens::get(ctx);
        let current = self.center.install_dir().to_path_buf();
        let default = self.center.default_install_dir().to_path_buf();

        ui.vertical(|ui| {
            ui.add(egui::Label::new(RichText::new(current.display().to_string()).monospace().color(tokens.text_faint)).truncate());
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let can_pick = self.picker.is_some();
                if ui.add_enabled_ui(can_pick, |ui| widgets::secondary_button(ui, "Change\u{2026}")).inner.clicked()
                    && let Some(chosen) = self.picker.as_ref().and_then(|pick| pick(&current))
                {
                    self.choose_install_dir(Some(chosen));
                }
                if ui.add_enabled_ui(current != default, |ui| widgets::secondary_button(ui, "Reset to default")).inner.clicked() {
                    self.choose_install_dir(None);
                }
            });

            if self.misplaced.is_empty() {
                return;
            }
            ui.add_space(6.0);
            let count = self.misplaced.len();
            let what = if count == 1 { "1 installed app is".to_owned() } else { format!("{count} installed apps are") };
            widgets::dim(ui, &format!("{what} still in the folder they were installed into, and still work there. New installs go to the folder above."));
            ui.add_space(4.0);
            if widgets::secondary_button(ui, "Move installed apps").clicked() && !self.any_busy() {
                self.move_misplaced(ctx);
            }
        });
    }

    fn choose_install_dir(&mut self, dir: Option<PathBuf>) {
        // The same exclusive borrow the rest of the settings need: a worker thread holding a
        // clone of the core means the change waits rather than racing an install in flight.
        match Arc::get_mut(&mut self.center) {
            Some(center) => match center.set_install_dir(dir.as_deref()) {
                Ok(()) => self.message = Some((format!("New apps will be installed in {}", center.install_dir().display()), Tone::Good)),
                Err(error) => self.message = Some((error.to_string(), Tone::Danger)),
            },
            None => self.message = Some(("The install location can be changed once the current download finishes".to_owned(), Tone::Warning)),
        }
        self.draft.install_dir = self.center.settings().install_dir.clone();
        self.misplaced = self.center.misplaced();
    }

    /// Move everything that is not in the current install location, one app at a time.
    ///
    /// One worker for the whole run, not one per app: each move copies a whole application and
    /// reads it back to check it, and several at once would only make each of them slower.
    fn move_misplaced(&mut self, ctx: &egui::Context) {
        let slugs = self.center.misplaced();
        if slugs.is_empty() {
            return;
        }
        for slug in &slugs {
            self.start(slug);
        }
        let center = Arc::clone(&self.center);
        let sender = self.sender.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            for slug in slugs {
                let mut reporter = |done: u64, total: Option<u64>| {
                    let _ = sender.send(Event::Progress { slug: slug.clone(), done, total });
                    ctx.request_repaint();
                };
                let event = match center.move_app(&slug, &mut reporter) {
                    Ok(_) => Event::Moved { slug: slug.clone() },
                    Err(error) => Event::Failed { slug: slug.clone(), message: error.to_string() },
                };
                let _ = sender.send(event);
                ctx.request_repaint();
            }
        });
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
            if self.restart_needed {
                ui.add_space(8.0);
                widgets::dim(ui, "The previous build is kept until the next start, in case the new one does not run.");
            }
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

/// The grid. A card may not be squeezed below [`TILE_MIN_WIDTH`]; when it would be, a column is
/// dropped. Three across is where a wide window settles — a fourth would leave each card
/// narrower than a name, a version and a button read well in.
const TILE_MIN_WIDTH: f32 = 340.0;
const TILE_HEIGHT: f32 = 132.0;
const TILE_GAP: f32 = 10.0;
const TILE_MAX_COLUMNS: usize = 3;

/// How many cards fit across `available`: three on a wide window, two on the width the app
/// opens at, one on a narrow one.
fn grid_columns(available: f32) -> usize {
    let mut columns = 1;
    while columns < TILE_MAX_COLUMNS {
        let next = columns + 1;
        if tile_width(available, next) < TILE_MIN_WIDTH {
            break;
        }
        columns = next;
    }
    columns
}

/// What one card gets when `columns` of them share `available` with a gap between each pair.
fn tile_width(available: f32, columns: usize) -> f32 {
    let columns = columns.max(1);
    (available - TILE_GAP * (columns - 1) as f32) / columns as f32
}

/// The two grids as one list in reading order, with a hole for each empty cell at the end of the
/// first grid's last row.
///
/// The padding is the whole point: without it, pressing down from a half-full last row of
/// "Installed" would land somewhere sideways in "Available" instead of in the same column.
fn grid_slots(installed: &[String], available: &[String], columns: usize) -> Vec<Option<String>> {
    let columns = columns.max(1);
    let mut slots: Vec<Option<String>> = installed.iter().cloned().map(Some).collect();
    let overhang = slots.len() % columns;
    if overhang != 0 && !available.is_empty() {
        slots.extend(std::iter::repeat_n(None, columns - overhang));
    }
    slots.extend(available.iter().cloned().map(Some));
    slots
}

/// The slugs of `rows`, in order: what [`grid_slots`] and the keyboard work in terms of.
fn slugs(rows: &[Row]) -> Vec<String> {
    rows.iter().map(|row| row.app.slug.clone()).collect()
}

/// Which way an arrow key moves the selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
    Left,
    Right,
    Up,
    Down,
}

/// Move the selection one step through the grid.
///
/// A step onto one of [`grid_slots`]' holes keeps going the same way; a step that would leave the
/// grid does not move at all, because a selection that wraps round to the far corner is a
/// selection you then have to go looking for.
fn step_focus(slots: &[Option<String>], current: usize, step: Step, columns: usize) -> usize {
    let columns = columns.max(1) as isize;
    let delta = match step {
        Step::Left => -1,
        Step::Right => 1,
        Step::Up => -columns,
        Step::Down => columns,
    };
    let mut index = current as isize;
    loop {
        index += delta;
        let Ok(candidate) = usize::try_from(index) else {
            return current;
        };
        match slots.get(candidate) {
            None => return current,
            Some(Some(_)) => return candidate,
            Some(None) => {}
        }
    }
}

/// The one thing a card is for, or nothing when there is nothing to do with this app yet.
fn primary_label(status: &Status, installed: bool) -> Option<&'static str> {
    match status {
        Status::Available => Some("Install"),
        Status::UpdateAvailable { .. } => Some("Update"),
        Status::UpToDate => Some("Launch"),
        // A check that has not happened, or failed, is no reason to stop an app that is already
        // installed from being started.
        Status::Unchecked | Status::NoRelease | Status::Unavailable { .. } => installed.then_some("Launch"),
    }
}

/// [`primary_label`] for a whole row.
fn row_action(row: &Row) -> Option<&'static str> {
    primary_label(&row.status, row.installed.is_some())
}

/// The mark in the footer's left slot: the state, before the words.
fn status_mark(status: &Status) -> (&'static str, Tone) {
    match status {
        Status::UpToDate => ("\u{2713}", Tone::Good),
        Status::UpdateAvailable { .. } => ("\u{2191}", Tone::Accent),
        Status::Available => ("\u{2193}", Tone::Neutral),
        Status::Unavailable { .. } => ("\u{26a0}", Tone::Warning),
        Status::NoRelease | Status::Unchecked => ("\u{00b7}", Tone::Neutral),
    }
}

/// What is installed and what is published, as one line. `None` when neither is known yet, in
/// which case the footer says the status in words instead.
fn version_line(installed: Option<&str>, latest: Option<&str>, status: &Status) -> Option<String> {
    match (installed, latest, status) {
        (Some(installed), _, Status::UpdateAvailable { to, .. }) => Some(format!("{installed} \u{2192} {to}")),
        (Some(installed), _, _) => Some(installed.to_owned()),
        (None, Some(latest), _) => Some(latest.to_owned()),
        _ => None,
    }
}

/// The footer's left slot: a mark for the state, then the versions — or the status in words when
/// there is no version to show.
fn footer_state(ui: &mut egui::Ui, row: &Row, activity: &Activity) {
    let tokens = Tokens::get(ui.ctx());
    ui.spacing_mut().item_spacing.x = 5.0;
    if activity.busy {
        ui.label(RichText::new("\u{2193}").color(tokens.accent_text));
        ui.add(egui::Label::new(RichText::new(widgets::transfer_label(activity.done, activity.total)).small().color(tokens.text_dim)).truncate());
        return;
    }
    let (mark, tone) = status_mark(&row.status);
    let colour = widgets::tone_text(ui, tone);
    ui.label(RichText::new(mark).color(colour));
    let installed = row.installed.as_ref().map(|installed| installed.version.as_str());
    let latest = row.latest.as_ref().map(|latest| latest.version.as_str());
    let text = match version_line(installed, latest, &row.status) {
        Some(version) => RichText::new(version).font(mono(10.0)).color(tokens.text_dim),
        None => RichText::new(row.status.label()).small().color(tokens.text_faint),
    };
    ui.add(egui::Label::new(text).truncate());
}

/// Which operating systems the latest release publishes something installable for.
///
/// Answered by the same selector the install itself uses, so a card cannot advertise a platform
/// that would then turn out to have no asset CraftCenter can use.
fn published_for(row: &Row) -> Vec<Os> {
    let Some(release) = &row.latest else {
        return Vec::new();
    };
    [Os::Linux, Os::MacOs, Os::Windows]
        .into_iter()
        .filter(|os| {
            Platform::ALL
                .iter()
                .filter(|platform| platform.os == *os)
                .any(|platform| select(&row.app, *platform, &release.assets, Preference::default()).is_ok())
        })
        .collect()
}

fn os_tag(os: Os) -> (&'static str, &'static str) {
    match os {
        Os::Linux => ("LIN", "Published for Linux"),
        Os::MacOs => ("MAC", "Published for macOS"),
        Os::Windows => ("WIN", "Published for Windows"),
    }
}

/// The platform marks in the card's top-right corner. The platform this copy of CraftCenter is
/// running on is drawn in the ordinary text colour and the rest faintly, so "does this one run
/// on my machine" is answerable without reading anything.
fn platform_tags(ui: &mut egui::Ui, platforms: &[Os]) {
    let tokens = Tokens::get(ui.ctx());
    let host = Platform::host().map(|platform| platform.os);
    ui.spacing_mut().item_spacing.x = 5.0;
    // The layout runs right to left, so this list is in reverse reading order.
    for os in [Os::Windows, Os::MacOs, Os::Linux] {
        if !platforms.contains(&os) {
            continue;
        }
        let (tag, hover) = os_tag(os);
        let colour = if host == Some(os) { tokens.text } else { tokens.text_faint };
        ui.label(RichText::new(tag).font(mono(9.0)).color(colour)).on_hover_text(hover);
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

    fn names(slugs: &[&str]) -> Vec<String> {
        slugs.iter().map(|slug| (*slug).to_owned()).collect()
    }

    #[test]
    fn status_marks_separate_what_needs_attention_from_what_does_not() {
        assert_eq!(status_mark(&Status::UpToDate).1, Tone::Good);
        assert_eq!(status_mark(&Status::UpdateAvailable { from: "1".into(), to: "2".into() }).1, Tone::Accent);
        assert_eq!(status_mark(&Status::Unavailable { reason: "x".into() }).1, Tone::Warning);
        assert_eq!(status_mark(&Status::Available).1, Tone::Neutral);
    }

    #[test]
    fn every_status_has_a_mark_and_a_label() {
        for status in every_status() {
            assert!(!status.label().is_empty(), "{status:?} has no label");
            assert!(!status_mark(&status).0.is_empty(), "{status:?} has no mark");
        }
    }

    #[test]
    fn only_actionable_rows_offer_a_primary_action() {
        assert!(Status::Available.is_actionable());
        assert!(Status::UpdateAvailable { from: "1".into(), to: "2".into() }.is_actionable());
        assert!(!Status::NoRelease.is_actionable());
        assert!(!Status::Unchecked.is_actionable());
    }

    #[test]
    fn a_cards_lead_button_follows_its_state() {
        assert_eq!(primary_label(&Status::Available, false), Some("Install"));
        assert_eq!(primary_label(&Status::UpdateAvailable { from: "1".into(), to: "2".into() }, true), Some("Update"));
        assert_eq!(primary_label(&Status::UpToDate, true), Some("Launch"));
        // A check that never happened, or failed, still leaves an installed app runnable.
        assert_eq!(primary_label(&Status::Unchecked, true), Some("Launch"));
        assert_eq!(primary_label(&Status::Unavailable { reason: "x".into() }, true), Some("Launch"));
        // Nothing installed and nothing to install: the card offers nothing.
        assert_eq!(primary_label(&Status::Unchecked, false), None);
        assert_eq!(primary_label(&Status::NoRelease, false), None);
    }

    #[test]
    fn the_footer_shows_both_versions_when_an_update_is_waiting() {
        let update = Status::UpdateAvailable { from: "0.1.0".into(), to: "0.2.0".into() };
        assert_eq!(version_line(Some("0.1.0"), Some("0.2.0"), &update).as_deref(), Some("0.1.0 \u{2192} 0.2.0"));
        assert_eq!(version_line(Some("0.2.0"), Some("0.2.0"), &Status::UpToDate).as_deref(), Some("0.2.0"));
        assert_eq!(version_line(None, Some("0.2.0"), &Status::Available).as_deref(), Some("0.2.0"));
        // Nothing known yet: the footer says the status in words instead.
        assert_eq!(version_line(None, None, &Status::Unchecked), None);
    }

    #[test]
    fn the_grid_is_three_across_on_a_wide_window_and_one_on_a_narrow_one() {
        assert_eq!(grid_columns(1_262.0), 3, "a 1300 px window");
        assert_eq!(grid_columns(900.0), 2, "the width the app opens at");
        assert_eq!(grid_columns(480.0), 1, "near the minimum window width");
        assert_eq!(grid_columns(4_000.0), TILE_MAX_COLUMNS, "a wide monitor never gets a fourth column");
    }

    #[test]
    fn a_row_of_cards_fills_its_width_exactly_and_never_below_the_minimum() {
        for available in (320..=3_000).step_by(13) {
            let available = available as f32;
            let columns = grid_columns(available);
            let width = tile_width(available, columns);
            let used = width * columns as f32 + TILE_GAP * (columns - 1) as f32;
            assert!((used - available).abs() < 0.01, "{columns} cards of {width} do not fill {available}");
            if columns > 1 {
                assert!(width >= TILE_MIN_WIDTH, "{columns} columns squeezed a card to {width} at {available}");
            }
        }
    }

    #[test]
    fn the_down_arrow_keeps_its_column_across_the_two_grids() {
        // Four installed in a three-wide grid, so the second row has two empty cells after it.
        let slots = grid_slots(&names(&["a", "b", "c", "d"]), &names(&["e", "f", "g"]), 3);
        assert_eq!(slots.len(), 9);
        assert_eq!(slots.get(4), Some(&None), "the hole after the last installed card");
        // Down from "a" (column 0, row 0) reaches "d" (column 0, row 1), then "e" below it.
        let a = 0;
        let d = step_focus(&slots, a, Step::Down, 3);
        assert_eq!(slots.get(d).cloned().flatten().as_deref(), Some("d"));
        let e = step_focus(&slots, d, Step::Down, 3);
        assert_eq!(slots.get(e).cloned().flatten().as_deref(), Some("e"));
        // Down from "b", whose cell below is empty, carries on to the next real card in the
        // same column rather than landing nowhere.
        let b = 1;
        let f = step_focus(&slots, b, Step::Down, 3);
        assert_eq!(slots.get(f).cloned().flatten().as_deref(), Some("f"));
    }

    #[test]
    fn an_arrow_at_the_edge_of_the_grid_does_not_move() {
        let slots = grid_slots(&names(&["a", "b"]), &names(&["c", "d", "e"]), 3);
        assert_eq!(step_focus(&slots, 0, Step::Left, 3), 0, "left from the first card");
        assert_eq!(step_focus(&slots, 0, Step::Up, 3), 0, "up from the top row");
        let last = slots.len() - 1;
        assert_eq!(step_focus(&slots, last, Step::Right, 3), last, "right from the last card");
        assert_eq!(step_focus(&slots, last, Step::Down, 3), last, "down from the bottom row");
    }

    #[test]
    fn left_and_right_walk_every_card_in_reading_order() {
        let slots = grid_slots(&names(&["a", "b", "c", "d"]), &names(&["e", "f"]), 3);
        let mut visited = vec!["a".to_owned()];
        let mut index = 0;
        loop {
            let next = step_focus(&slots, index, Step::Right, 3);
            if next == index {
                break;
            }
            index = next;
            if let Some(Some(slug)) = slots.get(index) {
                visited.push(slug.clone());
            }
        }
        assert_eq!(visited, names(&["a", "b", "c", "d", "e", "f"]), "a hole must not end the walk");
    }

    #[test]
    fn one_grid_alone_is_not_padded() {
        let slots = grid_slots(&names(&["a", "b"]), &[], 3);
        assert!(slots.iter().all(Option::is_some), "no trailing holes when there is no second grid");
    }

    fn every_status() -> [Status; 6] {
        [
            Status::NoRelease,
            Status::Available,
            Status::UpToDate,
            Status::UpdateAvailable { from: "0.1.0".into(), to: "0.2.0".into() },
            Status::Unavailable { reason: "nothing for this platform".into() },
            Status::Unchecked,
        ]
    }
}
