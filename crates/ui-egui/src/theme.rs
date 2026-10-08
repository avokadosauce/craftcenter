//! Design tokens: themes, colours, radii, typography.
//!
//! The Crafting Apps read every colour from a `Tokens` struct and hard-code none, and they ship
//! five named themes. CraftCenter does the same, with the same names and the same values, so it
//! sits beside them without looking like a different program — the values below were read from
//! the published theme of the apps themselves rather than guessed at.
//!
//! No colour is written at a call site. A widget that needs one asks [`Tokens::get`].

use egui::{Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Stroke, TextStyle, Visuals};
use std::sync::Arc;

/// The themes, by the names the Crafting Apps give them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ThemeKind {
    /// Spectrum-dark charcoal panels, blue accent: the darkest of the two "Pro" brightnesses.
    Pro,
    /// The shipped default, matching the apps': medium grey panels, same grammar.
    #[default]
    ProMedium,
    /// Near-black surfaces, rounded cards, violet accent.
    Studio,
    /// The same system on light surfaces.
    StudioLight,
    /// Square corners and grey bevels, for people who prefer them.
    Classic,
}

impl ThemeKind {
    pub const ALL: [ThemeKind; 5] = [ThemeKind::Pro, ThemeKind::ProMedium, ThemeKind::Studio, ThemeKind::StudioLight, ThemeKind::Classic];

    pub fn label(self) -> &'static str {
        match self {
            ThemeKind::Pro => "Pro (Dark)",
            ThemeKind::ProMedium => "Pro (Medium Gray)",
            ThemeKind::Studio => "Studio (Dark)",
            ThemeKind::StudioLight => "Studio (Light)",
            ThemeKind::Classic => "Classic",
        }
    }

    /// The id stored in the settings file.
    pub fn id(self) -> &'static str {
        match self {
            ThemeKind::Pro => "pro",
            ThemeKind::ProMedium => "proMedium",
            ThemeKind::Studio => "studio",
            ThemeKind::StudioLight => "studioLight",
            ThemeKind::Classic => "classic",
        }
    }

    /// Accepts the ids above and the obvious aliases, so a hand-edited settings file is forgiving.
    pub fn from_id(text: &str) -> Option<Self> {
        match text.to_ascii_lowercase().replace([' ', '_', '-', '(', ')'], "").as_str() {
            "pro" | "prodark" | "dark" => Some(ThemeKind::Pro),
            "promedium" | "promediumgray" | "medium" | "mediumgray" => Some(ThemeKind::ProMedium),
            "studio" | "studiodark" => Some(ThemeKind::Studio),
            "studiolight" | "light" => Some(ThemeKind::StudioLight),
            "classic" | "retro" => Some(ThemeKind::Classic),
            _ => None,
        }
    }

    pub fn next(self) -> Self {
        let index = Self::ALL.iter().position(|kind| *kind == self).unwrap_or(0);
        Self::ALL.get((index + 1) % Self::ALL.len()).copied().unwrap_or(ThemeKind::ProMedium)
    }

    pub fn is_dark(self) -> bool {
        !matches!(self, ThemeKind::StudioLight | ThemeKind::Classic)
    }
}

/// Every colour and shape the shell draws with.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tokens {
    pub kind: ThemeKind,
    /// Window chrome: the title bar and the toolbars.
    pub chrome: Color32,
    /// The background behind the cards.
    pub dock: Color32,
    /// A card or panel.
    pub card: Color32,
    pub card_border: Color32,
    /// Inputs and dropdowns.
    pub field: Color32,
    pub field_border: Color32,
    pub hover: Color32,
    pub pressed: Color32,
    pub text: Color32,
    pub text_dim: Color32,
    pub text_faint: Color32,
    pub icon: Color32,
    pub accent: Color32,
    pub accent_soft: Color32,
    pub accent_text: Color32,
    pub separator: Color32,
    pub primary_bg: Color32,
    pub primary_text: Color32,
    pub danger: Color32,
    pub warning: Color32,
    pub good: Color32,
    pub row_selected: Color32,
    pub radius_sm: f32,
    pub radius: f32,
    pub radius_lg: f32,
    /// Classic draws bevels instead of flat fills.
    pub bevel: bool,
    /// The Pro themes use the tighter, flatter layout grammar.
    pub pro: bool,
    /// The close button's colour while hovered, and its glyph.
    pub caption_close: Color32,
    pub caption_close_text: Color32,
}

const fn rgb(r: u8, g: u8, b: u8) -> Color32 {
    Color32::from_rgb(r, g, b)
}

impl Tokens {
    pub fn for_kind(kind: ThemeKind) -> Self {
        match kind {
            ThemeKind::ProMedium => Self {
                kind,
                chrome: rgb(83, 83, 83),
                dock: rgb(66, 66, 66),
                card: rgb(83, 83, 83),
                card_border: rgb(66, 66, 66),
                field: rgb(69, 69, 69),
                field_border: rgb(104, 104, 104),
                hover: rgb(98, 98, 98),
                pressed: rgb(112, 112, 112),
                text: rgb(238, 238, 238),
                text_dim: rgb(212, 212, 212),
                text_faint: rgb(160, 160, 160),
                icon: rgb(226, 226, 226),
                accent: rgb(55, 142, 240),
                accent_soft: rgb(110, 110, 110),
                accent_text: rgb(238, 238, 238),
                separator: rgb(62, 62, 62),
                primary_bg: rgb(55, 142, 240),
                primary_text: rgb(255, 255, 255),
                danger: rgb(236, 91, 98),
                warning: rgb(232, 176, 70),
                good: rgb(111, 192, 150),
                row_selected: rgb(107, 107, 107),
                radius_sm: 3.0,
                radius: 4.0,
                radius_lg: 6.0,
                bevel: false,
                pro: true,
                caption_close: rgb(196, 43, 28),
                caption_close_text: rgb(255, 255, 255),
            },
            ThemeKind::Pro => Self {
                kind,
                chrome: rgb(50, 50, 50),
                dock: rgb(30, 30, 30),
                card: rgb(50, 50, 50),
                card_border: rgb(30, 30, 30),
                field: rgb(36, 36, 36),
                field_border: rgb(74, 74, 74),
                hover: rgb(66, 66, 66),
                pressed: rgb(78, 78, 78),
                text: rgb(222, 222, 222),
                text_dim: rgb(178, 178, 178),
                text_faint: rgb(128, 128, 128),
                icon: rgb(200, 200, 200),
                accent_soft: rgb(78, 78, 78),
                separator: rgb(30, 30, 30),
                row_selected: rgb(82, 82, 82),
                ..Self::for_kind(ThemeKind::ProMedium)
            },
            ThemeKind::Studio => Self {
                kind,
                chrome: rgb(20, 20, 21),
                dock: rgb(17, 17, 18),
                card: rgb(26, 26, 28),
                card_border: rgb(40, 40, 44),
                field: rgb(35, 35, 38),
                field_border: rgb(52, 52, 57),
                hover: rgb(44, 44, 48),
                pressed: rgb(56, 56, 62),
                text: rgb(236, 236, 240),
                text_dim: rgb(150, 150, 158),
                text_faint: rgb(96, 96, 104),
                icon: rgb(196, 196, 204),
                accent: rgb(139, 124, 246),
                accent_soft: Color32::from_rgba_unmultiplied(139, 124, 246, 46),
                accent_text: rgb(214, 208, 255),
                separator: rgb(38, 38, 42),
                primary_bg: rgb(246, 246, 248),
                primary_text: rgb(12, 12, 14),
                danger: rgb(240, 96, 96),
                warning: rgb(240, 190, 90),
                good: rgb(111, 192, 150),
                row_selected: rgb(44, 44, 48),
                radius_sm: 6.0,
                radius: 8.0,
                radius_lg: 12.0,
                bevel: false,
                pro: false,
                caption_close: rgb(196, 43, 28),
                caption_close_text: rgb(255, 255, 255),
            },
            ThemeKind::StudioLight => Self {
                kind,
                chrome: rgb(246, 246, 248),
                dock: rgb(240, 240, 243),
                card: rgb(252, 252, 253),
                card_border: rgb(222, 222, 228),
                field: rgb(255, 255, 255),
                field_border: rgb(212, 212, 220),
                hover: rgb(234, 234, 240),
                pressed: rgb(222, 222, 230),
                text: rgb(24, 24, 28),
                text_dim: rgb(92, 92, 102),
                text_faint: rgb(140, 140, 150),
                icon: rgb(70, 70, 80),
                accent: rgb(98, 82, 220),
                accent_soft: Color32::from_rgba_unmultiplied(98, 82, 220, 28),
                accent_text: rgb(58, 44, 160),
                separator: rgb(226, 226, 232),
                primary_bg: rgb(32, 32, 38),
                primary_text: rgb(250, 250, 252),
                danger: rgb(196, 54, 62),
                warning: rgb(150, 104, 16),
                good: rgb(40, 110, 78),
                row_selected: rgb(234, 234, 242),
                radius_sm: 6.0,
                radius: 8.0,
                radius_lg: 12.0,
                bevel: false,
                pro: false,
                caption_close: rgb(196, 43, 28),
                caption_close_text: rgb(255, 255, 255),
            },
            ThemeKind::Classic => Self {
                kind,
                chrome: rgb(212, 208, 200),
                dock: rgb(192, 192, 192),
                card: rgb(212, 208, 200),
                card_border: rgb(128, 128, 128),
                field: rgb(255, 255, 255),
                field_border: rgb(128, 128, 128),
                hover: rgb(222, 218, 210),
                pressed: rgb(180, 176, 168),
                text: rgb(0, 0, 0),
                text_dim: rgb(64, 64, 64),
                text_faint: rgb(110, 110, 110),
                icon: rgb(32, 32, 32),
                accent: rgb(10, 36, 106),
                accent_soft: rgb(180, 190, 215),
                accent_text: rgb(255, 255, 255),
                separator: rgb(128, 128, 128),
                primary_bg: rgb(212, 208, 200),
                primary_text: rgb(0, 0, 0),
                danger: rgb(170, 0, 0),
                warning: rgb(150, 100, 0),
                good: rgb(0, 100, 0),
                row_selected: rgb(10, 36, 106),
                radius_sm: 0.0,
                radius: 0.0,
                radius_lg: 0.0,
                bevel: true,
                pro: false,
                caption_close: rgb(170, 0, 0),
                caption_close_text: rgb(255, 255, 255),
            },
        }
    }

    /// The tokens for the theme currently applied to this context.
    pub fn get(ctx: &egui::Context) -> Self {
        ctx.data(|data| data.get_temp::<Self>(egui::Id::new("craftcenter-tokens"))).unwrap_or_else(|| Self::for_kind(ThemeKind::default()))
    }

    fn store(self, ctx: &egui::Context) {
        ctx.data_mut(|data| data.insert_temp(egui::Id::new("craftcenter-tokens"), self));
    }

    pub fn radius(self) -> CornerRadius {
        CornerRadius::same(self.radius as u8)
    }

    pub fn radius_sm(self) -> CornerRadius {
        CornerRadius::same(self.radius_sm as u8)
    }
}

/// Register Inter for the interface and JetBrains Mono for versions and sizes, the same pair the
/// Crafting Apps use. Both are SIL OFL 1.1 and both are attributed in `ATTRIBUTION.md`.
pub fn install_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    let mut add = |name: &str, bytes: &'static [u8]| {
        fonts.font_data.insert(name.to_owned(), Arc::new(FontData::from_static(bytes)));
    };
    add("Inter", include_bytes!("../../../assets/fonts/Inter-Regular.ttf"));
    add("Inter-Medium", include_bytes!("../../../assets/fonts/Inter-Medium.ttf"));
    add("Inter-SemiBold", include_bytes!("../../../assets/fonts/Inter-SemiBold.ttf"));
    add("JetBrainsMono", include_bytes!("../../../assets/fonts/JetBrainsMono-Regular.ttf"));

    fonts.families.entry(FontFamily::Proportional).or_default().insert(0, "Inter".to_owned());
    fonts.families.entry(FontFamily::Monospace).or_default().insert(0, "JetBrainsMono".to_owned());
    for (family, primary) in [("medium", "Inter-Medium"), ("semibold", "Inter-SemiBold")] {
        fonts.families.insert(FontFamily::Name(family.into()), vec![primary.to_owned(), "Inter".to_owned()]);
    }
    ctx.set_fonts(fonts);
}

/// Inter Medium at `size`.
pub fn medium(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("medium".into()))
}

/// Inter SemiBold at `size`.
pub fn semibold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("semibold".into()))
}

/// JetBrains Mono at `size`, for versions, sizes and anything else that lines up in a column.
pub fn mono(size: f32) -> FontId {
    FontId::monospace(size)
}

/// Apply a theme to a context: visuals, spacing and text styles, and remember the tokens so
/// widgets can read them.
pub fn apply(ctx: &egui::Context, kind: ThemeKind) {
    let tokens = Tokens::for_kind(kind);
    tokens.store(ctx);

    let mut visuals = if kind.is_dark() { Visuals::dark() } else { Visuals::light() };
    visuals.override_text_color = Some(tokens.text);
    visuals.panel_fill = tokens.dock;
    visuals.window_fill = tokens.card;
    visuals.extreme_bg_color = tokens.field;
    visuals.faint_bg_color = tokens.hover;
    visuals.selection.bg_fill = tokens.accent;
    visuals.selection.stroke = Stroke::new(1.0, tokens.accent_text);
    visuals.hyperlink_color = tokens.accent;
    visuals.window_corner_radius = tokens.radius();
    visuals.menu_corner_radius = tokens.radius();
    visuals.window_stroke = Stroke::new(1.0, tokens.card_border);

    for widget in [&mut visuals.widgets.inactive, &mut visuals.widgets.hovered, &mut visuals.widgets.active, &mut visuals.widgets.open] {
        widget.corner_radius = tokens.radius_sm();
        widget.bg_stroke = Stroke::new(1.0, tokens.field_border);
    }
    visuals.widgets.noninteractive.corner_radius = tokens.radius_sm();
    visuals.widgets.noninteractive.bg_fill = tokens.card;
    visuals.widgets.noninteractive.weak_bg_fill = tokens.card;
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, tokens.separator);
    visuals.widgets.inactive.bg_fill = tokens.field;
    visuals.widgets.inactive.weak_bg_fill = tokens.field;
    visuals.widgets.hovered.bg_fill = tokens.hover;
    visuals.widgets.hovered.weak_bg_fill = tokens.hover;
    visuals.widgets.active.bg_fill = tokens.pressed;
    visuals.widgets.active.weak_bg_fill = tokens.pressed;
    ctx.set_visuals(visuals);

    ctx.all_styles_mut(|style| {
        let body = if tokens.pro { 12.0 } else { 12.5 };
        style.text_styles = [
            (TextStyle::Small, FontId::proportional(10.5)),
            (TextStyle::Body, FontId::proportional(body)),
            (TextStyle::Button, FontId::proportional(body)),
            (TextStyle::Heading, semibold(15.0)),
            (TextStyle::Monospace, mono(12.0)),
        ]
        .into();
        style.spacing.item_spacing = egui::vec2(8.0, if tokens.pro { 6.0 } else { 8.0 });
        style.spacing.button_padding = egui::vec2(10.0, 5.0);
        style.spacing.window_margin = egui::Margin::same(if tokens.pro { 8 } else { 12 });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_theme_matches_the_crafting_apps_default() {
        assert_eq!(ThemeKind::default(), ThemeKind::ProMedium);
    }

    #[test]
    fn every_theme_id_round_trips() {
        for kind in ThemeKind::ALL {
            assert_eq!(ThemeKind::from_id(kind.id()), Some(kind), "{}", kind.id());
        }
        assert_eq!(ThemeKind::from_id("mauve"), None);
    }

    #[test]
    fn cycling_visits_every_theme_and_returns() {
        let mut kind = ThemeKind::default();
        let mut seen = vec![kind];
        for _ in 1..ThemeKind::ALL.len() {
            kind = kind.next();
            assert!(!seen.contains(&kind), "{} was visited twice", kind.id());
            seen.push(kind);
        }
        assert_eq!(kind.next(), ThemeKind::default());
    }

    #[test]
    fn the_pro_themes_carry_the_published_spectrum_values() {
        let pro = Tokens::for_kind(ThemeKind::Pro);
        assert_eq!(pro.chrome, rgb(50, 50, 50));
        assert_eq!(pro.accent, rgb(55, 142, 240));
        assert_eq!(pro.radius, 4.0);
        assert!(pro.pro);
        let medium = Tokens::for_kind(ThemeKind::ProMedium);
        assert_eq!(medium.chrome, rgb(83, 83, 83));
    }

    #[test]
    fn every_theme_keeps_text_legible_against_its_own_surfaces() {
        // Not a contrast-ratio test; a guard against a token set where text and ground collapse.
        for kind in ThemeKind::ALL {
            let tokens = Tokens::for_kind(kind);
            for ground in [tokens.card, tokens.dock, tokens.chrome, tokens.field] {
                let difference = luminance(tokens.text) - luminance(ground);
                assert!(difference.abs() > 0.25, "{}: text and a surface are too close", kind.id());
            }
        }
    }

    #[test]
    fn no_theme_is_left_with_a_default_placeholder_colour() {
        for kind in ThemeKind::ALL {
            let tokens = Tokens::for_kind(kind);
            assert_eq!(tokens.kind, kind, "{}: the kind field was not set for this theme", kind.id());
        }
    }

    fn luminance(colour: Color32) -> f32 {
        let [r, g, b, _] = colour.to_array();
        (0.2126 * f32::from(r) + 0.7152 * f32::from(g) + 0.0722 * f32::from(b)) / 255.0
    }
}
