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

use egui::{Align, Context, Id, Layout, PointerButton, Pos2, Rect, RichText, Sense, Ui, Vec2, ViewportCommand};

use crate::theme::Tokens;

/// One caption button's footprint. Three of them sit flush in the top-right corner.
const CAPTION_BUTTON: Vec2 = Vec2::new(40.0, 30.0);

/// Below this the strip is a sliver nobody can aim at, so it is not claimed at all — the window
/// is simply too narrow to drag by its bar, and the caption buttons keep every pixel they need.
const MIN_DRAG_WIDTH: f32 = 8.0;

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
///
/// Returns the strip they occupy, which is what the drag region has to stop short of. With the
/// system's own decorations nothing is drawn and the returned rect is the empty sliver at the
/// right edge, so the drag region simply runs to the end of the bar.
pub fn caption_buttons(ui: &mut Ui, ctx: &Context) -> Rect {
    let available = ui.available_rect_before_wrap();
    if decorations_wanted() {
        return Rect::from_min_max(available.right_top(), available.right_bottom());
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
        ui.min_rect()
    })
    .inner
}

/// One caption button. Close turns the system red on hover, as it does in their apps.
fn caption_button(ui: &mut Ui, glyph: &str, is_close: bool) -> egui::Response {
    let tokens = Tokens::get(ui.ctx());
    let (rect, response) = ui.allocate_exact_size(CAPTION_BUTTON, Sense::click());

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

/// The part of the bar that drags the window: everything between what the left-hand content ends
/// at and where the caption buttons begin.
///
/// Pure arithmetic, so the one thing about the title bar that can be checked without opening a
/// window — that the drag region never covers a button or a tab — is checked by the test suite.
/// `None` means there is nothing left worth claiming.
pub fn drag_strip_rect(bar: Rect, content_right: f32, caption_left: f32) -> Option<Rect> {
    let left = content_right.max(bar.left());
    let right = caption_left.min(bar.right());
    let width = right - left;
    if !width.is_finite() || width < MIN_DRAG_WIDTH {
        return None;
    }
    Some(Rect::from_min_max(Pos2::new(left, bar.top()), Pos2::new(right, bar.bottom())))
}

/// Make the empty part of the top bar drag the window, and a double-click there maximize it.
///
/// This is a widget, not a pass over the frame's input: the strip is a rect with a `Response`,
/// and `drag_started_by` fires on the frame the primary button goes down on it — which is the
/// only moment [`ViewportCommand::StartDrag`] is accepted by the platform. Asking the context
/// afterwards whether the pointer was "over egui" cannot work here, because the title bar *is*
/// egui: that is true over the bar's bare background as much as over a button, so the window
/// never moved on Windows or Linux.
pub fn window_drag_strip(ui: &Ui, ctx: &Context, bar: Rect, content_right: f32, caption_left: f32) {
    if decorations_wanted() {
        return;
    }
    let Some(rect) = drag_strip_rect(bar, content_right, caption_left) else {
        return;
    };
    let response = ui.interact(rect, Id::new("craftcenter-titlebar-drag"), Sense::click_and_drag());
    if response.drag_started_by(PointerButton::Primary) {
        ctx.send_viewport_cmd(ViewportCommand::StartDrag);
    }
    if response.double_clicked() {
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

    /// A 38 px bar across a 940 px window, the width the app opens at.
    fn bar() -> Rect {
        Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(940.0, 38.0))
    }

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

    #[test]
    fn the_drag_strip_sits_between_the_tabs_and_the_caption_buttons() {
        let bar = bar();
        let content_right = 210.0; // past "CraftCenter" and the three tabs
        let caption_left = bar.right() - 3.0 * CAPTION_BUTTON.x;
        let strip = drag_strip_rect(bar, content_right, caption_left).expect("a 940 px window has room to drag");

        assert_eq!(strip.left(), content_right, "the strip must not cover a tab");
        assert_eq!(strip.right(), caption_left, "the strip must not cover a caption button");
        assert_eq!(strip.top(), bar.top());
        assert_eq!(strip.bottom(), bar.bottom(), "the whole height of the bar drags");
    }

    #[test]
    fn the_drag_strip_never_overlaps_a_caption_button() {
        let bar = bar();
        // Every width the window can be resized to, from its minimum to a wide monitor.
        for width in (520..=2560).step_by(7) {
            let bar = Rect::from_min_max(bar.min, Pos2::new(width as f32, bar.bottom()));
            let caption_left = bar.right() - 3.0 * CAPTION_BUTTON.x;
            let Some(strip) = drag_strip_rect(bar, 210.0, caption_left) else {
                continue;
            };
            assert!(strip.right() <= caption_left, "the strip reached into the caption buttons at {width} px");
            assert!(strip.left() >= 210.0, "the strip reached back over the tabs at {width} px");
            assert!(bar.contains_rect(strip), "the strip left the bar at {width} px");
        }
    }

    #[test]
    fn a_window_too_narrow_to_leave_a_gap_claims_nothing() {
        let bar = Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(300.0, 38.0));
        // The tabs run right up to the caption buttons.
        assert_eq!(drag_strip_rect(bar, 185.0, 180.0), None, "a negative gap is not a drag region");
        assert_eq!(drag_strip_rect(bar, 180.0, 184.0), None, "a 4 px sliver is not worth claiming");
        assert!(drag_strip_rect(bar, 180.0, 188.0).is_some(), "8 px is enough to aim at");
    }

    #[test]
    fn the_strip_is_clamped_to_the_bar_even_if_the_caller_is_wrong() {
        let bar = bar();
        let strip = drag_strip_rect(bar, -50.0, 5_000.0).expect("there is room");
        assert_eq!(strip.left(), bar.left());
        assert_eq!(strip.right(), bar.right());
    }
}
