//! The expression / node-graph panel: layout, boxes, editors, and the
//! `GraphOp` mutations they report back.
//!
//! Moved verbatim out of `main.rs` when it was split by concern; the
//! only edit was widening visibility to `pub(crate)`.

use crate::*;

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
    }
}

/// Which kind of parameter an "add" button creates. The UI's counterpart to
/// core's `ParamValue`, without carrying a value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ParamKind {
    Num,
    Vec,
    Color,
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
        }
    }
}

/// Every script node's current result, keyed by its `(property, tree-path)`
/// address. `Ok` is the formatted value, `Err` the first line of the error.
type ScriptResults = HashMap<(PropKind, Vec<usize>), Result<String, String>>;

/// One property in the graph panel: its kind, and — if it's expression-driven —
/// a clone of the tree to render and its printed form.
pub(crate) struct GraphProp {
    pub(crate) kind: PropKind,
    pub(crate) label: &'static str,
    pub(crate) is_expr: bool,
    pub(crate) expr: Option<Expr>,
    pub(crate) printed: Option<String>,
}

/// What the graph panel renders: the selected node, its editable properties, and
/// every node (for a reference target picker). Cloned before the UI pass so the
/// panel never borrows `App`.
pub(crate) struct GraphInfo {
    pub(crate) node_id: NodeId,
    pub(crate) node_name: String,
    pub(crate) props: Vec<GraphProp>,
    /// (id, name) of every node in the document.
    pub(crate) nodes: Vec<(u64, String)>,
    /// The selected node's parameter names and kinds, in display order.
    pub(crate) params: Vec<(String, &'static str)>,
    /// Each script node's result at the current frame, addressed the same way
    /// an edit is: `(property, tree-path)`. Evaluated **here**, against the
    /// document, because `value()`/`wiggle()` need a real `EvalCtx` — a
    /// doc-less preview would report "no node named …" for a script that in
    /// fact resolves fine at render time.
    pub(crate) script_results: ScriptResults,
}

impl GraphInfo {
    pub(crate) fn gather(doc: &Document, selected: Option<NodeId>, frame: f64) -> Option<GraphInfo> {
        let id = selected?;
        let node = doc.root.find(id)?;
        let props: Vec<GraphProp> = PropKind::ALL
            .iter()
            .filter_map(|&kind| {
                let p = prop_of(node, kind)?;
                Some(GraphProp {
                    kind,
                    label: kind.label(),
                    is_expr: p.is_expr(),
                    expr: p.expr().cloned(),
                    printed: p.expr().map(|e| e.to_string()),
                })
            })
            .collect();
        let mut nodes = Vec::new();
        collect_nodes(&doc.root, &mut nodes);

        let mut script_results = ScriptResults::new();
        for prop in &props {
            if let Some(expr) = &prop.expr {
                let mut ctx = EvalCtx::new(doc, frame);
                // In this node's context, so a script's `param("x")` previews
                // the same value it resolves to at render time.
                ctx.in_node(id, |ctx| {
                    collect_scripts(expr, prop.kind, &mut Vec::new(), ctx, &mut script_results)
                });
            }
        }
        let params =
            node.params.iter().map(|p| (p.name.clone(), p.value.kind_name())).collect();
        Some(GraphInfo {
            node_id: id,
            node_name: node.name.clone(),
            props,
            nodes,
            params,
            script_results,
        })
    }
}

/// Walk `expr`, evaluating every `Script` node it contains against `ctx` and
/// recording the result under its `(property, tree-path)` address — the address
/// the editor looks it up by.
pub(crate) fn collect_scripts(
    expr: &Expr,
    kind: PropKind,
    path: &mut Vec<usize>,
    ctx: &mut EvalCtx,
    out: &mut ScriptResults,
) {
    if let Expr::Script(src) = expr {
        let result = motion_core::eval_script_ctx(src, ctx)
            .map(|v| v.to_string())
            .map_err(|e| e.lines().next().unwrap_or("error").to_string());
        out.insert((kind, path.clone()), result);
    }
    for slot in 0..expr.arity() {
        if let Some(child) = child_ref(expr, slot) {
            path.push(slot);
            collect_scripts(child, kind, path, ctx, out);
            path.pop();
        }
    }
}

pub(crate) fn collect_nodes(node: &MNode, out: &mut Vec<(u64, String)>) {
    out.push((node.id.0, node.name.clone()));
    for c in &node.children {
        collect_nodes(c, out);
    }
}

/// One deferred graph edit. Like the dock, the panel records at most one per
/// frame against a `(property, tree-path)` address and `App` applies it after
/// the UI pass, so the tree isn't restructured mid-render.
pub(crate) enum GraphOp {
    /// Make a property expression-driven (seeded from its current value).
    Promote(PropKind),
    /// Freeze an expression back to a constant.
    Bake(PropKind),
    /// Replace the node at `path` with a fresh node of `new` kind.
    SetKind { kind: PropKind, path: Vec<usize>, new: ExprKind },
    /// Set the literal at `path`.
    SetLit { kind: PropKind, path: Vec<usize>, value: ExprValue },
    /// Set the reference at `path`.
    SetRef { kind: PropKind, path: Vec<usize>, node: NodeId, prop: PropPath, offset: f64 },
    /// Set the script source at `path`.
    SetScript { kind: PropKind, path: Vec<usize>, src: String },
    /// Set an oscillator's waveform at `path`.
    SetWaveform { kind: PropKind, path: Vec<usize>, wave: Waveform },
    /// Point a `param` node at a parameter of the node that owns the property.
    SetParam { kind: PropKind, path: Vec<usize>, name: String },
    /// Add a parameter to the selected node, seeded with a neutral value.
    AddParam { name: String, kind: ParamKind },
    /// Remove a parameter. Expressions reading it aren't rewritten — they warn.
    RemoveParam { name: String },
}

#[derive(Default)]
pub(crate) struct GraphEdits {
    pub(crate) op: Option<GraphOp>,
}

// Canvas geometry (logical points). A node box sits at (depth·COL_W, y) inside
// the scrolled content; its *height* varies by kind (see `box_height`).
pub(crate) const GRAPH_COL_W: f32 = 172.0;
pub(crate) const GRAPH_BOX_W: f32 = 152.0;
pub(crate) const GRAPH_V_GAP: f32 = 12.0;
pub(crate) const GRAPH_MARGIN: f32 = 10.0;
/// Extra height reserved for a knob box's slot-label line (`freq`/`amp`/…).
pub(crate) const GRAPH_LABEL_H: f32 = 15.0;

/// How tall a node's box needs to be, by kind — enough for its controls, plus a
/// line for its slot label when it's a labelled generator knob. A `ref` node
/// stacks three pickers plus an offset, a `script` a field and its result line, a
/// `value` its editor, an oscillator its kind + waveform pickers, another
/// generator just its kind picker (its knobs are separate boxes), and an
/// operator just its kind picker.
pub(crate) fn box_height(expr: &Expr, labeled: bool) -> f32 {
    let base = match expr {
        Expr::Ref { .. } => 100.0,
        Expr::Script(_) => 66.0,
        Expr::Param { .. } => 66.0,
        Expr::Lit(_) => 50.0,
        Expr::Gen(Generator::Oscillator { .. }) => 56.0,
        Expr::Gen(_) => 30.0,
        Expr::Add(..) | Expr::Mul(..) | Expr::Neg(..) => 30.0,
    };
    base + if labeled { GRAPH_LABEL_H } else { 0.0 }
}

/// A node's place in the auto-laid-out expression tree: its `path`, tree `depth`
/// (its column), and its box's `y` top and `height` (in content-local points).
/// The layout is a pure function of the tree, so it's unit-tested.
pub(crate) struct ExprBox {
    pub(crate) path: Vec<usize>,
    pub(crate) depth: usize,
    pub(crate) y: f32,
    pub(crate) height: f32,
}

#[cfg(test)]
impl ExprBox {
    /// The vertical centre of the box (wires attach here). Used by the layout
    /// tests; the canvas derives the same point from each box's rect.
    pub(crate) fn center_y(&self) -> f32 {
        self.y + self.height / 2.0
    }
}

/// Lay an expression tree out as a tidy tree: root on the left, children to the
/// right, leaves stacked top-to-bottom (each reserving its own height + a gap)
/// and every parent centred on the span of its children.
pub(crate) fn layout_expr(expr: &Expr) -> Vec<ExprBox> {
    // `labeled` reserves the extra line a generator knob's slot label needs, so
    // the box below still clears it. Returns the laid-out node's centre-y, so a
    // parent can centre on its kids.
    fn rec(
        expr: &Expr,
        path: &mut Vec<usize>,
        depth: usize,
        labeled: bool,
        cursor_y: &mut f32,
        out: &mut Vec<ExprBox>,
    ) -> f32 {
        let height = box_height(expr, labeled);
        if expr.arity() == 0 {
            let y = *cursor_y;
            *cursor_y += height + GRAPH_V_GAP;
            out.push(ExprBox { path: path.clone(), depth, y, height });
            y + height / 2.0
        } else {
            let (mut first, mut last) = (0.0, 0.0);
            for slot in 0..expr.arity() {
                let child_labeled = expr.slot_label(slot).is_some();
                path.push(slot);
                let c = rec(
                    child_ref(expr, slot).unwrap(),
                    path,
                    depth + 1,
                    child_labeled,
                    cursor_y,
                    out,
                );
                path.pop();
                if slot == 0 {
                    first = c;
                }
                last = c;
            }
            let center = (first + last) / 2.0;
            out.push(ExprBox { path: path.clone(), depth, y: center - height / 2.0, height });
            center
        }
    }
    let mut out = Vec::new();
    rec(expr, &mut Vec::new(), 0, false, &mut 0.0, &mut out);
    out
}

/// The expression/node-graph panel: for the selected node, promote a property to
/// an expression, edit its tree on a node canvas, or bake it back to a constant.
pub(crate) fn graph_ui(ui: &mut egui::Ui, info: &Option<GraphInfo>, frame: f64, out: &mut GraphEdits) {
    ui.add_space(8.0);
    ui.heading("Graph");
    let Some(info) = info else {
        ui.weak("Select a node to drive its properties with expressions.");
        return;
    };
    ui.weak(format!("Node: {}  ·  drag a node to arrange", info.node_name));
    ui.separator();
    let param_names = params_ui(ui, info, out);
    ui.separator();
    egui::ScrollArea::both()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for prop in &info.props {
                ui.horizontal(|ui| {
                    ui.strong(prop.label);
                    if prop.is_expr {
                        if ui.small_button("bake").on_hover_text("Freeze to a constant").clicked() {
                            out.op = Some(GraphOp::Bake(prop.kind));
                        }
                        if let Some(printed) = &prop.printed {
                            ui.weak(format!("= {printed}"));
                        }
                    } else if ui
                        .small_button("= fx")
                        .on_hover_text("Drive with an expression")
                        .clicked()
                    {
                        out.op = Some(GraphOp::Promote(prop.kind));
                    }
                });
                if let Some(expr) = &prop.expr {
                    expr_canvas(
                        ui,
                        expr,
                        info.node_id,
                        prop.kind,
                        frame,
                        &info.nodes,
                        &param_names,
                        &info.script_results,
                        out,
                    );
                    ui.separator();
                }
            }
        });
}

/// Manually-placed node positions for one property's canvas, in content-local
/// points. Absent entries fall back to the auto-layout; this is ephemeral view
/// state (egui memory), not saved with the document.
type GraphPositions = std::collections::HashMap<Vec<usize>, egui::Vec2>;

/// Draw one property's expression as a node canvas: boxes at their positions
/// (auto-laid-out, or wherever the user has dragged them), wires from each
/// node's output (right) to its parent's input (left), and a compact editor
/// inside each box. Every editor control reports a [`GraphOp`] against the
/// node's tree-path; dragging updates only the (ephemeral) position memory —
/// neither mutates the document here.
#[allow(clippy::too_many_arguments)]
pub(crate) fn expr_canvas(
    ui: &mut egui::Ui,
    expr: &Expr,
    node_id: NodeId,
    kind: PropKind,
    frame: f64,
    nodes: &[(u64, String)],
    params: &[String],
    results: &ScriptResults,
    out: &mut GraphEdits,
) {
    let boxes = layout_expr(expr);

    // Positions are remembered per (node, property) in egui memory; a box with
    // no stored position falls back to its auto-layout slot (column × its y).
    let mem_id = ui.id().with(("graphpos", node_id.0, kind.label()));
    let mut positions: GraphPositions =
        ui.data(|d| d.get_temp::<GraphPositions>(mem_id)).unwrap_or_default();
    let pos_of = |b: &ExprBox, positions: &GraphPositions| {
        positions
            .get(&b.path)
            .copied()
            .unwrap_or_else(|| egui::vec2(b.depth as f32 * GRAPH_COL_W, b.y))
    };
    // A box's rect at content-local top-left `p`, using its own (kind-based) height.
    let rect_of = |b: &ExprBox, p: egui::Vec2, origin: egui::Pos2| {
        egui::Rect::from_min_size(origin + p, egui::vec2(GRAPH_BOX_W, b.height))
    };

    // Content bounds cover every box (including dragged-out ones) so the scroll
    // area can reach them.
    let mut extent = egui::vec2(0.0, 0.0);
    for b in &boxes {
        let p = pos_of(b, &positions);
        extent.x = extent.x.max(p.x + GRAPH_BOX_W);
        extent.y = extent.y.max(p.y + b.height);
    }
    let (area, _) = ui.allocate_exact_size(extent + egui::vec2(GRAPH_MARGIN, GRAPH_MARGIN), egui::Sense::hover());
    let origin = area.min + egui::vec2(GRAPH_MARGIN, GRAPH_MARGIN);

    // Wires under the boxes: each node's left-centre to its parent's right-centre.
    let wire = ui.visuals().weak_text_color();
    for b in &boxes {
        if let Some((_, parent_path)) = b.path.split_last() {
            if let Some(pb) = boxes.iter().find(|x| x.path == parent_path) {
                let child_in = rect_of(b, pos_of(b, &positions), origin).left_center();
                let parent_out = rect_of(pb, pos_of(pb, &positions), origin).right_center();
                ui.painter().line_segment([parent_out, child_in], egui::Stroke::new(1.5, wire));
            }
        }
    }

    // Boxes on top: a drag response for the box body, then the editor widgets
    // (which take pointer priority where they sit, so dragging an empty part of
    // the box moves it while the controls stay usable).
    for b in &boxes {
        let mut p = pos_of(b, &positions);
        let drag_id = ui.id().with(("graphbox", node_id.0, kind.label(), b.path.as_slice()));
        let resp = ui.interact(rect_of(b, p, origin), drag_id, egui::Sense::drag());
        if resp.dragged() {
            p = (p + resp.drag_delta()).max(egui::vec2(0.0, 0.0));
            positions.insert(b.path.clone(), p);
        }
        let rect = rect_of(b, p, origin);
        let node = expr.at(&b.path).unwrap_or(expr);
        ui.painter().rect_filled(rect, 4.0, ui.visuals().extreme_bg_color);
        ui.painter().rect_stroke(
            rect,
            4.0,
            egui::Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color),
            egui::StrokeKind::Inside,
        );
        // A generator knob shows its name (`freq`/`amp`/…) so a wired box is
        // read the same as the labelled slot it fills; operator operands are
        // positional and unlabelled.
        let slot_label = b.path.split_last().and_then(|(&slot, parent)| {
            expr.at(parent).and_then(|p| p.slot_label(slot))
        });
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink(5.0)));
        expr_box(&mut child, node, kind, frame, &b.path, slot_label, nodes, params, results, out);
    }

    ui.data_mut(|d| d.insert_temp(mem_id, positions));
}

/// The controls inside one canvas box: a kind picker, and a compact editor for a
/// `Lit`/`Ref`. Operators (`Add`/`Mul`/`Neg`) show only their kind — their
/// inputs are separate boxes wired in.
#[allow(clippy::too_many_arguments)]
pub(crate) fn expr_box(
    ui: &mut egui::Ui,
    expr: &Expr,
    kind: PropKind,
    frame: f64,
    path: &[usize],
    slot_label: Option<&str>,
    nodes: &[(u64, String)],
    params: &[String],
    results: &ScriptResults,
    out: &mut GraphEdits,
) {
    if let Some(label) = slot_label {
        ui.small(label);
    }
    let cur = expr.kind();
    egui::ComboBox::from_id_salt(("ek", kind.label(), path))
        .width(60.0)
        .selected_text(cur.label())
        .show_ui(ui, |ui| {
            for k in ExprKind::ALL {
                if ui.selectable_label(k == cur, k.label()).clicked() && k != cur {
                    out.op = Some(GraphOp::SetKind { kind, path: path.to_vec(), new: k });
                }
            }
        });
    match expr {
        Expr::Lit(v) => lit_editor(ui, *v, kind, path, out),
        Expr::Ref { node, prop, time_offset } => {
            ref_editor(ui, *node, *prop, *time_offset, kind, path, nodes, out)
        }
        Expr::Param { name, .. } => param_editor(ui, name, params, kind, path, out),
        Expr::Script(src) => {
            let result = results.get(&(kind, path.to_vec()));
            script_editor(ui, src, frame, result, kind, path, out)
        }
        // A generator's knobs are wired-in child boxes; the only in-box control
        // is the oscillator's waveform picker.
        Expr::Gen(Generator::Oscillator { wave, .. }) => {
            wave_editor(ui, *wave, kind, path, out)
        }
        _ => {}
    }
}

/// The oscillator's waveform picker (`sine`/`triangle`/`square`/`saw`).
pub(crate) fn wave_editor(
    ui: &mut egui::Ui,
    wave: Waveform,
    kind: PropKind,
    path: &[usize],
    out: &mut GraphEdits,
) {
    egui::ComboBox::from_id_salt(("wave", kind.label(), path))
        .width(90.0)
        .selected_text(wave.label())
        .show_ui(ui, |ui| {
            for w in Waveform::ALL {
                if ui.selectable_label(w == wave, w.label()).clicked() && w != wave {
                    out.op = Some(GraphOp::SetWaveform { kind, path: path.to_vec(), wave: w });
                }
            }
        });
}

/// The selected node's parameters: what exists, plus add/remove. Returns the
/// names, which the `param` nodes below use to populate their picker.
///
/// Parameters live here rather than in the properties panel because they only
/// mean anything to expressions — a knob nothing reads is noise in a list of
/// real properties.
pub(crate) fn params_ui(ui: &mut egui::Ui, info: &GraphInfo, out: &mut GraphEdits) -> Vec<String> {
    let names: Vec<String> = info.params.iter().map(|(n, _)| n.clone()).collect();
    ui.horizontal_wrapped(|ui| {
        ui.strong("Parameters");
        // The new parameter's name is typed into egui memory, so the panel
        // stays a pure function of the document (the same rule as the canvas'
        // box positions).
        let buf_id = egui::Id::new(("param_new", info.node_id.0));
        let mut buf: String = ui.data_mut(|d| d.get_temp(buf_id).unwrap_or_default());
        ui.add(
            egui::TextEdit::singleline(&mut buf).hint_text("new name").desired_width(90.0),
        );
        for (label, kind) in
            [("+num", ParamKind::Num), ("+vec", ParamKind::Vec), ("+col", ParamKind::Color)]
        {
            let taken = names.contains(&buf);
            let ok = !buf.trim().is_empty() && !taken;
            if ui
                .add_enabled(ok, egui::Button::new(label).small())
                .on_disabled_hover_text(if taken {
                    "that name is taken"
                } else {
                    "type a name first"
                })
                .clicked()
            {
                out.op = Some(GraphOp::AddParam { name: buf.trim().to_string(), kind });
                buf.clear();
            }
        }
        ui.data_mut(|d| d.insert_temp(buf_id, buf));
    });
    if info.params.is_empty() {
        ui.weak("None. A parameter is a named knob expressions can read.");
    }
    for (name, kind) in &info.params {
        ui.horizontal(|ui| {
            if ui.small_button("x").on_hover_text("Remove").clicked() {
                out.op = Some(GraphOp::RemoveParam { name: name.clone() });
            }
            ui.label(name);
            ui.weak(*kind);
        });
    }
    names
}

/// Pick which of the owning node's parameters this node reads. A combo rather
/// than a free-text field: the parameters that exist are knowable, and a typo
/// would only surface as a warning at render time.
pub(crate) fn param_editor(
    ui: &mut egui::Ui,
    name: &str,
    params: &[String],
    kind: PropKind,
    path: &[usize],
    out: &mut GraphEdits,
) {
    if params.is_empty() {
        ui.weak("no parameters");
        ui.small("add one above");
        return;
    }
    egui::ComboBox::from_id_salt(("pn", kind.label(), path))
        .width(120.0)
        .selected_text(if name.is_empty() { "(pick)" } else { name })
        .show_ui(ui, |ui| {
            for p in params {
                if ui.selectable_label(p == name, p).clicked() && p != name {
                    out.op = Some(GraphOp::SetParam {
                        kind,
                        path: path.to_vec(),
                        name: p.clone(),
                    });
                }
            }
        });
    // A name that no longer matches any parameter (renamed or removed) would
    // otherwise look like a valid pick.
    if !name.is_empty() && !params.iter().any(|p| p == name) {
        ui.colored_label(egui::Color32::from_rgb(220, 90, 90), format!("'{name}' is gone"));
    }
}

/// A one-line Rhai editor with live feedback: below the field, the value the
/// script currently evaluates to, or the error (in red) if it doesn't compile.
pub(crate) fn script_editor(
    ui: &mut egui::Ui,
    src: &str,
    frame: f64,
    result: Option<&Result<String, String>>,
    kind: PropKind,
    path: &[usize],
    out: &mut GraphEdits,
) {
    let mut text = src.to_string();
    let resp = ui
        .add(
            egui::TextEdit::singleline(&mut text)
                .hint_text("frame * 2.0")
                .desired_width(f32::INFINITY)
                .font(egui::TextStyle::Monospace),
        )
        .on_hover_text(SCRIPT_HELP);
    if resp.changed() {
        out.op = Some(GraphOp::SetScript { kind, path: path.to_vec(), src: text.clone() });
    }
    // The result was computed in `GraphInfo::gather`, against the document, so
    // `value()`/`wiggle()` resolve here exactly as they do at render time.
    // While the field is being edited the snapshot is one frame behind the
    // text, so fall back to a doc-less eval for that frame only — it can't
    // resolve `value()`, and says so rather than showing a stale result.
    match result.map(|r| r.as_ref()) {
        Some(Ok(v)) => {
            ui.weak(format!("= {v}"));
        }
        Some(Err(e)) => {
            ui.colored_label(egui::Color32::from_rgb(220, 90, 90), e);
        }
        None => match motion_core::eval_script(&text, frame) {
            Ok(v) => {
                ui.weak(format!("= {v}"));
            }
            Err(e) => {
                let msg = e.lines().next().unwrap_or("error").to_string();
                ui.colored_label(egui::Color32::from_rgb(220, 90, 90), msg);
            }
        },
    }
}

/// What a script can call, shown on hover over the field. Kept short — it's a
/// reminder of the vocabulary, not documentation.
pub(crate) const SCRIPT_HELP: &str = "\
Rhai. Return a number, or an array: [x, y] or [r, g, b(, a)].

In scope:
  frame, time          the current frame
  value(\"node\", \"prop\")            another node's property
  value_at(\"node\", \"prop\", frame)  …at another frame
  wiggle(freq, amp)               smooth noise, deterministic
  wiggle(freq, amp, seed)         an independent stream

prop: position, rotation, scale, opacity, anchor, fill,
      stroke_color, stroke_width, size, radius

Nodes are named, not id'd; a vec/colour comes back as an array.";

/// The child expression at `slot` — an operator operand (0/1 for `Add`/`Mul`,
/// 0 for `Neg`) or a generator's knob. Delegates to the core so the canvas walks
/// the same slots the engine addresses.
pub(crate) fn child_ref(expr: &Expr, slot: usize) -> Option<&Expr> {
    expr.child(slot)
}

pub(crate) fn lit_editor(ui: &mut egui::Ui, v: ExprValue, kind: PropKind, path: &[usize], out: &mut GraphEdits) {
    let set = |value| Some(GraphOp::SetLit { kind, path: path.to_vec(), value });
    match v {
        ExprValue::Num(n) => {
            let mut n = n;
            if ui.add(egui::DragValue::new(&mut n).speed(0.1)).changed() {
                out.op = set(ExprValue::Num(n));
            }
        }
        ExprValue::Vec2(vec) => {
            let (mut x, mut y) = (vec.x, vec.y);
            let cx = ui.add(egui::DragValue::new(&mut x).speed(0.5)).changed();
            let cy = ui.add(egui::DragValue::new(&mut y).speed(0.5)).changed();
            if cx || cy {
                out.op = set(ExprValue::Vec2(Vec2::new(x, y)));
            }
        }
        ExprValue::Color(c) => {
            let mut rgb = [c.r as f32, c.g as f32, c.b as f32];
            if ui.color_edit_button_rgb(&mut rgb).changed() {
                out.op = set(ExprValue::Color(rgb_color(rgb)));
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn ref_editor(
    ui: &mut egui::Ui,
    node: NodeId,
    prop: PropPath,
    offset: f64,
    kind: PropKind,
    path: &[usize],
    nodes: &[(u64, String)],
    out: &mut GraphEdits,
) {
    let mut chosen_node = node;
    let cur_name = nodes
        .iter()
        .find(|(id, _)| *id == node.0)
        .map(|(_, n)| n.clone())
        .unwrap_or_else(|| format!("#{}", node.0));
    egui::ComboBox::from_id_salt(("rn", kind.label(), path))
        .selected_text(cur_name)
        .show_ui(ui, |ui| {
            for (id, name) in nodes {
                if ui.selectable_label(*id == node.0, format!("{name} (#{id})")).clicked() {
                    chosen_node = NodeId(*id);
                }
            }
        });
    let mut chosen_prop = prop;
    egui::ComboBox::from_id_salt(("rp", kind.label(), path))
        .width(80.0)
        .selected_text(prop_path_label(prop))
        .show_ui(ui, |ui| {
            for p in PROP_PATHS {
                if ui.selectable_label(p == prop, prop_path_label(p)).clicked() {
                    chosen_prop = p;
                }
            }
        });
    let mut off = offset;
    let off_changed = ui
        .add(egui::DragValue::new(&mut off).speed(0.5).prefix("t"))
        .on_hover_text("Frame offset")
        .changed();
    if chosen_node != node || chosen_prop != prop || off_changed {
        out.op = Some(GraphOp::SetRef { kind, path: path.to_vec(), node: chosen_node, prop: chosen_prop, offset: off });
    }
}

/// Apply a deferred graph-panel edit to `selected`'s property in `doc`. A free
/// function (not an `App` method) so the whole promote/edit/bake flow is unit-
/// testable against a plain `Document`, no window required.
pub(crate) fn apply_graph_op(doc: &mut Document, selected: NodeId, op: GraphOp, frame: i64) {
    let t = frame as f64;
    match op {
        GraphOp::Promote(kind) => {
            // Promoting a constant/keyframed value only reads its own current
            // value, so a document-less context is enough.
            let mut ctx = EvalCtx::at(t);
            if let Some(node) = doc.root.find_mut(selected) {
                if let Some(mut p) = prop_of_mut(node, kind) {
                    p.promote_to_expr(&mut ctx);
                }
            }
        }
        GraphOp::Bake(kind) => {
            // Baking resolves the *expression*, which may reference other nodes —
            // so it needs the document. Resolve against a clone so the read
            // context doesn't alias the node being mutated.
            let snapshot = doc.clone();
            let mut ctx = EvalCtx::new(&snapshot, t);
            if let Some(node) = doc.root.find_mut(selected) {
                if let Some(mut p) = prop_of_mut(node, kind) {
                    p.bake_to_const(&mut ctx);
                }
            }
        }
        GraphOp::SetKind { kind, path, new } => {
            edit_expr(doc, selected, kind, &path, |e| *e = Expr::seed(new));
        }
        GraphOp::SetLit { kind, path, value } => {
            edit_expr(doc, selected, kind, &path, |e| *e = Expr::Lit(value));
        }
        GraphOp::SetRef { kind, path, node, prop, offset } => {
            edit_expr(doc, selected, kind, &path, |e| {
                *e = Expr::Ref { node, prop, time_offset: offset }
            });
        }
        GraphOp::AddParam { name, kind } => {
            if let Some(node) = doc.root.find_mut(selected) {
                node.set_param(name, kind.seed());
            }
        }
        GraphOp::RemoveParam { name } => {
            if let Some(node) = doc.root.find_mut(selected) {
                node.remove_param(&name);
            }
        }
        GraphOp::SetParam { kind, path, name } => {
            edit_expr(doc, selected, kind, &path, |e| *e = Expr::Param { node: None, name });
        }
        GraphOp::SetScript { kind, path, src } => {
            edit_expr(doc, selected, kind, &path, |e| *e = Expr::Script(src));
        }
        GraphOp::SetWaveform { kind, path, wave } => {
            // Only touches the waveform; the knobs are left as they are.
            edit_expr(doc, selected, kind, &path, |e| {
                if let Expr::Gen(Generator::Oscillator { wave: w, .. }) = e {
                    *w = wave;
                }
            });
        }
    }
}

/// Mutate the expression subtree at `path` on `selected`'s `kind` property.
/// No-op if the property isn't an expression or the path is stale.
pub(crate) fn edit_expr(
    doc: &mut Document,
    selected: NodeId,
    kind: PropKind,
    path: &[usize],
    f: impl FnOnce(&mut Expr),
) {
    if let Some(node) = doc.root.find_mut(selected) {
        if let Some(mut p) = prop_of_mut(node, kind) {
            if let Some(target) = p.expr_mut().and_then(|e| e.at_mut(path)) {
                f(target);
            }
        }
    }
}
