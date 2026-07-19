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

/// A key at an exact frame. Frames are integers on purpose: a keyframe that
/// sits between frames can never be reached by playback, and float times
/// forced every comparison through an epsilon fudge. The frame *grid* lives in
/// [`crate::timebase::Timebase`]; a key knows only its index on that grid.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Keyframe<T> {
    #[serde(default)]
    pub frame: i64,
    /// Legacy `.pbc` docs stored keyframe times as float seconds. Deserializing
    /// one parks the value here; [`Document::migrate`] converts it to a frame
    /// once `fps` is known (a `Keyframe` alone can't — it has no timebase).
    /// Never re-serialized, so a doc is migrated permanently on first save.
    #[serde(default, rename = "time", skip_serializing)]
    pub(crate) legacy_seconds: Option<f64>,
    pub value: T,
    /// Timing handle leaving this key toward the next.
    pub out_handle: Handle,
    /// Timing handle arriving at this key from the previous.
    pub in_handle: Handle,
}

impl<T> Keyframe<T> {
    /// A linearly-timed key.
    pub fn linear(frame: i64, value: T) -> Self {
        Self {
            frame,
            legacy_seconds: None,
            value,
            out_handle: Handle::LINEAR_OUT,
            in_handle: Handle::LINEAR_IN,
        }
    }
    /// A smoothly-eased key.
    pub fn smooth(frame: i64, value: T) -> Self {
        Self {
            frame,
            legacy_seconds: None,
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
        keys.sort_by_key(|k| k.frame);
        Self { keys }
    }

    pub fn keys(&self) -> &[Keyframe<T>] {
        &self.keys
    }

    pub fn len(&self) -> usize {
        self.keys.len()
    }

    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    /// Frames of every keyframe, in order.
    pub fn frames(&self) -> Vec<i64> {
        self.keys.iter().map(|k| k.frame).collect()
    }

    /// Move keyframe `index` to `new_frame`, clamped so it can't cross or land
    /// on its neighbours. Clamping preserves the sorted invariant `sample`
    /// relies on and keeps the key's index stable across a drag.
    ///
    /// On the frame grid the gap is exactly one frame — no epsilon. If there is
    /// no room between the neighbours the key simply doesn't move.
    pub fn move_key(&mut self, index: usize, new_frame: i64) {
        let n = self.keys.len();
        if index >= n {
            return;
        }
        let lo = if index > 0 {
            self.keys[index - 1].frame + 1
        } else {
            i64::MIN
        };
        let hi = if index + 1 < n {
            self.keys[index + 1].frame - 1
        } else {
            i64::MAX
        };
        if lo <= hi {
            self.keys[index].frame = new_frame.clamp(lo, hi);
        }
    }

    /// Convert legacy float-seconds keys to frames at `fps`. See
    /// [`Keyframe::legacy_seconds`]. Re-sorts, since rounding can collide or
    /// reorder keys that were microscopically apart in the old format.
    pub(crate) fn migrate_frames(&mut self, fps: f64) {
        let mut touched = false;
        for k in &mut self.keys {
            if let Some(seconds) = k.legacy_seconds.take() {
                k.frame = (seconds * fps).round() as i64;
                touched = true;
            }
        }
        if touched {
            self.keys.sort_by_key(|k| k.frame);
            // Rounding can land two old keys on one frame; keep the first.
            self.keys.dedup_by_key(|k| k.frame);
        }
    }

    /// Remove keyframe `index`. A track is never emptied below one key.
    pub fn remove_key(&mut self, index: usize) {
        if self.keys.len() > 1 && index < self.keys.len() {
            self.keys.remove(index);
        }
    }

    /// The timing handles governing the segment leaving keyframe `index`
    /// (its out-handle and the next key's in-handle). `None` past the last key.
    pub fn segment_handles(&self, index: usize) -> Option<(Handle, Handle)> {
        if index + 1 < self.keys.len() {
            Some((self.keys[index].out_handle, self.keys[index + 1].in_handle))
        } else {
            None
        }
    }

    /// Set the handles for the segment leaving keyframe `index`.
    pub fn set_segment_handles(&mut self, index: usize, out: Handle, next_in: Handle) {
        if index + 1 < self.keys.len() {
            self.keys[index].out_handle = out;
            self.keys[index + 1].in_handle = next_in;
        }
    }

    /// Insert or update a keyframe at `frame`. If a key already sits on that
    /// frame its value is replaced (handles preserved); otherwise a new
    /// smoothly-eased key is inserted in sorted order. This is the "auto-key"
    /// behavior an editor uses when the user changes an animated value.
    ///
    /// Exact integer equality — the old float-epsilon match is gone, which is
    /// half the point of moving to a frame grid.
    pub fn set_key(&mut self, frame: i64, value: T) {
        if let Some(k) = self.keys.iter_mut().find(|k| k.frame == frame) {
            k.value = value;
        } else {
            self.keys.push(Keyframe::smooth(frame, value));
            self.keys.sort_by_key(|k| k.frame);
        }
    }

    /// Sample at `frame`, which is deliberately fractional: playback runs off a
    /// wall clock and the compositor will want sub-frame samples for motion
    /// blur. The *keys* are on the grid; the playhead need not be.
    pub fn sample(&self, frame: f64) -> T {
        match self.keys.as_slice() {
            [] => panic!("Track::sample on an empty track"),
            [only] => only.value.clone(),
            keys => {
                // Before first / after last: hold the endpoint.
                if frame <= keys[0].frame as f64 {
                    return keys[0].value.clone();
                }
                if frame >= keys[keys.len() - 1].frame as f64 {
                    return keys[keys.len() - 1].value.clone();
                }
                // Find the surrounding segment [a, b].
                let seg = keys
                    .windows(2)
                    .find(|w| frame >= w[0].frame as f64 && frame <= w[1].frame as f64);
                let [a, b] = match seg {
                    Some(w) => [&w[0], &w[1]],
                    None => return keys[keys.len() - 1].value.clone(),
                };
                let span = (b.frame - a.frame) as f64;
                let u = if span > 0.0 { (frame - a.frame as f64) / span } else { 0.0 };
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

    /// Resolve at `frame` (fractional allowed — see [`Track::sample`]).
    pub fn resolve(&self, frame: f64) -> T {
        match self {
            Value::Const(v) => v.clone(),
            Value::Keyframed(track) => track.sample(frame),
        }
    }

    /// Write `value` at `frame`. A constant is overwritten wholesale; an
    /// animated value gets a keyframe set there (auto-key). This is the single
    /// entry point an editor uses so it never has to branch on the value kind.
    pub fn set_at(&mut self, frame: i64, value: T) {
        match self {
            Value::Const(v) => *v = value,
            Value::Keyframed(track) => track.set_key(frame, value),
        }
    }

    /// Convert legacy float-seconds keys to frames at `fps` (no-op otherwise).
    pub(crate) fn migrate_frames(&mut self, fps: f64) {
        if let Value::Keyframed(track) = self {
            track.migrate_frames(fps);
        }
    }

    /// Whether this value is animated (has a keyframe track).
    pub fn is_animated(&self) -> bool {
        matches!(self, Value::Keyframed(_))
    }

    /// Keyframe frames, or empty for a constant. Lets a timeline enumerate keys
    /// without caring about the value type `T`.
    pub fn key_frames(&self) -> Vec<i64> {
        match self {
            Value::Const(_) => Vec::new(),
            Value::Keyframed(track) => track.frames(),
        }
    }

    /// Move keyframe `index` to `new_frame` (no-op on a constant).
    pub fn move_key(&mut self, index: usize, new_frame: i64) {
        if let Value::Keyframed(track) = self {
            track.move_key(index, new_frame);
        }
    }

    /// Remove keyframe `index` (no-op on a constant).
    pub fn remove_key(&mut self, index: usize) {
        if let Value::Keyframed(track) = self {
            track.remove_key(index);
        }
    }

    /// Insert a keyframe at `frame`, holding the value the property currently
    /// resolves to. A constant is promoted to a one-key track (this is how a
    /// property *starts* being animated); an existing track gets a key there.
    pub fn insert_key(&mut self, frame: i64) {
        if let Value::Const(v) = self {
            let v = v.clone();
            *self = Value::Keyframed(Track::new(vec![Keyframe::smooth(frame, v)]));
        } else if let Value::Keyframed(track) = self {
            let cur = track.sample(frame as f64);
            track.set_key(frame, cur);
        }
    }

    /// Handles for the segment leaving keyframe `index` (out of this key, in of
    /// the next). `None` for a constant or the last key.
    pub fn segment_handles(&self, index: usize) -> Option<(Handle, Handle)> {
        match self {
            Value::Const(_) => None,
            Value::Keyframed(track) => track.segment_handles(index),
        }
    }

    /// Set the segment handles leaving keyframe `index` (no-op on a constant).
    pub fn set_segment_handles(&mut self, index: usize, out: Handle, next_in: Handle) {
        if let Value::Keyframed(track) = self {
            track.set_segment_handles(index, out, next_in);
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
            Keyframe::linear(0, 0.0),
            Keyframe::linear(24, 100.0),
        ]));
        assert!((v.resolve(12.0) - 50.0).abs() < 1e-3, "got {}", v.resolve(12.0));
    }

    #[test]
    fn track_holds_outside_range() {
        let v: Value<f64> = Value::Keyframed(Track::new(vec![
            Keyframe::linear(24, 10.0),
            Keyframe::linear(48, 20.0),
        ]));
        assert_eq!(v.resolve(0.0), 10.0);
        assert_eq!(v.resolve(120.0), 20.0);
    }

    #[test]
    fn sample_accepts_fractional_frames() {
        // Keys are on the grid; the playhead need not be.
        let v: Value<f64> = Value::Keyframed(Track::new(vec![
            Keyframe::linear(0, 0.0),
            Keyframe::linear(10, 100.0),
        ]));
        assert!((v.resolve(2.5) - 25.0).abs() < 1e-9, "got {}", v.resolve(2.5));
    }

    #[test]
    fn smooth_ease_is_symmetric_at_midpoint() {
        // A symmetric ease should still pass through 50% at u=0.5.
        let v: Value<f64> = Value::Keyframed(Track::new(vec![
            Keyframe::smooth(0, 0.0),
            Keyframe::smooth(24, 100.0),
        ]));
        assert!((v.resolve(12.0) - 50.0).abs() < 1.0, "got {}", v.resolve(12.0));
        // ...but eased slower at the start than linear would be.
        assert!(v.resolve(6.0) < 25.0, "ease-in should lag: {}", v.resolve(6.0));
    }

    #[test]
    fn set_at_overwrites_a_constant() {
        let mut v = Value::constant(3.0);
        v.set_at(24, 9.0);
        assert_eq!(v.resolve(0.0), 9.0);
        assert!(!v.is_animated());
    }

    #[test]
    fn set_at_replaces_existing_key_and_inserts_new() {
        let mut v: Value<f64> = Value::Keyframed(Track::new(vec![
            Keyframe::linear(0, 0.0),
            Keyframe::linear(24, 100.0),
        ]));
        // Edit exactly on the first key: replaces its value, no new key.
        v.set_at(0, 50.0);
        assert_eq!(v.resolve(0.0), 50.0);
        // Edit between keys: inserts a new key on that frame.
        v.set_at(12, 75.0);
        assert!((v.resolve(12.0) - 75.0).abs() < 1e-6);
        if let Value::Keyframed(track) = &v {
            assert_eq!(track.keys().len(), 3, "a key should have been inserted");
        } else {
            panic!("expected keyframed");
        }
    }

    #[test]
    fn insert_key_promotes_constant_then_adds() {
        let mut v = Value::constant(7.0);
        assert!(!v.is_animated());
        v.insert_key(24);
        assert!(v.is_animated(), "constant should become a track");
        assert_eq!(v.key_frames(), vec![24]);
        assert_eq!(v.resolve(24.0), 7.0, "the held value carries over");
        // A second insert on a new frame adds a key holding the resolved value.
        v.insert_key(72);
        assert_eq!(v.key_frames().len(), 2);
    }

    #[test]
    fn segment_handles_round_trip() {
        let mut v: Value<f64> = Value::Keyframed(Track::new(vec![
            Keyframe::linear(0, 0.0),
            Keyframe::linear(24, 10.0),
        ]));
        let (out, inn) = v.segment_handles(0).unwrap();
        assert!((out.x - Handle::LINEAR_OUT.x).abs() < 1e-9);
        v.set_segment_handles(0, Handle::new(0.9, 0.1), Handle::new(0.1, 0.9));
        let (out2, in2) = v.segment_handles(0).unwrap();
        assert!((out2.x - 0.9).abs() < 1e-9 && (in2.y - 0.9).abs() < 1e-9);
        assert!(v.segment_handles(1).is_none(), "no segment past the last key");
        let _ = inn;
    }

    #[test]
    fn move_key_clamps_between_neighbours() {
        let mut v: Value<f64> = Value::Keyframed(Track::new(vec![
            Keyframe::linear(0, 0.0),
            Keyframe::linear(24, 100.0),
            Keyframe::linear(48, 0.0),
        ]));
        // Try to drag the middle key past the last one — it must stop short.
        v.move_key(1, 500);
        let f = v.key_frames();
        assert!(f[1] < f[2], "middle key must stay before the last");
        assert!(f[1] > f[0], "and after the first");
        // Order preserved, so sampling still works.
        assert!(v.resolve(12.0).is_finite());
    }

    #[test]
    fn move_key_into_a_full_gap_is_a_no_op() {
        // Adjacent frames leave nowhere to go; the key must not jump the fence.
        let mut v: Value<f64> = Value::Keyframed(Track::new(vec![
            Keyframe::linear(10, 0.0),
            Keyframe::linear(11, 1.0),
            Keyframe::linear(12, 2.0),
        ]));
        v.move_key(1, 99);
        assert_eq!(v.key_frames(), vec![10, 11, 12], "no room, so no movement");
    }

    #[test]
    fn vec2_track() {
        use kurbo::Vec2;
        let v: Value<Vec2> = Value::Keyframed(Track::new(vec![
            Keyframe::linear(0, Vec2::new(0.0, 0.0)),
            Keyframe::linear(24, Vec2::new(10.0, 20.0)),
        ]));
        let p = v.resolve(12.0);
        assert!((p.x - 5.0).abs() < 1e-3 && (p.y - 10.0).abs() < 1e-3);
    }

    #[test]
    fn legacy_seconds_migrate_to_frames() {
        // A pre-migration `.pbc` stored keyframe times as float seconds.
        let json = r#"{"Keyframed":{"keys":[
            {"time":0.0,"value":0.0,"out_handle":{"x":0.33,"y":0.33},"in_handle":{"x":0.67,"y":0.67}},
            {"time":2.0,"value":100.0,"out_handle":{"x":0.33,"y":0.33},"in_handle":{"x":0.67,"y":0.67}}
        ]}}"#;
        let mut v: Value<f64> = serde_json::from_str(json).unwrap();
        v.migrate_frames(24.0);
        assert_eq!(v.key_frames(), vec![0, 48], "2s @ 24fps is frame 48");
        // And it re-serializes in the new format, with no `time` field left.
        let out = serde_json::to_string(&v).unwrap();
        assert!(out.contains("\"frame\""), "should write frames: {out}");
        assert!(!out.contains("\"time\""), "legacy field must not persist: {out}");
    }

    #[test]
    fn migration_collapses_keys_that_round_onto_one_frame() {
        // Two keys 1ms apart cannot both survive on a 24fps grid.
        let json = r#"{"Keyframed":{"keys":[
            {"time":1.000,"value":0.0,"out_handle":{"x":0.33,"y":0.33},"in_handle":{"x":0.67,"y":0.67}},
            {"time":1.001,"value":100.0,"out_handle":{"x":0.33,"y":0.33},"in_handle":{"x":0.67,"y":0.67}}
        ]}}"#;
        let mut v: Value<f64> = serde_json::from_str(json).unwrap();
        v.migrate_frames(24.0);
        assert_eq!(v.key_frames(), vec![24], "collided keys collapse to one");
    }
}
