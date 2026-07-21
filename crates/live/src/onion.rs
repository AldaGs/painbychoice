//! Onion skins: ghosts of the frame either side of the playhead.
//!
//! ## Why this rather than more motion paths
//!
//! A motion path works for position because position *is* a spatial curve, in
//! the same space as the canvas. Nothing else on a layer has that property —
//! there is no natural geometry for "rotation over time", and inventing one per
//! property would mean a new visualisation for every property we ever add. A
//! ghost of the **rendered** layer sidesteps that entirely: rotation, scale,
//! opacity, fill, shape parameters and text all show up at once, because it is
//! the actual picture rather than a plot of one channel.
//!
//! ## Drawn by vello, unlike every other overlay
//!
//! The gizmo, the motion path and the aids are egui overlays. Ghosts are not:
//! they are *geometry* — filled and stroked paths — and vello already draws
//! exactly that. Going through the same rasteriser is what makes a ghost look
//! like a faded version of the frame instead of an approximation of it, and it
//! puts them under the current frame in the same scene rather than on a layer
//! above it.
//!
//! ## Cost
//!
//! Each ghost is a full `evaluate_comp`, so this is cached exactly like
//! [`MotionPath`]: rebuilt only when the selection, the playhead, the settings
//! or `App::doc_rev` move. Six ghosts is six evaluations per rebuild, which is
//! cheaper than the motion path's 121 — but each one retains its geometry, so
//! ghosts cost *memory* where the path costs only points.

use crate::*;

/// One ghost: the items to draw and how to fade them.
#[derive(Clone, Debug)]
pub(crate) struct Ghost {
    pub(crate) items: Vec<MRenderItem>,
    pub(crate) opacity: f64,
    /// Blended into every fill and stroke so the direction of time is readable
    /// at a glance — past warm, future cool, the Maya/Blender convention.
    pub(crate) tint: MColor,
}

/// The cached ghosts for the current frame.
#[derive(Clone, Debug, Default)]
pub(crate) struct OnionSkins {
    pub(crate) ghosts: Vec<Ghost>,
    key: Option<OnionKey>,
}

/// What a cached set of ghosts depends on. Anything here changing invalidates
/// them; nothing else may, or they silently lag the document.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct OnionKey {
    comp: CompId,
    node: Option<NodeId>,
    frame: i64,
    revision: u64,
    /// The settings, folded to a comparable shape (`f64` isn't `Eq`).
    shape: (bool, u32, u32, i64, u64),
}

fn key_shape(o: &Onion) -> (bool, u32, u32, i64, u64) {
    (o.visible, o.before, o.after, o.step, o.opacity.to_bits())
}

/// Past ghosts run warm, future ghosts cool.
const PAST: MColor = MColor::rgb(1.0, 0.45, 0.3);
const FUTURE: MColor = MColor::rgb(0.35, 0.65, 1.0);

impl OnionSkins {
    /// Rebuild if anything the ghosts depend on moved, otherwise keep them.
    /// Returns whether work was done, which only tests care about.
    pub(crate) fn cache(
        &mut self,
        project: &MProject,
        comp: CompId,
        selected: Option<NodeId>,
        frame: i64,
        onion: &Onion,
        revision: u64,
    ) -> bool {
        let key = OnionKey {
            comp,
            node: selected,
            frame,
            revision,
            shape: key_shape(onion),
        };
        if self.key == Some(key) {
            return false;
        }
        self.key = Some(key);
        self.ghosts.clear();

        let offsets = onion.offsets();
        if offsets.is_empty() {
            return true;
        }
        // With a selection, ghost just that subtree — you are usually watching
        // one layer move. With nothing selected, ghost the whole comp, which is
        // the "review the animation" case. Same cost either way: the evaluation
        // is whole-comp regardless and only the filter differs.
        let ids = selected.and_then(|id| {
            let node = project.comps.get(&comp)?.root.find(id)?;
            let mut v = Vec::new();
            collect_ids(node, &mut v);
            Some(v)
        });

        let last = project.comps.get(&comp).map(|c| c.duration_frames()).unwrap_or(0);
        for (offset, t) in offsets {
            let f = frame + offset;
            // Ghosts outside the comp are skipped rather than clamped: a clamped
            // ghost would pile duplicates of frame 0 on top of each other and
            // read as the animation stalling there.
            if f < 0 || f > last {
                continue;
            }
            let scene = evaluate_comp(project, comp, f as f64);
            let items: Vec<MRenderItem> = match &ids {
                Some(ids) => {
                    scene.items.iter().filter(|i| ids.contains(&i.source)).cloned().collect()
                }
                None => scene.items.clone(),
            };
            if items.is_empty() {
                continue;
            }
            self.ghosts.push(Ghost {
                items,
                // Fade to a quarter of the nearest ghost's opacity at the far
                // end, never to zero — an invisible ghost is just wasted work.
                opacity: onion.opacity.clamp(0.0, 1.0) * (1.0 - 0.75 * t),
                tint: if offset < 0 { PAST } else { FUTURE },
            });
        }
        true
    }

    pub(crate) fn clear(&mut self) {
        *self = Self::default();
    }
}

fn collect_ids(node: &MNode, out: &mut Vec<NodeId>) {
    out.push(node.id);
    for c in &node.children {
        collect_ids(c, out);
    }
}

/// Blend `c` toward `tint`, keeping some of the original so a ghost still says
/// what colour the layer was. Fully tinting them would turn a multi-coloured
/// scene into two flat silhouettes.
pub(crate) fn tinted(c: MColor, tint: MColor, amount: f64) -> MColor {
    let m = |a: f64, b: f64| a + (b - a) * amount;
    MColor::rgba(m(c.r, tint.r), m(c.g, tint.g), m(c.b, tint.b), c.a)
}

/// How far a ghost's colours are pulled toward its tint.
pub(crate) const TINT_AMOUNT: f64 = 0.55;
