//! Foreshortening: the projective map a tipped layer needs, and how a path is
//! carried through it.
//!
//! # Why there is no render-to-texture pass here
//!
//! A layer rotated about X or Y projects to a trapezoid — a homography, eight
//! degrees of freedom. `kurbo::Affine` has six and vello draws with nothing
//! else, so the textbook answer is to rasterize the layer offscreen and draw the
//! result as a projected quad.
//!
//! This engine does not need that, because **everything it draws is a
//! `BezPath`**. A path can be pushed through the homography directly, in
//! composition space, and handed to the renderer as ordinary geometry under an
//! identity transform. No offscreen target, no texture sampling, no seams, and
//! the result is resolution-independent the way the rest of the engine is — a
//! foreshortened title stays sharp at any zoom, which a rasterized quad would
//! not.
//!
//! The cost is that a projective map does not preserve Béziers: the image of a
//! cubic under a homography is a rational cubic, not a cubic. So segments are
//! subdivided until the error is below a tolerance and re-emitted as lines. That
//! is the same bargain `kurbo::flatten` already makes for stroking, at the same
//! kind of tolerance.
//!
//! # The near plane
//!
//! A layer that runs past the camera is cut, not dropped: camera depth is
//! linear across a layer's plane, so the near plane is a straight line in the
//! layer's own space and the path is clipped against it exactly before it is
//! projected ([`clip_near`]).
//!
//! # What is still left out
//!
//! Raster layers. An image is painted by the backend into its rectangle under an
//! affine, and warping the *rectangle* would not warp the pixels inside it. A
//! tipped footage layer therefore still draws unforeshortened; doing it properly
//! is the one case that genuinely wants a subdivided textured quad.

use kurbo::{BezPath, PathEl, Point};

use crate::mat4::Mat4;

/// A 3×3 projective transform, row-major: `p' = H · (x, y, 1)`, divided by w.
///
/// Built by [`crate::camera::Projector::homography`] from a layer's world matrix
/// and the eye. Never authored, never serialized — it is a per-frame derivative
/// of things that are.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Homography(pub [f64; 9]);

impl Homography {
    /// Map a point, or `None` if it lands at or behind the eye plane (`w <= 0`),
    /// where the projection is undefined and the divide would fling the point to
    /// infinity or mirror it through the origin.
    pub fn map(&self, p: Point) -> Option<Point> {
        let h = &self.0;
        let w = h[6] * p.x + h[7] * p.y + h[8];
        if w <= W_EPSILON {
            return None;
        }
        Some(Point::new(
            (h[0] * p.x + h[1] * p.y + h[2]) / w,
            (h[3] * p.x + h[4] * p.y + h[5]) / w,
        ))
    }

    /// Whether this map is really affine — the bottom row is `(0, 0, w)`, so
    /// every point divides by the same constant and no foreshortening happens.
    ///
    /// The escape hatch that keeps a screen-parallel layer off this whole code
    /// path even when a camera is present.
    pub fn is_affine(&self) -> bool {
        self.0[6] == 0.0 && self.0[7] == 0.0
    }
}

/// Points closer to the eye plane than this are treated as behind it. Guards the
/// perspective divide.
const W_EPSILON: f64 = 1e-9;

/// How far a subdivided chord may stray from the true projected curve, in
/// composition pixels, before it is split again. A quarter pixel is below what
/// any renderer can show and well above what the recursion depth cap can cost.
const FLATTEN_TOLERANCE: f64 = 0.25;

/// The most times one segment may be halved. A layer approaching edge-on drives
/// the curvature of the projected image toward infinity, and without a cap the
/// recursion would chase it. At this depth a segment has become 256 chords,
/// which is far past visible.
const MAX_DEPTH: u8 = 8;

/// Push a path through a homography, in composition space, cut at the near
/// plane.
///
/// `near` is the camera-space depth (pixels in front of the eye) below which
/// nothing is drawn. The part of the layer in front of it is kept and drawn
/// exactly; the part behind is cut away along the plane — so a floor or wall
/// that runs past the camera stays on screen up to where it passes it, instead
/// of vanishing whole. See [`clip_near`].
///
/// `None` only when nothing is left: the whole layer is behind the near plane.
pub fn warp_path(path: &BezPath, h: &Homography, near: f64) -> Option<BezPath> {
    let clipped = clip_near(path, h, near);
    let path = clipped.as_ref().unwrap_or(path);
    if path.elements().is_empty() {
        return None;
    }
    let mut out = BezPath::new();
    // The current point in *source* space — the homography is applied per
    // emitted point, so the walk has to keep the pre-image around.
    let mut cur = Point::ZERO;
    let mut start = Point::ZERO;

    for el in path.elements() {
        match *el {
            PathEl::MoveTo(p) => {
                out.move_to(h.map(p)?);
                cur = p;
                start = p;
            }
            PathEl::LineTo(p) => {
                // Even a straight line bends under a homography — the divide
                // varies along it — so it subdivides like everything else.
                flatten_into(&mut out, h, |t| lerp(cur, p, t))?;
                cur = p;
            }
            PathEl::QuadTo(c, p) => {
                flatten_into(&mut out, h, |t| quad_at(cur, c, p, t))?;
                cur = p;
            }
            PathEl::CurveTo(c1, c2, p) => {
                flatten_into(&mut out, h, |t| cubic_at(cur, c1, c2, p, t))?;
                cur = p;
            }
            PathEl::ClosePath => {
                out.close_path();
                cur = start;
            }
        }
    }
    Some(out)
}

/// Cut a layer-local path to the part at least `near` in front of the eye.
///
/// Camera depth is the homography's denominator, `w = h6·x + h7·y + h8`, and it
/// is **linear in layer-local space** — a layer is a plane, so its points at one
/// depth form a straight line across it. Near-plane clipping is therefore a 2D
/// half-plane clip, done on the source path *before* projecting, and done
/// exactly: a Bézier's depth along it is a polynomial in `t` (the linear `w`
/// applied to its control points), so each segment is split at the roots where
/// it crosses the plane and only the pieces in front are kept.
///
/// Filled (closed) subpaths are rejoined along the cut — the two crossing points
/// both lie on the plane, so the straight edge between them is the cut itself.
/// Open subpaths (strokes) simply break there.
///
/// `None` when nothing crosses: the path is drawn as it is, with no copy.
pub fn clip_near(path: &BezPath, h: &Homography, near: f64) -> Option<BezPath> {
    use kurbo::{CubicBez, Line, ParamCurve, PathSeg, QuadBez};
    let g = |p: Point| h.0[6] * p.x + h.0[7] * p.y + h.0[8] - near;
    // A curve lies in its control points' hull and `g` is linear, so if every
    // control point is in front, so is everything.
    let all_in = path.elements().iter().all(|el| match *el {
        PathEl::MoveTo(p) | PathEl::LineTo(p) => g(p) >= 0.0,
        PathEl::QuadTo(a, b) => g(a) >= 0.0 && g(b) >= 0.0,
        PathEl::CurveTo(a, b, c) => g(a) >= 0.0 && g(b) >= 0.0 && g(c) >= 0.0,
        PathEl::ClosePath => true,
    });
    if all_in {
        return None;
    }

    // Subpaths as segment lists, with whether each is closed.
    let mut subpaths: Vec<(Vec<PathSeg>, bool)> = Vec::new();
    let (mut cur, mut start) = (Point::ZERO, Point::ZERO);
    for el in path.elements() {
        match *el {
            PathEl::MoveTo(p) => {
                subpaths.push((Vec::new(), false));
                cur = p;
                start = p;
            }
            PathEl::ClosePath => {
                if let Some((segs, closed)) = subpaths.last_mut() {
                    if cur != start {
                        segs.push(PathSeg::Line(Line::new(cur, start)));
                    }
                    *closed = true;
                }
                cur = start;
            }
            el => {
                if subpaths.is_empty() {
                    subpaths.push((Vec::new(), false));
                }
                let seg = match el {
                    PathEl::LineTo(p) => PathSeg::Line(Line::new(cur, p)),
                    PathEl::QuadTo(a, b) => PathSeg::Quad(QuadBez::new(cur, a, b)),
                    PathEl::CurveTo(a, b, c) => PathSeg::Cubic(CubicBez::new(cur, a, b, c)),
                    _ => unreachable!(),
                };
                cur = seg.end();
                subpaths.last_mut().unwrap().0.push(seg);
            }
        }
    }

    let mut out = BezPath::new();
    for (segs, closed) in subpaths {
        // Whether the pen is down at an in-front point of this subpath.
        let mut drawing = false;
        let mut last = Point::ZERO;
        for seg in segs {
            // Roots of depth-minus-near along the segment, in power basis.
            let mut ts: Vec<f64> = match seg {
                PathSeg::Line(l) => {
                    let (a, b) = (g(l.p0), g(l.p1));
                    if (a < 0.0) != (b < 0.0) { vec![a / (a - b)] } else { vec![] }
                }
                PathSeg::Quad(q) => {
                    let (b0, b1, b2) = (g(q.p0), g(q.p1), g(q.p2));
                    kurbo::common::solve_quadratic(b0, 2.0 * (b1 - b0), b2 - 2.0 * b1 + b0).to_vec()
                }
                PathSeg::Cubic(c) => {
                    let (b0, b1, b2, b3) = (g(c.p0), g(c.p1), g(c.p2), g(c.p3));
                    kurbo::common::solve_cubic(
                        b0,
                        3.0 * (b1 - b0),
                        3.0 * (b2 - 2.0 * b1 + b0),
                        b3 - 3.0 * b2 + 3.0 * b1 - b0,
                    )
                    .to_vec()
                }
            };
            ts.retain(|t| *t > 0.0 && *t < 1.0);
            ts.sort_by(|a, b| a.partial_cmp(b).unwrap());
            let mut bounds = vec![0.0];
            bounds.extend(ts);
            bounds.push(1.0);
            for w in bounds.windows(2) {
                let (t0, t1) = (w[0], w[1]);
                if t1 - t0 <= 1e-12 {
                    continue;
                }
                let piece = seg.subsegment(t0..t1);
                if g(piece.eval(0.5)) < 0.0 {
                    // Behind: a stroke breaks here; a fill rejoins on re-entry.
                    if !closed {
                        drawing = false;
                    }
                    continue;
                }
                let p0 = piece.start();
                if drawing {
                    if last != p0 {
                        out.line_to(p0);
                    }
                } else {
                    out.move_to(p0);
                    drawing = true;
                }
                match piece {
                    PathSeg::Line(l) => out.line_to(l.p1),
                    PathSeg::Quad(q) => out.quad_to(q.p1, q.p2),
                    PathSeg::Cubic(c) => out.curve_to(c.p1, c.p2, c.p3),
                }
                last = piece.end();
            }
        }
        if closed && drawing {
            out.close_path();
        }
    }
    Some(out)
}

/// Emit one segment as a run of `line_to`s, splitting where the projected chord
/// strays from the projected curve.
fn flatten_into(
    out: &mut BezPath,
    h: &Homography,
    at: impl Fn(f64) -> Point,
) -> Option<()> {
    subdivide(out, h, &at, 0.0, 1.0, h.map(at(0.0))?, h.map(at(1.0))?, 0)?;
    Some(())
}

#[allow(clippy::too_many_arguments)]
fn subdivide(
    out: &mut BezPath,
    h: &Homography,
    at: &impl Fn(f64) -> Point,
    t0: f64,
    t1: f64,
    p0: Point,
    p1: Point,
    depth: u8,
) -> Option<()> {
    let tm = 0.5 * (t0 + t1);
    let pm = h.map(at(tm))?;
    // The test is on the *projected* midpoint: how far the real curve sits from
    // the straight chord we would otherwise emit. Measuring in source space
    // would miss exactly the error perspective introduces.
    let chord_mid = Point::new(0.5 * (p0.x + p1.x), 0.5 * (p0.y + p1.y));
    if depth >= MAX_DEPTH || (pm - chord_mid).hypot() <= FLATTEN_TOLERANCE {
        out.line_to(p1);
        return Some(());
    }
    subdivide(out, h, at, t0, tm, p0, pm, depth + 1)?;
    subdivide(out, h, at, tm, t1, pm, p1, depth + 1)?;
    Some(())
}

fn lerp(a: Point, b: Point, t: f64) -> Point {
    Point::new(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t)
}

fn quad_at(p0: Point, c: Point, p1: Point, t: f64) -> Point {
    let u = 1.0 - t;
    Point::new(
        u * u * p0.x + 2.0 * u * t * c.x + t * t * p1.x,
        u * u * p0.y + 2.0 * u * t * c.y + t * t * p1.y,
    )
}

fn cubic_at(p0: Point, c1: Point, c2: Point, p1: Point, t: f64) -> Point {
    let u = 1.0 - t;
    let (uu, tt) = (u * u, t * t);
    Point::new(
        uu * u * p0.x + 3.0 * uu * t * c1.x + 3.0 * u * tt * c2.x + tt * t * p1.x,
        uu * u * p0.y + 3.0 * uu * t * c1.y + 3.0 * u * tt * c2.y + tt * t * p1.y,
    )
}

/// Build the layer-local → screen homography for a world matrix and an eye.
///
/// Derivation, with `M` the world matrix, `d` the eye distance and `e` the eye
/// point. A local point `(x, y)` sits on the layer's own plane, so `z = 0`:
///
/// ```text
/// X = m0·x + m4·y + m12      Y = m1·x + m5·y + m13      Z = m2·x + m6·y + m14
/// ```
///
/// The camera scales about the eye by `d / (d + Z)`, so
///
/// ```text
/// screen.x = e.x + (X - e.x)·d/(d + Z) = (d·X + e.x·Z) / (d + Z)
/// ```
///
/// which is linear in `(x, y, 1)` over a common denominator — a homography, and
/// an exact one. The `Z` row is what makes it projective: it is zero exactly
/// when the layer has no out-of-plane rotation.
pub fn homography_for(m: &Mat4, eye: Point, distance: f64) -> Homography {
    let m = &m.0;
    let (ex, ey, d) = (eye.x, eye.y, distance);
    Homography([
        d * m[0] + ex * m[2],
        d * m[4] + ex * m[6],
        d * m[12] + ex * m[14],
        d * m[1] + ey * m[2],
        d * m[5] + ey * m[6],
        d * m[13] + ey * m[14],
        m[2],
        m[6],
        d + m[14],
    ])
}

#[cfg(test)]
mod tests {
    use kurbo::Shape as _;

    use super::*;
    use crate::vec3::Vec3;

    const EYE: Point = Point::new(0.0, 0.0);
    const D: f64 = 1000.0;

    fn square() -> BezPath {
        kurbo::Rect::new(-100.0, -100.0, 100.0, 100.0).to_path(0.01)
    }

    /// A layer that never left the plane must not touch the projective path at
    /// all — this is the predicate the fast path is chosen on.
    #[test]
    fn a_screen_parallel_layer_is_affine() {
        let m = Mat4::translate(Vec3::new(10.0, 20.0, 300.0)) * Mat4::rotate_z(0.7);
        assert!(homography_for(&m, EYE, D).is_affine());
    }

    #[test]
    fn a_tipped_layer_is_not_affine() {
        assert!(!homography_for(&Mat4::rotate_y(0.6), EYE, D).is_affine());
    }

    /// The whole point, stated as a measurement against the closed form: turn a
    /// square about Y and the edge that rotated *away* from the eye becomes
    /// shorter than the edge that came toward it, by exactly the ratio of their
    /// depths. An affine can never do this — it maps parallel edges to parallel
    /// edges of equal length.
    #[test]
    fn turning_a_card_about_y_foreshortens_by_the_depth_ratio() {
        const ANGLE: f64 = 0.6;
        // A near eye, so the effect is large enough to measure without fighting
        // the flattening tolerance.
        const DIST: f64 = 300.0;
        let h = homography_for(&Mat4::rotate_y(ANGLE), EYE, DIST);
        let warped = warp_path(&square(), &h, 1e-9).expect("in front of the eye");
        let pts: Vec<Point> = warped
            .elements()
            .iter()
            .filter_map(|e| match e {
                PathEl::MoveTo(p) | PathEl::LineTo(p) => Some(*p),
                _ => None,
            })
            .collect();

        // Height of the shape at its leftmost and rightmost extents — the two
        // vertical edges, one swung toward the eye and one away.
        let (min_x, max_x) = pts
            .iter()
            .fold((f64::MAX, f64::MIN), |(lo, hi), p| (lo.min(p.x), hi.max(p.x)));
        let span = |near: f64| {
            let ys: Vec<f64> =
                pts.iter().filter(|p| (p.x - near).abs() < 1.0).map(|p| p.y).collect();
            ys.iter().cloned().fold(f64::MIN, f64::max)
                - ys.iter().cloned().fold(f64::MAX, f64::min)
        };
        let (left, right) = (span(min_x), span(max_x));
        assert!(left > 0.0 && right > 0.0, "both edges present");

        // The edges sit at z = ±sin(angle)·halfwidth, so their heights are in
        // inverse proportion to their distance from the eye.
        let z = ANGLE.sin() * 100.0;
        let expected = (DIST + z) / (DIST - z);
        let got = left.max(right) / left.min(right);
        assert!(
            (got - expected).abs() < 0.02,
            "foreshortening ratio {got} should be {expected} ({left} vs {right})"
        );
    }

    /// Foreshortening is a *perspective* effect: with the eye pushed very far
    /// back the same rotation must flatten out to the orthographic answer.
    #[test]
    fn a_distant_eye_flattens_toward_orthographic() {
        let m = Mat4::rotate_y(0.6);
        let near = warp_path(&square(), &homography_for(&m, EYE, 500.0), 1e-9).unwrap();
        let far = warp_path(&square(), &homography_for(&m, EYE, 10_000_000.0), 1e-9).unwrap();
        let h = |p: &BezPath| p.bounding_box().height();
        // A near eye foreshortens hard, so the trapezoid's tall edge exceeds the
        // orthographic height; a distant one barely differs from it.
        assert!(h(&near) > h(&far) * 1.05, "{} vs {}", h(&near), h(&far));
    }

    /// Straight edges stay straight-ish: a homography maps lines to lines, so
    /// subdividing must not introduce a visible bow.
    #[test]
    fn a_straight_edge_stays_straight() {
        let h = homography_for(&Mat4::rotate_y(0.6), EYE, D);
        let mut line = BezPath::new();
        line.move_to(Point::new(-100.0, 0.0));
        line.line_to(Point::new(100.0, 0.0));
        let w = warp_path(&line, &h, 1e-9).unwrap();
        let pts: Vec<Point> = w
            .elements()
            .iter()
            .filter_map(|e| match e {
                PathEl::MoveTo(p) | PathEl::LineTo(p) => Some(*p),
                _ => None,
            })
            .collect();
        let (a, b) = (pts[0], *pts.last().unwrap());
        for p in &pts {
            let t = (p.x - a.x) / (b.x - a.x);
            let on_line = a.y + (b.y - a.y) * t;
            assert!((p.y - on_line).abs() < 1e-6, "bowed at {p:?}");
        }
    }

    /// A path crossing the eye plane is cut there, not refused: what is in
    /// front of the near plane still draws, finite and closed.
    #[test]
    fn a_path_crossing_the_eye_plane_is_cut() {
        // Tipped 90°: the layer is edge-on and runs from far behind the eye to
        // far in front of it.
        let h = homography_for(&Mat4::rotate_y(1.2), EYE, 50.0);
        let cut = warp_path(&square(), &h, 5.0).expect("the front part survives");
        let b = cut.bounding_box();
        assert!([b.x0, b.y0, b.x1, b.y1].iter().all(|v| v.is_finite()), "{b:?}");
        assert!(matches!(cut.elements().last(), Some(PathEl::ClosePath)), "still a fill");
    }

    /// Entirely behind the near plane: nothing to draw.
    #[test]
    fn a_path_wholly_behind_the_eye_is_refused() {
        let h = homography_for(&Mat4::translate(Vec3::new(0.0, 0.0, -1000.0)), EYE, 50.0);
        assert!(warp_path(&square(), &h, 5.0).is_none());
    }

    /// The cut is exact and in layer space: depth `w = x + 50` with the near
    /// plane at 10 keeps exactly `x >= -40` — for straight edges and curves.
    #[test]
    fn the_near_cut_is_a_straight_line_across_the_layer() {
        let h = Homography([1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 1.0, 0.0, 50.0]);
        let sq = clip_near(&square(), &h, 10.0).unwrap();
        let b = sq.bounding_box();
        assert!((b.x0 + 40.0).abs() < 1e-9 && b.x1 == 100.0, "{b:?}");
        assert_eq!((b.y0, b.y1), (-100.0, 100.0));

        let circle = kurbo::Circle::new((0.0, 0.0), 100.0).to_path(0.01);
        let c = clip_near(&circle, &h, 10.0).unwrap();
        let b = c.bounding_box();
        assert!((b.x0 + 40.0).abs() < 1e-6 && (b.x1 - 100.0).abs() < 1e-6, "{b:?}");
        // The cut ends where x = -40 meets the circle, y = ±√(100² − 40²)
        // (to the circle's cubic approximation).
        let half = (100f64.powi(2) - 40f64.powi(2)).sqrt();
        let on_cut: Vec<f64> = c
            .elements()
            .iter()
            .filter_map(|e| match *e {
                PathEl::MoveTo(p) | PathEl::LineTo(p) | PathEl::CurveTo(_, _, p) if (p.x + 40.0).abs() < 1e-6 => Some(p.y),
                _ => None,
            })
            .collect();
        assert!(!on_cut.is_empty());
        assert!(on_cut.iter().all(|y| (y.abs() - half).abs() < 0.05), "{on_cut:?} vs ±{half}");

        // Nothing crossing: no copy.
        assert!(clip_near(&square(), &h, -100.0).is_none());
    }
}
