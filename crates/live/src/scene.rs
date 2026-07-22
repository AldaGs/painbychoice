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
/// `canvas` is the preview area in **physical pixels** — the passepartout needs
/// to know how far to reach, and it is the only thing here that does.
#[allow(clippy::too_many_arguments)]
pub(crate) fn to_vello(
    scene: &MScene,
    fit: Affine,
    comp: (f64, f64),
    bg: MColor,
    passepartout: f64,
    canvas: kurbo::Rect,
    ghosts: &[Ghost],
    selected: Option<NodeId>,
    footage: &mut FootageCache,
    assets: &std::collections::BTreeMap<motion_core::AssetId, motion_core::Asset>,
) -> VScene {
    let mut vs = VScene::new();

    // Composition frame: the comp's own background colour. A per-comp user
    // setting (`Comp::bg`), not a constant.
    let comp_rect = kurbo::Rect::new(0.0, 0.0, comp.0, comp.1);
    vs.fill(Fill::NonZero, fit, to_peniko(bg, 1.0), None, &comp_rect);
    let scale = fit.as_coeffs()[0].abs().max(1e-6);

    // Onion skins go under the live frame: they are context, and the frame you
    // are actually editing must never be the faint one.
    for ghost in ghosts {
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

    for item in &scene.items {
        let xf = fit * item.transform;
        // Footage draws *instead of* the fill: the fill colour is what a
        // rectangle would paint, and a clip covers it entirely. A stroke still
        // applies, so a bordered video layer works like a bordered rect.
        let drew_footage = match item.image {
            Some(paint) => draw_footage(&mut vs, item, xf, paint, footage, assets),
            None => false,
        };
        if let Some(fill) = item.fill {
            if !drew_footage {
                vs.fill(Fill::NonZero, xf, to_peniko(fill, item.opacity), None, &item.path);
            }
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
    // Passepartout: dim everything outside the comp bounds, so the frame reads
    // as the shot and whatever is parked off-stage recedes without vanishing.
    if passepartout > 0.0 {
        vs.fill(
            Fill::EvenOdd,
            Affine::IDENTITY,
            Color::new([0.0, 0.0, 0.0, passepartout.clamp(0.0, 1.0) as f32]),
            None,
            &passepartout_path(fit, comp_rect, canvas),
        );
    }

    // Frame border, over both the shapes and the passepartout — it marks where
    // the render will crop, so nothing should paint over it.
    vs.stroke(
        &KurboStroke::new(1.5 / scale),
        fit,
        Color::new([0.35, 0.37, 0.42, 1.0]),
        None,
        &comp_rect,
    );

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
}

impl Default for CanvasNav {
    fn default() -> Self {
        Self { zoom: None, pan: (0.0, 0.0) }
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
    CanvasNav { zoom: Some(scale / ppp), pan }
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
