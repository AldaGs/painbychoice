//! **Raising** an [`Expr`] into a [`NodeGraph`] — the inverse of `lower`, and
//! the mechanism behind the *property-graph fold* (step 3, the README's design
//! section).
//!
//! Lowering compiles the composition graph *down* to the `Expr` IR the editor
//! animates. Raising goes the other way: it takes a property's existing
//! expression (built in the old per-property editor, or a module body) and lays
//! it out as nodes and wires, so the same recipe becomes editable on the
//! Blender-style canvas. With both directions the property graph and the
//! composition graph are two *views of one substrate* rather than two editors —
//! which is the whole point of the fold.
//!
//! `lower(raise(e)) == e` across the whole `Expr` enum — value / math /
//! generator / ref / param / time / script / module link, overrides and all —
//! so a round trip is lossless and importing a recipe onto the canvas can't
//! quietly change it.

use kurbo::Vec2;

use crate::expr::{Expr, ExprValue, Generator, MathOp, TimeSource};
use crate::graph::{Endpoint, GraphCtx, NodeGraph};
use crate::node::Shape;
use crate::value::Value;

/// Column width and row height for the auto-layout — children sit one column to
/// the left of their parent (output flows left→right), stacked down the rows.
const COL: f64 = 184.0;
const ROW: f64 = 96.0;

/// Raise `expr` into `graph`, laying its nodes out with the root near `at`, and
/// return the output [`Endpoint`] that produces its value — ready to wire into a
/// driver or another node.
pub fn raise(graph: &mut NodeGraph, ctx: &GraphCtx, expr: &Expr, at: Vec2) -> Endpoint {
    let mut cursor_y = at.y;
    raise_rec(graph, ctx, expr, at.x, &mut cursor_y).0
}

/// Returns the created root's output endpoint and the y it was placed at (so a
/// parent can centre on the span of its children).
fn raise_rec(
    graph: &mut NodeGraph,
    ctx: &GraphCtx,
    expr: &Expr,
    x: f64,
    cursor_y: &mut f64,
) -> (Endpoint, f64) {
    // A leaf: place it at the next free row and advance the cursor.
    let mut leaf = |graph: &mut NodeGraph, kind: &str| -> (crate::graph::GraphNodeId, f64) {
        let y = *cursor_y;
        *cursor_y += ROW;
        (graph.add_node(kind, Vec2::new(x, y)), y)
    };

    match expr {
        Expr::Lit(v) => {
            // A literal raises to the node whose output socket carries its type:
            // `string` for text, `vec2` for a vector, `value` for a number. They
            // lower identically, but the socket type is what colours the wire and
            // gates a connection — a vector literal on a Number output would draw
            // the wrong colour and refuse a legal wire into a Vector input.
            let kind = match v {
                ExprValue::Str(_) => "string",
                ExprValue::Vec3(_) => "vec2",
                _ => "value",
            };
            let (id, y) = leaf(graph, kind);
            graph.node_mut(id).unwrap().set_value("value", v.clone());
            (Endpoint::new(id, "value"), y)
        }
        Expr::Ref { node, prop, time_offset } => {
            let (id, y) = leaf(graph, "ref");
            graph.node_mut(id).unwrap().config.ref_target = Some((*node, *prop, *time_offset));
            (Endpoint::new(id, "value"), y)
        }
        Expr::Param { name, .. } => {
            let (id, y) = leaf(graph, "param");
            graph.node_mut(id).unwrap().config.param = name.clone();
            (Endpoint::new(id, "value"), y)
        }
        Expr::Time(ts) => {
            let (kind, socket) = match ts {
                TimeSource::Local => ("localTime", "time"),
                TimeSource::In => ("inPoint", "time"),
                TimeSource::Out => ("outPoint", "time"),
                TimeSource::T01 => ("t01", "value"),
            };
            let (id, y) = leaf(graph, kind);
            (Endpoint::new(id, socket), y)
        }
        Expr::Bin { op, a, b } => {
            raise_math(graph, ctx, MathOp::Bin(*op), a, Some(b), x, cursor_y)
        }
        Expr::Un { op, a } => raise_math(graph, ctx, MathOp::Un(*op), a, None, x, cursor_y),
        Expr::Gen(g) => raise_generator(graph, ctx, g, x, cursor_y),
        // Build-a-vector: raise both scalars into a `join` centred on them.
        Expr::Vec3 { x: xe, y: ye, z: ze } => {
            let (ex, yx) = raise_rec(graph, ctx, xe, x - COL, cursor_y);
            let (ey, yy) = raise_rec(graph, ctx, ye, x - COL, cursor_y);
            let (ez, yz) = raise_rec(graph, ctx, ze, x - COL, cursor_y);
            let center = (yx + yy + yz) / 3.0;
            let id = graph.add_node("join", Vec2::new(x, center));
            let _ = graph.connect(ctx, ex, Endpoint::new(id, "x"));
            let _ = graph.connect(ctx, ey, Endpoint::new(id, "y"));
            let _ = graph.connect(ctx, ez, Endpoint::new(id, "z"));
            (Endpoint::new(id, "value"), center)
        }
        // Read-an-axis: raise the source into a `split`, take the named output.
        Expr::Comp { a, axis } => {
            let (ea, y0) = raise_rec(graph, ctx, a, x - COL, cursor_y);
            let id = graph.add_node("split", Vec2::new(x, y0));
            let _ = graph.connect(ctx, ea, Endpoint::new(id, "value"));
            (Endpoint::new(id, axis.name()), y0)
        }
        Expr::Script(src) => {
            let (id, y) = leaf(graph, "script");
            graph.node_mut(id).unwrap().config.script = src.clone();
            (Endpoint::new(id, "value"), y)
        }
        // A module link. Its overrides are raised as ordinary sub-graphs wired
        // into the knob sockets `GraphCtx::descriptor_for` grows for the linked
        // module — so an override is a *recipe* on the canvas, editable by any
        // node, exactly as it was in the per-property editor. A knob left
        // inheriting gets no wire, which is how lowering reads inheritance back.
        Expr::Use { module, overrides } => {
            let (id, y) = leaf(graph, "use");
            graph.node_mut(id).unwrap().config.module = Some(*module);
            // The node must know its module *before* the wires go in: the knob
            // sockets don't exist until it does, and `connect` validates
            // against them.
            let mut raised = Vec::new();
            for (name, expr) in overrides {
                let (e, _) = raise_rec(graph, ctx, expr, x - COL, cursor_y);
                raised.push((name.clone(), e));
            }
            for (name, e) in raised {
                let _ = graph.connect(ctx, e, Endpoint::new(id, name));
            }
            (Endpoint::new(id, "value"), y)
        }
    }
}

/// Raise a two-operand operator (`add`/`mul`): raise both children, place the
/// operator centred on them, wire them into `a`/`b`.
/// Raise an operator — one `math` node, its operands raised into its inputs.
///
/// `b` is `None` for a unary operator, and that is the whole difference: the
/// node kind, the socket names and the wiring are identical, because the graph
/// has one Math node the way the IR has one `Bin`/`Un` pair.
///
/// The op is set **before** the wires go in, for the same reason a `use` node's
/// module is: a unary node has no `b` socket until it knows it's unary, and
/// `connect` validates against the descriptor the config produces.
fn raise_math(
    graph: &mut NodeGraph,
    ctx: &GraphCtx,
    op: MathOp,
    a: &Expr,
    b: Option<&Expr>,
    x: f64,
    cursor_y: &mut f64,
) -> (Endpoint, f64) {
    let (ea, ya) = raise_rec(graph, ctx, a, x - COL, cursor_y);
    let (eb, center) = match b {
        Some(b) => {
            let (eb, yb) = raise_rec(graph, ctx, b, x - COL, cursor_y);
            (Some(eb), (ya + yb) / 2.0)
        }
        None => (None, ya),
    };
    let id = graph.add_node("math", Vec2::new(x, center));
    graph.node_mut(id).expect("just added").config.math_op = op;
    let _ = graph.connect(ctx, ea, Endpoint::new(id, "a"));
    if let Some(eb) = eb {
        let _ = graph.connect(ctx, eb, Endpoint::new(id, "b"));
    }
    (Endpoint::new(id, "result"), center)
}

/// Raise a generator, wiring each knob `Expr` into the matching input socket.
fn raise_generator(
    graph: &mut NodeGraph,
    ctx: &GraphCtx,
    g: &Generator,
    x: f64,
    cursor_y: &mut f64,
) -> (Endpoint, f64) {
    // (kind, knob sockets in order) — the knob Exprs line up with these. The
    // oscillator's waveform isn't a knob (nothing wires into it), so it rides
    // along as config, set on the node once it exists.
    let mut wave = None;
    let (kind, knobs): (&str, Vec<(&str, &Expr)>) = match g {
        Generator::Oscillator { freq, amp, phase, offset, wave: w } => {
            wave = Some(*w);
            ("osc", vec![("freq", freq), ("amp", amp), ("phase", phase), ("offset", offset)])
        }
        Generator::Noise { freq, amp, seed } => {
            ("noise", vec![("freq", freq), ("amp", amp), ("seed", seed)])
        }
        Generator::Ramp { from, to, start, end } => {
            ("ramp", vec![("from", from), ("to", to), ("start", start), ("end", end)])
        }
        Generator::Bounce { amp, freq, decay } => {
            ("bounce", vec![("amp", amp), ("freq", freq), ("decay", decay)])
        }
    };
    let mut centers = Vec::new();
    let mut endpoints = Vec::new();
    for (socket, knob) in &knobs {
        let (e, y) = raise_rec(graph, ctx, knob, x - COL, cursor_y);
        centers.push(y);
        endpoints.push((*socket, e));
    }
    let center = (centers.first().copied().unwrap_or(*cursor_y)
        + centers.last().copied().unwrap_or(*cursor_y))
        / 2.0;
    let id = graph.add_node(kind, Vec2::new(x, center));
    if let Some(w) = wave {
        graph.node_mut(id).unwrap().config.wave = w;
    }
    for (socket, e) in endpoints {
        let _ = graph.connect(ctx, e, Endpoint::new(id, socket));
    }
    (Endpoint::new(id, "value"), center)
}

/// Why a [`Shape`] can't be raised onto the canvas.
///
/// Both arms are refusals rather than best-effort conversions, on purpose: a
/// raised shape is *bound* straight away, so anything lost in translation would
/// be lost from the document a moment later.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RaiseShapeError {
    /// This param is a keyframe track. A graph-authored param lowers to
    /// `Value::Expr` (see [`crate::lower::lower_geometry`]), so binding the
    /// raised shape would *replace* the track — the animation would simply be
    /// gone. Bake it to a constant first, the same rule the property fold
    /// follows when it asks you to promote before importing.
    ///
    /// Carries the socket's human label so the caller can name it.
    Keyframed(&'static str),
    /// A hand-drawn [`Shape::Path`] has no node form — its geometry isn't
    /// parametric, which is the same reason it has no `ShapeSize`.
    Unsupported,
    /// Footage has no node form *yet*. A [`crate::Shape::Image`] names a source
    /// and a source frame, and there is no image node on this canvas to raise
    /// it onto — that's the image-graph stage, not the property graph this fold
    /// belongs to. Named separately from [`RaiseShapeError::Unsupported`]
    /// because it is a "not built yet", not a "can't exist": a hand-drawn path
    /// will never be parametric, whereas a footage node is on the way.
    Footage,
}

impl std::fmt::Display for RaiseShapeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RaiseShapeError::Keyframed(what) => write!(
                f,
                "'{what}' is keyframed. Bake it to a constant first — a \
                 graph-authored param is an expression and would replace the track."
            ),
            RaiseShapeError::Unsupported => {
                f.write_str("a hand-drawn path has no node form — its geometry isn't parametric")
            }
            RaiseShapeError::Footage => f.write_str(
                "a footage layer has no node form yet — that's the image graph, not this one",
            ),
        }
    }
}

/// Raise a [`Shape`] into `graph` as a shape node with its params filled in, and
/// return the `geometry` output that reproduces it — ready to bind to a layer.
///
/// **The inverse of [`crate::lower::lower_geometry`]**, and tested as one:
/// lowering the returned endpoint reproduces the shape. That is the geometry
/// counterpart of the `lower(raise(e)) == e` guarantee the `Expr` fold gives,
/// and it's what lets a shape made with the toolbar be pulled onto the canvas
/// and keep editing instead of having to be rebuilt as nodes.
///
/// Each param is filled by what its `Value` *is*: a constant becomes the socket's
/// stored literal, an expression is raised through [`raise`] and wired in, and a
/// keyframe track is refused (see [`RaiseShapeError::Keyframed`]).
pub fn raise_geometry(
    graph: &mut NodeGraph,
    ctx: &GraphCtx,
    shape: &Shape,
    at: Vec2,
) -> Result<Endpoint, RaiseShapeError> {
    // Check every param *before* touching the graph: a refusal must leave no
    // orphaned nodes behind, and the checks are cheap.
    let params: Vec<(&str, &'static str, ShapeParam<'_>)> = match shape {
        Shape::Path(_) => return Err(RaiseShapeError::Unsupported),
        Shape::Image { .. } => return Err(RaiseShapeError::Footage),
        Shape::Rect { size, radius } => vec![
            ("size", "Size", ShapeParam::Vec2(size)),
            ("radius", "Radius", ShapeParam::Num(radius)),
        ],
        Shape::Ellipse { size } => vec![("size", "Size", ShapeParam::Vec2(size))],
        Shape::Text { content, size, .. } => vec![
            ("content", "Content", ShapeParam::Str(content)),
            ("size", "Font Size", ShapeParam::Num(size)),
        ],
    };
    for (_, label, p) in &params {
        if p.is_keyframed() {
            return Err(RaiseShapeError::Keyframed(label));
        }
    }

    let kind = match shape {
        Shape::Rect { .. } => "rect",
        Shape::Ellipse { .. } => "ellipse",
        Shape::Text { .. } => "text",
        Shape::Path(_) | Shape::Image { .. } => unreachable!("refused above"),
    };
    // Expression params are raised into the column to the left, sharing one row
    // cursor so two raised subtrees can't land on top of each other — the same
    // layout discipline `raise_rec` uses for a generator's knobs.
    let mut cursor_y = at.y;
    let mut wired = Vec::new();
    let mut literals = Vec::new();
    for (socket, _, p) in &params {
        match p.source() {
            ParamSource::Const(v) => literals.push((*socket, v)),
            ParamSource::Expr(e) => {
                let (ep, _) = raise_rec(graph, ctx, e, at.x - COL, &mut cursor_y);
                wired.push((*socket, ep));
            }
        }
    }
    let id = graph.add_node(kind, Vec2::new(at.x, at.y));
    {
        let node = graph.node_mut(id).unwrap();
        for (socket, v) in literals {
            node.set_value(socket, v);
        }
        // Text's non-value half: `family` names a system font (a lookup key, not
        // a value) and align/wrap select a shaping mode, so none of them is a
        // socket. `lower_geometry` reads them straight back out of here.
        if let Shape::Text { family, align, max_width, .. } = shape {
            node.config.text.family = family.clone();
            node.config.text.align = *align;
            node.config.text.max_width = *max_width;
        }
    }
    for (socket, ep) in wired {
        let _ = graph.connect(ctx, ep, Endpoint::new(id, socket));
    }
    Ok(Endpoint::new(id, "geometry"))
}

/// A shape param, type-erased just far enough to ask the two questions
/// `raise_geometry` has: is it a track, and if not, is it a constant or an
/// expression? Mirrors the `PropRef` trick in the editor, minus the ops.
enum ShapeParam<'a> {
    Num(&'a Value<f64>),
    Vec2(&'a Value<Vec2>),
    Str(&'a Value<String>),
}

/// What a param will become on the canvas.
enum ParamSource<'a> {
    Const(ExprValue),
    Expr(&'a Expr),
}

impl ShapeParam<'_> {
    fn is_keyframed(&self) -> bool {
        match self {
            ShapeParam::Num(v) => matches!(v, Value::Keyframed(_)),
            ShapeParam::Vec2(v) => matches!(v, Value::Keyframed(_)),
            ShapeParam::Str(v) => matches!(v, Value::Keyframed(_)),
        }
    }

    /// Only called after [`Self::is_keyframed`] has cleared every param, so the
    /// track arm is unreachable — it falls back to the type's neutral rather
    /// than panicking, keeping raising total the way lowering is.
    fn source(&self) -> ParamSource<'_> {
        use crate::expr::ToExpr;
        match self {
            ShapeParam::Num(Value::Const(v)) => ParamSource::Const(v.to_expr()),
            ShapeParam::Vec2(Value::Const(v)) => ParamSource::Const(v.to_expr()),
            ShapeParam::Str(Value::Const(v)) => ParamSource::Const(v.to_expr()),
            ShapeParam::Num(Value::Expr(e))
            | ShapeParam::Vec2(Value::Expr(e))
            | ShapeParam::Str(Value::Expr(e)) => ParamSource::Expr(e),
            ShapeParam::Num(_) => ParamSource::Const(ExprValue::Num(0.0)),
            ShapeParam::Vec2(_) => ParamSource::Const(ExprValue::Vec3(crate::vec3::Vec3::ZERO)),
            ShapeParam::Str(_) => ParamSource::Const(ExprValue::Str(String::new())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vec3::Vec3;
    use crate::expr::{BinOp, UnOp};
    use crate::expr::{ExprValue, PropPath, Waveform};
    use crate::lower::lower_output;
    use crate::registry::NodeRegistry;
    use crate::node::{ModuleId, NodeId};

    /// The built-in registry, kept alive by the caller so a `GraphCtx` can
    /// borrow it. Tests that link no module use `GraphCtx::bare`.
    fn reg() -> NodeRegistry {
        NodeRegistry::with_builtins()
    }

    /// The fold's core guarantee: raising an expression into a graph and lowering
    /// it back reproduces the original, for the lowerable subset. Checked by the
    /// printed form, which captures structure and every leaf value.
    fn round_trips(expr: Expr) {
        let reg = reg();
        let ctx = &GraphCtx::bare(&reg);
        let mut g = NodeGraph::new();
        let ep = raise(&mut g, ctx, &expr, Vec2::ZERO);
        let back = lower_output(&g, ctx, &ep);
        assert_eq!(back.to_string(), expr.to_string(), "round trip changed the expression");
    }

    /// The vector plumbing round-trips: a `join` built from two scalars, a `vec2`
    /// literal, and a `split` reading an axis — `vec2(3, vec2(5, 6).y)`, one of
    /// each node in one tree, raised to nodes and lowered back unchanged.
    #[test]
    fn the_vector_nodes_round_trip() {
        use crate::expr::Axis;
        let expr = Expr::Vec3 {
            x: Box::new(Expr::Lit(ExprValue::Num(3.0))),
            z: Box::new(Expr::num(0.0)),
            y: Box::new(Expr::Comp {
                a: Box::new(Expr::Lit(ExprValue::Vec3(Vec3::flat(5.0, 6.0)))),
                axis: Axis::Y,
            }),
        };
        round_trips(expr);
    }

    #[test]
    fn math_round_trips() {
        // ((3 * frameRef) + -(2)) — a nested tree of operators and leaves.
        let expr = Expr::bin(
            BinOp::Add,
            Expr::bin(
                BinOp::Mul,
                Expr::Lit(ExprValue::Num(3.0)),
                Expr::Ref { node: NodeId(2), prop: PropPath::Rotation, time_offset: 0.0 },
            ),
            Expr::un(UnOp::Neg, Expr::Lit(ExprValue::Num(2.0))),
        );
        round_trips(expr);
    }

    /// A text literal must survive the fold like a numeric one — and land on a
    /// `string` node, not a `value` node. The kind matters beyond aesthetics:
    /// the two carry different socket types, so raising text onto a `value`
    /// would paint the wrong wire colour and refuse a legal connection into a
    /// text input.
    #[test]
    fn a_string_literal_round_trips_through_its_own_node() {
        let expr = Expr::Lit(ExprValue::Str("hello".into()));
        round_trips(expr.clone());

        let reg = reg();
        let ctx = &GraphCtx::bare(&reg);
        let mut g = NodeGraph::new();
        let ep = raise(&mut g, ctx, &expr, Vec2::ZERO);
        assert_eq!(g.node(ep.node).unwrap().kind, "string");
    }

    /// Concatenation is an ordinary `Add`, so a mixed text/number sum folds with
    /// no special case — the property that lets `"take " + n` be built on the
    /// canvas out of parts that already exist.
    #[test]
    fn a_concatenation_round_trips() {
        round_trips(Expr::bin(BinOp::Add,Expr::Lit(ExprValue::Str("take ".into())),Expr::Lit(ExprValue::Num(3.0))));
    }

    #[test]
    fn a_generator_with_wired_knobs_round_trips() {
        // osc whose amp is itself an expression, not just a literal.
        let expr = Expr::Gen(Generator::Oscillator {
            freq: Box::new(Expr::Lit(ExprValue::Num(0.2))),
            amp: Box::new(Expr::bin(BinOp::Mul,Expr::Lit(ExprValue::Num(10.0)),Expr::Param { node: None, name: "gain".into() })),
            phase: Box::new(Expr::Lit(ExprValue::Num(0.0))),
            offset: Box::new(Expr::Lit(ExprValue::Num(0.0))),
            wave: Waveform::Sine,
        });
        round_trips(expr);
    }

    /// A non-sine oscillator keeps its waveform across the fold. Before the
    /// node carried one, a square raised onto the canvas lowered back as a sine
    /// — the recipe silently changed shape on import.
    #[test]
    fn an_oscillators_waveform_survives_the_fold() {
        let expr = Expr::Gen(Generator::Oscillator {
            freq: Box::new(Expr::Lit(ExprValue::Num(0.5))),
            amp: Box::new(Expr::Lit(ExprValue::Num(2.0))),
            phase: Box::new(Expr::Lit(ExprValue::Num(0.0))),
            offset: Box::new(Expr::Lit(ExprValue::Num(0.0))),
            wave: Waveform::Square,
        });
        round_trips(expr);

        // …and it's the *node's* waveform doing it, not a coincidence of
        // printing: the raised node carries the square in its config.
        let reg = reg();
        let ctx = &GraphCtx::bare(&reg);
        let mut g = NodeGraph::new();
        let expr = Expr::Gen(Generator::Oscillator {
            freq: Box::new(Expr::Lit(ExprValue::Num(0.5))),
            amp: Box::new(Expr::Lit(ExprValue::Num(1.0))),
            phase: Box::new(Expr::Lit(ExprValue::Num(0.0))),
            offset: Box::new(Expr::Lit(ExprValue::Num(0.0))),
            wave: Waveform::Saw,
        });
        let ep = raise(&mut g, ctx, &expr, Vec2::ZERO);
        assert_eq!(g.node(ep.node).unwrap().config.wave, Waveform::Saw);
    }

    #[test]
    fn a_time_source_feeding_math_round_trips() {
        // localTime into a mul — proves a Time output wires into a Number input.
        let expr = Expr::bin(BinOp::Mul,Expr::Time(TimeSource::Local),Expr::Lit(ExprValue::Num(4.0)));
        round_trips(expr);
    }

    /// A script leaf, wired into math, round-trips its source intact.
    #[test]
    fn a_script_round_trips() {
        let expr = Expr::bin(BinOp::Add,Expr::Script("frame * 2.0".into()),Expr::Lit(ExprValue::Num(1.0)));
        round_trips(expr);
    }

    /// A plain module link (no overrides) round-trips to the same `Use`.
    #[test]
    fn a_plain_module_link_round_trips() {
        let expr = Expr::Use { module: ModuleId(3), overrides: Vec::new() };
        round_trips(expr);
    }

    /// A link's **overrides** round-trip too, now that a `use` node's sockets
    /// come from the module it links. Before that, raising dropped them and the
    /// link silently fell back to the module's defaults on import.
    #[test]
    fn a_module_links_overrides_round_trip() {
        use crate::node::{Module, ParamValue};
        let module = Module::new("pulse", Expr::Param { node: None, name: "amp".into() })
            .with_param("amp", ParamValue::Num(crate::value::Value::constant(1.0)))
            .with_param("rate", ParamValue::Num(crate::value::Value::constant(2.0)));
        let modules: std::collections::BTreeMap<_, _> =
            [(ModuleId(3), module)].into_iter().collect();
        let reg = reg();
        let ctx = &GraphCtx::new(&reg, &modules);

        // `amp` overridden with a real sub-expression; `rate` left inheriting.
        let expr = Expr::Use {
            module: ModuleId(3),
            overrides: vec![(
                "amp".into(),
                Expr::bin(BinOp::Mul,Expr::Lit(ExprValue::Num(4.0)),Expr::Time(TimeSource::T01)),
            )],
        };
        let mut g = NodeGraph::new();
        let ep = raise(&mut g, ctx, &expr, Vec2::ZERO);
        let back = lower_output(&g, ctx, &ep);
        assert_eq!(back.to_string(), expr.to_string());
        // The inherited knob really is absent, not overridden with a copy of
        // its default — a copy would stop retiming in the caller's scope.
        let Expr::Use { overrides, .. } = &back else { panic!("{back:?}") };
        assert_eq!(overrides.len(), 1, "only the touched knob is overridden");
    }

    // ── Geometry fold ────────────────────────────────────────────────────────

    use crate::lower::lower_geometry;
    use crate::node::Shape;
    use crate::value::{Keyframe, Track, Value};

    /// The geometry counterpart of `round_trips`: raising a shape onto the
    /// canvas and lowering it back must reproduce it. Compares the *resolved*
    /// params, since lowering deliberately turns every param into an `Expr`
    /// (that's what makes a graph-authored shape animate) — so the recipes
    /// differ by construction while the values must not.
    fn shape_round_trips(shape: Shape) -> Shape {
        let reg = reg();
        let ctx = &GraphCtx::bare(&reg);
        let mut g = NodeGraph::new();
        let ep = raise_geometry(&mut g, ctx, &shape, Vec2::ZERO).expect("should raise");
        assert_eq!(ep.socket, "geometry", "a raised shape hands back its geometry");
        lower_geometry(&g, ctx, &ep).expect("a raised shape must lower back")
    }

    #[test]
    fn a_rect_round_trips_through_the_canvas() {
        let back = shape_round_trips(Shape::Rect {
            size: Value::constant(Vec2::new(320.0, 180.0)),
            radius: Value::constant(12.0),
        });
        let Shape::Rect { size, radius } = &back else { panic!("{back:?}") };
        let ctx = &mut crate::expr::EvalCtx::at(0.0);
        assert_eq!(size.resolve(ctx), Vec2::new(320.0, 180.0));
        assert_eq!(radius.resolve(ctx), 12.0);
    }

    #[test]
    fn an_ellipse_round_trips_through_the_canvas() {
        let back =
            shape_round_trips(Shape::Ellipse { size: Value::constant(Vec2::new(64.0, 48.0)) });
        let Shape::Ellipse { size } = &back else { panic!("{back:?}") };
        assert_eq!(size.resolve(&mut crate::expr::EvalCtx::at(0.0)), Vec2::new(64.0, 48.0));
    }

    /// Text carries the most across: a string param *and* three non-value
    /// fields that ride in `NodeConfig::text` rather than on sockets.
    #[test]
    fn text_round_trips_including_its_typography() {
        let back = shape_round_trips(Shape::Text {
            content: Value::constant("Chapter One".to_string()),
            family: "Georgia".into(),
            size: Value::constant(72.0),
            align: crate::text::TextAlign::Center,
            max_width: Some(400.0),
        });
        let Shape::Text { content, family, size, align, max_width } = &back else {
            panic!("{back:?}")
        };
        let ctx = &mut crate::expr::EvalCtx::at(0.0);
        assert_eq!(content.resolve(ctx), "Chapter One");
        assert_eq!(size.resolve(ctx), 72.0);
        assert_eq!(family, "Georgia");
        assert_eq!(*align, crate::text::TextAlign::Center);
        assert_eq!(*max_width, Some(400.0));
    }

    /// An expression param isn't flattened to its current value — it comes
    /// across as a *wired subtree*, which is the whole point of pulling a shape
    /// onto the canvas: you can keep editing the recipe.
    #[test]
    fn an_expression_param_raises_as_wired_nodes() {
        let reg = reg();
        let ctx = &GraphCtx::bare(&reg);
        let mut g = NodeGraph::new();
        let shape = Shape::Rect {
            size: Value::constant(Vec2::new(10.0, 10.0)),
            radius: Value::expr(Expr::bin(BinOp::Mul,Expr::Lit(ExprValue::Num(2.0)),Expr::Time(TimeSource::T01))),
        };
        let ep = raise_geometry(&mut g, ctx, &shape, Vec2::ZERO).unwrap();
        // The rect, plus the mul and its two leaves.
        assert_eq!(g.nodes.len(), 4, "the expression came across as nodes, not a literal");
        let back = lower_geometry(&g, ctx, &ep).unwrap();
        let Shape::Rect { radius, .. } = &back else { panic!("{back:?}") };
        assert_eq!(radius.expr_ref().unwrap().to_string(), "(2 * t01)");
    }

    /// The refusal that keeps the fold lossless. Binding a raised shape makes
    /// every param an expression, so a track would simply be replaced — and the
    /// error names which param, because "something is keyframed" is unactionable
    /// on a shape with several.
    #[test]
    fn a_keyframed_param_is_refused_by_name() {
        let reg = reg();
        let ctx = &GraphCtx::bare(&reg);
        let mut g = NodeGraph::new();
        let shape = Shape::Rect {
            size: Value::constant(Vec2::new(10.0, 10.0)),
            radius: Value::Keyframed(Track::new(vec![
                Keyframe::linear(0, 0.0),
                Keyframe::linear(24, 40.0),
            ])),
        };
        assert_eq!(
            raise_geometry(&mut g, ctx, &shape, Vec2::ZERO),
            Err(RaiseShapeError::Keyframed("Radius"))
        );
        // And it refused *before* touching the graph — a rejected raise must
        // not leave half a shape behind.
        assert!(g.nodes.is_empty(), "a refusal left orphaned nodes");
    }

    /// A hand-drawn path has no parametric form, so there is nothing to raise.
    #[test]
    fn a_hand_drawn_path_cannot_be_raised() {
        let reg = reg();
        let ctx = &GraphCtx::bare(&reg);
        let mut g = NodeGraph::new();
        assert_eq!(
            raise_geometry(&mut g, ctx, &Shape::Path(kurbo::BezPath::new()), Vec2::ZERO),
            Err(RaiseShapeError::Unsupported)
        );
        assert!(g.nodes.is_empty());
    }
}
