//! pbc — the live GPU shell with an egui overlay.
//!
//! Every frame: read the wall clock, compute a looped time `t`, call
//! `motion_core::evaluate(doc, t)`, rasterize the resulting `Scene` with vello,
//! then draw an egui transport bar (play/pause, restart, and a scrubbable
//! playhead) on top. Dragging the playhead just seeks — i.e. evaluates at a
//! different `t` — which is the whole non-linear model made interactive.
//!
//! Rendering order per frame:
//!   1. vello renders the scene into its offscreen target texture,
//!   2. we blit that target onto the swapchain surface,
//!   3. egui renders the UI on top (LoadOp::Load, so it composites over).
//!
//! The engine (`motion-core`) has no idea any of this exists.

use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::Instant;

use kurbo::{Affine, Point, Shape as _, Stroke as KurboStroke, Vec2};
use motion_core::{
    demo::demo_document, evaluate, Color as MColor, Document, Handle, Node as MNode, NodeId,
    Scene as MScene, Shape as MShape, Transform, Value,
};
use vello::peniko::{Color, Fill};
use vello::util::{RenderContext, RenderSurface};
use vello::wgpu;
use vello::{AaConfig, AaSupport, Renderer, RendererOptions, Scene as VScene};
use winit::application::ApplicationHandler;
use winit::event::{ElementState, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, NamedKey};
use winit::window::{Window, WindowId};

/// Convert one core `Color` into a vello/peniko color, folding in an opacity.
fn to_peniko(c: MColor, opacity: f64) -> Color {
    Color::new([c.r as f32, c.g as f32, c.b as f32, (c.a * opacity) as f32])
}

/// Convert an evaluated engine `Scene` into a `vello::Scene`, prepending a
/// global transform that fits the composition into the window. The composition
/// bounds are drawn first (so the editable frame is visible), then the shapes,
/// then the selection outline on top.
fn to_vello(scene: &MScene, fit: Affine, comp: (f64, f64), selected: Option<NodeId>) -> VScene {
    let mut vs = VScene::new();

    // Composition frame: a slightly lighter fill plus a border, so the comp
    // bounds stand out from the letterbox and resolution changes are visible.
    let comp_rect = kurbo::Rect::new(0.0, 0.0, comp.0, comp.1);
    vs.fill(Fill::NonZero, fit, Color::new([0.14, 0.15, 0.18, 1.0]), None, &comp_rect);
    let scale = fit.as_coeffs()[0].abs().max(1e-6);
    vs.stroke(
        &KurboStroke::new(1.5 / scale),
        fit,
        Color::new([0.35, 0.37, 0.42, 1.0]),
        None,
        &comp_rect,
    );

    for item in &scene.items {
        let xf = fit * item.transform;
        if let Some(fill) = item.fill {
            vs.fill(Fill::NonZero, xf, to_peniko(fill, item.opacity), None, &item.path);
        }
        if let Some((color, width)) = item.stroke {
            vs.stroke(
                &KurboStroke::new(width),
                xf,
                to_peniko(color, item.opacity),
                None,
                &item.path,
            );
        }
    }
    // Selection outline on top of everything.
    if let Some(sel) = selected {
        if let Some(item) = scene.items.iter().find(|i| i.source == sel) {
            let xf = fit * item.transform;
            // Width is in the item's local space; keep it visible but modest.
            vs.stroke(
                &KurboStroke::new(4.0),
                xf,
                Color::new([1.0, 0.85, 0.2, 1.0]),
                None,
                &item.path,
            );
        }
    }
    vs
}

/// Pick the front-most scene item under a point given in physical pixels.
/// Returns the `NodeId` that produced it, or `None` for empty space.
fn pick(scene: &MScene, fit: Affine, px: (f64, f64)) -> Option<NodeId> {
    let comp_point = fit.inverse() * Point::new(px.0, px.1);
    // Iterate back-to-front: the last item drawn is on top.
    scene.items.iter().rev().find_map(|item| {
        let local = item.transform.inverse() * comp_point;
        if item.fill.is_some() && item.path.contains(local) {
            Some(item.source)
        } else {
            None
        }
    })
}

/// "Contain" fit into the free canvas area: scale the doc uniformly to fit the
/// window minus the docked panels (right = properties, bottom = dopesheet +
/// transport) and center it there. `reserved_*` are in physical pixels.
fn fit_transform(
    doc: &Document,
    win_w: f64,
    win_h: f64,
    reserved_left: f64,
    reserved_right: f64,
    reserved_top: f64,
    reserved_bottom: f64,
) -> Affine {
    let avail_w = (win_w - reserved_left - reserved_right).max(1.0);
    let avail_h = (win_h - reserved_top - reserved_bottom).max(1.0);
    let scale = (avail_w / doc.width).min(avail_h / doc.height);
    let dx = reserved_left + (avail_w - doc.width * scale) * 0.5;
    let dy = reserved_top + (avail_h - doc.height * scale) * 0.5;
    Affine::translate((dx, dy)) * Affine::scale(scale)
}

/// Panel sizes, in logical points (egui space). Multiply by pixels-per-point to
/// reserve the matching number of physical pixels for the canvas fit.
const TRANSPORT_H: f64 = 56.0;
const PROPS_W: f64 = 260.0;
const TREE_W: f64 = 190.0;
const COMP_H: f64 = 34.0;

/// Composition-settings edits from the top bar. Any `Some` is a new value.
#[derive(Default)]
struct CompEdits {
    width: Option<f64>,
    height: Option<f64>,
    fps: Option<f64>,
    duration: Option<f64>,
}

/// Top composition bar: editable resolution, fps, and duration. These drive the
/// canvas fit, the playback clock, the frame step, and the timeline mapping —
/// so editing them here reshapes the whole comp. Reports edits into `out`.
fn comp_ui(root: &mut egui::Ui, width: f64, height: f64, fps: f64, duration: f64, out: &mut CompEdits) {
    egui::Panel::top("comp")
        .exact_size(COMP_H as f32)
        .show(root, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.add_space(8.0);
                ui.strong("Composition");
                ui.separator();

                ui.label("Size");
                let mut w = width;
                if ui.add(egui::DragValue::new(&mut w).speed(1.0).range(1.0..=16384.0)).changed() {
                    out.width = Some(w);
                }
                ui.label("×");
                let mut h = height;
                if ui.add(egui::DragValue::new(&mut h).speed(1.0).range(1.0..=16384.0)).changed() {
                    out.height = Some(h);
                }
                ui.separator();

                ui.label("FPS");
                let mut f = fps;
                if ui.add(egui::DragValue::new(&mut f).speed(0.5).range(1.0..=240.0)).changed() {
                    out.fps = Some(f);
                }
                ui.separator();

                ui.label("Duration");
                let mut dur = duration;
                if ui
                    .add(egui::DragValue::new(&mut dur).speed(0.1).range(0.1..=3600.0).suffix(" s"))
                    .changed()
                {
                    out.duration = Some(dur);
                }
            });
        });
}

/// What the transport UI reports back after a frame's interaction.
#[derive(Default)]
struct Transport {
    toggle: bool,
    restart: bool,
    /// Frame to scrub to. Snapped by the slider's integer step.
    scrub_to: Option<i64>,
}

/// A snapshot of the selected node's resolved properties at the current time,
/// gathered before the egui closure so the UI never borrows `App`. The `*_anim`
/// flags mark properties backed by a keyframe track (edits auto-key those).
struct NodeInfo {
    name: String,
    id: u64,
    pos: (f64, f64),
    rot: f64,
    scale: (f64, f64),
    opacity: f64,
    fill: Option<[f32; 3]>,
    pos_anim: bool,
    rot_anim: bool,
    scale_anim: bool,
    opacity_anim: bool,
    fill_anim: bool,
}

impl NodeInfo {
    fn resolve(node: &motion_core::Node, t: f64) -> Self {
        let tr = &node.transform;
        let pos = tr.position.resolve(t);
        let scale = tr.scale.resolve(t);
        NodeInfo {
            name: node.name.clone(),
            id: node.id.0,
            pos: (pos.x, pos.y),
            rot: tr.rotation_deg.resolve(t),
            scale: (scale.x, scale.y),
            opacity: tr.opacity.resolve(t),
            fill: node.fill.as_ref().map(|f| {
                let c = f.resolve(t);
                [c.r as f32, c.g as f32, c.b as f32]
            }),
            pos_anim: tr.position.is_animated(),
            rot_anim: tr.rotation_deg.is_animated(),
            scale_anim: tr.scale.is_animated(),
            opacity_anim: tr.opacity.is_animated(),
            fill_anim: node.fill.as_ref().is_some_and(|f| f.is_animated()),
        }
    }
}

/// Edits collected from the properties panel this frame. Any `Some` field is a
/// new value the user dialed in; `None` means untouched.
#[derive(Default)]
struct PropEdits {
    pos_x: Option<f64>,
    pos_y: Option<f64>,
    rot: Option<f64>,
    scale_x: Option<f64>,
    scale_y: Option<f64>,
    opacity: Option<f64>,
    fill: Option<[f32; 3]>,
    // Insert-keyframe-at-playhead requests (the "stopwatch").
    key_pos: bool,
    key_rot: bool,
    key_scale: bool,
    key_opacity: bool,
    key_fill: bool,
}

/// A "stopwatch" toggle: a filled dot when the property is animated, a hollow
/// ring when constant. Clicking it inserts a keyframe at the playhead
/// (promoting a constant to a track). The indicator is *painted* rather than
/// drawn from a glyph, since the circle/diamond glyphs are missing from egui's
/// default font and render as tofu boxes.
fn key_button(ui: &mut egui::Ui, animated: bool) -> bool {
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
struct EaseInfo {
    p1: (f32, f32),
    p2: (f32, f32),
}

/// A CSS-style cubic-bezier editor. Draws the timing curve in a unit square and
/// lets the two control points be dragged. New handles are reported in `out`.
fn ease_editor(ui: &mut egui::Ui, ease: &EaseInfo, out: &mut Option<((f32, f32), (f32, f32))>) {
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
fn properties_ui(
    root: &mut egui::Ui,
    info: &Option<NodeInfo>,
    edits: &mut PropEdits,
    ease: &Option<EaseInfo>,
    ease_out: &mut Option<((f32, f32), (f32, f32))>,
) {
    egui::Panel::right("properties")
        .default_size(260.0)
        .show(root, |ui| {
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
                edits.key_pos |= key_button(ui, n.pos_anim);
                ui.end_row();

                ui.label("Rotation");
                let mut rot = n.rot;
                if ui
                    .add(egui::DragValue::new(&mut rot).speed(0.5).suffix("°"))
                    .changed()
                {
                    edits.rot = Some(rot);
                }
                edits.key_rot |= key_button(ui, n.rot_anim);
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
                edits.key_scale |= key_button(ui, n.scale_anim);
                ui.end_row();

                ui.label("Opacity");
                let mut op = n.opacity;
                if ui
                    .add(egui::Slider::new(&mut op, 0.0..=1.0).show_value(false))
                    .changed()
                {
                    edits.opacity = Some(op);
                }
                edits.key_opacity |= key_button(ui, n.opacity_anim);
                ui.end_row();

                ui.label("Fill");
                if let Some(mut rgb) = n.fill {
                    if ui.color_edit_button_rgb(&mut rgb).changed() {
                        edits.fill = Some(rgb);
                    }
                    edits.key_fill |= key_button(ui, n.fill_anim);
                } else {
                    ui.weak("none");
                    ui.label("");
                }
                ui.end_row();
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
        });
}

/// Build the bottom transport bar. Reads the current time / playing state and
/// writes user intent into `out`; it never touches `App` directly, so it can't
/// collide with the borrows in `render`.
fn transport_ui(
    root: &mut egui::Ui,
    frame: i64,
    last_frame: i64,
    tb: motion_core::Timebase,
    playing: bool,
    out: &mut Transport,
) {
    egui::Panel::bottom("transport")
        .exact_size(TRANSPORT_H as f32)
        .show(root, |ui| {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.add_space(8.0);
                if ui.button(if playing { "Pause" } else { "Play" }).clicked() {
                    out.toggle = true;
                }
                if ui.button("Restart").clicked() {
                    out.restart = true;
                }
                // Frame-domain readout: hh:mm:ss.ff plus the raw frame number,
                // monospaced so the digits don't jitter during playback.
                ui.label(
                    egui::RichText::new(format!(
                        "{}  [{frame}/{last_frame}]",
                        tb.timecode(frame as f64)
                    ))
                    .monospace(),
                );

                // Full-width playhead scrubber. An integer slider, so dragging
                // it can only produce whole frames — snapping for free.
                let mut val = frame.clamp(0, last_frame);
                ui.spacing_mut().slider_width = (ui.available_width() - 16.0).max(60.0);
                let resp = ui.add(
                    egui::Slider::new(&mut val, 0..=last_frame.max(1))
                        .show_value(false)
                        .trailing_fill(true),
                );
                if resp.dragged() || resp.changed() {
                    out.scrub_to = Some(val);
                }
            });
        });
}

/// Which animated property a dopesheet row refers to. Lets the UI report a
/// keyframe drag back to `App` without knowing the property's value type.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum PropKind {
    Position,
    Rotation,
    Scale,
    Opacity,
}

/// One dopesheet row: an animated property and the frames of its keyframes.
struct DopeRow {
    label: &'static str,
    kind: PropKind,
    frames: Vec<i64>,
}

/// Gather the animated properties of a node into dopesheet rows.
fn dope_rows(node: &motion_core::Node) -> Vec<DopeRow> {
    let tr = &node.transform;
    let mut rows = Vec::new();
    // Each property is a distinct value type, so this is spelled out rather
    // than looped.
    if tr.position.is_animated() {
        rows.push(DopeRow { label: "Position", kind: PropKind::Position, frames: tr.position.key_frames() });
    }
    if tr.rotation_deg.is_animated() {
        rows.push(DopeRow { label: "Rotation", kind: PropKind::Rotation, frames: tr.rotation_deg.key_frames() });
    }
    if tr.scale.is_animated() {
        rows.push(DopeRow { label: "Scale", kind: PropKind::Scale, frames: tr.scale.key_frames() });
    }
    if tr.opacity.is_animated() {
        rows.push(DopeRow { label: "Opacity", kind: PropKind::Opacity, frames: tr.opacity.key_frames() });
    }
    rows
}

/// A keyframe's identity within a node: which property, which index.
type KeyRef = (PropKind, usize);

/// The dopesheet's keyframe selection. A `BTreeSet` so iteration order is
/// deterministic and indices come out sorted — which the group-move code
/// below relies on when it batches a selection per property.
type KeySelection = std::collections::BTreeSet<KeyRef>;

/// Bucket a selection into one `(property, sorted indices)` entry per property.
///
/// Relies on `BTreeSet<(PropKind, usize)>` ordering by property first: entries
/// for the same property are therefore *contiguous*, so a single pass that
/// extends the last bucket is enough. If `PropKind`'s `Ord` ever stops being
/// the primary key this silently starts producing duplicate buckets — hence
/// the test.
fn group_selection_by_prop(sel: &KeySelection) -> Vec<(PropKind, Vec<usize>)> {
    let mut out: Vec<(PropKind, Vec<usize>)> = Vec::new();
    for &(kind, index) in sel.iter() {
        match out.last_mut() {
            Some((k, idxs)) if *k == kind => idxs.push(index),
            _ => out.push((kind, vec![index])),
        }
    }
    out
}

/// Read the outgoing-segment handles for a given property + keyframe index.
fn segment_handles_of(node: &MNode, kind: PropKind, index: usize) -> Option<(Handle, Handle)> {
    let tr = &node.transform;
    match kind {
        PropKind::Position => tr.position.segment_handles(index),
        PropKind::Rotation => tr.rotation_deg.segment_handles(index),
        PropKind::Scale => tr.scale.segment_handles(index),
        PropKind::Opacity => tr.opacity.segment_handles(index),
    }
}

/// What the dopesheet reports after a frame: seek, keyframe move, and/or a
/// change to which keyframe is selected.
#[derive(Default)]
struct DopeEdits {
    /// Frame to seek to. Already snapped to the grid.
    seek_to: Option<i64>,
    /// Drag delta in frames, applied to the whole selection as a rigid block.
    move_by: Option<i64>,
    /// A diamond was clicked → make it the selection.
    select_key: Option<KeyRef>,
    /// A diamond was ctrl/shift-clicked → add or remove it from the selection.
    toggle_key: Option<KeyRef>,
    /// Empty track was clicked → clear the keyframe selection.
    clear_selection: bool,
    /// Zoom / pan produced a new visible window.
    set_view: Option<TimelineView>,
}

/// The visible frame window of the timeline. Zoom and pan only ever change
/// this; every frame↔pixel mapping reads it, so the ruler, the keyframes, and
/// the playhead cannot drift out of agreement.
#[derive(Clone, Copy, Debug)]
struct TimelineView {
    /// Leftmost visible frame (fractional — panning is continuous).
    start: f64,
    /// How many frames fit across the track.
    visible: f64,
}

impl TimelineView {
    fn full(last_frame: i64) -> Self {
        Self { start: 0.0, visible: last_frame.max(1) as f64 }
    }

    /// Keep the window inside `0..=last_frame` and never narrower than a few
    /// frames (past that the diamonds are wider than their spacing anyway).
    fn clamped(self, last_frame: i64) -> Self {
        let total = last_frame.max(1) as f64;
        let visible = self.visible.clamp(4.0, total);
        let start = self.start.clamp(0.0, (total - visible).max(0.0));
        Self { start, visible }
    }
}

/// Maps frames to pixels across one track's inset width. Built once from the
/// ruler's rect and shared by every row below it.
#[derive(Clone, Copy)]
struct Axis {
    x0: f32,
    span: f32,
    view: TimelineView,
}

impl Axis {
    fn new(track: egui::Rect, view: TimelineView) -> Self {
        // Inset so keys on the first/last visible frame sit fully inside the
        // track rather than clipped at the edge.
        const PAD: f32 = 8.0;
        let x0 = track.left() + PAD;
        let span = ((track.right() - PAD) - x0).max(1.0);
        Self { x0, span, view }
    }

    fn px_per_frame(&self) -> f32 {
        self.span / self.view.visible as f32
    }

    fn frame_to_x(&self, f: f64) -> f32 {
        self.x0 + ((f - self.view.start) as f32) * self.px_per_frame()
    }

    fn x_to_frame_exact(&self, x: f32) -> f64 {
        self.view.start + ((x - self.x0) / self.px_per_frame()) as f64
    }

    /// Snapped to the grid — this is where clicking and dragging become
    /// frame-exact, regardless of zoom.
    fn x_to_frame(&self, x: f32) -> i64 {
        self.x_to_frame_exact(x).round() as i64
    }
}

/// Choose a ruler tick interval (in frames) that leaves at least `min_px`
/// between labels. Candidates are the 1-2-5-10 frame steps plus whole-second
/// multiples, so labels land on round timecodes once you zoom out.
fn tick_step(px_per_frame: f32, fps: f64, min_px: f32) -> i64 {
    let f = fps.round().max(1.0) as i64;
    let mut cands: Vec<i64> = vec![1, 2, 5, 10];
    for secs in [1i64, 2, 5, 10, 15, 30, 60, 120, 300, 600, 1800, 3600] {
        cands.push(secs * f);
    }
    cands.sort_unstable();
    cands.dedup();
    *cands
        .iter()
        .find(|c| px_per_frame * (**c as f32) >= min_px)
        .unwrap_or_else(|| cands.last().unwrap())
}

/// How hard the timeline should auto-pan for a pointer at `x`, given the
/// track's `left`/`right` edges and the width of the sensitive zone.
///
/// Returns -1..0 in the left zone, 0..1 in the right zone, 0 in the middle;
/// magnitude ramps linearly with depth so a nudge scrolls slowly and pinning
/// the pointer to the edge scrolls fast. Past the edge it saturates at ±1
/// rather than accelerating without bound.
fn edge_pan_intensity(x: f32, left: f32, right: f32, edge: f32) -> f32 {
    if edge <= 0.0 || right <= left {
        return 0.0;
    }
    if x < left + edge {
        -(((left + edge - x) / edge).min(1.0))
    } else if x > right - edge {
        ((x - (right - edge)) / edge).min(1.0)
    } else {
        0.0
    }
}

const DOPESHEET_H: f64 = 178.0;
const RULER_H: f32 = 20.0;
/// Width of the auto-pan zone at each end of the track, in points.
const EDGE_PAN_W: f32 = 36.0;

/// Bottom dopesheet: one row per animated property, keyframes drawn as diamonds
/// along a shared time axis with a playhead line. Click a row's track to seek;
/// click a diamond to select it (Delete removes); drag a diamond to move it.
fn dopesheet_ui(
    root: &mut egui::Ui,
    rows: &[DopeRow],
    frame: f64,
    last_frame: i64,
    tb: motion_core::Timebase,
    view: TimelineView,
    selected_keys: &KeySelection,
    out: &mut DopeEdits,
) {
    egui::Panel::bottom("dopesheet")
        .exact_size(DOPESHEET_H as f32)
        .show(root, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.add_space(8.0);
                ui.strong("Timeline");
                ui.weak(
                    "— ctrl+click to multi-select, drag to move them together, Del removes",
                );
            });
            ui.separator();

            const LABEL_W: f32 = 80.0;
            const ROW_H: f32 = 22.0;
            let accent = egui::Color32::from_rgb(255, 216, 51);
            let playhead_col = egui::Color32::from_rgb(240, 90, 90);
            // Set by any drag on the timeline (ruler scrub or keyframe drag).
            // Gates the edge auto-pan below.
            let mut dragging = false;

            // --- Ruler. Allocated with the same layout as a property row, so
            // its axis geometry is exactly the rows' axis geometry. ---
            let mut axis = None;
            ui.horizontal(|ui| {
                ui.add_space(8.0);
                ui.allocate_ui_with_layout(
                    egui::vec2(LABEL_W, RULER_H),
                    egui::Layout::left_to_right(egui::Align::Center),
                    |ui| {
                        ui.weak("Frame");
                    },
                );
                let (rect, resp) = ui.allocate_exact_size(
                    egui::vec2(ui.available_width() - 8.0, RULER_H),
                    egui::Sense::click_and_drag(),
                );
                let a = Axis::new(rect, view);
                axis = Some(a);
                let painter = ui.painter_at(rect);
                painter.rect_filled(rect, 3.0, egui::Color32::from_gray(28));

                // Ticks. Minor ticks appear only once frames are far enough
                // apart to be legible as individual frames.
                let step = tick_step(a.px_per_frame(), tb.fps(), 58.0);
                let minor = if a.px_per_frame() >= 6.0 { 1 } else { 0 };
                let first = view.start.floor() as i64;
                let last = (view.start + view.visible).ceil() as i64;

                if minor > 0 {
                    let mut f = first;
                    while f <= last {
                        if f % step != 0 {
                            let x = a.frame_to_x(f as f64);
                            painter.line_segment(
                                [
                                    egui::pos2(x, rect.bottom() - 4.0),
                                    egui::pos2(x, rect.bottom()),
                                ],
                                egui::Stroke::new(1.0, egui::Color32::from_gray(58)),
                            );
                        }
                        f += 1;
                    }
                }

                let mut f = (first.div_euclid(step)) * step;
                while f <= last {
                    if f >= 0 {
                        let x = a.frame_to_x(f as f64);
                        painter.line_segment(
                            [egui::pos2(x, rect.top() + 3.0), egui::pos2(x, rect.bottom())],
                            egui::Stroke::new(1.0, egui::Color32::from_gray(110)),
                        );
                        painter.text(
                            egui::pos2(x + 3.0, rect.top() + 1.0),
                            egui::Align2::LEFT_TOP,
                            tb.timecode(f as f64),
                            egui::FontId::monospace(9.0),
                            egui::Color32::from_gray(165),
                        );
                    }
                    f += step;
                }

                // Playhead marker on the ruler.
                let px = a.frame_to_x(frame);
                painter.line_segment(
                    [egui::pos2(px, rect.top()), egui::pos2(px, rect.bottom())],
                    egui::Stroke::new(1.5, playhead_col),
                );

                // Dragging or clicking the ruler scrubs.
                if resp.clicked() || resp.dragged() {
                    if let Some(p) = resp.interact_pointer_pos() {
                        out.seek_to = Some(a.x_to_frame(p.x).clamp(0, last_frame));
                    }
                }
                dragging |= resp.dragged();
            });

            let axis = axis.expect("ruler always allocates the axis");

            // --- Zoom / pan. Scroll anywhere over the panel; zoom keeps the
            // frame under the cursor pinned, which is what makes it feel like
            // zooming rather than jumping. ---
            let panel_rect = ui.max_rect();
            let (scroll, hover) =
                ui.input(|i| (i.smooth_scroll_delta, i.pointer.hover_pos()));
            if let Some(p) = hover.filter(|p| panel_rect.contains(*p)) {
                // egui rewrites a shift+wheel gesture into a *horizontal*
                // scroll, so the shift modifier is already gone by the time we
                // see it — a nonzero x delta is the pan signal, not `shift`.
                // (Trackpad sideways swipes land here too, which is right.)
                let next = if scroll.x != 0.0 {
                    // Pan: one notch moves a tenth of the window.
                    Some(TimelineView {
                        start: view.start - (scroll.x as f64 / 120.0) * view.visible * 0.1,
                        visible: view.visible,
                    })
                } else if scroll.y != 0.0 {
                    let factor = (0.9f64).powf(scroll.y as f64 / 120.0);
                    let anchor = axis.x_to_frame_exact(p.x);
                    let visible = view.visible * factor;
                    // Keep `anchor` under the cursor at the new scale.
                    let ratio = (anchor - view.start) / view.visible.max(1e-9);
                    Some(TimelineView { start: anchor - ratio * visible, visible })
                } else {
                    None
                };
                if let Some(next) = next {
                    out.set_view = Some(next.clamped(last_frame));
                }
            }

            // No early return: the rows loop is a no-op on an empty slice, and
            // returning here would skip the edge auto-pan below (which should
            // still work while scrubbing the ruler with nothing selected).
            if rows.is_empty() {
                ui.add_space(8.0);
                ui.weak("Select a node with animated properties to see its keyframes.");
            }

            for (row_idx, row) in rows.iter().enumerate() {
                ui.horizontal(|ui| {
                    ui.add_space(8.0);
                    ui.allocate_ui_with_layout(
                        egui::vec2(LABEL_W, ROW_H),
                        egui::Layout::left_to_right(egui::Align::Center),
                        |ui| {
                            ui.label(row.label);
                        },
                    );

                    // The track: full remaining width, fixed height.
                    let (track, track_resp) = ui.allocate_exact_size(
                        egui::vec2(ui.available_width() - 8.0, ROW_H),
                        egui::Sense::click(),
                    );
                    let painter = ui.painter_at(track);
                    painter.rect_filled(track, 3.0, egui::Color32::from_gray(32));

                    let frame_to_x = |f: f64| axis.frame_to_x(f);
                    let x_to_frame = |x: f32| axis.x_to_frame(x);

                    // Playhead line.
                    let px = frame_to_x(frame);
                    painter.line_segment(
                        [egui::pos2(px, track.top()), egui::pos2(px, track.bottom())],
                        egui::Stroke::new(1.5, playhead_col),
                    );

                    // Click on empty track → seek and clear the key selection.
                    if track_resp.clicked() {
                        if let Some(p) = track_resp.interact_pointer_pos() {
                            out.seek_to = Some(x_to_frame(p.x).clamp(0, last_frame));
                            out.clear_selection = true;
                        }
                    }

                    // Keyframe diamonds (interactive, drawn on top).
                    let cy = track.center().y;
                    for (key_idx, &kf) in row.frames.iter().enumerate() {
                        let kx = frame_to_x(kf as f64);
                        // Skip keys scrolled out of the window — otherwise
                        // their hit rects stay live outside the visible track.
                        if kx < track.left() - 2.0 || kx > track.right() + 2.0 {
                            continue;
                        }
                        let is_sel = selected_keys.contains(&(row.kind, key_idx));
                        let r = if is_sel { 6.5 } else { 5.0 };
                        let hit = egui::Rect::from_center_size(
                            egui::pos2(kx, cy),
                            egui::vec2(r * 2.4, r * 2.4),
                        );
                        let id = ui.id().with((row_idx, key_idx));
                        let resp = ui.interact(hit, id, egui::Sense::click_and_drag());

                        let col = if is_sel || resp.dragged() || resp.hovered() {
                            egui::Color32::WHITE
                        } else {
                            accent
                        };
                        let border = if is_sel {
                            egui::Stroke::new(2.0, playhead_col)
                        } else {
                            egui::Stroke::new(1.0, egui::Color32::from_gray(16))
                        };
                        // Diamond = a rotated square.
                        let d = [
                            egui::pos2(kx, cy - r),
                            egui::pos2(kx + r, cy),
                            egui::pos2(kx, cy + r),
                            egui::pos2(kx - r, cy),
                        ];
                        painter.add(egui::Shape::convex_polygon(d.to_vec(), col, border));

                        if resp.clicked() {
                            // Ctrl/⌘ or shift extends; a plain click replaces.
                            let mods = ui.input(|i| i.modifiers);
                            if mods.command || mods.shift {
                                out.toggle_key = Some((row.kind, key_idx));
                            } else {
                                out.select_key = Some((row.kind, key_idx));
                            }
                        }
                        if resp.dragged() {
                            dragging = true;
                            if let Some(p) = resp.interact_pointer_pos() {
                                // Dragging an unselected key selects it first,
                                // so the drag acts on what's under the cursor.
                                if !is_sel {
                                    out.select_key = Some((row.kind, key_idx));
                                }
                                // Report a *delta* from this key's current
                                // frame, so the whole selection can move as a
                                // block. Recomputed each frame, so a clamped
                                // drag catches up once room appears.
                                let target = x_to_frame(p.x).clamp(0, last_frame);
                                let delta = target - kf;
                                if delta != 0 {
                                    out.move_by = Some(delta);
                                }
                            }
                        }
                    }
                });
            }

            // --- Edge auto-pan. While dragging (scrubbing the ruler or moving
            // a keyframe), holding the pointer near either end of the track
            // scrolls the window that way — so you can drag a key past the
            // visible range without letting go. Deliberately drag-only: doing
            // this on plain hover would scroll the timeline out from under the
            // pointer whenever it drifted near an edge. ---
            if dragging {
                if let Some(p) = ui.input(|i| i.pointer.latest_pos()) {
                    let intensity = edge_pan_intensity(
                        p.x,
                        axis.x0,
                        axis.x0 + axis.span,
                        EDGE_PAN_W,
                    );
                    if intensity != 0.0 {
                        // Time-based so the speed doesn't depend on frame rate;
                        // clamped in case a slow frame produces a huge dt.
                        let dt = (ui.input(|i| i.stable_dt) as f64).min(0.05);
                        let delta = intensity as f64 * view.visible * 0.8 * dt;
                        out.set_view = Some(
                            TimelineView { start: view.start + delta, visible: view.visible }
                                .clamped(last_frame),
                        );
                        // Redraw is event-driven, so without this the pan stops
                        // the moment the pointer stops moving.
                        ui.ctx().request_repaint();
                    }
                }
            }
        });
}

/// A flattened scene-tree row for the layers panel.
struct TreeRow {
    id: NodeId,
    name: String,
    depth: usize,
    is_group: bool,
}

/// Flatten the scene graph depth-first into indented rows.
fn tree_rows(node: &motion_core::Node, depth: usize, out: &mut Vec<TreeRow>) {
    out.push(TreeRow {
        id: node.id,
        name: node.name.clone(),
        depth,
        is_group: node.shape.is_none(),
    });
    for c in &node.children {
        tree_rows(c, depth + 1, out);
    }
}

/// A shape the "add" tools can create.
#[derive(Clone, Copy)]
enum NewShape {
    Rect,
    Ellipse,
    Group,
}

/// What the layers panel reports: selection, reorder, add, and/or delete.
#[derive(Default)]
struct TreeEdits {
    select: Option<NodeId>,
    /// (node, delta) — move among siblings (-1 up, +1 down).
    reorder: Option<(NodeId, i32)>,
    add: Option<NewShape>,
    delete: Option<NodeId>,
    save: bool,
    load: bool,
}

/// Left layers panel: the scene graph as a clickable, indented list. Clicking a
/// row selects that node; the ▲/▼ buttons restack it among its siblings.
fn tree_ui(root: &mut egui::Ui, rows: &[TreeRow], selected: Option<NodeId>, out: &mut TreeEdits) {
    egui::Panel::left("layers")
        .exact_size(TREE_W as f32)
        .show(root, |ui| {
            ui.add_space(8.0);
            ui.heading("Layers");
            ui.horizontal(|ui| {
                if ui.button("Save…").clicked() {
                    out.save = true;
                }
                if ui.button("Load…").clicked() {
                    out.load = true;
                }
            });
            ui.horizontal(|ui| {
                if ui.button("+ Rect").clicked() {
                    out.add = Some(NewShape::Rect);
                }
                if ui.button("+ Ellipse").clicked() {
                    out.add = Some(NewShape::Ellipse);
                }
                if ui.button("+ Group").clicked() {
                    out.add = Some(NewShape::Group);
                }
            });
            ui.weak("Adds into the selected node, else the root.");
            ui.separator();
            for row in rows {
                ui.horizontal(|ui| {
                    ui.add_space(6.0 + row.depth as f32 * 14.0);
                    let icon = if row.is_group { "▶" } else { "•" };
                    let label = format!("{icon} {}", row.name);
                    if ui
                        .selectable_label(selected == Some(row.id), label)
                        .clicked()
                    {
                        out.select = Some(row.id);
                    }
                    // Reorder + delete (not meaningful for the root).
                    if row.depth > 0 {
                        if ui.small_button("▲").clicked() {
                            out.reorder = Some((row.id, -1));
                        }
                        if ui.small_button("▼").clicked() {
                            out.reorder = Some((row.id, 1));
                        }
                        if ui.small_button("✕").clicked() {
                            out.delete = Some(row.id);
                        }
                    }
                });
            }
        });
}

enum RenderState {
    Active {
        surface: RenderSurface<'static>,
        window: Arc<Window>,
    },
    Suspended(Option<Arc<Window>>),
}

struct App {
    context: RenderContext,
    /// One vello renderer per wgpu device, indexed by `RenderSurface::dev_id`.
    renderers: Vec<Option<Renderer>>,
    state: RenderState,
    vscene: VScene,
    doc: Document,

    // egui (created lazily in `resumed`, once we have a window + device).
    egui_ctx: egui::Context,
    egui_state: Option<egui_winit::State>,
    egui_renderer: Option<egui_wgpu::Renderer>,

    // Playback clock.
    playing: bool,
    anchor: Instant,
    paused_t: f64,

    // Selection / picking (physical-pixel coordinates).
    cursor: (f64, f64),
    pending_pick: Option<(f64, f64)>,
    selected: Option<NodeId>,
    /// The keyframes selected in the dopesheet. Empty = nothing selected.
    selected_keys: KeySelection,
    /// The timeline's visible frame window (zoom / pan).
    view: TimelineView,
    /// Next unused node id, for shapes created in-app.
    next_id: u64,
}

/// The largest node id in a subtree, for seeding the id counter.
fn max_id(node: &MNode) -> u64 {
    node.children.iter().fold(node.id.0, |m, c| m.max(max_id(c)))
}

impl App {
    fn new(doc: Document) -> Self {
        let next_id = max_id(&doc.root) + 1;
        let view = TimelineView::full(doc.duration_frames());
        Self {
            context: RenderContext::new(),
            renderers: Vec::new(),
            state: RenderState::Suspended(None),
            vscene: VScene::new(),
            doc,
            egui_ctx: egui::Context::default(),
            egui_state: None,
            egui_renderer: None,
            playing: true,
            anchor: Instant::now(),
            paused_t: 0.0,
            cursor: (0.0, 0.0),
            pending_pick: None,
            selected: None,
            selected_keys: KeySelection::new(),
            view,
            next_id,
        }
    }

    /// Current looped position on the wall clock, in seconds. Continuous — this
    /// is the clock, not the frame grid. Use `current_frame` / `current_time`
    /// for anything that evaluates or displays.
    fn raw_time(&self) -> f64 {
        let raw = if self.playing {
            self.anchor.elapsed().as_secs_f64()
        } else {
            self.paused_t
        };
        if self.doc.duration > 0.0 {
            raw.rem_euclid(self.doc.duration)
        } else {
            raw
        }
    }

    /// The frame the playhead currently sits on.
    ///
    /// Floors rather than rounds: a frame must be *held* for its full duration,
    /// the way a projector does. Rounding would show frame N starting half a
    /// frame early and is the classic off-by-half in playback code.
    fn current_frame(&self) -> i64 {
        let tb = self.doc.timebase();
        tb.seconds_to_frames_exact(self.raw_time()).floor() as i64
    }

    /// Current document time in seconds, **snapped to the frame grid**. This is
    /// what the canvas evaluates at, so playback actually steps at `doc.fps`
    /// instead of running at the monitor's refresh rate.
    fn current_time(&self) -> f64 {
        self.doc.timebase().frames_to_seconds(self.current_frame() as f64)
    }

    /// Seek to a frame, wrapping around the composition length. All seeking
    /// goes through here, so the playhead can only ever land on the grid.
    fn seek_frame(&mut self, frame: i64) {
        let total = self.doc.duration_frames().max(1);
        let frame = frame.rem_euclid(total);
        self.seek(self.doc.timebase().frames_to_seconds(frame as f64));
    }

    fn seek(&mut self, t: f64) {
        let t = t.rem_euclid(self.doc.duration.max(f64::MIN_POSITIVE));
        self.paused_t = t;
        self.anchor = Instant::now() - std::time::Duration::from_secs_f64(t);
    }

    fn toggle_play(&mut self) {
        if self.playing {
            self.paused_t = self.current_time();
            self.playing = false;
        } else {
            self.anchor = Instant::now() - std::time::Duration::from_secs_f64(self.paused_t);
            self.playing = true;
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        let RenderState::Suspended(cached) = &mut self.state else {
            return;
        };
        let window = cached.take().unwrap_or_else(|| {
            let attrs = Window::default_attributes()
                .with_title("Pain By Choice")
                .with_inner_size(winit::dpi::LogicalSize::new(1280.0, 720.0));
            Arc::new(event_loop.create_window(attrs).unwrap())
        });

        let size = window.inner_size();
        let surface = pollster::block_on(self.context.create_surface(
            window.clone(),
            size.width.max(1),
            size.height.max(1),
            wgpu::PresentMode::AutoVsync,
        ))
        .expect("create surface");

        while self.renderers.len() <= surface.dev_id {
            self.renderers.push(None);
        }
        let device = &self.context.devices[surface.dev_id].device;
        if self.renderers[surface.dev_id].is_none() {
            self.renderers[surface.dev_id] = Some(
                Renderer::new(
                    device,
                    RendererOptions {
                        use_cpu: false,
                        antialiasing_support: AaSupport::area_only(),
                        num_init_threads: NonZeroUsize::new(1),
                        pipeline_cache: None,
                    },
                )
                .expect("create renderer"),
            );
        }

        // egui: input plumbing + its own wgpu renderer targeting the swapchain.
        if self.egui_state.is_none() {
            self.egui_state = Some(egui_winit::State::new(
                self.egui_ctx.clone(),
                egui::ViewportId::ROOT,
                &window,
                Some(window.scale_factor() as f32),
                Some(winit::window::Theme::Dark),
                None,
            ));
        }
        self.egui_renderer = Some(egui_wgpu::Renderer::new(
            device,
            surface.format,
            egui_wgpu::RendererOptions::default(),
        ));

        self.state = RenderState::Active { surface, window };
    }

    fn suspended(&mut self, _event_loop: &ActiveEventLoop) {
        if let RenderState::Active { window, .. } = &self.state {
            self.state = RenderState::Suspended(Some(window.clone()));
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _id: WindowId,
        event: WindowEvent,
    ) {
        let window = match &self.state {
            RenderState::Active { window, .. } => window.clone(),
            RenderState::Suspended(_) => return,
        };

        // Let egui see every event first; if it wants the event exclusively
        // (e.g. dragging the scrubber), don't also treat it as a canvas input.
        let consumed = self
            .egui_state
            .as_mut()
            .map(|st| st.on_window_event(&window, &event).consumed)
            .unwrap_or(false);

        // Whether the pointer is over any egui panel/widget. Combined with
        // `consumed` this decides if a click belongs to the UI rather than the
        // canvas. Both read egui's last frame, so we keep that frame fresh by
        // repainting on pointer motion (see CursorMoved below).
        let over_ui = consumed || self.egui_ctx.is_pointer_over_egui();

        match event {
            WindowEvent::CloseRequested => event_loop.exit(),

            WindowEvent::Resized(size) => {
                if let RenderState::Active { surface, .. } = &mut self.state {
                    self.context
                        .resize_surface(surface, size.width.max(1), size.height.max(1));
                }
                window.request_redraw();
            }

            WindowEvent::KeyboardInput { event, .. }
                if !consumed && event.state == ElementState::Pressed =>
            {
                match event.logical_key {
                    Key::Named(NamedKey::Space) => self.toggle_play(),
                    Key::Named(NamedKey::Escape) => event_loop.exit(),
                    Key::Named(NamedKey::ArrowRight) => {
                        self.playing = false;
                        self.seek_frame(self.current_frame() + 1);
                    }
                    Key::Named(NamedKey::ArrowLeft) => {
                        self.playing = false;
                        self.seek_frame(self.current_frame() - 1);
                    }
                    Key::Character(ref s) if s == "r" || s == "R" => self.seek_frame(0),
                    Key::Named(NamedKey::Delete) | Key::Named(NamedKey::Backspace) => {
                        self.delete_selected_keys();
                    }
                    _ => {}
                }
                window.request_redraw();
            }

            WindowEvent::CursorMoved { position, .. } => {
                self.cursor = (position.x, position.y);
                // Repaint so egui's hover/consumed state stays current even
                // while paused — otherwise the next click is judged against a
                // stale frame and canvas picking fires over the UI.
                window.request_redraw();
            }

            WindowEvent::MouseInput { state, button, .. }
                if !over_ui
                    && state == ElementState::Pressed
                    && button == winit::event::MouseButton::Left =>
            {
                // Defer the hit-test to render(), where the evaluated scene and
                // fit transform for this exact frame are in hand.
                self.pending_pick = Some(self.cursor);
                window.request_redraw();
            }

            WindowEvent::RedrawRequested => {
                self.render(&window);
                // Keep animating while playing; when paused, egui still asks
                // for repaints while the pointer interacts with the UI.
                if self.playing || self.egui_ctx.has_requested_repaint() {
                    window.request_redraw();
                }
            }

            // Any other event (mouse move/click for egui) → repaint.
            _ => window.request_redraw(),
        }
    }
}

impl App {
    /// Write the panel's edits into the selected node. Returns whether anything
    /// changed. An edit to a constant overwrites it; an edit to an animated
    /// property sets a keyframe on `frame` (via `Value::set_at`).
    fn apply_edits(&mut self, frame: i64, e: &PropEdits) -> bool {
        let t = frame as f64;
        let Some(id) = self.selected else {
            return false;
        };
        let Some(node) = self.doc.root.find_mut(id) else {
            return false;
        };
        let tr = &mut node.transform;
        let mut changed = false;

        if e.pos_x.is_some() || e.pos_y.is_some() {
            let cur = tr.position.resolve(t);
            let v = Vec2::new(e.pos_x.unwrap_or(cur.x), e.pos_y.unwrap_or(cur.y));
            tr.position.set_at(frame, v);
            changed = true;
        }
        if let Some(r) = e.rot {
            tr.rotation_deg.set_at(frame, r);
            changed = true;
        }
        if e.scale_x.is_some() || e.scale_y.is_some() {
            let cur = tr.scale.resolve(t);
            let v = Vec2::new(e.scale_x.unwrap_or(cur.x), e.scale_y.unwrap_or(cur.y));
            tr.scale.set_at(frame, v);
            changed = true;
        }
        if let Some(o) = e.opacity {
            tr.opacity.set_at(frame, o);
            changed = true;
        }
        if let Some(rgb) = e.fill {
            if let Some(fill) = node.fill.as_mut() {
                fill.set_at(frame, MColor::rgb(rgb[0] as f64, rgb[1] as f64, rgb[2] as f64));
                changed = true;
            }
        }

        // Stopwatch clicks: insert a keyframe at the playhead (promoting a
        // constant to a track the first time).
        if e.key_pos {
            tr.position.insert_key(frame);
            changed = true;
        }
        if e.key_rot {
            tr.rotation_deg.insert_key(frame);
            changed = true;
        }
        if e.key_scale {
            tr.scale.insert_key(frame);
            changed = true;
        }
        if e.key_opacity {
            tr.opacity.insert_key(frame);
            changed = true;
        }
        if e.key_fill {
            if let Some(fill) = node.fill.as_mut() {
                fill.insert_key(frame);
                changed = true;
            }
        }
        changed
    }

    /// Set the easing handles for the selected keyframe's outgoing segment.
    fn set_ease(&mut self, kind: PropKind, index: usize, p1: (f32, f32), p2: (f32, f32)) -> bool {
        let Some(id) = self.selected else {
            return false;
        };
        let Some(node) = self.doc.root.find_mut(id) else {
            return false;
        };
        let out = Handle::new(p1.0 as f64, p1.1 as f64);
        let next_in = Handle::new(p2.0 as f64, p2.1 as f64);
        let tr = &mut node.transform;
        match kind {
            PropKind::Position => tr.position.set_segment_handles(index, out, next_in),
            PropKind::Rotation => tr.rotation_deg.set_segment_handles(index, out, next_in),
            PropKind::Scale => tr.scale.set_segment_handles(index, out, next_in),
            PropKind::Opacity => tr.opacity.set_segment_handles(index, out, next_in),
        }
        true
    }

    /// Remove every dopesheet-selected keyframe (Delete). A track keeps at
    /// least one key, so this may be a partial no-op.
    fn delete_selected_keys(&mut self) -> bool {
        let Some(id) = self.selected else {
            return false;
        };
        if self.selected_keys.is_empty() {
            return false;
        }
        let Some(node) = self.doc.root.find_mut(id) else {
            return false;
        };
        let tr = &mut node.transform;
        // Descending index order: removing a key shifts every later index
        // down, so deleting from the back keeps the remaining ones valid.
        for &(kind, index) in self.selected_keys.iter().rev() {
            match kind {
                PropKind::Position => tr.position.remove_key(index),
                PropKind::Rotation => tr.rotation_deg.remove_key(index),
                PropKind::Scale => tr.scale.remove_key(index),
                PropKind::Opacity => tr.opacity.remove_key(index),
            }
        }
        self.selected_keys.clear();
        true
    }

    /// Move every selected keyframe by `delta` frames as one rigid block.
    ///
    /// Each property is a separate `Track`, so the limits are intersected
    /// across all of them *before* anything moves — otherwise a track that
    /// clamps early would slide out of sync with the others and the selection
    /// would deform instead of translating.
    fn move_selected_keys(&mut self, delta: i64) -> bool {
        let Some(id) = self.selected else {
            return false;
        };
        if self.selected_keys.is_empty() || delta == 0 {
            return false;
        }
        let Some(node) = self.doc.root.find_mut(id) else {
            return false;
        };

        let per_prop = group_selection_by_prop(&self.selected_keys);
        let tr = &node.transform;

        // Intersect the allowed delta across every affected track.
        let (mut lo, mut hi) = (i64::MIN, i64::MAX);
        for (kind, idxs) in &per_prop {
            let limits = match kind {
                PropKind::Position => tr.position.move_keys_limits(idxs),
                PropKind::Rotation => tr.rotation_deg.move_keys_limits(idxs),
                PropKind::Scale => tr.scale.move_keys_limits(idxs),
                PropKind::Opacity => tr.opacity.move_keys_limits(idxs),
            };
            if let Some((l, h)) = limits {
                lo = lo.max(l);
                hi = hi.min(h);
            }
        }
        if lo > hi {
            return false; // the block is boxed in somewhere
        }
        // Also keep the whole selection inside the composition.
        let last = self.doc.duration_frames().max(1);
        let node = self.doc.root.find_mut(id).expect("checked above");
        let tr = &mut node.transform;
        let mut min_frame = i64::MAX;
        let mut max_frame = i64::MIN;
        for (kind, idxs) in &per_prop {
            let frames = match kind {
                PropKind::Position => tr.position.key_frames(),
                PropKind::Rotation => tr.rotation_deg.key_frames(),
                PropKind::Scale => tr.scale.key_frames(),
                PropKind::Opacity => tr.opacity.key_frames(),
            };
            for &i in idxs {
                if let Some(&f) = frames.get(i) {
                    min_frame = min_frame.min(f);
                    max_frame = max_frame.max(f);
                }
            }
        }
        if min_frame <= max_frame {
            lo = lo.max(-min_frame);
            hi = hi.min(last - max_frame);
        }
        if lo > hi {
            return false;
        }

        let applied = delta.clamp(lo, hi);
        if applied == 0 {
            return false;
        }
        for (kind, idxs) in &per_prop {
            match kind {
                PropKind::Position => tr.position.move_keys(idxs, applied),
                PropKind::Rotation => tr.rotation_deg.move_keys(idxs, applied),
                PropKind::Scale => tr.scale.move_keys(idxs, applied),
                PropKind::Opacity => tr.opacity.move_keys(idxs, applied),
            };
        }
        true
    }

    /// Create a new shape/group, parent it under the selected node (or the
    /// root), select it, and return `true` (the doc changed).
    fn add_node(&mut self, kind: NewShape) -> bool {
        let id = self.next_id;
        self.next_id += 1;

        let center = Vec2::new(self.doc.width / 2.0, self.doc.height / 2.0);
        let at_center = Transform {
            position: Value::constant(center),
            ..Transform::default()
        };
        // A rotating palette so new shapes are visually distinct.
        let palette = [
            MColor::rgb(0.90, 0.25, 0.25),
            MColor::rgb(0.25, 0.65, 0.95),
            MColor::rgb(0.35, 0.80, 0.45),
            MColor::rgb(0.95, 0.75, 0.20),
            MColor::rgb(0.70, 0.45, 0.90),
        ];
        let fill = palette[(id as usize) % palette.len()];

        let node = match kind {
            NewShape::Rect => MNode::shape(
                id,
                format!("Rect {id}"),
                MShape::Rect {
                    size: Value::constant(Vec2::new(200.0, 200.0)),
                    radius: Value::constant(0.0),
                },
            )
            .with_fill(fill)
            .with_transform(at_center),
            NewShape::Ellipse => MNode::shape(
                id,
                format!("Ellipse {id}"),
                MShape::Ellipse { size: Value::constant(Vec2::new(200.0, 200.0)) },
            )
            .with_fill(fill)
            .with_transform(at_center),
            NewShape::Group => MNode::group(id, format!("Group {id}")).with_transform(at_center),
        };

        // Parent under the selected node if it still exists, else the root.
        let target = self.selected.filter(|sid| self.doc.root.find(*sid).is_some());
        let parent = match target {
            Some(sid) => self.doc.root.find_mut(sid).unwrap(),
            None => &mut self.doc.root,
        };
        parent.children.push(node);

        self.selected = Some(NodeId(id));
        self.selected_keys.clear();
        true
    }

    /// Serialize the document to a `.pbc` (JSON) file chosen via a native
    /// save dialog. The document already derives serde, so this is the whole
    /// file format.
    fn save(&self) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("Pain By Choice", &["pbc", "json"])
            .set_file_name("project.pbc")
            .save_file()
        else {
            return;
        };
        match serde_json::to_string_pretty(&self.doc) {
            Ok(json) => {
                if let Err(e) = std::fs::write(&path, json) {
                    eprintln!("save failed: {e}");
                }
            }
            Err(e) => eprintln!("serialize failed: {e}"),
        }
    }

    /// Load a `.pbc` document via a native open dialog, replacing the current
    /// one. Returns whether the document changed. Selection and the id counter
    /// are reset to match the loaded tree.
    fn load(&mut self) -> bool {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("Pain By Choice", &["pbc", "json"])
            .pick_file()
        else {
            return false;
        };
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("read failed: {e}");
                return false;
            }
        };
        match serde_json::from_str::<Document>(&text) {
            Ok(mut doc) => {
                // Pre-frame-grid docs stored keyframes as float seconds; this
                // converts them using the doc's own fps. No-op on new files.
                doc.migrate();
                self.next_id = max_id(&doc.root) + 1;
                self.view = TimelineView::full(doc.duration_frames());
                self.doc = doc;
                self.selected = None;
                self.selected_keys.clear();
                self.seek_frame(0);
                true
            }
            Err(e) => {
                eprintln!("parse failed: {e}");
                false
            }
        }
    }

    /// Evaluate + rasterize the current frame, then composite the egui overlay.
    fn render(&mut self, window: &Window) {
        // The whole render path is in the frame domain; seconds only ever
        // appear in the timecode string.
        let frame = self.current_frame();
        let t = frame as f64;
        let last_frame = self.doc.duration_frames().max(1);
        let scene = evaluate(&self.doc, t);
        for (id, msg) in &scene.warnings {
            eprintln!("warning [node {}]: {msg}", id.0);
        }

        let size = window.inner_size();
        // egui panel sizes are in points; convert to physical pixels for the fit.
        let ppp = window.scale_factor();
        let fit = fit_transform(
            &self.doc,
            size.width as f64,
            size.height as f64,
            TREE_W * ppp,
            PROPS_W * ppp,
            COMP_H * ppp,
            (TRANSPORT_H + DOPESHEET_H) * ppp,
        );

        // Resolve any pending click into a selection (or a deselect). Changing
        // the selected node invalidates any keyframe selection.
        if let Some(px) = self.pending_pick.take() {
            let picked = pick(&scene, fit, px);
            if picked != self.selected {
                self.selected = picked;
                self.selected_keys.clear();
            }
        }

        self.vscene = to_vello(&scene, fit, (self.doc.width, self.doc.height), self.selected);

        // Snapshot the selected node's properties before the UI closure so the
        // egui code borrows a plain struct, never `self`.
        let sel_node = self.selected.and_then(|id| self.doc.root.find(id));
        let sel_info = sel_node.map(|node| NodeInfo::resolve(node, t));
        let rows = sel_node.map(dope_rows).unwrap_or_default();

        // The selected keyframe's outgoing easing segment, if it has one.
        // Only meaningful for a single key — a segment belongs to one key, and
        // there's no sensible "the" curve for a multi-key selection.
        let single_key = if self.selected_keys.len() == 1 {
            self.selected_keys.iter().next().copied()
        } else {
            None
        };
        let ease_info = match (sel_node, single_key) {
            (Some(node), Some((kind, idx))) => {
                segment_handles_of(node, kind, idx).map(|(p1, p2)| EaseInfo {
                    p1: (p1.x as f32, p1.y as f32),
                    p2: (p2.x as f32, p2.y as f32),
                })
            }
            _ => None,
        };

        // Flatten the scene tree for the layers panel.
        let mut tree = Vec::new();
        tree_rows(&self.doc.root, 0, &mut tree);

        // --- Run egui for this frame (no `self` borrow leaks into the UI). ---
        let raw_input = self.egui_state.as_mut().unwrap().take_egui_input(window);
        let duration = self.doc.duration;
        let timebase = self.doc.timebase();
        let view = self.view;
        let playing = self.playing;
        let mut transport = Transport::default();
        let mut edits = PropEdits::default();
        let mut dope = DopeEdits::default();
        let mut tree_edits = TreeEdits::default();
        let selected_keys = std::mem::take(&mut self.selected_keys);
        let selected_node = self.selected;
        let mut ease_out: Option<((f32, f32), (f32, f32))> = None;
        let mut comp = CompEdits::default();
        let (doc_w, doc_h, doc_fps) = (self.doc.width, self.doc.height, self.doc.fps);
        let full_output = self.egui_ctx.run_ui(raw_input, |ui| {
            comp_ui(ui, doc_w, doc_h, doc_fps, duration, &mut comp);
            tree_ui(ui, &tree, selected_node, &mut tree_edits);
            transport_ui(ui, frame, last_frame, timebase, playing, &mut transport);
            dopesheet_ui(ui, &rows, t, last_frame, timebase, view, &selected_keys, &mut dope);
            properties_ui(ui, &sel_info, &mut edits, &ease_info, &mut ease_out);
        });

        // Composition settings.
        if let Some(w) = comp.width {
            self.doc.width = w.max(1.0);
        }
        if let Some(h) = comp.height {
            self.doc.height = h.max(1.0);
        }
        if let Some(f) = comp.fps {
            self.doc.fps = f.max(1.0);
        }
        if let Some(d) = comp.duration {
            self.doc.duration = d.max(0.1);
        }
        // fps/duration changes resize the frame axis under the view, so the
        // window may now hang past the end of the comp.
        if comp.fps.is_some() || comp.duration.is_some() {
            self.view = self.view.clamped(self.doc.duration_frames());
        }

        // Layers panel: selection + reorder.
        if let Some(id) = tree_edits.select {
            if Some(id) != self.selected {
                self.selected = Some(id);
                self.selected_keys.clear();
            }
        }

        // Zoom / pan from the timeline.
        if let Some(v) = dope.set_view {
            self.view = v;
        }

        // Keyframe selection changes from the dopesheet. The set was moved out
        // of `self` before the UI ran (so the closure couldn't borrow `App`);
        // put it back, then apply this frame's changes to it.
        self.selected_keys = selected_keys;
        if let Some(k) = dope.select_key {
            // Plain click: this key becomes the whole selection.
            self.selected_keys.clear();
            self.selected_keys.insert(k);
        } else if let Some(k) = dope.toggle_key {
            // Ctrl/shift click: add, or remove if already in.
            if !self.selected_keys.remove(&k) {
                self.selected_keys.insert(k);
            }
        } else if dope.clear_selection {
            self.selected_keys.clear();
        }
        // Apply the UI's intent to the playback clock.
        if transport.toggle {
            self.toggle_play();
        }
        if transport.restart {
            self.seek_frame(0);
        }
        if let Some(nf) = transport.scrub_to.or(dope.seek_to) {
            self.playing = false;
            self.seek_frame(nf);
        }

        // Apply property edits + keyframe drags to the selected node, then
        // re-evaluate so the change is visible on this very frame.
        let mut dirty = self.apply_edits(frame, &edits);
        if let Some(delta) = dope.move_by {
            dirty |= self.move_selected_keys(delta);
        }
        // Easing edits target the single selected key (the editor only appears
        // when exactly one is selected).
        let single_key = if self.selected_keys.len() == 1 {
            self.selected_keys.iter().next().copied()
        } else {
            None
        };
        if let (Some((kind, idx)), Some((p1, p2))) = (single_key, ease_out) {
            dirty |= self.set_ease(kind, idx, p1, p2);
        }
        if let Some((id, delta)) = tree_edits.reorder {
            dirty |= self.doc.root.reorder_child(id, delta);
        }
        if let Some(kind) = tree_edits.add {
            dirty |= self.add_node(kind);
        }
        if let Some(id) = tree_edits.delete {
            self.doc.root.remove(id);
            if self.selected == Some(id) {
                self.selected = None;
                self.selected_keys.clear();
            }
            dirty = true;
        }
        if tree_edits.save {
            self.save();
        }
        if tree_edits.load {
            dirty |= self.load();
        }
        if dirty {
            let scene = evaluate(&self.doc, t);
            self.vscene = to_vello(&scene, fit, (self.doc.width, self.doc.height), self.selected);
        }

        self.egui_state
            .as_mut()
            .unwrap()
            .handle_platform_output(window, full_output.platform_output);
        let ppp = self.egui_ctx.pixels_per_point();
        let paint_jobs = self.egui_ctx.tessellate(full_output.shapes, ppp);
        let tex_delta = full_output.textures_delta;

        // --- GPU (disjoint field borrows only past this point). ---
        let RenderState::Active { surface, .. } = &mut self.state else {
            return;
        };

        use wgpu::CurrentSurfaceTexture as Cst;
        let surface_texture = match surface.surface.get_current_texture() {
            Cst::Success(tx) | Cst::Suboptimal(tx) => tx,
            _ => {
                window.request_redraw();
                return;
            }
        };

        let device_handle = &self.context.devices[surface.dev_id];
        let vrenderer = self.renderers[surface.dev_id].as_mut().unwrap();
        vrenderer
            .render_to_texture(
                &device_handle.device,
                &device_handle.queue,
                &self.vscene,
                &surface.target_view,
                &vello::RenderParams {
                    base_color: Color::new([0.08, 0.09, 0.11, 1.0]),
                    width: surface.config.width,
                    height: surface.config.height,
                    antialiasing_method: AaConfig::Area,
                },
            )
            .expect("render");

        let egui_renderer = self.egui_renderer.as_mut().unwrap();
        for (id, delta) in &tex_delta.set {
            egui_renderer.update_texture(&device_handle.device, &device_handle.queue, *id, delta);
        }
        let screen = egui_wgpu::ScreenDescriptor {
            size_in_pixels: [surface.config.width, surface.config.height],
            pixels_per_point: ppp,
        };

        let mut encoder = device_handle
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("frame") });
        let user_buffers = egui_renderer.update_buffers(
            &device_handle.device,
            &device_handle.queue,
            &mut encoder,
            &paint_jobs,
            &screen,
        );

        let surface_view = surface_texture
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        // 1) vello target -> swapchain (overwrites the whole surface).
        surface
            .blitter
            .copy(&device_handle.device, &mut encoder, &surface.target_view, &surface_view);

        // 2) egui overlay composited on top.
        {
            let mut rpass = encoder
                .begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("egui"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &surface_view,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Load,
                            store: wgpu::StoreOp::Store,
                        },
                        depth_slice: None,
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                })
                .forget_lifetime();
            egui_renderer.render(&mut rpass, &paint_jobs, &screen);
        }

        for id in &tex_delta.free {
            egui_renderer.free_texture(id);
        }

        device_handle
            .queue
            .submit(user_buffers.into_iter().chain([encoder.finish()]));
        surface_texture.present();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_axis(view: TimelineView) -> Axis {
        // 8px pad each side → a 400px usable span.
        Axis::new(
            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(416.0, 20.0)),
            view,
        )
    }

    #[test]
    fn axis_round_trips_frames_through_pixels() {
        let a = test_axis(TimelineView { start: 0.0, visible: 100.0 });
        for f in [0i64, 1, 37, 99, 100] {
            assert_eq!(a.x_to_frame(a.frame_to_x(f as f64)), f, "frame {f}");
        }
    }

    #[test]
    fn axis_round_trips_when_panned_and_zoomed() {
        let a = test_axis(TimelineView { start: 240.0, visible: 12.0 });
        for f in [240i64, 243, 251, 252] {
            assert_eq!(a.x_to_frame(a.frame_to_x(f as f64)), f, "frame {f}");
        }
    }

    #[test]
    fn x_to_frame_snaps_to_the_nearest_frame() {
        let a = test_axis(TimelineView { start: 0.0, visible: 10.0 });
        // 40px per frame here, so a third of the way past frame 2 still snaps
        // back to 2, and two-thirds snaps up to 3.
        let x2 = a.frame_to_x(2.0);
        assert_eq!(a.x_to_frame(x2 + 13.0), 2);
        assert_eq!(a.x_to_frame(x2 + 27.0), 3);
    }

    #[test]
    fn zoom_keeps_the_anchored_frame_under_the_cursor() {
        // This is the property that makes zooming feel like zooming: whatever
        // frame is under the pointer must not move while the scale changes.
        let view = TimelineView { start: 0.0, visible: 120.0 };
        let a = test_axis(view);
        let cursor_x = a.frame_to_x(90.0);
        let anchor = a.x_to_frame_exact(cursor_x);

        for factor in [0.5f64, 0.8, 1.25, 2.0] {
            let visible = view.visible * factor;
            let ratio = (anchor - view.start) / view.visible;
            let next = TimelineView { start: anchor - ratio * visible, visible };
            let moved = test_axis(next).frame_to_x(anchor);
            assert!(
                (moved - cursor_x).abs() < 0.5,
                "factor {factor}: anchor drifted {cursor_x} -> {moved}"
            );
        }
    }

    #[test]
    fn view_clamp_keeps_the_window_inside_the_comp() {
        let v = TimelineView { start: -50.0, visible: 5000.0 }.clamped(120);
        assert_eq!(v.start, 0.0);
        assert_eq!(v.visible, 120.0, "cannot show more than the comp");

        // Panned past the end: slides back so the window ends at the last frame.
        let v = TimelineView { start: 900.0, visible: 20.0 }.clamped(120);
        assert!((v.start + v.visible - 120.0).abs() < 1e-9, "start = {}", v.start);

        // Zoomed in absurdly far: floored, not zero or negative.
        let v = TimelineView { start: 10.0, visible: 0.0001 }.clamped(120);
        assert!(v.visible >= 4.0, "visible = {}", v.visible);
    }

    #[test]
    fn tick_step_grows_as_you_zoom_out() {
        // Zoomed in: every frame is far apart, so a 1-frame step fits.
        assert_eq!(tick_step(80.0, 24.0, 58.0), 1);
        // Zoomed out: steps must land on whole seconds at 24fps.
        let wide = tick_step(0.5, 24.0, 58.0);
        assert!(wide % 24 == 0, "expected a whole-second step, got {wide}");
        // And it must actually satisfy the spacing it was asked for.
        assert!(0.5 * wide as f32 >= 58.0);
    }

    #[test]
    fn selection_groups_into_one_bucket_per_property() {
        let mut sel = KeySelection::new();
        // Inserted interleaved and out of order on purpose.
        sel.insert((PropKind::Rotation, 5));
        sel.insert((PropKind::Position, 3));
        sel.insert((PropKind::Rotation, 1));
        sel.insert((PropKind::Position, 0));
        sel.insert((PropKind::Opacity, 2));

        let grouped = group_selection_by_prop(&sel);
        assert_eq!(grouped.len(), 3, "one bucket per property: {grouped:?}");

        // Every property appears exactly once...
        let mut kinds: Vec<PropKind> = grouped.iter().map(|(k, _)| *k).collect();
        let before = kinds.len();
        kinds.dedup();
        assert_eq!(kinds.len(), before, "a property was split across buckets");

        // ...and each bucket's indices are sorted ascending, which is what
        // Track::move_keys and the descending-delete both assume.
        for (kind, idxs) in &grouped {
            assert!(idxs.windows(2).all(|w| w[0] < w[1]), "{kind:?} unsorted: {idxs:?}");
        }
    }

    #[test]
    fn empty_selection_groups_to_nothing() {
        assert!(group_selection_by_prop(&KeySelection::new()).is_empty());
    }

    #[test]
    fn edge_pan_is_dead_in_the_middle_and_signed_at_the_ends() {
        let (l, r, e) = (100.0f32, 500.0f32, 40.0f32);
        assert_eq!(edge_pan_intensity(300.0, l, r, e), 0.0, "middle is dead");
        assert_eq!(edge_pan_intensity(145.0, l, r, e), 0.0, "just inside the zone");
        assert!(edge_pan_intensity(120.0, l, r, e) < 0.0, "left zone pans left");
        assert!(edge_pan_intensity(480.0, l, r, e) > 0.0, "right zone pans right");
    }

    #[test]
    fn edge_pan_ramps_with_depth_and_saturates() {
        let (l, r, e) = (100.0f32, 500.0f32, 40.0f32);
        // Deeper into the zone → stronger.
        let shallow = edge_pan_intensity(130.0, l, r, e).abs();
        let deep = edge_pan_intensity(105.0, l, r, e).abs();
        assert!(deep > shallow, "{deep} should exceed {shallow}");
        // At and beyond the edge it saturates rather than running away.
        assert!((edge_pan_intensity(l, l, r, e) + 1.0).abs() < 1e-6);
        assert!((edge_pan_intensity(-9999.0, l, r, e) + 1.0).abs() < 1e-6);
        assert!((edge_pan_intensity(9999.0, l, r, e) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn edge_pan_handles_degenerate_tracks() {
        // A collapsed or inverted track must not produce a pan (or a NaN).
        assert_eq!(edge_pan_intensity(50.0, 100.0, 100.0, 36.0), 0.0);
        assert_eq!(edge_pan_intensity(50.0, 500.0, 100.0, 36.0), 0.0);
        assert_eq!(edge_pan_intensity(50.0, 100.0, 500.0, 0.0), 0.0);
    }

    #[test]
    fn tick_step_never_returns_zero() {
        // A degenerate/huge zoom-out must still yield a usable positive step,
        // since it's used as a modulus when drawing the ruler.
        for pxf in [1e-6f32, 0.0, 1000.0] {
            assert!(tick_step(pxf, 24.0, 58.0) > 0, "px/frame {pxf}");
        }
    }

    /// A click on the demo square (centered at t=0) should select it, and a
    /// click far outside should deselect. Fit is identity here so physical
    /// pixels equal composition coordinates.
    #[test]
    fn pick_hits_shape_and_misses_empty_space() {
        let doc = demo_document();
        let scene = evaluate(&doc, 0.0);
        let fit = Affine::IDENTITY;

        // The square sits at (300, 540) at t=0 with a 200x200 body.
        assert_eq!(pick(&scene, fit, (300.0, 540.0)), Some(NodeId(1)));
        // Empty corner — nothing there.
        assert_eq!(pick(&scene, fit, (5.0, 5.0)), None);
    }

    #[test]
    fn pick_prefers_front_most_item() {
        // The dot is a child drawn after the square, so where they overlap the
        // dot (front-most) wins. At t=0 the dot is above the square center.
        let doc = demo_document();
        let scene = evaluate(&doc, 0.0);
        let fit = Affine::IDENTITY;
        // Dot center: square pos (300,540) + child offset (0,-120) = (300,420).
        assert_eq!(pick(&scene, fit, (300.0, 420.0)), Some(NodeId(2)));
    }
}

fn main() {
    let event_loop = EventLoop::new().unwrap();
    event_loop.set_control_flow(ControlFlow::Wait);
    let mut app = App::new(demo_document());
    println!("Pain By Choice — live. Space=play/pause  ←/→=step  R=restart  Esc=quit");
    event_loop.run_app(&mut app).unwrap();
}
