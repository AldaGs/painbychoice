//! The Assets panel: every file imported into the project, whichever comp uses
//! it, filed into folders the user makes.

use crate::*;
use motion_core::{Asset, AssetKind};

/// What an Assets-panel row carries when dragged: a file from the library or
/// a composition, both of which become a layer when dropped on Layers, or move
/// between folders when dropped in this panel.
pub(crate) type AssetDrag = motion_core::ProjectItem;

/// A change to the panel's folders.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum FolderEdit {
    New,
    Rename(usize, String),
    /// Remove the folder; its items return to the top level.
    Delete(usize),
    /// File an item into a folder, or out to the top level with `None`.
    File(AssetDrag, Option<usize>),
}

/// Apply a folder edit to the project.
pub(crate) fn apply_folder_edit(project: &mut MProject, edit: FolderEdit) {
    match edit {
        FolderEdit::New => {
            let name = format!("Folder {}", project.folders.len() + 1);
            project.folders.push(motion_core::AssetFolder { name, items: Vec::new() });
        }
        FolderEdit::Rename(i, name) => {
            if let Some(f) = project.folders.get_mut(i) {
                f.name = name;
            }
        }
        FolderEdit::Delete(i) => {
            if i < project.folders.len() {
                project.folders.remove(i);
            }
        }
        FolderEdit::File(item, to) => project.file_into(item, to),
    }
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
    // The whole panel takes drops too: an item released outside every folder
    // goes back to the top level. Folder headers claim theirs first.
    let (_, dropped) = ui.dnd_drop_zone::<AssetDrag, _>(egui::Frame::NONE, |ui| {
        ui.set_min_width(ui.available_width());
        panel_ui(ui, project, current, out);
        ui.allocate_space(egui::vec2(ui.available_width(), ui.available_height().max(40.0)));
    });
    if let Some(item) = dropped {
        if project.folders.iter().any(|f| f.items.contains(&item)) {
            out.folder = Some(FolderEdit::File(*item, None));
        }
    }
}

fn panel_ui(ui: &mut egui::Ui, project: &MProject, current: CompId, out: &mut TreeEdits) {
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        ui.heading("Assets");
        if icon::button(ui, icon::IMPORT, "Import images, video or audio into the project").clicked() {
            out.import_to_library = true;
        }
        if icon::button(ui, icon::ADD, "New folder").clicked() {
            out.folder = Some(FolderEdit::New);
        }
    });
    ui.weak("Drag onto Layers to add, onto a folder to file · double-click a comp to open it");
    ui.separator();

    let rename_id = egui::Id::new("folder_rename");
    let mut renaming: Option<(usize, String)> = ui.data(|d| d.get_temp(rename_id));
    // Folded folders, by index. View state for the session, not saved.
    let fold_id = egui::Id::new("folder_folds");
    let mut folded: std::collections::HashSet<usize> = ui.data(|d| d.get_temp(fold_id)).unwrap_or_default();
    for (fi, folder) in project.folders.iter().enumerate() {
        // The header is one click target, allocated before its contents so
        // the rename box on top keeps its own clicks (as in the Layers panel).
        let (rect, r) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 22.0), egui::Sense::click());
        let open = !folded.contains(&fi);
        let mut row_ui = ui.new_child(
            egui::UiBuilder::new().max_rect(rect).layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        {
            let ui = &mut row_ui;
            ui.label(if open { "⏷" } else { "⏵" });
            ui.label(icon::text(icon::GROUP));
            match renaming.as_mut().filter(|(i, _)| *i == fi) {
                Some((_, buf)) => {
                    let t = ui.add(egui::TextEdit::singleline(buf).desired_width(120.0));
                    t.request_focus();
                    if t.lost_focus() {
                        if !ui.input(|i| i.key_pressed(egui::Key::Escape)) && !buf.trim().is_empty() {
                            out.folder = Some(FolderEdit::Rename(fi, buf.trim().to_string()));
                        }
                        renaming = None;
                    }
                }
                None => {
                    ui.label(&folder.name);
                }
            }
        }
        if r.clicked() && !folded.remove(&fi) {
            folded.insert(fi);
        }
        if r.double_clicked() {
            renaming = Some((fi, folder.name.clone()));
        }
        r.context_menu(|ui| {
            if ui.button("Rename").clicked() {
                renaming = Some((fi, folder.name.clone()));
                ui.close();
            }
            if ui.button("Delete folder").on_hover_text("Its contents move back to the top level").clicked() {
                out.folder = Some(FolderEdit::Delete(fi));
                ui.close();
            }
        });
        if r.dnd_hover_payload::<AssetDrag>().is_some() {
            let st = egui::Stroke::new(2.0, ui.visuals().selection.stroke.color);
            ui.painter().rect_stroke(rect, 2.0, st, egui::StrokeKind::Inside);
        }
        if let Some(item) = r.dnd_release_payload::<AssetDrag>() {
            out.folder = Some(FolderEdit::File(*item, Some(fi)));
        }
        if open {
            ui.indent(("folder", fi), |ui| {
                for item in &folder.items {
                    item_row(ui, project, current, *item, out);
                }
            });
        }
    }
    ui.data_mut(|d| d.insert_temp(fold_id, folded));
    ui.data_mut(|d| match renaming {
        Some(v) => {
            d.insert_temp(rename_id, v);
        }
        None => d.remove::<(usize, String)>(rename_id),
    });

    // Then everything in no folder: compositions first, as they are what the
    // project is made of.
    let filed: std::collections::HashSet<AssetDrag> =
        project.folders.iter().flat_map(|f| f.items.iter().copied()).collect();
    let loose = project
        .comps
        .keys()
        .map(|id| AssetDrag::Comp(*id))
        .chain(project.assets.keys().map(|id| AssetDrag::File(*id)))
        .filter(|i| !filed.contains(i));
    for item in loose {
        item_row(ui, project, current, item, out);
    }
}

/// One file or comp row. An id whose asset or comp is gone draws nothing.
fn item_row(ui: &mut egui::Ui, project: &MProject, current: CompId, item: AssetDrag, out: &mut TreeEdits) {
    match item {
        AssetDrag::Comp(id) => {
            let Some(c) = project.comps.get(&id) else { return };
            let open = id == current;
            let dbl = row(
                ui,
                item,
                |ui| {
                    ui.label(icon::text(icon::PRECOMP));
                },
                &c.label(id),
                |ui| {
                    ui.weak(comp_info(c));
                },
                if open { "The open composition".into() } else { "Composition".into() },
            );
            if dbl && !open {
                out.open_comp = Some(id);
            }
        }
        AssetDrag::File(id) => {
            let Some(a) = project.assets.get(&id) else { return };
            let _ = row(
                ui,
                item,
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
}
