//! The Bézier pen tool: draw and edit vector paths directly on the canvas.
//!
//! Like the gizmo (see [`crate::gizmo`]) it is **painted with egui** over the
//! finished vello frame and reports its result as an ordinary edit struct, so it
//! never becomes a second write path into the document. Two things keep it
//! tractable:
//!
//! * It works entirely in **resolved geometry** — [`PathSample`]s, tangents laid
//!   out at this frame — and never touches a `Value`. So an animated point is
//!   dragged on canvas exactly as a static one is, and the messy question of
//!   "keyframe or overwrite?" lives in one place: the apply phase
//!   ([`crate::App::apply_pen_edits`]), which merges the emitted all-`Const` path
//!   into the layer's shape, keying a point that is already animated and
//!   replacing one that is not.
//! * Three coordinate spaces, named at every edge: the layer's **local** space
//!   (where anchors are stored), **composition** space, and egui **logical
//!   points**. `m = fit * world` carries local all the way to physical pixels;
//!   dividing by `ppp` lands in logical points.

use crate::*;
use kurbo::{Affine, Point, Vec2};
use motion_core::{PathPart, PathSample, VectorPath};

/// Which canvas tool is active. Modal, unlike the gizmo's per-drag handles: the
/// pen stays armed so you can place a whole path click by click.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Tool {
    #[default]
    Select,
    /// Draw: each canvas click appends an anchor; click the first to close.
    Pen,
    /// Edit points: move anchors and tangents, insert on a segment, delete —
    /// but never append on empty canvas. Illustrator's direct-selection tool.
    EditPath,
}

impl Tool {
    /// The two path tools share `pen_ui`; this is which behaviour it runs.
    pub(crate) fn pen_mode(self) -> Option<PenMode> {
        match self {
            Tool::Pen => Some(PenMode::Draw),
            Tool::EditPath => Some(PenMode::Edit),
            Tool::Select => None,
        }
    }
}

/// What a canvas press does over empty space, the one thing the two path tools
/// differ on: Draw appends a new anchor, Edit does nothing.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum PenMode {
    Draw,
    Edit,
}

/// Everything the pen needs about the vector layer it is editing, gathered
/// before the UI pass like [`crate::gizmo::GizmoTarget`].
pub(crate) struct PenTarget {
    pub(crate) node: NodeId,
    /// Layer-local → composition space.
    pub(crate) world: Affine,
    /// The path's anchors resolved at this frame, tangents relative.
    pub(crate) samples: Vec<PathSample>,
    pub(crate) closed: bool,
}

/// What the pen reports back after a frame.
#[derive(Default)]
pub(crate) struct PenEdits {
    /// A whole new path (all-`Const`) to merge into the layer's shape.
    pub(crate) set_path: Option<(NodeId, VectorPath)>,
}

/// A control point being dragged, held on `App` across frames like `gizmo_drag`.
#[derive(Clone, Copy)]
pub(crate) struct PenDrag {
    node: NodeId,
    index: usize,
    grab: Grab,
}

/// What a press grabbed. `NewAnchor` is a just-placed point whose drag pulls
/// mirrored tangents (the classic pen gesture); the others edit an existing one.
#[derive(Clone, Copy, PartialEq)]
enum Grab {
    Point,
    In,
    Out,
    NewAnchor,
    /// Alt-dragging a corner anchor pulls symmetric tangents out of it,
    /// converting it to a smooth point — the drag math is [`Grab::NewAnchor`]'s.
    Pull,
}

/// Hit radius for an anchor or a handle, in logical points.
const GRAB_R: f32 = 7.0;

/// Land an emitted all-`Const` path onto an existing (possibly animated) one.
///
/// The window-free heart of [`crate::App::apply_pen_edits`], split out so it can
/// be unit-tested without a running editor — the same discipline as
/// `compile_drivers` / `import_shape`. A **structural** change (anchor count or
/// closedness differs) replaces the path wholesale; a same-topology change is a
/// per-point **move**, written through each `Value` so an animated point
/// auto-keys at `frame` and a static one stays constant.
pub(crate) fn merge_path(existing: &mut VectorPath, new_path: &VectorPath, frame: i64) {
    if existing.anchors.len() != new_path.anchors.len() || existing.closed != new_path.closed {
        *existing = new_path.clone();
        return;
    }
    for (i, a) in new_path.anchors.iter().enumerate() {
        for part in PathPart::ALL {
            if let Value::Const(v) = a.part(part) {
                if let Some(dst) = existing.value_mut(i, part) {
                    dst.set_at(frame, *v);
                }
            }
        }
    }
}

/// Build an all-`Const` [`VectorPath`] from resolved samples — what the pen
/// emits. The apply phase decides how it lands on the (possibly animated) shape.
fn path_from(samples: &[PathSample], closed: bool) -> VectorPath {
    VectorPath {
        anchors: samples
            .iter()
            .map(|s| motion_core::Anchor {
                point: Value::Const(s.point),
                in_tan: Value::Const(s.in_tan),
                out_tan: Value::Const(s.out_tan),
            })
            .collect(),
        closed,
    }
}

/// The pen tool's per-frame pass. Returns whether it owns the pointer (so canvas
/// picking stays suppressed while a path is being drawn).
#[allow(clippy::too_many_arguments)]
pub(crate) fn pen_ui(
    ui: &mut egui::Ui,
    canvas: egui::Rect,
    target: &PenTarget,
    fit: Affine,
    ppp: f64,
    mode: PenMode,
    drag: &mut Option<PenDrag>,
    out: &mut PenEdits,
) -> bool {
    let m = fit * target.world;
    let Some(inv) = invert(m) else {
        // A collapsed layer scale has no inverse — there is no sensible local
        // point under the pointer, so the pen does nothing this frame.
        return true;
    };
    let to_screen = |p: Vec2| {
        let q = m * Point::new(p.x, p.y);
        egui::pos2((q.x / ppp) as f32, (q.y / ppp) as f32)
    };
    let to_local = |pos: egui::Pos2| {
        let q = inv * Point::new(pos.x as f64 * ppp, pos.y as f64 * ppp);
        Vec2::new(q.x, q.y)
    };

    // A selection change mid-drag drops it: the snapshot describes another layer.
    if drag.is_some_and(|d| d.node != target.node) {
        *drag = None;
    }

    let mut samples = target.samples.clone();
    let mut closed = target.closed;
    let mut changed = false;

    // Reserve the canvas so egui gives us hover/cursor and marks the area used.
    let resp = ui.interact(canvas, ui.id().with("pen"), egui::Sense::click_and_drag());
    let pointer = ui.ctx().pointer_latest_pos();
    let (pressed, released, down, alt, delete) = ui.input(|i| {
        (
            i.pointer.primary_pressed(),
            i.pointer.primary_released(),
            i.pointer.primary_down(),
            i.modifiers.alt,
            i.key_pressed(egui::Key::Delete) || i.key_pressed(egui::Key::Backspace),
        )
    });

    // --- Press: grab an existing control point, or (mode-specific) close /
    //     append / insert. ---
    if pressed && resp.hovered() {
        let press = ui.ctx().input(|i| i.pointer.press_origin()).or(pointer);
        if let Some(p) = press {
            if let Some((index, grab)) = hit(&samples, p, &to_screen) {
                // Alt-dragging an anchor point pulls symmetric tangents out of a
                // corner (or re-smooths one), the standard convert gesture.
                // Removal is the Delete key, so Alt here is unambiguous.
                let grab = if alt && grab == Grab::Point && mode == PenMode::Edit {
                    Grab::Pull
                } else {
                    grab
                };
                *drag = Some(PenDrag { node: target.node, index, grab });
            } else if mode == PenMode::Draw
                && !closed
                && samples.len() >= 2
                && to_screen(samples[0].point).distance(p) <= GRAB_R * 1.5
            {
                // Draw mode: clicking the first anchor closes the contour.
                closed = true;
                changed = true;
            } else if mode == PenMode::Draw {
                // Draw mode: append a fresh corner anchor; a drag pulls tangents.
                samples.push(PathSample { point: to_local(p), in_tan: Vec2::ZERO, out_tan: Vec2::ZERO });
                *drag = Some(PenDrag { node: target.node, index: samples.len() - 1, grab: Grab::NewAnchor });
                changed = true;
            } else if let Some((seg, t)) = nearest_segment(&samples, closed, p, &to_screen) {
                // Edit mode: click on a segment inserts a point there, splitting
                // the curve so its shape is preserved, then drags the new point.
                let index = insert_on_segment(&mut samples, seg, t);
                *drag = Some(PenDrag { node: target.node, index, grab: Grab::Point });
                changed = true;
            }
        }
    }

    // --- Delete key: remove the anchor being dragged, else the hovered one. ---
    if delete && mode == PenMode::Edit {
        let victim = drag
            .map(|d| d.index)
            .or_else(|| pointer.and_then(|p| hit(&samples, p, &to_screen)).map(|(i, _)| i));
        if let Some(i) = victim.filter(|&i| i < samples.len()) {
            samples.remove(i);
            *drag = None;
            changed = true;
        }
    }

    // --- Drag: move the grabbed control point / pull the grabbed tangent. ---
    if down {
        if let (Some(d), Some(p)) = (*drag, pointer) {
            if let Some(s) = samples.get_mut(d.index) {
                let lp = to_local(p);
                match d.grab {
                    Grab::Point => s.point = lp,
                    // A new anchor's drag — or pulling handles out of a corner —
                    // sets a smooth (mirrored) tangent.
                    Grab::NewAnchor | Grab::Pull => {
                        let t = lp - s.point;
                        s.out_tan = t;
                        s.in_tan = -t;
                    }
                    // Editing an existing handle keeps the point smooth (mirrors
                    // the opposite tangent) unless Alt breaks it — the standard
                    // pen behaviour.
                    Grab::Out => {
                        let t = lp - s.point;
                        s.out_tan = t;
                        if !alt {
                            s.in_tan = -t;
                        }
                    }
                    Grab::In => {
                        let t = lp - s.point;
                        s.in_tan = t;
                        if !alt {
                            s.out_tan = -t;
                        }
                    }
                }
                changed = true;
            }
        }
    }

    if released {
        *drag = None;
    }

    if changed {
        out.set_path = Some((target.node, path_from(&samples, closed)));
    }

    // --- Paint: segments, then handles, then anchors on top. ---
    let painter = ui.painter_at(canvas);
    paint(&painter, &samples, closed, drag.map(|d| (d.index, d.grab)), &to_screen);
    if pointer.is_some_and(|p| canvas.contains(p)) {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
    }
    true
}

/// Which anchor / handle the press landed on, handles taking priority (they sit
/// on top and are smaller). Only the last anchor exposes tangent handles while
/// drawing — matching what [`paint`] draws grabbable.
fn hit(samples: &[PathSample], p: egui::Pos2, to_screen: &impl Fn(Vec2) -> egui::Pos2) -> Option<(usize, Grab)> {
    for (i, s) in samples.iter().enumerate() {
        let inh = to_screen(s.point + s.in_tan);
        let outh = to_screen(s.point + s.out_tan);
        if s.in_tan != Vec2::ZERO && inh.distance(p) <= GRAB_R {
            return Some((i, Grab::In));
        }
        if s.out_tan != Vec2::ZERO && outh.distance(p) <= GRAB_R {
            return Some((i, Grab::Out));
        }
    }
    for (i, s) in samples.iter().enumerate() {
        if to_screen(s.point).distance(p) <= GRAB_R {
            return Some((i, Grab::Point));
        }
    }
    None
}

const ANCHOR_COL: egui::Color32 = egui::Color32::from_rgb(90, 170, 255);
const HANDLE_COL: egui::Color32 = egui::Color32::from_rgb(200, 200, 210);

fn paint(
    painter: &egui::Painter,
    samples: &[PathSample],
    closed: bool,
    active: Option<(usize, Grab)>,
    to_screen: &impl Fn(Vec2) -> egui::Pos2,
) {
    // Segments as cubic polylines, so the drawn path matches what will render.
    let seg = |a: &PathSample, b: &PathSample| {
        let p0 = to_screen(a.point);
        let p1 = to_screen(a.point + a.out_tan);
        let p2 = to_screen(b.point + b.in_tan);
        let p3 = to_screen(b.point);
        cubic_polyline(p0, p1, p2, p3)
    };
    let stroke = egui::Stroke::new(1.6, ANCHOR_COL);
    for w in samples.windows(2) {
        painter.add(egui::Shape::line(seg(&w[0], &w[1]), stroke));
    }
    if closed && samples.len() > 1 {
        painter.add(egui::Shape::line(
            seg(samples.last().unwrap(), &samples[0]),
            stroke,
        ));
    }
    // Handles + anchors.
    for (i, s) in samples.iter().enumerate() {
        let a = to_screen(s.point);
        for (tan, grab) in [(s.in_tan, Grab::In), (s.out_tan, Grab::Out)] {
            if tan == Vec2::ZERO {
                continue;
            }
            let h = to_screen(s.point + tan);
            painter.line_segment([a, h], egui::Stroke::new(1.0, HANDLE_COL));
            let hot = active == Some((i, grab));
            painter.circle_filled(h, if hot { 4.5 } else { 3.5 }, HANDLE_COL);
        }
        let hot = active == Some((i, Grab::Point))
            || active == Some((i, Grab::NewAnchor))
            || active == Some((i, Grab::Pull));
        let r = if hot { 5.0 } else { 4.0 };
        painter.rect_filled(egui::Rect::from_center_size(a, egui::Vec2::splat(r)), 1.0, ANCHOR_COL);
    }
}

/// A cubic Bézier evaluated at `t`.
fn cubic_at(p0: egui::Pos2, p1: egui::Pos2, p2: egui::Pos2, p3: egui::Pos2, t: f32) -> egui::Pos2 {
    let u = 1.0 - t;
    let (a, b, c, d) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
    egui::pos2(
        a * p0.x + b * p1.x + c * p2.x + d * p3.x,
        a * p0.y + b * p1.y + c * p2.y + d * p3.y,
    )
}

/// Flatten a cubic to a screen polyline. Coarse (12 steps) is plenty for an
/// overlay redrawn every frame.
fn cubic_polyline(p0: egui::Pos2, p1: egui::Pos2, p2: egui::Pos2, p3: egui::Pos2) -> Vec<egui::Pos2> {
    (0..=12).map(|i| cubic_at(p0, p1, p2, p3, i as f32 / 12.0)).collect()
}

/// The segment nearest the pointer and the parameter along it, if within grab
/// range — where an Edit-mode click inserts a new point. Segments are indexed by
/// their start anchor; the closing segment (of a closed path) is index `n-1`.
fn nearest_segment(
    samples: &[PathSample],
    closed: bool,
    p: egui::Pos2,
    to_screen: &impl Fn(Vec2) -> egui::Pos2,
) -> Option<(usize, f64)> {
    let n = samples.len();
    if n < 2 {
        return None;
    }
    const STEPS: usize = 16;
    let seg_count = if closed { n } else { n - 1 };
    let mut best: Option<(usize, f64, f32)> = None;
    for i in 0..seg_count {
        let a = &samples[i];
        let b = &samples[(i + 1) % n];
        let (p0, p1) = (to_screen(a.point), to_screen(a.point + a.out_tan));
        let (p2, p3) = (to_screen(b.point + b.in_tan), to_screen(b.point));
        // Skip the endpoints — those are the anchors themselves.
        for s in 1..STEPS {
            let t = s as f32 / STEPS as f32;
            let d = cubic_at(p0, p1, p2, p3, t).distance(p);
            if best.is_none_or(|(_, _, bd)| d < bd) {
                best = Some((i, t as f64, d));
            }
        }
    }
    best.filter(|&(_, _, d)| d <= GRAB_R * 1.5).map(|(i, t, _)| (i, t))
}

/// Split segment `seg` at parameter `t` with de Casteljau, inserting a new anchor
/// **without changing the curve's shape** (the two halves reproduce the original
/// cubic). Returns the new anchor's index.
fn insert_on_segment(samples: &mut Vec<PathSample>, seg: usize, t: f64) -> usize {
    let n = samples.len();
    let next = (seg + 1) % n;
    let (p0, p3) = (samples[seg].point, samples[next].point);
    let p1 = p0 + samples[seg].out_tan;
    let p2 = p3 + samples[next].in_tan;
    let l = |u: Vec2, v: Vec2| u + (v - u) * t;
    let (p01, p12, p23) = (l(p0, p1), l(p1, p2), l(p2, p3));
    let (p012, p123) = (l(p01, p12), l(p12, p23));
    let mid = l(p012, p123);
    samples[seg].out_tan = p01 - p0;
    samples[next].in_tan = p23 - p3;
    let new = PathSample { point: mid, in_tan: p012 - mid, out_tan: p123 - mid };
    // A point inserted on the closing segment lands at the end of the list.
    let idx = if next == 0 { samples.len() } else { next };
    samples.insert(idx, new);
    idx
}

/// The inverse of an affine, or `None` if it is singular (a collapsed scale).
fn invert(m: Affine) -> Option<Affine> {
    if m.determinant().abs() < 1e-12 {
        None
    } else {
        Some(m.inverse())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn corner(x: f64, y: f64) -> PathSample {
        PathSample { point: Vec2::new(x, y), in_tan: Vec2::ZERO, out_tan: Vec2::ZERO }
    }

    #[test]
    fn inserting_on_a_straight_segment_lands_on_the_line() {
        let mut s = vec![corner(0.0, 0.0), corner(10.0, 0.0)];
        let idx = insert_on_segment(&mut s, 0, 0.5);
        assert_eq!(idx, 1);
        assert_eq!(s.len(), 3);
        // The new point is the midpoint of a straight segment.
        assert!((s[1].point.x - 5.0).abs() < 1e-9);
        assert!((s[1].point.y - 0.0).abs() < 1e-9);
        // Endpoints untouched.
        assert_eq!(s[0].point, Vec2::new(0.0, 0.0));
        assert_eq!(s[2].point, Vec2::new(10.0, 0.0));
    }

    #[test]
    fn inserting_preserves_a_curved_segment_shape() {
        // A curve with symmetric tangents; splitting at 0.5 must put the new
        // point at the curve's actual midpoint, not the chord's.
        let mut s = vec![
            PathSample { point: Vec2::new(0.0, 0.0), in_tan: Vec2::ZERO, out_tan: Vec2::new(0.0, 10.0) },
            PathSample { point: Vec2::new(10.0, 0.0), in_tan: Vec2::new(0.0, 10.0), out_tan: Vec2::ZERO },
        ];
        insert_on_segment(&mut s, 0, 0.5);
        // The cubic bulges upward (+y), so the midpoint sits above the chord.
        assert!(s[1].point.y > 5.0, "y={}", s[1].point.y);
        assert!((s[1].point.x - 5.0).abs() < 1e-9, "x={}", s[1].point.x);
    }

    #[test]
    fn inserting_on_the_closing_segment_appends() {
        let mut s = vec![corner(0.0, 0.0), corner(10.0, 0.0), corner(10.0, 10.0)];
        // Closing segment is index n-1 = 2 (from anchor 2 back to anchor 0).
        let idx = insert_on_segment(&mut s, 2, 0.5);
        assert_eq!(idx, 3, "a point on the closing segment lands at the end");
        assert_eq!(s.len(), 4);
    }
}
