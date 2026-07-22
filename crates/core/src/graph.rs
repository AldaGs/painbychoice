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

use std::borrow::Cow;
use std::collections::{HashSet, VecDeque};

use kurbo::Vec2;
use serde::{Deserialize, Serialize};

use crate::expr::{BinOp, MathOp, PropPath, UnOp, Waveform};
use crate::node::{ModuleId, NodeId};
use crate::registry::{NodeDescriptor, NodeRegistry};
use crate::socket::Socket;
use crate::text::TextAlign;

/// Everything needed to resolve a graph node's **descriptor**: the registry of
/// node kinds, plus the project's modules.
///
/// The registry alone answers `kind → descriptor`, which is enough while every
/// node of a kind has the same sockets. A `use` node breaks that: its inputs are
/// the *linked module's* knobs, so two `use` nodes in one graph have different
/// shapes. Resolution therefore has to happen per **placed node**, not per kind
/// — see [`GraphCtx::descriptor_for`] — and everything that reads sockets
/// (connecting, validating, lowering, drawing) goes through this rather than
/// touching the registry directly.
///
/// A borrowing view, built per call: it holds no state of its own, so it can't
/// go stale against a project that just changed.
#[derive(Clone, Copy)]
pub struct GraphCtx<'a> {
    pub reg: &'a NodeRegistry,
    pub modules: &'a std::collections::BTreeMap<ModuleId, crate::node::Module>,
}

/// A module map with nothing in it — for the tests and callers that have no
/// project, so `GraphCtx` needn't be `Option`al.
static NO_MODULES: std::sync::LazyLock<std::collections::BTreeMap<ModuleId, crate::node::Module>> =
    std::sync::LazyLock::new(Default::default);

impl<'a> GraphCtx<'a> {
    pub fn new(
        reg: &'a NodeRegistry,
        modules: &'a std::collections::BTreeMap<ModuleId, crate::node::Module>,
    ) -> Self {
        Self { reg, modules }
    }

    /// A context over `reg` with no modules — for a graph that links none, and
    /// for tests of the kinds that don't care.
    pub fn bare(reg: &'a NodeRegistry) -> Self {
        Self { reg, modules: &NO_MODULES }
    }

    /// The descriptor for one **placed** node: its kind's static descriptor,
    /// specialized where the node's config changes its shape.
    ///
    /// Today the only specialization is a `use` node, which grows one input
    /// socket per knob of the module it links — that's what makes an override
    /// *wireable* by any node instead of a literal typed into a side panel.
    /// Borrowed (free) for every other kind; the clone happens only for a `use`
    /// that actually resolves a module.
    ///
    /// **An override socket has no default, deliberately.** Unwired and unset
    /// means *inherit* the module's own default — override is a layering, not a
    /// fork — so there is no resting literal for lowering to read. See
    /// [`crate::lower`].
    pub fn descriptor_for(&self, node: &GraphNode) -> Option<Cow<'a, NodeDescriptor>> {
        let desc = self.reg.get(&node.kind)?;
        match node.kind.as_str() {
            "use" => {
                // An unlinked `use`, or one whose module was deleted, keeps the
                // bare descriptor: no knobs to offer. It lowers to neutral anyway.
                let Some(def) = node.config.module.and_then(|m| self.modules.get(&m)) else {
                    return Some(Cow::Borrowed(desc));
                };
                let mut specialized = desc.clone();
                for p in &def.params {
                    specialized
                        .inputs
                        .push(Socket::new(&p.name, &p.name, param_socket_type(&p.value)));
                }
                Some(Cow::Owned(specialized))
            }
            // A Math node's arity and resting operands follow its operator: a
            // unary op has no second operand to show, and Multiply's identity
            // is 1 where Add's is 0. Both come from the op, so picking one from
            // the node's own list reshapes it.
            "math" => {
                let mut specialized = desc.clone();
                match node.config.math_op {
                    MathOp::Bin(op) => {
                        let (a, b) = op.seed_operands();
                        if let Some(s) = specialized.inputs.get_mut(0) {
                            s.default = Some(crate::expr::ExprValue::Num(a));
                        }
                        if let Some(s) = specialized.inputs.get_mut(1) {
                            s.default = Some(crate::expr::ExprValue::Num(b));
                        }
                    }
                    // Unary: the `b` socket doesn't exist. Dropping it rather
                    // than hiding it is what makes a wire into it impossible
                    // instead of merely invisible.
                    MathOp::Un(_) => specialized.inputs.truncate(1),
                }
                Some(Cow::Owned(specialized))
            }
            // An `out` node's input is the *property* it drives, so its type
            // follows the target rather than the kind. An untargeted one keeps
            // the bare `Number` socket — it drives nothing until it's pointed
            // somewhere, and a typeless socket would accept a wire it might
            // later have to drop.
            "out" => {
                let Some((_, prop)) = node.config.out_target else {
                    return Some(Cow::Borrowed(desc));
                };
                let mut specialized = desc.clone();
                if let Some(s) = specialized.inputs.first_mut() {
                    s.ty = prop.socket_type();
                }
                Some(Cow::Owned(specialized))
            }
            _ => Some(Cow::Borrowed(desc)),
        }
    }
}

/// The socket type a module knob presents. [`ParamValue`] mirrors the
/// `ExprValue` kinds one for one, so this is total and exact.
///
/// [`ParamValue`]: crate::node::ParamValue
fn param_socket_type(v: &crate::node::ParamValue) -> crate::socket::SocketType {
    use crate::node::ParamValue;
    use crate::socket::SocketType;
    match v {
        ParamValue::Num(_) => SocketType::Number,
        ParamValue::Vec(_) => SocketType::Vector,
        ParamValue::Color(_) => SocketType::Color,
        ParamValue::Str(_) => SocketType::Text,
    }
}

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
    /// A `script` node's Rhai source. Empty until written; lowers to neutral
    /// while empty so a blank script never errors a frame.
    #[serde(default)]
    pub script: String,
    /// A `use` node's linked shared module. `None` until picked. Overrides
    /// aren't yet editable on this canvas, so a linked module runs at its
    /// defaults (`Expr::Use` with no overrides).
    #[serde(default)]
    pub module: Option<ModuleId>,
    /// An `osc` node's waveform. Not a socket: it selects *which* function the
    /// generator is, not a value fed into one, so there's nothing for a wire to
    /// carry. Defaults to `Sine`, the generator's own default, so an `osc`
    /// placed before this field existed reads back unchanged.
    #[serde(default)]
    pub wave: Waveform,
    /// A `text` node's non-animatable typography — the fields
    /// [`crate::node::Shape::Text`] holds as plain data because [`ExprValue`]
    /// has no string variant. Only the font *size* is a socket, so everything
    /// else a text shape needs has to live here.
    ///
    /// [`ExprValue`]: crate::expr::ExprValue
    #[serde(default)]
    pub text: TextConfig,
    /// An `out` node's target: which scene layer's which property this driver
    /// feeds. `None` until picked — an untargeted sink is inert, so a node
    /// dropped on the canvas doesn't seize a property before you've said which.
    ///
    /// This field plus the wire into the node's `value` socket *are* the driver
    /// (see [`NodeGraph::bindings`]); there is no separate list.
    #[serde(default)]
    pub out_target: Option<(NodeId, PropPath)>,
    /// A `shapeOut` node's target layer. Names no property, for the same reason
    /// [`ShapeBinding`] doesn't: the shape *is* what's bound.
    #[serde(default)]
    pub out_shape: Option<NodeId>,
    /// A `math` node's operator. Not a socket: it selects *which* function the
    /// node is, not a value fed into one — the same reason an `osc`'s waveform
    /// is config. Defaults to Add, so a node placed before this field existed
    /// reads back as the `add` it used to be.
    #[serde(default)]
    pub math_op: MathOp,
}

/// The plain-data half of a `text` node — everything
/// [`crate::node::Shape::Text`] carries that isn't the animatable `size`
/// socket. Split into its own struct so [`NodeConfig`] doesn't sprout four
/// loose text fields, and so lowering can hand the whole group over at once.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TextConfig {
    /// System font family name. Empty (or not installed) → sans-serif.
    ///
    /// `content` used to live here beside it and no longer does — it became a
    /// real `Text` **input socket** once `ExprValue` grew a string, so it can be
    /// wired, keyframed, and scripted like every other param. `family` stays
    /// config because it names a *system* font: a lookup key, not a value.
    pub family: String,
    pub align: TextAlign,
    /// Wrap width; `None` keeps the text on one line.
    pub max_width: Option<f64>,
}

impl Default for TextConfig {
    fn default() -> Self {
        Self { family: String::new(), align: TextAlign::Left, max_width: None }
    }
}

/// A **geometry driver**: a graph `geometry` output feeding a scene layer's
/// *shape*. The counterpart to [`Binding`], and the thing that lets the graph
/// **author** geometry rather than only drive numbers into a shape someone else
/// made — a bound layer's `Shape` is rebuilt from the graph on every recompile,
/// its params carrying the lowered `Expr`s as `Value::Expr`.
///
/// Deliberately *not* folded into `Binding`: a `Binding` names a [`PropPath`],
/// and geometry has none — it isn't an interpolatable `Value`, which is the
/// same reason `SocketType::Geometry` has no `ExprValue`. One binding kind per
/// thing bound keeps both honest.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ShapeBinding {
    pub output: Endpoint,
    pub target: NodeId,
}

impl GraphNode {
    /// The literal set on socket `id`, if the user has overridden it.
    pub fn value(&self, id: &str) -> Option<crate::expr::ExprValue> {
        self.values.get(id).cloned()
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
        ctx: &GraphCtx,
        from: Endpoint,
        to: Endpoint,
    ) -> Result<Edge, ConnectError> {
        // Both endpoints' nodes and kinds must resolve.
        let from_ty = self.output_type(ctx, &from)?;
        let to_ty = self.input_type(ctx, &to)?;
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

    /// Drop whatever feeds `input`, returning whether anything did. The
    /// endpoint-addressed form of [`Self::disconnect`], for when the caller
    /// knows the socket but not the wire — retyping an `out` node's socket, say.
    pub fn disconnect_input(&mut self, input: &Endpoint) -> bool {
        let before = self.edges.len();
        self.edges.retain(|e| &e.to != input);
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

    /// The **drivers this graph declares**: one per `out` node that both names a
    /// target and has something wired into it.
    ///
    /// Derived, never stored. A driver is a fact *about* the graph — an `out`
    /// node's config plus the wire feeding it — so reading it back out is the
    /// only way it can't drift from what the canvas shows. The old parallel
    /// `Project::bindings` list could disagree with the graph (a wire deleted
    /// under a binding, a binding pointing at a node that's gone); this can't
    /// represent that state at all.
    ///
    /// An `out` node that's untargeted, or unwired, contributes nothing rather
    /// than a half-driver: both are the resting state of a node you just placed.
    pub fn bindings(&self) -> Vec<Binding> {
        self.nodes
            .iter()
            .filter(|n| n.kind == "out")
            .filter_map(|n| {
                let (target, prop) = n.config.out_target?;
                let edge = self.incoming(&Endpoint::new(n.id, "value"))?;
                Some(Binding { output: edge.from.clone(), target, prop })
            })
            .collect()
    }

    /// The **geometry drivers** this graph declares — [`Self::bindings`] for
    /// `shapeOut` nodes, on the same derive-don't-store terms.
    pub fn shape_bindings(&self) -> Vec<ShapeBinding> {
        self.nodes
            .iter()
            .filter(|n| n.kind == "shapeOut")
            .filter_map(|n| {
                let target = n.config.out_shape?;
                let edge = self.incoming(&Endpoint::new(n.id, "geometry"))?;
                Some(ShapeBinding { output: edge.from.clone(), target })
            })
            .collect()
    }

    /// Place an `out` node driving `target`'s `prop` from `output`, and wire it
    /// — how anything that binds programmatically (the fold's import, a load-time
    /// migration) says "make this a driver".
    ///
    /// The wire is pushed directly rather than through [`Self::connect`]: the
    /// node is brand new, so neither the one-wire-per-input rule nor the cycle
    /// rule can be at stake, and a sink has no outputs to close a loop with.
    /// Types line up by construction — the socket is retyped from `prop` — and a
    /// caller passing a mismatched output would rather see it drive nothing than
    /// have the binding silently dropped on the floor.
    pub fn bind_output(&mut self, output: Endpoint, target: NodeId, prop: PropPath) -> GraphNodeId {
        let id = self.add_node("out", self.sink_pos(&output));
        self.node_mut(id).expect("just added").config.out_target = Some((target, prop));
        self.edges.push(Edge { from: output, to: Endpoint::new(id, "value") });
        id
    }

    /// [`Self::bind_output`] for geometry: a `shapeOut` node driving `target`'s
    /// shape.
    pub fn bind_geometry(&mut self, output: Endpoint, target: NodeId) -> GraphNodeId {
        let id = self.add_node("shapeOut", self.sink_pos(&output));
        self.node_mut(id).expect("just added").config.out_shape = Some(target);
        self.edges.push(Edge { from: output, to: Endpoint::new(id, "geometry") });
        id
    }

    /// Rewrite the node kinds that were **folded into another kind**, so a graph
    /// authored before the fold still evaluates.
    ///
    /// Only `add`/`mul`/`neg` → `math` so far. Cheap enough to be worth doing
    /// even though nothing shipped with those kinds: they're plain strings, an
    /// unrecognised one merely draws red and lowers to nothing, and a graph
    /// silently going inert is a bad way to find that out.
    pub fn migrate_kinds(&mut self) {
        for n in &mut self.nodes {
            let op = match n.kind.as_str() {
                "add" => MathOp::Bin(BinOp::Add),
                "mul" => MathOp::Bin(BinOp::Mul),
                "neg" => MathOp::Un(UnOp::Neg),
                _ => continue,
            };
            n.kind = "math".to_string();
            n.config.math_op = op;
        }
    }

    /// Where a sink node lands: just right of the node it reads, so the wire it
    /// arrives with is short and reads left-to-right like every other. Falls back
    /// to the origin when the source is missing, which only a caller binding a
    /// dangling endpoint can produce.
    fn sink_pos(&self, output: &Endpoint) -> Vec2 {
        self.node(output.node).map_or(Vec2::ZERO, |n| n.pos + Vec2::new(220.0, 0.0))
    }

    /// Resolve the socket type of an output endpoint, erroring the way `connect`
    /// needs (missing node / unknown kind / no such output socket).
    fn output_type(
        &self,
        ctx: &GraphCtx,
        ep: &Endpoint,
    ) -> Result<crate::socket::SocketType, ConnectError> {
        let node = self.node(ep.node).ok_or(ConnectError::NodeMissing(ep.node))?;
        let desc =
            ctx.descriptor_for(node).ok_or_else(|| ConnectError::UnknownKind(node.kind.clone()))?;
        desc.find_output(&ep.socket).map(|s| s.ty).ok_or_else(|| ConnectError::NoSuchSocket(ep.clone()))
    }

    fn input_type(
        &self,
        ctx: &GraphCtx,
        ep: &Endpoint,
    ) -> Result<crate::socket::SocketType, ConnectError> {
        let node = self.node(ep.node).ok_or(ConnectError::NodeMissing(ep.node))?;
        let desc =
            ctx.descriptor_for(node).ok_or_else(|| ConnectError::UnknownKind(node.kind.clone()))?;
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
    pub fn validate(&self, ctx: &GraphCtx) -> Vec<GraphError> {
        let mut out = Vec::new();
        for node in &self.nodes {
            if ctx.descriptor_for(node).is_none() {
                out.push(GraphError::UnknownKind { node: node.id, kind: node.kind.clone() });
            }
        }
        // One wire per input: report an input fed more than once.
        let mut seen_inputs: HashSet<&Endpoint> = HashSet::new();
        for edge in &self.edges {
            let out_ty = self.output_type(ctx, &edge.from);
            let in_ty = self.input_type(ctx, &edge.to);
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

    /// The built-in registry, kept alive by the caller so a `GraphCtx` can
    /// borrow it. Tests that link no module use `GraphCtx::bare`.
    fn reg() -> NodeRegistry {
        NodeRegistry::with_builtins()
    }

    #[test]
    fn a_valid_wire_connects_and_a_type_mismatch_is_refused() {
        let reg = reg();
        let ctx = &GraphCtx::bare(&reg);
        let mut g = NodeGraph::new();
        // rect.size (Vector output) → add.a (Number input) is a type mismatch.
        let rect = g.add_node("rect", Vec2::ZERO);
        let add = g.add_node("math", Vec2::new(200.0, 0.0));
        let err = g
            .connect(ctx, Endpoint::new(rect, "size"), Endpoint::new(add, "a"))
            .unwrap_err();
        assert_eq!(
            err,
            ConnectError::TypeMismatch { from: SocketType::Vector, to: SocketType::Number }
        );
        // rect.radius (Number output) → add.a (Number input) is fine.
        let ok = g.connect(ctx, Endpoint::new(rect, "radius"), Endpoint::new(add, "a"));
        assert!(ok.is_ok(), "{ok:?}");
        assert_eq!(g.edges.len(), 1);
    }

    #[test]
    fn an_input_takes_one_wire_and_a_second_replaces_the_first() {
        let reg = reg();
        let ctx = &GraphCtx::bare(&reg);
        let mut g = NodeGraph::new();
        let v1 = g.add_node("value", Vec2::ZERO);
        let v2 = g.add_node("value", Vec2::new(0.0, 60.0));
        let add = g.add_node("math", Vec2::new(200.0, 0.0));
        g.connect(ctx, Endpoint::new(v1, "value"), Endpoint::new(add, "a")).unwrap();
        g.connect(ctx, Endpoint::new(v2, "value"), Endpoint::new(add, "a")).unwrap();
        assert_eq!(g.edges.len(), 1, "the second wire replaces the first");
        assert_eq!(g.incoming(&Endpoint::new(add, "a")).unwrap().from.node, v2);
    }

    #[test]
    fn a_wire_that_would_close_a_cycle_is_refused() {
        let reg = reg();
        let ctx = &GraphCtx::bare(&reg);
        let mut g = NodeGraph::new();
        let a = g.add_node("math", Vec2::ZERO);
        g.node_mut(a).unwrap().config.math_op = MathOp::Un(UnOp::Neg);
        let b = g.add_node("math", Vec2::new(200.0, 0.0));
        g.node_mut(b).unwrap().config.math_op = MathOp::Un(UnOp::Neg);
        // a.result → b.a, then b.result → a.a would loop.
        g.connect(ctx, Endpoint::new(a, "result"), Endpoint::new(b, "a")).unwrap();
        let err = g
            .connect(ctx, Endpoint::new(b, "result"), Endpoint::new(a, "a"))
            .unwrap_err();
        assert_eq!(err, ConnectError::WouldCycle);
        // A node feeding its own input is the degenerate cycle.
        let self_loop = g.connect(ctx, Endpoint::new(a, "result"), Endpoint::new(a, "a"));
        assert_eq!(self_loop.unwrap_err(), ConnectError::WouldCycle);
    }

    #[test]
    fn connecting_a_missing_socket_or_node_errors() {
        let reg = reg();
        let ctx = &GraphCtx::bare(&reg);
        let mut g = NodeGraph::new();
        let add = g.add_node("math", Vec2::ZERO);
        // `add` has no output called "nope".
        let bad_socket = g.connect(ctx, Endpoint::new(add, "nope"), Endpoint::new(add, "a"));
        assert!(matches!(bad_socket, Err(ConnectError::NoSuchSocket(_))));
        // A node id that isn't in the graph.
        let ghost = GraphNodeId(999);
        let missing = g.connect(ctx, Endpoint::new(ghost, "value"), Endpoint::new(add, "a"));
        assert_eq!(missing, Err(ConnectError::NodeMissing(ghost)));
    }

    #[test]
    fn removing_a_node_drops_its_wires_and_ids_are_not_reused() {
        let reg = reg();
        let ctx = &GraphCtx::bare(&reg);
        let mut g = NodeGraph::new();
        let v = g.add_node("value", Vec2::ZERO);
        let add = g.add_node("math", Vec2::new(200.0, 0.0));
        g.connect(ctx, Endpoint::new(v, "value"), Endpoint::new(add, "a")).unwrap();
        assert!(g.remove_node(v));
        assert!(g.edges.is_empty(), "the incident wire went with the node");
        // A fresh node must not reuse the removed id, or the dropped wire could
        // silently reattach.
        let w = g.add_node("value", Vec2::ZERO);
        assert_ne!(w, v);
    }

    /// A Math node **reshapes itself** from its operator: a unary op has no `B`
    /// socket at all, and each op rests at its own identity. Dropping the socket
    /// rather than hiding it is what makes a wire into it impossible instead of
    /// merely invisible.
    #[test]
    fn a_math_nodes_shape_follows_its_operator() {
        use crate::expr::ExprValue;
        let reg = NodeRegistry::with_builtins();
        let ctx = GraphCtx::bare(&reg);
        let mut g = NodeGraph::new();
        let m = g.add_node("math", Vec2::ZERO);

        let shape = |g: &NodeGraph| {
            let d = ctx.descriptor_for(g.node(m).unwrap()).unwrap().into_owned();
            let defaults: Vec<_> = d.inputs.iter().map(|s| s.default.clone()).collect();
            (d.inputs.len(), defaults)
        };
        // Add, the default: two operands resting at its identity, 0.
        assert_eq!(shape(&g), (2, vec![Some(ExprValue::Num(0.0)); 2]));

        // Multiply: still two, but resting at 1 — an unwired operand must pass
        // its input through, not annihilate it.
        g.node_mut(m).unwrap().config.math_op = MathOp::Bin(BinOp::Mul);
        assert_eq!(shape(&g), (2, vec![Some(ExprValue::Num(1.0)); 2]));

        // Square Root: one operand. The `b` socket is gone, so a wire to it is
        // refused by the same rule that refuses any socket that doesn't exist.
        g.node_mut(m).unwrap().config.math_op = MathOp::Un(UnOp::Sqrt);
        assert_eq!(shape(&g).0, 1);
        let v = g.add_node("value", Vec2::new(-200.0, 0.0));
        assert!(matches!(
            g.connect(&ctx, Endpoint::new(v, "value"), Endpoint::new(m, "b")),
            Err(ConnectError::NoSuchSocket(_)),
        ));
    }

    /// A graph authored when Add and Multiply were their own kinds still
    /// evaluates: the kinds fold into `math` with the matching operator, rather
    /// than going unrecognised and silently lowering to nothing.
    #[test]
    fn the_retired_operator_kinds_fold_into_the_math_node() {
        use crate::node::{Comp, Node, Project};
        let mut project = Project::single(Comp::new(64.0, 64.0, Node::group(0, "root")));
        let add = project.graph.add_node("add", Vec2::ZERO);
        let mul = project.graph.add_node("mul", Vec2::new(0.0, 100.0));
        let neg = project.graph.add_node("neg", Vec2::new(0.0, 200.0));

        project.migrate();

        let op = |id| project.graph.node(id).unwrap().config.math_op;
        let kind = |id| project.graph.node(id).unwrap().kind.clone();
        assert_eq!((kind(add), op(add)), ("math".to_string(), MathOp::Bin(BinOp::Add)));
        assert_eq!((kind(mul), op(mul)), ("math".to_string(), MathOp::Bin(BinOp::Mul)));
        assert_eq!((kind(neg), op(neg)), ("math".to_string(), MathOp::Un(UnOp::Neg)));
        // And they're real Math nodes now — the registry knows the kind, so they
        // draw and lower like any other.
        let reg = NodeRegistry::with_builtins();
        assert!(GraphCtx::bare(&reg).descriptor_for(project.graph.node(mul).unwrap()).is_some());
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
        project.graph.bind_output(Endpoint::new(osc, "value"), NodeId(0), PropPath::Rotation);
        // A geometry driver, a text node's typography config, and a string
        // socket literal ride along too — all three are document data, so all
        // three must survive the trip.
        let text = project.graph.add_node("text", Vec2::new(20.0, 120.0));
        let tn = project.graph.node_mut(text).unwrap();
        tn.config.text.family = "Georgia".into();
        tn.set_value("content", crate::expr::ExprValue::Str("hello".into()));
        project.graph.bind_geometry(Endpoint::new(text, "geometry"), NodeId(0));

        let json = serde_json::to_string(&project).unwrap();
        let back: Project = serde_json::from_str(&json).unwrap();
        assert_eq!(back.graph, project.graph);
        // The drivers rode in the graph, so they come back derived from it.
        assert_eq!(back.graph.bindings(), project.graph.bindings());
        assert_eq!(back.graph.shape_bindings(), project.graph.shape_bindings());

        // A legacy project JSON with no graph at all loads with empty defaults.
        let mut legacy: serde_json::Value = serde_json::from_str(&json).unwrap();
        legacy.as_object_mut().unwrap().remove("graph");
        let old: Project = serde_json::from_value(legacy).unwrap();
        assert!(old.graph.nodes.is_empty() && old.graph.bindings().is_empty());
        assert!(old.graph.shape_bindings().is_empty());
    }

    /// A `.pbc` written **before drivers were nodes** carried them in two lists
    /// beside the graph. Loading one must turn each into the sink node that
    /// replaced it — otherwise every driver in every existing project silently
    /// stops driving, since nothing reads those lists any more.
    #[test]
    fn a_pre_node_projects_driver_lists_migrate_into_sink_nodes() {
        use crate::expr::PropPath;
        use crate::node::{Comp, Node, NodeId, Project};

        let mut project = Project::single(Comp::new(64.0, 64.0, Node::group(0, "root")));
        let osc = project.graph.add_node("osc", Vec2::new(20.0, 20.0));
        let rect = project.graph.add_node("rect", Vec2::new(20.0, 120.0));
        // Hand-build the old on-disk shape: a graph with no sinks, plus the two
        // lists. The fields are private now, so JSON is the only way to say it —
        // which is the point, since JSON is the only place it still exists.
        let mut json: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&project).unwrap()).unwrap();
        let obj = json.as_object_mut().unwrap();
        obj.insert(
            "bindings".into(),
            serde_json::json!([{
                "output": { "node": osc, "socket": "value" },
                "target": NodeId(0),
                "prop": PropPath::Rotation,
            }]),
        );
        obj.insert(
            "shape_bindings".into(),
            serde_json::json!([{
                "output": { "node": rect, "socket": "geometry" },
                "target": NodeId(0),
            }]),
        );

        let mut loaded: Project = serde_json::from_value(json).unwrap();
        // Before migration the lists are still lists, so the graph shows nothing.
        assert!(loaded.graph.bindings().is_empty(), "a sink can't exist before migration");
        loaded.migrate();

        assert_eq!(
            loaded.graph.bindings(),
            [Binding {
                output: Endpoint::new(osc, "value"),
                target: NodeId(0),
                prop: PropPath::Rotation,
            }],
        );
        assert_eq!(
            loaded.graph.shape_bindings(),
            [ShapeBinding { output: Endpoint::new(rect, "geometry"), target: NodeId(0) }],
        );
        // And they're real, visible, editable nodes — not a hidden list.
        assert_eq!(loaded.graph.nodes.iter().filter(|n| n.kind == "out").count(), 1);
        assert_eq!(loaded.graph.nodes.iter().filter(|n| n.kind == "shapeOut").count(), 1);

        // Saving now writes the nodes and *not* the legacy keys, so the file
        // converts once and a second migrate has nothing left to do.
        let round = serde_json::to_string(&loaded).unwrap();
        assert!(!round.contains("\"bindings\""), "the legacy lists must not be written back");
        let mut again: Project = serde_json::from_str(&round).unwrap();
        again.migrate();
        assert_eq!(again.graph.nodes.len(), loaded.graph.nodes.len());
    }

    /// An `out` node's input socket is typed by the property it targets, so the
    /// canvas can refuse a wire that would drive a colour from a number.
    #[test]
    fn an_out_nodes_socket_follows_its_target_property() {
        use crate::expr::PropPath;
        use crate::node::NodeId;

        let reg = reg();
        let ctx = GraphCtx::bare(&reg);
        let mut g = NodeGraph::new();
        let out = g.add_node("out", Vec2::ZERO);
        // Untargeted: the bare descriptor's resting Number socket.
        assert_eq!(ctx.descriptor_for(g.node(out).unwrap()).unwrap().inputs[0].ty, SocketType::Number);

        g.node_mut(out).unwrap().config.out_target = Some((NodeId(0), PropPath::Fill));
        assert_eq!(ctx.descriptor_for(g.node(out).unwrap()).unwrap().inputs[0].ty, SocketType::Color);

        // …and the type rule then applies to it like any other socket.
        let osc = g.add_node("osc", Vec2::new(0.0, 60.0));
        assert!(matches!(
            g.connect(&ctx, Endpoint::new(osc, "value"), Endpoint::new(out, "value")),
            Err(ConnectError::TypeMismatch { .. }),
        ));
    }

    /// A driver exists only where an `out` node has *both* a target and a wire.
    /// Deriving rather than storing is what makes that true — there is no state
    /// in which the list says one thing and the canvas shows another.
    #[test]
    fn a_driver_needs_both_a_target_and_a_wire() {
        use crate::expr::PropPath;
        use crate::node::NodeId;

        let reg = reg();
        let ctx = GraphCtx::bare(&reg);
        let mut g = NodeGraph::new();
        let osc = g.add_node("osc", Vec2::ZERO);
        let out = g.add_node("out", Vec2::new(220.0, 0.0));

        // Wired but untargeted.
        g.connect(&ctx, Endpoint::new(osc, "value"), Endpoint::new(out, "value")).unwrap();
        assert!(g.bindings().is_empty());

        // Targeted and wired.
        g.node_mut(out).unwrap().config.out_target = Some((NodeId(7), PropPath::Rotation));
        assert_eq!(g.bindings().len(), 1);

        // Pulling the wire retires the driver — no stale row left behind.
        let edge = g.incoming(&Endpoint::new(out, "value")).unwrap().clone();
        g.disconnect(&edge);
        assert!(g.bindings().is_empty());
    }

    #[test]
    fn validate_reports_unknown_kinds_and_survives_a_round_trip() {
        let reg = reg();
        let ctx = &GraphCtx::bare(&reg);
        let mut g = NodeGraph::new();
        g.add_node("acme.missing", Vec2::ZERO); // plugin not loaded
        let good = g.add_node("value", Vec2::new(0.0, 60.0));
        let add = g.add_node("math", Vec2::new(200.0, 0.0));
        g.connect(ctx, Endpoint::new(good, "value"), Endpoint::new(add, "a")).unwrap();

        let problems = g.validate(ctx);
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
