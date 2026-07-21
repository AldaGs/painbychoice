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

/// Which expression tree an edit addresses. The graph canvas, its editors, and
/// the `SetKind`/`SetLit`/… ops are all identical whether you're driving a
/// node's property or a **module's body** — the target just says which root the
/// `(path)` walks from. This is what gives module bodies a real editing surface
/// rather than only the value they were extracted with.
///
/// Derives `Hash` so it can salt egui ids directly (combobox ids, the canvas'
/// per-target box-position memory), the way `PropKind::label()` used to.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) enum GraphTarget {
    /// The selected node's property — today's behaviour.
    Prop(PropKind),
    /// A project-wide module's body.
    Module(ModuleId),
}

impl GraphTarget {
    /// The `PropKind` this target's script-result address is keyed by — the
    /// property itself, or the module-body placeholder ([`MODULE_BODY_KIND`]).
    pub(crate) fn result_kind(self) -> PropKind {
        match self {
            GraphTarget::Prop(k) => k,
            GraphTarget::Module(_) => MODULE_BODY_KIND,
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

/// What the graph panel renders. The panel has two independent halves: the
/// **selected node's** properties (`node`) and a **module's body** opened for
/// editing (`editing`). Either can be absent; the modules list and the node
/// list are always present. Cloned before the UI pass so the panel never
/// borrows `App`.
pub(crate) struct GraphInfo {
    /// (id, name) of every node in the document — the reference-target picker.
    pub(crate) nodes: Vec<(u64, String)>,
    /// Every module in the project: id, name, and its knob names. Drives the
    /// modules list, the link picker, and the override rows.
    pub(crate) modules: Vec<ModuleInfo>,
    /// The selected node's editable properties, if a node is selected.
    pub(crate) node: Option<NodeView>,
    /// The module whose body is open on the canvas, if one is being edited.
    pub(crate) editing: Option<ModuleEdit>,
}

/// The selected node as the graph panel needs it.
pub(crate) struct NodeView {
    pub(crate) node_id: NodeId,
    pub(crate) node_name: String,
    pub(crate) props: Vec<GraphProp>,
    /// The node's parameter names and kinds, in display order.
    pub(crate) params: Vec<(String, &'static str)>,
    /// Each script node's result at the current frame, addressed the same way
    /// an edit is: `(property, tree-path)`. Evaluated **here**, against the
    /// document, because `value()`/`wiggle()` need a real `EvalCtx` — a
    /// doc-less preview would report "no node named …" for a script that in
    /// fact resolves fine at render time.
    pub(crate) script_results: ScriptResults,
}

/// A module's body, opened on the canvas. Its knobs (`params`) are editable the
/// same way a node's are, since the body reads them with `param("…")`.
pub(crate) struct ModuleEdit {
    pub(crate) id: ModuleId,
    pub(crate) name: String,
    pub(crate) params: Vec<(String, &'static str)>,
    pub(crate) body: Expr,
    /// Script previews for the body, keyed `(kind, path)` with a placeholder
    /// kind (`ShapeSize`) so the address shape matches a property's. A body
    /// script's `param("x")` previews as a fallback — the module's own scope
    /// isn't pushed here — but resolves correctly at render time through the
    /// link.
    pub(crate) script_results: ScriptResults,
}

/// One module as the panel needs it.
pub(crate) struct ModuleInfo {
    pub(crate) id: ModuleId,
    pub(crate) name: String,
    /// Knob names, in the module's own display order.
    pub(crate) params: Vec<String>,
}

/// The stand-in `PropKind` a module body's `(kind, path)` addresses use. A
/// module body isn't a property, but the script-result map and the canvas were
/// built around that key; a fixed placeholder keeps the address shape without
/// pretending the body is `ShapeSize`.
pub(crate) const MODULE_BODY_KIND: PropKind = PropKind::ShapeSize;

impl GraphInfo {
    pub(crate) fn gather(
        doc: &Document,
        modules: &std::collections::BTreeMap<ModuleId, MModule>,
        selected: Option<NodeId>,
        editing: Option<ModuleId>,
        frame: f64,
    ) -> GraphInfo {
        let mut nodes = Vec::new();
        collect_nodes(&doc.root, &mut nodes);
        let module_infos = modules
            .iter()
            .map(|(id, m)| ModuleInfo {
                id: *id,
                name: m.name.clone(),
                params: m.params.iter().map(|p| p.name.clone()).collect(),
            })
            .collect();

        let node = selected.and_then(|id| doc.root.find(id)).map(|node| {
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
            let mut script_results = ScriptResults::new();
            for prop in &props {
                if let Some(expr) = &prop.expr {
                    let mut ctx = EvalCtx::new(doc, frame);
                    // In this node's context, so a script's `param("x")` previews
                    // the same value it resolves to at render time.
                    ctx.in_node(node.id, |ctx| {
                        collect_scripts(expr, prop.kind, &mut Vec::new(), ctx, &mut script_results)
                    });
                }
            }
            NodeView {
                node_id: node.id,
                node_name: node.name.clone(),
                props,
                params: node.params.iter().map(|p| (p.name.clone(), p.value.kind_name())).collect(),
                script_results,
            }
        });

        let editing = editing.and_then(|mid| modules.get(&mid).map(|m| (mid, m))).map(|(mid, m)| {
            let mut script_results = ScriptResults::new();
            let mut ctx = EvalCtx::new(doc, frame);
            collect_scripts(&m.body, MODULE_BODY_KIND, &mut Vec::new(), &mut ctx, &mut script_results);
            ModuleEdit {
                id: mid,
                name: m.name.clone(),
                params: m.params.iter().map(|p| (p.name.clone(), p.value.kind_name())).collect(),
                body: m.body.clone(),
                script_results,
            }
        });

        GraphInfo { nodes, modules: module_infos, node, editing }
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
    SetKind { target: GraphTarget, path: Vec<usize>, new: ExprKind },
    /// Set the literal at `path`.
    SetLit { target: GraphTarget, path: Vec<usize>, value: ExprValue },
    /// Set the reference at `path`.
    SetRef { target: GraphTarget, path: Vec<usize>, node: NodeId, prop: PropPath, offset: f64 },
    /// Set the script source at `path`.
    SetScript { target: GraphTarget, path: Vec<usize>, src: String },
    /// Set an oscillator's waveform at `path`.
    SetWaveform { target: GraphTarget, path: Vec<usize>, wave: Waveform },
    /// Point a `param` node at a knob of the target's owner (the node, or the
    /// module whose body is being edited).
    SetParam { target: GraphTarget, path: Vec<usize>, name: String },
    /// Add a knob to a node or module, seeded with a neutral value.
    AddParam { owner: ParamOwner, name: String, kind: ParamKind },
    /// Remove a knob. Expressions reading it aren't rewritten — they warn.
    RemoveParam { owner: ParamOwner, name: String },
    /// Lift this property's whole expression into a new project-wide module and
    /// leave a link in its place. How a module is *made*: you build the recipe
    /// once on a real property, then promote it.
    ExtractModule { kind: PropKind },
    /// Point this property at an existing module (promoting it if needed).
    LinkModule { kind: PropKind, module: ModuleId },
    /// Point the node at `path` at `module` as a link. Repoints an existing
    /// `Use` (keeping its overrides) *or* seeds a fresh one over any other kind
    /// (starting with no overrides) — which is how the kind picker creates a
    /// `Use`, since a bare `ExprKind` can't name the module.
    SetModule { target: GraphTarget, path: Vec<usize>, module: ModuleId },
    /// Override one knob at a link site, or clear it back to inheriting.
    SetOverride { target: GraphTarget, path: Vec<usize>, name: String, value: Option<ExprValue> },
    RenameModule { module: ModuleId, name: String },
    /// Delete a module. Links to it aren't rewritten — they warn and fall back,
    /// exactly like any other dangling reference.
    DeleteModule { module: ModuleId },
}

#[derive(Default)]
pub(crate) struct GraphEdits {
    pub(crate) op: Option<GraphOp>,
    /// Change which module's body the panel edits: `Some(Some(id))` opens one,
    /// `Some(None)` closes the current one, `None` leaves it unchanged. View
    /// state, so it rides beside the document ops rather than in one.
    pub(crate) edit_module: Option<Option<ModuleId>>,
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
        // A layer-clock leaf has no controls at all — it *is* its kind picker.
        Expr::Time(_) => 30.0,
        // A module link shows the module picker plus one inherit/override toggle
        // row per knob; each *overridden* knob's value is a wired-in child box
        // (its overrides are the node's children now), not edited in here.
        Expr::Use { .. } => 50.0,
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
                // A link's override children are labelled with the knob name
                // (derived in the canvas, not core), so reserve the label line
                // for them the same way a generator knob's does.
                let child_labeled =
                    matches!(expr, Expr::Use { .. }) || expr.slot_label(slot).is_some();
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

/// The expression/node-graph panel. Two independent halves under a shared
/// modules list: the selected node's properties, and — new in the graph-UI
/// step — a module's *body* opened on the same canvas, so a module is edited in
/// one place rather than only through the property it was extracted from.
pub(crate) fn graph_ui(ui: &mut egui::Ui, info: &GraphInfo, frame: f64, out: &mut GraphEdits) {
    ui.add_space(8.0);
    ui.heading("Graph");
    modules_ui(ui, info, out);
    ui.separator();

    // The module being edited sits above the node view: it's what you just
    // opened, and it drives every link, so it reads as the more global thing.
    if let Some(edit) = &info.editing {
        module_edit_ui(ui, info, edit, frame, out);
        ui.separator();
    }

    match &info.node {
        Some(node) => node_view_ui(ui, info, node, frame, out),
        // The placeholder only makes sense when nothing at all is open.
        None if info.editing.is_none() => {
            ui.weak("Select a node to drive its properties with expressions.");
        }
        None => {}
    }
}

/// The selected node's half: its knobs, then each property's promote/link/bake
/// controls and expression canvas.
pub(crate) fn node_view_ui(ui: &mut egui::Ui, info: &GraphInfo, node: &NodeView, frame: f64, out: &mut GraphEdits) {
    ui.weak(format!("Node: {}  ·  drag a node to arrange", node.node_name));
    ui.separator();
    let param_names = params_ui(ui, ParamOwner::Node(node.node_id), &node.params, out);
    ui.separator();
    egui::ScrollArea::both()
        .id_salt(("nodescroll", node.node_id.0))
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for prop in &node.props {
                ui.horizontal(|ui| {
                    ui.strong(prop.label);
                    if prop.is_expr {
                        if icon::button(ui, icon::BAKE, "Freeze to a constant").clicked() {
                            out.op = Some(GraphOp::Bake(prop.kind));
                        }
                        if icon::button(
                            ui,
                            icon::MODULE,
                            "Lift this recipe into a shared module and link it here",
                        )
                        .clicked()
                        {
                            out.op = Some(GraphOp::ExtractModule { kind: prop.kind });
                        }
                        if let Some(printed) = &prop.printed {
                            ui.weak(format!("= {printed}"));
                        }
                    } else {
                        if icon::button(ui, icon::EXPR, "Drive with an expression").clicked() {
                            out.op = Some(GraphOp::Promote(prop.kind));
                        }
                        // Linking an existing module is the other way in, and
                        // the one that makes a module reusable at all.
                        if !info.modules.is_empty() {
                            egui::ComboBox::from_id_salt(("link", prop.kind.label()))
                                .width(70.0)
                                .selected_text(icon::text(icon::LINK))
                                .show_ui(ui, |ui| {
                                    for m in &info.modules {
                                        if ui.selectable_label(false, &m.name).clicked() {
                                            out.op = Some(GraphOp::LinkModule {
                                                kind: prop.kind,
                                                module: m.id,
                                            });
                                        }
                                    }
                                });
                        }
                    }
                });
                if let Some(expr) = &prop.expr {
                    expr_canvas(
                        ui,
                        expr,
                        GraphTarget::Prop(prop.kind),
                        frame,
                        &info.nodes,
                        &param_names,
                        &node.script_results,
                        &info.modules,
                        out,
                    );
                    ui.separator();
                }
            }
        });
}

/// A module body opened for editing: a header (name + close), the module's own
/// knobs, and the body on the same node canvas every property uses. This is the
/// editing surface a module lacked — before it, a module could only be *made*
/// (by extracting a property) and its body edited nowhere.
pub(crate) fn module_edit_ui(
    ui: &mut egui::Ui,
    info: &GraphInfo,
    edit: &ModuleEdit,
    frame: f64,
    out: &mut GraphEdits,
) {
    ui.horizontal(|ui| {
        ui.label(icon::text(icon::MODULE));
        ui.strong(format!("Editing: {}", edit.name));
        if icon::button(ui, icon::CLOSE, "Stop editing this module").clicked() {
            out.edit_module = Some(None);
        }
    });
    ui.weak("Its knobs, and the body every link drives.");
    ui.separator();
    // The module's own knobs — the tunables a link overrides, and what the
    // body's `param("…")` nodes read.
    let param_names = params_ui(ui, ParamOwner::Module(edit.id), &edit.params, out);
    ui.separator();
    egui::ScrollArea::both()
        .id_salt(("modscroll", edit.id.0))
        .auto_shrink([false, false])
        .show(ui, |ui| {
            expr_canvas(
                ui,
                &edit.body,
                GraphTarget::Module(edit.id),
                frame,
                &info.nodes,
                &param_names,
                &edit.script_results,
                &info.modules,
                out,
            );
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
    target: GraphTarget,
    frame: f64,
    nodes: &[(u64, String)],
    params: &[String],
    results: &ScriptResults,
    modules: &[ModuleInfo],
    out: &mut GraphEdits,
) {
    let boxes = layout_expr(expr);

    // Positions are remembered per target (node+property, or module body) in
    // egui memory; a box with no stored position falls back to its auto-layout
    // slot (column × its y).
    let mem_id = ui.id().with(("graphpos", target));
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
        let drag_id = ui.id().with(("graphbox", target, b.path.as_slice()));
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
        // A generator knob shows its name (`freq`/`amp`/…) and a link override
        // shows the knob it overrides, so a wired box is read the same as the
        // labelled slot it fills; operator operands are positional and
        // unlabelled. A link's override name is dynamic (owned), so it's derived
        // here rather than through core's `&'static` slot labels.
        let slot_label = b.path.split_last().and_then(|(&slot, parent)| {
            let p = expr.at(parent)?;
            match p {
                Expr::Use { overrides, .. } => overrides.get(slot).map(|(n, _)| n.clone()),
                _ => p.slot_label(slot).map(str::to_string),
            }
        });
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink(5.0)));
        expr_box(
            &mut child, node, target, frame, &b.path, slot_label.as_deref(), nodes, params,
            results, modules, out,
        );
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
    target: GraphTarget,
    frame: f64,
    path: &[usize],
    slot_label: Option<&str>,
    nodes: &[(u64, String)],
    params: &[String],
    results: &ScriptResults,
    modules: &[ModuleInfo],
    out: &mut GraphEdits,
) {
    if let Some(label) = slot_label {
        ui.small(label);
    }
    let cur = expr.kind();
    egui::ComboBox::from_id_salt(("ek", target, path))
        .width(60.0)
        .selected_text(cur.label())
        .show_ui(ui, |ui| {
            for k in ExprKind::ALL {
                if ui.selectable_label(k == cur, k.label()).clicked() && k != cur {
                    out.op = Some(GraphOp::SetKind { target, path: path.to_vec(), new: k });
                }
            }
            // `Use` is deliberately absent from `ExprKind::ALL`: seeding a link
            // needs a module to point at, which a bare kind can't carry. So the
            // picker lists the modules themselves — each seeds a fresh link via
            // `SetModule`, which replaces whatever's here with `use <module>`
            // (no overrides, since the node wasn't a link). This is the kind
            // picker's half of the graph-UI "seed a `Use`" step.
            if !modules.is_empty() {
                ui.separator();
                ui.weak("use module");
                let cur_module = match expr {
                    Expr::Use { module, .. } => Some(*module),
                    _ => None,
                };
                for m in modules {
                    if ui.selectable_label(cur_module == Some(m.id), &m.name).clicked()
                        && cur_module != Some(m.id)
                    {
                        out.op =
                            Some(GraphOp::SetModule { target, path: path.to_vec(), module: m.id });
                    }
                }
            }
        });
    match expr {
        Expr::Lit(v) => lit_editor(ui, *v, target, path, out),
        Expr::Ref { node, prop, time_offset } => {
            ref_editor(ui, *node, *prop, *time_offset, target, path, nodes, out)
        }
        Expr::Param { name, .. } => param_editor(ui, name, params, target, path, out),
        Expr::Script(src) => {
            let result = results.get(&(target.result_kind(), path.to_vec()));
            script_editor(ui, src, frame, result, target, path, out)
        }
        Expr::Use { module, overrides } => {
            use_editor(ui, *module, overrides, target, path, modules, out)
        }
        // A generator's knobs are wired-in child boxes; the only in-box control
        // is the oscillator's waveform picker.
        Expr::Gen(Generator::Oscillator { wave, .. }) => {
            wave_editor(ui, *wave, target, path, out)
        }
        _ => {}
    }
}


/// A module link: which module, and one row per knob showing whether this link
/// **inherits** the module's default or **overrides** it.
///
/// Inherit is the resting state and is spelled out rather than implied by a
/// blank field — the whole point of a module is that unset knobs keep following
/// the definition, and a UI that can't show the difference hides it.
///
/// Each overridden knob's *value* is a wired-in child box on the canvas (a
/// link's overrides are its tree children now), edited like any other
/// sub-expression — so an override can be a literal, a `ref`, a `param`, a
/// script, anything. This row only toggles the two states: pressing **override**
/// seeds a literal `0` child to start from, **inherit** (the `x`) drops the
/// override so the knob follows the module again.
pub(crate) fn use_editor(
    ui: &mut egui::Ui,
    module: ModuleId,
    overrides: &[(String, Expr)],
    target: GraphTarget,
    path: &[usize],
    modules: &[ModuleInfo],
    out: &mut GraphEdits,
) {
    let current = modules.iter().find(|m| m.id == module);
    let label = current.map(|m| m.name.clone()).unwrap_or_else(|| format!("<missing {}>", module.0));
    egui::ComboBox::from_id_salt(("usemod", target, path))
        .width(110.0)
        .selected_text(label)
        .show_ui(ui, |ui| {
            for m in modules {
                if ui.selectable_label(m.id == module, &m.name).clicked() && m.id != module {
                    out.op =
                        Some(GraphOp::SetModule { target, path: path.to_vec(), module: m.id });
                }
            }
        });
    let Some(current) = current else { return };
    for name in &current.params {
        let overridden = overrides.iter().any(|(n, _)| n == name);
        ui.horizontal(|ui| {
            ui.small(name);
            if overridden {
                // The value lives in the wired child box labelled `name`; here
                // we only offer to stop overriding.
                ui.weak("override →");
                if ui.small_button("x").on_hover_text("Inherit from the module").clicked() {
                    out.op = Some(GraphOp::SetOverride {
                        target,
                        path: path.to_vec(),
                        name: name.clone(),
                        value: None,
                    });
                }
            } else if ui
                .small_button("inherit")
                .on_hover_text("Following the module — click to override here")
                .clicked()
            {
                // Seed a literal `0`; it appears as a child box to edit into
                // anything (a ref, a param, a script) from its kind picker.
                out.op = Some(GraphOp::SetOverride {
                    target,
                    path: path.to_vec(),
                    name: name.clone(),
                    value: Some(ExprValue::Num(0.0)),
                });
            }
        });
    }
}

/// The project's modules: rename and delete, plus the reminder that a module is
/// made by extracting one from a property.
pub(crate) fn modules_ui(ui: &mut egui::Ui, info: &GraphInfo, out: &mut GraphEdits) {
    ui.horizontal(|ui| {
        ui.label(icon::text(icon::MODULE));
        ui.strong("Modules");
        ui.weak(format!("{}", info.modules.len()));
    });
    if info.modules.is_empty() {
        ui.weak("None yet — build a recipe on a property, then press -> module.");
        return;
    }
    let open = info.editing.as_ref().map(|e| e.id);
    for m in &info.modules {
        ui.horizontal(|ui| {
            let mut name = m.name.clone();
            if ui
                .add(egui::TextEdit::singleline(&mut name).desired_width(120.0))
                .changed()
            {
                out.op = Some(GraphOp::RenameModule { module: m.id, name });
            }
            // Open (or, if it's already open, close) this module's body on the
            // canvas — the editing surface a module now has.
            let editing = open == Some(m.id);
            let tip = if editing { "Stop editing this module" } else { "Edit this module's body" };
            if icon::button(ui, icon::EXPR, tip).clicked() {
                out.edit_module = Some((!editing).then_some(m.id));
            }
            if editing {
                ui.weak("editing");
            }
            if icon::button(
                ui,
                icon::DELETE,
                "Delete. Links to it warn and fall back, like any dangling reference.",
            )
            .clicked()
            {
                out.op = Some(GraphOp::DeleteModule { module: m.id });
            }
        });
    }
}

/// The oscillator's waveform picker (`sine`/`triangle`/`square`/`saw`).
pub(crate) fn wave_editor(
    ui: &mut egui::Ui,
    wave: Waveform,
    target: GraphTarget,
    path: &[usize],
    out: &mut GraphEdits,
) {
    egui::ComboBox::from_id_salt(("wave", target, path))
        .width(90.0)
        .selected_text(wave.label())
        .show_ui(ui, |ui| {
            for w in Waveform::ALL {
                if ui.selectable_label(w == wave, w.label()).clicked() && w != wave {
                    out.op = Some(GraphOp::SetWaveform { target, path: path.to_vec(), wave: w });
                }
            }
        });
}

/// The knobs of a node or a module: what exists, plus add/remove. Returns the
/// names, which the `param` nodes below use to populate their picker.
///
/// The same surface serves both owners — a node's `param("x")` and a module
/// body's `param("x")` are read the same way — with `owner` deciding whose
/// knobs are touched and salting the egui state so two owners' add-fields don't
/// collide. Parameters live here rather than in the properties panel because
/// they only mean anything to expressions — a knob nothing reads is noise in a
/// list of real properties.
pub(crate) fn params_ui(
    ui: &mut egui::Ui,
    owner: ParamOwner,
    params: &[(String, &'static str)],
    out: &mut GraphEdits,
) -> Vec<String> {
    let names: Vec<String> = params.iter().map(|(n, _)| n.clone()).collect();
    ui.horizontal_wrapped(|ui| {
        ui.strong("Parameters");
        // The new parameter's name is typed into egui memory, so the panel
        // stays a pure function of the document (the same rule as the canvas'
        // box positions).
        let buf_id = egui::Id::new(("param_new", owner));
        let mut buf: String = ui.data_mut(|d| d.get_temp(buf_id).unwrap_or_default());
        ui.add(
            egui::TextEdit::singleline(&mut buf).hint_text("new name").desired_width(90.0),
        );
        for (label, kind) in
            [("num", ParamKind::Num), ("vec", ParamKind::Vec), ("col", ParamKind::Color)]
        {
            let taken = names.contains(&buf);
            let ok = !buf.trim().is_empty() && !taken;
            // The plus lives on the glyph, so the label is just the type.
            if ui
                .add_enabled(
                    ok,
                    egui::Button::new(format!("{} {label}", icon::ADD)).small(),
                )
                .on_disabled_hover_text(if taken {
                    "that name is taken"
                } else {
                    "type a name first"
                })
                .clicked()
            {
                out.op = Some(GraphOp::AddParam { owner, name: buf.trim().to_string(), kind });
                buf.clear();
            }
        }
        ui.data_mut(|d| d.insert_temp(buf_id, buf));
    });
    if params.is_empty() {
        ui.weak("None. A parameter is a named knob expressions can read.");
    }
    for (name, kind) in params {
        ui.horizontal(|ui| {
            if ui.small_button("x").on_hover_text("Remove").clicked() {
                out.op = Some(GraphOp::RemoveParam { owner, name: name.clone() });
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
    target: GraphTarget,
    path: &[usize],
    out: &mut GraphEdits,
) {
    if params.is_empty() {
        ui.weak("no parameters");
        ui.small("add one above");
        return;
    }
    egui::ComboBox::from_id_salt(("pn", target, path))
        .width(120.0)
        .selected_text(if name.is_empty() { "(pick)" } else { name })
        .show_ui(ui, |ui| {
            for p in params {
                if ui.selectable_label(p == name, p).clicked() && p != name {
                    out.op = Some(GraphOp::SetParam {
                        target,
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
    target: GraphTarget,
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
        out.op = Some(GraphOp::SetScript { target, path: path.to_vec(), src: text.clone() });
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

/// The child expression at `slot` — an operator operand (0/1 for `Add`/`Mul`,
/// 0 for `Neg`) or a generator's knob. Delegates to the core so the canvas walks
/// the same slots the engine addresses.
pub(crate) fn child_ref(expr: &Expr, slot: usize) -> Option<&Expr> {
    expr.child(slot)
}

pub(crate) fn lit_editor(ui: &mut egui::Ui, v: ExprValue, target: GraphTarget, path: &[usize], out: &mut GraphEdits) {
    let set = |value| Some(GraphOp::SetLit { target, path: path.to_vec(), value });
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
    target: GraphTarget,
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
    egui::ComboBox::from_id_salt(("rn", target, path))
        .selected_text(cur_name)
        .show_ui(ui, |ui| {
            for (id, name) in nodes {
                if ui.selectable_label(*id == node.0, format!("{name} (#{id})")).clicked() {
                    chosen_node = NodeId(*id);
                }
            }
        });
    let mut chosen_prop = prop;
    egui::ComboBox::from_id_salt(("rp", target, path))
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
        out.op = Some(GraphOp::SetRef { target, path: path.to_vec(), node: chosen_node, prop: chosen_prop, offset: off });
    }
}

/// Apply one deferred graph-panel edit. A free function (not an `App` method) so
/// the whole promote/edit/bake/module flow is unit-testable against a plain
/// project, no window required.
///
/// Takes the whole project, not just the open comp: modules are project-wide,
/// so extracting, relinking, or **editing a module body** reaches past the comp
/// being edited. `selected` is optional because a module-body edit needs no
/// node selection — the node-scoped ops (promote/bake/extract/link and any
/// property-targeted tree edit) simply no-op without one.
pub(crate) fn apply_graph_op(
    project: &mut MProject,
    comp: CompId,
    selected: Option<NodeId>,
    op: GraphOp,
    frame: i64,
) {
    let t = frame as f64;
    // The node-scoped arms edit the open comp; the module arms touch the
    // registry, which is why this can't take a bare `&mut Comp`.
    macro_rules! doc {
        () => {
            match project.comp_mut(comp) {
                Some(c) => c,
                None => return,
            }
        };
    }
    match op {
        GraphOp::Promote(kind) => {
            let Some(selected) = selected else { return };
            // Promoting a constant/keyframed value only reads its own current
            // value, so a document-less context is enough.
            let mut ctx = EvalCtx::at(t);
            if let Some(node) = doc!().root.find_mut(selected) {
                if let Some(mut p) = prop_of_mut(node, kind) {
                    p.promote_to_expr(&mut ctx);
                }
            }
        }
        GraphOp::Bake(kind) => {
            let Some(selected) = selected else { return };
            // Baking resolves the *expression*, which may reference other nodes —
            // so it needs the document. Resolve against a clone so the read
            // context doesn't alias the node being mutated.
            let snapshot = doc!().clone();
            let mut ctx = EvalCtx::new(&snapshot, t);
            if let Some(node) = doc!().root.find_mut(selected) {
                if let Some(mut p) = prop_of_mut(node, kind) {
                    p.bake_to_const(&mut ctx);
                }
            }
        }
        GraphOp::SetKind { target, path, new } => {
            edit_expr(project, comp, selected, target, &path, |e| *e = Expr::seed(new));
        }
        GraphOp::SetLit { target, path, value } => {
            edit_expr(project, comp, selected, target, &path, |e| *e = Expr::Lit(value));
        }
        GraphOp::SetRef { target, path, node, prop, offset } => {
            edit_expr(project, comp, selected, target, &path, |e| {
                *e = Expr::Ref { node, prop, time_offset: offset }
            });
        }
        GraphOp::AddParam { owner, name, kind } => match owner {
            ParamOwner::Node(id) => {
                if let Some(node) = doc!().root.find_mut(id) {
                    node.set_param(name, kind.seed());
                }
            }
            ParamOwner::Module(m) => {
                if let Some(m) = project.module_mut(m) {
                    m.set_param(name, kind.seed());
                }
            }
        },
        GraphOp::RemoveParam { owner, name } => match owner {
            ParamOwner::Node(id) => {
                if let Some(node) = doc!().root.find_mut(id) {
                    node.remove_param(&name);
                }
            }
            ParamOwner::Module(m) => {
                if let Some(m) = project.module_mut(m) {
                    m.remove_param(&name);
                }
            }
        },

        // Lift a property's whole recipe into a shared module, then link it back
        // in its place. The link starts with no overrides: it *is* the module,
        // so the frame is unchanged the instant you press the button.
        GraphOp::ExtractModule { kind } => {
            let Some(selected) = selected else { return };
            let name = {
                let doc = doc!();
                let Some(node) = doc.root.find(selected) else { return };
                format!("{} {}", node.name, kind.label())
            };
            let body = {
                let doc = doc!();
                let Some(node) = doc.root.find(selected) else { return };
                match prop_of(node, kind).and_then(|p| p.expr().cloned()) {
                    Some(e) => e,
                    None => return,
                }
            };
            let module = project.add_module(MModule::new(name, body));
            if let Some(node) = doc!().root.find_mut(selected) {
                if let Some(mut p) = prop_of_mut(node, kind) {
                    p.set_expr(Expr::Use { module, overrides: Vec::new() });
                }
            }
        }
        // Point a property at an existing module, promoting it if it was still
        // a constant — otherwise linking would silently do nothing.
        GraphOp::LinkModule { kind, module } => {
            let Some(selected) = selected else { return };
            if let Some(node) = doc!().root.find_mut(selected) {
                if let Some(mut p) = prop_of_mut(node, kind) {
                    p.set_expr(Expr::Use { module, overrides: Vec::new() });
                }
            }
        }
        GraphOp::SetModule { target, path, module } => {
            edit_expr(project, comp, selected, target, &path, |e| {
                // Repointing keeps the overrides: knobs are matched by name, and
                // any that the new module lacks warn rather than vanishing.
                let overrides = match e {
                    Expr::Use { overrides, .. } => std::mem::take(overrides),
                    _ => Vec::new(),
                };
                *e = Expr::Use { module, overrides };
            });
        }
        GraphOp::SetOverride { target, path, name, value } => {
            edit_expr(project, comp, selected, target, &path, |e| {
                if let Expr::Use { overrides, .. } = e {
                    overrides.retain(|(n, _)| n != &name);
                    // `None` means "inherit" — removing the entry *is* the
                    // inherit, since a knob with no override follows the module.
                    if let Some(v) = value {
                        overrides.push((name.clone(), Expr::Lit(v)));
                    }
                }
            });
        }
        GraphOp::RenameModule { module, name } => {
            if let Some(m) = project.module_mut(module) {
                m.name = name;
            }
        }
        GraphOp::DeleteModule { module } => {
            project.modules.remove(&module);
        }
        GraphOp::SetParam { target, path, name } => {
            edit_expr(project, comp, selected, target, &path, |e| {
                *e = Expr::Param { node: None, name }
            });
        }
        GraphOp::SetScript { target, path, src } => {
            edit_expr(project, comp, selected, target, &path, |e| *e = Expr::Script(src));
        }
        GraphOp::SetWaveform { target, path, wave } => {
            // Only touches the waveform; the knobs are left as they are.
            edit_expr(project, comp, selected, target, &path, |e| {
                if let Expr::Gen(Generator::Oscillator { wave: w, .. }) = e {
                    *w = wave;
                }
            });
        }
    }
}

/// Mutate the expression subtree at `path` on the [`GraphTarget`] — the selected
/// node's `kind` property (in `comp`), or a project-wide module's body. No-op if
/// the target isn't an expression, the selection is missing, or the path is
/// stale. This is the one seam that makes the canvas target both.
pub(crate) fn edit_expr(
    project: &mut MProject,
    comp: CompId,
    selected: Option<NodeId>,
    target: GraphTarget,
    path: &[usize],
    f: impl FnOnce(&mut Expr),
) {
    match target {
        GraphTarget::Prop(kind) => {
            let Some(selected) = selected else { return };
            let Some(c) = project.comp_mut(comp) else { return };
            let Some(node) = c.root.find_mut(selected) else { return };
            let Some(mut p) = prop_of_mut(node, kind) else { return };
            if let Some(e) = p.expr_mut().and_then(|e| e.at_mut(path)) {
                f(e);
            }
        }
        GraphTarget::Module(m) => {
            let Some(m) = project.module_mut(m) else { return };
            if let Some(e) = m.body.at_mut(path) {
                f(e);
            }
        }
    }
}
