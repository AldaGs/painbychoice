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
use crate::mat4::{Mat4, Xf};
use crate::vec3::Vec3;
use crate::value::Value;

/// A composition's camera. Present means the comp projects; absent means it is
/// flat, and the engine behaves exactly as it did before depth existed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Camera {
    /// Where the eye sits, as an **object in the composition** — not a lone
    /// setting.
    ///
    /// `x`/`y` offset the viewpoint from the comp centre, so panning the camera
    /// slides the vanishing point and pulls parallax between depths. `z` is the
    /// dolly: it is *signed depth*, negative in front of the `z = 0` plane, and
    /// `-z` is the eye-to-plane distance the perspective divides by. Pushing the
    /// camera toward the plane (z → 0) strengthens perspective; pulling it far
    /// back flattens toward orthographic.
    ///
    /// Every channel is animatable, so a dolly or a truck is a keyframed value
    /// like any other. The default sits the eye on the axis, `default_distance`
    /// in front, which reproduces the fixed camera this replaced.
    #[serde(default = "default_camera_position")]
    pub position: Value<Vec3>,
    /// The camera's orientation, euler degrees, default looking straight down
    /// `+Z` at the composition. Rotating it pivots the whole scene about the
    /// eye — the same operation the viewport orbit performs, but **part of the
    /// document**: this one renders.
    #[serde(default = "zero_rotation")]
    pub rotation: Value<Vec3>,
    /// A pre-object camera stored only a scalar `distance`. It cannot
    /// deserialize into `position` (a number is not a vector), so it lands here
    /// and [`Camera::migrate_frames`] folds it into `position.z` as `-distance`.
    /// Never written back out.
    #[serde(default, rename = "distance", skip_serializing)]
    legacy_distance: Option<Value<f64>>,
}

/// The eye offset a camera falls back to when its `position` is missing — only
/// reachable through a malformed file, since new cameras always write one and a
/// legacy camera's `distance` overwrites the `z` here. The `x`/`y` are the
/// meaningful part (centred); `z` is a placeholder the fold replaces.
fn default_camera_position() -> Value<Vec3> {
    Value::constant(Vec3::new(0.0, 0.0, -FALLBACK_DISTANCE))
}

/// Only used to seed [`default_camera_position`]; a real camera's depth comes
/// from [`Camera::default_distance`], which is sized to the comp.
const FALLBACK_DISTANCE: f64 = 2000.0;

fn zero_rotation() -> Value<Vec3> {
    Value::constant(Vec3::ZERO)
}

impl Camera {
    /// The default eye distance for a comp of this size: twice the diagonal.
    /// Chosen so a layer displaced by a comp-width of depth shrinks noticeably
    /// but not grotesquely — a working starting point, not a physical constant.
    pub fn default_distance(width: f64, height: f64) -> f64 {
        2.0 * width.hypot(height)
    }

    pub fn new(width: f64, height: f64) -> Self {
        Self {
            position: Value::constant(Vec3::new(0.0, 0.0, -Self::default_distance(width, height))),
            rotation: Value::constant(Vec3::ZERO),
            legacy_distance: None,
        }
    }

    /// Fold a pre-object scalar `distance` into the position, and migrate every
    /// camera track onto the frame grid. Mirrors [`crate::node::Transform`]'s
    /// own migration: the old value *was* the eye-to-plane distance, so the
    /// eye's depth is its negation, and a camera upgraded this way projects
    /// identically.
    /// A camera on the axis at a given eye-to-plane distance, looking straight
    /// on — the whole camera reduced to the one number it used to be. For tests
    /// and for the comp bar's "add camera", where only the framing is chosen.
    pub fn from_distance(distance: f64) -> Self {
        Self {
            position: Value::constant(Vec3::new(0.0, 0.0, -distance)),
            rotation: Value::constant(Vec3::ZERO),
            legacy_distance: None,
        }
    }

    pub(crate) fn migrate_frames(&mut self, fps: f64) {
        if let Some(distance) = self.legacy_distance.take() {
            self.position = deepen_distance(distance);
        }
        self.position.migrate_frames(fps);
        self.rotation.migrate_frames(fps);
    }

    pub(crate) fn retime(&mut self, ratio: f64) {
        self.position.retime(ratio);
        self.rotation.retime(ratio);
    }

    /// Resolve to the frame's actual projection.
    ///
    /// Split from [`Camera`] itself because the authored camera is a recipe
    /// (its distance can be keyframed or expression-driven) while the thing the
    /// walk carries down the tree must be a plain `Copy` value it can apply
    /// thousands of times without touching the evaluator again.
    pub fn resolve(&self, ctx: &mut EvalCtx, width: f64, height: f64) -> Projector {
        self.resolve_orbited(ctx, width, height, Mat4::IDENTITY)
    }

    /// Resolve with the editor standing somewhere other than straight on. Only
    /// the live preview passes anything but the identity here — see
    /// [`Projector::orbit`].
    pub fn resolve_orbited(
        &self,
        ctx: &mut EvalCtx,
        width: f64,
        height: f64,
        orbit: Mat4,
    ) -> Projector {
        let pos = self.position.resolve(ctx);
        let rot = self.rotation.resolve(ctx);
        // The camera's own rotation pivots the world about the eye — the very
        // thing the editor orbit does, so it composes as one more orbit. The
        // editor's is applied *outside* the camera's: you orbit around the shot
        // the camera has already framed, not around a fixed axis.
        let cam = Mat4::rotate_z(rot.z.to_radians())
            * Mat4::rotate_y(rot.y.to_radians())
            * Mat4::rotate_x(rot.x.to_radians());
        Projector {
            eye: kurbo::Point::new(width / 2.0 + pos.x, height / 2.0 + pos.y),
            // `-z`, guarded: the eye is in front of the plane (negative z), so a
            // positive distance is its negation. At or behind the plane the
            // divide is meaningless; a hair of distance keeps a frame drawing
            // rather than filling with NaNs while you drag the camera through.
            distance: (-pos.z).max(1e-3),
            orbit: orbit * cam,
        }
    }

    /// The eye-to-plane distance at this frame — what the comp bar edits as
    /// "eye", derived from the object's depth so the two can never disagree.
    pub fn eye_distance(&self, ctx: &mut EvalCtx) -> f64 {
        -self.position.resolve(ctx).z
    }
}

/// A camera resolved at one frame: where the eye is, how far back, and which
/// way the *viewer* is standing.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Projector {
    /// The optical axis' landing point in composition space — the comp centre.
    /// The one point depth does not move, and so the point everything converges
    /// toward as it recedes.
    pub eye: kurbo::Point,
    pub distance: f64,
    /// **The viewport orbit** — a rotation of the whole composition about the
    /// eye, applied before the perspective divide.
    ///
    /// This is not part of the composition. It is where the *editor* is
    /// standing, the same split Blender draws between its viewport camera and
    /// its render camera: a render always uses [`Mat4::IDENTITY`] here, so what
    /// ships is unaffected by where you happened to be looking from.
    ///
    /// It exists because a straight-down view cannot show rotation about X or
    /// Y at all. Those rings live in planes containing the depth axis, so
    /// looking along that axis puts them exactly edge-on — they are lines, and
    /// no gizmo drawing can honestly make them otherwise. Tilting the viewer is
    /// the only thing that opens them up.
    ///
    /// Note that orbiting makes every layer non-screen-parallel, so the whole
    /// scene takes the foreshortening path. That is correct rather than
    /// unfortunate: tipped away from the viewer is exactly what those layers
    /// now are. At identity it costs nothing.
    pub orbit: Mat4,
}

impl Projector {
    /// Composition space → viewed composition space: the orbit, taken about the
    /// eye rather than about the origin, so tilting the view pivots around what
    /// you are looking at instead of swinging the frame away.
    ///
    /// Composed *into* the chain ahead of the perspective divide, which is what
    /// makes the orbit a genuine change of viewpoint rather than a second,
    /// competing projection. At identity it is a no-op and every layer keeps
    /// the flat fast path.
    pub fn view(&self) -> Mat4 {
        if self.orbit == Mat4::IDENTITY {
            return Mat4::IDENTITY;
        }
        let e = Vec3::new(self.eye.x, self.eye.y, 0.0);
        Mat4::translate(e) * self.orbit * Mat4::translate(-e)
    }

    /// The layer-local → screen projective map for a world matrix.
    ///
    /// This is the exact projection, unlike [`Self::project`], which collapses
    /// a layer to a single depth. Used for the layers that need foreshortening;
    /// [`Homography::is_affine`] says when it would have made no difference.
    pub fn homography(&self, m: &crate::mat4::Mat4) -> crate::warp::Homography {
        crate::warp::homography_for(&(self.view() * *m), self.eye, self.distance)
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
        // to itself — but only while the viewer is standing straight on. Orbit
        // and even a flat layer has been tipped away from them.
        if let (Xf::Flat(a), true) = (xf, self.orbit == Mat4::IDENTITY) {
            return Some(Xf::Flat(a));
        }
        let m = self.view() * xf.to_mat4();
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

/// Widen a legacy scalar `distance` into a full eye position: centred in x/y,
/// with the depth as `-distance`. Keyframe timing is preserved, so an animated
/// dolly upgrades into an animated depth channel unchanged.
fn deepen_distance(distance: Value<f64>) -> Value<Vec3> {
    match distance {
        Value::Const(d) => Value::Const(Vec3::new(0.0, 0.0, -d)),
        Value::Keyframed(track) => {
            Value::Keyframed(track.map_value(|d| Vec3::new(0.0, 0.0, -d)))
        }
        Value::Expr(e) => Value::Expr(crate::expr::Expr::Vec3 {
            x: Box::new(crate::expr::Expr::num(0.0)),
            y: Box::new(crate::expr::Expr::num(0.0)),
            z: Box::new(crate::expr::Expr::un(crate::expr::UnOp::Neg, e)),
        }),
    }
}

/// Depths closer to the eye than this are treated as *at* it. Guards the divide
/// without pretending a layer 0.0001px in front of the lens is meaningful.
const EPSILON: f64 = 1e-6;

#[cfg(test)]
mod tests {
    use super::*;

    fn cam() -> Projector {
        Projector {
            eye: kurbo::Point::new(960.0, 540.0),
            distance: 1000.0,
            orbit: Mat4::IDENTITY,
        }
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

    /// The orbit is a change of *viewpoint*, so at identity it must cost
    /// nothing and change nothing — a render is exactly what it always was.
    #[test]
    fn an_identity_orbit_is_a_no_op() {
        let c = cam();
        assert_eq!(c.view(), Mat4::IDENTITY);
        let a = kurbo::Affine::translate((40.0, 15.0));
        assert!(c.project(Xf::Flat(a)).unwrap().is_flat(), "still on the fast path");
    }

    /// Orbiting tips everything away from the viewer, so a layer that was
    /// screen-parallel no longer is. That is the whole point — it is what opens
    /// the X and Y rotation rings from edge-on lines into ellipses — but it does
    /// mean the flat fast path is given up while the view is turned.
    #[test]
    fn orbiting_tips_even_a_flat_layer() {
        let mut c = cam();
        c.orbit = Mat4::rotate_x(0.5);
        let a = kurbo::Affine::translate((40.0, 15.0));
        assert!(!c.project(Xf::Flat(a)).unwrap().is_flat());
    }

    /// The orbit pivots about the eye, not the origin: what you are looking at
    /// stays put while the world turns around it.
    #[test]
    fn the_orbit_pivots_about_the_eye() {
        let mut c = cam();
        c.orbit = Mat4::rotate_y(0.4);
        let at_eye = Mat4::translate(Vec3::new(EYE.x, EYE.y, 0.0));
        let viewed = c.view().transform_point(Vec3::new(EYE.x, EYE.y, 0.0));
        assert!((viewed.x - EYE.x).abs() < 1e-9, "the eye point held still: {viewed:?}");
        assert!((viewed.y - EYE.y).abs() < 1e-9);
        assert!((viewed.z).abs() < 1e-9);
        let _ = at_eye;
    }

    /// The whole object generalisation must not disturb the fixed camera it
    /// replaced: on the axis, looking straight on, the projection is identical.
    #[test]
    fn a_default_camera_matches_the_old_fixed_one() {
        let cam = Camera::from_distance(1000.0);
        let mut ctx = crate::expr::EvalCtx::at(0.0);
        let p = cam.resolve(&mut ctx, 1920.0, 1080.0);
        assert_eq!(p.eye, kurbo::Point::new(960.0, 540.0));
        assert_eq!(p.distance, 1000.0);
        assert_eq!(p.orbit, Mat4::IDENTITY);
    }

    /// A pre-object `.pbc` stored only `distance`. It must load as a camera
    /// sitting that far in front on the axis — the eye's depth is the distance
    /// negated — with its dolly preserved.
    #[test]
    fn a_legacy_distance_only_camera_loads_as_an_object() {
        let mut cam: Camera = serde_json::from_str(r#"{"distance": {"Const": 800.0}}"#).unwrap();
        cam.migrate_frames(60.0);
        let mut ctx = crate::expr::EvalCtx::at(0.0);
        let pos = cam.position.resolve(&mut ctx);
        assert_eq!(pos, Vec3::new(0.0, 0.0, -800.0), "depth is -distance, centred");
        assert_eq!(cam.eye_distance(&mut ctx), 800.0);
    }

    /// Moving the camera in x/y slides the eye, so the vanishing point moves and
    /// depths pull apart — parallax, the reason a camera is an object.
    #[test]
    fn panning_the_camera_moves_the_eye() {
        let cam = Camera {
            position: Value::constant(Vec3::new(120.0, -40.0, -1000.0)),
            rotation: Value::constant(Vec3::ZERO),
            legacy_distance: None,
        };
        let mut ctx = crate::expr::EvalCtx::at(0.0);
        let p = cam.resolve(&mut ctx, 1920.0, 1080.0);
        assert_eq!(p.eye, kurbo::Point::new(960.0 + 120.0, 540.0 - 40.0));
    }

    /// Dollying the camera toward the plane shortens the eye-to-plane distance,
    /// strengthening perspective; pulling back lengthens it.
    #[test]
    fn dollying_the_camera_changes_the_distance() {
        let mut ctx = crate::expr::EvalCtx::at(0.0);
        let near = Camera { position: Value::constant(Vec3::new(0.0, 0.0, -300.0)), rotation: Value::constant(Vec3::ZERO), legacy_distance: None };
        let far = Camera { position: Value::constant(Vec3::new(0.0, 0.0, -3000.0)), rotation: Value::constant(Vec3::ZERO), legacy_distance: None };
        assert!(near.resolve(&mut ctx, 100.0, 100.0).distance < far.resolve(&mut ctx, 100.0, 100.0).distance);
    }

    /// A camera crossing the plane cannot divide by zero: the distance is
    /// clamped so a drag through it holds rather than filling the frame with
    /// NaNs.
    #[test]
    fn a_camera_on_the_plane_does_not_divide_by_zero() {
        let cam = Camera { position: Value::constant(Vec3::ZERO), rotation: Value::constant(Vec3::ZERO), legacy_distance: None };
        let mut ctx = crate::expr::EvalCtx::at(0.0);
        assert!(cam.resolve(&mut ctx, 100.0, 100.0).distance > 0.0);
    }

    /// The camera's own rotation renders, unlike the editor orbit: it is baked
    /// into the projector's orbit, so a straight-on evaluate already sees it.
    #[test]
    fn camera_rotation_is_part_of_the_projection() {
        let cam = Camera {
            position: Value::constant(Vec3::new(0.0, 0.0, -1000.0)),
            rotation: Value::constant(Vec3::new(0.0, 30.0, 0.0)),
            legacy_distance: None,
        };
        let mut ctx = crate::expr::EvalCtx::at(0.0);
        assert_ne!(cam.resolve(&mut ctx, 100.0, 100.0).orbit, Mat4::IDENTITY);
    }
}
