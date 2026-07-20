//! Engine scene -> vello, and the canvas fit/pick transforms.
//!
//! Moved verbatim out of `main.rs` when it was split by concern; the
//! only edit was widening visibility to `pub(crate)`.

use crate::*;

/// Convert one core `Color` into a vello/peniko color, folding in an opacity.
pub(crate) fn to_peniko(c: MColor, opacity: f64) -> Color {
    Color::new([c.r as f32, c.g as f32, c.b as f32, (c.a * opacity) as f32])
}

/// Convert an evaluated engine `Scene` into a `vello::Scene`, prepending a
/// global transform that fits the composition into the window. The composition
/// bounds are drawn first (so the editable frame is visible), then the shapes,
/// then the selection outline on top.
pub(crate) fn to_vello(scene: &MScene, fit: Affine, comp: (f64, f64), selected: Option<NodeId>) -> VScene {
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
