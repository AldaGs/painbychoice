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
use crate::clips::{Clip, Op, Patch, Side};

/// One layer's row: who it is, and its window in comp frames.
pub(crate) struct StripRow {
    pub(crate) id: NodeId,
    pub(crate) name: String,
    /// Nesting depth, used to indent the name — the tree's shape is part of
    /// reading the comp, and a flat list of names loses which layer is inside
    /// which group.
    pub(crate) depth: usize,
    pub(crate) kind: crate::layers::RowKind,
    /// `None` = live for the whole comp, which is what every layer is until
    /// someone gives it a range.
    pub(crate) timing: Option<LayerTiming>,
    /// Frames of every keyframe on the layer, merged across its properties and
    /// deduped. Drawn as ticks on the bar: a strip with no keys on it reads
    /// very differently from one packed with them, and that is exactly what you
    /// want to see when laying out a comp.
    pub(crate) keys: Vec<i64>,
    /// The sound this layer carries, if any, and whether it is unmuted.
    ///
    /// A muted layer keeps its waveform — dimmed — because muting is a
    /// temporary state and hiding the shape would make the row you are about
    /// to unmute the one you cannot read.
    pub(crate) audio: Option<(motion_core::AssetId, bool)>,
    /// The picture this layer carries, if any, and whether its time is
    /// remapped.
    ///
    /// The whole [`Asset`](motion_core::Asset) rather than its id because
    /// drawing the cache bar has to turn comp frames into source frames, and
    /// `Asset::source_frame` is already that conversion — re-deriving it from
    /// a couple of copied fields is how the strip and the renderer would
    /// eventually disagree about which frame a layer is showing.
    ///
    /// The flag is what the bar can't answer: a remapped layer picks its
    /// source frame from a curve resolved against the whole project, so the
    /// honest bar for one is no bar at all.
    pub(crate) footage: Option<(motion_core::Asset, bool)>,
    /// How many layers sit directly inside this one; above zero it folds.
    pub(crate) children: usize,
    /// The layer's sync-lock group, if any.
    pub(crate) sync: Option<u64>,
}

/// The rows left showing when only the rows in `open` show what is inside
/// them: everything with children starts closed, so a shape layer is one row
/// until you open it. The selected row's ancestors count as open, so picking a
/// layer on the canvas never leaves it hidden in a closed group.
pub(crate) fn visible_rows<'a>(
    rows: &'a [StripRow],
    open: &std::collections::HashSet<NodeId>,
    selected: Option<NodeId>,
) -> Vec<&'a StripRow> {
    // Ancestors of the selection: walking back, each row shallower than the
    // last one kept is the next parent up.
    let mut forced = std::collections::HashSet::new();
    if let Some(i) = rows.iter().position(|r| Some(r.id) == selected) {
        let mut depth = rows[i].depth;
        for r in rows[..i].iter().rev() {
            if r.depth < depth {
                forced.insert(r.id);
                depth = r.depth;
            }
        }
    }
    let mut hide_below: Option<usize> = None;
    let mut out = Vec::new();
    for row in rows {
        match hide_below {
            Some(d) if row.depth > d => continue,
            _ => hide_below = None,
        }
        if row.children > 0 && !open.contains(&row.id) && !forced.contains(&row.id) {
            hide_below = Some(row.depth);
        }
        out.push(row);
    }
    out
}

/// Flatten a comp's layers into strip rows.
///
/// The edit tools. Slip stays alt+drag in every tool, as it was.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum EditTool {
    #[default]
    Select,
    Razor,
    Ripple,
    Roll,
}

impl EditTool {
    fn glyph(self) -> ToolCursor {
        match self {
            EditTool::Select => ToolCursor::Select,
            EditTool::Razor => ToolCursor::Razor,
            EditTool::Ripple => ToolCursor::Ripple { start: false },
            EditTool::Roll => ToolCursor::Roll,
        }
    }

    const ALL: [EditTool; 4] = [EditTool::Select, EditTool::Razor, EditTool::Ripple, EditTool::Roll];
    fn label(self) -> &'static str {
        match self {
            EditTool::Select => "Select",
            EditTool::Razor => "Razor",
            EditTool::Ripple => "Ripple",
            EditTool::Roll => "Roll",
        }
    }
    fn hint(self) -> &'static str {
        match self {
            EditTool::Select => "Trim, slide, alt+drag to slip",
            EditTool::Razor => "Click a bar to cut it there",
            EditTool::Ripple => "Trims push or pull the rest of the row",
            EditTool::Roll => "Drag a cut between two touching bars: one grows, the other shrinks",
        }
    }
}

/// One clip bar, drawn into `track` (the row owns its background and the
/// playhead), with `resp` the interaction covering it. Shared by the per-layer
/// and packed views so a bar behaves the same in both. Returns whether it is
/// being dragged.
#[allow(clippy::too_many_arguments)]
fn clip_bar(
    ui: &mut egui::Ui,
    track: egui::Rect,
    resp: &egui::Response,
    row: &StripRow,
    selected: Option<NodeId>,
    axis: &Axis,
    tb: motion_core::Timebase,
    frame: f64,
    last_frame: i64,
    peaks: &std::collections::HashMap<motion_core::AssetId, std::sync::Arc<motion_render::Peaks>>,
    cached: &std::collections::HashSet<crate::footage::Key>,
    tool: EditTool,
    clips: &[Clip],
    snap: &Magnet,
    out: &mut DopeEdits,
) -> bool {
    let is_sel = selected == Some(row.id);
    let accent = egui::Color32::from_rgb(255, 216, 51);
    let mut drag = false;
    let dragging = &mut drag;
    let painter = ui.painter_at(track);

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
        // Tinted with the layer type's accent: faint when it has no
        // range, stronger when it does, strongest when selected.
        let tint = match (timing.is_some(), is_sel) {
            (false, _) => 0.18,
            (true, false) => 0.42,
            (true, true) => 0.58,
        };
        let fill = egui::Color32::from_gray(32).lerp_to_gamma(row.kind.accent(), tint);
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

    // The waveform, inside the bar and under the keyframe ticks. It
    // is drawn from the layer's *own* time, so trimming the head
    // scrolls the sound within the bar exactly as it scrolls the
    // picture of a trimmed video layer — the whole reason this view
    // makes sync editable rather than merely visible.
    if let Some((asset, on)) = row.audio {
        if let Some(pk) = peaks.get(&asset) {
            let start = timing.map(|t| t.start as f64).unwrap_or(0.0);
            waveform(&painter, bar, &axis, tb.fps(), start, pk, on);
        }
    }

    // Which of this layer's frames are decoded and ready. A remapped
    // layer gets the empty gutter and nothing in it: its source frame
    // comes from a curve resolved against the whole project, and a bar
    // drawn from the natural mapping would be confidently wrong about
    // every frame — worse than no bar, because it would be believed.
    if let Some((asset, remapped)) = &row.footage {
        let start = timing.map(|t| t.start as f64).unwrap_or(0.0);
        if !remapped {
            cache_bar(&painter, bar, &axis, tb.fps(), start, asset, cached);
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


    // Clicking a strip selects its layer — the row *is* the layer, so
    // anywhere on it should work, not only the name.
    let snap_on = snap.active(ui);
    let at = |x: f32| {
        let f = axis.x_to_frame(x).clamp(0, last_frame);
        if snap_on { snap.to(f, row.id, axis).unwrap_or(f) } else { f }
    };
    if resp.clicked() {
        if tool == EditTool::Razor {
            if let Some(p) = resp.interact_pointer_pos() {
                out.clip_op = Some(Op::Razor(row.id, at(p.x)));
            }
        } else if !is_sel {
            out.select_layer = Some(row.id);
        }
    }
    if row.depth == 0 {
        resp.context_menu(|ui| {
            if ui.button("Split at playhead").clicked() {
                out.clip_op = Some(Op::Razor(row.id, frame.round() as i64));
                ui.close();
            }
            if ui.button("Ripple delete").clicked() {
                out.clip_op = Some(Op::RippleDelete(row.id));
                ui.close();
            }
            if let Some(sel) = selected.filter(|s| *s != row.id) {
                if ui
                    .button("Sync-lock with selected")
                    .on_hover_text("Edit tools move, trim and cut them together")
                    .clicked()
                {
                    out.clip_op = Some(Op::Sync(row.id, sel));
                    ui.close();
                }
            }
            if row.sync.is_some() && ui.button("Remove sync-lock").clicked() {
                out.clip_op = Some(Op::Unsync(row.id));
                ui.close();
            }
        });
    }
    if tool == EditTool::Razor {
        if let Some(p) = resp.hover_pos().filter(|p| p.x >= bar.left() && p.x <= bar.right()) {
            // Where the cut will land (snapped), across the bar.
            let x = axis.frame_to_x(at(p.x) as f64);
            painter.line_segment([egui::pos2(x, bar.top()), egui::pos2(x, bar.bottom())], egui::Stroke::new(1.5, RAZOR));
            tool_cursor(ui, ToolCursor::Razor, p);
        }
        return false;
    }

    // Drag: trim an edge, slide the body, alt-drag to slip. The grab
    // mode and the timing it started from are latched at press, so
    // applying the *total* delta to the original (not an incremental
    // delta to the current) lets a drag that clamped at 0 spring back.
    // A layer with no range has nothing to drag.
    let Some(timing) = timing else { return false };
    let drag_id = ui.id().with(("strip_drag", row.id));
    if resp.drag_started() {
        // `press_origin`, not `interact_pointer_pos`: egui only fires
        // `drag_started` once the pointer crosses its threshold, by
        // which point the pointer has already left the handle it
        // grabbed — which would read every trim as a slide.
        if let Some(p) = ui.input(|i| i.pointer.press_origin()) {
            let alt = ui.input(|i| i.modifiers.alt);
            let grab = clip_grab_at(p.x, bar.left(), bar.right(), HANDLE_W, alt);
            let anchor = axis.x_to_frame(p.x);
            ui.ctx().data_mut(|d| {
                d.insert_temp(drag_id, (grab, anchor, timing));
                // Ripple and roll act on the row as it stood at the press.
                d.insert_temp(drag_id.with("clips"), clips.to_vec());
            });
        }
    }
    if resp.dragged() {
        let latched: Option<(ClipGrab, i64, LayerTiming)> =
            ui.ctx().data(|d| d.get_temp(drag_id));
        if let (Some((grab, anchor, orig)), Some(p)) =
            (latched, resp.interact_pointer_pos())
        {
            let mut d = axis.x_to_frame(p.x) - anchor;
            // The magnet: pull whichever edge is moving onto the nearest target.
            if snap_on {
                let edge = |f: i64| snap.to(f + d, row.id, axis).map(|t| (t - (f + d), t));
                let pick = match grab {
                    ClipGrab::TrimIn => edge(orig.in_),
                    ClipGrab::TrimOut => edge(orig.out),
                    ClipGrab::Slide => match (edge(orig.in_), edge(orig.out)) {
                        (Some(a), Some(b)) => Some(if a.0.abs() <= b.0.abs() { a } else { b }),
                        (a, b) => a.or(b),
                    },
                    ClipGrab::Slip => None,
                };
                if let Some((shift, target)) = pick {
                    d += shift;
                    let x = axis.frame_to_x(target as f64);
                    ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("snap_line"))).line_segment(
                        [egui::pos2(x, track.top() - ROW_H), egui::pos2(x, track.bottom() + ROW_H)],
                        egui::Stroke::new(1.0, SNAP),
                    );
                }
            }
            let snap: Vec<Clip> = ui.ctx().data(|m| m.get_temp(drag_id.with("clips"))).unwrap_or_default();
            let side = match grab {
                ClipGrab::TrimIn => Some(Side::Start),
                ClipGrab::TrimOut => Some(Side::End),
                _ => None,
            };
            let patch = match (tool, side) {
                (EditTool::Ripple, Some(side)) if row.depth == 0 => {
                    Some(crate::clips::ripple_trim(&snap, row.id, side, d))
                }
                (EditTool::Roll, Some(side)) if row.depth == 0 => crate::clips::roll(&snap, row.id, side, d),
                _ => {
                    let next = drag_clip(orig, grab, d);
                    (next != timing).then(|| Patch::from([(row.id, next)]))
                }
            };
            match patch {
                // A nested layer has no row and no sync group: a plain edit.
                Some(p) if row.depth > 0 => out.set_layer_timing = p.get(&row.id).map(|t| (row.id, Some(*t))),
                Some(p) if !p.is_empty() => out.clip_op = Some(Op::Patch(p)),
                _ => {}
            }
        }
        *dragging = true;
    }
    match tool {
        EditTool::Ripple | EditTool::Roll if row.depth == 0 => {
            let held: Option<(ClipGrab, i64, LayerTiming)> = ui.ctx().data(|d| d.get_temp(drag_id));
            let alt = ui.input(|i| i.modifiers.alt);
            let pos = if resp.dragged() { resp.interact_pointer_pos() } else { resp.hover_pos() };
            let grab = match (resp.dragged(), held, pos) {
                (true, Some(h), _) => Some(h.0),
                (false, _, Some(p)) if p.x > bar.left() - HANDLE_W && p.x < bar.right() + HANDLE_W => {
                    Some(clip_grab_at(p.x, bar.left(), bar.right(), HANDLE_W, alt))
                }
                _ => None,
            };
            match (grab, pos) {
                (Some(g @ (ClipGrab::TrimIn | ClipGrab::TrimOut)), Some(p)) => {
                    let start = g == ClipGrab::TrimIn;
                    tool_cursor(ui, if tool == EditTool::Ripple { ToolCursor::Ripple { start } } else { ToolCursor::Roll }, p);
                }
                _ => clip_cursor(ui, resp, drag_id, bar.left(), bar.right(), HANDLE_W),
            }
        }
        _ => clip_cursor(ui, resp, drag_id, bar.left(), bar.right(), HANDLE_W),
    }
    drag
}

const RAZOR: egui::Color32 = egui::Color32::from_rgb(255, 96, 96);
const RIPPLE: egui::Color32 = egui::Color32::from_rgb(255, 216, 51);
const ROLL: egui::Color32 = egui::Color32::from_rgb(110, 200, 255);
const SNAP: egui::Color32 = egui::Color32::from_rgb(90, 230, 200);

/// The pointer shapes egui has no icon for, drawn at the mouse with the system
/// pointer hidden: a blade for the razor, a bracket for a ripple trim (open
/// toward the clip, arrow the way it pushes), a double bracket for a roll. The
/// palette's buttons draw the same shapes, plus a select arrow and a magnet.
#[derive(Clone, Copy, PartialEq)]
enum ToolCursor {
    Select,
    Razor,
    Ripple { start: bool },
    Roll,
    Magnet,
}

/// `kind`'s strokes around `p` at `scale` (1 = cursor size, ~20 px tall), and its color.
fn glyph(kind: ToolCursor, p: egui::Pos2, scale: f32) -> (Vec<[egui::Pos2; 2]>, egui::Color32) {
    let v = |x: f32, y: f32| p + egui::vec2(x, y) * scale;
    let mut lines: Vec<[egui::Pos2; 2]> = Vec::new();
    let poly = |pts: &[(f32, f32)], closed: bool, lines: &mut Vec<[egui::Pos2; 2]>| {
        let n = pts.len();
        for i in 0..if closed { n } else { n - 1 } {
            let (a, b) = (pts[i], pts[(i + 1) % n]);
            lines.push([v(a.0, a.1), v(b.0, b.1)]);
        }
    };
    let color = match kind {
        ToolCursor::Select => {
            poly(&[(-4.0, -9.0), (-4.0, 6.0), (-0.5, 2.5), (2.0, 8.5), (4.0, 7.5), (1.5, 1.5), (6.0, 1.5)], true, &mut lines);
            egui::Color32::from_gray(230)
        }
        ToolCursor::Razor => {
            // A blade: the cutting edge down through the hotspot, a grip above.
            lines.push([v(0.0, -10.0), v(0.0, 10.0)]);
            poly(&[(-4.0, -10.0), (4.0, -10.0), (4.0, -4.0), (-4.0, -4.0)], true, &mut lines);
            RAZOR
        }
        ToolCursor::Ripple { start } => {
            let s = if start { 1.0 } else { -1.0 }; // caps point into the clip
            poly(&[(4.0 * s, -9.0), (0.0, -9.0), (0.0, 9.0), (4.0 * s, 9.0)], false, &mut lines);
            lines.push([v(-2.0 * s, 0.0), v(-10.0 * s, 0.0)]);
            poly(&[(-6.0 * s, -4.0), (-10.0 * s, 0.0), (-6.0 * s, 4.0)], false, &mut lines);
            RIPPLE
        }
        ToolCursor::Roll => {
            for s in [-1.0f32, 1.0] {
                let x = 2.5 * s;
                poly(&[(x - 4.0 * s, -9.0), (x, -9.0), (x, 9.0), (x - 4.0 * s, 9.0)], false, &mut lines);
                lines.push([v(x + 2.0 * s, 0.0), v(12.0 * s, 0.0)]);
                poly(&[(8.0 * s, -4.0), (12.0 * s, 0.0), (8.0 * s, 4.0)], false, &mut lines);
            }
            ROLL
        }
        ToolCursor::Magnet => {
            // A horseshoe: two legs joined by a half circle, poles at the top.
            let arc: Vec<(f32, f32)> = (0..=8)
                .map(|i| {
                    let a = std::f32::consts::PI * i as f32 / 8.0;
                    (-6.0 * a.cos(), 1.0 + 6.0 * a.sin())
                })
                .collect();
            let mut u = vec![(-6.0, -8.0)];
            u.extend(arc);
            u.push((6.0, -8.0));
            poly(&u, false, &mut lines);
            lines.push([v(-8.0, -5.0), v(-4.0, -5.0)]);
            lines.push([v(4.0, -5.0), v(8.0, -5.0)]);
            SNAP
        }
    };
    (lines, color)
}

fn stroke_glyph(painter: &egui::Painter, kind: ToolCursor, p: egui::Pos2, scale: f32, color: Option<egui::Color32>) {
    let (lines, c) = glyph(kind, p, scale);
    // Dark halo first so the shape reads on any bar or waveform.
    for l in &lines {
        painter.line_segment(*l, egui::Stroke::new(3.5 * scale, egui::Color32::from_black_alpha(200)));
    }
    for l in &lines {
        painter.line_segment(*l, egui::Stroke::new(1.5 * scale.max(0.8), color.unwrap_or(c)));
    }
}

fn tool_cursor(ui: &egui::Ui, kind: ToolCursor, p: egui::Pos2) {
    ui.ctx().set_cursor_icon(egui::CursorIcon::None);
    let painter = ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Tooltip, egui::Id::new("tool_cursor")));
    stroke_glyph(&painter, kind, p, 1.0, None);
}

/// A palette button: the tool's glyph, then its name. Drawn like a selectable
/// label, so it sits with egui's own controls. Off, the glyph is grey.
fn glyph_button(ui: &mut egui::Ui, on: bool, kind: ToolCursor, label: &str) -> egui::Response {
    let font = egui::TextStyle::Button.resolve(ui.style());
    let galley = ui.painter().layout_no_wrap(label.to_owned(), font, ui.visuals().text_color());
    let pad = ui.spacing().button_padding;
    let icon = 16.0;
    let size = egui::vec2(pad.x * 2.0 + icon + 4.0 + galley.size().x, (galley.size().y + pad.y * 2.0).max(22.0));
    let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click());
    if ui.is_rect_visible(rect) {
        let vis = ui.style().interact_selectable(&resp, on);
        if on || resp.hovered() {
            ui.painter().rect(rect, vis.corner_radius, vis.weak_bg_fill, vis.bg_stroke, egui::StrokeKind::Inside);
        }
        let c = egui::pos2(rect.left() + pad.x + icon / 2.0, rect.center().y);
        let tint = (!on).then(|| egui::Color32::from_gray(150));
        stroke_glyph(ui.painter(), kind, c, 0.7, tint);
        let text_pos = egui::pos2(c.x + icon / 2.0 + 4.0, rect.center().y - galley.size().y / 2.0);
        ui.painter().galley(text_pos, galley, vis.text_color());
    }
    resp
}

/// An on/off pill: a track with a knob that slides across, then the label.
fn pill(ui: &mut egui::Ui, on: &mut bool, label: &str) -> egui::Response {
    let h = ui.spacing().interact_size.y * 0.8;
    let (rect, mut resp) = ui.allocate_exact_size(egui::vec2(h * 1.8, h), egui::Sense::click());
    if resp.clicked() {
        *on = !*on;
        resp.mark_changed();
    }
    if ui.is_rect_visible(rect) {
        let t = ui.ctx().animate_bool_responsive(resp.id, *on);
        let vis = ui.style().interact_selectable(&resp, *on);
        let r = rect.height() / 2.0;
        ui.painter().rect(rect, r, vis.bg_fill, vis.bg_stroke, egui::StrokeKind::Inside);
        let x = egui::lerp((rect.left() + r)..=(rect.right() - r), t);
        ui.painter().circle(egui::pos2(x, rect.center().y), r * 0.75, vis.fg_stroke.color, egui::Stroke::NONE);
    }
    let text = ui.add(egui::Label::new(label).sense(egui::Sense::click()));
    if text.clicked() {
        *on = !*on;
        resp.mark_changed();
    }
    resp | text
}

/// The magnet: frames an edit snaps to, and whether it is on. Hold Ctrl to
/// flip it for one gesture.
struct Magnet {
    on: bool,
    /// Clip edges (with the layer they belong to, so a clip ignores its own),
    /// the playhead, the comp's ends and the work area.
    targets: Vec<(Option<NodeId>, i64)>,
}

/// Within this many pixels an edge is pulled onto a target.
const SNAP_PX: f32 = 8.0;

impl Magnet {
    fn active(&self, ui: &egui::Ui) -> bool {
        self.on != ui.input(|i| i.modifiers.ctrl)
    }

    /// The nearest target to frame `f` within [`SNAP_PX`], not counting layer `own`'s edges.
    fn to(&self, f: i64, own: NodeId, axis: &Axis) -> Option<i64> {
        nearest(&self.targets, f, own, |a, b| (axis.frame_to_x(a as f64) - axis.frame_to_x(b as f64)).abs(), SNAP_PX)
    }
}

fn nearest(targets: &[(Option<NodeId>, i64)], f: i64, own: NodeId, dist: impl Fn(i64, i64) -> f32, max: f32) -> Option<i64> {
    targets
        .iter()
        .filter(|(id, _)| *id != Some(own))
        .map(|(_, t)| (dist(*t, f), *t))
        .filter(|(d, _)| *d <= max)
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, t)| t)
}

#[cfg(test)]
mod snap_tests {
    /// The magnet takes the closest target in range and never a clip's own edges.
    #[test]
    fn snapping_takes_the_nearest_target_but_not_your_own_edges() {
        use super::NodeId;
        let t = [(Some(NodeId(1)), 10), (Some(NodeId(2)), 14), (None, 30)];
        let px = |a: i64, b: i64| (a - b).abs() as f32; // one pixel per frame
        assert_eq!(super::nearest(&t, 12, NodeId(9), px, 8.0), Some(10), "10 and 14 tie; first wins");
        assert_eq!(super::nearest(&t, 13, NodeId(9), px, 8.0), Some(14));
        assert_eq!(super::nearest(&t, 11, NodeId(1), px, 8.0), Some(14), "own edge skipped");
        assert_eq!(super::nearest(&t, 50, NodeId(9), px, 8.0), None, "out of range");
    }
}

fn playhead(ui: &egui::Ui, track: egui::Rect, axis: &Axis, frame: f64, col: egui::Color32) {
    let px = axis.frame_to_x(frame);
    ui.painter_at(track).line_segment(
        [egui::pos2(px, track.top()), egui::pos2(px, track.bottom())],
        egui::Stroke::new(1.5, col),
    );
}

/// The comp **root is skipped**: it is the composition itself, not a layer in
/// it, and offering to trim the thing that defines the time axis would be a
/// control with no coherent meaning.
pub(crate) fn strip_rows(
    root: &MNode,
    assets: &std::collections::BTreeMap<motion_core::AssetId, motion_core::Asset>,
) -> Vec<StripRow> {
    fn walk(
        node: &MNode,
        depth: usize,
        assets: &std::collections::BTreeMap<motion_core::AssetId, motion_core::Asset>,
        out: &mut Vec<StripRow>,
    ) {
        let mut keys: Vec<i64> =
            dope_rows(node).into_iter().flat_map(|r| r.frames).collect();
        keys.sort_unstable();
        keys.dedup();
        out.push(StripRow {
            id: node.id,
            name: node.name.clone(),
            depth,
            kind: crate::layers::RowKind::of(node),
            timing: node.timing,
            keys,
            audio: node.audio.as_ref().map(|a| (a.asset, a.enabled)),
            footage: match &node.shape {
                Some(motion_core::Shape::Image { asset, time_remap, .. }) => assets
                    .get(asset)
                    .map(|a| (a.clone(), time_remap.is_some())),
                _ => None,
            },
            children: node.children.len(),
            sync: node.sync,
        });
        // Front-most first, matching the layers panel — the two are the same
        // list of layers and must not disagree about their order.
        for c in node.children.iter().rev() {
            walk(c, depth + 1, assets, out);
        }
    }
    let mut out = Vec::new();
    for child in root.children.iter().rev() {
        walk(child, 0, assets, &mut out);
    }
    out
}

/// Width of a bar's trim handle, in points. Shared with the dopesheet's clip
/// bar so the same grab means the same thing in both places.
const HANDLE_W: f32 = 6.0;

/// Draw a sound's shape across a strip's bar.
///
/// One vertical segment per pixel column, spanning that column's loudest
/// excursion either side of the centre line — the standard reading, and the
/// one that makes a beat a visible landmark rather than a number.
///
/// `start` is the layer's local frame zero in comp frames, which is what turns
/// a comp frame into a position in the source: the same conversion
/// `collect_audio` makes when it decides which sample the in-point plays, so
/// what you see and what you hear cannot disagree.
fn waveform(
    painter: &egui::Painter,
    bar: egui::Rect,
    axis: &Axis,
    fps: f64,
    start: f64,
    peaks: &motion_render::Peaks,
    enabled: bool,
) {
    if bar.width() < 1.0 || fps <= 0.0 {
        return;
    }
    let col = if enabled {
        egui::Color32::from_rgba_unmultiplied(190, 220, 255, 190)
    } else {
        egui::Color32::from_rgba_unmultiplied(190, 190, 190, 80)
    };
    let mid = bar.center().y;
    // Inset so a full-scale sample stops short of the bar's edge instead of
    // merging with its outline.
    let half = (bar.height() / 2.0 - 2.0).max(1.0);
    let mut x = bar.left().floor();
    while x < bar.right() {
        let f0 = axis.x_to_frame_exact(x);
        let f1 = axis.x_to_frame_exact(x + 1.0);
        if let Some((lo, hi)) = peaks.range((f0 - start) / fps, (f1 - start) / fps) {
            let top = mid - (hi.clamp(-1.0, 1.0) * half);
            let bot = mid - (lo.clamp(-1.0, 1.0) * half);
            // A quiet column is still a column: without the floor, near
            // silence draws nothing and reads as "no sound here".
            let (top, bot) = if bot - top < 1.0 { (mid - 0.5, mid + 0.5) } else { (top, bot) };
            painter.line_segment(
                [egui::pos2(x + 0.5, top), egui::pos2(x + 0.5, bot)],
                egui::Stroke::new(1.0, col),
            );
        }
        x += 1.0;
    }
}

/// Draw which of a footage layer's frames are decoded and resident, as a thin
/// band along the bottom of its bar.
///
/// The green bar every NLE has, and for the same reason: the cache is the
/// difference between a scrub that is instant and one that restarts a decoder,
/// and without this the user can only find out by trying it.
///
/// Read per pixel column rather than per frame so a zoomed-out comp costs the
/// width of the bar instead of the length of the footage — and so a column
/// standing for several frames is only claimed as cached when **all** of them
/// are. Optimism here would be the one lie that matters: it would promise a
/// smooth playback that isn't there.
///
/// `start` is the layer's local frame zero in comp frames, the same conversion
/// [`waveform`] makes, so picture and sound cannot disagree about where in the
/// source a comp frame lands.
/// Whether one pixel column of a cache bar is cached: every source frame it
/// covers is resident.
///
/// **All**, not any. Zoomed out, one column stands for many frames, and a
/// column drawn green because one of thirty frames happened to be decoded
/// would promise playback that isn't there — the single lie this bar exists to
/// prevent, since its whole job is to say what will be smooth.
pub(crate) fn column_cached(
    asset: &motion_core::Asset,
    cached: &std::collections::HashSet<crate::footage::Key>,
    f0: f64,
    f1: f64,
    fps: f64,
) -> bool {
    let (a, b) = (asset.source_frame(f0, fps), asset.source_frame(f1, fps));
    (a.min(b)..=a.max(b)).all(|sf| cached.contains(&(asset.id, sf)))
}

fn cache_bar(
    painter: &egui::Painter,
    bar: egui::Rect,
    axis: &Axis,
    fps: f64,
    start: f64,
    asset: &motion_core::Asset,
    cached: &std::collections::HashSet<crate::footage::Key>,
) {
    if bar.width() < 1.0 {
        return;
    }
    // Along the bottom edge, where it reads as a status gutter for the bar
    // rather than as content in it — the waveform already owns the middle.
    let band = egui::Rect::from_min_max(
        egui::pos2(bar.left(), bar.bottom() - 3.0),
        egui::pos2(bar.right(), bar.bottom() - 1.0),
    );
    let col = egui::Color32::from_rgb(96, 190, 120);
    let mut x = band.left().floor();
    while x < band.right() {
        let f0 = axis.x_to_frame_exact(x) - start;
        let f1 = axis.x_to_frame_exact(x + 1.0) - start;
        if column_cached(asset, cached, f0, f1, fps) {
            painter.line_segment(
                [egui::pos2(x + 0.5, band.top()), egui::pos2(x + 0.5, band.bottom())],
                egui::Stroke::new(1.0, col),
            );
        }
        x += 1.0;
    }
}

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
    peaks: &std::collections::HashMap<
        motion_core::AssetId,
        std::sync::Arc<motion_render::Peaks>,
    >,
    cached: &std::collections::HashSet<crate::footage::Key>,
    out: &mut DopeEdits,
) {
    let label_w = clamp_label_w(label_w, ui.max_rect().width());
    timeline_header(ui, TimelineMode::Strips, frame, last_frame, view, out);

    // Tool palette and row packing. View state, kept for the session.
    let (tool_id, packed_id, magnet_id) =
        (egui::Id::new("strip_tool"), egui::Id::new("strip_packed"), egui::Id::new("strip_magnet"));
    let (mut tool, mut packed, mut magnet): (EditTool, bool, bool) = ui.data(|d| {
        (d.get_temp(tool_id).unwrap_or_default(), d.get_temp(packed_id).unwrap_or(false), d.get_temp(magnet_id).unwrap_or(true))
    });
    ui.horizontal(|ui| {
        ui.add_space(8.0);
        for t in EditTool::ALL {
            if glyph_button(ui, tool == t, t.glyph(), t.label()).on_hover_text(t.hint()).clicked() {
                tool = t;
            }
        }
        ui.separator();
        if glyph_button(ui, magnet, ToolCursor::Magnet, "Magnet")
            .on_hover_text("Edges, slides and razor cuts snap to clip edges, the playhead and the work area. Hold Ctrl to flip it for one drag.")
            .clicked()
        {
            magnet = !magnet;
        }
        ui.separator();
        pill(ui, &mut packed, "Packed rows").on_hover_text("Layers that don't overlap share a row, like clips on a track");
    });
    ui.data_mut(|d| {
        d.insert_temp(tool_id, tool);
        d.insert_temp(packed_id, packed);
        d.insert_temp(magnet_id, magnet);
    });
    // The top-level layers as clips, back to front, packed into rows.
    let clips: Vec<Clip> = {
        let whole = LayerTiming::new(0, last_frame + 1);
        let top: Vec<&StripRow> = rows.iter().filter(|r| r.depth == 0).rev().collect();
        let win: Vec<(i64, i64)> = top.iter().map(|r| r.timing.map_or((0, last_frame + 1), |t| (t.in_, t.out))).collect();
        top.iter()
            .zip(crate::clips::pack(&win))
            .map(|(r, row)| Clip { id: r.id, row, t: r.timing.unwrap_or(whole) })
            .collect()
    };

    let snap = {
        let mut targets: Vec<(Option<NodeId>, i64)> = rows
            .iter()
            .filter_map(|r| r.timing.map(|t| (r.id, t)))
            .flat_map(|(id, t)| [(Some(id), t.in_), (Some(id), t.out)])
            .collect();
        targets.extend([(None, frame.round() as i64), (None, 0), (None, last_frame + 1)]);
        if let Some(w) = work_area {
            targets.extend([(None, w.start), (None, w.end)]);
        }
        Magnet { on: magnet, targets }
    };

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

    // The rows scroll under the ruler, which stays pinned above them so it is
    // always there to scrub. Floating scroll bars take no width, so every
    // row's track keeps the ruler's axis.
    egui::ScrollArea::vertical().id_salt("timeline_rows").auto_shrink([false; 2]).show(ui, |ui| {
        if rows.is_empty() {
            ui.add_space(8.0);
            ui.weak("This composition has no layers yet.");
        }

        if packed {
            let n = clips.iter().map(|c| c.row + 1).max().unwrap_or(0);
            // Highest row on top: later rows hold what was stacked over earlier.
            for r in (0..n).rev() {
                ui.horizontal(|ui| {
                    ui.add_space(8.0);
                    let cell = label_cell(ui, label_w, ROW_H);
                    label_text(ui, cell, &format!("Row {}", r + 1), true);
                    ui.add_space(SPLIT_W);
                    let (track, _) =
                        ui.allocate_exact_size(egui::vec2(ui.available_width() - 8.0, ROW_H), egui::Sense::hover());
                    ui.painter_at(track).rect_filled(track, 3.0, egui::Color32::from_gray(32));
                    for c in clips.iter().filter(|c| c.row == r) {
                        let Some(row) = rows.iter().find(|x| x.id == c.id) else { continue };
                        let bar = egui::Rect::from_min_max(
                            egui::pos2(axis.frame_to_x(c.t.in_ as f64).max(track.left()), track.top()),
                            egui::pos2(axis.frame_to_x(c.t.out as f64).min(track.right()), track.bottom()),
                        );
                        if bar.width() <= 0.0 {
                            continue;
                        }
                        let resp =
                            ui.interact(bar, ui.id().with(("strip_packed", c.id)), egui::Sense::click_and_drag());
                        dragging |= clip_bar(
                            ui, track, &resp, row, selected, &axis, tb, frame, last_frame, peaks, cached, tool, &clips,
                            &snap, out,
                        );
                        let name = egui::Rect::from_min_max(bar.min + egui::vec2(4.0, 0.0), bar.max);
                        label_text(ui, name, &row.name, selected != Some(row.id));
                    }
                    playhead(ui, track, &axis, frame, playhead_col);
                });
            }
            return;
        }

        // Rows opened by hand, by id. View state for the session, not saved.
        let open_id = egui::Id::new("strip_open");
        let mut open: std::collections::HashSet<NodeId> = ui.data(|d| d.get_temp(open_id)).unwrap_or_default();
        for row in visible_rows(rows, &open, selected) {
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
                    ui.interact(name_cell, ui.id().with(("strip_name", row.id)), egui::Sense::click());
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
                // The fold arrow for a row with layers inside, then the type
                // glyph in its accent, then the name beside it.
                let arrow = egui::Rect::from_min_size(name_cell.min, egui::vec2(12.0, name_cell.height()));
                if row.children > 0 {
                    let resp = ui.interact(arrow, ui.id().with(("strip_fold", row.id)), egui::Sense::click());
                    let is_open = open.contains(&row.id);
                    let c = if resp.hovered() { ui.visuals().strong_text_color() } else { ui.visuals().text_color() };
                    let m = arrow.center();
                    let pts = if is_open {
                        vec![m + egui::vec2(-4.0, -2.0), m + egui::vec2(4.0, -2.0), m + egui::vec2(0.0, 3.0)]
                    } else {
                        vec![m + egui::vec2(-2.0, -4.0), m + egui::vec2(3.0, 0.0), m + egui::vec2(-2.0, 4.0)]
                    };
                    ui.painter_at(arrow).add(egui::Shape::convex_polygon(pts, c, egui::Stroke::NONE));
                    if resp.on_hover_text(if is_open { "Collapse" } else { "Expand" }).clicked() && !open.remove(&row.id) {
                        open.insert(row.id);
                    }
                }
                let name_cell = name_cell.with_min_x(arrow.right());
                ui.painter_at(name_cell).text(
                    egui::pos2(name_cell.left(), name_cell.center().y),
                    egui::Align2::LEFT_CENTER,
                    row.kind.glyph(),
                    egui::FontId::new(13.0, egui::FontFamily::Name(icon::FAMILY.into())),
                    row.kind.accent(),
                );
                let name_cell = name_cell.with_min_x(name_cell.left() + 18.0);
                label_text(ui, name_cell, &row.name, !is_sel);

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
                ui.painter_at(track).rect_filled(track, 3.0, egui::Color32::from_gray(32));
                dragging |= clip_bar(
                    ui, track, &resp, row, selected, &axis, tb, frame, last_frame, peaks, cached, tool, &clips, &snap, out,
                );
                playhead(ui, track, &axis, frame, playhead_col);
            });
        }
        ui.data_mut(|d| d.insert_temp(open_id, open));
    });

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
