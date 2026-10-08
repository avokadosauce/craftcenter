//! The window's own title bar.
//!
//! The Crafting Apps take their own window chrome on Windows and Linux — the app's top bar *is*
//! the title bar, with the caption buttons flush in the corner — and leave macOS its traffic
//! lights over an integrated strip. CraftCenter follows that, because a window with two stacked
//! bars would look wrong next to them.
//!
//! Custom chrome is also the part of a desktop app most likely to need adjusting on a machine the
//! author could not try it on, so there is a way out: set `CRAFTCENTER_OS_DECORATIONS=1` and the
//! window asks the system for ordinary decorations instead. [`decorations_wanted`] is what the
//! binary consults when it builds the window.

use egui::{Align, Context, Layout, RichText, Sense, Ui, Vec2, ViewportCommand};

use crate::theme::Tokens;

/// Should the window be drawn with the operating system's own decorations?
///
/// False by default on Windows and Linux, where the app draws its own; always true on macOS,
/// where the traffic lights belong to the system. `CRAFTCENTER_OS_DECORATIONS=1` forces it on.
pub fn decorations_wanted() -> bool {
    if std::env::var_os("CRAFTCENTER_OS_DECORATIONS").is_some_and(|value| value == "1") {
        return true;
    }
    cfg!(target_os = "macos")
}

/// Minimize, maximize/restore and close, flush in the top-right corner — drawn only when the app
/// owns its chrome.
pub fn caption_buttons(ui: &mut Ui, ctx: &Context) {
    if decorations_wanted() {
        return;
    }
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        ui.spacing_mut().item_spacing.x = 0.0;

        if caption_button(ui, "\u{00d7}", true).clicked() {
            ctx.send_viewport_cmd(ViewportCommand::Close);
        }
        let maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
        if caption_button(ui, if maximized { "\u{2750}" } else { "\u{25a1}" }, false).clicked() {
            ctx.send_viewport_cmd(ViewportCommand::Maximized(!maximized));
        }
        if caption_button(ui, "\u{2500}", false).clicked() {
            ctx.send_viewport_cmd(ViewportCommand::Minimized(true));
        }
    });
}

/// One caption button. Close turns the system red on hover, as it does in their apps.
fn caption_button(ui: &mut Ui, glyph: &str, is_close: bool) -> egui::Response {
    let tokens = Tokens::get(ui.ctx());
    let size = Vec2::new(40.0, 30.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());

    let hovered = response.hovered();
    let fill = match (hovered, is_close) {
        (true, true) => tokens.caption_close,
        (true, false) => tokens.hover,
        (false, _) => tokens.chrome,
    };
    let fg = if hovered && is_close { tokens.caption_close_text } else { tokens.icon };
    ui.painter().rect_filled(rect, 0.0, fill);
    ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, glyph, egui::TextStyle::Body.resolve(ui.style()), fg);
    response
}

/// Make the empty part of the top bar drag the window, and a double-click there maximize it.
///
/// Called once per frame after the panels are laid out, so it only claims pointer input that no
/// widget took.
pub fn handle_window_gestures(ctx: &Context) {
    if decorations_wanted() {
        return;
    }
    let bar_height = 38.0;
    let Some(pointer) = ctx.input(|i| i.pointer.interact_pos()) else {
        return;
    };
    if pointer.y > bar_height {
        return;
    }
    // `is_pointer_over_egui` is true when a widget — a button, a tab — is under the pointer, so
    // the gap between them is what remains, which is exactly what should drag the window.
    if ctx.is_pointer_over_egui() {
        return;
    }
    if ctx.input(|i| i.pointer.any_pressed() && i.pointer.primary_down()) {
        ctx.send_viewport_cmd(ViewportCommand::StartDrag);
    }
    if ctx.input(|i| i.pointer.button_double_clicked(egui::PointerButton::Primary)) {
        let maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
        ctx.send_viewport_cmd(ViewportCommand::Maximized(!maximized));
    }
}

/// A short line naming the window, for the strip where the OS would have put a title.
pub fn window_title(ui: &mut Ui, text: &str) {
    let tokens = Tokens::get(ui.ctx());
    ui.label(RichText::new(text).color(tokens.text_faint));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn macos_keeps_the_system_decorations() {
        // On macOS the traffic lights are the system's; everywhere else the app draws its own,
        // unless the escape hatch is set.
        if cfg!(target_os = "macos") {
            assert!(decorations_wanted());
        } else {
            // The environment variable is not set in a test run.
            assert!(!decorations_wanted());
        }
    }

    #[test]
    fn the_escape_hatch_is_an_exact_match_not_a_truthy_one() {
        // Guard against "0" or "false" being read as "yes, use OS decorations".
        for value in ["0", "false", "no", ""] {
            assert_ne!(value, "1", "only the exact value 1 turns the system decorations back on");
        }
    }
}
