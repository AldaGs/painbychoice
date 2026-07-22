//! Layer strips: every layer's life in time, on one shared axis.
//!
//! The dopesheet answers *when do this layer's keys happen*, the curve editor
//! *how do they move*; this answers **when is this layer even alive** — the
//! question After Effects' layer bars exist for, and the one that makes a comp
//! feel like a composition rather than a pile of always-on artwork.
//!
//! Nothing here is a new time model. [`LayerTiming`] already trims evaluation
//! (`eval::walk` skips a layer outside its window and offsets local frame by
//! `start`), and the dopesheet's clip bar already trims/slides/slips it for the
//! *selected* layer. This view promotes that one bar into one row per layer, so
//! the comp's structure in time is visible at a glance instead of one layer at
//! a time.

use crate::*;

/// One layer's row: who it is, and its window in comp frames.
pub(crate) struct StripRow {
    pub(crate) id: NodeId,
    pub(crate) name: String,
    /// Nesting depth, used to indent the name — the tree's shape is part of
    /// reading the comp, and a flat list of names loses which layer is inside
    /// which group.
    pub(crate) depth: usize,
    pub(crate) precomp: bool,
    /// `None` = live for the whole comp, which is what every layer is until
    /// someone gives it a range.
    pub(crate) timing: Option<LayerTiming>,
    /// Frames of every keyframe on the layer, merged across its properties and
    /// deduped. Drawn as ticks on the bar: a strip with no keys on it reads
    /// very differently from one packed with them, and that is exactly what you
    /// want to see when laying out a comp.
    pub(crate) keys: Vec<i64>,
}

/// Flatten a comp's layers into strip rows.
///
/// The comp **root is skipped**: it is the composition itself, not a layer in
/// it, and offering to trim the thing that defines the time axis would be a
/// control with no coherent meaning.
pub(crate) fn strip_rows(root: &MNode) -> Vec<StripRow> {
    fn walk(node: &MNode, depth: usize, out: &mut Vec<StripRow>) {
        let mut keys: Vec<i64> =
            dope_rows(node).into_iter().flat_map(|r| r.frames).collect();
        keys.sort_unstable();
        keys.dedup();
        out.push(StripRow {
            id: node.id,
            name: node.name.clone(),
            depth,
            precomp: node.precomp.is_some(),
            timing: node.timing,
            keys,
        });
        // Front-most first, matching the layers panel — the two are the same
        // list of layers and must not disagree about their order.
        for c in node.children.iter().rev() {
            walk(c, depth + 1, out);
        }
    }
    let mut out = Vec::new();
    for child in root.children.iter().rev() {
        walk(child, 0, &mut out);
    }
    out
}

/// Width of a bar's trim handle, in points. Shared with the dopesheet's clip
/// bar so the same grab means the same thing in both places.
const HANDLE_W: f32 = 6.0;

/// The layer-strips view.
#[allow(clippy::too_many_arguments)]
pub(crate) fn strips_ui(
    ui: &mut egui::Ui,
    rows: &[StripRow],
    frame: f64,
    last_frame: i64,
    tb: motion_core::Timebase,
    view: TimelineView,
    selected: Option<NodeId>,
    work_area: Option<WorkArea>,
    label_w: f32,
    out: &mut DopeEdits,
) {
    let label_w = clamp_label_w(label_w, ui.max_rect().width());
    timeline_header(ui, TimelineMode::Strips, frame, last_frame, view, out);

    let accent = egui::Color32::from_rgb(255, 216, 51);
    let playhead_col = egui::Color32::from_rgb(240, 90, 90);
    let mut dragging = false;
    let columns_top = ui.cursor().top();

    // --- Ruler, allocated with a layer row's layout so its axis *is* the rows'
    // axis. Same discipline as the dopesheet: one allocation shape, so no row
    // can disagree with another about where a frame sits. ---
    let mut axis = None;
    ui.horizontal(|ui| {
        ui.add_space(8.0);
        let cell = label_cell(ui, label_w, RULER_H);
        label_text(ui, cell, "Frame", true);
        ui.add_space(SPLIT_W);
        let (rect, resp) = ui.allocate_exact_size(
            egui::vec2(ui.available_width() - 8.0, RULER_H),
            egui::Sense::click_and_drag(),
        );
        let a = Axis::new(rect, view);
        axis = Some(a);
        dragging |= time_ruler(ui, rect, &a, &resp, frame, last_frame, tb, view, work_area, out);
    });
    let axis = axis.expect("the ruler always allocates the axis");

    if rows.is_empty() {
        ui.add_space(8.0);
        ui.weak("This composition has no layers yet.");
    }

    for (i, row) in rows.iter().enumerate() {
        let is_sel = selected == Some(row.id);
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            let cell = label_cell(ui, label_w, ROW_H);

            // The range button sits at the right of the label cell, *placed*
            // rather than laid out, so it can't widen this row's column and
            // slide its track out of step with the ruler.
            let btn_w = cell.width().min(22.0);
            let btn = egui::Rect::from_min_size(
                egui::pos2(cell.right() - btn_w, cell.top()),
                egui::vec2(btn_w, cell.height()),
            );
            let name_cell = egui::Rect::from_min_max(
                egui::pos2(cell.left() + row.depth as f32 * 10.0, cell.top()),
                egui::pos2(btn.left() - 2.0, cell.bottom()),
            );
            let name_resp =
                ui.interact(name_cell, ui.id().with(("strip_name", i)), egui::Sense::click());
            if is_sel || name_resp.hovered() {
                ui.painter_at(cell).rect_filled(
                    cell,
                    2.0,
                    egui::Color32::from_white_alpha(if is_sel { 22 } else { 10 }),
                );
            }
            if name_resp.clicked() {
                out.select_layer = Some(row.id);
            }
            let label = if row.precomp {
                format!("{} {}", icon::PRECOMP, row.name)
            } else {
                row.name.clone()
            };
            label_text(ui, name_cell, &label, !is_sel);

            match row.timing {
                // One click gives the layer a range covering the whole comp —
                // exactly what it does today — so arming trimming can never
                // change what is on screen.
                None => {
                    if ui
                        .put(btn, egui::Button::new(icon::text(icon::TRIM)).frame(false))
                        .on_hover_text("Give this layer a time range")
                        .clicked()
                    {
                        out.set_layer_timing =
                            Some((row.id, Some(LayerTiming::new(0, last_frame + 1))));
                    }
                }
                Some(_) => {
                    if ui
                        .put(btn, egui::Button::new(icon::text(icon::CLOSE)).frame(false))
                        .on_hover_text("Back to the whole comp")
                        .clicked()
                    {
                        out.set_layer_timing = Some((row.id, None));
                    }
                }
            }

            ui.add_space(SPLIT_W);
            let (track, resp) = ui.allocate_exact_size(
                egui::vec2(ui.available_width() - 8.0, ROW_H),
                egui::Sense::click_and_drag(),
            );
            let painter = ui.painter_at(track);
            painter.rect_filled(track, 3.0, egui::Color32::from_gray(32));

            // The bar. A layer with no range is drawn as a faint full-width
            // band rather than nothing: "alive the whole time" is a real state
            // and should look like one, not like a missing layer.
            let timing = row.timing;
            let (x0, x1) = match timing {
                Some(t) => (
                    axis.frame_to_x(t.in_ as f64),
                    axis.frame_to_x(t.out as f64),
                ),
                None => (track.left(), track.right()),
            };
            let bar = egui::Rect::from_min_max(
                egui::pos2(x0.max(track.left()), track.top() + 3.0),
                egui::pos2(x1.min(track.right()), track.bottom() - 3.0),
            );
            if bar.width() > 0.0 {
                let fill = match (timing.is_some(), is_sel) {
                    (false, _) => egui::Color32::from_gray(44),
                    (true, false) => egui::Color32::from_rgb(58, 84, 120),
                    (true, true) => egui::Color32::from_rgb(74, 106, 148),
                };
                painter.rect_filled(bar, 3.0, fill);
                if is_sel {
                    painter.rect_stroke(
                        bar,
                        3.0,
                        egui::Stroke::new(1.0, accent),
                        egui::StrokeKind::Inside,
                    );
                }
                // Where local frame 0 sits — the only feedback a slip gives,
                // since the bar itself doesn't move under one.
                if let Some(t) = timing {
                    let sx = axis.frame_to_x(t.start as f64);
                    if sx > bar.left() && sx < bar.right() {
                        painter.line_segment(
                            [egui::pos2(sx, bar.top()), egui::pos2(sx, bar.bottom())],
                            egui::Stroke::new(1.0, egui::Color32::from_gray(170)),
                        );
                    }
                }
            }

            // Keyframe ticks, clipped to the bar: a key outside the layer's
            // window never plays, so drawing it on the strip would advertise
            // motion that can't happen.
            for &kf in &row.keys {
                let kx = axis.frame_to_x(kf as f64);
                if kx < bar.left() || kx > bar.right() {
                    continue;
                }
                painter.line_segment(
                    [
                        egui::pos2(kx, bar.top() + 2.0),
                        egui::pos2(kx, bar.bottom() - 2.0),
                    ],
                    egui::Stroke::new(1.0, egui::Color32::from_gray(205)),
                );
            }

            let px = axis.frame_to_x(frame);
            painter.line_segment(
                [egui::pos2(px, track.top()), egui::pos2(px, track.bottom())],
                egui::Stroke::new(1.5, playhead_col),
            );

            // Clicking a strip selects its layer — the row *is* the layer, so
            // anywhere on it should work, not only the name.
            if resp.clicked() && !is_sel {
                out.select_layer = Some(row.id);
            }

            // Drag: trim an edge, slide the body, alt-drag to slip. The grab
            // mode and the timing it started from are latched at press, so
            // applying the *total* delta to the original (not an incremental
            // delta to the current) lets a drag that clamped at 0 spring back.
            // A layer with no range has nothing to drag.
            let Some(timing) = timing else { return };
            let drag_id = ui.id().with(("strip_drag", i));
            if resp.drag_started() {
                // `press_origin`, not `interact_pointer_pos`: egui only fires
                // `drag_started` once the pointer crosses its threshold, by
                // which point the pointer has already left the handle it
                // grabbed — which would read every trim as a slide.
                if let Some(p) = ui.input(|i| i.pointer.press_origin()) {
                    let alt = ui.input(|i| i.modifiers.alt);
                    let grab = clip_grab_at(p.x, bar.left(), bar.right(), HANDLE_W, alt);
                    let anchor = axis.x_to_frame(p.x);
                    ui.ctx().data_mut(|d| d.insert_temp(drag_id, (grab, anchor, timing)));
                }
            }
            if resp.dragged() {
                let latched: Option<(ClipGrab, i64, LayerTiming)> =
                    ui.ctx().data(|d| d.get_temp(drag_id));
                if let (Some((grab, anchor, orig)), Some(p)) =
                    (latched, resp.interact_pointer_pos())
                {
                    let next = drag_clip(orig, grab, axis.x_to_frame(p.x) - anchor);
                    if next != timing {
                        out.set_layer_timing = Some((row.id, Some(next)));
                    }
                }
                dragging = true;
            }
        });
    }

    // --- Column splitter, spanning the ruler and every row: the columns are
    // one division of the panel, so there is one thing to grab. Drawn and
    // interacted with last, so a drag near the boundary resizes the column
    // instead of dragging the strip under it. ---
    {
        let columns_bottom = ui.cursor().top();
        let x = ui.max_rect().left() + 8.0 + label_w;
        let strip = egui::Rect::from_min_max(
            egui::pos2(x, columns_top),
            egui::pos2(x + SPLIT_W, columns_bottom.max(columns_top + RULER_H)),
        );
        let resp = ui.interact(strip, ui.id().with("strip_splitter"), egui::Sense::drag());
        if resp.hovered() || resp.dragged() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
        }
        if resp.dragged() {
            if let Some(p) = resp.interact_pointer_pos() {
                let w = p.x - (ui.max_rect().left() + 8.0) - SPLIT_W / 2.0;
                out.set_label_w = Some(clamp_label_w(w, ui.max_rect().width()));
            }
            dragging = true;
        }
        let col = if resp.dragged() || resp.hovered() {
            egui::Color32::from_gray(150)
        } else {
            egui::Color32::from_gray(64)
        };
        let cx = strip.center().x;
        ui.painter().line_segment(
            [egui::pos2(cx, strip.top()), egui::pos2(cx, strip.bottom())],
            egui::Stroke::new(1.0, col),
        );
    }

    // --- Zoom / pan, the dopesheet's gesture so every timeline view scrolls
    // alike. ---
    let panel_rect = ui.max_rect();
    let (scroll, hover) = ui.input(|i| (i.smooth_scroll_delta, i.pointer.hover_pos()));
    if let Some(p) = hover.filter(|p| panel_rect.contains(*p)) {
        let next = if scroll.x != 0.0 {
            Some(TimelineView {
                start: view.start - (scroll.x as f64 / 120.0) * view.visible * 0.1,
                visible: view.visible,
            })
        } else if scroll.y != 0.0 {
            Some(zoomed(view, (0.9f64).powf(scroll.y as f64 / 120.0), axis.x_to_frame_exact(p.x)))
        } else {
            None
        };
        if let Some(next) = next {
            out.set_view = Some(next.clamped(last_frame));
        }
    }

    // --- Edge auto-pan while dragging, so a strip can be dragged past the
    // visible window without letting go. Drag-only on purpose: on plain hover
    // it would scroll the timeline out from under the pointer. ---
    if dragging {
        if let Some(p) = ui.input(|i| i.pointer.latest_pos()) {
            let intensity = edge_pan_intensity(p.x, axis.x0, axis.x0 + axis.span, EDGE_PAN_W);
            if intensity != 0.0 {
                let dt = (ui.input(|i| i.stable_dt) as f64).min(0.05);
                let delta = intensity as f64 * view.visible * 0.8 * dt;
                out.set_view = Some(
                    TimelineView { start: view.start + delta, visible: view.visible }
                        .clamped(last_frame),
                );
                ui.ctx().request_repaint();
            }
        }
    }
}
