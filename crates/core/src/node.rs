//! The scene graph: a tree of `Node`s where every animatable field is a
//! `Value<_>`. Nothing here is baked — `eval` turns a document + time into a
//! flat `Scene`.

use kurbo::{BezPath, Rect, RoundedRect, Shape as _, Vec2};
use serde::{Deserialize, Serialize};

use crate::asset::{Asset, AssetId, ImagePaint};
use crate::composite::{BlendMode, Mask, MatteMode};
use crate::expr::{EvalCtx, Expr};
use crate::mat4::{Mat4, Xf};
use crate::path::VectorPath;
use crate::text::TextAlign;
use crate::value::{Color, Value};
use crate::vec3::Vec3;

/// Stable identity for a node, used for selection and for tracing an evaluated
/// render item back to its source (EBN's line→nodeId map idea, applied to a
/// pull-based dataflow graph).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct NodeId(pub u64);

/// A transform, every channel animatable and every spatial channel a [`Vec3`].
/// Resolves to an [`Xf`] plus a scalar opacity at a given time.
///
/// The channels are 3D but the *result* is not necessarily: a transform whose
/// depth and out-of-plane rotations are all zero resolves to `Xf::Flat`, a plain
/// `kurbo::Affine`, exactly as before 2.5D existed. See [`crate::mat4`] for why
/// that distinction is load-bearing rather than an optimisation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(from = "TransformRepr")]
pub struct Transform {
    pub anchor: Value<Vec3>,
    pub position: Value<Vec3>,
    /// The in-plane spin (Z), in **degrees** — the rotation every 2D document
    /// already had, and still the one the 2D handle and a graph wire drive.
    pub rotation: Value<f64>,
    /// Out-of-plane tilt about the layer's X axis, degrees. Its own track, so
    /// it keys, eases and is scripted independently of the other two axes.
    #[serde(default = "zero_angle")]
    pub rotation_x: Value<f64>,
    /// Out-of-plane turn about the layer's Y axis, degrees.
    #[serde(default = "zero_angle")]
    pub rotation_y: Value<f64>,
    pub scale: Value<Vec3>,
    pub opacity: Value<f64>,
}

fn zero_angle() -> Value<f64> {
    Value::constant(0.0)
}

/// What a `.pbc` may hold for a transform. Rotation has had three shapes: the
/// pre-2.5D scalar `rotation_deg`, the 2.5D joined `rotation` 3-vector, and
/// today's three separate scalar channels. Both older shapes land here and are
/// split by [`From`] — the next save writes the separated form.
#[derive(Deserialize)]
struct TransformRepr {
    anchor: Value<Vec3>,
    position: Value<Vec3>,
    #[serde(default, alias = "rotation_deg")]
    rotation: Option<RotationRepr>,
    #[serde(default)]
    rotation_x: Option<Value<f64>>,
    #[serde(default)]
    rotation_y: Option<Value<f64>>,
    scale: Value<Vec3>,
    opacity: Value<f64>,
}

/// Untagged, and the scalar arm is tried first — the payloads are structurally
/// distinct (`{"Const": 1.5}` against `{"Const": {"x": …}}`).
#[derive(Deserialize)]
#[serde(untagged)]
enum RotationRepr {
    Planar(Value<f64>),
    Joined(Value<Vec3>),
}

impl From<TransformRepr> for Transform {
    fn from(r: TransformRepr) -> Self {
        let (x, y, z) = match r.rotation {
            None => (zero_angle(), zero_angle(), zero_angle()),
            Some(RotationRepr::Planar(z)) => (zero_angle(), zero_angle(), z),
            Some(RotationRepr::Joined(v)) => split_rotation(v),
        };
        Self {
            anchor: r.anchor,
            position: r.position,
            rotation: z,
            // An explicit separated channel beats one recovered from a join.
            rotation_x: r.rotation_x.unwrap_or(x),
            rotation_y: r.rotation_y.unwrap_or(y),
            scale: r.scale,
            opacity: r.opacity,
        }
    }
}

/// Split a joined 2.5D rotation into its three channels, preserving key timing,
/// easing and expression wiring, so an upgraded document animates identically.
/// An axis whose keys are all zero collapses to a constant rather than
/// carrying a row of meaningless keyframes.
fn split_rotation(v: Value<Vec3>) -> (Value<f64>, Value<f64>, Value<f64>) {
    use crate::expr::Axis;
    match v {
        Value::Const(r) => (Value::Const(r.x), Value::Const(r.y), Value::Const(r.z)),
        Value::Keyframed(track) => {
            let axis = |a: Axis| -> Value<f64> {
                let pick = move |v: Vec3| match a {
                    Axis::X => v.x,
                    Axis::Y => v.y,
                    Axis::Z => v.z,
                };
                let mut t = track.clone().map_value(pick);
                // The script saw the joined vector; rebuild it from this
                // channel alone (the other two read flat) and take ours back.
                t.expr = t.expr.map(|s| {
                    let join = match a {
                        Axis::X => "vec3(value, 0.0, 0.0)",
                        Axis::Y => "vec3(0.0, value, 0.0)",
                        Axis::Z => "vec3(0.0, 0.0, value)",
                    };
                    let i = match a { Axis::X => 0, Axis::Y => 1, Axis::Z => 2 };
                    format!("let value = {join}; ({{ {s} }})[{i}]")
                });
                if t.expr.is_none() && t.keys().iter().all(|k| k.value == 0.0) && a != Axis::Z {
                    return zero_angle();
                }
                Value::Keyframed(t)
            };
            (axis(Axis::X), axis(Axis::Y), axis(Axis::Z))
        }
        Value::Expr(e) => {
            // The common case is the graph's adapt: `vec3(0, 0, spin)`. Peel
            // it rather than wrapping each axis in a component read.
            if let Expr::Vec3 { x, y, z } = e {
                return (Value::Expr(*x), Value::Expr(*y), Value::Expr(*z));
            }
            let comp = |axis| Value::Expr(Expr::Comp { a: Box::new(e.clone()), axis });
            (comp(Axis::X), comp(Axis::Y), comp(Axis::Z))
        }
    }
}

impl Default for Transform {
    fn default() -> Self {
        Self {
            anchor: Value::constant(Vec3::ZERO),
            position: Value::constant(Vec3::ZERO),
            rotation: zero_angle(),
            rotation_x: zero_angle(),
            rotation_y: zero_angle(),
            scale: Value::constant(Vec3::ONE),
            opacity: Value::constant(1.0),
        }
    }
}

impl Transform {
    /// Resolve to (transform, opacity) against `ctx`. The transform maps local
    /// space to parent space: translate(position) · rotate · scale ·
    /// translate(-anchor) — the same construction as before, evaluated in
    /// whichever of the two tiers the channels actually need.
    pub fn resolve(&self, ctx: &mut EvalCtx) -> (Xf, f64) {
        let anchor = self.anchor.resolve(ctx);
        let position = self.position.resolve(ctx);
        let rot = Vec3::new(
            self.rotation_x.resolve(ctx),
            self.rotation_y.resolve(ctx),
            self.rotation.resolve(ctx),
        );
        let scale = self.scale.resolve(ctx);
        let opacity = self.opacity.resolve(ctx);

        // The fast path, and the one every existing document takes: no depth in
        // the translations, no out-of-plane rotation. Scale's z is ignored here
        // because scaling depth with nothing in depth is a no-op.
        if anchor.is_flat() && position.is_flat() && rot.x == 0.0 && rot.y == 0.0 {
            let m = kurbo::Affine::translate(position.xy())
                * kurbo::Affine::rotate(rot.z.to_radians())
                * kurbo::Affine::scale_non_uniform(scale.x, scale.y)
                * kurbo::Affine::translate(-anchor.xy());
            return (Xf::Flat(m), opacity);
        }

        // Z · Y · X: yaw last so a layer tipped in depth still spins about the
        // screen axis the way the 2D rotation handle implies.
        let m = Mat4::translate(position)
            * Mat4::rotate_z(rot.z.to_radians())
            * Mat4::rotate_y(rot.y.to_radians())
            * Mat4::rotate_x(rot.x.to_radians())
            * Mat4::scale(Vec3::new(scale.x, scale.y, if scale.z == 0.0 { 1.0 } else { scale.z }))
            * Mat4::translate(-anchor);
        (Xf::Spatial(m), opacity)
    }

    /// True when this transform can never leave the plane on any frame — every
    /// spatial channel is a flat constant. Cheaper and stricter than resolving:
    /// an animated channel counts as spatial even if it happens to be flat now,
    /// so a comp can be classified once instead of per frame.
    pub fn is_statically_flat(&self) -> bool {
        fn flat(v: &Value<Vec3>) -> bool {
            matches!(v, Value::Const(c) if c.is_flat())
        }
        flat(&self.anchor)
            && flat(&self.position)
            && matches!(self.rotation_x, Value::Const(r) if r == 0.0)
            && matches!(self.rotation_y, Value::Const(r) if r == 0.0)
    }

    pub(crate) fn migrate_frames(&mut self, fps: f64) {
        self.anchor.migrate_frames(fps);
        self.position.migrate_frames(fps);
        self.rotation.migrate_frames(fps);
        self.rotation_x.migrate_frames(fps);
        self.rotation_y.migrate_frames(fps);
        self.scale.migrate_frames(fps);
        self.opacity.migrate_frames(fps);
    }

    pub(crate) fn retime(&mut self, ratio: f64) {
        self.anchor.retime(ratio);
        self.position.retime(ratio);
        self.rotation.retime(ratio);
        self.rotation_x.retime(ratio);
        self.rotation_y.retime(ratio);
        self.scale.retime(ratio);
        self.opacity.retime(ratio);
    }
}

/// Accept either spelling of a text layer's `content`: the tagged `Value` form
/// written today, or the bare JSON string written before `content` became a
/// [`Value`].
///
/// Same spirit as [`crate::value::Keyframe::legacy_seconds`], but resolvable on
/// the spot — a plain string *is* a `Value::Const`, with nothing extra needed to
/// interpret it — so there's no deferred `migrate()` step and the next save
/// rewrites it in the new form.
fn de_text_content<'de, D>(d: D) -> Result<Value<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Either {
        // Ordered deliberately: `Value` first so a well-formed new document
        // never even attempts the legacy arm. `untagged` tries in order and
        // takes the first that fits, and the two forms are unambiguous anyway
        // (an object vs. a string).
        Value(Value<String>),
        Legacy(String),
    }
    Ok(match Either::deserialize(d)? {
        Either::Value(v) => v,
        Either::Legacy(s) => Value::Const(s),
    })
}

/// A drawable shape. Parametric variants resolve their geometry at time `t`,
/// so a rectangle's size can itself be keyframed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Shape {
    /// A **legacy** baked outline. Kept only so a `.pbc` written before editable
    /// paths existed still deserializes; [`Shape::migrate`] upgrades it to a
    /// [`Shape::Vector`] on load, so nothing new ever writes this variant and the
    /// functional arms below treat it as a static, un-animatable outline.
    Path(BezPath),
    /// An editable, animatable Bézier path — what the pen tool draws. Its control
    /// points are [`Value`]s (see [`crate::path`]), so it keyframes, retimes, and
    /// morphs through the same machinery as every other property.
    Vector { path: VectorPath },
    /// Rounded rectangle centered on the origin, animatable size and corner.
    Rect {
        size: Value<Vec2>,
        radius: Value<f64>,
    },
    /// Ellipse centered on the origin, animatable size (width/height).
    Ellipse { size: Value<Vec2> },
    /// Shaped text, centred on the origin like the other primitives.
    ///
    /// It resolves to glyph **outlines** (see [`crate::text`]), so it fills,
    /// strokes, transforms, and animates through exactly the same path as a rect
    /// — no renderer knows text exists. `size` is animatable (the channel you'd
    /// keyframe), and so is `content` since [`crate::expr::ExprValue::Str`]
    /// landed — a text layer can be keyframed (a step track, see
    /// `impl Animatable for String`) or driven by a script, which is what makes
    /// the typewriter effect an expression rather than a built-in.
    ///
    /// `family` stays plain data: it selects a *system* font by name, so it is
    /// a lookup key rather than a value, and animating it would mean animating
    /// which shaper runs.
    Text {
        /// Accepts a bare JSON string as well as the tagged `Value` form, so a
        /// `.pbc` saved while `content` was a plain `String` still opens. See
        /// [`de_text_content`].
        #[serde(deserialize_with = "de_text_content")]
        content: Value<String>,
        /// System font family name. Empty (or not installed) → sans-serif.
        family: String,
        /// 100–900, animatable. A variable font interpolates its `wght` axis
        /// continuously; a family of static faces snaps to the nearest one
        /// (CSS font matching). Accepts the bare number older `.pbc`s stored.
        #[serde(default = "regular_weight", deserialize_with = "de_weight")]
        weight: Value<f64>,
        #[serde(default)]
        italic: bool,
        /// Font size in pixels.
        size: Value<f64>,
        /// Letter spacing in 1/1000 em (AE's tracking unit).
        #[serde(default = "zero_value")]
        tracking: Value<f64>,
        /// Line spacing in px; 0 is auto.
        #[serde(default = "zero_value")]
        leading: Value<f64>,
        align: TextAlign,
        /// Wrap width; `None` keeps the text on one line.
        max_width: Option<f64>,
    },
    /// Footage — a still or a clip — drawn into a rectangle centred on the
    /// origin, exactly like [`Shape::Rect`].
    ///
    /// **A raster layer is a rect with pixels in it.** That is the whole trick
    /// of how this fits: `to_path` returns the frame rectangle, so hit-testing,
    /// bounding boxes, snapping, the transform gizmo and the motion path all
    /// keep working with no change at all — they consume `path`, and footage
    /// has one. The pixels ride alongside as [`ImagePaint`] on the render item,
    /// which only a backend that actually makes pixels ever looks at.
    Image {
        asset: AssetId,
        /// The rectangle the footage is drawn into, seeded at import from the
        /// asset's native size (so footage lands at 100%) and animatable from
        /// then on. Independent of the source's real size on purpose: scaling
        /// footage non-uniformly is an ordinary thing to want, and forcing it
        /// through `Transform::scale` would fight the anchor.
        size: Value<Vec2>,
        /// Which source frame to show, in **source** frames, when the footage
        /// should not simply play at its natural rate.
        ///
        /// `None` is natural playback: the layer's local time is converted to
        /// the source's rate and clamped (see [`Asset::source_frame`]), which
        /// is what a freshly imported clip does. `Some` is AE's "enable time
        /// remapping" — a `Value` like everything else, so freezing,
        /// reversing, or ramping a clip is keyframing one curve rather than a
        /// dedicated feature.
        #[serde(default)]
        time_remap: Option<Value<f64>>,
    },
}

fn regular_weight() -> Value<f64> {
    Value::constant(400.0)
}

/// `weight` was a plain `u16` before it was animatable.
fn de_weight<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Value<f64>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Either {
        Value(Value<f64>),
        Legacy(f64),
    }
    Ok(match Either::deserialize(d)? {
        Either::Value(v) => v,
        Either::Legacy(w) => Value::Const(w),
    })
}

/// A text animator's `position` was a plain `Vec2` before it was animatable.
fn de_opt_vec2_value<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<Value<Vec2>>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Either {
        Value(Value<Vec2>),
        Legacy(Vec2),
    }
    Ok(Option::<Either>::deserialize(d)?.map(|e| match e {
        Either::Value(v) => v,
        Either::Legacy(w) => Value::Const(w),
    }))
}

/// A text animator's `opacity` was a plain `Option<f64>` before.
fn de_opt_f64_value<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<Value<f64>>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Either {
        Value(Value<f64>),
        Legacy(f64),
    }
    Ok(Option::<Either>::deserialize(d)?.map(|e| match e {
        Either::Value(v) => v,
        Either::Legacy(w) => Value::Const(w),
    }))
}

fn hundred_value() -> Value<f64> {
    Value::constant(100.0)
}

fn zero_value() -> Value<f64> {
    Value::constant(0.0)
}

impl Shape {
    pub fn to_path(&self, ctx: &mut EvalCtx) -> BezPath {
        match self {
            Shape::Path(p) => p.clone(),
            Shape::Vector { path } => path.to_bez(ctx),
            Shape::Rect { size, radius } => {
                let s = size.resolve(ctx);
                let r = radius.resolve(ctx);
                let rect = Rect::new(-s.x / 2.0, -s.y / 2.0, s.x / 2.0, s.y / 2.0);
                RoundedRect::from_rect(rect, r).to_path(0.1)
            }
            Shape::Ellipse { size } => {
                let s = size.resolve(ctx);
                kurbo::Ellipse::new((0.0, 0.0), (s.x / 2.0, s.y / 2.0), 0.0).to_path(0.1)
            }
            Shape::Text { content, family, weight, italic, size, tracking, leading, align, max_width } => {
                // The substitution is silent by construction — parley falls back
                // and draws something perfectly good — so it has to be reported
                // explicitly or the frame just quietly uses the wrong typeface.
                if !crate::text::font_exists(family) {
                    ctx.warn_here(format!(
                        "font '{}' isn't installed here; drawing with the system default",
                        family.trim()
                    ));
                }
                // Resolved, not read: `content` is a recipe like every other
                // param, so this is where a keyframed title or a typewriter
                // script becomes the string this frame actually shapes.
                let content = content.resolve(ctx);
                let style = crate::text::TextStyle {
                    family,
                    weight: weight.resolve(ctx),
                    italic: *italic,
                    tracking: tracking.resolve(ctx),
                    leading: leading.resolve(ctx),
                };
                crate::text::text_to_path(&content, &style, size.resolve(ctx), *align, *max_width)
            }
            // The frame rectangle — the same geometry a `Rect` of this size
            // would produce, which is exactly the point: every overlay and
            // hit-test in the editor reads this and needs to know nothing
            // about footage.
            Shape::Image { size, .. } => {
                let s = size.resolve(ctx);
                Rect::new(-s.x / 2.0, -s.y / 2.0, s.x / 2.0, s.y / 2.0).to_path(0.1)
            }
        }
    }

    /// The footage this shape shows on `ctx`'s frame, if it is footage at all.
    ///
    /// Separate from [`Shape::to_path`] because the two answer different
    /// questions — "what shape is this" and "what pixels go in it" — and only
    /// the second needs the asset registry. Resolving them together would put
    /// a footage lookup in the path of every rectangle.
    pub fn image_paint(&self, ctx: &mut EvalCtx) -> Option<ImagePaint> {
        let Shape::Image { asset, time_remap, .. } = self else {
            return None;
        };
        // Time remapping is authored in source frames, so it bypasses the rate
        // conversion but not the clamp: a curve that runs past the end of the
        // clip holds the last frame, exactly as natural playback does.
        let requested = time_remap.as_ref().map(|v| v.resolve(ctx));
        let Some(a) = ctx.asset(*asset) else {
            // No registry behind this evaluation, or the asset was deleted
            // while a layer still pointed at it. Warned rather than silently
            // dropped, like a dangling precomp — and the paint is still
            // emitted so the backend can draw its own "missing footage"
            // placeholder in the right place.
            ctx.warn_here(format!("footage {} is not in this project", asset.0));
            return Some(ImagePaint {
                asset: *asset,
                source_frame: requested.unwrap_or(0.0).max(0.0) as i64,
            });
        };
        let source_frame = match requested {
            Some(f) => a.clamp_frame(f),
            None => a.source_frame(ctx.frame, ctx.comp_fps()),
        };
        Some(ImagePaint { asset: *asset, source_frame })
    }

    pub(crate) fn migrate_frames(&mut self, fps: f64) {
        match self {
            Shape::Path(_) => {}
            Shape::Vector { path } => path.migrate_frames(fps),
            Shape::Rect { size, radius } => {
                size.migrate_frames(fps);
                radius.migrate_frames(fps);
            }
            Shape::Ellipse { size } => size.migrate_frames(fps),
            Shape::Text { content, size, tracking, leading, weight, .. } => {
                weight.migrate_frames(fps);
                content.migrate_frames(fps);
                size.migrate_frames(fps);
                tracking.migrate_frames(fps);
                leading.migrate_frames(fps);
            }
            Shape::Image { size, time_remap, .. } => {
                size.migrate_frames(fps);
                if let Some(t) = time_remap {
                    t.migrate_frames(fps);
                }
            }
        }
    }

    /// Fold a legacy [`Shape::Path`] into an editable [`Shape::Vector`]. Called
    /// on load (see [`Node::migrate`]); a no-op for every other variant, so it is
    /// safe to run over the whole tree unconditionally.
    pub(crate) fn migrate(&mut self) {
        if let Shape::Path(bez) = self {
            *self = Shape::Vector { path: VectorPath::from_bez(bez) };
        }
    }

    pub(crate) fn retime(&mut self, ratio: f64) {
        match self {
            Shape::Path(_) => {}
            Shape::Vector { path } => path.retime(ratio),
            Shape::Rect { size, radius } => {
                size.retime(ratio);
                radius.retime(ratio);
            }
            Shape::Ellipse { size } => size.retime(ratio),
            Shape::Text { content, size, tracking, leading, weight, .. } => {
                weight.retime(ratio);
                content.retime(ratio);
                size.retime(ratio);
                tracking.retime(ratio);
                leading.retime(ratio);
            }
            Shape::Image { size, time_remap, .. } => {
                size.retime(ratio);
                // Only the *keys* move. A remap curve's values are source
                // frames — a property of the footage, not of the comp's rate —
                // so re-gridding the comp must slide when each remap key
                // happens without changing which frame of the clip it picks.
                // `retime` moves keys and leaves values alone, which is exactly
                // that.
                if let Some(t) = time_remap {
                    t.retime(ratio);
                }
            }
        }
    }
}

/// One text animator, After Effects style: a set of properties describing what
/// happens to a *fully selected* unit, and a selector deciding how much of each
/// unit (character, word, line) is selected, 0..1. Every property is a
/// [`Value`], so it can be held constant, keyframed or scripted; the selection
/// scales its effect per unit.
///
/// A unit selected by `s` is, around its own anchor (see [`AnchorGrouping`]):
/// moved by `anchor * s`, scaled toward `scale` by `s`, skewed by `skew * s`,
/// rotated by `rotation * s`, then offset by `position * s`. Opacity, fill and
/// stroke mix toward their targets by `s`, and `tracking` adds `tracking * s`
/// (1/1000 em) after the unit, pushing the rest of its line along.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TextAnimator {
    /// A label for the editor; not used in evaluation.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    pub based_on: TextUnit,
    pub selector: TextSelector,
    /// How the first selector folds into the (empty) selection: AE's Mode.
    /// Subtract on the first selector selects the inverse.
    #[serde(default, skip_serializing_if = "SelectorMode::is_default")]
    pub mode: SelectorMode,
    /// Further selectors, folded in order after the first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub more_selectors: Vec<ExtraSelector>,
    /// Which units transform as one, around a shared anchor.
    #[serde(default, skip_serializing_if = "AnchorGrouping::is_default")]
    pub grouping: AnchorGrouping,
    /// Offset in the layer's space at full selection.
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "de_opt_vec2_value")]
    pub position: Option<Value<Vec2>>,
    /// Moves the pivot the unit scales/rotates about, relative to its anchor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor: Option<Value<Vec2>>,
    /// Scale in percent at full selection (100 is unchanged).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scale: Option<Value<Vec2>>,
    /// Rotation in degrees at full selection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rotation: Option<Value<f64>>,
    /// Horizontal skew in degrees at full selection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skew: Option<Value<f64>>,
    /// Opacity (0..1) at full selection; `None` leaves opacity alone.
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "de_opt_f64_value")]
    pub opacity: Option<Value<f64>>,
    /// Fill color at full selection, mixed from the layer's own fill by the
    /// selection; `None` leaves the fill alone. What karaoke colors words with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fill: Option<Value<Color>>,
    /// Stroke color at full selection, mixed from the layer's own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stroke: Option<Value<Color>>,
    /// Stroke width at full selection, mixed from the layer's own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stroke_width: Option<Value<f64>>,
    /// Extra spacing after the unit in 1/1000 em at full selection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tracking: Option<Value<f64>>,
    /// Depth offset in px at full selection (positive is away from the
    /// viewer). Any of the three 3D properties makes the layer per-character
    /// 3D: each glyph is projected through the comp's camera on its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position_z: Option<Value<f64>>,
    /// Rotation about the glyph's X axis in degrees at full selection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rotation_x: Option<Value<f64>>,
    /// Rotation about the glyph's Y axis in degrees at full selection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rotation_y: Option<Value<f64>>,
}

impl TextAnimator {
    /// An animator that does nothing yet: a full-range selector over `based_on`.
    pub fn new(based_on: TextUnit) -> Self {
        Self {
            name: String::new(),
            based_on,
            selector: TextSelector::range(),
            mode: SelectorMode::Add,
            more_selectors: Vec::new(),
            grouping: AnchorGrouping::default(),
            position: None,
            anchor: None,
            scale: None,
            rotation: None,
            skew: None,
            opacity: None,
            fill: None,
            stroke: None,
            stroke_width: None,
            tracking: None,
            position_z: None,
            rotation_x: None,
            rotation_y: None,
        }
    }

    /// Whether this animator moves glyphs out of the plane.
    pub fn is_3d(&self) -> bool {
        self.position_z.is_some() || self.rotation_x.is_some() || self.rotation_y.is_some()
    }

    pub(crate) fn each_value(&mut self, f: &mut impl FnMut(&mut dyn Retimable)) {
        if let Some(v) = &mut self.position { f(v) }
        if let Some(v) = &mut self.anchor { f(v) }
        if let Some(v) = &mut self.scale { f(v) }
        if let Some(v) = &mut self.rotation { f(v) }
        if let Some(v) = &mut self.skew { f(v) }
        if let Some(v) = &mut self.opacity { f(v) }
        if let Some(v) = &mut self.fill { f(v) }
        if let Some(v) = &mut self.stroke { f(v) }
        if let Some(v) = &mut self.stroke_width { f(v) }
        if let Some(v) = &mut self.tracking { f(v) }
        if let Some(v) = &mut self.position_z { f(v) }
        if let Some(v) = &mut self.rotation_x { f(v) }
        if let Some(v) = &mut self.rotation_y { f(v) }
        self.selector.each_value(f);
        for s in &mut self.more_selectors {
            s.selector.each_value(f);
        }
    }

    /// Every selector with its mode, the first one first.
    pub fn selectors(&self) -> impl Iterator<Item = (SelectorMode, &TextSelector)> {
        std::iter::once((self.mode, &self.selector)).chain(self.more_selectors.iter().map(|s| (s.mode, &s.selector)))
    }

    /// Selector `k` (0 is the first).
    pub fn selector_at(&self, k: usize) -> Option<&TextSelector> {
        match k {
            0 => Some(&self.selector),
            k => self.more_selectors.get(k - 1).map(|s| &s.selector),
        }
    }

    pub fn selector_at_mut(&mut self, k: usize) -> Option<&mut TextSelector> {
        match k {
            0 => Some(&mut self.selector),
            k => self.more_selectors.get_mut(k - 1).map(|s| &mut s.selector),
        }
    }

    /// Selector `k`'s mode.
    pub fn mode_at_mut(&mut self, k: usize) -> Option<&mut SelectorMode> {
        match k {
            0 => Some(&mut self.mode),
            k => self.more_selectors.get_mut(k - 1).map(|s| &mut s.mode),
        }
    }

    /// How much of unit `i` of `n` the whole selector stack selects, -1..1.
    /// `karaoke` answers a Karaoke selector (it needs the subtitle's word).
    pub fn selection(&self, i: usize, n: usize, ctx: &mut crate::expr::EvalCtx, karaoke: impl Fn(bool) -> f64) -> f64 {
        let mut acc = 0.0;
        for (k, (mode, sel)) in self.selectors().enumerate() {
            let s = match sel {
                TextSelector::Karaoke { cumulative } => karaoke(*cumulative),
                _ => sel.select(i, n, ctx, acc),
            };
            acc = if k == 0 && mode == SelectorMode::Subtract { 1.0 - s } else { mode.combine(acc, s) };
            acc = acc.clamp(-1.0, 1.0);
        }
        acc
    }
}

/// One selector after an animator's first, with its fold mode.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExtraSelector {
    #[serde(default)]
    pub mode: SelectorMode,
    pub selector: TextSelector,
}

/// How a selector folds into the selection of the selectors above it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SelectorMode {
    #[default]
    Add,
    Subtract,
    Intersect,
    Min,
    Max,
    Difference,
}

impl SelectorMode {
    pub const ALL: [SelectorMode; 6] =
        [SelectorMode::Add, SelectorMode::Subtract, SelectorMode::Intersect, SelectorMode::Min, SelectorMode::Max, SelectorMode::Difference];

    fn is_default(&self) -> bool {
        *self == Self::default()
    }

    pub fn label(self) -> &'static str {
        match self {
            SelectorMode::Add => "Add",
            SelectorMode::Subtract => "Subtract",
            SelectorMode::Intersect => "Intersect",
            SelectorMode::Min => "Min",
            SelectorMode::Max => "Max",
            SelectorMode::Difference => "Difference",
        }
    }

    /// Fold selection `s` into the running `acc`.
    pub fn combine(self, acc: f64, s: f64) -> f64 {
        match self {
            SelectorMode::Add => acc + s,
            SelectorMode::Subtract => acc - s,
            SelectorMode::Intersect => acc * s,
            SelectorMode::Min => acc.min(s),
            SelectorMode::Max => acc.max(s),
            SelectorMode::Difference => (acc - s).abs(),
        }
    }
}

/// Anything with keyframe times that move when the comp is re-gridded.
pub(crate) trait Retimable {
    fn retime(&mut self, ratio: f64);
}

impl<T: crate::value::Animatable + crate::expr::FromExpr + crate::expr::ToExpr> Retimable for Value<T> {
    fn retime(&mut self, ratio: f64) {
        Value::retime(self, ratio)
    }
}

/// What a selector counts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TextUnit {
    Chars,
    CharsExcludingSpaces,
    Words,
    Lines,
}

/// AE's Anchor Point Grouping: what a selected glyph rotates and scales about.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum AnchorGrouping {
    /// Each glyph about its own baseline centre.
    #[default]
    Character,
    /// Each word's glyphs about the word's baseline centre.
    Word,
    /// Each line's glyphs about the line's baseline centre.
    Line,
    /// Every glyph about the centre of the whole block.
    All,
}

impl AnchorGrouping {
    fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

/// The falloff of a [`TextSelector::Range`] across its span.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum RangeShape {
    /// Fully selected inside, not at all outside; units cut by an edge are
    /// selected by the part inside.
    #[default]
    Square,
    /// 0 before the range, ramping to 1 across it, 1 after.
    RampUp,
    /// 1 before the range, ramping to 0 across it, 0 after.
    RampDown,
    /// Peaks at the range's middle, 0 at its edges.
    Triangle,
    /// A semicircle across the range.
    Round,
    /// A smooth bell across the range.
    Smooth,
}

impl RangeShape {
    fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum TextSelector {
    /// AE's Range selector: `start`..`end`, shifted by `offset`, all
    /// percentages of the unit count, with a falloff `shape`. `amount` (0..100,
    /// negative inverts) scales the result; `ease_high`/`ease_low` (-100..100)
    /// bend the falloff near full and no selection. `randomize` shuffles which
    /// unit is where in the range, stably per `seed`.
    Range {
        start: Value<f64>,
        end: Value<f64>,
        offset: Value<f64>,
        #[serde(default = "hundred_value")]
        amount: Value<f64>,
        #[serde(default, skip_serializing_if = "RangeShape::is_default")]
        shape: RangeShape,
        #[serde(default)]
        ease_high: f64,
        #[serde(default)]
        ease_low: f64,
        #[serde(default)]
        randomize: bool,
        #[serde(default)]
        seed: u32,
    },
    /// One curve (`amount`, 0..100) sampled earlier for each later unit — what
    /// the Type Array expression does. Unit `i` reads it `delays[i]` frames
    /// back (after the last listed delay, `step` more per unit), plus `shift`.
    /// `reverse` counts units from the end.
    Stagger {
        amount: Value<f64>,
        #[serde(default)]
        delays: Vec<f64>,
        #[serde(default)]
        step: f64,
        #[serde(default)]
        shift: f64,
        #[serde(default)]
        reverse: bool,
    },
    /// AE's Wiggly selector: each unit's selection drifts between `min` and
    /// `max` (percent, -100..100) at `wiggles` per second. `correlation`
    /// (0..100) is how much units move together; `seed` picks the pattern.
    Wiggly {
        max: Value<f64>,
        min: Value<f64>,
        wiggles: Value<f64>,
        #[serde(default)]
        correlation: f64,
        #[serde(default)]
        seed: u32,
    },
    /// The word being spoken in the layer's [`crate::subs::Subtitles`] — or,
    /// `cumulative`, every word of the subtitle spoken so far. Selects nothing
    /// on a layer without subtitles. Only meaningful with [`TextUnit::Words`].
    Karaoke {
        #[serde(default)]
        cumulative: bool,
    },
    /// AE's Expression selector: `script` runs once per unit with
    /// `textIndex` (1-based), `textTotal` and `selectorValue` (the selection
    /// of the selectors above, 0..100) in scope, and returns 0..100.
    /// `amount` (percent) scales the result. A failing script selects
    /// nothing and warns.
    Expression {
        script: String,
        #[serde(default = "hundred_value")]
        amount: Value<f64>,
    },
}

impl TextSelector {
    /// A full Square range selecting everything: the AE default.
    pub fn range() -> Self {
        TextSelector::Range {
            start: Value::constant(0.0),
            end: Value::constant(100.0),
            offset: Value::constant(0.0),
            amount: Value::constant(100.0),
            shape: RangeShape::Square,
            ease_high: 0.0,
            ease_low: 0.0,
            randomize: false,
            seed: 0,
        }
    }

    /// A wiggly selector with AE's defaults.
    pub fn wiggly() -> Self {
        TextSelector::Wiggly {
            max: Value::constant(100.0),
            min: Value::constant(-100.0),
            wiggles: Value::constant(2.0),
            correlation: 50.0,
            seed: 0,
        }
    }

    fn each_value(&mut self, f: &mut impl FnMut(&mut dyn Retimable)) {
        match self {
            TextSelector::Range { start, end, offset, amount, .. } => {
                f(start);
                f(end);
                f(offset);
                f(amount);
            }
            TextSelector::Stagger { amount, .. } => f(amount),
            TextSelector::Wiggly { max, min, wiggles, .. } => {
                f(max);
                f(min);
                f(wiggles);
            }
            TextSelector::Karaoke { .. } => {}
            TextSelector::Expression { amount, .. } => f(amount),
        }
    }

    /// How much of unit `i` of `n` is selected at `ctx`'s frame: 0..1, or
    /// down to -1 for a negative amount (which pushes the other way).
    pub fn amount(&self, i: usize, n: usize, ctx: &mut crate::expr::EvalCtx) -> f64 {
        self.select(i, n, ctx, 0.0)
    }

    /// [`Self::amount`], with `prev` — the selection of the selectors above
    /// this one, which an Expression selector reads as `selectorValue`.
    pub fn select(&self, i: usize, n: usize, ctx: &mut crate::expr::EvalCtx, prev: f64) -> f64 {
        match self {
            TextSelector::Expression { script, amount } => {
                let vars = [("textIndex", (i + 1) as f64), ("textTotal", n as f64), ("selectorValue", prev * 100.0)];
                let v = match crate::expr::eval_script_vars(script, ctx, &vars) {
                    Ok(crate::expr::ExprValue::Num(v)) => v,
                    Ok(_) => {
                        ctx.warn_here("text expression selector must return a number (0..100)".to_string());
                        0.0
                    }
                    Err(e) => {
                        ctx.warn_here(format!("text expression selector: {e}"));
                        0.0
                    }
                };
                (v / 100.0 * amount.resolve(ctx) / 100.0).clamp(-1.0, 1.0)
            }
            TextSelector::Range { start, end, offset, amount, shape, ease_high, ease_low, randomize, seed } => {
                let i = if *randomize { shuffled(i, n, *seed) } else { i };
                let nf = n.max(1) as f64;
                let o = offset.resolve(ctx);
                let (a, b) = (start.resolve(ctx) + o, end.resolve(ctx) + o);
                let (lo, hi) = (a.min(b) / 100.0 * nf, a.max(b) / 100.0 * nf);
                let i = i as f64;
                let v = match shape {
                    RangeShape::Square => (hi.min(i + 1.0) - lo.max(i)).clamp(0.0, 1.0),
                    _ => {
                        // Where the unit's centre sits across the span, 0..1.
                        let c = i + 0.5;
                        let t = if hi - lo < 1e-9 {
                            if c < lo { -1.0 } else { 2.0 }
                        } else {
                            (c - lo) / (hi - lo)
                        };
                        let inside = (0.0..=1.0).contains(&t);
                        match shape {
                            RangeShape::RampUp => t.clamp(0.0, 1.0),
                            RangeShape::RampDown => 1.0 - t.clamp(0.0, 1.0),
                            _ if !inside => 0.0,
                            RangeShape::Triangle => 1.0 - (2.0 * t - 1.0).abs(),
                            RangeShape::Round => (1.0 - (2.0 * t - 1.0).powi(2)).max(0.0).sqrt(),
                            _ => 0.5 - 0.5 * (std::f64::consts::TAU * t).cos(),
                        }
                    }
                };
                ease(v, *ease_high, *ease_low) * amount.resolve(ctx) / 100.0
            }
            TextSelector::Stagger { amount, delays, step, shift, reverse } => {
                let i = if *reverse { n.saturating_sub(1 + i) } else { i };
                let delay = match delays.len() {
                    0 => i as f64 * step,
                    k if i < k => delays[i],
                    k => delays[k - 1] + (i + 1 - k) as f64 * step,
                };
                let at = ctx.frame;
                ctx.frame = at - shift - delay;
                let v = amount.resolve(ctx);
                ctx.frame = at;
                (v / 100.0).clamp(0.0, 1.0)
            }
            TextSelector::Wiggly { max, min, wiggles, correlation, seed } => {
                let (hi, lo) = (max.resolve(ctx), min.resolve(ctx));
                let t = ctx.frame / ctx.comp_fps().max(1e-9) * wiggles.resolve(ctx);
                let seed = *seed as f64 * 7919.0;
                let shared = crate::expr::value_noise(t, seed);
                let own = crate::expr::value_noise(t + i as f64 * 17.31, seed + 1.0 + i as f64);
                let k = (correlation / 100.0).clamp(0.0, 1.0);
                let w = shared * k + own * (1.0 - k); // -1..1
                ((lo + (hi - lo) * (w * 0.5 + 0.5)) / 100.0).clamp(-1.0, 1.0)
            }
            // Needs the subtitle's active word, which only the text walk
            // knows; it answers this variant itself.
            TextSelector::Karaoke { .. } => 0.0,
        }
    }
}

/// Unit `i`'s place in a stable shuffle of `0..n` by `seed`.
fn shuffled(i: usize, n: usize, seed: u32) -> usize {
    let mut order: Vec<usize> = (0..n).collect();
    let mut h = 0x9E37_79B9_7F4A_7C15u64 ^ seed as u64;
    for k in (1..n).rev() {
        h ^= h >> 33;
        h = h.wrapping_mul(0xFF51_AFD7_ED55_8CCD);
        h ^= h >> 29;
        order.swap(k, (h % (k as u64 + 1)) as usize);
    }
    order.get(i).copied().unwrap_or(i)
}

/// AE's Ease High / Ease Low on a selection value `v` (0..1): positive eases
/// flatten the curve near that end, negative ones sharpen it.
fn ease(v: f64, high: f64, low: f64) -> f64 {
    if high == 0.0 && low == 0.0 {
        return v;
    }
    let sign = v.signum();
    let x = v.abs().clamp(0.0, 1.0);
    let bend = |x: f64, e: f64| {
        let e = (e / 100.0).clamp(-1.0, 1.0);
        // e > 0: x^(1+2e) near 0 (slow start); e < 0: the mirror.
        if e >= 0.0 { x.powf(1.0 + 2.0 * e) } else { 1.0 - (1.0 - x).powf(1.0 - 2.0 * e) }
    };
    // Low shapes the approach from 0, high the approach to 1.
    let y = if x < 0.5 { 0.5 * bend(2.0 * x, low) } else { 1.0 - 0.5 * bend(2.0 - 2.0 * x, high) };
    sign * y
}

/// A stroke: animatable color and width, and how its ends and corners look.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Stroke {
    pub color: Value<Color>,
    pub width: Value<f64>,
    #[serde(default, skip_serializing_if = "StrokeStyle::is_default")]
    pub style: StrokeStyle,
}

/// End caps and corner joins. Not animatable: they are switches, like AE's.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct StrokeStyle {
    pub cap: LineCap,
    pub join: LineJoin,
    /// A miter longer than this many stroke widths is beveled instead.
    pub miter_limit: f64,
    /// Dash and gap lengths; `None` is a solid line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dash: Option<[f64; 2]>,
}

impl Default for StrokeStyle {
    fn default() -> Self {
        Self { cap: LineCap::Butt, join: LineJoin::Miter, miter_limit: 4.0, dash: None }
    }
}

impl StrokeStyle {
    fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// The kurbo stroke of `width` in this style, for the vector backends.
    pub fn kurbo(&self, width: f64) -> kurbo::Stroke {
        let cap = match self.cap {
            LineCap::Butt => kurbo::Cap::Butt,
            LineCap::Round => kurbo::Cap::Round,
            LineCap::Square => kurbo::Cap::Square,
        };
        let join = match self.join {
            LineJoin::Miter => kurbo::Join::Miter,
            LineJoin::Round => kurbo::Join::Round,
            LineJoin::Bevel => kurbo::Join::Bevel,
        };
        let s = kurbo::Stroke::new(width).with_caps(cap).with_join(join).with_miter_limit(self.miter_limit);
        match self.dash {
            Some(d) => s.with_dashes(0.0, d),
            None => s,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum LineCap {
    Butt,
    Round,
    Square,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum LineJoin {
    Miter,
    Round,
    Bevel,
}

/// A user-exposed control on a node: a named, animatable knob that expressions
/// and scripts read by name (`param("speed")`).
///
/// This is the piece that makes a node a *reusable* thing rather than a bag of
/// hardcoded values — one parameter can drive many properties, and (once a
/// composition can be nested) it's what a pre-comp exposes to its parent.
/// A parameter is a `Value` like any property, so it keyframes and can itself
/// be expression-driven.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Param {
    /// How a script names it. Unique per node — [`Node::set_param`] enforces
    /// that, since a duplicate would make `param("x")` ambiguous.
    pub name: String,
    pub value: ParamValue,
}

/// A parameter's type. Mirrors the `ExprValue` kinds, so a parameter can drive
/// any property an expression can — including a text one.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ParamValue {
    Num(Value<f64>),
    Vec(Value<Vec2>),
    Color(Value<Color>),
    /// A text knob. Its `Value<String>` keyframes as a step track like any
    /// other string, so a module can expose "the caption" and each link supply
    /// its own.
    Str(Value<String>),
}

impl ParamValue {
    /// Resolve to the dynamic expression space. Takes `&mut EvalCtx` because a
    /// parameter's own value may be an expression.
    pub fn resolve(&self, ctx: &mut EvalCtx) -> crate::expr::ExprValue {
        use crate::expr::ToExpr;
        match self {
            ParamValue::Num(v) => v.resolve(ctx).to_expr(),
            ParamValue::Vec(v) => v.resolve(ctx).to_expr(),
            ParamValue::Color(v) => v.resolve(ctx).to_expr(),
            ParamValue::Str(v) => v.resolve(ctx).to_expr(),
        }
    }

    /// The knob's value as a plain literal, when it *is* one.
    ///
    /// `None` for a keyframed or expression-driven knob — a knob is a `Value`
    /// like any property, so it can be animated, and an editor that showed one
    /// number for a whole track would be lying. Callers offer an editor only
    /// when this is `Some`, and never overwrite a track with a constant behind
    /// the user's back.
    pub fn as_const(&self) -> Option<crate::expr::ExprValue> {
        use crate::expr::ToExpr;
        match self {
            ParamValue::Num(Value::Const(v)) => Some(v.to_expr()),
            ParamValue::Vec(Value::Const(v)) => Some(v.to_expr()),
            ParamValue::Color(Value::Const(v)) => Some(v.to_expr()),
            ParamValue::Str(Value::Const(v)) => Some(v.to_expr()),
            _ => None,
        }
    }

    /// Set the knob to a constant, **keeping its declared type**. A literal of
    /// the wrong shape is ignored rather than silently retyping the knob: an
    /// expression reading `param("x")` as a vector must not find a number there
    /// because an editor sent the wrong variant.
    pub fn set_const(&mut self, v: crate::expr::ExprValue) {
        use crate::expr::ExprValue as E;
        match (self, v) {
            (ParamValue::Num(slot), E::Num(n)) => *slot = Value::constant(n),
            (ParamValue::Vec(slot), E::Vec3(p)) => *slot = Value::constant(p.xy()),
            (ParamValue::Color(slot), E::Color(c)) => *slot = Value::constant(c),
            (ParamValue::Str(slot), E::Str(s)) => *slot = Value::constant(s),
            _ => {}
        }
    }

    /// The label a picker shows, and the word a serialized param reads as.
    pub fn kind_name(&self) -> &'static str {
        match self {
            ParamValue::Num(_) => "number",
            ParamValue::Vec(_) => "vector",
            ParamValue::Color(_) => "color",
            ParamValue::Str(_) => "text",
        }
    }
}

/// A layer's own time range, in **composition frames**.
///
/// Absent (`None`) means today's behaviour: the layer is live for the whole
/// comp and its local time *is* comp time. Present, it does two separable
/// things:
///
/// - **Trim** — the layer only draws while `comp_frame` is inside `[in_, out)`.
///   Half-open so two clips that meet at frame N don't both draw on N.
/// - **Slip** — `start` is the comp frame at which the layer's *local* frame 0
///   lands, so `local = comp_frame − start`. Keyframes and expressions inside
///   the layer are authored against that local frame, which is what lets one
///   animation be reused at a different in-point without moving any keys.
///
/// `start` is independent of `in_` on purpose: dragging the whole clip moves
/// all three together, but trimming an edge moves `in_`/`out` alone (the
/// content stays put) and slipping moves `start` alone (the window stays put).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LayerTiming {
    /// Comp frame where the layer's local frame 0 sits.
    pub start: i64,
    /// First comp frame the layer draws on.
    pub in_: i64,
    /// First comp frame it no longer draws on (exclusive).
    pub out: i64,
}

impl LayerTiming {
    /// A clip occupying `[in_, out)` with its local time starting at `in_` —
    /// what a freshly-trimmed layer gets.
    pub fn new(in_: i64, out: i64) -> Self {
        Self { start: in_, in_, out }
    }

    /// This layer's local frame for a given comp frame. Fractional in, so a
    /// playhead between frames stays between frames.
    pub fn local_frame(&self, comp_frame: f64) -> f64 {
        comp_frame - self.start as f64
    }

    /// Whether the layer draws at `comp_frame`. Half-open: `out` is the first
    /// frame that no longer draws.
    pub fn is_live(&self, comp_frame: f64) -> bool {
        comp_frame >= self.in_ as f64 && comp_frame < self.out as f64
    }

    /// Rescale this window onto a new frame grid, keeping its wall-clock
    /// position and length. `start` moves with it so the layer's local time —
    /// and therefore its keyframes — stays aligned to the same comp instants.
    pub(crate) fn retime(&mut self, ratio: f64) {
        let scale = |f: i64| (f as f64 * ratio).round() as i64;
        self.start = scale(self.start);
        self.in_ = scale(self.in_);
        self.out = scale(self.out);
    }

    /// Length of the visible window in frames (never negative).
    pub fn len(&self) -> i64 {
        (self.out - self.in_).max(0)
    }
}

/// Trim Paths: the part of each contour between `start` and `end` (fractions
/// of its length, 0–1), turned along it by `offset` (1 = a full lap).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Trim {
    pub start: Value<f64>,
    pub end: Value<f64>,
    pub offset: Value<f64>,
}

/// One node in the scene graph. A group (no shape) just composes its children;
/// a leaf carries a shape + paint.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Node {
    pub id: NodeId,
    pub name: String,
    pub transform: Transform,
    pub shape: Option<Shape>,
    pub fill: Option<Value<Color>>,
    pub stroke: Option<Stroke>,
    /// User-exposed controls, in display order. `#[serde(default)]` so a `.pbc`
    /// written before parameters existed still loads.
    #[serde(default)]
    pub params: Vec<Param>,
    /// Per-layer time range. `None` = live for the whole comp, local time =
    /// comp time (every layer before this field existed), so `#[serde(default)]`
    /// is the whole migration: an old `.pbc` loads unchanged.
    #[serde(default)]
    pub timing: Option<LayerTiming>,
    /// This layer *instances* another composition. Its own `shape`/`fill` still
    /// draw (a precomp layer is a normal layer that also renders a comp), and
    /// its `transform`/`opacity` fold into everything the nested comp emits.
    ///
    /// The nested comp is evaluated at this layer's **local** frame, so trimming
    /// and slipping a precomp retimes its whole contents — the reason the time
    /// model came first.
    #[serde(default)]
    pub precomp: Option<CompId>,
    /// How this layer combines with what is behind it.
    ///
    /// Anything but [`BlendMode::Normal`] makes the layer **isolated**: its
    /// subtree is composited into an image of its own and that image is blended
    /// into the frame. `#[serde(default)]` is the whole migration — every `.pbc`
    /// written before compositing existed loads as `Normal` and renders exactly
    /// as it did.
    ///
    /// **A blend mode is not inherited.** It covers this layer's own content —
    /// its artwork, and the composition it instances if it is a precomp — and
    /// stops there. A child layer composites on its own terms, with its own
    /// mode.
    ///
    /// The precomp half matters: a precomp layer's content genuinely *is* the
    /// nested comp, so blending one blends everything inside it, which is what
    /// putting a blend mode on a precomp is for. Its `children`, by contrast,
    /// are separate layers that merely inherit its transform — so a group with
    /// a blend mode and no artwork of its own does nothing, exactly as a null
    /// does.
    #[serde(default)]
    pub blend: BlendMode,
    /// A shape limiting where this layer draws.
    ///
    /// Scoped exactly like [`Node::blend`] — the layer's own content, and the
    /// comp it instances, but **not** its children. A mask on a group is
    /// therefore a no-op for the same reason a blend mode on one is: children
    /// are separate layers, not content.
    ///
    /// Masking forces isolation, because a clip has to apply to the finished
    /// content rather than to each item: a layer with a fill and a stroke is
    /// one picture, and clipping the two separately would let the stroke
    /// survive where the fill was cut.
    #[serde(default)]
    pub mask: Option<Mask>,
    /// Take coverage from the layer **above** this one — the next sibling in
    /// document order, which the layers panel shows directly above it.
    ///
    /// The matte layer is consumed rather than drawn: it contributes its shape
    /// and not its pixels. Set on the layer being cut, not on the one doing the
    /// cutting, which is the After Effects convention and reads correctly in
    /// the panel ("this layer is matted by the one above").
    ///
    /// `None`, and no next sibling, both mean no matte — a matte on the topmost
    /// layer of a group has nothing to take its shape from, so it simply draws.
    #[serde(default)]
    pub matte: Option<MatteMode>,
    /// Turns this group into a **compound path**: its direct shape children are
    /// combined by the boolean op into one filled contour drawn with *this*
    /// node's fill/stroke, and the children are consumed rather than drawn on
    /// their own. `None` is an ordinary group. `#[serde(default)]` so every older
    /// `.pbc` loads as a plain group. See [`crate::pathfinder`].
    #[serde(default)]
    pub compound: Option<crate::pathfinder::BoolOp>,
    /// The layer's effect stack: ordered pixel operations applied to its
    /// finished, isolated image before it composites with the backdrop.
    ///
    /// Scoped like [`Node::blend`] and [`Node::mask`] — the layer's own content
    /// and the comp it instances, never its children — because an effect reworks
    /// the pixels this layer produced, and its children are separate layers.
    /// Empty for every layer that has no effects, which is the ordinary case and
    /// `#[serde(default)]` so every older `.pbc` loads with an empty stack.
    ///
    /// A non-empty stack forces isolation (see [`Node::needs_isolation`]) for the
    /// same reason a blend mode does: an effect acts on the layer as one finished
    /// image, not on each item as it is painted.
    #[serde(default)]
    pub effects: Vec<crate::effect::Effect>,
    /// The sound this layer contributes, if any.
    ///
    /// **A field beside the picture, not a [`Shape`] variant.** The deciding
    /// case is a video with a soundtrack: one layer has both, so audio cannot be
    /// the thing a layer draws. It is also orthogonal to everything drawing
    /// cares about — a sound has no geometry, no fill, no bounds — and a
    /// `Shape::Audio` would have meant a meaningless arm in every match that
    /// asks a shape for its path.
    ///
    /// Timing rides the layer's own [`LayerTiming`], so trimming a sound is
    /// trimming a layer, and nothing new had to learn about in and out points.
    /// `#[serde(default)]`, so a pre-audio `.pbc` loads silent.
    #[serde(default)]
    pub audio: Option<crate::audio::AudioClip>,
    /// This layer's motion-blur switch. It only matters when the comp's
    /// [`MotionBlur`] is on; then a layer with it off renders at the frame
    /// centre while switched layers move through the shutter. Covers the
    /// subtree. On by default so the comp switch alone blurs everything.
    #[serde(default = "Node::default_motion_blur")]
    pub motion_blur: bool,
    /// The eye switch. A hidden layer draws nothing and sounds nothing, and
    /// neither do its children — hiding a group hides what is in it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub hidden: bool,
    /// An adjustment layer: draws nothing itself, and runs its effects on
    /// everything its siblings below it have drawn (AE's adjustment switch).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub adjustment: bool,
    /// Per-character motion for a text shape (AE's text animators), applied in
    /// order. Ignored on any other shape.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub text_animators: Vec<TextAnimator>,
    /// A transcript that writes this text layer's content: each frame shows
    /// the subtitle on screen then, replacing `content`. Ignored on any other
    /// shape.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subtitles: Option<crate::subs::Subtitles>,
    /// Draw only part of the path, by length (AE's Trim Paths).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trim: Option<Trim>,
    /// The lock switch: an editor-only guard against touching the layer on the
    /// canvas or in the properties panel. Renders exactly as unlocked. Covers
    /// the subtree, like `hidden`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub locked: bool,
    /// Follow another layer's transform without nesting under it — After
    /// Effects parenting. Nesting a layer under its parent would also move it
    /// in the stack and make it inherit the parent's visibility, trim and
    /// opacity; AE parenting does none of that, so it needs its own link.
    ///
    /// The linked layer's transform (and its own link, recursively) is
    /// resolved at *its* local time and applied between this layer's parent
    /// and its own transform. Intermediate groups between the linked layer and
    /// the comp root are not included: links are meant between siblings,
    /// which is how an AE import lays layers out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<NodeId>,
    pub children: Vec<Node>,
}

impl Node {
    /// Whether this layer must be composited as its own image.
    ///
    /// The single place that answers it, so the walk and any future backend
    /// can't disagree about which layers cost an offscreen target.
    pub fn needs_isolation(&self) -> bool {
        self.blend.needs_isolation() || self.mask.is_some() || !self.effects.is_empty()
    }
}

impl Node {
    fn default_motion_blur() -> bool {
        true
    }

    pub fn group(id: u64, name: impl Into<String>) -> Self {
        Self {
            id: NodeId(id),
            name: name.into(),
            transform: Transform::default(),
            shape: None,
            fill: None,
            stroke: None,
            params: Vec::new(),
            timing: None,
            precomp: None,
            blend: BlendMode::default(),
            mask: None,
            matte: None,
            compound: None,
            effects: Vec::new(),
            audio: None,
            motion_blur: true,
            hidden: false,
            adjustment: false,
            text_animators: Vec::new(),
            subtitles: None,
            trim: None,
            locked: false,
            link: None,
            children: Vec::new(),
        }
    }

    pub fn shape(id: u64, name: impl Into<String>, shape: Shape) -> Self {
        Self {
            id: NodeId(id),
            name: name.into(),
            transform: Transform::default(),
            shape: Some(shape),
            fill: None,
            stroke: None,
            params: Vec::new(),
            timing: None,
            precomp: None,
            blend: BlendMode::default(),
            mask: None,
            matte: None,
            compound: None,
            effects: Vec::new(),
            audio: None,
            motion_blur: true,
            hidden: false,
            adjustment: false,
            text_animators: Vec::new(),
            subtitles: None,
            trim: None,
            locked: false,
            link: None,
            children: Vec::new(),
        }
    }

    pub fn with_fill(mut self, color: Color) -> Self {
        self.fill = Some(Value::constant(color));
        self
    }

    /// Look a parameter up by name.
    pub fn param(&self, name: &str) -> Option<&Param> {
        self.params.iter().find(|p| p.name == name)
    }

    /// Add a parameter, or replace the one already using that name. Names are
    /// the only way a script addresses a parameter, so duplicates can't exist:
    /// `param("x")` has to mean one thing.
    pub fn set_param(&mut self, name: impl Into<String>, value: ParamValue) {
        let name = name.into();
        match self.params.iter_mut().find(|p| p.name == name) {
            Some(existing) => existing.value = value,
            None => self.params.push(Param { name, value }),
        }
    }

    /// Remove a parameter by name, returning whether it was there. Expressions
    /// referencing it aren't rewritten — they warn and fall back, the same as
    /// any other dangling reference.
    pub fn remove_param(&mut self, name: &str) -> bool {
        let before = self.params.len();
        self.params.retain(|p| p.name != name);
        before != self.params.len()
    }

    /// Builder form of [`Node::set_param`].
    pub fn with_param(mut self, name: impl Into<String>, value: ParamValue) -> Self {
        self.set_param(name, value);
        self
    }

    /// Give this node a stroke. The counterpart to [`Node::with_fill`], which
    /// takes a flat colour; a stroke has two animatable channels, so it takes
    /// the whole [`Stroke`].
    pub fn with_stroke(mut self, stroke: Stroke) -> Self {
        self.stroke = Some(stroke);
        self
    }

    /// Make this layer an instance of `comp`. See [`Node::precomp`].
    pub fn with_precomp(mut self, comp: CompId) -> Self {
        self.precomp = Some(comp);
        self
    }

    /// Give this layer a time range (trim + slip). See [`LayerTiming`].
    pub fn with_timing(mut self, timing: LayerTiming) -> Self {
        self.timing = Some(timing);
        self
    }

    pub fn with_transform(mut self, transform: Transform) -> Self {
        self.transform = transform;
        self
    }

    pub fn with_child(mut self, child: Node) -> Self {
        self.children.push(child);
        self
    }

    /// Depth-first search for a node by id, self included.
    pub fn find(&self, id: NodeId) -> Option<&Node> {
        if self.id == id {
            return Some(self);
        }
        self.children.iter().find_map(|c| c.find(id))
    }

    /// Depth-first search for a node by name, self included. Names aren't
    /// unique, so this is "the first one in tree order" — what a script's
    /// `value("A", …)` resolves to.
    pub fn find_named(&self, name: &str) -> Option<&Node> {
        if self.name == name {
            return Some(self);
        }
        self.children.iter().find_map(|c| c.find_named(name))
    }

    /// Mutable depth-first search for a node by id, self included.
    pub fn find_mut(&mut self, id: NodeId) -> Option<&mut Node> {
        if self.id == id {
            return Some(self);
        }
        self.children.iter_mut().find_map(|c| c.find_mut(id))
    }

    /// The node whose `children` directly contains `id`, if `id` is anywhere in
    /// this subtree. `None` for the root's own id (it has no parent here) or a
    /// missing id. The seam group/ungroup reach through to splice the tree.
    pub fn parent_of_mut(&mut self, id: NodeId) -> Option<&mut Node> {
        if self.children.iter().any(|c| c.id == id) {
            return Some(self);
        }
        self.children.iter_mut().find_map(|c| c.parent_of_mut(id))
    }

    /// Move the node `id` to be a child of `parent` at `index` in document
    /// order (later = drawn on top), clamped to the end. `index` counts the
    /// parent's children *as they are now*, so a move within one parent means
    /// what it says. Refuses the root, a missing node or parent, and a move
    /// into the node's own subtree. Returns whether anything changed.
    pub fn move_node(&mut self, id: NodeId, parent: NodeId, index: usize) -> bool {
        let valid = id != self.id
            && self.find(parent).is_some()
            && self.find(id).is_some_and(|n| n.find(parent).is_none());
        if !valid {
            return false;
        }
        let from = self.parent_of_mut(id).expect("non-root node has a parent");
        let old = from.children.iter().position(|c| c.id == id).unwrap();
        let same_parent = from.id == parent;
        let mut index = index;
        if same_parent && old < index {
            index -= 1;
        }
        if same_parent && old == index.min(from.children.len() - 1) {
            return false;
        }
        let node = from.children.remove(old);
        let to = self.find_mut(parent).expect("checked above");
        let index = index.min(to.children.len());
        to.children.insert(index, node);
        true
    }

    /// The parent of `id`, read-only.
    pub fn parent_of(&self, id: NodeId) -> Option<&Node> {
        if self.children.iter().any(|c| c.id == id) {
            return Some(self);
        }
        self.children.iter().find_map(|c| c.parent_of(id))
    }

    /// Swap the node with `id` for `new`, returning the old one. Keeps its
    /// position among its siblings, which is draw order — pre-composing must
    /// not restack the layer it replaces.
    pub fn replace(&mut self, id: NodeId, new: Node) -> Option<Node> {
        if let Some(i) = self.children.iter().position(|c| c.id == id) {
            return Some(std::mem::replace(&mut self.children[i], new));
        }
        self.children.iter_mut().find_map(|c| c.replace(id, new.clone()))
    }

    /// Remove the node with `id` from this subtree (cannot remove `self`).
    /// Returns the removed node, or `None` if not found.
    pub fn remove(&mut self, id: NodeId) -> Option<Node> {
        if let Some(i) = self.children.iter().position(|c| c.id == id) {
            return Some(self.children.remove(i));
        }
        self.children.iter_mut().find_map(|c| c.remove(id))
    }

    /// Recursively convert legacy float-seconds keyframes to frames at `fps`.
    pub(crate) fn migrate_frames(&mut self, fps: f64) {
        self.transform.migrate_frames(fps);
        if let Some(shape) = &mut self.shape {
            // Upgrade a legacy baked `Shape::Path` to an editable `Shape::Vector`
            // before touching its keys — a `Vector`'s control points are the
            // things that carry keyframes now.
            shape.migrate();
            shape.migrate_frames(fps);
        }
        // A mask's shape is parametric like any other, so its keys live on the
        // same grid and migrate with everything else.
        if let Some(mask) = &mut self.mask {
            mask.shape.migrate();
            mask.shape.migrate_frames(fps);
        }
        if let Some(fill) = &mut self.fill {
            fill.migrate_frames(fps);
        }
        if let Some(stroke) = &mut self.stroke {
            stroke.color.migrate_frames(fps);
            stroke.width.migrate_frames(fps);
        }
        for effect in &mut self.effects {
            effect.migrate_frames(fps);
        }
        for child in &mut self.children {
            child.migrate_frames(fps);
        }
    }

    /// Recursively move every frame position in this subtree onto a new frame
    /// grid. `ratio` is `new_fps / old_fps`.
    pub(crate) fn retime(&mut self, ratio: f64) {
        self.transform.retime(ratio);
        if let Some(shape) = &mut self.shape {
            shape.retime(ratio);
        }
        if let Some(mask) = &mut self.mask {
            mask.shape.retime(ratio);
        }
        if let Some(fill) = &mut self.fill {
            fill.retime(ratio);
        }
        if let Some(stroke) = &mut self.stroke {
            stroke.color.retime(ratio);
            stroke.width.retime(ratio);
        }
        for effect in &mut self.effects {
            effect.retime(ratio);
        }
        if let Some(timing) = &mut self.timing {
            timing.retime(ratio);
        }
        for a in &mut self.text_animators {
            a.each_value(&mut |v| v.retime(ratio));
            // A stagger's delays are frame counts on the same grid.
            let extra = a.more_selectors.iter_mut().map(|s| &mut s.selector);
            for sel in std::iter::once(&mut a.selector).chain(extra) {
                if let TextSelector::Stagger { delays, step, shift, .. } = sel {
                    delays.iter_mut().for_each(|d| *d *= ratio);
                    *step *= ratio;
                    *shift *= ratio;
                }
            }
        }
        for child in &mut self.children {
            child.retime(ratio);
        }
    }
}

/// One composition: a root node plus its own size, frame rate and length.
///
/// This is what `Document` always was — the rename is the whole point of the
/// multi-comp step. A project holds several of these, and a layer can *instance*
/// one (see [`Node::precomp`]), which is what makes a comp reusable rather than
/// merely nested.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Comp {
    /// What the comp switcher shows. `#[serde(default)]` so a pre-project
    /// `.pbc` loads with an empty name and falls back to a generated label.
    #[serde(default)]
    pub name: String,
    pub width: f64,
    pub height: f64,
    pub fps: f64,
    /// Total length of the composition, in **whole frames**.
    ///
    /// Stored as frames rather than seconds so the comp end always lands on a
    /// frame boundary. It used to be seconds with the count derived, which made
    /// "how many frames is this comp?" a rounding decision: 5.0s at 23.976fps
    /// is 119.88 frames, and a renderer asking how many frames to write got an
    /// answer from `.round()`. Harmless for a playhead, a ±1-frame difference
    /// in an exported file.
    ///
    /// `#[serde(default)]` (0) marks "not read from this file"; [`Comp::migrate`]
    /// then fills it from [`Comp::legacy_duration`] or the default length. Every
    /// loader must call `migrate()` — see the crate docs.
    #[serde(default)]
    pub duration_frames: i64,
    /// The pre-frames `duration`, in seconds, as read from an older `.pbc`.
    ///
    /// Deferred rather than resolved in place (unlike `de_text_content`) only
    /// because the conversion needs `fps`, which serde has no ordering
    /// guarantee about during field-by-field deserialization. Folded by
    /// [`Comp::migrate`] and never written back out.
    #[serde(default, rename = "duration", skip_serializing)]
    legacy_duration: Option<f64>,
    /// One past the highest layer id this comp has ever **removed**. Ids are
    /// handed out from the live tree's max, so without this a deleted top layer's
    /// id comes straight back — and a driver still naming it would seize the
    /// newcomer. Saved, so a reload can't forget it either.
    #[serde(default)]
    pub removed_id_floor: u64,
    /// The colour painted inside the comp bounds, behind every layer. A user
    /// setting, not a constant — `#[serde(default)]` so pre-bg `.pbc` files
    /// load with [`Comp::DEFAULT_BG`] rather than transparent.
    #[serde(default = "Comp::default_bg")]
    pub bg: Color,
    /// How strongly the preview dims everything *outside* the comp bounds —
    /// Blender's camera passepartout, applied to the composition frame. `0.0`
    /// is off, `1.0` is opaque black. Preview-only: it never reaches a render.
    #[serde(default = "Comp::default_passepartout")]
    pub passepartout: f64,
    /// How many frames either side of the playhead the preview's motion path
    /// covers. Preview-only, like `passepartout`.
    #[serde(default = "Comp::default_motion_path_range")]
    pub motion_path_range: i64,
    /// Grid, rulers and guides. Preview-only like the two above, but *saved*:
    /// a guide you dropped to line up a title is part of how the composition is
    /// built, and losing it on reopen would make guides useless for the one job
    /// they exist to do.
    #[serde(default)]
    pub aids: ViewAids,
    /// The comp's camera, or `None` for a flat composition.
    ///
    /// **This is the 2.5D switch.** Absent means depth is stored but never
    /// read, and the render path is the one that existed before 2.5D — which
    /// is why `#[serde(default)]` is the whole migration for every existing
    /// `.pbc`. Present means the comp projects. There is no separate "3D
    /// enabled" flag to fall out of sync with, because the camera's absence
    /// *is* the absence of projection.
    #[serde(default)]
    pub camera: Option<crate::camera::Camera>,
    /// Render-time motion blur. Off by default, and absent from old files.
    #[serde(default)]
    pub motion_blur: MotionBlur,
    pub root: Node,
}

/// Motion blur as a render setting: each output frame is the average of
/// `samples` evaluations spread across the shutter.
///
/// Gated per layer by [`Node::motion_blur`].
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MotionBlur {
    pub enabled: bool,
    /// Degrees of a frame the shutter is open: 180 is the film standard, 360
    /// smears across the whole frame interval.
    pub shutter_angle: f64,
    pub samples: u32,
}

impl Default for MotionBlur {
    fn default() -> Self {
        Self { enabled: false, shutter_angle: 180.0, samples: 8 }
    }
}

impl MotionBlur {
    /// The shutter offsets to sample, relative to the frame and centred on it
    /// (AE's default phase of minus half the angle). Just `[0.0]` when off.
    pub fn offsets(&self) -> Vec<f64> {
        let n = self.samples.clamp(1, 64);
        let span = self.shutter_angle.clamp(0.0, 720.0) / 360.0;
        if !self.enabled || n == 1 || span == 0.0 {
            return vec![0.0];
        }
        (0..n).map(|i| -span / 2.0 + span * (i as f64 + 0.5) / n as f64).collect()
    }
}

/// The preview's alignment aids. Grouped rather than five loose fields on
/// [`Comp`] because they are one feature to the user — the thing you toggle
/// when you're positioning something — and they are read together everywhere.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ViewAids {
    pub grid: Grid,
    /// Rulers along the preview's top and left edges. They take space from the
    /// canvas rather than floating over it, so toggling this resizes the
    /// drawing area.
    pub rulers: bool,
    pub guides: Guides,
    /// Whether a canvas drag snaps to the aids. Deliberately one flag rather
    /// than one per target: **you snap to what you can see**, so showing the
    /// grid arms grid snapping and hiding it disarms it. One less thing to keep
    /// consistent, and it matches what the screen is already telling you.
    /// Composition edges and centre always snap — they exist whether or not
    /// anything is shown.
    #[serde(default = "ViewAids::default_snap")]
    pub snap: bool,
    #[serde(default)]
    pub onion: Onion,
}

impl ViewAids {
    fn default_snap() -> bool {
        true
    }
}

impl Default for ViewAids {
    fn default() -> Self {
        Self {
            grid: Grid::default(),
            rulers: false,
            guides: Guides::default(),
            snap: Self::default_snap(),
            onion: Onion::default(),
        }
    }
}

/// Onion skins: ghosts of the frame either side of the playhead, so you can see
/// where the animation came from and where it is going without scrubbing.
///
/// This is the whole-layer answer to every property a motion path can't draw.
/// A path works for position because position *is* a spatial curve; rotation,
/// scale, opacity, colour and shape have no such geometry, but a ghost of the
/// rendered layer shows all of them at once.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Onion {
    pub visible: bool,
    /// Ghosts drawn before and after the playhead.
    pub before: u32,
    pub after: u32,
    /// Frames between consecutive ghosts. At 60fps neighbouring frames are
    /// nearly identical, so a step of 1 would draw six copies of the same
    /// picture; spacing them out is what makes the motion legible.
    pub step: i64,
    /// Opacity of the nearest ghost. Further ones fade from here.
    pub opacity: f64,
}

impl Default for Onion {
    fn default() -> Self {
        // Off by default, like the grid: ghosts over someone's artwork when
        // they didn't ask is worse than one click away.
        Self { visible: false, before: 3, after: 3, step: 2, opacity: 0.35 }
    }
}

impl Onion {
    /// Ghosts are capped because each one costs a full scene evaluation — this
    /// stops a typo in a `.pbc` from turning one repaint into thousands.
    pub const MAX_GHOSTS: u32 = 20;

    /// Frame offsets to draw, nearest first, paired with how far through the
    /// fade each one is (`0.0` nearest, `1.0` furthest).
    ///
    /// Returns nothing when hidden, so callers need no second check. A `step`
    /// of zero or less would stack every ghost on the playhead, so it clamps.
    pub fn offsets(&self) -> Vec<(i64, f64)> {
        if !self.visible {
            return Vec::new();
        }
        let step = self.step.max(1);
        let mut out = Vec::new();
        for (n, sign) in [(self.before, -1i64), (self.after, 1i64)] {
            let n = n.min(Self::MAX_GHOSTS);
            for i in 1..=n {
                // Fade across the count, not across the frame distance, so the
                // nearest ghost is always the most solid whatever the step is.
                let t = if n > 1 { (i - 1) as f64 / (n - 1) as f64 } else { 0.0 };
                out.push((sign * step * i as i64, t));
            }
        }
        out
    }
}

/// A regular grid drawn inside the composition bounds.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Grid {
    pub visible: bool,
    /// Distance between major lines, in **composition pixels** — so the grid
    /// describes the composition, not the screen, and stays put as you zoom.
    pub spacing: f64,
    /// Minor lines between each pair of major ones. `1` means none.
    pub subdivisions: u32,
}

impl Default for Grid {
    fn default() -> Self {
        // Off by default: an unasked-for grid over someone's artwork is worse
        // than one keystroke away. 100px with quarters suits the 1920×1080
        // default without dividing it awkwardly.
        Self { visible: false, spacing: Self::DEFAULT_SPACING, subdivisions: 4 }
    }
}

impl Grid {
    /// Spacing clamped to something drawable. A zero or negative spacing would
    /// make the line loop in the renderer never terminate.
    pub const MIN_SPACING: f64 = 1.0;
    pub const MAX_SPACING: f64 = 10_000.0;
    pub const DEFAULT_SPACING: f64 = 100.0;

    pub fn step(&self) -> f64 {
        // The NaN check is load-bearing, not defensive noise: `f64::clamp`
        // *propagates* NaN, and the renderer advances its line loop by `v +=
        // step`. A NaN step never advances, so the loop never terminates and
        // the editor hangs. A hand-edited `.pbc` is all it takes.
        if !self.spacing.is_finite() {
            return Self::DEFAULT_SPACING;
        }
        self.spacing.clamp(Self::MIN_SPACING, Self::MAX_SPACING)
    }

    /// Distance between minor lines, or `None` when there are no subdivisions.
    pub fn minor_step(&self) -> Option<f64> {
        (self.subdivisions > 1).then(|| self.step() / self.subdivisions as f64)
    }
}

/// Draggable alignment lines, in composition coordinates.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Guides {
    /// Hiding guides keeps them in the document — this is a view toggle, not a
    /// delete. Dragging one back onto its ruler is how you remove it.
    pub visible: bool,
    pub items: Vec<Guide>,
}

impl Default for Guides {
    fn default() -> Self {
        // Visible, unlike the grid: a guide only exists because the user made
        // one, so hiding it by default would hide their own work.
        Self { visible: true, items: Vec::new() }
    }
}

/// One alignment line. `at` is the coordinate on the axis it *crosses*: a
/// [`GuideAxis::Vertical`] guide is a vertical line at `x == at`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Guide {
    pub axis: GuideAxis,
    pub at: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum GuideAxis {
    Vertical,
    Horizontal,
}

impl Comp {
    /// The out-of-the-box composition background (#5d677e).
    pub const DEFAULT_BG: Color = Color::rgb(0.364_706, 0.403_922, 0.494_118);

    fn default_bg() -> Color {
        Self::DEFAULT_BG
    }

    /// Blender's own default is 0.5, and it reads well here for the same
    /// reason: enough to push the surroundings back without hiding what a
    /// layer is doing as it moves out of frame.
    pub const DEFAULT_PASSEPARTOUT: f64 = 0.5;

    fn default_passepartout() -> f64 {
        Self::DEFAULT_PASSEPARTOUT
    }

    /// Two seconds either side at 30fps, one at 60 — enough to read the shape
    /// of a move without burying the frame in dots.
    pub const DEFAULT_MOTION_PATH_RANGE: i64 = 60;

    fn default_motion_path_range() -> i64 {
        Self::DEFAULT_MOTION_PATH_RANGE
    }

    /// Five seconds at the default 60fps — the length a new comp used to get
    /// when the field was `duration: 5.0` seconds, preserved exactly so a fresh
    /// document is unchanged by the move to frames.
    pub const DEFAULT_DURATION_FRAMES: i64 = 300;

    pub fn new(width: f64, height: f64, root: Node) -> Self {
        Self {
            name: String::new(),
            // A new comp is flat: depth exists on every layer but nothing
            // reads it until you add a camera. New work behaves like old work
            // until you ask for otherwise.
            camera: None,
            motion_blur: MotionBlur::default(),
            width,
            height,
            fps: 60.0,
            duration_frames: Comp::DEFAULT_DURATION_FRAMES,
            legacy_duration: None,
            removed_id_floor: 0,
            bg: Self::DEFAULT_BG,
            passepartout: Self::DEFAULT_PASSEPARTOUT,
            motion_path_range: Self::DEFAULT_MOTION_PATH_RANGE,
            aids: ViewAids::default(),
            root,
        }
    }

    /// The name to show, falling back to a generated one so a comp is never
    /// nameless in the UI — old files and freshly split comps both land here.
    pub fn label(&self, id: CompId) -> String {
        if self.name.trim().is_empty() {
            format!("Comp {}", id.0 + 1)
        } else {
            self.name.clone()
        }
    }

    /// The composition's frame grid. Every seconds↔frames conversion and every
    /// timecode string goes through this — never divide by `fps` by hand.
    pub fn timebase(&self) -> crate::timebase::Timebase {
        crate::timebase::Timebase::new(self.fps)
    }

    /// Bring a freshly-deserialized document up to the current format.
    ///
    /// Today that means converting legacy float-seconds keyframes to frames
    /// using this document's `fps`. Must be called after *every* load — it is
    /// a no-op on an already-migrated doc, so calling it twice is safe.
    pub fn migrate(&mut self) {
        let fps = self.timebase().fps();
        // `duration_frames` at 0 means the file predates it. Fold the old
        // seconds field if it carried one, else fall back to the default
        // length — a comp with no duration at all would render nothing.
        if self.duration_frames == 0 {
            self.duration_frames = match self.legacy_duration.take() {
                Some(secs) => self.timebase().seconds_to_frames(secs).max(1),
                None => Comp::DEFAULT_DURATION_FRAMES,
            };
        }
        self.legacy_duration = None;
        self.root.migrate_frames(fps);
        if let Some(camera) = &mut self.camera {
            camera.migrate_frames(fps);
        }
    }

    /// Change the comp's frame rate, keeping every animated thing at the same
    /// **wall-clock time**. A key at frame 120 @ 60fps (two seconds in) is at
    /// frame 48 after switching to 24fps — the grid changes underneath the
    /// animation, the animation does not re-time.
    ///
    /// This is the only supported way to write `fps` on a comp that already has
    /// content: assigning the field directly leaves keys on their old frame
    /// numbers, which silently shifts them in seconds.
    ///
    /// Frames are whole, so a rate change is lossy — keys land on the nearest
    /// frame of the new grid, and two keys less than a frame apart can merge.
    /// Returns whether anything changed.
    pub fn set_fps(&mut self, fps: f64) -> bool {
        let old = self.timebase().fps();
        self.fps = fps;
        let new = self.timebase().fps();
        if new == old {
            return false;
        }
        // The length is in frames now, so it re-grids with everything else:
        // a 5-second comp stays 5 seconds long across a rate change.
        self.duration_frames = ((self.duration_frames as f64) * (new / old)).round() as i64;
        self.duration_frames = self.duration_frames.max(1);
        self.root.retime(new / old);
        // The camera's dolly and orientation are keyframed on the same grid, so
        // they re-time with everything else — miss it and an animated camera
        // drifts out of sync with the scene it is shooting.
        if let Some(camera) = &mut self.camera {
            camera.retime(new / old);
        }
        true
    }

    /// Total length of the composition in seconds — a *presentation* unit,
    /// derived from the stored frame count. Playback and the timecode readout
    /// want it; nothing should store it.
    pub fn duration_seconds(&self) -> f64 {
        self.timebase().frames_to_seconds(self.duration_frames as f64)
    }

    /// Set the length from a duration in seconds, snapping to whole frames.
    /// The edge where a user types "5s" and the comp has to pick a frame.
    pub fn set_duration_seconds(&mut self, seconds: f64) {
        self.duration_frames = self.timebase().seconds_to_frames(seconds).max(1);
    }
}

/// What a single-composition document used to be. Kept as an alias so the
/// hundreds of existing `Document` mentions still read, and so a `.pbc` written
/// before projects existed still deserializes into exactly this shape.
pub type Document = Comp;

/// Identifies a shared animation module within a [`Project`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ModuleId(pub u64);

/// A **named driver stored once for the whole project** that many properties
/// link to — edit it once, every link updates.
///
/// This is the promotion of a pattern the expression graph already supported by
/// convention (park the animation on a "controller" node and `Ref` it) into a
/// first-class object. What it adds over that convention is a real definition
/// site, per-link overrides, and — because the body reads `t01`/`localTime` —
/// automatic retiming to whichever layer resolves it.
///
/// A module is deliberately just an [`crate::expr::Expr`] plus its knobs: the
/// procedural generators are the ready-made bodies, and nothing new is needed in
/// the evaluator beyond the link itself.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Module {
    pub name: String,
    /// The tunables a link may override. A knob left unset at the link site
    /// falls back to the default here — override is a *layering*, not a fork.
    pub params: Vec<Param>,
    /// The graph fragment. Reads its knobs with `param("…")`, which resolve
    /// against the module's own scope rather than any node's.
    ///
    /// **This stays the IR** — it is what `eval_use` runs. When the module is
    /// authored on the node canvas, `graph`/`output` below are the source and
    /// this is their compiled result, recompiled on every edit. That's the same
    /// "alongside, lowers to the IR" contract `Project.graph` has with the
    /// properties it drives, applied one scope up.
    pub body: crate::expr::Expr,
    /// The module's own node canvas — the **document scope** of the node
    /// system. `#[serde(default)]` so a module authored before the canvas
    /// existed loads with an empty graph; opening it seeds the graph by
    /// *raising* `body`, so an old module becomes canvas-editable without a
    /// migration and its layout persists from then on.
    #[serde(default)]
    pub graph: crate::graph::NodeGraph,
    /// Which output of `graph` produces this module's value. `None` means the
    /// body isn't graph-authored (nothing has been laid out yet, or the output
    /// node was deleted) — in which case `body` is left exactly as it is rather
    /// than being blanked by an empty canvas.
    #[serde(default)]
    pub output: Option<crate::graph::Endpoint>,
}

impl Module {
    pub fn new(name: impl Into<String>, body: crate::expr::Expr) -> Self {
        Self {
            name: name.into(),
            params: Vec::new(),
            body,
            graph: crate::graph::NodeGraph::new(),
            output: None,
        }
    }

    /// Add or replace a knob. Same uniqueness rule as a node's parameters: a
    /// duplicate would make `param("x")` ambiguous.
    pub fn with_param(mut self, name: impl Into<String>, value: ParamValue) -> Self {
        self.set_param(name, value);
        self
    }

    /// Add or replace a knob in place — the editing-surface counterpart to
    /// [`Node::set_param`], since a module's body reads its knobs the same way.
    pub fn set_param(&mut self, name: impl Into<String>, value: ParamValue) {
        let name = name.into();
        match self.params.iter_mut().find(|p| p.name == name) {
            Some(existing) => existing.value = value,
            None => self.params.push(Param { name, value }),
        }
    }

    /// Remove a knob by name, returning whether it was there. A body `param("x")`
    /// left reading it warns and falls back, like any dangling reference.
    pub fn remove_param(&mut self, name: &str) -> bool {
        let before = self.params.len();
        self.params.retain(|p| p.name != name);
        before != self.params.len()
    }
}

/// Identifies a composition within a [`Project`]. Stable across edits — a
/// precomp layer stores one, so renaming or reordering comps can't break an
/// instance.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct CompId(pub u64);

/// A project: several compositions, one of which is the one you open.
///
/// **Registry + instances, not inline nesting.** A layer refers to a comp by
/// [`CompId`], so the same comp can be placed twice and edited once — inline
/// nesting would be less code but could never instance. It's also the shape the
/// shared-module story needs later: a comp *is* a graph node.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Project {
    /// Keyed, not a `Vec`: a precomp layer holds an id, and ids must survive
    /// a comp being removed from the middle.
    pub comps: std::collections::BTreeMap<CompId, Comp>,
    /// The comp a fresh open shows — the "main" one.
    pub root: CompId,
    /// Shared animation modules, addressable from any comp — this is the
    /// "document-wide" part of the property graph. `#[serde(default)]` so a
    /// `.pbc` written before modules existed still loads.
    #[serde(default)]
    pub modules: std::collections::BTreeMap<ModuleId, Module>,
    /// The composition node graph and its drivers — the Blender-style node
    /// canvas that drives layer properties (see `crate::graph`). Project-level,
    /// like `modules`: one graph for the project. `#[serde(default)]` so a `.pbc`
    /// written before the node graph existed still loads.
    #[serde(default)]
    pub graph: crate::graph::NodeGraph,
    /// Timing curves saved by hand, on top of [`crate::value::EasePreset::BUILT_IN`].
    /// Project-level because an ease is a house style: the bounce you tuned for
    /// one layer is the one every other layer in the piece should use, and it
    /// has to travel with the `.pbc` for that to hold. `#[serde(default)]` so a
    /// file written before the library existed still loads.
    #[serde(default)]
    pub eases: Vec<crate::value::EasePreset>,
    /// Imported footage, addressable from any comp — project-level for the
    /// same reason `modules` is: one import, usable everywhere, and a relink
    /// fixes every layer that shows it at once. Holds *references* only; see
    /// [`crate::asset`]. `#[serde(default)]` so a `.pbc` written before footage
    /// existed still loads.
    #[serde(default)]
    pub assets: std::collections::BTreeMap<AssetId, Asset>,
    /// Drivers as they were stored **before they became nodes**: a list beside
    /// the graph rather than `out` nodes in it.
    ///
    /// Load-only, and private. [`Project::migrate`] drains both of these into
    /// the graph as sink nodes, and [`crate::graph::NodeGraph::bindings`]
    /// derives the drivers from there on. They stay here purely so a `.pbc`
    /// written before the change still opens with its drivers intact — dropping
    /// the fields would make serde skip the keys silently, which is data loss
    /// wearing a compatible face. `skip_serializing` means nothing writes them
    /// back, so a file round-trips into the new shape once and stays there.
    #[serde(default, rename = "bindings", skip_serializing)]
    legacy_bindings: Vec<crate::graph::Binding>,
    #[serde(default, rename = "shape_bindings", skip_serializing)]
    legacy_shape_bindings: Vec<crate::graph::ShapeBinding>,
    /// How this piece is delivered: named export specs, saved with the project.
    ///
    /// Project data rather than app settings, and that is the whole point —
    /// an export spec is part of how a piece is handed over, so two people
    /// opening the same `.pbc` render identical files. Settings that live in
    /// whoever last opened the dialog are the failure mode this replaces.
    /// `#[serde(default)]` so a file written before presets existed still loads.
    #[serde(default)]
    pub render_presets: Vec<RenderPreset>,
    /// Where **Draft** writes, when the derived default is not wanted.
    ///
    /// `None` means the default: `<project stem>_draft.mp4` beside the project.
    /// That default is the point of Draft — a destination you can guess without
    /// being told — so this is an *override*, not a setting Draft depends on. A
    /// project that has never touched it behaves exactly as one from before the
    /// field existed.
    ///
    /// Stored like a preset's path: relative to the project when it sits under
    /// it, absolute otherwise.
    #[serde(default)]
    pub draft_out: Option<String>,
    /// The Assets panel's folders. Purely organisational: nothing evaluates
    /// them. `#[serde(default)]` so a file written before folders still loads.
    #[serde(default)]
    pub folders: Vec<AssetFolder>,
}

/// Something listed in the Assets panel: an imported file or a composition.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ProjectItem {
    File(AssetId),
    Comp(CompId),
}

/// A named Assets-panel folder. ponytail: flat, no nesting; add a parent
/// index if nested folders are ever wanted.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AssetFolder {
    pub name: String,
    /// Members, in the order they were filed. An item in no folder sits at the
    /// top level; an id whose asset or comp is gone is skipped when listed.
    #[serde(default)]
    pub items: Vec<ProjectItem>,
}

/// A named export spec, saved in the `.pbc`.
///
/// Deliberately small. [`decisions/0018`] is explicit that the preset table
/// stays small and that ffmpeg's own flags cover everything else — the moment
/// this grows a codec matrix it has become the output-module editor that
/// decision declines to build.
///
/// `quality` is a **word**, not the `render` crate's `Quality` enum, because
/// `core` is headless and does not depend on `render` ([`decisions/0001`]).
/// The renderer parses it; an unreadable word falls back to draft rather than
/// refusing to open the project.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RenderPreset {
    /// What the menu shows.
    pub name: String,
    /// Where it writes, relative to the project file unless absolute. The
    /// extension picks the container, exactly as it does on the command line —
    /// one rule, not a format dropdown that can disagree with the filename.
    pub out: String,
    /// `"draft"` or `"master"`.
    #[serde(default = "RenderPreset::default_quality")]
    pub quality: String,
    /// Resolution multiplier. `1.0` is the comp's own size.
    #[serde(default = "RenderPreset::default_scale")]
    pub scale: f64,
    /// Extra ffmpeg arguments, appended *after* the preset's own so they
    /// override rather than fight it.
    #[serde(default)]
    pub ffmpeg_args: Vec<String>,
}

impl RenderPreset {
    fn default_quality() -> String {
        "master".to_string()
    }

    fn default_scale() -> f64 {
        1.0
    }

    /// The preset a project gets when someone first presses Master with nothing
    /// configured: an H.264 deliverable next to the project, at full size.
    pub fn default_master() -> Self {
        RenderPreset {
            name: "Master".to_string(),
            out: "master.mp4".to_string(),
            quality: "master".to_string(),
            scale: 1.0,
            ffmpeg_args: Vec::new(),
        }
    }
}

impl Project {
    /// Wrap a single composition as a whole project. This is also the `.pbc`
    /// migration: a pre-project document loads as one comp, which becomes root.
    pub fn single(comp: Comp) -> Self {
        let root = CompId(0);
        Self {
            comps: [(root, comp)].into_iter().collect(),
            root,
            modules: Default::default(),
            graph: crate::graph::NodeGraph::new(),
            eases: Vec::new(),
            assets: std::collections::BTreeMap::new(),
            legacy_bindings: Vec::new(),
            legacy_shape_bindings: Vec::new(),
            render_presets: Vec::new(),
            draft_out: None,
            folders: Vec::new(),
        }
    }

    /// File `item` into folder `to`, or back to the top level with `None`. An
    /// item lives in at most one folder, so it leaves any other first.
    pub fn file_into(&mut self, item: ProjectItem, to: Option<usize>) {
        for f in &mut self.folders {
            f.items.retain(|i| *i != item);
        }
        if let Some(f) = to.and_then(|i| self.folders.get_mut(i)) {
            f.items.push(item);
        }
    }

    pub fn comp(&self, id: CompId) -> Option<&Comp> {
        self.comps.get(&id)
    }

    pub fn comp_mut(&mut self, id: CompId) -> Option<&mut Comp> {
        self.comps.get_mut(&id)
    }

    pub fn asset(&self, id: AssetId) -> Option<&Asset> {
        self.assets.get(&id)
    }

    pub fn asset_mut(&mut self, id: AssetId) -> Option<&mut Asset> {
        self.assets.get_mut(&id)
    }

    /// Import footage under a fresh id, returning it.
    ///
    /// Takes an [`Asset`] whose `id` is ignored — the same shape as
    /// [`Project::insert`] — so the caller builds metadata from a decoder
    /// without having to know what ids are taken.
    pub fn add_asset(&mut self, mut asset: Asset) -> AssetId {
        let id = AssetId(self.assets.keys().map(|k| k.0 + 1).max().unwrap_or(0));
        asset.id = id;
        self.assets.insert(id, asset);
        id
    }

    /// Forget a piece of footage.
    ///
    /// Layers pointing at it are **left alone**, and warn at render time like a
    /// dangling precomp does. Silently deleting them would turn "I removed the
    /// wrong item from the project panel" into lost work, and repointing them
    /// at nothing would lose the size and timing the user set up around the
    /// footage — a relink is meant to be recoverable.
    pub fn remove_asset(&mut self, id: AssetId) -> Option<Asset> {
        self.assets.remove(&id)
    }

    /// The comp a fresh open shows.
    pub fn root_comp(&self) -> &Comp {
        self.comps.get(&self.root).expect("a project always has its root comp")
    }

    /// Add a comp under a fresh id, returning it.
    pub fn insert(&mut self, comp: Comp) -> CompId {
        let id = CompId(self.comps.keys().map(|c| c.0).max().map_or(0, |m| m + 1));
        self.comps.insert(id, comp);
        id
    }

    pub fn module(&self, id: ModuleId) -> Option<&Module> {
        self.modules.get(&id)
    }

    pub fn module_mut(&mut self, id: ModuleId) -> Option<&mut Module> {
        self.modules.get_mut(&id)
    }

    /// Add a module under a fresh id.
    pub fn add_module(&mut self, module: Module) -> ModuleId {
        let id = ModuleId(self.modules.keys().map(|m| m.0).max().map_or(0, |m| m + 1));
        self.modules.insert(id, module);
        id
    }

    /// Bring every comp up to the current format — see [`Comp::migrate`] — and
    /// convert any pre-node drivers into the sink nodes that replaced them. Must
    /// be called after *every* load.
    ///
    /// The conversion is one-way and idempotent: draining the legacy lists into
    /// the graph leaves them empty, and nothing ever writes them again. A driver
    /// whose output no longer exists still becomes an `out` node — it lands at
    /// the origin with a dangling wire, which `validate` reports and the canvas
    /// shows, rather than vanishing without a word.
    /// Read a `.pbc`, in any of the three formats one has ever been written in,
    /// and migrate it.
    ///
    /// This lives in `core` rather than in the editor because it is knowledge
    /// about the *document*, not about the UI: a headless renderer needs it as
    /// much as the app does, and two readers of one format is how a format
    /// quietly grows two dialects. The editor still parses its own `layout`
    /// alongside — that half genuinely is UI — but the project comes from here.
    ///
    /// Formats, newest first: a project wrapper, a pre-comps wrapper holding a
    /// single document, and a bare document from before any wrapper existed.
    /// Each older one becomes a one-comp project, so nothing is stranded.
    ///
    /// [`Project::migrate`] is already called on the result — the one thing a
    /// caller must never forget, so it is not left to them.
    pub fn from_pbc(text: &str) -> Result<Project, String> {
        /// Only the project half. Unknown fields (the editor's `layout`) are
        /// ignored rather than rejected, which is what lets the two readers
        /// share a file without sharing a struct.
        #[derive(Deserialize)]
        struct Wrapper {
            #[serde(default)]
            project: Option<Project>,
            #[serde(default)]
            document: Option<Comp>,
        }

        let mut project = match serde_json::from_str::<Wrapper>(text) {
            Ok(Wrapper { project: Some(p), .. }) => p,
            Ok(Wrapper { document: Some(d), .. }) => Project::single(d),
            // Neither field: it parsed only because both default. Try the
            // oldest format — a bare `Comp` — before giving up.
            Ok(_) => Project::single(
                serde_json::from_str::<Comp>(text).map_err(|e| format!("not a .pbc: {e}"))?,
            ),
            Err(e) => return Err(format!("not a .pbc: {e}")),
        };
        project.migrate();
        Ok(project)
    }

    pub fn migrate(&mut self) {
        for comp in self.comps.values_mut() {
            comp.migrate();
        }
        self.graph.migrate_kinds();
        for m in self.modules.values_mut() {
            m.graph.migrate_kinds();
        }
        for b in std::mem::take(&mut self.legacy_bindings) {
            self.graph.bind_output(b.output, self.root, b.target, b.prop);
        }
        for b in std::mem::take(&mut self.legacy_shape_bindings) {
            self.graph.bind_geometry(b.output, self.root, b.target);
        }
    }
}

#[cfg(test)]
mod tests {

    fn ids(n: &Node) -> Vec<u64> {
        n.children.iter().map(|c| c.id.0).collect()
    }

    /// root: [1, 2, 3], 3 holds [4].
    fn tree() -> Node {
        Node::group(0, "root")
            .with_child(Node::group(1, "a"))
            .with_child(Node::group(2, "b"))
            .with_child(Node::group(3, "c").with_child(Node::group(4, "d")))
    }

    /// Folders are filed into one at a time, and a file from before folders
    /// existed still loads, with none.
    #[test]
    fn asset_folders_file_items_and_old_files_load() {
        let mut p = Project::single(Comp::new(64.0, 64.0, Node::group(0, "root")));
        let mut json: serde_json::Value = serde_json::to_value(&p).unwrap();
        json.as_object_mut().unwrap().remove("folders");
        let old: Project = serde_json::from_value(json).unwrap();
        assert!(old.folders.is_empty());

        p.folders = vec![AssetFolder { name: "a".into(), items: vec![] }, AssetFolder { name: "b".into(), items: vec![] }];
        let item = ProjectItem::Comp(CompId(0));
        p.file_into(item, Some(0));
        p.file_into(item, Some(1));
        assert!(p.folders[0].items.is_empty(), "it left the first folder");
        assert_eq!(p.folders[1].items, vec![item]);
        p.file_into(item, None);
        assert!(p.folders[1].items.is_empty(), "back at the top level");
    }

    fn rng(start: f64, end: f64, amount: f64, shape: RangeShape) -> TextSelector {
        TextSelector::Range {
            start: Value::constant(start),
            end: Value::constant(end),
            offset: Value::constant(0.0),
            amount: Value::constant(amount),
            shape,
            ease_high: 0.0,
            ease_low: 0.0,
            randomize: false,
            seed: 0,
        }
    }

    #[test]
    fn range_shapes_fall_off_across_the_span() {
        use crate::expr::EvalCtx;
        let mut ctx = EvalCtx::at(0.0);
        let with = |shape| rng(0.0, 100.0, 100.0, shape);
        let at = |s: &TextSelector, ctx: &mut EvalCtx| -> Vec<f64> { (0..4).map(|i| (s.amount(i, 4, ctx) * 1000.0).round() / 1000.0).collect() };
        assert_eq!(at(&with(RangeShape::RampUp), &mut ctx), vec![0.125, 0.375, 0.625, 0.875]);
        assert_eq!(at(&with(RangeShape::RampDown), &mut ctx), vec![0.875, 0.625, 0.375, 0.125]);
        assert_eq!(at(&with(RangeShape::Triangle), &mut ctx), vec![0.25, 0.75, 0.75, 0.25]);
        // Amount scales, negative inverts.
        let half = rng(0.0, 100.0, -50.0, RangeShape::Square);
        assert_eq!(half.amount(0, 4, &mut ctx), -0.5);
        // Ease keeps the ends where they are.
        assert_eq!(ease(0.0, 100.0, 100.0), 0.0);
        assert_eq!(ease(1.0, 100.0, 100.0), 1.0);
        assert!(ease(0.25, 0.0, 100.0) < 0.25, "ease low slows the start");
    }

    #[test]
    fn selector_modes_fold_the_stack() {
        use crate::expr::EvalCtx;
        let mut ctx = EvalCtx::at(0.0);
        // First half of 4 units, then the middle half folded in.
        let mut a = TextAnimator::new(TextUnit::Chars);
        a.selector = rng(0.0, 50.0, 100.0, RangeShape::Square);
        let sel = |a: &TextAnimator, ctx: &mut EvalCtx| -> Vec<f64> { (0..4).map(|i| a.selection(i, 4, ctx, |_| 0.0)).collect() };
        a.more_selectors.push(ExtraSelector { mode: SelectorMode::Intersect, selector: rng(25.0, 75.0, 100.0, RangeShape::Square) });
        assert_eq!(sel(&a, &mut ctx), vec![0.0, 1.0, 0.0, 0.0]);
        a.more_selectors[0].mode = SelectorMode::Add;
        assert_eq!(sel(&a, &mut ctx), vec![1.0, 1.0, 1.0, 0.0], "clamped to 1");
        a.more_selectors[0].mode = SelectorMode::Subtract;
        assert_eq!(sel(&a, &mut ctx), vec![1.0, 0.0, -1.0, 0.0]);
        a.more_selectors[0].mode = SelectorMode::Difference;
        assert_eq!(sel(&a, &mut ctx), vec![1.0, 0.0, 1.0, 0.0]);
        // A lone Subtract selects the inverse, as in AE.
        a.more_selectors.clear();
        a.mode = SelectorMode::Subtract;
        assert_eq!(sel(&a, &mut ctx), vec![0.0, 0.0, 1.0, 1.0]);
    }

    #[test]
    fn an_expression_selector_sees_its_unit() {
        use crate::expr::EvalCtx;
        let mut ctx = EvalCtx::at(0.0);
        let mut a = TextAnimator::new(TextUnit::Chars);
        a.more_selectors.push(ExtraSelector {
            mode: SelectorMode::Intersect,
            selector: TextSelector::Expression { script: "selectorValue * textIndex / textTotal".into(), amount: Value::constant(100.0) },
        });
        let got: Vec<f64> = (0..4).map(|i| a.selection(i, 4, &mut ctx, |_| 0.0)).collect();
        assert_eq!(got, vec![0.25, 0.5, 0.75, 1.0]);
        // A broken script selects nothing rather than failing the frame.
        let bad = TextSelector::Expression { script: "nope(".into(), amount: Value::constant(100.0) };
        assert_eq!(bad.amount(0, 4, &mut ctx), 0.0);
    }

    #[test]
    fn randomized_ranges_and_wiggles_are_stable() {
        use crate::expr::EvalCtx;
        let n = 10;
        let mut seen: Vec<usize> = (0..n).map(|i| shuffled(i, n, 7)).collect();
        assert_eq!((0..n).map(|i| shuffled(i, n, 7)).collect::<Vec<_>>(), seen, "same seed, same order");
        seen.sort();
        assert_eq!(seen, (0..n).collect::<Vec<_>>(), "a permutation");
        let w = TextSelector::wiggly();
        let mut ctx = EvalCtx::at(12.0);
        let a: Vec<f64> = (0..5).map(|i| w.amount(i, 5, &mut ctx)).collect();
        assert!(a.iter().all(|v| (-1.0..=1.0).contains(v)));
        assert_eq!(a, (0..5).map(|i| w.amount(i, 5, &mut ctx)).collect::<Vec<_>>());
    }

    /// Animators written before their properties were animatable still load.
    #[test]
    fn legacy_text_animators_deserialize() {
        let json = r#"{"based_on":"Chars","selector":{"Range":{"start":{"Const":0.0},"end":{"Const":100.0},"offset":{"Const":0.0}}},"position":{"x":0.0,"y":-20.0},"opacity":0.5}"#;
        let a: TextAnimator = serde_json::from_str(json).unwrap();
        let mut ctx = crate::expr::EvalCtx::at(0.0);
        assert_eq!(a.position.unwrap().resolve(&mut ctx), Vec2::new(0.0, -20.0));
        assert_eq!(a.opacity.unwrap().resolve(&mut ctx), 0.5);
        assert_eq!(a.selector.amount(0, 1, &mut ctx), 1.0, "amount defaults to 100%");
    }

    #[test]
    fn text_selectors_pick_units() {
        use crate::expr::EvalCtx;
        // A range over the middle half of 4 units, cutting unit 2 in half.
        let range = rng(25.0, 62.5, 100.0, RangeShape::Square);
        let mut ctx = EvalCtx::at(0.0);
        let got: Vec<f64> = (0..4).map(|i| range.amount(i, 4, &mut ctx)).collect();
        assert_eq!(got, vec![0.0, 1.0, 0.5, 0.0]);
        // A 100 -> 0 ramp over frames 0..10; each later unit reads it 5 frames late.
        let ramp = Value::Keyframed(crate::value::Track::new(vec![
            crate::value::Keyframe::linear(0, 100.0),
            crate::value::Keyframe::linear(10, 0.0),
        ]));
        let stagger = TextSelector::Stagger { amount: ramp, delays: vec![], step: 5.0, shift: 0.0, reverse: false };
        let mut ctx = EvalCtx::at(10.0);
        assert_eq!((stagger.amount(0, 3, &mut ctx), stagger.amount(1, 3, &mut ctx), stagger.amount(2, 3, &mut ctx)), (0.0, 0.5, 1.0));
        assert_eq!(ctx.frame, 10.0, "the clock is put back");
    }

    #[test]
    fn move_node_reorders_reparents_and_refuses_cycles() {
        let mut t = tree();
        // Within a parent, `index` is a slot in the list as it is now.
        assert!(t.move_node(NodeId(1), NodeId(0), 3));
        assert_eq!(ids(&t), [2, 3, 1]);
        assert!(t.move_node(NodeId(1), NodeId(0), 0));
        assert_eq!(ids(&t), [1, 2, 3]);
        assert!(!t.move_node(NodeId(2), NodeId(0), 2), "already there");
        // Into another parent, and back out.
        assert!(t.move_node(NodeId(2), NodeId(3), 0));
        assert_eq!(ids(&t), [1, 3]);
        assert_eq!(ids(t.find(NodeId(3)).unwrap()), [2, 4]);
        assert!(t.move_node(NodeId(4), NodeId(0), 99));
        assert_eq!(ids(&t), [1, 3, 4]);
        // Never into itself or its own subtree, never the root.
        assert!(!t.move_node(NodeId(3), NodeId(2), 0));
        assert!(!t.move_node(NodeId(3), NodeId(3), 0));
        assert!(!t.move_node(NodeId(0), NodeId(1), 0));
        assert!(!t.move_node(NodeId(9), NodeId(0), 0));
    }

    use super::*;
    use crate::value::{Keyframe, Track};

    /// Build a one-layer comp at `fps` with a position key on `frame`.
    fn comp_with_key(fps: f64, frame: i64) -> Comp {
        let mut transform = Transform::default();
        transform.rotation = Value::Keyframed(Track::new(vec![
            Keyframe::linear(0, 0.0),
            Keyframe::linear(frame, 90.0),
        ]));
        let layer = Node::group(1, "layer").with_transform(transform);
        let mut comp = Comp::new(1920.0, 1080.0, Node::group(0, "root").with_child(layer));
        comp.fps = fps;
        comp
    }

    fn key_frames(comp: &Comp) -> Vec<i64> {
        comp.root.children[0].transform.rotation.key_frames()
    }

    /// A comp serialized the way a pre-frames `.pbc` was: `duration` in
    /// seconds (or absent), and no `duration_frames` at all.
    fn legacy_json(fps: f64, seconds: Option<f64>) -> serde_json::Value {
        let mut comp = Comp::new(640.0, 480.0, Node::group(0, "root"));
        comp.fps = fps;
        let mut v = serde_json::to_value(&comp).unwrap();
        let obj = v.as_object_mut().unwrap();
        obj.remove("duration_frames");
        match seconds {
            Some(s) => {
                obj.insert("duration".into(), serde_json::json!(s));
            }
            None => {
                obj.remove("duration");
            }
        }
        v
    }

    /// The length is stored in frames now. A `.pbc` written when it was
    /// `duration` in seconds must still open at the same length, and the
    /// conversion is `migrate`'s job — the field alone can't do it, because
    /// serde gives no ordering guarantee that `fps` is read first.
    #[test]
    fn a_legacy_seconds_duration_migrates_to_frames() {
        let mut comp: Comp = serde_json::from_value(legacy_json(24.0, Some(5.0))).unwrap();
        // Before migration the field is the "not in this file" sentinel.
        assert_eq!(comp.duration_frames, 0);
        comp.migrate();
        assert_eq!(comp.duration_frames, 120, "5s @ 24fps");
        assert!((comp.duration_seconds() - 5.0).abs() < 1e-12);
    }

    /// A file with neither field — or one somehow carrying a zero — must not
    /// open as a comp of no length, which would render nothing at all.
    #[test]
    fn a_comp_with_no_duration_at_all_migrates_to_the_default_length() {
        let mut comp: Comp = serde_json::from_value(legacy_json(60.0, None)).unwrap();
        comp.migrate();
        assert_eq!(comp.duration_frames, Comp::DEFAULT_DURATION_FRAMES);
    }

    /// The point of the whole change: the frame count is stored, so it is never
    /// a rounding decision. At 23.976 a 5-second comp used to be 119.88 frames,
    /// and whether a renderer wrote 119 or 120 came down to `.round()`.
    #[test]
    fn the_frame_count_is_exact_at_a_fractional_rate() {
        let mut comp = Comp::new(64.0, 64.0, Node::group(0, "root"));
        comp.fps = 24000.0 / 1001.0; // 23.976
        comp.duration_frames = 120;
        // Whatever the rate, the comp is exactly the frames it says it is.
        assert_eq!(comp.duration_frames, 120);
        // And the seconds it reports are derived, not stored.
        assert!((comp.duration_seconds() - 120.0 * 1001.0 / 24000.0).abs() < 1e-12);
    }

    /// A rate change re-grids the animation; the length has to travel with it,
    /// or a comp silently gets longer or shorter in wall-clock terms.
    #[test]
    fn changing_the_rate_keeps_the_comp_the_same_length_in_seconds() {
        let mut comp = Comp::new(64.0, 64.0, Node::group(0, "root"));
        comp.fps = 60.0;
        comp.duration_frames = 300; // five seconds
        comp.set_fps(24.0);
        assert_eq!(comp.duration_frames, 120, "still five seconds, on a new grid");
        assert!((comp.duration_seconds() - 5.0).abs() < 1e-9);
    }

    /// The legacy field is read, never written: once migrated, a save must not
    /// carry a stale `duration` for some future loader to disagree with.
    #[test]
    fn a_migrated_comp_no_longer_serializes_the_legacy_field() {
        let mut comp: Comp = serde_json::from_value(legacy_json(24.0, Some(2.0))).unwrap();
        comp.migrate();
        let out = serde_json::to_string(&comp).unwrap();
        assert!(!out.contains("\"duration\""), "no legacy seconds field: {out}");
        assert!(out.contains("\"duration_frames\":48"));
    }

    /// Render presets travel with the project, which is the entire reason they
    /// are project data: two people opening one `.pbc` must render the same
    /// file. A round-trip is therefore the property under test.
    #[test]
    fn render_presets_round_trip_with_the_project() {
        let mut project = Project::single(Comp::new(64.0, 64.0, Node::group(0, "root")));
        project.render_presets.push(RenderPreset {
            name: "YouTube 1080p".to_string(),
            out: "deliver/yt.mp4".to_string(),
            quality: "master".to_string(),
            scale: 0.5,
            ffmpeg_args: vec!["-crf".to_string(), "18".to_string()],
        });
        let back: Project = serde_json::from_str(&serde_json::to_string(&project).unwrap()).unwrap();
        assert_eq!(back.render_presets, project.render_presets);
    }

    /// A `.pbc` written before presets existed must open with none rather than
    /// failing to parse — the `#[serde(default)]` *is* the migration, and this
    /// is the test that keeps it that way.
    #[test]
    fn a_project_without_presets_loads_with_none() {
        let project = Project::single(Comp::new(64.0, 64.0, Node::group(0, "root")));
        let mut json: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&project).unwrap()).unwrap();
        json.as_object_mut().unwrap().remove("render_presets");
        let back: Project = serde_json::from_value(json).expect("a pre-presets file must open");
        assert!(back.render_presets.is_empty());
    }

    /// The optional fields inside a preset default too, so a hand-written entry
    /// with only a name and a path is valid — presets are the kind of thing a
    /// person edits in the file by hand.
    #[test]
    fn a_minimal_preset_fills_in_its_defaults() {
        let preset: RenderPreset =
            serde_json::from_str(r#"{"name":"quick","out":"out.mp4"}"#).expect("must parse");
        assert_eq!(preset.quality, "master");
        assert_eq!(preset.scale, 1.0);
        assert!(preset.ffmpeg_args.is_empty());
    }

    /// The comp background is a setting, so it round-trips — and a `.pbc`
    /// written before the setting existed must land on the default rather than
    /// failing to parse or rendering the frame transparent.
    #[test]
    fn a_comp_without_a_background_loads_the_default_one() {
        let mut comp = Comp::new(64.0, 64.0, Node::group(0, "root"));
        comp.bg = Color::rgb(1.0, 0.0, 0.0);
        let json = serde_json::to_string(&comp).unwrap();
        let back: Comp = serde_json::from_str(&json).unwrap();
        assert_eq!(back.bg, Color::rgb(1.0, 0.0, 0.0));

        let mut legacy: serde_json::Value = serde_json::from_str(&json).unwrap();
        legacy.as_object_mut().unwrap().remove("bg");
        let old: Comp = serde_json::from_value(legacy).unwrap();
        assert_eq!(old.bg, Comp::DEFAULT_BG);
    }

    /// Aids are saved, and a file written before they existed must load with
    /// them rather than fail — the usual `#[serde(default)]` contract. A guide
    /// is part of how a composition was built, so losing it on reopen would
    /// defeat the point of having guides at all.
    #[test]
    fn a_comp_without_aids_loads_the_defaults_and_guides_round_trip() {
        let mut comp = Comp::new(64.0, 64.0, Node::group(0, "root"));
        comp.aids.grid.visible = true;
        comp.aids.grid.spacing = 25.0;
        comp.aids.guides.items.push(Guide { axis: GuideAxis::Vertical, at: 12.5 });
        let json = serde_json::to_string(&comp).unwrap();
        let back: Comp = serde_json::from_str(&json).unwrap();
        assert_eq!(back.aids, comp.aids, "aids survive a round trip");

        let mut legacy: serde_json::Value = serde_json::from_str(&json).unwrap();
        legacy.as_object_mut().unwrap().remove("aids");
        let old: Comp = serde_json::from_value(legacy).unwrap();
        assert_eq!(old.aids, ViewAids::default());
        assert!(!old.aids.grid.visible, "an unasked-for grid stays off");
        assert!(old.aids.guides.visible, "but guides you made are shown");
    }

    /// The grid's spacing is clamped where it is *read*, not where it is
    /// written: a zero or negative step would make the renderer's line loop
    /// never terminate, and a hand-edited `.pbc` can carry any number at all.
    #[test]
    fn a_degenerate_grid_spacing_cannot_hang_the_renderer() {
        for spacing in [0.0, -50.0, f64::NAN] {
            let grid = Grid { visible: true, spacing, subdivisions: 4 };
            assert!(grid.step() >= Grid::MIN_SPACING, "step was {}", grid.step());
            assert!(grid.minor_step().is_some_and(|m| m > 0.0));
        }
    }

    /// One subdivision means "no minor lines", not "a minor line on top of
    /// every major one".
    #[test]
    fn a_single_subdivision_means_no_minor_grid_lines() {
        let mut grid = Grid { visible: true, spacing: 100.0, subdivisions: 1 };
        assert_eq!(grid.minor_step(), None);
        grid.subdivisions = 4;
        assert_eq!(grid.minor_step(), Some(25.0));
    }

    /// Ghost offsets are symmetrical, spaced by `step`, and fade with their
    /// *index* rather than their frame distance - so the nearest ghost is
    /// always the most solid whatever the spacing is.
    #[test]
    fn onion_offsets_are_spaced_and_faded_from_the_playhead() {
        let o = Onion { visible: true, before: 3, after: 2, step: 2, opacity: 0.5 };
        let offs = o.offsets();
        let frames: Vec<i64> = offs.iter().map(|(f, _)| *f).collect();
        assert_eq!(frames, vec![-2, -4, -6, 2, 4]);
        assert_eq!(offs[0].1, 0.0, "nearest past is solid");
        assert_eq!(offs[2].1, 1.0, "furthest past is fully faded");
        assert_eq!(offs[3].1, 0.0, "nearest future is solid");
        assert_eq!(offs[4].1, 1.0, "furthest future is fully faded");
    }

    /// Hidden means no work at all, so callers need no second check.
    #[test]
    fn hidden_onion_skins_schedule_nothing() {
        let o = Onion { visible: false, before: 5, after: 5, step: 1, opacity: 0.5 };
        assert!(o.offsets().is_empty());
    }

    /// A zero or negative step would stack every ghost on the playhead, and the
    /// count is capped so a hand-edited `.pbc` cannot turn one repaint into
    /// thousands of scene evaluations.
    #[test]
    fn a_degenerate_onion_setting_cannot_stack_or_explode() {
        let o = Onion { visible: true, before: 2, after: 0, step: 0, opacity: 0.5 };
        let frames: Vec<i64> = o.offsets().iter().map(|(f, _)| *f).collect();
        assert_eq!(frames, vec![-1, -2], "step clamps to 1 rather than stacking at 0");

        let o = Onion { visible: true, before: 9_999, after: 9_999, step: 1, opacity: 0.5 };
        assert_eq!(o.offsets().len() as u32, Onion::MAX_GHOSTS * 2);
    }

    /// A single ghost on a side must be solid, not accidentally faded away by a
    /// divide-by-zero in the fade ramp.
    #[test]
    fn a_lone_ghost_is_fully_solid() {
        let o = Onion { visible: true, before: 1, after: 1, step: 3, opacity: 0.5 };
        for (_, t) in o.offsets() {
            assert_eq!(t, 0.0);
        }
    }

    /// Same contract for the passepartout: it round-trips, and a file written
    /// before it existed loads at the default rather than at 0 (which would
    /// silently *disable* the feature on every pre-existing project).
    #[test]
    fn a_comp_without_a_passepartout_loads_the_default_one() {
        let mut comp = Comp::new(64.0, 64.0, Node::group(0, "root"));
        comp.passepartout = 0.25;
        let json = serde_json::to_string(&comp).unwrap();
        let back: Comp = serde_json::from_str(&json).unwrap();
        assert_eq!(back.passepartout, 0.25);

        let mut legacy: serde_json::Value = serde_json::from_str(&json).unwrap();
        legacy.as_object_mut().unwrap().remove("passepartout");
        let old: Comp = serde_json::from_value(legacy).unwrap();
        assert_eq!(old.passepartout, Comp::DEFAULT_PASSEPARTOUT);
        assert_eq!(old.passepartout, 0.5, "Blender's default, and ours");
    }

    #[test]
    fn changing_fps_keeps_keys_at_their_wall_clock_time() {
        // Frame 120 @ 60fps is two seconds in; at 24fps that instant is frame 48.
        let mut comp = comp_with_key(60.0, 120);
        assert!(comp.set_fps(24.0));
        assert_eq!(key_frames(&comp), vec![0, 48]);

        // And the reverse direction, from a slow grid to a fast one.
        let mut comp = comp_with_key(24.0, 48);
        assert!(comp.set_fps(60.0));
        assert_eq!(key_frames(&comp), vec![0, 120]);
    }

    #[test]
    fn round_tripping_fps_is_stable() {
        let mut comp = comp_with_key(60.0, 120);
        comp.set_fps(24.0);
        comp.set_fps(60.0);
        assert_eq!(key_frames(&comp), vec![0, 120]);
    }

    #[test]
    fn setting_the_same_fps_moves_nothing() {
        let mut comp = comp_with_key(60.0, 121);
        assert!(!comp.set_fps(60.0), "an unchanged rate is not a retime");
        assert_eq!(key_frames(&comp), vec![0, 121]);
    }

    #[test]
    fn clip_windows_follow_the_new_grid() {
        let mut comp = comp_with_key(60.0, 120);
        comp.root.children[0].timing = Some(LayerTiming { start: 30, in_: 60, out: 120 });
        comp.set_fps(30.0);
        assert_eq!(
            comp.root.children[0].timing,
            Some(LayerTiming { start: 15, in_: 30, out: 60 }),
            "a clip covering 1s..2s must still cover 1s..2s"
        );
    }

    #[test]
    fn a_degenerate_rate_does_not_move_keys_to_nowhere() {
        // `Timebase` clamps bad rates to 1.0, and `set_fps` retimes against the
        // clamped value — never against a NaN ratio that would erase the keys.
        let mut comp = comp_with_key(60.0, 120);
        comp.set_fps(f64::NAN);
        assert_eq!(key_frames(&comp), vec![0, 2], "120 frames @ 60fps is 2s, so frame 2 @ 1fps");
    }
}

#[cfg(test)]
mod knob_tests {
    use super::*;
    use crate::vec3::Vec3;
    use crate::expr::ExprValue;


    /// A knob's value must be *settable*, not just creatable — without this
    /// every `param("x")` read resolves to the seed forever, which is exactly
    /// how a knob-driven graph silently does nothing.
    #[test]
    fn a_knobs_constant_round_trips_through_the_editor_seam() {
        let mut p = ParamValue::Num(Value::constant(0.0));
        assert_eq!(p.as_const(), Some(ExprValue::Num(0.0)));
        p.set_const(ExprValue::Num(12.5));
        assert_eq!(p.as_const(), Some(ExprValue::Num(12.5)));
    }

    /// A literal of the wrong shape is ignored: an expression reading a vector
    /// knob must not find a number there because an editor sent the wrong
    /// variant.
    #[test]
    fn a_mistyped_literal_does_not_retype_the_knob() {
        let mut p = ParamValue::Vec(Value::constant(Vec2::new(1.0, 2.0)));
        p.set_const(ExprValue::Num(9.0));
        assert_eq!(p.as_const(), Some(ExprValue::Vec3(Vec3::flat(1.0, 2.0))), "unchanged");
        assert_eq!(p.kind_name(), "vector");
    }

    /// An animated knob reports no constant, so an editor shows no field rather
    /// than one number standing for a whole track (and flattening it on edit).
    #[test]
    fn an_animated_knob_has_no_constant_to_show() {
        let track = Value::Keyframed(crate::value::Track::new(vec![
            crate::value::Keyframe::linear(0, 1.0),
            crate::value::Keyframe::linear(10, 5.0),
        ]));
        let p = ParamValue::Num(track);
        assert!(p.as_const().is_none());
    }

    #[test]
    fn ease_library_round_trips_and_defaults_empty_on_an_old_file() {
        let mut project = Project::single(Comp::new(64.0, 64.0, Node::group(0, "root")));
        project.eases.push(crate::value::EasePreset::new(
            "House Bounce",
            crate::value::Handle::new(0.68, -0.2),
            crate::value::Handle::new(0.32, 1.2),
        ));
        let json = serde_json::to_string(&project).unwrap();
        let back: Project = serde_json::from_str(&json).unwrap();
        assert_eq!(back.eases, project.eases, "saved curves must survive a save/open");

        // A `.pbc` written before the library existed has no key at all.
        let stripped = json.replace("\"eases\"", "\"eases_was_here\"");
        let old: Project = serde_json::from_str(&stripped).unwrap();
        assert!(old.eases.is_empty(), "a pre-library file loads with no presets");
    }

    /// The 2.5D migration's load-bearing promise: a `.pbc` written when
    /// rotation was one number and the vectors were two, opens as the *same*
    /// animation — flat, spinning about Z, and on the affine fast path.
    #[test]
    fn a_pre_2_5d_transform_loads_flat_and_unchanged() {
        let legacy = r#"{
            "anchor": {"Const": {"x": 5.0, "y": 6.0}},
            "position": {"Const": {"x": 10.0, "y": 20.0}},
            "rotation_deg": {"Const": 90.0},
            "scale": {"Const": {"x": 2.0, "y": 3.0}},
            "opacity": {"Const": 1.0}
        }"#;
        let tr: Transform = serde_json::from_str(legacy).unwrap();
        let Value::Const(rot) = tr.rotation else { panic!("still a constant") };
        assert_eq!(rot, 90.0, "the spin is Z");
        assert_eq!((tr.rotation_x.clone(), tr.rotation_y.clone()), (Value::constant(0.0), Value::constant(0.0)));
        assert!(tr.is_statically_flat());

        // And it resolves to the very matrix the old engine built.
        let doc = Document::new(100.0, 100.0, Node::group(0, "root"));
        let mut ctx = EvalCtx::new(&doc, 0.0);
        let (xf, _) = tr.resolve(&mut ctx);
        assert!(xf.is_flat(), "no depth anywhere, so no 4x4 and no projection");
        let expected = kurbo::Affine::translate((10.0, 20.0))
            * kurbo::Affine::rotate(90f64.to_radians())
            * kurbo::Affine::scale_non_uniform(2.0, 3.0)
            * kurbo::Affine::translate((-5.0, -6.0));
        for (l, r) in xf.to_affine().unwrap().as_coeffs().iter().zip(expected.as_coeffs().iter()) {
            assert!((l - r).abs() < 1e-12, "{l} vs {r}");
        }
    }

    /// A legacy *keyframed* rotation keeps its timing, not just its values —
    /// widening a track must not quietly re-ease it.
    #[test]
    fn a_legacy_rotation_track_widens_without_retiming() {
        let legacy = r#"{
            "anchor": {"Const": {"x": 0.0, "y": 0.0}},
            "position": {"Const": {"x": 0.0, "y": 0.0}},
            "rotation_deg": {"Keyframed": {"keys": [
                {"frame": 0, "value": 0.0, "out_handle": {"x": 0.0, "y": 0.0},
                 "in_handle": {"x": 0.0, "y": 0.0}},
                {"frame": 24, "value": 180.0, "out_handle": {"x": 0.0, "y": 0.0},
                 "in_handle": {"x": 0.0, "y": 0.0}}
            ]}},
            "scale": {"Const": {"x": 1.0, "y": 1.0}},
            "opacity": {"Const": 1.0}
        }"#;
        let tr: Transform = serde_json::from_str(legacy).unwrap();
        let Value::Keyframed(track) = &tr.rotation else { panic!("still a track") };
        assert_eq!(track.keys().len(), 2);
        assert_eq!(track.keys()[0].frame, 0);
        assert_eq!(track.keys()[1].frame, 24);
        assert_eq!(track.keys()[1].value, 180.0);
    }

    /// A 2.5D `.pbc` stored rotation as one joined 3-vector track. It splits
    /// into three channels with the same key frames, so each axis can then be
    /// keyed on its own — and an axis that never moved stays a constant.
    #[test]
    fn a_joined_rotation_track_splits_per_axis() {
        let joined = r#"{
            "anchor": {"Const": {"x": 0.0, "y": 0.0}},
            "position": {"Const": {"x": 0.0, "y": 0.0}},
            "rotation": {"Keyframed": {"keys": [
                {"frame": 0, "value": {"x": 0.0, "y": 0.0, "z": 0.0},
                 "out_handle": {"x": 0.0, "y": 0.0}, "in_handle": {"x": 0.0, "y": 0.0}},
                {"frame": 24, "value": {"x": 45.0, "y": 0.0, "z": 180.0},
                 "out_handle": {"x": 0.0, "y": 0.0}, "in_handle": {"x": 0.0, "y": 0.0}}
            ]}},
            "scale": {"Const": {"x": 1.0, "y": 1.0}},
            "opacity": {"Const": 1.0}
        }"#;
        let tr: Transform = serde_json::from_str(joined).unwrap();
        let Value::Keyframed(z) = &tr.rotation else { panic!("z keyed") };
        let Value::Keyframed(x) = &tr.rotation_x else { panic!("x keyed") };
        assert_eq!(z.frames(), vec![0, 24]);
        assert_eq!(x.frames(), vec![0, 24]);
        assert_eq!((x.keys()[1].value, z.keys()[1].value), (45.0, 180.0));
        assert_eq!(tr.rotation_y, Value::constant(0.0), "an unmoved axis is a constant");

        // Constant joins split too, and the separated form round-trips.
        let constant = joined.replace(
            &joined[joined.find("{\"Keyframed\"").unwrap()..joined.find(",\n            \"scale\"").unwrap()],
            r#"{"Const": {"x": 10.0, "y": 20.0, "z": 30.0}}"#,
        );
        let tr2: Transform = serde_json::from_str(&constant).unwrap();
        assert_eq!(
            (tr2.rotation_x.clone(), tr2.rotation_y.clone(), tr2.rotation.clone()),
            (Value::constant(10.0), Value::constant(20.0), Value::constant(30.0))
        );
        let back: Transform = serde_json::from_str(&serde_json::to_string(&tr).unwrap()).unwrap();
        assert_eq!(back, tr);
    }

    /// Each axis keys independently: different key frames on X and Z.
    #[test]
    fn rotation_axes_animate_independently() {
        use crate::value::{Keyframe, Track};
        let doc = Document::new(100.0, 100.0, Node::group(0, "root"));
        let mut tr = Transform::default();
        tr.rotation = Value::Keyframed(Track::new(vec![Keyframe::linear(0, 0.0), Keyframe::linear(10, 90.0)]));
        tr.rotation_x = Value::Keyframed(Track::new(vec![Keyframe::linear(5, 0.0), Keyframe::linear(20, 60.0)]));
        let at = |f: f64| {
            let mut ctx = EvalCtx::new(&doc, f);
            (tr.rotation.resolve(&mut ctx), tr.rotation_x.resolve(&mut ctx))
        };
        assert_eq!(at(10.0).0, 90.0);
        assert!(at(10.0).1 > 0.0 && at(10.0).1 < 60.0, "x is mid-move while z has landed");
        assert!(!tr.is_statically_flat());
    }

    /// Depth is what forces the expensive tier — and only depth. Asserted from
    /// the outside so the fast-path condition can't drift from its intent.
    #[test]
    fn only_out_of_plane_channels_escalate_to_a_matrix() {
        let doc = Document::new(100.0, 100.0, Node::group(0, "root"));
        let resolve = |tr: &Transform| {
            let mut ctx = EvalCtx::new(&doc, 0.0);
            tr.resolve(&mut ctx).0
        };

        let mut tr = Transform::default();
        tr.rotation = Value::constant(45.0);
        assert!(resolve(&tr).is_flat(), "a Z spin is in-plane");

        let mut tr = Transform::default();
        tr.position = Value::constant(Vec3::new(0.0, 0.0, 5.0));
        assert!(!resolve(&tr).is_flat(), "depth in position leaves the plane");

        let mut tr = Transform::default();
        tr.rotation_x = Value::constant(30.0);
        assert!(!resolve(&tr).is_flat(), "an X tilt leaves the plane");
    }

    /// Off is one sample at the frame; on spreads samples evenly across the
    /// shutter, centred on the frame.
    #[test]
    fn motion_blur_samples_centre_on_the_frame() {
        let mut mb = MotionBlur::default();
        assert_eq!(mb.offsets(), vec![0.0]);
        mb.enabled = true;
        mb.samples = 4;
        mb.shutter_angle = 360.0;
        assert_eq!(mb.offsets(), vec![-0.375, -0.125, 0.125, 0.375]);
    }
}
