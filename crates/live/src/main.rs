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
    demo::demo_document, evaluate, Color as MColor, Document, NodeId, Scene as MScene,
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
/// global transform that fits the composition into the window. The selected
/// node (if any) gets a bright outline drawn last, so it sits on top.
fn to_vello(scene: &MScene, fit: Affine, selected: Option<NodeId>) -> VScene {
    let mut vs = VScene::new();
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
    reserved_right: f64,
    reserved_bottom: f64,
) -> Affine {
    let avail_w = (win_w - reserved_right).max(1.0);
    let avail_h = (win_h - reserved_bottom).max(1.0);
    let scale = (avail_w / doc.width).min(avail_h / doc.height);
    let dx = (avail_w - doc.width * scale) * 0.5;
    let dy = (avail_h - doc.height * scale) * 0.5;
    Affine::translate((dx, dy)) * Affine::scale(scale)
}

/// Panel sizes, in logical points (egui space). Multiply by pixels-per-point to
/// reserve the matching number of physical pixels for the canvas fit.
const TRANSPORT_H: f64 = 56.0;
const PROPS_W: f64 = 260.0;

/// What the transport UI reports back after a frame's interaction.
#[derive(Default)]
struct Transport {
    toggle: bool,
    restart: bool,
    scrub_to: Option<f64>,
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
}

/// A small "●" marker shown next to animated properties (edits become keys).
fn anim_tag(ui: &mut egui::Ui, animated: bool) {
    if animated {
        ui.colored_label(egui::Color32::from_rgb(255, 216, 51), "●")
            .on_hover_text("Animated — editing sets a keyframe at the playhead");
    } else {
        ui.label("");
    }
}

/// Right-hand properties panel. Reads a resolved `NodeInfo` and writes any user
/// changes into `edits`; it never touches `App`.
fn properties_ui(root: &mut egui::Ui, info: &Option<NodeInfo>, edits: &mut PropEdits) {
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
                anim_tag(ui, n.pos_anim);
                ui.end_row();

                ui.label("Rotation");
                let mut rot = n.rot;
                if ui
                    .add(egui::DragValue::new(&mut rot).speed(0.5).suffix("°"))
                    .changed()
                {
                    edits.rot = Some(rot);
                }
                anim_tag(ui, n.rot_anim);
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
                anim_tag(ui, n.scale_anim);
                ui.end_row();

                ui.label("Opacity");
                let mut op = n.opacity;
                if ui
                    .add(egui::Slider::new(&mut op, 0.0..=1.0).show_value(false))
                    .changed()
                {
                    edits.opacity = Some(op);
                }
                anim_tag(ui, n.opacity_anim);
                ui.end_row();

                ui.label("Fill");
                if let Some(mut rgb) = n.fill {
                    if ui.color_edit_button_rgb(&mut rgb).changed() {
                        edits.fill = Some(rgb);
                    }
                } else {
                    ui.weak("none");
                }
                ui.label("");
                ui.end_row();
            });

            ui.add_space(6.0);
            ui.weak("Drag a field to nudge, or click to type; Enter commits.");
            ui.weak("● = animated. Editing an animated value keys it at the playhead.");
        });
}

/// Build the bottom transport bar. Reads the current time / playing state and
/// writes user intent into `out`; it never touches `App` directly, so it can't
/// collide with the borrows in `render`.
fn transport_ui(root: &mut egui::Ui, t: f64, duration: f64, playing: bool, out: &mut Transport) {
    egui::Panel::bottom("transport")
        .exact_size(TRANSPORT_H as f32)
        .show(root, |ui| {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.add_space(8.0);
                if ui.button(if playing { "❚❚  Pause" } else { "▶  Play" }).clicked() {
                    out.toggle = true;
                }
                if ui.button("⟲  Restart").clicked() {
                    out.restart = true;
                }
                ui.label(format!("{t:6.2}s / {duration:.2}s"));

                // Full-width playhead scrubber.
                let mut val = t.clamp(0.0, duration);
                ui.spacing_mut().slider_width = (ui.available_width() - 16.0).max(60.0);
                let resp = ui.add(
                    egui::Slider::new(&mut val, 0.0..=duration)
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
#[derive(Clone, Copy, PartialEq)]
enum PropKind {
    Position,
    Rotation,
    Scale,
    Opacity,
}

/// One dopesheet row: an animated property and the times of its keyframes.
struct DopeRow {
    label: &'static str,
    kind: PropKind,
    times: Vec<f64>,
}

/// Gather the animated properties of a node into dopesheet rows.
fn dope_rows(node: &motion_core::Node) -> Vec<DopeRow> {
    let tr = &node.transform;
    let mut rows = Vec::new();
    // Each property is a distinct value type, so this is spelled out rather
    // than looped.
    if tr.position.is_animated() {
        rows.push(DopeRow { label: "Position", kind: PropKind::Position, times: tr.position.key_times() });
    }
    if tr.rotation_deg.is_animated() {
        rows.push(DopeRow { label: "Rotation", kind: PropKind::Rotation, times: tr.rotation_deg.key_times() });
    }
    if tr.scale.is_animated() {
        rows.push(DopeRow { label: "Scale", kind: PropKind::Scale, times: tr.scale.key_times() });
    }
    if tr.opacity.is_animated() {
        rows.push(DopeRow { label: "Opacity", kind: PropKind::Opacity, times: tr.opacity.key_times() });
    }
    rows
}

/// What the dopesheet reports after a frame: a seek and/or a keyframe move.
#[derive(Default)]
struct DopeEdits {
    seek_to: Option<f64>,
    /// (property, keyframe index, new time)
    move_key: Option<(PropKind, usize, f64)>,
}

const DOPESHEET_H: f64 = 150.0;

/// Bottom dopesheet: one row per animated property, keyframes drawn as diamonds
/// along a shared time axis with a playhead line. Click a row's track to seek;
/// drag a diamond to move that keyframe in time.
fn dopesheet_ui(root: &mut egui::Ui, rows: &[DopeRow], t: f64, duration: f64, out: &mut DopeEdits) {
    egui::Panel::bottom("dopesheet")
        .exact_size(DOPESHEET_H as f32)
        .show(root, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.add_space(8.0);
                ui.strong("Timeline");
                ui.weak("— click a track to seek, drag a ◆ to move a key");
            });
            ui.separator();

            if rows.is_empty() {
                ui.add_space(8.0);
                ui.weak("Select a node with animated properties to see its keyframes.");
                return;
            }

            const LABEL_W: f32 = 80.0;
            const ROW_H: f32 = 22.0;
            let dur = duration.max(f64::MIN_POSITIVE) as f32;
            let accent = egui::Color32::from_rgb(255, 216, 51);
            let playhead_col = egui::Color32::from_rgb(240, 90, 90);

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

                    let time_to_x = |time: f64| track.left() + (time as f32 / dur) * track.width();
                    let x_to_time = |x: f32| ((x - track.left()) / track.width() * dur) as f64;

                    // Playhead line.
                    let px = time_to_x(t);
                    painter.line_segment(
                        [egui::pos2(px, track.top()), egui::pos2(px, track.bottom())],
                        egui::Stroke::new(1.5, playhead_col),
                    );

                    // Click on empty track → seek.
                    if track_resp.clicked() {
                        if let Some(p) = track_resp.interact_pointer_pos() {
                            out.seek_to = Some(x_to_time(p.x).clamp(0.0, duration));
                        }
                    }

                    // Keyframe diamonds (interactive, drawn on top).
                    let cy = track.center().y;
                    for (key_idx, &kt) in row.times.iter().enumerate() {
                        let kx = time_to_x(kt);
                        let r = 5.0;
                        let hit = egui::Rect::from_center_size(
                            egui::pos2(kx, cy),
                            egui::vec2(r * 2.4, r * 2.4),
                        );
                        let id = ui.id().with((row_idx, key_idx));
                        let resp = ui.interact(hit, id, egui::Sense::click_and_drag());

                        let col = if resp.dragged() || resp.hovered() {
                            egui::Color32::WHITE
                        } else {
                            accent
                        };
                        // Diamond = a rotated square.
                        let d = [
                            egui::pos2(kx, cy - r),
                            egui::pos2(kx + r, cy),
                            egui::pos2(kx, cy + r),
                            egui::pos2(kx - r, cy),
                        ];
                        painter.add(egui::Shape::convex_polygon(
                            d.to_vec(),
                            col,
                            egui::Stroke::new(1.0, egui::Color32::from_gray(16)),
                        ));

                        if resp.dragged() {
                            if let Some(p) = resp.interact_pointer_pos() {
                                let nt = x_to_time(p.x).clamp(0.0, duration);
                                out.move_key = Some((row.kind, key_idx, nt));
                            }
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
}

impl App {
    fn new(doc: Document) -> Self {
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
        }
    }

    /// Current looped document time in seconds.
    fn current_time(&self) -> f64 {
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
                let step = 1.0 / self.doc.fps.max(1.0);
                match event.logical_key {
                    Key::Named(NamedKey::Space) => self.toggle_play(),
                    Key::Named(NamedKey::Escape) => event_loop.exit(),
                    Key::Named(NamedKey::ArrowRight) => {
                        self.playing = false;
                        let t = self.current_time() + step;
                        self.seek(t);
                    }
                    Key::Named(NamedKey::ArrowLeft) => {
                        self.playing = false;
                        let t = self.current_time() - step;
                        self.seek(t);
                    }
                    Key::Character(ref s) if s == "r" || s == "R" => self.seek(0.0),
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
    /// property sets a keyframe at time `t` (via `Value::set_at`).
    fn apply_edits(&mut self, t: f64, e: &PropEdits) -> bool {
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
            tr.position.set_at(t, v);
            changed = true;
        }
        if let Some(r) = e.rot {
            tr.rotation_deg.set_at(t, r);
            changed = true;
        }
        if e.scale_x.is_some() || e.scale_y.is_some() {
            let cur = tr.scale.resolve(t);
            let v = Vec2::new(e.scale_x.unwrap_or(cur.x), e.scale_y.unwrap_or(cur.y));
            tr.scale.set_at(t, v);
            changed = true;
        }
        if let Some(o) = e.opacity {
            tr.opacity.set_at(t, o);
            changed = true;
        }
        if let Some(rgb) = e.fill {
            if let Some(fill) = node.fill.as_mut() {
                fill.set_at(t, MColor::rgb(rgb[0] as f64, rgb[1] as f64, rgb[2] as f64));
                changed = true;
            }
        }
        changed
    }

    /// Move a keyframe of the selected node's property (dopesheet drag).
    fn move_keyframe(&mut self, kind: PropKind, index: usize, new_time: f64) -> bool {
        let Some(id) = self.selected else {
            return false;
        };
        let Some(node) = self.doc.root.find_mut(id) else {
            return false;
        };
        let tr = &mut node.transform;
        match kind {
            PropKind::Position => tr.position.move_key(index, new_time),
            PropKind::Rotation => tr.rotation_deg.move_key(index, new_time),
            PropKind::Scale => tr.scale.move_key(index, new_time),
            PropKind::Opacity => tr.opacity.move_key(index, new_time),
        }
        true
    }

    /// Evaluate + rasterize the current frame, then composite the egui overlay.
    fn render(&mut self, window: &Window) {
        let t = self.current_time();
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
            PROPS_W * ppp,
            (TRANSPORT_H + DOPESHEET_H) * ppp,
        );

        // Resolve any pending click into a selection (or a deselect).
        if let Some(px) = self.pending_pick.take() {
            self.selected = pick(&scene, fit, px);
        }

        self.vscene = to_vello(&scene, fit, self.selected);

        // Snapshot the selected node's properties before the UI closure so the
        // egui code borrows a plain struct, never `self`.
        let sel_node = self.selected.and_then(|id| self.doc.root.find(id));
        let sel_info = sel_node.map(|node| NodeInfo::resolve(node, t));
        let rows = sel_node.map(dope_rows).unwrap_or_default();

        // --- Run egui for this frame (no `self` borrow leaks into the UI). ---
        let raw_input = self.egui_state.as_mut().unwrap().take_egui_input(window);
        let duration = self.doc.duration;
        let playing = self.playing;
        let mut transport = Transport::default();
        let mut edits = PropEdits::default();
        let mut dope = DopeEdits::default();
        let full_output = self.egui_ctx.run_ui(raw_input, |ui| {
            transport_ui(ui, t, duration, playing, &mut transport);
            dopesheet_ui(ui, &rows, t, duration, &mut dope);
            properties_ui(ui, &sel_info, &mut edits);
        });
        // Apply the UI's intent to the playback clock.
        if transport.toggle {
            self.toggle_play();
        }
        if transport.restart {
            self.seek(0.0);
        }
        if let Some(nt) = transport.scrub_to.or(dope.seek_to) {
            self.playing = false;
            self.seek(nt);
        }

        // Apply property edits + keyframe drags to the selected node, then
        // re-evaluate so the change is visible on this very frame.
        let mut dirty = self.apply_edits(t, &edits);
        if let Some((kind, idx, nt)) = dope.move_key {
            dirty |= self.move_keyframe(kind, idx, nt);
        }
        if dirty {
            let scene = evaluate(&self.doc, t);
            self.vscene = to_vello(&scene, fit, self.selected);
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
