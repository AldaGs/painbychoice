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
//! Scope of this first pass: the value / math / generator subset, the nodes that
//! map straight onto `Expr` scalars. `ref` / `param` / `use` / the time sources
//! / geometry nodes need target-addressing data the model doesn't carry yet;
//! they lower to a neutral literal for now (never a panic), and get their real
//! lowering in a later increment.

use std::collections::HashSet;

use crate::expr::{Expr, ExprValue, Generator, Waveform};
use crate::graph::{Endpoint, GraphNodeId, NodeGraph};
use crate::registry::NodeRegistry;
use crate::socket::SocketType;
use crate::value::Color;

/// Lower the value produced at `output` (a node's output socket) to an `Expr`.
///
/// The graph is a DAG — [`NodeGraph::connect`] rejects a cycle — but a
/// hand-edited file could still carry one, so the walk guards against back-edges
/// (a re-entered node lowers to a neutral literal, the same warn-don't-hang
/// contract the evaluator's cycle cache follows) rather than recursing forever.
pub fn lower_output(graph: &NodeGraph, reg: &NodeRegistry, output: &Endpoint) -> Expr {
    lower_out(graph, reg, output, &mut HashSet::new())
}

fn lower_out(
    graph: &NodeGraph,
    reg: &NodeRegistry,
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
            b(lower_in(graph, reg, output.node, "a", visiting)),
            b(lower_in(graph, reg, output.node, "b", visiting)),
        ),
        "mul" => Expr::Mul(
            b(lower_in(graph, reg, output.node, "a", visiting)),
            b(lower_in(graph, reg, output.node, "b", visiting)),
        ),
        "neg" => Expr::Neg(b(lower_in(graph, reg, output.node, "a", visiting))),
        "osc" => Expr::Gen(Generator::Oscillator {
            freq: b(lower_in(graph, reg, output.node, "freq", visiting)),
            amp: b(lower_in(graph, reg, output.node, "amp", visiting)),
            phase: b(lower_in(graph, reg, output.node, "phase", visiting)),
            offset: b(lower_in(graph, reg, output.node, "offset", visiting)),
            // A per-node waveform isn't stored yet, so the lowered oscillator is
            // a sine — the generator's own default. Storing/editing the waveform
            // is a later refinement.
            wave: Waveform::Sine,
        }),
        "noise" => Expr::Gen(Generator::Noise {
            freq: b(lower_in(graph, reg, output.node, "freq", visiting)),
            amp: b(lower_in(graph, reg, output.node, "amp", visiting)),
            seed: b(lower_in(graph, reg, output.node, "seed", visiting)),
        }),
        "ramp" => Expr::Gen(Generator::Ramp {
            from: b(lower_in(graph, reg, output.node, "from", visiting)),
            to: b(lower_in(graph, reg, output.node, "to", visiting)),
            start: b(lower_in(graph, reg, output.node, "start", visiting)),
            end: b(lower_in(graph, reg, output.node, "end", visiting)),
        }),
        "bounce" => Expr::Gen(Generator::Bounce {
            amp: b(lower_in(graph, reg, output.node, "amp", visiting)),
            freq: b(lower_in(graph, reg, output.node, "freq", visiting)),
            decay: b(lower_in(graph, reg, output.node, "decay", visiting)),
        }),
        // ref / param / use / time sources / geometry — not lowered in this
        // pass. Neutral rather than a panic, so a graph mixing them still lowers.
        _ => neutral(),
    };
    visiting.remove(&output.node);
    expr
}

/// Lower the value feeding input socket `socket` of `node`: follow its wire if
/// there is one, else use the node's stored literal, else the descriptor's
/// default, else the socket type's neutral.
fn lower_in(
    graph: &NodeGraph,
    reg: &NodeRegistry,
    node: GraphNodeId,
    socket: &str,
    visiting: &mut HashSet<GraphNodeId>,
) -> Expr {
    let ep = Endpoint::new(node, socket);
    if let Some(edge) = graph.incoming(&ep) {
        return lower_out(graph, reg, &edge.from, visiting);
    }
    // Unwired: the literal to feed. A user override wins; otherwise the
    // descriptor's default; otherwise the socket type's neutral.
    let n = graph.node(node);
    let stored = n.and_then(|n| n.value(socket));
    let by_desc = n
        .and_then(|n| reg.get(&n.kind))
        .and_then(|d| d.find_input(socket))
        .map(|s| s.default.unwrap_or_else(|| neutral_for(s.ty)));
    Expr::Lit(stored.or(by_desc).unwrap_or(ExprValue::Num(0.0)))
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
    use kurbo::Vec2;

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
        let mut g = NodeGraph::new();
        let a = g.add_node("value", Vec2::ZERO);
        let b_ = g.add_node("value", Vec2::new(0.0, 60.0));
        let add = g.add_node("add", Vec2::new(200.0, 0.0));
        g.node_mut(a).unwrap().set_value("value", ExprValue::Num(3.0));
        g.node_mut(b_).unwrap().set_value("value", ExprValue::Num(4.0));
        g.connect(&reg, Endpoint::new(a, "value"), Endpoint::new(add, "a")).unwrap();
        g.connect(&reg, Endpoint::new(b_, "value"), Endpoint::new(add, "b")).unwrap();

        let expr = lower_output(&g, &reg, &Endpoint::new(add, "result"));
        assert_eq!(expr.to_string(), "(3 + 4)");
        assert_eq!(eval0(&expr), 7.0);
    }

    /// An unwired input takes the descriptor default: an `add` with only `a`
    /// wired reads `b`'s default (0), and a `mul` reads its default (1) — so the
    /// two operators lower to different resting values from the same wiring.
    #[test]
    fn an_unwired_input_uses_the_descriptor_default() {
        let reg = reg();
        let mut g = NodeGraph::new();
        let v = g.add_node("value", Vec2::ZERO);
        g.node_mut(v).unwrap().set_value("value", ExprValue::Num(5.0));

        let add = g.add_node("add", Vec2::new(200.0, 0.0));
        g.connect(&reg, Endpoint::new(v, "value"), Endpoint::new(add, "a")).unwrap();
        // b is unwired → its default 0 → 5 + 0 = 5.
        assert_eq!(eval0(&lower_output(&g, &reg, &Endpoint::new(add, "result"))), 5.0);

        let mul = g.add_node("mul", Vec2::new(200.0, 120.0));
        g.connect(&reg, Endpoint::new(v, "value"), Endpoint::new(mul, "a")).unwrap();
        // b is unwired → mul's default 1 → 5 * 1 = 5 (not 0, which add would give).
        assert_eq!(eval0(&lower_output(&g, &reg, &Endpoint::new(mul, "result"))), 5.0);
    }

    /// A user override on an unwired socket beats the descriptor default.
    #[test]
    fn a_stored_override_beats_the_default() {
        let reg = reg();
        let mut g = NodeGraph::new();
        let neg = g.add_node("neg", Vec2::ZERO);
        g.node_mut(neg).unwrap().set_value("a", ExprValue::Num(2.0));
        // neg of a stored 2 → -2. Without the override, `a`'s default is 0 → 0,
        // so a non-zero result proves the stored value was read instead.
        assert_eq!(eval0(&lower_output(&g, &reg, &Endpoint::new(neg, "result"))), -2.0);
    }

    /// A fresh oscillator lowers with its knob defaults, so it already moves —
    /// at frame 0, `offset + amp·sin(freq·0 + phase)` = 0 + 1·sin(0) = 0, but at
    /// a quarter period it reaches the amplitude, proving the knobs lowered.
    #[test]
    fn a_generator_lowers_with_its_knob_defaults() {
        let reg = reg();
        let mut g = NodeGraph::new();
        let osc = g.add_node("osc", Vec2::ZERO);
        let expr = lower_output(&g, &reg, &Endpoint::new(osc, "value"));
        // freq default 0.1 cyc/frame → a quarter cycle is 2.5 frames.
        let mut ctx = EvalCtx::at(2.5);
        let v = match eval_expr(&expr, &mut ctx) {
            ExprValue::Num(n) => n,
            other => panic!("{other:?}"),
        };
        assert!((v - 1.0).abs() < 1e-6, "sine peak at a quarter period, got {v}");
    }

    /// An as-yet-unlowered kind (a `ref`) lowers to a neutral zero rather than
    /// panicking, so a mixed graph still compiles.
    #[test]
    fn an_unsupported_kind_lowers_to_neutral() {
        let reg = reg();
        let mut g = NodeGraph::new();
        let r = g.add_node("ref", Vec2::ZERO);
        assert_eq!(eval0(&lower_output(&g, &reg, &Endpoint::new(r, "value"))), 0.0);
    }
}
