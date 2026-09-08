//! Engine scene -> vello, and the canvas fit/pick transforms.
//!
//! Moved verbatim out of `main.rs` when it was split by concern; the
//! only edit was widening visibility to `pub(crate)`.

use crate::*;

/// The preview toolbar's reported intent for the frame. Applied to
/// [`App::nav`](crate::App) after the UI pass, never during it.
#[derive(Default)]
pub(crate) struct CanvasEdits {
    /// A choice from the zoom menu: `Some(None)` = Fit, `Some(Some(z))` = a
    /// fixed zoom in logical points-per-comp-pixel (100% = 1.0).
    pub set_zoom: Option<Option<f64>>,
    /// Step the current zoom by this factor, about the canvas centre (the
    /// − / + buttons).
    pub zoom_by: Option<f64>,
    /// Return the viewer to straight on — back to the view that actually
    /// renders.
    pub reset_orbit: bool,
    /// Switch the active canvas tool (Select ⇄ Pen).
    pub set_tool: Option<Tool>,
}

/// The fixed zoom stops offered in the toolbar menu, as percentages.
pub(crate) const ZOOM_STOPS: [i32; 6] = [25, 50, 100, 200, 400, 800];

/// Height of the preview's stacked tool strip, in logical points. The canvas
/// gives up this much at its bottom edge so the bar sits *below* the frame
/// rather than floating over it — a real docked strip we can grow later.
pub(crate) const CANVAS_BAR_H: f32 = 30.0;

/// The preview's stacked tool strip, filling `bar` at the bottom of the
/// canvas: `[-] [ Fit / 100% ▼ ] [+]`, controls left-aligned. Painted with the
/// panel fill so it reads as chrome, not a floating card. `zoom_pct` is the
/// live zoom read-out; `is_fit` picks the menu's checked row and button label.
#[allow(clippy::too_many_arguments)]
pub(crate) fn canvas_toolbar(
    ui: &mut egui::Ui,
    bar: egui::Rect,
    zoom_pct: i32,
    is_fit: bool,
    orbit: (f64, f64),
    tool: Tool,
    aids: &ViewAids,
    out: &mut CanvasEdits,
    aid_out: &mut AidEdits,
) {
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(bar));
    egui::Frame::new()
        .fill(child.visuals().panel_fill)
        .inner_margin(egui::Margin::symmetric(6, 3))
        .show(&mut child, |ui| {
            ui.horizontal_centered(|ui| {
                // Tool toggle first — it decides what a canvas click *does*, so
                // it belongs at the head of the strip.
                if ui
                    .selectable_label(tool == Tool::Select, "Select")
                    .on_hover_text("Select and transform layers")
                    .clicked()
                {
                    out.set_tool = Some(Tool::Select);
                }
                if ui
                    .selectable_label(tool == Tool::Pen, "Pen")
                    .on_hover_text("Draw a vector path — click to add points, click the first to close")
                    .clicked()
                {
                    out.set_tool = Some(Tool::Pen);
                }
                if ui
                    .selectable_label(tool == Tool::EditPath, "Points")
                    .on_hover_text(
                        "Edit points: drag anchors/handles; Alt-drag a corner to pull out \
                         handles; Alt-drag a handle to break it; click a segment to insert; \
                         Delete to remove",
                    )
                    .clicked()
                {
                    out.set_tool = Some(Tool::EditPath);
                }
                ui.separator();
                if ui.small_button("-").on_hover_text("Zoom out").clicked() {
                    out.zoom_by = Some(1.0 / 1.25);
                }
                let label = if is_fit { "Fit".to_string() } else { format!("{zoom_pct}%") };
                egui::ComboBox::from_id_salt("canvas_zoom")
                    .selected_text(label)
                    .show_ui(ui, |ui| {
                        if ui.selectable_label(is_fit, "Fit").clicked() {
                            out.set_zoom = Some(None);
                        }
                        for pct in ZOOM_STOPS {
                            let on = !is_fit && zoom_pct == pct;
                            if ui.selectable_label(on, format!("{pct}%")).clicked() {
                                out.set_zoom = Some(Some(pct as f64 / 100.0));
                            }
                        }
                    });
                if ui.small_button("+").on_hover_text("Zoom in").clicked() {
                    out.zoom_by = Some(1.25);
                }

                // The viewpoint badge. It appears **only** when the canvas is
                // showing something other than the rendered frame, because that
                // is the one state a user can otherwise mistake for the real
                // thing — Blender's "User Perspective", After Effects' Custom
                // View. Straight on, it costs no space at all.
                if orbit != (0.0, 0.0) {
                    ui.separator();
                    if ui
                        .button(format!("◳ {:.0}° {:.0}°", orbit.0, orbit.1))
                        .on_hover_text(
                            "Viewing from an orbited angle — this is not the rendered \
                             frame. Click to return to the camera's view.",
                        )
                        .clicked()
                    {
                        out.reset_orbit = true;
                    }
                }
                ui.separator();
                // Alignment aids. `selectable_label` rather than plain buttons
                // so the strip shows what is currently on at a glance.
                let grid = ui
                    .selectable_label(aids.grid.visible, "Grid")
                    .on_hover_text("Show the composition grid — right-click to set spacing");
                if grid.clicked() {
                    aid_out.toggle_grid = true;
                }
                // Spacing and subdivisions hang off the toggle rather than
                // taking two more slots in an already crowded composition bar.
                grid.context_menu(|ui| {
                    ui.label("Grid");
                    let mut spacing = aids.grid.spacing;
                    if ui
                        .add(
                            egui::DragValue::new(&mut spacing)
                                .speed(1.0)
                                .range(Grid::MIN_SPACING..=Grid::MAX_SPACING)
                                .prefix("spacing ")
                                .suffix(" px"),
                        )
                        .changed()
                    {
                        aid_out.set_grid_spacing = Some(spacing);
                    }
                    let mut subs = aids.grid.subdivisions;
                    if ui
                        .add(
                            egui::DragValue::new(&mut subs)
                                .speed(0.1)
                                .range(1..=100)
                                .prefix("subdivisions "),
                        )
                        .on_hover_text("1 = no minor lines")
                        .changed()
                    {
                        aid_out.set_grid_subdivisions = Some(subs);
                    }
                });
                if ui
                    .selectable_label(aids.rulers, "Rulers")
                    .on_hover_text("Show rulers — drag from one to make a guide")
                    .clicked()
                {
                    aid_out.toggle_rulers = true;
                }
                if ui
                    .selectable_label(aids.snap, "Snap")
                    .on_hover_text("Snap drags to the comp edges and to whatever aids are shown (hold Ctrl to bypass)")
                    .clicked()
                {
                    aid_out.toggle_snap = true;
                }
                let onion = ui
                    .selectable_label(aids.onion.visible, "Onion")
                    .on_hover_text("Ghost the frames either side of the playhead - right-click for counts");
                if onion.clicked() {
                    aid_out.toggle_onion = true;
                }
                onion.context_menu(|ui| {
                    ui.label("Onion skins");
                    let (mut before, mut after) = (aids.onion.before, aids.onion.after);
                    let b = ui.add(
                        egui::DragValue::new(&mut before)
                            .speed(0.1)
                            .range(0..=Onion::MAX_GHOSTS)
                            .prefix("before "),
                    );
                    let a = ui.add(
                        egui::DragValue::new(&mut after)
                            .speed(0.1)
                            .range(0..=Onion::MAX_GHOSTS)
                            .prefix("after "),
                    );
                    if b.changed() || a.changed() {
                        aid_out.set_onion_counts = Some((before, after));
                    }
                    let mut step = aids.onion.step;
                    if ui
                        .add(
                            egui::DragValue::new(&mut step)
                                .speed(0.2)
                                .range(1..=240)
                                .prefix("every ")
                                .suffix(" f"),
                        )
                        .on_hover_text("Frames between ghosts")
                        .changed()
                    {
                        aid_out.set_onion_step = Some(step);
                    }
                    let mut pct = aids.onion.opacity * 100.0;
                    if ui
                        .add(
                            egui::DragValue::new(&mut pct)
                                .speed(1.0)
                                .range(1.0..=100.0)
                                .prefix("opacity ")
                                .suffix("%"),
                        )
                        .changed()
                    {
                        aid_out.set_onion_opacity = Some(pct / 100.0);
                    }
                    ui.weak("Ghosts the selection, or the whole comp if nothing is selected.");
                });
                let guides = ui
                    .selectable_label(aids.guides.visible, "Guides")
                    .on_hover_text("Show guides — drag one back to a ruler to remove it");
                if guides.clicked() {
                    aid_out.toggle_guides = true;
                }
                guides.context_menu(|ui| {
                    let n = aids.guides.items.len();
                    if ui
                        .add_enabled(n > 0, egui::Button::new(format!("Clear {n} guides")))
                        .clicked()
                    {
                        aid_out.clear_guides = true;
                        ui.close();
                    }
                });
            });
        });
}

/// Convert one core `Color` into a vello/peniko color, folding in an opacity.
pub(crate) fn to_peniko(c: MColor, opacity: f64) -> Color {
    Color::new([c.r as f32, c.g as f32, c.b as f32, (c.a * opacity) as f32])
}

/// The editor-only decoration drawn around and over a composition: onion-skin
/// ghosts, the passepartout, the frame border, the selection outline.
///
/// Grouped into one parameter because it is exactly the set of things an
/// **export** must not draw. A render that baked in the frame border would put a
/// grey 1.5px rectangle around every delivered frame, and the border is drawn
/// unconditionally, so "pass the editor's values" and "pass nothing" needed to
/// be one decision rather than four defaults a caller could get individually
/// wrong. [`Chrome::none`] is the whole of what export passes.
pub(crate) struct Chrome<'a> {
    /// Onion-skin ghosts, drawn under the live frame.
    pub(crate) ghosts: &'a [Ghost],
    /// The selected layer, outlined on top of everything.
    pub(crate) selected: Option<NodeId>,
    /// How strongly to dim outside the comp bounds. `0.0` is off.
    pub(crate) passepartout: f64,
    /// The preview area in **physical pixels** — the passepartout needs to know
    /// how far to reach, and it is the only thing here that does.
    pub(crate) canvas: kurbo::Rect,
    /// Whether to stroke the comp bounds. Marks where the render will crop, so
    /// it is meaningless *in* a render.
    pub(crate) border: bool,
}

impl Chrome<'static> {
    /// No decoration at all: what an exported frame gets. Every field is the
    /// "draw nothing" value, so a frame rendered with this contains only the
    /// composition — which is what makes preview-equals-export a claim about
    /// the picture rather than about the picture plus the editor's furniture.
    pub(crate) fn none() -> Self {
        Chrome {
            ghosts: &[],
            selected: None,
            passepartout: 0.0,
            canvas: kurbo::Rect::ZERO,
            border: false,
        }
    }
}

/// Convert an evaluated engine `Scene` into a `vello::Scene`, prepending a
/// global transform that fits the composition into the window.
///
/// Draw order is load-bearing: composition background, then the **onion skin**
/// ghosts, then the shapes, then
/// the **passepartout** dimming everything outside the frame, then the frame
/// border, then the selection outline. The passepartout has to come after the
/// shapes (it dims the parts of them that hang outside the frame, which is the
/// whole point) but before the border and the selection, which stay crisp.
///
/// Everything in that list past the shapes is [`Chrome`], and an export passes
/// [`Chrome::none`] to get the composition and nothing else.
pub(crate) fn to_vello(
    scene: &MScene,
    fit: Affine,
    comp: (f64, f64),
    bg: MColor,
    chrome: &Chrome<'_>,
    footage: &mut FootageCache,
    assets: &std::collections::BTreeMap<motion_core::AssetId, motion_core::Asset>,
    // Layers whose effect stack needed the full-image path (a blur), already
    // rasterized and processed into a device-space image keyed by the layer's
    // node. When a group is here, its raw items are replaced by this image; when
    // it isn't — because it has no blur, or because the readback failed — the
    // items draw normally and the in-scene colour path still applies.
    effect_images: &std::collections::HashMap<NodeId, vello::peniko::ImageData>,
) -> VScene {
    let mut vs = VScene::new();

    // Composition frame: the comp's own background colour. A per-comp user
    // setting (`Comp::bg`), not a constant.
    let comp_rect = kurbo::Rect::new(0.0, 0.0, comp.0, comp.1);
    vs.fill(Fill::NonZero, fit, to_peniko(bg, 1.0), None, &comp_rect);
    let scale = fit.as_coeffs()[0].abs().max(1e-6);

    // Onion skins go under the live frame: they are context, and the frame you
    // are actually editing must never be the faint one.
    for ghost in chrome.ghosts {
        for item in &ghost.items {
            let xf = fit * item.transform;
            if let Some(fill) = item.fill {
                let c = tinted(fill, ghost.tint, TINT_AMOUNT);
                vs.fill(Fill::NonZero, xf, to_peniko(c, item.opacity * ghost.opacity), None, &item.path);
            }
            if let Some((color, width)) = item.stroke {
                let c = tinted(color, ghost.tint, TINT_AMOUNT);
                vs.stroke(
                    &KurboStroke::new(width),
                    xf,
                    to_peniko(c, item.opacity * ghost.opacity),
                    None,
                    &item.path,
                );
            }
        }
    }

    // Isolated layers, outermost first. `walk` emits them in post-order (a
    // child's group is pushed before its parent's, since a range isn't known
    // until its subtree is walked), so they have to be re-sorted before they
    // can be opened as nested layers: by where they start, and for a shared
    // start the longer range is the outer one.
    let groups = scene.nesting_order();
    let mut next_group = 0usize;
    // The layers currently open, innermost last. Holds the groups themselves
    // (not just their end index) so the item loop can reach each open layer's
    // effect stack while it draws inside it.
    let mut open: Vec<&LayerGroup> = Vec::new();
    // While `Some(end)`, the current layer's raw content has been replaced by a
    // processed image (a blurred layer); its own items and any nested groups are
    // skipped until `end`, since they are already baked into that image.
    let mut replaced_until: Option<usize> = None;

    for (i, item) in scene.items.iter().enumerate() {
        let inside_replaced = replaced_until.is_some_and(|e| i < e);
        // Open every layer that begins here. `push_layer` gives vello an
        // offscreen target: everything drawn until the matching `pop_layer`
        // composites as one image, which is what a blend mode needs and what a
        // mask and the colour effects hang off. An item inside a replaced region
        // opens nothing — its groups were consumed when the replacement drew.
        if !inside_replaced {
            while let Some(g) = groups.get(next_group).filter(|g| g.start == i) {
                let replacement = effect_images.get(&g.source);
                // A masked layer clips to its mask; an unmasked one clips to its
                // own extent — bounded rather than "everything", because vello
                // rasterizes the clip shape and an unbounded one would cost the
                // whole frame per group. A *replaced* (blurred) layer is the
                // exception: the blur spreads past the raw bounds, so an
                // unmasked one clips to the whole comp instead of shaving off the
                // halo.
                match &g.clip {
                    Some(mask) => vs.push_layer(
                        if mask.even_odd { Fill::EvenOdd } else { Fill::NonZero },
                        to_peniko_blend(g.blend, g.compose),
                        g.alpha.clamp(0.0, 1.0) as f32,
                        fit * mask.transform,
                        &mask.path,
                    ),
                    None => {
                        let bounds =
                            if replacement.is_some() { comp_rect } else { group_bounds(scene, g) };
                        vs.push_layer(
                            Fill::NonZero,
                            to_peniko_blend(g.blend, g.compose),
                            g.alpha.clamp(0.0, 1.0) as f32,
                            fit,
                            &bounds,
                        );
                    }
                }
                open.push(g);
                next_group += 1;

                if let Some(img) = replacement {
                    // The processed image is already in device space (rasterized
                    // through the same `fit`), so it draws at identity, inside
                    // this layer's push_layer so the blend and alpha still apply.
                    vs.draw_image(&vello::peniko::ImageBrush::new(img.clone()), Affine::IDENTITY);
                    replaced_until = Some(g.end);
                    // Skip every group nested inside this one: its effects are
                    // baked into the image, and leaving them for the loop would
                    // strand `next_group` on a range that can't match a later
                    // start.
                    while groups.get(next_group).is_some_and(|g2| g2.start < g.end) {
                        next_group += 1;
                    }
                    break;
                }
            }
        }

        if !inside_replaced {
            let xf = fit * item.transform;
            // Footage draws *instead of* the fill: the fill colour is what a
            // rectangle would paint, and a clip covers it entirely. A stroke
            // still applies, so a bordered video layer works like a bordered rect.
            let drew_footage = match item.image {
                Some(paint) => draw_footage(&mut vs, item, xf, paint, footage, assets),
                None => false,
            };
            if let Some(fill) = item.fill {
                if !drew_footage {
                    let c = with_effects(fill, &open);
                    vs.fill(Fill::NonZero, xf, to_peniko(c, item.opacity), None, &item.path);
                }
            }
            if let Some((color, width)) = item.stroke {
                vs.stroke(
                    &KurboStroke::new(width),
                    xf,
                    to_peniko(with_effects(color, &open), item.opacity),
                    None,
                    &item.path,
                );
            }
        }

        // Close every layer that ended with this item, innermost first.
        while open.last().map(|g| g.end) == Some(i + 1) {
            vs.pop_layer();
            if replaced_until == Some(i + 1) {
                replaced_until = None;
            }
            open.pop();
        }
    }
    // Belt and braces: an unbalanced push would corrupt everything drawn after
    // it, and the ranges come from data that a future edit could get wrong.
    for _ in 0..open.len() {
        vs.pop_layer();
    }
    // Passepartout: dim everything outside the comp bounds, so the frame reads
    // as the shot and whatever is parked off-stage recedes without vanishing.
    if chrome.passepartout > 0.0 {
        vs.fill(
            Fill::EvenOdd,
            Affine::IDENTITY,
            Color::new([0.0, 0.0, 0.0, chrome.passepartout.clamp(0.0, 1.0) as f32]),
            None,
            &passepartout_path(fit, comp_rect, chrome.canvas),
        );
    }

    // Frame border, over both the shapes and the passepartout — it marks where
    // the render will crop, so nothing should paint over it. An export is
    // *inside* that crop, so it never draws one.
    if chrome.border {
        vs.stroke(
            &KurboStroke::new(1.5 / scale),
            fit,
            Color::new([0.35, 0.37, 0.42, 1.0]),
            None,
            &comp_rect,
        );
    }

    // Selection outline on top of everything.
    if let Some(sel) = chrome.selected {
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

/// The passepartout region: everything in `canvas` that is *not* inside the
/// composition, as a single path in **physical pixels** (hence the identity
/// transform at the fill site — `fit` is already applied to the hole).
///
/// Two subpaths, filled even-odd, so the comp rect punches a hole in the canvas
/// rect. Building it as one path rather than four border rectangles matters at
/// fractional zoom: abutting rects leave hairline seams where their edges land
/// mid-pixel, and the seams shimmer as you pan.
///
/// The outer rect is `canvas` **unioned** with the comp so a comp larger than
/// the visible area still closes the hole. Without that the hole would extend
/// past the outer boundary and even-odd would invert, dimming the frame itself
/// and leaving the surroundings clear.
pub(crate) fn passepartout_path(fit: Affine, comp: kurbo::Rect, canvas: kurbo::Rect) -> BezPath {
    // `fit` is translate + uniform scale, never a rotation, so the comp stays
    // axis-aligned and its image is fully described by two corners.
    let a = fit * Point::new(comp.x0, comp.y0);
    let b = fit * Point::new(comp.x1, comp.y1);
    let hole = kurbo::Rect::new(a.x.min(b.x), a.y.min(b.y), a.x.max(b.x), a.y.max(b.y));
    let outer = canvas.union(hole);

    let mut path = BezPath::new();
    for r in [outer, hole] {
        path.move_to((r.x0, r.y0));
        path.line_to((r.x1, r.y0));
        path.line_to((r.x1, r.y1));
        path.line_to((r.x0, r.y1));
        path.close_path();
    }
    path
}

/// Pick the front-most scene item under a point given in physical pixels.
/// Returns the `NodeId` that produced it, or `None` for empty space.
pub(crate) fn pick(scene: &MScene, fit: Affine, px: (f64, f64)) -> Option<NodeId> {
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

/// "Contain" fit into the canvas area: scale the doc uniformly to fit `area`
/// and center it there. `area` is in **physical pixels** — the canvas leaf's
/// rect from the layout tree, scaled by pixels-per-point.
///
/// This used to subtract hardcoded panel sizes from the window. It couldn't
/// survive dockable panels: the moment a splitter moves, constants and reality
/// disagree and the canvas drifts out from under the cursor (which also breaks
/// click-picking, since `pick` inverts this very transform).
pub(crate) fn fit_transform(doc: &Document, area: kurbo::Rect) -> Affine {
    let avail_w = area.width().max(1.0);
    let avail_h = area.height().max(1.0);
    let scale = (avail_w / doc.width).min(avail_h / doc.height);
    let dx = area.x0 + (avail_w - doc.width * scale) * 0.5;
    let dy = area.y0 + (avail_h - doc.height * scale) * 0.5;
    Affine::translate((dx, dy)) * Affine::scale(scale)
}

/// The gap, in **logical points**, that "Fit" leaves between the composition
/// and the edges of the preview panel — so the frame never touches the
/// splitters of the panels around it.
pub(crate) const FIT_MARGIN: f64 = 20.0;

/// How far the user can zoom, as physical-pixels-per-composition-pixel. The
/// lower bound keeps a huge comp from vanishing; the upper bound keeps a small
/// one from filling the screen with a single pixel.
pub(crate) const MIN_SCALE: f64 = 0.02;
pub(crate) const MAX_SCALE: f64 = 64.0;

/// The preview panel's zoom + pan. `zoom == None` is **Fit** — the transform
/// is recomputed each frame from the (possibly resized) canvas rect, so the
/// comp stays framed as panels move. `Some(z)` pins the scale at `z`
/// *logical* points per composition pixel (100% = 1.0), positioned by `pan`,
/// an offset in **physical pixels** from the centered placement.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct CanvasNav {
    pub zoom: Option<f64>,
    pub pan: (f64, f64),
    /// Where the **viewer** is standing, in degrees: yaw about the composition's
    /// vertical axis, pitch about its horizontal one. `(0, 0)` is straight on.
    ///
    /// View state, never saved: this is where you are looking from, not
    /// something about the document. A render always evaluates straight on, so
    /// orbiting can never change what ships.
    ///
    /// It exists because a straight-down view physically cannot show rotation
    /// about X or Y — those rings lie in planes containing the depth axis, so
    /// looking along that axis puts them exactly edge-on. Tilting the viewer is
    /// the only honest way to open them.
    pub orbit: (f64, f64),
}

impl Default for CanvasNav {
    fn default() -> Self {
        Self { zoom: None, pan: (0.0, 0.0), orbit: (0.0, 0.0) }
    }
}

impl CanvasNav {
    /// Pitch is clamped just short of a pole. At exactly 90 degrees the
    /// composition is edge-on — a line — and yaw stops meaning anything, which
    /// is the flat-spin every orbit control has to refuse.
    pub const MAX_PITCH: f64 = 89.0;

    /// The orbit as a matrix, for [`motion_core::evaluate_comp_orbited`].
    ///
    /// Yaw first, then pitch, so dragging sideways always spins about the
    /// composition's own vertical rather than about a tilted one — the reading
    /// that stays predictable after you have already pitched.
    pub fn orbit_matrix(&self) -> motion_core::Mat4 {
        motion_core::Mat4::rotate_x(self.orbit.1.to_radians())
            * motion_core::Mat4::rotate_y(self.orbit.0.to_radians())
    }

}

/// The rectangle "Fit" actually fits into: the canvas rect pulled in by
/// [`FIT_MARGIN`] on every side (scaled to physical pixels), guarded so a
/// panel narrower than the margins can't invert it.
fn fit_area(area: kurbo::Rect, ppp: f64) -> kurbo::Rect {
    let m = (FIT_MARGIN * ppp).min(area.width() * 0.5 - 1.0).min(area.height() * 0.5 - 1.0);
    let m = m.max(0.0);
    kurbo::Rect::new(area.x0 + m, area.y0 + m, area.x1 - m, area.y1 - m)
}

/// The composition→canvas transform for the current navigation state, in
/// physical pixels. `Fit` centres the comp in the inset [`fit_area`]; a fixed
/// zoom centres it and then applies `pan`.
pub(crate) fn canvas_transform(doc: &Document, area: kurbo::Rect, nav: CanvasNav, ppp: f64) -> Affine {
    match nav.zoom {
        None => fit_transform(doc, fit_area(area, ppp)),
        Some(z) => {
            let scale = (z * ppp).clamp(MIN_SCALE, MAX_SCALE);
            let cx = (area.x0 + area.x1) * 0.5 + nav.pan.0;
            let cy = (area.y0 + area.y1) * 0.5 + nav.pan.1;
            let dx = cx - doc.width * scale * 0.5;
            let dy = cy - doc.height * scale * 0.5;
            Affine::translate((dx, dy)) * Affine::scale(scale)
        }
    }
}

/// The current scale in physical-pixels-per-composition-pixel, whichever mode
/// the nav is in. Used for the zoom read-out and to seed a pan/zoom that takes
/// over from Fit.
pub(crate) fn canvas_scale(doc: &Document, area: kurbo::Rect, nav: CanvasNav, ppp: f64) -> f64 {
    canvas_transform(doc, area, nav, ppp).as_coeffs()[0].abs()
}

/// Build a nav that pins `comp_pt` (a point in composition space) under
/// `cursor_px` (physical pixels) at the given physical `scale` — i.e. zoom
/// about the cursor. The resulting `pan` is measured from the centred
/// placement so it stays consistent with [`canvas_transform`].
pub(crate) fn nav_zoom_about(
    doc: &Document,
    area: kurbo::Rect,
    comp_pt: Point,
    cursor_px: (f64, f64),
    scale: f64,
    ppp: f64,
    // Carried through untouched: where you stand is independent of how close
    // you are standing, so zooming must not straighten the view out.
    orbit: (f64, f64),
) -> CanvasNav {
    let scale = scale.clamp(MIN_SCALE, MAX_SCALE);
    // Translation the transform must have for comp_pt to land on cursor_px.
    let dx = cursor_px.0 - scale * comp_pt.x;
    let dy = cursor_px.1 - scale * comp_pt.y;
    // Back out the pan the centred placement would need to reach that dx/dy.
    let cx = (area.x0 + area.x1) * 0.5;
    let cy = (area.y0 + area.y1) * 0.5;
    let pan = (
        dx - (cx - doc.width * scale * 0.5),
        dy - (cy - doc.height * scale * 0.5),
    );
    CanvasNav { zoom: Some(scale / ppp), pan, orbit }
}

/// Map a document blend mode onto peniko's.
///
/// A plain one-to-one translation, because `BlendMode` was deliberately
/// defined as the standard sixteen rather than a bespoke set — so this is a
/// rename, not an approximation. Compositing stays `SrcOver`: these modes
/// change how colours combine, not how coverage does.
fn to_peniko_blend(mode: MBlendMode, compose: MComposeMode) -> vello::peniko::BlendMode {
    use vello::peniko::{BlendMode as B, Compose, Mix};
    let mix = match mode {
        MBlendMode::Normal => Mix::Normal,
        MBlendMode::Multiply => Mix::Multiply,
        MBlendMode::Screen => Mix::Screen,
        MBlendMode::Overlay => Mix::Overlay,
        MBlendMode::Darken => Mix::Darken,
        MBlendMode::Lighten => Mix::Lighten,
        MBlendMode::ColorDodge => Mix::ColorDodge,
        MBlendMode::ColorBurn => Mix::ColorBurn,
        MBlendMode::HardLight => Mix::HardLight,
        MBlendMode::SoftLight => Mix::SoftLight,
        MBlendMode::Difference => Mix::Difference,
        MBlendMode::Exclusion => Mix::Exclusion,
        MBlendMode::Hue => Mix::Hue,
        MBlendMode::Saturation => Mix::Saturation,
        MBlendMode::Color => Mix::Color,
        MBlendMode::Luminosity => Mix::Luminosity,
    };
    // Coverage is the other half: a matte layer composites with `DestIn` or
    // `DestOut` so it contributes its shape and not its colour.
    let compose = match compose {
        MComposeMode::SrcOver => Compose::SrcOver,
        MComposeMode::DestIn => Compose::DestIn,
        MComposeMode::DestOut => Compose::DestOut,
    };
    B::new(mix, compose)
}

/// Apply the colour-adjustment effects of every open layer to one item colour,
/// innermost outward — the in-scene fast path for the effect stack (see
/// [`crate::fx`]). A blur in the stack is skipped here; it needs the whole
/// rasterized layer, which the readback compositor will supply. Most items sit
/// inside no effect layer, so the common case is a single `is_empty` check.
fn with_effects(color: MColor, open: &[&LayerGroup]) -> MColor {
    let mut c = color;
    for g in open.iter().rev() {
        if !g.effects.is_empty() {
            c = crate::fx::apply_color_effects(c, &g.effects);
        }
    }
    c
}

/// Draw one item's raw fill/stroke/footage into a scene at `fit`, with **no**
/// effects applied. Used to build the sub-scene a full-image effect layer is
/// rasterized from — [`crate::fx::apply_stack`] applies the whole stack (colour
/// *and* blur) to the readback, so the sub-scene must be the untouched pixels.
fn draw_item_raw(
    vs: &mut VScene,
    item: &motion_core::RenderItem,
    fit: Affine,
    footage: &mut FootageCache,
    assets: &std::collections::BTreeMap<motion_core::AssetId, motion_core::Asset>,
) {
    let xf = fit * item.transform;
    let drew_footage = match item.image {
        Some(paint) => draw_footage(vs, item, xf, paint, footage, assets),
        None => false,
    };
    if let Some(fill) = item.fill {
        if !drew_footage {
            vs.fill(Fill::NonZero, xf, to_peniko(fill, item.opacity), None, &item.path);
        }
    }
    if let Some((color, width)) = item.stroke {
        vs.stroke(&KurboStroke::new(width), xf, to_peniko(color, item.opacity), None, &item.path);
    }
}

/// Rasterize and process every layer whose effect stack needs the full image (a
/// blur), returning a device-space image per such layer for [`to_vello`] to draw
/// in place of the raw items.
///
/// This is the readback half of the compositor. vello renders one scene
/// atomically and has no layer-filter primitive, so a blur — which reworks a
/// pixel from its neighbours — can only be done by rendering the layer on its
/// own, reading the pixels back, filtering them, and drawing the result back in.
/// The colour adjustments don't need this (see [`with_effects`]); they ride the
/// in-scene path and are applied here only because [`crate::fx::apply_stack`]
/// runs the whole stack in order on the readback.
///
/// Robust by construction: any layer whose render or readback fails is simply
/// left out of the map, and [`to_vello`] then draws its raw items — so the worst
/// case is that a blur doesn't show, never a crash or a blank frame.
///
/// **Runtime assumptions to verify on a GPU** (untestable in a headless build):
/// vello writes straight-alpha `Rgba8Unorm`, and the target texture wants
/// `STORAGE_BINDING | COPY_SRC`. If edges come out dark, vello's output is
/// premultiplied and the readback needs to divide alpha out before filtering.
#[allow(clippy::too_many_arguments)]
pub(crate) fn rasterize_effect_layers(
    scene: &MScene,
    fit: Affine,
    width: u32,
    height: u32,
    device: &vello::wgpu::Device,
    queue: &vello::wgpu::Queue,
    renderer: &mut vello::Renderer,
    footage: &mut FootageCache,
    assets: &std::collections::BTreeMap<motion_core::AssetId, motion_core::Asset>,
) -> std::collections::HashMap<NodeId, vello::peniko::ImageData> {
    use vello::wgpu;
    let mut out = std::collections::HashMap::new();
    if width == 0 || height == 0 {
        return out;
    }
    for g in &scene.groups {
        if !crate::fx::needs_readback(&g.effects) {
            continue;
        }
        // The layer's raw pixels, rasterized through the same `fit` as the main
        // frame so the result lands on the identical device pixels.
        let mut sub = VScene::new();
        for item in &scene.items[g.start..g.end] {
            draw_item_raw(&mut sub, item, fit, footage, assets);
        }

        let tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("effect-layer"),
            size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
        let rendered = renderer.render_to_texture(
            device,
            queue,
            &sub,
            &view,
            &vello::RenderParams {
                // Transparent, so only the layer's own pixels are present — a
                // backdrop colour here would tint the whole processed image.
                base_color: vello::peniko::Color::TRANSPARENT,
                width,
                height,
                antialiasing_method: vello::AaConfig::Area,
            },
        );
        if rendered.is_err() {
            continue;
        }
        let Some(mut rgba) = read_texture_rgba(device, queue, &tex, width, height) else {
            continue;
        };
        crate::fx::apply_stack(&mut rgba, width as usize, height as usize, &g.effects);
        out.insert(
            g.source,
            vello::peniko::ImageData {
                data: vello::peniko::Blob::new(std::sync::Arc::new(rgba)),
                format: vello::peniko::ImageFormat::Rgba8,
                alpha_type: vello::peniko::ImageAlphaType::Alpha,
                width,
                height,
            },
        );
    }
    out
}

/// Copy an `Rgba8Unorm` texture back to a tight (unpadded) CPU RGBA8 buffer.
///
/// `copy_texture_to_buffer` requires each row padded to
/// `COPY_BYTES_PER_ROW_ALIGNMENT` (256 bytes), so the buffer is over-sized and
/// the padding stripped row by row on the way out. The map is waited on
/// synchronously — a stall, but a readback has to finish before the pixels can
/// be filtered this frame.
pub(crate) fn read_texture_rgba(
    device: &vello::wgpu::Device,
    queue: &vello::wgpu::Queue,
    tex: &vello::wgpu::Texture,
    width: u32,
    height: u32,
) -> Option<Vec<u8>> {
    use vello::wgpu;
    let unpadded = width * 4;
    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let padded = unpadded.div_ceil(align) * align;
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("effect-readback"),
        size: padded as u64 * height as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut enc = device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("effect-copy") });
    enc.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: tex,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded),
                rows_per_image: Some(height),
            },
        },
        wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
    );
    queue.submit([enc.finish()]);

    let slice = buffer.slice(..);
    slice.map_async(wgpu::MapMode::Read, |_| {});
    if device.poll(wgpu::PollType::wait_indefinitely()).is_err() {
        return None;
    }
    let data = slice.get_mapped_range();
    let mut rgba = vec![0u8; unpadded as usize * height as usize];
    for y in 0..height as usize {
        let src = y * padded as usize;
        let dst = y * unpadded as usize;
        rgba[dst..dst + unpadded as usize].copy_from_slice(&data[src..src + unpadded as usize]);
    }
    drop(data);
    buffer.unmap();
    Some(rgba)
}

/// The extent of an isolated layer's contents, in composition space.
///
/// Used as the clip when opening the layer. Strokes straddle the path they
/// follow, so half a stroke's width is added back — a clip tight to the fill
/// would shave the outer half off every outlined shape in the group.
pub(crate) fn group_bounds(scene: &MScene, group: &LayerGroup) -> kurbo::Rect {
    let mut bounds: Option<kurbo::Rect> = None;
    for item in &scene.items[group.start..group.end] {
        let mut b = (item.transform * item.path.clone()).bounding_box();
        if let Some((_, width)) = item.stroke {
            b = b.inflate(width / 2.0, width / 2.0);
        }
        bounds = Some(match bounds {
            Some(acc) => acc.union(b),
            None => b,
        });
    }
    bounds.unwrap_or_default()
}

/// Draw one footage item, returning whether pixels actually landed.
///
/// `false` means the caller should paint the layer's plain rectangle instead —
/// a missing file or a frame that wouldn't decode still has a *place*, and
/// showing it is how a broken import stays findable rather than looking like a
/// layer that silently stopped existing.
fn draw_footage(
    vs: &mut VScene,
    item: &motion_core::RenderItem,
    xf: Affine,
    paint: motion_core::ImagePaint,
    footage: &mut FootageCache,
    assets: &std::collections::BTreeMap<motion_core::AssetId, motion_core::Asset>,
) -> bool {
    let Some(asset) = assets.get(&paint.asset) else { return false };
    let opacity = item.opacity.clamp(0.0, 1.0) as f32;
    // The flag says whether this is the frame asked for or a neighbour standing
    // in while the real one decodes. Drawn either way: holding the previous
    // frame for a moment is how scrubbing stays continuous, where blanking to
    // the fill colour would read as flickering.
    let Some((image, _exact)) = footage.image(asset, paint) else { return false };
    // `draw_image` fills the source's own 0..w × 0..h rect, so the transform has
    // to carry the image onto the item's rectangle: scale native pixels to the
    // layer's size, then shift the centred rect's corner to the origin. Squeezed
    // rather than letterboxed, because the layer's `size` is what the user set
    // and what every overlay already draws.
    let target = item.path.bounding_box();
    let (nw, nh) = (image.width.max(1) as f64, image.height.max(1) as f64);
    let place = Affine::translate((target.x0, target.y0))
        * Affine::scale_non_uniform(target.width() / nw, target.height() / nh);
    let brush = vello::peniko::ImageBrush::new(image.clone()).with_alpha(opacity);
    vs.draw_image(&brush, xf * place);
    true
}
