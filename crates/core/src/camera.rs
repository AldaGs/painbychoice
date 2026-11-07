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
//! # The projection
//!
//! A real camera: an eye at `position` (relative to the comp centre, `z`
//! negative in front of the `z = 0` plane), turned by tilt / pan / roll, with
//! a lens of `zoom` pixels. World → camera space is the inverse of that
//! placement, so trucking slides the whole shot (nearer layers more) and a pan
//! turns it about the eye. The default camera sits `zoom` in front of the plane
//! on the axis, so a layer at `z = 0` renders at exactly 1:1 — switching a
//! camera on moves nothing.
//!
//! Internally the camera is one matrix ([`Projector::camera`]) into a fixed
//! "viewed space" with the eye on the axis at `(centre, -zoom)`. Everything
//! downstream — [`Projector::project`], the homography warp for tipped layers,
//! culling, the gizmo's ray — works in that space and never needs to know where
//! the camera is.
//!
//! A screen-parallel layer projects to a uniform scale about the frame centre,
//! which stays an affine and keeps the fast path; a layer tipped relative to the
//! camera is foreshortened exactly by `crate::warp`.

use serde::{Deserialize, Serialize};

use crate::expr::EvalCtx;
use crate::mat4::{Mat4, Xf};
use crate::vec3::Vec3;
use crate::value::Value;

/// A composition's camera. Present means the comp projects; absent means it is
/// flat, and the engine behaves exactly as it did before depth existed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(from = "CameraRepr")]
pub struct Camera {
    /// Where the eye sits, as an **object in the composition** — not a lone
    /// setting.
    ///
    /// `x`/`y` offset the eye from the comp centre — a truck, which slides the
    /// shot and pulls parallax between depths. `z` is the dolly: *signed depth*,
    /// negative in front of the `z = 0` plane. Pushing in at a fixed lens makes
    /// things bigger and the perspective stronger; the lens is [`Self::zoom`].
    ///
    /// Every channel is animatable, so a dolly or a truck is a keyframed value
    /// like any other. The default sits the eye on the axis, `default_distance`
    /// in front, which reproduces the fixed camera this replaced.
    #[serde(default = "default_camera_position")]
    pub position: Value<Vec3>,
    /// The camera's orientation, euler degrees, default looking straight down
    /// `+Z` at the composition. Turns the camera about its own eye, like a
    /// tripod head. Unlike the editor's viewport orbit, this one renders.
    ///
    /// Three separate tracks, like a layer's: `rotation` is the roll (Z),
    /// `rotation_x` the tilt and `rotation_y` the pan, each keyed and eased on
    /// its own. Applied Z · Y · X.
    pub rotation: Value<f64>,
    pub rotation_x: Value<f64>,
    pub rotation_y: Value<f64>,
    /// A pre-object camera stored only a scalar `distance`. It cannot
    /// deserialize into `position` (a number is not a vector), so it lands here
    /// and [`Camera::migrate_frames`] folds it into `position.z` as `-distance`.
    /// Never written back out.
    /// The lens: focal length in composition pixels — how far from the eye the
    /// image plane sits. Larger is more telephoto (narrower view, flatter
    /// depth); smaller is wider. Independent of the dolly, so a push-in and a
    /// zoom are different shots, and moving both at once is a dolly zoom.
    ///
    /// `None` is the pre-lens camera, kept for files written before the lens
    /// existed: the focal length follows the dolly (`-position.z`), so the
    /// `z = 0` plane always renders at 1:1 and those files frame as they did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zoom: Option<Value<f64>>,
    /// Where the camera looks — After Effects' two-node camera. Same space as
    /// [`Self::position`] (offset from the comp centre, `z` into the scene).
    ///
    /// When set, the camera is aimed at this point every frame, staying upright
    /// (no roll), and the tilt / pan / roll channels turn it further from
    /// there. So animating the eye orbits a subject that stays framed, and
    /// animating this point re-aims without touching the eye. `None` is a
    /// free (one-node) camera aimed by its rotation alone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub point_of_interest: Option<Value<Vec3>>,
    #[serde(default, rename = "distance", skip_serializing)]
    legacy_distance: Option<Value<f64>>,
}

/// What a `.pbc` may hold for a camera: rotation as one joined 3-vector (before
/// the axes were split) or as three channels. Split here, like a layer's.
#[derive(Deserialize)]
struct CameraRepr {
    #[serde(default = "default_camera_position")]
    position: Value<Vec3>,
    #[serde(default)]
    rotation: Option<crate::node::RotationRepr>,
    #[serde(default)]
    rotation_x: Option<Value<f64>>,
    #[serde(default)]
    rotation_y: Option<Value<f64>>,
    #[serde(default)]
    zoom: Option<Value<f64>>,
    #[serde(default)]
    point_of_interest: Option<Value<Vec3>>,
    #[serde(default, rename = "distance")]
    legacy_distance: Option<Value<f64>>,
}

impl From<CameraRepr> for Camera {
    fn from(r: CameraRepr) -> Self {
        use crate::node::RotationRepr as R;
        let (x, y, z) = match r.rotation {
            None => (zero_angle(), zero_angle(), zero_angle()),
            Some(R::Planar(z)) => (zero_angle(), zero_angle(), z),
            Some(R::Joined(v)) => crate::node::split_rotation(v),
        };
        Self {
            position: r.position,
            rotation: z,
            rotation_x: r.rotation_x.unwrap_or(x),
            rotation_y: r.rotation_y.unwrap_or(y),
            zoom: r.zoom,
            point_of_interest: r.point_of_interest,
            legacy_distance: r.legacy_distance,
        }
    }
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

fn zero_angle() -> Value<f64> {
    Value::constant(0.0)
}

impl Camera {
    /// The default eye distance for a comp of this size: twice the diagonal.
    /// Chosen so a layer displaced by a comp-width of depth shrinks noticeably
    /// but not grotesquely — a working starting point, not a physical constant.
    pub fn default_distance(width: f64, height: f64) -> f64 {
        2.0 * width.hypot(height)
    }

    pub fn new(width: f64, height: f64) -> Self {
        Self::from_distance(Self::default_distance(width, height))
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
            rotation: zero_angle(),
            rotation_x: zero_angle(),
            rotation_y: zero_angle(),
            // The lens matches the dolly, so the z = 0 plane starts at 1:1.
            zoom: Some(Value::constant(distance)),
            point_of_interest: None,
            legacy_distance: None,
        }
    }

    pub(crate) fn migrate_frames(&mut self, fps: f64) {
        if let Some(distance) = self.legacy_distance.take() {
            self.position = deepen_distance(distance);
        }
        self.position.migrate_frames(fps);
        self.rotation.migrate_frames(fps);
        self.rotation_x.migrate_frames(fps);
        self.rotation_y.migrate_frames(fps);
        if let Some(z) = &mut self.zoom {
            z.migrate_frames(fps);
        }
        if let Some(p) = &mut self.point_of_interest {
            p.migrate_frames(fps);
        }
    }

    pub(crate) fn retime(&mut self, ratio: f64) {
        self.position.retime(ratio);
        self.rotation.retime(ratio);
        self.rotation_x.retime(ratio);
        self.rotation_y.retime(ratio);
        if let Some(z) = &mut self.zoom {
            z.retime(ratio);
        }
        if let Some(p) = &mut self.point_of_interest {
            p.retime(ratio);
        }
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
        let (rx, ry, rz) = (
            self.rotation_x.resolve(ctx).to_radians(),
            self.rotation_y.resolve(ctx).to_radians(),
            self.rotation.resolve(ctx).to_radians(),
        );
        let focal = self.focal_length(ctx).max(1e-3);
        let centre = kurbo::Point::new(width / 2.0, height / 2.0);
        // A real camera: world → camera space is the inverse of the camera's
        // own placement (translate to the eye, then rotate by the inverse of
        // its orientation), so trucking slides the whole shot and a pan turns
        // it about the eye — not about a point on the z = 0 plane.
        //
        // Then into the "viewed space" every consumer already speaks
        // (`Projector::project`, the homography, the gizmo ray): eye on the
        // axis at `(centre, -focal)`, image plane at z = 0. Expressing the
        // camera as one more matrix ahead of that fixed projection is what
        // keeps the rest of the pipeline untouched.
        let eye = Vec3::new(centre.x + pos.x, centre.y + pos.y, pos.z);
        let mut camera = Mat4::translate(Vec3::new(centre.x, centre.y, -focal));
        if (rx, ry, rz) != (0.0, 0.0, 0.0) {
            camera = camera * Mat4::rotate_x(-rx) * Mat4::rotate_y(-ry) * Mat4::rotate_z(-rz);
        }
        // Aim at the point of interest first (the user's rotation is relative
        // to that aim, so it sits inside it in the inverse): pan, then tilt,
        // never roll, so the horizon stays level however the eye moves.
        if let Some(poi) = &self.point_of_interest {
            let d = poi.resolve(ctx) - pos;
            if d.x != 0.0 || d.y != 0.0 || d.z != 0.0 {
                let (pan, tilt) = look_angles(d);
                camera = camera * Mat4::rotate_x(-tilt) * Mat4::rotate_y(-pan);
            }
        }
        camera = camera * Mat4::translate(-eye);
        Projector { eye: centre, distance: focal, orbit, camera }
    }

    /// The lens at this frame, in composition pixels: the stored zoom, or for a
    /// pre-lens camera, the dolly distance (see [`Camera::zoom`]).
    pub fn focal_length(&self, ctx: &mut EvalCtx) -> f64 {
        match &self.zoom {
            Some(z) => z.resolve(ctx),
            None => -self.position.resolve(ctx).z,
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
    /// The comp camera itself: composition space → viewed space (eye on the
    /// axis at `(eye, -distance)`, looking down +Z). Identity for a camera
    /// sitting on the axis with its lens equal to its dolly — every camera
    /// from before the lens existed — so those still take the flat path.
    pub camera: Mat4,
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
            return self.camera;
        }
        let e = Vec3::new(self.eye.x, self.eye.y, 0.0);
        Mat4::translate(e) * self.orbit * Mat4::translate(-e) * self.camera
    }

    /// The layer-local → screen projective map for a world matrix.
    ///
    /// This is the exact projection, unlike [`Self::project`], which collapses
    /// a layer to a single depth. Used for the layers that need foreshortening;
    /// [`Homography::is_affine`] says when it would have made no difference.
    pub fn homography(&self, m: &crate::mat4::Mat4) -> crate::warp::Homography {
        crate::warp::homography_for(&(self.view() * *m), self.eye, self.distance)
    }

    /// The near plane: camera-space depth, in pixels in front of the eye, below
    /// which nothing draws. A fraction of the lens rather than a constant, so
    /// the worst magnification at the cut is the same for every lens
    /// (`distance / near` = 50x) — close enough to read as "right up against the
    /// camera" without flinging geometry to coordinates the rasterizer cannot
    /// hold.
    pub fn near(&self) -> f64 {
        (self.distance * NEAR_FRACTION).max(EPSILON)
    }

    /// A screen-parallel layer at this depth is in front of the near plane or
    /// not as a whole — every point of it shares the depth — so it is drawn or
    /// culled whole. (A tipped layer is cut instead; see [`Self::project`].)
    fn is_culled(&self, depth: f64) -> bool {
        self.distance + depth < self.near()
    }

    /// Whether a world transform's origin is in front of the near plane — the
    /// one point a layer that cannot be cut (footage, which paints into an
    /// affine rectangle) is placed by.
    pub fn origin_in_front(&self, xf: Xf) -> bool {
        !self.is_culled((self.view() * xf.to_mat4()).0[14])
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
        if let (Xf::Flat(a), true) = (xf, self.orbit == Mat4::IDENTITY && self.camera == Mat4::IDENTITY) {
            return Some(Xf::Flat(a));
        }
        let m = self.view() * xf.to_mat4();
        // The translation column: where this layer's origin landed in comp
        // space, depth included.
        let depth = m.0[14];
        if self.is_culled(depth) {
            // A tipped layer can run from behind the camera to in front of it
            // — a floor, a wall alongside — so its origin says nothing about
            // whether any of it shows. It is not culled: the warp cuts its
            // geometry at the near plane. The unscaled view is only a stand-in
            // for the affine readers (overlay, hit-testing).
            if m.as_affine_ignoring_depth().is_none() {
                return Some(Xf::Spatial(m));
            }
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

/// The pan (about Y) and tilt (about X), radians, that turn the camera's
/// forward axis (+Z) to point along `d`: `rotate_y(pan) · rotate_x(tilt) · Z`
/// is `d` normalised. Pan first so the result has no roll.
fn look_angles(d: Vec3) -> (f64, f64) {
    let pan = d.x.atan2(d.z);
    let tilt = (-d.y).atan2(d.x.hypot(d.z));
    (pan, tilt)
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

/// The near plane as a fraction of the lens — see [`Projector::near`].
const NEAR_FRACTION: f64 = 0.02;

#[cfg(test)]
mod tests {
    use super::*;

    fn cam() -> Projector {
        Projector {
            eye: kurbo::Point::new(960.0, 540.0),
            distance: 1000.0,
            orbit: Mat4::IDENTITY,
            camera: Mat4::IDENTITY,
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

    #[test]
    fn dollying_the_camera_changes_the_distance() {
        let mut ctx = crate::expr::EvalCtx::at(0.0);
        let near = Camera { position: Value::constant(Vec3::new(0.0, 0.0, -300.0)), ..Camera::from_distance(1.0) };
        let far = Camera { position: Value::constant(Vec3::new(0.0, 0.0, -3000.0)), ..Camera::from_distance(1.0) };
        // The lens stays put; the camera itself moves along its axis.
        let (n, f) = (near.resolve(&mut ctx, 100.0, 100.0), far.resolve(&mut ctx, 100.0, 100.0));
        assert_eq!(n.distance, f.distance, "a dolly is not a zoom");
        assert!(n.camera.0[14] < f.camera.0[14], "the scene sits closer to the near camera");
    }

    /// A camera crossing the plane cannot divide by zero: the distance is
    /// clamped so a drag through it holds rather than filling the frame with
    /// NaNs.
    #[test]
    fn a_camera_on_the_plane_does_not_divide_by_zero() {
        let cam = Camera { position: Value::constant(Vec3::ZERO), ..Camera::from_distance(1.0) };
        let mut ctx = crate::expr::EvalCtx::at(0.0);
        assert!(cam.resolve(&mut ctx, 100.0, 100.0).distance > 0.0);
    }

    /// The camera's own rotation renders, unlike the editor orbit: it is part
    /// of the camera matrix, so a straight-on evaluate already sees it, and
    /// the editor's orbit stays the editor's.
    #[test]
    fn camera_rotation_is_part_of_the_projection() {
        let cam = Camera {
            position: Value::constant(Vec3::new(0.0, 0.0, -1000.0)),
            rotation_y: Value::constant(30.0),
            ..Camera::from_distance(1.0)
        };
        let mut ctx = crate::expr::EvalCtx::at(0.0);
        let p = cam.resolve(&mut ctx, 100.0, 100.0);
        assert_ne!(p.camera, Mat4::IDENTITY);
        assert_eq!(p.orbit, Mat4::IDENTITY);
    }

    /// A camera saved with one joined rotation loads as three channels, each
    /// keeping its own value, and saves back in the split form.
    #[test]
    fn a_joined_camera_rotation_splits_per_axis() {
        let json = r#"{"position": {"Const": {"x": 0.0, "y": 0.0, "z": -900.0}},
                       "rotation": {"Const": {"x": 10.0, "y": 20.0, "z": 30.0}}}"#;
        let cam: Camera = serde_json::from_str(json).unwrap();
        assert_eq!(
            (cam.rotation_x.clone(), cam.rotation_y.clone(), cam.rotation.clone()),
            (Value::constant(10.0), Value::constant(20.0), Value::constant(30.0))
        );
        let back: Camera = serde_json::from_str(&serde_json::to_string(&cam).unwrap()).unwrap();
        assert_eq!(back, cam);
    }

    /// Where a layer origin at `p` (comp space) lands on screen.
    fn land(cam: &Camera, p: Vec3) -> Option<(f64, f64)> {
        let mut ctx = crate::expr::EvalCtx::at(0.0);
        let v = cam.resolve(&mut ctx, 1920.0, 1080.0);
        v.project(Xf::Spatial(Mat4::translate(p))).map(|x| {
            let m = x.to_mat4();
            ((m.0[12] * 1e6).round() / 1e6, (m.0[13] * 1e6).round() / 1e6)
        })
    }

    const MID: Vec3 = Vec3 { x: 960.0, y: 540.0, z: 0.0 };
    const FAR: Vec3 = Vec3 { x: 960.0, y: 540.0, z: 1000.0 };

    /// Trucking moves the shot like a real camera: everything slides the other
    /// way, the z = 0 plane by the full amount and farther layers by less.
    #[test]
    fn trucking_slides_the_whole_shot() {
        let mut cam = Camera::from_distance(1000.0);
        cam.position = Value::constant(Vec3::new(200.0, 0.0, -1000.0));
        let (near, far) = (land(&cam, MID).unwrap(), land(&cam, FAR).unwrap());
        assert_eq!(near, (760.0, 540.0), "the z = 0 plane moves the full truck");
        assert_eq!(far, (860.0, 540.0), "twice as far, half the shift");
    }

    /// A pan turns the camera about its own eye: what was dead ahead swings
    /// off to the side, and turning right moves the scene left.
    #[test]
    fn panning_turns_about_the_eye() {
        let mut cam = Camera::from_distance(1000.0);
        cam.rotation_y = Value::constant(10.0);
        let (x, _) = land(&cam, MID).unwrap();
        let expected = 960.0 - 1000.0 * 10f64.to_radians().tan();
        assert!((x - expected).abs() < 1e-6, "{x} vs {expected}");
    }

    /// The lens is independent of the dolly: doubling the zoom doubles the
    /// apparent size of the z = 0 plane without moving the camera, and a dolly
    /// at a fixed lens now changes the size.
    #[test]
    fn zoom_and_dolly_are_separate() {
        let mut ctx = crate::expr::EvalCtx::at(0.0);
        let mut cam = Camera::from_distance(1000.0);
        let off = Vec3::new(1060.0, 540.0, 0.0);
        assert_eq!(land(&cam, off).unwrap().0, 1060.0, "default lens frames z = 0 at 1:1");
        cam.zoom = Some(Value::constant(2000.0));
        assert_eq!(land(&cam, off).unwrap().0, 1160.0, "2x lens, 2x size");
        assert_eq!(cam.focal_length(&mut ctx), 2000.0);
        let mut pushed = Camera::from_distance(1000.0);
        pushed.position = Value::constant(Vec3::new(0.0, 0.0, -500.0));
        assert_eq!(land(&pushed, off).unwrap().0, 1160.0, "half the distance, 2x size");
    }

    /// A camera from before the lens existed has no zoom: its lens follows the
    /// dolly, so a keyed push-in still frames z = 0 at 1:1 as it did.
    #[test]
    fn a_pre_lens_camera_keeps_its_framing() {
        let json = r#"{"position": {"Const": {"x": 0.0, "y": 0.0, "z": -500.0}}}"#;
        let cam: Camera = serde_json::from_str(json).unwrap();
        assert!(cam.zoom.is_none());
        assert_eq!(land(&cam, Vec3::new(1060.0, 540.0, 0.0)).unwrap().0, 1060.0);
        let mut ctx = crate::expr::EvalCtx::at(0.0);
        assert_eq!(cam.resolve(&mut ctx, 1920.0, 1080.0).camera, Mat4::IDENTITY, "still the flat path");
    }

    /// Anything behind the camera — in its own space, not the comp's — is culled.
    #[test]
    fn behind_a_turned_camera_is_culled() {
        use kurbo::Shape as _;
        let mut cam = Camera::from_distance(1000.0);
        cam.rotation_y = Value::constant(180.0);
        let mut ctx = crate::expr::EvalCtx::at(0.0);
        let v = cam.resolve(&mut ctx, 1920.0, 1080.0);
        let layer = Mat4::translate(MID);
        let square = kurbo::Rect::new(-100.0, -100.0, 100.0, 100.0).to_path(0.01);
        assert!(crate::warp::warp_path(&square, &v.homography(&layer), v.near()).is_none());
    }

    /// A floor running under and past the camera is cut at the near plane:
    /// the part in front draws, nothing behind the eye does.
    #[test]
    fn a_floor_past_the_camera_is_cut_not_culled() {
        use kurbo::Shape as _;
        let cam = Camera::from_distance(1000.0);
        let mut ctx = crate::expr::EvalCtx::at(0.0);
        let v = cam.resolve(&mut ctx, 1920.0, 1080.0);
        // Origin 1500 behind the eye, tipped flat, 4000 deep toward the scene.
        let floor = Mat4::translate(Vec3::new(960.0, 800.0, -2500.0)) * Mat4::rotate_x(std::f64::consts::FRAC_PI_2);
        let xf = Xf::Spatial(floor);
        assert!(v.project(xf).is_some(), "not culled by its origin");
        assert!(!v.origin_in_front(xf));
        let rect = kurbo::Rect::new(-500.0, -4000.0, 500.0, 4000.0).to_path(0.01);
        let cut = crate::warp::warp_path(&rect, &v.homography(&floor), v.near()).expect("the front part draws");
        let b = cut.bounding_box();
        assert!(b.y1 > 1080.0 && b.y0 > 540.0, "runs from below frame up toward the horizon: {b:?}");
    }


    /// A two-node camera keeps its point of interest dead centre wherever the
    /// eye goes — above, beside, behind the subject.
    #[test]
    fn the_point_of_interest_stays_centred() {
        let target = Vec3::new(300.0, -200.0, 800.0); // offset from comp centre
        for eye in [
            Vec3::new(0.0, 0.0, -1000.0),
            Vec3::new(900.0, 0.0, -400.0),
            Vec3::new(-500.0, -700.0, -900.0),
            Vec3::new(200.0, 300.0, 2500.0), // past the subject, looking back
        ] {
            let mut cam = Camera::from_distance(1000.0);
            cam.position = Value::constant(eye);
            cam.point_of_interest = Some(Value::constant(target));
            let world = Vec3::new(960.0 + target.x, 540.0 + target.y, target.z);
            let (x, y) = land(&cam, world).expect("in front");
            assert!((x - 960.0).abs() < 1e-6 && (y - 540.0).abs() < 1e-6, "eye {eye:?} → {x},{y}");
        }
    }

    /// Aiming never rolls: a point level with the target, to its side, stays
    /// level with it on screen.
    #[test]
    fn aiming_keeps_the_horizon_level() {
        let mut cam = Camera::from_distance(1000.0);
        cam.position = Value::constant(Vec3::new(700.0, -400.0, -900.0));
        cam.point_of_interest = Some(Value::constant(Vec3::ZERO));
        let (_, y_side) = land(&cam, Vec3::new(960.0 + 50.0, 540.0, 0.0)).unwrap();
        let (_, y_mid) = land(&cam, Vec3::new(960.0, 540.0, 0.0)).unwrap();
        // The side point is off-centre in x; with no roll it may drop a little
        // from perspective but must not swing to the other side of centre.
        assert!((y_side - y_mid).abs() < 20.0, "{y_side} vs {y_mid}");
    }

    /// The camera's own rotation turns it further from the aim: pan 10° with a
    /// target dead ahead behaves exactly like pan 10° with no target.
    #[test]
    fn rotation_adds_to_the_aim() {
        let mut free = Camera::from_distance(1000.0);
        free.rotation_y = Value::constant(10.0);
        let mut aimed = free.clone();
        aimed.point_of_interest = Some(Value::constant(Vec3::new(0.0, 0.0, 500.0)));
        assert_eq!(land(&free, MID), land(&aimed, MID));
    }
}
