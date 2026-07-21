//! The generic **composition node graph**: a DAG of typed nodes wired
//! output→input, validated against the [`NodeRegistry`].
//!
//! Step 2 of the composition node graph (see the README's design section). This
//! is the authoring *model* the descriptor-driven panel reads and writes — still
//! **not** an evaluator. A node here names a registry kind by id and carries a
//! canvas position; an edge connects one node's output socket to another's
//! input. Every structural edit that could produce an illegal graph
//! ([`NodeGraph::connect`]) is checked against the registry, so the type
//! system and the DAG property are enforced at authoring time rather than
//! discovered at (a not-yet-built) lowering pass.
//!
//! Kept in `core`, headless and serializable, for the same reason the rest of
//! the model is: it must be testable without a window, and it will become
//! document data once lowering lands.

use std::collections::{HashSet, VecDeque};

use kurbo::Vec2;
use serde::{Deserialize, Serialize};

use crate::expr::PropPath;
use crate::node::NodeId;
use crate::registry::NodeRegistry;

/// Stable identity for a node *within a graph* — distinct from a scene
/// [`crate::node::NodeId`], which identifies a layer in the `Node` tree. A wire
/// names its endpoints by this, so it must survive other nodes being removed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct GraphNodeId(pub u64);

/// One placed node: which registry kind it is, where it sits on the canvas, and
/// an optional title override (else the descriptor's label is shown).
///
/// `kind` is a plain `String` — the registry key — so a node can name a
/// plugin-supplied type the core enums don't have. An unknown kind isn't a parse
/// error: it loads and [`NodeGraph::validate`] reports it, the same
/// warn-don't-fail contract a dangling reference follows elsewhere.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GraphNode {
    pub id: GraphNodeId,
    pub kind: String,
    /// Canvas position (logical points). **Saved** with the graph — unlike the
    /// old expression panel's egui-memory box positions, a composition graph's
    /// layout is part of how it was authored and must reopen unchanged.
    pub pos: Vec2,
    /// Shown instead of the descriptor label when set — a user-renamed node.
    #[serde(default)]
    pub title: Option<String>,
    /// Literal overrides for this node's scalar sockets, keyed by socket id — the
    /// value an unwired input feeds, or a `value` node's constant. **Sparse**: an
    /// absent entry means "use the descriptor's default", so lowering never needs
    /// the map to be pre-filled and a fresh node carries none. Editing a socket's
    /// field writes here.
    #[serde(default)]
    pub values: std::collections::BTreeMap<String, crate::expr::ExprValue>,
    /// Kind-specific configuration that isn't a socket value — a `ref`'s target,
    /// a `param`'s name. Sparse, like `values`: most nodes leave it default.
    #[serde(default)]
    pub config: NodeConfig,
}

/// A graph node's non-scalar settings — the addressing that a `ref` or `param`
/// node needs and a socket value can't carry. Kind-specific by design: a node
/// reads only the field its kind uses, the rest stay at their defaults.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct NodeConfig {
    /// A `ref` node's target: which scene node, which property, at what frame
    /// offset. `None` until the user picks one — lowers to a neutral value
    /// meanwhile, so an unconfigured `ref` never breaks the frame.
    #[serde(default)]
    pub ref_target: Option<(NodeId, PropPath, f64)>,
    /// A `param` node's knob name. Empty until set. Lowered as
    /// `Expr::Param { node: None, .. }`, so it reads whichever layer a driver
    /// points the graph at — the layer's *own* exposed knob.
    #[serde(default)]
    pub param: String,
}

impl GraphNode {
    /// The literal set on socket `id`, if the user has overridden it.
    pub fn value(&self, id: &str) -> Option<crate::expr::ExprValue> {
        self.values.get(id).copied()
    }

    /// Override socket `id`'s literal (an unwired input, or a `value` constant).
    pub fn set_value(&mut self, id: impl Into<String>, v: crate::expr::ExprValue) {
        self.values.insert(id.into(), v);
    }
}

/// Names one socket on one node: the addressing a wire endpoint uses. The socket
/// is a registry socket id, resolved against the node's descriptor.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Endpoint {
    pub node: GraphNodeId,
    pub socket: String,
}

impl Endpoint {
    pub fn new(node: GraphNodeId, socket: impl Into<String>) -> Self {
        Self { node, socket: socket.into() }
    }
}

/// A directed wire: an **output** socket feeds an **input** socket. Direction is
/// part of the type — `from` is always the producer — so the canvas never has to
/// guess which end is which.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Edge {
    pub from: Endpoint,
    pub to: Endpoint,
}

/// Why a [`NodeGraph::connect`] was refused. Returned rather than panicking so
/// the panel can show *why* a wire wouldn't take, and so a test can pin each
/// rule.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConnectError {
    /// An endpoint names a node that isn't in the graph.
    NodeMissing(GraphNodeId),
    /// An endpoint names a node whose kind isn't in the registry.
    UnknownKind(String),
    /// `from` isn't an output socket, or `to` isn't an input socket, on its
    /// node's descriptor (wrong side, or no such socket).
    NoSuchSocket(Endpoint),
    /// The output and input socket types don't match.
    TypeMismatch { from: crate::socket::SocketType, to: crate::socket::SocketType },
    /// The wire would close a cycle — the graph is a DAG.
    WouldCycle,
}

/// A problem found by [`NodeGraph::validate`] — the whole-graph counterpart to
/// [`ConnectError`], for checking a freshly-loaded graph rather than a single
/// edit. Same warn-don't-fail spirit: a broken graph loads and lists its
/// problems.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GraphError {
    /// A node's kind isn't registered — an unknown built-in or a missing plugin.
    UnknownKind { node: GraphNodeId, kind: String },
    /// An edge endpoint's node doesn't exist.
    DanglingEdge(Edge),
    /// An edge names a socket its node's descriptor doesn't have (on the right
    /// side), so it can't carry anything.
    BadSocket { edge: Edge, endpoint: Endpoint },
    /// An edge connects two mismatched socket types.
    TypeMismatch { edge: Edge, from: crate::socket::SocketType, to: crate::socket::SocketType },
    /// More than one wire feeds a single input socket — an input takes one.
    MultipleInputs(Endpoint),
}

/// A **driver**: a graph output socket feeding a scene layer's property. The
/// bridge from the value graph to the scene tree — the graph produces a value,
/// the binding says which property of which layer it becomes. The editor lowers
/// the output to an `Expr` and sets that property to `Value::Expr`, so
/// `evaluate` runs it like any other expression-driven property.
///
/// `prop` is core's [`PropPath`] (not the editor's `PropKind`) so a driver is
/// document data and serializes with the project.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Binding {
    pub output: Endpoint,
    pub target: NodeId,
    pub prop: PropPath,
}

/// A graph of placed nodes and the wires between them. The `next_id` counter
/// hands out stable [`GraphNodeId`]s that don't reuse a removed node's id, so a
/// stale wire can never silently reattach to a different node.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct NodeGraph {
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<Edge>,
    #[serde(default)]
    next_id: u64,
}

impl NodeGraph {
    pub fn new() -> Self {
        Self::default()
    }

    /// Place a node of `kind` at `pos`, returning its fresh id. The kind isn't
    /// checked here — a graph can hold a node whose descriptor is currently
    /// missing (a plugin not loaded); [`NodeGraph::validate`] is where that
    /// surfaces. Adding is always allowed so authoring never dead-ends on a
    /// registry that's still loading.
    pub fn add_node(&mut self, kind: impl Into<String>, pos: Vec2) -> GraphNodeId {
        let id = GraphNodeId(self.next_id);
        self.next_id += 1;
        self.nodes.push(GraphNode {
            id,
            kind: kind.into(),
            pos,
            title: None,
            values: std::collections::BTreeMap::new(),
            config: NodeConfig::default(),
        });
        id
    }

    /// Remove a node and every wire touching it. Returns whether it was there.
    pub fn remove_node(&mut self, id: GraphNodeId) -> bool {
        let before = self.nodes.len();
        self.nodes.retain(|n| n.id != id);
        if self.nodes.len() == before {
            return false;
        }
        self.edges.retain(|e| e.from.node != id && e.to.node != id);
        true
    }

    pub fn node(&self, id: GraphNodeId) -> Option<&GraphNode> {
        self.nodes.iter().find(|n| n.id == id)
    }

    pub fn node_mut(&mut self, id: GraphNodeId) -> Option<&mut GraphNode> {
        self.nodes.iter_mut().find(|n| n.id == id)
    }

    /// Connect `from` (an output) to `to` (an input), validated against `reg`.
    ///
    /// An input takes **one** wire, so a connection into an already-fed input
    /// **replaces** the old one — Blender's behaviour, and the thing that keeps
    /// `MultipleInputs` from ever arising through the normal path. Returns the
    /// edge on success.
    pub fn connect(
        &mut self,
        reg: &NodeRegistry,
        from: Endpoint,
        to: Endpoint,
    ) -> Result<Edge, ConnectError> {
        // Both endpoints' nodes and kinds must resolve.
        let from_ty = self.output_type(reg, &from)?;
        let to_ty = self.input_type(reg, &to)?;
        if !from_ty.feeds(to_ty) {
            return Err(ConnectError::TypeMismatch { from: from_ty, to: to_ty });
        }
        // A wire from an output back to a node it (transitively) feeds would
        // close a cycle. Check *before* inserting.
        if from.node == to.node || self.reaches(from.node, to.node) {
            return Err(ConnectError::WouldCycle);
        }
        // One wire per input: drop any existing feed into `to`.
        self.edges.retain(|e| e.to != to);
        let edge = Edge { from, to };
        self.edges.push(edge.clone());
        Ok(edge)
    }

    /// Remove a specific wire, returning whether it was there.
    pub fn disconnect(&mut self, edge: &Edge) -> bool {
        let before = self.edges.len();
        self.edges.retain(|e| e != edge);
        self.edges.len() != before
    }

    /// The edge feeding `input`, if any — an input has at most one.
    pub fn incoming(&self, input: &Endpoint) -> Option<&Edge> {
        self.edges.iter().find(|e| &e.to == input)
    }

    /// Every wire leaving `node`'s outputs.
    pub fn edges_from(&self, node: GraphNodeId) -> impl Iterator<Item = &Edge> {
        self.edges.iter().filter(move |e| e.from.node == node)
    }

    /// Resolve the socket type of an output endpoint, erroring the way `connect`
    /// needs (missing node / unknown kind / no such output socket).
    fn output_type(
        &self,
        reg: &NodeRegistry,
        ep: &Endpoint,
    ) -> Result<crate::socket::SocketType, ConnectError> {
        let node = self.node(ep.node).ok_or(ConnectError::NodeMissing(ep.node))?;
        let desc = reg.get(&node.kind).ok_or_else(|| ConnectError::UnknownKind(node.kind.clone()))?;
        desc.find_output(&ep.socket).map(|s| s.ty).ok_or_else(|| ConnectError::NoSuchSocket(ep.clone()))
    }

    fn input_type(
        &self,
        reg: &NodeRegistry,
        ep: &Endpoint,
    ) -> Result<crate::socket::SocketType, ConnectError> {
        let node = self.node(ep.node).ok_or(ConnectError::NodeMissing(ep.node))?;
        let desc = reg.get(&node.kind).ok_or_else(|| ConnectError::UnknownKind(node.kind.clone()))?;
        desc.find_input(&ep.socket).map(|s| s.ty).ok_or_else(|| ConnectError::NoSuchSocket(ep.clone()))
    }

    /// Whether `target` is reachable from `start` by following wires forward
    /// (output→input). Used to reject a connection that would close a cycle: if
    /// `to.node` already reaches `from.node`, wiring `from→to` would loop.
    fn reaches(&self, start: GraphNodeId, target: GraphNodeId) -> bool {
        // Forward reachability from `to.node`: does it already feed `from.node`?
        let mut seen: HashSet<GraphNodeId> = HashSet::new();
        let mut queue: VecDeque<GraphNodeId> = VecDeque::new();
        queue.push_back(target);
        while let Some(n) = queue.pop_front() {
            if n == start {
                return true;
            }
            if !seen.insert(n) {
                continue;
            }
            for e in self.edges_from(n) {
                queue.push_back(e.to.node);
            }
        }
        false
    }

    /// Check the whole graph against `reg`, collecting every problem. Empty means
    /// clean. Called after a load so a hand-edited or plugin-shy file surfaces
    /// its issues instead of failing to open — the same discipline as
    /// `Dock::is_valid` and the grid-spacing clamp.
    pub fn validate(&self, reg: &NodeRegistry) -> Vec<GraphError> {
        let mut out = Vec::new();
        for node in &self.nodes {
            if reg.get(&node.kind).is_none() {
                out.push(GraphError::UnknownKind { node: node.id, kind: node.kind.clone() });
            }
        }
        // One wire per input: report an input fed more than once.
        let mut seen_inputs: HashSet<&Endpoint> = HashSet::new();
        for edge in &self.edges {
            let out_ty = self.output_type(reg, &edge.from);
            let in_ty = self.input_type(reg, &edge.to);
            match (out_ty, in_ty) {
                (Ok(a), Ok(b)) if !a.feeds(b) => {
                    out.push(GraphError::TypeMismatch { edge: edge.clone(), from: a, to: b });
                }
                (Ok(_), Ok(_)) => {}
                // Distinguish "node/kind gone" (dangling) from "socket gone" (bad
                // socket) so the message points at the real fault.
                (a, b) => {
                    for (res, ep) in [(a, &edge.from), (b, &edge.to)] {
                        match res {
                            Err(ConnectError::NodeMissing(_)) | Err(ConnectError::UnknownKind(_)) => {
                                out.push(GraphError::DanglingEdge(edge.clone()));
                                break;
                            }
                            Err(ConnectError::NoSuchSocket(_)) => {
                                out.push(GraphError::BadSocket {
                                    edge: edge.clone(),
                                    endpoint: ep.clone(),
                                });
                            }
                            _ => {}
                        }
                    }
                }
            }
            if !seen_inputs.insert(&edge.to) {
                out.push(GraphError::MultipleInputs(edge.to.clone()));
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::NodeRegistry;
    use crate::socket::SocketType;

    fn reg() -> NodeRegistry {
        NodeRegistry::with_builtins()
    }

    #[test]
    fn a_valid_wire_connects_and_a_type_mismatch_is_refused() {
        let reg = reg();
        let mut g = NodeGraph::new();
        // rect.size (Vector output) → add.a (Number input) is a type mismatch.
        let rect = g.add_node("rect", Vec2::ZERO);
        let add = g.add_node("add", Vec2::new(200.0, 0.0));
        let err = g
            .connect(&reg, Endpoint::new(rect, "size"), Endpoint::new(add, "a"))
            .unwrap_err();
        assert_eq!(
            err,
            ConnectError::TypeMismatch { from: SocketType::Vector, to: SocketType::Number }
        );
        // rect.radius (Number output) → add.a (Number input) is fine.
        let ok = g.connect(&reg, Endpoint::new(rect, "radius"), Endpoint::new(add, "a"));
        assert!(ok.is_ok(), "{ok:?}");
        assert_eq!(g.edges.len(), 1);
    }

    #[test]
    fn an_input_takes_one_wire_and_a_second_replaces_the_first() {
        let reg = reg();
        let mut g = NodeGraph::new();
        let v1 = g.add_node("value", Vec2::ZERO);
        let v2 = g.add_node("value", Vec2::new(0.0, 60.0));
        let add = g.add_node("add", Vec2::new(200.0, 0.0));
        g.connect(&reg, Endpoint::new(v1, "value"), Endpoint::new(add, "a")).unwrap();
        g.connect(&reg, Endpoint::new(v2, "value"), Endpoint::new(add, "a")).unwrap();
        assert_eq!(g.edges.len(), 1, "the second wire replaces the first");
        assert_eq!(g.incoming(&Endpoint::new(add, "a")).unwrap().from.node, v2);
    }

    #[test]
    fn a_wire_that_would_close_a_cycle_is_refused() {
        let reg = reg();
        let mut g = NodeGraph::new();
        let a = g.add_node("neg", Vec2::ZERO);
        let b = g.add_node("neg", Vec2::new(200.0, 0.0));
        // a.result → b.a, then b.result → a.a would loop.
        g.connect(&reg, Endpoint::new(a, "result"), Endpoint::new(b, "a")).unwrap();
        let err = g
            .connect(&reg, Endpoint::new(b, "result"), Endpoint::new(a, "a"))
            .unwrap_err();
        assert_eq!(err, ConnectError::WouldCycle);
        // A node feeding its own input is the degenerate cycle.
        let self_loop = g.connect(&reg, Endpoint::new(a, "result"), Endpoint::new(a, "a"));
        assert_eq!(self_loop.unwrap_err(), ConnectError::WouldCycle);
    }

    #[test]
    fn connecting_a_missing_socket_or_node_errors() {
        let reg = reg();
        let mut g = NodeGraph::new();
        let add = g.add_node("add", Vec2::ZERO);
        // `add` has no output called "nope".
        let bad_socket = g.connect(&reg, Endpoint::new(add, "nope"), Endpoint::new(add, "a"));
        assert!(matches!(bad_socket, Err(ConnectError::NoSuchSocket(_))));
        // A node id that isn't in the graph.
        let ghost = GraphNodeId(999);
        let missing = g.connect(&reg, Endpoint::new(ghost, "value"), Endpoint::new(add, "a"));
        assert_eq!(missing, Err(ConnectError::NodeMissing(ghost)));
    }

    #[test]
    fn removing_a_node_drops_its_wires_and_ids_are_not_reused() {
        let reg = reg();
        let mut g = NodeGraph::new();
        let v = g.add_node("value", Vec2::ZERO);
        let add = g.add_node("add", Vec2::new(200.0, 0.0));
        g.connect(&reg, Endpoint::new(v, "value"), Endpoint::new(add, "a")).unwrap();
        assert!(g.remove_node(v));
        assert!(g.edges.is_empty(), "the incident wire went with the node");
        // A fresh node must not reuse the removed id, or the dropped wire could
        // silently reattach.
        let w = g.add_node("value", Vec2::ZERO);
        assert_ne!(w, v);
    }

    /// The graph and its drivers are document data: a project must carry them
    /// through a save/load, and a `.pbc` written before they existed must still
    /// load (with empty defaults) rather than fail to parse.
    #[test]
    fn a_project_persists_its_graph_and_drivers() {
        use crate::expr::PropPath;
        use crate::node::{Comp, Node, NodeId, Project};

        let mut project = Project::single(Comp::new(64.0, 64.0, Node::group(0, "root")));
        let osc = project.graph.add_node("osc", Vec2::new(20.0, 20.0));
        project.bindings.push(Binding {
            output: Endpoint::new(osc, "value"),
            target: NodeId(0),
            prop: PropPath::Rotation,
        });

        let json = serde_json::to_string(&project).unwrap();
        let back: Project = serde_json::from_str(&json).unwrap();
        assert_eq!(back.graph, project.graph);
        assert_eq!(back.bindings, project.bindings);

        // A legacy project JSON with neither field loads with empty defaults.
        let mut legacy: serde_json::Value = serde_json::from_str(&json).unwrap();
        let obj = legacy.as_object_mut().unwrap();
        obj.remove("graph");
        obj.remove("bindings");
        let old: Project = serde_json::from_value(legacy).unwrap();
        assert!(old.graph.nodes.is_empty() && old.bindings.is_empty());
    }

    #[test]
    fn validate_reports_unknown_kinds_and_survives_a_round_trip() {
        let reg = reg();
        let mut g = NodeGraph::new();
        g.add_node("acme.missing", Vec2::ZERO); // plugin not loaded
        let good = g.add_node("value", Vec2::new(0.0, 60.0));
        let add = g.add_node("add", Vec2::new(200.0, 0.0));
        g.connect(&reg, Endpoint::new(good, "value"), Endpoint::new(add, "a")).unwrap();

        let problems = g.validate(&reg);
        assert!(problems.iter().any(|p| matches!(
            p,
            GraphError::UnknownKind { kind, .. } if kind == "acme.missing"
        )));

        // The graph is document data: it must survive serde unchanged.
        let json = serde_json::to_string(&g).unwrap();
        let back: NodeGraph = serde_json::from_str(&json).unwrap();
        assert_eq!(back, g);
    }
}
