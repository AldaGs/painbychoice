//! Tabler icons, as a font.
//!
//! egui's default font has no icon glyphs — that's why the UI reached for
//! `▲`/`•`/`✕` and painted its own indicators, and why anything more expressive
//! (`◆◇●○`) rendered as tofu. Registering one icon font fixes that whole class
//! of problem: an icon is just a character in the `icons` family.
//!
//! **Every glyph is named here.** A raw `"\u{ea62}"` at a call site is
//! unsearchable and unreviewable — you cannot tell a chevron from a trash can by
//! reading the diff. Adding an icon means adding a const, which is also what
//! keeps the subset honest (see below).
//!
//! The bundled font is **subsetted to exactly these codepoints**: 8 KB instead
//! of the full 2.4 MB, 5,937-glyph webfont. To add an icon, add its const here
//! and re-run the subset with its codepoint included — a name that isn't in the
//! subset renders as tofu, so a missing regeneration is visible immediately
//! rather than silently shipping a blank button.
//!
//! Source: `@tabler/icons-webfont` 3.31.0, MIT.

/// The font family icons are drawn with. Registered by [`install`].
pub(crate) const FAMILY: &str = "icons";

// Transport.
pub(crate) const PLAY: &str = "\u{ed46}";
pub(crate) const PAUSE: &str = "\u{ed45}";
pub(crate) const RESTART: &str = "\u{ed48}";

// Project / layer management.
pub(crate) const SAVE: &str = "\u{eb62}";
pub(crate) const LOAD: &str = "\u{faf7}";
pub(crate) const RECT: &str = "\u{eb2c}";
pub(crate) const ELLIPSE: &str = "\u{ea6b}";
pub(crate) const GROUP: &str = "\u{eaad}";
pub(crate) const ADD: &str = "\u{eb0b}";
pub(crate) const DELETE: &str = "\u{eb41}";
pub(crate) const UP: &str = "\u{ea62}";
pub(crate) const DOWN: &str = "\u{ea5f}";
pub(crate) const CLOSE: &str = "\u{eb55}";

// Comps and the layer time model.
pub(crate) const PRECOMP: &str = "\u{efa5}";
pub(crate) const OPEN: &str = "\u{ea99}";
pub(crate) const PRECOMPOSE: &str = "\u{eef7}";
pub(crate) const TRIM: &str = "\u{eb1b}";

// The property graph.
pub(crate) const MODULE: &str = "\u{eb10}";
pub(crate) const LINK: &str = "\u{eade}";
pub(crate) const KEYFRAME: &str = "\u{f576}";
pub(crate) const EXPR: &str = "\u{eeb2}";
pub(crate) const BAKE: &str = "\u{ec0b}";

// Layout and status.
pub(crate) const SPLIT_V: &str = "\u{ead4}";
pub(crate) const SPLIT_H: &str = "\u{ead8}";
pub(crate) const WARNING: &str = "\u{ea06}";

/// Register the icon font with egui.
///
/// Icons go in their own family rather than being appended to the proportional
/// one: a fallback would let *any* missing character silently resolve to an
/// icon glyph, which turns a text bug into a baffling picture. Asking for the
/// family explicitly keeps icons deliberate.
pub(crate) fn install(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        FAMILY.to_owned(),
        std::sync::Arc::new(egui::FontData::from_static(include_bytes!(
            "../assets/tabler-icons.ttf"
        ))),
    );
    fonts
        .families
        .insert(egui::FontFamily::Name(FAMILY.into()), vec![FAMILY.to_owned()]);
    ctx.set_fonts(fonts);
}

/// An icon as rich text, sized to sit on a line of UI text.
pub(crate) fn text(glyph: &str) -> egui::RichText {
    egui::RichText::new(glyph)
        .family(egui::FontFamily::Name(FAMILY.into()))
        .size(14.0)
}

/// An icon-only button, square-ish and hoverable. `tip` is required — an icon
/// without a tooltip is a guessing game, and these replace buttons that used to
/// say what they did in words.
pub(crate) fn button(ui: &mut egui::Ui, glyph: &str, tip: &str) -> egui::Response {
    ui.add(egui::Button::new(text(glyph)).min_size(egui::vec2(22.0, 0.0)))
        .on_hover_text(tip)
}

/// An icon followed by a label, for buttons that keep their words.
pub(crate) fn labeled(ui: &mut egui::Ui, glyph: &str, label: &str, tip: &str) -> egui::Response {
    ui.horizontal(|ui| {
        let resp = ui.add(egui::Button::new(text(glyph)).min_size(egui::vec2(22.0, 0.0)));
        ui.small(label);
        resp
    })
    .inner
    .on_hover_text(tip)
}
