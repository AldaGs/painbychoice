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
    /// Jump to the start of the preview range.
    pub(crate) restart: bool,
    /// Jump to the last previewed frame.
    pub(crate) jump_end: bool,
    /// Seek to this frame. Already whole — every control that writes it works
    /// in frames, so nothing here can produce an off-grid time.
    pub(crate) scrub_to: Option<i64>,
    /// New work-area start / last previewed frame, from the Start/End fields.
    /// Inclusive on both ends, as the fields present them; the exclusive `end`
    /// is reconstructed by `with_work_end`.
    pub(crate) set_work_start: Option<i64>,
    pub(crate) set_work_end: Option<i64>,
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
    /// The pivot the layer rotates and scales about. It sits inside the local
    /// matrix, so the gizmo needs it to reconstruct that matrix — and it is an
    /// animatable property in its own right, with a row and a stopwatch.
    pub(crate) anchor: (f64, f64),
    pub(crate) opacity: f64,
    pub(crate) fill: Option<[f32; 3]>,
    /// Parametric geometry, `None` for a group or a hand-drawn `Path`.
    pub(crate) size: Option<(f64, f64)>,
    /// Corner radius — `Some` only for a Rect.
    pub(crate) radius: Option<f64>,
    /// Stroke color + width, `None` when the node has no stroke.
    pub(crate) stroke: Option<([f32; 3], f64)>,
    pub(crate) anchor_anim: bool,
    pub(crate) pos_anim: bool,
    pub(crate) rot_anim: bool,
    pub(crate) scale_anim: bool,
    pub(crate) opacity_anim: bool,
    pub(crate) fill_anim: bool,
    pub(crate) size_anim: bool,
    pub(crate) radius_anim: bool,
    pub(crate) stroke_color_anim: bool,
    pub(crate) stroke_width_anim: bool,
    /// Text-layer fields, `Some` only for a `Shape::Text`.
    pub(crate) text: Option<TextInfo>,
    /// The **knobs** this layer exposes — named values a `param` node in the
    /// graph reads (`param("gain")` resolves against whichever layer the driver
    /// points at).
    ///
    /// They live here, not in the Nodes panel, because they are this layer's own
    /// data: one layer's knobs are as much a property of it as its opacity, and
    /// editing them from a panel-wide "pick a layer" combo meant choosing the
    /// selection twice. What makes a knob *useful* is still the graph — but so
    /// is what makes `position` useful, and that has always lived here.
    pub(crate) knobs: Vec<KnobInfo>,
}

/// The text-specific half of a selected node. `content` and `size` are `Value`s
/// (and so keyframable and scriptable); family, alignment, and wrap width are
/// plain data, edited directly.
pub(crate) struct TextInfo {
    /// The string **as resolved this frame**, not the recipe — so a keyframed
    /// or scripted content shows what is actually on screen, the same way
    /// `size` does.
    pub(crate) content: String,
    pub(crate) family: String,
    pub(crate) size: f64,
    pub(crate) align: TextAlign,
    /// `None` = one line, no wrapping.
    pub(crate) max_width: Option<f64>,
    pub(crate) size_anim: bool,
    pub(crate) content_anim: bool,
    /// The named family isn't installed, so the frame is drawing a substitute.
    /// Blank is never "missing" — that's the default on purpose.
    pub(crate) family_missing: bool,
}

/// egui's color buttons speak `[f32; 3]`; the document speaks `Color`.
pub(crate) fn rgb_color(rgb: [f32; 3]) -> MColor {
    MColor::rgb(rgb[0] as f64, rgb[1] as f64, rgb[2] as f64)
}

/// The amber used for "this still works, but not the way you asked" — the same
/// hue the comp bar's warning count uses, so one colour means one thing.
pub(crate) const WARN_COLOR: egui::Color32 = egui::Color32::from_rgb(220, 160, 60);

/// What the font picker draws from: every installed family, and the ones
/// applied this session. Borrowed from `App`, which enumerates the system once.
pub(crate) struct FontList<'a> {
    pub(crate) all: &'a [String],
    pub(crate) recent: &'a [String],
}

/// The label shown for a family, so "no family chosen" reads as a real choice
/// rather than an empty box.
fn family_label(family: &str) -> &str {
    if family.trim().is_empty() {
        "(system default)"
    } else {
        family
    }
}

/// A font picker in the shape modern editors use: a searchable list of every
/// installed family, the ones you've used recently pinned at the top, and
/// **hover to preview, click to apply**.
///
/// Hovering only reports [`PropEdits::text_family_preview`]; the document is
/// untouched until a click. That's what makes browsing 300 fonts non-destructive
/// — `App` renders the hovered family for that frame and drops it the moment the
/// pointer leaves (see `App::preview_project`).
fn font_picker(ui: &mut egui::Ui, t: &TextInfo, fonts: &FontList<'_>, edits: &mut PropEdits) {
    egui::ComboBox::from_id_salt("font_family")
        .width(150.0)
        .selected_text(family_label(&t.family))
        .show_ui(ui, |ui| {
            // The search box keeps its text across frames in egui memory, the
            // same way the graph panel's "new parameter" field does — the panel
            // stays a pure function of the document.
            let filter_id = egui::Id::new("font_filter");
            let mut filter: String = ui.data_mut(|d| d.get_temp(filter_id).unwrap_or_default());
            ui.add(
                egui::TextEdit::singleline(&mut filter)
                    .hint_text("search fonts")
                    .desired_width(f32::INFINITY),
            );
            ui.data_mut(|d| d.insert_temp(filter_id, filter.clone()));
            let needle = filter.trim().to_lowercase();
            let matches = |name: &str| needle.is_empty() || name.to_lowercase().contains(&needle);

            // One row: hover previews, click commits. Both report through
            // `edits`; neither writes the document here.
            let row = |ui: &mut egui::Ui, name: &str, edits: &mut PropEdits| {
                let resp = ui.selectable_label(name == t.family, family_label(name));
                if resp.hovered() {
                    edits.text_family_preview = Some(name.to_string());
                }
                if resp.clicked() {
                    edits.text_family = Some(name.to_string());
                }
            };

            egui::ScrollArea::vertical().max_height(260.0).show(ui, |ui| {
                // The deliberate default, always reachable and never filtered
                // away — it's how you get *back* from a named family.
                row(ui, "", edits);

                let recent: Vec<&String> =
                    fonts.recent.iter().filter(|n| matches(n)).collect();
                if !recent.is_empty() {
                    ui.separator();
                    ui.weak("Recent");
                    for name in recent {
                        row(ui, name, edits);
                    }
                }

                ui.separator();
                ui.weak("All fonts");
                let mut any = false;
                for name in fonts.all.iter().filter(|n| matches(n)) {
                    any = true;
                    row(ui, name, edits);
                }
                if !any {
                    ui.weak("no match");
                }
            });
        });
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
        let anchor = tr.anchor.resolve(ctx);
        NodeInfo {
            name: node.name.clone(),
            id: node.id.0,
            pos: (pos.x, pos.y),
            rot: tr.rotation_deg.resolve(ctx),
            scale: (scale.x, scale.y),
            anchor: (anchor.x, anchor.y),
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
            anchor_anim: tr.anchor.is_animated(),
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
            text: match node.shape.as_ref() {
                Some(MShape::Text { content, family, size, align, max_width }) => Some(TextInfo {
                    content: content.resolve(ctx),
                    family: family.clone(),
                    size: size.resolve(ctx),
                    align: *align,
                    max_width: *max_width,
                    size_anim: is_anim(node, PropKind::TextSize),
                    content_anim: is_anim(node, PropKind::TextContent),
                    family_missing: !motion_core::text::font_exists(family),
                }),
                _ => None,
            },
            knobs: node.params.iter().map(crate::nodegraph::knob_info).collect(),
        }
    }
}

/// Edits collected from the properties panel this frame. Any `Some` field is a
/// new value the user dialed in; `None` means untouched.
#[derive(Default)]
pub(crate) struct PropEdits {
    pub(crate) anchor_x: Option<f64>,
    pub(crate) anchor_y: Option<f64>,
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
    // Text-layer edits. `text_max_width` is a double option on purpose: the
    // outer says "the user changed it this frame", the inner is the value —
    // `Some(None)` is "stop wrapping", which a flat `Option` couldn't express.
    pub(crate) text_content: Option<String>,
    pub(crate) text_family: Option<String>,
    /// The family hovered in the picker this frame — previewed on the canvas,
    /// never written to the document. `None` (the default) means "nothing
    /// hovered", which is what ends a preview.
    pub(crate) text_family_preview: Option<String>,
    pub(crate) text_size: Option<f64>,
    pub(crate) text_align: Option<TextAlign>,
    #[allow(clippy::option_option)]
    pub(crate) text_max_width: Option<Option<f64>>,
    // Insert-keyframe-at-playhead requests (the "stopwatch"). Keyed by
    // `PropKind` rather than one bool per property, so adding an animatable
    // property doesn't grow this struct.
    pub(crate) key: KeySelectionKinds,
    /// A knob added, removed, or re-valued this frame. Reuses the Nodes panel's
    /// owner-agnostic op verbatim — a knob is the same thing whichever panel you
    /// reach it from, so it gets one op and one apply path, not two that have to
    /// agree.
    pub(crate) knob: Option<NgKnobOp>,
}

/// The set of properties whose stopwatch was clicked this frame.
pub(crate) type KeySelectionKinds = std::collections::BTreeSet<PropKind>;

/// A "stopwatch" toggle: a filled dot when the property is animated, a hollow
/// ring when constant. Clicking it inserts a keyframe at the playhead
/// (promoting a constant to a track). The indicator is *painted* rather than
/// drawn from a glyph, since the circle/diamond glyphs are missing from egui's
/// default font and render as tofu boxes.
pub(crate) fn key_button(ui: &mut egui::Ui, animated: bool) -> bool {
    // This used to be two painted circles (filled = animated, hollow = not),
    // because egui's font had no keyframe glyph. It has one now, so the state
    // rides on colour instead: accent when the property is already a track,
    // dim when clicking would *start* animating it.
    let colour = if animated {
        egui::Color32::from_rgb(255, 216, 51)
    } else {
        egui::Color32::from_gray(120)
    };
    let tip = if animated {
        "Insert a keyframe at the playhead"
    } else {
        "Start animating: insert the first keyframe at the playhead"
    };
    ui.add(egui::Button::new(icon::text(icon::KEYFRAME).color(colour)).frame(false))
        .on_hover_text(tip)
        .clicked()
}

/// The two normalized cubic-bezier control points of the selected keyframe's
/// outgoing timing segment (`cubic-bezier(p1, p2)` with endpoints 0,0 and 1,1).
pub(crate) struct EaseInfo {
    pub(crate) p1: (f32, f32),
    pub(crate) p2: (f32, f32),
}

/// What the ease library wants done to the *project's* saved curves. Reported
/// out like every other edit here: this module never touches `App`.
pub(crate) enum EaseLibEdit {
    /// Save the selected key's current handles under this name.
    Save(String),
    /// Drop the project preset at this index.
    Delete(usize),
}

/// The preset row above the curve: pick a built-in or a project curve, and save
/// the current one into the project.
///
/// Built-ins and project presets share one dropdown because to the user they
/// are the same thing — a named curve to apply. Only the project half can be
/// deleted, which is also the only thing that marks the two apart.
pub(crate) fn ease_library_ui(
    ui: &mut egui::Ui,
    ease: &EaseInfo,
    eases: &[EasePreset],
    out: &mut Option<((f32, f32), (f32, f32))>,
    lib_out: &mut Option<EaseLibEdit>,
) {
    let cur = |h: (f32, f32)| Handle::new(h.0 as f64, h.1 as f64);
    let (p1, p2) = (cur(ease.p1), cur(ease.p2));
    // What the combo shows: the preset the handles currently *are*, if any.
    let label = EasePreset::BUILT_IN
        .iter()
        .find(|(_, o, i)| EasePreset::new("", *o, *i).matches(p1, p2))
        .map(|(n, _, _)| (*n).to_string())
        .or_else(|| eases.iter().find(|p| p.matches(p1, p2)).map(|p| p.name.clone()))
        .unwrap_or_else(|| "Custom".to_string());

    ui.horizontal(|ui| {
        egui::ComboBox::from_id_salt("ease_preset").selected_text(label).show_ui(ui, |ui| {
            for (name, o, i) in EasePreset::BUILT_IN {
                if ui.selectable_label(false, *name).clicked() {
                    *out = Some(((o.x as f32, o.y as f32), (i.x as f32, i.y as f32)));
                }
            }
            if !eases.is_empty() {
                ui.separator();
                ui.weak("Project");
            }
            for (idx, p) in eases.iter().enumerate() {
                ui.horizontal(|ui| {
                    if ui.selectable_label(false, &p.name).clicked() {
                        *out = Some((
                            (p.out.x as f32, p.out.y as f32),
                            (p.into.x as f32, p.into.y as f32),
                        ));
                    }
                    if ui.small_button("✕").on_hover_text("Remove from project").clicked() {
                        *lib_out = Some(EaseLibEdit::Delete(idx));
                    }
                });
            }
        });

        // The name buffer lives in egui's temp store: it's scratch for one
        // save, and parking it on `App` would make it a document-shaped thing
        // that has to be cleared on every selection change.
        let id = ui.id().with("ease_save_name");
        let mut name = ui.data_mut(|d| d.get_temp::<String>(id).unwrap_or_default());
        let edit = ui
            .add(egui::TextEdit::singleline(&mut name).desired_width(96.0).hint_text("Save as…"));
        let entered = edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        let named = !name.trim().is_empty();
        let clicked = ui
            .add_enabled(named, egui::Button::new("＋"))
            .on_hover_text("Save this curve to the project")
            .clicked();
        if named && (clicked || entered) {
            *lib_out = Some(EaseLibEdit::Save(name.trim().to_string()));
            name.clear();
        }
        ui.data_mut(|d| d.insert_temp(id, name));
    });
}

/// How far outside [0,1] an ease handle's y may reach, as a fraction of the
/// unit square. The editor reserves this much margin above and below so an
/// overshooting handle — and a handle parked exactly on 0 or 1 — still draws
/// whole instead of being clipped at the widget edge.
const OVERSHOOT: f32 = 0.25;

/// A CSS-style cubic-bezier editor. Draws the timing curve in a unit square and
/// lets the two control points be dragged. New handles are reported in `out`.
pub(crate) fn ease_editor(ui: &mut egui::Ui, ease: &EaseInfo, out: &mut Option<((f32, f32), (f32, f32))>) {
    let sz = (ui.available_width() - 8.0).clamp(80.0, 180.0);
    // The unit square is inset inside the widget so a handle parked at y=0 or
    // y=1 — and the overshoot beyond them that back/anticipation eases need —
    // still draws entirely inside the box instead of being clipped at the edge.
    let pad = sz * OVERSHOOT;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(sz, sz + 2.0 * pad), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    let unit = rect.shrink2(egui::vec2(0.0, pad));

    // value (x right, y up) in [0,1] → screen (y is down).
    let map = |v: (f32, f32)| {
        egui::pos2(unit.left() + v.0 * unit.width(), unit.bottom() - v.1 * unit.height())
    };
    let unmap = |p: egui::Pos2| {
        (
            ((p.x - unit.left()) / unit.width()).clamp(0.0, 1.0),
            ((unit.bottom() - p.y) / unit.height()).clamp(-OVERSHOOT, 1.0 + OVERSHOOT),
        )
    };

    painter.rect_filled(rect, 3.0, egui::Color32::from_gray(28));
    // Mark where the unit square's 0 and 1 sit, now that they're inset.
    for y in [unit.top(), unit.bottom()] {
        painter.line_segment(
            [egui::pos2(unit.left(), y), egui::pos2(unit.right(), y)],
            egui::Stroke::new(1.0, egui::Color32::from_gray(46)),
        );
    }
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

    // Numeric readout, also editable: dragging a knob is fast but imprecise,
    // and this is the only way to type an exact `cubic-bezier(...)`.
    let mut typed = false;
    ui.horizontal(|ui| {
        for (v, lo, hi) in [
            (&mut p1.0, 0.0, 1.0),
            (&mut p1.1, -OVERSHOOT, 1.0 + OVERSHOOT),
            (&mut p2.0, 0.0, 1.0),
            (&mut p2.1, -OVERSHOOT, 1.0 + OVERSHOOT),
        ] {
            let r = ui.add(
                egui::DragValue::new(v)
                    .speed(0.005)
                    .range(lo..=hi)
                    .max_decimals(3),
            );
            typed |= r.changed();
        }
    });
    if typed {
        *out = Some((p1, p2));
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
    eases: &[EasePreset],
    lib_out: &mut Option<EaseLibEdit>,
    fonts: &FontList<'_>,
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

        // Anchor (x, y) — the pivot rotation and scale turn about. Editing it
        // *here* moves the layer, because position is measured from it; the
        // canvas handle instead compensates position so the layer stays put.
        // Both are wanted, and which you get is which one you reached for.
        ui.label("Anchor");
        ui.horizontal(|ui| {
            let mut x = n.anchor.0;
            let mut y = n.anchor.1;
            if ui.add(egui::DragValue::new(&mut x).speed(0.5)).changed() {
                edits.anchor_x = Some(x);
            }
            if ui.add(egui::DragValue::new(&mut y).speed(0.5)).changed() {
                edits.anchor_y = Some(y);
            }
        });
        if key_button(ui, n.anchor_anim) {
            edits.key.insert(PropKind::Anchor);
        }
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

        // --- Text. Content and font size are `Value`s and carry stopwatches;
        // font/align/wrap are plain data. The content field shows the string
        // *resolved this frame*, so a keyframed or scripted title reads back
        // what is actually on screen — and typing into it while the property is
        // animated writes a key at the playhead rather than replacing the
        // track. ---
        if let Some(t) = &n.text {
            ui.label("Text");
            let mut content = t.content.clone();
            if ui
                .add(
                    egui::TextEdit::multiline(&mut content)
                        .desired_rows(2)
                        .desired_width(f32::INFINITY),
                )
                .changed()
            {
                edits.text_content = Some(content);
            }
            if key_button(ui, t.content_anim) {
                edits.key.insert(PropKind::TextContent);
            }
            ui.end_row();

            ui.label("Font");
            ui.horizontal(|ui| {
                // The substitution is invisible on the canvas — the text draws
                // perfectly well in the wrong face — so the missing font gets a
                // marker right beside the control that caused it.
                if t.family_missing {
                    ui.colored_label(WARN_COLOR, icon::text(icon::WARNING)).on_hover_text(
                        format!(
                            "'{}' isn't installed on this machine.\n\
                             Drawing with the system default instead.\n\
                             The project still names '{}', so it will look right \
                             on a machine that has it.",
                            t.family.trim(),
                            t.family.trim()
                        ),
                    );
                }
                font_picker(ui, t, fonts, edits);
            });
            ui.label("");
            ui.end_row();

            ui.label("Font Size");
            let mut size = t.size;
            if ui
                .add(egui::DragValue::new(&mut size).speed(0.5).range(0.0..=f64::MAX))
                .changed()
            {
                edits.text_size = Some(size);
            }
            if key_button(ui, t.size_anim) {
                edits.key.insert(PropKind::TextSize);
            }
            ui.end_row();

            ui.label("Align");
            ui.horizontal(|ui| {
                for a in TextAlign::ALL {
                    if ui.selectable_label(a == t.align, a.label()).clicked() && a != t.align {
                        edits.text_align = Some(a);
                    }
                }
            });
            ui.label("");
            ui.end_row();

            // Wrapping is off until asked for, so a caption stays on one line
            // unless you give it a width to break against.
            ui.label("Wrap");
            ui.horizontal(|ui| match t.max_width {
                Some(w) => {
                    let mut w = w;
                    if ui
                        .add(egui::DragValue::new(&mut w).speed(1.0).range(1.0..=f64::MAX))
                        .changed()
                    {
                        edits.text_max_width = Some(Some(w));
                    }
                    if ui.small_button("✕").on_hover_text("Stop wrapping").clicked() {
                        edits.text_max_width = Some(None);
                    }
                }
                None => {
                    ui.weak("off");
                    if ui.small_button("+ wrap").clicked() {
                        edits.text_max_width = Some(Some(400.0));
                    }
                }
            });
            ui.label("");
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
        ease_library_ui(ui, e, eases, ease_out, lib_out);
        ease_editor(ui, e, ease_out);
    }

    // The layer's exposed **knobs**, last: they're the seam to the graph rather
    // than something the layer draws with, so they sit below the properties that
    // do. The widget is the Nodes panel's own `knobs_ui` — the module-scope knob
    // editor and this are the same editor, because a knob is the same thing at
    // both scopes.
    ui.separator();
    crate::nodegraph::knobs_ui(ui, ParamOwner::Node(NodeId(n.id)), &n.knobs, &mut edits.knob);
    ui.weak("A `param(\"name\")` node in the graph reads these, on whichever layer it drives.");
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
    Anchor,
    Position,
    Rotation,
    Scale,
    Opacity,
    Fill,
    StrokeColor,
    StrokeWidth,
    ShapeSize,
    ShapeRadius,
    TextSize,
    TextContent,
    TimeRemap,
}

impl PropKind {
    /// Every property that can be animated, in row order.
    pub(crate) const ALL: [PropKind; 13] = [
        PropKind::Anchor,
        PropKind::Position,
        PropKind::Rotation,
        PropKind::Scale,
        PropKind::Opacity,
        PropKind::Fill,
        PropKind::StrokeColor,
        PropKind::StrokeWidth,
        PropKind::ShapeSize,
        PropKind::ShapeRadius,
        PropKind::TextSize,
        PropKind::TextContent,
        PropKind::TimeRemap,
    ];

    pub(crate) fn label(self) -> &'static str {
        match self {
            PropKind::Anchor => "Anchor",
            PropKind::Position => "Position",
            PropKind::Rotation => "Rotation",
            PropKind::Scale => "Scale",
            PropKind::Opacity => "Opacity",
            PropKind::Fill => "Fill",
            PropKind::StrokeColor => "Stroke",
            PropKind::StrokeWidth => "Stroke W",
            PropKind::ShapeSize => "Size",
            PropKind::ShapeRadius => "Radius",
            PropKind::TextSize => "Font Size",
            PropKind::TextContent => "Content",
            PropKind::TimeRemap => "Time Remap",
        }
    }

    /// The editor `PropKind` for a core [`PropPath`]. The two enumerate the same
    /// properties from opposite sides of the crate boundary — core names the
    /// referenceable property, the editor keys its keyframe/expression machinery
    /// — so a node-graph driver (stored as a `PropPath`) maps here to reach
    /// [`prop_of_mut`].
    pub(crate) fn from_path(p: PropPath) -> PropKind {
        match p {
            PropPath::Anchor => PropKind::Anchor,
            PropPath::Position => PropKind::Position,
            PropPath::Rotation => PropKind::Rotation,
            PropPath::Scale => PropKind::Scale,
            PropPath::Opacity => PropKind::Opacity,
            PropPath::Fill => PropKind::Fill,
            PropPath::StrokeColor => PropKind::StrokeColor,
            PropPath::StrokeWidth => PropKind::StrokeWidth,
            PropPath::ShapeSize => PropKind::ShapeSize,
            PropPath::ShapeRadius => PropKind::ShapeRadius,
            PropPath::TextSize => PropKind::TextSize,
            PropPath::TextContent => PropKind::TextContent,
            PropPath::TimeRemap => PropKind::TimeRemap,
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
    /// Text. Its track holds rather than interpolates (see `Animatable for
    /// String`), so its easing handles are inert — but it goes through the same
    /// machinery regardless, which is what gives a text layer's content a
    /// dopesheet row, marquee-select, retiming, and copy/paste for free.
    Str(&'a Value<String>),
}

/// Per-channel curve colours. X/R warm, Y/G green, B blue — the axis colours
/// the canvas gizmo already uses, so "red is X" means one thing in the app.
pub(crate) const CH_X: egui::Color32 = egui::Color32::from_rgb(232, 96, 96);
pub(crate) const CH_Y: egui::Color32 = egui::Color32::from_rgb(120, 210, 120);
pub(crate) const CH_R: egui::Color32 = CH_X;
pub(crate) const CH_G: egui::Color32 = CH_Y;
pub(crate) const CH_B: egui::Color32 = egui::Color32::from_rgb(110, 160, 240);

/// One numeric channel of an animated property, as its own scalar track.
pub(crate) struct Channel {
    /// "X", "Y", "R"… Empty for a property that has only one channel, where a
    /// suffix would be noise.
    pub(crate) name: &'static str,
    pub(crate) color: egui::Color32,
    pub(crate) track: Track<f64>,
}

impl Channel {
    fn from<T: Clone>(
        keys: &[Keyframe<T>],
        name: &'static str,
        color: egui::Color32,
        get: impl Fn(&T) -> f64,
    ) -> Self {
        let keys = keys
            .iter()
            .map(|k| {
                Keyframe::shaped(
                    k.frame,
                    get(&k.value),
                    k.out_handle,
                    k.in_handle,
                    k.interp,
                    k.broken,
                )
            })
            .collect();
        Self { name, color, track: Track::new(keys) }
    }
}

pub(crate) enum PropRefMut<'a> {
    Vec2(&'a mut Value<Vec2>),
    Num(&'a mut Value<f64>),
    Color(&'a mut Value<MColor>),
    Str(&'a mut Value<String>),
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
            PropRef::Str($v) => $body,
        }
    };
}

macro_rules! on_prop_mut {
    ($p:expr, $v:ident => $body:expr) => {
        match $p {
            PropRefMut::Vec2($v) => $body,
            PropRefMut::Num($v) => $body,
            PropRefMut::Color($v) => $body,
            PropRefMut::Str($v) => $body,
        }
    };
}

impl PropRef<'_> {
    pub(crate) fn is_animated(&self) -> bool {
        on_prop!(self, v => v.is_animated())
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
    /// This property as one scalar track per numeric channel — what a curve
    /// editor plots. A `Vec2` becomes X and Y, a colour becomes R/G/B; text has
    /// no numeric channel at all, so it yields none and simply doesn't appear.
    ///
    /// The channel tracks carry the *original* keys' timing (see
    /// [`Keyframe::shaped`]), so sampling one is sampling the real animation —
    /// the drawn curve can't drift from what plays back.
    pub(crate) fn channels(&self) -> Vec<Channel> {
        match self {
            PropRef::Vec2(v) => vec![
                Channel::from(v.keys(), "X", CH_X, |p: &Vec2| p.x),
                Channel::from(v.keys(), "Y", CH_Y, |p: &Vec2| p.y),
            ],
            PropRef::Num(v) => vec![Channel::from(v.keys(), "", CH_X, |n: &f64| *n)],
            PropRef::Color(v) => vec![
                Channel::from(v.keys(), "R", CH_R, |c: &MColor| c.r),
                Channel::from(v.keys(), "G", CH_G, |c: &MColor| c.g),
                Channel::from(v.keys(), "B", CH_B, |c: &MColor| c.b),
            ],
            PropRef::Str(_) => Vec::new(),
        }
    }
    /// Copy the keys at `idxs` onto the clipboard, tagged with their type.
    pub(crate) fn keys_at(&self, idxs: &[usize]) -> ClipTrack {
        match self {
            PropRef::Vec2(v) => ClipTrack::Vec2(v.keys_at(idxs)),
            PropRef::Num(v) => ClipTrack::Num(v.keys_at(idxs)),
            PropRef::Color(v) => ClipTrack::Color(v.keys_at(idxs)),
            PropRef::Str(v) => ClipTrack::Str(v.keys_at(idxs)),
        }
    }
}

impl PropRefMut<'_> {
    pub(crate) fn move_keys(&mut self, idxs: &[usize], delta: i64) {
        on_prop_mut!(self, v => { v.move_keys(idxs, delta); })
    }
    /// Freeze an expression back to a constant (see [`Value::bake_to_const`]).
    pub(crate) fn bake_to_const(&mut self, ctx: &mut EvalCtx) {
        on_prop_mut!(self, v => v.bake_to_const(ctx))
    }
    /// The expression tree mutably, for structured editing by path.
    /// Replace the whole value with an expression, whatever it was before.
    /// Unlike `promote_to_expr` (which seeds from the current value), this is
    /// for handing a property a recipe outright — linking a module, say.
    pub(crate) fn set_expr(&mut self, expr: Expr) {
        on_prop_mut!(self, v => **v = Value::expr(expr.clone()))
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
    /// Write one numeric channel of keyframe `index`, leaving the others (and
    /// the key's timing) alone — the inverse of [`PropRef::channels`], and what
    /// dragging a key up or down in the curve editor produces.
    ///
    /// Channel indices match `channels()` order. A channel that doesn't exist
    /// on this property is ignored rather than clamped to a neighbour: a stale
    /// index should do nothing, not silently edit the wrong axis.
    pub(crate) fn set_channel_value(&mut self, index: usize, channel: usize, value: f64) {
        match self {
            PropRefMut::Vec2(v) => {
                let Some(k) = v.keys().get(index) else { return };
                let mut p = k.value;
                match channel {
                    0 => p.x = value,
                    1 => p.y = value,
                    _ => return,
                }
                v.set_key_value(index, p);
            }
            PropRefMut::Num(v) => {
                if channel == 0 {
                    v.set_key_value(index, value);
                }
            }
            PropRefMut::Color(v) => {
                let Some(k) = v.keys().get(index) else { return };
                let mut c = k.value;
                // Colour channels stay in [0,1]: the curve editor's vertical
                // axis is unbounded, but a colour outside the unit range is not
                // a colour, and clamping here beats every renderer guessing.
                let value = value.clamp(0.0, 1.0);
                match channel {
                    0 => c.r = value,
                    1 => c.g = value,
                    2 => c.b = value,
                    _ => return,
                }
                v.set_key_value(index, c);
            }
            // Text has no numeric channel to plot, so nothing can address one.
            PropRefMut::Str(_) => {}
        }
    }

    pub(crate) fn set_segment_interp(&mut self, index: usize, interp: Interp) {
        on_prop_mut!(self, v => v.set_segment_interp(index, interp))
    }
    pub(crate) fn set_key_broken(&mut self, index: usize, broken: bool) {
        on_prop_mut!(self, v => v.set_key_broken(index, broken))
    }
    /// Paste a clipboard track, but only onto a property of the same type — a
    /// `Vec2` clip must never land on a scalar. Mismatches can't happen through
    /// the UI (a clip is tagged at copy time) so they're simply ignored.
    pub(crate) fn insert_keys(&mut self, clip: &ClipTrack, offset: i64) -> Vec<usize> {
        match (self, clip) {
            (PropRefMut::Vec2(v), ClipTrack::Vec2(k)) => v.insert_keys(k, offset),
            (PropRefMut::Num(v), ClipTrack::Num(k)) => v.insert_keys(k, offset),
            (PropRefMut::Color(v), ClipTrack::Color(k)) => v.insert_keys(k, offset),
            (PropRefMut::Str(v), ClipTrack::Str(k)) => v.insert_keys(k, offset),
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
        PropKind::Anchor => PropRef::Vec2(&tr.anchor),
        PropKind::Position => PropRef::Vec2(&tr.position),
        PropKind::Rotation => PropRef::Num(&tr.rotation_deg),
        PropKind::Scale => PropRef::Vec2(&tr.scale),
        PropKind::Opacity => PropRef::Num(&tr.opacity),
        PropKind::Fill => PropRef::Color(node.fill.as_ref()?),
        PropKind::StrokeColor => PropRef::Color(&node.stroke.as_ref()?.color),
        PropKind::StrokeWidth => PropRef::Num(&node.stroke.as_ref()?.width),
        PropKind::ShapeSize => match node.shape.as_ref()? {
            MShape::Rect { size, .. }
            | MShape::Ellipse { size }
            // Footage's frame rectangle is its size, so scaling a clip goes
            // through the same property (and the same gizmo handles) a
            // rectangle uses.
            | MShape::Image { size, .. } => PropRef::Vec2(size),
            MShape::Path(_) | MShape::Text { .. } => return None,
        },
        // Only a *remapped* clip has this: an unremapped one plays at its
        // natural rate and has no curve to key.
        PropKind::TimeRemap => match node.shape.as_ref()? {
            MShape::Image { time_remap, .. } => PropRef::Num(time_remap.as_ref()?),
            _ => return None,
        },
        PropKind::ShapeRadius => match node.shape.as_ref()? {
            MShape::Rect { radius, .. } => PropRef::Num(radius),
            _ => return None,
        },
        PropKind::TextSize => match node.shape.as_ref()? {
            MShape::Text { size, .. } => PropRef::Num(size),
            _ => return None,
        },
        PropKind::TextContent => match node.shape.as_ref()? {
            MShape::Text { content, .. } => PropRef::Str(content),
            _ => return None,
        },
    })
}

/// Mutable twin of [`prop_of`]. Kept adjacent on purpose: the two must agree on
/// which properties exist, and they're only correct read together.
pub(crate) fn prop_of_mut(node: &mut MNode, kind: PropKind) -> Option<PropRefMut<'_>> {
    let tr = &mut node.transform;
    Some(match kind {
        PropKind::Anchor => PropRefMut::Vec2(&mut tr.anchor),
        PropKind::Position => PropRefMut::Vec2(&mut tr.position),
        PropKind::Rotation => PropRefMut::Num(&mut tr.rotation_deg),
        PropKind::Scale => PropRefMut::Vec2(&mut tr.scale),
        PropKind::Opacity => PropRefMut::Num(&mut tr.opacity),
        PropKind::Fill => PropRefMut::Color(node.fill.as_mut()?),
        PropKind::StrokeColor => PropRefMut::Color(&mut node.stroke.as_mut()?.color),
        PropKind::StrokeWidth => PropRefMut::Num(&mut node.stroke.as_mut()?.width),
        PropKind::ShapeSize => match node.shape.as_mut()? {
            MShape::Rect { size, .. }
            | MShape::Ellipse { size }
            | MShape::Image { size, .. } => PropRefMut::Vec2(size),
            MShape::Path(_) | MShape::Text { .. } => return None,
        },
        PropKind::TimeRemap => match node.shape.as_mut()? {
            MShape::Image { time_remap, .. } => PropRefMut::Num(time_remap.as_mut()?),
            _ => return None,
        },
        PropKind::ShapeRadius => match node.shape.as_mut()? {
            MShape::Rect { radius, .. } => PropRefMut::Num(radius),
            _ => return None,
        },
        PropKind::TextSize => match node.shape.as_mut()? {
            MShape::Text { size, .. } => PropRefMut::Num(size),
            _ => return None,
        },
        PropKind::TextContent => match node.shape.as_mut()? {
            MShape::Text { content, .. } => PropRefMut::Str(content),
            _ => return None,
        },
    })
}
