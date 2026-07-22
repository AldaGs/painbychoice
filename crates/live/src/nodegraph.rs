//! The **composition node-graph panel**: a Blender-style canvas that draws a
//! [`NodeGraph`] from the [`NodeRegistry`] and reports edits back as [`NgOp`]s.
//!
//! Everything the panel knows about a node type — its title, its coloured input
//! and output sockets — it reads from the node's *descriptor*, never from a
//! hardcoded per-kind branch. That is the whole point of step 2: a new node type
//! (built-in or plugin) draws itself the moment it's registered, with no edit
//! here. Contrast `graph.rs`, whose per-`ExprKind` editors are hand-written.
//!
//! UI discipline, as everywhere in `live/`: the egui closure never borrows
//! `App`. The panel takes read-only snapshots (`&NodeGraph`, `&NodeRegistry`)
//! and records at most one [`NgOp`] per frame into [`NgEdits`]; `App` applies it
//! after the pass, so the graph is never restructured mid-render. In-progress
//! view state (a half-drawn wire) rides in egui memory, not the model.

use crate::*;

// ── Shared vocabulary, kept from the retired expression panel. ─────────────

/// The `PropPath`s an expression can reference, with labels. Core owns the list
/// (`PropPath::ALL`) so a new property shows up in the picker automatically.
pub(crate) const PROP_PATHS: [PropPath; PropPath::ALL.len()] = PropPath::ALL;

pub(crate) fn prop_path_label(p: PropPath) -> &'static str {
    match p {
        PropPath::Position => "Position",
        PropPath::Rotation => "Rotation",
        PropPath::Scale => "Scale",
        PropPath::Opacity => "Opacity",
        PropPath::Anchor => "Anchor",
        PropPath::Fill => "Fill",
        PropPath::StrokeColor => "Stroke Color",
        PropPath::StrokeWidth => "Stroke Width",
        PropPath::ShapeSize => "Size",
        PropPath::ShapeRadius => "Radius",
        PropPath::TextSize => "Font Size",
        PropPath::TextContent => "Content",
        PropPath::TimeRemap => "Time Remap",
        PropPath::MaskSize => "Mask Size",
    }
}


/// Which kind of parameter an "add" button creates. The UI's counterpart to
/// core's `ParamValue`, without carrying a value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ParamKind {
    Num,
    Vec,
    Color,
    Str,
}

impl ParamKind {
    /// A fresh parameter of this kind, at a neutral value.
    pub(crate) fn seed(self) -> ParamValue {
        match self {
            ParamKind::Num => ParamValue::Num(Value::constant(0.0)),
            ParamKind::Vec => ParamValue::Vec(Value::constant(Vec2::ZERO)),
            ParamKind::Color => {
                ParamValue::Color(Value::constant(MColor::rgba(1.0, 1.0, 1.0, 1.0)))
            }
            // Seeded empty, unlike the text *node*'s placeholder: a knob is
            // filled in per link, and a default of "Text" would silently ship
            // placeholder copy anywhere the caller forgot to set it.
            ParamKind::Str => ParamValue::Str(Value::constant(String::new())),
        }
    }
}


/// Whose knobs the parameters section is editing: the selected node's, or a
/// module's. A module body reads its knobs with `param("…")`, so editing a
/// module needs the same add/remove/rename knob surface a node has.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) enum ParamOwner {
    /// A node, by id — carried so a param op needn't lean on the selection.
    Node(NodeId),
    /// A project-wide module.
    Module(ModuleId),
}


pub(crate) const SCRIPT_HELP: &str = "\
Rhai. Return a number, or an array: [x, y] or [r, g, b(, a)].

In scope:
  frame, time          the current frame
  localTime            …in this layer's own time
  inPoint, outPoint    this layer's local in/out
  t01                  0 at in, 1 at out (clamped)
  value(\"node\", \"prop\")            another node's property
  value_at(\"node\", \"prop\", frame)  …at another frame
  wiggle(freq, amp)               smooth noise, deterministic
  wiggle(freq, amp, seed)         an independent stream

prop: position, rotation, scale, opacity, anchor, fill,
      stroke_color, stroke_width, size, radius

Nodes are named, not id'd; a vec/colour comes back as an array.";



// ── Canvas geometry (logical points). ────────────────────────────────────────
// Wide enough for a label column *and* a field beside it: a node carries its
// own values now, so the box has to hold them. A vector row is the binding
// constraint — two drag fields sharing what's left after the label.
const NODE_W: f32 = 208.0;
/// The label column every socket row's text gets, before its field starts.
const LABEL_W: f32 = 52.0;
/// How far a field stays clear of the node's edge.
const FIELD_PAD: f32 = 6.0;
/// A field's height inside its `ROW_H` row — a little air top and bottom.
const FIELD_H: f32 = 16.0;
const HEADER_H: f32 = 24.0;
const ROW_H: f32 = 20.0;
const BODY_PAD: f32 = 8.0;
/// A line of in-node config (a combo, a drag row, a note) below the socket rows.
const CFG_LINE: f32 = 22.0;
/// The gap between the last socket row and the first config line.
const CFG_TOP_GAP: f32 = 6.0;
const DOT_R: f32 = 5.0;
/// How close the pointer must land to a socket to hit it on drop.
const DOT_HIT: f32 = 9.0;
const MARGIN: f32 = 16.0;

/// One deferred node-graph edit. At most one per frame, like `GraphOp` and the
/// dock's `DockCmd`, so `App` mutates the model after the UI pass.
pub(crate) enum NgOp {
    /// Add a node of `kind` at a canvas position.
    Add { kind: String, pos: Vec2 },
    /// Move a node to a new position (a drag delta already applied).
    Move { id: GraphNodeId, pos: Vec2 },
    /// Remove a node and its wires.
    Remove { id: GraphNodeId },
    /// Wire an output to an input. Validated by the model against the registry,
    /// so a type-mismatched or cycle-closing drop is dropped there.
    Connect { from: Endpoint, to: Endpoint },
    /// Remove a specific wire.
    Disconnect { edge: Edge },
    /// Set a node's stored literal for a socket — a `value` node's constant, or
    /// an unwired input's resting value. Re-lowers any driver reading it.
    SetValue { id: GraphNodeId, socket: String, value: ExprValue },
    /// Set a `ref` node's target — which layer's which property, at what offset.
    SetRef { id: GraphNodeId, target: Option<(NodeId, PropPath, f64)> },
    /// Set a `param` node's knob name.
    SetParam { id: GraphNodeId, name: String },
    /// Set a `script` node's Rhai source.
    SetScript { id: GraphNodeId, src: String },
    /// Set a `use` node's linked module.
    SetModule { id: GraphNodeId, module: Option<ModuleId> },
    /// Set a `text` node's plain-data typography (content / family / align /
    /// wrap) — everything a text shape carries that isn't the `size` socket.
    SetText { id: GraphNodeId, text: TextConfig },
    /// Set an `osc` node's waveform — which function it is, not a value it takes.
    SetWaveform { id: GraphNodeId, wave: Waveform },
    /// Set a `math` node's operator. Like a waveform, it picks which function
    /// the node *is* — and here it also decides how many operands it takes, so
    /// `App` drops a wire into `b` when the new op is unary.
    SetMathOp { id: GraphNodeId, op: MathOp },
    /// Drop a socket's stored literal. On a `use` node's knob that is how an
    /// override goes back to *inheriting* the module's default — an absent
    /// entry and a zero mean different things there.
    ClearValue { id: GraphNodeId, socket: String },
    /// Point an `out` node at a layer's property — half of what makes it a
    /// driver (the wire into it is the other half).
    SetOutTarget { id: GraphNodeId, target: Option<(NodeId, PropPath)> },
    /// Point a `shapeOut` node at a layer. Names no property: the shape *is*
    /// what's bound.
    SetOutShape { id: GraphNodeId, target: Option<NodeId> },
}

/// One deferred edit to the project's **modules** — the document scope's own
/// ops. Separate from [`NgOp`] (which edits a graph) because these create,
/// rename, delete, or re-point a module.
pub(crate) enum NgModuleOp {
    /// Create an empty module and open its body.
    New,
    Rename { module: ModuleId, name: String },
    Delete { module: ModuleId },
    /// Set which graph output is the module's value.
    SetOutput { module: ModuleId, output: Option<Endpoint> },
}

/// One deferred edit to an **exposed knob** — a named value a `param` node
/// reads. Owner-agnostic on purpose: a layer's knob and a module's knob are the
/// same idea at two scopes (`param("x")` resolves against whichever owns the
/// expression), so they share one op, one editor, and one apply path.
pub(crate) enum NgKnobOp {
    Add { owner: ParamOwner, name: String, kind: ParamKind },
    Remove { owner: ParamOwner, name: String },
    /// Set a knob's constant value — the thing every `param("x")` read
    /// resolves to until the knob is keyframed.
    SetValue { owner: ParamOwner, name: String, value: ExprValue },
}

/// One exposed knob as the panel needs it: its name, its type word, and its
/// value *if* that value is a plain constant.
///
/// `value` is `None` for a keyframed or expression-driven knob. The editor then
/// shows no field, because one number can't stand for a whole track and writing
/// one back would flatten it.
pub(crate) struct KnobInfo {
    pub(crate) name: String,
    pub(crate) kind: &'static str,
    pub(crate) value: Option<ExprValue>,
}

/// A scene layer as the panel needs it: its id, its name, and the knobs it
/// exposes. The knobs ride along because the `param` node's picker and the
/// layer-knob editor both need them, and re-deriving them per section would
/// mean walking the tree twice a frame.
pub(crate) struct LayerInfo {
    pub(crate) id: u64,
    pub(crate) name: String,
    pub(crate) knobs: Vec<KnobInfo>,
}

/// Flatten the layer tree into what the panel needs, knobs included. The tree
/// order is the layers panel's order, so the two read the same way.
pub(crate) fn knob_info(p: &motion_core::node::Param) -> KnobInfo {
    KnobInfo { name: p.name.clone(), kind: p.value.kind_name(), value: p.value.as_const() }
}

pub(crate) fn collect_layer_info(node: &MNode, out: &mut Vec<LayerInfo>) {
    out.push(LayerInfo {
        id: node.id.0,
        name: node.name.clone(),
        knobs: node.params.iter().map(knob_info).collect(),
    });
    for c in &node.children {
        collect_layer_info(c, out);
    }
}

#[derive(Default)]
pub(crate) struct NgEdits {
    pub(crate) op: Option<NgOp>,
    pub(crate) module_op: Option<NgModuleOp>,
    pub(crate) knob: Option<NgKnobOp>,
    /// Switch which graph the canvas edits (project ↔ a module's body). View
    /// state, so it rides beside the document ops rather than in one.
    pub(crate) scope: Option<NgScope>,
    /// Fill a sink node by **import**: raise the expression its target property
    /// already has onto the canvas and wire it into this node. Names the sink,
    /// not the property — the node is where the layer and property are named, so
    /// the fold's two directions meet on one object.
    pub(crate) import: Option<GraphNodeId>,
    /// The geometry half of the same fold: raise the target layer's *shape* into
    /// this `shapeOut` node.
    pub(crate) import_shape: Option<GraphNodeId>,
    /// Create a **new layer** whose shape is this geometry output. The one
    /// action that makes something exist from the graph rather than binding to
    /// a layer the tree already had.
    pub(crate) create_layer: Option<Endpoint>,
}

/// A core [`motion_core::Color`] as an egui colour. Socket dots and header tints
/// come from `core`, so the palette is defined once (in `SocketType::color`)
/// rather than duplicated in the UI.
fn col32(c: MColor) -> egui::Color32 {
    let b = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    egui::Color32::from_rgba_unmultiplied(b(c.r), b(c.g), b(c.b), b(c.a))
}

/// The header tint for a category — a muted band so a glance sorts geometry from
/// math from effects. Distinct from socket colours, which key the *wires*.
fn category_tint(c: NodeCategory) -> egui::Color32 {
    match c {
        NodeCategory::Geometry => egui::Color32::from_rgb(52, 96, 60),
        NodeCategory::Math => egui::Color32::from_rgb(54, 66, 96),
        NodeCategory::Input => egui::Color32::from_rgb(48, 84, 88),
        NodeCategory::Generator => egui::Color32::from_rgb(72, 56, 96),
        NodeCategory::Module => egui::Color32::from_rgb(96, 72, 44),
        NodeCategory::Layer => egui::Color32::from_rgb(44, 72, 104),
        NodeCategory::Effect => egui::Color32::from_rgb(104, 56, 56),
        NodeCategory::Matte => egui::Color32::from_rgb(68, 70, 78),
    }
}

/// A node's box height for a descriptor: the header plus a row per socket on the
/// taller side. An unknown-kind node (missing descriptor) still gets one row so
/// its kind string is readable.
fn node_height(node: &GraphNode, desc: Option<&NodeDescriptor>) -> f32 {
    let rows = desc.map_or(1, |d| d.inputs.len().max(d.outputs.len()).max(1));
    HEADER_H + rows as f32 * ROW_H + config_height(node, desc) + BODY_PAD
}

/// An **upper bound** on the height a node's in-box config editors take, below
/// its socket rows. Only a bound: the box is painted to the *measured* height of
/// what the editors actually draw (see [`draw_node`]), so a generous estimate
/// here just leaves the scroll area a little roomier and never clips a node.
fn config_height(node: &GraphNode, desc: Option<&NodeDescriptor>) -> f32 {
    let lines = config_lines(node, desc);
    if lines == 0 { 0.0 } else { CFG_TOP_GAP + lines as f32 * CFG_LINE }
}

/// Roughly how many config lines a node draws on its box — see [`config_height`].
/// Padded for notes that may wrap in a node's narrow body; the exact box height
/// is measured at draw time, so this only has to be a safe over-estimate.
fn config_lines(node: &GraphNode, desc: Option<&NodeDescriptor>) -> usize {
    let mut lines = match node.kind.as_str() {
        "math" | "osc" => 2,
        "ref" | "text" | "string" => 3,
        "param" | "script" | "out" | "shapeOut" => 4,
        // A `use` node's knobs are its module's — one override row each.
        "use" => 2 + desc.map_or(0, |d| d.inputs.len()),
        _ => 0,
    };
    // Any geometry-producing node carries a "Create layer" action line.
    if desc.is_some_and(|d| d.outputs.iter().any(|s| s.ty == SocketType::Geometry)) {
        lines += 1;
    }
    lines
}

/// The screen-space centre of input socket `i` (from the left edge) or output
/// socket `j` (from the right edge) of a box whose top-left is `origin`.
fn socket_center(origin: egui::Pos2, index: usize, is_input: bool) -> egui::Pos2 {
    let y = origin.y + HEADER_H + index as f32 * ROW_H + ROW_H / 2.0;
    let x = if is_input { origin.x } else { origin.x + NODE_W };
    egui::pos2(x, y)
}

/// A tidy S-curve from an output to an input, coloured by the wire's type.
fn wire(painter: &egui::Painter, from: egui::Pos2, to: egui::Pos2, color: egui::Color32) {
    let dx = ((to.x - from.x).abs() * 0.5).max(36.0);
    let pts = [from, egui::pos2(from.x + dx, from.y), egui::pos2(to.x - dx, to.y), to];
    let shape = egui::epaint::CubicBezierShape::from_points_stroke(
        pts,
        false,
        egui::Color32::TRANSPARENT,
        egui::Stroke::new(2.0, color),
    );
    painter.add(shape);
}

/// What the canvas is currently editing — the node system's **scope**.
///
/// `Project` is the composition scope: the graph that drives layers. `Module`
/// is the document scope: one shared module's own body, on its own canvas. Same
/// panel, same ops, different graph — which is the point of the three-scopes
/// design rather than three editors.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum NgScope {
    Project,
    Module(ModuleId),
}

/// The panel. A scope bar, then (in module scope) the module header, then the
/// graph on a scrollable canvas. There is no side inspector: every node carries
/// its own properties — socket literals *and* non-socket config alike — on the
/// box itself (see [`node_editors`]), so selecting a node is only a highlight.
///
/// The drivers are **not** a section here any more: a driver is an `out` /
/// `shapeOut` node on the canvas, added from the palette like anything else and
/// configured in the inspector like anything else. What used to be two lists of
/// combo-box rows sitting above the graph is now the graph.
#[allow(clippy::too_many_arguments)]
pub(crate) fn nodegraph_ui(
    ui: &mut egui::Ui,
    graph: &NodeGraph,
    ctx: &GraphCtx,
    scope: NgScope,
    layers: &[LayerInfo],
    modules: &[(ModuleId, String)],
    knobs: &[KnobInfo],
    module_output: Option<&Endpoint>,
    script_preview: Option<&(GraphNodeId, Result<String, String>)>,
    status: Option<&str>,
    out: &mut NgEdits,
) {
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.label(icon::text(icon::NODES).size(16.0));
        ui.heading("Nodes");
        ui.weak(format!("{} nodes", graph.nodes.len()));
        palette_menu(ui, graph, ctx.reg, out);
    });
    ui.separator();
    match scope {
        NgScope::Project => {
            // Amber, not red: the action didn't happen, but nothing is broken —
            // the same hue the comp bar's warning count uses. Most refusals come
            // from a sink's Import, which is in the inspector below.
            if let Some(msg) = status {
                ui.colored_label(crate::props::WARN_COLOR, format!("{} {msg}", icon::WARNING));
            }
        }
        NgScope::Module(id) => {
            let name = modules
                .iter()
                .find(|(m, _)| *m == id)
                .map(|(_, n)| n.clone())
                .unwrap_or_else(|| format!("#{}", id.0));
            module_scope_ui(ui, graph, ctx, id, &name, knobs, module_output, out);
        }
    }
    ui.separator();
    if graph.nodes.is_empty() {
        ui.weak("Empty. Add a node from the palette above, then drag between sockets to wire.");
    }
    egui::ScrollArea::both().auto_shrink([false, false]).show(ui, |ui| {
        canvas(ui, graph, ctx, layers, modules, script_preview, out);
    });
}

/// Which graph node is selected for the inspector — ephemeral view state, so it
/// lives in egui memory under a fixed id (the canvas writes it, the inspector,
/// drawn in a different `Ui`, reads it, so a per-`Ui` id wouldn't match).
pub(crate) fn read_selection(ctx: &egui::Context) -> Option<GraphNodeId> {
    ctx.data(|d| d.get_temp::<Option<GraphNodeId>>(egui::Id::new("ng_sel"))).flatten()
}

fn write_selection(ctx: &egui::Context, sel: Option<GraphNodeId>) {
    ctx.data_mut(|d| d.insert_temp(egui::Id::new("ng_sel"), sel));
}

/// Every output socket in the graph whose type satisfies `want`, labelled
/// `node.socket` — the source picker for a driver row. Filtered by type because
/// the two driver lists take different things: a value driver wants a
/// number/vector/colour, a geometry driver wants `Geometry` and nothing else.
fn graph_outputs(
    graph: &NodeGraph,
    ctx: &GraphCtx,
    want: impl Fn(SocketType) -> bool,
) -> Vec<(Endpoint, String)> {
    let mut out = Vec::new();
    for n in &graph.nodes {
        if let Some(d) = ctx.descriptor_for(n) {
            let title = n.title.clone().unwrap_or_else(|| d.label.clone());
            for s in d.outputs.iter().filter(|s| want(s.ty)) {
                out.push((Endpoint::new(n.id, &s.id), format!("{title}.{}", s.label)));
            }
        }
    }
    out
}

/// Whether a socket type carries something a *property* driver can take — i.e.
/// something with an `ExprValue`. `Geometry`/`Layer`/`Matte` don't, so they're
/// kept out of the value driver's picker instead of offered and then lowering
/// to a neutral zero.
fn is_value_type(ty: SocketType) -> bool {
    matches!(ty, SocketType::Number | SocketType::Vector | SocketType::Color | SocketType::Time)
}

/// The header of **module scope**: which module is open, how to leave, its name,
/// its knobs, and — the piece that makes the canvas mean anything here — which
/// node output is the module's *value*.
#[allow(clippy::too_many_arguments)]
fn module_scope_ui(
    ui: &mut egui::Ui,
    graph: &NodeGraph,
    ctx: &GraphCtx,
    id: ModuleId,
    name: &str,
    knobs: &[KnobInfo],
    output: Option<&Endpoint>,
    out: &mut NgEdits,
) {
    ui.horizontal(|ui| {
        if icon::button(ui, icon::BACK, "Back to the layer-driving graph").clicked() {
            out.scope = Some(NgScope::Project);
        }
        ui.label(icon::text(icon::MODULE));
        ui.strong("Editing module");
        let mut n = name.to_string();
        if ui.add(egui::TextEdit::singleline(&mut n).desired_width(120.0)).changed() {
            out.module_op = Some(NgModuleOp::Rename { module: id, name: n });
        }
        if icon::button(
            ui,
            icon::DELETE,
            "Delete this module. Links to it warn and fall back, like any dangling reference.",
        )
        .clicked()
        {
            out.module_op = Some(NgModuleOp::Delete { module: id });
        }
    });

    // The output: which node produces this module's value. Without one the body
    // isn't graph-authored and the canvas drives nothing.
    let outputs = graph_outputs(graph, ctx, is_value_type);
    ui.horizontal(|ui| {
        ui.label(icon::text(icon::OUTPUT));
        ui.strong("Output");
        let cur = output
            .and_then(|e| outputs.iter().find(|(o, _)| o == e))
            .map(|(_, l)| l.clone())
            .unwrap_or_else(|| "(none)".into());
        egui::ComboBox::from_id_salt(("mod_out", id.0)).width(140.0).selected_text(cur).show_ui(
            ui,
            |ui| {
                for (e, l) in &outputs {
                    if ui.selectable_label(Some(e) == output, l).clicked() {
                        out.module_op =
                            Some(NgModuleOp::SetOutput { module: id, output: Some(e.clone()) });
                    }
                }
            },
        );
    });
    if output.is_none() {
        ui.weak("Pick an output — until then this module keeps whatever body it already had.");
    }

    // Knobs: what a link may override. A module with no knobs is a fixed
    // recipe; each one added here becomes an input socket on every `use` node
    // linking this module (see `GraphCtx::descriptor_for`).
    knobs_ui(ui, ParamOwner::Module(id), knobs, &mut out.knob);
}

/// The **knob editor**, for either owner: what knobs exist, plus add (in each
/// of the three value types) and remove.
///
/// One surface for a layer's knobs and a module's, because a knob *is* the same
/// thing at both scopes — a named value `param("x")` reads from whatever owns
/// the expression. What differs is only what the knob is *for*: a module's
/// knobs become override sockets on every link to it, a layer's are read by a
/// `param` node in whatever drives that layer.
///
/// Takes the op channel directly rather than the whole [`NgEdits`], because the
/// two owners are now edited from **two panels**: a module's knobs here (they're
/// its signature, and the canvas is where you see it), a layer's in the
/// properties panel (they're that layer's own data, like every other row there).
/// One widget, one op, one apply path — only the caller differs.
pub(crate) fn knobs_ui(
    ui: &mut egui::Ui,
    owner: ParamOwner,
    knobs: &[KnobInfo],
    out: &mut Option<NgKnobOp>,
) {
    ui.horizontal_wrapped(|ui| {
        ui.label(icon::text(icon::KNOB));
        ui.strong("Knobs");
        ui.weak(format!("{}", knobs.len()));
        // The pending name lives in egui memory, salted by owner so two owners'
        // fields don't share a buffer — the same rule the old panel follows.
        let mem = egui::Id::new(("ng_knob_new", owner));
        let mut pending: String = ui.ctx().data(|d| d.get_temp(mem)).unwrap_or_default();
        ui.add(egui::TextEdit::singleline(&mut pending).hint_text("new knob").desired_width(90.0));
        let taken = knobs.iter().any(|k| k.name == pending.trim());
        let ok = !pending.trim().is_empty() && !taken;
        for (label, kind) in
        [
            ("num", ParamKind::Num),
            ("vec", ParamKind::Vec),
            ("col", ParamKind::Color),
            ("txt", ParamKind::Str),
        ]
        {
            if ui
                .add_enabled(ok, egui::Button::new(format!("{} {label}", icon::ADD)).small())
                .on_disabled_hover_text(if taken {
                    "that name is taken"
                } else {
                    "type a name first"
                })
                .clicked()
            {
                *out = Some(NgKnobOp::Add {
                    owner,
                    name: pending.trim().to_string(),
                    kind,
                });
                pending.clear();
            }
        }
        ui.ctx().data_mut(|d| d.insert_temp(mem, pending));
    });
    if knobs.is_empty() {
        ui.weak("None. A knob is a named value a `param` node reads.");
    }
    for k in knobs {
        ui.horizontal(|ui| {
            if icon::button(ui, icon::CLOSE, "Remove this knob").clicked() {
                *out = Some(NgKnobOp::Remove { owner, name: k.name.clone() });
            }
            ui.label(&k.name);
            match k.value.clone() {
                Some(v) => {
                    if let Some(v) = literal_field(ui, ("knobv", owner, &k.name), v) {
                        *out = Some(NgKnobOp::SetValue { owner, name: k.name.clone(), value: v });
                    }
                }
                // Keyframed or expression-driven: no field, since one number
                // can't stand for a track and writing it back would flatten it.
                None => {
                    ui.weak("animated");
                }
            }
            ui.weak(k.kind);
        });
    }
}

/// An inline editor for one literal, by its own type — the widget a knob row
/// and any other `ExprValue` field needs. Returns the new value when edited.
fn literal_field(
    ui: &mut egui::Ui,
    salt: impl std::hash::Hash,
    cur: ExprValue,
) -> Option<ExprValue> {
    match cur {
        ExprValue::Num(n) => {
            let mut v = n;
            ui.add(egui::DragValue::new(&mut v).speed(0.1)).changed().then_some(ExprValue::Num(v))
        }
        // Single-line: a knob row is one line tall, and the multi-line editor
        // belongs to the text node's own content field where the extra height
        // is affordable.
        ExprValue::Str(t) => {
            let mut v = t;
            ui.add(egui::TextEdit::singleline(&mut v).desired_width(140.0))
                .changed()
                .then_some(ExprValue::Str(v))
        }
        ExprValue::Vec2(p) => {
            let (mut x, mut y) = (p.x, p.y);
            let mut changed = false;
            changed |= ui.add(egui::DragValue::new(&mut x).speed(0.5).prefix("x ")).changed();
            changed |= ui.add(egui::DragValue::new(&mut y).speed(0.5).prefix("y ")).changed();
            changed.then(|| ExprValue::Vec2(Vec2::new(x, y)))
        }
        ExprValue::Color(c) => {
            let mut rgb = [c.r as f32, c.g as f32, c.b as f32];
            let _ = salt;
            ui.color_edit_button_rgb(&mut rgb)
                .changed()
                .then(|| ExprValue::Color(MColor::rgba(rgb[0] as f64, rgb[1] as f64, rgb[2] as f64, c.a)))
        }
    }
}

/// The editor for a `param` node: which knob it reads.
///
/// A picker *and* a free-text field, because the two carry different truths. A
/// `param` node lowers to `Expr::Param { node: None, .. }`, which resolves
/// against whichever layer a driver points at — so there is no single owner
/// whose knobs are *the* candidate list, and the picker can only offer the
/// union of what exists. Typing a name that doesn't exist yet is legitimate
/// (the knob may be added later, or on a layer this graph doesn't drive yet),
/// so the field stays; a name nothing exposes is flagged rather than refused,
/// matching the warn-don't-fail contract a dangling read follows at render time.
fn param_editor(ui: &mut egui::Ui, node: &GraphNode, layers: &[LayerInfo], out: &mut NgEdits) {
    let cur = node.config.param.clone();
    // Every knob name exposed anywhere in the scene, deduped, with the layers
    // that expose it — so a pick is informed by *whose* knob it is.
    let mut known: Vec<(String, Vec<&str>)> = Vec::new();
    for l in layers {
        for k in &l.knobs {
            match known.iter_mut().find(|(n, _)| *n == k.name) {
                Some((_, on)) => on.push(&l.name),
                None => known.push((k.name.clone(), vec![&l.name])),
            }
        }
    }
    if known.is_empty() {
        ui.weak("No layer exposes a knob yet — add one under 'Layer knobs'.");
    } else {
        egui::ComboBox::from_id_salt(("param_pick", node.id.0))
            .width(140.0)
            .selected_text(if cur.is_empty() { "(pick)".to_string() } else { cur.clone() })
            .show_ui(ui, |ui| {
                for (name, on) in &known {
                    let label = format!("{name}  —  {}", on.join(", "));
                    if ui.selectable_label(*name == cur, label).clicked() && *name != cur {
                        out.op = Some(NgOp::SetParam { id: node.id, name: name.clone() });
                    }
                }
            });
    }
    let mut name = cur.clone();
    if ui
        .add(egui::TextEdit::singleline(&mut name).hint_text("knob name").desired_width(140.0))
        .on_hover_text("Reads this knob on whichever layer a driver points at")
        .changed()
    {
        out.op = Some(NgOp::SetParam { id: node.id, name });
    }
    if !cur.is_empty() && !known.iter().any(|(n, _)| *n == cur) {
        ui.colored_label(
            egui::Color32::from_rgb(220, 160, 60),
            format!("no layer exposes '{cur}' — it reads 0 until one does"),
        );
    }
}

/// A node's **non-socket properties**, drawn inside the node box (below its
/// sockets) rather than in a side panel.
///
/// This is the other half of what makes a node self-contained. A socket value —
/// a `value` node's number, an oscillator's `freq` — already rides on the socket
/// row (see [`socket_field`]). Everything that *isn't* a socket rides here: a
/// Math node's operator, an oscillator's waveform, a `ref`'s target, a `use`'s
/// module, a sink's layer/property, a text node's typography. So a node carries
/// its whole self on the canvas, and nothing is stranded in an inspector that
/// described whichever node happened to be selected.
///
/// Every node draws its own editors every frame — the config is not gated on
/// selection, exactly like the socket fields aren't. A pure layout pass records
/// no edit, because each control writes `out` only on `.changed()`/`.clicked()`.
#[allow(clippy::too_many_arguments)]
fn node_editors(
    ui: &mut egui::Ui,
    graph: &NodeGraph,
    node: &GraphNode,
    desc: &NodeDescriptor,
    layers: &[LayerInfo],
    modules: &[(ModuleId, String)],
    script_preview: Option<&(GraphNodeId, Result<String, String>)>,
    out: &mut NgEdits,
) {
    // A geometry node can *become* a layer — the one action that makes something
    // exist from the graph rather than binding to a layer the tree already had.
    if desc.outputs.iter().any(|s| s.ty == SocketType::Geometry)
        && ui
            .button(format!("{} Create layer", icon::ADD))
            .on_hover_text("Add a new layer to this composition whose shape is this node's geometry.")
            .clicked()
    {
        out.create_layer = Some(Endpoint::new(node.id, "geometry"));
    }

    match node.kind.as_str() {
        // The sinks: where the graph meets the scene. Like `ref`, they carry
        // addressing rather than a socket value — the difference is direction.
        "out" => out_editor(ui, graph, node, layers, out),
        "shapeOut" => shape_out_editor(ui, graph, node, layers, out),
        // A `ref` reads another layer's property; a `param` reads the driven
        // layer's own knob. Both carry addressing rather than a socket value.
        "ref" => ref_editor(ui, node, layers, out),
        "param" => param_editor(ui, node, layers, out),
        // A text node's typography: `family` names a system font (a lookup key,
        // not a value) and align/wrap are shaping modes. Its `content` is a real
        // `Text` input socket, so it draws on the socket row like any literal.
        "text" => text_editor(ui, node, out),
        "script" => {
            let mut src = node.config.script.clone();
            if ui
                .add(
                    egui::TextEdit::multiline(&mut src)
                        .hint_text("frame * 2.0")
                        .desired_width(f32::INFINITY)
                        .desired_rows(2)
                        .font(egui::TextStyle::Monospace),
                )
                .on_hover_text(SCRIPT_HELP)
                .changed()
            {
                out.op = Some(NgOp::SetScript { id: node.id, src });
            }
            // The live result, at the playhead, in the context of a layer it
            // drives — computed only for the selected node (the one the user is
            // writing), so it shows under that node and no other.
            match script_preview {
                Some((id, Ok(v))) if *id == node.id => {
                    ui.weak(format!("= {v}"));
                }
                Some((id, Err(e))) if *id == node.id => {
                    ui.colored_label(egui::Color32::from_rgb(220, 90, 90), e);
                }
                _ => {}
            }
        }
        "use" => {
            if modules.is_empty() {
                ui.weak("No modules yet — Add ▸ Module ▸ New module makes one.");
                return;
            }
            let cur = node
                .config
                .module
                .and_then(|m| modules.iter().find(|(id, _)| *id == m))
                .map(|(_, n)| n.clone())
                .unwrap_or_else(|| "(pick)".into());
            ui.horizontal(|ui| {
                egui::ComboBox::from_id_salt(("use_mod", node.id.0)).selected_text(cur).show_ui(
                    ui,
                    |ui| {
                        for (m, name) in modules {
                            let picked = node.config.module == Some(*m);
                            if ui.selectable_label(picked, name).clicked() && !picked {
                                out.op = Some(NgOp::SetModule { id: node.id, module: Some(*m) });
                            }
                        }
                    },
                );
                // The way into module scope, on the node that names the module.
                if let Some(m) = node.config.module {
                    if icon::button(ui, icon::ENTER, "Open this module's body on the canvas")
                        .clicked()
                    {
                        out.scope = Some(NgScope::Module(m));
                    }
                }
            });
            knob_rows(ui, graph, node, desc, out);
        }
        // A Math node's operator: which function it is, not a value it takes —
        // and the one config that reshapes the node, since a unary op has no B.
        "math" => {
            let cur = node.config.math_op;
            ui.horizontal(|ui| {
                egui::ComboBox::from_id_salt(("math_op", node.id.0))
                    .width(120.0)
                    .selected_text(cur.label())
                    .show_ui(ui, |ui| {
                        for op in MathOp::all() {
                            if ui.selectable_label(op == cur, op.label()).clicked() && op != cur {
                                out.op = Some(NgOp::SetMathOp { id: node.id, op });
                            }
                        }
                    });
                if cur.arity() == 1 {
                    ui.weak("one operand");
                }
            });
        }
        // An oscillator's waveform picks *which* function the generator is, not a
        // value fed into one, so there's nothing for a wire to carry.
        "osc" => {
            let cur = node.config.wave;
            ui.horizontal(|ui| {
                ui.label(icon::text(icon::WAVE));
                egui::ComboBox::from_id_salt(("osc_wave", node.id.0))
                    .width(90.0)
                    .selected_text(cur.label())
                    .show_ui(ui, |ui| {
                        for w in Waveform::ALL {
                            if ui.selectable_label(w == cur, w.label()).clicked() && w != cur {
                                out.op = Some(NgOp::SetWaveform { id: node.id, wave: w });
                            }
                        }
                    });
            });
        }
        // A `string` node's constant gets a **roomier** multiline here as well as
        // its one-line socket field: a caption with an embedded newline would be
        // invisible in the socket row. The same value either way.
        "string" => {
            let mut cur = match node.value("value") {
                Some(ExprValue::Str(t)) => t,
                _ => String::new(),
            };
            if ui
                .add(egui::TextEdit::multiline(&mut cur).hint_text("text").desired_rows(2))
                .changed()
            {
                out.op = Some(set_value(node.id, "value", ExprValue::Str(cur)));
            }
        }
        _ => {}
    }
}

/// The knob rows of a `use` node: one per socket the linked module contributes,
/// each in one of three states — **wired** (a recipe on the canvas drives it),
/// **overridden** (a literal typed here), or **inheriting** (the module's own
/// default).
///
/// Inheriting is the resting state and has no field, deliberately: showing a
/// zero for a knob whose module default is 3 would be a lie, and a module
/// default is resolved lazily in the caller's scope, so it can't be previewed
/// as a literal anyway. "Override" seeds a neutral literal to start from and
/// "×" drops back to inheriting — the same two-state toggle the old
/// per-property editor's link box had, now with the third state (wired) that
/// only a canvas can offer.
fn knob_rows(
    ui: &mut egui::Ui,
    graph: &NodeGraph,
    node: &GraphNode,
    desc: &NodeDescriptor,
    out: &mut NgEdits,
) {
    if desc.inputs.is_empty() {
        ui.weak("This module exposes no knobs.");
        return;
    }
    ui.weak("Knobs — unwired and unset means inherit the module's default.");
    for s in &desc.inputs {
        ui.horizontal(|ui| {
            ui.label(&s.label);
            if graph.incoming(&Endpoint::new(node.id, &s.id)).is_some() {
                ui.weak("← wired");
                return;
            }
            match node.value(&s.id) {
                Some(ExprValue::Num(n)) => {
                    if let Some(v) = num_field(ui, "override", n) {
                        out.op = Some(set_value(node.id, &s.id, ExprValue::Num(v)));
                    }
                    if icon::button(ui, icon::BACK, "Back to inheriting").clicked() {
                        out.op = Some(NgOp::ClearValue { id: node.id, socket: s.id.clone() });
                    }
                }
                // A vector or colour override is set on the canvas (wire a node
                // into it); only the scalar case gets an inline field.
                Some(_) => {
                    ui.weak("overridden");
                    if icon::button(ui, icon::BACK, "Back to inheriting").clicked() {
                        out.op = Some(NgOp::ClearValue { id: node.id, socket: s.id.clone() });
                    }
                }
                None => {
                    ui.weak("inherit");
                    if icon::button(ui, icon::EDIT, "Replace the module's default for this link")
                        .clicked()
                    {
                        out.op = Some(set_value(node.id, &s.id, neutral_literal(s.ty)));
                    }
                }
            }
        });
    }
}

/// The literal an override starts from when a knob is first overridden — a zero
/// of the socket's own shape, so the field that appears is the right kind.
fn neutral_literal(ty: SocketType) -> ExprValue {
    match ty {
        SocketType::Vector => ExprValue::Vec2(Vec2::ZERO),
        SocketType::Color => ExprValue::Color(MColor::rgba(0.0, 0.0, 0.0, 1.0)),
        SocketType::Text => ExprValue::Str(String::new()),
        // Number/Time, and the three that have no literal at all — a caller
        // that asks about those has already decided a field belongs there.
        _ => ExprValue::Num(0.0),
    }
}

/// A drag field for one scalar socket, returning the edited value. A free fn,
/// not a closure over `out`: the inspector's loop needs to write `out.op` for
/// the vector case too, and a closure holding that borrow would lock it out.
fn num_field(ui: &mut egui::Ui, label: &str, cur: f64) -> Option<f64> {
    let mut v = cur;
    let changed =
        ui.add(egui::DragValue::new(&mut v).speed(0.1).prefix(format!("{label}: "))).changed();
    changed.then_some(v)
}

/// Build the op that stores a socket literal — the one edit every inspector
/// field makes.
fn set_value(id: GraphNodeId, socket: &str, value: ExprValue) -> NgOp {
    NgOp::SetValue { id, socket: socket.to_string(), value }
}

/// The editor for a `text` node's **typography**: the family, the alignment,
/// and the wrap width. Not the string — `content` is a `Text` input socket now,
/// so it is wirable and lives with the other socket literals.
///
/// These three stay config because none of them is a value: `family` is a
/// system-font lookup key, and align/wrap select a shaping mode. Lowering copies
/// them straight into `Shape::Text`.
fn text_editor(ui: &mut egui::Ui, node: &GraphNode, out: &mut NgEdits) {
    let mut t = node.config.text.clone();
    let mut changed = false;
    ui.horizontal(|ui| {
        changed |= ui
            .add(
                egui::TextEdit::singleline(&mut t.family)
                    .hint_text("font family (blank = default)")
                    .desired_width(140.0),
            )
            .changed();
        egui::ComboBox::from_id_salt(("txt_align", node.id.0))
            .width(72.0)
            .selected_text(t.align.label())
            .show_ui(ui, |ui| {
                for a in [TextAlign::Left, TextAlign::Center, TextAlign::Right] {
                    if ui.selectable_label(a == t.align, a.label()).clicked() && a != t.align {
                        t.align = a;
                        changed = true;
                    }
                }
            });
    });
    ui.horizontal(|ui| {
        // `None` is "one line"; ticking the box starts wrapping at a width.
        let mut wraps = t.max_width.is_some();
        if ui.checkbox(&mut wraps, "wrap").changed() {
            t.max_width = wraps.then_some(400.0);
            changed = true;
        }
        if let Some(w) = t.max_width.as_mut() {
            changed |= ui.add(egui::DragValue::new(w).speed(1.0).range(1.0..=f64::MAX)).changed();
        }
    });
    if changed {
        out.op = Some(NgOp::SetText { id: node.id, text: t });
    }
}

/// The "Add" menu: every registered descriptor, grouped by category. A category
/// the compositor gates ([`NodeCategory::is_buildable_now`] is false) is shown
/// but marked, so a plugin's effect node is discoverable without pretending it
/// evaluates yet.
fn palette_menu(ui: &mut egui::Ui, graph: &NodeGraph, reg: &NodeRegistry, out: &mut NgEdits) {
    ui.menu_button(format!("{} Add", icon::ADD), |ui| {
        for cat in NodeCategory::ALL {
            let mut kinds = reg.by_category(cat).peekable();
            if kinds.peek().is_none() {
                continue;
            }
            let title = if cat.is_buildable_now() {
                cat.label().to_string()
            } else {
                format!("{} (needs compositor)", cat.label())
            };
            ui.menu_button(title, |ui| {
                for desc in kinds {
                    if ui.button(&desc.label).clicked() {
                        // Stagger fresh nodes so they don't stack exactly; the
                        // user drags them where they want.
                        let n = graph.nodes.len() as f64;
                        let off = 28.0 * (n % 6.0);
                        out.op = Some(NgOp::Add {
                            kind: desc.id.clone(),
                            pos: Vec2::new(40.0 + off, 40.0 + off),
                        });
                    }
                }
                // Making a shared module is the one palette entry that isn't a
                // registered kind: it creates a *document* object as well as a
                // node. It belongs here anyway — what you get is a `use` node
                // linked to a fresh module, which is a node like any other, and
                // "New module" being a button in a list somewhere else was the
                // last reason that list existed.
                if cat == NodeCategory::Module {
                    ui.separator();
                    if ui
                        .button(format!("{} New module…", icon::ADD))
                        .on_hover_text(
                            "Create an empty shared module, link a node to it, and open its body.",
                        )
                        .clicked()
                    {
                        out.module_op = Some(NgModuleOp::New);
                    }
                }
            });
        }
    });
}

/// Lay the graph out and interact with it. Node positions come from the model
/// (they're saved), so a drag emits a `Move` op and — for the frame in hand —
/// the delta is applied locally so the box tracks the pointer without a lag.
#[allow(clippy::too_many_arguments)]
fn canvas(
    ui: &mut egui::Ui,
    graph: &NodeGraph,
    ctx: &GraphCtx,
    layers: &[LayerInfo],
    modules: &[(ModuleId, String)],
    script_preview: Option<&(GraphNodeId, Result<String, String>)>,
    out: &mut NgEdits,
) {
    // Content extent covers every node so the scroll area can reach them.
    let mut extent = egui::vec2(NODE_W, ROW_H);
    for n in &graph.nodes {
        let h = node_height(n, ctx.descriptor_for(n).as_deref());
        extent.x = extent.x.max(n.pos.x as f32 + NODE_W);
        extent.y = extent.y.max(n.pos.y as f32 + h);
    }
    let (area, _) =
        ui.allocate_exact_size(extent + egui::vec2(MARGIN, MARGIN), egui::Sense::hover());
    let origin = area.min;
    let selected = read_selection(ui.ctx());

    // Pending wire (an in-flight connection drag): ephemeral view state, so it
    // lives in egui memory keyed to this panel, not in the model.
    let pending_id = ui.id().with("ng_pending");
    let mut pending: Option<Endpoint> = ui.data(|d| d.get_temp(pending_id)).flatten();

    // Pass 1 — body drags, so every node's live top-left is known before we draw
    // sockets or wires off it. `live_pos` is this frame's position (model + any
    // drag delta); a drag also emits the `Move` that persists it.
    let mut live_pos: std::collections::HashMap<GraphNodeId, egui::Pos2> =
        std::collections::HashMap::new();
    for n in &graph.nodes {
        let base = origin + egui::vec2(n.pos.x as f32, n.pos.y as f32);
        let h = node_height(n, ctx.descriptor_for(n).as_deref());
        let rect = egui::Rect::from_min_size(base, egui::vec2(NODE_W, h));
        // The header bar is the drag handle (like Blender); sockets sit below it
        // and take pointer priority in their own spots.
        let header = egui::Rect::from_min_size(rect.min, egui::vec2(NODE_W, HEADER_H));
        let resp =
            ui.interact(header, ui.id().with(("ng_drag", n.id.0)), egui::Sense::click_and_drag());
        // A click (no drag) selects the node for the inspector.
        if resp.clicked() {
            write_selection(ui.ctx(), Some(n.id));
        }
        let mut pos = base;
        if resp.dragged() {
            pos += resp.drag_delta();
            let model = pos - origin.to_vec2();
            out.op = Some(NgOp::Move { id: n.id, pos: Vec2::new(model.x as f64, model.y as f64) });
        }
        live_pos.insert(n.id, pos);
    }

    // Socket screen positions, from the live top-lefts.
    let mut out_pos: std::collections::HashMap<Endpoint, egui::Pos2> =
        std::collections::HashMap::new();
    let mut in_pos: std::collections::HashMap<Endpoint, egui::Pos2> =
        std::collections::HashMap::new();
    for n in &graph.nodes {
        let Some(desc) = ctx.descriptor_for(n) else { continue };
        let top = live_pos[&n.id];
        for (i, s) in desc.inputs.iter().enumerate() {
            in_pos.insert(Endpoint::new(n.id, &s.id), socket_center(top, i, true));
        }
        for (j, s) in desc.outputs.iter().enumerate() {
            out_pos.insert(Endpoint::new(n.id, &s.id), socket_center(top, j, false));
        }
    }

    // Pass 2 — wires under the boxes. A wire's colour is its output socket's
    // type, so the dataflow reads at a glance.
    let painter = ui.painter().clone();
    for e in &graph.edges {
        if let (Some(&a), Some(&b)) = (out_pos.get(&e.from), in_pos.get(&e.to)) {
            let ty = graph
                .node(e.from.node)
                .and_then(|n| ctx.descriptor_for(n))
                .and_then(|d| d.find_output(&e.from.socket).map(|s| s.ty));
            let c = ty.map_or(egui::Color32::GRAY, |t| col32(t.color()));
            wire(&painter, a, b, c);
        }
    }

    // Pass 3 — the boxes: fill, header, sockets, delete. Painted over the wires,
    // and their socket/×/ interactions take pointer priority where they sit.
    for n in &graph.nodes {
        let top = live_pos[&n.id];
        let desc = ctx.descriptor_for(n);
        let h = node_height(n, desc.as_deref());
        let rect = egui::Rect::from_min_size(top, egui::vec2(NODE_W, h));
        let is_sel = selected == Some(n.id);
        draw_node(
            ui, &painter, n, desc.as_deref(), rect, is_sel, graph, layers, modules,
            script_preview, out, &mut pending, &in_pos, &out_pos,
        );
    }

    // A pending wire follows the pointer until it's dropped.
    if let Some(src) = &pending {
        if let (Some(&from), Some(ptr)) =
            (out_pos.get(src), ui.input(|i| i.pointer.interact_pos()))
        {
            wire(&painter, from, ptr, egui::Color32::from_gray(200));
        }
    }

    // Resolve a drop: on pointer release, a pending wire lands on whichever input
    // socket is under the pointer (type/cycle checked by the model), else it's
    // cancelled. Handled globally, off the release position, so it doesn't
    // depend on which widget happened to claim the drag.
    let released = ui.input(|i| i.pointer.any_released());
    if released {
        if let Some(src) = pending.take() {
            if let Some(ptr) = ui.input(|i| i.pointer.interact_pos()) {
                if let Some((ep, _)) =
                    in_pos.iter().find(|(_, &p)| p.distance(ptr) <= DOT_HIT)
                {
                    out.op = Some(NgOp::Connect { from: src, to: ep.clone() });
                }
            }
        }
    }

    ui.data_mut(|d| d.insert_temp(pending_id, pending));
}

/// Draw one node box and interact with its header button and sockets.
#[allow(clippy::too_many_arguments)]
fn draw_node(
    ui: &mut egui::Ui,
    painter: &egui::Painter,
    node: &GraphNode,
    desc: Option<&NodeDescriptor>,
    rect: egui::Rect,
    selected: bool,
    graph: &NodeGraph,
    layers: &[LayerInfo],
    modules: &[(ModuleId, String)],
    script_preview: Option<&(GraphNodeId, Result<String, String>)>,
    out: &mut NgEdits,
    pending: &mut Option<Endpoint>,
    in_pos: &std::collections::HashMap<Endpoint, egui::Pos2>,
    out_pos: &std::collections::HashMap<Endpoint, egui::Pos2>,
) {
    let rounding = 6.0;
    let bg = ui.visuals().extreme_bg_color;
    // The body fill and the border are painted **last**, once the in-node config
    // below has reported how tall the node really is — so the box grows to fit a
    // node's properties. Their draw order is reserved here (fill behind the
    // header, border in front of it) and filled in at the end via `paint_box`.
    let body_idx = painter.add(egui::Shape::Noop);
    // Header band, tinted by category (or red for an unknown kind).
    let header = egui::Rect::from_min_size(rect.min, egui::vec2(rect.width(), HEADER_H));
    let tint = desc.map_or(egui::Color32::from_rgb(120, 52, 52), |d| category_tint(d.category));
    painter.rect_filled(header, rounding, tint);
    // A flat lower edge under the rounded header so it reads as a band.
    painter.rect_filled(
        egui::Rect::from_min_size(egui::pos2(header.min.x, header.max.y - rounding), egui::vec2(header.width(), rounding)),
        0.0,
        tint,
    );
    let border_stroke = if selected {
        egui::Stroke::new(2.0, egui::Color32::from_rgb(220, 160, 60))
    } else {
        egui::Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color)
    };
    let border_idx = painter.add(egui::Shape::Noop);
    // Fill the reserved fill/border shapes for a box `r` tall — the whole node
    // rectangle, once its real height is known. Borrows only `Copy` state and
    // the shared painter, so it coexists with the config `Ui` below.
    let paint_box = |r: egui::Rect| {
        painter.set(body_idx, egui::Shape::rect_filled(r, rounding, bg));
        painter.set(
            border_idx,
            egui::Shape::rect_stroke(r, rounding, border_stroke, egui::StrokeKind::Inside),
        );
    };

    // Title, and a delete button at the header's right.
    let title = node_title(node, desc, layers);
    painter.text(
        egui::pos2(header.min.x + 8.0, header.center().y),
        egui::Align2::LEFT_CENTER,
        title,
        egui::FontId::proportional(13.0),
        egui::Color32::from_gray(235),
    );
    let del = egui::Rect::from_center_size(
        egui::pos2(header.max.x - 12.0, header.center().y),
        egui::vec2(16.0, 16.0),
    );
    let del_resp = ui.interact(del, ui.id().with(("ng_del", node.id.0)), egui::Sense::click());
    let del_col = if del_resp.hovered() { egui::Color32::from_gray(255) } else { egui::Color32::from_gray(200) };
    // The icon family, not the proportional one: `✕` is a text character that
    // happens to look like a close button, and it sits differently from every
    // other control in the UI.
    painter.text(
        del.center(),
        egui::Align2::CENTER_CENTER,
        icon::CLOSE,
        egui::FontId::new(12.0, egui::FontFamily::Name(icon::FAMILY.into())),
        del_col,
    );
    if del_resp.clicked() {
        out.op = Some(NgOp::Remove { id: node.id });
    }

    // An unknown-kind node has no sockets or config, so its box is just `rect`.
    let Some(desc) = desc else {
        paint_box(rect);
        return;
    };

    // Input sockets: dot, label, and — where the row has a literal — the field
    // that edits it, right there on the node. Secondary-click on the dot
    // disconnects the wire feeding it. It's also a drop target, resolved
    // globally on release.
    for (i, s) in desc.inputs.iter().enumerate() {
        let ep = Endpoint::new(node.id, &s.id);
        let c = in_pos[&ep];
        let connected = graph.incoming(&ep).is_some();
        socket_dot(painter, c, s.ty, connected);
        painter.text(
            egui::pos2(c.x + DOT_R + 5.0, c.y),
            egui::Align2::LEFT_CENTER,
            &s.label,
            egui::FontId::proportional(11.0),
            egui::Color32::from_gray(190),
        );
        if let Some(cur) = row_literal(graph, node, s, true) {
            let row = field_rect(rect, c, true);
            if let Some(v) = socket_field(ui, row, (node.id.0, s.id.as_str()), cur) {
                out.op = Some(NgOp::SetValue { id: node.id, socket: s.id.clone(), value: v });
            }
        }
        // After the field, so the socket's own hit area wins where they touch:
        // dropping a wire on a dot must not be swallowed by a drag field.
        let hit = egui::Rect::from_center_size(c, egui::vec2(DOT_HIT * 2.0, DOT_HIT * 2.0));
        let resp = ui.interact(hit, ui.id().with(("ng_in", node.id.0, i)), egui::Sense::click());
        if resp.secondary_clicked() {
            if let Some(edge) = graph.incoming(&ep) {
                out.op = Some(NgOp::Disconnect { edge: edge.clone() });
            }
        }
    }

    // Output sockets: dot + label on the right. Dragging one starts a wire. A
    // constant node's value lives here too — it's the only socket it has.
    for (j, s) in desc.outputs.iter().enumerate() {
        let ep = Endpoint::new(node.id, &s.id);
        let c = out_pos[&ep];
        socket_dot(painter, c, s.ty, false);
        painter.text(
            egui::pos2(c.x - DOT_R - 5.0, c.y),
            egui::Align2::RIGHT_CENTER,
            &s.label,
            egui::FontId::proportional(11.0),
            egui::Color32::from_gray(190),
        );
        if let Some(cur) = row_literal(graph, node, s, false) {
            let row = field_rect(rect, c, false);
            if let Some(v) = socket_field(ui, row, (node.id.0, s.id.as_str()), cur) {
                out.op = Some(NgOp::SetValue { id: node.id, socket: s.id.clone(), value: v });
            }
        }
        let hit = egui::Rect::from_center_size(c, egui::vec2(DOT_HIT * 2.0, DOT_HIT * 2.0));
        let resp = ui.interact(hit, ui.id().with(("ng_out", node.id.0, j)), egui::Sense::drag());
        if resp.drag_started() {
            *pending = Some(ep);
        }
    }

    // The node's non-socket properties, drawn on the box below its sockets. The
    // box is then painted to fit whatever they measured to, so a `ref`'s combos
    // or a script's editor are never clipped.
    let rows = desc.inputs.len().max(desc.outputs.len()).max(1);
    let sockets_bottom = rect.top() + HEADER_H + rows as f32 * ROW_H;
    let mut content_bottom = sockets_bottom;
    if config_lines(node, Some(desc)) > 0 {
        let cfg_rect = egui::Rect::from_min_max(
            egui::pos2(rect.left() + BODY_PAD, sockets_bottom + CFG_TOP_GAP),
            egui::pos2(rect.right() - BODY_PAD, sockets_bottom + CFG_TOP_GAP + 600.0),
        );
        let builder = egui::UiBuilder::new()
            .max_rect(cfg_rect)
            .id_salt(("ng_cfg", node.id.0))
            .layout(egui::Layout::top_down(egui::Align::Min));
        let used = ui.scope_builder(builder, |ui| {
            ui.spacing_mut().item_spacing.y = 3.0;
            node_editors(ui, graph, node, desc, layers, modules, script_preview, out);
            ui.min_rect()
        });
        content_bottom = used.inner.bottom();
    }
    paint_box(egui::Rect::from_min_max(
        rect.min,
        egui::pos2(rect.right(), content_bottom + BODY_PAD),
    ));
}

/// The slot an inline field occupies in a socket row: the gap between the
/// label column and the node's far edge (mirrored for an output, whose label is
/// right-aligned against its dot).
///
/// A fixed label column rather than a measured one, so every row's field starts
/// at the same x and a node reads as a column of values rather than a ragged
/// edge. A label longer than the column is clipped by the painter, which is the
/// same thing that happened before there were fields.
fn field_rect(node: egui::Rect, socket: egui::Pos2, is_input: bool) -> egui::Rect {
    let (x0, x1) = if is_input {
        (socket.x + DOT_R + 5.0 + LABEL_W, node.right() - FIELD_PAD)
    } else {
        (node.left() + FIELD_PAD, socket.x - DOT_R - 5.0 - LABEL_W)
    };
    egui::Rect::from_min_max(
        egui::pos2(x0, socket.y - FIELD_H / 2.0),
        egui::pos2(x1.max(x0), socket.y + FIELD_H / 2.0),
    )
}

/// The layer picker both sinks need: which layer this driver writes to.
/// Returns the pick when it changes.
fn layer_picker(
    ui: &mut egui::Ui,
    salt: impl std::hash::Hash + std::fmt::Debug,
    cur: Option<NodeId>,
    layers: &[LayerInfo],
) -> Option<NodeId> {
    let label = cur
        .and_then(|t| layers.iter().find(|l| l.id == t.0))
        .map(|l| l.name.clone())
        .or_else(|| cur.map(|t| format!("#{}", t.0)))
        .unwrap_or_else(|| "(pick a layer)".into());
    let mut picked = None;
    egui::ComboBox::from_id_salt(salt).width(110.0).selected_text(label).show_ui(ui, |ui| {
        for l in layers {
            if ui.selectable_label(cur == Some(NodeId(l.id)), &l.name).clicked() {
                picked = Some(NodeId(l.id));
            }
        }
    });
    picked.filter(|p| Some(*p) != cur)
}

/// The editor for an `out` node: which layer's which property it drives.
///
/// The two combos are what the Drivers list's rows used to be — moved onto the
/// node they configure, so the binding and the thing bound are one object. What
/// the row *couldn't* show is here too: whether anything is actually wired in,
/// since a driver with a target and no wire drives nothing.
///
/// Changing the property retypes the input socket (see
/// `GraphCtx::descriptor_for`), so a wire that no longer fits is dropped by
/// `App` when it applies the op — said plainly here rather than discovered.
fn out_editor(
    ui: &mut egui::Ui,
    graph: &NodeGraph,
    node: &GraphNode,
    layers: &[LayerInfo],
    out: &mut NgEdits,
) {
    if layers.is_empty() {
        ui.weak("No layers to drive. Add one first.");
        return;
    }
    let cur = node.config.out_target;
    let (cur_layer, cur_prop) = (cur.map(|(l, _)| l), cur.map(|(_, p)| p));
    ui.horizontal(|ui| {
        ui.label(icon::text(icon::LINK)).on_hover_text("This node is a driver: it writes into the scene.");
        if let Some(l) = layer_picker(ui, ("out_layer", node.id.0), cur_layer, layers) {
            // Seeding Rotation on the first pick matches the driver list's old
            // default: a scalar property, so the socket stays Number and any
            // wire already dropped on it survives the targeting.
            let prop = cur_prop.unwrap_or(PropPath::Rotation);
            out.op = Some(NgOp::SetOutTarget { id: node.id, target: Some((l, prop)) });
        }
        egui::ComboBox::from_id_salt(("out_prop", node.id.0))
            .width(90.0)
            .selected_text(cur_prop.map_or("(property)", prop_path_label))
            .show_ui(ui, |ui| {
                for p in PROP_PATHS {
                    if ui.selectable_label(cur_prop == Some(p), prop_path_label(p)).clicked()
                        && cur_prop != Some(p)
                    {
                        // A property with no layer yet is legitimate — pick
                        // either half first — so this seeds the layer the same
                        // way the layer combo seeds the property.
                        let layer = cur_layer.unwrap_or(NodeId(layers[0].id));
                        out.op = Some(NgOp::SetOutTarget { id: node.id, target: Some((layer, p)) });
                    }
                }
            });
    });
    match (cur, graph.incoming(&Endpoint::new(node.id, "value")).is_some()) {
        (Some((_, p)), true) => {
            ui.weak(format!("Driving it as {}.", p.socket_type().label()));
        }
        // Unwired and targeted is where **import** belongs — the fold's other
        // direction, on the node that already names the property. If that
        // property has an expression of its own, this pulls it onto the canvas
        // as nodes and wires it in here, so the recipe becomes editable in the
        // place that was about to drive it anyway.
        (Some(_), false) => {
            ui.weak("Nothing wired in — wire a value into this node to drive it.");
            import_button(
                ui,
                "Import its expression",
                "Raise this property's existing expression onto the canvas and wire it in.\n\
                 Nothing to do unless the property is expression-driven (= fx).",
                || out.import = Some(node.id),
            );
        }
        (None, _) => {
            ui.weak("Pick a layer and a property. Until then this node drives nothing.");
        }
    };
}

/// The **import** button both sinks carry: the fold's front door, on the node
/// that names what it would pull in.
///
/// It used to be a section of its own with its own layer + property combos,
/// duplicating the pickers a sink already has. Here the target is whatever the
/// node targets, so there is one place a layer and a property are named and one
/// object that means "this graph and that property are the same thing".
fn import_button(ui: &mut egui::Ui, label: &str, hover: &str, mut on_click: impl FnMut()) {
    if ui.button(format!("{} {label}", icon::IMPORT)).on_hover_text(hover).clicked() {
        on_click();
    }
}

/// The editor for a `shapeOut` node: which layer's *shape* it authors. One
/// combo, because a shape names no property — the same asymmetry
/// `ShapeBinding` has against `Binding`.
fn shape_out_editor(
    ui: &mut egui::Ui,
    graph: &NodeGraph,
    node: &GraphNode,
    layers: &[LayerInfo],
    out: &mut NgEdits,
) {
    if layers.is_empty() {
        ui.weak("No layers to drive. Add one first, or use a shape node's 'Create layer'.");
        return;
    }
    let cur = node.config.out_shape;
    ui.horizontal(|ui| {
        ui.label(icon::text(icon::SHAPE))
            .on_hover_text("This node is a geometry driver: it authors a layer's shape.");
        if let Some(l) = layer_picker(ui, ("shapeout_layer", node.id.0), cur, layers) {
            out.op = Some(NgOp::SetOutShape { id: node.id, target: Some(l) });
        }
    });
    match (cur.is_some(), graph.incoming(&Endpoint::new(node.id, "geometry")).is_some()) {
        (true, true) => {
            ui.weak("This layer's shape is rebuilt from the graph on every edit.");
        }
        (true, false) => {
            ui.weak("Nothing wired in — wire a shape node's geometry into this.");
            import_button(
                ui,
                "Import its shape",
                "Raise this layer's shape onto the canvas as nodes and wire it in.\n\
                 Refused if a shape param is keyframed — bake it first.",
                || out.import_shape = Some(node.id),
            );
        }
        (false, _) => {
            ui.weak("Pick a layer. Until then this node drives nothing.");
        }
    };
}

/// What a node's header reads. The descriptor's label, unless the user renamed
/// it — except for a **sink**, which says what it drives instead of what it is.
/// "Rotation → Star" on the canvas is the whole reason a driver is worth being a
/// node: the binding is legible without selecting anything.
fn node_title(node: &GraphNode, desc: Option<&NodeDescriptor>, layers: &[LayerInfo]) -> String {
    if let Some(t) = &node.title {
        return t.clone();
    }
    let layer_name = |t: NodeId| {
        layers
            .iter()
            .find(|l| l.id == t.0)
            .map(|l| l.name.clone())
            .unwrap_or_else(|| format!("#{}", t.0))
    };
    match (node.kind.as_str(), node.config.out_target, node.config.out_shape) {
        ("out", Some((t, p)), _) => format!("{} → {}", prop_path_label(p), layer_name(t)),
        ("shapeOut", _, Some(t)) => format!("Shape → {}", layer_name(t)),
        _ => desc.map(|d| d.label.clone()).unwrap_or_else(|| format!("? {}", node.kind)),
    }
}

/// The editor for a `ref` node: which layer, which property, at what frame
/// offset. A fresh ref has no target, so the first pick seeds one (frame 0's
/// first layer, Position, offset 0), and each combo edits one field of it.
fn ref_editor(ui: &mut egui::Ui, node: &GraphNode, layers: &[LayerInfo], out: &mut NgEdits) {
    if layers.is_empty() {
        ui.weak("No layers to reference.");
        return;
    }
    let (cur_node, cur_prop, cur_off) =
        node.config.ref_target.unwrap_or((NodeId(layers[0].id), PropPath::Position, 0.0));
    let mut node_id = cur_node;
    let mut prop = cur_prop;
    let mut off = cur_off;
    // Nothing emits until the user actually picks a field — selecting a ref node
    // shouldn't silently mutate the document. The first pick seeds the whole
    // target from the shown defaults.
    let mut changed = false;

    let cur_name = layers
        .iter()
        .find(|l| l.id == cur_node.0)
        .map(|l| l.name.clone())
        .unwrap_or_else(|| format!("#{}", cur_node.0));
    egui::ComboBox::from_id_salt(("ref_node", node.id.0)).selected_text(cur_name).show_ui(ui, |ui| {
        for l in layers {
            if ui.selectable_label(l.id == cur_node.0, &l.name).clicked() && l.id != cur_node.0 {
                node_id = NodeId(l.id);
                changed = true;
            }
        }
    });
    egui::ComboBox::from_id_salt(("ref_prop", node.id.0))
        .selected_text(prop_path_label(cur_prop))
        .show_ui(ui, |ui| {
            for p in PROP_PATHS {
                if ui.selectable_label(p == cur_prop, prop_path_label(p)).clicked() && p != cur_prop {
                    prop = p;
                    changed = true;
                }
            }
        });
    if ui
        .add(egui::DragValue::new(&mut off).speed(0.5).prefix("offset "))
        .on_hover_text("Frame offset — read the target this many frames away")
        .changed()
    {
        changed = true;
    }
    if changed {
        out.op = Some(NgOp::SetRef { id: node.id, target: Some((node_id, prop, off)) });
    }
}

/// The socket id whose literal is this node kind's **constant** — the value the
/// node exists to hold, stored under an *output* socket because that's the only
/// socket it has.
///
/// One function rather than a branch in each place that cares, so the canvas and
/// the inspector can't disagree about which nodes are constants. `None` for every
/// other kind, including the other leaves (`ref`, `param`, `script`, the time
/// sources): they're leaves too, but what they produce is a read, not a literal
/// anyone can type.
pub(crate) fn const_socket(kind: &str) -> Option<&'static str> {
    match kind {
        "value" | "string" => Some("value"),
        _ => None,
    }
}

/// The literal a socket row **edits in place**, if it has one.
///
/// The rule is structural, not a list of kinds: a row is editable when there is
/// a literal there to edit — a stored override, or the descriptor's resting
/// default. Three things fall out of that without being special-cased:
///
/// - a **wired** input has no field, because the wire is the value;
/// - a `use` node's **override** socket has neither a default nor (until you
///   override it) a stored value, so it shows nothing — which is right, since
///   unset there means *inherit the module's default*, and a field seeded to
///   zero would state the opposite. Its explicit inherit/override toggle stays
///   in the inspector;
/// - a **geometry / layer / matte** socket has no default either, and no
///   `ExprValue` to put in a field if it did.
pub(crate) fn row_literal(
    graph: &NodeGraph,
    node: &GraphNode,
    socket: &motion_core::Socket,
    is_input: bool,
) -> Option<ExprValue> {
    if is_input {
        if graph.incoming(&Endpoint::new(node.id, &socket.id)).is_some() {
            return None; // wired — the wire is the value
        }
        node.value(&socket.id).or_else(|| socket.default.clone())
    } else if const_socket(&node.kind) == Some(socket.id.as_str()) {
        // The node's constant. Unset until first edited, so fall back to a
        // neutral of the socket's own kind rather than showing nothing.
        Some(node.value(&socket.id).unwrap_or_else(|| neutral_literal(socket.ty)))
    } else {
        None
    }
}

/// Draw the editor for one socket literal **on the node**, in the gap its row
/// leaves between the label and the far edge. Returns the new value when edited.
///
/// This is what makes a node self-contained: the number a `value` node holds, or
/// an oscillator's freq, is on the box you are looking at rather than in a panel
/// above it that describes whichever node happens to be selected. The inspector
/// keeps only what *isn't* a socket — a `ref`'s target, a script's source, a
/// sink's property — which is a much clearer remit than "some of the values".
fn socket_field(
    ui: &mut egui::Ui,
    row: egui::Rect,
    salt: (u64, &str),
    cur: ExprValue,
) -> Option<ExprValue> {
    // A scoped `Ui` over the row, rather than `put` per widget: a vector needs
    // two fields sharing the space, and a colour button isn't a `Widget`.
    let builder = egui::UiBuilder::new()
        .max_rect(row)
        .id_salt(salt)
        .layout(egui::Layout::left_to_right(egui::Align::Center));
    ui.scope_builder(builder, |ui| {
        // No spacing worth the name at this size, and no visible frame gap —
        // the node body is the frame.
        ui.spacing_mut().item_spacing.x = 2.0;
        ui.style_mut().spacing.interact_size.y = row.height();
        match cur {
            ExprValue::Num(n) => {
                let mut v = n;
                let w = ui.available_width();
                ui.add_sized([w, row.height()], egui::DragValue::new(&mut v).speed(0.1))
                    .changed()
                    .then_some(ExprValue::Num(v))
            }
            ExprValue::Vec2(p) => {
                let (mut x, mut y) = (p.x, p.y);
                // Halved, less the one gap between them.
                let w = (ui.available_width() - 2.0) / 2.0;
                let mut changed = false;
                changed |= ui
                    .add_sized([w, row.height()], egui::DragValue::new(&mut x).speed(0.5))
                    .changed();
                changed |= ui
                    .add_sized([w, row.height()], egui::DragValue::new(&mut y).speed(0.5))
                    .changed();
                changed.then(|| ExprValue::Vec2(Vec2::new(x, y)))
            }
            ExprValue::Color(c) => {
                let mut rgb = [c.r as f32, c.g as f32, c.b as f32];
                egui::color_picker::color_edit_button_rgb(ui, &mut rgb).changed().then(|| {
                    ExprValue::Color(MColor::rgba(
                        rgb[0] as f64,
                        rgb[1] as f64,
                        rgb[2] as f64,
                        c.a,
                    ))
                })
            }
            ExprValue::Str(t) => {
                let mut v = t;
                let w = ui.available_width();
                ui.add_sized(
                    [w, row.height()],
                    egui::TextEdit::singleline(&mut v).clip_text(true),
                )
                .changed()
                .then_some(ExprValue::Str(v))
            }
        }
    })
    .inner
}

/// A socket dot: a filled circle in the type's colour, ringed. A connected input
/// is filled solid; an unconnected one is hollow, so you can see what's wired.
fn socket_dot(painter: &egui::Painter, c: egui::Pos2, ty: SocketType, filled: bool) {
    let color = col32(ty.color());
    if filled {
        painter.circle_filled(c, DOT_R, color);
    } else {
        painter.circle(c, DOT_R, egui::Color32::from_gray(40), egui::Stroke::new(1.5, color));
    }
}
