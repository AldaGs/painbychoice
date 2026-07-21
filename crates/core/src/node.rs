//! The scene graph: a tree of `Node`s where every animatable field is a
//! `Value<_>`. Nothing here is baked — `eval` turns a document + time into a
//! flat `Scene`.

use kurbo::{BezPath, Rect, RoundedRect, Shape as _, Vec2};
use serde::{Deserialize, Serialize};

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
    /// keyframe); `content` and `family` are plain data because
    /// [`Value`] only carries interpolatable, expression-typed values
    /// (`f64`/`Vec2`/`Color`) — there is no string in `ExprValue`, so a
    /// keyframed or scripted string would need a wider value model.
    Text {
        content: String,
        /// System font family name. Empty (or not installed) → sans-serif.
        family: String,
        /// Font size in pixels.
        size: Value<f64>,
        align: TextAlign,
        /// Wrap width; `None` keeps the text on one line.
        max_width: Option<f64>,
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
                crate::text::text_to_path(content, family, size.resolve(ctx), *align, *max_width)
            }
        }
    }

    pub(crate) fn migrate_frames(&mut self, fps: f64) {
        match self {
            Shape::Path(_) => {}
            Shape::Rect { size, radius } => {
                size.migrate_frames(fps);
                radius.migrate_frames(fps);
            }
            Shape::Ellipse { size } => size.migrate_frames(fps),
            Shape::Text { size, .. } => size.migrate_frames(fps),
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
            Shape::Text { size, .. } => size.retime(ratio),
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

/// A parameter's type. Mirrors the three `ExprValue` kinds, so a parameter can
/// drive any property an expression can.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ParamValue {
    Num(Value<f64>),
    Vec(Value<Vec2>),
    Color(Value<Color>),
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
        }
    }

    /// The label a picker shows, and the word a serialized param reads as.
    pub fn kind_name(&self) -> &'static str {
        match self {
            ParamValue::Num(_) => "number",
            ParamValue::Vec(_) => "vector",
            ParamValue::Color(_) => "color",
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
    pub children: Vec<Node>,
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
    pub root: Node,
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
    pub body: crate::expr::Expr,
}

impl Module {
    pub fn new(name: impl Into<String>, body: crate::expr::Expr) -> Self {
        Self { name: name.into(), params: Vec::new(), body }
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
        }
    }

    pub fn comp(&self, id: CompId) -> Option<&Comp> {
        self.comps.get(&id)
    }

    pub fn comp_mut(&mut self, id: CompId) -> Option<&mut Comp> {
        self.comps.get_mut(&id)
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

    /// Bring every comp up to the current format — see [`Comp::migrate`]. Must
    /// be called after *every* load.
    pub fn migrate(&mut self) {
        for comp in self.comps.values_mut() {
            comp.migrate();
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
