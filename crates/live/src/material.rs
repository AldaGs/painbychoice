//! Material icons for the edit tools: Google's Material Symbols (Outlined,
//! weight 400) for the ones it has, and Pictogrammers' Material Design Icons
//! for the two it lacks (roll, magnet). Both Apache-2.0; see
//! `assets/NOTICE.md` for how the subsets are made.
//!
//! A family of their own, apart from Tabler's ([`crate::icon`]): both fonts put
//! glyphs in the Private Use Area, so one family would let a codepoint resolve
//! to the wrong set's picture. The two Material files don't collide (Symbols is
//! in the BMP, MDI in plane 15), so they share this one.

pub(crate) const FAMILY: &str = "material";

/// Symbols `arrow_selector_tool`.
pub(crate) const SELECT: &str = "\u{f82f}";
/// Symbols `content_cut`.
pub(crate) const RAZOR: &str = "\u{e14e}";
/// Symbols `keyboard_tab`: an arrow pushing into an edge.
pub(crate) const RIPPLE: &str = "\u{e31c}";
/// MDI `arrow-split-vertical`: a cut, arrows both ways.
pub(crate) const ROLL: &str = "\u{f093c}";
/// MDI `magnet`.
pub(crate) const MAGNET: &str = "\u{f0347}";

/// Add the family to `fonts` (called from [`crate::icon::install`]).
pub(crate) fn add(fonts: &mut egui::FontDefinitions) {
    for (name, bytes) in [
        ("material-symbols", &include_bytes!("../assets/material-symbols.ttf")[..]),
        ("mdi", &include_bytes!("../assets/mdi.ttf")[..]),
    ] {
        fonts.font_data.insert(name.to_owned(), std::sync::Arc::new(egui::FontData::from_static(bytes)));
    }
    fonts
        .families
        .insert(egui::FontFamily::Name(FAMILY.into()), vec!["material-symbols".to_owned(), "mdi".to_owned()]);
}

/// An icon-only toggle: the glyph, highlighted when `on`. The name goes in the
/// tooltip — the caller adds it with `on_hover_text`.
pub(crate) fn toggle(ui: &mut egui::Ui, on: bool, glyph: &str) -> egui::Response {
    let text = egui::RichText::new(glyph).family(egui::FontFamily::Name(FAMILY.into())).size(18.0);
    ui.add(egui::Button::selectable(on, text).min_size(egui::vec2(28.0, 24.0)))
}

#[cfg(test)]
mod tests {
    /// Every glyph named here is in the bundled subsets: a missing codepoint
    /// renders as tofu, so a forgotten regeneration fails here instead.
    #[test]
    fn every_material_icon_is_in_the_bundled_fonts() {
        use skrifa::{FontRef, MetadataProvider};
        let fonts = [&include_bytes!("../assets/material-symbols.ttf")[..], &include_bytes!("../assets/mdi.ttf")[..]];
        for g in [super::SELECT, super::RAZOR, super::RIPPLE, super::ROLL, super::MAGNET] {
            let c = g.chars().next().unwrap();
            assert!(
                fonts.iter().any(|f| FontRef::new(f).unwrap().charmap().map(c).is_some()),
                "U+{:X} is not in the subset",
                c as u32
            );
        }
    }
}
