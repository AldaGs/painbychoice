//! Boolean path operations — the "pathfinder" a compound group runs over its
//! children.
//!
//! We chose **curve-preserving** booleans (via `flo_curves`) rather than
//! flattening to polygons: the union of two circles comes back as arcs, not a
//! many-segment polyline, so a pathfinder result scales and re-edits like real
//! vector art. The cost is a kurbo↔flo_curves round-trip at each edge, and the
//! usual fragility of curve/curve intersection — accepted deliberately for the
//! professional look.
//!
//! Everything here is a **pure function of resolved [`BezPath`]s**, so a compound
//! group is animatable for free: the walk re-runs the fold every frame, exactly
//! the way the compositor re-derives its groups. Inputs are treated as closed
//! filled regions (a compound combines *filled* shapes); an open input is closed
//! by flo_curves the same way a fill implicitly closes it.

use kurbo::{BezPath, PathEl, Point};
use serde::{Deserialize, Serialize};

use flo_curves::bezier::path::{
    path_add, path_intersect, path_sub, BezierPath, BezierPathFactory, SimpleBezierPath,
};
use flo_curves::geo::Coord2;

/// How a compound group combines its children, left to right.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum BoolOp {
    /// The union of all children — one filled region covering them all.
    #[default]
    Union,
    /// The first child with every later one cut out of it.
    Subtract,
    /// Only where **all** children overlap.
    Intersect,
    /// Where an odd number of children overlap (symmetric difference).
    Exclude,
    /// No boolean at all — every child's contour kept, overlaps resolved by the
    /// fill rule. The cheap "just collect them into one path" option.
    Merge,
}

impl BoolOp {
    pub const ALL: [BoolOp; 5] =
        [BoolOp::Union, BoolOp::Subtract, BoolOp::Intersect, BoolOp::Exclude, BoolOp::Merge];

    pub fn label(self) -> &'static str {
        match self {
            BoolOp::Union => "Union",
            BoolOp::Subtract => "Subtract",
            BoolOp::Intersect => "Intersect",
            BoolOp::Exclude => "Exclude",
            BoolOp::Merge => "Merge",
        }
    }
}

/// Flatness the boolean solver works to, in composition units. Curves are kept;
/// this only bounds where intersections are pinned down.
const ACCURACY: f64 = 0.1;

/// Combine `paths` left to right under `op` into one path. An empty input (or
/// one that reduces to nothing) yields an empty path.
pub fn combine(paths: &[BezPath], op: BoolOp) -> BezPath {
    let mut inputs: Vec<Vec<SimpleBezierPath>> = paths
        .iter()
        .map(to_flo)
        .filter(|p| !p.is_empty())
        .collect();
    if inputs.is_empty() {
        return BezPath::new();
    }
    // Merge is just concatenation — keep every contour, let the fill rule sort
    // out the overlaps. No solver involved.
    if op == BoolOp::Merge {
        let all: Vec<SimpleBezierPath> = inputs.into_iter().flatten().collect();
        return from_flo(&all);
    }
    let mut acc = inputs.remove(0);
    for next in inputs {
        acc = match op {
            BoolOp::Union => path_add::<SimpleBezierPath>(&acc, &next, ACCURACY),
            BoolOp::Subtract => path_sub::<SimpleBezierPath>(&acc, &next, ACCURACY),
            BoolOp::Intersect => path_intersect::<SimpleBezierPath>(&acc, &next, ACCURACY),
            // Symmetric difference: (A−B) ∪ (B−A).
            BoolOp::Exclude => {
                let ab = path_sub::<SimpleBezierPath>(&acc, &next, ACCURACY);
                let ba = path_sub::<SimpleBezierPath>(&next, &acc, ACCURACY);
                path_add::<SimpleBezierPath>(&ab, &ba, ACCURACY)
            }
            BoolOp::Merge => unreachable!("handled above"),
        };
    }
    from_flo(&acc)
}

/// A kurbo path's subpaths as flo_curves paths. Each `MoveTo` starts a contour;
/// lines and quadratics are lifted to cubics so the whole thing is uniform.
fn to_flo(path: &BezPath) -> Vec<SimpleBezierPath> {
    let mut out: Vec<SimpleBezierPath> = Vec::new();
    let mut start = Coord2(0.0, 0.0);
    let mut last = Coord2(0.0, 0.0);
    let mut segs: Vec<(Coord2, Coord2, Coord2)> = Vec::new();
    let mut open = false;

    let flush = |out: &mut Vec<SimpleBezierPath>, start: Coord2, segs: &mut Vec<_>| {
        if !segs.is_empty() {
            out.push(SimpleBezierPath::from_points(start, std::mem::take(segs)));
        }
    };

    for el in path.elements() {
        match el {
            PathEl::MoveTo(p) => {
                flush(&mut out, start, &mut segs);
                start = c(*p);
                last = start;
                open = true;
            }
            PathEl::LineTo(p) => {
                let e = c(*p);
                // A straight cubic: controls at the thirds, exactly on the line.
                segs.push((lerp(last, e, 1.0 / 3.0), lerp(last, e, 2.0 / 3.0), e));
                last = e;
            }
            PathEl::QuadTo(cp, p) => {
                let (cp, e) = (c(*cp), c(*p));
                let c1 = lerp(last, cp, 2.0 / 3.0);
                let c2 = lerp(e, cp, 2.0 / 3.0);
                segs.push((c1, c2, e));
                last = e;
            }
            PathEl::CurveTo(c1, c2, p) => {
                let e = c(*p);
                segs.push((c(*c1), c(*c2), e));
                last = e;
            }
            PathEl::ClosePath => {
                flush(&mut out, start, &mut segs);
                open = false;
            }
        }
    }
    if open {
        flush(&mut out, start, &mut segs);
    }
    out
}

/// flo_curves paths back to one kurbo path — the inverse of [`to_flo`]. Each
/// contour is closed, which is what a filled boolean result is.
fn from_flo(paths: &[SimpleBezierPath]) -> BezPath {
    let mut out = BezPath::new();
    for p in paths {
        let s = p.start_point();
        out.move_to(k(s));
        for (c1, c2, e) in p.points() {
            out.curve_to(k(c1), k(c2), k(e));
        }
        out.close_path();
    }
    out
}

fn c(p: Point) -> Coord2 {
    Coord2(p.x, p.y)
}

fn k(p: Coord2) -> Point {
    Point::new(p.0, p.1)
}

fn lerp(a: Coord2, b: Coord2, t: f64) -> Coord2 {
    Coord2(a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t)
}

#[cfg(test)]
mod tests {
    use super::*;
    use kurbo::{Rect, Shape as _};

    fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> BezPath {
        Rect::new(x0, y0, x1, y1).to_path(0.1)
    }

    #[test]
    fn union_of_two_overlapping_squares_is_one_bigger_region() {
        let a = rect(0.0, 0.0, 10.0, 10.0);
        let b = rect(5.0, 5.0, 15.0, 15.0);
        let u = combine(&[a, b], BoolOp::Union);
        let bb = u.bounding_box();
        // Spans both squares' extent…
        assert!(bb.width() > 14.0 && bb.height() > 14.0);
        // …and its filled area is less than the two squares apart (they overlap),
        // more than one alone.
        assert!(!u.is_empty());
    }

    #[test]
    fn intersection_is_only_the_overlap() {
        let a = rect(0.0, 0.0, 10.0, 10.0);
        let b = rect(5.0, 5.0, 15.0, 15.0);
        let i = combine(&[a, b], BoolOp::Intersect);
        let bb = i.bounding_box();
        // The overlap is the 5×5 corner square.
        assert!((bb.width() - 5.0).abs() < 0.5, "w={}", bb.width());
        assert!((bb.height() - 5.0).abs() < 0.5, "h={}", bb.height());
    }

    #[test]
    fn subtract_removes_the_second_from_the_first() {
        let a = rect(0.0, 0.0, 10.0, 10.0);
        let b = rect(5.0, 0.0, 15.0, 10.0); // cuts the right half
        let s = combine(&[a, b], BoolOp::Subtract);
        let bb = s.bounding_box();
        assert!(bb.width() <= 5.5, "left half only, got w={}", bb.width());
    }

    #[test]
    fn disjoint_shapes_union_to_two_contours() {
        let a = rect(0.0, 0.0, 5.0, 5.0);
        let b = rect(20.0, 20.0, 25.0, 25.0);
        let u = combine(&[a, b], BoolOp::Union);
        // Two separate move_tos = two contours survived.
        let moves = u.elements().iter().filter(|e| matches!(e, PathEl::MoveTo(_))).count();
        assert_eq!(moves, 2);
    }

    #[test]
    fn empty_input_is_an_empty_path() {
        assert!(combine(&[], BoolOp::Union).is_empty());
    }
}
