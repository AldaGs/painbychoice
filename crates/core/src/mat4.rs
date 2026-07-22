//! A 4×4 matrix, and the two-tier transform ([`Xf`]) the 2.5D pipeline turns on.
//!
//! # Why there are two tiers
//!
//! A 2D affine is six numbers and, crucially, **closed under composition**: a
//! parent affine times a child affine is still an affine, so the whole scene
//! graph flattens into one matrix per layer and vello draws it directly. That
//! property is what [`crate::eval`] is built on.
//!
//! Rotating a layer about X or Y breaks it. A card turned about its Y axis is a
//! *trapezoid* on screen — a homography, eight degrees of freedom, not six —
//! and no affine can express it. Such a layer has to be rendered to an offscreen
//! target and drawn as a projected quad.
//!
//! So [`Xf`] keeps the two cases apart in the type system rather than paying for
//! the general case everywhere. [`Xf::Flat`] is the old engine, unchanged and
//! undegraded: a document with no depth in it never constructs a `Mat4` at all.
//! [`Xf::Spatial`] is the escalation, and it is *contagious* — a 3D parent makes
//! its children spatial, because their placement now depends on a rotation the
//! plane cannot hold.

use crate::vec3::Vec3;

/// Column-major 4×4, `m[col * 4 + row]` — the OpenGL/wgpu convention, so this
/// uploads to a uniform buffer without a transpose when the projected pipeline
/// lands.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Mat4(pub [f64; 16]);

impl Mat4 {
    pub const IDENTITY: Mat4 = Mat4([
        1.0, 0.0, 0.0, 0.0, //
        0.0, 1.0, 0.0, 0.0, //
        0.0, 0.0, 1.0, 0.0, //
        0.0, 0.0, 0.0, 1.0,
    ]);

    pub fn translate(v: Vec3) -> Mat4 {
        let mut m = Mat4::IDENTITY;
        m.0[12] = v.x;
        m.0[13] = v.y;
        m.0[14] = v.z;
        m
    }

    pub fn scale(v: Vec3) -> Mat4 {
        let mut m = Mat4::IDENTITY;
        m.0[0] = v.x;
        m.0[5] = v.y;
        m.0[10] = v.z;
        m
    }

    pub fn rotate_x(rad: f64) -> Mat4 {
        let (s, c) = rad.sin_cos();
        let mut m = Mat4::IDENTITY;
        m.0[5] = c;
        m.0[6] = s;
        m.0[9] = -s;
        m.0[10] = c;
        m
    }

    pub fn rotate_y(rad: f64) -> Mat4 {
        let (s, c) = rad.sin_cos();
        let mut m = Mat4::IDENTITY;
        m.0[0] = c;
        m.0[2] = -s;
        m.0[8] = s;
        m.0[10] = c;
        m
    }

    /// Rotation in the plane — the only one a pre-2.5D document ever had, and
    /// the one that still lowers to a `kurbo::Affine`.
    pub fn rotate_z(rad: f64) -> Mat4 {
        let (s, c) = rad.sin_cos();
        let mut m = Mat4::IDENTITY;
        m.0[0] = c;
        m.0[1] = s;
        m.0[4] = -s;
        m.0[5] = c;
        m
    }

    /// Promote a planar affine. The affine's six numbers land in the X/Y block;
    /// Z passes through untouched, which is what makes `Flat` composed into
    /// `Spatial` mean the same thing it did before.
    pub fn from_affine(a: kurbo::Affine) -> Mat4 {
        let c = a.as_coeffs(); // [a b c d e f] = the two columns then translation
        let mut m = Mat4::IDENTITY;
        m.0[0] = c[0];
        m.0[1] = c[1];
        m.0[4] = c[2];
        m.0[5] = c[3];
        m.0[12] = c[4];
        m.0[13] = c[5];
        m
    }

    /// Recover a `kurbo::Affine` **if** this matrix never left the plane: no
    /// depth, no out-of-plane rotation, no perspective. The gate that lets a
    /// spatial subtree fall back to the cheap renderer once its 3D-ness cancels
    /// out (a layer rotated 90° and back, a camera-less comp).
    pub fn as_affine(&self) -> Option<kurbo::Affine> {
        let m = &self.0;
        let planar = m[2] == 0.0
            && m[6] == 0.0
            && m[8] == 0.0
            && m[9] == 0.0
            && m[10] == 1.0
            && m[14] == 0.0
            && m[3] == 0.0
            && m[7] == 0.0
            && m[11] == 0.0
            && m[15] == 1.0;
        planar.then(|| kurbo::Affine::new([m[0], m[1], m[4], m[5], m[12], m[13]]))
    }

    /// Recover the in-plane affine of a matrix that **may** carry depth, as
    /// long as it is still screen-parallel: no out-of-plane rotation and no
    /// perspective, but any `z` translation allowed.
    ///
    /// This is the billboard test. [`Self::as_affine`] asks "did this never
    /// leave the plane at all", which a layer merely pushed back in Z fails —
    /// yet such a layer is exactly the one a camera can draw with a plain
    /// scale. The depth is dropped here because the caller has already spent it
    /// on the projection; calling this without doing so would silently flatten
    /// the scene.
    pub fn as_affine_ignoring_depth(&self) -> Option<kurbo::Affine> {
        let m = &self.0;
        let parallel = m[2] == 0.0
            && m[6] == 0.0
            && m[8] == 0.0
            && m[9] == 0.0
            && m[3] == 0.0
            && m[7] == 0.0
            && m[11] == 0.0
            && m[15] == 1.0;
        parallel.then(|| kurbo::Affine::new([m[0], m[1], m[4], m[5], m[12], m[13]]))
    }

    /// Transform a point (w = 1), dividing through by w so a later perspective
    /// matrix works without a second code path.
    pub fn transform_point(&self, p: Vec3) -> Vec3 {
        let m = &self.0;
        let x = m[0] * p.x + m[4] * p.y + m[8] * p.z + m[12];
        let y = m[1] * p.x + m[5] * p.y + m[9] * p.z + m[13];
        let z = m[2] * p.x + m[6] * p.y + m[10] * p.z + m[14];
        let w = m[3] * p.x + m[7] * p.y + m[11] * p.z + m[15];
        if w == 1.0 || w == 0.0 {
            Vec3::new(x, y, z)
        } else {
            Vec3::new(x / w, y / w, z / w)
        }
    }
}

impl std::ops::Mul for Mat4 {
    type Output = Mat4;
    fn mul(self, rhs: Mat4) -> Mat4 {
        let (a, b) = (&self.0, &rhs.0);
        let mut out = [0.0; 16];
        for col in 0..4 {
            for row in 0..4 {
                out[col * 4 + row] = (0..4).map(|k| a[k * 4 + row] * b[col * 4 + k]).sum();
            }
        }
        Mat4(out)
    }
}

/// A resolved transform: either a plain 2D affine or a full spatial matrix.
///
/// See the module docs for why this is an enum and not just a `Mat4`. The short
/// version: `Flat` is the entire existing engine and must stay free.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Xf {
    /// Planar. Composes with `*`, hands straight to vello.
    Flat(kurbo::Affine),
    /// Left the plane. Needs the camera and the render-to-texture path; a
    /// `Flat` composed with one of these becomes one of these.
    Spatial(Mat4),
}

impl Xf {
    pub const IDENTITY: Xf = Xf::Flat(kurbo::Affine::IDENTITY);

    /// The 4×4 view, promoting a flat transform if needed.
    pub fn to_mat4(self) -> Mat4 {
        match self {
            Xf::Flat(a) => Mat4::from_affine(a),
            Xf::Spatial(m) => m,
        }
    }

    /// The affine view, `None` when this transform genuinely needs projection.
    /// Callers that can only draw affines (the current renderer, the gizmo
    /// overlay, hit-testing) ask with this and take the `None` as "not my job".
    pub fn to_affine(self) -> Option<kurbo::Affine> {
        match self {
            Xf::Flat(a) => Some(a),
            Xf::Spatial(m) => m.as_affine(),
        }
    }

    /// The affine view for code that must produce *something* — falls back to
    /// the spatial matrix's X/Y block, i.e. an orthographic projection with the
    /// depth dropped. Wrong in the same way the old engine was wrong (it draws
    /// a rotated card as its unforeshortened self) but never blanks a frame.
    pub fn to_affine_lossy(self) -> kurbo::Affine {
        self.to_affine().unwrap_or_else(|| {
            let m = self.to_mat4().0;
            kurbo::Affine::new([m[0], m[1], m[4], m[5], m[12], m[13]])
        })
    }

    pub fn is_flat(self) -> bool {
        matches!(self, Xf::Flat(_))
    }
}

/// Parent × child. `Flat * Flat` stays flat — the property the whole fast path
/// rests on — and anything else escalates.
impl std::ops::Mul for Xf {
    type Output = Xf;
    fn mul(self, rhs: Xf) -> Xf {
        match (self, rhs) {
            (Xf::Flat(a), Xf::Flat(b)) => Xf::Flat(a * b),
            (a, b) => Xf::Spatial(a.to_mat4() * b.to_mat4()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The invariant the performance story depends on: composing planar
    /// transforms never allocates a 4×4 or leaves the affine path.
    #[test]
    fn flat_composition_stays_flat() {
        let a = Xf::Flat(kurbo::Affine::translate((10.0, 0.0)));
        let b = Xf::Flat(kurbo::Affine::rotate(0.5));
        assert!((a * b).is_flat());
    }

    #[test]
    fn spatial_is_contagious() {
        let flat = Xf::Flat(kurbo::Affine::IDENTITY);
        let spatial = Xf::Spatial(Mat4::rotate_y(0.4));
        assert!(!(flat * spatial).is_flat());
        assert!(!(spatial * flat).is_flat());
    }

    /// A promoted affine round-trips exactly, so escalating a subtree and then
    /// asking for the affine back is lossless when no depth was ever added.
    #[test]
    fn affine_round_trips_through_mat4() {
        let a = kurbo::Affine::translate((3.0, -7.0)) * kurbo::Affine::rotate(0.9);
        let back = Mat4::from_affine(a).as_affine().expect("planar");
        for (l, r) in a.as_coeffs().iter().zip(back.as_coeffs().iter()) {
            assert!((l - r).abs() < 1e-12, "{l} vs {r}");
        }
    }

    /// Z rotation is in-plane: it must stay recoverable as an affine, or every
    /// existing document would fall off the fast path.
    #[test]
    fn z_rotation_is_still_planar() {
        assert!(Mat4::rotate_z(0.7).as_affine().is_some());
        assert!(Mat4::rotate_y(0.7).as_affine().is_none());
        assert!(Mat4::rotate_x(0.7).as_affine().is_none());
    }

    #[test]
    fn z_rotation_matches_kurbo() {
        let m = Mat4::rotate_z(0.7).as_affine().unwrap();
        let k = kurbo::Affine::rotate(0.7);
        for (l, r) in m.as_coeffs().iter().zip(k.as_coeffs().iter()) {
            assert!((l - r).abs() < 1e-12, "{l} vs {r}");
        }
    }
}
