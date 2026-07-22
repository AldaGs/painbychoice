//! The transport bar and the timeline: ruler, clip bar, and dopesheet.
//!
//! Moved verbatim out of `main.rs` when it was split by concern; the
//! only edit was widening visibility to `pub(crate)`.

use crate::*;

/// Build the bottom transport bar. Reads the current time / playing state and
/// writes user intent into `out`; it never touches `App` directly, so it can't
/// collide with the borrows in `render`.
///
/// Modelled on Blender's timeline header rather than a scrub slider: a cluster
/// of transport buttons, the frame readout, and the preview range as numeric
/// Start/End fields.
///
/// There is deliberately **no playhead slider**. The ruler's red playhead is
/// already a full-width scrubber over the same axis, and a second one that
/// disagreed about its mapping (the slider spanned `0..=last_frame` regardless
/// of zoom, the ruler spans the visible window) was two controls for one piece
/// of state — you could drag the slider and watch the playhead land somewhere
/// else once the timeline was zoomed.
pub(crate) fn transport_ui(
    ui: &mut egui::Ui,
    frame: i64,
    last_frame: i64,
    tb: motion_core::Timebase,
    playing: bool,
    key_frames: &[i64],
    work_area: Option<WorkArea>,
    out: &mut Transport,
) {
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.add_space(8.0);

        // Outer pair jumps to the ends of the preview range; inner pair steps
        // between keyframes. Both are disabled when there's nowhere to go, so
        // the buttons report the state of the animation rather than always
        // looking live.
        let (lo, hi) = loop_bounds(work_area, last_frame + 1);
        let prev = neighbor_key(key_frames, frame, false);
        let next = neighbor_key(key_frames, frame, true);

        if icon::button_enabled(ui, icon::RESTART, "Jump to the start of the preview range", frame != lo)
            .clicked()
        {
            out.restart = true;
        }
        if icon::button_enabled(ui, icon::PREV_KEY, "Previous keyframe", prev.is_some()).clicked() {
            out.scrub_to = prev;
        }

        let (glyph, tip) = if playing { (icon::PAUSE, "Pause") } else { (icon::PLAY, "Play") };
        if icon::button(ui, glyph, tip).clicked() {
            out.toggle = true;
        }

        if icon::button_enabled(ui, icon::NEXT_KEY, "Next keyframe", next.is_some()).clicked() {
            out.scrub_to = next;
        }
        if icon::button_enabled(ui, icon::JUMP_END, "Jump to the end of the preview range", frame != hi - 1)
            .clicked()
        {
            out.jump_end = true;
        }

        ui.separator();
        // Frame-domain readout: hh:mm:ss.ff plus the raw frame number,
        // monospaced so the digits don't jitter during playback.
        ui.label(
            egui::RichText::new(format!("{}  [{frame}/{last_frame}]", tb.timecode(frame as f64)))
                .monospace(),
        );

        // Preview range, as Blender's Start/End. These are the *same* state as
        // the B/N keys and the blue band on the ruler — no work area means the
        // whole comp, which is what the fields show by default. Both bounds are
        // inclusive here, so End is the last frame that plays.
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add_space(8.0);
            let mut end = work_area.map_or(last_frame, |w| w.end - 1);
            if ui
                .add(egui::DragValue::new(&mut end).speed(1.0).range(0..=last_frame))
                .on_hover_text("Last frame of the preview range (N)")
                .changed()
            {
                out.set_work_end = Some(end);
            }
            ui.weak("End");
            let mut start = work_area.map_or(0, |w| w.start);
            if ui
                .add(egui::DragValue::new(&mut start).speed(1.0).range(0..=last_frame))
                .on_hover_text("First frame of the preview range (B)")
                .changed()
            {
                out.set_work_start = Some(start);
            }
            ui.weak("Start");
        });
    });
}

/// One dopesheet row: an animated property and the frames of its keyframes.
pub(crate) struct DopeRow {
    pub(crate) label: &'static str,
    pub(crate) kind: PropKind,
    pub(crate) frames: Vec<i64>,
}

/// Gather the animated properties of a node into dopesheet rows.
pub(crate) fn dope_rows(node: &motion_core::Node) -> Vec<DopeRow> {
    PropKind::ALL
        .iter()
        .filter_map(|&kind| {
            let p = prop_of(node, kind)?;
            p.is_animated().then(|| DopeRow {
                label: kind.label(),
                kind,
                frames: p.key_frames(),
            })
        })
        .collect()
}

/// A keyframe's identity within a node: which property, which index.
pub(crate) type KeyRef = (PropKind, usize);

/// The dopesheet's keyframe selection. A `BTreeSet` so iteration order is
/// deterministic and indices come out sorted — which the group-move code
/// below relies on when it batches a selection per property.
pub(crate) type KeySelection = std::collections::BTreeSet<KeyRef>;

/// Bucket a selection into one `(property, sorted indices)` entry per property.
///
/// Relies on `BTreeSet<(PropKind, usize)>` ordering by property first: entries
/// for the same property are therefore *contiguous*, so a single pass that
/// extends the last bucket is enough. If `PropKind`'s `Ord` ever stops being
/// the primary key this silently starts producing duplicate buckets — hence
/// the test.
pub(crate) fn group_selection_by_prop(sel: &KeySelection) -> Vec<(PropKind, Vec<usize>)> {
    let mut out: Vec<(PropKind, Vec<usize>)> = Vec::new();
    for &(kind, index) in sel.iter() {
        match out.last_mut() {
            Some((k, idxs)) if *k == kind => idxs.push(index),
            _ => out.push((kind, vec![index])),
        }
    }
    out
}

/// Carry a keyframe selection across a retime.
///
/// A [`KeyRef`] is an *index* into a track, not a frame, so anything that
/// changes how many keys a track has invalidates it. Retiming does exactly
/// that: keys land on whole frames, so two keys less than a frame apart on the
/// new grid merge and every index after the merge shifts down. Left alone, the
/// dopesheet would keep drawing a selection that quietly refers to different
/// keys than the user picked — or, past the end, to none at all.
///
/// So the selection is re-resolved *through frames*: read each selected key's
/// frame on `before`, scale it the way the retime did, and look up whichever
/// key now sits there. Keys that merged collapse onto the survivor, which is
/// the honest answer — the key the user selected no longer exists separately.
pub(crate) fn remap_selection(
    sel: &KeySelection,
    before: &MNode,
    after: &MNode,
    ratio: f64,
) -> KeySelection {
    let mut out = KeySelection::new();
    for &(kind, idx) in sel.iter() {
        let Some(before_frames) = prop_of(before, kind).map(|p| p.key_frames()) else {
            continue;
        };
        let Some(&frame) = before_frames.get(idx) else {
            continue;
        };
        // The same rounding `Track::retime` applies, so the lookup below can be
        // an exact match rather than a nearest-neighbour guess.
        let target = (frame as f64 * ratio).round() as i64;
        if let Some(after_frames) = prop_of(after, kind).map(|p| p.key_frames()) {
            if let Some(new_idx) = after_frames.iter().position(|&f| f == target) {
                out.insert((kind, new_idx));
            }
        }
    }
    out
}

/// One property's worth of copied keyframes. Two variants because `Value<T>` is
/// generic and the transform's properties are either `Vec2` or `f64` — the enum
/// is the type-erasure boundary, so paste can only ever put `Vec2` keys back on
/// a `Vec2` property.
#[derive(Clone)]
pub(crate) enum ClipTrack {
    Vec2(Vec<Keyframe<Vec2>>),
    Num(Vec<Keyframe<f64>>),
    Color(Vec<Keyframe<MColor>>),
    Str(Vec<Keyframe<String>>),
}

impl ClipTrack {
    /// Frame of the earliest copied key, or `None` if nothing was copied.
    pub(crate) fn first_frame(&self) -> Option<i64> {
        match self {
            ClipTrack::Vec2(k) => k.first().map(|k| k.frame),
            ClipTrack::Num(k) => k.first().map(|k| k.frame),
            ClipTrack::Color(k) => k.first().map(|k| k.frame),
            ClipTrack::Str(k) => k.first().map(|k| k.frame),
        }
    }
}

/// Keyframes on the clipboard, with the frame they were copied from.
///
/// Storing `origin` (the earliest copied frame) rather than pre-baked offsets is
/// what makes paste land the *block* at the playhead with its internal spacing
/// intact, regardless of where in the timeline it was copied from.
#[derive(Clone)]
pub(crate) struct KeyClipboard {
    pub(crate) origin: i64,
    pub(crate) tracks: Vec<(PropKind, ClipTrack)>,
}

/// Read the outgoing-segment handles for a given property + keyframe index.
pub(crate) fn segment_handles_of(node: &MNode, kind: PropKind, index: usize) -> Option<(Handle, Handle)> {
    prop_of(node, kind)?.segment_handles(index)
}

/// What the dopesheet reports after a frame: seek, keyframe move, and/or a
/// change to which keyframe is selected.
#[derive(Default)]
pub(crate) struct DopeEdits {
    /// Frame to seek to. Already snapped to the grid.
    pub(crate) seek_to: Option<i64>,
    /// Drag delta in frames, applied to the whole selection as a rigid block.
    pub(crate) move_by: Option<i64>,
    /// A diamond was clicked → make it the selection.
    pub(crate) select_key: Option<KeyRef>,
    /// A diamond was ctrl/shift-clicked → add or remove it from the selection.
    pub(crate) toggle_key: Option<KeyRef>,
    /// Empty track was clicked → clear the keyframe selection.
    pub(crate) clear_selection: bool,
    /// A marquee is being dragged: every key inside it, this frame. Reported
    /// live (not on release) so the selection previews as the box is drawn.
    pub(crate) box_select: Option<KeySelection>,
    /// Zoom / pan produced a new visible window.
    pub(crate) set_view: Option<TimelineView>,
    /// The selected layer's time range was edited (trim / slide / slip), or
    /// cleared back to `None` — "live for the whole comp".
    pub(crate) set_timing: Option<Option<LayerTiming>>,
    /// The column splitter was dragged: the label column's new width. Already
    /// clamped against the panel, so it is safe to store as-is.
    pub(crate) set_label_w: Option<f32>,
    /// The dopesheet/curves selector was clicked.
    pub(crate) set_mode: Option<TimelineMode>,
    /// A key was dragged vertically in the curve editor: property, key index,
    /// channel, new value.
    pub(crate) set_channel_value: Option<(PropKind, usize, usize, f64)>,
    /// Tangent edits: property, segment index, and that segment's two handles.
    /// A list, not one: a tool applies to every selected key, and one drag of a
    /// *locked* tangent moves the segments on both sides of its key. Applied in
    /// order, so a later edit to the same segment wins.
    pub(crate) set_handles: Vec<(PropKind, usize, Handle, Handle)>,
    /// How the segment leaving a key interpolates. A list, for the same reason.
    pub(crate) set_interp: Vec<(PropKind, usize, Interp)>,
    /// Keys whose tangents were locked or unlocked.
    pub(crate) set_broken: Vec<(PropKind, usize, bool)>,
    /// The curve editor's vertical window changed (zoom, pan, or frame-all).
    pub(crate) set_value_view: Option<ValueView>,
    /// The curve editor's property column was clicked: the new set of plotted
    /// properties (empty = all of them).
    pub(crate) set_shown_props: Option<PropSelection>,
    /// A layer strip was clicked: select that layer.
    pub(crate) select_layer: Option<NodeId>,
    /// A named layer's time range was edited or cleared. Distinct from
    /// `set_timing`, which always means *the selected layer* — the strips view
    /// edits any row, selected or not.
    pub(crate) set_layer_timing: Option<(NodeId, Option<LayerTiming>)>,
}

/// What the clip bar needs to draw the selected layer's time range.
#[derive(Clone, Copy)]
pub(crate) struct ClipInfo {
    /// `None` = the layer has no time range yet (live for the whole comp).
    pub(crate) timing: Option<LayerTiming>,
}

/// Which part of a clip bar a drag grabbed. Decided once, at drag start, from
/// where the press landed — so a slide can't turn into a trim mid-drag when the
/// pointer crosses back over an edge handle.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum ClipGrab {
    /// The left edge: moves `in_` only (the content stays where it is).
    TrimIn,
    /// The right edge: moves `out` only.
    TrimOut,
    /// The body: moves all three together, so the clip plays the same content
    /// at a different comp time.
    Slide,
    /// Alt+drag anywhere on the bar: moves `start` only, so the window stays put
    /// and the content shifts *underneath* it — the clip shows an earlier or
    /// later part of its own animation. AE spells this the same way.
    Slip,
}

/// Decide what a press at `px` grabbed, given the clip bar's **painted** edges.
///
/// Painted, not raw: a clip can extend past the visible window (the default
/// range runs one frame past the view), and an edge you can't see is an edge
/// you can't grab — so the clamped end of the bar is what the drag reaches for.
///
/// Nearest edge wins when the handles overlap, which they do on a short clip or
/// one zoomed out to a few pixels wide. Testing `in_` first instead would let it
/// always claim the press, leaving the right edge of a narrow clip untrimmable.
/// `alt` short-circuits to a slip: held, the whole bar slips regardless of where
/// the press landed. Trimming an edge *while* slipping isn't a coherent gesture
/// — slip is about the content behind a fixed window — so alt wins outright
/// rather than combining with the edge handles.
pub(crate) fn clip_grab_at(px: f32, left: f32, right: f32, handle_w: f32, alt: bool) -> ClipGrab {
    if alt {
        return ClipGrab::Slip;
    }
    let (dl, dr) = ((px - left).abs(), (px - right).abs());
    if dl.min(dr) > handle_w {
        ClipGrab::Slide
    } else if dl <= dr {
        ClipGrab::TrimIn
    } else {
        ClipGrab::TrimOut
    }
}

/// Apply a drag of `delta` frames to `t`, given what the drag grabbed. Trims
/// clamp against each other (a clip can't be shorter than one frame) and `in_`
/// clamps at 0; sliding can't push the clip before frame 0 either.
///
/// **Deliberately comp-agnostic** (decided 2026-07-20): there is *no* upper
/// clamp against the comp's duration, so a layer's `out` may extend past the
/// comp end — a layer **may outlive the comp**, as in AE. It costs nothing
/// (evaluation is half-open `[in, out)` and the comp only renders `[0,
/// duration)`, so the overhang simply never draws) and keeps this a pure
/// function of the clip, needing no duration threaded in. If a hard clamp is
/// ever wanted, it belongs at the call site with the comp in hand, not here.
pub(crate) fn drag_clip(t: LayerTiming, grab: ClipGrab, delta: i64) -> LayerTiming {
    match grab {
        ClipGrab::TrimIn => LayerTiming { in_: (t.in_ + delta).clamp(0, t.out - 1), ..t },
        ClipGrab::TrimOut => LayerTiming { out: (t.out + delta).max(t.in_ + 1), ..t },
        ClipGrab::Slide => {
            let d = delta.max(-t.in_);
            LayerTiming { start: t.start + d, in_: t.in_ + d, out: t.out + d }
        }
        // Deliberately unclamped: `start` is where local frame 0 sits, and a
        // layer is free to show any part of its own timeline — including
        // negative local frames, which a track holds at its first key. AE
        // clamps slip to the bounds of the source footage; we have no footage,
        // so there is nothing to run out of.
        ClipGrab::Slip => LayerTiming { start: t.start + delta, ..t },
    }
}

/// The visible frame window of the timeline. Zoom and pan only ever change
/// this; every frame↔pixel mapping reads it, so the ruler, the keyframes, and
/// the playhead cannot drift out of agreement.
#[derive(Clone, Copy, Debug)]
pub(crate) struct TimelineView {
    /// Leftmost visible frame (fractional — panning is continuous).
    pub(crate) start: f64,
    /// How many frames fit across the track.
    pub(crate) visible: f64,
}

impl TimelineView {
    pub(crate) fn full(last_frame: i64) -> Self {
        Self { start: 0.0, visible: last_frame.max(1) as f64 }
    }

    /// Keep the window inside `0..=last_frame` and never narrower than a few
    /// frames (past that the diamonds are wider than their spacing anyway).
    pub(crate) fn clamped(self, last_frame: i64) -> Self {
        let total = last_frame.max(1) as f64;
        let visible = self.visible.clamp(4.0, total);
        let start = self.start.clamp(0.0, (total - visible).max(0.0));
        Self { start, visible }
    }
}

/// Scale the visible window by `factor` about `anchor`, keeping the frame at
/// `anchor` where it is. `factor < 1` zooms in. Shared by the wheel (anchored
/// at the cursor) and the zoom buttons (anchored at the playhead), so both
/// gestures agree about what zooming means.
pub(crate) fn zoomed(view: TimelineView, factor: f64, anchor: f64) -> TimelineView {
    let visible = view.visible * factor;
    let ratio = (anchor - view.start) / view.visible.max(1e-9);
    TimelineView { start: anchor - ratio * visible, visible }
}

/// The nearest keyframe strictly before / after `from`, for the transport's
/// key-stepping buttons. `keys` need not be sorted or deduped — rows are
/// gathered per property and then merged, so duplicates across properties are
/// normal. `None` when there is no key that way, which is what greys the
/// button out.
pub(crate) fn neighbor_key(keys: &[i64], from: i64, forward: bool) -> Option<i64> {
    if forward {
        keys.iter().copied().filter(|&k| k > from).min()
    } else {
        keys.iter().copied().filter(|&k| k < from).max()
    }
}

/// Keep the dopesheet's label column usable: wide enough to read a property
/// name, never so wide it leaves no track to draw keyframes on. The upper
/// bound is a fraction of the panel rather than a constant so the column can't
/// swallow a narrow panel — the exact bug the split is meant to fix.
pub(crate) fn clamp_label_w(w: f32, panel_w: f32) -> f32 {
    const MIN: f32 = 44.0;
    let max = (panel_w * 0.45).max(MIN);
    w.clamp(MIN, max)
}

/// AE's **work area**: a comp-level preview range in frames, half-open
/// `[start, end)`. This is **view state** — it bounds *playback looping*, never
/// evaluation — so it's deliberately not saved with the document (unlike a
/// layer's in/out, which is document state that changes the frame). `None` on
/// `App` means the whole comp.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct WorkArea {
    pub(crate) start: i64,
    /// Exclusive — the last previewed frame is `end - 1`, matching the layer
    /// clip's half-open window so "the frame at the end" isn't double-counted.
    pub(crate) end: i64,
}

/// The playback loop's frame bounds `[lo, hi)`: the work area clamped into the
/// comp, or the whole comp when there's none. Pure, so the clamping is
/// unit-tested rather than only exercised by playing. `hi > lo` always, so the
/// span is never empty.
pub(crate) fn loop_bounds(work_area: Option<WorkArea>, total_frames: i64) -> (i64, i64) {
    let total = total_frames.max(1);
    match work_area {
        Some(wa) => {
            let lo = wa.start.clamp(0, total - 1);
            let hi = wa.end.clamp(lo + 1, total);
            (lo, hi)
        }
        None => (0, total),
    }
}

/// Fold an absolute playback time `raw` into a loop span `[lo, hi)` (any unit,
/// here seconds). This is what makes playback cycle within the work area; a
/// span that collapsed to zero holds at `lo` rather than dividing by it.
pub(crate) fn wrap_into(raw: f64, lo: f64, hi: f64) -> f64 {
    let span = hi - lo;
    if span > 0.0 {
        lo + (raw - lo).rem_euclid(span)
    } else {
        lo
    }
}

/// The work area after setting its **start** at `frame` (AE's `B`). The other
/// edge is seeded from the comp extent on the first press, so one keystroke
/// makes a valid range. Pure, so the seeding is unit-tested. `loop_bounds`
/// re-clamps at read time, so a start dragged past the end still can't invert
/// the loop.
pub(crate) fn with_work_start(current: Option<WorkArea>, frame: i64, total_frames: i64) -> WorkArea {
    let total = total_frames.max(1);
    let end = current.map_or(total, |w| w.end);
    WorkArea { start: frame.clamp(0, total - 1), end }
}

/// The work area after setting its **end** at `frame` (AE's `N`). The end is
/// exclusive, so `frame` stays the last previewed frame; the start is seeded
/// from 0 on the first press.
pub(crate) fn with_work_end(current: Option<WorkArea>, frame: i64, total_frames: i64) -> WorkArea {
    let total = total_frames.max(1);
    let start = current.map_or(0, |w| w.start);
    WorkArea { start, end: (frame + 1).clamp(1, total) }
}

/// Maps frames to pixels across one track's inset width. Built once from the
/// ruler's rect and shared by every row below it.
#[derive(Clone, Copy)]
pub(crate) struct Axis {
    pub(crate) x0: f32,
    pub(crate) span: f32,
    pub(crate) view: TimelineView,
}

impl Axis {
    pub(crate) fn new(track: egui::Rect, view: TimelineView) -> Self {
        // Inset so keys on the first/last visible frame sit fully inside the
        // track rather than clipped at the edge.
        const PAD: f32 = 8.0;
        let x0 = track.left() + PAD;
        let span = ((track.right() - PAD) - x0).max(1.0);
        Self { x0, span, view }
    }

    pub(crate) fn px_per_frame(&self) -> f32 {
        self.span / self.view.visible as f32
    }

    pub(crate) fn frame_to_x(&self, f: f64) -> f32 {
        self.x0 + ((f - self.view.start) as f32) * self.px_per_frame()
    }

    pub(crate) fn x_to_frame_exact(&self, x: f32) -> f64 {
        self.view.start + ((x - self.x0) / self.px_per_frame()) as f64
    }

    /// Snapped to the grid — this is where clicking and dragging become
    /// frame-exact, regardless of zoom.
    pub(crate) fn x_to_frame(&self, x: f32) -> i64 {
        self.x_to_frame_exact(x).round() as i64
    }
}

/// Choose a ruler tick interval (in frames) that leaves at least `min_px`
/// between labels. Candidates are the 1-2-5-10 frame steps plus whole-second
/// multiples, so labels land on round timecodes once you zoom out.
pub(crate) fn tick_step(px_per_frame: f32, fps: f64, min_px: f32) -> i64 {
    let f = fps.round().max(1.0) as i64;
    let mut cands: Vec<i64> = vec![1, 2, 5, 10];
    for secs in [1i64, 2, 5, 10, 15, 30, 60, 120, 300, 600, 1800, 3600] {
        cands.push(secs * f);
    }
    cands.sort_unstable();
    cands.dedup();
    *cands
        .iter()
        .find(|c| px_per_frame * (**c as f32) >= min_px)
        .unwrap_or_else(|| cands.last().unwrap())
}

/// How hard the timeline should auto-pan for a pointer at `x`, given the
/// track's `left`/`right` edges and the width of the sensitive zone.
///
/// Returns -1..0 in the left zone, 0..1 in the right zone, 0 in the middle;
/// magnitude ramps linearly with depth so a nudge scrolls slowly and pinning
/// the pointer to the edge scrolls fast. Past the edge it saturates at ±1
/// rather than accelerating without bound.
pub(crate) fn edge_pan_intensity(x: f32, left: f32, right: f32, edge: f32) -> f32 {
    if edge <= 0.0 || right <= left {
        return 0.0;
    }
    if x < left + edge {
        -(((left + edge - x) / edge).min(1.0))
    } else if x > right - edge {
        ((x - (right - edge)) / edge).min(1.0)
    } else {
        0.0
    }
}

/// Allocate one row's label cell in the dopesheet's left column.
///
/// Every row — the ruler, the clip bar, each property — goes through this, so
/// the column boundary is identical on all of them *by construction*. The rows
/// used to allocate their own label areas with `allocate_ui_with_layout`, which
/// grows past the size it is given when its contents don't fit: a wide button
/// or a long name would push that one row's track right while its keyframes
/// were still positioned from the ruler's axis, so the row silently disagreed
/// with every other about where a frame is. `allocate_exact_size` cannot grow,
/// which is the whole point of using it here.
///
/// Returns the cell for the caller to paint or place a widget in.
pub(crate) fn label_cell(ui: &mut egui::Ui, w: f32, h: f32) -> egui::Rect {
    ui.allocate_exact_size(egui::vec2(w, h), egui::Sense::hover()).0
}

/// Draw a row's label text into its cell, clipped to the column so a long name
/// is cut off at the splitter instead of bleeding over the keyframes.
pub(crate) fn label_text(ui: &mut egui::Ui, cell: egui::Rect, text: &str, weak: bool) {
    let color = if weak {
        ui.style().visuals.weak_text_color()
    } else {
        ui.style().visuals.text_color()
    };
    ui.painter_at(cell).text(
        egui::pos2(cell.left(), cell.center().y),
        egui::Align2::LEFT_CENTER,
        text,
        egui::TextStyle::Body.resolve(ui.style()),
        color,
    );
}

pub(crate) const ROW_H: f32 = 22.0;
/// Gap between the label column and the track, where the splitter is drawn and
/// grabbed. Module scope because the curve editor splits its panel on the same
/// boundary — one number, so the two views can't disagree about where it is.
pub(crate) const SPLIT_W: f32 = 5.0;
pub(crate) const DOPESHEET_H: f32 = 178.0;
pub(crate) const RULER_H: f32 = 20.0;
/// What one press of the zoom buttons multiplies the visible span by. Matches
/// roughly three wheel notches, so the buttons and the wheel feel like the same
/// control at different granularities.
pub(crate) const ZOOM_STEP: f64 = 0.7;
/// Width of the auto-pan zone at each end of the track, in points.
pub(crate) const EDGE_PAN_W: f32 = 36.0;

/// Which view the timeline panel is showing. The dopesheet and the curve editor
/// are two readings of the same keyframes — same rows, same selection, same time
/// axis — so they share one panel and one dock slot rather than competing for
/// screen space as separate editors.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum TimelineMode {
    #[default]
    Dopesheet,
    Curves,
    /// One bar per layer: when each layer is alive, rather than what its keys
    /// do. The comp's structure in time.
    Strips,
}

impl TimelineMode {
    pub(crate) fn label(self) -> &'static str {
        match self {
            TimelineMode::Dopesheet => "Dopesheet",
            TimelineMode::Curves => "Curve Editor",
            TimelineMode::Strips => "Layer Strips",
        }
    }
}

/// The timeline panel's header: its name, the dopesheet/curves selector, and
/// the zoom controls. Shared by both views so the two never drift apart in
/// wording or in where the buttons sit — switching mode should change the
/// picture, not the furniture.
pub(crate) fn timeline_header(
    ui: &mut egui::Ui,
    mode: TimelineMode,
    frame: f64,
    last_frame: i64,
    view: TimelineView,
    out: &mut DopeEdits,
) {
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.add_space(8.0);
        ui.strong("Timeline");
        for m in [TimelineMode::Strips, TimelineMode::Dopesheet, TimelineMode::Curves] {
            if ui.selectable_label(mode == m, m.label()).clicked() && mode != m {
                out.set_mode = Some(m);
            }
        }

        // Zoom controls, right-aligned so they sit clear of the hint text and
        // stay put as the panel resizes. Buttons anchor at the playhead rather
        // than the view centre: the playhead is what you are looking at, and
        // zooming it off-screen is the classic annoyance here.
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add_space(8.0);
            if icon::button(ui, icon::ZOOM_FIT, "Fit the whole comp (Home)").clicked() {
                out.set_view = Some(TimelineView::full(last_frame));
            }
            if icon::button(ui, icon::ZOOM_OUT, "Zoom out").clicked() {
                out.set_view = Some(zoomed(view, 1.0 / ZOOM_STEP, frame).clamped(last_frame));
            }
            if icon::button(ui, icon::ZOOM_IN, "Zoom in").clicked() {
                out.set_view = Some(zoomed(view, ZOOM_STEP, frame).clamped(last_frame));
            }
            ui.weak(match mode {
                TimelineMode::Dopesheet => {
                    "— ctrl+click or drag a box to multi-select, drag to move them                      together, ctrl+C/V copies, Del removes, B/N set the preview range"
                }
                TimelineMode::Curves => {
                    "— drag a key to move it in time and value, drag a tangent to                      reshape, or pick a tool for the selected keys"
                }
                TimelineMode::Strips => {
                    "— drag a bar's end to trim, its body to slide, alt+drag to                      slip (the content moves, the window stays)"
                }
            });
        });
    });
    ui.separator();
}

/// The time ruler: work-area band, ticks, timecode, playhead — and scrubbing.
///
/// Shared by the dopesheet and the curve editor rather than written twice: the
/// ruler *is* the panel's time axis made visible, and two implementations would
/// eventually disagree about where a frame is. The caller allocates the rect
/// (the two views lay out differently) and passes its response; this draws into
/// it and reports whether a scrub drag is live.
#[allow(clippy::too_many_arguments)]
pub(crate) fn time_ruler(
    ui: &egui::Ui,
    rect: egui::Rect,
    axis: &Axis,
    resp: &egui::Response,
    frame: f64,
    last_frame: i64,
    tb: motion_core::Timebase,
    view: TimelineView,
    work_area: Option<WorkArea>,
    out: &mut DopeEdits,
) -> bool {
    let playhead_col = egui::Color32::from_rgb(240, 90, 90);
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 3.0, egui::Color32::from_gray(28));

    // Work-area band: a translucent strip over the previewed range, with a
    // brighter tick at each edge. Drawn under the ticks/playhead so it reads
    // as a background wash, not a foreground marker. Only when one is set —
    // no work area means "the whole comp", which needs no band.
    if let Some(wa) = work_area {
        let (lo, hi) = loop_bounds(Some(wa), last_frame + 1);
        let (x0, x1) = (axis.frame_to_x(lo as f64), axis.frame_to_x(hi as f64));
        let band =
            egui::Rect::from_min_max(egui::pos2(x0, rect.top()), egui::pos2(x1, rect.bottom()))
                .intersect(rect);
        painter.rect_filled(band, 0.0, egui::Color32::from_rgba_unmultiplied(80, 150, 235, 46));
        let edge = egui::Stroke::new(1.5, egui::Color32::from_rgb(120, 180, 245));
        for x in [x0, x1] {
            if rect.x_range().contains(x) {
                painter.line_segment([egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())], edge);
            }
        }
    }

    // Ticks. Minor ticks appear only once frames are far enough apart to be
    // legible as individual frames.
    let step = tick_step(axis.px_per_frame(), tb.fps(), 58.0);
    let minor = if axis.px_per_frame() >= 6.0 { 1 } else { 0 };
    let first = view.start.floor() as i64;
    let last = (view.start + view.visible).ceil() as i64;

    if minor > 0 {
        let mut f = first;
        while f <= last {
            if f % step != 0 {
                let x = axis.frame_to_x(f as f64);
                painter.line_segment(
                    [egui::pos2(x, rect.bottom() - 4.0), egui::pos2(x, rect.bottom())],
                    egui::Stroke::new(1.0, egui::Color32::from_gray(58)),
                );
            }
            f += 1;
        }
    }

    let mut f = first.div_euclid(step) * step;
    while f <= last {
        if f >= 0 {
            let x = axis.frame_to_x(f as f64);
            painter.line_segment(
                [egui::pos2(x, rect.top() + 3.0), egui::pos2(x, rect.bottom())],
                egui::Stroke::new(1.0, egui::Color32::from_gray(110)),
            );
            painter.text(
                egui::pos2(x + 3.0, rect.top() + 1.0),
                egui::Align2::LEFT_TOP,
                tb.timecode(f as f64),
                egui::FontId::monospace(9.0),
                egui::Color32::from_gray(165),
            );
        }
        f += step;
    }

    // Playhead marker on the ruler.
    let px = axis.frame_to_x(frame);
    painter.line_segment(
        [egui::pos2(px, rect.top()), egui::pos2(px, rect.bottom())],
        egui::Stroke::new(1.5, playhead_col),
    );

    // Dragging or clicking the ruler scrubs.
    if resp.clicked() || resp.dragged() {
        if let Some(p) = resp.interact_pointer_pos() {
            out.seek_to = Some(axis.x_to_frame(p.x).clamp(0, last_frame));
        }
    }
    resp.dragged()
}

/// Bottom dopesheet: one row per animated property, keyframes drawn as diamonds
/// along a shared time axis with a playhead line. Click a row's track to seek;
/// click a diamond to select it (Delete removes); drag a diamond to move it.
#[allow(clippy::too_many_arguments)]
pub(crate) fn dopesheet_ui(
    ui: &mut egui::Ui,
    rows: &[DopeRow],
    frame: f64,
    last_frame: i64,
    tb: motion_core::Timebase,
    view: TimelineView,
    selected_keys: &KeySelection,
    clip: Option<ClipInfo>,
    work_area: Option<WorkArea>,
    label_w: f32,
    out: &mut DopeEdits,
) {
    // The label column is user-resizable, but a panel that shrank since the
    // last drag must not leave it wider than the panel — so it is re-clamped
    // on every pass, not only when dragged.
    let label_w = clamp_label_w(label_w, ui.max_rect().width());

    timeline_header(ui, TimelineMode::Dopesheet, frame, last_frame, view, out);

    let accent = egui::Color32::from_rgb(255, 216, 51);
    let playhead_col = egui::Color32::from_rgb(240, 90, 90);
    // Set by any drag on the timeline (ruler scrub or keyframe drag).
    // Gates the edge auto-pan below.
    let mut dragging = false;

    // Top of the two-column area, for the full-height splitter drawn at the end.
    let columns_top = ui.cursor().top();

    // --- Ruler. Allocated with the same layout as a property row, so
    // its axis geometry is exactly the rows' axis geometry. ---
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

    let axis = axis.expect("ruler always allocates the axis");

    // --- Clip bar: the selected layer's time range, drawn on the same axis as
    // the keyframe rows. Drag an edge to trim, the body to slide. ---
    if let Some(clip) = clip {
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            // The cell is allocated at the shared column width like every other
            // row; the button is *placed* inside it, so a wide button can't
            // widen this row's column the way it could when the button drove
            // the layout.
            let cell = label_cell(ui, label_w, ROW_H);
            let btn = egui::Rect::from_min_size(
                cell.left_top(),
                egui::vec2(cell.width().min(24.0), cell.height()),
            );
            match clip.timing {
                // No range yet: one click gives the layer one covering the
                // whole comp, which is exactly what it does today — so
                // enabling trimming never moves anything on screen.
                None => {
                    if ui
                        .put(btn, egui::Button::new(icon::text(icon::TRIM)))
                        .on_hover_text("Give this layer a time range")
                        .clicked()
                    {
                        out.set_timing = Some(Some(LayerTiming::new(0, last_frame + 1)));
                    }
                }
                Some(_) => {
                    if ui
                        .put(btn, egui::Button::new(icon::text(icon::CLOSE)))
                        .on_hover_text(
                            "Back to full comp.\n\
                             Drag an end to trim, the body to slide, \
                             alt+drag to slip (content moves, window stays).",
                        )
                        .clicked()
                    {
                        out.set_timing = Some(None);
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

            let Some(timing) = clip.timing else {
                painter.text(
                    egui::pos2(track.left() + 6.0, track.center().y),
                    egui::Align2::LEFT_CENTER,
                    "live for the whole comp",
                    egui::FontId::proportional(10.0),
                    egui::Color32::from_gray(110),
                );
                return;
            };

            // The bar. Clamped to the track so a clip scrolled off-screen
            // still paints a sliver at the edge instead of drawing outside.
            let (x0, x1) = (axis.frame_to_x(timing.in_ as f64), axis.frame_to_x(timing.out as f64));
            let bar = egui::Rect::from_min_max(
                egui::pos2(x0.max(track.left()), track.top() + 3.0),
                egui::pos2(x1.min(track.right()), track.bottom() - 3.0),
            );
            // Handles live on the *painted* edges, not the raw ones. A clip can
            // extend past the visible window in either direction (the default
            // range runs to `last_frame + 1`, one frame past the view), and an
            // edge you can't see is an edge you can't grab — so the clamped end
            // of the bar is what a drag reaches for.
            let (grab_l, grab_r) = (bar.left(), bar.right());
            if bar.width() > 0.0 {
                painter.rect_filled(bar, 3.0, egui::Color32::from_rgb(58, 84, 120));
                painter.rect_stroke(
                    bar,
                    3.0,
                    egui::Stroke::new(1.0, accent),
                    egui::StrokeKind::Inside,
                );
                // Where local frame 0 sits: the slip marker, and the only
                // feedback a slip gives — the bar itself doesn't move, so
                // without this the gesture would look like nothing happening.
                let sx = axis.frame_to_x(timing.start as f64);
                if sx > bar.left() && sx < bar.right() {
                    painter.line_segment(
                        [egui::pos2(sx, bar.top()), egui::pos2(sx, bar.bottom())],
                        egui::Stroke::new(1.0, egui::Color32::from_gray(170)),
                    );
                } else {
                    // Slipped out of view: pin an arrow to the edge it went
                    // past, so "local 0 is off that way" stays legible instead
                    // of the marker just vanishing.
                    let (x, text) = if sx <= bar.left() {
                        (bar.left() + 3.0, "<")
                    } else {
                        (bar.right() - 3.0, ">")
                    };
                    if bar.width() > 10.0 {
                        painter.text(
                            egui::pos2(x, bar.center().y),
                            if sx <= bar.left() {
                                egui::Align2::LEFT_CENTER
                            } else {
                                egui::Align2::RIGHT_CENTER
                            },
                            text,
                            egui::FontId::proportional(11.0),
                            egui::Color32::from_gray(170),
                        );
                    }
                }
            }

            // Playhead over the bar, so the clip reads against the current time.
            let px = axis.frame_to_x(frame);
            painter.line_segment(
                [egui::pos2(px, track.top()), egui::pos2(px, track.bottom())],
                egui::Stroke::new(1.5, playhead_col),
            );

            // Drag. The grab mode and the timing the drag started from are both
            // latched at press: applying the *total* delta to the original (not
            // an incremental delta to the current) is what makes a drag that
            // clamps at 0 spring back when you drag away again.
            const HANDLE_W: f32 = 6.0;
            let drag_id = ui.id().with("clip_drag");
            if resp.drag_started() {
                // `press_origin`, not `interact_pointer_pos`: egui only fires
                // `drag_started` once the pointer has crossed its drag
                // threshold, and by then `interact_pointer_pos` reports where
                // the pointer is *now* — already off the handle and into the
                // body, so every trim read as a slide. The marquee below uses
                // the same input for the same reason.
                if let Some(p) = ui.input(|i| i.pointer.press_origin()) {
                    let alt = ui.input(|i| i.modifiers.alt);
                    let grab = clip_grab_at(p.x, grab_l, grab_r, HANDLE_W, alt);
                    let anchor = axis.x_to_frame(p.x);
                    ui.ctx().data_mut(|d| d.insert_temp(drag_id, (grab, anchor, timing)));
                }
            }
            if resp.dragged() {
                let latched: Option<(ClipGrab, i64, LayerTiming)> =
                    ui.ctx().data(|d| d.get_temp(drag_id));
                if let (Some((grab, anchor, orig)), Some(p)) = (latched, resp.interact_pointer_pos())
                {
                    let next = drag_clip(orig, grab, axis.x_to_frame(p.x) - anchor);
                    if next != timing {
                        out.set_timing = Some(Some(next));
                    }
                }
                dragging = true;
            }
        });
    }

    // --- Zoom / pan. Scroll anywhere over the panel; zoom keeps the
    // frame under the cursor pinned, which is what makes it feel like
    // zooming rather than jumping. ---
    let panel_rect = ui.max_rect();
    let (scroll, hover) =
        ui.input(|i| (i.smooth_scroll_delta, i.pointer.hover_pos()));
    if let Some(p) = hover.filter(|p| panel_rect.contains(*p)) {
        // egui rewrites a shift+wheel gesture into a *horizontal*
        // scroll, so the shift modifier is already gone by the time we
        // see it — a nonzero x delta is the pan signal, not `shift`.
        // (Trackpad sideways swipes land here too, which is right.)
        let next = if scroll.x != 0.0 {
            // Pan: one notch moves a tenth of the window.
            Some(TimelineView {
                start: view.start - (scroll.x as f64 / 120.0) * view.visible * 0.1,
                visible: view.visible,
            })
        } else if scroll.y != 0.0 {
            // Anchored at the cursor, where the buttons anchor at the playhead.
            let factor = (0.9f64).powf(scroll.y as f64 / 120.0);
            Some(zoomed(view, factor, axis.x_to_frame_exact(p.x)))
        } else {
            None
        };
        if let Some(next) = next {
            out.set_view = Some(next.clamped(last_frame));
        }
    }

    // --- Box-select. A drag that *starts* on empty track (rather than
    // on a diamond, which grabs the press first) draws a marquee; every
    // key inside it becomes the selection.
    //
    // The rect has to be known before the rows loop, but only a row's
    // response can tell us the drag began on a track — so the "a
    // marquee is live" flag round-trips through egui memory and is read
    // on the following frame. The one-frame lag is invisible: the
    // marquee has no area worth hit-testing until the pointer has
    // actually moved. ---
    let marquee_id = ui.id().with("marquee");
    let mut marquee_active: bool =
        ui.ctx().data(|d| d.get_temp(marquee_id).unwrap_or(false));
    let (press, latest, any_down) = ui.input(|i| {
        (i.pointer.press_origin(), i.pointer.latest_pos(), i.pointer.any_down())
    });
    if marquee_active && !any_down {
        // Released: the last live report already produced the selection.
        marquee_active = false;
        ui.ctx().data_mut(|d| d.insert_temp(marquee_id, false));
    }
    let marquee = match (marquee_active, press, latest) {
        (true, Some(a), Some(b)) => Some(egui::Rect::from_two_pos(a, b)),
        _ => None,
    };
    let mut marquee_hits = KeySelection::new();

    // No early return: the rows loop is a no-op on an empty slice, and
    // returning here would skip the edge auto-pan below (which should
    // still work while scrubbing the ruler with nothing selected).
    if rows.is_empty() {
        ui.add_space(8.0);
        ui.weak("Select a node with animated properties to see its keyframes.");
    }

    for (row_idx, row) in rows.iter().enumerate() {
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            let cell = label_cell(ui, label_w, ROW_H);
            label_text(ui, cell, row.label, false);

            // The track: full remaining width, fixed height.
            ui.add_space(SPLIT_W);
            let (track, track_resp) = ui.allocate_exact_size(
                egui::vec2(ui.available_width() - 8.0, ROW_H),
                egui::Sense::click_and_drag(),
            );
            if track_resp.drag_started() {
                ui.ctx().data_mut(|d| d.insert_temp(marquee_id, true));
            }
            let painter = ui.painter_at(track);
            painter.rect_filled(track, 3.0, egui::Color32::from_gray(32));

            let frame_to_x = |f: f64| axis.frame_to_x(f);
            let x_to_frame = |x: f32| axis.x_to_frame(x);

            // Playhead line.
            let px = frame_to_x(frame);
            painter.line_segment(
                [egui::pos2(px, track.top()), egui::pos2(px, track.bottom())],
                egui::Stroke::new(1.5, playhead_col),
            );

            // Click on empty track → seek and clear the key selection.
            if track_resp.clicked() {
                if let Some(p) = track_resp.interact_pointer_pos() {
                    out.seek_to = Some(x_to_frame(p.x).clamp(0, last_frame));
                    out.clear_selection = true;
                }
            }

            // Keyframe diamonds (interactive, drawn on top).
            let cy = track.center().y;
            for (key_idx, &kf) in row.frames.iter().enumerate() {
                let kx = frame_to_x(kf as f64);
                // Skip keys scrolled out of the window — otherwise
                // their hit rects stay live outside the visible track.
                if kx < track.left() - 2.0 || kx > track.right() + 2.0 {
                    continue;
                }
                if let Some(m) = marquee {
                    if m.contains(egui::pos2(kx, cy)) {
                        marquee_hits.insert((row.kind, key_idx));
                    }
                }
                let is_sel = selected_keys.contains(&(row.kind, key_idx));
                let r = if is_sel { 6.5 } else { 5.0 };
                let hit = egui::Rect::from_center_size(
                    egui::pos2(kx, cy),
                    egui::vec2(r * 2.4, r * 2.4),
                );
                let id = ui.id().with((row_idx, key_idx));
                let resp = ui.interact(hit, id, egui::Sense::click_and_drag());

                let col = if is_sel || resp.dragged() || resp.hovered() {
                    egui::Color32::WHITE
                } else {
                    accent
                };
                let border = if is_sel {
                    egui::Stroke::new(2.0, playhead_col)
                } else {
                    egui::Stroke::new(1.0, egui::Color32::from_gray(16))
                };
                // Diamond = a rotated square.
                let d = [
                    egui::pos2(kx, cy - r),
                    egui::pos2(kx + r, cy),
                    egui::pos2(kx, cy + r),
                    egui::pos2(kx - r, cy),
                ];
                painter.add(egui::Shape::convex_polygon(d.to_vec(), col, border));

                if resp.clicked() {
                    // Ctrl/⌘ or shift extends; a plain click replaces.
                    let mods = ui.input(|i| i.modifiers);
                    if mods.command || mods.shift {
                        out.toggle_key = Some((row.kind, key_idx));
                    } else {
                        out.select_key = Some((row.kind, key_idx));
                    }
                }
                if resp.dragged() {
                    dragging = true;
                    if let Some(p) = resp.interact_pointer_pos() {
                        // Dragging an unselected key selects it first,
                        // so the drag acts on what's under the cursor.
                        if !is_sel {
                            out.select_key = Some((row.kind, key_idx));
                        }
                        // Report a *delta* from this key's current
                        // frame, so the whole selection can move as a
                        // block. Recomputed each frame, so a clamped
                        // drag catches up once room appears.
                        let target = x_to_frame(p.x).clamp(0, last_frame);
                        let delta = target - kf;
                        if delta != 0 {
                            out.move_by = Some(delta);
                        }
                    }
                }
            }
        });
    }

    // --- Column splitter. One continuous strip spanning the ruler and every
    // row, rather than a handle per row: the columns are a single division of
    // the panel, so there is one thing to grab. Drawn last so it sits over the
    // row backgrounds, and interacted with after the rows so a drag near the
    // boundary resizes the column instead of scrubbing the track under it. ---
    {
        let columns_bottom = ui.cursor().top();
        let x = ui.max_rect().left() + 8.0 + label_w;
        let strip = egui::Rect::from_min_max(
            egui::pos2(x, columns_top),
            egui::pos2(x + SPLIT_W, columns_bottom.max(columns_top + RULER_H)),
        );
        let resp = ui.interact(strip, ui.id().with("col_splitter"), egui::Sense::drag());
        if resp.hovered() || resp.dragged() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
        }
        if resp.dragged() {
            // Drive the width from the pointer directly rather than
            // accumulating `drag_delta`, so a drag that clamps at either end
            // springs back the moment the pointer comes back in range.
            if let Some(p) = resp.interact_pointer_pos() {
                let w = p.x - (ui.max_rect().left() + 8.0) - SPLIT_W / 2.0;
                out.set_label_w = Some(clamp_label_w(w, ui.max_rect().width()));
            }
            dragging = true;
        }
        // A hairline, brighter while grabbed — otherwise the columns read as
        // one surface and the handle is invisible until you happen to hit it.
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

    // Report and draw the marquee. Reported even when empty, so
    // dragging a box over nothing clears the selection like a click on
    // empty track does.
    if let Some(m) = marquee {
        dragging = true;
        out.box_select = Some(std::mem::take(&mut marquee_hits));
        let painter = ui.painter_at(ui.max_rect());
        painter.rect_filled(m, 2.0, egui::Color32::from_white_alpha(18));
        painter.rect_stroke(
            m,
            2.0,
            egui::Stroke::new(1.0, accent),
            egui::StrokeKind::Inside,
        );
    }

    // --- Edge auto-pan. While dragging (scrubbing the ruler or moving
    // a keyframe), holding the pointer near either end of the track
    // scrolls the window that way — so you can drag a key past the
    // visible range without letting go. Deliberately drag-only: doing
    // this on plain hover would scroll the timeline out from under the
    // pointer whenever it drifted near an edge. ---
    if dragging {
        if let Some(p) = ui.input(|i| i.pointer.latest_pos()) {
            let intensity = edge_pan_intensity(
                p.x,
                axis.x0,
                axis.x0 + axis.span,
                EDGE_PAN_W,
            );
            if intensity != 0.0 {
                // Time-based so the speed doesn't depend on frame rate;
                // clamped in case a slow frame produces a huge dt.
                let dt = (ui.input(|i| i.stable_dt) as f64).min(0.05);
                let delta = intensity as f64 * view.visible * 0.8 * dt;
                out.set_view = Some(
                    TimelineView { start: view.start + delta, visible: view.visible }
                        .clamped(last_frame),
                );
                // Redraw is event-driven, so without this the pan stops
                // the moment the pointer stops moving.
                ui.ctx().request_repaint();
            }
        }
    }
}
