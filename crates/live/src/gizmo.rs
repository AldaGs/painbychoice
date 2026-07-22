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

use motion_core::{Mat4, Vec3 as MVec3};

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

/// The layer's rotation as a matrix, composed **exactly as
/// `Transform::resolve` composes it**: Z, then Y, then X. If that order ever
/// changes there, it must change here, or the gizmo will draw a frame the
/// renderer does not use.
fn rot_matrix(rot_z: f64, rot_xy: (f64, f64)) -> Mat4 {
    Mat4::rotate_z(rot_z.to_radians())
        * Mat4::rotate_y(rot_xy.1.to_radians())
        * Mat4::rotate_x(rot_xy.0.to_radians())
}

/// Column `axis` of a rotation matrix — the direction that axis points after
/// the rotation, as a unit vector in the parent's space.
fn axis_column(m: &Mat4, axis: GizmoAxis) -> MVec3 {
    let i = axis as usize * 4;
    MVec3::new(m.0[i], m.0[i + 1], m.0[i + 2])
}

/// The frame a rotation about `axis` actually turns in — **the gimbal**.
///
/// Euler angles are not three independent rotations; they are a nested chain.
/// `Transform::resolve` applies Z, then Y, then X, so the Z ring turns in the
/// parent's own frame, the Y ring turns inside whatever Z has already done, and
/// the X ring turns inside both. Drawing three fixed circles would be a lie
/// about which numbers a drag changes — and would hide gimbal lock, which is a
/// real state of this rotation model that the user needs to be able to see: as
/// the rings fold onto one another, two of the three stop being independent.
fn gimbal_frame(axis: GizmoAxis, rot_z: f64, rot_xy: (f64, f64)) -> Mat4 {
    match axis {
        // Outermost: turns in the parent's frame, untouched by the other two.
        GizmoAxis::Z => Mat4::IDENTITY,
        // Inside Z.
        GizmoAxis::Y => Mat4::rotate_z(rot_z.to_radians()),
        // Innermost: inside both.
        GizmoAxis::X => {
            Mat4::rotate_z(rot_z.to_radians()) * Mat4::rotate_y(rot_xy.1.to_radians())
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
    /// Parent space → composition space, **unprojected and three-dimensional**.
    ///
    /// This is the real chain, straight from `Placement`, not a 2D shadow of it.
    /// The gizmo used to recover a `parent` affine by dividing the layer's own
    /// local matrix out of its *projected, flattened* world matrix — which for a
    /// tipped layer divided out of a matrix that had already thrown the
    /// foreshortening away, so every direction it derived was wrong by however
    /// much the layer was turned. Handles wandered, and a rotation ring could
    /// jump.
    pub(crate) parent_xf: motion_core::mat4::Xf,
    /// Parent space → comp space, flattened. Retained **only** for snapping,
    /// which reasons about guides in the composition plane.
    pub(crate) parent: Affine,
    pub(crate) pos: Vec2,
    pub(crate) pos_z: f64,
    pub(crate) rot_deg: f64,
    /// The out-of-plane rotations, in degrees.
    pub(crate) rot_xy: (f64, f64),
    pub(crate) scale: (f64, f64),
    /// Needed to *recover* `parent`, and now to drag the anchor itself.
    pub(crate) anchor: Vec2,
    /// The composition's camera as the *viewer* sees it — eye, distance and
    /// orbit — or `None` in a flat comp.
    ///
    /// **This gates the depth handles.** Without a camera there is no
    /// projection, so depth has no direction on screen and a Z arrow would be a
    /// control you could drag with nothing happening. The gizmo shows exactly
    /// the axes the composition can actually express.
    ///
    /// It is the *same* projector the frame was drawn through, orbit included.
    /// Anything less and the handles would describe a view nobody is looking
    /// at — which is precisely the class of bug that made the rings collapse
    /// into one circle.
    pub(crate) view: Option<motion_core::Projector>,
}

impl GizmoTarget {
    pub(crate) fn new(
        node: u64,
        parent_xf: motion_core::mat4::Xf,
        info: &NodeInfo,
        view: Option<motion_core::Projector>,
    ) -> Self {
        let pos = Vec2::new(info.pos.0, info.pos.1);
        let anchor = Vec2::new(info.anchor.0, info.anchor.1);
        Self {
            node,
            parent_xf,
            parent: parent_xf.to_affine_lossy(),
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

    /// The layer's own axes as unit vectors in parent space, depth included.
    ///
    /// Scale is deliberately *not* folded in: the arrows show orientation, and
    /// a squashed layer should not get squashed handles. Rotation **is**, all
    /// three of it — an arrow that ignored the layer's tilt would point
    /// somewhere the layer does not go.
    pub(crate) fn axis_parent(&self, axis: GizmoAxis) -> MVec3 {
        axis_column(&rot_matrix(self.rot_deg, self.rot_xy), axis)
    }

    /// Map a point in the layer's parent space — **depth included** — to
    /// logical screen points, through the same projection the frame was drawn
    /// with.
    ///
    /// `parent` already carries the camera's scale for the layer's own depth,
    /// so a point at a *different* depth has to have that scale divided back
    /// out and the right one reapplied. Without a camera there is nothing to
    /// undo and depth simply does not move the point, which is the honest
    /// picture of an orthographic comp.
    /// The exact way back — the inverse of [`Self::to_screen3`]: a pointer
    /// position to a point in the layer's parent space, by **intersecting the
    /// view ray with the layer's own plane**.
    ///
    /// A screen point names a ray, not a point — every depth along it projects
    /// to the same pixel — so the answer is only unique once you say which
    /// surface to land on. The layer's own plane (`z = pos_z` in parent space)
    /// is the right one: it is the plane a move slides in, so a grab lands
    /// exactly where the pointer is and the layer neither jumps toward the eye
    /// nor away from it.
    ///
    /// Doing this as a ray, rather than unprojecting onto a single depth, is
    /// what makes it exact under a **tipped parent**. There, parent-space points
    /// sharing a `z` sit at *different* composition depths, so no one depth
    /// could stand in for the plane — the shortcut round-trips the origin and
    /// drifts everywhere else, which is the sort of error that reads as a gizmo
    /// slipping under the cursor the further you drag.
    pub(crate) fn unproject(self, fit: Affine, ppp: f64, p: egui::Pos2) -> Point {
        let phys = Point::new(p.x as f64 * ppp, p.y as f64 * ppp);
        // Where this pixel crosses the composition's zero plane. A point at
        // `z = 0` projects to itself, so this is a point on the ray.
        let at_zero = fit.inverse() * phys;

        // The ray, in composition space. With a camera it starts at the eye —
        // which sits `distance` in *front* of the zero plane, hence the
        // negative z — and runs through `at_zero`. Without one the projection
        // is orthographic and the ray is simply the depth axis.
        let (origin, dir) = match self.view {
            Some(v) => (
                MVec3::new(v.eye.x, v.eye.y, -v.distance),
                MVec3::new(at_zero.x - v.eye.x, at_zero.y - v.eye.y, v.distance),
            ),
            None => (
                MVec3::new(at_zero.x, at_zero.y, 0.0),
                MVec3::new(0.0, 0.0, 1.0),
            ),
        };

        // The ray lives in *viewed* composition space, so the chain to undo is
        // the whole forward one: the parent, then the orbit. Inverting their
        // product does both at once and cannot get the order wrong.
        let m = self.view.map(|v| v.view()).unwrap_or(motion_core::Mat4::IDENTITY)
            * self.parent_xf.to_mat4();
        let Some(inv) = m.inverse_affine() else {
            // A collapsed scale somewhere above has no inverse. Holding the
            // grab still beats writing a NaN position into the document.
            return Point::new(self.pos.x, self.pos.y);
        };
        // Into parent space: the origin as a point, the direction as a
        // direction — translating a direction would aim the ray at nothing.
        let o = inv.transform_point(origin);
        let d = inv.transform_dir(dir);

        // Intersect with the plane the layer lives in. A ray parallel to it
        // never meets it: that is the layer seen exactly edge-on, where the
        // pointer genuinely does not name a point on it.
        if d.z.abs() < 1e-9 {
            return Point::new(self.pos.x, self.pos.y);
        }
        let t = (self.pos_z - o.z) / d.z;
        Point::new(o.x + d.x * t, o.y + d.y * t)
    }

    pub(crate) fn to_screen3(self, fit: Affine, ppp: f64, d: MVec3) -> egui::Pos2 {
        let p_parent = MVec3::new(self.pos.x + d.x, self.pos.y + d.y, self.pos_z + d.z);
        let p_comp = self.parent_xf.to_mat4().transform_point(p_parent);
        let comp = match self.view {
            Some(v) => {
                // The orbit first — it is a change of viewpoint and belongs
                // ahead of the divide — then the perspective itself.
                let q = v.view().transform_point(p_comp);
                let s = v.distance / (v.distance + q.z);
                if !s.is_finite() || s.abs() < 1e-9 {
                    Point::new(q.x, q.y)
                } else {
                    Point::new(v.eye.x + (q.x - v.eye.x) * s, v.eye.y + (q.y - v.eye.y) * s)
                }
            }
            None => Point::new(p_comp.x, p_comp.y),
        };
        let c = fit * comp;
        egui::pos2((c.x / ppp) as f32, (c.y / ppp) as f32)
    }

}

/// The axis each rotation ring actually turns about, in the layer's parent
/// space — the gimbal, made inspectable so a test can assert its nesting
/// rather than a picture of it.
#[cfg(test)]
pub(crate) fn gimbal_axes(rot_z: f64, rot_xy: (f64, f64)) -> [(f64, f64, f64); 3] {
    let mut out = [(0.0, 0.0, 0.0); 3];
    for a in [GizmoAxis::X, GizmoAxis::Y, GizmoAxis::Z] {
        let v = axis_column(&gimbal_frame(a, rot_z, rot_xy), a);
        out[a as usize] = (v.x, v.y, v.z);
    }
    out
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
    /// Screen travel per **unit** of movement along each of the layer's own
    /// axes — direction and gearing in one vector. Not normalised: the length
    /// is what a drag divides by to get back to units, and an axis pointing
    /// away from the viewer legitimately has a short one.
    axis: [egui::Vec2; 3],
    /// The same three, normalised, for drawing.
    dir: [egui::Vec2; 3],
    /// Each rotation ring's screen basis, in **gimbal** order — see
    /// [`gimbal_frame`].
    ///
    /// `None` where the ring is genuinely edge-on: its plane contains the
    /// viewing direction, so it projects to a *line*, not an ellipse. Such a
    /// ring is neither drawn nor grabbable, because the alternative — falling
    /// back to a circle in the screen plane — draws three identical circles
    /// stacked on one another and invites you to drag a rotation that is not
    /// the one you think. Orbit the view and it opens up.
    ring: [Option<(egui::Vec2, egui::Vec2)>; 3],
    /// Whether the depth handles are live: a camera exists and the layer is not
    /// sitting on the optical axis, where receding moves it nowhere and a drag
    /// would have no gearing to invert.
    spatial: bool,
}

impl Layout {
    fn tip(&self, axis: GizmoAxis, at: f32) -> egui::Pos2 {
        self.origin + self.dir[axis as usize] * at
    }
    /// The axes with handles: X and Y always, Z only where depth reads.
    fn axes(&self) -> &'static [GizmoAxis] {
        if self.spatial {
            &[GizmoAxis::X, GizmoAxis::Y, GizmoAxis::Z]
        } else {
            &[GizmoAxis::X, GizmoAxis::Y]
        }
    }
    /// A point on the ring that turns about `axis`, at parameter `t` radians.
    fn ring_point(&self, axis: GizmoAxis, t: f32) -> Option<egui::Pos2> {
        let (u, v) = self.ring[axis as usize]?;
        Some(self.origin + u * (RING_R * t.cos()) + v * (RING_R * t.sin()))
    }
    /// The screen basis a drag measures its ring angle in.
    fn screen(&self, now: egui::Pos2) -> ScreenDrag {
        ScreenDrag { now, axis: self.axis, ring: self.ring }
    }
    /// Whether this ring can be seen, and so grabbed, from here.
    fn ring_visible(&self, axis: GizmoAxis) -> bool {
        self.ring[axis as usize].is_some()
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

fn layout(t: &GizmoTarget, fit: Affine, ppp: f64) -> Layout {
    // The origin goes through the **same** map as every direction below it. It
    // used to take a flattened shortcut, which put the handles a perspective
    // factor away from the axes they were supposed to start at.
    let origin = t.to_screen3(fit, ppp, MVec3::ZERO);
    // Every direction is *measured*: step one unit along it in parent space,
    // ask where that lands on screen, and take the difference. One rule serves
    // the in-plane axes, depth, and the ring bases alike, and it goes through
    // the very projection the frame was drawn with — so an arrow can never
    // point somewhere the layer would not actually go. Deriving any of it in
    // closed form would be a second copy of the camera's arithmetic, free to
    // drift from the first.
    let step = |d: MVec3| t.to_screen3(fit, ppp, d) - origin;

    let mut axis = [egui::Vec2::ZERO; 3];
    let mut dir = [egui::Vec2::X, egui::Vec2::Y, egui::Vec2::ZERO];
    for a in [GizmoAxis::X, GizmoAxis::Y, GizmoAxis::Z] {
        let i = a as usize;
        axis[i] = step(t.axis_parent(a));
        if axis[i].length() > 1e-4 {
            dir[i] = axis[i].normalized();
        }
    }

    // The gimbal. Each ring is drawn in the frame its own rotation turns in,
    // not in the layer's final orientation — so the Z ring stays put while the
    // inner two follow what the outer ones have already done, and the rings
    // fold together exactly when the Euler angles stop being independent.
    let mut ring = [None; 3];
    for a in [GizmoAxis::X, GizmoAxis::Y, GizmoAxis::Z] {
        let frame = gimbal_frame(a, t.rot_deg, t.rot_xy);
        let (u, v) = a.ring_basis();
        let du = step(axis_column(&frame, u));
        let dv = step(axis_column(&frame, v));
        // Both spanning directions must read on screen. If either does not, the
        // ring is edge-on and there is no ellipse to draw — say so with `None`
        // rather than inventing one.
        if du.length() > 1e-4 && dv.length() > 1e-4 {
            ring[a as usize] = Some((du.normalized(), dv.normalized()));
        }
    }

    // Depth handles need a camera *and* somewhere to go: a layer sitting on the
    // optical axis recedes without moving on screen, so a drag there would have
    // no gearing to invert and is better withheld than made infinitely
    // sensitive.
    let spatial = t.view.is_some() && step(MVec3::new(0.0, 0.0, 1.0)).length() > 1e-5;
    Layout { origin, axis, dir, ring, spatial }
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
        if !l.ring_visible(axis) {
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
    let Some(mut prev) = l.ring_point(axis, 0.0) else {
        // Edge-on: not on screen, so not grabbable.
        return f32::MAX;
    };
    let mut best = f32::MAX;
    for i in 1..=RING_SAMPLES {
        let t = i as f32 / RING_SAMPLES as f32 * std::f32::consts::TAU;
        let Some(next) = l.ring_point(axis, t) else { return f32::MAX };
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
        let pts: Option<Vec<egui::Pos2>> = (0..=RING_SAMPLES)
            .map(|i| l.ring_point(axis, i as f32 / RING_SAMPLES as f32 * std::f32::consts::TAU))
            .collect();
        // An edge-on ring simply is not there to draw.
        if let Some(pts) = pts {
            painter.add(egui::Shape::line(pts, egui::Stroke::new(1.5, c)));
        }
    }

    // The depth arrow, toward the vanishing point. Drawn before the in-plane
    // pair so those stay on top where they overlap — they are the ones you
    // reach for most.
    if l.spatial {
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
    /// Screen travel per unit of movement along each of the layer's own axes.
    /// Direction and gearing together — the length is what a drag divides by.
    pub(crate) axis: [egui::Vec2; 3],
    /// Each rotation ring's screen basis, in gimbal order. `None` where the
    /// ring is edge-on and no angle about it can be read.
    pub(crate) ring: [Option<(egui::Vec2, egui::Vec2)>; 3],
}

impl ScreenDrag {
    /// The basis of an unrotated layer at 1:1 zoom in a flat comp — the
    /// identity case, and what the in-plane tests measure through.
    #[cfg(test)]
    pub(crate) fn flat() -> Self {
        Self {
            now: egui::Pos2::ZERO,
            axis: [egui::Vec2::X, egui::Vec2::Y, egui::Vec2::ZERO],
            ring: [Some((egui::Vec2::X, egui::Vec2::Y)); 3],
        }
    }

    /// The angle of `p` about the ring that turns around `axis`, in that ring's
    /// own basis. `None` where the ring has collapsed to a line on screen and an
    /// angle around it would mean nothing — the gimbal seen edge-on.
    fn ring_angle(&self, axis: GizmoAxis, origin: egui::Pos2, p: egui::Pos2) -> Option<f64> {
        let (du, dv) = self.ring[axis as usize]?;
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
        // Every arrow, in-plane or not, resolves the same way: project the
        // screen drag onto that axis' screen step and divide by its length to
        // get units. One rule, and it matches the drawn arrow by construction —
        // the same vector was used to draw it.
        //
        // A drag of N points therefore moves the layer however far actually
        // reads as N points from here, so a distant layer travels further per
        // point than a near one. That is what keeps a handle feeling attached
        // to the artwork rather than to the number behind it.
        //
        // A tilted axis moves the layer in **depth as well**: its unit vector
        // has a z component, and pretending otherwise would slide the layer off
        // its own axis the moment it left the plane.
        GizmoHandle::MoveAxis(axis) => {
            let v = screen.axis[axis as usize];
            let len2 = v.length_sq() as f64;
            if len2 < 1e-10 {
                // The axis points straight at the viewer: no screen travel can
                // mean movement along it.
                return base;
            }
            let along = (screen.now - drag.grab_screen).dot(v) as f64 / len2;
            let a = axis_column(&rot_matrix(rot, base.rot_xy), axis);
            Resolved {
                pos: pos + Vec2::new(a.x, a.y) * along,
                pos_z: base.pos_z + a.z * along,
                ..base
            }
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
                    grab_parent: target.unproject(fit, ppp, p),
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
        let screen = l.screen(p);
        let r = resolve_drag(&d, target.unproject(fit, ppp, p), screen);
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
