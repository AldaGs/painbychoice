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

use crate::node::{Document, NodeId};
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
/// A first, useful slice — the transform channels plus fill, covering all three
/// [`ExprValue`] kinds. Extending it to stroke/shape params is one more variant
/// here and one arm in [`EvalCtx::resolve_prop`]; nothing else changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PropPath {
    Position,
    Rotation,
    Scale,
    Opacity,
    Anchor,
    Fill,
}

impl PropPath {
    /// The neutral value of this property's kind, for the error cases (missing
    /// node, no document, or a cycle) where there's no real value to return.
    fn zero(self) -> ExprValue {
        match self {
            PropPath::Position | PropPath::Scale | PropPath::Anchor => {
                ExprValue::Vec2(kurbo::Vec2::ZERO)
            }
            PropPath::Rotation | PropPath::Opacity => ExprValue::Num(0.0),
            PropPath::Fill => ExprValue::Color(Color::rgba(0.0, 0.0, 0.0, 0.0)),
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
        }
    }

    /// How many child slots this node has: `Add`/`Mul` two, `Neg` one, others
    /// none. Lets an editor iterate a node's inputs without matching the kind.
    pub fn arity(&self) -> usize {
        match self {
            Expr::Add(..) | Expr::Mul(..) => 2,
            Expr::Neg(..) => 1,
            Expr::Lit(_) | Expr::Ref { .. } => 0,
        }
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
}

impl ExprKind {
    /// Every kind, in picker order.
    pub const ALL: [ExprKind; 5] =
        [ExprKind::Lit, ExprKind::Ref, ExprKind::Add, ExprKind::Mul, ExprKind::Neg];

    pub fn label(self) -> &'static str {
        match self {
            ExprKind::Lit => "value",
            ExprKind::Ref => "ref",
            ExprKind::Add => "add",
            ExprKind::Mul => "mul",
            ExprKind::Neg => "neg",
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
        }
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
}

impl<'a> EvalCtx<'a> {
    /// A context over `doc` at `frame` — the one `evaluate` uses.
    pub fn new(doc: &'a Document, frame: f64) -> Self {
        Self { frame, doc: Some(doc), cache: ResolveCache::default(), warnings: Vec::new() }
    }

    /// A document-less context at `frame`, for resolving values that don't
    /// reference the scene (constants and keyframe tracks). Expression
    /// references fall back to a neutral value.
    pub fn at(frame: f64) -> Self {
        Self { frame, doc: None, cache: ResolveCache::default(), warnings: Vec::new() }
    }

    /// Take the warnings gathered while resolving (cycles, dangling refs), so
    /// `evaluate` can fold them into the `Scene`'s provenance-tagged list.
    pub fn take_warnings(&mut self) -> Vec<(NodeId, String)> {
        std::mem::take(&mut self.warnings)
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
        let value = self.resolve_on(target, prop);
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
        }
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
                ExprKind::Lit | ExprKind::Ref => 0,
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
}
