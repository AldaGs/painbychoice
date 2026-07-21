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

// ── Canvas geometry (logical points). ────────────────────────────────────────
const NODE_W: f32 = 156.0;
const HEADER_H: f32 = 24.0;
const ROW_H: f32 = 20.0;
const BODY_PAD: f32 = 8.0;
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
}

/// One deferred edit to the driver list ([`motion_core::Binding`]). Separate
/// from [`NgOp`] because it touches the project's drivers, not the graph model.
pub(crate) enum BindingOp {
    Add { output: Endpoint, target: NodeId, prop: PropPath },
    SetOutput { index: usize, output: Endpoint },
    SetTarget { index: usize, target: NodeId },
    SetProp { index: usize, prop: PropPath },
    Remove { index: usize },
}

#[derive(Default)]
pub(crate) struct NgEdits {
    pub(crate) op: Option<NgOp>,
    pub(crate) binding: Option<BindingOp>,
    /// Raise a scene layer's property expression onto the canvas (the fold): the
    /// panel names `(layer, property)`, `App` reads its `Expr`, raises it, and
    /// binds the result back so editing the nodes drives the property.
    pub(crate) import: Option<(NodeId, PropPath)>,
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
fn node_height(desc: Option<&NodeDescriptor>) -> f32 {
    let rows = desc.map_or(1, |d| d.inputs.len().max(d.outputs.len()).max(1));
    HEADER_H + rows as f32 * ROW_H + BODY_PAD
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

/// The panel. Palette + drivers + a selected-node inspector, then the graph on a
/// scrollable canvas.
pub(crate) fn nodegraph_ui(
    ui: &mut egui::Ui,
    graph: &NodeGraph,
    reg: &NodeRegistry,
    layers: &[(u64, String)],
    bindings: &[Binding],
    out: &mut NgEdits,
) {
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.heading("Nodes");
        ui.weak(format!("{} nodes", graph.nodes.len()));
        palette_menu(ui, graph, reg, out);
    });
    ui.separator();
    drivers_ui(ui, graph, reg, layers, bindings, out);
    ui.separator();
    import_ui(ui, layers, out);
    ui.separator();
    inspector_ui(ui, graph, reg, layers, out);
    ui.separator();
    if graph.nodes.is_empty() {
        ui.weak("Empty. Add a node from the palette above, then drag between sockets to wire.");
    }
    egui::ScrollArea::both().auto_shrink([false, false]).show(ui, |ui| {
        canvas(ui, graph, reg, out);
    });
}

/// Which graph node is selected for the inspector — ephemeral view state, so it
/// lives in egui memory under a fixed id (the canvas writes it, the inspector,
/// drawn in a different `Ui`, reads it, so a per-`Ui` id wouldn't match).
fn read_selection(ctx: &egui::Context) -> Option<GraphNodeId> {
    ctx.data(|d| d.get_temp::<Option<GraphNodeId>>(egui::Id::new("ng_sel"))).flatten()
}

fn write_selection(ctx: &egui::Context, sel: Option<GraphNodeId>) {
    ctx.data_mut(|d| d.insert_temp(egui::Id::new("ng_sel"), sel));
}

/// Every output socket in the graph, labelled `node.socket` — the driver row's
/// source picker, and reused to colour wires.
fn graph_outputs(graph: &NodeGraph, reg: &NodeRegistry) -> Vec<(Endpoint, String)> {
    let mut out = Vec::new();
    for n in &graph.nodes {
        if let Some(d) = reg.get(&n.kind) {
            let title = n.title.clone().unwrap_or_else(|| d.label.clone());
            for s in &d.outputs {
                out.push((Endpoint::new(n.id, &s.id), format!("{title}.{}", s.label)));
            }
        }
    }
    out
}

/// The **Drivers**: each row binds one graph output to one layer's property.
/// This is what makes the graph *do* something — `App` lowers each binding to an
/// `Expr` and hands it to the property.
fn drivers_ui(
    ui: &mut egui::Ui,
    graph: &NodeGraph,
    reg: &NodeRegistry,
    layers: &[(u64, String)],
    bindings: &[Binding],
    out: &mut NgEdits,
) {
    let outputs = graph_outputs(graph, reg);
    ui.horizontal(|ui| {
        ui.strong("Drivers");
        ui.weak(format!("{}", bindings.len()));
        let can_add = !outputs.is_empty() && !layers.is_empty();
        if ui
            .add_enabled(can_add, egui::Button::new(format!("{} Add", icon::ADD)).small())
            .on_disabled_hover_text("Add a node and a layer first")
            .clicked()
        {
            out.binding = Some(BindingOp::Add {
                output: outputs[0].0.clone(),
                target: NodeId(layers[0].0),
                prop: PropPath::Rotation,
            });
        }
    });
    if bindings.is_empty() {
        ui.weak("None. Bind a graph output to a layer's property to drive it.");
    }
    for (i, b) in bindings.iter().enumerate() {
        ui.horizontal(|ui| {
            if ui.small_button("x").on_hover_text("Remove (freezes the property)").clicked() {
                out.binding = Some(BindingOp::Remove { index: i });
            }
            let cur_out = outputs
                .iter()
                .find(|(e, _)| e == &b.output)
                .map(|(_, l)| l.clone())
                .unwrap_or_else(|| "<gone>".into());
            egui::ComboBox::from_id_salt(("drv_out", i)).width(110.0).selected_text(cur_out).show_ui(
                ui,
                |ui| {
                    for (e, l) in &outputs {
                        if ui.selectable_label(e == &b.output, l).clicked() && e != &b.output {
                            out.binding =
                                Some(BindingOp::SetOutput { index: i, output: e.clone() });
                        }
                    }
                },
            );
            ui.label("→");
            let cur_layer = layers
                .iter()
                .find(|(id, _)| *id == b.target.0)
                .map(|(_, n)| n.clone())
                .unwrap_or_else(|| format!("#{}", b.target.0));
            egui::ComboBox::from_id_salt(("drv_tgt", i)).width(90.0).selected_text(cur_layer).show_ui(
                ui,
                |ui| {
                    for (id, name) in layers {
                        if ui.selectable_label(*id == b.target.0, name).clicked()
                            && *id != b.target.0
                        {
                            out.binding =
                                Some(BindingOp::SetTarget { index: i, target: NodeId(*id) });
                        }
                    }
                },
            );
            egui::ComboBox::from_id_salt(("drv_prop", i))
                .width(80.0)
                .selected_text(prop_path_label(b.prop))
                .show_ui(ui, |ui| {
                    for k in PROP_PATHS {
                        if ui.selectable_label(k == b.prop, prop_path_label(k)).clicked()
                            && k != b.prop
                        {
                            out.binding = Some(BindingOp::SetProp { index: i, prop: k });
                        }
                    }
                });
        });
    }
}

/// The **Import** row — the fold's front door: pull an expression-driven
/// property onto the canvas as nodes. Picks a layer + property; `App` raises that
/// property's `Expr` into the graph and binds it back, so the recipe you built in
/// the old per-property editor becomes editable here. The pending pick lives in
/// egui memory (view state), like the driver combos.
fn import_ui(ui: &mut egui::Ui, layers: &[(u64, String)], out: &mut NgEdits) {
    if layers.is_empty() {
        return;
    }
    let mem = egui::Id::new("ng_import_sel");
    let mut sel: (u64, PropPath) =
        ui.ctx().data(|d| d.get_temp(mem)).unwrap_or((layers[0].0, PropPath::Rotation));
    ui.horizontal(|ui| {
        ui.strong("Import");
        let cur_layer = layers
            .iter()
            .find(|(i, _)| *i == sel.0)
            .map(|(_, n)| n.clone())
            .unwrap_or_else(|| format!("#{}", sel.0));
        egui::ComboBox::from_id_salt("imp_layer").width(90.0).selected_text(cur_layer).show_ui(
            ui,
            |ui| {
                for (i, n) in layers {
                    if ui.selectable_label(*i == sel.0, n).clicked() {
                        sel.0 = *i;
                    }
                }
            },
        );
        egui::ComboBox::from_id_salt("imp_prop").width(80.0).selected_text(prop_path_label(sel.1)).show_ui(
            ui,
            |ui| {
                for p in PROP_PATHS {
                    if ui.selectable_label(p == sel.1, prop_path_label(p)).clicked() {
                        sel.1 = p;
                    }
                }
            },
        );
        if ui
            .button("→ nodes")
            .on_hover_text("Raise this property's expression onto the canvas")
            .clicked()
        {
            out.import = Some((NodeId(sel.0), sel.1));
        }
    });
    ui.weak("Pulls an expression-driven property onto the graph as nodes.");
    ui.ctx().data_mut(|d| d.insert_temp(mem, sel));
}

/// The inspector for the selected node: drag editors for its `value` constant
/// and any **unwired** numeric input, so a graph's literals are tunable without
/// canvas widgets. A wired input has no field — its value comes down the wire.
fn inspector_ui(
    ui: &mut egui::Ui,
    graph: &NodeGraph,
    reg: &NodeRegistry,
    layers: &[(u64, String)],
    out: &mut NgEdits,
) {
    let sel = read_selection(ui.ctx());
    let Some((node, desc)) = sel.and_then(|id| {
        let n = graph.node(id)?;
        Some((n, reg.get(&n.kind)?))
    }) else {
        ui.weak("Select a node (click its header) to edit its values.");
        return;
    };
    ui.strong(format!("Values — {}", node.title.clone().unwrap_or_else(|| desc.label.clone())));
    // A `ref` reads another layer's property; a `param` reads the driven layer's
    // own knob. Both carry addressing rather than a socket value.
    if node.kind == "ref" {
        ref_editor(ui, node, layers, out);
        return;
    }
    if node.kind == "param" {
        let mut name = node.config.param.clone();
        if ui
            .add(egui::TextEdit::singleline(&mut name).hint_text("knob name").desired_width(140.0))
            .on_hover_text("Reads this knob on whichever layer a driver points at")
            .changed()
        {
            out.op = Some(NgOp::SetParam { id: node.id, name });
        }
        return;
    }
    let mut num_field = |ui: &mut egui::Ui, socket: &str, label: &str, cur: f64| {
        let mut v = cur;
        if ui.add(egui::DragValue::new(&mut v).speed(0.1).prefix(format!("{label}: "))).changed() {
            out.op = Some(NgOp::SetValue {
                id: node.id,
                socket: socket.to_string(),
                value: ExprValue::Num(v),
            });
        }
    };
    // A `value` node's constant lives under its output socket id.
    if node.kind == "value" {
        let cur = match node.value("value") {
            Some(ExprValue::Num(n)) => n,
            _ => 0.0,
        };
        num_field(ui, "value", "value", cur);
    }
    for s in &desc.inputs {
        if !matches!(s.ty, SocketType::Number | SocketType::Time) {
            continue;
        }
        if graph.incoming(&Endpoint::new(node.id, &s.id)).is_some() {
            continue; // wired — no literal to edit
        }
        let cur = match node.value(&s.id).or(s.default) {
            Some(ExprValue::Num(n)) => n,
            _ => 0.0,
        };
        num_field(ui, &s.id, &s.label, cur);
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
            });
        }
    });
}

/// Lay the graph out and interact with it. Node positions come from the model
/// (they're saved), so a drag emits a `Move` op and — for the frame in hand —
/// the delta is applied locally so the box tracks the pointer without a lag.
fn canvas(ui: &mut egui::Ui, graph: &NodeGraph, reg: &NodeRegistry, out: &mut NgEdits) {
    // Content extent covers every node so the scroll area can reach them.
    let mut extent = egui::vec2(NODE_W, ROW_H);
    for n in &graph.nodes {
        let h = node_height(reg.get(&n.kind));
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
        let h = node_height(reg.get(&n.kind));
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
        let Some(desc) = reg.get(&n.kind) else { continue };
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
            let ty = reg
                .get(&graph.node(e.from.node).map(|n| n.kind.clone()).unwrap_or_default())
                .and_then(|d| d.find_output(&e.from.socket))
                .map(|s| s.ty);
            let c = ty.map_or(egui::Color32::GRAY, |t| col32(t.color()));
            wire(&painter, a, b, c);
        }
    }

    // Pass 3 — the boxes: fill, header, sockets, delete. Painted over the wires,
    // and their socket/×/ interactions take pointer priority where they sit.
    for n in &graph.nodes {
        let top = live_pos[&n.id];
        let desc = reg.get(&n.kind);
        let h = node_height(desc);
        let rect = egui::Rect::from_min_size(top, egui::vec2(NODE_W, h));
        let is_sel = selected == Some(n.id);
        draw_node(ui, &painter, n, desc, rect, is_sel, graph, out, &mut pending, &in_pos, &out_pos);
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
    out: &mut NgEdits,
    pending: &mut Option<Endpoint>,
    in_pos: &std::collections::HashMap<Endpoint, egui::Pos2>,
    out_pos: &std::collections::HashMap<Endpoint, egui::Pos2>,
) {
    let rounding = 6.0;
    // Body.
    painter.rect_filled(rect, rounding, ui.visuals().extreme_bg_color);
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
    let border = if selected {
        egui::Stroke::new(2.0, egui::Color32::from_rgb(220, 160, 60))
    } else {
        egui::Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color)
    };
    painter.rect_stroke(rect, rounding, border, egui::StrokeKind::Inside);

    // Title, and a delete button at the header's right.
    let title = node.title.clone().unwrap_or_else(|| {
        desc.map(|d| d.label.clone()).unwrap_or_else(|| format!("? {}", node.kind))
    });
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
    painter.text(del.center(), egui::Align2::CENTER_CENTER, "✕", egui::FontId::proportional(12.0), del_col);
    if del_resp.clicked() {
        out.op = Some(NgOp::Remove { id: node.id });
    }

    let Some(desc) = desc else { return };

    // Input sockets: dot + label on the left. Secondary-click disconnects the
    // wire feeding it. It's also a drop target, resolved globally on release.
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
        let hit = egui::Rect::from_center_size(c, egui::vec2(DOT_HIT * 2.0, DOT_HIT * 2.0));
        let resp = ui.interact(hit, ui.id().with(("ng_in", node.id.0, i)), egui::Sense::click());
        if resp.secondary_clicked() {
            if let Some(edge) = graph.incoming(&ep) {
                out.op = Some(NgOp::Disconnect { edge: edge.clone() });
            }
        }
    }

    // Output sockets: dot + label on the right. Dragging one starts a wire.
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
        let hit = egui::Rect::from_center_size(c, egui::vec2(DOT_HIT * 2.0, DOT_HIT * 2.0));
        let resp = ui.interact(hit, ui.id().with(("ng_out", node.id.0, j)), egui::Sense::drag());
        if resp.drag_started() {
            *pending = Some(ep);
        }
    }
}

/// The editor for a `ref` node: which layer, which property, at what frame
/// offset. A fresh ref has no target, so the first pick seeds one (frame 0's
/// first layer, Position, offset 0), and each combo edits one field of it.
fn ref_editor(ui: &mut egui::Ui, node: &GraphNode, layers: &[(u64, String)], out: &mut NgEdits) {
    if layers.is_empty() {
        ui.weak("No layers to reference.");
        return;
    }
    let (cur_node, cur_prop, cur_off) =
        node.config.ref_target.unwrap_or((NodeId(layers[0].0), PropPath::Position, 0.0));
    let mut node_id = cur_node;
    let mut prop = cur_prop;
    let mut off = cur_off;
    // Nothing emits until the user actually picks a field — selecting a ref node
    // shouldn't silently mutate the document. The first pick seeds the whole
    // target from the shown defaults.
    let mut changed = false;

    let cur_name = layers
        .iter()
        .find(|(id, _)| *id == cur_node.0)
        .map(|(_, n)| n.clone())
        .unwrap_or_else(|| format!("#{}", cur_node.0));
    egui::ComboBox::from_id_salt(("ref_node", node.id.0)).selected_text(cur_name).show_ui(ui, |ui| {
        for (id, name) in layers {
            if ui.selectable_label(*id == cur_node.0, name).clicked() && *id != cur_node.0 {
                node_id = NodeId(*id);
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
