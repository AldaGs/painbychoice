//! Editable, animatable vector paths — the thing a Bézier pen tool draws.
//!
//! `Shape::Path(BezPath)` used to be inert: a baked outline with no recipe, so
//! it could not keyframe, could not be graph-driven, could not be morphed. This
//! module replaces it with a live path whose every control point is an ordinary
//! [`Value`], which is what lets a hand-drawn path flow through the *entire*
//! existing animation stack — keyframes, the dopesheet, retiming, the curve
//! editor, undo — with no new machinery, exactly the way text got animation for
//! free by resolving to outlines through `to_path`.
//!
//! **Per-point storage, whole-path default.** Each [`Anchor`] holds its point
//! and its two tangent handles as separate [`Value<Vec2>`]s. Freshly drawn, they
//! are all [`Value::Const`], so a path is cheap and the dopesheet shows it as one
//! collapsed "Path" row. "Animate this point" promotes a single anchor's value to
//! a track — the same promote-then-key move every other property uses — which is
//! how independent point animation stays opt-in and the timeline stays uncrowded.
//!
//! **Points are flat.** Per the doctrine in [`crate::vec3`], a layer's *contents*
//! are two-dimensional; only its *transform* is 2.5D. So an anchor is a
//! [`kurbo::Vec2`], living in the layer's own plane, and the layer's transform is
//! what tips it into space. This also means an anchor channel is a plain X/Y pair
//! the curve editor and `PropRef::Vec2` already know how to draw.

use kurbo::{BezPath, PathEl, Point, Vec2};
use serde::{Deserialize, Serialize};

use crate::expr::EvalCtx;
use crate::value::Value;

/// Which of an anchor's three control points a per-point property addresses.
///
/// Ordered `Point` < `In` < `Out` so a `(index, part)` key sorts anchor-major,
/// which is the order the dopesheet lists a point's channels in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum PathPart {
    /// The on-curve anchor point itself.
    Point,
    /// The incoming tangent handle, stored **relative** to the point.
    In,
    /// The outgoing tangent handle, stored **relative** to the point.
    Out,
}

impl PathPart {
    /// The three parts in address order — what a row enumerator walks.
    pub const ALL: [PathPart; 3] = [PathPart::Point, PathPart::In, PathPart::Out];

    /// Suffix shown next to an anchor's row: `P3`, `P3 in`, `P3 out`.
    pub fn suffix(self) -> &'static str {
        match self {
            PathPart::Point => "",
            PathPart::In => " in",
            PathPart::Out => " out",
        }
    }
}

/// One on-curve point with its two Bézier tangent handles.
///
/// Tangents are stored **relative to the point** (an offset, not an absolute
/// position), so moving the anchor carries its handles with it and a straight
/// segment is simply a zero tangent. A smooth anchor keeps `in_tan == -out_tan`;
/// a corner lets them diverge — but that mirroring is a UI convenience, not an
/// invariant here (the pen tool's Alt-drag breaks it deliberately).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Anchor {
    pub point: Value<Vec2>,
    pub in_tan: Value<Vec2>,
    pub out_tan: Value<Vec2>,
}

impl Anchor {
    /// A corner anchor: on-curve point, both tangents zero (straight segments).
    pub fn corner(point: Vec2) -> Self {
        Self {
            point: Value::Const(point),
            in_tan: Value::Const(Vec2::ZERO),
            out_tan: Value::Const(Vec2::ZERO),
        }
    }

    /// A smooth anchor with mirrored tangents (`out` one way, `in` the other).
    pub fn smooth(point: Vec2, out_tan: Vec2) -> Self {
        Self {
            point: Value::Const(point),
            in_tan: Value::Const(-out_tan),
            out_tan: Value::Const(out_tan),
        }
    }

    /// Borrow the [`Value`] for one control point — the seam `prop_of` reaches
    /// through to give a single anchor its own keyframe track.
    pub fn part(&self, part: PathPart) -> &Value<Vec2> {
        match part {
            PathPart::Point => &self.point,
            PathPart::In => &self.in_tan,
            PathPart::Out => &self.out_tan,
        }
    }

    pub fn part_mut(&mut self, part: PathPart) -> &mut Value<Vec2> {
        match part {
            PathPart::Point => &mut self.point,
            PathPart::In => &mut self.in_tan,
            PathPart::Out => &mut self.out_tan,
        }
    }

    /// Every control point of this anchor, for whole-anchor operations
    /// (retime, migrate) that don't care which is which.
    fn parts_mut(&mut self) -> [&mut Value<Vec2>; 3] {
        [&mut self.point, &mut self.in_tan, &mut self.out_tan]
    }

    /// Is any of this anchor's control points keyframed? Drives whether the
    /// anchor shows its own dopesheet rows or folds into the collapsed path row.
    pub fn is_animated(&self) -> bool {
        self.point.is_animated() || self.in_tan.is_animated() || self.out_tan.is_animated()
    }

    /// The three control points resolved at `ctx`'s frame: on-curve point and
    /// the two handles as **absolute** positions (point + relative tangent).
    fn resolve(&self, ctx: &mut EvalCtx) -> ResolvedAnchor {
        let point = self.point.resolve(ctx);
        ResolvedAnchor {
            point: to_point(point),
            in_handle: to_point(point + self.in_tan.resolve(ctx)),
            out_handle: to_point(point + self.out_tan.resolve(ctx)),
        }
    }
}

struct ResolvedAnchor {
    point: Point,
    in_handle: Point,
    out_handle: Point,
}

/// One anchor resolved at a frame, tangents kept **relative** to the point — the
/// form the on-canvas pen tool draws and hit-tests against. Distinct from the
/// private [`ResolvedAnchor`] above, which pre-adds the tangents into absolute
/// handle points for [`VectorPath::to_bez`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PathSample {
    pub point: Vec2,
    pub in_tan: Vec2,
    pub out_tan: Vec2,
}

/// A whole editable path: a sequence of anchors and whether it closes.
///
/// This is **not** a [`Value`] — the animation lives one level down, in each
/// anchor's control-point values. That is the per-point model: the path's
/// *topology* (how many anchors, and whether it's closed) is fixed structure,
/// and only the point positions move over time. Morphing one path shape into
/// another is therefore keyframing every anchor between two poses, and it needs
/// the counts to match — there is no interpolation across a change in topology,
/// the same limitation every professional pen tool carries.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct VectorPath {
    pub anchors: Vec<Anchor>,
    pub closed: bool,
}

impl VectorPath {
    /// An empty path — what the "add vector layer" button seeds before the pen
    /// tool places a first anchor.
    pub fn empty() -> Self {
        Self { anchors: Vec::new(), closed: false }
    }

    /// A polyline through `points`, corners only (zero tangents).
    pub fn polyline(points: impl IntoIterator<Item = Vec2>, closed: bool) -> Self {
        Self { anchors: points.into_iter().map(Anchor::corner).collect(), closed }
    }

    /// Borrow one anchor's control-point value, if the index is in range. The
    /// entry point `prop_of` uses to hand a single point its own track.
    pub fn value(&self, index: usize, part: PathPart) -> Option<&Value<Vec2>> {
        self.anchors.get(index).map(|a| a.part(part))
    }

    pub fn value_mut(&mut self, index: usize, part: PathPart) -> Option<&mut Value<Vec2>> {
        self.anchors.get_mut(index).map(|a| a.part_mut(part))
    }

    /// Resolve every anchor at `ctx`'s frame, tangents relative — what the pen
    /// tool draws and hit-tests. The overlay never touches `Value`s directly, so
    /// an animated point is edited on canvas exactly as a static one is: through
    /// its resolved position this frame.
    pub fn sample_anchors(&self, ctx: &mut EvalCtx) -> Vec<PathSample> {
        self.anchors
            .iter()
            .map(|a| PathSample {
                point: a.point.resolve(ctx),
                in_tan: a.in_tan.resolve(ctx),
                out_tan: a.out_tan.resolve(ctx),
            })
            .collect()
    }

    /// Resolve the path at `ctx`'s frame into a concrete [`BezPath`] — the seam
    /// every renderer, hit-test, bounding box, snap and overlay already consumes,
    /// which is why a vector layer needs zero changes anywhere downstream.
    ///
    /// A segment between two anchors is the cubic `curve_to(a.out, b.in, b)`; a
    /// zero tangent on both ends degrades to a straight line, so a corner
    /// polyline and a smooth curve are the same code path.
    pub fn to_bez(&self, ctx: &mut EvalCtx) -> BezPath {
        let mut path = BezPath::new();
        if self.anchors.is_empty() {
            return path;
        }
        let pts: Vec<ResolvedAnchor> = self.anchors.iter().map(|a| a.resolve(ctx)).collect();
        path.move_to(pts[0].point);
        for win in pts.windows(2) {
            let (a, b) = (&win[0], &win[1]);
            path.curve_to(a.out_handle, b.in_handle, b.point);
        }
        if self.closed && pts.len() > 1 {
            let (a, b) = (pts.last().unwrap(), &pts[0]);
            path.curve_to(a.out_handle, b.in_handle, b.point);
            path.close_path();
        }
        path
    }

    /// Best-effort conversion of a baked [`BezPath`] into editable anchors.
    ///
    /// Used to migrate a legacy `Shape::Path`, and later to import an SVG path or
    /// bring a pathfinder result back under the pen. Each on-curve endpoint
    /// becomes an anchor; a `CurveTo`'s control points become the outgoing
    /// tangent of the previous anchor and the incoming tangent of the new one.
    /// Multi-subpath inputs keep only the first subpath (the editable-path model
    /// is one contour; compound shapes are a group of layers, not one path).
    pub fn from_bez(bez: &BezPath) -> Self {
        let mut anchors: Vec<Anchor> = Vec::new();
        let mut closed = false;
        // The anchors carry only consts here, so the previous on-curve point is
        // tracked as a plain vector rather than read back out of a `Value`.
        let mut last = Vec2::ZERO;
        let mut first = Vec2::ZERO;
        for el in bez.elements() {
            match el {
                PathEl::MoveTo(p) => {
                    if anchors.is_empty() {
                        let v = to_vec2(*p);
                        anchors.push(Anchor::corner(v));
                        last = v;
                        first = v;
                    } else {
                        break; // second subpath — stop at the first contour
                    }
                }
                PathEl::LineTo(p) => {
                    let v = to_vec2(*p);
                    anchors.push(Anchor::corner(v));
                    last = v;
                }
                PathEl::QuadTo(c, p) => {
                    // Elevate the quadratic to a cubic so the anchor model (which
                    // is cubic throughout) is exact rather than approximate.
                    let (c, pe) = (to_vec2(*c), to_vec2(*p));
                    let c1 = last + (c - last) * (2.0 / 3.0);
                    if let Some(prev) = anchors.last_mut() {
                        prev.out_tan = Value::Const(c1 - last);
                    }
                    let c2 = pe + (c - pe) * (2.0 / 3.0);
                    let mut a = Anchor::corner(pe);
                    a.in_tan = Value::Const(c2 - pe);
                    anchors.push(a);
                    last = pe;
                }
                PathEl::CurveTo(c1, c2, p) => {
                    let (c1, c2, pe) = (to_vec2(*c1), to_vec2(*c2), to_vec2(*p));
                    if let Some(prev) = anchors.last_mut() {
                        prev.out_tan = Value::Const(c1 - last);
                    }
                    let mut a = Anchor::corner(pe);
                    a.in_tan = Value::Const(c2 - pe);
                    anchors.push(a);
                    last = pe;
                }
                PathEl::ClosePath => {
                    closed = true;
                    // A close often repeats the start point as a final anchor;
                    // drop a duplicate so the closing segment isn't zero-length.
                    if anchors.len() > 1 && (last - first).hypot() < 1e-6 {
                        anchors.pop();
                    }
                    break;
                }
            }
        }
        Self { anchors, closed }
    }

    pub(crate) fn migrate_frames(&mut self, fps: f64) {
        for a in &mut self.anchors {
            for v in a.parts_mut() {
                v.migrate_frames(fps);
            }
        }
    }

    pub(crate) fn retime(&mut self, ratio: f64) {
        for a in &mut self.anchors {
            for v in a.parts_mut() {
                v.retime(ratio);
            }
        }
    }
}

fn to_point(v: Vec2) -> Point {
    Point::new(v.x, v.y)
}

fn to_vec2(p: Point) -> Vec2 {
    Vec2::new(p.x, p.y)
}

#[cfg(test)]
mod tests {
    use super::*;
    use kurbo::Shape as _;

    fn ctx() -> EvalCtx<'static> {
        EvalCtx::at(0.0)
    }

    #[test]
    fn a_corner_polyline_resolves_to_straight_segments() {
        let p = VectorPath::polyline(
            [Vec2::new(0.0, 0.0), Vec2::new(10.0, 0.0), Vec2::new(10.0, 10.0)],
            false,
        );
        let bez = p.to_bez(&mut ctx());
        // Three anchors, one move + two segments, path not empty.
        assert_eq!(bez.elements().len(), 3);
        assert!(bez.bounding_box().width() > 0.0);
    }

    #[test]
    fn closing_adds_a_segment_and_a_close() {
        let open = VectorPath::polyline([Vec2::ZERO, Vec2::new(10.0, 0.0), Vec2::new(5.0, 8.0)], false);
        let closed = VectorPath { closed: true, ..open.clone() };
        assert!(closed.to_bez(&mut ctx()).elements().len() > open.to_bez(&mut ctx()).elements().len());
    }

    #[test]
    fn tangents_bulge_the_bounding_box_out() {
        // A single straight segment vs. the same with outward tangents: the curve
        // must reach beyond the chord.
        let straight = VectorPath::polyline([Vec2::ZERO, Vec2::new(10.0, 0.0)], false);
        let mut curved = straight.clone();
        curved.anchors[0].out_tan = Value::Const(Vec2::new(0.0, 6.0));
        curved.anchors[1].in_tan = Value::Const(Vec2::new(0.0, 6.0));
        assert!(
            curved.to_bez(&mut ctx()).bounding_box().height()
                > straight.to_bez(&mut ctx()).bounding_box().height()
        );
    }

    #[test]
    fn from_bez_round_trips_a_cubic_contour() {
        let src = VectorPath::polyline([Vec2::ZERO, Vec2::new(10.0, 0.0), Vec2::new(10.0, 10.0)], true);
        let bez = src.to_bez(&mut ctx());
        let back = VectorPath::from_bez(&bez);
        assert_eq!(back.anchors.len(), src.anchors.len());
        assert!(back.closed);
    }

    #[test]
    fn empty_path_is_a_no_op() {
        assert!(VectorPath::empty().to_bez(&mut ctx()).is_empty());
    }
}
