//! The scene graph: a tree of `Node`s where every animatable field is a
//! `Value<_>`. Nothing here is baked — `eval` turns a document + time into a
//! flat `Scene`.

use kurbo::{BezPath, Rect, RoundedRect, Shape as _, Vec2};
use serde::{Deserialize, Serialize};

use crate::asset::{Asset, AssetId, ImagePaint};
use crate::composite::{BlendMode, Mask};
use crate::expr::EvalCtx;
use crate::text::TextAlign;
use crate::value::{Color, Value};

/// Stable identity for a node, used for selection and for tracing an evaluated
/// render item back to its source (EBN's line→nodeId map idea, applied to a
/// pull-based dataflow graph).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct NodeId(pub u64);

/// An affine transform, every channel animatable. Resolves to a
/// `kurbo::Affine` plus a scalar opacity at a given time.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Transform {
    pub anchor: Value<Vec2>,
    pub position: Value<Vec2>,
    pub rotation_deg: Value<f64>,
    pub scale: Value<Vec2>,
    pub opacity: Value<f64>,
}

impl Default for Transform {
    fn default() -> Self {
        Self {
            anchor: Value::constant(Vec2::ZERO),
            position: Value::constant(Vec2::ZERO),
            rotation_deg: Value::constant(0.0),
            scale: Value::constant(Vec2::new(1.0, 1.0)),
            opacity: Value::constant(1.0),
        }
    }
}

impl Transform {
    /// Resolve to (matrix, opacity) against `ctx`. The matrix maps local space
    /// to parent space: translate(position) · rotate · scale · translate(-anchor).
    pub fn resolve(&self, ctx: &mut EvalCtx) -> (kurbo::Affine, f64) {
        let anchor = self.anchor.resolve(ctx);
        let position = self.position.resolve(ctx);
        let rot = self.rotation_deg.resolve(ctx).to_radians();
        let scale = self.scale.resolve(ctx);
        let m = kurbo::Affine::translate(position)
            * kurbo::Affine::rotate(rot)
            * kurbo::Affine::scale_non_uniform(scale.x, scale.y)
            * kurbo::Affine::translate(-anchor);
        (m, self.opacity.resolve(ctx))
    }

    pub(crate) fn migrate_frames(&mut self, fps: f64) {
        self.anchor.migrate_frames(fps);
        self.position.migrate_frames(fps);
        self.rotation_deg.migrate_frames(fps);
        self.scale.migrate_frames(fps);
        self.opacity.migrate_frames(fps);
    }

    pub(crate) fn retime(&mut self, ratio: f64) {
        self.anchor.retime(ratio);
        self.position.retime(ratio);
        self.rotation_deg.retime(ratio);
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
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Shape {
    /// A pre-built path (imported / drawn by hand).
    Path(BezPath),
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
        /// Font size in pixels.
        size: Value<f64>,
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

impl Shape {
    pub fn to_path(&self, ctx: &mut EvalCtx) -> BezPath {
        match self {
            Shape::Path(p) => p.clone(),
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
            Shape::Text { content, family, size, align, max_width } => {
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
                crate::text::text_to_path(&content, family, size.resolve(ctx), *align, *max_width)
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
            Shape::Rect { size, radius } => {
                size.migrate_frames(fps);
                radius.migrate_frames(fps);
            }
            Shape::Ellipse { size } => size.migrate_frames(fps),
            Shape::Text { content, size, .. } => {
                content.migrate_frames(fps);
                size.migrate_frames(fps);
            }
            Shape::Image { size, time_remap, .. } => {
                size.migrate_frames(fps);
                if let Some(t) = time_remap {
                    t.migrate_frames(fps);
                }
            }
        }
    }

    pub(crate) fn retime(&mut self, ratio: f64) {
        match self {
            Shape::Path(_) => {}
            Shape::Rect { size, radius } => {
                size.retime(ratio);
                radius.retime(ratio);
            }
            Shape::Ellipse { size } => size.retime(ratio),
            Shape::Text { content, size, .. } => {
                content.retime(ratio);
                size.retime(ratio);
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

/// A stroke: animatable color and width.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Stroke {
    pub color: Value<Color>,
    pub width: Value<f64>,
}

/// A user-exposed control on a node: a named, animatable knob that expressions
/// and scripts read by name (`param("speed")`).
///
/// This is the piece that makes a node a *reusable* thing rather than a bag of
/// hardcoded values — one parameter can drive many properties, and (once a
/// composition can be nested) it's what a pre-comp exposes to its parent.
/// A parameter is a `Value` like any property, so it keyframes and can itself
/// be expression-driven.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Param {
    /// How a script names it. Unique per node — [`Node::set_param`] enforces
    /// that, since a duplicate would make `param("x")` ambiguous.
    pub name: String,
    pub value: ParamValue,
}

/// A parameter's type. Mirrors the `ExprValue` kinds, so a parameter can drive
/// any property an expression can — including a text one.
#[derive(Clone, Debug, Serialize, Deserialize)]
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
            (ParamValue::Vec(slot), E::Vec2(p)) => *slot = Value::constant(p),
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

/// One node in the scene graph. A group (no shape) just composes its children;
/// a leaf carries a shape + paint.
#[derive(Clone, Debug, Serialize, Deserialize)]
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
    pub children: Vec<Node>,
}

impl Node {
    /// Whether this layer must be composited as its own image.
    ///
    /// The single place that answers it, so the walk and any future backend
    /// can't disagree about which layers cost an offscreen target.
    pub fn needs_isolation(&self) -> bool {
        self.blend.needs_isolation() || self.mask.is_some()
    }
}

impl Node {
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

    /// Move the child with `id` among its siblings by `delta` (e.g. -1 up, +1
    /// down), clamped to the ends. Searches the whole subtree for the parent.
    /// Returns whether a move happened. Child order is also draw order, so this
    /// restacks the node visually.
    pub fn reorder_child(&mut self, id: NodeId, delta: i32) -> bool {
        if let Some(i) = self.children.iter().position(|c| c.id == id) {
            let j = (i as i32 + delta).clamp(0, self.children.len() as i32 - 1) as usize;
            if i != j {
                self.children.swap(i, j);
                return true;
            }
            return false;
        }
        self.children.iter_mut().any(|c| c.reorder_child(id, delta))
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
            shape.migrate_frames(fps);
        }
        // A mask's shape is parametric like any other, so its keys live on the
        // same grid and migrate with everything else.
        if let Some(mask) = &mut self.mask {
            mask.shape.migrate_frames(fps);
        }
        if let Some(fill) = &mut self.fill {
            fill.migrate_frames(fps);
        }
        if let Some(stroke) = &mut self.stroke {
            stroke.color.migrate_frames(fps);
            stroke.width.migrate_frames(fps);
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
        if let Some(timing) = &mut self.timing {
            timing.retime(ratio);
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
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Comp {
    /// What the comp switcher shows. `#[serde(default)]` so a pre-project
    /// `.pbc` loads with an empty name and falls back to a generated label.
    #[serde(default)]
    pub name: String,
    pub width: f64,
    pub height: f64,
    pub fps: f64,
    pub duration: f64,
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
    pub root: Node,
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

    pub fn new(width: f64, height: f64, root: Node) -> Self {
        Self {
            name: String::new(),
            width,
            height,
            fps: 60.0,
            duration: 5.0,
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
        self.root.migrate_frames(fps);
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
        self.root.retime(new / old);
        true
    }

    /// Total length of the composition in whole frames: 5s @ 24fps = 120.
    pub fn duration_frames(&self) -> i64 {
        self.timebase().seconds_to_frames(self.duration)
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
#[derive(Clone, Debug, Serialize, Deserialize)]
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
#[derive(Clone, Debug, Serialize, Deserialize)]
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
    pub fn migrate(&mut self) {
        for comp in self.comps.values_mut() {
            comp.migrate();
        }
        self.graph.migrate_kinds();
        for m in self.modules.values_mut() {
            m.graph.migrate_kinds();
        }
        for b in std::mem::take(&mut self.legacy_bindings) {
            self.graph.bind_output(b.output, b.target, b.prop);
        }
        for b in std::mem::take(&mut self.legacy_shape_bindings) {
            self.graph.bind_geometry(b.output, b.target);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::{Keyframe, Track};

    /// Build a one-layer comp at `fps` with a position key on `frame`.
    fn comp_with_key(fps: f64, frame: i64) -> Comp {
        let mut transform = Transform::default();
        transform.rotation_deg = Value::Keyframed(Track::new(vec![
            Keyframe::linear(0, 0.0),
            Keyframe::linear(frame, 90.0),
        ]));
        let layer = Node::group(1, "layer").with_transform(transform);
        let mut comp = Comp::new(1920.0, 1080.0, Node::group(0, "root").with_child(layer));
        comp.fps = fps;
        comp
    }

    fn key_frames(comp: &Comp) -> Vec<i64> {
        comp.root.children[0].transform.rotation_deg.key_frames()
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
        assert_eq!(p.as_const(), Some(ExprValue::Vec2(Vec2::new(1.0, 2.0))), "unchanged");
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
}
