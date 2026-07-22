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
    /// A ring: rotate about the anchor, around the named axis. Z is the
    /// in-plane spin a 2D layer has always had; X and Y tip the layer out of
    /// the plane and only appear in a comp with a camera.
    RotateAxis(GizmoAxis),
    /// A box at the end of an axis: scale that axis about the anchor.
    ScaleAxis(GizmoAxis),
    /// The corner box: scale both axes together, preserving aspect.
    ScaleUniform,
    /// The ring just outside the centre: move the **anchor** without moving the
    /// layer. Position is compensated to cancel it out — see `resolve_drag`.
    Anchor,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GizmoAxis {
    X,
    Y,
    /// Depth. Unlike X and Y it has no direction of its own on screen — it is
    /// the axis you are looking *along* — so its handles are drawn toward the
    /// vanishing point and only exist when a camera makes that point real.
    Z,
}

impl GizmoAxis {
    /// The two axes spanning the plane a rotation about this one turns in,
    /// right-handed: rotating about X takes Y toward Z.
    fn ring_basis(self) -> (GizmoAxis, GizmoAxis) {
        match self {
            GizmoAxis::X => (GizmoAxis::Y, GizmoAxis::Z),
            GizmoAxis::Y => (GizmoAxis::Z, GizmoAxis::X),
            GizmoAxis::Z => (GizmoAxis::X, GizmoAxis::Y),
        }
    }
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
    /// Depth is snapshotted like everything else, so a Z drag is a delta from
    /// where it began rather than an accumulation.
    pub(crate) start_pos_z: f64,
    pub(crate) start_rot: f64,
    /// The out-of-plane rotations at grab time, in degrees.
    pub(crate) start_rot_xy: (f64, f64),
    pub(crate) start_scale: (f64, f64),
    pub(crate) start_anchor: Vec2,
    /// Where the pointer grabbed, in **parent** space.
    pub(crate) grab_parent: Point,
    /// The gizmo's origin in logical screen points, as it was at grab time.
    ///
    /// Snapshotted rather than read live because a depth drag *moves* the
    /// origin — the layer slides toward the vanishing point as it recedes — and
    /// a ring angle measured against a moving centre would creep under the
    /// pointer instead of following it.
    pub(crate) origin_screen: egui::Pos2,
    /// Where the pointer grabbed, in logical screen points.
    ///
    /// The depth handles need this and cannot use `grab_parent`: parent space
    /// is two-dimensional, and the whole point of a Z drag is that it moves
    /// along the one direction that space cannot express. So the 3D handles
    /// measure on screen, where the projection has already made depth visible.
    pub(crate) grab_screen: egui::Pos2,
}

/// Everything the gizmo needs about the selected layer, gathered before the UI
/// pass like every other snapshot in this crate.
#[derive(Clone, Copy, Debug)]
pub(crate) struct GizmoTarget {
    pub(crate) node: u64,
    /// Parent space → comp space. Built from the node's world matrix (from
    /// `Scene::places`) with the layer's *own* local matrix divided back out.
    pub(crate) parent: Affine,
    pub(crate) pos: Vec2,
    pub(crate) pos_z: f64,
    pub(crate) rot_deg: f64,
    /// The out-of-plane rotations, in degrees.
    pub(crate) rot_xy: (f64, f64),
    pub(crate) scale: (f64, f64),
    /// Needed to *recover* `parent`, and now to drag the anchor itself.
    pub(crate) anchor: Vec2,
    /// The comp's eye point and distance, when it has a camera.
    ///
    /// **This is what gates the depth handles.** Without a camera there is no
    /// projection, so depth has no direction on screen and a Z arrow would be a
    /// control you could drag with nothing happening. The gizmo therefore shows
    /// exactly the axes the composition can actually express.
    pub(crate) view: Option<(Point, f64)>,
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

    /// Build a target from a node's world transform and the layer's resolved
    /// values. `world = parent · local`, so `parent = world · local⁻¹`
    /// — which is why the anchor has to be in [`NodeInfo`]: leave it out and
    /// `local` is wrong, so the recovered parent is wrong, and the gizmo
    /// tracks the cursor at an offset.
    pub(crate) fn new(
        node: u64,
        world: Affine,
        info: &NodeInfo,
        view: Option<(Point, f64)>,
    ) -> Self {
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
            pos_z: info.pos.2,
            rot_deg: info.rot,
            rot_xy: info.rot_xy,
            view,
            // The in-plane pair only: this gizmo drags handles in screen
            // space, so depth scale is not something it can express or edit.
            scale: (info.scale.0, info.scale.1),
            anchor,
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
            // Depth has no parent-plane direction by construction. Callers that
            // want it ask the layout, which measures it on screen.
            GizmoAxis::Z => Vec2::ZERO,
        }
    }

    /// Where this layer's origin sits in composition space **before** the
    /// camera, recovered by undoing the projection the placement already
    /// carries. Needed to measure how far a unit of depth moves on screen.
    /// The depth used is the layer's **own** `z`, not the total depth of the
    /// chain above it. Exact for a layer parented to the comp root, and an
    /// approximation inside a parent that itself carries depth — the arrow
    /// still points the right way there, its gearing is just measured against
    /// the wrong rung of the ladder.
    fn origin_comp_unprojected(&self) -> Option<(Point, Point, f64)> {
        let (eye, d) = self.view?;
        let s = d / (d + self.pos_z);
        if !s.is_finite() || s.abs() < 1e-9 {
            return None;
        }
        // `parent` already carries the camera's scale, so this point is where
        // the layer landed *after* projection. Dividing that scale back out
        // recovers where it would sit with the eye at infinity, which is the
        // point receding actually pivots around.
        let projected = self.parent * self.origin_parent();
        Some((eye + (projected - eye) / s, eye, d))
    }
}

/// Handle geometry, in **logical points** so the gizmo stays the same size on
/// screen at every zoom — a gizmo that scaled with the comp would vanish when
/// you zoomed out, which is exactly when you want to grab it.
const ARROW_LEN: f32 = 62.0;
const ARROW_HEAD: f32 = 11.0;
const SCALE_BOX_AT: f32 = 78.0;
const BOX_HALF: f32 = 5.0;
const CENTRE_HALF: f32 = 5.0;
/// Radius of the anchor ring, and how far either side of it counts as a grab.
/// It sits just outside the centre square so the two never fight, and inside
/// everything else so it can't be confused with the rotation ring.
const ANCHOR_R: f32 = 12.0;
const ANCHOR_GRAB: f32 = 4.5;
const RING_R: f32 = 96.0;
/// How close (in points) the pointer must be to the ring to grab it.
const RING_GRAB: f32 = 7.0;
/// Where the corner (uniform-scale) box sits: this far along *both* axes, so
/// it lands off the diagonal and can't be mistaken for either arrow.
const CORNER_AT: f32 = 46.0;

const X_COL: egui::Color32 = egui::Color32::from_rgb(232, 92, 92);
const Y_COL: egui::Color32 = egui::Color32::from_rgb(126, 200, 96);
const Z_COL: egui::Color32 = egui::Color32::from_rgb(120, 170, 235);
const RING_COL: egui::Color32 = egui::Color32::from_rgb(120, 170, 235);
const CENTRE_COL: egui::Color32 = egui::Color32::from_rgb(240, 216, 90);
const HOT_COL: egui::Color32 = egui::Color32::WHITE;

/// Screen positions of every handle, in logical points. Derived once per frame
/// and shared by the painter and the hit-tester so the two cannot drift apart.
struct Layout {
    origin: egui::Pos2,
    /// Unit screen directions for the layer's X / Y / Z. Screen Y grows
    /// downward and `fit` may flip, so these come from the transform rather
    /// than being assumed.
    ///
    /// The Z entry is only meaningful when [`Layout::spatial`] is set: it is
    /// measured toward the vanishing point, which does not exist without a
    /// camera.
    dir: [egui::Vec2; 3],
    /// Logical screen points travelled per unit of depth, and the sign of that
    /// travel. `None` in a flat comp, or where the layer sits exactly on the
    /// optical axis and depth moves it nowhere at all — there the drag has no
    /// gearing to invert, so the depth handles are withheld rather than made
    /// infinitely sensitive.
    spatial: Option<f32>,
}

impl Layout {
    fn tip(&self, axis: GizmoAxis, at: f32) -> egui::Pos2 {
        self.origin + self.dir[axis as usize] * at
    }
    /// The axes with handles: X and Y always, Z only where depth reads.
    fn axes(&self) -> &'static [GizmoAxis] {
        if self.spatial.is_some() {
            &[GizmoAxis::X, GizmoAxis::Y, GizmoAxis::Z]
        } else {
            &[GizmoAxis::X, GizmoAxis::Y]
        }
    }
    /// A point on the ring that turns about `axis`, at parameter `t` radians.
    fn ring_point(&self, axis: GizmoAxis, t: f32) -> egui::Pos2 {
        let (u, v) = axis.ring_basis();
        self.origin
            + self.dir[u as usize] * (RING_R * t.cos())
            + self.dir[v as usize] * (RING_R * t.sin())
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
            GizmoAxis::Z => Z_COL,
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
    let mut dir = [egui::Vec2::X, egui::Vec2::Y, egui::Vec2::ZERO];
    for (i, axis) in [GizmoAxis::X, GizmoAxis::Y].into_iter().enumerate() {
        let a = t.axis_parent(axis);
        let far = to_screen(t, fit, ppp, o_parent + a);
        let v = far - origin;
        dir[i] = if v.length() > 1e-4 { v.normalized() } else { dir[i] };
    }

    // Depth, measured rather than derived. Stepping the layer one unit further
    // away and asking where it lands gives both the on-screen direction of
    // depth *and* the gearing a drag has to invert — in one finite difference,
    // through the very projection the frame was drawn with. Deriving it in
    // closed form would be a second copy of the camera's arithmetic, free to
    // disagree with the first.
    let spatial = t.origin_comp_unprojected().and_then(|(p, eye, d)| {
        let at = |z: f64| {
            let s = d / (d + z);
            let c = fit * (eye + (p - eye) * s);
            egui::pos2((c.x / ppp) as f32, (c.y / ppp) as f32)
        };
        let step = at(t.pos_z + 1.0) - at(t.pos_z);
        let len = step.length();
        // Too small to point anywhere: the layer is on the optical axis, where
        // receding moves it nowhere and no drag could be geared back.
        (len > 1e-5).then(|| {
            dir[GizmoAxis::Z as usize] = step.normalized();
            len
        })
    });
    Layout { origin, dir, spatial }
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
    if (p - l.origin).length() <= CENTRE_HALF + 2.0 {
        return Some(GizmoHandle::Move);
    }
    // Before the arrows, which pass straight through this radius on their way
    // out — the ring is the smaller, more specific target, so it wins here.
    if ((p - l.origin).length() - ANCHOR_R).abs() <= ANCHOR_GRAB {
        return Some(GizmoHandle::Anchor);
    }
    if (p - l.corner()).length() <= BOX_HALF + 3.0 {
        return Some(GizmoHandle::ScaleUniform);
    }
    // Scale has no depth handle: `scale.z` multiplies a dimension the geometry
    // does not have (every layer is flat in its own space), so a Z scale box
    // would be a control with nothing behind it.
    for axis in [GizmoAxis::X, GizmoAxis::Y] {
        if (p - l.tip(axis, SCALE_BOX_AT)).length() <= BOX_HALF + 3.0 {
            return Some(GizmoHandle::ScaleAxis(axis));
        }
    }
    for &axis in l.axes() {
        if dist_to_segment(p, l.origin, l.tip(axis, ARROW_LEN)) <= 6.0 {
            return Some(GizmoHandle::MoveAxis(axis));
        }
    }
    // Rings last, and Z first among them: it is the one a 2D layer has, so a
    // click where the rings cross keeps meaning what it always did.
    for &axis in [GizmoAxis::Z, GizmoAxis::X, GizmoAxis::Y].iter() {
        if axis != GizmoAxis::Z && l.spatial.is_none() {
            continue;
        }
        if ring_distance(l, axis, p) <= RING_GRAB {
            return Some(GizmoHandle::RotateAxis(axis));
        }
    }
    None
}

/// Distance from `p` to the ring that turns about `axis`, by sampling it.
///
/// The ring is a circle in space but an **ellipse** on screen — sometimes a
/// near-flat one, when the layer is turned almost edge-on to it. Sampling costs
/// a few dozen distances and stays correct for every degenerate case a closed
/// form would have to special-case.
fn ring_distance(l: &Layout, axis: GizmoAxis, p: egui::Pos2) -> f32 {
    let mut best = f32::MAX;
    let mut prev = l.ring_point(axis, 0.0);
    for i in 1..=RING_SAMPLES {
        let t = i as f32 / RING_SAMPLES as f32 * std::f32::consts::TAU;
        let next = l.ring_point(axis, t);
        best = best.min(dist_to_segment(p, prev, next));
        prev = next;
    }
    best
}

/// How finely a rotation ring is sampled, for both drawing and hit-testing.
const RING_SAMPLES: usize = 64;

fn paint(painter: &egui::Painter, l: &Layout, hot: Option<GizmoHandle>) {
    let col = |h: GizmoHandle, base: egui::Color32| {
        if hot == Some(h) {
            HOT_COL
        } else {
            base
        }
    };

    // Rotation rings, drawn first so the arrows sit over them. Each is a circle
    // in space, so on screen it is an ellipse that flattens as the layer turns
    // edge-on to it — which is exactly the feedback that tells you which way
    // the layer is already facing.
    for &axis in l.axes() {
        let base = if axis == GizmoAxis::Z { RING_COL } else { axis.colour() };
        let c = col(GizmoHandle::RotateAxis(axis), base);
        let pts: Vec<egui::Pos2> = (0..=RING_SAMPLES)
            .map(|i| l.ring_point(axis, i as f32 / RING_SAMPLES as f32 * std::f32::consts::TAU))
            .collect();
        painter.add(egui::Shape::line(pts, egui::Stroke::new(1.5, c)));
    }

    // The depth arrow, toward the vanishing point. Drawn before the in-plane
    // pair so those stay on top where they overlap — they are the ones you
    // reach for most.
    if l.spatial.is_some() {
        let c = col(GizmoHandle::MoveAxis(GizmoAxis::Z), Z_COL);
        let tip = l.tip(GizmoAxis::Z, ARROW_LEN);
        painter.line_segment([l.origin, tip], egui::Stroke::new(2.0, c));
        let d = l.dir[GizmoAxis::Z as usize];
        let n = egui::vec2(-d.y, d.x);
        // Hollow head, so depth never reads as a third in-plane axis at a
        // glance: the two solid arrows are the ones that move within the frame.
        painter.add(egui::Shape::closed_line(
            vec![
                tip + d * ARROW_HEAD,
                tip + n * (ARROW_HEAD * 0.42),
                tip - n * (ARROW_HEAD * 0.42),
            ],
            egui::Stroke::new(1.6, c),
        ));
    }

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

    // The anchor ring, drawn as AE draws an anchor: a circle crossed by four
    // ticks, so it reads as a pivot rather than another scale handle.
    let ac = col(GizmoHandle::Anchor, CENTRE_COL);
    painter.circle_stroke(l.origin, ANCHOR_R, egui::Stroke::new(1.3, ac));
    for (dx, dy) in [(1.0, 0.0), (-1.0, 0.0), (0.0, 1.0), (0.0, -1.0)] {
        let d = egui::vec2(dx, dy);
        painter.line_segment(
            [l.origin + d * (ANCHOR_R - 3.0), l.origin + d * (ANCHOR_R + 3.0)],
            egui::Stroke::new(1.3, ac),
        );
    }

    // Anchor / free-move square last, on top of everything.
    let mc = col(GizmoHandle::Move, CENTRE_COL);
    painter.rect_filled(
        egui::Rect::from_center_size(l.origin, egui::Vec2::splat(CENTRE_HALF * 2.0)),
        1.0,
        mc,
    );
}

/// What one frame of a drag resolves to. Only the fields the dragged handle
/// owns differ from the values the drag started with.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Resolved {
    pub(crate) pos: Vec2,
    pub(crate) pos_z: f64,
    pub(crate) rot: f64,
    /// The out-of-plane rotations, in degrees.
    pub(crate) rot_xy: (f64, f64),
    pub(crate) scale: (f64, f64),
    pub(crate) anchor: Vec2,
}

/// What the depth-aware handles need and parent space cannot give them: where
/// the pointer is on screen, the screen basis the rings turn in, and the gearing
/// between a screen point and a unit of depth.
///
/// Passed alongside the parent-space point rather than replacing it, so the
/// in-plane handles keep resolving in the space their values actually live in —
/// unchanged, and still exact at any zoom.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ScreenDrag {
    pub(crate) now: egui::Pos2,
    /// Unit screen directions for X / Y / Z.
    pub(crate) dir: [egui::Vec2; 3],
    /// Logical screen points travelled per unit of depth. `None` in a flat
    /// comp, where no depth handle can be grabbed in the first place.
    pub(crate) z_px: Option<f32>,
}

impl ScreenDrag {
    /// A basis with no depth in it — what a flat comp resolves against, and
    /// what the in-plane tests measure through.
    #[cfg(test)]
    pub(crate) fn flat() -> Self {
        Self {
            now: egui::Pos2::ZERO,
            dir: [egui::Vec2::X, egui::Vec2::Y, egui::Vec2::ZERO],
            z_px: None,
        }
    }

    /// The angle of `p` about the ring that turns around `axis`, in that ring's
    /// own basis. `None` where the ring has collapsed to a line on screen and an
    /// angle around it would mean nothing.
    fn ring_angle(&self, axis: GizmoAxis, origin: egui::Pos2, p: egui::Pos2) -> Option<f64> {
        let (u, v) = axis.ring_basis();
        let (du, dv) = (self.dir[u as usize], self.dir[v as usize]);
        let w = p - origin;
        let (a, b) = (w.dot(du) as f64, w.dot(dv) as f64);
        (a.hypot(b) > 1e-4).then(|| b.atan2(a))
    }
}

/// Resolve one frame of a drag, given where the pointer is now in parent space.
/// Pure — no egui, no `App` — so the arithmetic is unit-testable without a
/// window, like `apply_fps_edit`.
pub(crate) fn resolve_drag(
    drag: &GizmoDrag,
    now_parent: Point,
    screen: ScreenDrag,
) -> Resolved {
    let base = Resolved {
        pos: drag.start_pos,
        pos_z: drag.start_pos_z,
        rot: drag.start_rot,
        rot_xy: drag.start_rot_xy,
        scale: drag.start_scale,
        anchor: drag.start_anchor,
    };
    let (pos, rot, scale) = (base.pos, base.rot, base.scale);
    let delta = now_parent - drag.grab_parent;
    let origin = Point::new(pos.x, pos.y);

    // A drag that starts *on* the pivot has no radius to measure an angle or a
    // ratio from, so rotate/scale would divide by ~0. Hold the values instead.
    let grab_r = (drag.grab_parent - origin).hypot();

    match drag.handle {
        GizmoHandle::Move => Resolved { pos: pos + delta, ..base },
        // Depth: measured on screen, then divided by the gearing the layout
        // measured through the real projection. A drag of N points along the
        // arrow moves the layer however far actually reads as N points from
        // here — so a distant layer travels further per point than a near one,
        // which is what keeps the handle feeling attached to the artwork rather
        // than to the number behind it.
        GizmoHandle::MoveAxis(GizmoAxis::Z) => {
            let Some(px) = screen.z_px.filter(|p| *p > 1e-5) else { return base };
            let along = (screen.now - drag.grab_screen).dot(screen.dir[GizmoAxis::Z as usize]);
            Resolved { pos_z: base.pos_z + (along / px) as f64, ..base }
        }
        GizmoHandle::MoveAxis(axis) => {
            let a = axis_of(rot, axis);
            let along = delta.x * a.x + delta.y * a.y;
            Resolved { pos: pos + a * along, ..base }
        }
        // The in-plane spin keeps resolving in parent space — exact there, and
        // needing no basis — while the two depth rings measure their angle in
        // the screen basis, the only place their plane is visible at all.
        GizmoHandle::RotateAxis(GizmoAxis::Z) => {
            if grab_r < 1e-6 {
                return base;
            }
            let a0 = (drag.grab_parent - origin).atan2();
            let a1 = (now_parent - origin).atan2();
            Resolved { rot: rot + (a1 - a0).to_degrees(), ..base }
        }
        GizmoHandle::RotateAxis(axis) => {
            let (Some(a0), Some(a1)) = (
                screen.ring_angle(axis, drag.origin_screen, drag.grab_screen),
                screen.ring_angle(axis, drag.origin_screen, screen.now),
            ) else {
                return base;
            };
            // Shortest way round, so dragging across the seam at the back of the
            // ring does not spin the layer a whole turn in one frame.
            let d = wrap_pi(a1 - a0).to_degrees();
            let (rx, ry) = base.rot_xy;
            let rot_xy = match axis {
                GizmoAxis::X => (rx + d, ry),
                _ => (rx, ry + d),
            };
            Resolved { rot_xy, ..base }
        }
        // Depth scale has no geometry to act on, so it is never offered and
        // never resolved — see the note in `hit`.
        GizmoHandle::ScaleAxis(GizmoAxis::Z) => base,
        GizmoHandle::ScaleAxis(axis) => {
            let a = axis_of(rot, axis);
            let d0 = (drag.grab_parent - origin).dot(a);
            let d1 = (now_parent - origin).dot(a);
            if d0.abs() < 1e-6 {
                return base;
            }
            let f = d1 / d0;
            let s = match axis {
                GizmoAxis::X => (scale.0 * f, scale.1),
                _ => (scale.0, scale.1 * f),
            };
            Resolved { scale: s, ..base }
        }
        GizmoHandle::ScaleUniform => {
            if grab_r < 1e-6 {
                return base;
            }
            let f = (now_parent - origin).hypot() / grab_r;
            Resolved { scale: (scale.0 * f, scale.1 * f), ..base }
        }
        // Move the pivot *without moving the layer* — After Effects' Pan Behind
        // tool. The layer is drawn at `pos + R·S·(q - anchor)` for each local
        // point `q`, so holding that fixed while the pivot follows the pointer
        // by `delta` needs both halves:
        //
        //     pos'    = pos + delta
        //     anchor' = anchor + (R·S)⁻¹ · delta
        //
        // Compensating only one of them is the classic version of this bug:
        // move just the anchor and the artwork jumps; move just the position
        // and the pivot doesn't go where you dropped it. Editing Anchor in the
        // properties panel deliberately does *not* compensate — there you are
        // asking to re-origin the layer, and it should move.
        GizmoHandle::Anchor => {
            let rs = Affine::rotate(rot.to_radians())
                * Affine::scale_non_uniform(scale.0, scale.1);
            // A collapsed scale makes `R·S` singular and its inverse infinite.
            // Nothing sensible can be computed, so hold rather than emit NaN
            // into the document.
            if scale.0.abs() < 1e-9 || scale.1.abs() < 1e-9 {
                return base;
            }
            let d = rs.inverse() * Point::new(delta.x, delta.y) - Point::ZERO;
            Resolved { pos: pos + delta, anchor: base.anchor + d, ..base }
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
        GizmoAxis::Z => Vec2::ZERO,
    }
}

/// Fold an angle into (-pi, pi] — the shortest way round.
fn wrap_pi(a: f64) -> f64 {
    use std::f64::consts::{PI, TAU};
    let a = (a + PI).rem_euclid(TAU) - PI;
    if a <= -PI {
        a + TAU
    } else {
        a
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
#[allow(clippy::too_many_arguments)]
pub(crate) fn gizmo_ui(
    ui: &mut egui::Ui,
    canvas: egui::Rect,
    target: &GizmoTarget,
    fit: Affine,
    ppp: f64,
    snap_ctx: SnapCtx<'_>,
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
            // Hit-test where the button went **down**. egui reports
            // `drag_started` only once the pointer has passed its drag
            // threshold, so the live position may already have slid off the
            // handle — testing it would silently drop the grab. (Note
            // `interact_pointer_pos()` is not this: it follows the ongoing
            // interaction rather than staying at the press.) The handles here
            // are large enough that this rarely bit, but it is the same bug
            // that made guides need a held click.
            let press = ui.ctx().input(|i| i.pointer.press_origin()).or(pointer);
            if let (Some(p), Some(handle)) = (press, press.and_then(|p| hit(&l, p))) {
                *drag = Some(GizmoDrag {
                    handle,
                    node: target.node,
                    start_pos: target.pos,
                    start_pos_z: target.pos_z,
                    start_rot: target.rot_deg,
                    start_rot_xy: target.rot_xy,
                    start_scale: target.scale,
                    start_anchor: target.anchor,
                    grab_parent: to_parent(target, fit, ppp, p),
                    grab_screen: p,
                    origin_screen: l.origin,
                });
            }
        }
        if resp.drag_stopped() {
            *drag = None;
        }
    }

    let mut snap = Snap::default();
    if let (Some(d), Some(p)) = (*drag, pointer) {
        // The basis is rebuilt from *this* frame's layout, so a depth drag stays
        // geared to where the layer is now rather than where it started — the
        // arrow keeps pointing at the vanishing point as the layer recedes.
        let screen = ScreenDrag { now: p, dir: l.dir, z_px: l.spatial };
        let r = resolve_drag(&d, to_parent(target, fit, ppp, p), screen);
        let (mut pos, rot, scale, anchor) = (r.pos, r.rot, r.scale, r.anchor);
        // Snapping applies to moves only. Rotating or scaling *to* a guide is a
        // different question with a different answer (an angle, not a point),
        // and pretending a position snap covers it would just make the handles
        // stick for no visible reason.
        if matches!(
            d.handle,
            GizmoHandle::Move | GizmoHandle::MoveAxis(GizmoAxis::X | GizmoAxis::Y)
        ) {
            let (snapped, s) = snap_move(target, &d, pos, snap_ctx, fit, ppp);
            pos = snapped;
            snap = s;
        }
        match d.handle {
            // Depth alone: writing x and y here too would fight the snapper,
            // which has nothing to say about an axis it cannot see.
            GizmoHandle::MoveAxis(GizmoAxis::Z) => out.pos_z = Some(r.pos_z),
            GizmoHandle::Move | GizmoHandle::MoveAxis(_) => {
                out.pos_x = Some(pos.x);
                out.pos_y = Some(pos.y);
            }
            GizmoHandle::RotateAxis(GizmoAxis::Z) => out.rot = Some(rot),
            GizmoHandle::RotateAxis(GizmoAxis::X) => out.rot_x = Some(r.rot_xy.0),
            GizmoHandle::RotateAxis(GizmoAxis::Y) => out.rot_y = Some(r.rot_xy.1),
            GizmoHandle::ScaleAxis(_) | GizmoHandle::ScaleUniform => {
                out.scale_x = Some(scale.0);
                out.scale_y = Some(scale.1);
            }
            // Both halves, always together — see `resolve_drag`. Emitting one
            // without the other is what makes the artwork jump.
            GizmoHandle::Anchor => {
                out.anchor_x = Some(anchor.x);
                out.anchor_y = Some(anchor.y);
                out.pos_x = Some(pos.x);
                out.pos_y = Some(pos.y);
            }
        }
    }

    let hot = drag.map(|d| d.handle).or(over);
    if hot.is_some() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
    }
    let painter = ui.painter_at(canvas);
    draw_snap(&painter, canvas, &snap, fit, ppp);
    paint(&painter, &l, hot);
    hot.is_some()
}

/// Apply snapping to a move, in composition space, and report what was hit.
///
/// The layer's values live in **parent** space but guides and the grid live in
/// **composition** space, so the point is taken up to comp space, snapped
/// there, and brought back. Snapping in parent space would silently mean
/// something different for every nested layer.
///
/// An axis-constrained move keeps its constraint: the correction is projected
/// onto the axis, so an arrow drag can slide *along* its axis onto a guide but
/// can never be pulled off it. Applying the raw 2D offset would quietly break
/// the one promise the arrow makes.
pub(crate) fn snap_move(
    target: &GizmoTarget,
    drag: &GizmoDrag,
    pos: Vec2,
    ctx: SnapCtx<'_>,
    fit: Affine,
    ppp: f64,
) -> (Vec2, Snap) {
    if !ctx.enabled {
        return (pos, Snap::default());
    }
    let pivot = target.parent * Point::new(pos.x, pos.y);
    // The cached bounds describe the layer where the *scene* last put it, which
    // during a drag is a frame behind. A move is a pure translation, so shifting
    // them by how far the pivot has travelled is exact — and far cheaper than
    // re-evaluating the comp for every drag frame just to re-measure a box.
    let here = target.parent * Point::new(target.pos.x, target.pos.y);
    let shift = pivot - here;
    let bounds = ctx.bounds.map(|b| b + shift);
    let world = SnapWorld { aids: ctx.aids, comp: ctx.comp, others: ctx.others };
    let snap = snap_point(pivot, bounds, world, snap_tolerance(fit, ppp));
    let mut offset = snap.offset();
    if offset == Vec2::ZERO {
        return (pos, snap);
    }
    if let GizmoHandle::MoveAxis(axis) = drag.handle {
        // The axis in *comp* space — the parent may rotate it.
        let a = axis_of(drag.start_rot, axis);
        let o = target.parent * Point::ZERO;
        let dir = (target.parent * Point::new(a.x, a.y)) - o;
        let len = dir.hypot();
        if len < 1e-9 {
            return (pos, Snap::default());
        }
        let unit = dir / len;
        offset = unit * offset.dot(unit);
    }
    // Back down to parent space as a *vector*: a difference of two mapped
    // points, so the parent's translation cancels and only its rotation and
    // scale apply.
    let inv = target.parent.inverse();
    let moved = inv * (pivot + offset);
    let base = inv * pivot;
    (pos + (moved - base), snap)
}

/// What a drag is allowed to snap to this frame.
#[derive(Clone, Copy)]
pub(crate) struct SnapCtx<'a> {
    pub(crate) aids: &'a ViewAids,
    pub(crate) comp: (f64, f64),
    /// The dragged layer's extent, as the scene last evaluated it. Translated
    /// to the proposed position before use — a move only translates, so that is
    /// exact and avoids re-evaluating the comp per drag frame.
    pub(crate) bounds: Option<kurbo::Rect>,
    /// Every other layer's extent, so edges can align against siblings.
    pub(crate) others: &'a [kurbo::Rect],
    /// Cleared while the bypass modifier is held, so precise placement is
    /// always one key away rather than a trip to a toggle.
    pub(crate) enabled: bool,
}

/// One bounding box per drawable item in the selection's subtree, in
/// **composition** space. Empty when the selection draws nothing.
///
/// Per item rather than one union around the whole subtree: a union tells you
/// only the extent of the group, which is the least informative thing about it.
/// Boxing each item shows what the group actually contains and where each piece
/// sits — and for a plain single-shape layer the two are identical anyway, so
/// nothing is lost in the common case.
///
/// Bounds are taken from each item's path through its world transform, so a
/// rotated layer yields the axis-aligned box of the rotated shape (what you
/// want for "how much room does this take up"), not a rotated rectangle.
pub(crate) fn selection_boxes(scene: &MScene, root: &MNode) -> Vec<kurbo::Rect> {
    let mut ids = Vec::new();
    collect_ids(root, &mut ids);
    scene
        .items
        .iter()
        .filter(|i| ids.contains(&i.source))
        .map(|i| (i.transform * i.path.clone()).bounding_box())
        .collect()
}

/// Ids that must not be offered as snap targets while `target` is dragged:
/// everything in its own subtree, plus every **ancestor** up to the root.
///
/// The ancestors matter and are easy to miss. A group's extent is the union of
/// its children's, so any ancestor's box *contains* the dragged layer and moves
/// with it — offering those edges would let a layer snap to a box it is itself
/// defining, which pins the drag against a target that runs away from it. The
/// root is an ancestor of everything, so this excludes it for free.
pub(crate) fn snap_excluded(root: &MNode, target: NodeId) -> Vec<NodeId> {
    let mut out = Vec::new();
    fn walk(node: &MNode, target: NodeId, out: &mut Vec<NodeId>) -> bool {
        if node.id == target {
            collect_ids(node, out);
            return true;
        }
        for c in &node.children {
            if walk(c, target, out) {
                out.push(node.id);
                return true;
            }
        }
        false
    }
    walk(root, target, &mut out);
    out
}

fn collect_ids(node: &MNode, out: &mut Vec<NodeId>) {
    out.push(node.id);
    for c in &node.children {
        collect_ids(c, out);
    }
}

const BBOX_COL: egui::Color32 = egui::Color32::from_rgba_premultiplied(210, 215, 230, 150);

/// Draw the selection's bounding box: a thin rectangle with corner ticks, so it
/// reads as a measurement rather than as another draggable frame. It is
/// deliberately *not* grabbable — resizing by bbox corner would fight the scale
/// handles, which already own that gesture.
pub(crate) fn draw_bounds(
    painter: &egui::Painter,
    bounds: kurbo::Rect,
    fit: Affine,
    ppp: f64,
) {
    let a = fit * Point::new(bounds.x0, bounds.y0);
    let b = fit * Point::new(bounds.x1, bounds.y1);
    let r = egui::Rect::from_min_max(
        egui::pos2((a.x.min(b.x) / ppp) as f32, (a.y.min(b.y) / ppp) as f32),
        egui::pos2((a.x.max(b.x) / ppp) as f32, (a.y.max(b.y) / ppp) as f32),
    );
    let stroke = egui::Stroke::new(1.0, BBOX_COL);
    painter.rect_stroke(r, 0.0, stroke, egui::StrokeKind::Middle);

    // Corner ticks, clamped so they never overlap on a tiny selection.
    let t = (r.width().min(r.height()) * 0.25).clamp(2.0, 10.0);
    for (c, sx, sy) in [
        (r.left_top(), 1.0, 1.0),
        (r.right_top(), -1.0, 1.0),
        (r.left_bottom(), 1.0, -1.0),
        (r.right_bottom(), -1.0, -1.0),
    ] {
        painter.line_segment([c, c + egui::vec2(t * sx, 0.0)], egui::Stroke::new(1.6, BBOX_COL));
        painter.line_segment([c, c + egui::vec2(0.0, t * sy)], egui::Stroke::new(1.6, BBOX_COL));
    }
}
