//! The shared widget set.
//!
//! Every one of these reads its colours from [`Tokens`], so switching theme changes the whole
//! window and no widget has to know which theme is current.

use egui::{Color32, Rect, Response, RichText, Sense, Stroke, StrokeKind, Ui, Vec2};

use crate::theme::{Tokens, medium, semibold};

/// A panel group: the container everything in the window sits in.
///
/// The Pro themes draw a flat panel with a hairline, the Studio themes a rounded card, and
/// Classic a raised bevel — the same three grammars the Crafting Apps use.
pub fn card<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    let tokens = Tokens::get(ui.ctx());
    let mut frame = egui::Frame::NONE.fill(tokens.card).inner_margin(egui::Margin::symmetric(12, 10)).corner_radius(tokens.radius());
    frame = if tokens.bevel { frame.stroke(Stroke::new(1.0, Color32::WHITE)) } else { frame.stroke(Stroke::new(1.0, tokens.card_border)) };
    frame.show(ui, add).inner
}

/// One card of the catalogue grid: a fixed-size panel whose shell is painted here and whose
/// contents the caller draws into [`Tile::inner`].
///
/// Fixed size is the point of a grid — cards of different heights would stop being a grid — so
/// the tile allocates the size it is given rather than growing to its contents, and anything
/// that does not fit is the caller's problem to truncate.
pub struct Tile {
    /// The whole card, including the part the caller must not draw in.
    pub rect: Rect,
    /// The card inside its padding: where the contents go.
    pub inner: Rect,
    /// Clicks on the card body. Widgets drawn afterwards take precedence over it, so a click on
    /// the action button is not also a click on the card.
    pub response: Response,
}

pub fn tile(ui: &mut Ui, size: Vec2, focused: bool) -> Tile {
    let tokens = Tokens::get(ui.ctx());
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let fill = if response.hovered() { tokens.hover } else { tokens.card };
    let stroke = if focused {
        Stroke::new(2.0, tokens.accent)
    } else if tokens.bevel {
        Stroke::new(1.0, Color32::WHITE)
    } else {
        Stroke::new(1.0, tokens.card_border)
    };
    ui.painter().rect(rect, tokens.radius(), fill, stroke, StrokeKind::Inside);
    Tile { rect, inner: rect.shrink2(Vec2::new(12.0, 10.0)), response }
}

/// The lead action on a card, as a pill. Same fill as [`primary_button`]; the shape is what
/// separates the one thing a card is for from everything in its overflow menu.
pub fn pill_button(ui: &mut Ui, text: &str) -> Response {
    let tokens = Tokens::get(ui.ctx());
    ui.add(
        egui::Button::new(RichText::new(text).font(medium(12.0)).color(tokens.primary_text))
            .fill(tokens.primary_bg)
            .corner_radius(egui::CornerRadius::same(12))
            .stroke(Stroke::NONE)
            .min_size(Vec2::new(74.0, 24.0)),
    )
}

/// A download in flight, drawn as a bar across the foot of a card rather than as a widget in the
/// layout, so a card does not change height when one starts.
pub fn card_progress(ui: &Ui, card: Rect, done: u64, total: Option<u64>) {
    let tokens = Tokens::get(ui.ctx());
    let track = Rect::from_min_max(egui::pos2(card.left() + 12.0, card.bottom() - 7.0), egui::pos2(card.right() - 12.0, card.bottom() - 3.0));
    ui.painter().rect_filled(track, tokens.radius_sm(), tokens.field);
    let fraction = match total {
        Some(total) if total > 0 => (done as f32 / total as f32).clamp(0.0, 1.0),
        // No declared length: a bar that never fills would lie, so only the track is drawn.
        _ => 0.0,
    };
    if fraction > 0.0 {
        let filled = Rect::from_min_max(track.min, egui::pos2(track.left() + track.width() * fraction, track.bottom()));
        ui.painter().rect_filled(filled, tokens.radius_sm(), tokens.accent);
    }
}

/// A full-width hairline, for separating rows.
pub fn hairline(ui: &mut Ui) {
    let tokens = Tokens::get(ui.ctx());
    let height = 1.0;
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), height), Sense::hover());
    ui.painter().rect_filled(rect, 0.0, tokens.separator);
}

/// The affirmative button in a row: install, update, update all.
pub fn primary_button(ui: &mut Ui, text: &str) -> Response {
    let tokens = Tokens::get(ui.ctx());
    ui.add(
        egui::Button::new(RichText::new(text).font(medium(12.0)).color(tokens.primary_text))
            .fill(tokens.primary_bg)
            .corner_radius(tokens.radius_sm())
            .stroke(Stroke::NONE)
            .min_size(Vec2::new(78.0, 24.0)),
    )
}

/// Everything else: launch, remove, settings.
pub fn secondary_button(ui: &mut Ui, text: &str) -> Response {
    let tokens = Tokens::get(ui.ctx());
    ui.add(
        egui::Button::new(RichText::new(text).color(tokens.text_dim))
            .fill(tokens.field)
            .corner_radius(tokens.radius_sm())
            .stroke(Stroke::new(1.0, tokens.field_border))
            .min_size(Vec2::new(78.0, 24.0)),
    )
}

/// A destructive action, which never gets the primary fill.
pub fn danger_button(ui: &mut Ui, text: &str) -> Response {
    let tokens = Tokens::get(ui.ctx());
    ui.add(
        egui::Button::new(RichText::new(text).color(tokens.danger))
            .fill(tokens.field)
            .corner_radius(tokens.radius_sm())
            .stroke(Stroke::new(1.0, tokens.field_border))
            .min_size(Vec2::new(78.0, 24.0)),
    )
}

/// How a status pill reads: state carried by colour as well as by the words, so what needs
/// attention is visible without reading every row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Neutral,
    Accent,
    Good,
    Warning,
    Danger,
}

/// A small filled label. Used for the per-row status.
pub fn pill(ui: &mut Ui, text: &str, tone: Tone) -> Response {
    let tokens = Tokens::get(ui.ctx());
    let (fill, fg) = match tone {
        Tone::Neutral => (tokens.field, tokens.text_faint),
        Tone::Accent => (tokens.accent_soft, tokens.accent_text),
        Tone::Good => (tokens.field, tokens.good),
        Tone::Warning => (tokens.field, tokens.warning),
        Tone::Danger => (tokens.field, tokens.danger),
    };
    let galley = ui.painter().layout_no_wrap(text.to_owned(), egui::TextStyle::Small.resolve(ui.style()), fg);
    let padding = Vec2::new(8.0, 3.0);
    let (rect, response) = ui.allocate_exact_size(galley.size() + padding * 2.0, Sense::hover());
    ui.painter().rect_filled(rect, tokens.radius_sm(), fill);
    ui.painter().galley(rect.min + padding, galley, fg);
    response
}

/// The colour a [`Tone`] writes in. Shared with the cards, so a status reads the same wherever
/// it is drawn.
pub fn tone_text(ui: &Ui, tone: Tone) -> Color32 {
    let tokens = Tokens::get(ui.ctx());
    match tone {
        Tone::Neutral => tokens.text_faint,
        Tone::Accent => tokens.accent_text,
        Tone::Good => tokens.good,
        Tone::Warning => tokens.warning,
        Tone::Danger => tokens.danger,
    }
}

/// How far a download has got, in words.
pub fn transfer_label(done: u64, total: Option<u64>) -> String {
    match total {
        Some(total) if total > 0 => format!("{:.0} of {:.0} MiB", mib(done), mib(total)),
        _ => format!("{:.0} MiB", mib(done)),
    }
}

/// A heading above a group of rows.
pub fn section_title(ui: &mut Ui, text: &str) {
    let tokens = Tokens::get(ui.ctx());
    ui.label(RichText::new(text).font(semibold(13.0)).color(tokens.text));
}

/// Dimmed supporting text.
pub fn dim(ui: &mut Ui, text: &str) {
    let tokens = Tokens::get(ui.ctx());
    ui.label(RichText::new(text).color(tokens.text_dim));
}

/// A version, a size, anything that should line up down a column.
pub fn numeric(ui: &mut Ui, text: &str) {
    let tokens = Tokens::get(ui.ctx());
    ui.label(RichText::new(text).monospace().color(tokens.text_dim));
}

/// A download in flight.
pub fn progress(ui: &mut Ui, done: u64, total: Option<u64>) {
    let tokens = Tokens::get(ui.ctx());
    let fraction = match total {
        Some(total) if total > 0 => (done as f32 / total as f32).clamp(0.0, 1.0),
        _ => 0.0,
    };
    let text = transfer_label(done, total);
    ui.add(
        egui::ProgressBar::new(fraction)
            .desired_width(160.0)
            .corner_radius(tokens.radius_sm())
            .fill(tokens.accent)
            .text(RichText::new(text).small().color(tokens.text_dim)),
    );
}

fn mib(bytes: u64) -> f64 {
    bytes as f64 / 1_048_576.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mebibytes_are_reported_in_mebibytes() {
        assert!((mib(2 * 1_048_576) - 2.0).abs() < f64::EPSILON);
    }

    #[test]
    fn every_tone_is_distinct_in_at_least_one_theme() {
        let tokens = Tokens::for_kind(crate::theme::ThemeKind::ProMedium);
        let colours = [tokens.text_faint, tokens.accent_text, tokens.good, tokens.warning, tokens.danger];
        for (index, colour) in colours.iter().enumerate() {
            for other in colours.iter().skip(index + 1) {
                assert_ne!(colour, other, "two tones would be drawn in the same colour");
            }
        }
    }
}
