//! The node-type **registry**: `kind-id → NodeDescriptor`, plus the descriptor
//! and category types it stores.
//!
//! This is the keystone of the composition node graph (see the README's design
//! section). A descriptor is **metadata** — a node type's category, label, and
//! typed sockets — and *nothing more*: it does not evaluate. The registry is the
//! seam that makes "adding a new object/layer/effect/plugin already integrates
//! itself as a node" true: a built-in registers a descriptor at startup, a
//! plugin registers one at load, and the descriptor-driven canvas draws either
//! without knowing which it is. Lowering a graph of these to the `Node`/`Expr`
//! IR (the thing `evaluate` actually runs) is a **separate, later** step, kept
//! out of here so this layer stays pure metadata and never becomes a second
//! evaluator.

use std::collections::BTreeMap;

use crate::socket::{Socket, SocketType};

/// A node type's category. Groups the palette, and is the axis the
/// "auto-integrate" story sorts on — register a descriptor under a category and
/// it appears in that palette section for free, built-in or plugin alike.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NodeCategory {
    /// Produces geometry — a shape primitive (rectangle, ellipse, text).
    Geometry,
    /// Numeric / vector / colour math — inputs and a result.
    Math,
    /// Reads a value in: a literal, a reference to another node's property, an
    /// exposed parameter, or a layer-local clock.
    Input,
    /// A procedural motion generator (osc / noise / ramp / bounce).
    Generator,
    /// A shared-module link — reuse of a document-level driver.
    Module,
    /// A whole layer or composition instance. Structural nodes that lower to the
    /// `Node` tree / `precomp` rather than to an `Expr`.
    Layer,
    /// A pixel effect. **Behind the compositor stage, which isn't built** — a
    /// descriptor in this category is a stub until that lands.
    Effect,
    /// A matte / mask source. Also gated on the compositor stage.
    Matte,
}

impl NodeCategory {
    pub const ALL: [NodeCategory; 8] = [
        NodeCategory::Geometry,
        NodeCategory::Math,
        NodeCategory::Input,
        NodeCategory::Generator,
        NodeCategory::Module,
        NodeCategory::Layer,
        NodeCategory::Effect,
        NodeCategory::Matte,
    ];

    pub fn label(self) -> &'static str {
        match self {
            NodeCategory::Geometry => "Geometry",
            NodeCategory::Math => "Math",
            NodeCategory::Input => "Input",
            NodeCategory::Generator => "Generator",
            NodeCategory::Module => "Module",
            NodeCategory::Layer => "Layer",
            NodeCategory::Effect => "Effect",
            NodeCategory::Matte => "Matte",
        }
    }

    /// Whether nodes in this category can actually *do* anything yet, or are
    /// stubs waiting on the compositor stage. The palette uses this to mark (and
    /// a test to pin) that we don't ship a promise we can't evaluate.
    pub fn is_buildable_now(self) -> bool {
        !matches!(self, NodeCategory::Effect | NodeCategory::Matte)
    }
}

/// A node type described for the graph: its stable kind id, its category and
/// label, and its typed input / output sockets. **Pure metadata** — enough to
/// draw the node and validate a wire, nothing about evaluation.
///
/// Owned fields throughout, because a plugin builds one of these at runtime
/// through the exact same API a built-in uses.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NodeDescriptor {
    /// The registry key and how a graph node names its type. Built-ins mirror
    /// the IR they will lower to (`"rect"`, `"add"`, `"osc"`); a plugin uses a
    /// namespaced id (`"acme.blur"`) so it can't collide with a built-in.
    pub id: String,
    pub category: NodeCategory,
    /// What the node's title bar shows.
    pub label: String,
    pub inputs: Vec<Socket>,
    pub outputs: Vec<Socket>,
}

impl NodeDescriptor {
    /// Start a descriptor. Sockets are added with [`Self::input`] / [`Self::output`].
    pub fn new(id: impl Into<String>, category: NodeCategory, label: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            category,
            label: label.into(),
            inputs: Vec::new(),
            outputs: Vec::new(),
        }
    }

    /// Add an input socket (builder form).
    pub fn input(mut self, id: impl Into<String>, label: impl Into<String>, ty: SocketType) -> Self {
        self.inputs.push(Socket::new(id, label, ty));
        self
    }

    /// Add an output socket (builder form).
    pub fn output(mut self, id: impl Into<String>, label: impl Into<String>, ty: SocketType) -> Self {
        self.outputs.push(Socket::new(id, label, ty));
        self
    }

    /// Look an input socket up by id.
    pub fn find_input(&self, id: &str) -> Option<&Socket> {
        self.inputs.iter().find(|s| s.id == id)
    }

    /// Look an output socket up by id.
    pub fn find_output(&self, id: &str) -> Option<&Socket> {
        self.outputs.iter().find(|s| s.id == id)
    }

    /// Whether every socket id is unique within its side. A wire names an
    /// endpoint by `(node, socket-id)`, so a duplicate id on one side would make
    /// that endpoint ambiguous. The registry checks this at registration.
    fn socket_ids_are_unique(&self) -> bool {
        let unique = |sockets: &[Socket]| {
            let mut ids: Vec<&str> = sockets.iter().map(|s| s.id.as_str()).collect();
            ids.sort_unstable();
            let n = ids.len();
            ids.dedup();
            ids.len() == n
        };
        unique(&self.inputs) && unique(&self.outputs)
    }
}

/// Why a descriptor was refused registration. Returned rather than panicking so
/// a plugin's bad descriptor is a reportable error, not a crash of the host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RegisterError {
    /// Another descriptor already claims this id.
    DuplicateId(String),
    /// A socket id repeats within the inputs or within the outputs.
    DuplicateSocket(String),
}

/// The node-type registry. Keeps descriptors by id **and** in registration
/// order, so the palette lists them deterministically (built-ins in a sensible
/// order, plugins appended as they load) while lookup stays O(log n).
#[derive(Clone, Debug, Default)]
pub struct NodeRegistry {
    by_id: BTreeMap<String, NodeDescriptor>,
    order: Vec<String>,
}

impl NodeRegistry {
    /// An empty registry. [`NodeRegistry::with_builtins`] is the usual entry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a descriptor. Rejects a duplicate kind id or a repeated socket
    /// id rather than silently overwriting — a plugin clobbering a built-in (or
    /// itself) is a bug the host should see, not swallow.
    pub fn register(&mut self, desc: NodeDescriptor) -> Result<(), RegisterError> {
        if self.by_id.contains_key(&desc.id) {
            return Err(RegisterError::DuplicateId(desc.id));
        }
        if !desc.socket_ids_are_unique() {
            return Err(RegisterError::DuplicateSocket(desc.id));
        }
        self.order.push(desc.id.clone());
        self.by_id.insert(desc.id.clone(), desc);
        Ok(())
    }

    /// Look up a descriptor by kind id.
    pub fn get(&self, id: &str) -> Option<&NodeDescriptor> {
        self.by_id.get(id)
    }

    /// Whether a kind id is registered.
    pub fn contains(&self, id: &str) -> bool {
        self.by_id.contains_key(id)
    }

    /// How many descriptors are registered.
    pub fn len(&self) -> usize {
        self.order.len()
    }

    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }

    /// Descriptors in registration order — the palette's order.
    pub fn iter(&self) -> impl Iterator<Item = &NodeDescriptor> {
        self.order.iter().map(|id| &self.by_id[id])
    }

    /// Descriptors in one category, in registration order — for a palette
    /// section.
    pub fn by_category(&self, category: NodeCategory) -> impl Iterator<Item = &NodeDescriptor> {
        self.iter().filter(move |d| d.category == category)
    }

    /// A registry pre-loaded with every built-in node type — the ones that lower
    /// to today's IR (geometry, math, inputs, generators, modules). Effect and
    /// matte nodes are **deliberately absent**: they need the compositor stage,
    /// and a palette entry that can't evaluate would be a false promise. They
    /// join here when that stage lands.
    ///
    /// Built-ins go through the same `register` a plugin uses — dogfooding the
    /// seam — so the `expect` only fires on a programming error in this list
    /// (a duplicate id), never on user or plugin input.
    pub fn with_builtins() -> Self {
        let mut reg = Self::new();
        for desc in builtin_descriptors() {
            reg.register(desc).expect("built-in descriptors are unique and well-formed");
        }
        reg
    }
}

/// The built-in descriptors, in palette order. A free function so the list is
/// testable on its own (uniqueness, socket well-formedness) without a registry.
///
/// Each mirrors the IR it will lower to in step 3 — the shapes' params, the
/// operators' operands, the generators' knobs (names taken straight from
/// `Generator::knob_labels`) — so lowering is a rename, not a redesign.
pub fn builtin_descriptors() -> Vec<NodeDescriptor> {
    // `Geometry`/`Layer`/`Matte` name both a category and a socket type, so the
    // category is aliased and the socket types are globbed — bare names below
    // are always socket types.
    use NodeCategory as Cat;
    use SocketType::*;
    vec![
        // ── Geometry: a shape's params are inputs; it outputs its geometry and
        //    echoes its resolved params so math can chain off them. ───────────
        NodeDescriptor::new("rect", Cat::Geometry, "Rectangle")
            .input("size", "Size", Vector)
            .input("radius", "Radius", Number)
            .output("geometry", "Geometry", Geometry)
            .output("size", "Size", Vector)
            .output("radius", "Radius", Number),
        NodeDescriptor::new("ellipse", Cat::Geometry, "Ellipse")
            .input("size", "Size", Vector)
            .output("geometry", "Geometry", Geometry)
            .output("size", "Size", Vector),
        NodeDescriptor::new("text", Cat::Geometry, "Text")
            .input("size", "Font Size", Number)
            .output("geometry", "Geometry", Geometry),
        // ── Math: an input (or two) and a result. ────────────────────────────
        NodeDescriptor::new("add", Cat::Math, "Add")
            .input("a", "A", Number)
            .input("b", "B", Number)
            .output("result", "Result", Number),
        NodeDescriptor::new("mul", Cat::Math, "Multiply")
            .input("a", "A", Number)
            .input("b", "B", Number)
            .output("result", "Result", Number),
        NodeDescriptor::new("neg", Cat::Math, "Negate")
            .input("a", "A", Number)
            .output("result", "Result", Number),
        // ── Inputs: leaves that read a value in. ─────────────────────────────
        NodeDescriptor::new("value", Cat::Input, "Value").output("value", "Value", Number),
        NodeDescriptor::new("ref", Cat::Input, "Reference").output("value", "Value", Number),
        NodeDescriptor::new("param", Cat::Input, "Parameter").output("value", "Value", Number),
        NodeDescriptor::new("localTime", Cat::Input, "Local Time").output("time", "Time", Time),
        NodeDescriptor::new("inPoint", Cat::Input, "In Point").output("time", "Time", Time),
        NodeDescriptor::new("outPoint", Cat::Input, "Out Point").output("time", "Time", Time),
        NodeDescriptor::new("t01", Cat::Input, "Progress (t01)").output("value", "Value", Number),
        // ── Generators: typed-knob motion primitives (knob names match the IR).
        NodeDescriptor::new("osc", Cat::Generator, "Oscillator")
            .input("freq", "Freq", Number)
            .input("amp", "Amp", Number)
            .input("phase", "Phase", Number)
            .input("offset", "Offset", Number)
            .output("value", "Value", Number),
        NodeDescriptor::new("noise", Cat::Generator, "Noise")
            .input("freq", "Freq", Number)
            .input("amp", "Amp", Number)
            .input("seed", "Seed", Number)
            .output("value", "Value", Number),
        NodeDescriptor::new("ramp", Cat::Generator, "Ramp")
            .input("from", "From", Number)
            .input("to", "To", Number)
            .input("start", "Start", Time)
            .input("end", "End", Time)
            .output("value", "Value", Number),
        NodeDescriptor::new("bounce", Cat::Generator, "Bounce")
            .input("amp", "Amp", Number)
            .input("freq", "Freq", Number)
            .input("decay", "Decay", Number)
            .output("value", "Value", Number),
        // ── Module reuse. ────────────────────────────────────────────────────
        NodeDescriptor::new("use", Cat::Module, "Module").output("value", "Value", Number),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtins_register_cleanly_and_are_findable() {
        let reg = NodeRegistry::with_builtins();
        assert_eq!(reg.len(), builtin_descriptors().len());
        // A representative id from each buildable-now category resolves.
        for id in ["rect", "add", "value", "osc", "use"] {
            assert!(reg.contains(id), "{id} should be a built-in");
        }
    }

    #[test]
    fn a_duplicate_kind_id_is_refused_not_overwritten() {
        let mut reg = NodeRegistry::new();
        let d = NodeDescriptor::new("dup", NodeCategory::Math, "One").output("v", "V", SocketType::Number);
        reg.register(d).unwrap();
        let clash = NodeDescriptor::new("dup", NodeCategory::Input, "Two");
        assert_eq!(reg.register(clash), Err(RegisterError::DuplicateId("dup".into())));
        // The original survives untouched.
        assert_eq!(reg.get("dup").unwrap().label, "One");
    }

    #[test]
    fn a_descriptor_with_a_repeated_socket_id_is_refused() {
        let mut reg = NodeRegistry::new();
        let bad = NodeDescriptor::new("bad", NodeCategory::Math, "Bad")
            .input("a", "A", SocketType::Number)
            .input("a", "Also A", SocketType::Number);
        assert_eq!(reg.register(bad), Err(RegisterError::DuplicateSocket("bad".into())));
        assert!(reg.is_empty(), "a refused descriptor must not be stored");
    }

    /// A plugin registers through the same path a built-in does, and its node
    /// then lists and looks up like any other — the whole point of the registry.
    #[test]
    fn a_plugin_descriptor_registers_like_a_builtin() {
        let mut reg = NodeRegistry::with_builtins();
        let before = reg.len();
        let plugin = NodeDescriptor::new("acme.blur", NodeCategory::Effect, "Acme Blur")
            .input("layer", "Layer", SocketType::Layer)
            .input("radius", "Radius", SocketType::Number)
            .output("layer", "Layer", SocketType::Layer);
        reg.register(plugin).unwrap();
        assert_eq!(reg.len(), before + 1);
        let got = reg.get("acme.blur").unwrap();
        assert_eq!(got.category, NodeCategory::Effect);
        assert_eq!(got.find_input("radius").unwrap().ty, SocketType::Number);
        // It's the only Effect node so far, and it's a stub until the compositor.
        assert!(!NodeCategory::Effect.is_buildable_now());
        assert_eq!(reg.by_category(NodeCategory::Effect).count(), 1);
    }

    /// `by_category` preserves registration order and only returns that category.
    #[test]
    fn by_category_filters_and_keeps_order() {
        let reg = NodeRegistry::with_builtins();
        let math: Vec<_> = reg.by_category(NodeCategory::Math).map(|d| d.id.as_str()).collect();
        assert_eq!(math, ["add", "mul", "neg"]);
    }

    /// Every built-in must be in a category the palette can evaluate today —
    /// this pins the "don't ship effect/matte nodes before the compositor" rule
    /// so adding one prematurely fails the build.
    #[test]
    fn no_builtin_is_gated_on_the_unbuilt_compositor() {
        for d in builtin_descriptors() {
            assert!(
                d.category.is_buildable_now(),
                "built-in '{}' is in {:?}, which needs the compositor stage",
                d.id,
                d.category,
            );
        }
    }

    /// The generators' knob sockets must match the IR's knob names exactly, or
    /// step 3's lowering can't line them up. Guards against the two lists
    /// drifting.
    #[test]
    fn generator_sockets_match_the_ir_knob_names() {
        let reg = NodeRegistry::with_builtins();
        let knobs = |id: &str| -> Vec<String> {
            reg.get(id).unwrap().inputs.iter().map(|s| s.id.clone()).collect()
        };
        assert_eq!(knobs("osc"), ["freq", "amp", "phase", "offset"]);
        assert_eq!(knobs("noise"), ["freq", "amp", "seed"]);
        assert_eq!(knobs("ramp"), ["from", "to", "start", "end"]);
        assert_eq!(knobs("bounce"), ["amp", "freq", "decay"]);
    }
}
