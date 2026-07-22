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
            Some(MShape::Path(_)) => RowKind::Path,
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
/// This is a **display** order only: the document is untouched, and
/// `Node::reorder_child` still speaks in document indices. What flips with it
/// is the *meaning* of the panel's up/down buttons — see `layers_ui`.
pub(crate) fn tree_rows(node: &motion_core::Node, depth: usize, out: &mut Vec<TreeRow>) {
    out.push(TreeRow {
        id: node.id,
        name: node.name.clone(),
        depth,
        kind: RowKind::of(node),
        precomp: node.precomp,
    });
    for c in node.children.iter().rev() {
        tree_rows(c, depth + 1, out);
    }
}

/// The document-space delta for a "move up"/"move down" click.
///
/// The panel lists layers **front-most first** but `Node::reorder_child` speaks
/// in document indices, where a *later* sibling paints on top. So moving a row
/// up the list — towards the front — is `+1` in the document, not `-1`. The
/// inversion lives here, named, because it is exactly the sort of sign flip
/// that gets "fixed" back into a bug by someone reading only one side of it.
pub(crate) fn reorder_delta(up: bool) -> i32 {
    if up {
        1
    } else {
        -1
    }
}

/// A shape the "add" tools can create.
#[derive(Clone, Copy)]
pub(crate) enum NewShape {
    Rect,
    Ellipse,
    Text,
    Group,
}

/// What the layers panel reports: selection, reorder, add, and/or delete.
#[derive(Default)]
pub(crate) struct TreeEdits {
    pub(crate) select: Option<NodeId>,
    /// Move this layer's own shape into a child layer, so it can be stacked
    /// against its siblings like any other layer.
    pub(crate) split_shape: Option<NodeId>,
    /// (node, delta) — move among siblings in **document** order, where a
    /// later sibling paints on top. The panel lists layers front-first, so its
    /// up/down buttons invert this via [`reorder_delta`].
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
        if icon::button(ui, icon::TEXT, "Add a text layer").clicked() {
            out.add = Some(NewShape::Text);
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
            ui.label(icon::text(row_glyph(row)));
            let label = row.name.clone();
            let resp = ui.selectable_label(selected == Some(row.id), label);
            if resp.clicked() {
                out.select = Some(row.id);
            }
            // Structural commands live in a context menu rather than another
            // row button: they're rare, they need words to be unambiguous, and
            // the row is already carrying four icons.
            resp.context_menu(|ui| {
                // Only a layer that *has* artwork of its own can be split, and
                // splitting the root would restructure the comp itself.
                if row.kind != RowKind::Group && row.depth > 0 {
                    if ui
                        .button("Split shape into child layer")
                        .on_hover_text(
                            "A layer's own shape always draws behind its children.
                             This moves it into a real child layer, so it can be                              stacked like any other.",
                        )
                        .clicked()
                    {
                        out.split_shape = Some(row.id);
                        ui.close();
                    }
                } else {
                    ui.weak("No shape of its own to split.");
                }
            });
            if let Some(comp) = row.precomp {
                if icon::button(ui, icon::OPEN, "Edit this composition").clicked() {
                    out.open_comp = Some(comp);
                }
            }
            // Reorder + delete (not meaningful for the root).
            if row.depth > 0 {
                if icon::button(ui, icon::UP, "Move up (in front)").clicked() {
                    out.reorder = Some((row.id, reorder_delta(true)));
                }
                if icon::button(ui, icon::DOWN, "Move down (behind)").clicked() {
                    out.reorder = Some((row.id, reorder_delta(false)));
                }
                if icon::button(ui, icon::DELETE, "Delete this layer").clicked() {
                    out.delete = Some(row.id);
                }
            }
        });
    }
}
