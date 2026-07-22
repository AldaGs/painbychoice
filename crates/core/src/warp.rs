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

/// Push a path through a homography, in composition space.
///
/// `None` when any part of the path falls at or behind the eye plane. That is a
/// whole-path answer rather than a clipped one on purpose: a partially-crossing
/// layer needs true near-plane clipping to draw correctly, and silently emitting
/// the half that happens to project would put a torn shape on screen and call it
/// a frame.
pub fn warp_path(path: &BezPath, h: &Homography) -> Option<BezPath> {
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
        let warped = warp_path(&square(), &h).expect("in front of the eye");
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
        let near = warp_path(&square(), &homography_for(&m, EYE, 500.0)).unwrap();
        let far = warp_path(&square(), &homography_for(&m, EYE, 10_000_000.0)).unwrap();
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
        let w = warp_path(&line, &h).unwrap();
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

    /// A path crossing the eye plane yields nothing rather than a torn shape.
    #[test]
    fn a_path_crossing_the_eye_plane_is_refused() {
        // Tipped 90°: the layer is edge-on and runs from far behind the eye to
        // far in front of it.
        let h = homography_for(&Mat4::rotate_y(std::f64::consts::FRAC_PI_2), EYE, 50.0);
        assert!(warp_path(&square(), &h).is_none());
    }
}
