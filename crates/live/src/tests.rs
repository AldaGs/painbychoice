//! Unit tests for the editor: axis math, clip drags, dock ops, and the
//! property/keyframe plumbing.
//!
//! Moved verbatim out of `main.rs` when it was split by concern.

use crate::*;

/// Apply a graph op to a bare `Document`, for the tests that predate projects.
///
/// `apply_graph_op` takes the whole project now (modules are project-wide), so
/// this wraps the doc, applies, and unwraps it again. Tests that exercise the
/// module ops themselves use a real project instead.
fn apply_op(doc: &mut Document, id: NodeId, op: GraphOp, frame: i64) {
    let mut project = MProject::single(doc.clone());
    let comp = project.root;
    apply_graph_op(&mut project, comp, Some(id), op, frame);
    *doc = project.comps.remove(&comp).expect("the comp survives");
}


fn test_axis(view: TimelineView) -> Axis {
    // 8px pad each side → a 400px usable span.
    Axis::new(
        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(416.0, 20.0)),
        view,
    )
}

#[test]
fn axis_round_trips_frames_through_pixels() {
    let a = test_axis(TimelineView { start: 0.0, visible: 100.0 });
    for f in [0i64, 1, 37, 99, 100] {
        assert_eq!(a.x_to_frame(a.frame_to_x(f as f64)), f, "frame {f}");
    }
}

#[test]
fn axis_round_trips_when_panned_and_zoomed() {
    let a = test_axis(TimelineView { start: 240.0, visible: 12.0 });
    for f in [240i64, 243, 251, 252] {
        assert_eq!(a.x_to_frame(a.frame_to_x(f as f64)), f, "frame {f}");
    }
}

#[test]
fn x_to_frame_snaps_to_the_nearest_frame() {
    let a = test_axis(TimelineView { start: 0.0, visible: 10.0 });
    // 40px per frame here, so a third of the way past frame 2 still snaps
    // back to 2, and two-thirds snaps up to 3.
    let x2 = a.frame_to_x(2.0);
    assert_eq!(a.x_to_frame(x2 + 13.0), 2);
    assert_eq!(a.x_to_frame(x2 + 27.0), 3);
}

/// The regression that shipped in the first cut: the default range ends one
/// frame *past* the visible window, so hit-testing the raw right edge put
/// the handle off-screen and every press near it read as a slide. Handles
/// are on the painted edges, so the clamped end stays grabbable.
#[test]
fn the_right_handle_is_grabbable_when_the_clip_runs_past_the_view() {
    // Bar painted from 100 to 500, clamped at the track's right edge.
    assert_eq!(clip_grab_at(499.0, 100.0, 500.0, 6.0, false), ClipGrab::TrimOut);
    assert_eq!(clip_grab_at(101.0, 100.0, 500.0, 6.0, false), ClipGrab::TrimIn);
    assert_eq!(clip_grab_at(300.0, 100.0, 500.0, 6.0, false), ClipGrab::Slide);
}

/// Handles overlap on a clip only a few pixels wide. The nearer edge has to
/// win, or the right one can never be grabbed.
#[test]
fn a_narrow_clip_splits_its_overlapping_handles_by_nearest_edge() {
    assert_eq!(clip_grab_at(100.5, 100.0, 104.0, 6.0, false), ClipGrab::TrimIn);
    assert_eq!(clip_grab_at(103.5, 100.0, 104.0, 6.0, false), ClipGrab::TrimOut);
}

/// Alt wins over the edge handles: slipping from a point that would
/// otherwise trim is still a slip, since trim-while-slipping isn't a
/// coherent gesture.
#[test]
fn alt_slips_from_anywhere_on_the_bar() {
    assert_eq!(clip_grab_at(300.0, 100.0, 500.0, 6.0, true), ClipGrab::Slip);
    assert_eq!(clip_grab_at(100.0, 100.0, 500.0, 6.0, true), ClipGrab::Slip);
    assert_eq!(clip_grab_at(500.0, 100.0, 500.0, 6.0, true), ClipGrab::Slip);
}

/// Slip moves the content under a fixed window: `start` shifts, `in_`/`out`
/// don't, so the clip plays a different part of its own animation in the
/// same slot. Unclamped in both directions — there's no source footage to
/// run past, and a negative local frame just holds the first key.
#[test]
fn slipping_moves_only_the_start() {
    let t = LayerTiming { start: 10, in_: 10, out: 30 };
    let later = drag_clip(t, ClipGrab::Slip, 6);
    assert_eq!((later.start, later.in_, later.out), (16, 10, 30));
    let earlier = drag_clip(t, ClipGrab::Slip, -40);
    assert_eq!((earlier.start, earlier.in_, earlier.out), (-30, 10, 30));
    // The window is untouched, so the clip occupies exactly the same frames.
    assert_eq!(later.len(), t.len());
}

/// Trimming an edge moves only that edge: the content keeps its place in
/// time, which is the whole difference between a trim and a slide.
#[test]
fn trimming_moves_one_edge_and_leaves_the_content_put() {
    let t = LayerTiming { start: 10, in_: 10, out: 30 };
    let a = drag_clip(t, ClipGrab::TrimIn, 5);
    assert_eq!((a.start, a.in_, a.out), (10, 15, 30));
    let b = drag_clip(t, ClipGrab::TrimOut, -5);
    assert_eq!((b.start, b.in_, b.out), (10, 10, 25));
}

#[test]
fn sliding_moves_the_whole_clip_so_it_plays_the_same_content_later() {
    let t = LayerTiming { start: 10, in_: 12, out: 30 };
    let slid = drag_clip(t, ClipGrab::Slide, 7);
    assert_eq!((slid.start, slid.in_, slid.out), (17, 19, 37));
    // The offset between in and start (the slip) survives the move, so the
    // clip shows the same local frames it did before.
    assert_eq!(slid.local_frame(slid.in_ as f64), t.local_frame(t.in_ as f64));
}

/// The clamps: a clip can't invert, can't shrink past one frame, and can't
/// be dragged off the front of the comp.
#[test]
fn clip_drags_clamp_instead_of_inverting() {
    let t = LayerTiming { start: 0, in_: 4, out: 8 };
    assert_eq!(drag_clip(t, ClipGrab::TrimIn, 999).in_, 7, "stops one short of out");
    assert_eq!(drag_clip(t, ClipGrab::TrimOut, -999).out, 5, "stops one past in");
    assert_eq!(drag_clip(t, ClipGrab::TrimIn, -999).in_, 0, "in never goes negative");
    let pushed = drag_clip(t, ClipGrab::Slide, -999);
    assert_eq!((pushed.start, pushed.in_, pushed.out), (-4, 0, 4), "slide stops at frame 0");
}

/// Pins the decided behaviour (2026-07-20): a layer **may outlive the comp**.
/// `drag_clip` has no upper clamp against comp duration — it's comp-agnostic by
/// design — so trimming or sliding can push `out` well past any comp end. This
/// is deliberate (eval is half-open and the comp only renders `[0, duration)`),
/// not an oversight; a regression that clamped here would fail this.
#[test]
fn a_clip_may_extend_past_the_comp_end() {
    let t = LayerTiming { start: 0, in_: 4, out: 8 };
    // Trim the out-point way past a nominal 30-frame comp.
    assert_eq!(drag_clip(t, ClipGrab::TrimOut, 999).out, 1007, "out is unclamped upward");
    // Sliding right carries the whole window past the end too.
    let slid = drag_clip(t, ClipGrab::Slide, 999);
    assert_eq!((slid.in_, slid.out), (1003, 1007), "the window rides past the comp");
}

/// The work area's loop bounds: clamped into the comp, never inverted, and the
/// whole comp when there's none.
#[test]
fn work_area_loop_bounds_stay_inside_the_comp() {
    // No work area → the whole comp.
    assert_eq!(loop_bounds(None, 30), (0, 30));
    // A sane range passes through.
    assert_eq!(loop_bounds(Some(WorkArea { start: 5, end: 20 }), 30), (5, 20));
    // start past the comp is pulled back to the last frame; end can't cross it.
    assert_eq!(loop_bounds(Some(WorkArea { start: 40, end: 50 }), 30), (29, 30));
    // An inverted range still yields a non-empty span (hi >= lo + 1).
    let (lo, hi) = loop_bounds(Some(WorkArea { start: 10, end: 3 }), 30);
    assert!(hi > lo, "the loop span is never empty: {lo}..{hi}");
    // A zero-duration comp still gives a usable span.
    assert_eq!(loop_bounds(None, 0), (0, 1));
}

/// `wrap_into` folds the wall clock into the loop span, cycling within it.
#[test]
fn wrap_into_cycles_within_the_span() {
    // Inside the span: unchanged.
    assert!((wrap_into(7.0, 5.0, 10.0) - 7.0).abs() < 1e-9);
    // One past the end wraps back to the start.
    assert!((wrap_into(10.0, 5.0, 10.0) - 5.0).abs() < 1e-9);
    // Well past wraps around by the span (width 5): 13 → 8.
    assert!((wrap_into(13.0, 5.0, 10.0) - 8.0).abs() < 1e-9);
    // Before the start wraps up from the top.
    assert!((wrap_into(4.0, 5.0, 10.0) - 9.0).abs() < 1e-9);
    // A collapsed span holds at the start rather than dividing by zero.
    assert!((wrap_into(42.0, 5.0, 5.0) - 5.0).abs() < 1e-9);
}

/// Setting one work-area edge seeds the other from the comp on the first press,
/// so a single B or N keystroke makes a valid range.
#[test]
fn setting_a_work_edge_seeds_the_other_from_the_comp() {
    // First B at frame 8, in a 30-frame comp: end seeds to the comp extent.
    let a = with_work_start(None, 8, 30);
    assert_eq!(a, WorkArea { start: 8, end: 30 });
    // Then N at frame 20: end is exclusive, so 20 stays the last frame.
    let b = with_work_end(Some(a), 20, 30);
    assert_eq!(b, WorkArea { start: 8, end: 21 });
    // First N (no prior area) seeds the start from 0.
    assert_eq!(with_work_end(None, 12, 30), WorkArea { start: 0, end: 13 });
    // Edges are clamped into the comp.
    assert_eq!(with_work_start(None, 99, 30), WorkArea { start: 29, end: 30 });
    assert_eq!(with_work_end(None, 99, 30), WorkArea { start: 0, end: 30 });
}

#[test]
fn zoom_keeps_the_anchored_frame_under_the_cursor() {
    // This is the property that makes zooming feel like zooming: whatever
    // frame is under the pointer must not move while the scale changes.
    let view = TimelineView { start: 0.0, visible: 120.0 };
    let a = test_axis(view);
    let cursor_x = a.frame_to_x(90.0);
    let anchor = a.x_to_frame_exact(cursor_x);

    for factor in [0.5f64, 0.8, 1.25, 2.0] {
        let visible = view.visible * factor;
        let ratio = (anchor - view.start) / view.visible;
        let next = TimelineView { start: anchor - ratio * visible, visible };
        let moved = test_axis(next).frame_to_x(anchor);
        assert!(
            (moved - cursor_x).abs() < 0.5,
            "factor {factor}: anchor drifted {cursor_x} -> {moved}"
        );
    }
}

#[test]
fn view_clamp_keeps_the_window_inside_the_comp() {
    let v = TimelineView { start: -50.0, visible: 5000.0 }.clamped(120);
    assert_eq!(v.start, 0.0);
    assert_eq!(v.visible, 120.0, "cannot show more than the comp");

    // Panned past the end: slides back so the window ends at the last frame.
    let v = TimelineView { start: 900.0, visible: 20.0 }.clamped(120);
    assert!((v.start + v.visible - 120.0).abs() < 1e-9, "start = {}", v.start);

    // Zoomed in absurdly far: floored, not zero or negative.
    let v = TimelineView { start: 10.0, visible: 0.0001 }.clamped(120);
    assert!(v.visible >= 4.0, "visible = {}", v.visible);
}

#[test]
fn tick_step_grows_as_you_zoom_out() {
    // Zoomed in: every frame is far apart, so a 1-frame step fits.
    assert_eq!(tick_step(80.0, 24.0, 58.0), 1);
    // Zoomed out: steps must land on whole seconds at 24fps.
    let wide = tick_step(0.5, 24.0, 58.0);
    assert!(wide % 24 == 0, "expected a whole-second step, got {wide}");
    // And it must actually satisfy the spacing it was asked for.
    assert!(0.5 * wide as f32 >= 58.0);
}

#[test]
fn selection_groups_into_one_bucket_per_property() {
    let mut sel = KeySelection::new();
    // Inserted interleaved and out of order on purpose.
    sel.insert((PropKind::Rotation, 5));
    sel.insert((PropKind::Position, 3));
    sel.insert((PropKind::Rotation, 1));
    sel.insert((PropKind::Position, 0));
    sel.insert((PropKind::Opacity, 2));

    let grouped = group_selection_by_prop(&sel);
    assert_eq!(grouped.len(), 3, "one bucket per property: {grouped:?}");

    // Every property appears exactly once...
    let mut kinds: Vec<PropKind> = grouped.iter().map(|(k, _)| *k).collect();
    let before = kinds.len();
    kinds.dedup();
    assert_eq!(kinds.len(), before, "a property was split across buckets");

    // ...and each bucket's indices are sorted ascending, which is what
    // Track::move_keys and the descending-delete both assume.
    for (kind, idxs) in &grouped {
        assert!(idxs.windows(2).all(|w| w[0] < w[1]), "{kind:?} unsorted: {idxs:?}");
    }
}

#[test]
fn empty_selection_groups_to_nothing() {
    assert!(group_selection_by_prop(&KeySelection::new()).is_empty());
}

#[test]
fn edge_pan_is_dead_in_the_middle_and_signed_at_the_ends() {
    let (l, r, e) = (100.0f32, 500.0f32, 40.0f32);
    assert_eq!(edge_pan_intensity(300.0, l, r, e), 0.0, "middle is dead");
    assert_eq!(edge_pan_intensity(145.0, l, r, e), 0.0, "just inside the zone");
    assert!(edge_pan_intensity(120.0, l, r, e) < 0.0, "left zone pans left");
    assert!(edge_pan_intensity(480.0, l, r, e) > 0.0, "right zone pans right");
}

#[test]
fn edge_pan_ramps_with_depth_and_saturates() {
    let (l, r, e) = (100.0f32, 500.0f32, 40.0f32);
    // Deeper into the zone → stronger.
    let shallow = edge_pan_intensity(130.0, l, r, e).abs();
    let deep = edge_pan_intensity(105.0, l, r, e).abs();
    assert!(deep > shallow, "{deep} should exceed {shallow}");
    // At and beyond the edge it saturates rather than running away.
    assert!((edge_pan_intensity(l, l, r, e) + 1.0).abs() < 1e-6);
    assert!((edge_pan_intensity(-9999.0, l, r, e) + 1.0).abs() < 1e-6);
    assert!((edge_pan_intensity(9999.0, l, r, e) - 1.0).abs() < 1e-6);
}

#[test]
fn edge_pan_handles_degenerate_tracks() {
    // A collapsed or inverted track must not produce a pan (or a NaN).
    assert_eq!(edge_pan_intensity(50.0, 100.0, 100.0, 36.0), 0.0);
    assert_eq!(edge_pan_intensity(50.0, 500.0, 100.0, 36.0), 0.0);
    assert_eq!(edge_pan_intensity(50.0, 100.0, 500.0, 0.0), 0.0);
}

#[test]
fn tick_step_never_returns_zero() {
    // A degenerate/huge zoom-out must still yield a usable positive step,
    // since it's used as a modulus when drawing the ruler.
    for pxf in [1e-6f32, 0.0, 1000.0] {
        assert!(tick_step(pxf, 24.0, 58.0) > 0, "px/frame {pxf}");
    }
}

/// A click on the demo square (centered at t=0) should select it, and a
/// click far outside should deselect. Fit is identity here so physical
/// pixels equal composition coordinates.
#[test]
fn pick_hits_shape_and_misses_empty_space() {
    let doc = demo_document();
    let scene = motion_core::evaluate(&doc, 0.0);
    let fit = Affine::IDENTITY;

    // The square sits at (300, 540) at t=0 with a 200x200 body.
    assert_eq!(pick(&scene, fit, (300.0, 540.0)), Some(NodeId(1)));
    // Empty corner — nothing there.
    assert_eq!(pick(&scene, fit, (5.0, 5.0)), None);
}

#[test]
fn pick_prefers_front_most_item() {
    // The dot is a child drawn after the square, so where they overlap the
    // dot (front-most) wins. At t=0 the dot is above the square center.
    let doc = demo_document();
    let scene = motion_core::evaluate(&doc, 0.0);
    let fit = Affine::IDENTITY;
    // Dot center: square pos (300,540) + child offset (0,-120) = (300,420).
    assert_eq!(pick(&scene, fit, (300.0, 420.0)), Some(NodeId(2)));
}

#[test]
fn fit_centers_and_letterboxes_inside_the_canvas_rect() {
    let doc = Document::new(100.0, 100.0, MNode::group(0, "root"));
    // A wide area: the square doc is limited by height and centered on x.
    let fit = fit_transform(&doc, kurbo::Rect::new(0.0, 0.0, 300.0, 100.0));
    assert_eq!(fit * Point::new(0.0, 0.0), Point::new(100.0, 0.0));
    assert_eq!(fit * Point::new(100.0, 100.0), Point::new(200.0, 100.0));
}

#[test]
fn fit_respects_a_canvas_rect_that_does_not_start_at_the_origin() {
    // The regression the layout tree exists to prevent: with panels around
    // it the canvas is an *offset* rect, and picking inverts this transform
    // — an ignored origin puts every click in the wrong place.
    let doc = Document::new(100.0, 100.0, MNode::group(0, "root"));
    let area = kurbo::Rect::new(190.0, 34.0, 290.0, 134.0);
    let fit = fit_transform(&doc, area);
    assert_eq!(fit * Point::new(0.0, 0.0), Point::new(190.0, 34.0));
    assert_eq!(fit * Point::new(50.0, 50.0), Point::new(240.0, 84.0));
    // And a click there inverts back to the doc's center.
    let back = fit.inverse() * Point::new(240.0, 84.0);
    assert!((back.x - 50.0).abs() < 1e-9 && (back.y - 50.0).abs() < 1e-9);
}

#[test]
fn the_default_layout_shows_every_editor_exactly_once() {
    // A leaf reachable in the tree is a panel that renders; one that isn't
    // is a panel that silently vanished. Cheap invariant, easy to break
    // while rearranging the default.
    fn walk(d: &Dock, out: &mut Vec<Editor>) {
        match d {
            Dock::Leaf(e) => out.push(*e),
            Dock::Split { first, second, .. } => {
                walk(first, out);
                walk(second, out);
            }
        }
    }
    let mut found = Vec::new();
    walk(&Dock::default_layout(), &mut found);
    for e in [
        Editor::Comp,
        Editor::Layers,
        Editor::Transport,
        Editor::Dopesheet,
        Editor::Properties,
        Editor::Canvas,
    ] {
        assert_eq!(found.iter().filter(|f| **f == e).count(), 1, "{e:?}");
    }
    assert_eq!(found.len(), 6, "no extra leaves");
}

#[test]
fn the_canvas_is_the_innermost_leaf() {
    // It has to be: every other panel takes a fixed edge, and the canvas is
    // whatever is left. If some editor ends up nested *inside* the canvas
    // leaf's remainder, the canvas rect measures the wrong hole.
    let mut d = Dock::default_layout();
    loop {
        match d {
            Dock::Split { second, .. } => d = *second,
            Dock::Leaf(e) => {
                assert_eq!(e, Editor::Canvas, "innermost leaf must be the canvas");
                break;
            }
        }
    }
}

/// Flatten a layout tree to the editors it shows, in walk order.
fn dock_editors(d: &Dock) -> Vec<Editor> {
    fn go(d: &Dock, out: &mut Vec<Editor>) {
        match d {
            Dock::Leaf(e) => out.push(*e),
            Dock::Split { first, second, .. } => {
                go(first, out);
                go(second, out);
            }
        }
    }
    let mut out = Vec::new();
    go(d, &mut out);
    out
}

/// The address of the first leaf showing `target`, or `None`.
fn path_to(d: &Dock, target: Editor) -> Option<Vec<Branch>> {
    match d {
        Dock::Leaf(e) => (*e == target).then(Vec::new),
        Dock::Split { first, second, .. } => path_to(first, target)
            .map(|mut p| {
                p.insert(0, Branch::First);
                p
            })
            .or_else(|| {
                path_to(second, target).map(|mut p| {
                    p.insert(0, Branch::Second);
                    p
                })
            }),
    }
}

fn count_editor(d: &Dock, e: Editor) -> usize {
    dock_editors(d).into_iter().filter(|x| *x == e).count()
}

/// Following `.second` from the root always lands on the canvas — the
/// invariant the canvas-rect measurement depends on.
fn innermost_is_canvas(d: &Dock) -> bool {
    let mut cur = d;
    loop {
        match cur {
            Dock::Split { second, .. } => cur = second,
            Dock::Leaf(e) => return *e == Editor::Canvas,
        }
    }
}

#[test]
fn retype_swaps_the_editor_in_place() {
    let mut d = Dock::default_layout();
    let path = path_to(&d, Editor::Layers).unwrap();
    d.apply(DockCmd::Retype { path, editor: Editor::Properties });
    assert_eq!(count_editor(&d, Editor::Layers), 0, "old editor gone");
    assert_eq!(count_editor(&d, Editor::Properties), 2, "now shown twice");
    // Structure is otherwise untouched.
    assert_eq!(dock_editors(&d).len(), 6);
    assert!(innermost_is_canvas(&d));
}

#[test]
fn split_then_close_the_new_half_round_trips() {
    let mut d = Dock::default_layout();
    let before = dock_editors(&d);
    let path = path_to(&d, Editor::Properties).unwrap();
    d.apply(DockCmd::Split {
        path: path.clone(),
        side: DockSide::Left,
        size: 120.0,
    });
    // The leaf became a split of two Properties.
    assert_eq!(count_editor(&d, Editor::Properties), 2);
    assert!(innermost_is_canvas(&d), "a split must not dislodge the canvas");
    // Close the newly-made second half; its sibling (the original) absorbs it.
    let mut new_half = path.clone();
    new_half.push(Branch::Second);
    d.apply(DockCmd::Close { path: new_half });
    assert_eq!(dock_editors(&d), before, "join undoes the split exactly");
}

#[test]
fn closing_an_area_keeps_the_canvas() {
    // Properties sits beside the canvas; closing it must leave the canvas as
    // the surviving sibling, never remove it.
    let mut d = Dock::default_layout();
    let path = path_to(&d, Editor::Properties).unwrap();
    d.apply(DockCmd::Close { path });
    assert_eq!(count_editor(&d, Editor::Properties), 0, "area closed");
    assert_eq!(count_editor(&d, Editor::Canvas), 1, "canvas survives");
    assert!(innermost_is_canvas(&d));
}

#[test]
fn every_builtin_preset_is_a_valid_layout() {
    // A preset a user can switch to must keep the structural guarantees the
    // rest of the app leans on: exactly one canvas as the innermost leaf, and
    // the two headerless toolbars present (there's no way to bring back a
    // Comp or Transport that a preset dropped — they carry no picker).
    for preset in builtin_presets() {
        let d = &preset.dock;
        assert_eq!(count_editor(d, Editor::Canvas), 1, "{}: one canvas", preset.name);
        assert!(innermost_is_canvas(d), "{}: canvas innermost", preset.name);
        assert_eq!(count_editor(d, Editor::Comp), 1, "{}: comp bar", preset.name);
        assert_eq!(count_editor(d, Editor::Transport), 1, "{}: transport", preset.name);
    }
}

#[test]
fn presets_offer_more_than_one_arrangement() {
    // The whole point of presets: the Design layout drops the dopesheet the
    // default keeps, so switching visibly changes the panels.
    let presets = builtin_presets();
    assert!(presets.len() >= 3);
    assert!(presets.iter().all(|p| p.builtin));
    let default = &presets.iter().find(|p| p.name == "Default").unwrap().dock;
    let design = &presets.iter().find(|p| p.name == "Design").unwrap().dock;
    assert_eq!(count_editor(default, Editor::Dopesheet), 1);
    assert_eq!(count_editor(design, Editor::Dopesheet), 0);
}

#[test]
fn is_valid_accepts_layouts_and_rejects_broken_ones() {
    for p in builtin_presets() {
        assert!(p.dock.is_valid(), "{} should be valid", p.name);
    }
    // No toolbars, no comp: a lone canvas leaves the user no way back.
    assert!(!Dock::Leaf(Editor::Canvas).is_valid());
    // Two canvases — the vello target measurement expects exactly one.
    let two = Dock::split(DockSide::Top, 10.0, false, Editor::Canvas, Dock::default_layout());
    assert!(!two.is_valid());
    // Canvas present but not the innermost leaf (it's a split's `first`).
    let off = Dock::split(
        DockSide::Top,
        COMP_H,
        false,
        Editor::Comp,
        Dock::split(
            DockSide::Bottom,
            TRANSPORT_H,
            false,
            Editor::Transport,
            Dock::split(DockSide::Left, 10.0, true, Editor::Canvas, Dock::Leaf(Editor::Properties)),
        ),
    );
    assert!(!off.is_valid());
}

#[test]
fn project_round_trips_document_and_layout() {
    // A non-default arrangement plus a user preset must survive save/load.
    let mut dock = Dock::default_layout();
    let path = path_to(&dock, Editor::Properties).unwrap();
    dock.apply(DockCmd::Split { path, side: DockSide::Top, size: 60.0 });
    let project = SaveFile {
        project: Some(MProject::single(demo_document())),
        document: None,
        layout: LayoutState {
            dock: Some(dock.clone()),
            user_presets: vec![Preset {
                name: "Mine".into(),
                dock: Dock::design_layout(),
                builtin: true, // deliberately set — serialization must drop it
            }],
        },
    };
    let json = serde_json::to_string(&project).unwrap();
    let back: SaveFile = serde_json::from_str(&json).unwrap();
    assert_eq!(back.project.expect("the project survives").comps.len(), 1);
    assert_eq!(dock_editors(&back.layout.dock.unwrap()), dock_editors(&dock));
    assert_eq!(back.layout.user_presets.len(), 1);
    assert_eq!(back.layout.user_presets[0].name, "Mine");
    assert!(!back.layout.user_presets[0].builtin, "builtin is skipped → false on load");
}

#[test]
fn a_bare_document_file_still_loads() {
    // Oldest format: a bare Document, no wrapper at all. Every `SaveFile` field
    // defaults, so such a file *parses* as one — carrying neither a project nor
    // a document. That empty-parse is the loader's signal to retry as a bare
    // document, which is why the fallback can't be keyed on a parse failure.
    let json = serde_json::to_string(&demo_document()).unwrap();
    let empty: SaveFile = serde_json::from_str(&json).expect("defaults let it parse");
    assert!(empty.project.is_none() && empty.document.is_none(), "nothing usable in it");
    assert!(serde_json::from_str::<Document>(&json).is_ok(), "parses as a bare doc");
}

/// Renaming a comp is a trimmed assign to its `name`; the switcher reads
/// `Comp::label`, which falls back to a positional "Comp N" when the name is
/// blank. So renaming to whitespace can't produce an empty, unclickable entry —
/// the one non-obvious edge a click-test of the rename field would catch.
#[test]
fn a_blank_comp_name_falls_back_to_a_positional_label() {
    let mut comp = Document::new(100.0, 100.0, MNode::group(0, "root"));
    comp.name = "Intro".into();
    assert_eq!(comp.label(CompId(0)), "Intro", "a real name shows as-is");

    // The rename path: trim the field, assign. Blank in, fallback out.
    comp.name = "   ".trim().to_string();
    assert_eq!(comp.label(CompId(2)), "Comp 3", "blank → positional (1-based)");
}

/// A genuinely multi-comp project — two comps, a precomp *instance* linking one
/// into the other, and a shared module driving a property — must survive the
/// full save→load path (serialize as the app's `SaveFile`, reparse, `migrate`)
/// unchanged. This is the data risk behind "multi-comp save/load"; the native
/// file dialog is just chrome around exactly this round-trip.
#[test]
fn a_multi_comp_project_round_trips_through_save_and_load() {
    // The inner comp: a lone ellipse.
    let mut project = MProject::single(Document::new(
        200.0,
        100.0,
        MNode::group(0, "root"),
    ));
    let root = project.root;
    let inner = project.insert(Document::new(
        50.0,
        50.0,
        MNode::group(0, "inner_root").with_child(MNode::shape(
            1,
            "dot",
            MShape::Ellipse { size: Value::constant(Vec2::new(20.0, 20.0)) },
        )),
    ));
    // A module driving opacity to 0.5, linked from the instance layer.
    let module = project.add_module(MModule::new("fade", Expr::Lit(ExprValue::Num(0.5))));
    let mut instance = MNode::group(2, "instance").with_transform(Transform {
        opacity: Value::expr(Expr::Use { module, overrides: Vec::new() }),
        ..Transform::default()
    });
    instance.precomp = Some(inner);
    project.comp_mut(root).unwrap().root.children.push(instance);

    let before = motion_core::evaluate_comp(&project, root, 0.0);
    assert!(before.warnings.is_empty(), "sanity: {:?}", before.warnings);

    // Save exactly as `App::save` does, then load exactly as `App::load` does.
    let file = SaveFile {
        project: Some(project.clone()),
        document: None,
        layout: LayoutState { dock: Some(Dock::default_layout()), user_presets: vec![] },
    };
    let json = serde_json::to_string(&file).unwrap();
    let back: SaveFile = serde_json::from_str(&json).unwrap();
    let mut loaded = back.project.expect("the project survives");
    loaded.migrate();

    // The registry, the instance's target, and the module all come back.
    assert_eq!(loaded.comps.len(), 2, "both comps");
    assert!(loaded.comp(inner).is_some(), "the inner comp's id is preserved");
    assert_eq!(loaded.modules.len(), 1, "the module survives");
    let inst = loaded
        .comp(root)
        .unwrap()
        .root
        .children
        .iter()
        .find(|n| n.name == "instance")
        .expect("the instance layer");
    assert_eq!(inst.precomp, Some(inner), "it still instances the inner comp");

    // And the frame is identical — the link still resolves post-load.
    let after = motion_core::evaluate_comp(&loaded, root, 0.0);
    assert!(after.warnings.is_empty(), "{:?}", after.warnings);
    assert_eq!(after.items.len(), before.items.len(), "same number of drawn items");
    for (a, b) in after.items.iter().zip(&before.items) {
        assert!((a.opacity - b.opacity).abs() < 1e-9, "opacity via the module survives");
    }
}

#[test]
fn a_pre_comps_save_file_loads_as_a_one_comp_project() {
    // The middle format: a wrapper holding a single `document`. It must come
    // back as a one-comp project, with its layout intact.
    let legacy = serde_json::json!({
        "document": serde_json::to_value(demo_document()).unwrap(),
        "layout": { "dock": serde_json::Value::Null, "user_presets": [] },
    });
    let file: SaveFile = serde_json::from_value(legacy).unwrap();
    assert!(file.project.is_none(), "no project field in the old format");
    let doc = file.document.expect("the single document is there");
    let project = MProject::single(doc);
    assert_eq!(project.comps.len(), 1);
    assert_eq!(project.root_comp().root.children.len(), demo_document().root.children.len());
}

#[test]
fn only_content_areas_carry_a_header() {
    // The three structural leaves must never offer split/close/retype, or the
    // canvas-rect and innermost-canvas invariants could be broken from the UI.
    assert!(Editor::Layers.is_swappable());
    assert!(Editor::Properties.is_swappable());
    assert!(Editor::Dopesheet.is_swappable());
    assert!(Editor::Graph.is_swappable());
    assert!(!Editor::Canvas.is_swappable());
    assert!(!Editor::Comp.is_swappable());
    assert!(!Editor::Transport.is_swappable());
}

/// A one-node doc (id 1) with the given opacity, under a root.
fn graph_doc(opacity: Value<f64>) -> (Document, NodeId) {
    let n = MNode::group(1, "n")
        .with_transform(Transform { opacity, ..Transform::default() });
    let doc = Document::new(100.0, 100.0, MNode::group(0, "root").with_child(n));
    (doc, NodeId(1))
}

fn resolved_opacity(doc: &Document, id: NodeId) -> f64 {
    let node = doc.root.find(id).unwrap();
    let mut ctx = EvalCtx::new(doc, 0.0);
    ctx.in_node(node.id, |ctx| node.transform.opacity.resolve(ctx))
}

#[test]
fn gather_lists_the_selected_nodes_properties() {
    let (doc, id) = graph_doc(Value::constant(0.5));
    let info = GraphInfo::gather(&doc, &Default::default(), Some(id), None, 0.0);
    let node = info.node.as_ref().unwrap();
    let opacity = node.props.iter().find(|p| p.kind == PropKind::Opacity).unwrap();
    assert!(!opacity.is_expr, "starts as a plain value");
    assert!(opacity.expr.is_none());
    // The reference-target list includes every node.
    assert!(info.nodes.iter().any(|(nid, _)| *nid == 1));
    assert!(info.nodes.iter().any(|(nid, _)| *nid == 0), "root too");
}

#[test]
fn the_script_preview_resolves_against_the_document() {
    // The panel's result line must use a doc-backed context: a doc-less
    // preview would report "no node named 'root'" for a script that
    // resolves fine at render time.
    let (mut doc, id) = graph_doc(Value::expr(Expr::Script(
        "value(\"root\", \"opacity\") + wiggle(0.0, 0.0)".into(),
    )));
    // Give the root a distinctive opacity to read back.
    doc.root.transform.opacity = Value::constant(0.25);
    let info = GraphInfo::gather(&doc, &Default::default(), Some(id), None, 0.0);
    let node = info.node.as_ref().unwrap();
    let result = node.script_results.get(&(PropKind::Opacity, vec![])).unwrap();
    assert_eq!(result.as_ref().unwrap(), "0.25");
}

#[test]
fn the_script_preview_is_addressed_by_tree_path() {
    // A script nested under an operator is keyed by its path, so two
    // scripts in one tree can't show each other's result.
    let (mut doc, id) = graph_doc(Value::constant(0.0));
    apply_op(&mut doc, id, GraphOp::Promote(PropKind::Opacity), 0);
    apply_op(
        &mut doc,
        id,
        GraphOp::SetKind { target: GraphTarget::Prop(PropKind::Opacity), path: vec![], new: ExprKind::Add },
        0,
    );
    for (slot, src) in [(0usize, "1.0"), (1, "2.0")] {
        apply_op(
            &mut doc,
            id,
            GraphOp::SetKind {
                target: GraphTarget::Prop(PropKind::Opacity),
                path: vec![slot],
                new: ExprKind::Script,
            },
            0,
        );
        apply_op(
            &mut doc,
            id,
            GraphOp::SetScript {
                target: GraphTarget::Prop(PropKind::Opacity),
                path: vec![slot],
                src: src.into(),
            },
            0,
        );
    }
    let info = GraphInfo::gather(&doc, &Default::default(), Some(id), None, 0.0);
    let node = info.node.as_ref().unwrap();
    let at = |path: Vec<usize>| {
        node.script_results.get(&(PropKind::Opacity, path)).unwrap().clone().unwrap()
    };
    assert_eq!(at(vec![0]), "1");
    assert_eq!(at(vec![1]), "2");
    assert!(
        !node.script_results.contains_key(&(PropKind::Opacity, vec![])),
        "the root is an Add, not a script"
    );
}

#[test]
fn a_bad_script_shows_one_line_of_error_in_the_preview() {
    let (doc, id) =
        graph_doc(Value::expr(Expr::Script("value(\"nope\", \"opacity\")".into())));
    let info = GraphInfo::gather(&doc, &Default::default(), Some(id), None, 0.0);
    let node = info.node.as_ref().unwrap();
    let err = node.script_results[&(PropKind::Opacity, vec![])].clone().unwrap_err();
    assert!(err.contains("nope"), "{err}");
    assert!(!err.contains('\n'), "one line, so it fits under the field");
}

#[test]
fn add_param_then_remove_it_through_the_graph_ops() {
    let (mut doc, id) = graph_doc(Value::constant(0.5));
    apply_op(
        &mut doc,
        id,
        GraphOp::AddParam { owner: ParamOwner::Node(id), name: "gain".into(), kind: ParamKind::Num },
        0,
    );
    assert!(doc.root.find(id).unwrap().param("gain").is_some());
    // The panel lists it, so a `param` node can pick it.
    let info = GraphInfo::gather(&doc, &Default::default(), Some(id), None, 0.0);
    let node = info.node.as_ref().unwrap();
    assert_eq!(node.params, vec![("gain".to_string(), "number")]);

    apply_op(&mut doc, id, GraphOp::RemoveParam { owner: ParamOwner::Node(id), name: "gain".into() }, 0);
    assert!(doc.root.find(id).unwrap().param("gain").is_none());
}

#[test]
fn a_param_node_drives_a_property_end_to_end() {
    // The whole flow through the panel's ops: add a knob, point the
    // property's expression at it, and see the property take its value.
    let (mut doc, id) = graph_doc(Value::constant(0.0));
    apply_op(
        &mut doc,
        id,
        GraphOp::AddParam { owner: ParamOwner::Node(id), name: "gain".into(), kind: ParamKind::Num },
        0,
    );
    doc.root
        .find_mut(id)
        .unwrap()
        .set_param("gain", ParamValue::Num(Value::constant(0.8)));
    apply_op(&mut doc, id, GraphOp::Promote(PropKind::Opacity), 0);
    apply_op(
        &mut doc,
        id,
        GraphOp::SetKind { target: GraphTarget::Prop(PropKind::Opacity), path: vec![], new: ExprKind::Param },
        0,
    );
    apply_op(
        &mut doc,
        id,
        GraphOp::SetParam { target: GraphTarget::Prop(PropKind::Opacity), path: vec![], name: "gain".into() },
        0,
    );
    assert_eq!(resolved_opacity(&doc, id), 0.8);
}

#[test]
fn promote_edit_then_bake_round_trips_a_property() {
    let (mut doc, id) = graph_doc(Value::constant(0.5));
    // Promote seeds a literal of the current value — unchanged.
    apply_op(&mut doc, id, GraphOp::Promote(PropKind::Opacity), 0);
    assert!(doc.root.find(id).unwrap().transform.opacity.is_expr());
    assert_eq!(resolved_opacity(&doc, id), 0.5);
    // Edit the literal.
    apply_op(
        &mut doc,
        id,
        GraphOp::SetLit { target: GraphTarget::Prop(PropKind::Opacity), path: vec![], value: ExprValue::Num(0.9) },
        0,
    );
    assert_eq!(resolved_opacity(&doc, id), 0.9);
    // Bake back to a constant, freezing the value.
    apply_op(&mut doc, id, GraphOp::Bake(PropKind::Opacity), 0);
    assert!(!doc.root.find(id).unwrap().transform.opacity.is_expr());
    assert_eq!(resolved_opacity(&doc, id), 0.9);
}

#[test]
fn set_kind_grows_a_tree_that_evaluates() {
    // Promote, turn the root into Add, then set its two children: 0.2 + 0.3.
    let (mut doc, id) = graph_doc(Value::constant(0.0));
    apply_op(&mut doc, id, GraphOp::Promote(PropKind::Opacity), 0);
    apply_op(
        &mut doc,
        id,
        GraphOp::SetKind { target: GraphTarget::Prop(PropKind::Opacity), path: vec![], new: ExprKind::Add },
        0,
    );
    apply_op(
        &mut doc,
        id,
        GraphOp::SetLit { target: GraphTarget::Prop(PropKind::Opacity), path: vec![0], value: ExprValue::Num(0.2) },
        0,
    );
    apply_op(
        &mut doc,
        id,
        GraphOp::SetLit { target: GraphTarget::Prop(PropKind::Opacity), path: vec![1], value: ExprValue::Num(0.3) },
        0,
    );
    assert!((resolved_opacity(&doc, id) - 0.5).abs() < 1e-9);
}

fn box_at_path<'a>(boxes: &'a [ExprBox], path: &[usize]) -> &'a ExprBox {
    boxes.iter().find(|b| b.path == path).expect("box for path")
}

#[test]
fn layout_stacks_leaves_and_centres_parents() {
    // A single leaf: one box at the top-left column.
    let single = layout_expr(&Expr::num(1.0));
    assert_eq!(single.len(), 1);
    assert_eq!((single[0].depth, single[0].y), (0, 0.0));

    // Add(Lit, Lit): two leaves stacked (the second below the first by at
    // least the first's height), the operator centred between them.
    let add = layout_expr(&Expr::Add(Box::new(Expr::num(1.0)), Box::new(Expr::num(2.0))));
    assert_eq!(add.len(), 3);
    let (root, a, b) = (
        box_at_path(&add, &[]),
        box_at_path(&add, &[0]),
        box_at_path(&add, &[1]),
    );
    assert_eq!((root.depth, a.depth), (0, 1), "inputs one column right");
    assert_eq!(a.y, 0.0);
    assert!(b.y >= a.y + a.height, "second leaf clears the first");
    assert!(
        (root.center_y() - (a.center_y() + b.center_y()) / 2.0).abs() < 1e-4,
        "operator centred over its inputs"
    );
}

#[test]
fn layout_handles_a_nested_tree() {
    // Add(Lit, Mul(Lit, Lit)): three leaves stacked top-to-bottom; each
    // parent centred on its children's span.
    let e = Expr::Add(
        Box::new(Expr::num(1.0)),
        Box::new(Expr::Mul(Box::new(Expr::num(2.0)), Box::new(Expr::num(3.0)))),
    );
    let boxes = layout_expr(&e);
    assert_eq!(boxes.len(), 5);
    let cy = |p: &[usize]| box_at_path(&boxes, p).center_y();
    assert!(cy(&[0]) < cy(&[1, 0]) && cy(&[1, 0]) < cy(&[1, 1]), "leaves stacked in order");
    assert!((cy(&[1]) - (cy(&[1, 0]) + cy(&[1, 1])) / 2.0).abs() < 1e-4, "Mul centred");
    assert!((cy(&[]) - (cy(&[0]) + cy(&[1])) / 2.0).abs() < 1e-4, "root centred on its span");
    assert_eq!(box_at_path(&boxes, &[1, 1]).depth, 2, "deepest column");
}

#[test]
fn taller_nodes_reserve_more_vertical_room() {
    // A ref node is taller than a value node, and the leaf stacked below it
    // clears its full height (so their boxes don't overlap).
    let e = Expr::Add(
        Box::new(Expr::reference(NodeId(1), PropPath::Position)),
        Box::new(Expr::num(0.0)),
    );
    let boxes = layout_expr(&e);
    let refb = box_at_path(&boxes, &[0]);
    let litb = box_at_path(&boxes, &[1]);
    assert!(refb.height > litb.height, "a ref box is taller than a value box");
    assert!(litb.y >= refb.y + refb.height, "the box below clears the taller one");
}

#[test]
fn set_script_drives_a_property_from_the_frame() {
    let (mut doc, id) = graph_doc(Value::constant(0.0));
    apply_op(&mut doc, id, GraphOp::Promote(PropKind::Opacity), 0);
    apply_op(
        &mut doc,
        id,
        GraphOp::SetKind { target: GraphTarget::Prop(PropKind::Opacity), path: vec![], new: ExprKind::Script },
        0,
    );
    apply_op(
        &mut doc,
        id,
        GraphOp::SetScript { target: GraphTarget::Prop(PropKind::Opacity), path: vec![], src: "frame + 0.25".into() },
        0,
    );
    // resolved_opacity samples at frame 0, so the script yields 0.25.
    assert!((resolved_opacity(&doc, id) - 0.25).abs() < 1e-9);
}

#[test]
fn set_ref_links_one_property_to_another() {
    // Two nodes: a (id 1) opacity 0.4, b (id 2) empty; b.opacity references a.
    let a = MNode::group(1, "a")
        .with_transform(Transform { opacity: Value::constant(0.4), ..Transform::default() });
    let b = MNode::group(2, "b");
    let mut doc =
        Document::new(100.0, 100.0, MNode::group(0, "root").with_child(a).with_child(b));
    apply_op(&mut doc, NodeId(2), GraphOp::Promote(PropKind::Opacity), 0);
    apply_op(
        &mut doc,
        NodeId(2),
        GraphOp::SetRef {
            target: GraphTarget::Prop(PropKind::Opacity),
            path: vec![],
            node: NodeId(1),
            prop: PropPath::Opacity,
            offset: 0.0,
        },
        0,
    );
    assert_eq!(resolved_opacity(&doc, NodeId(2)), 0.4, "b now mirrors a");
}

#[test]
fn set_kind_to_a_generator_drives_the_property() {
    // Promote, then retype the root to a ramp, and edit its `to` knob (slot
    // 1) to 5 — at frame 30 (its default `end`) the ramp is fully at `to`.
    let (mut doc, id) = graph_doc(Value::constant(0.0));
    apply_op(&mut doc, id, GraphOp::Promote(PropKind::Opacity), 0);
    apply_op(
        &mut doc,
        id,
        GraphOp::SetKind { target: GraphTarget::Prop(PropKind::Opacity), path: vec![], new: ExprKind::Ramp },
        0,
    );
    apply_op(
        &mut doc,
        id,
        GraphOp::SetLit { target: GraphTarget::Prop(PropKind::Opacity), path: vec![1], value: ExprValue::Num(5.0) },
        0,
    );
    let node = doc.root.find(id).unwrap();
    let mut ctx = EvalCtx::new(&doc, 30.0);
    let v = ctx.in_node(id, |ctx| node.transform.opacity.resolve(ctx));
    assert!((v - 5.0).abs() < 1e-9, "ramp reaches its edited `to` at end");
}

#[test]
fn set_waveform_retunes_an_oscillator_without_touching_its_knobs() {
    // A saw at freq 1.0 (one cycle per frame), amp 1: at frame 0.5 sine gives
    // 0 but saw gives 0; use a clearer split — sine(0.25 cycle)=1, square=1,
    // saw(0.25)=−0.5. Switch sine→saw and read the change at a quarter cycle.
    let (mut doc, id) = graph_doc(Value::constant(0.0));
    apply_op(&mut doc, id, GraphOp::Promote(PropKind::Opacity), 0);
    apply_op(
        &mut doc,
        id,
        GraphOp::SetKind { target: GraphTarget::Prop(PropKind::Opacity), path: vec![], new: ExprKind::Oscillator },
        0,
    );
    // freq 1.0 so `frame` counts cycles directly; amp 1, phase/offset 0.
    apply_op(
        &mut doc,
        id,
        GraphOp::SetLit { target: GraphTarget::Prop(PropKind::Opacity), path: vec![0], value: ExprValue::Num(1.0) },
        0,
    );
    let sample = |doc: &Document| {
        let node = doc.root.find(id).unwrap();
        let mut ctx = EvalCtx::new(doc, 0.25);
        ctx.in_node(id, |ctx| node.transform.opacity.resolve(ctx))
    };
    assert!((sample(&doc) - 1.0).abs() < 1e-9, "sine at a quarter cycle is +1");
    apply_op(
        &mut doc,
        id,
        GraphOp::SetWaveform {
            target: GraphTarget::Prop(PropKind::Opacity),
            path: vec![],
            wave: Waveform::Saw,
        },
        0,
    );
    assert!((sample(&doc) + 0.5).abs() < 1e-9, "saw at a quarter cycle is −0.5");
    // The knobs are untouched — freq is still the 1.0 we set.
    let opacity = &doc.root.find(id).unwrap().transform.opacity;
    match opacity {
        Value::Expr(Expr::Gen(Generator::Oscillator { freq, wave, .. })) => {
            assert_eq!(freq.to_string(), "1");
            assert_eq!(*wave, Waveform::Saw);
        }
        _ => panic!("still an oscillator"),
    }
}

#[test]
fn a_generator_knob_box_reserves_room_for_its_label() {
    // A noise generator lays out with its three knobs as child boxes; each
    // knob box is taller than a bare literal because it reserves a line for
    // its slot label, and the stack still clears (no overlap).
    let e = Expr::seed(ExprKind::Noise);
    let boxes = layout_expr(&e);
    assert_eq!(boxes.len(), 4, "the generator plus three knob boxes");
    let bare = box_height(&Expr::num(0.0), false);
    let freq = box_at_path(&boxes, &[0]);
    assert!(freq.height > bare, "a labelled knob reserves more than a bare value");
    let amp = box_at_path(&boxes, &[1]);
    assert!(amp.y >= freq.y + freq.height, "the knob below clears the labelled one");
}

/// A rect with every optional property present.
fn full_node() -> MNode {
    let mut n = MNode::shape(
        1,
        "rect",
        MShape::Rect {
            size: Value::constant(Vec2::new(100.0, 50.0)),
            radius: Value::constant(4.0),
        },
    )
    .with_fill(MColor::rgb(1.0, 0.0, 0.0));
    n.stroke = Some(motion_core::Stroke {
        color: Value::constant(MColor::rgb(0.0, 0.0, 1.0)),
        width: Value::constant(2.0),
    });
    n
}

#[test]
fn prop_of_and_prop_of_mut_agree_on_what_exists() {
    // The two are separate matches over the same 9 variants, and every
    // keyframe operation trusts them to describe the same node. If they
    // ever disagree, reads and writes silently target different properties.
    for node in [full_node(), MNode::group(1, "g")] {
        for kind in PropKind::ALL {
            let mut m = node.clone();
            assert_eq!(
                prop_of(&node, kind).is_some(),
                prop_of_mut(&mut m, kind).is_some(),
                "{kind:?} disagrees on {}",
                node.name
            );
        }
    }
}

#[test]
fn optional_properties_are_absent_when_the_node_lacks_them() {
    // A group has no paint and no geometry...
    let g = MNode::group(1, "g");
    for kind in [
        PropKind::Fill,
        PropKind::StrokeColor,
        PropKind::StrokeWidth,
        PropKind::ShapeSize,
        PropKind::ShapeRadius,
    ] {
        assert!(prop_of(&g, kind).is_none(), "group should not have {kind:?}");
    }
    // ...but it still transforms.
    assert!(prop_of(&g, PropKind::Position).is_some());

    // An ellipse has a size but no corner radius.
    let e = MNode::shape(2, "e", MShape::Ellipse { size: Value::constant(Vec2::new(10.0, 10.0)) });
    assert!(prop_of(&e, PropKind::ShapeSize).is_some());
    assert!(prop_of(&e, PropKind::ShapeRadius).is_none(), "ellipse has no radius");

    // A hand-drawn path has neither: its geometry isn't parametric.
    let p = MNode::shape(3, "p", MShape::Path(kurbo::BezPath::new()));
    assert!(prop_of(&p, PropKind::ShapeSize).is_none());
    assert!(prop_of(&p, PropKind::ShapeRadius).is_none());
}

/// Each layer kind gets its own icon: the bug was an ellipse borrowing the
/// rectangle's square because the row only distinguished group/precomp/rect.
#[test]
fn layer_rows_pick_an_icon_per_shape() {
    let rect = MNode::shape(1, "r", MShape::Rect {
        size: Value::constant(Vec2::new(10.0, 10.0)),
        radius: Value::constant(0.0),
    });
    let ellipse =
        MNode::shape(2, "e", MShape::Ellipse { size: Value::constant(Vec2::new(10.0, 10.0)) });
    let path = MNode::shape(3, "p", MShape::Path(kurbo::BezPath::new()));
    let group = MNode::group(4, "g");
    let root = MNode::group(0, "root")
        .with_child(rect)
        .with_child(ellipse)
        .with_child(path)
        .with_child(group);

    let mut rows = Vec::new();
    tree_rows(&root, 0, &mut rows);
    let glyph = |name: &str| row_glyph(rows.iter().find(|r| r.name == name).unwrap());

    assert_eq!(glyph("r"), icon::RECT);
    assert_eq!(glyph("e"), icon::ELLIPSE, "an ellipse shows the ellipse glyph, not the square");
    assert_eq!(glyph("g"), icon::GROUP);
    assert_eq!(glyph("p"), icon::RECT, "a path shares the rect glyph (no dedicated one)");
    assert_ne!(icon::RECT, icon::ELLIPSE, "the two glyphs really are different");
}

/// A precomp reads as a comp first: its glyph wins over whatever shape the
/// instance layer happens to carry.
#[test]
fn a_precomp_row_shows_the_comp_icon_over_its_shape() {
    let mut inst =
        MNode::shape(1, "inst", MShape::Ellipse { size: Value::constant(Vec2::new(1.0, 1.0)) });
    inst.precomp = Some(CompId(7));
    let root = MNode::group(0, "root").with_child(inst);

    let mut rows = Vec::new();
    tree_rows(&root, 0, &mut rows);
    let row = rows.iter().find(|r| r.name == "inst").unwrap();
    assert_eq!(row_glyph(row), icon::PRECOMP);
}

#[test]
fn dope_rows_lists_animated_shape_and_stroke_properties() {
    let mut n = full_node();
    // Nothing animated yet → no rows, even though every property exists.
    assert!(dope_rows(&n).is_empty());

    prop_of_mut(&mut n, PropKind::ShapeRadius).unwrap().insert_key(5);
    prop_of_mut(&mut n, PropKind::StrokeWidth).unwrap().insert_key(7);
    prop_of_mut(&mut n, PropKind::Fill).unwrap().insert_key(9);

    let rows = dope_rows(&n);
    let kinds: Vec<_> = rows.iter().map(|r| r.kind).collect();
    // Row order follows PropKind's declaration order, not insertion order.
    assert_eq!(
        kinds,
        vec![PropKind::Fill, PropKind::StrokeWidth, PropKind::ShapeRadius]
    );
    assert_eq!(rows[2].frames, vec![5], "radius keyed at frame 5");
}

#[test]
fn a_color_clip_will_not_paste_onto_a_scalar_property() {
    // The type tag on ClipTrack is the only thing standing between a fill
    // copy and a width track full of nonsense.
    let mut n = full_node();
    prop_of_mut(&mut n, PropKind::Fill).unwrap().insert_key(0);
    let clip = prop_of(&n, PropKind::Fill).unwrap().keys_at(&[0]);
    assert!(matches!(clip, ClipTrack::Color(_)));

    let landed = prop_of_mut(&mut n, PropKind::StrokeWidth).unwrap().insert_keys(&clip, 0);
    assert!(landed.is_empty(), "color keys must not land on a width track");
    assert!(!is_anim(&n, PropKind::StrokeWidth), "width stays constant");
}

// --- Pre-composing (stage 4) ---

/// A three-layer comp, to check what pre-composing does to the middle one.
fn three_layer_comp() -> MProject {
    let layer = |id: u64, name: &str| {
        MNode::shape(
            id,
            name,
            MShape::Rect {
                size: Value::constant(Vec2::new(10.0, 10.0)),
                radius: Value::constant(0.0),
            },
        )
    };
    MProject::single(Document::new(
        640.0,
        480.0,
        MNode::group(0, "root")
            .with_child(layer(1, "back"))
            .with_child(layer(2, "middle"))
            .with_child(layer(3, "front")),
    ))
}

#[test]
fn precomposing_replaces_the_layer_in_place_with_an_instance() {
    let mut project = three_layer_comp();
    let current = project.root;
    let (comp_id, instance_id) =
        precompose_into(&mut project, current, NodeId(2), 99).expect("a layer can be precomposed");

    // Draw order is sibling order — the instance must land where the layer was.
    let open = project.comp(current).unwrap();
    let ids: Vec<u64> = open.root.children.iter().map(|c| c.id.0).collect();
    assert_eq!(ids, vec![1, 99, 3], "instance sits in the layer's slot");

    let inst = open.root.find(instance_id).unwrap();
    assert_eq!(inst.precomp, Some(comp_id), "it instances the new comp");
    assert_eq!(inst.name, "middle", "and inherits the layer's name");

    // The layer itself now lives in the new comp, under its root.
    let inner = project.comp(comp_id).unwrap();
    assert_eq!(inner.root.children.len(), 1);
    assert_eq!(inner.root.children[0].id, NodeId(2), "the original node moved");
    assert_eq!(inner.name, "middle");
}

/// The new comp inherits the open one's format, so nested content keeps its
/// coordinate space and timing rather than being silently rescaled or retimed.
#[test]
fn a_precomp_inherits_the_open_comps_format() {
    let mut project = three_layer_comp();
    let current = project.root;
    {
        let open = project.comp_mut(current).unwrap();
        open.fps = 24.0;
        open.duration = 7.5;
    }
    let (comp_id, _) = precompose_into(&mut project, current, NodeId(2), 99).unwrap();
    let inner = project.comp(comp_id).unwrap();
    assert_eq!((inner.width, inner.height), (640.0, 480.0));
    assert_eq!(inner.fps, 24.0);
    assert_eq!(inner.duration, 7.5);
}

/// Pre-composing must be visually a no-op: the layer's transform travels *into*
/// the comp with it, and the instance is neutral. Applying it at both levels
/// would double it — the classic way this goes wrong.
#[test]
fn precomposing_does_not_double_the_layers_transform() {
    let mut project = three_layer_comp();
    let current = project.root;
    project
        .comp_mut(current)
        .unwrap()
        .root
        .find_mut(NodeId(2))
        .unwrap()
        .transform
        .position = Value::constant(Vec2::new(100.0, 50.0));

    let before = motion_core::evaluate_comp(&project, current, 0.0);
    let x_of = |scene: &MScene, src: u64| {
        scene.items.iter().find(|i| i.source == NodeId(src)).unwrap().transform.as_coeffs()[4]
    };
    assert!((x_of(&before, 2) - 100.0).abs() < 1e-9);

    precompose_into(&mut project, current, NodeId(2), 99).unwrap();
    let after = motion_core::evaluate_comp(&project, current, 0.0);
    assert_eq!(after.items.len(), before.items.len(), "same layers on screen");
    assert!(after.warnings.is_empty(), "{:?}", after.warnings);
    // Still 100, not 200: the transform is applied once, inside the comp.
    assert!((x_of(&after, 2) - 100.0).abs() < 1e-9, "transform applied twice");
}

/// The root of a comp *is* the comp, so it can't be precomposed into itself.
#[test]
fn the_root_cannot_be_precomposed() {
    let mut project = three_layer_comp();
    let current = project.root;
    assert!(precompose_into(&mut project, current, NodeId(0), 99).is_none());
    assert_eq!(project.comps.len(), 1, "no comp was created");
}


// --- Module ops (the module UI) ---

/// A one-node project whose opacity is expression-driven, ready to extract.
fn project_with_expr_opacity() -> (MProject, CompId, NodeId) {
    let node = MNode::shape(
        1,
        "dot",
        MShape::Rect {
            size: Value::constant(Vec2::new(10.0, 10.0)),
            radius: Value::constant(0.0),
        },
    )
    .with_transform(Transform {
        opacity: Value::expr(Expr::Lit(ExprValue::Num(0.25))),
        ..Transform::default()
    });
    let project = MProject::single(Document::new(
        100.0,
        100.0,
        MNode::group(0, "root").with_child(node),
    ));
    let comp = project.root;
    (project, comp, NodeId(1))
}

/// Extracting must be a **no-op on the rendered frame**: the recipe moves to the
/// module and the property links it, so the value is identical either side.
/// That's what makes it safe to press on work you care about.
#[test]
fn extracting_a_module_leaves_the_value_unchanged() {
    let (mut project, comp, id) = project_with_expr_opacity();
    let before = motion_core::evaluate_comp(&project, comp, 0.0).items[0].opacity;

    apply_graph_op(&mut project, comp, Some(id), GraphOp::ExtractModule { kind: PropKind::Opacity }, 0);

    assert_eq!(project.modules.len(), 1, "a module was created");
    let scene = motion_core::evaluate_comp(&project, comp, 0.0);
    assert!(scene.warnings.is_empty(), "{:?}", scene.warnings);
    assert!((scene.items[0].opacity - before).abs() < 1e-9, "the frame is unchanged");

    // And the property is now a link, not a copy of the recipe.
    let node = project.comp(comp).unwrap().root.find(id).unwrap();
    let expr = prop_of(node, PropKind::Opacity).unwrap().expr().cloned().unwrap();
    assert!(matches!(expr, Expr::Use { .. }), "the property links the module");
}

/// The extracted module keeps the recipe itself, so editing the module is what
/// changes the frame from then on.
#[test]
fn editing_the_extracted_module_drives_the_property() {
    let (mut project, comp, id) = project_with_expr_opacity();
    apply_graph_op(&mut project, comp, Some(id), GraphOp::ExtractModule { kind: PropKind::Opacity }, 0);
    let module = *project.modules.keys().next().unwrap();

    project.module_mut(module).unwrap().body = Expr::Lit(ExprValue::Num(0.75));
    let scene = motion_core::evaluate_comp(&project, comp, 0.0);
    assert!((scene.items[0].opacity - 0.75).abs() < 1e-9);
}

/// Overriding a knob and then clearing it must return the link to *inheriting*,
/// not to some captured copy of the old value — that distinction is the whole
/// point of a module.
#[test]
fn clearing_an_override_returns_the_link_to_inheriting() {
    let (mut project, comp, id) = project_with_expr_opacity();
    let module = project.add_module(
        MModule::new("level", Expr::Param { node: None, name: "amount".into() })
            .with_param("amount", ParamValue::Num(Value::constant(0.4))),
    );
    apply_graph_op(&mut project, comp, Some(id), GraphOp::LinkModule { kind: PropKind::Opacity, module }, 0);
    let opacity = |p: &MProject| motion_core::evaluate_comp(p, comp, 0.0).items[0].opacity;
    assert!((opacity(&project) - 0.4).abs() < 1e-9, "inherits the module default");

    // Override this one link.
    apply_graph_op(
        &mut project,
        comp,
        Some(id),
        GraphOp::SetOverride {
            target: GraphTarget::Prop(PropKind::Opacity),
            path: Vec::new(),
            name: "amount".into(),
            value: Some(ExprValue::Num(0.9)),
        },
        0,
    );
    assert!((opacity(&project) - 0.9).abs() < 1e-9, "the override wins");

    // Now change the module's default and clear the override: the link must
    // follow the *new* default, proving it inherits rather than restoring 0.4.
    project.module_mut(module).unwrap().params[0].value =
        ParamValue::Num(Value::constant(0.1));
    apply_graph_op(
        &mut project,
        comp,
        Some(id),
        GraphOp::SetOverride {
            target: GraphTarget::Prop(PropKind::Opacity),
            path: Vec::new(),
            name: "amount".into(),
            value: None,
        },
        0,
    );
    assert!((opacity(&project) - 0.1).abs() < 1e-9, "back to inheriting, at the new default");
}

/// Repointing a link keeps its overrides: knobs are matched by name, so a knob
/// the two modules share carries over rather than being silently dropped.
#[test]
fn repointing_a_link_keeps_overrides_that_still_apply() {
    let (mut project, comp, id) = project_with_expr_opacity();
    let knob = |name: &str, default: f64| {
        MModule::new(name, Expr::Param { node: None, name: "amount".into() })
            .with_param("amount", ParamValue::Num(Value::constant(default)))
    };
    let a = project.add_module(knob("a", 0.2));
    let b = project.add_module(knob("b", 0.8));

    apply_graph_op(&mut project, comp, Some(id), GraphOp::LinkModule { kind: PropKind::Opacity, module: a }, 0);
    apply_graph_op(
        &mut project,
        comp,
        Some(id),
        GraphOp::SetOverride {
            target: GraphTarget::Prop(PropKind::Opacity),
            path: Vec::new(),
            name: "amount".into(),
            value: Some(ExprValue::Num(0.55)),
        },
        0,
    );
    apply_graph_op(
        &mut project,
        comp,
        Some(id),
        GraphOp::SetModule { target: GraphTarget::Prop(PropKind::Opacity), path: Vec::new(), module: b },
        0,
    );

    let scene = motion_core::evaluate_comp(&project, comp, 0.0);
    assert!(scene.warnings.is_empty(), "{:?}", scene.warnings);
    assert!((scene.items[0].opacity - 0.55).abs() < 1e-9, "the override carried over");
}

/// Deleting a module doesn't rewrite its links — they warn and fall back, the
/// same as any other dangling reference. A silent revert would hide the loss.
#[test]
fn deleting_a_module_leaves_its_links_warning() {
    let (mut project, comp, id) = project_with_expr_opacity();
    let module = project.add_module(MModule::new("gone", Expr::Lit(ExprValue::Num(0.3))));
    apply_graph_op(&mut project, comp, Some(id), GraphOp::LinkModule { kind: PropKind::Opacity, module }, 0);
    apply_graph_op(&mut project, comp, Some(id), GraphOp::DeleteModule { module }, 0);

    let scene = motion_core::evaluate_comp(&project, comp, 0.0);
    assert!(
        scene.warnings.iter().any(|(_, m)| m.contains("no longer exists")),
        "{:?}",
        scene.warnings
    );
}

/// The headline of the graph-UI step: a module's *body* is edited on the same
/// canvas a property is, addressed by [`GraphTarget::Module`] and applied
/// through the same [`apply_graph_op`]. No node need be selected — the body
/// isn't any node's property — so the op goes through with `selected: None`.
#[test]
fn editing_a_module_body_drives_every_link() {
    let (mut project, comp, id) = project_with_expr_opacity();
    apply_graph_op(&mut project, comp, Some(id), GraphOp::ExtractModule { kind: PropKind::Opacity }, 0);
    let module = *project.modules.keys().next().unwrap();
    let opacity = |p: &MProject| motion_core::evaluate_comp(p, comp, 0.0).items[0].opacity;
    assert!((opacity(&project) - 0.25).abs() < 1e-9, "starts at the extracted value");

    // Set the body's literal through the module target, with nothing selected.
    apply_graph_op(
        &mut project,
        comp,
        None,
        GraphOp::SetLit {
            target: GraphTarget::Module(module),
            path: vec![],
            value: ExprValue::Num(0.8),
        },
        0,
    );
    assert!((opacity(&project) - 0.8).abs() < 1e-9, "the link follows the edited body");
}

/// Growing the body's tree — kind picker → operator, then its operands — works
/// through the module target exactly as it does for a property.
#[test]
fn a_module_body_grows_its_tree_through_set_kind() {
    let (mut project, comp, id) = project_with_expr_opacity();
    apply_graph_op(&mut project, comp, Some(id), GraphOp::ExtractModule { kind: PropKind::Opacity }, 0);
    let module = *project.modules.keys().next().unwrap();
    let target = GraphTarget::Module(module);
    apply_graph_op(&mut project, comp, None, GraphOp::SetKind { target, path: vec![], new: ExprKind::Add }, 0);
    apply_graph_op(&mut project, comp, None, GraphOp::SetLit { target, path: vec![0], value: ExprValue::Num(0.3) }, 0);
    apply_graph_op(&mut project, comp, None, GraphOp::SetLit { target, path: vec![1], value: ExprValue::Num(0.4) }, 0);

    let scene = motion_core::evaluate_comp(&project, comp, 0.0);
    assert!(scene.warnings.is_empty(), "{:?}", scene.warnings);
    assert!((scene.items[0].opacity - 0.7).abs() < 1e-9, "the two operands summed");
}

/// A module grows knobs the same way a node does, via [`ParamOwner::Module`],
/// and its body reads one through a `param` node — the whole point of an
/// editable module body, since the tunables are what a link overrides.
#[test]
fn a_module_knob_added_through_the_ops_drives_the_body() {
    let (mut project, comp, id) = project_with_expr_opacity();
    apply_graph_op(&mut project, comp, Some(id), GraphOp::ExtractModule { kind: PropKind::Opacity }, 0);
    let module = *project.modules.keys().next().unwrap();
    let target = GraphTarget::Module(module);
    let opacity = |p: &MProject| motion_core::evaluate_comp(p, comp, 0.0).items[0].opacity;

    // Add a knob, then point the body at it.
    apply_graph_op(
        &mut project,
        comp,
        None,
        GraphOp::AddParam { owner: ParamOwner::Module(module), name: "level".into(), kind: ParamKind::Num },
        0,
    );
    apply_graph_op(&mut project, comp, None, GraphOp::SetKind { target, path: vec![], new: ExprKind::Param }, 0);
    apply_graph_op(&mut project, comp, None, GraphOp::SetParam { target, path: vec![], name: "level".into() }, 0);
    assert!((opacity(&project) - 0.0).abs() < 1e-9, "reads the knob's neutral default");

    // Move the knob's default: the body follows it.
    project.module_mut(module).unwrap().set_param("level", ParamValue::Num(Value::constant(0.6)));
    assert!((opacity(&project) - 0.6).abs() < 1e-9);

    // Remove the knob: the body's `param("level")` warns and falls back, like
    // any dangling reference — not a silent no-op.
    apply_graph_op(
        &mut project,
        comp,
        None,
        GraphOp::RemoveParam { owner: ParamOwner::Module(module), name: "level".into() },
        0,
    );
    let scene = motion_core::evaluate_comp(&project, comp, 0.0);
    assert!(scene.warnings.iter().any(|(_, m)| m.contains("level")), "{:?}", scene.warnings);
}

/// `gather` exposes the edited module's body and knobs so the panel can draw
/// them — and only when a module is actually opened for editing.
#[test]
fn gather_exposes_the_edited_module_body() {
    let (mut project, comp, id) = project_with_expr_opacity();
    apply_graph_op(&mut project, comp, Some(id), GraphOp::ExtractModule { kind: PropKind::Opacity }, 0);
    let module = *project.modules.keys().next().unwrap();
    project.module_mut(module).unwrap().set_param("amp", ParamValue::Num(Value::constant(1.0)));
    let doc = project.comp(comp).unwrap();

    // Nothing opened → no module-edit view, even though the module exists.
    let closed = GraphInfo::gather(doc, &project.modules, None, None, 0.0);
    assert!(closed.editing.is_none());
    assert_eq!(closed.modules.len(), 1, "but the module still lists");

    // Opened → the body and its knobs come through.
    let open = GraphInfo::gather(doc, &project.modules, None, Some(module), 0.0);
    let edit = open.editing.expect("the opened module's body");
    assert_eq!(edit.id, module);
    assert!(matches!(edit.body, Expr::Lit(ExprValue::Num(_))), "the extracted body");
    assert_eq!(edit.params, vec![("amp".to_string(), "number")]);
}

/// Renaming is just a label — every link is by id, so nothing breaks.
#[test]
fn renaming_a_module_keeps_its_links() {
    let (mut project, comp, id) = project_with_expr_opacity();
    let module = project.add_module(MModule::new("old", Expr::Lit(ExprValue::Num(0.3))));
    apply_graph_op(&mut project, comp, Some(id), GraphOp::LinkModule { kind: PropKind::Opacity, module }, 0);
    apply_graph_op(
        &mut project,
        comp,
        Some(id),
        GraphOp::RenameModule { module, name: "new".into() },
        0,
    );
    assert_eq!(project.module(module).unwrap().name, "new");
    let scene = motion_core::evaluate_comp(&project, comp, 0.0);
    assert!(scene.warnings.is_empty(), "{:?}", scene.warnings);
    assert!((scene.items[0].opacity - 0.3).abs() < 1e-9);
}
