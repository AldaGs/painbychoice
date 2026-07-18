//! The beating heart: every animatable property is a `Value<T>` that resolves
//! to a concrete `T` at a given time. A value is never baked — it is a recipe
//! (a constant, or a keyframe track, and later an expression / parametric IR).
//! Non-destructive and non-linear scrubbing both fall out of this for free.

use serde::{Deserialize, Serialize};

/// Anything that can be interpolated between two states.
pub trait Animatable: Clone {
    fn lerp(a: &Self, b: &Self, t: f64) -> Self;
}

impl Animatable for f64 {
    fn lerp(a: &Self, b: &Self, t: f64) -> Self {
        a + (b - a) * t
    }
}

impl Animatable for kurbo::Vec2 {
    fn lerp(a: &Self, b: &Self, t: f64) -> Self {
        *a + (*b - *a) * t
    }
}

/// Straight RGBA in [0,1]. Interpolated per channel (naive but predictable;
/// perceptual/gamma-correct blending is a later refinement).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Color {
    pub r: f64,
    pub g: f64,
    pub b: f64,
    pub a: f64,
}

impl Color {
    pub const fn rgba(r: f64, g: f64, b: f64, a: f64) -> Self {
        Self { r, g, b, a }
    }
    pub const fn rgb(r: f64, g: f64, b: f64) -> Self {
        Self::rgba(r, g, b, 1.0)
    }
}

impl Animatable for Color {
    fn lerp(a: &Self, b: &Self, t: f64) -> Self {
        Color {
            r: f64::lerp(&a.r, &b.r, t),
            g: f64::lerp(&a.g, &b.g, t),
            b: f64::lerp(&a.b, &b.b, t),
            a: f64::lerp(&a.a, &b.a, t),
        }
    }
}

/// A normalized cubic-bezier easing control point, CSS `cubic-bezier` style.
/// The out-handle of key A and the in-handle of key B together define the
/// timing curve `cubic-bezier(out.x, out.y, in.x, in.y)` across the segment.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Handle {
    pub x: f64,
    pub y: f64,
}

impl Handle {
    pub const fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }
    /// Linear timing: control points sit on the diagonal.
    pub const LINEAR_OUT: Handle = Handle::new(1.0 / 3.0, 1.0 / 3.0);
    pub const LINEAR_IN: Handle = Handle::new(2.0 / 3.0, 2.0 / 3.0);
    /// A gentle symmetric ease, roughly `ease-in-out`.
    pub const SMOOTH_OUT: Handle = Handle::new(0.42, 0.0);
    pub const SMOOTH_IN: Handle = Handle::new(0.58, 1.0);
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Keyframe<T> {
    pub time: f64,
    pub value: T,
    /// Timing handle leaving this key toward the next.
    pub out_handle: Handle,
    /// Timing handle arriving at this key from the previous.
    pub in_handle: Handle,
}

impl<T> Keyframe<T> {
    /// A linearly-timed key.
    pub fn linear(time: f64, value: T) -> Self {
        Self {
            time,
            value,
            out_handle: Handle::LINEAR_OUT,
            in_handle: Handle::LINEAR_IN,
        }
    }
    /// A smoothly-eased key.
    pub fn smooth(time: f64, value: T) -> Self {
        Self {
            time,
            value,
            out_handle: Handle::SMOOTH_OUT,
            in_handle: Handle::SMOOTH_IN,
        }
    }
}

/// A sorted list of keyframes. Sampling clamps outside the first/last key
/// (hold), and eases + lerps within a segment.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Track<T> {
    keys: Vec<Keyframe<T>>,
}

impl<T: Animatable> Track<T> {
    pub fn new(mut keys: Vec<Keyframe<T>>) -> Self {
        keys.sort_by(|a, b| a.time.partial_cmp(&b.time).unwrap_or(std::cmp::Ordering::Equal));
        Self { keys }
    }

    pub fn keys(&self) -> &[Keyframe<T>] {
        &self.keys
    }

    pub fn sample(&self, t: f64) -> T {
        match self.keys.as_slice() {
            [] => panic!("Track::sample on an empty track"),
            [only] => only.value.clone(),
            keys => {
                // Before first / after last: hold the endpoint.
                if t <= keys[0].time {
                    return keys[0].value.clone();
                }
                if t >= keys[keys.len() - 1].time {
                    return keys[keys.len() - 1].value.clone();
                }
                // Find the surrounding segment [a, b].
                let seg = keys.windows(2).find(|w| t >= w[0].time && t <= w[1].time);
                let [a, b] = match seg {
                    Some(w) => [&w[0], &w[1]],
                    None => return keys[keys.len() - 1].value.clone(),
                };
                let span = b.time - a.time;
                let u = if span > 0.0 { (t - a.time) / span } else { 0.0 };
                // Temporal easing: solve the timing bezier for eased fraction.
                let eased = solve_ease(u, a.out_handle, b.in_handle);
                T::lerp(&a.value, &b.value, eased)
            }
        }
    }
}

/// A property's value source. Adding `Expr` / `Parametric` variants later is
/// how expressions and node-graph-driven values plug in — the same lowered-IR
/// discipline EBN uses for control flow, applied to dataflow values.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Value<T> {
    Const(T),
    Keyframed(Track<T>),
}

impl<T: Animatable> Value<T> {
    pub fn constant(v: T) -> Self {
        Value::Const(v)
    }

    pub fn resolve(&self, t: f64) -> T {
        match self {
            Value::Const(v) => v.clone(),
            Value::Keyframed(track) => track.sample(t),
        }
    }
}

/// Given a normalized segment position `u` in [0,1] and the two timing handles,
/// return the eased fraction. The timing curve is `cubic-bezier(p1, p2)` with
/// endpoints fixed at (0,0) and (1,1); we invert x(s)=u for the parameter s,
/// then read y(s).
fn solve_ease(u: f64, p1: Handle, p2: Handle) -> f64 {
    if u <= 0.0 {
        return 0.0;
    }
    if u >= 1.0 {
        return 1.0;
    }
    let bez = |a: f64, b: f64, s: f64| {
        // Cubic bezier component with endpoints 0 and 1.
        let mt = 1.0 - s;
        3.0 * mt * mt * s * a + 3.0 * mt * s * s * b + s * s * s
    };
    // Invert x(s) = u via bisection (robust; the curve is monotonic in x for
    // well-formed easing handles, and bisection degrades gracefully if not).
    let (mut lo, mut hi) = (0.0f64, 1.0f64);
    let mut s = u;
    for _ in 0..40 {
        let x = bez(p1.x, p2.x, s);
        if (x - u).abs() < 1e-6 {
            break;
        }
        if x < u {
            lo = s;
        } else {
            hi = s;
        }
        s = 0.5 * (lo + hi);
    }
    bez(p1.y, p2.y, s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn const_resolves_anywhere() {
        let v = Value::constant(5.0);
        assert_eq!(v.resolve(0.0), 5.0);
        assert_eq!(v.resolve(99.0), 5.0);
    }

    #[test]
    fn linear_track_midpoint() {
        let v: Value<f64> = Value::Keyframed(Track::new(vec![
            Keyframe::linear(0.0, 0.0),
            Keyframe::linear(1.0, 100.0),
        ]));
        assert!((v.resolve(0.5) - 50.0).abs() < 1e-3, "got {}", v.resolve(0.5));
    }

    #[test]
    fn track_holds_outside_range() {
        let v: Value<f64> = Value::Keyframed(Track::new(vec![
            Keyframe::linear(1.0, 10.0),
            Keyframe::linear(2.0, 20.0),
        ]));
        assert_eq!(v.resolve(0.0), 10.0);
        assert_eq!(v.resolve(5.0), 20.0);
    }

    #[test]
    fn smooth_ease_is_symmetric_at_midpoint() {
        // A symmetric ease should still pass through 50% at u=0.5.
        let v: Value<f64> = Value::Keyframed(Track::new(vec![
            Keyframe::smooth(0.0, 0.0),
            Keyframe::smooth(1.0, 100.0),
        ]));
        assert!((v.resolve(0.5) - 50.0).abs() < 1.0, "got {}", v.resolve(0.5));
        // ...but eased slower at the start than linear would be.
        assert!(v.resolve(0.25) < 25.0, "ease-in should lag: {}", v.resolve(0.25));
    }

    #[test]
    fn vec2_track() {
        use kurbo::Vec2;
        let v: Value<Vec2> = Value::Keyframed(Track::new(vec![
            Keyframe::linear(0.0, Vec2::new(0.0, 0.0)),
            Keyframe::linear(1.0, Vec2::new(10.0, 20.0)),
        ]));
        let p = v.resolve(0.5);
        assert!((p.x - 5.0).abs() < 1e-3 && (p.y - 10.0).abs() < 1e-3);
    }
}
