//! The layers panel: the flattened scene tree and its edits.
//!
//! Moved verbatim out of `main.rs` when it was split by concern; the
//! only edit was widening visibility to `pub(crate)`.

use crate::*;

/// What kind of thing a layer row is, for its icon. A group has no shape; a
/// precomp is flagged separately (`TreeRow::precomp`) because it reads as a comp
/// first and a layer second.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum RowKind {
    Group,
    Rect,
    Ellipse,
    Path,
    Text,
    Footage,
}

impl RowKind {
    /// Read a node's row kind from its shape. Precomp-ness is orthogonal and
    /// lives on the row, not here.
    pub(crate) fn of(node: &motion_core::Node) -> RowKind {
        match &node.shape {
            None => RowKind::Group,
            Some(MShape::Rect { .. }) => RowKind::Rect,
            Some(MShape::Ellipse { .. }) => RowKind::Ellipse,
            Some(MShape::Path(_)) | Some(MShape::Vector { .. }) => RowKind::Path,
            Some(MShape::Text { .. }) => RowKind::Text,
            Some(MShape::Image { .. }) => RowKind::Footage,
        }
    }
}

/// A flattened scene-tree row for the layers panel.
pub(crate) struct TreeRow {
    pub(crate) id: NodeId,
    pub(crate) name: String,
    pub(crate) depth: usize,
    pub(crate) kind: RowKind,
    /// Set when this layer instances a composition — the row then offers to
    /// open it, which is how you get *into* a precomp.
    pub(crate) precomp: Option<CompId>,
    /// The containing layer and this row's slot among its siblings, in
    /// **document** order — what a drop next to this row is measured against.
    pub(crate) parent: Option<NodeId>,
    pub(crate) index: usize,
    pub(crate) children: usize,
    pub(crate) hidden: bool,
    pub(crate) locked: bool,
}

/// The icon a row shows. A precomp wins over its shape — the row is a comp
/// first — and each shape kind gets its own glyph, so an ellipse no longer
/// borrows the rectangle's square. Pure, so the mapping is unit-tested rather
/// than only reachable by eye. A `Path` has no dedicated glyph in the subset, so
/// it shares the rectangle's — the closest generic filled shape.
pub(crate) fn row_glyph(row: &TreeRow) -> &'static str {
    if row.precomp.is_some() {
        return icon::PRECOMP;
    }
    match row.kind {
        RowKind::Group => icon::GROUP,
        RowKind::Rect | RowKind::Path => icon::RECT,
        RowKind::Ellipse => icon::ELLIPSE,
        RowKind::Text => icon::TEXT,
        // Borrowed rather than its own glyph: a photo/film icon would mean
        // regenerating the subsetted icon font, and a missed regen renders
        // tofu. Footage is the one thing you *import*, so this reads.
        RowKind::Footage => icon::IMPORT,
    }
}

/// Flatten the scene graph depth-first into indented rows, **front-most first**.
///
/// Siblings are listed in reverse document order on purpose. Draw order is
/// document order — a later sibling paints over an earlier one (see
/// `eval::walk`) — so the last child is the front-most, and every tool this app
/// is modelled on (After Effects, Figma, Illustrator) puts the front-most layer
/// at the *top* of the list. Listing them in raw document order made the panel
/// read upside-down to anyone with that muscle memory.
///
/// This is a **display** order only: the document is untouched, and a drop is
/// translated back to document order by [`drop_target`].
pub(crate) fn tree_rows(node: &motion_core::Node, depth: usize, out: &mut Vec<TreeRow>) {
    push_rows(node, depth, None, 0, out);
}

fn push_rows(node: &motion_core::Node, depth: usize, parent: Option<NodeId>, index: usize, out: &mut Vec<TreeRow>) {
    out.push(TreeRow {
        id: node.id,
        name: node.name.clone(),
        depth,
        kind: RowKind::of(node),
        precomp: node.precomp,
        parent,
        index,
        children: node.children.len(),
        hidden: node.hidden,
        locked: node.locked,
    });
    for (i, c) in node.children.iter().enumerate().rev() {
        push_rows(c, depth + 1, Some(node.id), i, out);
    }
}

/// Where on a row a dragged layer is released.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum DropZone {
    /// The top band: in front of this row's layer.
    Above,
    /// The middle: inside it, as its front-most child.
    Into,
    /// The bottom band: behind it.
    Below,
}

impl DropZone {
    /// Which band of a row of height `h` the pointer at `y` (from the row's
    /// top) is in. Quarter bands top and bottom, the rest means "into".
    pub(crate) fn at(y: f32, h: f32) -> DropZone {
        if y < h * 0.25 {
            DropZone::Above
        } else if y > h * 0.75 {
            DropZone::Below
        } else {
            DropZone::Into
        }
    }
}

/// Where a drop lands, as `(parent, document index)` for `Node::move_node`.
///
/// The panel lists front-most first but the document is back-to-front, so a
/// drop *above* a row is the slot after it in the document (in front), and a
/// drop *below* is its own slot (behind). The root takes drops only inside.
pub(crate) fn drop_target(row: &TreeRow, zone: DropZone) -> (NodeId, usize) {
    match (row.parent, zone) {
        (Some(p), DropZone::Above) => (p, row.index + 1),
        (Some(p), DropZone::Below) => (p, row.index),
        _ => (row.id, row.children),
    }
}

/// Where "unparent" moves `id`: one level up (`all == false`) to sit just in
/// front of its old parent, or all the way to the comp root (`all`) in front
/// of the top-level layer it was inside. `None` when it is already top-level.
pub(crate) fn unparent_target(root: &motion_core::Node, id: NodeId, all: bool) -> Option<(NodeId, usize)> {
    let parent = root.parent_of(id)?;
    if parent.id == root.id {
        return None;
    }
    // The ancestor whose slot we land beside, and the node that holds it.
    let beside = if all {
        root.children.iter().find(|c| c.find(id).is_some())?.id
    } else {
        parent.id
    };
    let holder = root.parent_of(beside)?;
    let i = holder.children.iter().position(|c| c.id == beside)?;
    Some((holder.id, i + 1))
}

/// A layer being dragged in the panel.
#[derive(Clone, Copy, Debug)]
pub(crate) struct LayerDrag(pub(crate) NodeId);

/// Whether `id` or any layer above it in the tree is locked. Lock covers the
/// subtree, so a child of a locked group is as untouchable as the group.
pub(crate) fn is_locked(root: &motion_core::Node, id: NodeId) -> bool {
    fn walk(n: &motion_core::Node, id: NodeId, above: bool) -> Option<bool> {
        let here = above || n.locked;
        if n.id == id {
            return Some(here);
        }
        n.children.iter().find_map(|c| walk(c, id, here))
    }
    walk(root, id, false).unwrap_or(false)
}

/// A shape the "add" tools can create.
#[derive(Clone, Copy)]
pub(crate) enum NewShape {
    Rect,
    Ellipse,
    Text,
    Group,
    /// An empty editable path; adding one also arms the pen tool so the next
    /// canvas clicks place its anchors.
    Vector,
}

/// What the layers panel reports: selection, reorder, add, and/or delete.
#[derive(Default)]
pub(crate) struct TreeEdits {
    pub(crate) select: Option<NodeId>,
    /// Move this layer's own shape into a child layer, so it can be stacked
    /// against its siblings like any other layer.
    pub(crate) split_shape: Option<NodeId>,
    /// (node, new parent, document index) — a drag-and-drop in the panel.
    pub(crate) move_to: Option<(NodeId, NodeId, usize)>,
    pub(crate) toggle_hidden: Option<NodeId>,
    pub(crate) toggle_locked: Option<NodeId>,
    pub(crate) rename: Option<(NodeId, String)>,
    /// (layer, all the way to the root?) — out of its parent.
    pub(crate) unparent: Option<(NodeId, bool)>,
    pub(crate) add: Option<NewShape>,
    /// Open the import dialog. A bare flag rather than a path because the
    /// dialog is blocking and must not run during the UI pass.
    pub(crate) import: bool,
    /// The same dialog, but files only join the library.
    pub(crate) import_to_library: bool,
    /// Add this library item to the open comp — an Assets-panel drop or
    /// double-click.
    pub(crate) place: Option<AssetDrag>,
    pub(crate) delete: Option<NodeId>,
    /// Move the selection into a new composition and leave an instance behind —
    /// the core AE workflow.
    pub(crate) precompose: Option<NodeId>,
    /// Wrap this layer in a new group node, in place.
    pub(crate) group: Option<NodeId>,
    /// Dissolve this group, splicing its children into the parent.
    pub(crate) ungroup: Option<NodeId>,
    /// Open the composition this precomp layer instances.
    pub(crate) open_comp: Option<CompId>,
}

/// Left layers panel: the scene graph as a clickable, indented list. Clicking a
/// row selects that node; the ▲/▼ buttons restack it among its siblings.
pub(crate) fn tree_ui(ui: &mut egui::Ui, rows: &[TreeRow], selected: Option<NodeId>, out: &mut TreeEdits) {
    // The whole panel is a drop target for Assets-panel rows.
    let (_, dropped) = ui.dnd_drop_zone::<AssetDrag, _>(egui::Frame::NONE, |ui| {
        ui.set_min_width(ui.available_width());
        panel_ui(ui, rows, selected, out);
        // Fill the rest of the panel, so a drop anywhere lands.
        ui.allocate_space(egui::vec2(ui.available_width(), ui.available_height().max(40.0)));
    });
    if let Some(item) = dropped {
        out.place = Some(*item);
    }
}

fn panel_ui(ui: &mut egui::Ui, rows: &[TreeRow], selected: Option<NodeId>, out: &mut TreeEdits) {
    ui.add_space(8.0);
    ui.heading("Layers");
    ui.horizontal(|ui| {
        if icon::button(ui, icon::RECT, "Add a rectangle").clicked() {
            out.add = Some(NewShape::Rect);
        }
        if icon::button(ui, icon::ELLIPSE, "Add an ellipse").clicked() {
            out.add = Some(NewShape::Ellipse);
        }
        if icon::button(ui, icon::TEXT, "Add a text layer").clicked() {
            out.add = Some(NewShape::Text);
        }
        if icon::button(ui, icon::GROUP, "Add a group").clicked() {
            out.add = Some(NewShape::Group);
        }
        // No dedicated pen glyph in the icon subset (adding one needs a font
        // regen), so a plain text button — distinct enough beside the icons.
        if ui.button("Pen").on_hover_text("Draw a vector path with the pen tool").clicked() {
            out.add = Some(NewShape::Vector);
        }
        if icon::button(ui, icon::IMPORT, "Import images, video or audio (Ctrl+I)").clicked() {
            out.import = true;
        }
    });
    ui.weak("Adds into the selected node, else the root.");
    // Pre-compose: only meaningful with a non-root layer selected, since the
    // root *is* the comp.
    if let Some(id) = selected.filter(|id| rows.iter().any(|r| r.id == *id && r.depth > 0)) {
        if icon::labeled(
            ui,
            icon::PRECOMPOSE,
            "Pre-compose",
            "Move this layer into a new comp and leave an instance in its place",
        )
        .clicked()
        {
            out.precompose = Some(id);
        }
    }
    ui.separator();
    rows_ui(ui, rows, selected, out);
}

/// The rows left showing once folded layers hide everything inside them.
pub(crate) fn unfolded_rows<'a>(rows: &'a [TreeRow], folded: &std::collections::HashSet<NodeId>) -> Vec<&'a TreeRow> {
    // While inside a folded layer, the depth of that layer.
    let mut hide_below: Option<usize> = None;
    let mut out = Vec::new();
    for row in rows {
        match hide_below {
            Some(d) if row.depth > d => continue,
            _ => hide_below = None,
        }
        if folded.contains(&row.id) {
            hide_below = Some(row.depth);
        }
        out.push(row);
    }
    out
}

/// A painted eye (open, or struck through) or padlock (shut, or open). Painted
/// because the icon font subset has neither glyph.
fn switch(ui: &mut egui::Ui, eye: bool, on: bool, tip: &str) -> bool {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(16.0, 16.0), egui::Sense::click());
    let c = if resp.hovered() { ui.visuals().strong_text_color() } else { ui.visuals().text_color() };
    let p = ui.painter();
    let m = rect.center();
    if eye {
        // Dimmed when hidden, like Blender's closed eye.
        let c = if on { c } else { c.gamma_multiply(0.45) };
        let st = egui::Stroke::new(1.3, c);
        let lid = |sign: f32| {
            (0..=8)
                .map(|i| {
                    let t = i as f32 / 8.0 * std::f32::consts::PI;
                    m + egui::vec2(-6.0 * t.cos(), sign * 3.5 * t.sin())
                })
                .collect::<Vec<_>>()
        };
        p.add(egui::Shape::line(lid(-1.0), st));
        p.add(egui::Shape::line(lid(1.0), st));
        p.circle_filled(m, 1.8, c);
        if !on {
            p.line_segment([m + egui::vec2(-6.0, 5.0), m + egui::vec2(6.0, -5.0)], st);
        }
    } else {
        // Dimmed when unlocked: the lock is the exception worth seeing.
        let c = if on { c } else { c.gamma_multiply(0.45) };
        let st = egui::Stroke::new(1.3, c);
        let body = egui::Rect::from_center_size(m + egui::vec2(0.0, 2.5), egui::vec2(10.0, 7.0));
        p.rect_filled(body, 1.5, c);
        // The shackle: shut sits on both posts, open lifts off the right one.
        let lift = if on { 0.0 } else { -2.5 };
        let (l, r, top) = (body.left() + 2.0, body.right() - 2.0, body.top());
        p.line_segment([egui::pos2(l, top), egui::pos2(l, top - 4.0)], st);
        p.line_segment([egui::pos2(r, top + lift), egui::pos2(r, top - 4.0 + lift)], st);
        p.line_segment([egui::pos2(l, top - 4.0), egui::pos2(r, top - 4.0 + lift)], st);
    }
    resp.on_hover_text(tip).clicked()
}

fn rows_ui(ui: &mut egui::Ui, rows: &[TreeRow], selected: Option<NodeId>, out: &mut TreeEdits) {
    // The row whose name is being edited, kept in egui memory across frames.
    let rename_id = egui::Id::new("layer_rename");
    let mut renaming: Option<(NodeId, String)> = ui.data(|d| d.get_temp(rename_id));
    let guide = ui.visuals().widgets.noninteractive.bg_stroke;
    // Folded layers, by id. View state for the session, not saved.
    let fold_id = egui::Id::new("layer_folds");
    let mut folded: std::collections::HashSet<NodeId> = ui.data(|d| d.get_temp(fold_id)).unwrap_or_default();
    ui.spacing_mut().item_spacing.y = 0.0;
    for (n, row) in unfolded_rows(rows, &folded).into_iter().enumerate() {
        // The whole row is one click/drag target, allocated *before* its
        // contents so the arrow, switches and rename box sit on top of it and
        // keep their own clicks. (Wrapping just the name in `dnd_drag_source`
        // put a drag sensor over the label that ate double- and right-clicks.)
        let id = egui::Id::new(("layer_row", row.id));
        let (rect, r) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 22.0), egui::Sense::click_and_drag());
        let lifted = row.depth > 0 && r.dragged();
        if lifted {
            egui::DragAndDrop::set_payload(ui.ctx(), LayerDrag(row.id));
        }
        // A lifted row paints on a top layer that follows the pointer.
        let layer = if lifted { egui::LayerId::new(egui::Order::Tooltip, id) } else { ui.layer_id() };
        let fill = if selected == Some(row.id) {
            ui.visuals().selection.bg_fill
        } else if lifted || ui.rect_contains_pointer(rect) {
            egui::Color32::from_gray(132)
        } else if n % 2 == 0 {
            egui::Color32::from_gray(50)
        } else {
            egui::Color32::from_gray(60)
        };
        let mut row_ui = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(rect)
                .layer_id(layer)
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        row_ui.painter().rect_filled(rect, 0.0, fill);
        row_ui.style_mut().interaction.selectable_labels = false;
        {
            let ui = &mut row_ui;
            let indent = 6.0 + row.depth as f32 * 14.0;
            let (space, _) = ui.allocate_exact_size(egui::vec2(indent, 18.0), egui::Sense::hover());
            // Indent guides, one per ancestor level, like Blender's outliner.
            for d in 1..=row.depth {
                let x = space.left() + 6.0 + (d as f32 - 0.5) * 14.0;
                ui.painter().vline(x, space.y_range(), guide);
            }
            // The fold arrow, or its width in blank so names stay aligned.
            let (rect, resp) = ui.allocate_exact_size(egui::vec2(12.0, 16.0), egui::Sense::click());
            if row.children > 0 {
                let open = !folded.contains(&row.id);
                let c = if resp.hovered() { ui.visuals().strong_text_color() } else { ui.visuals().text_color() };
                let m = rect.center();
                let pts = if open {
                    vec![m + egui::vec2(-4.0, -2.0), m + egui::vec2(4.0, -2.0), m + egui::vec2(0.0, 3.0)]
                } else {
                    vec![m + egui::vec2(-2.0, -4.0), m + egui::vec2(3.0, 0.0), m + egui::vec2(-2.0, 4.0)]
                };
                ui.painter().add(egui::Shape::convex_polygon(pts, c, egui::Stroke::NONE));
                if resp.on_hover_text(if open { "Collapse" } else { "Expand" }).clicked() && !folded.remove(&row.id) {
                    folded.insert(row.id);
                }
            }
            ui.label(icon::text(row_glyph(row)));
            match renaming.as_mut().filter(|(id, _)| *id == row.id) {
                Some((_, buf)) => {
                    let r = ui.add(egui::TextEdit::singleline(buf).desired_width(110.0));
                    r.request_focus();
                    if r.lost_focus() {
                        if !ui.input(|i| i.key_pressed(egui::Key::Escape)) && !buf.trim().is_empty() {
                            out.rename = Some((row.id, buf.trim().to_string()));
                        }
                        renaming = None;
                    }
                }
                None => {
                    let name = if row.hidden {
                        egui::RichText::new(&row.name).weak()
                    } else {
                        egui::RichText::new(&row.name)
                    };
                    ui.label(name);
                }
            }
            if let Some(comp) = row.precomp {
                if icon::button(ui, icon::OPEN, "Edit this composition").clicked() {
                    out.open_comp = Some(comp);
                }
            }
            if row.depth > 0 {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add_space(4.0);
                    let tip = if row.locked { "Unlock" } else { "Lock: no picking or editing" };
                    if switch(ui, false, row.locked, tip) {
                        out.toggle_locked = Some(row.id);
                    }
                    let tip = if row.hidden { "Show" } else { "Hide" };
                    if switch(ui, true, !row.hidden, tip) {
                        out.toggle_hidden = Some(row.id);
                    }
                });
            }
        }
        if lifted {
            if let Some(p) = ui.ctx().pointer_interact_pos() {
                let lift = egui::emath::TSTransform::from_translation(egui::vec2(0.0, p.y - rect.center().y));
                ui.ctx().transform_layer_shapes(layer, lift);
            }
        }
        if r.clicked() {
            out.select = Some(row.id);
        }
        if r.double_clicked() && row.depth > 0 {
            renaming = Some((row.id, row.name.clone()));
        }
        if context_menu(&r, row, out) {
            renaming = Some((row.id, row.name.clone()));
        }
        // Dropping a dragged layer on this row: above, into, or below it.
        let hover = r.dnd_hover_payload::<LayerDrag>().and_then(|_| ui.ctx().pointer_hover_pos());
        if let Some(pos) = hover {
            let zone = if row.depth == 0 { DropZone::Into } else { DropZone::at(pos.y - r.rect.top(), r.rect.height()) };
            let st = egui::Stroke::new(2.0, ui.visuals().selection.stroke.color);
            let p = ui.painter();
            match zone {
                DropZone::Above => p.hline(r.rect.x_range(), r.rect.top(), st),
                DropZone::Below => p.hline(r.rect.x_range(), r.rect.bottom(), st),
                DropZone::Into => p.rect_stroke(r.rect, 2.0, st, egui::StrokeKind::Inside),
            };
            if let Some(drag) = r.dnd_release_payload::<LayerDrag>() {
                let (parent, index) = drop_target(row, zone);
                out.move_to = Some((drag.0, parent, index));
            }
        }
    }
    ui.data_mut(|d| d.insert_temp(fold_id, folded));
    ui.data_mut(|d| match renaming {
        Some(v) => {
            d.insert_temp(rename_id, v);
        }
        None => d.remove::<(NodeId, String)>(rename_id),
    });
}

/// The row's right-click menu: the structural commands that need words.
/// Returns whether Rename was chosen.
fn context_menu(resp: &egui::Response, row: &TreeRow, out: &mut TreeEdits) -> bool {
    let mut rename = false;
    resp.context_menu(|ui| {
        if row.depth == 0 {
            ui.weak("The composition root.");
            return;
        }
        if ui.button("Rename").clicked() {
            rename = true;
            ui.close();
        }
        // Only a layer that *has* artwork of its own can be split.
        if row.kind != RowKind::Group
            && ui
                .button("Split shape into child layer")
                .on_hover_text(
                    "A layer's own shape always draws behind its children. \
                     This moves it into a real child layer, so it can be \
                     stacked like any other.",
                )
                .clicked()
        {
            out.split_shape = Some(row.id);
            ui.close();
        }
        if ui.button("Group").on_hover_text("Wrap this layer in a new group node").clicked() {
            out.group = Some(row.id);
            ui.close();
        }
        if row.kind == RowKind::Group
            && ui
                .button("Ungroup")
                .on_hover_text("Dissolve this group, keeping its children in place")
                .clicked()
        {
            out.ungroup = Some(row.id);
            ui.close();
        }
        // Out of the parent: one level, or out of every parent at once.
        if row.depth > 1 {
            ui.separator();
            if ui.button("Unparent").on_hover_text("Move out one level, keeping its place on screen").clicked() {
                out.unparent = Some((row.id, false));
                ui.close();
            }
            if row.depth > 2
                && ui.button("Unparent to root").on_hover_text("Move out of every parent to the top level").clicked()
            {
                out.unparent = Some((row.id, true));
                ui.close();
            }
        }
        ui.separator();
        if ui.button("Delete").on_hover_text("Delete (Del)").clicked() {
            out.delete = Some(row.id);
            ui.close();
        }
    });
    rename
}
