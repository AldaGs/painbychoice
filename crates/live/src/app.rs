//! `App`: window + GPU state, the winit handler, and the per-frame update
//! that runs the UI pass and applies every panel's reported edits.
//!
//! Moved verbatim out of `main.rs` when it was split by concern; the
//! only edit was widening visibility to `pub(crate)`.

use crate::*;

pub(crate) enum RenderState {
    Active {
        surface: RenderSurface<'static>,
        window: Arc<Window>,
    },
    Suspended(Option<Arc<Window>>),
}

/// How many recently-applied fonts the picker pins above the full list. Enough
/// to cover the handful a project actually uses, short enough to stay scannable.
pub(crate) const RECENT_FONTS: usize = 8;

/// Record `family` as just-used: most recent first, no duplicates, capped at
/// [`RECENT_FONTS`]. Re-applying a font already in the list **moves** it to the
/// front rather than adding a second copy, which is what keeps the section a
/// true most-recently-used order.
///
/// A free function so the ordering rules are unit-tested without a window —
/// the same reason `apply_fps_edit` and `apply_graph_op` are free functions.
pub(crate) fn remember_font(recent: &mut Vec<String>, family: &str) {
    // The empty family is "system default", a state rather than a font choice;
    // pinning it as "recent" would be noise.
    if family.trim().is_empty() {
        return;
    }
    recent.retain(|f| f != family);
    recent.insert(0, family.to_string());
    recent.truncate(RECENT_FONTS);
}

pub(crate) struct App {
    pub(crate) context: RenderContext,
    /// One vello renderer per wgpu device, indexed by `RenderSurface::dev_id`.
    pub(crate) renderers: Vec<Option<Renderer>>,
    pub(crate) state: RenderState,
    pub(crate) vscene: VScene,
    /// Every composition in the project. The editor always edits *one* of them
    /// (`current`); a precomp layer instances another.
    pub(crate) project: MProject,
    /// Which comp is open. Stage 4 adds the switcher — for now it is always the
    /// project's root, but every read already goes through it.
    pub(crate) current: CompId,

    // egui (created lazily in `resumed`, once we have a window + device).
    pub(crate) egui_ctx: egui::Context,
    pub(crate) egui_state: Option<egui_winit::State>,
    pub(crate) egui_renderer: Option<egui_wgpu::Renderer>,

    // Playback clock.
    pub(crate) playing: bool,
    pub(crate) anchor: Instant,
    pub(crate) paused_t: f64,

    // Selection / picking (physical-pixel coordinates).
    pub(crate) cursor: (f64, f64),
    pub(crate) pending_pick: Option<(f64, f64)>,
    pub(crate) selected: Option<NodeId>,
    /// The last frame's evaluation warnings (node id + message), kept so the
    /// comp bar can show them and stderr only prints when the set changes.
    pub(crate) warnings: Vec<(u64, String)>,
    /// The keyframes selected in the dopesheet. Empty = nothing selected.
    pub(crate) selected_keys: KeySelection,
    /// Copied keyframes, pasteable onto any node's matching properties.
    pub(crate) key_clipboard: Option<KeyClipboard>,
    /// The timeline's visible frame window (zoom / pan).
    pub(crate) view: TimelineView,
    /// The panel layout.
    pub(crate) dock: Dock,
    /// Named layouts (built-ins + session-made user presets) offered in the
    /// Layout menu. Applying one replaces `dock` with a clone of its tree.
    pub(crate) presets: Vec<Preset>,
    /// The Layout menu's "save current as" name field, kept across frames.
    pub(crate) preset_name_buf: String,
    /// Edit buffer for the open comp's name, so typing doesn't rewrite the
    /// document on every keystroke. Same take/restore dance as the preset name.
    pub(crate) comp_name_buf: String,
    /// Every system font family, gathered once at startup. Enumerating the
    /// system collection is far too slow to redo per frame, and the list can't
    /// system collection is far too slow to redo per frame, and the list can't
    /// change while the app runs.
    pub(crate) font_families: Vec<String>,
    /// Fonts applied this session, most recent first — the picker's "Recent"
    /// section. Session-only on purpose: it's app state, not project state, so
    /// it has no business in the `.pbc`.
    pub(crate) recent_fonts: Vec<String>,
    /// The family currently *hovered* in the font picker. Rendered instead of
    /// the node's own family for that frame, so hovering previews the font
    /// without touching the document — only a click commits (see `render`).
    pub(crate) font_preview: Option<String>,
    /// Canvas area in physical pixels, measured from the layout tree's canvas
    /// leaf during the last UI pass. `None` until the first pass has run.
    pub(crate) canvas_rect: Option<kurbo::Rect>,
    /// The preview's zoom + pan. `Fit` by default, so the comp stays framed as
    /// panels resize; scroll, drag, or a preset button pins it to a fixed zoom.
    pub(crate) nav: CanvasNav,
    /// A middle-button pan in progress: `(cursor at press, nav.pan at press)`,
    /// both in physical pixels. Pan tracks the cursor 1:1, so no scale is kept.
    pub(crate) pan_drag: Option<((f64, f64), (f64, f64))>,
    /// Next unused node id, for shapes created in-app.
    pub(crate) next_id: u64,
    /// The module whose body is open on the graph canvas, if any. View state —
    /// which module you're editing isn't part of the document. A delete clears
    /// it (see the graph-op apply) so it can't dangle.
    pub(crate) editing_module: Option<ModuleId>,
    /// AE's work area: a comp-level *preview* range that bounds the playback
    /// loop. View state, like `view` — reset when a comp opens, never saved with
    /// the document. `None` = the whole comp. Set with `B`/`N` at the playhead.
    pub(crate) work_area: Option<WorkArea>,
    /// Width of the dopesheet's label column, in points. View state — how you
    /// like the panel split isn't part of the document. Re-clamped against the
    /// panel on every pass, so a stored width can't outlive a resize.
    pub(crate) dope_label_w: f32,
    /// The comp's state from before an in-progress FPS drag: `(fps, root, selection)`.
    ///
    /// Retiming is lossy — keys land on whole frames — so applying it once per
    /// drag delta would run 60→24 as thirty-six successive roundings and shred
    /// dense keys on the way down. Every delta instead restores this snapshot
    /// and retimes from it, making the whole drag a single conversion off the
    /// grid the user started on. The selection rides along for the same reason —
    /// see `apply_fps_edit`. `None` when no drag is in flight.
    pub(crate) fps_drag: Option<(f64, MNode, KeySelection)>,
    /// The on-canvas transform gizmo's live drag, if one is in flight. Like
    /// `fps_drag` this holds the pre-drag values so every delta resolves off
    /// the state the grab started from — see `gizmo::resolve_drag`.
    pub(crate) gizmo_drag: Option<GizmoDrag>,
    /// Whether last frame's UI pass found a gizmo handle under the pointer (or
    /// had a drag in flight). Gates canvas click-picking: without it, pressing
    /// a handle *also* runs the picker, which hits empty canvas out at the end
    /// of an arrow and deselects the very layer you were about to transform.
    ///
    /// It has to be our own flag because `is_pointer_over_egui` — the check the
    /// rest of the winit handler uses — is area-based, not widget-based, and
    /// stays `false` everywhere inside the canvas hole no matter what we draw
    /// there. One frame stale, like `over_ui` itself, which is fine: the
    /// pointer must hover a handle before it can press one.
    pub(crate) gizmo_hot: bool,
    /// The selected layer's sampled trajectory, rebuilt only when `doc_rev`,
    /// the selection or the frame window moves — each sample is a full scene
    /// evaluation, so this must never be recomputed per UI frame.
    pub(crate) motion_path: MotionPath,
    /// Cached onion-skin ghosts. Each is a full scene evaluation, so this obeys
    /// the same rebuild-only-on-change rule as `motion_path`.
    pub(crate) onion: OnionSkins,
    /// Bumped whenever the document changes. The motion-path cache keys off it;
    /// without it the path would keep drawing the pre-edit trajectory.
    pub(crate) doc_rev: u64,
    /// A guide being dragged — out of a ruler, or along the canvas. Lives here
    /// rather than in the document so an in-flight drag isn't a document edit
    /// (and so can't be saved, keyed, or bump `doc_rev`) until it's released.
    pub(crate) guide_drag: Option<GuideDrag>,
    /// Whether last frame's UI pass had the pointer over a ruler or a guide.
    /// Gates click-picking for the same reason as `gizmo_hot` — see its docs.
    pub(crate) aids_hot: bool,
    /// Every node type the composition graph can place, built once at startup.
    /// Built-ins today; the seam a plugin registers through later.
    pub(crate) node_registry: NodeRegistry,
    /// The composition node graph being authored. Session/authoring state for
    /// now — step 2 builds the model and its panel; lowering it to the `Node`/
    /// `Expr` IR and saving it with the document is step 3.
    pub(crate) node_graph: NodeGraph,
}

/// Apply the comp bar's FPS edit, keeping keyframes on their wall-clock time.
///
/// A rate change re-grids the animation rather than re-timing it (see
/// [`Comp::set_fps`]), and that conversion rounds to whole frames. Because the
/// spinner reports a value on every drag delta, applying each one in turn would
/// compound those roundings — a drag from 60 down to 24 would pass through
/// thirty-six grids and merge keys at each. So `drag` holds the comp as it was
/// when the drag began, and every delta retimes from *that*, making a drag in
/// either direction a single conversion no matter what it travelled through.
///
/// Typed edits carry no drag, and are applied directly.
///
/// `selected_keys` is remapped alongside the document: keyframe selection is
/// by index, and retiming can merge keys, so the selection has to be carried
/// across the conversion or it starts pointing at the wrong keys. Like the
/// retime itself, a drag remaps from the pre-drag state rather than from the
/// previous delta, so the selection can't drift over a long drag either.
pub(crate) fn apply_fps_edit(
    doc: &mut Comp,
    drag: &mut Option<(f64, MNode, KeySelection)>,
    edits: &CompEdits,
    selected: Option<NodeId>,
    selected_keys: &mut KeySelection,
) {
    // Snapshot first: egui reports a drag's start and its first delta together.
    if edits.fps_drag_started {
        *drag = Some((doc.fps, doc.root.clone(), selected_keys.clone()));
    }
    if let Some(fps) = edits.fps {
        // Baseline for this conversion: the drag snapshot, or the live state
        // for a typed edit. The selection is part of it for the same reason the
        // tree is — remapping the *already-remapped* selection against the
        // baseline tree reads an index from one numbering against another, so
        // one merge at an intermediate rate would move it permanently.
        let (base_fps, base_root, base_sel) = match drag.as_ref() {
            Some((f, r, s)) => (*f, r.clone(), s.clone()),
            None => (doc.fps, doc.root.clone(), selected_keys.clone()),
        };
        doc.root = base_root.clone();
        doc.fps = base_fps;

        let before_fps = doc.timebase().fps();
        let ratio = if doc.set_fps(fps.max(1.0)) {
            doc.timebase().fps() / before_fps
        } else {
            // Unchanged rate: still re-resolve, so the selection tracks the
            // baseline rather than whatever the previous delta left behind.
            1.0
        };
        if let Some(id) = selected {
            if let (Some(before), Some(after)) = (base_root.find(id), doc.root.find(id)) {
                *selected_keys = remap_selection(&base_sel, before, after, ratio);
            }
        }
    }
    if edits.fps_drag_stopped {
        *drag = None;
    }
}

/// The largest node id in a subtree, for seeding the id counter.
pub(crate) fn max_id(node: &MNode) -> u64 {
    node.children.iter().fold(node.id.0, |m, c| m.max(max_id(c)))
}

impl App {
    /// The composition being edited. Every panel reads through this, so opening
    /// a different comp (stage 4) is a one-field change rather than a rewrite.
    pub(crate) fn doc(&self) -> &Comp {
        self.project.comp(self.current).expect("the open comp always exists")
    }

    pub(crate) fn doc_mut(&mut self) -> &mut Comp {
        let id = self.current;
        self.project.comp_mut(id).expect("the open comp always exists")
    }

    pub(crate) fn new(doc: Document) -> Self {
        let next_id = max_id(&doc.root) + 1;
        let view = TimelineView::full(doc.duration_frames());
        let project = MProject::single(doc);
        let current = project.root;
        Self {
            context: RenderContext::new(),
            renderers: Vec::new(),
            state: RenderState::Suspended(None),
            warnings: Vec::new(),
            vscene: VScene::new(),
            project,
            current,
            egui_ctx: {
                // The icon font has to be registered before the first UI pass,
                // or the first frame draws tofu where every icon should be.
                let ctx = egui::Context::default();
                icon::install(&ctx);
                theme::install(&ctx);
                ctx
            },
            egui_state: None,
            egui_renderer: None,
            playing: true,
            anchor: Instant::now(),
            paused_t: 0.0,
            cursor: (0.0, 0.0),
            pending_pick: None,
            selected: None,
            selected_keys: KeySelection::new(),
            key_clipboard: None,
            view,
            dock: Dock::default_layout(),
            // Enumerated once here: the system font collection is expensive to
            // build and can't change under us while the app is running.
            font_families: motion_core::text::system_families(),
            recent_fonts: Vec::new(),
            font_preview: None,
            presets: builtin_presets(),
            preset_name_buf: String::new(),
            comp_name_buf: String::new(),
            canvas_rect: None,
            nav: CanvasNav::default(),
            pan_drag: None,
            next_id,
            editing_module: None,
            work_area: None,
            dope_label_w: 80.0,
            fps_drag: None,
            gizmo_drag: None,
            gizmo_hot: false,
            motion_path: MotionPath::default(),
            onion: OnionSkins::default(),
            doc_rev: 0,
            guide_drag: None,
            aids_hot: false,
            node_registry: NodeRegistry::with_builtins(),
            node_graph: NodeGraph::new(),
        }
    }

    /// Apply one node-graph edit after the UI pass. A free-standing method, like
    /// the other post-pass appliers, so the panel stays a pure snapshot→intent
    /// function. A connection is validated against the registry inside
    /// [`NodeGraph::connect`]; a rejected drop simply doesn't wire.
    pub(crate) fn apply_ng_op(&mut self, op: NgOp) {
        match op {
            NgOp::Add { kind, pos } => {
                self.node_graph.add_node(kind, pos);
            }
            NgOp::Move { id, pos } => {
                if let Some(n) = self.node_graph.node_mut(id) {
                    n.pos = pos;
                }
            }
            NgOp::Remove { id } => {
                self.node_graph.remove_node(id);
            }
            NgOp::Connect { from, to } => {
                let _ = self.node_graph.connect(&self.node_registry, from, to);
            }
            NgOp::Disconnect { edge } => {
                self.node_graph.disconnect(&edge);
            }
        }
    }

    /// Apply one frame's worth of alignment-aid intent to the open comp.
    ///
    /// Guides live in the document, so adding, moving or removing one is a real
    /// edit — but a *visibility* toggle is one too, deliberately: `Comp::aids`
    /// is saved, so reopening a file restores the aids you had up. None of it
    /// touches the rendered frame, so nothing here marks the scene dirty.
    pub(crate) fn apply_aid_edits(&mut self, e: &AidEdits) {
        if e.toggle_grid {
            self.doc_mut().aids.grid.visible ^= true;
        }
        if e.toggle_rulers {
            self.doc_mut().aids.rulers ^= true;
        }
        if e.toggle_guides {
            self.doc_mut().aids.guides.visible ^= true;
        }
        if e.toggle_snap {
            self.doc_mut().aids.snap ^= true;
        }
        if e.toggle_onion {
            self.doc_mut().aids.onion.visible ^= true;
        }
        if let Some((b, a)) = e.set_onion_counts {
            let o = &mut self.doc_mut().aids.onion;
            o.before = b.min(Onion::MAX_GHOSTS);
            o.after = a.min(Onion::MAX_GHOSTS);
        }
        if let Some(st) = e.set_onion_step {
            self.doc_mut().aids.onion.step = st.max(1);
        }
        if let Some(op) = e.set_onion_opacity {
            self.doc_mut().aids.onion.opacity = op.clamp(0.0, 1.0);
        }
        if let Some(sp) = e.set_grid_spacing {
            self.doc_mut().aids.grid.spacing = sp.clamp(Grid::MIN_SPACING, Grid::MAX_SPACING);
        }
        if let Some(n) = e.set_grid_subdivisions {
            self.doc_mut().aids.grid.subdivisions = n.max(1);
        }
        if e.clear_guides {
            self.doc_mut().aids.guides.items.clear();
        }
        if let Some(g) = e.add_guide {
            self.doc_mut().aids.guides.items.push(g);
        }
        // Indices come from the frame that drew them, so re-check rather than
        // index blindly: a `Retype`/undo between drawing and applying could
        // have shortened the list.
        if let Some((i, at)) = e.move_guide {
            if let Some(g) = self.doc_mut().aids.guides.items.get_mut(i) {
                g.at = at;
            }
        }
        if let Some(i) = e.remove_guide {
            let items = &mut self.doc_mut().aids.guides.items;
            if i < items.len() {
                items.remove(i);
            }
        }
    }

    /// The playback loop's frame bounds `[lo, hi)` — the work area clamped into
    /// the comp, or the whole comp.
    pub(crate) fn loop_bounds_frames(&self) -> (i64, i64) {
        loop_bounds(self.work_area, self.doc().duration_frames())
    }

    /// The same bounds in seconds, for the wall-clock playback loop. The
    /// no-work-area case returns the comp's exact `duration` (not a frame
    /// round-trip) so playback timing is byte-for-byte what it was before work
    /// areas existed.
    fn loop_bounds_secs(&self) -> (f64, f64) {
        match self.work_area {
            None => (0.0, self.doc().duration),
            Some(_) => {
                let tb = self.doc().timebase();
                let (lo, hi) = self.loop_bounds_frames();
                (tb.frames_to_seconds(lo as f64), tb.frames_to_seconds(hi as f64))
            }
        }
    }

    /// Current looped position on the wall clock, in seconds. Continuous — this
    /// is the clock, not the frame grid. Use `current_frame` / `current_time`
    /// for anything that evaluates or displays.
    ///
    /// **While playing**, the wall clock folds into the work-area span, so a
    /// preview loops within it. **While paused**, the playhead sits exactly
    /// where it was placed (wrapped only at the comp bounds) — so you can still
    /// scrub *outside* the work area to inspect a frame, the way AE lets you.
    pub(crate) fn raw_time(&self) -> f64 {
        if self.playing {
            let (lo, hi) = self.loop_bounds_secs();
            wrap_into(self.anchor.elapsed().as_secs_f64(), lo, hi)
        } else if self.doc().duration > 0.0 {
            self.paused_t.rem_euclid(self.doc().duration)
        } else {
            self.paused_t
        }
    }

    /// Set the work area's start (`B`) or end (`N`) at `frame`. Thin wrappers
    /// over the pure `with_work_*` (which own the seeding + clamping, so it's
    /// unit-tested); a degenerate range is re-clamped by `loop_bounds` at read
    /// time, so the loop span can never invert.
    pub(crate) fn set_work_start(&mut self, frame: i64) {
        let total = self.doc().duration_frames();
        self.work_area = Some(with_work_start(self.work_area, frame, total));
    }

    pub(crate) fn set_work_end(&mut self, frame: i64) {
        let total = self.doc().duration_frames();
        self.work_area = Some(with_work_end(self.work_area, frame, total));
    }

    /// The frame the playhead currently sits on.
    ///
    /// Floors rather than rounds: a frame must be *held* for its full duration,
    /// the way a projector does. Rounding would show frame N starting half a
    /// frame early and is the classic off-by-half in playback code.
    pub(crate) fn current_frame(&self) -> i64 {
        let tb = self.doc().timebase();
        tb.seconds_to_frames_exact(self.raw_time()).floor() as i64
    }

    /// Current document time in seconds, **snapped to the frame grid**. This is
    /// what the canvas evaluates at, so playback actually steps at `doc.fps`
    /// instead of running at the monitor's refresh rate.
    pub(crate) fn current_time(&self) -> f64 {
        self.doc().timebase().frames_to_seconds(self.current_frame() as f64)
    }

    /// Seek to a frame, wrapping around the composition length. All seeking
    /// goes through here, so the playhead can only ever land on the grid.
    pub(crate) fn seek_frame(&mut self, frame: i64) {
        let total = self.doc().duration_frames().max(1);
        let frame = frame.rem_euclid(total);
        self.seek(self.doc().timebase().frames_to_seconds(frame as f64));
    }

    pub(crate) fn seek(&mut self, t: f64) {
        let t = t.rem_euclid(self.doc().duration.max(f64::MIN_POSITIVE));
        self.paused_t = t;
        self.anchor = Instant::now() - std::time::Duration::from_secs_f64(t);
    }

    pub(crate) fn toggle_play(&mut self) {
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
                    Key::Character(ref s) if s == "r" || s == "R" => {
                        // Restart the *preview*: to the work-area start, not
                        // always frame 0.
                        self.seek_frame(self.loop_bounds_frames().0);
                    }
                    // AE's work-area keys: B sets the start at the playhead, N
                    // the end. View state — nothing in the document changes.
                    Key::Character(ref s) if s == "b" || s == "B" => {
                        self.set_work_start(self.current_frame());
                    }
                    Key::Character(ref s) if s == "n" || s == "N" => {
                        self.set_work_end(self.current_frame());
                    }
                    Key::Named(NamedKey::Delete) | Key::Named(NamedKey::Backspace) => {
                        self.delete_selected_keys();
                    }
                    _ => {}
                }
                window.request_redraw();
            }

            WindowEvent::CursorMoved { position, .. } => {
                self.cursor = (position.x, position.y);
                // A middle-button pan tracks the cursor 1:1 in physical pixels.
                if let Some((start_cursor, start_pan)) = self.pan_drag {
                    self.nav.pan = (
                        start_pan.0 + (position.x - start_cursor.0),
                        start_pan.1 + (position.y - start_cursor.1),
                    );
                }
                // Repaint so egui's hover/consumed state stays current even
                // while paused — otherwise the next click is judged against a
                // stale frame and canvas picking fires over the UI.
                window.request_redraw();
            }

            // `!self.gizmo_hot` is what keeps a handle click from doubling as a
            // deselect — see the field's docs for why `over_ui` can't cover it.
            WindowEvent::MouseInput { state, button, .. }
                if !over_ui
                    && !self.gizmo_hot
                    && !self.aids_hot
                    && state == ElementState::Pressed
                    && button == winit::event::MouseButton::Left =>
            {
                // Defer the hit-test to render(), where the evaluated scene and
                // fit transform for this exact frame are in hand.
                self.pending_pick = Some(self.cursor);
                window.request_redraw();
            }

            // Middle-button drag pans the preview. Grabbing anywhere on the
            // canvas works, so it never fights click-to-select (left button).
            // Starting from Fit pins the current framing as an explicit zoom
            // first, so the pan has a fixed scale to move against.
            WindowEvent::MouseInput { state, button, .. }
                if button == winit::event::MouseButton::Middle =>
            {
                match state {
                    ElementState::Pressed if !over_ui => {
                        if self.nav.zoom.is_none() {
                            if let Some(canvas) = self.canvas_rect {
                                let ppp = window.scale_factor();
                                let scale = canvas_scale(self.doc(), canvas, self.nav, ppp);
                                self.nav = CanvasNav { zoom: Some(scale / ppp), pan: (0.0, 0.0) };
                            }
                        }
                        self.pan_drag = Some((self.cursor, self.nav.pan));
                    }
                    ElementState::Released => self.pan_drag = None,
                    _ => {}
                }
                window.request_redraw();
            }

            // Scroll over the canvas zooms about the cursor (AE-style). Uses the
            // last measured canvas rect + fit to find the comp point under the
            // pointer and keep it fixed across the zoom.
            WindowEvent::MouseWheel { delta, .. } if !over_ui => {
                if let Some(canvas) = self.canvas_rect {
                    let ppp = window.scale_factor();
                    let steps = match delta {
                        winit::event::MouseScrollDelta::LineDelta(_, y) => y as f64,
                        winit::event::MouseScrollDelta::PixelDelta(p) => p.y / 60.0,
                    };
                    if steps != 0.0 {
                        let scale = canvas_scale(self.doc(), canvas, self.nav, ppp);
                        let comp_pt = canvas_transform(self.doc(), canvas, self.nav, ppp)
                            .inverse()
                            * Point::new(self.cursor.0, self.cursor.1);
                        let new_scale = scale * 1.25_f64.powf(steps);
                        self.nav =
                            nav_zoom_about(self.doc(), canvas, comp_pt, self.cursor, new_scale, ppp);
                        window.request_redraw();
                    }
                }
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
    pub(crate) fn apply_edits(&mut self, frame: i64, e: &PropEdits) -> bool {
        let t = frame as f64;
        let mut ctx = EvalCtx::at(t);
        let Some(id) = self.selected else {
            return false;
        };
        // Field path, not `doc_mut()` — see the note in `delete_selected_keys`.
        let Some(node) = self.project.comp_mut(self.current).unwrap().root.find_mut(id) else {
            return false;
        };
        let tr = &mut node.transform;
        let mut changed = false;

        if e.anchor_x.is_some() || e.anchor_y.is_some() {
            let cur = tr.anchor.resolve(&mut ctx);
            let v = Vec2::new(e.anchor_x.unwrap_or(cur.x), e.anchor_y.unwrap_or(cur.y));
            tr.anchor.set_at(frame, v);
            changed = true;
        }
        if e.pos_x.is_some() || e.pos_y.is_some() {
            let cur = tr.position.resolve(&mut ctx);
            let v = Vec2::new(e.pos_x.unwrap_or(cur.x), e.pos_y.unwrap_or(cur.y));
            tr.position.set_at(frame, v);
            changed = true;
        }
        if let Some(r) = e.rot {
            tr.rotation_deg.set_at(frame, r);
            changed = true;
        }
        if e.scale_x.is_some() || e.scale_y.is_some() {
            let cur = tr.scale.resolve(&mut ctx);
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
                fill.set_at(frame, rgb_color(rgb));
                changed = true;
            }
        }

        // Stroke add/remove first, so a stroke added this frame is immediately
        // editable by the value edits below rather than a frame later.
        if e.add_stroke && node.stroke.is_none() {
            node.stroke = Some(motion_core::Stroke {
                color: Value::constant(MColor::rgb(0.0, 0.0, 0.0)),
                width: Value::constant(2.0),
            });
            changed = true;
        }
        if e.remove_stroke {
            node.stroke = None;
            // Its keyframes go with it, so drop any selection pointing at them
            // — stale `(kind, index)` refs would otherwise address a track that
            // no longer exists.
            self.selected_keys
                .retain(|(k, _)| !matches!(k, PropKind::StrokeColor | PropKind::StrokeWidth));
            changed = true;
        }
        let node = self.doc_mut().root.find_mut(id).expect("checked above");
        if let Some(rgb) = e.stroke_color {
            if let Some(s) = node.stroke.as_mut() {
                s.color.set_at(frame, rgb_color(rgb));
                changed = true;
            }
        }
        if let Some(w) = e.stroke_width {
            if let Some(s) = node.stroke.as_mut() {
                s.width.set_at(frame, w);
                changed = true;
            }
        }

        // Shape geometry. Size is a `Vec2` edited as two independent fields, so
        // the untouched axis has to be read back from the current value — same
        // pattern as position/scale above.
        if e.size_x.is_some() || e.size_y.is_some() {
            if let Some(MShape::Rect { size, .. }) | Some(MShape::Ellipse { size }) =
                node.shape.as_mut()
            {
                let cur = size.resolve(&mut ctx);
                let v = Vec2::new(e.size_x.unwrap_or(cur.x), e.size_y.unwrap_or(cur.y));
                size.set_at(frame, v);
                changed = true;
            }
        }
        if let Some(r) = e.radius {
            if let Some(MShape::Rect { radius, .. }) = node.shape.as_mut() {
                radius.set_at(frame, r);
                changed = true;
            }
        }

        // Text. Only `size` goes through `set_at` (it's the one `Value` here and
        // so the one that can auto-key); the rest are plain fields assigned
        // outright.
        if let Some(MShape::Text { content, family, size, align, max_width }) = node.shape.as_mut()
        {
            if let Some(v) = e.text_content.clone() {
                *content = v;
                changed = true;
            }
            if let Some(v) = e.text_family.clone() {
                *family = v;
                changed = true;
            }
            if let Some(v) = e.text_size {
                size.set_at(frame, v);
                changed = true;
            }
            if let Some(v) = e.text_align {
                *align = v;
                changed = true;
            }
            if let Some(v) = e.text_max_width {
                *max_width = v;
                changed = true;
            }
        }

        // Stopwatch clicks: insert a keyframe at the playhead (promoting a
        // constant to a track the first time). Driven off `PropKind` so a new
        // animatable property needs no new branch here.
        for &kind in &e.key {
            if let Some(mut p) = prop_of_mut(node, kind) {
                p.insert_key(frame);
                changed = true;
            }
        }
        changed
    }

    /// Set the easing handles for the selected keyframe's outgoing segment.
    pub(crate) fn set_ease(&mut self, kind: PropKind, index: usize, p1: (f32, f32), p2: (f32, f32)) -> bool {
        let Some(id) = self.selected else {
            return false;
        };
        let Some(node) = self.doc_mut().root.find_mut(id) else {
            return false;
        };
        let out = Handle::new(p1.0 as f64, p1.1 as f64);
        let next_in = Handle::new(p2.0 as f64, p2.1 as f64);
        let Some(mut p) = prop_of_mut(node, kind) else {
            return false;
        };
        p.set_segment_handles(index, out, next_in);
        true
    }

    /// Remove every dopesheet-selected keyframe (Delete). A track keeps at
    /// least one key, so this may be a partial no-op.
    pub(crate) fn delete_selected_keys(&mut self) -> bool {
        let Some(id) = self.selected else {
            return false;
        };
        if self.selected_keys.is_empty() {
            return false;
        }
        // Reached through the field rather than `doc_mut()` on purpose: an
        // accessor borrows all of `self`, and `selected_keys` is read below.
        let Some(node) = self.project.comp_mut(self.current).unwrap().root.find_mut(id) else {
            return false;
        };
        // Descending index order: removing a key shifts every later index
        // down, so deleting from the back keeps the remaining ones valid.
        for &(kind, index) in self.selected_keys.iter().rev() {
            if let Some(mut p) = prop_of_mut(node, kind) {
                p.remove_key(index);
            }
        }
        self.selected_keys.clear();
        true
    }

    /// Copy the selected keyframes (Ctrl+C). Whole keys — value and easing —
    /// so a paste reproduces the curve, not just the timing.
    pub(crate) fn copy_selected_keys(&mut self) -> bool {
        let Some(node) = self.selected.and_then(|id| self.doc().root.find(id)) else {
            return false;
        };
        if self.selected_keys.is_empty() {
            return false;
        }
        let mut tracks = Vec::new();
        let mut origin = i64::MAX;
        for (kind, idxs) in group_selection_by_prop(&self.selected_keys) {
            let Some(p) = prop_of(node, kind) else { continue };
            let clip = p.keys_at(&idxs);
            let Some(first) = clip.first_frame() else { continue };
            origin = origin.min(first);
            tracks.push((kind, clip));
        }
        if tracks.is_empty() {
            return false;
        }
        self.key_clipboard = Some(KeyClipboard { origin, tracks });
        true
    }

    /// Paste the clipboard with its earliest key on the playhead (Ctrl+V), and
    /// select what landed — so the very next drag moves the paste, which is the
    /// motion the user almost always wants next.
    pub(crate) fn paste_keys(&mut self) -> bool {
        let Some(clip) = self.key_clipboard.clone() else {
            return false;
        };
        let Some(id) = self.selected else {
            return false;
        };
        let offset = self.current_frame() - clip.origin;
        let Some(node) = self.doc_mut().root.find_mut(id) else {
            return false;
        };
        let mut landed = KeySelection::new();
        for (kind, track) in &clip.tracks {
            // Skipped when the paste target lacks the property entirely —
            // copying an ellipse's Size and pasting onto a group, say.
            let Some(mut p) = prop_of_mut(node, *kind) else { continue };
            for i in p.insert_keys(track, offset) {
                landed.insert((*kind, i));
            }
        }
        if landed.is_empty() {
            return false;
        }
        self.selected_keys = landed;
        true
    }

    /// Move every selected keyframe by `delta` frames as one rigid block.
    ///
    /// Each property is a separate `Track`, so the limits are intersected
    /// across all of them *before* anything moves — otherwise a track that
    /// clamps early would slide out of sync with the others and the selection
    /// would deform instead of translating.
    pub(crate) fn move_selected_keys(&mut self, delta: i64) -> bool {
        let Some(id) = self.selected else {
            return false;
        };
        if self.selected_keys.is_empty() || delta == 0 {
            return false;
        }
        // Grouped before the mutable borrow: `doc_mut()` borrows all of `self`.
        let per_prop = group_selection_by_prop(&self.selected_keys);
        let Some(node) = self.doc_mut().root.find_mut(id) else {
            return false;
        };

        // Intersect the allowed delta across every affected track.
        let (mut lo, mut hi) = (i64::MIN, i64::MAX);
        for (kind, idxs) in &per_prop {
            let Some(p) = prop_of(node, *kind) else { continue };
            if let Some((l, h)) = p.move_keys_limits(idxs) {
                lo = lo.max(l);
                hi = hi.min(h);
            }
        }
        if lo > hi {
            return false; // the block is boxed in somewhere
        }
        // Also keep the whole selection inside the composition.
        let last = self.doc().duration_frames().max(1);
        let node = self.doc_mut().root.find_mut(id).expect("checked above");
        let mut min_frame = i64::MAX;
        let mut max_frame = i64::MIN;
        for (kind, idxs) in &per_prop {
            let Some(p) = prop_of(node, *kind) else { continue };
            let frames = p.key_frames();
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
            if let Some(mut p) = prop_of_mut(node, *kind) {
                p.move_keys(idxs, applied);
            }
        }
        true
    }

    /// Create a new shape/group, parent it under the selected node (or the
    /// root), select it, and return `true` (the doc changed).
    pub(crate) fn add_node(&mut self, kind: NewShape) -> bool {
        let id = self.next_id;
        self.next_id += 1;

        let center = Vec2::new(self.doc().width / 2.0, self.doc().height / 2.0);
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
            // Seeded with visible placeholder text: an empty text layer would
            // shape to an empty path and look like the add button did nothing.
            NewShape::Text => MNode::shape(
                id,
                format!("Text {id}"),
                MShape::Text {
                    content: "Text".to_string(),
                    family: String::new(),
                    size: Value::constant(96.0),
                    align: TextAlign::Left,
                    max_width: None,
                },
            )
            .with_fill(fill)
            .with_transform(at_center),
            NewShape::Group => MNode::group(id, format!("Group {id}")).with_transform(at_center),
        };

        // Parent under the selected node if it still exists, else the root.
        let target = self.selected.filter(|sid| self.doc().root.find(*sid).is_some());
        let parent = match target {
            Some(sid) => self.doc_mut().root.find_mut(sid).unwrap(),
            None => &mut self.doc_mut().root,
        };
        parent.children.push(node);

        self.selected = Some(NodeId(id));
        self.selected_keys.clear();
        true
    }

    /// Open a different composition for editing.
    ///
    /// Everything comp-scoped has to be rebuilt: node ids are per-comp, so a
    /// stale `next_id` would hand out ids that collide with the newly opened
    /// tree, and a stale selection would point at a node in the comp we left.
    pub(crate) fn open_comp(&mut self, id: CompId) {
        if self.project.comp(id).is_none() || id == self.current {
            return;
        }
        self.current = id;
        // Read everything off the comp before writing back — `doc()` borrows
        // all of `self`, so the reads can't straddle an assignment.
        let comp = self.doc();
        let (next_id, frames, name) =
            (max_id(&comp.root) + 1, comp.duration_frames(), comp.name.clone());
        self.next_id = next_id;
        self.view = TimelineView::full(frames);
        // The work area is per-comp view state; a fresh open starts with none.
        self.work_area = None;
        // A different comp has its own size; re-fit so it lands framed.
        self.nav = CanvasNav::default();
        self.comp_name_buf = name;
        self.selected = None;
        self.selected_keys.clear();
    }

    /// Move `id`'s subtree into a new composition and leave an instance in its
    /// place — the core AE workflow. See [`precompose_into`] for the semantics.
    pub(crate) fn precompose(&mut self, id: NodeId) {
        let Some((_, instance)) =
            precompose_into(&mut self.project, self.current, id, self.next_id)
        else {
            return;
        };
        self.next_id += 1;
        self.selected = Some(instance);
        self.selected_keys.clear();
    }

    /// Serialize the document *and the current UI layout* to a `.pbc` (JSON)
    /// file chosen via a native save dialog. The layout (active dock + user
    /// presets) rides in a [`Project`] wrapper alongside the document; built-in
    /// presets are code, so only user ones are written.
    pub(crate) fn save(&self) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("Pain By Choice", &["pbc", "json"])
            .set_file_name("project.pbc")
            .save_file()
        else {
            return;
        };
        let project = SaveFile {
            project: Some(self.project.clone()),
            document: None,
            layout: LayoutState {
                dock: Some(self.dock.clone()),
                user_presets: self.presets.iter().filter(|p| !p.builtin).cloned().collect(),
            },
        };
        match serde_json::to_string_pretty(&project) {
            Ok(json) => {
                if let Err(e) = std::fs::write(&path, json) {
                    eprintln!("save failed: {e}");
                }
            }
            Err(e) => eprintln!("serialize failed: {e}"),
        }
    }

    /// Load a `.pbc` via a native open dialog, replacing the current document
    /// *and* layout. Returns whether the document changed. Selection and the id
    /// counter are reset to match the loaded tree.
    ///
    /// Reads both the current [`Project`] format and the older bare-`Document`
    /// files (which carry no layout): the wrapper is tried first, and a bare doc
    /// fails it — it has no `document` field — so it falls through to the plain
    /// parse and keeps the default layout.
    pub(crate) fn load(&mut self) -> bool {
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
        // Three formats, newest first: a project, a pre-comps wrapper holding a
        // single document, and a bare document from before the wrapper existed.
        // Each older one loads as a one-comp project, so nothing is stranded.
        let (mut project, layout) = match serde_json::from_str::<SaveFile>(&text) {
            Ok(f) => {
                let layout = Some(f.layout);
                match (f.project, f.document) {
                    (Some(p), _) => (p, layout),
                    (None, Some(d)) => (MProject::single(d), layout),
                    // A `SaveFile` with neither is not one of our files: it
                    // parsed only because every field defaults.
                    (None, None) => match serde_json::from_str::<Document>(&text) {
                        Ok(d) => (MProject::single(d), None),
                        Err(e) => {
                            eprintln!("parse failed: {e}");
                            return false;
                        }
                    },
                }
            }
            Err(e) => {
                eprintln!("parse failed: {e}");
                return false;
            }
        };
        // Pre-frame-grid docs stored keyframes as float seconds; this converts
        // them using each comp's own fps. No-op on new files.
        project.migrate();
        let open = project.root_comp();
        self.next_id = max_id(&open.root) + 1;
        self.view = TimelineView::full(open.duration_frames());
        // The work area is view state, not saved with the document.
        self.work_area = None;
        self.project = project;
        self.current = self.project.root;
        self.selected = None;
        self.selected_keys.clear();

        // Restore the layout. Built-ins are always rebuilt from code; loaded user
        // presets (and the active dock) are validated, so a corrupt or edited
        // file can never wedge the editor with an unusable arrangement.
        self.presets = builtin_presets();
        let restored = match layout {
            Some(l) => {
                self.presets.extend(l.user_presets.into_iter().filter(|p| p.dock.is_valid()));
                l.dock
            }
            None => None,
        };
        self.dock = match restored {
            Some(d) if d.is_valid() => d,
            Some(_) => {
                eprintln!("ignoring invalid saved layout; using default");
                Dock::default_layout()
            }
            None => Dock::default_layout(),
        };
        self.seek_frame(0);
        true
    }

    /// A throwaway copy of the project with the hovered font swapped into the
    /// selected text layer, or `None` when nothing is being previewed.
    ///
    /// This is what makes "hover to preview, click to apply" non-destructive:
    /// the document keeps the family it had, and the substitution lives exactly
    /// one frame. The clone only happens while the picker is open and the
    /// pointer is over a row, so the common path pays nothing.
    pub(crate) fn preview_project(&self) -> Option<MProject> {
        let family = self.font_preview.as_ref()?;
        let id = self.selected?;
        let mut preview = self.project.clone();
        let node = preview.comp_mut(self.current)?.root.find_mut(id)?;
        match node.shape.as_mut() {
            Some(MShape::Text { family: f, .. }) => {
                *f = family.clone();
                Some(preview)
            }
            // Hovering a font with a non-text layer selected previews nothing
            // rather than cloning for no reason.
            _ => None,
        }
    }

    /// Evaluate + rasterize the current frame, then composite the egui overlay.
    pub(crate) fn render(&mut self, window: &Window) {
        // The whole render path is in the frame domain; seconds only ever
        // appear in the timecode string.
        let frame = self.current_frame();
        let t = frame as f64;
        let last_frame = self.doc().duration_frames().max(1);
        // While a font is hovered in the picker, this frame is drawn from a
        // preview copy instead of the real project (see `preview_project`).
        let previewing = self.preview_project();
        let scene = evaluate_comp(previewing.as_ref().unwrap_or(&self.project), self.current, t);
        // Warnings are re-derived every frame, so print only when the set
        // actually changes — a broken script would otherwise spam stderr at the
        // refresh rate. The current set is kept for the comp bar's indicator.
        let warnings: Vec<(u64, String)> =
            scene.warnings.iter().map(|(id, m)| (id.0, m.clone())).collect();
        if warnings != self.warnings {
            for (id, msg) in &warnings {
                eprintln!("warning [node {id}]: {msg}");
            }
        }
        self.warnings = warnings;
        // Cloned for the UI closure, which must not borrow `self`.
        let warnings = self.warnings.clone();

        let size = window.inner_size();
        // egui works in points; the canvas fit works in physical pixels.
        let ppp = window.scale_factor();
        // The canvas area comes from the layout tree's canvas leaf, measured
        // during *last* frame's UI pass — the rect isn't known until the panels
        // have laid out, and the fit is needed before this frame's UI runs (to
        // pick and to build the vello scene). One frame stale only while a
        // splitter or the window is actively being dragged, and it self-corrects
        // on the next repaint, which a drag guarantees.
        let canvas = self.canvas_rect.unwrap_or_else(|| {
            // First frame: nothing measured yet, so fill the window.
            kurbo::Rect::new(0.0, 0.0, size.width as f64, size.height as f64)
        });
        let fit = canvas_transform(self.doc(), canvas, self.nav, ppp);

        // Resolve any pending click into a selection (or a deselect). Changing
        // the selected node invalidates any keyframe selection.
        if let Some(px) = self.pending_pick.take() {
            let picked = pick(&scene, fit, px);
            if picked != self.selected {
                self.selected = picked;
                self.selected_keys.clear();
            }
        }

        // Ghosts are baked into the vello scene, so they must be cached *before*
        // it is built or they lag a frame behind the playhead — which on a fast
        // scrub reads as them drifting out of step with the artwork. Cached on
        // the same terms as the motion path: rebuilt only when something they
        // depend on moves.
        if self.doc().aids.onion.visible {
            let onion = self.doc().aids.onion.clone();
            let (comp, sel, rev) = (self.current, self.selected, self.doc_rev);
            let now = self.current_frame();
            self.onion.cache(&self.project, comp, sel, now, &onion, rev);
        } else {
            self.onion.clear();
        }

        let bg = self.doc().bg;
        let pp = self.doc().passepartout;
        self.vscene =
            to_vello(
                &scene,
                fit,
                (self.doc().width, self.doc().height),
                bg,
                pp,
                canvas,
                &self.onion.ghosts,
                self.selected,
            );

        // Motion path: only for a layer whose *position* is actually animated —
        // a constant position has no trajectory, and drawing one dot under the
        // gizmo would be noise. Cached, so this is a no-op on most frames.
        //
        // Runs *before* `sel_node` is bound: that binding holds an immutable
        // borrow of `self` for the rest of the frame, and caching needs `&mut`.
        let path_frame = self.current_frame();
        let pos_animated = self
            .selected
            .and_then(|id| self.doc().root.find(id))
            .is_some_and(|n| n.transform.position.is_animated());
        match (pos_animated, self.selected) {
            (true, Some(id)) => {
                let range = self.doc().motion_path_range;
                let rev = self.doc_rev;
                self.motion_path.cache(&self.project, self.current, id, path_frame, range, rev);
            }
            _ => self.motion_path.clear(),
        }
        // Cloned for the UI closure, which must not borrow `self`.
        let motion_path = self.motion_path.clone();
        let path_now = (path_frame - motion_path.first_frame).try_into().ok();

        // Snapshot the selected node's properties before the UI closure so the
        // egui code borrows a plain struct, never `self`.
        let sel_node = self.selected.and_then(|id| self.doc().root.find(id));
        // Pass the doc so an expression-driven property resolves against the
        // scene (a doc-less context would show its fallback instead).
        let sel_info = sel_node.map(|node| NodeInfo::resolve(node, self.doc(), t));
        // The gizmo needs the selected layer's *world* matrix, which only the
        // evaluated scene knows (it is the whole parent chain multiplied out).
        // Taken from `Scene::places`, not from a `RenderItem`: a group or null
        // draws nothing and so has no item, but it is exactly the sort of layer
        // you parent things to and want handles on. `None` here now means only
        // "the layer isn't live on this frame".
        let gizmo_target = match (self.selected, &sel_info) {
            (Some(id), Some(info)) => scene
                .place(id)
                .map(|place| GizmoTarget::new(id.0, place.world, info)),
            _ => None,
        };
        // One box per drawable item in the selection's subtree — a group shows
        // its pieces, not just its extent. Comp space, projected at paint time
        // like the motion path.
        let sel_boxes = sel_node.map(|n| selection_boxes(&scene, n)).unwrap_or_default();
        // Snap candidates: the dragged layer's own extent, and every *other*
        // node's. Both come straight from `Scene::places`, so they agree with
        // what the walk actually did rather than being re-derived here.
        let sel_extent = self.selected.and_then(|id| scene.place(id).and_then(|p| p.bounds));
        let excluded: Vec<NodeId> = self
            .selected
            .map(|id| snap_excluded(&self.doc().root, id))
            .unwrap_or_default();
        let other_extents: Vec<kurbo::Rect> = scene
            .places
            .iter()
            .filter(|(id, _)| !excluded.contains(id))
            .filter_map(|(_, p)| p.bounds)
            .collect();
        let rows = sel_node.map(dope_rows).unwrap_or_default();
        // Every key on the selected node, flattened, for the transport's
        // key-stepping buttons. Duplicates across properties are fine —
        // `neighbor_key` takes a nearest, not a position in a list.
        let key_frames: Vec<i64> = rows.iter().flat_map(|r| r.frames.iter().copied()).collect();
        // The clip bar only exists for a selected layer (the root isn't one).
        let clip = sel_node
            .filter(|n| Some(n.id) != Some(self.doc().root.id))
            .map(|n| ClipInfo { timing: n.timing });
        // Snapshot for the graph panel (clones the selected node's expressions
        // and the module body being edited, if any).
        let graph_info = GraphInfo::gather(
            self.doc(),
            &self.project.modules,
            self.selected,
            self.editing_module,
            t,
        );

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
        tree_rows(&self.doc().root, 0, &mut tree);

        // --- Run egui for this frame (no `self` borrow leaks into the UI). ---
        let raw_input = self.egui_state.as_mut().unwrap().take_egui_input(window);
        let duration = self.doc().duration;
        let comp_bg = self.doc().bg;
        let comp_pp = self.doc().passepartout;
        let comp_path_range = self.doc().motion_path_range;
        let timebase = self.doc().timebase();
        let view = self.view;
        let work_area = self.work_area;
        let dope_label_w = self.dope_label_w;
        let playing = self.playing;
        let mut transport = Transport::default();
        let mut edits = PropEdits::default();
        // Moved out of `self` for the UI pass and put back after it, like the
        // keyframe selection — the closure must not borrow `App`.
        let mut gizmo_drag = self.gizmo_drag.take();
        let mut guide_drag = self.guide_drag.take();
        let mut aid_edits = AidEdits::default();
        let mut aids_hot = false;
        // Cloned for the UI closure, which must not borrow `self`.
        let aids = self.doc().aids.clone();
        // Ctrl temporarily disables snapping, the way it does in Blender and
        // Figma — precise placement is one key away rather than a trip to a
        // toggle and back.
        let snap_bypass = self.egui_ctx.input(|i| i.modifiers.ctrl || i.modifiers.command);
        // Recomputed every frame: with no selection there is no gizmo, so the
        // flag must fall back to false rather than latch on from a stale frame.
        let mut gizmo_hot = false;
        let mut dope = DopeEdits::default();
        let mut tree_edits = TreeEdits::default();
        let mut selected_keys = std::mem::take(&mut self.selected_keys);
        let selected_node = self.selected;
        let mut ease_out: Option<((f32, f32), (f32, f32))> = None;
        let mut comp = CompEdits::default();
        let (doc_w, doc_h, doc_fps) = (self.doc().width, self.doc().height, self.doc().fps);
        // Layout-preset menu: the names to list, the save-field buffer (taken so
        // the UI never borrows `self`, restored after), and the reported intent.
        let preset_names: Vec<String> = self.presets.iter().map(|p| p.name.clone()).collect();
        let mut preset_name_buf = std::mem::take(&mut self.preset_name_buf);
        let mut comp_name_buf = std::mem::take(&mut self.comp_name_buf);
        // Comps for the switcher, in id order (which is creation order).
        let comp_entries: Vec<CompEntry> = self
            .project
            .comps
            .iter()
            .map(|(id, c)| CompEntry { id: *id, label: c.label(*id) })
            .collect();
        let current_comp = self.current;
        let mut layout = LayoutEdits::default();
        // Panels are drawn by walking the layout tree; each leaf dispatches to
        // the matching editor. Nothing here knows *where* a panel is — that's
        // the tree's business, which is the whole point of the refactor.
        // Live zoom read-out for the preview toolbar. `canvas` is last frame's
        // rect (one frame stale, like the fit), which is plenty for a label.
        let zoom_pct = (canvas_scale(self.doc(), canvas, self.nav, ppp) / ppp * 100.0).round() as i32;
        let is_fit = self.nav.zoom.is_none();
        let mut canvas_edits = CanvasEdits::default();
        let dock = &mut self.dock;
        // Borrowed (not cloned) beside `dock`: disjoint fields, and the font
        // list is a few hundred strings we don't want to copy every frame.
        let font_families = &self.font_families;
        let recent_fonts = &self.recent_fonts;
        let mut canvas_pts: Option<egui::Rect> = None;
        // At most one layout edit (split/join/retype) from an area header this
        // frame; applied to the tree after the UI pass, never during it.
        let mut dock_cmd: Option<DockCmd> = None;
        let mut graph_edits = GraphEdits::default();
        // The composition node graph + its registry, borrowed read-only for the
        // panel; its edits (at most one) applied after the pass. Disjoint fields
        // from `dock`, so these coexist with the mutable dock borrow.
        let node_registry = &self.node_registry;
        let node_graph = &self.node_graph;
        let mut ng_edits = NgEdits::default();
        let full_output = self.egui_ctx.run_ui(raw_input, |ui| {
            let mut next_id = 0;
            let mut path = Vec::new();
            show_dock(
                dock,
                ui,
                &mut next_id,
                &mut path,
                &mut |editor, ui| match editor {
                    Editor::Comp => comp_ui(
                        ui,
                        doc_w,
                        doc_h,
                        doc_fps,
                        duration,
                        comp_bg,
                        comp_pp,
                        comp_path_range,
                        &mut comp,
                        &preset_names,
                        &mut preset_name_buf,
                        &mut layout,
                        &warnings,
                        &comp_entries,
                        current_comp,
                        &mut comp_name_buf,
                    ),
                    Editor::Layers => tree_ui(ui, &tree, selected_node, &mut tree_edits),
                    Editor::Transport => transport_ui(
                        ui,
                        frame,
                        last_frame,
                        timebase,
                        playing,
                        &key_frames,
                        work_area,
                        &mut transport,
                    ),
                    Editor::Dopesheet => dopesheet_ui(
                        ui,
                        &rows,
                        t,
                        last_frame,
                        timebase,
                        view,
                        &selected_keys,
                        clip,
                        work_area,
                        dope_label_w,
                        &mut dope,
                    ),
                    Editor::Properties => {
                        properties_ui(
                            ui,
                            &sel_info,
                            &mut edits,
                            &ease_info,
                            &mut ease_out,
                            &FontList { all: font_families, recent: recent_fonts },
                        )
                    }
                    Editor::Graph => graph_ui(ui, &graph_info, t, &mut graph_edits),
                    Editor::NodeGraph => nodegraph_ui(ui, node_graph, node_registry, &mut ng_edits),
                    // vello paints the frame here; egui only measures the hole
                    // and floats the zoom toolbar over it. `max_rect` is the
                    // whole window (egui doesn't shrink it for the sibling
                    // panels shown before this leaf); the leftover central
                    // region — what the canvas actually owns — is what's still
                    // available to lay into.
                    Editor::Canvas => {
                        // Split the leftover region: the canvas takes all but a
                        // bottom strip, and the stacked tool bar fills that strip
                        // so it sits below the frame instead of floating over it.
                        let full = ui.available_rect_before_wrap();
                        let split = (full.max.y - CANVAS_BAR_H).max(full.min.y);
                        // Rulers claim a band off the top and left. It comes out
                        // *here*, so the rect published as `canvas_pts` is the
                        // real drawing area — it feeds `fit` and therefore
                        // `pick`, and a click under a ruler must not select
                        // geometry hidden behind it.
                        let (rl, rt) = ruler_inset(aids.rulers);
                        canvas_pts = Some(egui::Rect::from_min_max(
                            egui::pos2(full.min.x + rl, full.min.y + rt),
                            egui::pos2(full.max.x, split),
                        ));
                        let bar = egui::Rect::from_min_max(
                            egui::pos2(full.min.x, split),
                            full.max,
                        );
                        canvas_toolbar(
                            ui,
                            bar,
                            zoom_pct,
                            is_fit,
                            &aids,
                            &mut canvas_edits,
                            &mut aid_edits,
                        );
                        // Aids underneath everything: they orient the frame, and
                        // must never sit over the things you grab.
                        if let Some(rect) = canvas_pts {
                            aids_hot = aids_ui(
                                ui,
                                rect,
                                &aids,
                                (doc_w, doc_h),
                                fit,
                                ppp,
                                &mut guide_drag,
                                &mut aid_edits,
                            );
                        }
                        // The selection box sits with the aids: it measures,
                        // it isn't grabbed.
                        if let Some(rect) = canvas_pts {
                            let painter = ui.painter_at(rect);
                            for b in &sel_boxes {
                                draw_bounds(&painter, *b, fit, ppp);
                            }
                        }
                        // Path next, gizmo last: the gizmo is what you grab, so
                        // it must never be obscured by the trajectory.
                        if let Some(rect) = canvas_pts {
                            motionpath::draw(
                                &ui.painter_at(rect),
                                &motion_path,
                                fit,
                                ppp,
                                path_now,
                            );
                        }
                        // The gizmo paints over the frame and reports into the
                        // ordinary property edits, so a handle drag auto-keys
                        // exactly like a DragValue drag does.
                        if let (Some(t), Some(rect)) = (&gizmo_target, canvas_pts) {
                            gizmo_hot =
                                gizmo_ui(
                                    ui,
                                    rect,
                                    t,
                                    fit,
                                    ppp,
                                    SnapCtx {
                                        aids: &aids,
                                        comp: (doc_w, doc_h),
                                        bounds: sel_extent,
                                        others: &other_extents,
                                        enabled: !snap_bypass,
                                    },
                                    &mut gizmo_drag,
                                    &mut edits,
                                );
                        }
                    }
                },
                &mut dock_cmd,
            );
        });
        // Open/close a module for editing (view state) before applying any op,
        // so a delete-then-nothing leaves the panel in a sane place.
        if let Some(change) = graph_edits.edit_module.take() {
            self.editing_module = change;
            window.request_redraw();
        }
        // Apply a graph edit (promote/bake/tree change, or a module-body edit)
        // after the UI pass. Node-scoped ops no-op without a selection; module
        // ops don't need one, so this runs regardless.
        if let Some(op) = graph_edits.op.take() {
            // A module delete must also close it if it was the one open, or the
            // panel would keep editing a body that no longer exists.
            if let GraphOp::DeleteModule { module } = &op {
                if self.editing_module == Some(*module) {
                    self.editing_module = None;
                }
            }
            apply_graph_op(&mut self.project, self.current, self.selected, op, frame);
            window.request_redraw();
        }
        // Apply a composition node-graph edit (add/move/remove/connect/
        // disconnect). Connection validity is enforced inside the model.
        if let Some(op) = ng_edits.op.take() {
            self.apply_ng_op(op);
            window.request_redraw();
        }
        // Now that egui has finished, restructure the layout tree if an area
        // header asked to. Doing it here (not mid-pass) keeps the panels and
        // their egui ids stable for the frame that was just drawn.
        if let Some(cmd) = dock_cmd {
            self.dock.apply(cmd);
            window.request_redraw();
        }
        // Restore the save-field buffer taken for the UI pass.
        self.preset_name_buf = preset_name_buf;
        self.comp_name_buf = comp_name_buf;
        // Layout presets: switch to one, or save the current arrangement as a
        // session preset. Both re-lay out the panels, so a redraw is due.
        if let Some(i) = layout.apply {
            if let Some(preset) = self.presets.get(i) {
                self.dock = preset.dock.clone();
                window.request_redraw();
            }
        }
        if let Some(name) = layout.save_as {
            let current = self.dock.clone();
            // Overwrite a user preset of the same name; never clobber a built-in.
            match self.presets.iter_mut().find(|p| !p.builtin && p.name == name) {
                Some(existing) => existing.dock = current,
                None => self.presets.push(Preset { name, dock: current, builtin: false }),
            }
        }
        // Points → physical pixels for the next frame's fit.
        self.canvas_rect = canvas_pts.map(|r| {
            kurbo::Rect::new(
                r.min.x as f64 * ppp,
                r.min.y as f64 * ppp,
                r.max.x as f64 * ppp,
                r.max.y as f64 * ppp,
            )
        });

        // Preview zoom toolbar: a menu pick sets the mode outright; the − / +
        // buttons step the live scale about the canvas centre (which turns Fit
        // into an explicit zoom, since a fixed step needs a fixed anchor).
        if let Some(mode) = canvas_edits.set_zoom {
            self.nav = match mode {
                None => CanvasNav::default(),
                Some(z) => CanvasNav { zoom: Some(z), pan: (0.0, 0.0) },
            };
            window.request_redraw();
        }
        if let Some(factor) = canvas_edits.zoom_by {
            let scale = canvas_scale(self.doc(), canvas, self.nav, ppp);
            let center = ((canvas.x0 + canvas.x1) * 0.5, (canvas.y0 + canvas.y1) * 0.5);
            let comp_pt = canvas_transform(self.doc(), canvas, self.nav, ppp).inverse()
                * Point::new(center.0, center.1);
            self.nav = nav_zoom_about(self.doc(), canvas, comp_pt, center, scale * factor, ppp);
            window.request_redraw();
        }

        // Composition settings.
        if let Some(w) = comp.width {
            self.doc_mut().width = w.max(1.0);
        }
        if let Some(h) = comp.height {
            self.doc_mut().height = h.max(1.0);
        }
        // `selected_keys` is the local taken for the UI pass — `self.selected_keys`
        // is empty until it's put back below, so the remap has to act on this one.
        let selected_node_id = self.selected;
        apply_fps_edit(
            self.project.comp_mut(self.current).expect("open comp"),
            &mut self.fps_drag,
            &comp,
            selected_node_id,
            &mut selected_keys,
        );
        if let Some(d) = comp.duration {
            self.doc_mut().duration = d.max(0.1);
        }
        // fps/duration changes resize the frame axis under the view, so the
        // window may now hang past the end of the comp.
        if comp.fps.is_some() || comp.duration.is_some() {
            self.view = self.view.clamped(self.doc().duration_frames());
        }

        if let Some(name) = comp.rename {
            self.doc_mut().name = name.trim().to_string();
        }
        // Opening a comp — from the switcher, or from a precomp layer's button.
        if let Some(id) = comp.open.or(tree_edits.open_comp) {
            self.open_comp(id);
        }
        // Pre-compose: the selected layer moves into a fresh comp and an
        // instance takes its place.
        if let Some(id) = tree_edits.precompose {
            self.precompose(id);
        }

        // Layers panel: selection + reorder.
        if let Some(id) = tree_edits.select {
            if Some(id) != self.selected {
                self.selected = Some(id);
                self.selected_keys.clear();
            }
        }

        // Clip bar: trim / slide / clear the selected layer's time range.
        if let Some(timing) = dope.set_timing {
            if let Some(node) = self.selected.and_then(|id| self.doc_mut().root.find_mut(id)) {
                node.timing = timing;
                window.request_redraw();
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
        if let Some(hits) = dope.box_select {
            // A live marquee owns the selection outright while it is being
            // dragged — shrinking the box has to deselect, so this replaces
            // rather than merges.
            self.selected_keys = hits;
        } else if let Some(k) = dope.select_key {
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
            // Restart the preview at the work-area start (frame 0 when there's
            // no work area), matching the R key.
            self.seek_frame(self.loop_bounds_frames().0);
        }
        if transport.jump_end {
            // `hi` is exclusive, so the last previewed frame is one before it.
            self.seek_frame(self.loop_bounds_frames().1 - 1);
        }
        if let Some(nf) = transport.scrub_to.or(dope.seek_to) {
            self.playing = false;
            self.seek_frame(nf);
        }
        // Start/End fields write the same work area the B/N keys do.
        let total = self.doc().duration_frames();
        if let Some(f) = transport.set_work_start {
            self.work_area = Some(with_work_start(self.work_area, f, total));
        }
        if let Some(f) = transport.set_work_end {
            self.work_area = Some(with_work_end(self.work_area, f, total));
        }
        if let Some(w) = dope.set_label_w {
            self.dope_label_w = w;
        }

        // Apply property edits + keyframe drags to the selected node, then
        // re-evaluate so the change is visible on this very frame.
        self.gizmo_drag = gizmo_drag;
        self.gizmo_hot = gizmo_hot;
        self.guide_drag = guide_drag;
        self.aids_hot = aids_hot;
        self.apply_aid_edits(&aid_edits);
        // The hovered font, straight from this frame's picker: `None` (nothing
        // hovered, or the picker closed) is what *ends* a preview, so this is a
        // plain assignment rather than a conditional one. A repaint is due
        // whenever it changes, or the preview would linger a frame.
        if self.font_preview != edits.text_family_preview {
            self.font_preview = edits.text_family_preview.clone();
            window.request_redraw();
        }
        // A click commits the font, so it joins the recents.
        if let Some(family) = edits.text_family.as_ref() {
            remember_font(&mut self.recent_fonts, family);
        }
        let mut dirty = self.apply_edits(frame, &edits);
        // Applied here rather than with the other comp settings above so it can
        // mark the scene dirty — the backdrop is baked into `vscene`, which is
        // only rebuilt when something says it changed.
        if let Some(rgb) = comp.bg {
            self.doc_mut().bg = rgb_color(rgb);
            dirty = true;
        }
        if let Some(pp) = comp.passepartout {
            self.doc_mut().passepartout = pp.clamp(0.0, 1.0);
            dirty = true;
        }
        if let Some(r) = comp.motion_path_range {
            self.doc_mut().motion_path_range = r.clamp(0, MAX_RANGE);
            dirty = true;
        }
        if let Some(delta) = dope.move_by {
            dirty |= self.move_selected_keys(delta);
        }

        // Keyframe copy/paste. Read off egui's input rather than the winit
        // handler because that one never sees a modifier state, and suppressed
        // while a text field has focus so Ctrl+V in a numeric box still pastes
        // text instead of keyframes.
        if !self.egui_ctx.egui_wants_keyboard_input() {
            let (copy, paste) = self.egui_ctx.input(|i| {
                (
                    i.modifiers.command && i.key_pressed(egui::Key::C),
                    i.modifiers.command && i.key_pressed(egui::Key::V),
                )
            });
            if copy {
                self.copy_selected_keys();
            }
            if paste {
                dirty |= self.paste_keys();
            }
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
            dirty |= self.doc_mut().root.reorder_child(id, delta);
        }
        if let Some(kind) = tree_edits.add {
            dirty |= self.add_node(kind);
        }
        if let Some(id) = tree_edits.delete {
            self.doc_mut().root.remove(id);
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
            // Every document change invalidates the motion-path cache.
            self.doc_rev = self.doc_rev.wrapping_add(1);
            // Re-derive the preview rather than reusing the one from the top of
            // the frame: an edit may have just changed the node it applies to,
            // and a font being hovered must survive an unrelated change.
            let previewing = self.preview_project();
            let scene =
                evaluate_comp(previewing.as_ref().unwrap_or(&self.project), self.current, t);
            let bg = self.doc().bg;
            let pp = self.doc().passepartout;
        self.vscene =
            to_vello(
                &scene,
                fit,
                (self.doc().width, self.doc().height),
                bg,
                pp,
                canvas,
                &self.onion.ghosts,
                self.selected,
            );
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
                    // The preview letterbox — the area around the comp frame.
                    base_color: theme::PREVIEW_BACKDROP,
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

/// Move the layer `id` out of `current` into a brand-new comp, leaving an
/// instance behind. Returns `(new comp, instance node)`, or `None` if `id`
/// isn't a movable layer (the root *is* the comp, so it can't be precomposed).
///
/// The new comp inherits the open one's size/fps/duration, so nested content
/// keeps its coordinate space and timing.
///
/// The instance takes the layer's name and its **place among its siblings**
/// (draw order), but a *neutral* transform: the layer's own transform travels
/// inside the comp with it, and applying it at both levels would double it.
/// This is also why pre-composing is visually a no-op, which is the point — it
/// reorganizes without changing the frame.
pub(crate) fn precompose_into(
    project: &mut MProject,
    current: CompId,
    id: NodeId,
    next_id: u64,
) -> Option<(CompId, NodeId)> {
    let open = project.comp(current)?;
    if id == open.root.id {
        return None;
    }
    let layer = open.root.find(id)?.clone();
    let (w, h, fps, duration) = (open.width, open.height, open.fps, open.duration);
    let name = if layer.name.trim().is_empty() { "Precomp".to_string() } else { layer.name.clone() };

    let mut inner = Comp::new(w, h, MNode::group(0, "root").with_child(layer));
    inner.fps = fps;
    inner.duration = duration;
    inner.name = name.clone();
    let comp_id = project.insert(inner);

    let instance = MNode::group(next_id, name).with_precomp(comp_id);
    let instance_id = instance.id;
    project.comp_mut(current)?.root.replace(id, instance);
    Some((comp_id, instance_id))
}
