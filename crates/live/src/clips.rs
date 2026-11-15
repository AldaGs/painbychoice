//! NLE edits on a comp's top-level layers: razor, ripple, roll, plus the
//! auto-packed rows they act within (ADR 0025).
//!
//! Ported from Sonant's `tools.ts` / `tracks.ts` (and their tests), onto
//! [`LayerTiming`]. The mapping is `at = in_`, `len = out - in_`, and Sonant's
//! media `offset` is implicit: PBC's local time is `comp - start`, so moving
//! `in_` alone already trims a head without moving the content. That is why
//! there is no offset to patch, and why a razor needs no keyframe surgery — both
//! halves keep `start`, so every key stays on the comp frame it was on.
//!
//! The document has no tracks. A **row** is computed: layers are packed, back to
//! front, into the lowest row they don't overlap in. Ripple and roll act within
//! a row, the way an NLE's act within a track. Layers sharing a `sync` id are
//! sync-locked: an edit to one applies the same change to the others.
//!
//! Free functions over a comp root, applied after the UI pass (invariant 12).

use std::collections::BTreeMap;

use crate::{LayerTiming, NodeId};

use crate::MNode;

/// Shortest clip an edit may leave, in frames.
const MIN: i64 = 1;

/// A top-level layer as the tools see it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Clip {
    pub(crate) id: NodeId,
    pub(crate) row: usize,
    pub(crate) t: LayerTiming,
}

/// Lowest row for each `(in, out)` window, given back-to-front order: each goes
/// in the first row it overlaps nothing in. Half-open, so touching clips share.
pub(crate) fn pack(windows: &[(i64, i64)]) -> Vec<usize> {
    let mut rows: Vec<Vec<(i64, i64)>> = Vec::new();
    windows
        .iter()
        .map(|&(a, b)| {
            let r = rows.iter().position(|row| row.iter().all(|&(x, y)| b <= x || a >= y)).unwrap_or(rows.len());
            if r == rows.len() {
                rows.push(Vec::new());
            }
            rows[r].push((a, b));
            r
        })
        .collect()
}

/// The comp's top-level layers as clips, packed. A layer with no range spans
/// the whole comp (`0..frames`), which is what it draws over.
pub(crate) fn clips(root: &MNode, frames: i64) -> Vec<Clip> {
    let ts: Vec<(NodeId, LayerTiming)> =
        root.children.iter().map(|n| (n.id, n.timing.unwrap_or(LayerTiming::new(0, frames)))).collect();
    let rows = pack(&ts.iter().map(|(_, t)| (t.in_, t.out)).collect::<Vec<_>>());
    ts.into_iter().zip(rows).map(|((id, t), row)| Clip { id, row, t }).collect()
}

/// New timings, by layer.
pub(crate) type Patch = BTreeMap<NodeId, LayerTiming>;

fn shift(t: LayerTiming, d: i64) -> LayerTiming {
    LayerTiming { start: t.start + d, in_: t.in_ + d, out: t.out + d }
}

/// Everything after `c` in its row (starting at or past `end`), shifted by `d`.
fn ripple_followers(clips: &[Clip], c: &Clip, end: i64, d: i64, out: &mut Patch) {
    for x in clips.iter().filter(|x| x.id != c.id && x.row == c.row && x.t.in_ >= end) {
        let mut t = shift(x.t, d);
        // Never past frame 0; the content moves with the window.
        if t.in_ < 0 {
            t = shift(t, -t.in_);
        }
        out.insert(x.id, t);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Side {
    Start,
    End,
}

/// Ripple-trim one edge by `d` frames. End: the clip gets longer or shorter and
/// its row's later clips move by as much. Start: the clip stays put, its head is
/// trimmed (the content slides under it), and later clips move the other way.
pub(crate) fn ripple_trim(clips: &[Clip], id: NodeId, side: Side, d: i64) -> Patch {
    let mut out = Patch::new();
    let Some(c) = clips.iter().find(|c| c.id == id) else { return out };
    let t = c.t;
    let delta = match side {
        Side::End => {
            let delta = d.max(MIN - t.len());
            out.insert(id, LayerTiming { out: t.out + delta, ..t });
            delta
        }
        Side::Start => {
            let dd = d.min(t.len() - MIN); // >0 trims the head off
            out.insert(id, LayerTiming { start: t.start - dd, out: t.out - dd, ..t });
            -dd
        }
    };
    if delta == 0 {
        return Patch::new();
    }
    ripple_followers(clips, c, t.out, delta, &mut out);
    out
}

/// Close the gap a deleted clip leaves: its row's later clips move back by its
/// length. The caller removes the clip itself.
pub(crate) fn ripple_delete(clips: &[Clip], id: NodeId) -> Patch {
    let mut out = Patch::new();
    if let Some(c) = clips.iter().find(|c| c.id == id) {
        ripple_followers(clips, c, c.t.out, -c.t.len(), &mut out);
    }
    out
}

/// The clip touching `id`'s edge in the same row — its partner across the cut.
pub(crate) fn neighbour(clips: &[Clip], id: NodeId, side: Side) -> Option<Clip> {
    let c = clips.iter().find(|c| c.id == id)?;
    clips
        .iter()
        .find(|x| {
            x.id != id
                && x.row == c.row
                && match side {
                    Side::End => x.t.in_ == c.t.out,
                    Side::Start => x.t.out == c.t.in_,
                }
        })
        .copied()
}

/// Roll the cut at `id`'s edge by `d`: the left clip's out and the right clip's
/// in move together, so the total stays the same. `None` with no partner.
pub(crate) fn roll(clips: &[Clip], id: NodeId, side: Side, d: i64) -> Option<Patch> {
    let c = *clips.iter().find(|c| c.id == id)?;
    let n = neighbour(clips, id, side)?;
    let (l, r) = if side == Side::End { (c, n) } else { (n, c) };
    let dd = d.clamp(MIN - l.t.len(), r.t.len() - MIN);
    Some(Patch::from([
        (l.id, LayerTiming { out: l.t.out + dd, ..l.t }),
        (r.id, LayerTiming { in_: r.t.in_ + dd, ..r.t }),
    ]))
}

/// The two halves of a cut at comp frame `f`, or `None` unless both keep at
/// least a frame. Both keep `start`, so the content doesn't move.
pub(crate) fn split(t: LayerTiming, f: i64) -> Option<(LayerTiming, LayerTiming)> {
    (f - t.in_ >= MIN && t.out - f >= MIN).then(|| (LayerTiming { out: f, ..t }, LayerTiming { in_: f, ..t }))
}

// --- sync groups --------------------------------------------------------------

/// Spread a patch over sync groups: a layer that shares a `sync` id with a
/// patched one gets the same change — the same edge deltas — unless the patch
/// already names it.
pub(crate) fn with_sync(root: &MNode, frames: i64, mut patch: Patch) -> Patch {
    let timing = |n: &MNode| n.timing.unwrap_or(LayerTiming::new(0, frames));
    let edits: Vec<(Option<u64>, LayerTiming, LayerTiming)> = patch
        .iter()
        .filter_map(|(id, new)| root.children.iter().find(|n| n.id == *id).map(|n| (n.sync, timing(n), *new)))
        .collect();
    for (group, old, new) in edits {
        let Some(g) = group else { continue };
        for n in root.children.iter().filter(|n| n.sync == Some(g)) {
            patch.entry(n.id).or_insert_with(|| {
                let t = timing(n);
                LayerTiming {
                    start: t.start + new.start - old.start,
                    in_: (t.in_ + new.in_ - old.in_).max(0),
                    out: (t.out + new.out - old.out).max(t.in_ + MIN),
                }
            });
        }
    }
    patch
}

/// The layers a delete or razor of `id` takes with it: `id` and its sync group.
pub(crate) fn synced(root: &MNode, id: NodeId) -> Vec<NodeId> {
    let g = root.children.iter().find(|n| n.id == id).and_then(|n| n.sync);
    root.children
        .iter()
        .filter(|n| n.id == id || (g.is_some() && n.sync == g))
        .map(|n| n.id)
        .collect()
}

// --- the op the strips view reports ------------------------------------------

/// One edit-tool action, reported by the strips view and applied after the UI
/// pass by [`run`].
#[derive(Clone, Debug)]
pub(crate) enum Op {
    /// New timings (a trim, slide, slip, ripple or roll). Spread over sync groups.
    Patch(Patch),
    /// Cut this layer and its sync group at a comp frame.
    Razor(NodeId, i64),
    /// Delete this layer and its sync group, closing the gaps.
    RippleDelete(NodeId),
    /// Put the first layer in the second's sync group (or a new one).
    Sync(NodeId, NodeId),
    /// Take a layer out of its sync group.
    Unsync(NodeId),
}

/// Apply `op` to comp `comp`. Returns whether the document changed.
pub(crate) fn run(project: &mut crate::MProject, comp: crate::CompId, op: Op, next_id: &mut u64) -> bool {
    let Some(c) = project.comps.get_mut(&comp) else { return false };
    let frames = c.duration_frames;
    let root = &mut c.root;
    match op {
        Op::Patch(p) => {
            let p = with_sync(root, frames, p);
            apply(root, &p);
            !p.is_empty()
        }
        Op::Razor(id, f) => {
            let ids = synced(root, id);
            !razor(root, frames, &ids, f, next_id).is_empty()
        }
        Op::RippleDelete(id) => {
            let ids = synced(root, id);
            let all = clips(root, frames);
            // ponytail: two synced layers in one row each close only their own
            // gap from the original layout; fine for the A/V-pair case.
            let mut p = Patch::new();
            for i in &ids {
                p.extend(ripple_delete(&all, *i).into_iter().filter(|(k, _)| !ids.contains(k)));
            }
            apply(root, &p);
            for i in ids {
                crate::app::delete_layer(project, comp, i);
            }
            true
        }
        Op::Sync(a, b) => {
            let fresh = root.children.iter().filter_map(|n| n.sync).max().map_or(1, |m| m + 1);
            let find = |id| root.children.iter().find(|n| n.id == id).and_then(|n| n.sync);
            let g = find(b).or(find(a)).unwrap_or(fresh);
            let mut hit = false;
            for n in root.children.iter_mut().filter(|n| n.id == a || n.id == b) {
                hit |= n.sync != Some(g);
                n.sync = Some(g);
            }
            hit
        }
        Op::Unsync(id) => root.children.iter_mut().find(|n| n.id == id).is_some_and(|n| n.sync.take().is_some()),
    }
}

// --- applying ------------------------------------------------------------------

/// Write a patch's timings into the comp's top-level layers.
pub(crate) fn apply(root: &mut MNode, patch: &Patch) {
    for n in root.children.iter_mut() {
        if let Some(t) = patch.get(&n.id) {
            n.timing = Some(*t);
        }
    }
}

/// Give `n` and everything inside it fresh ids from `next`.
fn reid(n: &mut MNode, next: &mut u64) {
    n.id = NodeId(*next);
    *next += 1;
    for c in n.children.iter_mut() {
        reid(c, next);
    }
}

/// Razor `ids` at comp frame `f`: each layer it lands inside is cut in two, the
/// right half a copy (fresh ids from `next`) placed just above it. Halves share
/// a fresh sync id when the original had one, so cut pairs stay paired.
/// Returns the right halves' ids.
pub(crate) fn razor(root: &mut MNode, frames: i64, ids: &[NodeId], f: i64, next: &mut u64) -> Vec<NodeId> {
    let mut made = Vec::new();
    let mut groups: BTreeMap<u64, u64> = BTreeMap::new();
    let fresh_sync = root.children.iter().filter_map(|n| n.sync).max().map_or(1, |m| m + 1);
    let mut i = 0;
    while i < root.children.len() {
        let n = &root.children[i];
        let halves = ids.contains(&n.id).then(|| split(n.timing.unwrap_or(LayerTiming::new(0, frames)), f)).flatten();
        if let Some((l, r)) = halves {
            let mut right = root.children[i].clone();
            reid(&mut right, next);
            right.timing = Some(r);
            if let Some(g) = right.sync {
                let k = groups.len() as u64;
                right.sync = Some(*groups.entry(g).or_insert(fresh_sync + k));
            }
            root.children[i].timing = Some(l);
            made.push(right.id);
            // Above the original in the stack (children draw back to front).
            root.children.insert(i + 1, right);
            i += 1;
        }
        i += 1;
    }
    made
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lt(in_: i64, len: i64) -> LayerTiming {
        LayerTiming::new(in_, in_ + len)
    }
    fn clip(id: u64, row: usize, t: LayerTiming) -> Clip {
        Clip { id: NodeId(id), row, t }
    }
    // Sonant's fixture in frames: a|b touch on row 0, c later on row 0, x on row 1.
    fn all() -> Vec<Clip> {
        vec![clip(1, 0, lt(0, 40)), clip(2, 0, lt(40, 30)), clip(3, 0, lt(90, 20)), clip(9, 1, lt(50, 20))]
    }
    fn ids(p: &Patch) -> Vec<u64> {
        p.keys().map(|k| k.0).collect()
    }

    #[test]
    fn packing_shares_rows_until_clips_overlap() {
        assert_eq!(pack(&[(0, 10), (10, 20), (5, 15), (0, 30), (20, 30)]), vec![0, 0, 1, 2, 0]);
    }

    #[test]
    fn ripple_end_moves_the_rest_of_the_row() {
        let p = ripple_trim(&all(), NodeId(1), Side::End, 10);
        assert_eq!(ids(&p), vec![1, 2, 3], "x is on another row");
        assert_eq!(p[&NodeId(1)].out, 50);
        assert_eq!(p[&NodeId(2)], shift(lt(40, 30), 10));
        assert_eq!(p[&NodeId(3)].in_, 100);
        // Never shorter than a frame.
        assert_eq!(ripple_trim(&all(), NodeId(1), Side::End, -999)[&NodeId(1)].len(), 1);
    }

    #[test]
    fn ripple_start_trims_the_head_in_place() {
        let p = ripple_trim(&all(), NodeId(1), Side::Start, 10);
        let a = p[&NodeId(1)];
        assert_eq!((a.in_, a.out, a.start), (0, 30, -10), "stays at 0; content slides 10 earlier");
        assert_eq!(p[&NodeId(2)].in_, 30);
        assert_eq!(p[&NodeId(3)].in_, 80);
        assert!(ripple_trim(&all(), NodeId(1), Side::Start, 0).is_empty());
    }

    #[test]
    fn ripple_delete_closes_the_gap() {
        let p = ripple_delete(&all(), NodeId(2));
        assert_eq!(ids(&p), vec![3]);
        assert_eq!(p[&NodeId(3)].in_, 60);
    }

    #[test]
    fn neighbours_touch_in_the_same_row() {
        assert_eq!(neighbour(&all(), NodeId(1), Side::End).map(|c| c.id), Some(NodeId(2)));
        assert_eq!(neighbour(&all(), NodeId(2), Side::Start).map(|c| c.id), Some(NodeId(1)));
        assert_eq!(neighbour(&all(), NodeId(2), Side::End), None);
    }

    #[test]
    fn roll_moves_one_cut_from_either_side() {
        let p = roll(&all(), NodeId(1), Side::End, 10).unwrap();
        assert_eq!(p[&NodeId(1)].out, 50);
        let b = p[&NodeId(2)];
        assert_eq!((b.in_, b.out, b.start), (50, 70, 40), "b's content stays put");
        assert_eq!(Some(p.clone()), roll(&all(), NodeId(2), Side::Start, 10));
        // Clamped so b keeps a frame.
        assert_eq!(roll(&all(), NodeId(1), Side::End, 999).unwrap()[&NodeId(2)].len(), 1);
        assert!(roll(&all(), NodeId(2), Side::End, 10).is_none());
    }

    #[test]
    fn split_keeps_the_content_in_place() {
        let t = LayerTiming { start: -5, in_: 10, out: 50 };
        let (l, r) = split(t, 30).unwrap();
        assert_eq!((l.in_, l.out, l.start), (10, 30, -5));
        assert_eq!((r.in_, r.out, r.start), (30, 50, -5));
        assert!(split(t, 10).is_none() && split(t, 50).is_none() && split(t, 60).is_none());
    }

    fn comp() -> MNode {
        let mut a = MNode::group(1, "a").with_child(MNode::group(2, "inside"));
        a.timing = Some(lt(0, 40));
        a.sync = Some(7);
        let mut b = MNode::group(3, "b");
        b.timing = Some(lt(0, 40));
        b.sync = Some(7);
        let mut c = MNode::group(4, "c");
        c.timing = Some(lt(0, 40));
        MNode::group(0, "root").with_child(a).with_child(b).with_child(c)
    }

    #[test]
    fn sync_groups_take_the_same_edit() {
        let root = comp();
        let p = with_sync(&root, 100, Patch::from([(NodeId(1), lt(5, 40))]));
        assert_eq!(ids(&p), vec![1, 3], "c is not in the group");
        assert_eq!(p[&NodeId(3)].in_, 5);
        assert_eq!(synced(&root, NodeId(3)), vec![NodeId(1), NodeId(3)]);
        assert_eq!(synced(&root, NodeId(4)), vec![NodeId(4)]);
    }

    #[test]
    fn razor_cuts_a_group_into_paired_halves_with_fresh_ids() {
        let mut root = comp();
        let mut next = 10;
        let ids = synced(&root, NodeId(1));
        let made = razor(&mut root, 100, &ids, 20, &mut next);
        assert_eq!(made, vec![NodeId(10), NodeId(12)], "a's copy re-ids its child too");
        let order: Vec<u64> = root.children.iter().map(|n| n.id.0).collect();
        assert_eq!(order, vec![1, 10, 3, 12, 4]);
        let right = &root.children[1];
        assert_eq!(right.children[0].id, NodeId(11));
        assert_eq!(right.timing.unwrap().in_, 20);
        assert_eq!(root.children[0].timing.unwrap().out, 20);
        // The right halves pair up with each other, not with the left halves.
        assert_eq!(right.sync, root.children[3].sync);
        assert_ne!(right.sync, Some(7));
        assert_eq!(root.children[4].timing, Some(lt(0, 40)), "c untouched");
    }

    #[test]
    fn ripple_delete_takes_the_sync_group_and_closes_the_gap() {
        let mut root = comp();
        let mut d = MNode::group(5, "after");
        d.timing = Some(lt(40, 10));
        root.children.push(d);
        let mut project = crate::MProject::single(crate::Comp::new(100.0, 100.0, root));
        let id = project.root;
        project.comps.get_mut(&id).unwrap().duration_frames = 100;
        let mut next = 20;
        assert!(run(&mut project, id, Op::RippleDelete(NodeId(1)), &mut next));
        let root = &project.comps[&id].root;
        let left: Vec<u64> = root.children.iter().map(|n| n.id.0).collect();
        assert_eq!(left, vec![4, 5], "a and its sync partner b are gone");
        assert_eq!(root.children[1].timing.unwrap().in_, 0, "the gap closed");
        assert!(run(&mut project, id, Op::Sync(NodeId(4), NodeId(5)), &mut next));
        let r = &project.comps[&id].root;
        assert!(r.children[0].sync.is_some() && r.children[0].sync == r.children[1].sync);
        assert!(run(&mut project, id, Op::Unsync(NodeId(4)), &mut next));
        assert!(!run(&mut project, id, Op::Unsync(NodeId(4)), &mut next));
    }
}
