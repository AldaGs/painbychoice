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
}

#[derive(Default)]
pub(crate) struct NgEdits {
    pub(crate) op: Option<NgOp>,
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

/// The panel. Draws the palette, then the graph on a scrollable canvas.
pub(crate) fn nodegraph_ui(
    ui: &mut egui::Ui,
    graph: &NodeGraph,
    reg: &NodeRegistry,
    out: &mut NgEdits,
) {
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.heading("Nodes");
        ui.weak(format!("{} nodes", graph.nodes.len()));
        palette_menu(ui, graph, reg, out);
    });
    ui.separator();
    if graph.nodes.is_empty() {
        ui.weak("Empty. Add a node from the palette above, then drag between sockets to wire.");
    }
    egui::ScrollArea::both().auto_shrink([false, false]).show(ui, |ui| {
        canvas(ui, graph, reg, out);
    });
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
        let resp = ui.interact(header, ui.id().with(("ng_drag", n.id.0)), egui::Sense::drag());
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
        draw_node(ui, &painter, n, desc, rect, graph, out, &mut pending, &in_pos, &out_pos);
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
    painter.rect_stroke(
        rect,
        rounding,
        egui::Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color),
        egui::StrokeKind::Inside,
    );

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
