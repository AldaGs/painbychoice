//! The editor's side of the clip tools: one [`Op`] per gesture, run after the
//! UI pass. The edits themselves are [`motion_core::edit`] (ADR 0025, 0028).

pub(crate) use motion_core::edit::*;
use crate::NodeId;

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
    /// Remove comp frames `a..b` from every top-level layer and close the gap
    /// (text-based cutting, ADR 0027).
    RemoveRange(i64, i64),
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
        Op::RemoveRange(a, b) => {
            let fps = c.fps;
            let gone = remove_range(root, frames, fps, a, b, next_id);
            if b > a {
                c.duration_frames = (frames - (b - a)).max(1);
            }
            for id in &gone {
                crate::app::delete_layer(project, comp, *id);
            }
            b > a
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{LayerTiming, MNode, NodeId};

    fn lt(in_: i64, len: i64) -> LayerTiming {
        LayerTiming::new(in_, in_ + len)
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
