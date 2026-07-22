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
///
/// Not `Eq`: a socket's `default` carries an `ExprValue` (holds `f64`s), so a
/// descriptor is only `PartialEq`. It's never a map key, so that's enough.
#[derive(Clone, Debug, PartialEq)]
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

    /// Add an input socket with no unwired default — one that must be wired
    /// (geometry / layer / matte), or whose neutral is left to lowering.
    pub fn input(mut self, id: impl Into<String>, label: impl Into<String>, ty: SocketType) -> Self {
        self.inputs.push(Socket::new(id, label, ty));
        self
    }

    /// Add an input socket whose unwired value is `default` — the resting value
    /// lowering reads when nothing feeds the socket (a generator knob's default,
    /// a math operand's identity).
    pub fn input_def(
        mut self,
        id: impl Into<String>,
        label: impl Into<String>,
        ty: SocketType,
        default: crate::expr::ExprValue,
    ) -> Self {
        self.inputs.push(Socket::with_default(id, label, ty, default));
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
    // Unwired input defaults (math operands, generator knobs) mirror the IR's
    // own seed values, so lowering a fresh node reproduces `Expr::seed`.
    let num = crate::expr::ExprValue::Num;
    // Shape sizes are planar, so the two-argument spelling stays — it just
    // builds the one vector kind with a zero depth.
    let vec2 = |x, y| crate::expr::ExprValue::Vec3(crate::vec3::Vec3::flat(x, y));
    let text = |s: &str| crate::expr::ExprValue::Str(s.to_string());
    vec![
        // ── Geometry: a shape's params are inputs; it outputs its geometry and
        //    echoes its resolved params so math can chain off them. An echo
        //    output shares its input's id and type — that pairing *is* the echo,
        //    and `lower_out` reads it structurally rather than per-kind.
        //
        //    Their defaults match the layers panel's add-shape seeds, so a rect
        //    dropped on the canvas is the same rect the toolbar makes. ─────────
        NodeDescriptor::new("rect", Cat::Geometry, "Rectangle")
            .input_def("size", "Size", Vector, vec2(200.0, 200.0))
            .input_def("radius", "Radius", Number, num(0.0))
            .output("geometry", "Geometry", Geometry)
            .output("size", "Size", Vector)
            .output("radius", "Radius", Number),
        NodeDescriptor::new("ellipse", Cat::Geometry, "Ellipse")
            .input_def("size", "Size", Vector, vec2(200.0, 200.0))
            .output("geometry", "Geometry", Geometry)
            .output("size", "Size", Vector),
        // `content` is a wirable input like any other param — that is what makes
        // a typewriter a wire from a script node rather than a built-in effect.
        // Its default is the same visible placeholder the add-text button uses:
        // an empty string shapes to an empty path and reads as a broken node.
        NodeDescriptor::new("text", Cat::Geometry, "Text")
            .input_def("content", "Content", Text, text("Text"))
            .input_def("size", "Font Size", Number, num(96.0))
            .output("geometry", "Geometry", Geometry)
            .output("content", "Content", Text)
            .output("size", "Font Size", Number),
        // ── Math: **one** node for every operator. ───────────────────────────
        //
        // Add and Multiply were separate kinds, and every operator we wanted
        // next (subtract, divide, power, min, sqrt, trig) would have been
        // another. They differ only in which `f64 → f64 → f64` they apply, so
        // as separate kinds the palette would grow a wall of near-identical
        // entries and every one would need its own lowering arm. Which operator
        // it runs is *config*, like an oscillator's waveform.
        //
        // Safe in a way a `value`/`string` merge would not be: every mode is
        // Number → Number, so the output type — the thing that colours a wire —
        // never changes under you. What *does* change is the arity, and
        // `GraphCtx::descriptor_for` drops the `b` socket for a unary op, the
        // same per-placed-node specialization a `use` node's knobs use.
        //
        // The defaults here are Add's identity; `descriptor_for` re-seeds them
        // from the chosen operator, so an unwired Multiply rests at 1, not 0.
        NodeDescriptor::new("math", Cat::Math, "Math")
            .input_def("a", "A", Number, num(0.0))
            .input_def("b", "B", Number, num(0.0))
            .output("result", "Result", Number),
        // ── Vector plumbing: build a vector from scalars, and take it apart
        //    again. These are what let a graph reach *inside* a
        //    multidimensional property — drive `position.x` from one recipe and
        //    `position.y` from another, or read a layer's scale apart to react to
        //    just its width. `split` is the built-in with the most value
        //    outputs, one per axis.
        //
        //    Three axes since 2.5D. A graph built when there were two reloads
        //    with its `z` input unwired, resting on the `0.0` default — i.e. in
        //    the plane, exactly where it was authored. ─────────────────────────
        NodeDescriptor::new("join", Cat::Math, "Join Vector")
            .input_def("x", "X", Number, num(0.0))
            .input_def("y", "Y", Number, num(0.0))
            .input_def("z", "Z", Number, num(0.0))
            .output("value", "Vector", Vector),
        NodeDescriptor::new("split", Cat::Math, "Split Vector")
            .input("value", "Vector", Vector)
            .output("x", "X", Number)
            .output("y", "Y", Number)
            .output("z", "Z", Number),
        // ── Inputs: leaves that read a value in. ─────────────────────────────
        NodeDescriptor::new("value", Cat::Input, "Value").output("value", "Value", Number),
        // A text literal. Separate from `value` rather than a mode of it: the
        // socket type is what the canvas colours a wire by, and one node that
        // changed its output type under you would make a graph unreadable.
        NodeDescriptor::new("string", Cat::Input, "String").output("value", "Value", Text),
        // A vector constant — `value` at two dimensions. Its Vec2 rides on the
        // output socket (edited inline as two drag fields), so a graph can pin a
        // `position` or `scale` without wiring two `join` inputs by hand.
        NodeDescriptor::new("vec2", Cat::Input, "Vector").output("value", "Value", Vector),
        NodeDescriptor::new("param", Cat::Input, "Parameter").output("value", "Value", Number),
        // A leaf holding Rhai source — pulls from `frame`, not wired inputs.
        NodeDescriptor::new("script", Cat::Input, "Script").output("value", "Value", Number),
        NodeDescriptor::new("localTime", Cat::Input, "Local Time").output("time", "Time", Time),
        NodeDescriptor::new("inPoint", Cat::Input, "In Point").output("time", "Time", Time),
        NodeDescriptor::new("outPoint", Cat::Input, "Out Point").output("time", "Time", Time),
        NodeDescriptor::new("t01", Cat::Input, "Progress (t01)").output("value", "Value", Number),
        // ── Generators: typed-knob motion primitives (knob names match the IR).
        NodeDescriptor::new("osc", Cat::Generator, "Oscillator")
            .input_def("freq", "Freq", Number, num(0.1))
            .input_def("amp", "Amp", Number, num(1.0))
            .input_def("phase", "Phase", Number, num(0.0))
            .input_def("offset", "Offset", Number, num(0.0))
            .output("value", "Value", Number),
        NodeDescriptor::new("noise", Cat::Generator, "Noise")
            .input_def("freq", "Freq", Number, num(0.1))
            .input_def("amp", "Amp", Number, num(1.0))
            .input_def("seed", "Seed", Number, num(0.0))
            .output("value", "Value", Number),
        NodeDescriptor::new("ramp", Cat::Generator, "Ramp")
            .input_def("from", "From", Number, num(0.0))
            .input_def("to", "To", Number, num(1.0))
            .input_def("start", "Start", Time, num(0.0))
            .input_def("end", "End", Time, num(30.0))
            .output("value", "Value", Number),
        NodeDescriptor::new("bounce", Cat::Generator, "Bounce")
            .input_def("amp", "Amp", Number, num(1.0))
            .input_def("freq", "Freq", Number, num(0.1))
            .input_def("decay", "Decay", Number, num(0.05))
            .output("value", "Value", Number),
        // ── Module reuse. ────────────────────────────────────────────────────
        NodeDescriptor::new("use", Cat::Module, "Module").output("value", "Value", Number),
        // ── Where the graph meets the scene: one node reads a layer property,
        //    one drives it. They're a pair and they live together — `ref` used
        //    to sit off in Input as "Reference", which hid the only node that
        //    answers "what is that layer doing right now?" from anyone looking
        //    for the counterpart of Property Out.
        //
        // Like `out`, its socket is typed by the property it names rather than
        // by its kind (`GraphCtx::descriptor_for`), so reading a `position`
        // hands down a Vector wire and reading a `fill` a Colour one. `Number`
        // is only its unconfigured resting shape.
        NodeDescriptor::new("ref", Cat::Layer, "Property In").output("value", "Value", Number),
        //
        // The sinks below are the only nodes with no outputs, and that is the
        // point: a driver *ends* the dataflow. Binding a graph output to a layer used to
        // be a row in a side list, which made the one edit that gives a graph
        // any effect the one edit you couldn't make on the canvas. As nodes,
        // binding is the same gesture as every other wire.
        //
        // `out`'s input is typed `Number` here only as its unconfigured resting
        // shape — once it targets a property, `GraphCtx::descriptor_for`
        // retypes the socket from that property (see `PropPath::socket_type`),
        // so the canvas refuses a colour wire into a rotation.
        //
        // No `input_def` on either: an unwired sink drives nothing, and a
        // resting literal would mean "silently pin this property to zero".
        NodeDescriptor::new("out", Cat::Layer, "Property Out").input("value", "Value", Number),
        NodeDescriptor::new("shapeOut", Cat::Layer, "Shape Out")
            .input("geometry", "Geometry", Geometry),
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
        for id in ["rect", "math", "value", "osc", "use"] {
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
        // One operator node, plus the vector plumbing that also lives in Math.
        assert_eq!(math, ["math", "join", "split"]);
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

    /// A geometry node's scalar outputs are **echoes**: each shares an input's
    /// id and type. That pairing is what `lower_out` reads to pass a resolved
    /// param through to math, structurally rather than per-kind — an output that
    /// didn't pair up would lower to nothing.
    #[test]
    fn a_geometry_nodes_scalar_outputs_echo_its_inputs() {
        let reg = NodeRegistry::with_builtins();
        for id in ["rect", "ellipse", "text"] {
            let d = reg.get(id).unwrap();
            assert_eq!(d.outputs[0].id, "geometry", "{id}: the geometry output comes first");
            for s in &d.outputs[1..] {
                let echoed = d
                    .find_input(&s.id)
                    .unwrap_or_else(|| panic!("{id}.{} echoes no input of that id", s.id));
                assert_eq!(echoed.ty, s.ty, "{id}.{}: the echo's type differs", s.id);
            }
            // Every shape param has a resting literal, so an unwired shape node
            // lowers to a *visible* shape rather than a zero-sized one.
            assert!(
                d.inputs.iter().all(|s| s.default.is_some()),
                "{id}: a shape param with no default would lower to zero"
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
