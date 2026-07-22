//! The curve editor: the timeline panel's other view of the same keyframes.
//!
//! Where the dopesheet answers *when*, this answers *how* — every animated
//! channel drawn as its value over time, with the keys' tangents exposed for
//! direct editing. It deliberately shares the dopesheet's time axis, row set and
//! selection (see [`crate::TimelineMode`]): flipping between the two views must
//! feel like turning a card over, not like opening a different tool.
//!
//! The curves are drawn by **sampling the real track** rather than by
//! re-deriving the bezier here, so what you see cannot drift from what plays
//! back — including [`Interp::Hold`], which has no bezier form at all.

use crate::*;

/// The vertical window: what range of *values* the plot shows. The horizontal
/// counterpart is [`TimelineView`], which the dopesheet already owns.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ValueView {
    /// Value at the middle of the plot.
    pub(crate) center: f64,
    /// Total value range covered top to bottom. Never zero — a flat curve still
    /// needs a scale to be drawn against.
    pub(crate) span: f64,
}

impl Default for ValueView {
    fn default() -> Self {
        Self { center: 0.0, span: 200.0 }
    }
}

impl ValueView {
    /// A window framing every value in the *shown* rows, with a margin so keys
    /// at the extremes aren't drawn half off the edge. Hidden properties are
    /// skipped: framing to a curve that isn't drawn would leave the visible one
    /// squashed against an edge for no visible reason.
    ///
    /// Properties are plotted on one shared vertical axis, so a comp mixing
    /// opacity (0–1) with position (hundreds of px) frames to the position and
    /// flattens the opacity. That's the honest picture of "these are the same
    /// axis" — normalizing per row would draw curves that can't be compared to
    /// each other, which is the thing a graph editor is for.
    pub(crate) fn fit(rows: &[CurveRow], shown: &PropSelection) -> Self {
        let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
        for row in rows.iter().filter(|r| shown.shows(r.kind)) {
            for ch in &row.channels {
                for k in ch.track.keys() {
                    lo = lo.min(k.value);
                    hi = hi.max(k.value);
                }
            }
        }
        if !lo.is_finite() || !hi.is_finite() {
            return Self::default();
        }
        let span = ((hi - lo) * 1.3).max(1e-3);
        Self { center: (lo + hi) / 2.0, span }
    }

    fn top(self) -> f64 {
        self.center + self.span / 2.0
    }

    /// Scale the window by `factor` about `anchor`, keeping the value at
    /// `anchor` put — the vertical twin of [`zoomed`].
    fn zoomed(self, factor: f64, anchor: f64) -> Self {
        let span = (self.span * factor).clamp(1e-3, 1e9);
        let ratio = (anchor - self.center) / self.span.max(1e-9);
        Self { center: anchor - ratio * span, span }
    }
}

/// One property's worth of plottable curves: a `Vec2` contributes two, a colour
/// three, a scalar one. Text contributes none and so never gets a row.
pub(crate) struct CurveRow {
    pub(crate) label: &'static str,
    pub(crate) kind: PropKind,
    pub(crate) channels: Vec<Channel>,
}

/// Gather the animated properties of a node into curve rows.
pub(crate) fn curve_rows(node: &MNode) -> Vec<CurveRow> {
    PropKind::ALL
        .iter()
        .filter_map(|&kind| {
            let p = prop_of(node, kind)?;
            if !p.is_animated() {
                return None;
            }
            let channels = p.channels();
            // A text track is animated but has nothing numeric to plot.
            (!channels.is_empty()).then_some(CurveRow { label: kind.label(), kind, channels })
        })
        .collect()
}

/// Which side of a key a tangent belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Side {
    /// Leaving the key, shaping the segment to its right.
    Out,
    /// Arriving at the key, shaping the segment to its left.
    In,
}

/// The tools that set a key's shape. Each acts on the **outgoing** segment of
/// every selected key, matching the properties panel's ease editor — one
/// meaning of "the selected key's easing" across the whole app.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum CurveTool {
    Linear,
    Bezier,
    Separate,
    Hold,
}

impl CurveTool {
    fn label(self) -> &'static str {
        match self {
            CurveTool::Linear => "Linear",
            CurveTool::Bezier => "Bezier",
            CurveTool::Separate => "Separate",
            CurveTool::Hold => "Hold",
        }
    }

    fn hint(self) -> &'static str {
        match self {
            CurveTool::Linear => "Straight into the next key",
            CurveTool::Bezier => "Eased, with the two tangents locked together",
            CurveTool::Separate => "Unlock the tangents so each side moves alone",
            CurveTool::Hold => "Stay on this value, then jump at the next key",
        }
    }
}

/// Apply a tool to one key, writing the edits it implies into `out`.
pub(crate) fn apply_tool(tool: CurveTool, kind: PropKind, index: usize, out: &mut DopeEdits) {
    match tool {
        CurveTool::Linear => {
            out.set_interp.push((kind, index, Interp::Bezier));
            out.set_handles.push((kind, index, Handle::LINEAR_OUT, Handle::LINEAR_IN));
        }
        CurveTool::Bezier => {
            out.set_interp.push((kind, index, Interp::Bezier));
            out.set_handles.push((kind, index, Handle::SMOOTH_OUT, Handle::SMOOTH_IN));
            // Last, so the re-lock mirrors the handles just set rather than the
            // ones they replaced.
            out.set_broken.push((kind, index, false));
        }
        CurveTool::Separate => out.set_broken.push((kind, index, true)),
        CurveTool::Hold => out.set_interp.push((kind, index, Interp::Hold)),
    }
}

/// How many pixels apart the curve is sampled. One sample per 2px is past the
/// point where more would be visible, and keeps a long comp cheap.
const SAMPLE_PX: f32 = 2.0;
/// Hit radius for a key or a tangent knob, in points.
const GRAB_R: f32 = 7.0;

/// The curve editor panel.
#[allow(clippy::too_many_arguments)]
pub(crate) fn curves_ui(
    ui: &mut egui::Ui,
    rows: &[CurveRow],
    frame: f64,
    last_frame: i64,
    tb: motion_core::Timebase,
    view: TimelineView,
    value_view: ValueView,
    selected_keys: &KeySelection,
    shown: &PropSelection,
    work_area: Option<WorkArea>,
    label_w: f32,
    out: &mut DopeEdits,
) {
    // The label column is shared with the dopesheet, so the two views agree on
    // where the split is and switching mode doesn't shift the page under you.
    let label_w = clamp_label_w(label_w, ui.max_rect().width());
    timeline_header(ui, TimelineMode::Curves, frame, last_frame, view, out);

    // --- Toolbar. The tools are disabled with nothing selected rather than
    // hidden: a control that vanishes teaches nothing about why. ---
    let has_sel = !selected_keys.is_empty();
    ui.horizontal(|ui| {
        ui.add_space(8.0);
        for tool in [CurveTool::Linear, CurveTool::Bezier, CurveTool::Separate, CurveTool::Hold] {
            let btn = ui.add_enabled(has_sel, egui::Button::new(tool.label()).small());
            if btn.on_hover_text(tool.hint()).clicked() {
                for &(kind, index) in selected_keys.iter() {
                    apply_tool(tool, kind, index, out);
                }
            }
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add_space(8.0);
            if ui
                .small_button("Frame All")
                .on_hover_text("Fit every curve in view (vertically)")
                .clicked()
            {
                out.set_value_view = Some(ValueView::fit(rows, shown));
            }
            if !has_sel {
                ui.weak("select a key to shape it");
            }
        });
    });
    ui.separator();

    let accent = egui::Color32::from_rgb(255, 216, 51);
    let playhead_col = egui::Color32::from_rgb(240, 90, 90);

    // --- Two columns, like the dopesheet: the properties down the left, the
    // plot to the right. Both are allocated from one horizontal region so the
    // column boundary is the same object in both views. ---
    ui.add_space(2.0);
    let avail = ui.available_size();
    let body_h = (avail.y - 8.0).max(80.0);
    let (column, ruler, plot) = {
        let (rect, _) =
            ui.allocate_exact_size(egui::vec2(avail.x - 16.0, body_h), egui::Sense::hover());
        let split = rect.left() + label_w;
        let right = egui::Rect::from_min_max(
            egui::pos2(split + SPLIT_W, rect.top()),
            rect.right_bottom(),
        );
        // The ruler takes the top strip, exactly as it does in the dopesheet.
        // Without one there is nowhere to scrub: the plot's own background drag
        // is the marquee, and the two gestures can't share a press.
        let ruler = egui::Rect::from_min_max(
            right.left_top(),
            egui::pos2(right.right(), right.top() + RULER_H),
        );
        (
            egui::Rect::from_min_max(
                egui::pos2(rect.left(), rect.top() + RULER_H),
                egui::pos2(split, rect.bottom()),
            ),
            ruler,
            egui::Rect::from_min_max(
                egui::pos2(right.left(), ruler.bottom()),
                right.right_bottom(),
            ),
        )
    };
    property_column(ui, column, rows, shown, label_w, out);
    label_text(
        ui,
        egui::Rect::from_min_size(
            egui::pos2(column.left(), column.top() - RULER_H),
            egui::vec2(label_w, RULER_H),
        ),
        "Frame",
        true,
    );

    // The same splitter the dopesheet has, on the same boundary — a column you
    // can only resize in one of two views of one panel would be a trap.
    {
        let strip = egui::Rect::from_min_max(
            egui::pos2(column.right(), ruler.top()),
            egui::pos2(column.right() + SPLIT_W, column.bottom()),
        );
        let sr = ui.interact(strip, ui.id().with("curve_splitter"), egui::Sense::drag());
        if sr.hovered() || sr.dragged() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
        }
        if sr.dragged() {
            // Driven from the pointer, not an accumulated delta, so a drag that
            // clamps at either end springs back when it comes into range.
            if let Some(p) = sr.interact_pointer_pos() {
                let w = p.x - column.left() - SPLIT_W / 2.0;
                out.set_label_w = Some(clamp_label_w(w, ui.max_rect().width()));
            }
        }
        let col = if sr.dragged() || sr.hovered() {
            egui::Color32::from_gray(150)
        } else {
            egui::Color32::from_gray(64)
        };
        ui.painter().line_segment(
            [
                egui::pos2(strip.center().x, strip.top()),
                egui::pos2(strip.center().x, strip.bottom()),
            ],
            egui::Stroke::new(1.0, col),
        );
    }
    let resp = ui.interact(plot, ui.id().with("curve_plot"), egui::Sense::click_and_drag());
    let painter = ui.painter_at(plot);
    painter.rect_filled(plot, 3.0, egui::Color32::from_gray(28));

    let axis = Axis::new(plot, view);
    let value_to_y = |v: f64| {
        plot.top() + ((value_view.top() - v) / value_view.span * plot.height() as f64) as f32
    };
    let y_to_value = |y: f32| {
        value_view.top() - (y - plot.top()) as f64 / plot.height() as f64 * value_view.span
    };

    {
        let rr = ui.interact(ruler, ui.id().with("curve_ruler"), egui::Sense::click_and_drag());
        // One `Axis` for both: the ruler and the plot are the same x range, so a
        // separate axis for the ruler could only ever be the same or wrong.
        time_ruler(ui, ruler, &axis, &rr, frame, last_frame, tb, view, work_area, out);
    }

    // Value gridlines at a round step, labelled at the left edge.
    {
        let rough = value_view.span / 6.0;
        let mag = 10f64.powf(rough.abs().max(1e-9).log10().floor());
        let step = [1.0, 2.0, 5.0, 10.0]
            .iter()
            .map(|m| m * mag)
            .find(|s| *s >= rough)
            .unwrap_or(mag);
        let first = (value_view.center - value_view.span / 2.0 - step) / step;
        let mut v = first.ceil() * step;
        while v <= value_view.top() {
            let y = value_to_y(v);
            // Zero gets a brighter line: it's the line people look for.
            let bright = v.abs() < step * 1e-6;
            painter.line_segment(
                [egui::pos2(plot.left(), y), egui::pos2(plot.right(), y)],
                egui::Stroke::new(
                    1.0,
                    egui::Color32::from_gray(if bright { 70 } else { 42 }),
                ),
            );
            painter.text(
                egui::pos2(plot.left() + 3.0, y - 1.0),
                egui::Align2::LEFT_BOTTOM,
                format!("{v:.10}").trim_end_matches('0').trim_end_matches('.').to_string(),
                egui::FontId::monospace(9.0),
                egui::Color32::from_gray(110),
            );
            v += step;
        }
    }

    // Time gridlines, on the same tick step the dopesheet's ruler uses so the
    // two views line up frame for frame.
    {
        let step = tick_step(axis.px_per_frame(), tb.fps(), 58.0);
        let first = view.start.floor() as i64;
        let last = (view.start + view.visible).ceil() as i64;
        let mut f = first.div_euclid(step) * step;
        while f <= last {
            if f >= 0 {
                let x = axis.frame_to_x(f as f64);
                painter.line_segment(
                    [egui::pos2(x, plot.top()), egui::pos2(x, plot.bottom())],
                    egui::Stroke::new(1.0, egui::Color32::from_gray(42)),
                );
            }
            f += step;
        }
    }

    if rows.is_empty() {
        painter.text(
            plot.center(),
            egui::Align2::CENTER_CENTER,
            "Select a node with animated properties to see its curves.",
            egui::FontId::proportional(12.0),
            egui::Color32::from_gray(110),
        );
    }

    // --- Curves, then keys, then tangents: later passes draw over earlier
    // ones, which is also the order they should be grabbed in. ---
    for row in rows.iter().filter(|r| shown.shows(r.kind)) {
        for ch in &row.channels {
            if ch.track.is_empty() {
                continue;
            }
            // Sample the *real* track across the visible span. Outside the key
            // range a track holds its endpoint, which this reproduces for free
            // — the flat run either side is genuine, not decoration.
            let steps = ((plot.width() / SAMPLE_PX).ceil() as usize).max(2);
            let line: Vec<egui::Pos2> = (0..=steps)
                .map(|i| {
                    let x = plot.left() + plot.width() * i as f32 / steps as f32;
                    let f = axis.x_to_frame_exact(x);
                    egui::pos2(x, value_to_y(ch.track.sample(f)))
                })
                .collect();
            painter.add(egui::Shape::line(line, egui::Stroke::new(1.6, ch.color)));
        }
    }

    // --- Box-select. A drag that *starts* on the empty plot (rather than on a
    // key or a tangent, which grab the press first) draws a marquee; every key
    // inside it becomes the selection.
    //
    // The rect has to be known before the keys are drawn, but only the plot's
    // response can tell us the drag began on the background — so "a marquee is
    // live" round-trips through egui memory and is read on the following frame,
    // exactly as the dopesheet does it. The one-frame lag is invisible: the box
    // has no area worth hit-testing until the pointer has actually moved. ---
    let marquee_id = ui.id().with("curve_marquee");
    let mut marquee_active: bool = ui.ctx().data(|d| d.get_temp(marquee_id).unwrap_or(false));
    if resp.drag_started() {
        ui.ctx().data_mut(|d| d.insert_temp(marquee_id, true));
    }
    let (press, latest, any_down) = ui.input(|i| {
        (i.pointer.press_origin(), i.pointer.latest_pos(), i.pointer.any_down())
    });
    if marquee_active && !any_down {
        marquee_active = false;
        ui.ctx().data_mut(|d| d.insert_temp(marquee_id, false));
    }
    let marquee = match (marquee_active, press, latest) {
        (true, Some(a), Some(b)) => Some(egui::Rect::from_two_pos(a, b)),
        _ => None,
    };
    let mut marquee_hits = KeySelection::new();

    // Keys and tangents.
    //
    // Every channel draws its own key knobs — a `Vec2` key has an X value and a
    // Y value, and you must be able to grab either. The **tangents**, though,
    // belong to the keyframe, not the channel: one key has one pair of handles
    // shared by every channel, so they're drawn on the first channel only.
    // Two sets of arms for one pair of handles could be dragged into
    // disagreeing with each other, and only one of them could win.
    for row in rows.iter().filter(|r| shown.shows(r.kind)) {
        for (ci, ch) in row.channels.iter().enumerate() {
            let keys = ch.track.keys();
            for (i, k) in keys.iter().enumerate() {
                let kp = egui::pos2(axis.frame_to_x(k.frame as f64), value_to_y(k.value));
                let is_sel = selected_keys.contains(&(row.kind, i));
                if !plot.expand(GRAB_R).contains(kp) {
                    continue;
                }
                // A key counts as inside the marquee if *any* of its channel
                // knobs is: they share one keyframe, so selecting "the X but
                // not the Y" is not a state the selection can hold.
                if let Some(m) = marquee {
                    if m.contains(kp) {
                        marquee_hits.insert((row.kind, i));
                    }
                }

                if is_sel && ci == 0 {
                    for side in [Side::Out, Side::In] {
                        let Some((hp, seg)) = tangent_pos(keys, i, side, &axis, &value_to_y)
                        else {
                            continue;
                        };
                        painter.line_segment([kp, hp], egui::Stroke::new(1.0, accent));
                        painter.circle_filled(hp, 3.5, accent);

                        let id = ui.id().with(("tangent", row.label, i, side == Side::Out));
                        let hit =
                            egui::Rect::from_center_size(hp, egui::Vec2::splat(GRAB_R * 2.0));
                        let tr = ui.interact(hit, id, egui::Sense::drag());
                        if tr.dragged() {
                            if let Some(p) = tr.interact_pointer_pos() {
                                drag_tangent(
                                    keys, i, side, seg, p, row.kind, &axis, &y_to_value, out,
                                );
                            }
                        }
                    }
                }

                // The key itself: a square, so it reads as a different thing
                // from the dopesheet's diamond — this one has a value as well
                // as a time.
                let r = if is_sel { 5.0 } else { 4.0 };
                let fill = if is_sel { egui::Color32::WHITE } else { ch.color };
                painter.rect_filled(
                    egui::Rect::from_center_size(kp, egui::Vec2::splat(r * 2.0)),
                    1.0,
                    fill,
                );
                // A held key draws the step it produces, so "this one holds" is
                // legible without selecting it.
                if k.interp == Interp::Hold {
                    if let Some(next) = keys.get(i + 1) {
                        painter.line_segment(
                            [kp, egui::pos2(axis.frame_to_x(next.frame as f64), kp.y)],
                            egui::Stroke::new(1.0, egui::Color32::from_gray(150)),
                        );
                    }
                }
                // Broken tangents get a ring, the one bit of key state that has
                // no other visible consequence until you drag an arm.
                if k.broken && is_sel {
                    painter.circle_stroke(kp, r + 3.0, egui::Stroke::new(1.0, accent));
                }

                let id = ui.id().with(("curve_key", row.label, ci, i));
                let hit = egui::Rect::from_center_size(kp, egui::Vec2::splat(GRAB_R * 2.0));
                let kr = ui.interact(hit, id, egui::Sense::click_and_drag());
                if kr.clicked() {
                    let mods = ui.input(|i| i.modifiers);
                    if mods.command || mods.shift {
                        out.toggle_key = Some((row.kind, i));
                    } else {
                        out.select_key = Some((row.kind, i));
                    }
                }
                if kr.dragged() {
                    if let Some(p) = kr.interact_pointer_pos() {
                        if !is_sel {
                            out.select_key = Some((row.kind, i));
                        }
                        // Horizontal is a *delta*, so the whole selection moves
                        // as a block exactly as it does in the dopesheet.
                        // Vertical is absolute and belongs to the grabbed
                        // channel alone: there is no shared "up" between a
                        // rotation in degrees and an opacity in [0,1].
                        let target = axis.x_to_frame(p.x).clamp(0, last_frame);
                        if target != k.frame {
                            out.move_by = Some(target - k.frame);
                        }
                        out.set_channel_value = Some((row.kind, i, ci, y_to_value(p.y)));
                    }
                }
            }
        }
    }

    // Playhead, over everything — it is the one line you must always find.
    let px = axis.frame_to_x(frame);
    painter.line_segment(
        [egui::pos2(px, plot.top()), egui::pos2(px, plot.bottom())],
        egui::Stroke::new(1.5, playhead_col),
    );

    // --- Click on empty plot seeks and clears, like the dopesheet's track.
    // A *drag* there draws the marquee instead, so the two gestures don't
    // fight over the same press. ---
    if resp.clicked() {
        if let Some(p) = resp.interact_pointer_pos() {
            out.seek_to = Some(axis.x_to_frame(p.x).clamp(0, last_frame));
            out.clear_selection = true;
        }
    }

    // Report and draw the marquee. Reported even when empty, so dragging a box
    // over nothing clears the selection the way a click on empty plot does.
    if let Some(m) = marquee {
        out.box_select = Some(std::mem::take(&mut marquee_hits));
        painter.rect_filled(m, 2.0, egui::Color32::from_white_alpha(18));
        painter.rect_stroke(m, 2.0, egui::Stroke::new(1.0, accent), egui::StrokeKind::Inside);
    }

    // --- Zoom. Plain wheel zooms time (the dopesheet's gesture, so the two
    // views scroll alike); ctrl+wheel zooms the value axis, which only exists
    // here. Both anchor at the cursor. ---
    let (scroll, zoom_mod, hover) =
        ui.input(|i| (i.smooth_scroll_delta, i.modifiers.command, i.pointer.hover_pos()));
    if let Some(p) = hover.filter(|p| plot.contains(*p)) {
        if scroll.y != 0.0 {
            let factor = (0.9f64).powf(scroll.y as f64 / 120.0);
            if zoom_mod {
                out.set_value_view = Some(value_view.zoomed(factor, y_to_value(p.y)));
            } else {
                out.set_view =
                    Some(zoomed(view, factor, axis.x_to_frame_exact(p.x)).clamped(last_frame));
            }
        }
        if scroll.x != 0.0 {
            out.set_view = Some(
                TimelineView {
                    start: view.start - (scroll.x as f64 / 120.0) * view.visible * 0.1,
                    visible: view.visible,
                }
                .clamped(last_frame),
            );
        }
    }
}

/// Which properties the curve editor plots. Empty means **all of them** — the
/// state you get before choosing anything, and what "show me everything again"
/// returns to. A `BTreeSet` for the same reason [`KeySelection`] is one:
/// deterministic iteration, cheap membership.
pub(crate) type PropSelection = std::collections::BTreeSet<PropKind>;

/// Whether a property's curves are drawn.
pub(crate) trait Shows {
    fn shows(&self, kind: PropKind) -> bool;
}

impl Shows for PropSelection {
    fn shows(&self, kind: PropKind) -> bool {
        self.is_empty() || self.contains(&kind)
    }
}

/// The left column: one clickable row per animated property, with a swatch per
/// channel so the colours in the plot are named.
///
/// Clicking a row shows *only* that property — with several properties on one
/// vertical axis a busy layer is unreadable, and isolating one is the first
/// thing anybody does. Ctrl/shift-click adds to the shown set, and clicking
/// below the rows clears it back to showing everything.
fn property_column(
    ui: &mut egui::Ui,
    column: egui::Rect,
    rows: &[CurveRow],
    shown: &PropSelection,
    label_w: f32,
    out: &mut DopeEdits,
) {
    let painter = ui.painter_at(column);
    // The blank area under the last row is a target too, so "show everything"
    // never requires hunting for a button.
    let bg = ui.interact(column, ui.id().with("curve_column_bg"), egui::Sense::click());
    if bg.clicked() {
        out.set_shown_props = Some(PropSelection::new());
    }

    for (i, row) in rows.iter().enumerate() {
        let top = column.top() + 4.0 + i as f32 * ROW_H;
        if top + ROW_H > column.bottom() {
            break;
        }
        let cell = egui::Rect::from_min_size(
            egui::pos2(column.left(), top),
            egui::vec2(label_w, ROW_H),
        );
        let is_shown = shown.shows(row.kind);
        // "Explicitly picked" reads differently from "shown because nothing is
        // picked": only the former gets the highlight, or an untouched panel
        // would look like every row had been selected.
        let picked = shown.contains(&row.kind);
        let resp = ui.interact(cell, ui.id().with(("curve_row", i)), egui::Sense::click());
        if picked || resp.hovered() {
            painter.rect_filled(
                cell,
                2.0,
                egui::Color32::from_white_alpha(if picked { 22 } else { 10 }),
            );
        }
        painter.text(
            egui::pos2(cell.left() + 4.0, cell.center().y),
            egui::Align2::LEFT_CENTER,
            row.label,
            egui::TextStyle::Body.resolve(ui.style()),
            if is_shown {
                ui.style().visuals.text_color()
            } else {
                ui.style().visuals.weak_text_color()
            },
        );
        // Channel swatches, right-aligned: this is what makes the plot's
        // colours mean something without a legend crowding the curves. A
        // multi-channel property shows its letters (X, Y, R…) in the curve's
        // own colour; a single-channel one has no letter worth printing, so it
        // gets a plain dot.
        let mut x = cell.right() - 4.0;
        for ch in row.channels.iter().rev() {
            let color = if is_shown { ch.color } else { ch.color.gamma_multiply(0.35) };
            let w = if ch.name.is_empty() { 6.0 } else { 9.0 };
            if x - w < cell.left() + 4.0 {
                break;
            }
            if ch.name.is_empty() {
                painter.rect_filled(
                    egui::Rect::from_center_size(
                        egui::pos2(x - w / 2.0, cell.center().y),
                        egui::vec2(6.0, 6.0),
                    ),
                    1.0,
                    color,
                );
            } else {
                painter.text(
                    egui::pos2(x, cell.center().y),
                    egui::Align2::RIGHT_CENTER,
                    ch.name,
                    egui::FontId::monospace(10.0),
                    color,
                );
            }
            x -= w + 3.0;
        }

        if resp.clicked() {
            let mods = ui.input(|i| i.modifiers);
            let mut next = shown.clone();
            if mods.command || mods.shift {
                // Toggling the last one off would leave an empty set, which
                // means "show all" — so it lands back where it started rather
                // than on a blank plot.
                if !next.remove(&row.kind) {
                    next.insert(row.kind);
                }
            } else if next.len() == 1 && next.contains(&row.kind) {
                // Clicking the isolated row again un-isolates it.
                next.clear();
            } else {
                next = [row.kind].into_iter().collect();
            }
            out.set_shown_props = Some(next);
        }
    }
}

/// Where a key's tangent knob sits on screen, and which segment it shapes.
///
/// A handle is stored normalized to its segment (`cubic-bezier` style), so this
/// is the one place that converts it into the curve's own space: x across the
/// segment's frames, y across the segment's two values. Returns `None` when
/// there is no segment on that side — the first key has no incoming one, and
/// the last no outgoing.
pub(crate) fn tangent_pos(
    keys: &[Keyframe<f64>],
    index: usize,
    side: Side,
    axis: &Axis,
    value_to_y: &impl Fn(f64) -> f32,
) -> Option<(egui::Pos2, usize)> {
    let (seg, h) = match side {
        Side::Out => (index, keys.get(index)?.out_handle),
        Side::In => (index.checked_sub(1)?, keys.get(index)?.in_handle),
    };
    let (a, b) = (keys.get(seg)?, keys.get(seg + 1)?);
    // A held segment has no curve to shape, so it has no tangent to show.
    if a.interp == Interp::Hold {
        return None;
    }
    let f = a.frame as f64 + h.x * (b.frame - a.frame) as f64;
    let v = a.value + h.y * (b.value - a.value);
    Some((egui::pos2(axis.frame_to_x(f), value_to_y(v)), seg))
}

/// Turn a tangent drag into handle edits.
///
/// Locked tangents (`broken == false`) move as one: dragging either side writes
/// the mirrored partner onto the neighbouring segment too, which is why this
/// can emit two edits. When the key is broken, only the dragged side moves.
#[allow(clippy::too_many_arguments)]
pub(crate) fn drag_tangent(
    keys: &[Keyframe<f64>],
    index: usize,
    side: Side,
    seg: usize,
    p: egui::Pos2,
    kind: PropKind,
    axis: &Axis,
    y_to_value: &impl Fn(f32) -> f64,
    out: &mut DopeEdits,
) {
    let (Some(a), Some(b)) = (keys.get(seg), keys.get(seg + 1)) else { return };
    let span = (b.frame - a.frame) as f64;
    let dv = b.value - a.value;
    if span <= 0.0 {
        return;
    }
    // x is always recoverable; y is not when the segment is flat (every value
    // maps to the same place), so a flat segment keeps the y it had rather than
    // dividing by zero.
    let x = ((axis.x_to_frame_exact(p.x) - a.frame as f64) / span).clamp(0.0, 1.0);
    let old = match side {
        Side::Out => a.out_handle,
        Side::In => b.in_handle,
    };
    let y = if dv.abs() < 1e-12 { old.y } else { (y_to_value(p.y) - a.value) / dv };
    let moved = Handle::new(x, y);

    let (out_h, in_h) = match side {
        Side::Out => (moved, b.in_handle),
        Side::In => (a.out_handle, moved),
    };
    out.set_handles.push((kind, seg, out_h, in_h));

    // Locked: mirror onto the segment on the far side of the dragged key, so
    // the curve stays smooth through it. The neighbour may not exist (the key
    // is at either end), in which case there is nothing to keep smooth.
    if !keys.get(index).map(|k| k.broken).unwrap_or(true) {
        match side {
            Side::Out if index > 0 => {
                let prev = index - 1;
                if let Some(pk) = keys.get(prev) {
                    out.set_handles.push((kind, prev, pk.out_handle, mirror_handle(moved)));
                }
            }
            Side::In => {
                if let Some(next) = keys.get(index + 1) {
                    out.set_handles.push((kind, index, mirror_handle(moved), next.in_handle));
                }
            }
            _ => {}
        }
    }
}
