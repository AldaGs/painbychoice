//! Undo / redo.
//!
//! The history stores **whole-document snapshots**, not inverse operations.
//! That choice is load-bearing: every panel reports its intent and *all*
//! document mutation happens in one contiguous phase after the UI pass, so a
//! single copy taken before that phase and one comparison after it covers
//! every edit site there is — including ones written later, which get undo for
//! free rather than needing a hand-written inverse each. An op-based history
//! would be cheaper in memory and forgetting one site would corrupt the stack
//! silently; a snapshot cannot miss an edit.
//!
//! The document is a *recipe* — shapes, tracks, expressions; footage is stored
//! as a path and a size, never pixels — so a snapshot is small, and the
//! comparison early-outs on the first differing field.
//!
//! Coalescing is per **pointer-down session**: everything between a press and
//! its release is one step, so dragging a gizmo, a `DragValue`, or a curve
//! handle costs one undo rather than one per frame. That is the rule Blender
//! and After Effects use, and it is why the step carries `open` — the entry at
//! the top of the stack is still accepting edits until the pointer comes up.

use crate::{AidEdits, CompEdits, DopeEdits, MProject, NewShape, NgEdits, NgOp, TreeEdits};

/// How many steps to keep. Old steps fall off the bottom; the document is
/// small, but an afternoon of dragging shouldn't grow without bound.
pub(crate) const MAX_STEPS: usize = 128;

/// One undoable step: the document *as it was before* the edit, plus what to
/// call the edit in the UI.
pub(crate) struct Step {
    project: MProject,
    /// What the edit was, for the Undo/Redo tooltips.
    label: String,
    /// Whether this step is still gathering edits — true while the pointer that
    /// started it is down. Never true for a step on the redo stack.
    open: bool,
}

/// The undo stack and its redo counterpart.
#[derive(Default)]
pub(crate) struct History {
    past: Vec<Step>,
    future: Vec<Step>,
}

/// What the Undo/Redo buttons ask for. At most one per frame — the two are
/// mutually exclusive by construction, since a click lands on one button.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum HistoryCmd {
    Undo,
    Redo,
}

/// Name this frame's edit, for the Undo/Redo tooltips.
///
/// The history itself is snapshot-based and needs no cooperation from the edit
/// sites, so a label is *only* a nicety — which is why this reads the reported
/// intents rather than asking every site to pass a string. An unrecognised edit
/// is simply "Edit": a vague label is a much smaller failure than a missing
/// undo step, so nothing here may gate whether a step is taken.
///
/// Read at the top of the apply phase, while the edit structs are still whole
/// (several are `take`n as they're applied).
pub(crate) fn edit_label(
    tree: &TreeEdits,
    ng: &NgEdits,
    dope: &DopeEdits,
    comp: &CompEdits,
    aid: &AidEdits,
) -> &'static str {
    if let Some(shape) = tree.add {
        return match shape {
            NewShape::Rect => "Add rectangle",
            NewShape::Ellipse => "Add ellipse",
            NewShape::Text => "Add text",
            NewShape::Group => "Add group",
        };
    }
    if tree.delete.is_some() {
        return "Delete layer";
    }
    if tree.reorder.is_some() {
        return "Reorder layer";
    }
    if tree.precompose.is_some() {
        return "Pre-compose";
    }
    if tree.split_shape.is_some() {
        return "Split shape";
    }
    if tree.import_footage {
        return "Import footage";
    }
    if let Some(op) = &ng.op {
        return match op {
            NgOp::Add { .. } => "Add node",
            NgOp::Remove { .. } => "Delete node",
            NgOp::Move { .. } | NgOp::MoveBy { .. } => "Move node",
            NgOp::Connect { .. } => "Connect nodes",
            NgOp::Disconnect { .. } => "Disconnect nodes",
            _ => "Edit node",
        };
    }
    if ng.module_op.is_some() {
        return "Edit module";
    }
    if ng.knob.is_some() {
        return "Edit knob";
    }
    if ng.import.is_some() || ng.import_shape.is_some() {
        return "Import to graph";
    }
    if ng.create_layer.is_some() {
        return "Create layer from node";
    }
    if dope.move_by.is_some() {
        return "Move keyframes";
    }
    if dope.set_timing.is_some() || dope.set_layer_timing.is_some() {
        return "Retime layer";
    }
    if comp.fps.is_some() {
        return "Change frame rate";
    }
    if comp.width.is_some() || comp.height.is_some() || comp.duration.is_some() {
        return "Composition settings";
    }
    if comp.camera.is_some() {
        return "Camera";
    }
    if comp.rename.is_some() {
        return "Rename composition";
    }
    if aid.add_guide.is_some() || aid.move_guide.is_some() || aid.remove_guide.is_some() {
        return "Guides";
    }
    "Edit"
}

impl History {
    /// Record `before` — the document as it stood ahead of this frame's edits —
    /// as an undoable step.
    ///
    /// `interacting` is "the pointer is down", which is what makes a drag one
    /// step: the open entry keeps the *oldest* `before` of the run, so undoing
    /// returns to where the drag began rather than to its previous frame.
    ///
    /// Any new edit invalidates the redo stack: history is a line, not a tree.
    pub(crate) fn record(&mut self, before: MProject, label: impl Into<String>, interacting: bool) {
        self.future.clear();
        // Merge into the run already in progress. The label of the first edit
        // of a drag wins — it named the gesture.
        if interacting && self.past.last().is_some_and(|s| s.open) {
            return;
        }
        self.past.push(Step { project: before, label: label.into(), open: interacting });
        if self.past.len() > MAX_STEPS {
            self.past.remove(0);
        }
    }

    /// The pointer is up: whatever run was gathering is finished, so the next
    /// edit starts a fresh step. Called every frame the pointer is not down.
    pub(crate) fn end_interaction(&mut self) {
        if let Some(s) = self.past.last_mut() {
            s.open = false;
        }
    }

    /// Swap `current` for the previous state, pushing what it was onto the redo
    /// stack. Returns the label of the undone edit, or `None` at the bottom.
    pub(crate) fn undo(&mut self, current: &mut MProject) -> Option<String> {
        let step = self.past.pop()?;
        let label = step.label;
        let undone = std::mem::replace(current, step.project);
        self.future.push(Step { project: undone, label: label.clone(), open: false });
        Some(label)
    }

    /// The mirror of [`undo`](Self::undo).
    pub(crate) fn redo(&mut self, current: &mut MProject) -> Option<String> {
        let step = self.future.pop()?;
        let label = step.label;
        let redone = std::mem::replace(current, step.project);
        // Closed, not open: a redone step must never absorb the next drag.
        self.past.push(Step { project: redone, label: label.clone(), open: false });
        Some(label)
    }

    /// What Undo would revert, for the button's label and tooltip.
    pub(crate) fn undo_label(&self) -> Option<&str> {
        self.past.last().map(|s| s.label.as_str())
    }

    /// What Redo would re-apply.
    pub(crate) fn redo_label(&self) -> Option<&str> {
        self.future.last().map(|s| s.label.as_str())
    }

    /// Forget everything. Loading a different project is not an edit *of* the
    /// open one, and undoing across the boundary would resurrect a document the
    /// user has moved on from.
    pub(crate) fn clear(&mut self) {
        self.past.clear();
        self.future.clear();
    }

    /// How deep the two stacks are — for tests, which assert on step *counts*
    /// (a drag is one step) rather than on the documents themselves.
    #[cfg(test)]
    pub(crate) fn depth(&self) -> (usize, usize) {
        (self.past.len(), self.future.len())
    }
}
