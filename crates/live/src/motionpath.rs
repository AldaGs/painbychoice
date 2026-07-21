//! The motion path: where an animated layer's pivot travels, drawn on the
//! preview as a curve with a dot per frame.
//!
//! ## Why only position gets one
//!
//! Position *is* a spatial curve — it lives in the same space as the canvas, so
//! drawing it shows the data itself rather than a visualisation invented for
//! it. Nothing else on a layer has that property: a "rotation path" would be a
//! made-up mapping, and value-over-time for every other property is already
//! served better by the dopesheet and the graph editor. The whole-layer
//! equivalent for everything else is onion skinning, which ghosts the rendered
//! layer rather than plotting one channel.
//!
//! ## Why it samples the real evaluator
//!
//! Each sample is a full [`evaluate_comp`], and the point is read out of the
//! resulting scene's `places` table. That is not the cheap way — the cheap way
//! would walk the ancestor transforms directly — but it is the only way the path cannot
//! disagree with the canvas. Parent chains, expressions, pre-comp instancing
//! and `LayerTiming`'s local-frame shift all bend where a layer actually is,
//! and re-deriving that here would be a second implementation of `eval.rs`'s
//! walk, silently drifting from the first. Same reasoning as the gizmo emitting
//! ordinary `PropEdits` instead of writing the document itself.
//!
//! The cost is real and is paid by [`MotionPath::cache`]: a window of ±60
//! frames is 121 scene evaluations, so the path is rebuilt only when its key
//! changes (selection, frame window, or a document revision), never per UI
//! frame. It *is* rebuilt on every frame of a gizmo drag, since each drag delta
//! bumps the revision — that is the case to watch on a heavy comp, and the
//! first thing to optimise if this ever feels slow.

use crate::*;

/// A sampled path in **composition** space, plus which of its samples land on
/// keyframes. Comp space rather than screen space so it survives zoom and pan
/// without resampling — only the cheap projection re-runs each frame.
#[derive(Clone, Debug, Default)]
pub(crate) struct MotionPath {
    /// One point per frame in the window, in comp space. Frames where the layer
    /// doesn't exist (outside its `LayerTiming`) produce no point, so the path
    /// can have gaps — see `segments`.
    pub(crate) points: Vec<Option<Point>>,
    /// Frame number of `points[0]`, so a sample can be named.
    pub(crate) first_frame: i64,
    /// Indices into `points` that sit on a position keyframe.
    pub(crate) keys: Vec<usize>,
    /// The key this path was built for. `None` means "nothing cached".
    key: Option<PathKey>,
}

/// What a cached path depends on. Anything here changing invalidates it;
/// nothing else may, or the path silently lags the document.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PathKey {
    node: NodeId,
    comp: CompId,
    lo: i64,
    hi: i64,
    /// Bumped by `App` whenever the document changes. Without it the path would
    /// keep showing the pre-edit trajectory.
    revision: u64,
}

/// How far either side of the playhead a path is sampled, before clamping to
/// the comp. A guard, not a preference — the preference is
/// `Comp::motion_path_range`; this only stops a pathological value from
/// launching tens of thousands of scene evaluations.
pub(crate) const MAX_RANGE: i64 = 500;

impl MotionPath {
    /// Rebuild the path if `revision`/selection/window moved, otherwise keep
    /// what's cached. Returns whether anything was recomputed, which is only of
    /// interest to tests.
    pub(crate) fn cache(
        &mut self,
        project: &MProject,
        comp: CompId,
        node: NodeId,
        frame: i64,
        range: i64,
        revision: u64,
    ) -> bool {
        let Some(c) = project.comps.get(&comp) else {
            *self = Self::default();
            return false;
        };
        let range = range.clamp(0, MAX_RANGE);
        let last = c.duration_frames();
        let lo = (frame - range).max(0);
        let hi = (frame + range).min(last);
        let key = PathKey { node, comp, lo, hi, revision };
        if self.key == Some(key) {
            return false;
        }

        let mut points = Vec::with_capacity((hi - lo + 1).max(0) as usize);
        for f in lo..=hi {
            points.push(sample(project, comp, node, f));
        }
        self.keys = key_indices(c, node, lo, hi);
        self.points = points;
        self.first_frame = lo;
        self.key = Some(key);
        true
    }

    /// Drop the cache so the next `cache` call rebuilds. Used when the path
    /// should disappear (nothing selected, or position isn't animated) so a
    /// stale trajectory can't flash back on re-selection.
    pub(crate) fn clear(&mut self) {
        *self = Self::default();
    }

    /// Contiguous runs of visible points — the path is broken wherever the
    /// layer is outside its time range, rather than drawing a straight line
    /// across the gap to somewhere it never was.
    pub(crate) fn segments(&self) -> Vec<Vec<Point>> {
        let mut out: Vec<Vec<Point>> = Vec::new();
        let mut run: Vec<Point> = Vec::new();
        for p in &self.points {
            match p {
                Some(p) => run.push(*p),
                None => {
                    if run.len() > 1 {
                        out.push(std::mem::take(&mut run));
                    } else {
                        run.clear();
                    }
                }
            }
        }
        if run.len() > 1 {
            out.push(run);
        }
        out
    }
}

/// Where the layer's **pivot** is at `frame`, in comp space.
///
/// Read straight out of the evaluated scene's `places` table rather than from a
/// `RenderItem`: a group or a null draws nothing and so has no item, but it is
/// exactly the kind of layer you animate and want a path for. `None` means the
/// walk never reached the node — it is outside its time window on this frame —
/// which is the signal [`MotionPath::segments`] breaks the polyline on.
fn sample(project: &MProject, comp: CompId, node: NodeId, frame: i64) -> Option<Point> {
    evaluate_comp(project, comp, frame as f64).pivot(node)
}

/// Which sample indices land on a position keyframe, so they can be drawn
/// larger than the in-between dots.
fn key_indices(comp: &Comp, node: NodeId, lo: i64, hi: i64) -> Vec<usize> {
    let Some(n) = comp.root.find(node) else {
        return Vec::new();
    };
    n.transform
        .position
        .key_frames()
        .into_iter()
        .filter(|f| *f >= lo && *f <= hi)
        .map(|f| (f - lo) as usize)
        .collect()
}

const PATH_COL: egui::Color32 = egui::Color32::from_rgb(230, 230, 235);
const DOT_COL: egui::Color32 = egui::Color32::from_rgb(190, 195, 205);
const KEY_COL: egui::Color32 = egui::Color32::from_rgb(255, 216, 51);

/// Draw the path over the preview. Purely a projection of the cached comp-space
/// samples — no evaluation happens here, so this is cheap enough to run every
/// frame at any zoom.
pub(crate) fn draw(
    painter: &egui::Painter,
    path: &MotionPath,
    fit: Affine,
    ppp: f64,
    playhead_index: Option<usize>,
) {
    let to_screen = |p: Point| {
        let c = fit * p;
        egui::pos2((c.x / ppp) as f32, (c.y / ppp) as f32)
    };

    for seg in path.segments() {
        let pts: Vec<egui::Pos2> = seg.into_iter().map(to_screen).collect();
        painter.add(egui::Shape::line(pts, egui::Stroke::new(1.2, PATH_COL)));
    }

    // A dot per frame. Their *spacing* is the reading: bunched dots are slow
    // motion, spread dots are fast, which is the thing a bare curve can't show.
    for (i, p) in path.points.iter().enumerate() {
        let Some(p) = p else { continue };
        let at = to_screen(*p);
        if path.keys.contains(&i) {
            painter.rect_filled(
                egui::Rect::from_center_size(at, egui::Vec2::splat(6.0)),
                1.0,
                KEY_COL,
            );
        } else {
            painter.circle_filled(at, 1.6, DOT_COL);
        }
    }

    // The current frame, ringed so you can see where "now" sits on the path.
    if let Some(Some(p)) = playhead_index.map(|i| path.points.get(i).copied().flatten()) {
        painter.circle_stroke(to_screen(p), 4.5, egui::Stroke::new(1.6, KEY_COL));
    }
}
