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
//! Free functions over a comp root, in core so the editor and `motion apply`
//! share them (ADR 0028). The editor applies them after the UI pass
//! (invariant 12).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::node::{CompId, LayerTiming, Node as MNode, NodeId, Project};
use crate::subs::Word;

/// Shortest clip an edit may leave, in frames.
pub const MIN: i64 = 1;

/// A top-level layer as the tools see it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Clip {
    pub id: NodeId,
    pub row: usize,
    pub t: LayerTiming,
}

/// Lowest row for each `(in, out)` window, given back-to-front order: each goes
/// in the first row it overlaps nothing in. Half-open, so touching clips share.
pub fn pack(windows: &[(i64, i64)]) -> Vec<usize> {
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
pub fn clips(root: &MNode, frames: i64) -> Vec<Clip> {
    let ts: Vec<(NodeId, LayerTiming)> =
        root.children.iter().map(|n| (n.id, n.timing.unwrap_or(LayerTiming::new(0, frames)))).collect();
    let rows = pack(&ts.iter().map(|(_, t)| (t.in_, t.out)).collect::<Vec<_>>());
    ts.into_iter().zip(rows).map(|((id, t), row)| Clip { id, row, t }).collect()
}

/// New timings, by layer.
pub type Patch = BTreeMap<NodeId, LayerTiming>;

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
pub enum Side {
    Start,
    End,
}

/// Ripple-trim one edge by `d` frames. End: the clip gets longer or shorter and
/// its row's later clips move by as much. Start: the clip stays put, its head is
/// trimmed (the content slides under it), and later clips move the other way.
pub fn ripple_trim(clips: &[Clip], id: NodeId, side: Side, d: i64) -> Patch {
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
pub fn ripple_delete(clips: &[Clip], id: NodeId) -> Patch {
    let mut out = Patch::new();
    if let Some(c) = clips.iter().find(|c| c.id == id) {
        ripple_followers(clips, c, c.t.out, -c.t.len(), &mut out);
    }
    out
}

/// The clip touching `id`'s edge in the same row — its partner across the cut.
pub fn neighbour(clips: &[Clip], id: NodeId, side: Side) -> Option<Clip> {
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
pub fn roll(clips: &[Clip], id: NodeId, side: Side, d: i64) -> Option<Patch> {
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
pub fn split(t: LayerTiming, f: i64) -> Option<(LayerTiming, LayerTiming)> {
    (f - t.in_ >= MIN && t.out - f >= MIN).then(|| (LayerTiming { out: f, ..t }, LayerTiming { in_: f, ..t }))
}

// --- sync groups --------------------------------------------------------------

/// Spread a patch over sync groups: a layer that shares a `sync` id with a
/// patched one gets the same change — the same edge deltas — unless the patch
/// already names it.
pub fn with_sync(root: &MNode, frames: i64, mut patch: Patch) -> Patch {
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
pub fn synced(root: &MNode, id: NodeId) -> Vec<NodeId> {
    let g = root.children.iter().find(|n| n.id == id).and_then(|n| n.sync);
    root.children
        .iter()
        .filter(|n| n.id == id || (g.is_some() && n.sync == g))
        .map(|n| n.id)
        .collect()
}

// --- the op the strips view reports ------------------------------------------

/// Time taken out of a comp by [`cut_range`], kept so it can be put back
/// ([`restore_cut`]) after other edits, not only by undo.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Cut {
    /// Comp frame the cut starts at, in the timeline as it is now.
    pub at: i64,
    /// Frames removed.
    pub len: i64,
    /// The layers that were wholly inside, with their place in the stack.
    pub removed: Vec<(usize, MNode)>,
    /// Razor cuts made at the edges, (left, right), in the order made.
    pub joins: Vec<(NodeId, NodeId)>,
    /// Words taken from each subtitled layer, at their original times.
    pub words: Vec<(NodeId, Vec<Word>)>,
    /// Each subtitled layer's timing before the cut.
    pub timings: Vec<(NodeId, Option<LayerTiming>)>,
    /// What was said in the cut, for a list to show.
    pub text: String,
}

/// Cut comp frames `a..b` out of every top-level layer and pull what follows
/// left by `b - a`. Layers crossing an edge are razored there; a subtitled
/// layer is not split but loses the words spoken in the range, later words
/// moving up. Layers now wholly inside are taken out (into the returned
/// [`Cut`]); the caller deletes them from the project's graph too.
pub fn remove_range(root: &mut MNode, frames: i64, fps: f64, a: i64, b: i64, next: &mut u64) -> Cut {
    let mut cut = Cut { at: a, len: (b - a).max(0), removed: Vec::new(), joins: Vec::new(), words: Vec::new(), timings: Vec::new(), text: String::new() };
    if b <= a {
        return cut;
    }
    let d = b - a;
    let window = |n: &MNode| n.timing.unwrap_or(LayerTiming::new(0, frames));
    for f in [a, b] {
        let ids: Vec<NodeId> = root
            .children
            .iter()
            .filter(|n| n.subtitles.is_none() && window(n).in_ < f && f < window(n).out)
            .map(|n| n.id)
            .collect();
        cut.joins.extend(razor_pairs(root, frames, &ids, f, next));
    }
    let mut said = Vec::new();
    for n in root.children.iter_mut() {
        if let Some(s) = n.subtitles.as_mut() {
            let start = n.timing.map_or(0, |t| t.start);
            let (la, lb) = ((a - start) as f64 / fps, (b - start) as f64 / fps);
            let inside = |w: &Word| (la..lb).contains(&((w.start + w.end) / 2.0));
            let taken: Vec<Word> = s.transcript.words.iter().filter(|w| inside(w)).cloned().collect();
            s.transcript.words.retain(|w| !inside(w));
            for w in s.transcript.words.iter_mut().filter(|w| w.start >= lb) {
                (w.start, w.end) = (w.start - (lb - la), w.end - (lb - la));
            }
            said.extend(taken.iter().map(|w| w.text.clone()));
            cut.words.push((n.id, taken));
            cut.timings.push((n.id, n.timing));
            if let Some(t) = n.timing.as_mut() {
                if t.in_ >= b {
                    *t = shift(*t, -d);
                } else if t.out > a {
                    t.out = (t.out - d).max(a.max(t.in_ + MIN));
                }
            }
            continue;
        }
        let t = window(n);
        if t.in_ >= b {
            n.timing = Some(shift(t, -d));
        }
    }
    let mut i = 0;
    while i < root.children.len() {
        let n = &root.children[i];
        let t = window(n);
        if n.subtitles.is_none() && t.in_ >= a && t.out <= b {
            cut.removed.push((i, root.children.remove(i)));
        } else {
            i += 1;
        }
    }
    cut.text = said.join(" ");
    cut
}

/// Put cut `cut` back into `root`: later content moves right again, removed
/// layers return to their places, razor halves are joined, captions get
/// their words and timing back. Layers edited away since are skipped.
pub fn restore_range(root: &mut MNode, frames: i64, fps: f64, cut: &Cut) {
    let (a, d) = (cut.at, cut.len);
    let window = |n: &MNode| n.timing.unwrap_or(LayerTiming::new(0, frames));
    for n in root.children.iter_mut() {
        if let Some(s) = n.subtitles.as_mut() {
            let start = n.timing.map_or(0, |t| t.start);
            let (la, dl) = ((a - start) as f64 / fps, d as f64 / fps);
            for w in s.transcript.words.iter_mut().filter(|w| w.start >= la) {
                (w.start, w.end) = (w.start + dl, w.end + dl);
            }
            if let Some((_, words)) = cut.words.iter().find(|(id, _)| *id == n.id) {
                s.transcript.words.extend(words.iter().cloned());
                s.transcript.words.sort_by(|x, y| x.start.total_cmp(&y.start));
            }
            if let Some((_, t)) = cut.timings.iter().find(|(id, _)| *id == n.id) {
                n.timing = *t;
            }
            continue;
        }
        let t = window(n);
        if t.in_ >= a {
            n.timing = Some(shift(t, d));
        }
    }
    for (i, n) in &cut.removed {
        let at = (*i).min(root.children.len());
        root.children.insert(at, n.clone());
    }
    for (l, r) in cut.joins.iter().rev() {
        let Some(rt) = root.children.iter().find(|n| n.id == *r).and_then(|n| n.timing) else { continue };
        let Some(left) = root.children.iter_mut().find(|n| n.id == *l) else { continue };
        if let Some(lt) = left.timing.as_mut() {
            lt.out = rt.out;
            root.children.retain(|n| n.id != *r);
        }
    }
}

/// Text-based cutting on a project: cut frames `a..b` from `comp`, record the
/// cut, and delete what was inside from the graph too. Whether anything changed.
pub fn cut_range(project: &mut Project, comp: CompId, a: i64, b: i64, next: &mut u64) -> bool {
    let Some(c) = project.comps.get_mut(&comp) else { return false };
    if b <= a {
        return false;
    }
    let (frames, fps) = (c.duration_frames, c.fps);
    let cut = remove_range(&mut c.root, frames, fps, a, b, next);
    let d = cut.len;
    c.duration_frames = (frames - d).max(1);
    // ponytail: positions of older cuts are kept consistent for the common
    // case (restoring newest first); interleaved restores at one spot are approximate.
    for other in &mut c.cuts {
        if other.at > a {
            other.at = (other.at - d).max(a);
        }
    }
    fn ids(n: &MNode, out: &mut Vec<NodeId>) {
        out.push(n.id);
        n.children.iter().for_each(|c| ids(c, out));
    }
    let mut gone = Vec::new();
    cut.removed.iter().for_each(|(_, n)| ids(n, &mut gone));
    // Reserved while the cut can be restored, so no new layer takes one.
    c.removed_id_floor = c.removed_id_floor.max(gone.iter().map(|i| i.0 + 1).max().unwrap_or(0)).max(*next);
    c.cuts.push(cut);
    // Already out of the tree; the graph still has to forget them. Their ids
    // stay reserved by the cut, which may put them back.
    project.graph.forget_layers(comp, &gone);
    true
}

/// Put back cut `index` of `comp`. Whether it existed.
pub fn restore_cut(project: &mut Project, comp: CompId, index: usize) -> bool {
    let Some(c) = project.comps.get_mut(&comp) else { return false };
    if index >= c.cuts.len() {
        return false;
    }
    let cut = c.cuts.remove(index);
    let (frames, fps) = (c.duration_frames + cut.len, c.fps);
    restore_range(&mut c.root, frames, fps, &cut);
    c.duration_frames += cut.len;
    for other in &mut c.cuts {
        if other.at >= cut.at {
            other.at += cut.len;
        }
    }
    true
}

// --- applying ------------------------------------------------------------------

/// Write a patch's timings into the comp's top-level layers.
pub fn apply(root: &mut MNode, patch: &Patch) {
    for n in root.children.iter_mut() {
        if let Some(t) = patch.get(&n.id) {
            n.timing = Some(*t);
        }
    }
}

/// Duplicate layer `id` in place: a copy with fresh ids from `next`, named
/// "<name> copy", stacked just above the original. Returns the copy's id, or
/// `None` for the root or a missing id.
pub fn duplicate(root: &mut MNode, id: NodeId, next: &mut u64) -> Option<NodeId> {
    let parent = root.parent_of_mut(id)?;
    let i = parent.children.iter().position(|n| n.id == id)?;
    let mut copy = parent.children[i].clone();
    reid(&mut copy, next);
    copy.name = format!("{} copy", copy.name);
    // A copy is its own clip, not a member of the original's sync group.
    copy.sync = None;
    let new = copy.id;
    parent.children.insert(i + 1, copy);
    Some(new)
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
pub fn razor(root: &mut MNode, frames: i64, ids: &[NodeId], f: i64, next: &mut u64) -> Vec<NodeId> {
    razor_pairs(root, frames, ids, f, next).into_iter().map(|(_, r)| r).collect()
}

/// [`razor`], returning each cut as (left half, right half) — what a
/// [`Cut`] needs to join them again.
pub fn razor_pairs(root: &mut MNode, frames: i64, ids: &[NodeId], f: i64, next: &mut u64) -> Vec<(NodeId, NodeId)> {
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
            made.push((root.children[i].id, right.id));
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
    fn duplicate_stacks_a_reided_copy_above() {
        let mut root = comp();
        let mut next = 10;
        let new = duplicate(&mut root, NodeId(1), &mut next).unwrap();
        assert_eq!(new, NodeId(10));
        assert_eq!(next, 12, "the child got a fresh id too");
        let c = &root.children[1];
        assert_eq!((c.name.as_str(), c.sync, c.children[0].id), ("a copy", None, NodeId(11)));
        assert!(duplicate(&mut root, NodeId(0), &mut next).is_none(), "the root can't be duplicated");
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

    /// Cutting frames 20..30 razors the layers crossing them, drops the piece
    /// inside, pulls later content left, and takes the words out of a
    /// subtitled layer without splitting it.
    #[test]
    fn removing_a_range_cuts_everything_and_the_words() {
        use crate::subs::{Subtitles, Transcript, Word};
        let word = |t: &str, s: f64, e: f64| Word { text: t.into(), start: s, end: e, p: None, brk: Default::default(), locked: false };
        let mut root = MNode::group(0, "root");
        let mut a = MNode::group(1, "a");
        a.timing = Some(lt(0, 50));
        let mut b = MNode::group(2, "b");
        b.timing = Some(lt(40, 20));
        let mut cap = MNode::group(3, "cap");
        cap.subtitles = Some(Subtitles {
            transcript: Transcript { language: None, words: vec![word("keep", 0.0, 1.0), word("cut", 2.0, 3.0), word("moved", 4.0, 5.0)] },
            rules: Default::default(),
            uppercase: false,
        });
        root.children = vec![a, b, cap];
        let mut next = 10;
        let cut = remove_range(&mut root, 100, 10.0, 20, 30, &mut next);
        let gone: Vec<NodeId> = cut.removed.iter().map(|(_, n)| n.id).collect();
        assert_eq!(gone, vec![NodeId(10)], "a's middle piece is wholly inside");
        assert_eq!(cut.text, "cut");
        assert_eq!(root.children[0].timing.unwrap().out, 20);
        let tail = root.children.iter().find(|n| n.id == NodeId(11)).unwrap().timing.unwrap();
        assert_eq!((tail.in_, tail.out, tail.start), (20, 40, -10), "content keeps its place in the source");
        assert_eq!(root.children.iter().find(|n| n.id == NodeId(2)).unwrap().timing.unwrap().in_, 30);
        let words = &root.children.iter().find(|n| n.id == NodeId(3)).unwrap().subtitles.as_ref().unwrap().transcript.words;
        assert_eq!(words.iter().map(|w| w.text.as_str()).collect::<Vec<_>>(), ["keep", "moved"]);
        assert_eq!(words[1].start, 3.0);
    }

    /// Restoring a cut gives back exactly the timeline before it: the same
    /// layers with the same timing, and the words where they were.
    #[test]
    fn a_restored_cut_is_the_timeline_before_it() {
        use crate::subs::{Subtitles, Transcript, Word};
        let word = |t: &str, s: f64, e: f64| Word { text: t.into(), start: s, end: e, p: None, brk: Default::default(), locked: false };
        let mut a = MNode::group(1, "a");
        a.timing = Some(lt(0, 50));
        let mut b = MNode::group(2, "b");
        b.timing = Some(lt(40, 20));
        let mut cap = MNode::group(3, "cap");
        cap.subtitles = Some(Subtitles {
            transcript: Transcript { language: None, words: vec![word("keep", 0.0, 1.0), word("cut", 2.0, 3.0), word("moved", 4.0, 5.0)] },
            rules: Default::default(),
            uppercase: false,
        });
        let mut comp = crate::node::Comp::new(10.0, 10.0, MNode::group(0, "root").with_child(a).with_child(b).with_child(cap));
        comp.fps = 10.0;
        comp.duration_frames = 100;
        let mut p = Project::single(comp);
        let before = p.comps[&p.root].root.clone();
        let mut next = 10;
        let root = p.root;
        assert!(cut_range(&mut p, root, 20, 30, &mut next));
        assert_eq!(p.comps[&p.root].duration_frames, 90);
        assert_eq!(p.comps[&p.root].cuts[0].text, "cut");
        assert!(restore_cut(&mut p, root, 0));
        let after = &p.comps[&p.root].root;
        assert_eq!(p.comps[&p.root].duration_frames, 100);
        assert!(p.comps[&p.root].cuts.is_empty());
        let shape = |r: &MNode| r.children.iter().map(|n| (n.id, n.timing)).collect::<Vec<_>>();
        assert_eq!(shape(after), shape(&before), "same layers, same timing");
        let words = |r: &MNode| r.children[2].subtitles.as_ref().unwrap().transcript.words.clone();
        assert_eq!(words(after), words(&before));
    }
}
