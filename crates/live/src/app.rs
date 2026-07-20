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
    /// Canvas area in physical pixels, measured from the layout tree's canvas
    /// leaf during the last UI pass. `None` until the first pass has run.
    pub(crate) canvas_rect: Option<kurbo::Rect>,
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
            presets: builtin_presets(),
            preset_name_buf: String::new(),
            comp_name_buf: String::new(),
            canvas_rect: None,
            next_id,
            editing_module: None,
            work_area: None,
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

    /// Evaluate + rasterize the current frame, then composite the egui overlay.
    pub(crate) fn render(&mut self, window: &Window) {
        // The whole render path is in the frame domain; seconds only ever
        // appear in the timecode string.
        let frame = self.current_frame();
        let t = frame as f64;
        let last_frame = self.doc().duration_frames().max(1);
        let scene = evaluate_comp(&self.project, self.current, t);
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
        let fit = fit_transform(self.doc(), canvas);

        // Resolve any pending click into a selection (or a deselect). Changing
        // the selected node invalidates any keyframe selection.
        if let Some(px) = self.pending_pick.take() {
            let picked = pick(&scene, fit, px);
            if picked != self.selected {
                self.selected = picked;
                self.selected_keys.clear();
            }
        }

        self.vscene = to_vello(&scene, fit, (self.doc().width, self.doc().height), self.selected);

        // Snapshot the selected node's properties before the UI closure so the
        // egui code borrows a plain struct, never `self`.
        let sel_node = self.selected.and_then(|id| self.doc().root.find(id));
        // Pass the doc so an expression-driven property resolves against the
        // scene (a doc-less context would show its fallback instead).
        let sel_info = sel_node.map(|node| NodeInfo::resolve(node, self.doc(), t));
        let rows = sel_node.map(dope_rows).unwrap_or_default();
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
        let timebase = self.doc().timebase();
        let view = self.view;
        let work_area = self.work_area;
        let playing = self.playing;
        let mut transport = Transport::default();
        let mut edits = PropEdits::default();
        let mut dope = DopeEdits::default();
        let mut tree_edits = TreeEdits::default();
        let selected_keys = std::mem::take(&mut self.selected_keys);
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
        let dock = &mut self.dock;
        let mut canvas_pts: Option<egui::Rect> = None;
        // At most one layout edit (split/join/retype) from an area header this
        // frame; applied to the tree after the UI pass, never during it.
        let mut dock_cmd: Option<DockCmd> = None;
        let mut graph_edits = GraphEdits::default();
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
                    Editor::Transport => {
                        transport_ui(ui, frame, last_frame, timebase, playing, &mut transport)
                    }
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
                        &mut dope,
                    ),
                    Editor::Properties => {
                        properties_ui(ui, &sel_info, &mut edits, &ease_info, &mut ease_out)
                    }
                    Editor::Graph => graph_ui(ui, &graph_info, t, &mut graph_edits),
                    // vello paints here; egui only measures the hole.
                    Editor::Canvas => canvas_pts = Some(ui.max_rect()),
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

        // Composition settings.
        if let Some(w) = comp.width {
            self.doc_mut().width = w.max(1.0);
        }
        if let Some(h) = comp.height {
            self.doc_mut().height = h.max(1.0);
        }
        if let Some(f) = comp.fps {
            self.doc_mut().fps = f.max(1.0);
        }
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
            let scene = evaluate_comp(&self.project, self.current, t);
            self.vscene = to_vello(&scene, fit, (self.doc().width, self.doc().height), self.selected);
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
