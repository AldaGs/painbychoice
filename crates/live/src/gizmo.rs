//! The on-canvas transform gizmo: move / rotate / scale the selected layer by
//! dragging handles over the preview instead of typing into the properties
//! panel.
//!
//! Two things make this simpler than it looks:
//!
//! * It is **painted with egui**, not vello. egui's pass runs *after* the vello
//!   render (see `App::redraw`), so a plain `ui.painter()` lands on top of the
//!   frame, and `ui.interact` gives hover and drag for free.
//! * It emits ordinary [`PropEdits`] (`pos_x`/`rot`/`scale_x`/…), the same
//!   struct the DragValues fill in. So dragging a handle auto-keys exactly like
//!   dragging the number does, and there is no second write path into the
//!   document that could disagree with the first.
//!
//! Three coordinate spaces are in play and mixing them is the only real hazard:
//! **comp** space (what the document stores), **physical pixels** (what `fit`
//! produces), and egui's **logical points** (physical / `ppp`). Everything
//! below converts at its edges and names the space in the variable.

use crate::*;

/// Which handle is being dragged. The gizmo is modal only for the duration of
/// one drag — there is no persistent "rotate mode".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GizmoHandle {
    /// The centre square: translate freely in the parent's plane.
    Move,
    /// An arrow: translate along the layer's own X or Y.
    MoveAxis(GizmoAxis),
    /// The ring: rotate about the anchor.
    Rotate,
    /// A box at the end of an axis: scale that axis about the anchor.
    ScaleAxis(GizmoAxis),
    /// The corner box: scale both axes together, preserving aspect.
    ScaleUniform,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GizmoAxis {
    X,
    Y,
}

/// The transform values as they were when a drag began, plus where the pointer
/// grabbed. Every delta is applied to *this*, never stacked on the previous
/// frame's result — the same reason the fps drag snapshots (see
/// `apply_fps_edit`): re-deriving from a moving base accumulates error, and
/// with an auto-keying property it would also bake that error into a keyframe.
#[derive(Clone, Copy, Debug)]
pub(crate) struct GizmoDrag {
    pub(crate) handle: GizmoHandle,
    /// Node the drag started on. A selection change mid-drag cancels it rather
    /// than silently retargeting.
    pub(crate) node: u64,
    pub(crate) start_pos: Vec2,
    pub(crate) start_rot: f64,
    pub(crate) start_scale: (f64, f64),
    /// Where the pointer grabbed, in **parent** space.
    pub(crate) grab_parent: Point,
}

/// Everything the gizmo needs about the selected layer, gathered before the UI
/// pass like every other snapshot in this crate.
#[derive(Clone, Copy, Debug)]
pub(crate) struct GizmoTarget {
    pub(crate) node: u64,
    /// Parent space → comp space. Built from the scene item's world matrix with
    /// the layer's *own* local matrix divided back out.
    pub(crate) parent: Affine,
    pub(crate) pos: Vec2,
    pub(crate) rot_deg: f64,
    pub(crate) scale: (f64, f64),
}

impl GizmoTarget {
    /// The layer's own local matrix, exactly as `Transform::resolve` builds it.
    /// Kept in lockstep with that function by construction — if the composition
    /// order there changes, this must change with it.
    fn local(pos: Vec2, rot_deg: f64, scale: (f64, f64), anchor: Vec2) -> Affine {
        Affine::translate(pos)
            * Affine::rotate(rot_deg.to_radians())
            * Affine::scale_non_uniform(scale.0, scale.1)
            * Affine::translate(-anchor)
    }

    /// Build a target from a scene item's world transform and the layer's
    /// resolved values. `world = parent · local`, so `parent = world · local⁻¹`
    /// — which is why the anchor has to be in [`NodeInfo`]: leave it out and
    /// `local` is wrong, so the recovered parent is wrong, and the gizmo
    /// tracks the cursor at an offset.
    pub(crate) fn new(node: u64, world: Affine, info: &NodeInfo) -> Self {
        let pos = Vec2::new(info.pos.0, info.pos.1);
        let anchor = Vec2::new(info.anchor.0, info.anchor.1);
        // A zero scale collapses `local`, and inverting a singular matrix gives
        // infinities that would put the handles at NaN. Fall back to a scale of
        // 1 for the *recovery* only. The result is an approximation — `world`
        // was built with the real scale — but a placed gizmo on a flattened
        // layer beats no gizmo, since scaling back up is what you came for.
        let safe = (
            if info.scale.0.abs() < 1e-9 { 1.0 } else { info.scale.0 },
            if info.scale.1.abs() < 1e-9 { 1.0 } else { info.scale.1 },
        );
        let local = Self::local(pos, info.rot, safe, anchor);
        Self {
            node,
            parent: world * local.inverse(),
            pos,
            rot_deg: info.rot,
            scale: info.scale,
        }
    }

    /// The gizmo's origin in parent space. `local` maps the *anchor point* to
    /// `position` by construction, so `position` already is the anchor as the
    /// parent sees it — which is why rotation and scale pivot here.
    fn origin_parent(&self) -> Point {
        Point::new(self.pos.x, self.pos.y)
    }

    /// The layer's own X / Y direction as a unit vector in parent space. Scale
    /// is deliberately *not* folded in: the arrows show orientation, and a
    /// squashed layer should not get squashed handles.
    fn axis_parent(&self, axis: GizmoAxis) -> Vec2 {
        let r = self.rot_deg.to_radians();
        match axis {
            GizmoAxis::X => Vec2::new(r.cos(), r.sin()),
            GizmoAxis::Y => Vec2::new(-r.sin(), r.cos()),
        }
    }
}

/// Handle geometry, in **logical points** so the gizmo stays the same size on
/// screen at every zoom — a gizmo that scaled with the comp would vanish when
/// you zoomed out, which is exactly when you want to grab it.
const ARROW_LEN: f32 = 62.0;
const ARROW_HEAD: f32 = 11.0;
const SCALE_BOX_AT: f32 = 78.0;
const BOX_HALF: f32 = 5.0;
const CENTRE_HALF: f32 = 6.0;
const RING_R: f32 = 96.0;
/// How close (in points) the pointer must be to the ring to grab it.
const RING_GRAB: f32 = 7.0;
/// Where the corner (uniform-scale) box sits: this far along *both* axes, so
/// it lands off the diagonal and can't be mistaken for either arrow.
const CORNER_AT: f32 = 46.0;

const X_COL: egui::Color32 = egui::Color32::from_rgb(232, 92, 92);
const Y_COL: egui::Color32 = egui::Color32::from_rgb(126, 200, 96);
const RING_COL: egui::Color32 = egui::Color32::from_rgb(120, 170, 235);
const CENTRE_COL: egui::Color32 = egui::Color32::from_rgb(240, 216, 90);
const HOT_COL: egui::Color32 = egui::Color32::WHITE;

/// Screen positions of every handle, in logical points. Derived once per frame
/// and shared by the painter and the hit-tester so the two cannot drift apart.
struct Layout {
    origin: egui::Pos2,
    /// Unit screen directions for the layer's X / Y. Screen Y grows downward
    /// and `fit` may flip, so these come from the transform rather than being
    /// assumed.
    dir: [egui::Vec2; 2],
}

impl Layout {
    fn tip(&self, axis: GizmoAxis, at: f32) -> egui::Pos2 {
        self.origin + self.dir[axis as usize] * at
    }
    fn corner(&self) -> egui::Pos2 {
        self.origin + self.dir[0] * CORNER_AT + self.dir[1] * CORNER_AT
    }
}

impl GizmoAxis {
    fn colour(self) -> egui::Color32 {
        match self {
            GizmoAxis::X => X_COL,
            GizmoAxis::Y => Y_COL,
        }
    }
}

/// Map a parent-space point to logical screen points.
fn to_screen(t: &GizmoTarget, fit: Affine, ppp: f64, p: Point) -> egui::Pos2 {
    let c = fit * (t.parent * p);
    egui::pos2((c.x / ppp) as f32, (c.y / ppp) as f32)
}

/// Map logical screen points back to parent space — the inverse of
/// [`to_screen`], used to put the pointer into the space the values live in.
fn to_parent(t: &GizmoTarget, fit: Affine, ppp: f64, p: egui::Pos2) -> Point {
    let phys = Point::new(p.x as f64 * ppp, p.y as f64 * ppp);
    // Parent → comp → physical is `fit · parent`, so the way back is its
    // inverse. Writing the product the other way round silently yields a
    // plausible-looking transform that drifts as soon as the layer is nested.
    (fit * t.parent).inverse() * phys
}

fn layout(t: &GizmoTarget, fit: Affine, ppp: f64) -> Layout {
    let o_parent = t.origin_parent();
    let origin = to_screen(t, fit, ppp, o_parent);
    // Take each axis direction through the *same* transform as the origin and
    // renormalise on screen: that way a rotated, flipped or non-uniformly
    // scaled parent still produces arrows pointing the way the layer actually
    // moves, at a fixed on-screen length.
    let mut dir = [egui::Vec2::X, egui::Vec2::Y];
    for (i, axis) in [GizmoAxis::X, GizmoAxis::Y].into_iter().enumerate() {
        let a = t.axis_parent(axis);
        let far = to_screen(t, fit, ppp, o_parent + a);
        let v = far - origin;
        dir[i] = if v.length() > 1e-4 { v.normalized() } else { dir[i] };
    }
    Layout { origin, dir }
}

/// Distance from `p` to the segment `a`–`b`, for arrow hit-testing.
fn dist_to_segment(p: egui::Pos2, a: egui::Pos2, b: egui::Pos2) -> f32 {
    let ab = b - a;
    let len2 = ab.length_sq();
    if len2 < 1e-6 {
        return (p - a).length();
    }
    let t = ((p - a).dot(ab) / len2).clamp(0.0, 1.0);
    (p - (a + ab * t)).length()
}

/// Which handle the pointer is over, if any. Ordered smallest-target-first so
/// the centre square wins over the arrows that pass through it, and the arrows
/// win over the ring.
fn hit(l: &Layout, p: egui::Pos2) -> Option<GizmoHandle> {
    if (p - l.origin).length() <= CENTRE_HALF + 3.0 {
        return Some(GizmoHandle::Move);
    }
    if (p - l.corner()).length() <= BOX_HALF + 3.0 {
        return Some(GizmoHandle::ScaleUniform);
    }
    for axis in [GizmoAxis::X, GizmoAxis::Y] {
        if (p - l.tip(axis, SCALE_BOX_AT)).length() <= BOX_HALF + 3.0 {
            return Some(GizmoHandle::ScaleAxis(axis));
        }
    }
    for axis in [GizmoAxis::X, GizmoAxis::Y] {
        if dist_to_segment(p, l.origin, l.tip(axis, ARROW_LEN)) <= 6.0 {
            return Some(GizmoHandle::MoveAxis(axis));
        }
    }
    if ((p - l.origin).length() - RING_R).abs() <= RING_GRAB {
        return Some(GizmoHandle::Rotate);
    }
    None
}

fn paint(painter: &egui::Painter, l: &Layout, hot: Option<GizmoHandle>) {
    let col = |h: GizmoHandle, base: egui::Color32| {
        if hot == Some(h) {
            HOT_COL
        } else {
            base
        }
    };

    // Rotation ring, drawn first so the arrows sit over it.
    let ring = col(GizmoHandle::Rotate, RING_COL);
    painter.circle_stroke(l.origin, RING_R, egui::Stroke::new(1.5, ring));

    for axis in [GizmoAxis::X, GizmoAxis::Y] {
        let c = col(GizmoHandle::MoveAxis(axis), axis.colour());
        let tip = l.tip(axis, ARROW_LEN);
        painter.line_segment([l.origin, tip], egui::Stroke::new(2.0, c));
        // Arrowhead: a triangle spanning the axis direction at the tip.
        let d = l.dir[axis as usize];
        let n = egui::vec2(-d.y, d.x);
        painter.add(egui::Shape::convex_polygon(
            vec![
                tip + d * ARROW_HEAD,
                tip + n * (ARROW_HEAD * 0.42),
                tip - n * (ARROW_HEAD * 0.42),
            ],
            c,
            egui::Stroke::NONE,
        ));

        let sc = col(GizmoHandle::ScaleAxis(axis), axis.colour());
        let bp = l.tip(axis, SCALE_BOX_AT);
        painter.rect_filled(egui::Rect::from_center_size(bp, egui::Vec2::splat(BOX_HALF * 2.0)), 1.0, sc);
    }

    // Uniform scale, off the diagonal so it can't be confused with either axis.
    let uc = col(GizmoHandle::ScaleUniform, CENTRE_COL);
    painter.rect_stroke(
        egui::Rect::from_center_size(l.corner(), egui::Vec2::splat(BOX_HALF * 2.0)),
        1.0,
        egui::Stroke::new(1.6, uc),
        egui::StrokeKind::Middle,
    );

    // Anchor / free-move square last, on top of everything.
    let mc = col(GizmoHandle::Move, CENTRE_COL);
    painter.rect_filled(
        egui::Rect::from_center_size(l.origin, egui::Vec2::splat(CENTRE_HALF * 2.0)),
        1.0,
        mc,
    );
}

/// Resolve one frame of a drag into transform values, given where the pointer
/// is now in parent space. Pure — no egui, no `App` — so the arithmetic is
/// unit-testable without a window, like `apply_fps_edit`.
///
/// Returns `(pos, rot_deg, scale)`; only the fields the handle owns differ from
/// the drag's starting values.
pub(crate) fn resolve_drag(drag: &GizmoDrag, now_parent: Point) -> (Vec2, f64, (f64, f64)) {
    let (pos, rot, scale) = (drag.start_pos, drag.start_rot, drag.start_scale);
    let delta = now_parent - drag.grab_parent;
    let origin = Point::new(pos.x, pos.y);

    // A drag that starts *on* the pivot has no radius to measure an angle or a
    // ratio from, so rotate/scale would divide by ~0. Hold the values instead.
    let grab_r = (drag.grab_parent - origin).hypot();

    match drag.handle {
        GizmoHandle::Move => (pos + delta, rot, scale),
        GizmoHandle::MoveAxis(axis) => {
            let a = axis_of(rot, axis);
            let along = delta.x * a.x + delta.y * a.y;
            (pos + a * along, rot, scale)
        }
        GizmoHandle::Rotate => {
            if grab_r < 1e-6 {
                return (pos, rot, scale);
            }
            let a0 = (drag.grab_parent - origin).atan2();
            let a1 = (now_parent - origin).atan2();
            (pos, rot + (a1 - a0).to_degrees(), scale)
        }
        GizmoHandle::ScaleAxis(axis) => {
            let a = axis_of(rot, axis);
            let d0 = (drag.grab_parent - origin).dot(a);
            let d1 = (now_parent - origin).dot(a);
            if d0.abs() < 1e-6 {
                return (pos, rot, scale);
            }
            let f = d1 / d0;
            let s = match axis {
                GizmoAxis::X => (scale.0 * f, scale.1),
                GizmoAxis::Y => (scale.0, scale.1 * f),
            };
            (pos, rot, s)
        }
        GizmoHandle::ScaleUniform => {
            if grab_r < 1e-6 {
                return (pos, rot, scale);
            }
            let f = (now_parent - origin).hypot() / grab_r;
            (pos, rot, (scale.0 * f, scale.1 * f))
        }
    }
}

/// The layer's X / Y unit vector in parent space for a given rotation. Free
/// function so [`resolve_drag`] can use it against the drag's *starting*
/// rotation rather than the live one — during a rotate the two differ, and
/// mixing them would make an axis drag curve.
fn axis_of(rot_deg: f64, axis: GizmoAxis) -> Vec2 {
    let r = rot_deg.to_radians();
    match axis {
        GizmoAxis::X => Vec2::new(r.cos(), r.sin()),
        GizmoAxis::Y => Vec2::new(-r.sin(), r.cos()),
    }
}

/// Draw the gizmo over the canvas and turn any drag into [`PropEdits`].
///
/// `drag` is the caller's persistent drag state (`App::gizmo_drag`); it is
/// started, updated and cleared here. `canvas` clips the interaction so a drag
/// that wanders over the timeline stops grabbing.
///
/// **Returns whether the gizmo owns the pointer** — a handle is under it, or a
/// drag is live. The caller must store that and refuse to start a canvas pick
/// while it holds, or clicking a handle also deselects the layer. `ui.interact`
/// is *not* enough on its own: `is_pointer_over_egui`, which is what the winit
/// handler consults, is **area-based, not widget-based** — over the canvas hole
/// it answers "is the pointer outside the root Ui's available rect", and an
/// interactive rect painted inside that hole doesn't change the answer.
pub(crate) fn gizmo_ui(
    ui: &mut egui::Ui,
    canvas: egui::Rect,
    target: &GizmoTarget,
    fit: Affine,
    ppp: f64,
    drag: &mut Option<GizmoDrag>,
    out: &mut PropEdits,
) -> bool {
    let l = layout(target, fit, ppp);

    // A selection change mid-drag drops it: the snapshot describes a different
    // layer, so continuing would write one layer's delta onto another's values.
    if drag.is_some_and(|d| d.node != target.node) {
        *drag = None;
    }

    let pointer = ui.ctx().pointer_latest_pos().filter(|p| canvas.contains(*p));
    let over = pointer.and_then(|p| hit(&l, p));

    // Claim the pointer **only** over a handle (or for the length of a drag) —
    // never the whole canvas. `is_pointer_over_egui` is what tells the winit
    // handler not to run click-picking, so a canvas-wide interactive rect would
    // make the preview unclickable everywhere the gizmo is shown.
    let resp = match (drag.is_some(), over, pointer) {
        (true, _, _) => Some(canvas),
        (false, Some(_), Some(p)) => {
            Some(egui::Rect::from_center_size(p, egui::Vec2::splat(20.0)).intersect(canvas))
        }
        _ => None,
    }
    .map(|rect| ui.interact(rect, ui.id().with("gizmo"), egui::Sense::click_and_drag()));

    if let Some(resp) = &resp {
        if resp.drag_started() {
            if let (Some(p), Some(handle)) = (pointer, over) {
                *drag = Some(GizmoDrag {
                    handle,
                    node: target.node,
                    start_pos: target.pos,
                    start_rot: target.rot_deg,
                    start_scale: target.scale,
                    grab_parent: to_parent(target, fit, ppp, p),
                });
            }
        }
        if resp.drag_stopped() {
            *drag = None;
        }
    }

    if let (Some(d), Some(p)) = (*drag, pointer) {
        let (pos, rot, scale) = resolve_drag(&d, to_parent(target, fit, ppp, p));
        match d.handle {
            GizmoHandle::Move | GizmoHandle::MoveAxis(_) => {
                out.pos_x = Some(pos.x);
                out.pos_y = Some(pos.y);
            }
            GizmoHandle::Rotate => out.rot = Some(rot),
            GizmoHandle::ScaleAxis(_) | GizmoHandle::ScaleUniform => {
                out.scale_x = Some(scale.0);
                out.scale_y = Some(scale.1);
            }
        }
    }

    let hot = drag.map(|d| d.handle).or(over);
    if hot.is_some() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
    }
    paint(&ui.painter_at(canvas), &l, hot);
    hot.is_some()
}
