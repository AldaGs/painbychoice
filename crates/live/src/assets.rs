//! The Assets panel: every file imported into the project, whichever comp uses
//! it, filed into folders the user makes.

use crate::*;
use motion_core::{Asset, AssetKind};

/// What an Assets-panel row carries when dragged: a file from the library or
/// a composition, both of which become a layer when dropped on Layers, or move
/// between folders when dropped in this panel.
pub(crate) type AssetDrag = motion_core::ProjectItem;

/// The panel's selected rows, kept in egui memory: view state, not document.
fn sel_id() -> egui::Id {
    egui::Id::new("asset_sel")
}

fn selection(ctx: &egui::Context) -> Vec<AssetDrag> {
    ctx.data(|d| d.get_temp(sel_id())).unwrap_or_default()
}

/// `item` and, when it is selected, everything else that is: what a drag,
/// a double-click or a menu acts on.
pub(crate) fn with_selection(ctx: &egui::Context, item: AssetDrag) -> Vec<AssetDrag> {
    let sel = selection(ctx);
    if sel.contains(&item) { sel } else { vec![item] }
}

/// A change to the panel's folders.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum FolderEdit {
    New,
    Rename(usize, String),
    /// Remove the folder; its items return to the top level.
    Delete(usize),
    /// File an item into a folder, or out to the top level with `None`.
    File(AssetDrag, Option<usize>),
    /// Set or clear a folder's colour label.
    Label(usize, Option<u8>),
}

/// Apply a folder edit to the project.
pub(crate) fn apply_folder_edit(project: &mut MProject, edit: FolderEdit) {
    match edit {
        FolderEdit::New => {
            let name = format!("Folder {}", project.folders.len() + 1);
            project.folders.push(motion_core::AssetFolder { name, items: Vec::new() , label: None });
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
        FolderEdit::Label(i, label) => {
            if let Some(f) = project.folders.get_mut(i) {
                f.label = label;
            }
        }
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

/// One draggable row: icon, name, info line. Handles its own selection
/// clicks and context menu; returns whether it was double-clicked.
fn row(ui: &mut egui::Ui, payload: AssetDrag, icon: impl FnOnce(&mut egui::Ui), name: &str, info: impl FnOnce(&mut egui::Ui), hover: String, out: &mut TreeEdits) -> bool {
    let id = egui::Id::new(("asset_row", payload));
    let sel = selection(ui.ctx());
    let selected = sel.contains(&payload);
    let n = ui.data(|d| d.get_temp::<usize>(egui::Id::new("asset_zebra"))).unwrap_or(0);
    ui.data_mut(|d| d.insert_temp(egui::Id::new("asset_zebra"), n + 1));
    let resp = ui.dnd_drag_source(id, payload, |ui| {
        let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 22.0), egui::Sense::hover());
        let hovered = ui.rect_contains_pointer(rect);
        ui.painter().rect_filled(rect, 0.0, crate::layers::row_fill(ui, n, selected, hovered));
        let mut row = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink2(egui::vec2(4.0, 0.0))).layout(egui::Layout::left_to_right(egui::Align::Center)));
        row.style_mut().interaction.selectable_labels = false;
        icon(&mut row);
        row.label(name);
        row.with_layout(egui::Layout::right_to_left(egui::Align::Center), info);
    })
    .response
    .on_hover_text(hover);
    ui.data_mut(|d| d.get_temp_mut_or_default::<Vec<(AssetDrag, egui::Rect)>>(egui::Id::new("asset_rects")).push((payload, resp.rect)));
    // The drag source senses drags only, so clicks go to a click sensor laid
    // over it: a press that moves becomes the drag, one that doesn't a click.
    let menu = ui.interact(resp.rect, id.with("menu"), egui::Sense::click());
    if menu.clicked() {
        let next = crate::layers::combine(&sel, &[payload], PickMode::click(crate::layers::click_mods(ui)));
        ui.data_mut(|d| d.insert_temp(sel_id(), next));
    }
    // A right-click on an unselected row selects it, so the menu's "these"
    // is what is highlighted.
    if menu.secondary_clicked() && !selected {
        ui.data_mut(|d| d.insert_temp(sel_id(), vec![payload]));
    }
    let dbl = menu.double_clicked();
    menu.context_menu(|ui| {
        let these = with_selection(ui.ctx(), payload);
        let n = these.len();
        let label = |s: &str| if n > 1 { format!("{s} ({n})") } else { s.to_string() };
        if ui.button(label("Add to composition")).clicked() {
            out.place = these.clone();
            ui.close();
        }
        if ui.button(label("Remove from project")).on_hover_text("Layers using a file keep their place and show it missing").clicked() {
            out.remove_assets = these;
            ui.close();
        }
    });
    dbl
}

/// `proxy`: whether the preview reads proxies, how many are being made, and
/// the last proxy error.
pub(crate) fn assets_ui(ui: &mut egui::Ui, project: &MProject, current: CompId, proxy: (bool, usize, Option<&str>), out: &mut TreeEdits) {
    // The whole panel takes drops too: an item released outside every folder
    // goes back to the top level. Folder headers claim theirs first.
    let (_, dropped) = ui.dnd_drop_zone::<AssetDrag, _>(egui::Frame::NONE, |ui| {
        ui.set_min_width(ui.available_width());
        ui.data_mut(|d| d.insert_temp(egui::Id::new("asset_rects"), Vec::<(AssetDrag, egui::Rect)>::new()));
        panel_ui(ui, project, current, proxy, out);
        // The empty space under the rows: a click clears the selection, a
        // drag sweeps a marquee over the rows.
        let size = egui::vec2(ui.available_width(), ui.available_height().max(40.0));
        let (_, empty) = ui.allocate_exact_size(size, egui::Sense::click_and_drag());
        let rects: Vec<(AssetDrag, egui::Rect)> = ui.data(|d| d.get_temp(egui::Id::new("asset_rects"))).unwrap_or_default();
        if let Some((hits, mode)) = crate::layers::sweep_rows(ui, &empty, &rects, egui::Id::new("asset_marquee")) {
            let next = crate::layers::combine(&selection(ui.ctx()), &hits, mode);
            ui.data_mut(|d| d.insert_temp(sel_id(), next));
        }
    });
    // Delete with the pointer over the panel removes the selection from the
    // project (the app then skips deleting layers).
    let over = ui.rect_contains_pointer(ui.max_rect());
    if over && !ui.ctx().egui_wants_keyboard_input() && ui.input(|i| i.key_pressed(egui::Key::Delete) || i.key_pressed(egui::Key::Backspace)) {
        out.remove_assets = selection(ui.ctx());
    }
    if let Some(item) = dropped {
        if project.folders.iter().any(|f| f.items.contains(&item)) {
            out.folder = Some(FolderEdit::File(*item, None));
        }
    }
}

fn panel_ui(ui: &mut egui::Ui, project: &MProject, current: CompId, proxy: (bool, usize, Option<&str>), out: &mut TreeEdits) {
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
    ui.horizontal(|ui| {
        let mut on = proxy.0;
        if crate::strips::pill(ui, &mut on, "Preview proxies")
            .on_hover_text("Play and scrub heavy video from small all-intra copies. Export always reads the originals.")
            .changed()
        {
            out.proxies = Some(on);
        }
        if proxy.1 > 0 {
            ui.spinner();
            ui.weak(format!("making {}", proxy.1));
        }
    });
    if let Some(e) = proxy.2 {
        ui.colored_label(egui::Color32::from_rgb(240, 150, 60), e);
    }
    let search_id = egui::Id::new("asset_search");
    let mut search: String = ui.data(|d| d.get_temp(search_id)).unwrap_or_default();
    ui.add(egui::TextEdit::singleline(&mut search).hint_text("🔍 Search").desired_width(f32::INFINITY))
        .on_hover_text("Drag onto Layers to add, onto a folder to file · double-click a file to add it, a comp to open it · Delete removes the selection from the project");
    ui.data_mut(|d| d.insert_temp(search_id, search.clone()));
    ui.data_mut(|d| d.insert_temp(egui::Id::new("asset_zebra"), 0usize));
    let needle = search.trim().to_lowercase();
    let matches = |item: &AssetDrag| needle.is_empty() || item_name(project, *item).to_lowercase().contains(&needle);
    ui.spacing_mut().item_spacing.y = 0.0;

    let rename_id = egui::Id::new("folder_rename");
    let mut renaming: Option<(usize, String)> = ui.data(|d| d.get_temp(rename_id));
    // Folded folders, by index. View state for the session, not saved.
    let fold_id = egui::Id::new("folder_folds");
    let mut folded: std::collections::HashSet<usize> = ui.data(|d| d.get_temp(fold_id)).unwrap_or_default();
    for (fi, folder) in project.folders.iter().enumerate() {
        // The header is one click target, allocated before its contents so
        // the rename box on top keeps its own clicks (as in the Layers panel).
        let items: Vec<AssetDrag> = folder.items.iter().copied().filter(|i| matches(i)).collect();
        if !needle.is_empty() && items.is_empty() {
            continue;
        }
        let (rect, r) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 22.0), egui::Sense::click());
        // A search opens every folder it finds something in.
        let open = !folded.contains(&fi) || !needle.is_empty();
        let n = ui.data(|d| d.get_temp::<usize>(egui::Id::new("asset_zebra"))).unwrap_or(0);
        ui.data_mut(|d| d.insert_temp(egui::Id::new("asset_zebra"), n + 1));
        ui.painter().rect_filled(rect, 0.0, crate::layers::row_fill(ui, n, false, r.hovered()));
        let mut row_ui = ui.new_child(
            egui::UiBuilder::new().max_rect(rect.shrink2(egui::vec2(4.0, 0.0))).layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        row_ui.style_mut().interaction.selectable_labels = false;
        {
            let ui = &mut row_ui;
            // The colour label swatch, Premiere-style; grey when unset.
            let (sw, _) = ui.allocate_exact_size(egui::vec2(12.0, 12.0), egui::Sense::hover());
            let c = crate::settings::label_color(folder.label).unwrap_or(egui::Color32::from_gray(90));
            ui.painter().rect_filled(sw, 2.0, c);
            let (ar, _) = ui.allocate_exact_size(egui::vec2(12.0, 16.0), egui::Sense::hover());
            crate::layers::fold_arrow(ui, ar, open, r.hovered());
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
            if let Some(l) = crate::settings::label_menu(ui, folder.label) {
                out.folder = Some(FolderEdit::Label(fi, l));
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
                for item in items {
                    item_row(ui, project, current, item, out);
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
        .filter(|i| !filed.contains(i) && matches(i));
    for item in loose {
        item_row(ui, project, current, item, out);
    }
}

/// What a row is called, for search.
fn item_name(project: &MProject, item: AssetDrag) -> String {
    match item {
        AssetDrag::Comp(id) => project.comps.get(&id).map(|c| c.label(id)).unwrap_or_default(),
        AssetDrag::File(id) => project.assets.get(&id).map(|a| a.name.clone()).unwrap_or_default(),
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
                out,
            );
            if dbl && !open {
                out.open_comp = Some(id);
            }
        }
        AssetDrag::File(id) => {
            let Some(a) = project.assets.get(&id) else { return };
            let dbl = row(
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
                out,
            );
            // Double-click a file: into the open comp, with the rest of the
            // selection.
            if dbl {
                out.place = with_selection(ui.ctx(), item);
            }
        }
    }
}
