//! The composition camera — what turns a depth into something you can see.
//!
//! # Why a camera is opt-in
//!
//! A comp with `camera: None` is a 2D comp: `z` is stored on every transform,
//! but nothing reads it, and the render path is bit-for-bit the one that existed
//! before 2.5D. That is the toggle, expressed as data rather than a mode flag —
//! there is no "is 3D enabled" boolean to get out of sync, because the absence
//! of a camera *is* the absence of projection.
//!
//! # The projection, and what it does not do
//!
//! The camera sits on the composition's optical axis, `distance` pixels in front
//! of the `z = 0` plane, looking down +Z. A layer at `z = 0` therefore renders at
//! exactly 1:1 — every existing document is unmoved by switching a camera on,
//! which is the property that makes this safe to add to a live project.
//!
//! Depth becomes a **uniform scale about the eye point**: a layer at `z = 200`
//! with the camera 1000px back draws at 1000/1200 of its size and slides toward
//! the centre of frame, exactly as it would if you walked away from it. Because
//! that is a scale and a translation, a screen-parallel layer *stays an affine*
//! after projection and keeps the whole fast path — see [`Camera::project`].
//!
//! What this does **not** do is foreshorten a layer rotated about X or Y. That
//! needs a homography, which no affine can express and which vello cannot draw;
//! such a layer still projects its position and its apparent size correctly, but
//! is drawn unturned in depth. It is the remaining half of the 2.5D story and it
//! wants a render-to-texture pass of its own.

use serde::{Deserialize, Serialize};

use crate::expr::EvalCtx;
use crate::mat4::Xf;
use crate::value::Value;

/// A composition's camera. Present means the comp projects; absent means it is
/// flat, and the engine behaves exactly as it did before depth existed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Camera {
    /// How far in front of the `z = 0` plane the eye sits, in composition
    /// pixels. Animatable, so a dolly is a keyframed value like anything else.
    ///
    /// Larger is flatter: at 100000 the projection is very nearly orthographic,
    /// and depth reads only as a faint size change. The default is sized to the
    /// comp so a layer pushed a few hundred pixels back reads as *moved*, not as
    /// slightly resized.
    pub distance: Value<f64>,
}

impl Camera {
    /// The default eye distance for a comp of this size: twice the diagonal.
    /// Chosen so a layer displaced by a comp-width of depth shrinks noticeably
    /// but not grotesquely — a working starting point, not a physical constant.
    pub fn default_distance(width: f64, height: f64) -> f64 {
        2.0 * width.hypot(height)
    }

    pub fn new(width: f64, height: f64) -> Self {
        Self { distance: Value::constant(Self::default_distance(width, height)) }
    }

    /// Resolve to the frame's actual projection.
    ///
    /// Split from [`Camera`] itself because the authored camera is a recipe
    /// (its distance can be keyframed or expression-driven) while the thing the
    /// walk carries down the tree must be a plain `Copy` value it can apply
    /// thousands of times without touching the evaluator again.
    pub fn resolve(&self, ctx: &mut EvalCtx, width: f64, height: f64) -> Projector {
        Projector {
            eye: kurbo::Point::new(width / 2.0, height / 2.0),
            distance: self.distance.resolve(ctx),
        }
    }
}

/// A camera resolved at one frame: where the eye is, and how far back.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Projector {
    /// The optical axis' landing point in composition space — the comp centre.
    /// The one point depth does not move, and so the point everything converges
    /// toward as it recedes.
    pub eye: kurbo::Point,
    pub distance: f64,
}

impl Projector {
    /// The layer-local → screen projective map for a world matrix.
    ///
    /// This is the exact projection, unlike [`Self::project`], which collapses
    /// a layer to a single depth. Used for the layers that need foreshortening;
    /// [`Homography::is_affine`] says when it would have made no difference.
    pub fn homography(&self, m: &crate::mat4::Mat4) -> crate::warp::Homography {
        crate::warp::homography_for(m, self.eye, self.distance)
    }

    /// A layer at this depth is at or behind the eye. Nothing in front of the
    /// camera can be seen through it, and the scale factor would be infinite or
    /// negative (an inside-out layer), so it is culled instead.
    fn is_culled(&self, depth: f64) -> bool {
        self.distance + depth <= EPSILON
    }

    /// Project a world transform through this camera.
    ///
    /// `None` means the layer is at or behind the eye and must not be drawn.
    ///
    /// The layer's **origin depth** drives the whole projection — one scale for
    /// the entire layer, not a per-vertex divide. That is the defining
    /// approximation of a screen-parallel (billboard) tier, and it is exact for
    /// any layer that has not been tipped out of the plane, which is every layer
    /// this tier is responsible for.
    pub fn project(&self, xf: Xf) -> Option<Xf> {
        // A flat transform cannot carry depth by construction, so it projects
        // to itself. Checked first so a 2D layer in a 3D comp costs nothing.
        if let Xf::Flat(a) = xf {
            return Some(Xf::Flat(a));
        }
        let m = xf.to_mat4();
        // The translation column: where this layer's origin landed in comp
        // space, depth included.
        let depth = m.0[14];
        if self.is_culled(depth) {
            return None;
        }
        let s = self.distance / (self.distance + depth);

        // Scale about the eye, in the plane. This is the entire projection for a
        // screen-parallel layer — and being a scale plus a translation, it
        // composes with an affine to give an affine.
        let view = kurbo::Affine::translate(self.eye.to_vec2())
            * kurbo::Affine::scale(s)
            * kurbo::Affine::translate(-self.eye.to_vec2());

        Some(match m.as_affine_ignoring_depth() {
            // Screen-parallel despite carrying depth — the common 2.5D layer.
            // Drop to the affine path now that the depth has been spent on the
            // scale.
            Some(a) => Xf::Flat(view * a),
            // Genuinely tipped out of the plane. Its position and apparent size
            // are projected; its foreshortening is not, and cannot be until
            // there is a pass that can draw a non-affine quad.
            None => Xf::Spatial(crate::mat4::Mat4::from_affine(view) * m),
        })
    }
}

/// Depths closer to the eye than this are treated as *at* it. Guards the divide
/// without pretending a layer 0.0001px in front of the lens is meaningful.
const EPSILON: f64 = 1e-6;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mat4::Mat4;
    use crate::vec3::Vec3;

    fn cam() -> Projector {
        Projector { eye: kurbo::Point::new(960.0, 540.0), distance: 1000.0 }
    }
    const EYE: kurbo::Point = kurbo::Point::new(960.0, 540.0);

    /// The property that makes a camera safe to switch on mid-project: nothing
    /// at the zero plane moves by so much as a pixel.
    #[test]
    fn a_layer_at_zero_depth_is_untouched() {
        let a = kurbo::Affine::translate((100.0, 200.0)) * kurbo::Affine::rotate(0.4);
        let out = cam().project(Xf::Flat(a)).expect("visible");
        let got = out.to_affine().expect("still planar");
        for (l, r) in got.as_coeffs().iter().zip(a.as_coeffs().iter()) {
            assert!((l - r).abs() < 1e-12, "{l} vs {r}");
        }
    }

    /// Depth reads as size, and the arithmetic is the plain similar-triangles
    /// one: at one camera-distance back, everything is half size.
    #[test]
    fn depth_scales_by_similar_triangles() {
        let xf = Xf::Spatial(Mat4::translate(Vec3::new(960.0, 540.0, 1000.0)));
        let out = cam().project(xf).expect("visible");
        let a = out.to_affine().expect("a billboard stays planar");
        assert!((a.as_coeffs()[0] - 0.5).abs() < 1e-12, "half size at 1x distance");
    }

    /// A layer pushed back drifts toward the eye point rather than staying put:
    /// that convergence is what actually reads as depth on screen.
    #[test]
    fn depth_pulls_a_layer_toward_the_eye_point() {
        let corner = Vec3::new(0.0, 0.0, 1000.0);
        let out = cam().project(Xf::Spatial(Mat4::translate(corner))).unwrap();
        let a = out.to_affine().unwrap();
        let p = a * kurbo::Point::ZERO;
        assert!(p.x > 0.0 && p.x < EYE.x, "moved toward centre, not past it: {p:?}");
        assert!((p.x - EYE.x / 2.0).abs() < 1e-9);
    }

    /// Projection must not silently rescue a layer that has left the plane —
    /// the foreshortening is still missing, and pretending otherwise would draw
    /// a wrong frame rather than an honestly incomplete one.
    #[test]
    fn a_tipped_layer_stays_spatial() {
        let xf = Xf::Spatial(Mat4::translate(Vec3::new(0.0, 0.0, 100.0)) * Mat4::rotate_y(0.5));
        let out = cam().project(xf).expect("visible");
        assert!(!out.is_flat(), "still needs the projected pass");
    }

    #[test]
    fn a_layer_at_or_behind_the_eye_is_culled() {
        let c = cam();
        let at = Xf::Spatial(Mat4::translate(Vec3::new(0.0, 0.0, -1000.0)));
        assert!(c.project(at).is_none(), "exactly at the eye");
        let behind = Xf::Spatial(Mat4::translate(Vec3::new(0.0, 0.0, -1500.0)));
        assert!(c.project(behind).is_none(), "behind the eye");
    }
}
