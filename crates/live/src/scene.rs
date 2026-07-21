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
pub(crate) fn canvas_toolbar(
    ui: &mut egui::Ui,
    bar: egui::Rect,
    zoom_pct: i32,
    is_fit: bool,
    out: &mut CanvasEdits,
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
            });
        });
}

/// Convert one core `Color` into a vello/peniko color, folding in an opacity.
pub(crate) fn to_peniko(c: MColor, opacity: f64) -> Color {
    Color::new([c.r as f32, c.g as f32, c.b as f32, (c.a * opacity) as f32])
}

/// Convert an evaluated engine `Scene` into a `vello::Scene`, prepending a
/// global transform that fits the composition into the window. The composition
/// bounds are drawn first (so the editable frame is visible), then the shapes,
/// then the selection outline on top.
pub(crate) fn to_vello(
    scene: &MScene,
    fit: Affine,
    comp: (f64, f64),
    bg: MColor,
    selected: Option<NodeId>,
) -> VScene {
    let mut vs = VScene::new();

    // Composition frame: the comp's own background colour plus a border, so the
    // comp bounds stand out from the letterbox and resolution changes are
    // visible. The fill is a per-comp user setting (`Comp::bg`), not a constant.
    let comp_rect = kurbo::Rect::new(0.0, 0.0, comp.0, comp.1);
    vs.fill(Fill::NonZero, fit, to_peniko(bg, 1.0), None, &comp_rect);
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
