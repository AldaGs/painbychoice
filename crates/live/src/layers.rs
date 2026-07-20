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
}

/// Flatten the scene graph depth-first into indented rows.
pub(crate) fn tree_rows(node: &motion_core::Node, depth: usize, out: &mut Vec<TreeRow>) {
    out.push(TreeRow {
        id: node.id,
        name: node.name.clone(),
        depth,
        is_group: node.shape.is_none(),
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
}

/// Left layers panel: the scene graph as a clickable, indented list. Clicking a
/// row selects that node; the ▲/▼ buttons restack it among its siblings.
pub(crate) fn tree_ui(ui: &mut egui::Ui, rows: &[TreeRow], selected: Option<NodeId>, out: &mut TreeEdits) {
    ui.add_space(8.0);
    ui.heading("Layers");
    ui.horizontal(|ui| {
        if ui.button("Save…").clicked() {
            out.save = true;
        }
        if ui.button("Load…").clicked() {
            out.load = true;
        }
    });
    ui.horizontal(|ui| {
        if ui.button("+ Rect").clicked() {
            out.add = Some(NewShape::Rect);
        }
        if ui.button("+ Ellipse").clicked() {
            out.add = Some(NewShape::Ellipse);
        }
        if ui.button("+ Group").clicked() {
            out.add = Some(NewShape::Group);
        }
    });
    ui.weak("Adds into the selected node, else the root.");
    ui.separator();
    for row in rows {
        ui.horizontal(|ui| {
            ui.add_space(6.0 + row.depth as f32 * 14.0);
            let icon = if row.is_group { "▶" } else { "•" };
            let label = format!("{icon} {}", row.name);
            if ui
                .selectable_label(selected == Some(row.id), label)
                .clicked()
            {
                out.select = Some(row.id);
            }
            // Reorder + delete (not meaningful for the root).
            if row.depth > 0 {
                if ui.small_button("▲").clicked() {
                    out.reorder = Some((row.id, -1));
                }
                if ui.small_button("▼").clicked() {
                    out.reorder = Some((row.id, 1));
                }
                if ui.small_button("✕").clicked() {
                    out.delete = Some(row.id);
                }
            }
        });
    }
}
