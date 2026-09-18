//! The Assets panel: every file imported into the project, whichever comp uses
//! it. Read-only for now — the library is edited by importing.

use crate::*;
use motion_core::{Asset, AssetKind};

/// What an Assets-panel row carries when dragged: a file from the library or
/// a composition, both of which become a layer when dropped on Layers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum AssetDrag {
    File(motion_core::AssetId),
    Comp(CompId),
}

/// A comp's info line, in the same shape as a file's.
pub(crate) fn comp_info(c: &Comp) -> String {
    let fps = format!("{:.3}", c.fps);
    let fps = fps.trim_end_matches('0').trim_end_matches('.');
    let secs = if c.fps > 0.0 { c.duration_frames as f64 / c.fps } else { 0.0 };
    format!("{:.0}×{:.0} · {fps} fps · {secs:.1} s", c.width, c.height)
}

/// The one-line summary under an asset's name. Pure, so it is tested rather
/// than only checked by eye.
pub(crate) fn asset_info(a: &Asset) -> String {
    let secs = |s: f64| format!("{s:.1} s");
    let sound = |a: &Asset| {
        let ch = match a.channels {
            1 => "mono".to_string(),
            2 => "stereo".to_string(),
            n => format!("{n} ch"),
        };
        format!("{:.1} kHz {ch}", a.sample_rate as f64 / 1000.0)
    };
    match a.kind {
        AssetKind::Image => format!("{:.0}×{:.0} · still", a.width, a.height),
        AssetKind::Video => {
            let mut s = format!("{:.0}×{:.0} · {:.3} fps", a.width, a.height, a.fps);
            s = s.replace(".000 fps", " fps");
            if a.fps > 0.0 {
                s += &format!(" · {}", secs(a.frames as f64 / a.fps));
            }
            if a.sample_rate > 0 {
                s += &format!(" · {}", sound(a));
            }
            s
        }
        AssetKind::Audio if a.sample_rate > 0 => {
            format!("{} · {}", sound(a), secs(a.samples as f64 / a.sample_rate as f64))
        }
        AssetKind::Audio => "audio".to_string(),
    }
}

/// A small painted type icon. Painted rather than a glyph: the icon font is a
/// fixed subset and has no photo or film glyph. Audio borrows the sine glyph.
fn kind_icon(ui: &mut egui::Ui, kind: AssetKind) {
    if kind == AssetKind::Audio {
        ui.label(icon::text(icon::WAVE));
        return;
    }
    let (rect, _) = ui.allocate_exact_size(egui::vec2(16.0, 16.0), egui::Sense::hover());
    let p = ui.painter();
    let c = ui.visuals().text_color();
    let frame = rect.shrink2(egui::vec2(1.0, 3.0));
    p.rect_stroke(frame, 1.5, egui::Stroke::new(1.3, c), egui::StrokeKind::Inside);
    match kind {
        // A mountain: a photo.
        AssetKind::Image => {
            let (l, b) = (frame.left() + 2.0, frame.bottom() - 2.0);
            p.add(egui::Shape::convex_polygon(
                vec![egui::pos2(l, b), egui::pos2(l + 4.5, b - 5.0), egui::pos2(l + 9.0, b)],
                c,
                egui::Stroke::NONE,
            ));
        }
        // A play triangle: a clip.
        _ => {
            let m = frame.center();
            p.add(egui::Shape::convex_polygon(
                vec![m + egui::vec2(-2.5, -3.5), m + egui::vec2(3.5, 0.0), m + egui::vec2(-2.5, 3.5)],
                c,
                egui::Stroke::NONE,
            ));
        }
    }
}

/// One draggable row: icon, name, info line.
/// Returns whether the row was double-clicked.
fn row(ui: &mut egui::Ui, payload: AssetDrag, icon: impl FnOnce(&mut egui::Ui), name: &str, info: impl FnOnce(&mut egui::Ui), hover: String) -> bool {
    let id = egui::Id::new(("asset_row", payload));
    let resp = ui.dnd_drag_source(id, payload, |ui| {
        ui.horizontal(|ui| {
            icon(ui);
            ui.vertical(|ui| {
                ui.label(name);
                info(ui);
            });
        });
    })
    .response
    .on_hover_text(hover);
    // The drag source senses drags only, so the double-click is read off the
    // pointer rather than the response.
    resp.hovered() && ui.input(|i| i.pointer.button_double_clicked(egui::PointerButton::Primary))
}

pub(crate) fn assets_ui(ui: &mut egui::Ui, project: &MProject, current: CompId, out: &mut TreeEdits) {
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        ui.heading("Assets");
        if icon::button(ui, icon::IMPORT, "Import images, video or audio into the project").clicked() {
            out.import_to_library = true;
        }
    });
    ui.weak("Drag onto Layers to add · double-click a comp to open it");
    ui.separator();
    // Compositions first: they are what the project is made of.
    for (id, c) in &project.comps {
        let open = *id == current;
        let dbl = row(
            ui,
            AssetDrag::Comp(*id),
            |ui| {
                ui.label(icon::text(icon::PRECOMP));
            },
            &c.label(*id),
            |ui| {
                ui.weak(comp_info(c));
            },
            if open { "The open composition".into() } else { "Composition".into() },
        );
        if dbl && !open {
            out.open_comp = Some(*id);
        }
    }
    for a in project.assets.values() {
        let _ = row(
            ui,
            AssetDrag::File(a.id),
            |ui| kind_icon(ui, a.kind),
            &a.name,
            |ui| {
                // ponytail: a stat per asset per frame; cache if a library
                // ever holds hundreds of files.
                if a.path.exists() {
                    ui.weak(asset_info(a));
                } else {
                    ui.colored_label(
                        egui::Color32::from_rgb(220, 160, 60),
                        format!("{} missing", icon::WARNING),
                    );
                }
            },
            a.path.display().to_string(),
        );
    }
}
