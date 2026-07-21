//! The editor's chrome colours, in one place.
//!
//! Three surfaces, deliberately distinct so a glance tells you which one you
//! are looking at:
//!
//! * **UI chrome** — every panel, header and widget background: [`UI_BASE`].
//! * **Preview backdrop** — the letterbox around the comp frame:
//!   [`PREVIEW_BACKDROP`]. Painted by vello as the render's `base_color`, not
//!   by egui, because the canvas hole is a GPU surface with no egui behind it.
//! * **Composition area** — inside the comp bounds. *Not* here: it is a
//!   per-comp user setting (`Comp::bg`, default `#5d677e`), so it is
//!   document data and lives in `core`.

use crate::*;

/// The UI chrome base, `#2d2d2d`. Panels, window fills and widget backgrounds
/// are derived from it so the whole editor reads as one flat surface.
pub(crate) const UI_BASE: egui::Color32 = egui::Color32::from_rgb(0x2d, 0x2d, 0x2d);

/// The preview letterbox, `#23262d` — darker and cooler than the chrome, so
/// the comp frame reads as a lit object sitting in a recessed well.
pub(crate) const PREVIEW_BACKDROP: Color =
    Color::new([0x23 as f32 / 255.0, 0x26 as f32 / 255.0, 0x2d as f32 / 255.0, 1.0]);

/// Shift a chrome colour by `d` steps per channel, clamped. Widget states
/// (hovered / active / open) are offsets from [`UI_BASE`] rather than free
/// constants, so re-tinting the editor is a one-line change here.
fn shade(c: egui::Color32, d: i16) -> egui::Color32 {
    fn ch(v: u8, d: i16) -> u8 {
        (v as i16 + d).clamp(0, 255) as u8
    }
    egui::Color32::from_rgb(ch(c.r(), d), ch(c.g(), d), ch(c.b(), d))
}

/// Apply the chrome palette to an egui context. Called once at startup,
/// alongside the icon font — both must land before the first UI pass.
pub(crate) fn install(ctx: &egui::Context) {
    let mut v = egui::Visuals::dark();
    v.panel_fill = UI_BASE;
    v.window_fill = UI_BASE;
    v.faint_bg_color = shade(UI_BASE, 6);
    // Text edits, combo popups and the graph's box interiors read as wells cut
    // into the chrome, so they go darker rather than lighter.
    v.extreme_bg_color = shade(UI_BASE, -18);
    v.widgets.noninteractive.bg_fill = UI_BASE;
    v.widgets.noninteractive.weak_bg_fill = UI_BASE;
    v.widgets.inactive.bg_fill = shade(UI_BASE, 14);
    v.widgets.inactive.weak_bg_fill = shade(UI_BASE, 14);
    v.widgets.hovered.bg_fill = shade(UI_BASE, 28);
    v.widgets.hovered.weak_bg_fill = shade(UI_BASE, 28);
    v.widgets.active.bg_fill = shade(UI_BASE, 42);
    v.widgets.active.weak_bg_fill = shade(UI_BASE, 42);
    v.widgets.open.bg_fill = shade(UI_BASE, 20);
    v.widgets.open.weak_bg_fill = shade(UI_BASE, 20);
    ctx.set_visuals(v);
}
