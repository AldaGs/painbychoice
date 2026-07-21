//! **Lowering** a [`NodeGraph`] to the [`Expr`] IR — the "alongside, lowers to
//! the IR" contract made concrete (step 3 of the composition node graph; see the
//! README's design section).
//!
//! The node graph is the authoring front-end; `Node`/`Expr` is the IR that
//! `evaluate` runs. This module is the compiler between them, and — in the EBN
//! "IR + dumb-printer" spirit — it is a pure tree-walk with no evaluation of its
//! own: it *builds* an `Expr`, it does not run one. Following an input backward
//! through its wire (or, unwired, reading the node's stored literal / the
//! descriptor default) is the whole algorithm.
//!
//! There are **two** lowerings here, because the graph produces two kinds of
//! thing: [`lower_output`] compiles a value socket to an `Expr` (the value /
//! math / generator / input / module subset), and [`lower_geometry`] compiles a
//! shape node's `geometry` socket to a [`Shape`] whose params are the lowered
//! `Expr`s. The second is what makes the graph able to *author* geometry rather
//! than only drive numbers; a shape's scalar outputs meanwhile **echo** its
//! resolved params back into the value world, so math can chain off a
//! rectangle's size.
//!
//! Nothing here panics on a graph it can't compile: an unknown kind, a
//! dangling wire, or a cycle a hand-edited file smuggled in all lower to a
//! neutral literal (or, for geometry, to `None`).

use std::collections::HashSet;

use crate::expr::{Expr, ExprValue, Generator, TimeSource};
use crate::graph::{Endpoint, GraphCtx, GraphNodeId, NodeGraph};
use crate::node::Shape;
use crate::registry::NodeCategory;
use crate::socket::SocketType;
use crate::value::{Color, Value};

/// Lower the value produced at `output` (a node's output socket) to an `Expr`.
///
/// The graph is a DAG — [`NodeGraph::connect`] rejects a cycle — but a
/// hand-edited file could still carry one, so the walk guards against back-edges
/// (a re-entered node lowers to a neutral literal, the same warn-don't-hang
/// contract the evaluator's cycle cache follows) rather than recursing forever.
pub fn lower_output(graph: &NodeGraph, ctx: &GraphCtx, output: &Endpoint) -> Expr {
    lower_out(graph, ctx, output, &mut HashSet::new())
}

fn lower_out(
    graph: &NodeGraph,
    ctx: &GraphCtx,
    output: &Endpoint,
    visiting: &mut HashSet<GraphNodeId>,
) -> Expr {
    let Some(node) = graph.node(output.node) else { return neutral() };
    // A back-edge (the node is already on the walk stack) is a cycle: stop with
    // a neutral literal instead of looping. Shared outputs (a DAG diamond)
    // re-enter after this node is popped, so they lower fine.
    if !visiting.insert(output.node) {
        return neutral();
    }
    let expr = match node.kind.as_str() {
        // A constant. Its literal is stored under the output socket's id (the
        // one place a source node keeps a value); absent means zero.
        "value" => Expr::Lit(node.value(&output.socket).unwrap_or(ExprValue::Num(0.0))),
        "add" => Expr::Add(
            b(lower_in(graph, ctx, output.node, "a", visiting)),
            b(lower_in(graph, ctx, output.node, "b", visiting)),
        ),
        "mul" => Expr::Mul(
            b(lower_in(graph, ctx, output.node, "a", visiting)),
            b(lower_in(graph, ctx, output.node, "b", visiting)),
        ),
        "neg" => Expr::Neg(b(lower_in(graph, ctx, output.node, "a", visiting))),
        "osc" => Expr::Gen(Generator::Oscillator {
            freq: b(lower_in(graph, ctx, output.node, "freq", visiting)),
            amp: b(lower_in(graph, ctx, output.node, "amp", visiting)),
            phase: b(lower_in(graph, ctx, output.node, "phase", visiting)),
            offset: b(lower_in(graph, ctx, output.node, "offset", visiting)),
            // The waveform is config, not a socket: it picks *which* function
            // the generator is, so there's nothing for a wire to carry.
            wave: node.config.wave,
        }),
        "noise" => Expr::Gen(Generator::Noise {
            freq: b(lower_in(graph, ctx, output.node, "freq", visiting)),
            amp: b(lower_in(graph, ctx, output.node, "amp", visiting)),
            seed: b(lower_in(graph, ctx, output.node, "seed", visiting)),
        }),
        "ramp" => Expr::Gen(Generator::Ramp {
            from: b(lower_in(graph, ctx, output.node, "from", visiting)),
            to: b(lower_in(graph, ctx, output.node, "to", visiting)),
            start: b(lower_in(graph, ctx, output.node, "start", visiting)),
            end: b(lower_in(graph, ctx, output.node, "end", visiting)),
        }),
        "bounce" => Expr::Gen(Generator::Bounce {
            amp: b(lower_in(graph, ctx, output.node, "amp", visiting)),
            freq: b(lower_in(graph, ctx, output.node, "freq", visiting)),
            decay: b(lower_in(graph, ctx, output.node, "decay", visiting)),
        }),
        // A reference to another layer's property, at a frame offset. Neutral
        // until a target is picked, so an unconfigured `ref` never breaks a frame.
        "ref" => match node.config.ref_target {
            Some((n, prop, time_offset)) => Expr::Ref { node: n, prop, time_offset },
            None => neutral(),
        },
        // An exposed-knob read. `node: None` → the *driven* layer's own knob, so
        // one graph output fits each layer a driver points it at.
        "param" => {
            if node.config.param.is_empty() {
                neutral()
            } else {
                Expr::Param { node: None, name: node.config.param.clone() }
            }
        }
        // Layer-clock leaves — no config, no children.
        "localTime" => Expr::Time(TimeSource::Local),
        "inPoint" => Expr::Time(TimeSource::In),
        "outPoint" => Expr::Time(TimeSource::Out),
        "t01" => Expr::Time(TimeSource::T01),
        // A Rhai leaf. Neutral while blank, so an empty field never errors.
        "script" => {
            if node.config.script.trim().is_empty() {
                neutral()
            } else {
                Expr::Script(node.config.script.clone())
            }
        }
        // A shared-module link. Its input sockets *are* the module's knobs
        // (`GraphCtx::descriptor_for` grows them per node), and each one carries
        // an override only when the user actually set it — **unwired and unset
        // means inherit**, which is what makes an override a layering over the
        // module's default rather than a fork of it. Neutral until a module is
        // picked.
        "use" => match node.config.module {
            Some(module) => {
                let knobs = ctx.descriptor_for(node).map(|d| d.into_owned()).unwrap_or_else(|| {
                    crate::registry::NodeDescriptor::new("use", NodeCategory::Module, "Module")
                });
                let overrides = knobs
                    .inputs
                    .iter()
                    .filter(|s| is_overridden(graph, node, &s.id))
                    .map(|s| (s.id.clone(), lower_in(graph, ctx, output.node, &s.id, visiting)))
                    .collect();
                Expr::Use { module, overrides }
            }
            None => neutral(),
        },
        // A shape's **echo** outputs: `rect.size`, `rect.radius`,
        // `ellipse.size`, `text.size` each hand back the value feeding the input
        // of the same id, so math can chain off a rectangle's size — the
        // object-scope case the README asks for ("a `Rect` node's size/radius
        // become output sockets"). The pairing is by socket id, pinned by a
        // registry test, so a new shape primitive needs no arm here.
        //
        // The `geometry` output isn't an `Expr` at all — it's a `Shape`, and
        // lowers through [`lower_geometry`]. Neutral here rather than a panic,
        // so a graph that wires geometry into math still lowers.
        "rect" | "ellipse" | "text" => match output.socket.as_str() {
            "geometry" => neutral(),
            echoed => lower_in(graph, ctx, output.node, echoed, visiting),
        },
        // An unknown kind (a plugin that isn't loaded). Neutral, not a panic.
        _ => neutral(),
    };
    visiting.remove(&output.node);
    expr
}

/// Lower the geometry produced at `output` to a [`Shape`] — the scene-tree
/// counterpart of [`lower_output`], and the piece that lets a graph **author**
/// geometry rather than only drive numbers into a shape made elsewhere.
///
/// Every shape param becomes a `Value::Expr` holding the lowered input, so a
/// wired `size` animates through `evaluate` exactly like a hand-written
/// expression and an unwired one carries its literal. That uniformity is
/// deliberate: the shape a driver writes is *entirely* the graph's, with no
/// mixture of graph-owned and hand-owned params to reason about.
///
/// `None` when `output` isn't a shape node's `geometry` socket — a missing
/// node, an unknown (plugin) kind, or a scalar output. A geometry driver then
/// leaves its target's shape untouched rather than blanking the layer.
pub fn lower_geometry(graph: &NodeGraph, ctx: &GraphCtx, output: &Endpoint) -> Option<Shape> {
    if output.socket != "geometry" {
        return None;
    }
    let node = graph.node(output.node)?;
    // Seed the cycle guard with this node: its echo outputs are ordinary
    // `Expr`s, so a hand-edited file could wire one back into its own input.
    // `connect` refuses that, but lowering must not hang on a file that has it.
    let mut visiting = HashSet::from([output.node]);
    let param = |socket: &str, visiting: &mut HashSet<GraphNodeId>| {
        lower_in(graph, ctx, output.node, socket, visiting)
    };
    Some(match node.kind.as_str() {
        "rect" => Shape::Rect {
            size: Value::expr(param("size", &mut visiting)),
            radius: Value::expr(param("radius", &mut visiting)),
        },
        "ellipse" => Shape::Ellipse { size: Value::expr(param("size", &mut visiting)) },
        "text" => {
            let t = &node.config.text;
            Shape::Text {
                content: t.content.clone(),
                family: t.family.clone(),
                size: Value::expr(param("size", &mut visiting)),
                align: t.align,
                max_width: t.max_width,
            }
        }
        _ => return None,
    })
}

/// Recompile every **module body** from its own canvas — the document scope's
/// counterpart to a driver recompile.
///
/// A module whose `output` is set has a graph-authored body: it's lowered and
/// written to `body`, which is what `eval_use` runs. A module with no `output`
/// is left alone — its body was built elsewhere (the per-property editor, or an
/// import) and an empty canvas must not blank it.
///
/// Two passes over the map, because a `use` node inside one module's graph
/// resolves its sockets against *all* modules: the reads finish before any
/// write begins, so no module ever compiles against a half-updated sibling.
/// Order between modules doesn't matter — a module linking another lowers to an
/// `Expr::Use`, which `eval_use` resolves at evaluation time, so no body
/// depends on another body having been compiled first.
pub fn compile_modules(
    modules: &mut std::collections::BTreeMap<crate::node::ModuleId, crate::node::Module>,
    reg: &crate::registry::NodeRegistry,
) {
    let compiled: Vec<(crate::node::ModuleId, Expr)> = {
        let ctx = GraphCtx::new(reg, modules);
        modules
            .iter()
            .filter_map(|(id, m)| {
                let out = m.output.as_ref()?;
                Some((*id, lower_output(&m.graph, &ctx, out)))
            })
            .collect()
    };
    for (id, body) in compiled {
        if let Some(m) = modules.get_mut(&id) {
            m.body = body;
        }
    }
}

/// Lower the value feeding input socket `socket` of `node`: follow its wire if
/// there is one, else use the node's stored literal, else the descriptor's
/// default, else the socket type's neutral.
fn lower_in(
    graph: &NodeGraph,
    ctx: &GraphCtx,
    node: GraphNodeId,
    socket: &str,
    visiting: &mut HashSet<GraphNodeId>,
) -> Expr {
    let ep = Endpoint::new(node, socket);
    if let Some(edge) = graph.incoming(&ep) {
        return lower_out(graph, ctx, &edge.from, visiting);
    }
    // Unwired: the literal to feed. A user override wins; otherwise the
    // descriptor's default; otherwise the socket type's neutral.
    let n = graph.node(node);
    let stored = n.and_then(|n| n.value(socket));
    let by_desc = n
        .and_then(|n| ctx.descriptor_for(n))
        .and_then(|d| d.find_input(socket).cloned())
        .map(|s| s.default.unwrap_or_else(|| neutral_for(s.ty)));
    Expr::Lit(stored.or(by_desc).unwrap_or(ExprValue::Num(0.0)))
}

/// Whether a `use` node's knob socket carries an **override**, as opposed to
/// inheriting the module's own default. Either a wire feeds it or the user
/// typed a literal into it; anything else is inheritance, and lowering must emit
/// no entry at all — an override of the default *by* the default would still
/// differ, because a module default is resolved lazily in the caller's scope
/// (so a default reading `t01` retimes, and a literal copy of it wouldn't).
fn is_overridden(graph: &NodeGraph, node: &crate::graph::GraphNode, socket: &str) -> bool {
    graph.incoming(&Endpoint::new(node.id, socket)).is_some() || node.value(socket).is_some()
}

/// A neutral scalar zero — the fallback for a cycle, a missing node, or an
/// as-yet-unlowered kind.
fn neutral() -> Expr {
    Expr::Lit(ExprValue::Num(0.0))
}

/// The neutral value of a socket type, when neither an override nor a descriptor
/// default is present. Mirrors `FromExpr::fallback`: a zero of the right shape.
fn neutral_for(ty: SocketType) -> ExprValue {
    match ty {
        SocketType::Number | SocketType::Time => ExprValue::Num(0.0),
        SocketType::Vector => ExprValue::Vec2(kurbo::Vec2::ZERO),
        SocketType::Color => ExprValue::Color(Color::rgba(0.0, 0.0, 0.0, 0.0)),
        // No scalar literal — these inputs are meant to be wired. Degenerate
        // zero keeps lowering total.
        SocketType::Geometry | SocketType::Layer | SocketType::Matte => ExprValue::Num(0.0),
    }
}

fn b(e: Expr) -> Box<Expr> {
    Box::new(e)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::expr::{eval_expr, EvalCtx};
    use crate::graph::Endpoint;
    use crate::registry::NodeRegistry;
    use crate::node::ModuleId;
    use crate::graph::GraphCtx;
    use kurbo::Vec2;

    /// The built-in registry, kept alive by the caller so a `GraphCtx` can
    /// borrow it. Tests that link no module use `GraphCtx::bare`.
    fn reg() -> NodeRegistry {
        NodeRegistry::with_builtins()
    }

    /// The number an `Expr` evaluates to at frame 0, in a doc-less context — the
    /// value / math / generator subset needs no document.
    fn eval0(e: &Expr) -> f64 {
        let mut ctx = EvalCtx::at(0.0);
        match eval_expr(e, &mut ctx) {
            ExprValue::Num(n) => n,
            other => panic!("expected a number, got {other:?}"),
        }
    }

    /// value(3) → add.a, value(4) → add.b lowers to `(3 + 4)` and evaluates to 7.
    #[test]
    fn a_math_graph_lowers_and_evaluates() {
        let reg = reg();
        let ctx = &GraphCtx::bare(&reg);
        let mut g = NodeGraph::new();
        let a = g.add_node("value", Vec2::ZERO);
        let b_ = g.add_node("value", Vec2::new(0.0, 60.0));
        let add = g.add_node("add", Vec2::new(200.0, 0.0));
        g.node_mut(a).unwrap().set_value("value", ExprValue::Num(3.0));
        g.node_mut(b_).unwrap().set_value("value", ExprValue::Num(4.0));
        g.connect(ctx, Endpoint::new(a, "value"), Endpoint::new(add, "a")).unwrap();
        g.connect(ctx, Endpoint::new(b_, "value"), Endpoint::new(add, "b")).unwrap();

        let expr = lower_output(&g, ctx, &Endpoint::new(add, "result"));
        assert_eq!(expr.to_string(), "(3 + 4)");
        assert_eq!(eval0(&expr), 7.0);
    }

    /// An unwired input takes the descriptor default: an `add` with only `a`
    /// wired reads `b`'s default (0), and a `mul` reads its default (1) — so the
    /// two operators lower to different resting values from the same wiring.
    #[test]
    fn an_unwired_input_uses_the_descriptor_default() {
        let reg = reg();
        let ctx = &GraphCtx::bare(&reg);
        let mut g = NodeGraph::new();
        let v = g.add_node("value", Vec2::ZERO);
        g.node_mut(v).unwrap().set_value("value", ExprValue::Num(5.0));

        let add = g.add_node("add", Vec2::new(200.0, 0.0));
        g.connect(ctx, Endpoint::new(v, "value"), Endpoint::new(add, "a")).unwrap();
        // b is unwired → its default 0 → 5 + 0 = 5.
        assert_eq!(eval0(&lower_output(&g, ctx, &Endpoint::new(add, "result"))), 5.0);

        let mul = g.add_node("mul", Vec2::new(200.0, 120.0));
        g.connect(ctx, Endpoint::new(v, "value"), Endpoint::new(mul, "a")).unwrap();
        // b is unwired → mul's default 1 → 5 * 1 = 5 (not 0, which add would give).
        assert_eq!(eval0(&lower_output(&g, ctx, &Endpoint::new(mul, "result"))), 5.0);
    }

    /// A user override on an unwired socket beats the descriptor default.
    #[test]
    fn a_stored_override_beats_the_default() {
        let reg = reg();
        let ctx = &GraphCtx::bare(&reg);
        let mut g = NodeGraph::new();
        let neg = g.add_node("neg", Vec2::ZERO);
        g.node_mut(neg).unwrap().set_value("a", ExprValue::Num(2.0));
        // neg of a stored 2 → -2. Without the override, `a`'s default is 0 → 0,
        // so a non-zero result proves the stored value was read instead.
        assert_eq!(eval0(&lower_output(&g, ctx, &Endpoint::new(neg, "result"))), -2.0);
    }

    /// A fresh oscillator lowers with its knob defaults, so it already moves —
    /// at frame 0, `offset + amp·sin(freq·0 + phase)` = 0 + 1·sin(0) = 0, but at
    /// a quarter period it reaches the amplitude, proving the knobs lowered.
    #[test]
    fn a_generator_lowers_with_its_knob_defaults() {
        let reg = reg();
        let ctx = &GraphCtx::bare(&reg);
        let mut g = NodeGraph::new();
        let osc = g.add_node("osc", Vec2::ZERO);
        let expr = lower_output(&g, ctx, &Endpoint::new(osc, "value"));
        // freq default 0.1 cyc/frame → a quarter cycle is 2.5 frames.
        let mut ctx = EvalCtx::at(2.5);
        let v = match eval_expr(&expr, &mut ctx) {
            ExprValue::Num(n) => n,
            other => panic!("{other:?}"),
        };
        assert!((v - 1.0).abs() < 1e-6, "sine peak at a quarter period, got {v}");
    }

    /// A `ref` lowers to `Expr::Ref` once a target is set, and to neutral before
    /// — a configured ref feeding math proves the reference reaches the IR.
    #[test]
    fn a_ref_lowers_to_an_expr_ref() {
        use crate::expr::PropPath;
        use crate::node::NodeId;
        let reg = reg();
        let ctx = &GraphCtx::bare(&reg);
        let mut g = NodeGraph::new();
        let r = g.add_node("ref", Vec2::ZERO);
        // Unconfigured → neutral.
        assert_eq!(lower_output(&g, ctx, &Endpoint::new(r, "value")).to_string(), "0");
        // Point it at node #7's rotation, offset -5.
        g.node_mut(r).unwrap().config.ref_target = Some((NodeId(7), PropPath::Rotation, -5.0));
        let expr = lower_output(&g, ctx, &Endpoint::new(r, "value"));
        assert!(
            matches!(&expr, Expr::Ref { node, prop, time_offset }
                if *node == NodeId(7) && *prop == PropPath::Rotation && *time_offset == -5.0),
            "got {expr:?}"
        );
    }

    /// A `param` lowers to a node-relative `Expr::Param` (reads the driven
    /// layer's own knob), and to neutral while its name is empty.
    #[test]
    fn a_param_lowers_to_a_node_relative_param() {
        let reg = reg();
        let ctx = &GraphCtx::bare(&reg);
        let mut g = NodeGraph::new();
        let p = g.add_node("param", Vec2::ZERO);
        assert_eq!(lower_output(&g, ctx, &Endpoint::new(p, "value")).to_string(), "0");
        g.node_mut(p).unwrap().config.param = "speed".into();
        let expr = lower_output(&g, ctx, &Endpoint::new(p, "value"));
        assert!(
            matches!(&expr, Expr::Param { node: None, name } if name == "speed"),
            "got {expr:?}"
        );
    }

    /// A rectangle's `geometry` output lowers to a `Shape::Rect` whose params
    /// carry the graph's expressions — an unwired size resting on the
    /// descriptor default, a wired radius animating off a generator. Resolving
    /// the shape at two frames proves the wire reached the `Value`.
    #[test]
    fn a_rectangle_lowers_to_a_shape_whose_params_come_from_the_graph() {
        let reg = reg();
        let ctx = &GraphCtx::bare(&reg);
        let mut g = NodeGraph::new();
        let rect = g.add_node("rect", Vec2::ZERO);
        let ramp = g.add_node("ramp", Vec2::new(-200.0, 0.0));
        // ramp 0 → 20 over frames 0..10, into the corner radius.
        g.node_mut(ramp).unwrap().set_value("to", ExprValue::Num(20.0));
        g.node_mut(ramp).unwrap().set_value("end", ExprValue::Num(10.0));
        g.connect(ctx, Endpoint::new(ramp, "value"), Endpoint::new(rect, "radius")).unwrap();

        let shape = lower_geometry(&g, ctx, &Endpoint::new(rect, "geometry")).unwrap();
        let crate::node::Shape::Rect { size, radius } = &shape else { panic!("{shape:?}") };
        // The unwired size rests on the descriptor's default…
        assert_eq!(size.resolve(&mut EvalCtx::at(0.0)), Vec2::new(200.0, 200.0));
        // …and the wired radius animates.
        assert_eq!(radius.resolve(&mut EvalCtx::at(0.0)), 0.0);
        assert_eq!(radius.resolve(&mut EvalCtx::at(10.0)), 20.0);
        // The shape draws — geometry authored by the graph reaches a path.
        assert!(!shape.to_path(&mut EvalCtx::at(5.0)).is_empty());
    }

    /// A shape's scalar outputs **echo** its resolved params, so math can chain
    /// off a rectangle's radius — the object-scope case the fold is for. The
    /// echo follows the wire feeding the input, not just its literal.
    #[test]
    fn a_shapes_scalar_outputs_echo_its_params_into_math() {
        let reg = reg();
        let ctx = &GraphCtx::bare(&reg);
        let mut g = NodeGraph::new();
        let rect = g.add_node("rect", Vec2::ZERO);
        let neg = g.add_node("neg", Vec2::new(200.0, 0.0));
        g.node_mut(rect).unwrap().set_value("radius", ExprValue::Num(12.0));
        g.connect(ctx, Endpoint::new(rect, "radius"), Endpoint::new(neg, "a")).unwrap();
        assert_eq!(eval0(&lower_output(&g, ctx, &Endpoint::new(neg, "result"))), -12.0);

        // Now feed the rect's radius from a value node: the echo follows the
        // wire, so the math sees 7, not the stored literal.
        let v = g.add_node("value", Vec2::new(-200.0, 0.0));
        g.node_mut(v).unwrap().set_value("value", ExprValue::Num(7.0));
        g.connect(ctx, Endpoint::new(v, "value"), Endpoint::new(rect, "radius")).unwrap();
        assert_eq!(eval0(&lower_output(&g, ctx, &Endpoint::new(neg, "result"))), -7.0);
    }

    /// `lower_geometry` answers only for a shape node's `geometry` socket: a
    /// scalar output, a non-shape kind, and a missing node all give `None`, so a
    /// driver pointed at one leaves its layer's shape alone.
    #[test]
    fn lowering_geometry_from_a_non_geometry_output_is_none() {
        let reg = reg();
        let ctx = &GraphCtx::bare(&reg);
        let mut g = NodeGraph::new();
        let rect = g.add_node("rect", Vec2::ZERO);
        let osc = g.add_node("osc", Vec2::new(200.0, 0.0));
        assert!(lower_geometry(&g, ctx, &Endpoint::new(rect, "size")).is_none());
        assert!(lower_geometry(&g, ctx, &Endpoint::new(osc, "geometry")).is_none());
        let ghost = crate::graph::GraphNodeId(999);
        assert!(lower_geometry(&g, ctx, &Endpoint::new(ghost, "geometry")).is_none());
    }

    /// An ellipse and a text node lower to their own shapes — text carrying the
    /// plain-data typography its config holds, since `ExprValue` has no string.
    #[test]
    fn the_other_shape_kinds_lower_too() {
        let reg = reg();
        let ctx = &GraphCtx::bare(&reg);
        let mut g = NodeGraph::new();
        let e = g.add_node("ellipse", Vec2::ZERO);
        assert!(matches!(
            lower_geometry(&g, ctx, &Endpoint::new(e, "geometry")),
            Some(crate::node::Shape::Ellipse { .. })
        ));

        let t = g.add_node("text", Vec2::new(0.0, 120.0));
        g.node_mut(t).unwrap().config.text.content = "hi".into();
        g.node_mut(t).unwrap().set_value("size", ExprValue::Num(48.0));
        let shape = lower_geometry(&g, ctx, &Endpoint::new(t, "geometry")).unwrap();
        let crate::node::Shape::Text { content, size, .. } = &shape else { panic!("{shape:?}") };
        assert_eq!(content, "hi");
        assert_eq!(size.resolve(&mut EvalCtx::at(0.0)), 48.0);
    }

    /// A cycle a hand-edited file smuggled past `connect` — a rect's own size
    /// echo feeding its size input — must lower, not hang.
    #[test]
    fn a_self_feeding_shape_lowers_instead_of_hanging() {
        let reg = reg();
        let ctx = &GraphCtx::bare(&reg);
        let mut g = NodeGraph::new();
        let rect = g.add_node("rect", Vec2::ZERO);
        // Forge the edge the model would refuse.
        g.edges.push(crate::graph::Edge {
            from: Endpoint::new(rect, "size"),
            to: Endpoint::new(rect, "size"),
        });
        let shape = lower_geometry(&g, ctx, &Endpoint::new(rect, "geometry")).unwrap();
        let crate::node::Shape::Rect { size, .. } = &shape else { panic!("{shape:?}") };
        assert_eq!(size.resolve(&mut EvalCtx::at(0.0)), Vec2::ZERO, "the cycle lowers to neutral");
    }

    /// A module with knobs, for the `use` tests: `amp * 2`, where `amp` is a
    /// knob defaulting to 3.
    fn module_project() -> (NodeRegistry, std::collections::BTreeMap<ModuleId, crate::node::Module>)
    {
        use crate::node::{Module, ParamValue};
        let body = Expr::Mul(
            Box::new(Expr::Param { node: None, name: "amp".into() }),
            Box::new(Expr::Lit(ExprValue::Num(2.0))),
        );
        let m = Module::new("pulse", body)
            .with_param("amp", ParamValue::Num(crate::value::Value::constant(3.0)));
        (NodeRegistry::with_builtins(), [(ModuleId(1), m)].into_iter().collect())
    }

    /// A `use` node's input sockets **are** its module's knobs — that's the
    /// per-node descriptor resolution. An unlinked `use` has none; linking one
    /// grows them, typed to match the knob.
    #[test]
    fn a_use_nodes_sockets_come_from_the_module_it_links() {
        let (reg, modules) = module_project();
        let ctx = &GraphCtx::new(&reg, &modules);
        let mut g = NodeGraph::new();
        let u = g.add_node("use", Vec2::ZERO);
        assert!(ctx.descriptor_for(g.node(u).unwrap()).unwrap().inputs.is_empty());

        g.node_mut(u).unwrap().config.module = Some(ModuleId(1));
        let desc = ctx.descriptor_for(g.node(u).unwrap()).unwrap();
        let amp = desc.find_input("amp").expect("the module's knob is a socket");
        assert_eq!(amp.ty, SocketType::Number);
        // Inheritance is the resting state, so a knob socket has no default.
        assert!(amp.default.is_none(), "an override socket must not rest on a literal");
    }

    /// The override rule: an unwired, unset knob **inherits** (no entry in the
    /// lowered `Use`), and wiring one overrides it. Inheriting must not lower to
    /// a literal copy of the default — a module default is resolved lazily in
    /// the caller's scope, so a copy would stop retiming.
    #[test]
    fn an_unwired_knob_inherits_and_a_wired_one_overrides() {
        let (reg, modules) = module_project();
        let ctx = &GraphCtx::new(&reg, &modules);
        let mut g = NodeGraph::new();
        let u = g.add_node("use", Vec2::ZERO);
        g.node_mut(u).unwrap().config.module = Some(ModuleId(1));

        let expr = lower_output(&g, ctx, &Endpoint::new(u, "value"));
        let Expr::Use { overrides, .. } = &expr else { panic!("{expr:?}") };
        assert!(overrides.is_empty(), "an untouched knob inherits, got {overrides:?}");

        // Wire a value into the knob: now it's an override.
        let v = g.add_node("value", Vec2::new(-200.0, 0.0));
        g.node_mut(v).unwrap().set_value("value", ExprValue::Num(10.0));
        g.connect(ctx, Endpoint::new(v, "value"), Endpoint::new(u, "amp")).unwrap();
        let expr = lower_output(&g, ctx, &Endpoint::new(u, "value"));
        let Expr::Use { overrides, .. } = &expr else { panic!("{expr:?}") };
        assert_eq!(overrides.len(), 1);
        assert_eq!(overrides[0].0, "amp");
        assert_eq!(overrides[0].1.to_string(), "10");
    }

    /// The type system reaches the knob sockets too: a vector can't feed a
    /// number knob, and a wire into a knob the module doesn't have is refused
    /// as a missing socket rather than silently accepted.
    #[test]
    fn a_knob_socket_is_type_checked_like_any_other() {
        let (reg, modules) = module_project();
        let ctx = &GraphCtx::new(&reg, &modules);
        let mut g = NodeGraph::new();
        let u = g.add_node("use", Vec2::ZERO);
        g.node_mut(u).unwrap().config.module = Some(ModuleId(1));
        let rect = g.add_node("rect", Vec2::new(-200.0, 0.0));
        let err = g
            .connect(ctx, Endpoint::new(rect, "size"), Endpoint::new(u, "amp"))
            .unwrap_err();
        assert!(matches!(err, crate::graph::ConnectError::TypeMismatch { .. }), "{err:?}");
        let missing =
            g.connect(ctx, Endpoint::new(rect, "radius"), Endpoint::new(u, "nope")).unwrap_err();
        assert!(matches!(missing, crate::graph::ConnectError::NoSuchSocket(_)), "{missing:?}");
    }

    /// A module's body compiled from its **own canvas** — the document scope.
    /// The graph reads the module's knob with a `param` node, so the compiled
    /// body is a real recipe, not just a constant.
    #[test]
    fn a_module_body_compiles_from_its_own_graph() {
        use crate::node::{Module, ParamValue};
        let reg = reg();
        let mut m = Module::new("pulse", Expr::Lit(ExprValue::Num(0.0)))
            .with_param("amp", ParamValue::Num(crate::value::Value::constant(3.0)));
        // amp * 2, built as nodes.
        let p = m.graph.add_node("param", Vec2::ZERO);
        m.graph.node_mut(p).unwrap().config.param = "amp".into();
        let two = m.graph.add_node("value", Vec2::new(0.0, 60.0));
        m.graph.node_mut(two).unwrap().set_value("value", ExprValue::Num(2.0));
        let mul = m.graph.add_node("mul", Vec2::new(200.0, 0.0));
        {
            let ctx = &GraphCtx::bare(&reg);
            m.graph.connect(ctx, Endpoint::new(p, "value"), Endpoint::new(mul, "a")).unwrap();
            m.graph.connect(ctx, Endpoint::new(two, "value"), Endpoint::new(mul, "b")).unwrap();
        }
        m.output = Some(Endpoint::new(mul, "result"));

        let mut modules = std::collections::BTreeMap::new();
        modules.insert(ModuleId(1), m);
        compile_modules(&mut modules, &reg);
        assert_eq!(modules[&ModuleId(1)].body.to_string(), "(param(amp) * 2)");
    }

    /// A module with no `output` keeps the body it already has — an empty
    /// canvas must not blank a body that was authored elsewhere.
    #[test]
    fn a_module_without_a_graph_output_keeps_its_body() {
        use crate::node::Module;
        let reg = reg();
        let m = Module::new("hand-built", Expr::Lit(ExprValue::Num(42.0)));
        let mut modules = std::collections::BTreeMap::new();
        modules.insert(ModuleId(1), m);
        compile_modules(&mut modules, &reg);
        assert_eq!(modules[&ModuleId(1)].body.to_string(), "42");
    }

    /// End to end at the document scope: a graph-authored module body, linked
    /// by a `use` node whose knob is overridden, evaluates to the overridden
    /// recipe — every seam in one test.
    #[test]
    fn a_graph_authored_module_evaluates_through_a_link() {
        use crate::node::{Module, ParamValue};
        let reg = reg();
        // Module `pulse`: body = param(amp), knob amp defaulting to 3.
        let mut m = Module::new("pulse", Expr::Lit(ExprValue::Num(0.0)))
            .with_param("amp", ParamValue::Num(crate::value::Value::constant(3.0)));
        let p = m.graph.add_node("param", Vec2::ZERO);
        m.graph.node_mut(p).unwrap().config.param = "amp".into();
        m.output = Some(Endpoint::new(p, "value"));
        let mut modules = std::collections::BTreeMap::new();
        modules.insert(ModuleId(1), m);
        compile_modules(&mut modules, &reg);

        // A project graph that links it. Inheriting first → the default, 3.
        let mut g = NodeGraph::new();
        let u = g.add_node("use", Vec2::ZERO);
        g.node_mut(u).unwrap().config.module = Some(ModuleId(1));
        let ctx = &GraphCtx::new(&reg, &modules);
        let doc = crate::node::Comp::new(64.0, 64.0, crate::node::Node::group(0, "root"));
        let project = crate::node::Project { modules: modules.clone(), ..crate::node::Project::single(doc) };
        let eval = |g: &NodeGraph| {
            let expr = lower_output(g, ctx, &Endpoint::new(u, "value"));
            let comp = project.comp(project.root).unwrap();
            let mut c = EvalCtx::new(comp, 0.0);
            c.modules = Some(&project.modules);
            match eval_expr(&expr, &mut c) {
                ExprValue::Num(n) => n,
                other => panic!("{other:?}"),
            }
        };
        assert_eq!(eval(&g), 3.0, "an inheriting knob takes the module's default");

        // Override it with 7 → the link now runs the body against 7.
        g.node_mut(u).unwrap().set_value("amp", ExprValue::Num(7.0));
        assert_eq!(eval(&g), 7.0, "the override reaches the module's body");
    }

    /// The layer-clock leaves lower to their `Expr::Time` readings.
    #[test]
    fn the_time_sources_lower_to_time_readings() {
        use crate::expr::TimeSource;
        let reg = reg();
        let ctx = &GraphCtx::bare(&reg);
        let mut g = NodeGraph::new();
        for (kind, want) in [
            ("localTime", TimeSource::Local),
            ("inPoint", TimeSource::In),
            ("outPoint", TimeSource::Out),
            ("t01", TimeSource::T01),
        ] {
            let n = g.add_node(kind, Vec2::ZERO);
            let expr = lower_output(&g, ctx, &Endpoint::new(n, "time"));
            assert!(matches!(&expr, Expr::Time(t) if *t == want), "{kind}: got {expr:?}");
        }
    }
}
