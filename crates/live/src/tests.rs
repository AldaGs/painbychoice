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
    let scene = evaluate(&doc, 0.0);
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
    let scene = evaluate(&doc, 0.0);
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
    let project = Project {
        document: demo_document(),
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
    let back: Project = serde_json::from_str(&json).unwrap();
    assert_eq!(dock_editors(&back.layout.dock.unwrap()), dock_editors(&dock));
    assert_eq!(back.layout.user_presets.len(), 1);
    assert_eq!(back.layout.user_presets[0].name, "Mine");
    assert!(!back.layout.user_presets[0].builtin, "builtin is skipped → false on load");
}

#[test]
fn a_bare_document_file_still_loads() {
    // Old `.pbc` files are a bare Document with no layout wrapper; the loader
    // tries Project first (which lacks a `document` field here and fails),
    // then falls back to the plain document parse.
    let json = serde_json::to_string(&demo_document()).unwrap();
    assert!(serde_json::from_str::<Project>(&json).is_err(), "no `document` field");
    assert!(serde_json::from_str::<Document>(&json).is_ok(), "parses as a bare doc");
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
    let info = GraphInfo::gather(&doc, Some(id), 0.0).unwrap();
    let opacity = info.props.iter().find(|p| p.kind == PropKind::Opacity).unwrap();
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
    let info = GraphInfo::gather(&doc, Some(id), 0.0).unwrap();
    let result = info.script_results.get(&(PropKind::Opacity, vec![])).unwrap();
    assert_eq!(result.as_ref().unwrap(), "0.25");
}

#[test]
fn the_script_preview_is_addressed_by_tree_path() {
    // A script nested under an operator is keyed by its path, so two
    // scripts in one tree can't show each other's result.
    let (mut doc, id) = graph_doc(Value::constant(0.0));
    apply_graph_op(&mut doc, id, GraphOp::Promote(PropKind::Opacity), 0);
    apply_graph_op(
        &mut doc,
        id,
        GraphOp::SetKind { kind: PropKind::Opacity, path: vec![], new: ExprKind::Add },
        0,
    );
    for (slot, src) in [(0usize, "1.0"), (1, "2.0")] {
        apply_graph_op(
            &mut doc,
            id,
            GraphOp::SetKind {
                kind: PropKind::Opacity,
                path: vec![slot],
                new: ExprKind::Script,
            },
            0,
        );
        apply_graph_op(
            &mut doc,
            id,
            GraphOp::SetScript {
                kind: PropKind::Opacity,
                path: vec![slot],
                src: src.into(),
            },
            0,
        );
    }
    let info = GraphInfo::gather(&doc, Some(id), 0.0).unwrap();
    let at = |path: Vec<usize>| {
        info.script_results.get(&(PropKind::Opacity, path)).unwrap().clone().unwrap()
    };
    assert_eq!(at(vec![0]), "1");
    assert_eq!(at(vec![1]), "2");
    assert!(
        !info.script_results.contains_key(&(PropKind::Opacity, vec![])),
        "the root is an Add, not a script"
    );
}

#[test]
fn a_bad_script_shows_one_line_of_error_in_the_preview() {
    let (doc, id) =
        graph_doc(Value::expr(Expr::Script("value(\"nope\", \"opacity\")".into())));
    let info = GraphInfo::gather(&doc, Some(id), 0.0).unwrap();
    let err = info.script_results[&(PropKind::Opacity, vec![])].clone().unwrap_err();
    assert!(err.contains("nope"), "{err}");
    assert!(!err.contains('\n'), "one line, so it fits under the field");
}

#[test]
fn add_param_then_remove_it_through_the_graph_ops() {
    let (mut doc, id) = graph_doc(Value::constant(0.5));
    apply_graph_op(
        &mut doc,
        id,
        GraphOp::AddParam { name: "gain".into(), kind: ParamKind::Num },
        0,
    );
    assert!(doc.root.find(id).unwrap().param("gain").is_some());
    // The panel lists it, so a `param` node can pick it.
    let info = GraphInfo::gather(&doc, Some(id), 0.0).unwrap();
    assert_eq!(info.params, vec![("gain".to_string(), "number")]);

    apply_graph_op(&mut doc, id, GraphOp::RemoveParam { name: "gain".into() }, 0);
    assert!(doc.root.find(id).unwrap().param("gain").is_none());
}

#[test]
fn a_param_node_drives_a_property_end_to_end() {
    // The whole flow through the panel's ops: add a knob, point the
    // property's expression at it, and see the property take its value.
    let (mut doc, id) = graph_doc(Value::constant(0.0));
    apply_graph_op(
        &mut doc,
        id,
        GraphOp::AddParam { name: "gain".into(), kind: ParamKind::Num },
        0,
    );
    doc.root
        .find_mut(id)
        .unwrap()
        .set_param("gain", ParamValue::Num(Value::constant(0.8)));
    apply_graph_op(&mut doc, id, GraphOp::Promote(PropKind::Opacity), 0);
    apply_graph_op(
        &mut doc,
        id,
        GraphOp::SetKind { kind: PropKind::Opacity, path: vec![], new: ExprKind::Param },
        0,
    );
    apply_graph_op(
        &mut doc,
        id,
        GraphOp::SetParam { kind: PropKind::Opacity, path: vec![], name: "gain".into() },
        0,
    );
    assert_eq!(resolved_opacity(&doc, id), 0.8);
}

#[test]
fn promote_edit_then_bake_round_trips_a_property() {
    let (mut doc, id) = graph_doc(Value::constant(0.5));
    // Promote seeds a literal of the current value — unchanged.
    apply_graph_op(&mut doc, id, GraphOp::Promote(PropKind::Opacity), 0);
    assert!(doc.root.find(id).unwrap().transform.opacity.is_expr());
    assert_eq!(resolved_opacity(&doc, id), 0.5);
    // Edit the literal.
    apply_graph_op(
        &mut doc,
        id,
        GraphOp::SetLit { kind: PropKind::Opacity, path: vec![], value: ExprValue::Num(0.9) },
        0,
    );
    assert_eq!(resolved_opacity(&doc, id), 0.9);
    // Bake back to a constant, freezing the value.
    apply_graph_op(&mut doc, id, GraphOp::Bake(PropKind::Opacity), 0);
    assert!(!doc.root.find(id).unwrap().transform.opacity.is_expr());
    assert_eq!(resolved_opacity(&doc, id), 0.9);
}

#[test]
fn set_kind_grows_a_tree_that_evaluates() {
    // Promote, turn the root into Add, then set its two children: 0.2 + 0.3.
    let (mut doc, id) = graph_doc(Value::constant(0.0));
    apply_graph_op(&mut doc, id, GraphOp::Promote(PropKind::Opacity), 0);
    apply_graph_op(
        &mut doc,
        id,
        GraphOp::SetKind { kind: PropKind::Opacity, path: vec![], new: ExprKind::Add },
        0,
    );
    apply_graph_op(
        &mut doc,
        id,
        GraphOp::SetLit { kind: PropKind::Opacity, path: vec![0], value: ExprValue::Num(0.2) },
        0,
    );
    apply_graph_op(
        &mut doc,
        id,
        GraphOp::SetLit { kind: PropKind::Opacity, path: vec![1], value: ExprValue::Num(0.3) },
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
    apply_graph_op(&mut doc, id, GraphOp::Promote(PropKind::Opacity), 0);
    apply_graph_op(
        &mut doc,
        id,
        GraphOp::SetKind { kind: PropKind::Opacity, path: vec![], new: ExprKind::Script },
        0,
    );
    apply_graph_op(
        &mut doc,
        id,
        GraphOp::SetScript { kind: PropKind::Opacity, path: vec![], src: "frame + 0.25".into() },
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
    apply_graph_op(&mut doc, NodeId(2), GraphOp::Promote(PropKind::Opacity), 0);
    apply_graph_op(
        &mut doc,
        NodeId(2),
        GraphOp::SetRef {
            kind: PropKind::Opacity,
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
    apply_graph_op(&mut doc, id, GraphOp::Promote(PropKind::Opacity), 0);
    apply_graph_op(
        &mut doc,
        id,
        GraphOp::SetKind { kind: PropKind::Opacity, path: vec![], new: ExprKind::Ramp },
        0,
    );
    apply_graph_op(
        &mut doc,
        id,
        GraphOp::SetLit { kind: PropKind::Opacity, path: vec![1], value: ExprValue::Num(5.0) },
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
    apply_graph_op(&mut doc, id, GraphOp::Promote(PropKind::Opacity), 0);
    apply_graph_op(
        &mut doc,
        id,
        GraphOp::SetKind { kind: PropKind::Opacity, path: vec![], new: ExprKind::Oscillator },
        0,
    );
    // freq 1.0 so `frame` counts cycles directly; amp 1, phase/offset 0.
    apply_graph_op(
        &mut doc,
        id,
        GraphOp::SetLit { kind: PropKind::Opacity, path: vec![0], value: ExprValue::Num(1.0) },
        0,
    );
    let sample = |doc: &Document| {
        let node = doc.root.find(id).unwrap();
        let mut ctx = EvalCtx::new(doc, 0.25);
        ctx.in_node(id, |ctx| node.transform.opacity.resolve(ctx))
    };
    assert!((sample(&doc) - 1.0).abs() < 1e-9, "sine at a quarter cycle is +1");
    apply_graph_op(
        &mut doc,
        id,
        GraphOp::SetWaveform {
            kind: PropKind::Opacity,
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
