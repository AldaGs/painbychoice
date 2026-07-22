//! The vector every transform channel is made of.
//!
//! The engine used to be built on `kurbo::Vec2` end to end. It still is for
//! *geometry* — a path point, a rect's size, anything that lives inside a
//! layer's own plane is genuinely two-dimensional and stays so. What moved to
//! three components is the **transform**: position, anchor, scale, rotation.
//! That is the 2.5D seam. A layer's contents are flat; where the flat thing
//! sits, and how it is turned, is not.
//!
//! `z` is the depth axis, positive going *away* from the viewer (the same sense
//! as the composition's camera looks down +Z). Every `.pbc` written before this
//! existed deserializes with `z = 0`, which is exactly the old behaviour — see
//! the `Deserialize` note below.

use std::ops::{Add, AddAssign, Div, Mul, Neg, Sub, SubAssign};

use serde::{Deserialize, Serialize};

use crate::value::Animatable;

/// A 3-component vector: the dimensionality of a transform channel.
///
/// `#[serde(default)]` on `z` alone is the entire file-format migration. A
/// pre-2.5D `.pbc` stores `{"x": 10, "y": 20}` — exactly what `kurbo::Vec2`
/// wrote — and reads back as `z = 0`, a flat layer in the plane it was authored
/// in. Nothing needs a version bump and nothing needs rewriting on load.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Vec3 {
    pub x: f64,
    pub y: f64,
    #[serde(default)]
    pub z: f64,
}

impl Vec3 {
    pub const ZERO: Vec3 = Vec3 { x: 0.0, y: 0.0, z: 0.0 };
    /// The identity for `scale` — not for position. Spelled out because a
    /// `Vec3::ZERO` default on a scale channel collapses the layer to nothing,
    /// and that bug is invisible until something is scaled.
    pub const ONE: Vec3 = Vec3 { x: 1.0, y: 1.0, z: 1.0 };

    pub const fn new(x: f64, y: f64, z: f64) -> Self {
        Self { x, y, z }
    }

    /// A vector in the plane: `z = 0`. The constructor for everything that was
    /// a `Vec2` literal before, and the one the 2D authoring paths use.
    pub const fn flat(x: f64, y: f64) -> Self {
        Self { x, y, z: 0.0 }
    }

    /// The same value on all three axes — `splat(2.0)` is a uniform scale.
    pub const fn splat(v: f64) -> Self {
        Self { x: v, y: v, z: v }
    }

    /// Drop the depth. Used at the boundary where a 3D channel meets 2D
    /// geometry or a flat-path renderer, and by the `Vec2` expression bridge.
    pub const fn xy(self) -> kurbo::Vec2 {
        kurbo::Vec2::new(self.x, self.y)
    }

    /// Lift a planar vector to `z = 0`.
    pub const fn from_xy(v: kurbo::Vec2) -> Self {
        Self::flat(v.x, v.y)
    }

    /// Is this vector confined to the plane? The predicate the affine fast path
    /// is decided on: a transform whose every channel is planar composes to a
    /// `kurbo::Affine` and never needs the projected pipeline.
    pub fn is_flat(self) -> bool {
        self.z == 0.0
    }

    /// Component-wise map — the shape [`crate::expr::ExprValue`] arithmetic
    /// wants.
    pub fn map(self, f: impl Fn(f64) -> f64) -> Self {
        Self::new(f(self.x), f(self.y), f(self.z))
    }

    /// Component-wise combine.
    pub fn zip(self, other: Self, f: impl Fn(f64, f64) -> f64) -> Self {
        Self::new(f(self.x, other.x), f(self.y, other.y), f(self.z, other.z))
    }
}

impl Default for Vec3 {
    fn default() -> Self {
        Vec3::ZERO
    }
}

impl From<kurbo::Vec2> for Vec3 {
    fn from(v: kurbo::Vec2) -> Self {
        Vec3::from_xy(v)
    }
}

impl From<Vec3> for kurbo::Vec2 {
    fn from(v: Vec3) -> Self {
        v.xy()
    }
}

impl Add for Vec3 {
    type Output = Vec3;
    fn add(self, rhs: Vec3) -> Vec3 {
        self.zip(rhs, |a, b| a + b)
    }
}
impl Sub for Vec3 {
    type Output = Vec3;
    fn sub(self, rhs: Vec3) -> Vec3 {
        self.zip(rhs, |a, b| a - b)
    }
}
impl Mul<f64> for Vec3 {
    type Output = Vec3;
    fn mul(self, rhs: f64) -> Vec3 {
        self.map(|a| a * rhs)
    }
}
impl Mul<Vec3> for f64 {
    type Output = Vec3;
    fn mul(self, rhs: Vec3) -> Vec3 {
        rhs * self
    }
}
impl Div<f64> for Vec3 {
    type Output = Vec3;
    fn div(self, rhs: f64) -> Vec3 {
        self.map(|a| a / rhs)
    }
}
impl Neg for Vec3 {
    type Output = Vec3;
    fn neg(self) -> Vec3 {
        self.map(|a| -a)
    }
}
impl AddAssign for Vec3 {
    fn add_assign(&mut self, rhs: Vec3) {
        *self = *self + rhs;
    }
}
impl SubAssign for Vec3 {
    fn sub_assign(&mut self, rhs: Vec3) {
        *self = *self - rhs;
    }
}

impl Animatable for Vec3 {
    fn lerp(a: &Self, b: &Self, t: f64) -> Self {
        *a + (*b - *a) * t
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole file-format migration, asserted: a transform channel written
    /// before the z axis existed reads back flat, not garbage and not an error.
    #[test]
    fn legacy_two_component_json_loads_flat() {
        let v: Vec3 = serde_json::from_str(r#"{"x": 10.0, "y": 20.0}"#).unwrap();
        assert_eq!(v, Vec3::new(10.0, 20.0, 0.0));
        assert!(v.is_flat());
    }

    #[test]
    fn round_trips_through_json_with_z() {
        let v = Vec3::new(1.0, 2.0, 3.0);
        let s = serde_json::to_string(&v).unwrap();
        assert_eq!(serde_json::from_str::<Vec3>(&s).unwrap(), v);
    }

    #[test]
    fn lerps_all_three_axes() {
        let a = Vec3::new(0.0, 0.0, 0.0);
        let b = Vec3::new(10.0, 20.0, 30.0);
        assert_eq!(Vec3::lerp(&a, &b, 0.5), Vec3::new(5.0, 10.0, 15.0));
    }
}
