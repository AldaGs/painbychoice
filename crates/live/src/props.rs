//! The properties panel: resolved values, keyframing, easing, and the
//! `PropKind` enumeration of every animatable property behind it.
//!
//! Moved verbatim out of `main.rs` when it was split by concern; the
//! only edit was widening visibility to `pub(crate)`.

use crate::*;

/// What the transport UI reports back after a frame's interaction.
#[derive(Default)]
pub(crate) struct Transport {
    pub(crate) toggle: bool,
    pub(crate) restart: bool,
    /// Frame to scrub to. Snapped by the slider's integer step.
    pub(crate) scrub_to: Option<i64>,
}

/// A snapshot of the selected node's resolved properties at the current time,
/// gathered before the egui closure so the UI never borrows `App`. The `*_anim`
/// flags mark properties backed by a keyframe track (edits auto-key those).
pub(crate) struct NodeInfo {
    pub(crate) name: String,
    pub(crate) id: u64,
    pub(crate) pos: (f64, f64),
    pub(crate) rot: f64,
    pub(crate) scale: (f64, f64),
    pub(crate) opacity: f64,
    pub(crate) fill: Option<[f32; 3]>,
    /// Parametric geometry, `None` for a group or a hand-drawn `Path`.
    pub(crate) size: Option<(f64, f64)>,
    /// Corner radius — `Some` only for a Rect.
    pub(crate) radius: Option<f64>,
    /// Stroke color + width, `None` when the node has no stroke.
    pub(crate) stroke: Option<([f32; 3], f64)>,
    pub(crate) pos_anim: bool,
    pub(crate) rot_anim: bool,
    pub(crate) scale_anim: bool,
    pub(crate) opacity_anim: bool,
    pub(crate) fill_anim: bool,
    pub(crate) size_anim: bool,
    pub(crate) radius_anim: bool,
    pub(crate) stroke_color_anim: bool,
    pub(crate) stroke_width_anim: bool,
}

/// egui's color buttons speak `[f32; 3]`; the document speaks `Color`.
pub(crate) fn rgb_color(rgb: [f32; 3]) -> MColor {
    MColor::rgb(rgb[0] as f64, rgb[1] as f64, rgb[2] as f64)
}

/// Whether `kind` exists on this node *and* is keyframed.
pub(crate) fn is_anim(node: &MNode, kind: PropKind) -> bool {
    prop_of(node, kind).is_some_and(|p| p.is_animated())
}

impl NodeInfo {
    pub(crate) fn resolve(node: &motion_core::Node, doc: &Document, t: f64) -> Self {
        let mut ctx = EvalCtx::new(doc, t);
        // Mark the node, as `evaluate`'s walk does: a `param("x")` with no
        // explicit owner reads this node's knobs, so the panel would otherwise
        // show a fallback where the canvas shows the real value.
        ctx.in_node(node.id, |ctx| Self::resolve_in(node, ctx))
    }

    pub(crate) fn resolve_in(node: &motion_core::Node, ctx: &mut EvalCtx) -> Self {
        let tr = &node.transform;
        let pos = tr.position.resolve(ctx);
        let scale = tr.scale.resolve(ctx);
        NodeInfo {
            name: node.name.clone(),
            id: node.id.0,
            pos: (pos.x, pos.y),
            rot: tr.rotation_deg.resolve(ctx),
            scale: (scale.x, scale.y),
            opacity: tr.opacity.resolve(ctx),
            fill: node.fill.as_ref().map(|f| {
                let c = f.resolve(ctx);
                [c.r as f32, c.g as f32, c.b as f32]
            }),
            size: match node.shape.as_ref() {
                Some(MShape::Rect { size, .. }) | Some(MShape::Ellipse { size }) => {
                    let s = size.resolve(ctx);
                    Some((s.x, s.y))
                }
                _ => None,
            },
            radius: match node.shape.as_ref() {
                Some(MShape::Rect { radius, .. }) => Some(radius.resolve(ctx)),
                _ => None,
            },
            stroke: node.stroke.as_ref().map(|s| {
                let c = s.color.resolve(ctx);
                ([c.r as f32, c.g as f32, c.b as f32], s.width.resolve(ctx))
            }),
            pos_anim: tr.position.is_animated(),
            rot_anim: tr.rotation_deg.is_animated(),
            scale_anim: tr.scale.is_animated(),
            opacity_anim: tr.opacity.is_animated(),
            // Whether each optional property is animated. `prop_of` already
            // encodes "does this node even have it", so ask it rather than
            // re-deriving the shape/stroke cases here and risking disagreement.
            fill_anim: is_anim(node, PropKind::Fill),
            size_anim: is_anim(node, PropKind::ShapeSize),
            radius_anim: is_anim(node, PropKind::ShapeRadius),
            stroke_color_anim: is_anim(node, PropKind::StrokeColor),
            stroke_width_anim: is_anim(node, PropKind::StrokeWidth),
        }
    }
}

/// Edits collected from the properties panel this frame. Any `Some` field is a
/// new value the user dialed in; `None` means untouched.
#[derive(Default)]
pub(crate) struct PropEdits {
    pub(crate) pos_x: Option<f64>,
    pub(crate) pos_y: Option<f64>,
    pub(crate) rot: Option<f64>,
    pub(crate) scale_x: Option<f64>,
    pub(crate) scale_y: Option<f64>,
    pub(crate) opacity: Option<f64>,
    pub(crate) fill: Option<[f32; 3]>,
    pub(crate) size_x: Option<f64>,
    pub(crate) size_y: Option<f64>,
    pub(crate) radius: Option<f64>,
    pub(crate) stroke_color: Option<[f32; 3]>,
    pub(crate) stroke_width: Option<f64>,
    /// Add a default stroke to a node that has none / drop the one it has.
    pub(crate) add_stroke: bool,
    pub(crate) remove_stroke: bool,
    // Insert-keyframe-at-playhead requests (the "stopwatch"). Keyed by
    // `PropKind` rather than one bool per property, so adding an animatable
    // property doesn't grow this struct.
    pub(crate) key: KeySelectionKinds,
}

/// The set of properties whose stopwatch was clicked this frame.
pub(crate) type KeySelectionKinds = std::collections::BTreeSet<PropKind>;

/// A "stopwatch" toggle: a filled dot when the property is animated, a hollow
/// ring when constant. Clicking it inserts a keyframe at the playhead
/// (promoting a constant to a track). The indicator is *painted* rather than
/// drawn from a glyph, since the circle/diamond glyphs are missing from egui's
/// default font and render as tofu boxes.
pub(crate) fn key_button(ui: &mut egui::Ui, animated: bool) -> bool {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(16.0, 16.0), egui::Sense::click());
    let c = rect.center();
    let painter = ui.painter();
    if animated {
        painter.circle_filled(c, 4.0, egui::Color32::from_rgb(255, 216, 51));
    } else {
        let col = if resp.hovered() {
            egui::Color32::from_gray(200)
        } else {
            egui::Color32::from_gray(120)
        };
        painter.circle_stroke(c, 4.0, egui::Stroke::new(1.5, col));
    }
    resp.on_hover_text("Insert a keyframe at the playhead").clicked()
}

/// The two normalized cubic-bezier control points of the selected keyframe's
/// outgoing timing segment (`cubic-bezier(p1, p2)` with endpoints 0,0 and 1,1).
pub(crate) struct EaseInfo {
    pub(crate) p1: (f32, f32),
    pub(crate) p2: (f32, f32),
}

/// A CSS-style cubic-bezier editor. Draws the timing curve in a unit square and
/// lets the two control points be dragged. New handles are reported in `out`.
pub(crate) fn ease_editor(ui: &mut egui::Ui, ease: &EaseInfo, out: &mut Option<((f32, f32), (f32, f32))>) {
    let sz = (ui.available_width() - 8.0).clamp(80.0, 180.0);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(sz, sz), egui::Sense::hover());
    let painter = ui.painter_at(rect);

    // value (x right, y up) in [0,1] → screen (y is down).
    let map = |v: (f32, f32)| {
        egui::pos2(rect.left() + v.0 * rect.width(), rect.bottom() - v.1 * rect.height())
    };
    let unmap = |p: egui::Pos2| {
        (
            ((p.x - rect.left()) / rect.width()).clamp(0.0, 1.0),
            ((rect.bottom() - p.y) / rect.height()).clamp(0.0, 1.0),
        )
    };

    painter.rect_filled(rect, 3.0, egui::Color32::from_gray(28));
    // Reference diagonal (linear).
    painter.line_segment(
        [map((0.0, 0.0)), map((1.0, 1.0))],
        egui::Stroke::new(1.0, egui::Color32::from_gray(60)),
    );

    // Drag the control points first, so the curve draws with fresh values.
    let mut p1 = ease.p1;
    let mut p2 = ease.p2;
    let mut changed = false;
    for (i, hp) in [&mut p1, &mut p2].into_iter().enumerate() {
        let sp = map(*hp);
        let hit = egui::Rect::from_center_size(sp, egui::vec2(16.0, 16.0));
        let resp = ui.interact(hit, ui.id().with(("ease_handle", i)), egui::Sense::drag());
        if resp.dragged() {
            if let Some(p) = resp.interact_pointer_pos() {
                *hp = unmap(p);
                changed = true;
            }
        }
    }
    if changed {
        *out = Some((p1, p2));
    }

    // Handle guide lines.
    let accent = egui::Color32::from_rgb(255, 216, 51);
    painter.line_segment([map((0.0, 0.0)), map(p1)], egui::Stroke::new(1.0, accent));
    painter.line_segment([map((1.0, 1.0)), map(p2)], egui::Stroke::new(1.0, accent));

    // The timing curve itself.
    let bez = |a: f32, b: f32, s: f32| {
        let mt = 1.0 - s;
        3.0 * mt * mt * s * a + 3.0 * mt * s * s * b + s * s * s
    };
    let curve: Vec<egui::Pos2> = (0..=48)
        .map(|i| {
            let s = i as f32 / 48.0;
            map((bez(p1.0, p2.0, s), bez(p1.1, p2.1, s)))
        })
        .collect();
    painter.add(egui::Shape::line(curve, egui::Stroke::new(2.0, egui::Color32::WHITE)));

    // Control-point knobs.
    for hp in [p1, p2] {
        painter.circle_filled(map(hp), 4.0, accent);
    }
}

/// Right-hand properties panel. Reads a resolved `NodeInfo` and writes any user
/// changes into `edits`; it never touches `App`. `ease` is the selected key's
/// segment (if any) and edits go to `ease_out`.
pub(crate) fn properties_ui(
    ui: &mut egui::Ui,
    info: &Option<NodeInfo>,
    edits: &mut PropEdits,
    ease: &Option<EaseInfo>,
    ease_out: &mut Option<((f32, f32), (f32, f32))>,
) {
    ui.add_space(8.0);
    ui.heading("Properties");
    ui.separator();
    let Some(n) = info else {
        ui.add_space(8.0);
        ui.weak("Click a shape on the canvas to select it.");
        return;
    };

    egui::Grid::new("props").num_columns(3).striped(true).show(ui, |ui| {
        ui.label("Name");
        ui.strong(&n.name);
        ui.label("");
        ui.end_row();

        ui.label("Node id");
        ui.monospace(n.id.to_string());
        ui.label("");
        ui.end_row();

        // Position (x, y). DragValue gives both interactions: drag to
        // nudge, or click to type a value and commit with Enter.
        ui.label("Position");
        ui.horizontal(|ui| {
            let mut x = n.pos.0;
            let mut y = n.pos.1;
            if ui.add(egui::DragValue::new(&mut x).speed(0.5)).changed() {
                edits.pos_x = Some(x);
            }
            if ui.add(egui::DragValue::new(&mut y).speed(0.5)).changed() {
                edits.pos_y = Some(y);
            }
        });
        if key_button(ui, n.pos_anim) {
            edits.key.insert(PropKind::Position);
        }
        ui.end_row();

        ui.label("Rotation");
        let mut rot = n.rot;
        if ui
            .add(egui::DragValue::new(&mut rot).speed(0.5).suffix("°"))
            .changed()
        {
            edits.rot = Some(rot);
        }
        if key_button(ui, n.rot_anim) {
            edits.key.insert(PropKind::Rotation);
        }
        ui.end_row();

        ui.label("Scale");
        ui.horizontal(|ui| {
            let mut sx = n.scale.0;
            let mut sy = n.scale.1;
            if ui.add(egui::DragValue::new(&mut sx).speed(0.01)).changed() {
                edits.scale_x = Some(sx);
            }
            if ui.add(egui::DragValue::new(&mut sy).speed(0.01)).changed() {
                edits.scale_y = Some(sy);
            }
        });
        if key_button(ui, n.scale_anim) {
            edits.key.insert(PropKind::Scale);
        }
        ui.end_row();

        ui.label("Opacity");
        let mut op = n.opacity;
        if ui
            .add(egui::Slider::new(&mut op, 0.0..=1.0).show_value(false))
            .changed()
        {
            edits.opacity = Some(op);
        }
        if key_button(ui, n.opacity_anim) {
            edits.key.insert(PropKind::Opacity);
        }
        ui.end_row();

        ui.label("Fill");
        if let Some(mut rgb) = n.fill {
            if ui.color_edit_button_rgb(&mut rgb).changed() {
                edits.fill = Some(rgb);
            }
            if key_button(ui, n.fill_anim) {
                edits.key.insert(PropKind::Fill);
            }
        } else {
            ui.weak("none");
            ui.label("");
        }
        ui.end_row();

        // --- Stroke. Optional, so the row doubles as its add/remove
        // control: a node without one gets a "+ add" button rather
        // than disabled widgets. ---
        ui.label("Stroke");
        if let Some((mut rgb, _)) = n.stroke {
            ui.horizontal(|ui| {
                if ui.color_edit_button_rgb(&mut rgb).changed() {
                    edits.stroke_color = Some(rgb);
                }
                if ui.small_button("✕").on_hover_text("Remove stroke").clicked() {
                    edits.remove_stroke = true;
                }
            });
            if key_button(ui, n.stroke_color_anim) {
                edits.key.insert(PropKind::StrokeColor);
            }
        } else {
            if ui.small_button("+ add").clicked() {
                edits.add_stroke = true;
            }
            ui.label("");
        }
        ui.end_row();

        if let Some((_, w)) = n.stroke {
            ui.label("Stroke W");
            let mut w = w;
            if ui
                .add(egui::DragValue::new(&mut w).speed(0.1).range(0.0..=f64::MAX))
                .changed()
            {
                edits.stroke_width = Some(w);
            }
            if key_button(ui, n.stroke_width_anim) {
                edits.key.insert(PropKind::StrokeWidth);
            }
            ui.end_row();
        }

        // --- Parametric geometry. Absent for groups and for imported
        // `Path` shapes, whose geometry isn't expressed as parameters. ---
        if let Some((w, h)) = n.size {
            ui.label("Size");
            ui.horizontal(|ui| {
                let (mut w, mut h) = (w, h);
                if ui
                    .add(egui::DragValue::new(&mut w).speed(0.5).range(0.0..=f64::MAX))
                    .changed()
                {
                    edits.size_x = Some(w);
                }
                if ui
                    .add(egui::DragValue::new(&mut h).speed(0.5).range(0.0..=f64::MAX))
                    .changed()
                {
                    edits.size_y = Some(h);
                }
            });
            if key_button(ui, n.size_anim) {
                edits.key.insert(PropKind::ShapeSize);
            }
            ui.end_row();
        }

        if let Some(r) = n.radius {
            ui.label("Radius");
            let mut r = r;
            if ui
                .add(egui::DragValue::new(&mut r).speed(0.5).range(0.0..=f64::MAX))
                .changed()
            {
                edits.radius = Some(r);
            }
            if key_button(ui, n.radius_anim) {
                edits.key.insert(PropKind::ShapeRadius);
            }
            ui.end_row();
        }
    });

    ui.add_space(6.0);
    ui.weak("Drag a field to nudge, or click to type; Enter commits.");
    ui.weak("The dot button inserts a keyframe at the playhead (hollow ring = start animating).");

    // Easing editor for the selected keyframe's outgoing segment.
    if let Some(e) = ease {
        ui.separator();
        ui.strong("Easing");
        ui.weak("Timing of the selected key's outgoing segment.");
        ui.horizontal(|ui| {
            if ui.small_button("Linear").clicked() {
                *ease_out = Some(((1.0 / 3.0, 1.0 / 3.0), (2.0 / 3.0, 2.0 / 3.0)));
            }
            if ui.small_button("Smooth").clicked() {
                *ease_out = Some(((0.42, 0.0), (0.58, 1.0)));
            }
            if ui.small_button("Ease In").clicked() {
                *ease_out = Some(((0.42, 0.0), (1.0, 1.0)));
            }
            if ui.small_button("Ease Out").clicked() {
                *ease_out = Some(((0.0, 0.0), (0.58, 1.0)));
            }
        });
        ease_editor(ui, e, ease_out);
    }
}

/// Which animated property a dopesheet row refers to. Lets the UI report a
/// keyframe drag back to `App` without knowing the property's value type.
///
/// Declaration order is meaningful twice over: it's the dopesheet's row order,
/// and — because `KeySelection` is a `BTreeSet` keyed on this — it's what makes
/// a selection's entries for one property contiguous (see
/// `group_selection_by_prop`). Transform first, then paint, then geometry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum PropKind {
    Position,
    Rotation,
    Scale,
    Opacity,
    Fill,
    StrokeColor,
    StrokeWidth,
    ShapeSize,
    ShapeRadius,
}

impl PropKind {
    /// Every property that can be animated, in row order.
    pub(crate) const ALL: [PropKind; 9] = [
        PropKind::Position,
        PropKind::Rotation,
        PropKind::Scale,
        PropKind::Opacity,
        PropKind::Fill,
        PropKind::StrokeColor,
        PropKind::StrokeWidth,
        PropKind::ShapeSize,
        PropKind::ShapeRadius,
    ];

    pub(crate) fn label(self) -> &'static str {
        match self {
            PropKind::Position => "Position",
            PropKind::Rotation => "Rotation",
            PropKind::Scale => "Scale",
            PropKind::Opacity => "Opacity",
            PropKind::Fill => "Fill",
            PropKind::StrokeColor => "Stroke",
            PropKind::StrokeWidth => "Stroke W",
            PropKind::ShapeSize => "Size",
            PropKind::ShapeRadius => "Radius",
        }
    }
}

/// A borrowed animatable property, with its value type erased down to the three
/// the document actually uses.
///
/// This exists so the keyframe machinery — dopesheet rows, retiming, delete,
/// copy/paste, easing — matches on `PropKind` in exactly *one* place
/// ([`prop_of`] / [`prop_of_mut`]) instead of once per operation. Adding a new
/// animatable property is then a `PropKind` variant plus two match arms, rather
/// than an edit to eight call sites that all have to agree.
pub(crate) enum PropRef<'a> {
    Vec2(&'a Value<Vec2>),
    Num(&'a Value<f64>),
    Color(&'a Value<MColor>),
}

pub(crate) enum PropRefMut<'a> {
    Vec2(&'a mut Value<Vec2>),
    Num(&'a mut Value<f64>),
    Color(&'a mut Value<MColor>),
}

/// Call the same method on whichever `Value<T>` a `PropRef`/`PropRefMut` holds.
/// The body is written once and monomorphized per arm, which is the whole point
/// — every op below is identical apart from `T`.
macro_rules! on_prop {
    ($p:expr, $v:ident => $body:expr) => {
        match $p {
            PropRef::Vec2($v) => $body,
            PropRef::Num($v) => $body,
            PropRef::Color($v) => $body,
        }
    };
}

macro_rules! on_prop_mut {
    ($p:expr, $v:ident => $body:expr) => {
        match $p {
            PropRefMut::Vec2($v) => $body,
            PropRefMut::Num($v) => $body,
            PropRefMut::Color($v) => $body,
        }
    };
}

impl PropRef<'_> {
    pub(crate) fn is_animated(&self) -> bool {
        on_prop!(self, v => v.is_animated())
    }
    pub(crate) fn is_expr(&self) -> bool {
        on_prop!(self, v => v.is_expr())
    }
    /// The expression tree, if this property is expression-driven.
    pub(crate) fn expr(&self) -> Option<&Expr> {
        on_prop!(self, v => v.expr_ref())
    }
    pub(crate) fn key_frames(&self) -> Vec<i64> {
        on_prop!(self, v => v.key_frames())
    }
    pub(crate) fn move_keys_limits(&self, idxs: &[usize]) -> Option<(i64, i64)> {
        on_prop!(self, v => v.move_keys_limits(idxs))
    }
    pub(crate) fn segment_handles(&self, index: usize) -> Option<(Handle, Handle)> {
        on_prop!(self, v => v.segment_handles(index))
    }
    /// Copy the keys at `idxs` onto the clipboard, tagged with their type.
    pub(crate) fn keys_at(&self, idxs: &[usize]) -> ClipTrack {
        match self {
            PropRef::Vec2(v) => ClipTrack::Vec2(v.keys_at(idxs)),
            PropRef::Num(v) => ClipTrack::Num(v.keys_at(idxs)),
            PropRef::Color(v) => ClipTrack::Color(v.keys_at(idxs)),
        }
    }
}

impl PropRefMut<'_> {
    pub(crate) fn move_keys(&mut self, idxs: &[usize], delta: i64) {
        on_prop_mut!(self, v => { v.move_keys(idxs, delta); })
    }
    /// Seed an expression from the current value (see [`Value::promote_to_expr`]).
    pub(crate) fn promote_to_expr(&mut self, ctx: &mut EvalCtx) {
        on_prop_mut!(self, v => v.promote_to_expr(ctx))
    }
    /// Freeze an expression back to a constant (see [`Value::bake_to_const`]).
    pub(crate) fn bake_to_const(&mut self, ctx: &mut EvalCtx) {
        on_prop_mut!(self, v => v.bake_to_const(ctx))
    }
    /// The expression tree mutably, for structured editing by path.
    pub(crate) fn expr_mut(&mut self) -> Option<&mut Expr> {
        on_prop_mut!(self, v => v.expr_mut())
    }
    pub(crate) fn remove_key(&mut self, index: usize) {
        on_prop_mut!(self, v => v.remove_key(index))
    }
    pub(crate) fn insert_key(&mut self, frame: i64) {
        on_prop_mut!(self, v => v.insert_key(frame))
    }
    pub(crate) fn set_segment_handles(&mut self, index: usize, out: Handle, next_in: Handle) {
        on_prop_mut!(self, v => v.set_segment_handles(index, out, next_in))
    }
    /// Paste a clipboard track, but only onto a property of the same type — a
    /// `Vec2` clip must never land on a scalar. Mismatches can't happen through
    /// the UI (a clip is tagged at copy time) so they're simply ignored.
    pub(crate) fn insert_keys(&mut self, clip: &ClipTrack, offset: i64) -> Vec<usize> {
        match (self, clip) {
            (PropRefMut::Vec2(v), ClipTrack::Vec2(k)) => v.insert_keys(k, offset),
            (PropRefMut::Num(v), ClipTrack::Num(k)) => v.insert_keys(k, offset),
            (PropRefMut::Color(v), ClipTrack::Color(k)) => v.insert_keys(k, offset),
            _ => Vec::new(),
        }
    }
}

/// Borrow one of a node's animatable properties. `None` when the node doesn't
/// have it at all — a group has no fill, an ellipse has no corner radius, and a
/// hand-drawn `Path` has no parametric size.
pub(crate) fn prop_of(node: &MNode, kind: PropKind) -> Option<PropRef<'_>> {
    let tr = &node.transform;
    Some(match kind {
        PropKind::Position => PropRef::Vec2(&tr.position),
        PropKind::Rotation => PropRef::Num(&tr.rotation_deg),
        PropKind::Scale => PropRef::Vec2(&tr.scale),
        PropKind::Opacity => PropRef::Num(&tr.opacity),
        PropKind::Fill => PropRef::Color(node.fill.as_ref()?),
        PropKind::StrokeColor => PropRef::Color(&node.stroke.as_ref()?.color),
        PropKind::StrokeWidth => PropRef::Num(&node.stroke.as_ref()?.width),
        PropKind::ShapeSize => match node.shape.as_ref()? {
            MShape::Rect { size, .. } | MShape::Ellipse { size } => PropRef::Vec2(size),
            MShape::Path(_) => return None,
        },
        PropKind::ShapeRadius => match node.shape.as_ref()? {
            MShape::Rect { radius, .. } => PropRef::Num(radius),
            _ => return None,
        },
    })
}

/// Mutable twin of [`prop_of`]. Kept adjacent on purpose: the two must agree on
/// which properties exist, and they're only correct read together.
pub(crate) fn prop_of_mut(node: &mut MNode, kind: PropKind) -> Option<PropRefMut<'_>> {
    let tr = &mut node.transform;
    Some(match kind {
        PropKind::Position => PropRefMut::Vec2(&mut tr.position),
        PropKind::Rotation => PropRefMut::Num(&mut tr.rotation_deg),
        PropKind::Scale => PropRefMut::Vec2(&mut tr.scale),
        PropKind::Opacity => PropRefMut::Num(&mut tr.opacity),
        PropKind::Fill => PropRefMut::Color(node.fill.as_mut()?),
        PropKind::StrokeColor => PropRefMut::Color(&mut node.stroke.as_mut()?.color),
        PropKind::StrokeWidth => PropRefMut::Num(&mut node.stroke.as_mut()?.width),
        PropKind::ShapeSize => match node.shape.as_mut()? {
            MShape::Rect { size, .. } | MShape::Ellipse { size } => PropRefMut::Vec2(size),
            MShape::Path(_) => return None,
        },
        PropKind::ShapeRadius => match node.shape.as_mut()? {
            MShape::Rect { radius, .. } => PropRefMut::Num(radius),
            _ => return None,
        },
    })
}
