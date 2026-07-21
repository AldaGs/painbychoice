//! Alignment aids over the preview: the grid, the rulers, and guides.
//!
//! All three are **egui overlays**, like the gizmo and the motion path — egui's
//! pass runs after the vello render, so a painter lands on top of the frame for
//! free. All three read `Comp::aids`, which is saved in the `.pbc`: a guide you
//! dropped to line up a title is part of how the composition was built, and
//! losing it on reopen would defeat the one job guides have.
//!
//! ## Rulers take space; everything else floats
//!
//! The rulers are a band along the canvas's top and left edges, subtracted from
//! the drawing area the same way the zoom strip already subtracts
//! [`CANVAS_BAR_H`]. That matters beyond looks: the remaining rect *is* the
//! canvas rect, so it feeds `canvas_transform` and therefore `pick`. Draw the
//! rulers over the canvas instead and every click under them would land on
//! geometry the user can't see. Toggling rulers consequently resizes the
//! drawing area, which is why [`ruler_inset`] is the single place that decides
//! it — `App` and this module must never disagree about where the canvas is.
//!
//! ## Everything is in composition coordinates
//!
//! Guide positions and grid spacing are stored in comp pixels, never screen
//! pixels, so they stay put under zoom and pan and mean the same thing at any
//! magnification. Conversion happens at the edges here.

use crate::*;

/// Thickness of the ruler bands, in logical points.
pub(crate) const RULER: f32 = 18.0;

/// How close (in points) the pointer must come to a guide to grab it.
const GUIDE_GRAB: f32 = 5.0;

const GRID_MAJOR: egui::Color32 = egui::Color32::from_rgba_premultiplied(120, 130, 150, 90);
const GRID_MINOR: egui::Color32 = egui::Color32::from_rgba_premultiplied(120, 130, 150, 38);
const GUIDE_COL: egui::Color32 = egui::Color32::from_rgb(90, 190, 255);
const GUIDE_HOT: egui::Color32 = egui::Color32::from_rgb(255, 255, 255);
const RULER_BG: egui::Color32 = egui::Color32::from_rgb(38, 38, 38);
const RULER_TICK: egui::Color32 = egui::Color32::from_rgb(120, 120, 128);
const RULER_TEXT: egui::Color32 = egui::Color32::from_rgb(160, 160, 168);

/// The space the rulers claim from the canvas leaf, in logical points: `(left,
/// top)`. Zero when rulers are off.
///
/// The one authority on the question. `App` insets the canvas rect with this
/// before computing `fit`, and this module positions its bands with it, so the
/// two cannot drift.
pub(crate) fn ruler_inset(show: bool) -> (f32, f32) {
    if show {
        (RULER, RULER)
    } else {
        (0.0, 0.0)
    }
}

/// What the aids want changed on the document, gathered during the UI pass and
/// applied after it — the same defer-then-apply discipline as every panel.
#[derive(Default)]
pub(crate) struct AidEdits {
    pub(crate) add_guide: Option<Guide>,
    /// Move guide `.0` to a new coordinate.
    pub(crate) move_guide: Option<(usize, f64)>,
    pub(crate) remove_guide: Option<usize>,
    pub(crate) toggle_grid: bool,
    pub(crate) toggle_rulers: bool,
    pub(crate) toggle_guides: bool,
    pub(crate) set_grid_spacing: Option<f64>,
    pub(crate) set_grid_subdivisions: Option<u32>,
    /// Delete every guide at once — the escape hatch for when dragging them
    /// back to a ruler one at a time is not what you want.
    pub(crate) clear_guides: bool,
}

/// A guide being dragged: either an existing one, or a new one pulled out of a
/// ruler and not yet committed.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct GuideDrag {
    pub(crate) axis: GuideAxis,
    /// `None` while dragging a *new* guide out of a ruler.
    pub(crate) index: Option<usize>,
    /// Live position in comp coordinates, so the preview line tracks exactly
    /// where the guide will land.
    pub(crate) at: f64,
}

/// Comp-space X (for a vertical guide) or Y (horizontal) under a screen point.
fn comp_coord(axis: GuideAxis, fit: Affine, ppp: f64, p: egui::Pos2) -> f64 {
    let phys = Point::new(p.x as f64 * ppp, p.y as f64 * ppp);
    let c = fit.inverse() * phys;
    match axis {
        GuideAxis::Vertical => c.x,
        GuideAxis::Horizontal => c.y,
    }
}

/// Screen position (logical points) of a guide's line along its crossing axis.
///
/// Feeding `at` into *both* components is deliberate, not a typo: `fit` is
/// translate + uniform scale with no rotation, so its x output depends only on
/// x and its y only on y. One call therefore serves either axis.
fn screen_coord(axis: GuideAxis, fit: Affine, ppp: f64, at: f64) -> f32 {
    let c = fit * Point::new(at, at);
    (match axis {
        GuideAxis::Vertical => c.x,
        GuideAxis::Horizontal => c.y,
    } / ppp) as f32
}

/// Draw the grid inside the composition bounds.
///
/// Bounded by the comp rather than the whole viewport (Blender's choice) because
/// this is a composition tool: the grid exists to subdivide the frame you are
/// going to render, and running it out into the letterbox would just decorate
/// the passepartout.
pub(crate) fn draw_grid(
    painter: &egui::Painter,
    grid: &Grid,
    comp: (f64, f64),
    fit: Affine,
    ppp: f64,
) {
    if !grid.visible {
        return;
    }
    let scale = fit.as_coeffs()[0].abs() / ppp;
    let x0 = screen_coord(GuideAxis::Vertical, fit, ppp, 0.0);
    let y0 = screen_coord(GuideAxis::Horizontal, fit, ppp, 0.0);
    let x1 = x0 + (comp.0 * scale) as f32;
    let y1 = y0 + (comp.1 * scale) as f32;

    // Skip a level once its lines are closer than this on screen. Without it a
    // zoomed-out comp would draw thousands of overlapping lines — slow, and a
    // solid wash rather than a grid.
    const MIN_GAP: f64 = 5.0;

    let mut levels: Vec<(f64, egui::Color32)> = Vec::new();
    if let Some(minor) = grid.minor_step() {
        levels.push((minor, GRID_MINOR));
    }
    levels.push((grid.step(), GRID_MAJOR));

    for (step, col) in levels {
        if step * scale < MIN_GAP {
            continue;
        }
        let stroke = egui::Stroke::new(1.0, col);
        let mut v = 0.0;
        while v <= comp.0 {
            let x = x0 + (v * scale) as f32;
            painter.line_segment([egui::pos2(x, y0), egui::pos2(x, y1)], stroke);
            v += step;
        }
        let mut h = 0.0;
        while h <= comp.1 {
            let y = y0 + (h * scale) as f32;
            painter.line_segment([egui::pos2(x0, y), egui::pos2(x1, y)], stroke);
            h += step;
        }
    }
}

/// A "nice" ruler step: the smallest of 1/2/5×10ⁿ whose on-screen gap clears
/// `min_px`. Same idea as the timeline's adaptive ticks, so labels land on
/// round numbers instead of drifting with the zoom.
pub(crate) fn ruler_step(scale: f64, min_px: f64) -> f64 {
    if scale <= 0.0 || !scale.is_finite() {
        return 1.0;
    }
    let raw = min_px / scale;
    let mag = 10f64.powf(raw.log10().floor());
    for m in [1.0, 2.0, 5.0, 10.0] {
        if mag * m >= raw {
            return mag * m;
        }
    }
    mag * 10.0
}

/// Draw the ruler bands and return their rects, so the caller can interact with
/// them. `canvas` is the *drawing* area (already inset), and the bands sit
/// immediately outside it.
pub(crate) fn draw_rulers(
    painter: &egui::Painter,
    canvas: egui::Rect,
    comp: (f64, f64),
    fit: Affine,
    ppp: f64,
) -> (egui::Rect, egui::Rect) {
    let top = egui::Rect::from_min_max(
        egui::pos2(canvas.min.x - RULER, canvas.min.y - RULER),
        egui::pos2(canvas.max.x, canvas.min.y),
    );
    let left = egui::Rect::from_min_max(
        egui::pos2(canvas.min.x - RULER, canvas.min.y),
        egui::pos2(canvas.min.x, canvas.max.y),
    );
    painter.rect_filled(top, 0.0, RULER_BG);
    painter.rect_filled(left, 0.0, RULER_BG);

    let scale = fit.as_coeffs()[0].abs() / ppp;
    let step = ruler_step(scale, 60.0);
    let font = egui::FontId::proportional(9.0);

    // Ticks are laid out from comp 0, and extended a whole comp either side so
    // the ruler keeps counting past the frame edges instead of stopping dead.
    let origin_x = screen_coord(GuideAxis::Vertical, fit, ppp, 0.0);
    let origin_y = screen_coord(GuideAxis::Horizontal, fit, ppp, 0.0);

    let mut v = -comp.0;
    while v <= comp.0 * 2.0 {
        let x = origin_x + (v * scale) as f32;
        if x >= canvas.min.x && x <= canvas.max.x {
            painter.line_segment(
                [egui::pos2(x, top.max.y - 4.0), egui::pos2(x, top.max.y)],
                egui::Stroke::new(1.0, RULER_TICK),
            );
            painter.text(
                egui::pos2(x + 2.0, top.min.y + 1.0),
                egui::Align2::LEFT_TOP,
                format!("{v:.0}"),
                font.clone(),
                RULER_TEXT,
            );
        }
        v += step;
    }
    let mut h = -comp.1;
    while h <= comp.1 * 2.0 {
        let y = origin_y + (h * scale) as f32;
        if y >= canvas.min.y && y <= canvas.max.y {
            painter.line_segment(
                [egui::pos2(left.max.x - 4.0, y), egui::pos2(left.max.x, y)],
                egui::Stroke::new(1.0, RULER_TICK),
            );
            painter.text(
                egui::pos2(left.min.x + 1.0, y + 1.0),
                egui::Align2::LEFT_TOP,
                format!("{h:.0}"),
                font.clone(),
                RULER_TEXT,
            );
        }
        h += step;
    }
    (top, left)
}

/// Which ruler `p` falls in, and therefore which guide a press there would
/// create: pulling *down* from the top ruler gives a horizontal line, and out
/// from the left ruler a vertical one — the guide runs parallel to its ruler.
fn ruler_axis_at(
    top: Option<egui::Rect>,
    left: Option<egui::Rect>,
    p: egui::Pos2,
) -> Option<GuideAxis> {
    if top.is_some_and(|r| r.contains(p)) {
        Some(GuideAxis::Horizontal)
    } else if left.is_some_and(|r| r.contains(p)) {
        Some(GuideAxis::Vertical)
    } else {
        None
    }
}

/// Which guide (if any) the pointer is near, as an index into `guides`.
pub(crate) fn guide_under(guides: &[Guide], fit: Affine, ppp: f64, p: egui::Pos2) -> Option<usize> {
    guides.iter().position(|g| {
        let at = screen_coord(g.axis, fit, ppp, g.at);
        let d = match g.axis {
            GuideAxis::Vertical => (p.x - at).abs(),
            GuideAxis::Horizontal => (p.y - at).abs(),
        };
        d <= GUIDE_GRAB
    })
}

/// Draw and interact with rulers and guides.
///
/// Returns whether the pointer is over any of it — the caller must gate canvas
/// click-picking on that, exactly as it does for the gizmo, because
/// `is_pointer_over_egui` is area-based and stays false inside the canvas hole.
#[allow(clippy::too_many_arguments)]
pub(crate) fn aids_ui(
    ui: &mut egui::Ui,
    canvas: egui::Rect,
    aids: &ViewAids,
    comp: (f64, f64),
    fit: Affine,
    ppp: f64,
    drag: &mut Option<GuideDrag>,
    out: &mut AidEdits,
) -> bool {
    let painter = ui.painter_at(canvas.expand(RULER));
    draw_grid(&painter, &aids.grid, comp, fit, ppp);

    let (top, left) = if aids.rulers {
        let (t, l) = draw_rulers(&painter, canvas, comp, fit, ppp);
        (Some(t), Some(l))
    } else {
        (None, None)
    };

    let pointer = ui.ctx().pointer_latest_pos();
    let in_canvas = pointer.is_some_and(|p| canvas.contains(p));
    let over_ruler = pointer.and_then(|p| ruler_axis_at(top, left, p));

    // Guides are only grabbable when shown — a hidden guide must not intercept
    // a click on the artwork underneath it.
    let hot = if aids.guides.visible {
        pointer.filter(|_| in_canvas).and_then(|p| guide_under(&aids.guides.items, fit, ppp, p))
    } else {
        None
    };

    // Claim the pointer only where something is actually grabbable, never the
    // whole canvas — see the module note and `App::gizmo_hot`.
    let owns = drag.is_some() || over_ruler.is_some() || hot.is_some();
    if owns {
        // The interact region covers the rulers as well as the canvas, so a
        // drag that starts in a ruler keeps tracking once it crosses into the
        // drawing area — and a guide dragged back out stays grabbed.
        let region = canvas.expand(RULER);
        let resp = ui.interact(region, ui.id().with("aids"), egui::Sense::click_and_drag());

        if resp.drag_started() {
            // Hit-test where the button went **down**, not where the pointer is
            // now. egui only reports `drag_started` once the pointer has moved
            // past its drag threshold, and by then it has usually left the few
            // points around the guide — so testing the live position finds
            // nothing and the grab silently fails. That was the bug behind
            // "I have to hold the click to drag a guide": holding still kept
            // the pointer inside the band long enough to be found.
            //
            // `interact_pointer_pos()` is *not* this — it tracks the ongoing
            // interaction, so it moves with the drag. `press_origin` is the
            // press point.
            let press = ui.ctx().input(|i| i.pointer.press_origin()).or(pointer);
            if let Some(p) = press {
                *drag = match (ruler_axis_at(top, left, p), guide_under(&aids.guides.items, fit, ppp, p)) {
                    (Some(axis), _) => {
                        Some(GuideDrag { axis, index: None, at: comp_coord(axis, fit, ppp, p) })
                    }
                    (None, Some(i)) if aids.guides.visible => {
                        let g = aids.guides.items[i];
                        Some(GuideDrag {
                            axis: g.axis,
                            index: Some(i),
                            at: comp_coord(g.axis, fit, ppp, p),
                        })
                    }
                    _ => None,
                };
            }
        }
        if let (Some(d), Some(p)) = (drag.as_mut(), pointer) {
            d.at = comp_coord(d.axis, fit, ppp, p);
        }
        if resp.drag_stopped() {
            if let (Some(d), Some(p)) = (*drag, pointer) {
                // Released back over a ruler (or off the canvas entirely) means
                // "get rid of it" — the standard gesture, and the only way to
                // delete a guide.
                let discarded = !canvas.contains(p);
                match (d.index, discarded) {
                    (Some(i), true) => out.remove_guide = Some(i),
                    (Some(i), false) => out.move_guide = Some((i, d.at)),
                    (None, false) => {
                        out.add_guide = Some(Guide { axis: d.axis, at: d.at })
                    }
                    // A new guide dropped straight back on the ruler: nothing
                    // was ever added, so there is nothing to undo.
                    (None, true) => {}
                }
            }
            *drag = None;
        }
    } else if drag.is_some() {
        // Lost the pointer mid-drag (window blur): drop it rather than leaving a
        // ghost guide following nothing.
        *drag = None;
    }

    if aids.guides.visible {
        for (i, g) in aids.guides.items.iter().enumerate() {
            // The one being dragged is drawn from the drag's live position.
            if drag.is_some_and(|d| d.index == Some(i)) {
                continue;
            }
            draw_guide(&painter, canvas, g.axis, screen_coord(g.axis, fit, ppp, g.at), hot == Some(i));
        }
    }
    if let Some(d) = *drag {
        draw_guide(&painter, canvas, d.axis, screen_coord(d.axis, fit, ppp, d.at), true);
    }

    // A vertical guide moves horizontally, so the cursor is the *opposite* of
    // the guide's own orientation.
    let cursor_axis = drag
        .map(|d| d.axis)
        .or(over_ruler)
        .or_else(|| hot.map(|i| aids.guides.items[i].axis));
    match cursor_axis {
        Some(GuideAxis::Vertical) => ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal),
        Some(GuideAxis::Horizontal) => ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeVertical),
        None => {}
    }
    owns
}

fn draw_guide(painter: &egui::Painter, canvas: egui::Rect, axis: GuideAxis, at: f32, hot: bool) {
    let col = if hot { GUIDE_HOT } else { GUIDE_COL };
    let stroke = egui::Stroke::new(1.0, col);
    match axis {
        GuideAxis::Vertical => {
            if at >= canvas.min.x && at <= canvas.max.x {
                painter.line_segment(
                    [egui::pos2(at, canvas.min.y), egui::pos2(at, canvas.max.y)],
                    stroke,
                );
            }
        }
        GuideAxis::Horizontal => {
            if at >= canvas.min.y && at <= canvas.max.y {
                painter.line_segment(
                    [egui::pos2(canvas.min.x, at), egui::pos2(canvas.max.x, at)],
                    stroke,
                );
            }
        }
    }
}
