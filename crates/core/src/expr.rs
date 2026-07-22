//! Expressions: a `Value` that computes itself from other values instead of
//! sampling keyframes. This is the shared substrate roadmap #5 is built on — a
//! small dataflow IR that both hand-written expressions and (later) a node graph
//! lower to, evaluated through one context with a memo + cycle-detection cache.
//!
//! The design mirrors EBN's IR + dumb-printer split: the IR is data, evaluation
//! is a pure tree-walk over it. Determinism is by construction here — every node
//! is a pure function of the frame and the values it references.

use std::collections::{HashMap, HashSet};
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::node::{Document, Module, ModuleId, NodeId, Shape};
use crate::value::Color;

/// A value flowing through an expression, before it's converted back to a
/// property's concrete `T`. Dynamic on purpose: an expression mixes scalars,
/// positions, and colours, and only pins the type down at the property edge.
/// **Not `Copy`** — `Str` owns its bytes. Every other variant would still be a
/// register move, but one heap variant demotes the whole enum, so an
/// `ExprValue` is cloned explicitly at the few sites that need two copies.
/// Interning the strings to keep `Copy` was considered and rejected: `Expr::Lit`
/// is *serialized* into the `.pbc`, so an interner would have to round-trip
/// through the document format too.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ExprValue {
    Num(f64),
    Vec2(kurbo::Vec2),
    Color(Color),
    /// Text. The odd one out: it has no arithmetic, so `Mul`/`Neg` pass it
    /// through untouched and only `Add` means anything (concatenation, handled
    /// in [`eval_expr`] rather than in [`ExprValue::zip`], which only knows how
    /// to combine numbers). It exists so a string can be keyframed and scripted
    /// like any other property — the typewriter effect and everything past it.
    Str(String),
}

impl ExprValue {
    /// Combine two values with a scalar op, component-wise. Same-kind pairs map
    /// directly; a `Num` broadcasts across a `Vec2`/`Color`. Only used for the
    /// commutative ops (`Add`, `Mul`), so the broadcast order doesn't matter.
    /// Incompatible kinds (a `Vec2` with a `Color`) fall back to the left value.
    fn zip(self, other: ExprValue, f: impl Fn(f64, f64) -> f64) -> ExprValue {
        use ExprValue::*;
        match (self, other) {
            (Num(a), Num(b)) => Num(f(a, b)),
            (Vec2(a), Vec2(b)) => Vec2(kurbo::Vec2::new(f(a.x, b.x), f(a.y, b.y))),
            (Color(a), Color(b)) => {
                Color(self::Color::rgba(f(a.r, b.r), f(a.g, b.g), f(a.b, b.b), f(a.a, b.a)))
            }
            (Num(s), Vec2(v)) | (Vec2(v), Num(s)) => Vec2(kurbo::Vec2::new(f(v.x, s), f(v.y, s))),
            (Num(s), Color(c)) | (Color(c), Num(s)) => {
                Color(self::Color::rgba(f(c.r, s), f(c.g, s), f(c.b, s), f(c.a, s)))
            }
            (a, _) => a,
        }
    }

    /// Map every component through `f` (used by `Neg`). A `Str` has no
    /// components, so it maps to itself — negating text is meaningless, and
    /// passing it through is the same "never fail a frame" rule a kind mismatch
    /// follows.
    fn map(self, f: impl Fn(f64) -> f64) -> ExprValue {
        use ExprValue::*;
        match self {
            Num(a) => Num(f(a)),
            Vec2(v) => Vec2(kurbo::Vec2::new(f(v.x), f(v.y))),
            Color(c) => Color(self::Color::rgba(f(c.r), f(c.g), f(c.b), f(c.a))),
            Str(s) => Str(s),
        }
    }

    /// Render as text for concatenation. Numbers and compound values get a
    /// readable spelling so `"frame " + frame` works in a graph the way it does
    /// in a script, rather than silently dropping the right operand.
    fn to_str(&self) -> String {
        match self {
            ExprValue::Str(s) => s.clone(),
            ExprValue::Num(n) => format!("{n}"),
            ExprValue::Vec2(v) => format!("[{}, {}]", v.x, v.y),
            ExprValue::Color(c) => format!("[{}, {}, {}, {}]", c.r, c.g, c.b, c.a),
        }
    }
}

/// Convert a concrete property type into the dynamic [`ExprValue`] space, and
/// back. Implemented only for the scriptable value types (`f64` / `Vec2` /
/// `Color`) — never `BezPath`, which is why a `Shape::Path` isn't a `Value`.
pub trait ToExpr {
    fn to_expr(&self) -> ExprValue;
}

pub trait FromExpr: Sized {
    /// Convert if the kinds match; `None` on a type mismatch (e.g. a colour
    /// expression feeding a scalar property).
    fn from_expr(v: ExprValue) -> Option<Self>;
    /// The value to use when an expression can't produce this type — a type
    /// mismatch, a missing reference, or a detected cycle. A neutral zero.
    fn fallback() -> Self;
}

impl ToExpr for f64 {
    fn to_expr(&self) -> ExprValue {
        ExprValue::Num(*self)
    }
}
impl FromExpr for f64 {
    fn from_expr(v: ExprValue) -> Option<Self> {
        match v {
            ExprValue::Num(n) => Some(n),
            _ => None,
        }
    }
    fn fallback() -> Self {
        0.0
    }
}

impl ToExpr for kurbo::Vec2 {
    fn to_expr(&self) -> ExprValue {
        ExprValue::Vec2(*self)
    }
}
impl FromExpr for kurbo::Vec2 {
    fn from_expr(v: ExprValue) -> Option<Self> {
        match v {
            ExprValue::Vec2(v) => Some(v),
            _ => None,
        }
    }
    fn fallback() -> Self {
        kurbo::Vec2::ZERO
    }
}

impl ToExpr for Color {
    fn to_expr(&self) -> ExprValue {
        ExprValue::Color(*self)
    }
}
impl FromExpr for Color {
    fn from_expr(v: ExprValue) -> Option<Self> {
        match v {
            ExprValue::Color(c) => Some(c),
            _ => None,
        }
    }
    fn fallback() -> Self {
        Color::rgba(0.0, 0.0, 0.0, 0.0)
    }
}

impl ToExpr for String {
    fn to_expr(&self) -> ExprValue {
        ExprValue::Str(self.clone())
    }
}
impl FromExpr for String {
    /// **Strict**, like every other type here: only a `Str` converts.
    ///
    /// Stringifying whatever arrived was tried and reverted. The failure case
    /// decides it: a script that errors resolves to `Num(0.0)` as its universal
    /// "nothing", and a lenient conversion renders that as the text **"0"** —
    /// so a broken expression puts a plausible-looking `0` on the canvas
    /// instead of reading as broken. Falling back to empty (plus the warning
    /// the error already raises) is honest.
    ///
    /// Mixing kinds is still available, just **explicit**: `Add` concatenates
    /// when either side is text, so `"take " + n` is a counter and the user
    /// asked for the conversion by writing the `+`.
    fn from_expr(v: ExprValue) -> Option<Self> {
        match v {
            ExprValue::Str(s) => Some(s),
            _ => None,
        }
    }
    fn fallback() -> Self {
        String::new()
    }
}

/// Which animatable property of a node an expression can reference.
///
/// Mirrors the editor's own `PropKind` row list: the transform channels, fill,
/// stroke, and the shape params. A node that doesn't *have* the property (no
/// stroke, or a shape with no radius) resolves to the kind's neutral value
/// rather than erroring — the same rule a dangling `Ref` follows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PropPath {
    Position,
    Rotation,
    Scale,
    Opacity,
    Anchor,
    Fill,
    StrokeColor,
    StrokeWidth,
    /// `Rect`/`Ellipse` size. A `Path` shape has none.
    ShapeSize,
    /// `Rect` corner radius. Neither `Ellipse` nor `Path` has one.
    ShapeRadius,
    /// A `Text` shape's font size, in pixels. Only a text layer has one.
    TextSize,
    /// A `Text` shape's string. The only non-numeric property here — it resolves
    /// to an `ExprValue::Str`, which is what a typewriter script reads and
    /// writes.
    TextContent,
    /// A footage layer's source frame, when it is time-remapped. Only a
    /// `Shape::Image` whose `time_remap` is set has one — an unremapped clip
    /// plays at its natural rate and has no curve to read.
    TimeRemap,
    /// A mask's size. Animatable like the shape it is — which is the point of a
    /// mask being a [`crate::node::Shape`] rather than a fixed outline.
    MaskSize,
}

impl PropPath {
    /// The name a script writes: `value("A", "position")`. Also what
    /// [`std::fmt::Debug`] would give lowercased, but spelled out so renaming a
    /// variant can't silently change the script-facing vocabulary.
    pub fn name(self) -> &'static str {
        match self {
            PropPath::Position => "position",
            PropPath::Rotation => "rotation",
            PropPath::Scale => "scale",
            PropPath::Opacity => "opacity",
            PropPath::Anchor => "anchor",
            PropPath::Fill => "fill",
            PropPath::StrokeColor => "stroke_color",
            PropPath::StrokeWidth => "stroke_width",
            PropPath::ShapeSize => "size",
            PropPath::ShapeRadius => "radius",
            PropPath::TextSize => "text_size",
            PropPath::TextContent => "content",
            PropPath::TimeRemap => "time_remap",
            PropPath::MaskSize => "mask_size",
        }
    }

    /// Every referenceable property — for a picker, and for the script node's
    /// list of what `value()` accepts.
    pub const ALL: [PropPath; 14] = [
        PropPath::Position,
        PropPath::Rotation,
        PropPath::Scale,
        PropPath::Opacity,
        PropPath::Anchor,
        PropPath::Fill,
        PropPath::StrokeColor,
        PropPath::StrokeWidth,
        PropPath::ShapeSize,
        PropPath::ShapeRadius,
        PropPath::TextSize,
        PropPath::TextContent,
        PropPath::TimeRemap,
        PropPath::MaskSize,
    ];

    /// Parse a script-facing property name, case-insensitively.
    pub fn parse(s: &str) -> Option<PropPath> {
        let s = s.trim().to_ascii_lowercase();
        PropPath::ALL.into_iter().find(|p| p.name() == s)
    }

    /// The neutral value of this property's kind, for the error cases (missing
    /// node, no document, or a cycle) where there's no real value to return.
    fn zero(self) -> ExprValue {
        match self {
            PropPath::Position | PropPath::Scale | PropPath::Anchor | PropPath::ShapeSize | PropPath::MaskSize => {
                ExprValue::Vec2(kurbo::Vec2::ZERO)
            }
            PropPath::Rotation
            | PropPath::Opacity
            | PropPath::StrokeWidth
            | PropPath::ShapeRadius
            | PropPath::TextSize
            | PropPath::TimeRemap => ExprValue::Num(0.0),
            PropPath::Fill | PropPath::StrokeColor => {
                ExprValue::Color(Color::rgba(0.0, 0.0, 0.0, 0.0))
            }
            PropPath::TextContent => ExprValue::Str(String::new()),
        }
    }

    /// The socket type a wire must carry to drive this property — the same kind
    /// split [`Self::zero`] makes, said in the graph's vocabulary.
    ///
    /// This is what lets an `out` node be typed by the property it targets: pick
    /// Fill and its input socket turns colour, so the canvas refuses a number
    /// wire at authoring time rather than resolving it to a fallback at render
    /// time.
    pub fn socket_type(self) -> crate::socket::SocketType {
        use crate::socket::SocketType as S;
        match self.zero() {
            ExprValue::Num(_) => S::Number,
            ExprValue::Vec2(_) => S::Vector,
            ExprValue::Color(_) => S::Color,
            ExprValue::Str(_) => S::Text,
        }
    }
}

/// A two-operand arithmetic operator.
///
/// Deliberately a *parameter* of one [`Expr::Bin`] arm rather than an arm each.
/// Every one of these is the same shape — resolve both sides, combine them
/// component-wise — so as separate arms they would be nine copies of one
/// tree-walk, and `arity`/`child`/`Display` would each grow a nine-way match.
/// The graph's Math node is the mirror image: one node, one mode picker.
///
/// **Nothing here can fail a frame.** Division by zero, a fractional power of a
/// negative, a modulo by zero: each is a non-finite result, and `apply` returns
/// zero rather than letting a NaN travel into a transform where it would blank
/// the layer with no clue why. Same warn-don't-fail contract a dangling
/// reference follows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BinOp {
    #[default]
    Add,
    Sub,
    Mul,
    Div,
    Pow,
    Min,
    Max,
    /// Remainder. The wrap-around workhorse — `frame % 24` is a loop.
    Mod,
    /// The angle to a point, **in degrees**, so it can drive `rotation` without
    /// a conversion node in between. See [`UnOp::Sin`] on why degrees.
    Atan2,
}

/// A one-operand operator. See [`BinOp`] for why these are a parameter rather
/// than an arm each, and for the never-produce-a-NaN rule.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum UnOp {
    #[default]
    Neg,
    Abs,
    Sqrt,
    Floor,
    Round,
    /// Trig works in **degrees**, not radians.
    ///
    /// A deliberate break with `f64`'s own convention: every angle a user
    /// touches in this app is degrees (`Transform::rotation_deg`, the
    /// properties panel, the gizmo), and a graph that needed `× 57.2958` to
    /// point one layer at another would be quietly telling on itself. The
    /// conversion lives here, once, instead of in every graph.
    Sin,
    Cos,
}

impl BinOp {
    pub const ALL: [BinOp; 9] = [
        BinOp::Add,
        BinOp::Sub,
        BinOp::Mul,
        BinOp::Div,
        BinOp::Pow,
        BinOp::Min,
        BinOp::Max,
        BinOp::Mod,
        BinOp::Atan2,
    ];

    pub fn label(self) -> &'static str {
        match self {
            BinOp::Add => "Add",
            BinOp::Sub => "Subtract",
            BinOp::Mul => "Multiply",
            BinOp::Div => "Divide",
            BinOp::Pow => "Power",
            BinOp::Min => "Minimum",
            BinOp::Max => "Maximum",
            BinOp::Mod => "Modulo",
            BinOp::Atan2 => "Arctan2",
        }
    }

    /// Apply to one component pair. Guarded: see the type's docs.
    pub fn apply(self, x: f64, y: f64) -> f64 {
        let v = match self {
            BinOp::Add => x + y,
            BinOp::Sub => x - y,
            BinOp::Mul => x * y,
            BinOp::Div => x / y,
            BinOp::Pow => x.powf(y),
            BinOp::Min => x.min(y),
            BinOp::Max => x.max(y),
            BinOp::Mod => x % y,
            BinOp::Atan2 => x.atan2(y).to_degrees(),
        };
        finite_or_zero(v)
    }

    /// What a freshly placed operator rests at, so an unwired node is harmless:
    /// the operator's **identity** where it has one (0 for add, 1 for multiply
    /// and divide), and a value that doesn't blow up where it doesn't.
    pub fn seed_operands(self) -> (f64, f64) {
        match self {
            BinOp::Add | BinOp::Sub | BinOp::Min | BinOp::Max | BinOp::Atan2 => (0.0, 0.0),
            BinOp::Mul | BinOp::Div | BinOp::Pow | BinOp::Mod => (1.0, 1.0),
        }
    }

    /// The infix symbol, for the printed tree. `None` for the ones that read
    /// better as a call — `min(a, b)` rather than an invented sign.
    fn symbol(self) -> Option<&'static str> {
        match self {
            BinOp::Add => Some("+"),
            BinOp::Sub => Some("-"),
            BinOp::Mul => Some("*"),
            BinOp::Div => Some("/"),
            BinOp::Pow => Some("^"),
            BinOp::Min | BinOp::Max | BinOp::Mod | BinOp::Atan2 => None,
        }
    }

    /// The name a printed call uses, and what a script would spell.
    fn call_name(self) -> &'static str {
        match self {
            BinOp::Add => "add",
            BinOp::Sub => "sub",
            BinOp::Mul => "mul",
            BinOp::Div => "div",
            BinOp::Pow => "pow",
            BinOp::Min => "min",
            BinOp::Max => "max",
            BinOp::Mod => "mod",
            BinOp::Atan2 => "atan2",
        }
    }
}

impl UnOp {
    pub const ALL: [UnOp; 7] =
        [UnOp::Neg, UnOp::Abs, UnOp::Sqrt, UnOp::Floor, UnOp::Round, UnOp::Sin, UnOp::Cos];

    pub fn label(self) -> &'static str {
        match self {
            UnOp::Neg => "Negate",
            UnOp::Abs => "Absolute",
            UnOp::Sqrt => "Square Root",
            UnOp::Floor => "Floor",
            UnOp::Round => "Round",
            UnOp::Sin => "Sine",
            UnOp::Cos => "Cosine",
        }
    }

    /// Apply to one component. Guarded: see [`BinOp`]'s docs.
    pub fn apply(self, x: f64) -> f64 {
        let v = match self {
            UnOp::Neg => -x,
            UnOp::Abs => x.abs(),
            UnOp::Sqrt => x.sqrt(),
            UnOp::Floor => x.floor(),
            UnOp::Round => x.round(),
            UnOp::Sin => x.to_radians().sin(),
            UnOp::Cos => x.to_radians().cos(),
        };
        finite_or_zero(v)
    }

    fn call_name(self) -> &'static str {
        match self {
            UnOp::Neg => "neg",
            UnOp::Abs => "abs",
            UnOp::Sqrt => "sqrt",
            UnOp::Floor => "floor",
            UnOp::Round => "round",
            UnOp::Sin => "sin",
            UnOp::Cos => "cos",
        }
    }
}

/// Which operator a Math node is running — either arity, in one value, because
/// the node picks from one list and stores one thing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MathOp {
    Bin(BinOp),
    Un(UnOp),
}

impl Default for MathOp {
    fn default() -> Self {
        MathOp::Bin(BinOp::Add)
    }
}

impl MathOp {
    /// Every operator, in picker order: the binaries first (the common ones
    /// lead), then the unaries.
    pub fn all() -> Vec<MathOp> {
        BinOp::ALL.into_iter().map(MathOp::Bin).chain(UnOp::ALL.into_iter().map(MathOp::Un)).collect()
    }

    pub fn label(self) -> &'static str {
        match self {
            MathOp::Bin(o) => o.label(),
            MathOp::Un(o) => o.label(),
        }
    }

    /// How many operands this operator takes — 2 or 1. The Math node's socket
    /// count follows it, which is why picking `Square Root` drops the B input.
    pub fn arity(self) -> usize {
        match self {
            MathOp::Bin(_) => 2,
            MathOp::Un(_) => 1,
        }
    }
}

/// Which component of a 2-vector a [`Expr::Comp`] reads, and the axis a `split`
/// node's two outputs name. Two axes because the values here are 2-vectors —
/// position, scale, size, anchor — the dimensionality the whole app is built on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Axis {
    X,
    Y,
}

impl Axis {
    /// The socket id a `split` node's output carries, and the suffix
    /// [`fmt::Display`] prints (`v.x` / `v.y`).
    pub fn name(self) -> &'static str {
        match self {
            Axis::X => "x",
            Axis::Y => "y",
        }
    }
}

/// Replace a non-finite result with zero — the guard that keeps arithmetic from
/// ending a frame. A NaN in a transform silently blanks the layer, which is the
/// least debuggable failure this engine can produce; a zero is wrong in an
/// obvious place instead.
fn finite_or_zero(v: f64) -> f64 {
    if v.is_finite() {
        v
    } else {
        0.0
    }
}

/// The dataflow IR. Deliberately tiny: a literal, a reference to another
/// property (optionally at a shifted time — the `valueAtTime(t')` case), and
/// arithmetic. Richer front-end syntax lowers down to this, and a node that
/// looks like an operator needn't *be* one — `mix(a, b, t)` is
/// `a + (b - a) * t` in this IR, not an arm of it. Keeping that line is what
/// stops the graph becoming a second evaluator.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Expr {
    Lit(ExprValue),
    /// Another node's property, sampled at `frame + time_offset`. The offset is
    /// what makes off-time sampling work and is why the cache is keyed on frame.
    Ref {
        node: NodeId,
        prop: PropPath,
        #[serde(default)]
        time_offset: f64,
    },
    /// A two-operand arithmetic operator. One arm for every binary op rather
    /// than one arm each: they differ only in the `f64 → f64 → f64` they apply,
    /// and `ExprValue::zip` already broadcasts that across numbers, vectors and
    /// colours, so a new operator is a new [`BinOp`] and nothing else.
    Bin { op: BinOp, a: Box<Expr>, b: Box<Expr> },
    /// A one-operand operator — the [`Expr::Bin`] story with `map` in place of
    /// `zip`.
    Un { op: UnOp, a: Box<Expr> },
    /// A user-defined parameter, by name. `node: None` means "this node's" —
    /// the common case, and what keeps a parameterised node self-contained
    /// (copy the node and its expressions still point at its own knobs).
    Param {
        #[serde(default)]
        node: Option<NodeId>,
        name: String,
    },
    /// A Rhai script, evaluated each frame with `frame`/`time` in scope. Returns
    /// a number (→ `Num`) or a 2/3/4-element array (→ `Vec2`/`Color`). A leaf: it
    /// pulls its inputs from `frame`, not from wired-in child nodes.
    Script(String),
    /// Link a shared [`crate::node::Module`], optionally overriding its knobs.
    ///
    /// **Override is a layering, not a fork**: an override supplies one knob,
    /// and every knob left out inherits the module's default — the same shape as
    /// `Value`'s const→keyframe→expr layering.
    ///
    /// Overrides are evaluated **in the linking property's own scope**, before
    /// the module body runs. That is what lets a link pass `t01` or one of its
    /// own node's params into a shared module, and it keeps the module body a
    /// pure function of its knobs.
    Use {
        module: ModuleId,
        #[serde(default)]
        overrides: Vec<(String, Expr)>,
    },
    /// The layer's own clock: local frame, in/out point, or normalized progress.
    /// A leaf, like a literal, but one whose value depends on *which layer* is
    /// resolving — which is what lets one expression fit itself to any clip.
    Time(TimeSource),
    /// A procedural generator — a typed-knob motion primitive (oscillator, noise,
    /// ramp, bounce) that computes a number from the frame, without a script. Its
    /// knobs are themselves `Expr`s (children in the graph), so a knob can be a
    /// literal, a `Param`, or any expression — the point of shipping generators
    /// after parameters. Always resolves to a `Num`; feed it through `Mul` to
    /// broadcast onto a vec/colour property.
    Gen(Generator),
    /// Build a 2-vector from two scalar sub-expressions — what a `join` node
    /// lowers to. Each child resolves to a number (a vec/colour coerces to a
    /// component, a string to zero) and the pair becomes a `Vec2`. The one IR
    /// node that *raises* dimensionality, so a graph can drive `position` or
    /// `scale` from two independently-built scalars.
    Vec2 { x: Box<Expr>, y: Box<Expr> },
    /// One component of a vector-valued sub-expression — what a `split` node's
    /// `x`/`y` outputs lower to. The inverse of [`Expr::Vec2`]: a `Vec2` yields
    /// the named axis, a colour reads r/g, and a scalar passes through.
    Comp { a: Box<Expr>, axis: Axis },
}

/// Which reading of the current layer's clock an [`Expr::Time`] takes.
///
/// **Everything here is in layer-local frames**, not comp frames — the same
/// domain the layer's keyframes are authored in (see [`crate::node::LayerTiming`]).
/// So `InPoint` is where the layer becomes visible *relative to its own frame
/// 0*, and an expression written against these reads identically on two clips
/// with different in-points. That is the whole point: one animation, auto-fitted.
///
/// A layer with no timing has local time = comp time, so it reads `In = 0` and
/// `Out = ` the comp duration — an untimed layer behaves as if it were one clip
/// spanning the composition, which keeps `T01` meaningful everywhere.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TimeSource {
    /// The current local frame — `frame`, but explicit about whose clock it is.
    Local,
    /// First local frame the layer draws on.
    In,
    /// First local frame it no longer draws on (exclusive).
    Out,
    /// Progress through the layer, `0` at the in-point and `1` at the out-point,
    /// clamped outside. The piece that makes "ease over the first/last N frames"
    /// fit any clip length without touching a keyframe.
    T01,
}

impl TimeSource {
    /// The name a picker shows and a printed expression reads as. Matches the
    /// identifier a Rhai script uses for the same reading, so the graph and the
    /// script spellings are one vocabulary.
    pub fn label(self) -> &'static str {
        match self {
            TimeSource::Local => "localTime",
            TimeSource::In => "inPoint",
            TimeSource::Out => "outPoint",
            TimeSource::T01 => "t01",
        }
    }

    pub fn kind(self) -> ExprKind {
        match self {
            TimeSource::Local => ExprKind::LocalTime,
            TimeSource::In => ExprKind::InPoint,
            TimeSource::Out => ExprKind::OutPoint,
            TimeSource::T01 => ExprKind::T01,
        }
    }
}

/// A periodic waveform for [`Generator::Oscillator`]. Sampled over a phase in
/// *cycles* (`1.0` = one full period), so every shape shares the oscillator's
/// `freq`/`phase` units. Each returns a value in `[-1, 1]`.
/// `Sine` is the default — the shape an oscillator is unless something says
/// otherwise, and what a graph node lowers to when its config predates the
/// waveform field.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Waveform {
    #[default]
    Sine,
    Triangle,
    Square,
    Saw,
}

impl Waveform {
    /// Every waveform, in picker order.
    pub const ALL: [Waveform; 4] = [Waveform::Sine, Waveform::Triangle, Waveform::Square, Waveform::Saw];

    pub fn label(self) -> &'static str {
        match self {
            Waveform::Sine => "sine",
            Waveform::Triangle => "triangle",
            Waveform::Square => "square",
            Waveform::Saw => "saw",
        }
    }

    /// Sample the wave at `phase` cycles. Sine is the true trig curve; the other
    /// three are built off the fractional cycle `u ∈ [0, 1)` so they stay exactly
    /// periodic and land on `±1`.
    pub fn sample(self, phase: f64) -> f64 {
        let u = phase - phase.floor();
        match self {
            Waveform::Sine => (std::f64::consts::TAU * phase).sin(),
            // Peaks at u = 0.5, troughs at the ends — a symmetric triangle.
            Waveform::Triangle => 1.0 - 4.0 * (u - 0.5).abs(),
            Waveform::Square => {
                if u < 0.5 {
                    1.0
                } else {
                    -1.0
                }
            }
            Waveform::Saw => 2.0 * u - 1.0,
        }
    }
}

/// A procedural generator: a named motion primitive with typed knobs. Each knob
/// is a boxed [`Expr`], so it defaults to a literal you drag in the canvas but
/// can be rewired to a `Param`/`Ref`/expression like any other node. All four
/// are pure functions of the frame — deterministic, so scrubbing is stable and a
/// render matches the preview, the same contract as `wiggle`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Generator {
    /// `offset + amp · wave(freq · frame + phase)`. `freq` is cycles per frame,
    /// `phase` is in cycles. A steady periodic wobble.
    Oscillator {
        freq: Box<Expr>,
        amp: Box<Expr>,
        phase: Box<Expr>,
        offset: Box<Expr>,
        wave: Waveform,
    },
    /// `amp · noise(freq · frame, seed)` — smooth value noise in `[-amp, amp]`,
    /// the same generator behind the `wiggle()` script fn. `seed` picks an
    /// independent stream so two channels can wobble differently.
    Noise { freq: Box<Expr>, amp: Box<Expr>, seed: Box<Expr> },
    /// A linear ramp from `from` to `to` across frames `start..end`, clamped flat
    /// outside that window. A keyframe pair as a knob you can drive.
    Ramp { from: Box<Expr>, to: Box<Expr>, start: Box<Expr>, end: Box<Expr> },
    /// `amp · e^(−decay · frame) · cos(2π · freq · frame)` — a damped oscillation
    /// that overshoots and settles to zero: the classic bounce/elastic settle.
    Bounce { amp: Box<Expr>, freq: Box<Expr>, decay: Box<Expr> },
}

impl Generator {
    /// The kind, for the editor's picker.
    pub fn kind(&self) -> ExprKind {
        match self {
            Generator::Oscillator { .. } => ExprKind::Oscillator,
            Generator::Noise { .. } => ExprKind::Noise,
            Generator::Ramp { .. } => ExprKind::Ramp,
            Generator::Bounce { .. } => ExprKind::Bounce,
        }
    }

    /// This generator's knob labels, in slot order — the canvas labels each knob
    /// box with these, and they name the child slots `at`/`at_mut` address.
    pub fn knob_labels(&self) -> &'static [&'static str] {
        match self {
            Generator::Oscillator { .. } => &["freq", "amp", "phase", "offset"],
            Generator::Noise { .. } => &["freq", "amp", "seed"],
            Generator::Ramp { .. } => &["from", "to", "start", "end"],
            Generator::Bounce { .. } => &["amp", "freq", "decay"],
        }
    }

    /// How many knob slots — the generator node's arity in the graph.
    pub fn arity(&self) -> usize {
        self.knob_labels().len()
    }

    /// Borrow the knob at `slot`, in the order [`Generator::knob_labels`] gives.
    pub fn knob(&self, slot: usize) -> Option<&Expr> {
        let b = match (self, slot) {
            (Generator::Oscillator { freq, .. }, 0) => freq,
            (Generator::Oscillator { amp, .. }, 1) => amp,
            (Generator::Oscillator { phase, .. }, 2) => phase,
            (Generator::Oscillator { offset, .. }, 3) => offset,
            (Generator::Noise { freq, .. }, 0) => freq,
            (Generator::Noise { amp, .. }, 1) => amp,
            (Generator::Noise { seed, .. }, 2) => seed,
            (Generator::Ramp { from, .. }, 0) => from,
            (Generator::Ramp { to, .. }, 1) => to,
            (Generator::Ramp { start, .. }, 2) => start,
            (Generator::Ramp { end, .. }, 3) => end,
            (Generator::Bounce { amp, .. }, 0) => amp,
            (Generator::Bounce { freq, .. }, 1) => freq,
            (Generator::Bounce { decay, .. }, 2) => decay,
            _ => return None,
        };
        Some(b.as_ref())
    }

    /// Mutably borrow the knob at `slot` — the write path `at_mut` recurses into.
    pub fn knob_mut(&mut self, slot: usize) -> Option<&mut Expr> {
        let b = match (self, slot) {
            (Generator::Oscillator { freq, .. }, 0) => freq,
            (Generator::Oscillator { amp, .. }, 1) => amp,
            (Generator::Oscillator { phase, .. }, 2) => phase,
            (Generator::Oscillator { offset, .. }, 3) => offset,
            (Generator::Noise { freq, .. }, 0) => freq,
            (Generator::Noise { amp, .. }, 1) => amp,
            (Generator::Noise { seed, .. }, 2) => seed,
            (Generator::Ramp { from, .. }, 0) => from,
            (Generator::Ramp { to, .. }, 1) => to,
            (Generator::Ramp { start, .. }, 2) => start,
            (Generator::Ramp { end, .. }, 3) => end,
            (Generator::Bounce { amp, .. }, 0) => amp,
            (Generator::Bounce { freq, .. }, 1) => freq,
            (Generator::Bounce { decay, .. }, 2) => decay,
            _ => return None,
        };
        Some(b.as_mut())
    }

    /// A fresh generator of `kind`, with knobs seeded to sensible literal
    /// defaults so a just-created generator already animates.
    fn seed(kind: ExprKind) -> Generator {
        let lit = |n: f64| Box::new(Expr::num(n));
        match kind {
            ExprKind::Oscillator => Generator::Oscillator {
                freq: lit(0.05),
                amp: lit(1.0),
                phase: lit(0.0),
                offset: lit(0.0),
                wave: Waveform::Sine,
            },
            ExprKind::Noise => Generator::Noise { freq: lit(0.1), amp: lit(1.0), seed: lit(0.0) },
            ExprKind::Ramp => {
                Generator::Ramp { from: lit(0.0), to: lit(1.0), start: lit(0.0), end: lit(30.0) }
            }
            ExprKind::Bounce => Generator::Bounce { amp: lit(1.0), freq: lit(0.1), decay: lit(0.05) },
            // Not a generator kind — the callers only pass the four above.
            _ => Generator::Noise { freq: lit(0.1), amp: lit(1.0), seed: lit(0.0) },
        }
    }

    /// Evaluate the generator at `ctx`'s frame. Knobs resolve first (so a knob
    /// can be a param or expression); each is coerced to a scalar. Deterministic.
    fn eval(&self, ctx: &mut EvalCtx) -> f64 {
        let frame = ctx.frame;
        let knob = |i: usize, ctx: &mut EvalCtx| -> f64 {
            match self.knob(i) {
                Some(e) => eval_num(e, ctx),
                None => 0.0,
            }
        };
        match self {
            Generator::Oscillator { wave, .. } => {
                let (freq, amp, phase, offset) =
                    (knob(0, ctx), knob(1, ctx), knob(2, ctx), knob(3, ctx));
                offset + amp * wave.sample(freq * frame + phase)
            }
            Generator::Noise { .. } => {
                let (freq, amp, seed) = (knob(0, ctx), knob(1, ctx), knob(2, ctx));
                amp * value_noise(freq * frame, seed)
            }
            Generator::Ramp { .. } => {
                let (from, to, start, end) =
                    (knob(0, ctx), knob(1, ctx), knob(2, ctx), knob(3, ctx));
                let t = if end <= start {
                    // A zero/negative window is a step at `start`.
                    if frame >= start {
                        1.0
                    } else {
                        0.0
                    }
                } else {
                    ((frame - start) / (end - start)).clamp(0.0, 1.0)
                };
                from + (to - from) * t
            }
            Generator::Bounce { .. } => {
                let (amp, freq, decay) = (knob(0, ctx), knob(1, ctx), knob(2, ctx));
                amp * (-decay * frame).exp() * (std::f64::consts::TAU * freq * frame).cos()
            }
        }
    }
}

/// Evaluate a knob expression down to a scalar. A knob is usually a `Num`; a vec
/// or colour knob (say a `Param` of the wrong kind) coerces to its first
/// component rather than erroring, so a generator always produces a value.
fn eval_num(expr: &Expr, ctx: &mut EvalCtx) -> f64 {
    // Text has no scalar reading, so a knob fed a string is 0 — the same neutral
    // answer a kind mismatch gets everywhere else. See [`as_scalar`].
    as_scalar(eval_expr(expr, ctx))
}

impl Expr {
    /// A literal number, the common case.
    pub fn num(n: f64) -> Expr {
        Expr::Lit(ExprValue::Num(n))
    }
    /// A binary operator over two sub-expressions.
    pub fn bin(op: BinOp, a: Expr, b: Expr) -> Expr {
        Expr::Bin { op, a: Box::new(a), b: Box::new(b) }
    }
    /// A unary operator over one sub-expression.
    pub fn un(op: UnOp, a: Expr) -> Expr {
        Expr::Un { op, a: Box::new(a) }
    }
    /// A reference to `prop` on `node`, at the current frame.
    pub fn reference(node: NodeId, prop: PropPath) -> Expr {
        Expr::Ref { node, prop, time_offset: 0.0 }
    }
    /// A reference shifted by `frames` (negative = earlier — a trailing echo).
    pub fn reference_at(node: NodeId, prop: PropPath, frames: f64) -> Expr {
        Expr::Ref { node, prop, time_offset: frames }
    }

    /// Which variant this node is — for an editor's kind picker, without
    /// exposing the payloads.
    pub fn kind(&self) -> ExprKind {
        match self {
            Expr::Lit(_) => ExprKind::Lit,
            Expr::Ref { .. } => ExprKind::Ref,
            Expr::Bin { .. } => ExprKind::Bin,
            Expr::Un { .. } => ExprKind::Un,
            Expr::Param { .. } => ExprKind::Param,
            Expr::Script(_) => ExprKind::Script,
            Expr::Use { .. } => ExprKind::Use,
            Expr::Time(t) => t.kind(),
            Expr::Gen(g) => g.kind(),
            Expr::Vec2 { .. } => ExprKind::Vec2,
            Expr::Comp { .. } => ExprKind::Comp,
        }
    }

    /// A fresh node of `kind`, with children seeded to neutral literals so an
    /// editor can grow a tree by changing one node's kind at a time (a `Lit`
    /// becomes a `Bin` of two zeros you then edit or change further). The
    /// operands come from [`BinOp::seed_operands`] — the operator's identity
    /// where it has one — so a half-built node is harmless.
    pub fn seed(kind: ExprKind) -> Expr {
        match kind {
            ExprKind::Lit => Expr::num(0.0),
            ExprKind::Ref => Expr::Ref {
                node: NodeId(0),
                prop: PropPath::Position,
                time_offset: 0.0,
            },
            ExprKind::Bin => Expr::bin(BinOp::default(), Expr::num(0.0), Expr::num(0.0)),
            ExprKind::Un => Expr::un(UnOp::default(), Expr::num(0.0)),
            ExprKind::Param => Expr::Param { node: None, name: String::new() },
            ExprKind::Use => Expr::Use { module: ModuleId(0), overrides: Vec::new() },
            ExprKind::Script => Expr::Script(String::new()),
            ExprKind::LocalTime => Expr::Time(TimeSource::Local),
            ExprKind::InPoint => Expr::Time(TimeSource::In),
            ExprKind::OutPoint => Expr::Time(TimeSource::Out),
            ExprKind::T01 => Expr::Time(TimeSource::T01),
            ExprKind::Oscillator | ExprKind::Noise | ExprKind::Ramp | ExprKind::Bounce => {
                Expr::Gen(Generator::seed(kind))
            }
            ExprKind::Vec2 => Expr::Vec2 { x: Box::new(Expr::num(0.0)), y: Box::new(Expr::num(0.0)) },
            ExprKind::Comp => Expr::Comp { a: Box::new(Expr::num(0.0)), axis: Axis::X },
        }
    }

    /// How many child slots this node has: a binary operator two, a unary one,
    /// others none. Lets an editor iterate a node's inputs without matching the
    /// kind.
    pub fn arity(&self) -> usize {
        match self {
            Expr::Bin { .. } | Expr::Vec2 { .. } => 2,
            Expr::Un { .. } | Expr::Comp { .. } => 1,
            Expr::Gen(g) => g.arity(),
            // A link's children are its overrides, in order — so the canvas
            // lays each override out as a wired box and edits it like any other
            // sub-expression. A link with no overrides is still a leaf.
            Expr::Use { overrides, .. } => overrides.len(),
            Expr::Lit(_)
            | Expr::Ref { .. }
            | Expr::Param { .. }
            | Expr::Script(_)
            | Expr::Time(_) => 0,
        }
    }

    /// Borrow this node's child at `slot` (one level), whatever the kind — the
    /// operator operands, a generator's knobs, *or* a link's overrides. Lets an
    /// editor walk inputs without matching on the variant.
    pub fn child(&self, slot: usize) -> Option<&Expr> {
        match (self, slot) {
            (Expr::Bin { a, .. } | Expr::Un { a, .. } | Expr::Comp { a, .. }, 0) => Some(a),
            (Expr::Bin { b, .. }, 1) => Some(b),
            (Expr::Vec2 { x, .. }, 0) => Some(x),
            (Expr::Vec2 { y, .. }, 1) => Some(y),
            (Expr::Gen(g), _) => g.knob(slot),
            (Expr::Use { overrides, .. }, _) => overrides.get(slot).map(|(_, e)| e),
            _ => None,
        }
    }

    /// The label for child `slot`, if it has one: a generator names its knobs
    /// (`freq`/`amp`/…); operator operands are positional and return `None`.
    pub fn slot_label(&self, slot: usize) -> Option<&'static str> {
        match self {
            Expr::Gen(g) => g.knob_labels().get(slot).copied(),
            Expr::Vec2 { .. } => ["x", "y"].get(slot).copied(),
            _ => None,
        }
    }

    /// Borrow the subtree at `path` (a sequence of child slots). `None` for an
    /// out-of-range slot.
    pub fn at(&self, path: &[usize]) -> Option<&Expr> {
        let Some((&slot, rest)) = path.split_first() else {
            return Some(self);
        };
        let child = match (self, slot) {
            (Expr::Bin { a, .. } | Expr::Un { a, .. } | Expr::Comp { a, .. }, 0) => a.as_ref(),
            (Expr::Bin { b, .. }, 1) => b.as_ref(),
            (Expr::Vec2 { x, .. }, 0) => x.as_ref(),
            (Expr::Vec2 { y, .. }, 1) => y.as_ref(),
            (Expr::Gen(g), _) => g.knob(slot)?,
            (Expr::Use { overrides, .. }, _) => &overrides.get(slot)?.1,
            _ => return None,
        };
        child.at(rest)
    }

    /// Borrow the subtree at `path` (a sequence of child slots from this node).
    /// An out-of-range slot yields `None`, so a stale editor path no-ops.
    pub fn at_mut(&mut self, path: &[usize]) -> Option<&mut Expr> {
        let Some((&slot, rest)) = path.split_first() else {
            return Some(self);
        };
        let child = match (self, slot) {
            (Expr::Bin { a, .. } | Expr::Un { a, .. } | Expr::Comp { a, .. }, 0) => a.as_mut(),
            (Expr::Bin { b, .. }, 1) => b.as_mut(),
            (Expr::Vec2 { x, .. }, 0) => x.as_mut(),
            (Expr::Vec2 { y, .. }, 1) => y.as_mut(),
            (Expr::Gen(g), _) => g.knob_mut(slot)?,
            (Expr::Use { overrides, .. }, _) => &mut overrides.get_mut(slot)?.1,
            _ => return None,
        };
        child.at_mut(rest)
    }
}

/// The variant of an [`Expr`] node, for an editor's kind picker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExprKind {
    Lit,
    Ref,
    /// Any two-operand operator; which one is the [`BinOp`] it carries.
    Bin,
    /// Any one-operand operator; which one is the [`UnOp`] it carries.
    Un,
    Param,
    Script,
    /// A link to a shared module. Deliberately **not** in [`ExprKind::ALL`]:
    /// that list is the graph picker, and seeding a link needs a module to point
    /// at, which a bare kind can't carry. The picker instead lists the modules
    /// themselves and seeds the link with a `SetModule` op (see the graph UI's
    /// kind picker), so a `Use` is created by choosing *which* module, not just
    /// the kind.
    Use,
    LocalTime,
    InPoint,
    OutPoint,
    T01,
    Oscillator,
    Noise,
    Ramp,
    Bounce,
    /// Build a vector from two scalars — the `join` node. Like [`ExprKind::Use`],
    /// kept out of [`ExprKind::ALL`]: it's created by placing the graph node, not
    /// by the generic kind picker.
    Vec2,
    /// Read one axis of a vector — the `split` node. Out of `ALL` for the same
    /// reason as [`ExprKind::Vec2`].
    Comp,
}

impl ExprKind {
    /// Every kind, in picker order — the generators grouped after the primitives.
    pub const ALL: [ExprKind; 14] = [
        ExprKind::Lit,
        ExprKind::Ref,
        ExprKind::Bin,
        ExprKind::Un,
        ExprKind::Param,
        ExprKind::Script,
        ExprKind::LocalTime,
        ExprKind::InPoint,
        ExprKind::OutPoint,
        ExprKind::T01,
        ExprKind::Oscillator,
        ExprKind::Noise,
        ExprKind::Ramp,
        ExprKind::Bounce,
    ];

    pub fn label(self) -> &'static str {
        match self {
            ExprKind::Lit => "value",
            ExprKind::Ref => "ref",
            ExprKind::Bin => "math",
            ExprKind::Un => "math",
            ExprKind::Param => "param",
            ExprKind::Script => "script",
            ExprKind::Use => "use",
            ExprKind::LocalTime => "localTime",
            ExprKind::InPoint => "inPoint",
            ExprKind::OutPoint => "outPoint",
            ExprKind::T01 => "t01",
            ExprKind::Oscillator => "osc",
            ExprKind::Noise => "noise",
            ExprKind::Ramp => "ramp",
            ExprKind::Bounce => "bounce",
            ExprKind::Vec2 => "join",
            ExprKind::Comp => "split",
        }
    }

    /// Whether this kind is a procedural generator (vs. a primitive/operator).
    pub fn is_generator(self) -> bool {
        matches!(self, ExprKind::Oscillator | ExprKind::Noise | ExprKind::Ramp | ExprKind::Bounce)
    }
}

impl fmt::Display for ExprValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ExprValue::Num(n) => write!(f, "{n}"),
            ExprValue::Vec2(v) => write!(f, "[{}, {}]", v.x, v.y),
            ExprValue::Color(c) => write!(f, "rgba({}, {}, {}, {})", c.r, c.g, c.b, c.a),
            // Quoted and escaped, so a printed expression round-trips visually
            // and an empty string is visible rather than a gap.
            ExprValue::Str(s) => write!(f, "{:?}", s),
        }
    }
}

/// The "dumb printer": an [`Expr`] rendered back to a compact, readable form.
/// A reference reads `@node.Prop` (with `[+n]`/`[-n]` for a time offset).
impl fmt::Display for Expr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Expr::Lit(v) => write!(f, "{v}"),
            Expr::Ref { node, prop, time_offset } => {
                if *time_offset == 0.0 {
                    write!(f, "@{}.{prop:?}", node.0)
                } else {
                    write!(f, "@{}.{prop:?}[{time_offset:+}]", node.0)
                }
            }

            Expr::Bin { op, a, b } => match op.symbol() {
                Some(sym) => write!(f, "({a} {sym} {b})"),
                None => write!(f, "{}({a}, {b})", op.call_name()),
            },
            Expr::Un { op: UnOp::Neg, a } => write!(f, "-{a}"),
            Expr::Un { op, a } => write!(f, "{}({a})", op.call_name()),
            Expr::Param { node: None, name } => write!(f, "param({name})"),
            Expr::Param { node: Some(n), name } => write!(f, "param(#{}, {name})", n.0),
            Expr::Script(src) => write!(f, "{{ {src} }}"),
            Expr::Use { module, overrides } => {
                write!(f, "use(#{}", module.0)?;
                for (name, value) in overrides {
                    write!(f, ", {name}: {value}")?;
                }
                write!(f, ")")
            }
            Expr::Time(t) => f.write_str(t.label()),
            Expr::Gen(g) => write!(f, "{g}"),
            Expr::Vec2 { x, y } => write!(f, "vec2({x}, {y})"),
            Expr::Comp { a, axis } => write!(f, "{a}.{}", axis.name()),
        }
    }
}

impl fmt::Display for Waveform {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// `osc(freq: .., amp: .., …)` — the generator's label with its knobs named, so
/// a printed tree reads the same as the canvas boxes.
impl fmt::Display for Generator {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}(", self.kind().label())?;
        for (slot, label) in self.knob_labels().iter().enumerate() {
            if slot > 0 {
                write!(f, ", ")?;
            }
            write!(f, "{label}: {}", self.knob(slot).unwrap())?;
        }
        if let Generator::Oscillator { wave, .. } = self {
            write!(f, ", {wave}")?;
        }
        write!(f, ")")
    }
}

thread_local! {
    /// One Rhai engine per thread, reused across evaluations. Building an engine
    /// isn't free, and resolving runs on the (single) render thread.
    static SCRIPT_ENGINE: rhai::Engine = build_engine();
}

/// The bridge that lets a `'static` Rhai function reach the `&mut EvalCtx` of
/// the evaluation that called it.
///
/// Rhai's registered functions must be `'static`, so the borrow can't be
/// captured; it's parked in a thread-local raw pointer for exactly the span of
/// one `eval_with_scope` call. Three rules keep that sound, and the module is
/// small so they can be checked by reading it:
///
/// 1. **Lifetime.** [`enter`] stores a pointer derived from a live `&mut
///    EvalCtx` and the returned guard clears it on drop, so the pointer is only
///    observable while that borrow is alive. Rhai calls back synchronously,
///    inside `eval_with_scope`, which is inside the guard's scope.
/// 2. **Aliasing.** [`with_ctx`] *takes* the pointer (leaving null) for the
///    duration of the callback, so a second `&mut` can never exist alongside
///    the first. A nested script (`value()` → a referenced property that is
///    itself a script) re-parks a pointer through `enter` from the inner
///    borrow, which is the correct nesting order; its guard restores the outer
///    pointer on the way out.
/// 3. **Threads.** The pointer is thread-local, so it cannot escape to another
///    thread, and `EvalCtx` need not be `Send`.
///
/// Outside a script call the pointer is null and the `value()` family returns a
/// script error rather than pretending to have a document.
mod bridge {
    use super::EvalCtx;
    use std::cell::Cell;
    use std::ptr;

    thread_local! {
        /// Type-erased `*mut EvalCtx<'_>`; the lifetime can't be named here and
        /// is re-attached only inside [`with_ctx`], for the callback's span.
        static CTX: Cell<*mut ()> = const { Cell::new(ptr::null_mut()) };
    }

    /// Restores the previously parked pointer, so nesting is transparent.
    pub(super) struct Guard(*mut ());

    impl Drop for Guard {
        fn drop(&mut self) {
            CTX.with(|c| c.set(self.0));
        }
    }

    /// Park `ctx` for the lifetime of the returned guard.
    pub(super) fn enter(ctx: &mut EvalCtx<'_>) -> Guard {
        let raw = ctx as *mut EvalCtx<'_> as *mut ();
        Guard(CTX.with(|c| c.replace(raw)))
    }

    /// Run `f` against the parked context, or return `None` if there is none.
    pub(super) fn with_ctx<R>(f: impl FnOnce(&mut EvalCtx<'_>) -> R) -> Option<R> {
        // Take it out for the call: while `f` holds the `&mut`, no other
        // `with_ctx` on this thread can hand out a second one (rule 2).
        let raw = CTX.with(|c| c.replace(ptr::null_mut()));
        if raw.is_null() {
            return None;
        }
        let _restore = Guard(raw);
        // SAFETY: `raw` was derived from a `&mut EvalCtx` in `enter`, whose
        // guard is still alive (it is only cleared on drop), and it was taken
        // out of the cell above so no other borrow can be handed out while this
        // one lives. The erased lifetime is re-attached only for this call,
        // which cannot outlive the original borrow.
        Some(f(unsafe { &mut *(raw as *mut EvalCtx<'_>) }))
    }
}

/// Build the per-thread engine with the scene-access functions registered.
fn build_engine() -> rhai::Engine {
    let mut engine = rhai::Engine::new();
    engine.register_fn("value", |name: &str, prop: &str| script_value(name, prop, None));
    engine.register_fn("value_at", |name: &str, prop: &str, frame: f64| {
        script_value(name, prop, Some(frame))
    });
    engine.register_fn("param", |name: &str| script_param(None, name));
    engine.register_fn("param_of", |node: &str, name: &str| script_param(Some(node), name));
    engine.register_fn("wiggle", |freq: f64, amp: f64| script_wiggle(freq, amp, 0.0));
    engine.register_fn("wiggle", |freq: f64, amp: f64, seed: f64| script_wiggle(freq, amp, seed));
    engine
}

/// A script error, phrased for the field under the editor.
fn script_err(msg: impl Into<String>) -> Box<rhai::EvalAltResult> {
    Box::new(rhai::EvalAltResult::ErrorRuntime(msg.into().into(), rhai::Position::NONE))
}

/// `value(name, prop)` / `value_at(name, prop, frame)`: another node's property,
/// by node name, at the current frame or an explicit one. Goes through the same
/// memoized, cycle-guarded [`EvalCtx::resolve_prop`] as an `Expr::Ref`, so a
/// script that references itself warns and falls back instead of recursing.
fn script_value(
    name: &str,
    prop: &str,
    frame: Option<f64>,
) -> Result<rhai::Dynamic, Box<rhai::EvalAltResult>> {
    let prop = PropPath::parse(prop)
        .ok_or_else(|| script_err(format!("unknown property '{prop}'")))?;
    let out = bridge::with_ctx(|ctx| {
        let node = ctx.find_named(name)?;
        let frame = frame.unwrap_or(ctx.frame);
        Some(ctx.resolve_prop(node, prop, frame))
    })
    .ok_or_else(|| script_err("value() is only available while evaluating a document"))?
    .ok_or_else(|| script_err(format!("no node named '{name}'")))?;
    Ok(expr_value_to_dynamic(out))
}

/// `param("speed")` — this node's parameter; `param_of("A", "speed")` — another
/// node's. Resolved through the same memoized, cycle-guarded path as a
/// property, so a parameter driven by an expression that reads it back warns
/// instead of hanging.
fn script_param(
    node: Option<&str>,
    name: &str,
) -> Result<rhai::Dynamic, Box<rhai::EvalAltResult>> {
    let out = bridge::with_ctx(|ctx| {
        let owner = match node {
            Some(n) => ctx.find_named(n).ok_or_else(|| format!("no node named '{n}'"))?,
            None => ctx.current.ok_or_else(|| {
                "param() has no owning node here; use param_of(\"node\", \"name\")".to_string()
            })?,
        };
        let frame = ctx.frame;
        ctx.resolve_param(owner, name, frame)
            .ok_or_else(|| format!("no parameter named '{name}' on node {}", owner.0))
    })
    .ok_or_else(|| script_err("param() is only available while evaluating a document"))?
    .map_err(script_err)?;
    Ok(expr_value_to_dynamic(out))
}

/// `wiggle(freq, amp)` — smooth pseudo-random deviation around zero, sampled at
/// the current frame. Deterministic: the same frame always gives the same value
/// (scrubbing back and forth is stable, and a render matches the preview).
/// `freq` is in wiggles per frame; `seed` picks an independent stream, so `x`
/// and `y` can wiggle separately.
fn script_wiggle(freq: f64, amp: f64, seed: f64) -> Result<f64, Box<rhai::EvalAltResult>> {
    let frame = bridge::with_ctx(|ctx| ctx.frame)
        .ok_or_else(|| script_err("wiggle() is only available while evaluating a document"))?;
    Ok(value_noise(frame * freq, seed) * amp)
}

/// Value noise in [-1, 1]: hashed lattice points, smoothstep-interpolated.
fn value_noise(t: f64, seed: f64) -> f64 {
    let i = t.floor();
    let f = t - i;
    let f = f * f * (3.0 - 2.0 * f); // smoothstep — C1, so no kinks at the lattice
    let a = hash_to_unit(i, seed);
    let b = hash_to_unit(i + 1.0, seed);
    a + (b - a) * f
}

/// A stable hash of a lattice point to [-1, 1].
fn hash_to_unit(i: f64, seed: f64) -> f64 {
    let mut h = (i as i64 as u64) ^ ((seed as i64 as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15));
    h ^= h >> 33;
    h = h.wrapping_mul(0xFF51_AFD7_ED55_8CCD);
    h ^= h >> 33;
    h = h.wrapping_mul(0xC4CE_B9FE_1A85_EC53);
    h ^= h >> 33;
    // Top 53 bits → [0, 1), then recentre.
    ((h >> 11) as f64 / (1u64 << 53) as f64) * 2.0 - 1.0
}

/// The inverse of [`dynamic_to_expr_value`], for handing a resolved property
/// back to a script: a number stays a number, a vec/colour becomes an array.
fn expr_value_to_dynamic(v: ExprValue) -> rhai::Dynamic {
    match v {
        ExprValue::Num(n) => rhai::Dynamic::from_float(n),
        // Rhai has a native string type, so text crosses the bridge as itself —
        // which is what makes the whole Rhai string library (`sub_string`, `+`,
        // `len`) available to a text property for free.
        ExprValue::Str(s) => rhai::Dynamic::from(s),
        ExprValue::Vec2(v) => rhai::Dynamic::from_array(vec![
            rhai::Dynamic::from_float(v.x),
            rhai::Dynamic::from_float(v.y),
        ]),
        ExprValue::Color(c) => rhai::Dynamic::from_array(vec![
            rhai::Dynamic::from_float(c.r),
            rhai::Dynamic::from_float(c.g),
            rhai::Dynamic::from_float(c.b),
            rhai::Dynamic::from_float(c.a),
        ]),
    }
}

/// Evaluate a Rhai `src` at `frame`, with `frame` and `time` in scope as
/// constants, with no document behind it — `value()`/`wiggle()` then error out.
/// Prefer [`eval_script_ctx`] anywhere a context exists.
pub fn eval_script(src: &str, frame: f64) -> Result<ExprValue, String> {
    eval_script_ctx(src, &mut EvalCtx::at(frame))
}

/// Evaluate a Rhai `src` against `ctx` (at `ctx.frame`), with `frame`/`time` in
/// scope and the `value()`/`wiggle()` functions wired to the document. The
/// script's result becomes an [`ExprValue`]: a number → `Num`, a 2/3/4-element
/// array → `Vec2`/`Color`. Returns a message on a compile-time, run-time, or
/// return-type error — the UI surfaces it; [`eval_expr`] falls back to a neutral
/// value so a bad script never breaks the frame.
pub fn eval_script_ctx(src: &str, ctx: &mut EvalCtx<'_>) -> Result<ExprValue, String> {
    let frame = ctx.frame;
    // Read the layer clock before parking the context — the same four readings
    // `Expr::Time` exposes, under the same names, so a script and a graph node
    // are two spellings of one vocabulary.
    let (in_point, out_point) = ctx.local_window();
    let t01 = ctx.time_source(TimeSource::T01);
    let _parked = bridge::enter(ctx);
    SCRIPT_ENGINE.with(|engine| {
        let mut scope = rhai::Scope::new();
        scope.push_constant("frame", frame);
        scope.push_constant("time", frame);
        scope.push_constant("localTime", frame);
        scope.push_constant("inPoint", in_point);
        scope.push_constant("outPoint", out_point);
        scope.push_constant("t01", t01);
        let out: rhai::Dynamic =
            engine.eval_with_scope(&mut scope, src).map_err(|e| e.to_string())?;
        dynamic_to_expr_value(&out)
    })
}

fn dynamic_to_expr_value(d: &rhai::Dynamic) -> Result<ExprValue, String> {
    if let Ok(f) = d.as_float() {
        return Ok(ExprValue::Num(f));
    }
    if let Ok(i) = d.as_int() {
        return Ok(ExprValue::Num(i as f64));
    }
    // Checked before the array branch but after the numeric ones: Rhai reports
    // a string as neither float nor int, so ordering only matters against
    // `is_array` (a string is not one) — kept here for readability.
    if d.is_string() {
        return Ok(ExprValue::Str(d.clone().into_string().unwrap_or_default()));
    }
    if d.is_array() {
        let arr = d.clone().into_array().map_err(|_| "expected an array".to_string())?;
        let nums = arr.iter().map(dynamic_to_num).collect::<Result<Vec<f64>, String>>()?;
        return match nums.as_slice() {
            [x, y] => Ok(ExprValue::Vec2(kurbo::Vec2::new(*x, *y))),
            [r, g, b] => Ok(ExprValue::Color(Color::rgb(*r, *g, *b))),
            [r, g, b, a] => Ok(ExprValue::Color(Color::rgba(*r, *g, *b, *a))),
            _ => Err("array must have 2 (vec), 3 or 4 (color) numbers".into()),
        };
    }
    Err("script must return a number, a string, or an array".into())
}

fn dynamic_to_num(d: &rhai::Dynamic) -> Result<f64, String> {
    if let Ok(f) = d.as_float() {
        Ok(f)
    } else if let Ok(i) = d.as_int() {
        Ok(i as f64)
    } else {
        Err("array element is not a number".into())
    }
}

/// What a reference points at on a node: a built-in property, or a user-defined
/// parameter (by name).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Target {
    Prop(PropPath),
    Param(String),
}

/// A cache key: a reference at an exact frame. The frame is in the key
/// (`to_bits`) so an off-time sample can't poison the primary value's memo slot.
type PropKey = (NodeId, Target, u64);

/// Per-evaluation memoization + cycle detection for expression resolution.
///
/// The dependency graph is never built explicitly: a pull-based DFS resolves a
/// dependency before its dependent because it recurses into it first. `visiting`
/// catches a back-edge (a cycle); `memo` collapses a diamond (two dependents
/// sharing one dependency) to a single resolve.
#[derive(Default)]
struct ResolveCache {
    visiting: HashSet<PropKey>,
    memo: HashMap<PropKey, ExprValue>,
}

/// The context a [`crate::value::Value`] resolves against.
///
/// Carries the frame, the document (so an expression can reach another node's
/// properties), a resolve cache, and a warnings sink. `resolve` takes it by
/// `&mut` because resolving an expression mutates the cache. Built once per
/// evaluate and shared down the whole walk.
pub struct EvalCtx<'a> {
    /// The frame to resolve at, in the **current layer's local time**.
    /// Fractional on purpose — keys sit on the grid, the playhead need not.
    /// Equal to `comp_frame` for a layer with no [`crate::node::LayerTiming`],
    /// which is every layer until one is given a time range.
    pub frame: f64,
    /// The composition's own frame — the playhead, unshifted by any layer's
    /// timing. Layer-local time is derived from this; expressions that want
    /// comp time (and the timeline UI) read it directly.
    pub comp_frame: f64,
    /// Project-wide modules, if this evaluation has a project behind it. `None`
    /// for a bare single-comp evaluate, where a link warns like a precomp does.
    pub modules: Option<&'a std::collections::BTreeMap<ModuleId, Module>>,
    /// Project-wide footage, when this evaluation has a project behind it.
    /// `None` for a bare single-comp evaluate, where an image layer warns like
    /// a precomp does rather than pretending to resolve.
    pub assets:
        Option<&'a std::collections::BTreeMap<crate::asset::AssetId, crate::asset::Asset>>,
    /// Modules currently being resolved further up, so a module that links
    /// itself warns instead of recursing forever — the same discipline as the
    /// property cycle guard and the comp one.
    module_stack: Vec<ModuleId>,
    /// The knob values of the module invocation being resolved, if any. A
    /// `param("x")` inside a module body reads this rather than any node's
    /// parameters — which is what makes a module a closed, reusable thing.
    module_scope: Vec<Vec<(String, ExprValue)>>,
    /// The timing of the layer currently resolving, so [`Expr::Time`] can read
    /// its in/out points. `None` = the layer has no range, which reads as a
    /// clip spanning the whole composition. Saved and restored by `walk`
    /// alongside `frame`, for the same reason.
    pub timing: Option<crate::node::LayerTiming>,
    /// The document, for cross-property references. `None` for a context that
    /// only samples constants/keyframes (a value with no expressions) — a
    /// `Ref` then resolves to its neutral fallback.
    doc: Option<&'a Document>,
    cache: ResolveCache,
    warnings: Vec<(NodeId, String)>,
    /// Whose property is being resolved right now, so a warning raised deep in
    /// a tree-walk (a bad script, an ambiguous name) can be attributed to the
    /// node that owns it. `None` outside a walk.
    current: Option<NodeId>,
}

impl<'a> EvalCtx<'a> {
    /// A context over `doc` at `frame` — the one `evaluate` uses.
    pub fn new(doc: &'a Document, frame: f64) -> Self {
        Self {
            frame,
            comp_frame: frame,
            timing: None,
            modules: None,
            assets: None,
            module_stack: Vec::new(),
            module_scope: Vec::new(),
            doc: Some(doc),
            cache: ResolveCache::default(),
            warnings: Vec::new(),
            current: None,
        }
    }

    /// A document-less context at `frame`, for resolving values that don't
    /// reference the scene (constants and keyframe tracks). Expression
    /// references fall back to a neutral value.
    pub fn at(frame: f64) -> Self {
        Self {
            frame,
            comp_frame: frame,
            timing: None,
            modules: None,
            assets: None,
            module_stack: Vec::new(),
            module_scope: Vec::new(),
            doc: None,
            cache: ResolveCache::default(),
            warnings: Vec::new(),
            current: None,
        }
    }

    /// Look up a piece of footage. `None` when this evaluation has no project
    /// behind it, or when the asset was removed while a layer still showed it.
    pub fn asset(&self, id: crate::asset::AssetId) -> Option<&crate::asset::Asset> {
        self.assets?.get(&id)
    }

    /// The rate of the comp being evaluated — what a source frame rate is
    /// converted *against*.
    ///
    /// Falls back to the source's own rate meaning "no conversion" by
    /// returning `0.0`, which every caller reads as "unknown"; guessing a rate
    /// here would silently retime footage in a document-less context.
    pub fn comp_fps(&self) -> f64 {
        self.doc.map(|d| d.fps).unwrap_or(0.0)
    }

    /// The current layer's `[in, out)` window **in local frames** — the domain
    /// its keyframes and expressions are authored in.
    ///
    /// A layer with no timing falls back to `[0, comp duration)`: local time is
    /// comp time for it, so it reads as one clip spanning the composition and
    /// `t01` stays meaningful. With no document either (a bare `EvalCtx::at`),
    /// the window is empty and `t01` is 0 — there is nothing to be a fraction of.
    pub fn local_window(&self) -> (f64, f64) {
        match self.timing {
            Some(t) => ((t.in_ - t.start) as f64, (t.out - t.start) as f64),
            None => (0.0, self.doc.map_or(0.0, |d| d.duration_frames() as f64)),
        }
    }

    /// Read one of the layer-clock sources. `T01` clamps, and a zero-length
    /// window reads 0 rather than dividing by zero.
    pub fn time_source(&self, which: TimeSource) -> f64 {
        let (in_, out) = self.local_window();
        match which {
            TimeSource::Local => self.frame,
            TimeSource::In => in_,
            TimeSource::Out => out,
            TimeSource::T01 => {
                if out <= in_ {
                    0.0
                } else {
                    ((self.frame - in_) / (out - in_)).clamp(0.0, 1.0)
                }
            }
        }
    }

    /// Take the warnings gathered while resolving (cycles, dangling refs), so
    /// `evaluate` can fold them into the `Scene`'s provenance-tagged list.
    pub fn take_warnings(&mut self) -> Vec<(NodeId, String)> {
        std::mem::take(&mut self.warnings)
    }

    /// Find a node by name, for the script-facing `value("A", …)`. Names aren't
    /// unique in the model, so this takes the first match in tree order — the
    /// same rule the layers panel shows top-down.
    fn find_named(&mut self, name: &str) -> Option<NodeId> {
        let doc = self.doc?;
        let mut matches = Vec::new();
        collect_named(&doc.root, name, &mut matches);
        match matches.as_slice() {
            [] => None,
            [only] => Some(*only),
            [first, ..] => {
                // Silently picking one of several would make a script's meaning
                // depend on tree order — surface it instead.
                self.warn_here(format!(
                    "{} nodes are named '{name}'; a script reference takes the first",
                    matches.len()
                ));
                Some(*first)
            }
        }
    }

    /// Resolve something in the context of `node`: warnings are attributed to
    /// it, and a `param("x")` with no explicit owner reads *its* parameters.
    ///
    /// Anything resolving a node's properties outside [`crate::evaluate`] — a
    /// panel showing one node's values, say — must go through this, or a
    /// node-relative `param()` has no owner to look in (it warns and falls
    /// back rather than guessing).
    pub fn in_node<R>(&mut self, node: NodeId, f: impl FnOnce(&mut Self) -> R) -> R {
        let prev = self.enter_node(node);
        let out = f(self);
        self.exit_node(prev);
        out
    }

    /// Mark `node` as the one being resolved, returning the previous mark to
    /// hand back to [`EvalCtx::exit_node`]. The walk brackets each node with
    /// this so warnings land on the right layer.
    pub fn enter_node(&mut self, node: NodeId) -> Option<NodeId> {
        self.current.replace(node)
    }

    /// Restore the mark [`EvalCtx::enter_node`] returned.
    pub fn exit_node(&mut self, prev: Option<NodeId>) {
        self.current = prev;
    }

    /// Warn against whichever node is being resolved. Falls back to the root
    /// (id 0) outside a walk — a warning with no home is still worth surfacing.
    ///
    /// `pub(crate)` so a *shape* can warn too, not just an expression: a text
    /// layer naming a font this machine hasn't got resolves to the default and
    /// says so here, which is the same "fall back to something drawable, but
    /// make it visible" rule a dangling `Ref` follows.
    pub(crate) fn warn_here(&mut self, msg: impl Into<String>) {
        let node = self.current.unwrap_or(NodeId(0));
        self.warn(node, msg);
    }

    fn warn(&mut self, node: NodeId, msg: impl Into<String>) {
        self.warnings.push((node, msg.into()));
    }

    /// Resolve `prop` on `node` at `frame`, dynamically. This is the one place
    /// an expression reaches back into the document. Memoized and cycle-guarded.
    fn resolve_prop(&mut self, node: NodeId, prop: PropPath, frame: f64) -> ExprValue {
        self.resolve_target(node, Target::Prop(prop), frame)
    }

    /// Resolve a parameter by name on `node`. Same memo + cycle guard as a
    /// property: a parameter can be expression-driven, so it can loop.
    fn resolve_param(&mut self, node: NodeId, name: &str, frame: f64) -> Option<ExprValue> {
        // Check it exists first — a missing parameter is a script error worth
        // reporting, not a silent zero.
        let target = self.doc?.root.find(node)?;
        target.param(name)?;
        Some(self.resolve_target(node, Target::Param(name.to_string()), frame))
    }

    fn resolve_target(&mut self, node: NodeId, target: Target, frame: f64) -> ExprValue {
        let zero = match &target {
            Target::Prop(p) => p.zero(),
            // A parameter's neutral value isn't knowable without finding it,
            // and the only callers that reach the failure paths below have
            // already established it exists.
            Target::Param(_) => ExprValue::Num(0.0),
        };
        let key = (node, target.clone(), frame.to_bits());
        if let Some(v) = self.cache.memo.get(&key) {
            return v.clone();
        }
        if self.cache.visiting.contains(&key) {
            let what = match &target {
                Target::Prop(p) => format!("{p:?}"),
                Target::Param(n) => format!("param '{n}'"),
            };
            self.warn(node, format!("expression cycle through {what} on node {}", node.0));
            return zero;
        }
        // Copy the document reference out (it's `&'a`, independent of `self`) so
        // the recursive resolve below can borrow `self` mutably for the cache.
        let Some(doc) = self.doc else {
            return zero;
        };
        let Some(found) = doc.root.find(node) else {
            self.warn(node, format!("expression references missing node {}", node.0));
            return zero;
        };

        self.cache.visiting.insert(key.clone());
        let saved = self.frame;
        self.frame = frame;
        // Warnings raised while resolving the *target* belong to the target.
        let prev = self.enter_node(node);
        let value = match &target {
            Target::Prop(prop) => self.resolve_on(found, *prop),
            Target::Param(name) => match found.param(name) {
                Some(p) => p.value.resolve(self),
                None => zero,
            },
        };
        self.exit_node(prev);
        self.frame = saved;
        self.cache.visiting.remove(&key);
        self.cache.memo.insert(key, value.clone());
        value
    }

    /// Resolve a single property of an already-found node to an [`ExprValue`].
    /// The property's own `resolve` runs against `self`, so a referenced value
    /// that is itself an expression recurses here (guarded by `visiting`).
    fn resolve_on(&mut self, node: &crate::node::Node, prop: PropPath) -> ExprValue {
        let tr = &node.transform;
        match prop {
            PropPath::Position => tr.position.resolve(self).to_expr(),
            PropPath::Rotation => tr.rotation_deg.resolve(self).to_expr(),
            PropPath::Scale => tr.scale.resolve(self).to_expr(),
            PropPath::Opacity => tr.opacity.resolve(self).to_expr(),
            PropPath::Anchor => tr.anchor.resolve(self).to_expr(),
            PropPath::Fill => match &node.fill {
                Some(fill) => fill.resolve(self).to_expr(),
                None => prop.zero(),
            },
            // A node without a stroke, or a shape without this param, resolves
            // neutral rather than erroring — same rule as a dangling `Ref`.
            PropPath::StrokeColor => match &node.stroke {
                Some(stroke) => stroke.color.resolve(self).to_expr(),
                None => prop.zero(),
            },
            PropPath::StrokeWidth => match &node.stroke {
                Some(stroke) => stroke.width.resolve(self).to_expr(),
                None => prop.zero(),
            },
            PropPath::ShapeSize => match &node.shape {
                Some(Shape::Rect { size, .. }) | Some(Shape::Ellipse { size }) => {
                    size.resolve(self).to_expr()
                }
                _ => prop.zero(),
            },
            PropPath::ShapeRadius => match &node.shape {
                Some(Shape::Rect { radius, .. }) => radius.resolve(self).to_expr(),
                _ => prop.zero(),
            },
            PropPath::TextSize => match &node.shape {
                Some(Shape::Text { size, .. }) => size.resolve(self).to_expr(),
                _ => prop.zero(),
            },
            PropPath::TextContent => match &node.shape {
                Some(Shape::Text { content, .. }) => content.resolve(self).to_expr(),
                _ => prop.zero(),
            },
            PropPath::TimeRemap => match &node.shape {
                Some(Shape::Image { time_remap: Some(t), .. }) => t.resolve(self).to_expr(),
                _ => prop.zero(),
            },
            PropPath::MaskSize => match node.mask.as_ref().map(|m| &m.shape) {
                Some(Shape::Rect { size, .. }) | Some(Shape::Ellipse { size }) => {
                    size.resolve(self).to_expr()
                }
                _ => prop.zero(),
            },
        }
    }
}

/// Every node named `name`, in tree order. Names aren't unique in the model, so
/// a lookup has to know whether it's ambiguous rather than just taking one.
fn collect_named(node: &crate::node::Node, name: &str, out: &mut Vec<NodeId>) {
    if node.name == name {
        out.push(node.id);
    }
    for c in &node.children {
        collect_named(c, name, out);
    }
}

/// Evaluate an expression against `ctx` into a dynamic [`ExprValue`]. The
/// property's `resolve` converts the result back to its concrete `T`.
pub fn eval_expr(expr: &Expr, ctx: &mut EvalCtx) -> ExprValue {
    match expr {
        Expr::Lit(v) => v.clone(),
        Expr::Ref { node, prop, time_offset } => {
            let frame = ctx.frame + time_offset;
            ctx.resolve_prop(*node, *prop, frame)
        }
        Expr::Bin { op, a, b } => {
            let op = *op;
            let (a, b) = (eval_expr(a, ctx), eval_expr(b, ctx));
            // `+` on text is concatenation, and it's contagious: if *either*
            // side is a string the whole sum is one, so `"take " + n` reads the
            // way it does in every scripting language. `zip` can't express this
            // — it only knows how to combine two numbers component-wise.
            //
            // **Add only.** Every other operator on a string falls through to
            // `zip`, which leaves the left side untouched — subtracting from
            // text has no meaning to guess at, and inventing one would make the
            // strictness the rest of the IR promises a lie.
            match (op, &a, &b) {
                (BinOp::Add, ExprValue::Str(_), _) | (BinOp::Add, _, ExprValue::Str(_)) => {
                    ExprValue::Str(format!("{}{}", a.to_str(), b.to_str()))
                }
                _ => a.zip(b, move |x, y| op.apply(x, y)),
            }
        }
        Expr::Un { op, a } => {
            let op = *op;
            eval_expr(a, ctx).map(move |x| op.apply(x))
        }
        // A parameter reads off its owning node — `None` meaning "the node
        // being resolved", which `walk` and `resolve_target` keep current.
        Expr::Param { node, name } => {
            // Inside a module body, `param("x")` means *the module's* knob. An
            // explicit node still wins, so a module can deliberately reach a
            // named node, but the bare form stays closed over the module.
            if node.is_none() {
                if let Some(scope) = ctx.module_scope.last() {
                    return match scope.iter().find(|(n, _)| n == name) {
                        Some((_, v)) => v.clone(),
                        None => {
                            ctx.warn_here(format!("module has no parameter named '{name}'"));
                            ExprValue::Num(0.0)
                        }
                    };
                }
            }
            let owner = node.or(ctx.current);
            let Some(owner) = owner else {
                ctx.warn_here(format!("param '{name}' has no owning node"));
                return ExprValue::Num(0.0);
            };
            let frame = ctx.frame;
            match ctx.resolve_param(owner, name, frame) {
                Some(v) => v,
                None => {
                    ctx.warn_here(format!("no parameter named '{name}' on node {}", owner.0));
                    ExprValue::Num(0.0)
                }
            }
        }
        // A bad script (compile/run/type error) falls back to a neutral value
        // rather than breaking the frame. The error also becomes a scene
        // warning, so a broken script is visible without opening the graph
        // panel — the fallback is a real value and would otherwise look
        // deliberate.
        Expr::Script(src) => match eval_script_ctx(src, ctx) {
            Ok(v) => v,
            Err(e) => {
                let msg = e.lines().next().unwrap_or("error").to_string();
                ctx.warn_here(format!("script: {msg}"));
                ExprValue::Num(0.0)
            }
        },
        // Linking a shared module. Overrides are evaluated *here*, in the
        // linking property's own scope, so a link can feed the module its own
        // `t01` or node params; the body then sees plain knob values.
        Expr::Use { module, overrides } => eval_use(*module, overrides, ctx),
        // Like a generator, the layer clock is always a scalar.
        Expr::Time(which) => ExprValue::Num(ctx.time_source(*which)),
        // A generator always produces a scalar; a vec/colour property broadcasts
        // it through the same `Num` edge a literal number would.
        Expr::Gen(g) => ExprValue::Num(g.eval(ctx)),
        // Two scalars up into a vector, or one axis down out of one — the join /
        // split pair. Both coerce, so a mismatched wire lowers dimensionality
        // gracefully rather than failing a frame.
        Expr::Vec2 { x, y } => {
            let (x, y) = (as_scalar(eval_expr(x, ctx)), as_scalar(eval_expr(y, ctx)));
            ExprValue::Vec2(kurbo::Vec2::new(x, y))
        }
        Expr::Comp { a, axis } => ExprValue::Num(component(&eval_expr(a, ctx), *axis)),
    }
}

/// Coerce an [`ExprValue`] to a scalar — the reading `Vec2`'s operands and the
/// generator knobs share: a number is itself, a vec/colour gives its first
/// component, a string has no number and gives zero.
fn as_scalar(v: ExprValue) -> f64 {
    match v {
        ExprValue::Num(n) => n,
        ExprValue::Vec2(p) => p.x,
        ExprValue::Color(c) => c.r,
        ExprValue::Str(_) => 0.0,
    }
}

/// One axis of a value, for [`Expr::Comp`]. A vector gives x/y, a colour reads
/// r/g, and a scalar has no axes so it passes through. Total and non-failing,
/// like every other coercion here.
fn component(v: &ExprValue, axis: Axis) -> f64 {
    match (v, axis) {
        (ExprValue::Vec2(p), Axis::X) => p.x,
        (ExprValue::Vec2(p), Axis::Y) => p.y,
        (ExprValue::Color(c), Axis::X) => c.r,
        (ExprValue::Color(c), Axis::Y) => c.g,
        (ExprValue::Num(n), _) => *n,
        (ExprValue::Str(_), _) => 0.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::{Document, Node, Transform};
    use crate::value::{Keyframe, Track, Value};
    use kurbo::Vec2;

    /// Two nodes under a root: `a` (id 1) with the given opacity, `b` (id 2)
    /// whose opacity is an expression. Returns the doc so a test can resolve
    /// `b`'s opacity and see what the expression computed.
    fn doc_with(a_opacity: Value<f64>, b_opacity: Value<f64>) -> Document {
        let a = Node::group(1, "a")
            .with_transform(Transform { opacity: a_opacity, ..Transform::default() });
        let b = Node::group(2, "b")
            .with_transform(Transform { opacity: b_opacity, ..Transform::default() });
        Document::new(100.0, 100.0, Node::group(0, "root").with_child(a).with_child(b))
    }

    fn opacity_of(doc: &Document, id: u64, frame: f64) -> f64 {
        let node = doc.root.find(NodeId(id)).unwrap();
        let mut ctx = EvalCtx::new(doc, frame);
        // `in_node` is what `evaluate`'s walk does; a node-relative `param()`
        // needs it to know whose knobs to read.
        ctx.in_node(node.id, |ctx| node.transform.opacity.resolve(ctx))
    }

    #[test]
    fn a_literal_resolves_to_itself() {
        let v: Value<f64> = Value::expr(Expr::num(0.7));
        let mut ctx = EvalCtx::at(0.0);
        assert_eq!(v.resolve(&mut ctx), 0.7);
    }

    #[test]
    fn a_reference_reads_another_nodes_property() {
        // b.opacity = a.opacity, and a.opacity is 0.5.
        let doc = doc_with(
            Value::constant(0.5),
            Value::expr(Expr::reference(NodeId(1), PropPath::Opacity)),
        );
        assert_eq!(opacity_of(&doc, 2, 0.0), 0.5);
    }

    #[test]
    fn a_type_mismatch_falls_back_rather_than_failing() {
        // b.opacity references a *Vec2* (position) — a colour/vec can't become a
        // scalar, so it falls back to 0.0 instead of poisoning the frame.
        let a = Node::group(1, "a").with_transform(Transform {
            position: Value::constant(Vec2::new(10.0, 20.0)),
            ..Transform::default()
        });
        let b = Node::group(2, "b").with_transform(Transform {
            opacity: Value::expr(Expr::reference(NodeId(1), PropPath::Position)),
            ..Transform::default()
        });
        let doc = Document::new(100.0, 100.0, Node::group(0, "r").with_child(a).with_child(b));
        assert_eq!(opacity_of(&doc, 2, 0.0), 0.0);
    }

    #[test]
    fn a_time_offset_samples_the_reference_at_another_frame() {
        // a.opacity ramps 0 -> 1 over frames 0..10; b reads a at +10 frames, so
        // at frame 0 b sees a's value at frame 10, i.e. 1.0.
        let a_track = Value::Keyframed(Track::new(vec![
            Keyframe::linear(0, 0.0),
            Keyframe::linear(10, 1.0),
        ]));
        let doc = doc_with(
            a_track,
            Value::expr(Expr::reference_at(NodeId(1), PropPath::Opacity, 10.0)),
        );
        assert!((opacity_of(&doc, 2, 0.0) - 1.0).abs() < 1e-9);
        // And with no offset it tracks a live: at frame 5, a is 0.5.
        let doc2 = doc_with(
            Value::Keyframed(Track::new(vec![Keyframe::linear(0, 0.0), Keyframe::linear(10, 1.0)])),
            Value::expr(Expr::reference(NodeId(1), PropPath::Opacity)),
        );
        assert!((opacity_of(&doc2, 2, 5.0) - 0.5).abs() < 1e-9);
    }

    /// Every operator, at one worked value each — the table that says what this
    /// node actually computes, in one place.
    #[test]
    fn every_operator_computes_what_it_says() {
        let at = |e: Expr| {
            let v: Value<f64> = Value::expr(e);
            v.resolve(&mut EvalCtx::at(0.0))
        };
        let bin = |op, x: f64, y: f64| at(Expr::bin(op, Expr::num(x), Expr::num(y)));
        assert_eq!(bin(BinOp::Add, 2.0, 3.0), 5.0);
        assert_eq!(bin(BinOp::Sub, 2.0, 3.0), -1.0);
        assert_eq!(bin(BinOp::Mul, 2.0, 3.0), 6.0);
        assert_eq!(bin(BinOp::Div, 6.0, 3.0), 2.0);
        assert_eq!(bin(BinOp::Pow, 2.0, 3.0), 8.0);
        assert_eq!(bin(BinOp::Min, 2.0, 3.0), 2.0);
        assert_eq!(bin(BinOp::Max, 2.0, 3.0), 3.0);
        assert_eq!(bin(BinOp::Mod, 7.0, 4.0), 3.0);
        // Degrees, not radians — straight up is 90, ready to drive `rotation`.
        assert_eq!(bin(BinOp::Atan2, 1.0, 0.0), 90.0);

        let un = |op, x: f64| at(Expr::un(op, Expr::num(x)));
        assert_eq!(un(UnOp::Neg, 3.0), -3.0);
        assert_eq!(un(UnOp::Abs, -3.0), 3.0);
        assert_eq!(un(UnOp::Sqrt, 9.0), 3.0);
        assert_eq!(un(UnOp::Floor, 2.7), 2.0);
        assert_eq!(un(UnOp::Round, 2.7), 3.0);
        // Degrees here too, for the same reason.
        assert_eq!(un(UnOp::Sin, 90.0), 1.0);
        assert_eq!(un(UnOp::Cos, 0.0), 1.0);
    }

    /// **No operator may produce a NaN or an infinity.** A NaN reaching a
    /// transform blanks the layer with no clue why — the least debuggable
    /// failure this engine has — so every ill-defined case resolves to zero
    /// instead, the same warn-don't-fail contract a dangling reference follows.
    #[test]
    fn arithmetic_never_produces_a_nan_or_an_infinity() {
        let at = |e: Expr| {
            let v: Value<f64> = Value::expr(e);
            v.resolve(&mut EvalCtx::at(0.0))
        };
        let bin = |op, x: f64, y: f64| at(Expr::bin(op, Expr::num(x), Expr::num(y)));
        assert_eq!(bin(BinOp::Div, 1.0, 0.0), 0.0, "divide by zero");
        assert_eq!(bin(BinOp::Div, 0.0, 0.0), 0.0, "zero over zero");
        assert_eq!(bin(BinOp::Mod, 1.0, 0.0), 0.0, "modulo by zero");
        assert_eq!(bin(BinOp::Pow, -8.0, 0.5), 0.0, "fractional power of a negative");
        assert_eq!(at(Expr::un(UnOp::Sqrt, Expr::num(-9.0))), 0.0, "root of a negative");
    }

    /// The operand a fresh operator rests at is its **identity** where it has
    /// one, so an unwired node passes its input through rather than annihilating
    /// it. A Multiply seeded at 0 would silently zero whatever you wired in.
    #[test]
    fn an_operator_rests_at_its_identity() {
        assert_eq!(BinOp::Add.seed_operands(), (0.0, 0.0));
        assert_eq!(BinOp::Sub.seed_operands(), (0.0, 0.0));
        assert_eq!(BinOp::Mul.seed_operands(), (1.0, 1.0));
        assert_eq!(BinOp::Div.seed_operands(), (1.0, 1.0));
    }

    /// Operators broadcast a scalar across a vector or a colour, because
    /// `ExprValue::zip` already did — so every new operator gained that for
    /// free, and `position / 2` means what it looks like.
    #[test]
    fn an_operator_broadcasts_a_scalar_over_a_vector() {
        let e = Expr::bin(
            BinOp::Div,
            Expr::Lit(ExprValue::Vec2(Vec2::new(10.0, 20.0))),
            Expr::num(2.0),
        );
        let v: Value<Vec2> = Value::expr(e);
        assert_eq!(v.resolve(&mut EvalCtx::at(0.0)), Vec2::new(5.0, 10.0));
    }

    /// `+` concatenates when either side is text — but **only** `+`. Subtracting
    /// from a string has no meaning worth guessing at, and inventing one would
    /// make the strictness the rest of the IR promises a lie.
    #[test]
    fn only_add_concatenates_text() {
        let cat = |op| {
            let e = Expr::bin(op, Expr::Lit(ExprValue::Str("take ".into())), Expr::num(3.0));
            let v: Value<String> = Value::expr(e);
            v.resolve(&mut EvalCtx::at(0.0))
        };
        assert_eq!(cat(BinOp::Add), "take 3");
        assert_eq!(cat(BinOp::Mul), "take ", "a non-Add op leaves the text alone");
    }

    #[test]
    fn arithmetic_composes() {
        // 2 + 3 * 4 = 14.
        let e = Expr::bin(BinOp::Add,Expr::num(2.0),Expr::bin(BinOp::Mul, Expr::num(3.0), Expr::num(4.0)));
        let v: Value<f64> = Value::expr(e);
        let mut ctx = EvalCtx::at(0.0);
        assert_eq!(v.resolve(&mut ctx), 14.0);
    }

    #[test]
    fn a_cycle_is_broken_with_a_warning_not_a_hang() {
        // a.opacity = b.opacity and b.opacity = a.opacity.
        let doc = doc_with(
            Value::expr(Expr::reference(NodeId(2), PropPath::Opacity)),
            Value::expr(Expr::reference(NodeId(1), PropPath::Opacity)),
        );
        let node = doc.root.find(NodeId(1)).unwrap();
        let mut ctx = EvalCtx::new(&doc, 0.0);
        // Terminates (the test itself finishing is half the assertion) and yields
        // the neutral fallback.
        let v = node.transform.opacity.resolve(&mut ctx);
        assert_eq!(v, 0.0);
        assert!(!ctx.take_warnings().is_empty(), "the cycle should be reported");
    }

    #[test]
    fn an_expression_value_round_trips_through_json() {
        let v: Value<f64> =
            Value::expr(Expr::reference_at(NodeId(7), PropPath::Rotation, -3.0));
        let json = serde_json::to_string(&v).unwrap();
        let back: Value<f64> = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, Value::Expr(Expr::Ref { .. })));
    }

    /// A layer-clock leaf has to survive a `.pbc` round-trip like any other
    /// expression — it's a saved document's contents, not just a runtime value.
    #[test]
    fn a_time_source_round_trips_through_json() {
        for src in [TimeSource::Local, TimeSource::In, TimeSource::Out, TimeSource::T01] {
            let v: Value<f64> = Value::expr(Expr::Time(src));
            let json = serde_json::to_string(&v).unwrap();
            let back: Value<f64> = serde_json::from_str(&json).unwrap();
            match back {
                Value::Expr(Expr::Time(got)) => assert_eq!(got, src),
                other => panic!("{src:?} came back as {other:?}"),
            }
        }
    }

    #[test]
    fn seed_matches_its_kind_and_arity() {
        for k in ExprKind::ALL {
            let e = Expr::seed(k);
            assert_eq!(e.kind(), k);
            let expected = match k {
                ExprKind::Bin => 2,
                ExprKind::Un => 1,
                // Leaves: a literal, a lookup, a module link, or a reading of
                // the layer clock.
                ExprKind::Use
                | ExprKind::Lit
                | ExprKind::Ref
                | ExprKind::Param
                | ExprKind::Script
                | ExprKind::LocalTime
                | ExprKind::InPoint
                | ExprKind::OutPoint
                | ExprKind::T01 => 0,
                // Generators carry their own knob count — assert it's non-empty
                // and matches the labels, checked in full elsewhere.
                ExprKind::Oscillator | ExprKind::Noise | ExprKind::Ramp | ExprKind::Bounce => {
                    e.arity()
                }
                // Not in `ALL` (created by placing the graph node), but the match
                // must still cover them: a `join` takes two scalars, a `split` one
                // vector.
                ExprKind::Vec2 => 2,
                ExprKind::Comp => 1,
            };
            assert_eq!(e.arity(), expected, "{k:?}");
        }
    }

    #[test]
    fn at_mut_addresses_a_subtree_by_slot_path() {
        // (2 + (3 * 4)) — edit the 4 (path [1, 1]) into a 9.
        let mut e = Expr::bin(BinOp::Add,Expr::num(2.0),Expr::bin(BinOp::Mul, Expr::num(3.0), Expr::num(4.0)));
        *e.at_mut(&[1, 1]).unwrap() = Expr::num(9.0);
        assert_eq!(e.to_string(), "(2 + (3 * 9))");
        // A slot past the node's arity is None (Neg has only slot 0).
        assert!(Expr::seed(ExprKind::Un).at_mut(&[1]).is_none());
    }

    #[test]
    fn a_links_overrides_are_addressable_children() {
        // A link with two overrides has arity 2, its children are those override
        // expressions in order, and `at_mut` reaches into one to edit it — which
        // is what lets the graph canvas render and edit overrides as wired boxes.
        let mut e = Expr::Use {
            module: ModuleId(3),
            overrides: vec![
                ("amount".into(), Expr::num(0.2)),
                ("speed".into(), Expr::num(1.0)),
            ],
        };
        assert_eq!(e.arity(), 2);
        assert!(matches!(e.child(0), Some(Expr::Lit(ExprValue::Num(n))) if *n == 0.2));
        assert!(matches!(e.child(1), Some(Expr::Lit(ExprValue::Num(n))) if *n == 1.0));
        assert!(e.child(2).is_none());

        // Edit the second override into a reference; the first is untouched.
        *e.at_mut(&[1]).unwrap() = Expr::reference(NodeId(7), PropPath::Opacity);
        match &e {
            Expr::Use { overrides, .. } => {
                assert!(matches!(overrides[0].1, Expr::Lit(_)));
                assert_eq!(overrides[0].0, "amount");
                assert!(matches!(overrides[1].1, Expr::Ref { .. }));
                assert_eq!(overrides[1].0, "speed", "the knob name is kept");
            }
            _ => unreachable!(),
        }

        // A link with no overrides is still a leaf.
        assert_eq!(Expr::Use { module: ModuleId(0), overrides: vec![] }.arity(), 0);
    }

    #[test]
    fn display_prints_the_tree_readably() {
        let e = Expr::bin(BinOp::Add,Expr::num(2.0),Expr::bin(BinOp::Mul, Expr::num(3.0), Expr::num(4.0)));
        assert_eq!(e.to_string(), "(2 + (3 * 4))");
        assert_eq!(Expr::reference(NodeId(1), PropPath::Position).to_string(), "@1.Position");
        assert_eq!(
            Expr::reference_at(NodeId(2), PropPath::Rotation, 5.0).to_string(),
            "@2.Rotation[+5]"
        );
    }

    #[test]
    fn a_script_evaluates_against_the_frame() {
        // Number result → Num, read from `frame`.
        let v: Value<f64> = Value::expr(Expr::Script("frame * 2.0".into()));
        let mut ctx = EvalCtx::at(5.0);
        assert_eq!(v.resolve(&mut ctx), 10.0);
        // `time` is an alias for `frame`.
        assert_eq!(eval_script("time + 1", 4.0).unwrap(), ExprValue::Num(5.0));
    }

    #[test]
    fn a_script_array_becomes_a_vec_or_color() {
        assert_eq!(
            eval_script("[frame, frame * 2.0]", 3.0).unwrap(),
            ExprValue::Vec2(kurbo::Vec2::new(3.0, 6.0))
        );
        assert_eq!(
            eval_script("[1.0, 0.5, 0.0]", 0.0).unwrap(),
            ExprValue::Color(Color::rgb(1.0, 0.5, 0.0))
        );
    }

    #[test]
    fn a_bad_script_errors_but_does_not_break_the_frame() {
        // A syntax error is reported by eval_script...
        assert!(eval_script("frame *", 0.0).is_err());
        // ...and resolves to the neutral fallback rather than panicking.
        let v: Value<f64> = Value::expr(Expr::Script("this is not rhai".into()));
        let mut ctx = EvalCtx::at(0.0);
        assert_eq!(v.resolve(&mut ctx), 0.0);
        // A string is *not* an error any more — it's `ExprValue::Str`, which is
        // what a typewriter script returns. It only becomes neutral at the
        // property edge, where a scalar can't accept it.
        assert_eq!(eval_script("\"hello\"", 0.0).unwrap(), ExprValue::Str("hello".into()));
        let v: Value<f64> = Value::expr(Expr::Script("\"hello\"".into()));
        assert_eq!(v.resolve(&mut ctx), 0.0, "a string feeding a scalar falls back");
        // A type with no reading at all still errors.
        assert!(eval_script("true", 0.0).is_err());
    }

    #[test]
    fn promote_seeds_with_the_current_value_then_bake_freezes_it() {
        let mut v: Value<f64> = Value::constant(0.5);
        let mut ctx = EvalCtx::at(0.0);
        v.promote_to_expr(&mut ctx);
        assert!(v.is_expr(), "now an expression");
        assert_eq!(v.resolve(&mut ctx), 0.5, "seeded with the old value, unchanged");
        // Edit the literal, then bake: the constant freezes the resolved value.
        *v.expr_mut().unwrap() = Expr::num(0.9);
        v.bake_to_const(&mut ctx);
        assert!(matches!(v, Value::Const(_)));
        assert_eq!(v.resolve(&mut ctx), 0.9);
    }

    // ---- the scripting bridge: value() / value_at() / wiggle() ----

    #[test]
    fn a_script_reads_another_node_by_name() {
        let doc = doc_with(
            Value::constant(0.5),
            Value::expr(Expr::Script("value(\"a\", \"opacity\") * 2.0".into())),
        );
        assert_eq!(opacity_of(&doc, 2, 0.0), 1.0);
    }

    #[test]
    fn a_script_reads_a_vec_property_as_an_array() {
        let a = Node::group(1, "a").with_transform(Transform {
            position: Value::constant(Vec2::new(30.0, 4.0)),
            ..Transform::default()
        });
        // Take the x of a's position: an array subscript, straight from Rhai.
        let b = Node::group(2, "b").with_transform(Transform {
            opacity: Value::expr(Expr::Script("value(\"a\", \"position\")[0]".into())),
            ..Transform::default()
        });
        let doc =
            Document::new(100.0, 100.0, Node::group(0, "root").with_child(a).with_child(b));
        assert_eq!(opacity_of(&doc, 2, 0.0), 30.0);
    }

    #[test]
    fn value_at_samples_an_animated_property_off_time() {
        // a.opacity ramps 0 → 1 over frames 0..10; b reads it 10 frames late.
        let track = Track::new(vec![Keyframe::linear(0, 0.0), Keyframe::linear(10, 1.0)]);
        let doc = doc_with(
            Value::Keyframed(track),
            Value::expr(Expr::Script("value_at(\"a\", \"opacity\", frame - 10.0)".into())),
        );
        assert_eq!(opacity_of(&doc, 2, 15.0), 0.5, "frame 15 reads a at frame 5");
    }

    #[test]
    fn a_script_referencing_itself_is_caught_as_a_cycle() {
        // b.opacity = value("b", "opacity") — the cycle guard must stop this
        // rather than recursing until the stack goes.
        let doc = doc_with(
            Value::constant(0.5),
            Value::expr(Expr::Script("value(\"b\", \"opacity\")".into())),
        );
        assert_eq!(opacity_of(&doc, 2, 0.0), 0.0, "falls back to neutral");
    }

    #[test]
    fn a_script_can_reference_a_node_that_is_itself_a_script() {
        // Nested bridge entry: b's script resolves a, whose value is a script.
        let a = Node::group(1, "a").with_transform(Transform {
            opacity: Value::expr(Expr::Script("frame * 0.1".into())),
            ..Transform::default()
        });
        let b = Node::group(2, "b").with_transform(Transform {
            opacity: Value::expr(Expr::Script("value(\"a\", \"opacity\") + 1.0".into())),
            ..Transform::default()
        });
        let doc =
            Document::new(100.0, 100.0, Node::group(0, "root").with_child(a).with_child(b));
        assert_eq!(opacity_of(&doc, 2, 20.0), 3.0);
    }

    #[test]
    fn a_bad_name_or_property_is_a_script_error() {
        let doc = doc_with(Value::constant(0.5), Value::constant(0.0));
        let mut ctx = EvalCtx::new(&doc, 0.0);
        let err = eval_script_ctx("value(\"nope\", \"opacity\")", &mut ctx).unwrap_err();
        assert!(err.contains("nope"), "names the missing node: {err}");
        let err = eval_script_ctx("value(\"a\", \"wobble\")", &mut ctx).unwrap_err();
        assert!(err.contains("wobble"), "names the bad property: {err}");
    }

    #[test]
    fn value_without_a_document_errors_rather_than_lying() {
        // A document-less context has no nodes at all, so the lookup fails and
        // the script errors — it never silently resolves to a neutral value.
        let err = eval_script("value(\"a\", \"opacity\")", 0.0).unwrap_err();
        assert!(err.contains("no node named"), "{err}");
    }

    #[test]
    fn a_script_reads_stroke_and_shape_params() {
        use crate::node::{Shape, Stroke};
        let a = Node::shape(
            1,
            "a",
            Shape::Rect {
                size: Value::constant(Vec2::new(200.0, 80.0)),
                radius: Value::constant(12.0),
            },
        )
        .with_stroke(Stroke {
            color: Value::constant(Color::rgb(1.0, 0.0, 0.0)),
            width: Value::constant(4.0),
        });
        let b = Node::group(2, "b").with_transform(Transform {
            opacity: Value::expr(Expr::Script(
                "value(\"a\", \"stroke_width\") + value(\"a\", \"radius\") \
                 + value(\"a\", \"size\")[1] + value(\"a\", \"stroke_color\")[0]"
                    .into(),
            )),
            ..Transform::default()
        });
        let doc =
            Document::new(100.0, 100.0, Node::group(0, "root").with_child(a).with_child(b));
        assert_eq!(opacity_of(&doc, 2, 0.0), 4.0 + 12.0 + 80.0 + 1.0);
    }

    #[test]
    fn a_missing_stroke_or_shape_param_resolves_neutral() {
        // `a` is a plain group: no stroke, no shape. Reading either is not an
        // error — it's the kind's neutral value, like a dangling `Ref`.
        let doc = doc_with(
            Value::constant(0.5),
            Value::expr(Expr::Script(
                "value(\"a\", \"stroke_width\") + value(\"a\", \"radius\") + 1.0".into(),
            )),
        );
        assert_eq!(opacity_of(&doc, 2, 0.0), 1.0);
    }

    #[test]
    fn an_ellipse_has_a_size_but_no_radius() {
        use crate::node::Shape;
        let a = Node::shape(1, "a", Shape::Ellipse { size: Value::constant(Vec2::new(9.0, 5.0)) });
        let b = Node::group(2, "b").with_transform(Transform {
            opacity: Value::expr(Expr::Script(
                "value(\"a\", \"size\")[0] + value(\"a\", \"radius\")".into(),
            )),
            ..Transform::default()
        });
        let doc =
            Document::new(100.0, 100.0, Node::group(0, "root").with_child(a).with_child(b));
        assert_eq!(opacity_of(&doc, 2, 0.0), 9.0, "size reads, radius is neutral");
    }

    #[test]
    fn every_property_name_round_trips_and_is_unique() {
        // `name`/`parse` are the script-facing vocabulary; a duplicate or a
        // name that won't parse back would silently shadow a property.
        let mut seen = HashSet::new();
        for p in PropPath::ALL {
            assert_eq!(PropPath::parse(p.name()), Some(p), "{p:?} round-trips");
            assert!(seen.insert(p.name()), "duplicate name {}", p.name());
        }
        assert_eq!(PropPath::parse("  POSITION "), Some(PropPath::Position), "trims + folds case");
        assert_eq!(PropPath::parse("nope"), None);
    }

    // ---- exposed parameters ----

    #[test]
    fn a_param_node_reads_its_own_nodes_knob() {
        use crate::node::ParamValue;
        // b.opacity = param("gain"), and b's `gain` is 0.75.
        let mut doc = doc_with(
            Value::constant(0.5),
            Value::expr(Expr::Param { node: None, name: "gain".into() }),
        );
        doc.root
            .find_mut(NodeId(2))
            .unwrap()
            .set_param("gain", ParamValue::Num(Value::constant(0.75)));
        assert_eq!(opacity_of(&doc, 2, 0.0), 0.75);
    }

    #[test]
    fn one_param_drives_several_properties() {
        use crate::node::ParamValue;
        // The point of parameters: one knob, many properties. `size` drives
        // both scale (a vec) and opacity (a scalar) — the param is a scalar,
        // and the vec property takes it broadcast through a Mul.
        use crate::node::{Shape, Transform as T};
        let mut n = Node::shape(
            1,
            "n",
            Shape::Rect {
                size: Value::constant(Vec2::new(10.0, 10.0)),
                radius: Value::constant(0.0),
            },
        )
        .with_transform(T {
            opacity: Value::expr(Expr::Param { node: None, name: "size".into() }),
            scale: Value::expr(Expr::bin(BinOp::Mul,Expr::Lit(ExprValue::Vec2(Vec2::new(1.0, 1.0))),Expr::Param { node: None, name: "size".into() })),
            ..T::default()
        });
        n.set_param("size", ParamValue::Num(Value::constant(0.4)));
        let doc = Document::new(100.0, 100.0, Node::group(0, "root").with_child(n));
        let node = doc.root.find(NodeId(1)).unwrap();
        let mut ctx = EvalCtx::new(&doc, 0.0);
        ctx.in_node(node.id, |ctx| {
            assert_eq!(node.transform.opacity.resolve(ctx), 0.4);
            assert_eq!(node.transform.scale.resolve(ctx), Vec2::new(0.4, 0.4));
        });
    }

    #[test]
    fn a_param_can_be_keyframed_like_any_value() {
        use crate::node::ParamValue;
        let track = Track::new(vec![Keyframe::linear(0, 0.0), Keyframe::linear(10, 1.0)]);
        let mut doc = doc_with(
            Value::constant(0.5),
            Value::expr(Expr::Param { node: None, name: "gain".into() }),
        );
        doc.root.find_mut(NodeId(2)).unwrap().set_param("gain", ParamValue::Num(Value::Keyframed(track)));
        assert_eq!(opacity_of(&doc, 2, 5.0), 0.5, "the knob animates");
    }

    #[test]
    fn a_script_reads_params_on_its_own_and_other_nodes() {
        use crate::node::ParamValue;
        let mut doc = doc_with(
            Value::constant(0.5),
            Value::expr(Expr::Script("param(\"mine\") + param_of(\"a\", \"theirs\")".into())),
        );
        doc.root
            .find_mut(NodeId(1))
            .unwrap()
            .set_param("theirs", ParamValue::Num(Value::constant(3.0)));
        doc.root
            .find_mut(NodeId(2))
            .unwrap()
            .set_param("mine", ParamValue::Num(Value::constant(4.0)));
        assert_eq!(opacity_of(&doc, 2, 0.0), 7.0);
    }

    #[test]
    fn a_missing_param_warns_instead_of_resolving_to_zero_silently() {
        let doc = doc_with(
            Value::constant(0.5),
            Value::expr(Expr::Param { node: None, name: "nope".into() }),
        );
        let node = doc.root.find(NodeId(2)).unwrap();
        let mut ctx = EvalCtx::new(&doc, 0.0);
        let v = ctx.in_node(node.id, |ctx| node.transform.opacity.resolve(ctx));
        assert_eq!(v, 0.0, "falls back");
        let warnings = ctx.take_warnings();
        assert!(warnings.iter().any(|(_, m)| m.contains("nope")), "{warnings:?}");
    }

    #[test]
    fn a_param_that_reads_itself_is_caught_as_a_cycle() {
        use crate::node::ParamValue;
        // `gain` is driven by param("gain") — the guard has to catch this the
        // same way it catches a property cycle, or the stack goes.
        let mut doc = doc_with(
            Value::constant(0.5),
            Value::expr(Expr::Param { node: None, name: "gain".into() }),
        );
        let b = doc.root.find_mut(NodeId(2)).unwrap();
        b.set_param(
            "gain",
            ParamValue::Num(Value::expr(Expr::Param { node: None, name: "gain".into() })),
        );
        assert_eq!(opacity_of(&doc, 2, 0.0), 0.0, "falls back rather than hanging");
    }

    #[test]
    fn setting_a_param_twice_replaces_rather_than_duplicates() {
        use crate::node::ParamValue;
        let mut n = Node::group(1, "n");
        n.set_param("x", ParamValue::Num(Value::constant(1.0)));
        n.set_param("x", ParamValue::Num(Value::constant(2.0)));
        assert_eq!(n.params.len(), 1, "a duplicate name would make param(\"x\") ambiguous");
        assert!(n.remove_param("x"));
        assert!(!n.remove_param("x"), "already gone");
    }

    #[test]
    fn wiggle_is_deterministic_bounded_and_seed_separable() {
        let at = |frame: f64, src: &str| {
            let mut ctx = EvalCtx::at(frame);
            match eval_script_ctx(src, &mut ctx).unwrap() {
                ExprValue::Num(n) => n,
                other => panic!("expected a number, got {other:?}"),
            }
        };
        // Same frame, same value — scrubbing back and forth is stable.
        assert_eq!(at(12.0, "wiggle(0.5, 10.0)"), at(12.0, "wiggle(0.5, 10.0)"));
        // Bounded by the amplitude, over a decent sweep.
        for f in 0..200 {
            let v = at(f as f64 * 0.37, "wiggle(0.5, 10.0)");
            assert!(v.abs() <= 10.0, "frame {f}: {v} out of amplitude");
        }
        // It actually moves.
        assert_ne!(at(0.0, "wiggle(0.5, 10.0)"), at(7.0, "wiggle(0.5, 10.0)"));
        // Different seeds are independent streams — x and y can differ.
        assert_ne!(at(7.0, "wiggle(0.5, 10.0)"), at(7.0, "wiggle(0.5, 10.0, 2.0)"));
    }

    // ---- procedural generators ----

    /// Resolve a bare generator expression at `frame`, document-less (its knobs
    /// are literals here), and unwrap the `Num`.
    fn gen_at(g: Generator, frame: f64) -> f64 {
        let mut ctx = EvalCtx::at(frame);
        match eval_expr(&Expr::Gen(g), &mut ctx) {
            ExprValue::Num(n) => n,
            other => panic!("a generator must resolve to a number, got {other:?}"),
        }
    }

    #[test]
    fn oscillator_is_a_sine_around_its_offset() {
        // offset 5, amp 2, freq 0.25 (a full cycle over 4 frames), no phase.
        let g = || Generator::Oscillator {
            freq: Box::new(Expr::num(0.25)),
            amp: Box::new(Expr::num(2.0)),
            phase: Box::new(Expr::num(0.0)),
            offset: Box::new(Expr::num(5.0)),
            wave: Waveform::Sine,
        };
        assert!((gen_at(g(), 0.0) - 5.0).abs() < 1e-9, "sin(0) = 0 → the offset");
        assert!((gen_at(g(), 1.0) - 7.0).abs() < 1e-9, "quarter cycle → +amp");
        assert!((gen_at(g(), 3.0) - 3.0).abs() < 1e-9, "three-quarter cycle → −amp");
    }

    #[test]
    fn every_waveform_stays_in_unit_range_and_hits_its_shape() {
        // Sampled densely, each wave is bounded by [-1, 1].
        for w in Waveform::ALL {
            for k in 0..400 {
                let v = w.sample(k as f64 * 0.013);
                assert!(v.abs() <= 1.0 + 1e-9, "{w:?} at {k}: {v}");
            }
        }
        // Signature points: saw ramps -1→1 across a cycle; square flips at 0.5;
        // triangle peaks at 0.5.
        assert!((Waveform::Saw.sample(0.0) + 1.0).abs() < 1e-9);
        assert!((Waveform::Saw.sample(0.999) - 0.998).abs() < 1e-2);
        assert_eq!(Waveform::Square.sample(0.25), 1.0);
        assert_eq!(Waveform::Square.sample(0.75), -1.0);
        assert!((Waveform::Triangle.sample(0.5) - 1.0).abs() < 1e-9);
        assert!((Waveform::Triangle.sample(0.0) + 1.0).abs() < 1e-9);
    }

    #[test]
    fn noise_matches_wiggle_and_is_bounded() {
        // The Noise generator is the same value noise `wiggle()` uses, so equal
        // freq/amp/seed give equal results — one source of truth.
        let freq = 0.5;
        let amp = 10.0;
        for f in 0..50 {
            let frame = f as f64 * 0.37;
            let g = Generator::Noise {
                freq: Box::new(Expr::num(freq)),
                amp: Box::new(Expr::num(amp)),
                seed: Box::new(Expr::num(0.0)),
            };
            let via_gen = gen_at(g, frame);
            let via_script = value_noise(frame * freq, 0.0) * amp;
            assert!((via_gen - via_script).abs() < 1e-12, "frame {frame}");
            assert!(via_gen.abs() <= amp, "bounded by amp");
        }
    }

    #[test]
    fn ramp_clamps_flat_outside_its_window_and_lerps_inside() {
        let g = || Generator::Ramp {
            from: Box::new(Expr::num(10.0)),
            to: Box::new(Expr::num(20.0)),
            start: Box::new(Expr::num(4.0)),
            end: Box::new(Expr::num(8.0)),
        };
        assert_eq!(gen_at(g(), 0.0), 10.0, "before start: flat at from");
        assert_eq!(gen_at(g(), 4.0), 10.0, "at start");
        assert_eq!(gen_at(g(), 6.0), 15.0, "midpoint");
        assert_eq!(gen_at(g(), 8.0), 20.0, "at end");
        assert_eq!(gen_at(g(), 100.0), 20.0, "after end: flat at to");
        // A degenerate (zero-width) window is a clean step at `start`, not a
        // divide-by-zero.
        let step = || Generator::Ramp {
            from: Box::new(Expr::num(1.0)),
            to: Box::new(Expr::num(2.0)),
            start: Box::new(Expr::num(5.0)),
            end: Box::new(Expr::num(5.0)),
        };
        assert_eq!(gen_at(step(), 4.9), 1.0, "before the step");
        assert_eq!(gen_at(step(), 5.0), 2.0, "at and after the step");
    }

    #[test]
    fn bounce_overshoots_then_settles_to_zero() {
        let g = || Generator::Bounce {
            amp: Box::new(Expr::num(3.0)),
            freq: Box::new(Expr::num(0.1)),
            decay: Box::new(Expr::num(0.1)),
        };
        assert!((gen_at(g(), 0.0) - 3.0).abs() < 1e-9, "starts at full amplitude");
        // The envelope amp·e^(−decay·frame) decays, so a late frame is small.
        assert!(gen_at(g(), 200.0).abs() < 1e-3, "settles toward zero");
        // Deterministic: the same frame gives the same value.
        assert_eq!(gen_at(g(), 12.0), gen_at(g(), 12.0));
    }

    #[test]
    fn a_generator_knob_can_be_a_parameter() {
        use crate::node::ParamValue;
        // b.opacity = osc whose `amp` knob reads b's `gain` parameter — the whole
        // point of shipping generators after parameters. gain = 4, sine at a
        // quarter cycle → offset(0) + 4·1 = 4.
        let mut doc = doc_with(
            Value::constant(0.5),
            Value::expr(Expr::Gen(Generator::Oscillator {
                freq: Box::new(Expr::num(0.25)),
                amp: Box::new(Expr::Param { node: None, name: "gain".into() }),
                phase: Box::new(Expr::num(0.0)),
                offset: Box::new(Expr::num(0.0)),
                wave: Waveform::Sine,
            })),
        );
        doc.root
            .find_mut(NodeId(2))
            .unwrap()
            .set_param("gain", ParamValue::Num(Value::constant(4.0)));
        assert!((opacity_of(&doc, 2, 1.0) - 4.0).abs() < 1e-9);
    }

    #[test]
    fn generator_seed_arity_and_knob_addressing_line_up() {
        for k in [ExprKind::Oscillator, ExprKind::Noise, ExprKind::Ramp, ExprKind::Bounce] {
            let e = Expr::seed(k);
            assert_eq!(e.kind(), k);
            assert!(k.is_generator());
            let Expr::Gen(g) = &e else { panic!("seed({k:?}) should be a generator") };
            // Arity equals the number of labelled knobs, and every slot resolves
            // both by label and by `child`/`at`.
            assert_eq!(e.arity(), g.knob_labels().len());
            for slot in 0..e.arity() {
                assert!(g.knob(slot).is_some(), "{k:?} knob {slot}");
                assert!(e.child(slot).is_some(), "{k:?} child {slot}");
                assert!(e.at(&[slot]).is_some(), "{k:?} at [{slot}]");
                assert!(e.slot_label(slot).is_some(), "{k:?} slot label {slot}");
            }
            // One past the last slot is out of range everywhere.
            assert!(e.child(e.arity()).is_none());
            assert!(e.at(&[e.arity()]).is_none());
        }
    }

    #[test]
    fn at_mut_edits_a_generator_knob_in_place() {
        // An oscillator whose `amp` (slot 1) starts at 1, edited to 9.
        let mut e = Expr::seed(ExprKind::Oscillator);
        *e.at_mut(&[1]).unwrap() = Expr::num(9.0);
        match &e {
            Expr::Gen(g) => assert_eq!(g.knob(1).unwrap().to_string(), "9"),
            _ => panic!("still a generator"),
        }
        // And the change is what drives the value: freq 0 → sine of phase 0 = 0,
        // so at frame 0 the value is offset(0) + amp·0 = 0 regardless; use a
        // quarter-cycle phase instead to read amp back out.
        *e.at_mut(&[0]).unwrap() = Expr::num(0.0); // freq
        *e.at_mut(&[2]).unwrap() = Expr::num(0.25); // phase (quarter cycle)
        assert!((gen_at(match e { Expr::Gen(g) => g, _ => unreachable!() }, 0.0) - 9.0).abs() < 1e-9);
    }

    #[test]
    fn a_generator_round_trips_through_json_and_prints_readably() {
        let v: Value<f64> = Value::expr(Expr::seed(ExprKind::Ramp));
        let json = serde_json::to_string(&v).unwrap();
        let back: Value<f64> = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, Value::Expr(Expr::Gen(Generator::Ramp { .. }))));
        assert_eq!(
            Expr::seed(ExprKind::Ramp).to_string(),
            "ramp(from: 0, to: 1, start: 0, end: 30)"
        );
        assert_eq!(
            Expr::seed(ExprKind::Oscillator).to_string(),
            "osc(freq: 0.05, amp: 1, phase: 0, offset: 0, sine)"
        );
    }

    #[test]
    fn wiggle_is_continuous_across_a_lattice_point() {
        // freq 1.0 puts lattice points on integer frames; the smoothstep must
        // not leave a visible jump at frame 3.
        let at = |frame: f64| match eval_script_ctx(
            "wiggle(1.0, 1.0)",
            &mut EvalCtx::at(frame),
        )
        .unwrap()
        {
            ExprValue::Num(n) => n,
            other => panic!("expected a number, got {other:?}"),
        };
        let (before, on, after) = (at(2.999), at(3.0), at(3.001));
        assert!((before - on).abs() < 1e-2, "{before} → {on}");
        assert!((after - on).abs() < 1e-2, "{on} → {after}");
    }
}

/// Resolve an [`Expr::Use`]: bind the module's knobs, then evaluate its body.
///
/// Binding is the override layering made concrete — an explicitly overridden
/// knob wins, anything else falls back to the module's own default. Both are
/// resolved in the **caller's** scope (before the module's scope is pushed), so
/// a default that reads `t01` retimes to the linking layer just as an override
/// that does would.
fn eval_use(module: ModuleId, overrides: &[(String, Expr)], ctx: &mut EvalCtx) -> ExprValue {
    let Some(modules) = ctx.modules else {
        ctx.warn_here("a module link needs a project to resolve".to_string());
        return ExprValue::Num(0.0);
    };
    let Some(def) = modules.get(&module).cloned() else {
        ctx.warn_here(format!("module {} no longer exists", module.0));
        return ExprValue::Num(0.0);
    };
    if ctx.module_stack.contains(&module) {
        ctx.warn_here(format!("module '{}' links itself; not expanded", def.name));
        return ExprValue::Num(0.0);
    }

    // Knobs first, in the caller's scope.
    let mut scope: Vec<(String, ExprValue)> = Vec::with_capacity(def.params.len());
    for param in &def.params {
        let value = match overrides.iter().find(|(n, _)| *n == param.name) {
            Some((_, expr)) => eval_expr(expr, ctx),
            None => param.value.resolve(ctx),
        };
        scope.push((param.name.clone(), value));
    }
    // An override naming a knob the module doesn't have is a typo that would
    // otherwise silently do nothing.
    for (name, _) in overrides {
        if !def.params.iter().any(|p| &p.name == name) {
            ctx.warn_here(format!("module '{}' has no parameter '{name}' to override", def.name));
        }
    }

    ctx.module_stack.push(module);
    ctx.module_scope.push(scope);
    let out = eval_expr(&def.body, ctx);
    ctx.module_scope.pop();
    ctx.module_stack.pop();
    out
}
