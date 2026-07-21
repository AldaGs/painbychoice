//! Unit tests for the editor: axis math, clip drags, dock ops, and the
//! property/keyframe plumbing.
//!
//! Moved verbatim out of `main.rs` when it was split by concern.

use crate::*;

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
    assert!(Editor::NodeGraph.is_swappable());
    assert!(!Editor::Canvas.is_swappable());
    assert!(!Editor::Comp.is_swappable());
    assert!(!Editor::Transport.is_swappable());
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

/// A text layer's `content` is a `Value` like any other property, so it must
/// reach the keyframe machinery: a dopesheet row, a stopwatch, retiming,
/// copy/paste. Before the string value model it was a plain field and had none
/// of that.
#[test]
fn a_text_layers_content_is_an_animatable_property() {
    let t = text_node();
    assert!(prop_of(&t, PropKind::TextContent).is_some(), "text has content");
    assert!(prop_of(&t, PropKind::TextSize).is_some(), "and a font size");

    // Only a text layer has one — the same rule radius follows on an ellipse.
    let e = MNode::shape(2, "e", MShape::Ellipse { size: Value::constant(Vec2::new(10.0, 10.0)) });
    assert!(prop_of(&e, PropKind::TextContent).is_none(), "an ellipse has no content");
    assert!(prop_of(&MNode::group(1, "g"), PropKind::TextContent).is_none());
}

/// Copy/paste is typed: a clip is tagged at copy time and must only land on a
/// property of the same type. A string clip pasted onto a scalar has to be
/// ignored, not coerced — the guard that keeps a text track off a rotation row.
#[test]
fn a_string_key_clip_only_pastes_onto_a_string_property() {
    let mut t = text_node();
    // Give content two keys, then lift them onto a clipboard.
    if let Some(PropRefMut::Str(v)) = prop_of_mut(&mut t, PropKind::TextContent) {
        *v = Value::Keyframed(motion_core::Track::new(vec![
            motion_core::Keyframe::linear(0, "one".to_string()),
            motion_core::Keyframe::linear(10, "two".to_string()),
        ]));
    } else {
        panic!("content should borrow as a string property");
    }
    let clip = prop_of(&t, PropKind::TextContent).unwrap().keys_at(&[0, 1]);

    // Onto the same property: lands.
    let mut ok = t.clone();
    let landed = prop_of_mut(&mut ok, PropKind::TextContent).unwrap().insert_keys(&clip, 5);
    assert_eq!(landed.len(), 2, "a string clip lands on a string property");

    // Onto a scalar: refused, and the property is left alone.
    let mut wrong = t.clone();
    let landed = prop_of_mut(&mut wrong, PropKind::Rotation).unwrap().insert_keys(&clip, 5);
    assert!(landed.is_empty(), "a string clip must not land on rotation");
    assert!(!prop_of(&wrong, PropKind::Rotation).unwrap().is_animated());
}

/// A text layer for the property tests above.
fn text_node() -> MNode {
    MNode::shape(
        7,
        "caption",
        MShape::Text {
            content: Value::constant("hello".to_string()),
            family: String::new(),
            size: Value::constant(48.0),
            align: motion_core::text::TextAlign::Left,
            max_width: None,
        },
    )
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


/// Build a one-layer comp at `fps` with a rotation key on `frame`.
fn comp_at_fps(fps: f64, frame: i64) -> motion_core::node::Comp {
    use motion_core::value::{Keyframe, Track};
    let mut transform = motion_core::node::Transform::default();
    transform.rotation_deg = motion_core::Value::Keyframed(Track::new(vec![
        Keyframe::linear(0, 0.0),
        Keyframe::linear(frame, 90.0),
    ]));
    let layer = MNode::group(1, "layer").with_transform(transform);
    let mut comp =
        motion_core::node::Comp::new(640.0, 480.0, MNode::group(0, "root").with_child(layer));
    comp.fps = fps;
    comp
}

fn rot_keys(comp: &motion_core::node::Comp) -> Vec<i64> {
    comp.root.children[0].transform.rotation_deg.key_frames()
}

/// `apply_fps_edit` with no node selected — the selection remap is exercised
/// separately, by the tests that actually build one.
fn fps_edit_apply(comp: &mut motion_core::node::Comp, drag: &mut Option<(f64, MNode, KeySelection)>, e: &CompEdits) {
    crate::app::apply_fps_edit(comp, drag, e, None, &mut KeySelection::new());
}

fn fps_edit(fps: f64) -> CompEdits {
    CompEdits { fps: Some(fps), ..Default::default() }
}

/// Dragging the spinner resolves the keys live, on every delta, in either
/// direction — the user sees the dopesheet move as they drag.
#[test]
fn dragging_the_fps_spinner_retimes_on_every_delta() {
    let mut comp = comp_at_fps(60.0, 120);
    let mut drag = None;

    let mut start = fps_edit(30.0);
    start.fps_drag_started = true;
    fps_edit_apply(&mut comp, &mut drag, &start);
    assert_eq!(rot_keys(&comp), vec![0, 60], "the first delta already moved the key");

    // A later delta in the same drag, further down.
    fps_edit_apply(&mut comp, &mut drag, &fps_edit(24.0));
    assert_eq!(rot_keys(&comp), vec![0, 48], "2s is frame 48 @ 24fps");

    // And back up, past where the drag began — still measured off the start.
    fps_edit_apply(&mut comp, &mut drag, &fps_edit(120.0));
    assert_eq!(rot_keys(&comp), vec![0, 240], "2s is frame 240 @ 120fps");
}

/// The point of snapshotting: a drag is one conversion off the grid it started
/// on, so travelling over lossy intermediate rates costs nothing. Applied
/// naively, each delta would round again and drag keys off their seconds.
#[test]
fn a_long_drag_does_not_compound_rounding() {
    use motion_core::value::{Keyframe, Track};
    // Keys one frame apart @ 60fps cannot all survive a 7fps grid.
    let mut comp = comp_at_fps(60.0, 120);
    comp.root.children[0].transform.rotation_deg = motion_core::Value::Keyframed(Track::new(vec![
        Keyframe::linear(0, 0.0),
        Keyframe::linear(120, 45.0),
        Keyframe::linear(121, 90.0),
    ]));
    let mut drag = None;
    let mut start = fps_edit(59.0);
    start.fps_drag_started = true;
    fps_edit_apply(&mut comp, &mut drag, &start);
    // Sweep all the way down through every intermediate rate, then back up.
    for fps in (7..59).rev() {
        fps_edit_apply(&mut comp, &mut drag, &fps_edit(fps as f64));
    }
    for fps in 8..=60 {
        fps_edit_apply(&mut comp, &mut drag, &fps_edit(fps as f64));
    }
    let mut stop = fps_edit(60.0);
    stop.fps_drag_stopped = true;
    fps_edit_apply(&mut comp, &mut drag, &stop);

    assert_eq!(comp.fps, 60.0);
    assert_eq!(rot_keys(&comp), vec![0, 120, 121], "returning to 60fps restores every key");
    assert!(drag.is_none(), "releasing the drag drops the snapshot");
}

/// Once released, the next drag snapshots the *new* state — it must not rewind
/// to a stale grid from a drag the user already committed.
#[test]
fn a_second_drag_starts_from_the_committed_rate() {
    let mut comp = comp_at_fps(60.0, 120);
    let mut drag = None;

    let mut first = fps_edit(24.0);
    first.fps_drag_started = true;
    fps_edit_apply(&mut comp, &mut drag, &first);
    let mut stop = fps_edit(24.0);
    stop.fps_drag_stopped = true;
    fps_edit_apply(&mut comp, &mut drag, &stop);
    assert_eq!(rot_keys(&comp), vec![0, 48]);

    let mut second = fps_edit(48.0);
    second.fps_drag_started = true;
    fps_edit_apply(&mut comp, &mut drag, &second);
    assert_eq!(rot_keys(&comp), vec![0, 96], "2s @ 48fps, measured from 24 not 60");
}

/// Typing a rate carries no drag, and applies as a plain one-shot retime.
#[test]
fn a_typed_fps_retimes_without_a_drag() {
    let mut comp = comp_at_fps(60.0, 120);
    let mut drag = None;
    fps_edit_apply(&mut comp, &mut drag, &fps_edit(24.0));
    assert_eq!(rot_keys(&comp), vec![0, 48]);
    assert!(drag.is_none());
}

/// Zooming about an anchor keeps that frame where it is — the property that
/// makes the wheel feel like zooming rather than jumping, and the reason the
/// buttons anchor at the playhead.
#[test]
fn zoom_keeps_the_anchor_frame_put() {
    let view = TimelineView { start: 0.0, visible: 120.0 };
    for anchor in [0.0, 30.0, 60.0, 119.0] {
        for factor in [0.5, 0.7, 1.0, 2.0] {
            let z = zoomed(view, factor, anchor);
            let before = (anchor - view.start) / view.visible;
            let after = (anchor - z.start) / z.visible;
            assert!(
                (before - after).abs() < 1e-9,
                "anchor {anchor} moved at factor {factor}: {before} -> {after}"
            );
            assert!((z.visible - view.visible * factor).abs() < 1e-9);
        }
    }
}

/// Zoom in then out returns to where it started, so tapping the buttons an
/// equal number of times is a no-op rather than a slow drift.
#[test]
fn zoom_in_then_out_round_trips() {
    let view = TimelineView { start: 10.0, visible: 60.0 };
    let there = zoomed(view, ZOOM_STEP, 40.0);
    let back = zoomed(there, 1.0 / ZOOM_STEP, 40.0);
    assert!((back.start - view.start).abs() < 1e-9);
    assert!((back.visible - view.visible).abs() < 1e-9);
}

/// Scroll/`+`/`−` zoom about the cursor: the composition point under the
/// pointer must stay under it across the zoom, the same invariant the timeline
/// wheel has. Without it the canvas would jump out from under you.
#[test]
fn canvas_zoom_keeps_the_point_under_the_cursor() {
    let doc = Document::new(320.0, 240.0, MNode::group(0, "root"));
    let area = kurbo::Rect::new(0.0, 0.0, 800.0, 600.0);
    let ppp = 1.5;
    for cursor in [(120.0, 90.0), (400.0, 300.0), (760.0, 40.0)] {
        // Start from Fit, then zoom about the cursor a few steps.
        let mut nav = CanvasNav::default();
        for factor in [1.25, 1.25, 0.8, 2.0] {
            let scale = canvas_scale(&doc, area, nav, ppp);
            let pt = canvas_transform(&doc, area, nav, ppp).inverse()
                * Point::new(cursor.0, cursor.1);
            nav = nav_zoom_about(&doc, area, pt, cursor, scale * factor, ppp);
            // The same comp point must map back to the cursor after the zoom.
            let landed = canvas_transform(&doc, area, nav, ppp) * pt;
            assert!(
                (landed.x - cursor.0).abs() < 1e-6 && (landed.y - cursor.1).abs() < 1e-6,
                "cursor {cursor:?} drifted to ({}, {})",
                landed.x,
                landed.y
            );
        }
    }
}

/// Fit leaves a `FIT_MARGIN`-point gap: the fitted comp touches neither the
/// full canvas edge (there is a margin) nor overflows it (it still fits).
#[test]
fn fit_leaves_a_margin_around_the_comp() {
    let doc = Document::new(400.0, 400.0, MNode::group(0, "root"));
    let area = kurbo::Rect::new(0.0, 0.0, 500.0, 500.0);
    let ppp = 1.0;
    let xf = canvas_transform(&doc, area, CanvasNav::default(), ppp);
    let tl = xf * Point::new(0.0, 0.0);
    let br = xf * Point::new(doc.width, doc.height);
    // A square comp in a square area is margin-bound on all four sides.
    assert!((tl.x - FIT_MARGIN).abs() < 1e-6, "left gap {}", tl.x);
    assert!((tl.y - FIT_MARGIN).abs() < 1e-6, "top gap {}", tl.y);
    assert!((area.x1 - br.x - FIT_MARGIN).abs() < 1e-6, "right gap {}", area.x1 - br.x);
    assert!((area.y1 - br.y - FIT_MARGIN).abs() < 1e-6, "bottom gap {}", area.y1 - br.y);
}

#[test]
fn neighbor_key_finds_the_nearest_each_way() {
    let keys = [0i64, 12, 12, 30, 48];
    assert_eq!(neighbor_key(&keys, 12, true), Some(30), "strictly after, skipping the dupe");
    assert_eq!(neighbor_key(&keys, 12, false), Some(0), "strictly before");
    assert_eq!(neighbor_key(&keys, 0, false), None, "nothing before the first key");
    assert_eq!(neighbor_key(&keys, 48, true), None, "nothing after the last key");
    assert_eq!(neighbor_key(&keys, 20, true), Some(30), "from between keys");
    assert_eq!(neighbor_key(&keys, 20, false), Some(12));
    assert_eq!(neighbor_key(&[], 5, true), None, "an unanimated node has no neighbours");
}

/// Unsorted input is normal — rows are gathered per property and concatenated.
#[test]
fn neighbor_key_does_not_assume_sorted_input() {
    let keys = [48i64, 0, 30, 12];
    assert_eq!(neighbor_key(&keys, 20, true), Some(30));
    assert_eq!(neighbor_key(&keys, 20, false), Some(12));
}

/// The label column must always leave room for the track, whatever is stored
/// and however narrow the panel gets — the bug the two-column split fixes.
#[test]
fn the_label_column_can_never_swallow_the_panel() {
    for panel in [60.0f32, 120.0, 400.0, 1600.0] {
        for want in [-50.0f32, 0.0, 80.0, 5000.0] {
            let w = clamp_label_w(want, panel);
            assert!(w >= 44.0, "panel {panel} want {want} gave {w}, below the readable minimum");
            assert!(
                w <= (panel * 0.45).max(44.0),
                "panel {panel} want {want} gave {w}, past the cap"
            );
        }
    }
}

/// A width the user picked is kept verbatim when it already fits — clamping
/// must not creep the column on every pass.
#[test]
fn a_fitting_label_width_is_left_alone() {
    let w = clamp_label_w(120.0, 800.0);
    assert_eq!(w, 120.0);
    assert_eq!(clamp_label_w(w, 800.0), w, "re-clamping is idempotent");
}

/// Retiming must carry the keyframe selection with it. A `KeyRef` is an index,
/// so a merge shifts every index after it — left alone, the dopesheet would
/// keep drawing a selection pointing at keys the user never picked.
#[test]
fn the_key_selection_survives_a_retime() {
    use motion_core::value::{Keyframe, Track};
    let mut comp = comp_at_fps(60.0, 120);
    comp.root.children[0].transform.rotation_deg = motion_core::Value::Keyframed(Track::new(vec![
        Keyframe::linear(0, 0.0),
        Keyframe::linear(60, 30.0),
        Keyframe::linear(120, 90.0),
    ]));
    let mut sel = KeySelection::new();
    sel.insert((PropKind::Rotation, 2)); // the key at frame 120

    let mut drag = None;
    crate::app::apply_fps_edit(
        &mut comp,
        &mut drag,
        &fps_edit(30.0),
        Some(NodeId(1)),
        &mut sel,
    );

    // 120 @ 60fps -> 60 @ 30fps, still the last of three keys.
    assert_eq!(rot_keys(&comp), vec![0, 30, 60]);
    assert_eq!(sel.iter().copied().collect::<Vec<_>>(), vec![(PropKind::Rotation, 2)]);
}

/// The case indices actually break on: two keys that merge. Everything after
/// the merge shifts down one, and a selection on the *later* of the two must
/// land on the survivor rather than on a stale index.
#[test]
fn a_selection_follows_keys_that_merge() {
    use motion_core::value::{Keyframe, Track};
    let mut comp = comp_at_fps(60.0, 120);
    comp.root.children[0].transform.rotation_deg = motion_core::Value::Keyframed(Track::new(vec![
        Keyframe::linear(0, 0.0),
        Keyframe::linear(120, 45.0),
        Keyframe::linear(121, 60.0),
        Keyframe::linear(240, 90.0),
    ]));
    let mut sel = KeySelection::new();
    sel.insert((PropKind::Rotation, 2)); // frame 121
    sel.insert((PropKind::Rotation, 3)); // frame 240

    let mut drag = None;
    // A quarter-rate grid: 120 and 121 both land on 30, which is the merge.
    // (At half rate they would not — 121 * 0.5 rounds up to 61, away from zero.)
    crate::app::apply_fps_edit(
        &mut comp,
        &mut drag,
        &fps_edit(15.0),
        Some(NodeId(1)),
        &mut sel,
    );

    // 120 and 121 both round to 30; 240 -> 60. Three keys survive.
    assert_eq!(rot_keys(&comp), vec![0, 30, 60]);
    assert_eq!(
        sel.iter().copied().collect::<Vec<_>>(),
        vec![(PropKind::Rotation, 1), (PropKind::Rotation, 2)],
        "the merged key collapses onto the survivor, and 240 follows to its new index"
    );
}

/// Over a drag the remap is measured from the pre-drag state, like the retime,
/// so a long sweep can't walk the selection off its keys.
#[test]
fn the_selection_does_not_drift_over_a_drag() {
    use motion_core::value::{Keyframe, Track};
    let mut comp = comp_at_fps(60.0, 120);
    comp.root.children[0].transform.rotation_deg = motion_core::Value::Keyframed(Track::new(vec![
        Keyframe::linear(0, 0.0),
        Keyframe::linear(120, 45.0),
        Keyframe::linear(121, 60.0),
    ]));
    let mut sel = KeySelection::new();
    sel.insert((PropKind::Rotation, 2));

    let mut drag = None;
    let mut start = fps_edit(59.0);
    start.fps_drag_started = true;
    crate::app::apply_fps_edit(&mut comp, &mut drag, &start, Some(NodeId(1)), &mut sel);
    for fps in (7..59).rev() {
        crate::app::apply_fps_edit(
            &mut comp,
            &mut drag,
            &fps_edit(fps as f64),
            Some(NodeId(1)),
            &mut sel,
        );
    }
    for fps in 8..=60 {
        crate::app::apply_fps_edit(
            &mut comp,
            &mut drag,
            &fps_edit(fps as f64),
            Some(NodeId(1)),
            &mut sel,
        );
    }
    assert_eq!(rot_keys(&comp), vec![0, 120, 121], "every key came back");
    assert_eq!(
        sel.iter().copied().collect::<Vec<_>>(),
        vec![(PropKind::Rotation, 2)],
        "and the selection is still on the key it started on"
    );
}

/// A selection on a property that isn't animated, or an index past the end,
/// is dropped rather than panicking or resurrecting as some other key.
#[test]
fn a_stale_selection_entry_is_dropped() {
    let comp = comp_at_fps(60.0, 120);
    let node = &comp.root.children[0];
    let mut sel = KeySelection::new();
    sel.insert((PropKind::Rotation, 99)); // out of range
    sel.insert((PropKind::Position, 0)); // not animated
    let out = remap_selection(&sel, node, node, 1.0);
    assert!(out.is_empty());
}

// --- Transform gizmo -------------------------------------------------------

/// A drag snapshot, positioned so the pivot is at the origin unless a test
/// says otherwise. Every gizmo test resolves against one of these rather than
/// through egui, because `resolve_drag` is deliberately free of it.
fn drag_at(handle: GizmoHandle, rot: f64, grab: (f64, f64)) -> GizmoDrag {
    GizmoDrag {
        handle,
        node: 1,
        start_pos: Vec2::new(0.0, 0.0),
        start_rot: rot,
        start_scale: (1.0, 1.0),
        start_anchor: Vec2::ZERO,
        grab_parent: Point::new(grab.0, grab.1),
    }
}

/// The centre handle moves the layer by exactly the pointer's parent-space
/// delta — no scaling, no rotation folded in.
#[test]
fn the_centre_handle_tracks_the_pointer_one_to_one() {
    let d = drag_at(GizmoHandle::Move, 30.0, (10.0, 10.0));
    let Resolved { pos, rot, scale, .. } = resolve_drag(&d, Point::new(35.0, -5.0));
    assert_eq!((pos.x, pos.y), (25.0, -15.0));
    assert_eq!(rot, 30.0, "a move must not touch rotation");
    assert_eq!(scale, (1.0, 1.0));
}

/// An axis arrow constrains the move to the *layer's* axis, not the parent's:
/// at 90° the layer's X points along parent +Y, so a purely vertical drag
/// moves the full distance and a horizontal one moves nothing.
#[test]
fn an_axis_arrow_projects_the_drag_onto_the_rotated_axis() {
    let d = drag_at(GizmoHandle::MoveAxis(GizmoAxis::X), 90.0, (0.0, 0.0));
    let pos = resolve_drag(&d, Point::new(0.0, 40.0)).pos;
    assert!((pos.x - 0.0).abs() < 1e-9, "no movement across the axis");
    assert!((pos.y - 40.0).abs() < 1e-9, "the whole drag lands along it");

    let pos = resolve_drag(&d, Point::new(40.0, 0.0)).pos;
    assert!(pos.hypot() < 1e-9, "a drag square to the axis moves nothing");
}

/// The ring adds the angle the pointer sweeps about the pivot, and leaves
/// position and scale alone.
#[test]
fn the_ring_adds_the_swept_angle() {
    let d = drag_at(GizmoHandle::Rotate, 10.0, (100.0, 0.0));
    // Straight out on +X, swung to +Y: a quarter turn.
    let Resolved { pos, rot, scale, .. } = resolve_drag(&d, Point::new(0.0, 100.0));
    assert!((rot - 100.0).abs() < 1e-6, "10° + 90°, got {rot}");
    assert_eq!((pos.x, pos.y), (0.0, 0.0));
    assert_eq!(scale, (1.0, 1.0));
}

/// Scale handles are a *ratio* of distances from the pivot, so halving the
/// grab distance halves the scale — and the axis handle touches one axis only.
#[test]
fn scale_handles_use_the_distance_ratio_from_the_pivot() {
    let d = drag_at(GizmoHandle::ScaleUniform, 0.0, (50.0, 0.0));
    let scale = resolve_drag(&d, Point::new(100.0, 0.0)).scale;
    assert_eq!(scale, (2.0, 2.0));

    let d = drag_at(GizmoHandle::ScaleAxis(GizmoAxis::Y), 0.0, (0.0, 40.0));
    let scale = resolve_drag(&d, Point::new(0.0, 20.0)).scale;
    assert!((scale.1 - 0.5).abs() < 1e-9, "Y halved, got {}", scale.1);
    assert_eq!(scale.0, 1.0, "X untouched");
}

/// A rotate or scale grab that lands *on* the pivot has no radius to measure
/// from. It must hold the values rather than divide by zero and emit NaN into
/// the document.
#[test]
fn a_grab_on_the_pivot_is_inert_rather_than_nan() {
    for handle in [GizmoHandle::Rotate, GizmoHandle::ScaleUniform] {
        let d = drag_at(handle, 0.0, (0.0, 0.0));
        let Resolved { pos, rot, scale, .. } = resolve_drag(&d, Point::new(30.0, 30.0));
        assert!(pos.hypot().is_finite() && rot.is_finite());
        assert_eq!(scale, (1.0, 1.0), "{handle:?} held its scale");
        assert_eq!(rot, 0.0, "{handle:?} held its rotation");
    }
}

/// The gizmo recovers the *parent* transform by dividing the layer's own local
/// matrix back out of its world matrix. If that arithmetic is off, the handles
/// draw somewhere other than where the layer's pivot actually is — so check it
/// against a nested layer with a non-trivial anchor.
#[test]
fn the_gizmo_pivot_lands_on_the_layers_anchor_in_the_world() {
    let parent = Affine::translate((300.0, 40.0)) * Affine::rotate(0.4) * Affine::scale(2.0);
    let mut info = NodeInfo::resolve(
        &MNode::group(1, "layer"),
        &Comp::new(100.0, 100.0, MNode::group(0, "root")),
        0.0,
    );
    info.pos = (25.0, -60.0);
    info.rot = 33.0;
    info.scale = (1.5, 0.5);
    info.anchor = (12.0, 7.0);

    let local = Affine::translate(Vec2::new(info.pos.0, info.pos.1))
        * Affine::rotate(info.rot.to_radians())
        * Affine::scale_non_uniform(info.scale.0, info.scale.1)
        * Affine::translate(Vec2::new(-info.anchor.0, -info.anchor.1));
    let world = parent * local;

    let t = GizmoTarget::new(1, world, &info);
    // The recovered parent must map `position` to wherever the world matrix
    // puts the anchor point — that is the definition of the pivot.
    let via_parent = t.parent * Point::new(info.pos.0, info.pos.1);
    let via_world = world * Point::new(info.anchor.0, info.anchor.1);
    assert!(
        (via_parent - via_world).hypot() < 1e-6,
        "pivot drifted: {via_parent:?} vs {via_world:?}"
    );
}

/// Every content leaf must either scroll inside its area or fill it exactly —
/// never allocate past it. egui hands `show_dock` a **content-driven** panel
/// rect and persists it as the panel's own size, so a leaf that overflows
/// resizes its panel and shoves every other leaf around. That is what made
/// selecting a layer resize the whole window: the dopesheet grows a row per
/// animatable property.
///
/// The node graph is the one exemption — it runs its own `ScrollArea::both`.
#[test]
fn every_content_leaf_is_kept_from_resizing_its_own_panel() {
    for editor in SWAPPABLE {
        let wrapped = editor.scroll_wrapped();
        if editor == Editor::NodeGraph {
            assert!(!wrapped, "{editor:?} scrolls itself; nesting a second area fights it");
        } else {
            assert!(wrapped, "{editor:?} would resize its panel as its content changes");
        }
    }
    // The structural leaves must stay unwrapped: the canvas measures an exact
    // rect for the vello target, and a scroll area would both offset and
    // (with a scrollbar) narrow it.
    assert!(!Editor::Canvas.scroll_wrapped(), "the canvas rect must stay exact");
    assert!(!Editor::Comp.scroll_wrapped());
    assert!(!Editor::Transport.scroll_wrapped());
}

/// The font picker's "Recent" section is a most-recently-used list: re-picking a
/// font already there moves it to the front instead of duplicating it, and the
/// list stays capped. Without the move-to-front, the section would drift into
/// "fonts I used once, ages ago".
#[test]
fn recent_fonts_are_most_recently_used_and_deduplicated() {
    let mut recent = Vec::new();
    remember_font(&mut recent, "Georgia");
    remember_font(&mut recent, "Inter");
    assert_eq!(recent, ["Inter", "Georgia"], "most recent first");

    // Re-picking an existing font moves it up rather than adding a copy.
    remember_font(&mut recent, "Georgia");
    assert_eq!(recent, ["Georgia", "Inter"], "moved to front, not duplicated");

    // "System default" isn't a font choice, so it never enters the list.
    remember_font(&mut recent, "");
    remember_font(&mut recent, "   ");
    assert_eq!(recent, ["Georgia", "Inter"], "blank family is not a recent font");

    // And the list is capped.
    for i in 0..RECENT_FONTS + 5 {
        remember_font(&mut recent, &format!("Font {i}"));
    }
    assert_eq!(recent.len(), RECENT_FONTS, "capped");
    assert_eq!(recent[0], format!("Font {}", RECENT_FONTS + 4), "newest first");
}

// --- Passepartout ----------------------------------------------------------

/// Is `p` painted by the passepartout? Must be asked with the **even-odd** rule,
/// because that is what `to_vello` fills the path with. `Shape::contains` uses
/// *nonzero*, and the two disagree exactly where it matters: the hole and the
/// outer rect wind the same way, so nonzero counts the comp interior as inside
/// (winding 2) while even-odd correctly reads it as the hole.
fn dimmed(path: &kurbo::BezPath, p: Point) -> bool {
    path.winding(p) % 2 != 0
}

/// The passepartout is a canvas-sized rect with the composition punched out of
/// it, so the dimming covers the surroundings and stops exactly at the frame.
#[test]
fn the_passepartout_covers_the_canvas_and_spares_the_comp() {
    let comp = kurbo::Rect::new(0.0, 0.0, 100.0, 50.0);
    let canvas = kurbo::Rect::new(0.0, 0.0, 400.0, 300.0);
    // Comp scaled 2x and offset, the way `fit` places it.
    let fit = Affine::translate((50.0, 40.0)) * Affine::scale(2.0);
    let path = passepartout_path(fit, comp, canvas);

    // A point well outside the frame is dimmed; the frame's centre is not.
    assert!(dimmed(&path, Point::new(10.0, 10.0)), "the surroundings are dimmed");
    assert!(
        !dimmed(&path, Point::new(150.0, 90.0)),
        "the comp interior is spared"
    );
    // Just inside each edge of the placed comp (50,40)-(250,140) stays clear.
    for p in [
        Point::new(52.0, 90.0),
        Point::new(248.0, 90.0),
        Point::new(150.0, 42.0),
        Point::new(150.0, 138.0),
    ] {
        assert!(!dimmed(&path, p), "{p:?} is inside the frame");
    }
    // And just outside each of those edges is dimmed.
    for p in [
        Point::new(48.0, 90.0),
        Point::new(252.0, 90.0),
        Point::new(150.0, 38.0),
        Point::new(150.0, 142.0),
    ] {
        assert!(dimmed(&path, p), "{p:?} is outside the frame");
    }
}

/// Zoomed in past the edges of the preview, the comp is *larger* than the
/// canvas. The hole must still be a hole: if the outer rect didn't grow to
/// contain it, even-odd would invert and dim the frame instead of its
/// surroundings — the exact opposite of the feature.
#[test]
fn a_comp_larger_than_the_canvas_does_not_invert_the_passepartout() {
    let comp = kurbo::Rect::new(0.0, 0.0, 100.0, 100.0);
    let canvas = kurbo::Rect::new(0.0, 0.0, 200.0, 200.0);
    // 4x zoom: the comp covers (-100,-100)-(300,300), swallowing the canvas.
    let fit = Affine::translate((-100.0, -100.0)) * Affine::scale(4.0);
    let path = passepartout_path(fit, comp, canvas);

    for p in [
        Point::new(100.0, 100.0),
        Point::new(5.0, 5.0),
        Point::new(195.0, 195.0),
    ] {
        assert!(
            !dimmed(&path, p),
            "{p:?} is inside the comp, so nothing visible should be dimmed"
        );
    }
}

// --- Motion path -----------------------------------------------------------

/// A project with one layer moving from (0,0) at frame 0 to (100,200) at 100.
fn moving_project() -> (MProject, NodeId, CompId) {
    use motion_core::value::{Keyframe, Track};
    // `set_at` overwrites a constant rather than promoting it, so build the
    // track directly — the same idiom `comp_at_fps` uses.
    let mut layer = MNode::group(1, "mover");
    layer.transform.position = Value::Keyframed(Track::new(vec![
        Keyframe::linear(0, Vec2::new(0.0, 0.0)),
        Keyframe::linear(100, Vec2::new(100.0, 200.0)),
    ]));
    // Deliberately a bare group with no shape: a null/group draws nothing and
    // so has no `RenderItem`, and it is exactly the sort of layer you animate
    // and want a path for.
    let comp = Comp::new(640.0, 480.0, MNode::group(0, "root").with_child(layer));
    let project = MProject::single(comp);
    let root = project.root;
    (project, NodeId(1), root)
}

/// The path samples the window around the playhead, one point per frame, and
/// its endpoints match the keyframed positions.
#[test]
fn the_motion_path_samples_a_window_around_the_playhead() {
    let (project, node, comp) = moving_project();
    let mut path = MotionPath::default();
    assert!(path.cache(&project, comp, node, 50, 10, 0), "first build");

    assert_eq!(path.points.len(), 21, "±10 frames inclusive");
    assert_eq!(path.first_frame, 40);
    // Frame 50 is halfway along a linear move.
    let mid = path.points[10].expect("the layer exists at the playhead");
    assert!((mid.x - 50.0).abs() < 1e-6, "x at frame 50: {}", mid.x);
    assert!((mid.y - 100.0).abs() < 1e-6, "y at frame 50: {}", mid.y);
}

/// The window clamps to the composition — it must not sample negative frames
/// or run past the end, which would either panic or invent trajectory.
#[test]
fn the_motion_path_window_clamps_to_the_comp() {
    let (project, node, comp) = moving_project();
    let mut path = MotionPath::default();
    path.cache(&project, comp, node, 0, 60, 0);
    assert_eq!(path.first_frame, 0, "never samples before frame 0");
    assert!(
        path.points.len() <= 61,
        "half the window is clipped away, got {}",
        path.points.len()
    );
}

/// The cache is the whole reason this is affordable: each sample is a full
/// scene evaluation, so an unchanged key must not rebuild, and a document
/// revision must.
#[test]
fn the_motion_path_rebuilds_only_when_its_key_changes() {
    let (project, node, comp) = moving_project();
    let mut path = MotionPath::default();

    assert!(path.cache(&project, comp, node, 50, 10, 0), "first build");
    assert!(!path.cache(&project, comp, node, 50, 10, 0), "identical key reuses");
    assert!(path.cache(&project, comp, node, 51, 10, 0), "playhead moved");
    assert!(path.cache(&project, comp, node, 51, 20, 0), "range changed");
    assert!(path.cache(&project, comp, node, 51, 20, 1), "document changed");
    assert!(!path.cache(&project, comp, node, 51, 20, 1), "and settles again");
}

/// Keyframed samples are flagged so they can be drawn larger, and only those
/// inside the window are listed.
#[test]
fn the_motion_path_flags_its_keyframes() {
    let (project, node, comp) = moving_project();
    let mut path = MotionPath::default();

    path.cache(&project, comp, node, 0, 10, 0);
    assert_eq!(path.keys, vec![0], "frame 0's key, at index 0");

    // A window containing neither key.
    path.cache(&project, comp, node, 50, 10, 0);
    assert!(path.keys.is_empty(), "no key between frames 40 and 60");
}

/// `segments` breaks the polyline wherever the layer doesn't exist, rather
/// than drawing a straight line across the gap to somewhere it never was. A
/// lone visible point is not a segment — there is nothing to connect it to.
#[test]
fn the_motion_path_breaks_where_the_layer_is_absent() {
    let mut path = MotionPath::default();
    path.points = vec![
        Some(Point::new(0.0, 0.0)),
        Some(Point::new(1.0, 1.0)),
        None,
        Some(Point::new(5.0, 5.0)),
        Some(Point::new(6.0, 6.0)),
        Some(Point::new(7.0, 7.0)),
        None,
        Some(Point::new(9.0, 9.0)),
    ];
    let segs = path.segments();
    assert_eq!(segs.len(), 2, "two runs, and the lone tail point is dropped");
    assert_eq!(segs[0].len(), 2);
    assert_eq!(segs[1].len(), 3);
}

/// End-to-end: a bare **group** now gets a gizmo. It draws nothing, so it has
/// no `RenderItem` and used to be skipped entirely — but a null you parent
/// things to is exactly what you want handles on. The target is built from
/// `Scene::places`, and its pivot must land on the same point the scene
/// reports, or the handles would sit away from the layer.
#[test]
fn a_bare_group_gets_a_gizmo_target_from_its_place() {
    let (project, node, comp) = moving_project();
    let scene = evaluate_comp(&project, comp, 50.0);
    assert!(scene.items.is_empty(), "the fixture is a bare group");

    let c = project.comps.get(&comp).expect("the comp");
    let n = c.root.find(node).expect("the group");
    let info = NodeInfo::resolve(n, c, 50.0);
    let place = scene.place(node).expect("a group has a place");

    let target = GizmoTarget::new(node.0, place.world, &info);
    let origin = target.parent * Point::new(target.pos.x, target.pos.y);
    let pivot = scene.pivot(node).expect("and a pivot");
    assert!(
        (origin - pivot).hypot() < 1e-6,
        "gizmo origin {origin:?} should sit on the pivot {pivot:?}"
    );
    // Halfway along the linear move, so both agree with the animation too.
    assert!((pivot.x - 50.0).abs() < 1e-6 && (pivot.y - 100.0).abs() < 1e-6, "{pivot:?}");
}

/// A layer outside its time window has no place, so it gets no gizmo — the
/// handles must not linger over a layer that isn't on screen.
#[test]
fn a_layer_outside_its_window_has_no_place_to_hang_a_gizmo_on() {
    use motion_core::node::LayerTiming;
    let (mut project, node, comp) = moving_project();
    project
        .comp_mut(comp)
        .expect("the comp")
        .root
        .find_mut(node)
        .expect("the group")
        .timing = Some(LayerTiming { start: 0, in_: 10, out: 20 });

    assert!(evaluate_comp(&project, comp, 15.0).place(node).is_some(), "inside");
    assert!(evaluate_comp(&project, comp, 50.0).place(node).is_none(), "outside");
}

// --- Alignment aids --------------------------------------------------------

/// Ruler ticks land on round numbers at any zoom — the same contract the
/// timeline ruler has. The step is the smallest 1/2/5×10ⁿ whose on-screen gap
/// clears the minimum, so labels never drift into 37s and 74s.
#[test]
fn ruler_ticks_land_on_round_numbers_at_every_zoom() {
    for scale in [0.05, 0.1, 0.37, 1.0, 2.5, 8.0, 64.0] {
        let step = ruler_step(scale, 60.0);
        assert!(step > 0.0 && step.is_finite(), "scale {scale} gave {step}");
        assert!(
            step * scale >= 60.0,
            "scale {scale}: step {step} is denser than the 60px minimum"
        );
        // The mantissa must be one of 1, 2, 5 (or 10, which is 1 of the next
        // decade) — never an arbitrary value.
        let mantissa = step / 10f64.powf(step.log10().floor());
        assert!(
            [1.0, 2.0, 5.0].iter().any(|m| (mantissa - m).abs() < 1e-9),
            "scale {scale}: step {step} has mantissa {mantissa}"
        );
    }
}

/// A degenerate scale must not produce a zero or non-finite step: the ruler
/// advances its tick loop by it, so either would hang the editor.
#[test]
fn a_degenerate_scale_cannot_hang_the_ruler() {
    for scale in [0.0, -1.0, f64::NAN, f64::INFINITY] {
        let step = ruler_step(scale, 60.0);
        assert!(step > 0.0 && step.is_finite(), "scale {scale} gave {step}");
    }
}

/// Rulers take their band out of the canvas rect, because that rect feeds the
/// fit transform and therefore click-picking. If the inset were applied only
/// when painting, every click under a ruler would select geometry hidden
/// behind it.
#[test]
fn rulers_only_claim_space_when_they_are_shown() {
    assert_eq!(ruler_inset(false), (0.0, 0.0));
    let (l, t) = ruler_inset(true);
    assert!(l > 0.0 && t > 0.0, "rulers must claim space on both axes");
}

/// Guides are grabbed by proximity to their line, within a few points either
/// side. The band has to be wide enough to hit but narrow enough that two
/// nearby guides stay separable — and the *nearest* one wins.
#[test]
fn a_guide_is_grabbed_from_a_narrow_band_around_its_line() {
    let fit = Affine::IDENTITY;
    let guides = vec![
        Guide { axis: GuideAxis::Vertical, at: 100.0 },
        Guide { axis: GuideAxis::Horizontal, at: 50.0 },
    ];
    // Dead on the vertical guide, and just off it.
    assert_eq!(guide_under(&guides, fit, 1.0, egui::pos2(100.0, 200.0)), Some(0));
    assert_eq!(guide_under(&guides, fit, 1.0, egui::pos2(102.0, 200.0)), Some(0));
    assert_eq!(guide_under(&guides, fit, 1.0, egui::pos2(140.0, 200.0)), None);
    // The horizontal one is matched on y, whatever x is.
    assert_eq!(guide_under(&guides, fit, 1.0, egui::pos2(999.0, 50.0)), Some(1));
    // Nowhere near either.
    assert_eq!(guide_under(&guides, fit, 1.0, egui::pos2(400.0, 400.0)), None);
}

/// The grab band is in *screen* points, so zooming out must not make guides
/// harder to grab: a guide 40 comp-units away is far off screen at 0.1x, and
/// one 2 comp-units away is a long way off at 8x.
#[test]
fn the_guide_grab_band_follows_the_screen_not_the_composition() {
    let guides = vec![Guide { axis: GuideAxis::Vertical, at: 100.0 }];
    // Zoomed out 10x: the guide sits at screen x = 10.
    let out = Affine::scale(0.1);
    assert_eq!(guide_under(&guides, out, 1.0, egui::pos2(10.0, 0.0)), Some(0));
    assert_eq!(guide_under(&guides, out, 1.0, egui::pos2(30.0, 0.0)), None);
    // Zoomed in 8x: the guide sits at screen x = 800, and 2 comp-units away
    // (screen 816) is now well outside the band.
    let inn = Affine::scale(8.0);
    assert_eq!(guide_under(&guides, inn, 1.0, egui::pos2(800.0, 0.0)), Some(0));
    assert_eq!(guide_under(&guides, inn, 1.0, egui::pos2(816.0, 0.0)), None);
}

// --- Snapping --------------------------------------------------------------

/// Snap a bare pivot with no bounds and no sibling layers — the shape the
/// original pivot-only tests were written against.
fn snap_pivot(p: Point, aids: &ViewAids, comp: (f64, f64), tol: f64) -> Snap {
    snap_point(p, None, SnapWorld { aids, comp, others: &[] }, tol)
}

fn aids_with(grid: bool, guides: Vec<Guide>) -> ViewAids {
    ViewAids {
        grid: Grid { visible: grid, spacing: 100.0, subdivisions: 4 },
        rulers: false,
        guides: Guides { visible: true, items: guides },
        snap: true,
        onion: Onion::default(),
    }
}

/// The composition's edges and centre always snap, whether or not any aid is
/// switched on — they exist regardless, and are what you align to most.
#[test]
fn the_comp_edges_and_centre_always_snap() {
    let aids = aids_with(false, Vec::new());
    let comp = (1920.0, 1080.0);

    let s = snap_pivot(Point::new(4.0, 540.0 - 3.0), &aids, comp, 8.0);
    assert_eq!(s.x.map(|a| a.target), Some(0.0), "left edge");
    assert_eq!(s.y.map(|a| a.target), Some(540.0), "vertical centre");

    let s = snap_pivot(Point::new(1918.0, 1078.0), &aids, comp, 8.0);
    assert_eq!(s.x.map(|a| a.target), Some(1920.0), "right edge");
    assert_eq!(s.y.map(|a| a.target), Some(1080.0), "bottom edge");

    // Well away from anything: no pull at all.
    let s = snap_pivot(Point::new(700.0, 300.0), &aids, comp, 8.0);
    assert_eq!(s, Snap::default());
}

/// Each axis decides independently: a drag can land on a vertical guide while
/// its Y stays exactly where the pointer put it.
#[test]
fn the_two_axes_snap_independently() {
    let aids = aids_with(false, vec![Guide { axis: GuideAxis::Vertical, at: 300.0 }]);
    let s = snap_pivot(Point::new(302.0, 777.0), &aids, (1920.0, 1080.0), 8.0);
    assert_eq!(s.x.map(|a| a.target), Some(300.0));
    assert!(s.y.is_none(), "y had nothing near it and must stay free");
    assert_eq!(s.offset(), Vec2::new(-2.0, 0.0));
}

/// You snap to what you can see: a hidden grid or hidden guides must not pull a
/// drag, or it reads as the cursor sticking for no reason.
#[test]
fn hidden_aids_do_not_snap() {
    let comp = (1000.0, 1000.0);
    let at = Point::new(402.0, 402.0);

    let shown = aids_with(true, vec![Guide { axis: GuideAxis::Vertical, at: 400.0 }]);
    assert!(snap_pivot(at, &shown, comp, 8.0).x.is_some(), "shown guide pulls");

    let mut hidden = shown.clone();
    hidden.guides.visible = false;
    hidden.grid.visible = false;
    assert_eq!(snap_pivot(at, &hidden, comp, 8.0), Snap::default());

    // And the master switch beats everything, including the comp edges.
    let mut off = shown.clone();
    off.snap = false;
    assert_eq!(snap_pivot(Point::new(2.0, 2.0), &off, comp, 8.0), Snap::default());
}

/// Grid snapping computes the nearest multiple rather than enumerating lines,
/// so it must work far from the origin and pick minor lines too.
#[test]
fn the_grid_snaps_to_the_nearest_line_at_any_distance() {
    let aids = aids_with(true, Vec::new());
    let comp = (100_000.0, 100_000.0);

    // 100px majors: a long way out, still exact.
    let s = snap_pivot(Point::new(49_998.0, 3.0), &aids, comp, 8.0);
    assert_eq!(s.x.map(|a| a.target), Some(50_000.0));

    // 4 subdivisions of 100 = minor lines every 25.
    let s = snap_pivot(Point::new(74.0, 5_000.0), &aids, comp, 8.0);
    assert_eq!(s.x.map(|a| a.target), Some(75.0), "nearest minor line");
}

/// The nearest target wins when several are in range — a guide sitting between
/// two grid lines must not be overruled by one of them.
#[test]
fn the_nearest_snap_target_wins() {
    let aids = aids_with(true, vec![Guide { axis: GuideAxis::Vertical, at: 103.0 }]);
    let s = snap_pivot(Point::new(102.0, 500.0), &aids, (1920.0, 1080.0), 8.0);
    assert_eq!(s.x.map(|a| a.target), Some(103.0), "the guide is 1 away, the grid 2");
}

/// The tolerance is in screen points, so the same 8px pull covers a wide comp
/// range when zoomed out and a narrow one when zoomed in — zooming in is how
/// you escape a snap.
#[test]
fn the_snap_tolerance_shrinks_as_you_zoom_in() {
    let out = snap_tolerance(Affine::scale(0.1), 1.0);
    let one = snap_tolerance(Affine::scale(1.0), 1.0);
    let inn = snap_tolerance(Affine::scale(8.0), 1.0);
    assert!(out > one && one > inn, "{out} > {one} > {inn}");
    assert!((one - SNAP_PX).abs() < 1e-9, "1:1 zoom is exactly the pixel budget");
    // A degenerate transform must not produce a NaN tolerance that snaps
    // everything (or nothing) unpredictably.
    assert!(snap_tolerance(Affine::scale(0.0), 1.0).is_infinite());
}

/// An axis arrow keeps its constraint even when snapping. The correction is
/// projected onto the axis, so a drag can slide *along* the arrow onto a guide
/// but can never be pulled sideways off it — applying the raw 2D offset would
/// quietly break the one promise the arrow makes.
#[test]
fn an_axis_constrained_drag_only_snaps_along_its_axis() {
    let target = GizmoTarget {
        node: 1,
        parent: Affine::IDENTITY,
        pos: Vec2::new(0.0, 0.0),
        rot_deg: 0.0,
        scale: (1.0, 1.0),
        anchor: Vec2::ZERO,
    };
    // A vertical guide at x=300 and a horizontal one at y=300: an unconstrained
    // move near their crossing would be pulled on both axes.
    let aids = aids_with(
        false,
        vec![
            Guide { axis: GuideAxis::Vertical, at: 300.0 },
            Guide { axis: GuideAxis::Horizontal, at: 300.0 },
        ],
    );
    let ctx = SnapCtx {
        aids: &aids,
        comp: (1920.0, 1080.0),
        bounds: None,
        others: &[],
        enabled: true,
    };
    let fit = Affine::IDENTITY;
    let at = Vec2::new(302.0, 302.0);

    // Free move: both axes snap.
    let free = GizmoDrag {
        handle: GizmoHandle::Move,
        node: 1,
        start_pos: at,
        start_rot: 0.0,
        start_scale: (1.0, 1.0),
        start_anchor: Vec2::ZERO,
        grab_parent: Point::new(302.0, 302.0),
    };
    let (pos, snap) = snap_move(&target, &free, at, ctx, fit, 1.0);
    assert_eq!((pos.x, pos.y), (300.0, 300.0));
    assert!(snap.x.is_some() && snap.y.is_some());

    // Along X: x snaps, y must be left exactly where it was.
    let axis = GizmoDrag { handle: GizmoHandle::MoveAxis(GizmoAxis::X), ..free };
    let (pos, _) = snap_move(&target, &axis, at, ctx, fit, 1.0);
    assert!((pos.x - 300.0).abs() < 1e-9, "x snapped: {}", pos.x);
    assert!((pos.y - 302.0).abs() < 1e-9, "y stayed off the guide: {}", pos.y);

    // Along Y: the mirror image.
    let axis = GizmoDrag { handle: GizmoHandle::MoveAxis(GizmoAxis::Y), ..free };
    let (pos, _) = snap_move(&target, &axis, at, ctx, fit, 1.0);
    assert!((pos.x - 302.0).abs() < 1e-9, "x stayed off the guide: {}", pos.x);
    assert!((pos.y - 300.0).abs() < 1e-9, "y snapped: {}", pos.y);
}

/// Snapping happens in *composition* space but the layer's values are in its
/// parent's, so a nested layer under a scaled parent must still land exactly on
/// the guide. Snapping in parent space would mean something different at every
/// nesting depth.
#[test]
fn a_nested_layer_snaps_to_the_guide_in_composition_space() {
    // Parent is offset and scaled 2x, so 1 parent unit is 2 comp units.
    let parent = Affine::translate((100.0, 40.0)) * Affine::scale(2.0);
    let target = GizmoTarget {
        node: 1,
        parent,
        pos: Vec2::ZERO,
        rot_deg: 0.0,
        scale: (1.0, 1.0),
        anchor: Vec2::ZERO,
    };
    let aids = aids_with(false, vec![Guide { axis: GuideAxis::Vertical, at: 300.0 }]);
    let ctx = SnapCtx {
        aids: &aids,
        comp: (1920.0, 1080.0),
        bounds: None,
        others: &[],
        enabled: true,
    };

    // Parent-space x=101 maps to comp 100 + 2*101 = 302, two from the guide.
    let at = Vec2::new(101.0, 200.0);
    let drag = GizmoDrag {
        handle: GizmoHandle::Move,
        node: 1,
        start_pos: at,
        start_rot: 0.0,
        start_scale: (1.0, 1.0),
        start_anchor: Vec2::ZERO,
        grab_parent: Point::new(101.0, 200.0),
    };
    let (pos, snap) = snap_move(&target, &drag, at, ctx, Affine::IDENTITY, 1.0);

    assert_eq!(snap.x.map(|a| a.target), Some(300.0));
    // Landing on comp x=300 means parent x=100, not 300 — the parent's scale
    // and offset have to be undone for the value written to the document.
    assert!((pos.x - 100.0).abs() < 1e-9, "parent-space x: {}", pos.x);
    let comp_x = (parent * Point::new(pos.x, pos.y)).x;
    assert!((comp_x - 300.0).abs() < 1e-9, "lands on the guide in comp space");
}

/// Holding the bypass modifier must defeat snapping entirely, including the
/// always-on composition edges.
#[test]
fn the_bypass_modifier_defeats_every_snap_target() {
    let target = GizmoTarget {
        node: 1,
        parent: Affine::IDENTITY,
        pos: Vec2::ZERO,
        rot_deg: 0.0,
        scale: (1.0, 1.0),
        anchor: Vec2::ZERO,
    };
    let aids = aids_with(true, vec![Guide { axis: GuideAxis::Vertical, at: 300.0 }]);
    let at = Vec2::new(301.0, 2.0);
    let drag = GizmoDrag {
        handle: GizmoHandle::Move,
        node: 1,
        start_pos: at,
        start_rot: 0.0,
        start_scale: (1.0, 1.0),
        start_anchor: Vec2::ZERO,
        grab_parent: Point::new(301.0, 2.0),
    };
    let ctx = SnapCtx {
        aids: &aids,
        comp: (1920.0, 1080.0),
        bounds: None,
        others: &[],
        enabled: false,
    };
    let (pos, snap) = snap_move(&target, &drag, at, ctx, Affine::IDENTITY, 1.0);
    assert_eq!(pos, at, "the drag lands exactly where the pointer put it");
    assert_eq!(snap, Snap::default(), "and nothing is drawn as snapped");
}

// --- Anchor handle + selection bounds --------------------------------------

/// Dragging the anchor handle moves the pivot to the pointer **without moving
/// the layer**: position is compensated so every drawn point stays put. Moving
/// only the anchor makes the artwork jump; moving only the position leaves the
/// pivot behind. Both halves, together, or neither.
#[test]
fn dragging_the_anchor_moves_the_pivot_but_not_the_artwork() {
    // A rotated, non-uniformly scaled layer — the case where a naive
    // "anchor += delta" is visibly wrong.
    let (rot, scale) = (90.0, (2.0, 4.0));
    let d = GizmoDrag {
        handle: GizmoHandle::Anchor,
        node: 1,
        start_pos: Vec2::new(100.0, 50.0),
        start_rot: rot,
        start_scale: scale,
        start_anchor: Vec2::new(7.0, -3.0),
        grab_parent: Point::new(100.0, 50.0),
    };
    let delta = Vec2::new(20.0, -12.0);
    let r = resolve_drag(&d, Point::new(100.0 + delta.x, 50.0 + delta.y));

    // The pivot follows the pointer exactly.
    assert!((r.pos - (d.start_pos + delta)).hypot() < 1e-9, "pivot: {:?}", r.pos);

    // And the layer doesn't move: check a local point maps to the same place
    // before and after, through the real local matrix.
    let local = |pos: Vec2, anchor: Vec2| {
        Affine::translate(pos)
            * Affine::rotate(rot.to_radians())
            * Affine::scale_non_uniform(scale.0, scale.1)
            * Affine::translate(-anchor)
    };
    let before = local(d.start_pos, d.start_anchor);
    let after = local(r.pos, r.anchor);
    for q in [Point::ZERO, Point::new(50.0, 0.0), Point::new(-13.0, 29.0)] {
        assert!(
            ((before * q) - (after * q)).hypot() < 1e-9,
            "{q:?} moved: {:?} -> {:?}",
            before * q,
            after * q
        );
    }
}

/// A collapsed scale makes the compensation matrix singular. Inverting it would
/// put infinities into the document, so the drag holds instead.
#[test]
fn an_anchor_drag_on_a_flattened_layer_is_inert() {
    let d = GizmoDrag {
        handle: GizmoHandle::Anchor,
        node: 1,
        start_pos: Vec2::new(10.0, 10.0),
        start_rot: 0.0,
        start_scale: (0.0, 1.0),
        start_anchor: Vec2::ZERO,
        grab_parent: Point::new(10.0, 10.0),
    };
    let r = resolve_drag(&d, Point::new(60.0, 60.0));
    assert_eq!(r.pos, d.start_pos);
    assert_eq!(r.anchor, d.start_anchor);
    assert!(r.anchor.x.is_finite() && r.anchor.y.is_finite());
}

/// Selecting a group boxes **each item** in it, not one rect around the lot. A
/// union would tell you only the group's extent, which is the least
/// informative thing about it; per-item shows where the pieces actually are.
#[test]
fn the_selection_boxes_cover_each_item_in_a_group() {
    let mut a = MNode::group(1, "a");
    a.shape = Some(MShape::Rect {
        size: Value::constant(Vec2::new(100.0, 100.0)),
        radius: Value::constant(0.0),
    });
    a.fill = Some(Value::constant(MColor::rgb(1.0, 1.0, 1.0)));
    a.transform.position = Value::constant(Vec2::new(0.0, 0.0));

    let mut b = MNode::group(2, "b");
    b.shape = Some(MShape::Rect {
        size: Value::constant(Vec2::new(100.0, 100.0)),
        radius: Value::constant(0.0),
    });
    b.fill = Some(Value::constant(MColor::rgb(1.0, 1.0, 1.0)));
    b.transform.position = Value::constant(Vec2::new(200.0, 0.0));

    let group = MNode::group(3, "group").with_child(a).with_child(b);
    let comp = Comp::new(1000.0, 1000.0, MNode::group(0, "root").with_child(group));
    let project = MProject::single(comp);
    let scene = evaluate_comp(&project, project.root, 0.0);
    let root = project.comps[&project.root].root.find(NodeId(3)).unwrap();

    let boxes = selection_boxes(&scene, root);
    assert_eq!(boxes.len(), 2, "one box per drawable item, not one union");
    // Each is its own 100-wide square, centred at x=0 and x=200 — crucially
    // *not* the -50..250 union those two would collapse into.
    assert!((boxes[0].x0 - -50.0).abs() < 0.5, "{:?}", boxes[0]);
    assert!((boxes[0].x1 - 50.0).abs() < 0.5, "{:?}", boxes[0]);
    assert!((boxes[1].x0 - 150.0).abs() < 0.5, "{:?}", boxes[1]);
    assert!((boxes[1].x1 - 250.0).abs() < 0.5, "{:?}", boxes[1]);
    for r in &boxes {
        assert!((r.width() - 100.0).abs() < 0.5 && (r.height() - 100.0).abs() < 0.5);
    }
}

/// A plain single-shape layer is the common case, and there per-item and a
/// union are the same thing — exactly one box.
#[test]
fn a_single_shape_layer_gets_exactly_one_box() {
    let mut a = MNode::group(1, "a");
    a.shape = Some(MShape::Rect {
        size: Value::constant(Vec2::new(80.0, 40.0)),
        radius: Value::constant(0.0),
    });
    a.fill = Some(Value::constant(MColor::rgb(1.0, 1.0, 1.0)));
    let comp = Comp::new(500.0, 500.0, MNode::group(0, "root").with_child(a));
    let project = MProject::single(comp);
    let scene = evaluate_comp(&project, project.root, 0.0);
    let root = project.comps[&project.root].root.find(NodeId(1)).unwrap();

    let boxes = selection_boxes(&scene, root);
    assert_eq!(boxes.len(), 1);
    assert!((boxes[0].width() - 80.0).abs() < 0.5, "{:?}", boxes[0]);
    assert!((boxes[0].height() - 40.0).abs() < 0.5, "{:?}", boxes[0]);
}

/// A layer that draws nothing gets no boxes at all — better than a zero-size
/// rect at the origin, which would look like a bug.
#[test]
fn a_layer_that_draws_nothing_has_no_selection_box() {
    let (project, node, comp) = moving_project();
    let scene = evaluate_comp(&project, comp, 0.0);
    let root = project.comps[&comp].root.find(node).unwrap();
    assert!(selection_boxes(&scene, root).is_empty());
}

// --- Onion skins -----------------------------------------------------------

/// Like `moving_project`, but the layer actually draws something — ghosts are
/// geometry, so a bare group would produce none.
fn boxed_project() -> (MProject, NodeId, CompId) {
    use motion_core::value::{Keyframe, Track};
    let mut layer = MNode::group(1, "mover");
    layer.transform.position = Value::Keyframed(Track::new(vec![
        Keyframe::linear(0, Vec2::new(0.0, 0.0)),
        Keyframe::linear(200, Vec2::new(400.0, 400.0)),
    ]));
    layer.shape = Some(MShape::Rect {
        size: Value::constant(Vec2::new(50.0, 50.0)),
        radius: Value::constant(0.0),
    });
    layer.fill = Some(Value::constant(MColor::rgb(1.0, 1.0, 1.0)));
    let comp = Comp::new(640.0, 480.0, MNode::group(0, "root").with_child(layer));
    let project = MProject::single(comp);
    let root = project.root;
    (project, NodeId(1), root)
}

/// Ghosts are cached like the motion path, because each one is a full scene
/// evaluation. An unchanged key must not rebuild; a document change must.
#[test]
fn onion_skins_rebuild_only_when_their_key_changes() {
    let (project, node, comp) = boxed_project();
    let onion = Onion { visible: true, before: 2, after: 2, step: 2, opacity: 0.5 };
    let mut skins = OnionSkins::default();

    assert!(skins.cache(&project, comp, Some(node), 50, &onion, 0), "first build");
    assert!(!skins.cache(&project, comp, Some(node), 50, &onion, 0), "identical key reuses");
    assert!(skins.cache(&project, comp, Some(node), 51, &onion, 0), "playhead moved");
    assert!(skins.cache(&project, comp, None, 51, &onion, 0), "selection changed");
    assert!(skins.cache(&project, comp, None, 51, &onion, 1), "document changed");
    assert!(!skins.cache(&project, comp, None, 51, &onion, 1), "and settles again");

    // The settings are part of the key too, floats included.
    let dimmer = Onion { opacity: 0.9, ..onion.clone() };
    assert!(skins.cache(&project, comp, None, 51, &dimmer, 1), "opacity changed");
}

/// Ghosts outside the composition are **skipped, not clamped**. Clamping would
/// pile duplicates of frame 0 on top of each other, which reads as the
/// animation stalling there rather than as running out of frames.
#[test]
fn ghosts_outside_the_comp_are_skipped_rather_than_clamped() {
    let (project, node, comp) = boxed_project();
    let onion = Onion { visible: true, before: 3, after: 3, step: 10, opacity: 0.5 };
    let mut skins = OnionSkins::default();

    // At frame 0 every "before" ghost is off the start of the comp.
    skins.cache(&project, comp, Some(node), 0, &onion, 0);
    assert_eq!(skins.ghosts.len(), 3, "only the three future ghosts survive");
    for g in &skins.ghosts {
        assert!(g.tint.b > g.tint.r, "all cool-tinted, none from the past");
    }
}

/// Past ghosts run warm and future ghosts cool, so the direction of time is
/// readable without counting. The nearest is the most opaque.
#[test]
fn ghosts_are_tinted_by_direction_and_fade_with_distance() {
    let (project, node, comp) = boxed_project();
    let onion = Onion { visible: true, before: 2, after: 2, step: 2, opacity: 0.6 };
    let mut skins = OnionSkins::default();
    skins.cache(&project, comp, Some(node), 100, &onion, 0);

    assert_eq!(skins.ghosts.len(), 4);
    // Order is nearest-past, further-past, nearest-future, further-future.
    assert!(skins.ghosts[0].tint.r > skins.ghosts[0].tint.b, "past is warm");
    assert!(skins.ghosts[2].tint.b > skins.ghosts[2].tint.r, "future is cool");
    assert!(
        skins.ghosts[0].opacity > skins.ghosts[1].opacity,
        "the nearer past ghost is more solid"
    );
    assert!(
        skins.ghosts[0].opacity <= 0.6 + 1e-9,
        "and none exceeds the configured opacity"
    );
}

/// Hiding onion skins produces no ghosts, so nothing is evaluated or drawn.
#[test]
fn hidden_onion_skins_produce_no_ghosts() {
    let (project, node, comp) = boxed_project();
    let off = Onion { visible: false, before: 3, after: 3, step: 2, opacity: 0.5 };
    let mut skins = OnionSkins::default();
    skins.cache(&project, comp, Some(node), 100, &off, 0);
    assert!(skins.ghosts.is_empty());
}

/// With nothing selected the whole comp is ghosted — the "review the animation"
/// case. It costs the same, since the evaluation is whole-comp either way and
/// only the filter differs.
#[test]
fn with_no_selection_the_whole_comp_is_ghosted() {
    let (project, node, comp) = boxed_project();
    let onion = Onion { visible: true, before: 1, after: 1, step: 2, opacity: 0.5 };

    let mut selected = OnionSkins::default();
    selected.cache(&project, comp, Some(node), 100, &onion, 0);
    let mut all = OnionSkins::default();
    all.cache(&project, comp, None, 100, &onion, 0);

    assert!(!all.ghosts.is_empty());
    assert_eq!(all.ghosts.len(), selected.ghosts.len());
    assert_eq!(all.ghosts[0].items.len(), selected.ghosts[0].items.len());
}

/// The tint blends toward the direction colour but keeps some of the layer's
/// own — fully tinting would flatten a multi-coloured scene into two
/// silhouettes and lose which layer is which.
#[test]
fn a_ghost_keeps_some_of_its_own_colour() {
    let own = MColor::rgb(0.0, 1.0, 0.0);
    let tint = MColor::rgb(1.0, 0.0, 0.0);
    let out = tinted(own, tint, TINT_AMOUNT);
    assert!(out.r > 0.0 && out.r < 1.0, "pulled toward the tint: {}", out.r);
    assert!(out.g > 0.0 && out.g < 1.0, "but still recognisably green: {}", out.g);
    // Zero blend is a no-op; full blend is the tint.
    assert_eq!(tinted(own, tint, 0.0).g, 1.0);
    assert_eq!(tinted(own, tint, 1.0).r, 1.0);
}

// --- Bounding-box snapping -------------------------------------------------

/// A layer's **edge** can land on a guide, not just its pivot. This is the
/// whole point of bbox snapping: the pivot usually sits in the middle of the
/// artwork, so pivot-only snapping puts the *centre* on the margin when what
/// you asked for was the edge flush against it.
#[test]
fn a_layers_edge_snaps_to_a_guide_not_just_its_pivot() {
    let aids = aids_with(false, vec![Guide { axis: GuideAxis::Vertical, at: 300.0 }]);
    let comp = (1920.0, 1080.0);
    // A 100-wide layer centred at x=352: its left edge is at 302, two from the
    // guide, while the pivot is 52 away and far out of tolerance.
    let bounds = kurbo::Rect::new(302.0, 0.0, 402.0, 100.0);
    let pivot = Point::new(352.0, 50.0);

    let world = SnapWorld { aids: &aids, comp, others: &[] };
    let s = snap_point(pivot, Some(bounds), world, 8.0);
    assert_eq!(s.x.map(|a| a.target), Some(300.0), "the left edge caught it");
    assert_eq!(s.offset(), Vec2::new(-2.0, 0.0), "and only by the edge's gap");

    // Without bounds the same drag is out of range entirely.
    assert!(snap_pivot(pivot, &aids, comp, 8.0).x.is_none());
}

/// Edges align against *other layers*, which is what makes "line this up with
/// that" work without dropping a guide first.
#[test]
fn a_layer_snaps_to_another_layers_edge() {
    let aids = aids_with(false, Vec::new());
    let comp = (1920.0, 1080.0);
    let other = kurbo::Rect::new(800.0, 0.0, 900.0, 100.0);

    // Dragged layer's right edge is at 797, three short of the other's left.
    let bounds = kurbo::Rect::new(697.0, 0.0, 797.0, 100.0);
    let world = SnapWorld { aids: &aids, comp, others: std::slice::from_ref(&other) };
    let s = snap_point(Point::new(747.0, 50.0), Some(bounds), world, 8.0);
    assert_eq!(s.x.map(|a| a.target), Some(800.0));

    // With no siblings offered, nothing pulls it.
    let alone = SnapWorld { aids: &aids, comp, others: &[] };
    assert!(snap_point(Point::new(747.0, 50.0), Some(bounds), alone, 8.0).x.is_none());
}

/// Centres align to centres, so two layers can be lined up on their middles
/// rather than their edges.
#[test]
fn layer_centres_align_to_each_other() {
    let aids = aids_with(false, Vec::new());
    let comp = (4000.0, 4000.0);
    // A sibling spanning 1000..1200, so its centre is 1100.
    let other = kurbo::Rect::new(1000.0, 0.0, 1200.0, 50.0);
    // Dragged layer spans 1057..1097 — centre 1077, twenty-three short. Its
    // edges are far from anything, so only the centre can be the match.
    let bounds = kurbo::Rect::new(1057.0, 500.0, 1097.0, 540.0);
    let world = SnapWorld { aids: &aids, comp, others: std::slice::from_ref(&other) };
    let s = snap_point(Point::new(1077.0, 520.0), Some(bounds), world, 25.0);
    assert_eq!(s.x.map(|a| a.target), Some(1100.0), "centre to centre");
}

/// The smallest correction wins across *every* source/target pair — an edge two
/// away must beat a pivot five away, or the snap feels arbitrary.
#[test]
fn the_smallest_correction_wins_across_edges_and_pivot() {
    let aids = aids_with(
        false,
        vec![
            Guide { axis: GuideAxis::Vertical, at: 500.0 },
            Guide { axis: GuideAxis::Vertical, at: 447.0 },
        ],
    );
    let comp = (1920.0, 1080.0);
    // Pivot at 505 is 5 from the guide at 500; the left edge at 445 is 2 from
    // the guide at 447. The edge should win.
    let bounds = kurbo::Rect::new(445.0, 0.0, 565.0, 80.0);
    let world = SnapWorld { aids: &aids, comp, others: &[] };
    let s = snap_point(Point::new(505.0, 40.0), Some(bounds), world, 8.0);
    assert_eq!(s.x.map(|a| a.target), Some(447.0));
    assert_eq!(s.offset(), Vec2::new(2.0, 0.0));
}

/// A layer that draws nothing has no edges, so it falls back to pivot-only
/// snapping rather than snapping to a degenerate box at the origin.
#[test]
fn a_layer_with_no_geometry_snaps_only_its_pivot() {
    let aids = aids_with(false, vec![Guide { axis: GuideAxis::Vertical, at: 300.0 }]);
    let comp = (1920.0, 1080.0);
    let world = SnapWorld { aids: &aids, comp, others: &[] };

    assert!(snap_point(Point::new(303.0, 500.0), None, world, 8.0).x.is_some());
    assert!(snap_point(Point::new(600.0, 500.0), None, world, 8.0).x.is_none());
}

/// `Scene::places` reports every node's extent, unioned over its subtree, so a
/// group's bounds cover its children even though the group draws nothing
/// itself. Snapping reads sibling bounds straight from here, which is why it
/// has to be right for containers and not just for shapes.
#[test]
fn a_groups_bounds_cover_its_children() {
    let mut a = MNode::group(1, "a");
    a.shape = Some(MShape::Rect {
        size: Value::constant(Vec2::new(100.0, 100.0)),
        radius: Value::constant(0.0),
    });
    a.fill = Some(Value::constant(MColor::rgb(1.0, 1.0, 1.0)));

    let mut b = MNode::group(2, "b");
    b.shape = Some(MShape::Rect {
        size: Value::constant(Vec2::new(100.0, 100.0)),
        radius: Value::constant(0.0),
    });
    b.fill = Some(Value::constant(MColor::rgb(1.0, 1.0, 1.0)));
    b.transform.position = Value::constant(Vec2::new(200.0, 0.0));

    let group = MNode::group(3, "group").with_child(a).with_child(b);
    let comp = Comp::new(1000.0, 1000.0, MNode::group(0, "root").with_child(group));
    let project = MProject::single(comp);
    let scene = evaluate_comp(&project, project.root, 0.0);

    let g = scene.place(NodeId(3)).expect("the group has a place");
    let bounds = g.bounds.expect("and, through its children, an extent");
    assert!((bounds.x0 - -50.0).abs() < 0.5, "x0 {}", bounds.x0);
    assert!((bounds.x1 - 250.0).abs() < 0.5, "x1 {}", bounds.x1);

    // Its own children still report their own, narrower boxes.
    let child = scene.place(NodeId(1)).and_then(|p| p.bounds).expect("child bounds");
    assert!((child.x1 - 50.0).abs() < 0.5, "the child is not widened by its sibling");
}

/// A node that draws nothing anywhere in its subtree has no extent at all —
/// better than a zero-size box at the origin, which would act as a phantom snap
/// target sitting in the corner of every composition.
#[test]
fn a_subtree_that_draws_nothing_has_no_extent() {
    let (project, node, comp) = moving_project();
    let scene = evaluate_comp(&project, comp, 0.0);
    assert!(scene.place(node).expect("still has a place").bounds.is_none());
}

/// A dragged layer must not be offered its own ancestors as snap targets. A
/// group's extent is the union of its children's, so an ancestor's box contains
/// the dragged layer and moves with it — snapping to one pins the drag against
/// a target that runs away from it. The root is an ancestor of everything, so
/// excluding ancestors excludes it for free.
#[test]
fn a_drag_is_never_offered_its_own_subtree_or_ancestors() {
    let leaf = MNode::group(3, "leaf");
    let inner = MNode::group(2, "inner").with_child(leaf);
    let sibling = MNode::group(4, "sibling");
    let root = MNode::group(0, "root").with_child(inner).with_child(sibling);

    let excluded = snap_excluded(&root, NodeId(2));
    for id in [NodeId(0), NodeId(2), NodeId(3)] {
        assert!(excluded.contains(&id), "{id:?} must be excluded");
    }
    assert!(!excluded.contains(&NodeId(4)), "a sibling is a legitimate target");

    // Dragging the leaf excludes it and both containers, but still not the
    // sibling branch.
    let excluded = snap_excluded(&root, NodeId(3));
    for id in [NodeId(0), NodeId(2), NodeId(3)] {
        assert!(excluded.contains(&id), "{id:?} must be excluded");
    }
    assert!(!excluded.contains(&NodeId(4)));
}

// ── Geometry drivers: the graph authoring a layer's shape. ───────────────────

/// A project holding one empty group layer, ready to be given a shape.
fn shape_driver_project() -> (MProject, CompId, NodeId) {
    let target = MNode::group(1, "target");
    let comp = Comp::new(200.0, 200.0, MNode::group(0, "root").with_child(target));
    let project = MProject::single(comp);
    let root = project.root;
    (project, root, NodeId(1))
}

/// The headline of shape lowering: a rectangle node's `geometry` output, bound
/// to a layer, *makes* that layer a rectangle — kind and params both — and a
/// wire into one of its params animates it. The layer starts with no shape at
/// all, so nothing but the graph could have put one there.
#[test]
fn a_geometry_driver_authors_a_layers_shape() {
    let (mut project, comp, target) = shape_driver_project();
    let reg = NodeRegistry::with_builtins();
    let rect = project.graph.add_node("rect", Vec2::new(0.0, 0.0));
    let ramp = project.graph.add_node("ramp", Vec2::new(-200.0, 0.0));
    // ramp 0 → 40 over frames 0..10, into the corner radius.
    project.graph.node_mut(ramp).unwrap().set_value("to", ExprValue::Num(40.0));
    project.graph.node_mut(ramp).unwrap().set_value("end", ExprValue::Num(10.0));
    project
        .graph
        .connect(&GraphCtx::bare(&reg), Endpoint::new(ramp, "value"), Endpoint::new(rect, "radius"))
        .unwrap();
    project
        .shape_bindings
        .push(ShapeBinding { output: Endpoint::new(rect, "geometry"), target });

    compile_drivers(&mut project, &reg, comp);

    let node = project.comp(comp).unwrap().root.find(target).unwrap();
    let Some(MShape::Rect { radius, .. }) = &node.shape else {
        panic!("the driver should have made it a rect, got {:?}", node.shape)
    };
    assert!(radius.is_expr(), "a graph-authored param is expression-driven");
    // The animation reaches the rendered frame, not just the model.
    let bounds = |f: f64| evaluate_comp(&project, comp, f).place(target).unwrap().bounds;
    assert!(bounds(0.0).is_some(), "the layer draws once the graph gave it a shape");
    // A 200×200 square with a growing corner radius loses area at the corners,
    // so the path changes with the frame — proof the wire is live.
    let ctx_radius = |f: f64| {
        let n = project.comp(comp).unwrap().root.find(target).unwrap();
        let Some(MShape::Rect { radius, .. }) = &n.shape else { unreachable!() };
        radius.resolve(&mut EvalCtx::new(project.comp(comp).unwrap(), f))
    };
    assert_eq!(ctx_radius(0.0), 0.0);
    assert_eq!(ctx_radius(10.0), 40.0);
}

/// A **value** driver on `size` still wins over the geometry driver that made
/// the shape: the geometry pass decides the kind, the property pass then
/// overrides that one param. The other order would silently discard it.
#[test]
fn a_property_driver_overrides_one_param_of_a_graph_authored_shape() {
    let (mut project, comp, target) = shape_driver_project();
    let reg = NodeRegistry::with_builtins();
    let rect = project.graph.add_node("rect", Vec2::new(0.0, 0.0));
    let v = project.graph.add_node("value", Vec2::new(-200.0, 0.0));
    project.graph.node_mut(v).unwrap().set_value("value", ExprValue::Vec2(Vec2::new(50.0, 50.0)));
    project
        .shape_bindings
        .push(ShapeBinding { output: Endpoint::new(rect, "geometry"), target });
    project.bindings.push(Binding {
        output: Endpoint::new(v, "value"),
        target,
        prop: PropPath::ShapeSize,
    });

    compile_drivers(&mut project, &reg, comp);

    let node = project.comp(comp).unwrap().root.find(target).unwrap();
    let Some(MShape::Rect { size, .. }) = &node.shape else { panic!("{:?}", node.shape) };
    let got = size.resolve(&mut EvalCtx::new(project.comp(comp).unwrap(), 0.0));
    assert_eq!(got, Vec2::new(50.0, 50.0), "the property driver's size, not the rect node's 200");
}

/// A geometry driver whose output isn't a shape node's geometry (a stale
/// binding left pointing at a math node) leaves the layer's own shape alone
/// rather than blanking it.
#[test]
fn a_stale_geometry_driver_leaves_the_shape_untouched() {
    let (mut project, comp, target) = shape_driver_project();
    let reg = NodeRegistry::with_builtins();
    // Give the layer a hand-made ellipse first.
    project.comp_mut(comp).unwrap().root.find_mut(target).unwrap().shape =
        Some(MShape::Ellipse { size: Value::constant(Vec2::new(10.0, 10.0)) });
    let add = project.graph.add_node("add", Vec2::new(0.0, 0.0));
    project
        .shape_bindings
        .push(ShapeBinding { output: Endpoint::new(add, "geometry"), target });

    compile_drivers(&mut project, &reg, comp);

    let node = project.comp(comp).unwrap().root.find(target).unwrap();
    assert!(matches!(node.shape, Some(MShape::Ellipse { .. })), "{:?}", node.shape);
}

// ── Module scope: the canvas authoring a shared module's body. ───────────────

/// Opening a module whose canvas is empty **seeds it from the body**, so an
/// existing module becomes node-editable without a migration — and lowering it
/// straight back reproduces the body it came from, rather than replacing a
/// working module with a blank sheet.
#[test]
fn opening_a_module_seeds_its_canvas_from_its_body() {
    let reg = NodeRegistry::with_builtins();
    let body = Expr::Mul(
        Box::new(Expr::Time(motion_core::expr::TimeSource::T01)),
        Box::new(Expr::Lit(ExprValue::Num(90.0))),
    );
    let mut modules = std::collections::BTreeMap::new();
    modules.insert(ModuleId(1), MModule::new("spin", body.clone()));

    // What `App::open_module` does, without a window.
    let snapshot = modules.clone();
    let ctx = GraphCtx::new(&reg, &snapshot);
    let m = modules.get_mut(&ModuleId(1)).unwrap();
    let output = motion_core::raise(&mut m.graph, &ctx, &body, Vec2::new(40.0, 40.0));
    m.output = Some(output);
    assert!(!modules[&ModuleId(1)].graph.nodes.is_empty(), "the body became nodes");

    // Recompiling from the seeded canvas gives back the same body.
    compile_modules(&mut modules, &reg);
    assert_eq!(modules[&ModuleId(1)].body.to_string(), body.to_string());
}

/// A module knob added in module scope becomes an input socket on every `use`
/// node linking it — the descriptor seam and the document scope meeting. This
/// is what makes an override wireable at all.
#[test]
fn a_module_knob_becomes_a_socket_on_its_links() {
    let reg = NodeRegistry::with_builtins();
    let mut modules = std::collections::BTreeMap::new();
    modules.insert(ModuleId(1), MModule::new("spin", Expr::Lit(ExprValue::Num(0.0))));

    let mut g = NodeGraph::new();
    let u = g.add_node("use", Vec2::ZERO);
    g.node_mut(u).unwrap().config.module = Some(ModuleId(1));
    assert!(
        GraphCtx::new(&reg, &modules).descriptor_for(g.node(u).unwrap()).unwrap().inputs.is_empty(),
        "no knobs yet"
    );

    // Add one, the way `NgModuleOp::AddKnob` does.
    modules
        .get_mut(&ModuleId(1))
        .unwrap()
        .set_param("speed", ParamValue::Num(Value::constant(0.0)));
    let ctx = GraphCtx::new(&reg, &modules);
    let desc = ctx.descriptor_for(g.node(u).unwrap()).unwrap();
    assert!(desc.find_input("speed").is_some(), "the knob is a socket on the link");
}

// ── Layer exposed knobs: one recipe fitting many layers. ────────────────────

/// A placed layer's rotation in degrees, read out of its world matrix — the
/// angle that actually reached the frame, rather than the recipe that made it.
fn placed_angle_deg(scene: &MScene, id: NodeId) -> f64 {
    let c = scene.place(id).expect("the layer is placed").world.as_coeffs();
    c[1].atan2(c[0]).to_degrees()
}

/// The point of a layer knob: **one** graph output drives several layers, each
/// at its own value, because a `param` node lowers to a node-*relative*
/// `Expr::Param` that resolves against whichever layer the driver points at.
/// Two layers, two knob values, one recipe.
#[test]
fn one_param_recipe_drives_two_layers_at_their_own_knob_values() {
    let mut a = MNode::group(1, "a");
    let mut b = MNode::group(2, "b");
    a.set_param("gain", ParamValue::Num(Value::constant(10.0)));
    b.set_param("gain", ParamValue::Num(Value::constant(40.0)));
    let comp = Comp::new(64.0, 64.0, MNode::group(0, "root").with_child(a).with_child(b));
    let mut project = MProject::single(comp);
    let comp_id = project.root;
    let reg = NodeRegistry::with_builtins();

    // One `param("gain")` node, bound to both layers' rotation.
    let p = project.graph.add_node("param", Vec2::ZERO);
    project.graph.node_mut(p).unwrap().config.param = "gain".into();
    for target in [NodeId(1), NodeId(2)] {
        project.bindings.push(Binding {
            output: Endpoint::new(p, "value"),
            target,
            prop: PropPath::Rotation,
        });
    }
    compile_drivers(&mut project, &reg, comp_id);

    // Read it back through `evaluate_comp`, not off the property: a `param`
    // node lowers to a node-*relative* read, which only resolves inside the
    // walk that knows which layer it is evaluating.
    let scene = evaluate_comp(&project, comp_id, 0.0);
    assert!((placed_angle_deg(&scene, NodeId(1)) - 10.0).abs() < 1e-6);
    assert!(
        (placed_angle_deg(&scene, NodeId(2)) - 40.0).abs() < 1e-6,
        "the same recipe reads each layer's own knob"
    );
}

/// Removing a knob a `param` node still reads doesn't break the frame: the read
/// warns and falls back to zero, the same warn-don't-fail contract a dangling
/// reference follows.
#[test]
fn removing_a_knob_a_param_still_reads_falls_back_instead_of_failing() {
    let mut layer = MNode::group(1, "a");
    layer.set_param("gain", ParamValue::Num(Value::constant(7.0)));
    let comp = Comp::new(64.0, 64.0, MNode::group(0, "root").with_child(layer));
    let mut project = MProject::single(comp);
    let comp_id = project.root;
    let reg = NodeRegistry::with_builtins();

    let p = project.graph.add_node("param", Vec2::ZERO);
    project.graph.node_mut(p).unwrap().config.param = "gain".into();
    project.bindings.push(Binding {
        output: Endpoint::new(p, "value"),
        target: NodeId(1),
        prop: PropPath::Rotation,
    });
    compile_drivers(&mut project, &reg, comp_id);

    let scene = evaluate_comp(&project, comp_id, 0.0);
    assert!((placed_angle_deg(&scene, NodeId(1)) - 7.0).abs() < 1e-6);

    // Pull the knob out from under it.
    project.comp_mut(comp_id).unwrap().root.find_mut(NodeId(1)).unwrap().remove_param("gain");
    compile_drivers(&mut project, &reg, comp_id);
    let scene = evaluate_comp(&project, comp_id, 0.0);
    assert!(
        placed_angle_deg(&scene, NodeId(1)).abs() < 1e-6,
        "a gone knob reads neutral, it doesn't panic"
    );
    assert!(
        scene.warnings.iter().any(|(_, w)| w.contains("gain")),
        "and it says so: {:?}",
        scene.warnings
    );
}

// ── Module semantics, ported from the retired expression panel. ─────────────
//
// These covered `GraphOp::{RenameModule, DeleteModule, SetModule}` and body
// editing. The capabilities didn't go away with that panel — they moved to the
// Nodes panel's module scope — so their guarantees are re-pinned here against
// the node path that owns them now.

/// A module with one knob and a body reading it, linked from a graph.
fn linked_module_project() -> (MProject, NodeRegistry, ModuleId, GraphNodeId) {
    let comp = Comp::new(64.0, 64.0, MNode::group(0, "root").with_child(MNode::group(1, "a")));
    let mut project = MProject::single(comp);
    let reg = NodeRegistry::with_builtins();
    let mut m = MModule::new("spin", Expr::Lit(ExprValue::Num(0.0)));
    m.set_param("amp", ParamValue::Num(Value::constant(3.0)));
    let p = m.graph.add_node("param", Vec2::ZERO);
    m.graph.node_mut(p).unwrap().config.param = "amp".into();
    m.output = Some(Endpoint::new(p, "value"));
    let id = project.add_module(m);
    compile_modules(&mut project.modules, &reg);

    let u = project.graph.add_node("use", Vec2::ZERO);
    project.graph.node_mut(u).unwrap().config.module = Some(id);
    project.bindings.push(Binding {
        output: Endpoint::new(u, "value"),
        target: NodeId(1),
        prop: PropPath::Rotation,
    });
    let comp_id = project.root;
    compile_drivers(&mut project, &reg, comp_id);
    (project, reg, id, u)
}

/// Renaming a module keeps its links: a link names the module by **id**, so the
/// label is free to change under it.
#[test]
fn renaming_a_module_keeps_its_links() {
    let (mut project, _reg, id, _u) = linked_module_project();
    let before = placed_angle_deg(&evaluate_comp(&project, project.root, 0.0), NodeId(1));
    project.modules.get_mut(&id).unwrap().name = "renamed".into();
    let after = placed_angle_deg(&evaluate_comp(&project, project.root, 0.0), NodeId(1));
    assert!((before - after).abs() < 1e-9, "the link followed the rename");
    assert!((after - 3.0).abs() < 1e-9, "and still runs the body");
}

/// Deleting a module leaves its links **warning and falling back**, not
/// crashing — the same contract a dangling reference follows everywhere else.
#[test]
fn deleting_a_module_leaves_its_links_warning() {
    let (mut project, _reg, id, _u) = linked_module_project();
    project.modules.remove(&id);
    let scene = evaluate_comp(&project, project.root, 0.0);
    assert!(placed_angle_deg(&scene, NodeId(1)).abs() < 1e-9, "falls back to neutral");
    assert!(
        scene.warnings.iter().any(|(_, w)| w.contains("no longer exists")),
        "and says so: {:?}",
        scene.warnings
    );
}

/// Editing a module's body on its canvas drives **every** link — the whole
/// reason a module exists rather than copying a recipe per property.
#[test]
fn editing_a_module_body_drives_every_link() {
    let (mut project, reg, id, _u) = linked_module_project();
    // A second layer, linked to the same module.
    project.comp_mut(project.root).unwrap().root.children.push(MNode::group(2, "b"));
    let u2 = project.graph.add_node("use", Vec2::new(0.0, 200.0));
    project.graph.node_mut(u2).unwrap().config.module = Some(id);
    project.bindings.push(Binding {
        output: Endpoint::new(u2, "value"),
        target: NodeId(2),
        prop: PropPath::Rotation,
    });
    let comp_id = project.root;
    compile_drivers(&mut project, &reg, comp_id);

    // Rebuild the body on the module's own canvas: param(amp) * 10.
    {
        let m = project.modules.get_mut(&id).unwrap();
        let p = m.output.clone().unwrap().node;
        let ten = m.graph.add_node("value", Vec2::new(0.0, 60.0));
        m.graph.node_mut(ten).unwrap().set_value("value", ExprValue::Num(10.0));
        let mul = m.graph.add_node("mul", Vec2::new(200.0, 0.0));
        let ctx = &GraphCtx::bare(&reg);
        m.graph.connect(ctx, Endpoint::new(p, "value"), Endpoint::new(mul, "a")).unwrap();
        m.graph.connect(ctx, Endpoint::new(ten, "value"), Endpoint::new(mul, "b")).unwrap();
        m.output = Some(Endpoint::new(mul, "result"));
    }
    compile_modules(&mut project.modules, &reg);
    compile_drivers(&mut project, &reg, comp_id);

    let scene = evaluate_comp(&project, comp_id, 0.0);
    for id in [NodeId(1), NodeId(2)] {
        assert!(
            (placed_angle_deg(&scene, id) - 30.0).abs() < 1e-9,
            "{id:?} should follow the edited body"
        );
    }
}

/// Re-pointing a link at a different module keeps the overrides whose knob
/// names the new module also has, and drops the rest — an override is keyed by
/// name, so it applies wherever that name means something.
#[test]
fn repointing_a_link_keeps_overrides_that_still_apply() {
    let (mut project, reg, _id, u) = linked_module_project();
    project.graph.node_mut(u).unwrap().set_value("amp", ExprValue::Num(9.0));
    project.graph.node_mut(u).unwrap().set_value("gone", ExprValue::Num(1.0));

    // A second module sharing the `amp` knob but not `gone`.
    let mut other = MModule::new("other", Expr::Lit(ExprValue::Num(0.0)));
    other.set_param("amp", ParamValue::Num(Value::constant(1.0)));
    let p = other.graph.add_node("param", Vec2::ZERO);
    other.graph.node_mut(p).unwrap().config.param = "amp".into();
    other.output = Some(Endpoint::new(p, "value"));
    let other_id = project.add_module(other);
    compile_modules(&mut project.modules, &reg);

    project.graph.node_mut(u).unwrap().config.module = Some(other_id);
    let ctx = GraphCtx::new(&reg, &project.modules);
    let expr = lower_output(&project.graph, &ctx, &Endpoint::new(u, "value"));
    let Expr::Use { overrides, .. } = &expr else { panic!("{expr:?}") };
    assert_eq!(overrides.len(), 1, "only knobs the new module has: {overrides:?}");
    assert_eq!(overrides[0].0, "amp");
}
