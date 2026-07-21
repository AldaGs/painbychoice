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
//! `lower(raise(e)) == e` for the lowerable subset (value / math / generator /
//! ref / param / time), so a round trip is lossless there; a `Script`/`Use`
//! (no node kind yet) raises to a placeholder rather than panicking.

use kurbo::Vec2;

use crate::expr::{Expr, ExprValue, Generator, TimeSource};
use crate::graph::{Endpoint, NodeGraph};
use crate::registry::NodeRegistry;

/// Column width and row height for the auto-layout — children sit one column to
/// the left of their parent (output flows left→right), stacked down the rows.
const COL: f64 = 184.0;
const ROW: f64 = 96.0;

/// Raise `expr` into `graph`, laying its nodes out with the root near `at`, and
/// return the output [`Endpoint`] that produces its value — ready to wire into a
/// driver or another node.
pub fn raise(graph: &mut NodeGraph, reg: &NodeRegistry, expr: &Expr, at: Vec2) -> Endpoint {
    let mut cursor_y = at.y;
    raise_rec(graph, reg, expr, at.x, &mut cursor_y).0
}

/// Returns the created root's output endpoint and the y it was placed at (so a
/// parent can centre on the span of its children).
fn raise_rec(
    graph: &mut NodeGraph,
    reg: &NodeRegistry,
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
            let (id, y) = leaf(graph, "value");
            graph.node_mut(id).unwrap().set_value("value", *v);
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
        Expr::Add(a, b) => op2(graph, reg, "add", a, b, x, cursor_y),
        Expr::Mul(a, b) => op2(graph, reg, "mul", a, b, x, cursor_y),
        Expr::Neg(a) => {
            let (ea, cy) = raise_rec(graph, reg, a, x - COL, cursor_y);
            let id = graph.add_node("neg", Vec2::new(x, cy));
            let _ = graph.connect(reg, ea, Endpoint::new(id, "a"));
            (Endpoint::new(id, "result"), cy)
        }
        Expr::Gen(g) => raise_generator(graph, reg, g, x, cursor_y),
        // No node kind for these yet — a placeholder keeps raising total (never a
        // panic), and it's visible so the loss is obvious rather than silent.
        Expr::Script(_) | Expr::Use { .. } => {
            let (id, y) = leaf(graph, "value");
            graph.node_mut(id).unwrap().set_value("value", ExprValue::Num(0.0));
            (Endpoint::new(id, "value"), y)
        }
    }
}

/// Raise a two-operand operator (`add`/`mul`): raise both children, place the
/// operator centred on them, wire them into `a`/`b`.
fn op2(
    graph: &mut NodeGraph,
    reg: &NodeRegistry,
    kind: &str,
    a: &Expr,
    b: &Expr,
    x: f64,
    cursor_y: &mut f64,
) -> (Endpoint, f64) {
    let (ea, ya) = raise_rec(graph, reg, a, x - COL, cursor_y);
    let (eb, yb) = raise_rec(graph, reg, b, x - COL, cursor_y);
    let center = (ya + yb) / 2.0;
    let id = graph.add_node(kind, Vec2::new(x, center));
    let _ = graph.connect(reg, ea, Endpoint::new(id, "a"));
    let _ = graph.connect(reg, eb, Endpoint::new(id, "b"));
    (Endpoint::new(id, "result"), center)
}

/// Raise a generator, wiring each knob `Expr` into the matching input socket.
fn raise_generator(
    graph: &mut NodeGraph,
    reg: &NodeRegistry,
    g: &Generator,
    x: f64,
    cursor_y: &mut f64,
) -> (Endpoint, f64) {
    // (kind, knob sockets in order) — the knob Exprs line up with these.
    let (kind, knobs): (&str, Vec<(&str, &Expr)>) = match g {
        Generator::Oscillator { freq, amp, phase, offset, .. } => (
            "osc",
            vec![("freq", freq), ("amp", amp), ("phase", phase), ("offset", offset)],
        ),
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
        let (e, y) = raise_rec(graph, reg, knob, x - COL, cursor_y);
        centers.push(y);
        endpoints.push((*socket, e));
    }
    let center = (centers.first().copied().unwrap_or(*cursor_y)
        + centers.last().copied().unwrap_or(*cursor_y))
        / 2.0;
    let id = graph.add_node(kind, Vec2::new(x, center));
    for (socket, e) in endpoints {
        let _ = graph.connect(reg, e, Endpoint::new(id, socket));
    }
    (Endpoint::new(id, "value"), center)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::expr::{PropPath, Waveform};
    use crate::lower::lower_output;
    use crate::node::NodeId;

    fn reg() -> NodeRegistry {
        NodeRegistry::with_builtins()
    }

    /// The fold's core guarantee: raising an expression into a graph and lowering
    /// it back reproduces the original, for the lowerable subset. Checked by the
    /// printed form, which captures structure and every leaf value.
    fn round_trips(expr: Expr) {
        let reg = reg();
        let mut g = NodeGraph::new();
        let ep = raise(&mut g, &reg, &expr, Vec2::ZERO);
        let back = lower_output(&g, &reg, &ep);
        assert_eq!(back.to_string(), expr.to_string(), "round trip changed the expression");
    }

    #[test]
    fn math_round_trips() {
        // ((3 * frameRef) + -(2)) — a nested tree of operators and leaves.
        let expr = Expr::Add(
            Box::new(Expr::Mul(
                Box::new(Expr::Lit(ExprValue::Num(3.0))),
                Box::new(Expr::Ref { node: NodeId(2), prop: PropPath::Rotation, time_offset: 0.0 }),
            )),
            Box::new(Expr::Neg(Box::new(Expr::Lit(ExprValue::Num(2.0))))),
        );
        round_trips(expr);
    }

    #[test]
    fn a_generator_with_wired_knobs_round_trips() {
        // osc whose amp is itself an expression, not just a literal.
        let expr = Expr::Gen(Generator::Oscillator {
            freq: Box::new(Expr::Lit(ExprValue::Num(0.2))),
            amp: Box::new(Expr::Mul(
                Box::new(Expr::Lit(ExprValue::Num(10.0))),
                Box::new(Expr::Param { node: None, name: "gain".into() }),
            )),
            phase: Box::new(Expr::Lit(ExprValue::Num(0.0))),
            offset: Box::new(Expr::Lit(ExprValue::Num(0.0))),
            wave: Waveform::Sine,
        });
        round_trips(expr);
    }

    #[test]
    fn a_time_source_feeding_math_round_trips() {
        // localTime into a mul — proves a Time output wires into a Number input.
        let expr = Expr::Mul(
            Box::new(Expr::Time(TimeSource::Local)),
            Box::new(Expr::Lit(ExprValue::Num(4.0))),
        );
        round_trips(expr);
    }
}
