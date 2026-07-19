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

use crate::node::{Document, NodeId, Shape};
use crate::value::Color;

/// A value flowing through an expression, before it's converted back to a
/// property's concrete `T`. Dynamic on purpose: an expression mixes scalars,
/// positions, and colours, and only pins the type down at the property edge.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum ExprValue {
    Num(f64),
    Vec2(kurbo::Vec2),
    Color(Color),
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

    /// Map every component through `f` (used by `Neg`).
    fn map(self, f: impl Fn(f64) -> f64) -> ExprValue {
        use ExprValue::*;
        match self {
            Num(a) => Num(f(a)),
            Vec2(v) => Vec2(kurbo::Vec2::new(f(v.x), f(v.y))),
            Color(c) => Color(self::Color::rgba(f(c.r), f(c.g), f(c.b), f(c.a))),
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
        }
    }

    /// Every referenceable property — for a picker, and for the script node's
    /// list of what `value()` accepts.
    pub const ALL: [PropPath; 10] = [
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
            PropPath::Position | PropPath::Scale | PropPath::Anchor | PropPath::ShapeSize => {
                ExprValue::Vec2(kurbo::Vec2::ZERO)
            }
            PropPath::Rotation
            | PropPath::Opacity
            | PropPath::StrokeWidth
            | PropPath::ShapeRadius => ExprValue::Num(0.0),
            PropPath::Fill | PropPath::StrokeColor => {
                ExprValue::Color(Color::rgba(0.0, 0.0, 0.0, 0.0))
            }
        }
    }
}

/// The dataflow IR. Deliberately tiny for now: a literal, a reference to another
/// property (optionally at a shifted time — the `valueAtTime(t')` case), and
/// arithmetic. `Add`/`Mul`/`Neg` are enough to express the rest (`a - b` is
/// `Add(a, Neg(b))`); front-ends lower richer syntax down to this.
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
    Add(Box<Expr>, Box<Expr>),
    Mul(Box<Expr>, Box<Expr>),
    Neg(Box<Expr>),
    /// A Rhai script, evaluated each frame with `frame`/`time` in scope. Returns
    /// a number (→ `Num`) or a 2/3/4-element array (→ `Vec2`/`Color`). A leaf: it
    /// pulls its inputs from `frame`, not from wired-in child nodes.
    Script(String),
}

impl Expr {
    /// A literal number, the common case.
    pub fn num(n: f64) -> Expr {
        Expr::Lit(ExprValue::Num(n))
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
            Expr::Add(..) => ExprKind::Add,
            Expr::Mul(..) => ExprKind::Mul,
            Expr::Neg(..) => ExprKind::Neg,
            Expr::Script(_) => ExprKind::Script,
        }
    }

    /// A fresh node of `kind`, with children seeded to neutral literals so an
    /// editor can grow a tree by changing one node's kind at a time (a `Lit`
    /// becomes an `Add` of two zeros you then edit or change further). `Add`
    /// seeds 0+0, `Mul` 1×1 — the identities, so a half-built node is harmless.
    pub fn seed(kind: ExprKind) -> Expr {
        match kind {
            ExprKind::Lit => Expr::num(0.0),
            ExprKind::Ref => Expr::Ref {
                node: NodeId(0),
                prop: PropPath::Position,
                time_offset: 0.0,
            },
            ExprKind::Add => Expr::Add(Box::new(Expr::num(0.0)), Box::new(Expr::num(0.0))),
            ExprKind::Mul => Expr::Mul(Box::new(Expr::num(1.0)), Box::new(Expr::num(1.0))),
            ExprKind::Neg => Expr::Neg(Box::new(Expr::num(0.0))),
            ExprKind::Script => Expr::Script(String::new()),
        }
    }

    /// How many child slots this node has: `Add`/`Mul` two, `Neg` one, others
    /// none. Lets an editor iterate a node's inputs without matching the kind.
    pub fn arity(&self) -> usize {
        match self {
            Expr::Add(..) | Expr::Mul(..) => 2,
            Expr::Neg(..) => 1,
            Expr::Lit(_) | Expr::Ref { .. } | Expr::Script(_) => 0,
        }
    }

    /// Borrow the subtree at `path` (a sequence of child slots). `None` for an
    /// out-of-range slot.
    pub fn at(&self, path: &[usize]) -> Option<&Expr> {
        let Some((&slot, rest)) = path.split_first() else {
            return Some(self);
        };
        let child = match (self, slot) {
            (Expr::Add(a, _) | Expr::Mul(a, _) | Expr::Neg(a), 0) => a.as_ref(),
            (Expr::Add(_, b) | Expr::Mul(_, b), 1) => b.as_ref(),
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
            (Expr::Add(a, _) | Expr::Mul(a, _) | Expr::Neg(a), 0) => a.as_mut(),
            (Expr::Add(_, b) | Expr::Mul(_, b), 1) => b.as_mut(),
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
    Add,
    Mul,
    Neg,
    Script,
}

impl ExprKind {
    /// Every kind, in picker order.
    pub const ALL: [ExprKind; 6] = [
        ExprKind::Lit,
        ExprKind::Ref,
        ExprKind::Add,
        ExprKind::Mul,
        ExprKind::Neg,
        ExprKind::Script,
    ];

    pub fn label(self) -> &'static str {
        match self {
            ExprKind::Lit => "value",
            ExprKind::Ref => "ref",
            ExprKind::Add => "add",
            ExprKind::Mul => "mul",
            ExprKind::Neg => "neg",
            ExprKind::Script => "script",
        }
    }
}

impl fmt::Display for ExprValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ExprValue::Num(n) => write!(f, "{n}"),
            ExprValue::Vec2(v) => write!(f, "[{}, {}]", v.x, v.y),
            ExprValue::Color(c) => write!(f, "rgba({}, {}, {}, {})", c.r, c.g, c.b, c.a),
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
            Expr::Add(a, b) => write!(f, "({a} + {b})"),
            Expr::Mul(a, b) => write!(f, "({a} * {b})"),
            Expr::Neg(a) => write!(f, "-{a}"),
            Expr::Script(src) => write!(f, "{{ {src} }}"),
        }
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
    let _parked = bridge::enter(ctx);
    SCRIPT_ENGINE.with(|engine| {
        let mut scope = rhai::Scope::new();
        scope.push_constant("frame", frame);
        scope.push_constant("time", frame);
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
    Err("script must return a number or an array".into())
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

/// A cache key: a property reference at an exact frame. The frame is in the key
/// (`to_bits`) so an off-time sample can't poison the primary value's memo slot.
type PropKey = (NodeId, PropPath, u64);

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
    /// The frame to resolve at. Fractional on purpose — keys sit on the grid,
    /// the playhead need not.
    pub frame: f64,
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
            doc: None,
            cache: ResolveCache::default(),
            warnings: Vec::new(),
            current: None,
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
    fn warn_here(&mut self, msg: impl Into<String>) {
        let node = self.current.unwrap_or(NodeId(0));
        self.warn(node, msg);
    }

    fn warn(&mut self, node: NodeId, msg: impl Into<String>) {
        self.warnings.push((node, msg.into()));
    }

    /// Resolve `prop` on `node` at `frame`, dynamically. This is the one place
    /// an expression reaches back into the document. Memoized and cycle-guarded.
    fn resolve_prop(&mut self, node: NodeId, prop: PropPath, frame: f64) -> ExprValue {
        let key = (node, prop, frame.to_bits());
        if let Some(v) = self.cache.memo.get(&key) {
            return *v;
        }
        if self.cache.visiting.contains(&key) {
            self.warn(node, format!("expression cycle through {prop:?} on node {}", node.0));
            return prop.zero();
        }
        // Copy the document reference out (it's `&'a`, independent of `self`) so
        // the recursive resolve below can borrow `self` mutably for the cache.
        let Some(doc) = self.doc else {
            return prop.zero();
        };
        let Some(target) = doc.root.find(node) else {
            self.warn(node, format!("expression references missing node {}", node.0));
            return prop.zero();
        };

        self.cache.visiting.insert(key);
        let saved = self.frame;
        self.frame = frame;
        // Warnings raised while resolving the *target* belong to the target.
        let prev = self.enter_node(node);
        let value = self.resolve_on(target, prop);
        self.exit_node(prev);
        self.frame = saved;
        self.cache.visiting.remove(&key);
        self.cache.memo.insert(key, value);
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
        Expr::Lit(v) => *v,
        Expr::Ref { node, prop, time_offset } => {
            let frame = ctx.frame + time_offset;
            ctx.resolve_prop(*node, *prop, frame)
        }
        Expr::Add(a, b) => {
            let (a, b) = (eval_expr(a, ctx), eval_expr(b, ctx));
            a.zip(b, |x, y| x + y)
        }
        Expr::Mul(a, b) => {
            let (a, b) = (eval_expr(a, ctx), eval_expr(b, ctx));
            a.zip(b, |x, y| x * y)
        }
        Expr::Neg(a) => eval_expr(a, ctx).map(|x| -x),
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
        node.transform.opacity.resolve(&mut ctx)
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

    #[test]
    fn arithmetic_composes() {
        // 2 + 3 * 4 = 14.
        let e = Expr::Add(
            Box::new(Expr::num(2.0)),
            Box::new(Expr::Mul(Box::new(Expr::num(3.0)), Box::new(Expr::num(4.0)))),
        );
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

    #[test]
    fn seed_matches_its_kind_and_arity() {
        for k in ExprKind::ALL {
            let e = Expr::seed(k);
            assert_eq!(e.kind(), k);
            let expected = match k {
                ExprKind::Add | ExprKind::Mul => 2,
                ExprKind::Neg => 1,
                ExprKind::Lit | ExprKind::Ref | ExprKind::Script => 0,
            };
            assert_eq!(e.arity(), expected, "{k:?}");
        }
    }

    #[test]
    fn at_mut_addresses_a_subtree_by_slot_path() {
        // (2 + (3 * 4)) — edit the 4 (path [1, 1]) into a 9.
        let mut e = Expr::Add(
            Box::new(Expr::num(2.0)),
            Box::new(Expr::Mul(Box::new(Expr::num(3.0)), Box::new(Expr::num(4.0)))),
        );
        *e.at_mut(&[1, 1]).unwrap() = Expr::num(9.0);
        assert_eq!(e.to_string(), "(2 + (3 * 9))");
        // A slot past the node's arity is None (Neg has only slot 0).
        assert!(Expr::seed(ExprKind::Neg).at_mut(&[1]).is_none());
    }

    #[test]
    fn display_prints_the_tree_readably() {
        let e = Expr::Add(
            Box::new(Expr::num(2.0)),
            Box::new(Expr::Mul(Box::new(Expr::num(3.0)), Box::new(Expr::num(4.0)))),
        );
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
        // A wrong return type (string) is also an error.
        assert!(eval_script("\"hello\"", 0.0).is_err());
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
