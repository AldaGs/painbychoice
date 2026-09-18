//! The Assets panel: every file imported into the project, whichever comp uses
//! it. Read-only for now — the library is edited by importing.

use crate::*;
use motion_core::{Asset, AssetKind};

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

pub(crate) fn assets_ui(ui: &mut egui::Ui, project: &MProject, out: &mut TreeEdits) {
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        ui.heading("Assets");
        if icon::button(ui, icon::IMPORT, "Import images, video or audio (Ctrl+I)").clicked() {
            out.import = true;
        }
    });
    if project.assets.is_empty() {
        ui.weak("Nothing imported yet.");
        return;
    }
    ui.separator();
    for a in project.assets.values() {
        ui.horizontal(|ui| {
            kind_icon(ui, a.kind);
            ui.vertical(|ui| {
                ui.label(&a.name).on_hover_text(a.path.display().to_string());
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
            });
        });
    }
}
