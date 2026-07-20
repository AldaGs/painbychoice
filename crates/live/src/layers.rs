//! The layers panel: the flattened scene tree and its edits.
//!
//! Moved verbatim out of `main.rs` when it was split by concern; the
//! only edit was widening visibility to `pub(crate)`.

use crate::*;

/// A flattened scene-tree row for the layers panel.
pub(crate) struct TreeRow {
    pub(crate) id: NodeId,
    pub(crate) name: String,
    pub(crate) depth: usize,
    pub(crate) is_group: bool,
    /// Set when this layer instances a composition — the row then offers to
    /// open it, which is how you get *into* a precomp.
    pub(crate) precomp: Option<CompId>,
}

/// Flatten the scene graph depth-first into indented rows.
pub(crate) fn tree_rows(node: &motion_core::Node, depth: usize, out: &mut Vec<TreeRow>) {
    out.push(TreeRow {
        id: node.id,
        name: node.name.clone(),
        depth,
        is_group: node.shape.is_none(),
        precomp: node.precomp,
    });
    for c in &node.children {
        tree_rows(c, depth + 1, out);
    }
}

/// A shape the "add" tools can create.
#[derive(Clone, Copy)]
pub(crate) enum NewShape {
    Rect,
    Ellipse,
    Group,
}

/// What the layers panel reports: selection, reorder, add, and/or delete.
#[derive(Default)]
pub(crate) struct TreeEdits {
    pub(crate) select: Option<NodeId>,
    /// (node, delta) — move among siblings (-1 up, +1 down).
    pub(crate) reorder: Option<(NodeId, i32)>,
    pub(crate) add: Option<NewShape>,
    pub(crate) delete: Option<NodeId>,
    pub(crate) save: bool,
    pub(crate) load: bool,
    /// Move the selection into a new composition and leave an instance behind —
    /// the core AE workflow.
    pub(crate) precompose: Option<NodeId>,
    /// Open the composition this precomp layer instances.
    pub(crate) open_comp: Option<CompId>,
}

/// Left layers panel: the scene graph as a clickable, indented list. Clicking a
/// row selects that node; the ▲/▼ buttons restack it among its siblings.
pub(crate) fn tree_ui(ui: &mut egui::Ui, rows: &[TreeRow], selected: Option<NodeId>, out: &mut TreeEdits) {
    ui.add_space(8.0);
    ui.heading("Layers");
    ui.horizontal(|ui| {
        if icon::button(ui, icon::SAVE, "Save the project (.pbc)").clicked() {
            out.save = true;
        }
        if icon::button(ui, icon::LOAD, "Load a project").clicked() {
            out.load = true;
        }
    });
    ui.horizontal(|ui| {
        if icon::button(ui, icon::RECT, "Add a rectangle").clicked() {
            out.add = Some(NewShape::Rect);
        }
        if icon::button(ui, icon::ELLIPSE, "Add an ellipse").clicked() {
            out.add = Some(NewShape::Ellipse);
        }
        if icon::button(ui, icon::GROUP, "Add a group").clicked() {
            out.add = Some(NewShape::Group);
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
    for row in rows {
        ui.horizontal(|ui| {
            ui.add_space(6.0 + row.depth as f32 * 14.0);
            // A precomp reads as a comp first and a layer second — it's the
            // one row whose contents live somewhere else.
            let glyph = match (row.precomp.is_some(), row.is_group) {
                (true, _) => icon::PRECOMP,
                (_, true) => icon::GROUP,
                (_, false) => icon::RECT,
            };
            ui.label(icon::text(glyph));
            let label = row.name.clone();
            if ui
                .selectable_label(selected == Some(row.id), label)
                .clicked()
            {
                out.select = Some(row.id);
            }
            if let Some(comp) = row.precomp {
                if icon::button(ui, icon::OPEN, "Edit this composition").clicked() {
                    out.open_comp = Some(comp);
                }
            }
            // Reorder + delete (not meaningful for the root).
            if row.depth > 0 {
                if icon::button(ui, icon::UP, "Move up (draw later)").clicked() {
                    out.reorder = Some((row.id, -1));
                }
                if icon::button(ui, icon::DOWN, "Move down (draw earlier)").clicked() {
                    out.reorder = Some((row.id, 1));
                }
                if icon::button(ui, icon::DELETE, "Delete this layer").clicked() {
                    out.delete = Some(row.id);
                }
            }
        });
    }
}
