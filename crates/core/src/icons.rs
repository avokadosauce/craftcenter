//! The app icons, compiled in.
//!
//! Each icon is the file from that app's own repository, where its
//! `assets/app-icon/LICENSE.txt` licenses the whole `hicolor/` tree under MIT OR Apache-2.0 as the
//! app owner's original work. `ATTRIBUTION.md` carries a row per file. The ArtCraft wordmark and
//! mark, which are trademarks and not open source, appear nowhere in this program.

/// `(catalogue icon field, PNG bytes)`.
pub static ICONS: &[(&str, &[u8])] = &[
    ("photocraft-64.png", include_bytes!("../../../assets/app-icon/photocraft-64.png")),
    ("vectorcraft-64.png", include_bytes!("../../../assets/app-icon/vectorcraft-64.png")),
    ("filmcraft-64.png", include_bytes!("../../../assets/app-icon/filmcraft-64.png")),
    ("lightcraft-64.png", include_bytes!("../../../assets/app-icon/lightcraft-64.png")),
    ("pdfcraft-64.png", include_bytes!("../../../assets/app-icon/pdfcraft-64.png")),
    ("effectcraft-64.png", include_bytes!("../../../assets/app-icon/effectcraft-64.png")),
    ("designcraft-64.png", include_bytes!("../../../assets/app-icon/designcraft-64.png")),
    ("wordcraft-64.png", include_bytes!("../../../assets/app-icon/wordcraft-64.png")),
    ("cadcraft-64.png", include_bytes!("../../../assets/app-icon/cadcraft-64.png")),
    ("gridcraft-64.png", include_bytes!("../../../assets/app-icon/gridcraft-64.png")),
    ("deckcraft-64.png", include_bytes!("../../../assets/app-icon/deckcraft-64.png")),
    ("soundcraft-64.png", include_bytes!("../../../assets/app-icon/soundcraft-64.png")),
];

/// The PNG for a catalogue row's `icon` field.
pub fn icon(name: &str) -> Option<&'static [u8]> {
    ICONS.iter().find(|(n, _)| *n == name).map(|(_, bytes)| *bytes)
}

#[cfg(test)]
mod tests {
    use craftcenter_catalogue::Catalogue;

    use super::*;

    #[test]
    fn every_catalogue_icon_is_compiled_in() {
        let catalogue = Catalogue::embedded().expect("catalogue parses");
        for app in catalogue.installable() {
            let Some(name) = &app.icon else {
                panic!("{} has no icon in the catalogue", app.slug);
            };
            let bytes = icon(name).unwrap_or_else(|| panic!("{name} is referenced but not compiled in"));
            assert!(bytes.starts_with(b"\x89PNG"), "{name} is not a PNG");
        }
    }

    #[test]
    fn no_icon_is_compiled_in_without_a_catalogue_row_referring_to_it() {
        let catalogue = Catalogue::embedded().expect("catalogue parses");
        for (name, _) in ICONS {
            assert!(catalogue.apps().iter().any(|a| a.icon.as_deref() == Some(*name)), "{name} is compiled in but no catalogue row uses it");
        }
    }
}
