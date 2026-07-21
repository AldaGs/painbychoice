//! Evaluation: the pure function `(Document, t) -> Scene`. Scrubbing to any
//! time is just calling this with a different `t`; nothing is cached or baked,
//! which is what makes the whole thing non-linear and non-destructive.

use kurbo::{Affine, BezPath};

use crate::expr::EvalCtx;
use crate::node::{CompId, Document, Node, NodeId, Project};
use crate::value::Color;

/// One flat, ready-to-draw item. `source` traces it back to the node that
/// produced it — provenance for selection and debugging.
#[derive(Clone, Debug)]
pub struct RenderItem {
    pub source: NodeId,
    pub transform: Affine,
    pub path: BezPath,
    pub fill: Option<Color>,
    pub stroke: Option<(Color, f64)>,
    /// Effective opacity after multiplying down the ancestor chain.
    pub opacity: f64,
}

/// The evaluated frame: a flat draw list plus any warnings gathered while
/// resolving (e.g. a value that came out non-finite, tagged with its node).
#[derive(Clone, Debug, Default)]
pub struct Scene {
    pub items: Vec<RenderItem>,
    pub warnings: Vec<(NodeId, String)>,
    /// Every *live* node's pivot — its anchor point in composition space —
    /// recorded as the walk passes through it.
    ///
    /// Separate from `items` because a node need not draw anything: a group or
    /// a null has no shape and so no [`RenderItem`], but it still has a place,
    /// and editor overlays (the motion path) need that place. Reading it here
    /// rather than re-deriving the parent chain outside is what keeps those
    /// overlays from drifting away from what the walk actually did with
    /// `LayerTiming`, pre-comps and expression-driven transforms.
    ///
    /// Nodes outside their time window are absent, not zeroed — the walk
    /// returns before reaching them, which is exactly the "layer isn't here on
    /// this frame" signal a path needs to break itself on.
    pub pivots: Vec<(NodeId, kurbo::Point)>,
}

impl Scene {
    /// Where `node`'s anchor sits in composition space, if it was live.
    pub fn pivot(&self, node: NodeId) -> Option<kurbo::Point> {
        self.pivots.iter().find(|(id, _)| *id == node).map(|(_, p)| *p)
    }
}

/// Evaluate a document at `frame` into a flat `Scene`.
///
/// The frame may be fractional — keys sit on the grid, the playhead need not.
/// Seconds never reach this layer; convert at the edges with
/// [`crate::timebase::Timebase`].
pub fn evaluate(doc: &Document, frame: f64) -> Scene {
    let mut scene = Scene::default();
    // The resolve context is built once here and shared (by `&mut`) down the
    // whole walk, so every property resolves against the same document, cache,
    // and warnings sink. Expression warnings gathered during the walk are folded
    // into the scene's provenance-tagged list afterward.
    let mut ctx = EvalCtx::new(doc, frame);
    walk(&doc.root, Affine::IDENTITY, 1.0, &mut ctx, None, &mut Vec::new(), &mut scene);
    scene.warnings.append(&mut ctx.take_warnings());
    scene
}

/// Evaluate one composition of a project at `frame`, recursing into any precomp
/// layers it instances.
///
/// A comp's contents resolve against **that comp** — expressions inside a
/// precomp reach its own nodes, not the parent's. Cross-comp references are
/// deliberately out of scope for v1.
pub fn evaluate_comp(project: &Project, comp: CompId, frame: f64) -> Scene {
    let mut scene = Scene::default();
    eval_comp(project, comp, frame, Affine::IDENTITY, 1.0, &mut Vec::new(), &mut scene);
    scene
}

/// Evaluate a project's root composition — what opening a `.pbc` shows.
pub fn evaluate_project(project: &Project, frame: f64) -> Scene {
    evaluate_comp(project, project.root, frame)
}

/// Walk one comp's tree, with `stack` recording which comps are already being
/// evaluated further up so a cycle can be caught rather than recursed into.
fn eval_comp(
    project: &Project,
    id: CompId,
    frame: f64,
    xf: Affine,
    opacity: f64,
    stack: &mut Vec<CompId>,
    scene: &mut Scene,
) {
    let Some(comp) = project.comp(id) else {
        // A dangling instance: the comp was deleted but a layer still points at
        // it. Warn against the *comp's* root id — there's no better provenance
        // here, and silently drawing nothing would look like a broken frame.
        scene.warnings.push((NodeId(0), format!("precomp {} no longer exists", id.0)));
        return;
    };
    // Each comp gets its own resolve context: its cache and name lookups are
    // scoped to its own tree, which is exactly what "a comp is a boundary" means.
    let mut ctx = EvalCtx::new(comp, frame);
    // Modules are project-wide: the same definition resolves from any comp.
    ctx.modules = Some(&project.modules);
    stack.push(id);
    walk(&comp.root, xf, opacity, &mut ctx, Some(project), stack, scene);
    stack.pop();
    scene.warnings.append(&mut ctx.take_warnings());
}

fn walk(
    node: &Node,
    parent_xf: Affine,
    parent_opacity: f64,
    ctx: &mut EvalCtx,
    project: Option<&Project>,
    stack: &mut Vec<CompId>,
    scene: &mut Scene,
) {
    // A trimmed layer outside its window contributes nothing — and neither do
    // its children, which live in its time. Checked before anything resolves,
    // so a hidden layer costs nothing.
    if let Some(timing) = &node.timing {
        if !timing.is_live(ctx.comp_frame) {
            return;
        }
    }

    // Inside its window, the layer resolves at its *local* frame. Saved and
    // restored around the whole subtree (the same mechanism `resolve_target`
    // uses for off-time sampling) so a sibling can't inherit the shift.
    let prev_frame = ctx.frame;
    let prev_timing = ctx.timing;
    if let Some(timing) = &node.timing {
        ctx.frame = timing.local_frame(ctx.comp_frame);
        // Also publish the window, so `Expr::Time` (in/out/t01) reads *this*
        // layer's clock rather than an ancestor's.
        ctx.timing = Some(*timing);
    }

    // Everything resolved below belongs to this node, so a warning raised deep
    // in an expression (a bad script, an ambiguous name) is tagged with it.
    let prev_node = ctx.enter_node(node.id);
    let (local_xf, local_opacity) = node.transform.resolve(ctx);
    let xf = parent_xf * local_xf;
    let opacity = parent_opacity * local_opacity.clamp(0.0, 1.0);

    // `local_xf` maps the anchor point to `position` by construction, so this
    // is the point the layer rotates and scales about — and the point an
    // on-canvas gizmo centres on. Recorded for every node, drawable or not.
    let anchor = node.transform.anchor.resolve(ctx);
    scene.pivots.push((node.id, xf * kurbo::Point::new(anchor.x, anchor.y)));

    if let Some(shape) = &node.shape {
        let path = shape.to_path(ctx);
        let fill = node.fill.as_ref().map(|f| f.resolve(ctx));
        let stroke = node
            .stroke
            .as_ref()
            .map(|s| (s.color.resolve(ctx), s.width.resolve(ctx)));

        // Provenance-tagged sanity check: surface non-finite geometry instead
        // of silently emitting a broken frame.
        if !xf.as_coeffs().iter().all(|c| c.is_finite()) {
            scene
                .warnings
                .push((node.id, "transform resolved to a non-finite value".into()));
        } else {
            scene.items.push(RenderItem {
                source: node.id,
                transform: xf,
                path,
                fill,
                stroke,
                opacity,
            });
        }
    }

    // A precomp layer renders another comp *into* this one, folded through this
    // layer's transform and opacity — the "vector paste-through" of the plan.
    // No isolated rasterization, so no blend modes or 2D/3D collapse yet; those
    // need the compositor stage.
    if let Some(id) = node.precomp {
        match project {
            // The nested comp's own comp-time is this layer's *local* frame, so
            // trimming or slipping a precomp retimes everything inside it. This
            // is also where nested timing becomes properly relative — a comp
            // boundary is what stage 1 left open.
            Some(project) if !stack.contains(&id) => {
                eval_comp(project, id, ctx.frame, xf, opacity, stack, scene);
            }
            // Comp-level cycle guard, mirroring the expression one: a comp that
            // contains itself warns and stops rather than recursing forever.
            Some(_) => {
                scene.warnings.push((
                    node.id,
                    format!("precomp {} contains itself; not expanded", id.0),
                ));
            }
            // Evaluated as a bare comp rather than through a project, so there
            // is no registry to look the instance up in.
            None => {
                scene
                    .warnings
                    .push((node.id, "precomp layer needs a project to resolve".into()));
            }
        }
    }

    // Children are walked *inside* this node's mark only in the sense that each
    // re-marks itself; restore ours first so a sibling can't inherit it.
    ctx.exit_node(prev_node);

    for child in &node.children {
        walk(child, xf, opacity, ctx, project, stack, scene);
    }

    ctx.frame = prev_frame;
    ctx.timing = prev_timing;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::{Node, Shape, Transform};
    use crate::value::{Keyframe, Track, Value};
    use kurbo::Vec2;

    /// A group draws nothing, so it produces no `RenderItem` — but it is
    /// exactly the sort of layer you parent things to and animate, so it must
    /// still report a pivot. This is what lets the editor draw a motion path
    /// for a null.
    #[test]
    fn a_group_has_no_render_item_but_still_has_a_pivot() {
        let mut group = Node::group(1, "null");
        group.transform.position = Value::constant(Vec2::new(30.0, 40.0));
        let doc = Document::new(100.0, 100.0, Node::group(0, "root").with_child(group));

        let scene = evaluate(&doc, 0.0);
        assert!(scene.items.is_empty(), "a bare group renders nothing");
        assert_eq!(scene.pivot(NodeId(1)), Some(kurbo::Point::new(30.0, 40.0)));
    }

    /// The pivot is the *anchor* in comp space, not the local origin, and it
    /// composes through the parent chain. Anything else and an overlay drawn
    /// at the pivot would sit away from where the layer turns.
    #[test]
    fn a_pivot_is_the_anchor_through_the_whole_parent_chain() {
        let mut child = Node::group(2, "child");
        child.transform.position = Value::constant(Vec2::new(10.0, 0.0));
        child.transform.anchor = Value::constant(Vec2::new(5.0, 5.0));

        let mut parent = Node::group(1, "parent");
        parent.transform.position = Value::constant(Vec2::new(100.0, 100.0));
        let doc = Document::new(
            500.0,
            500.0,
            Node::group(0, "root").with_child(parent.with_child(child)),
        );

        let scene = evaluate(&doc, 0.0);
        // local maps anchor -> position, so the child's pivot is its position
        // in the parent's space, offset by the parent's own position.
        assert_eq!(scene.pivot(NodeId(2)), Some(kurbo::Point::new(110.0, 100.0)));
        assert_eq!(scene.pivot(NodeId(1)), Some(kurbo::Point::new(100.0, 100.0)));
    }

    /// A layer outside its time window is absent from the pivot table rather
    /// than reported at the origin — "the layer isn't here on this frame" is
    /// the signal the motion path breaks its polyline on, and a zero would
    /// draw a line to the corner instead.
    #[test]
    fn a_trimmed_layer_has_no_pivot_outside_its_window() {
        use crate::node::LayerTiming;
        let mut layer = Node::group(1, "clip");
        layer.transform.position = Value::constant(Vec2::new(20.0, 20.0));
        layer.timing = Some(LayerTiming { start: 0, in_: 10, out: 20 });
        let doc = Document::new(100.0, 100.0, Node::group(0, "root").with_child(layer));

        assert_eq!(evaluate(&doc, 15.0).pivot(NodeId(1)), Some(kurbo::Point::new(20.0, 20.0)));
        assert_eq!(evaluate(&doc, 5.0).pivot(NodeId(1)), None, "before its in-point");
        assert_eq!(evaluate(&doc, 25.0).pivot(NodeId(1)), None, "after its out-point");
    }

    /// The canonical smoke test: a keyframed square whose position animates.
    /// Halfway between its two keys the evaluated render item's transform must
    /// reflect the midpoint.
    #[test]
    fn keyframed_square_is_halfway_at_midpoint() {
        let square = Node::shape(
            1,
            "square",
            Shape::Rect {
                size: Value::constant(Vec2::new(100.0, 100.0)),
                radius: Value::constant(0.0),
            },
        )
        .with_fill(Color::rgb(1.0, 0.0, 0.0))
        .with_transform(Transform {
            position: Value::Keyframed(Track::new(vec![
                Keyframe::linear(0, Vec2::new(0.0, 0.0)),
                Keyframe::linear(24, Vec2::new(200.0, 100.0)),
            ])),
            ..Transform::default()
        });

        let doc = Document::new(1920.0, 1080.0, Node::group(0, "root").with_child(square));

        let scene = evaluate(&doc, 12.0);
        assert_eq!(scene.items.len(), 1, "one drawable expected");
        assert!(scene.warnings.is_empty(), "no warnings expected");

        // The translation component of the resolved matrix = the eased position.
        let coeffs = scene.items[0].transform.as_coeffs();
        let (tx, ty) = (coeffs[4], coeffs[5]);
        assert!((tx - 100.0).abs() < 1e-3, "tx = {tx}");
        assert!((ty - 50.0).abs() < 1e-3, "ty = {ty}");
    }

    #[test]
    fn opacity_multiplies_down_the_tree() {
        let child = Node::shape(2, "c", Shape::Ellipse { size: Value::constant(Vec2::new(10.0, 10.0)) })
            .with_transform(Transform { opacity: Value::constant(0.5), ..Transform::default() });
        let parent = Node::group(1, "g")
            .with_transform(Transform { opacity: Value::constant(0.5), ..Transform::default() })
            .with_child(child);
        let doc = Document::new(100.0, 100.0, parent);

        let scene = evaluate(&doc, 0.0);
        assert!((scene.items[0].opacity - 0.25).abs() < 1e-6);
    }

    #[test]
    fn an_expression_drives_an_evaluated_property() {
        use crate::expr::{Expr, PropPath};
        // A driver node holds opacity 0.4; a visible square mirrors it via an
        // expression. The evaluated square's opacity should be the driver's.
        let driver = Node::group(1, "driver")
            .with_transform(Transform { opacity: Value::constant(0.4), ..Transform::default() });
        let square = Node::shape(
            2,
            "square",
            Shape::Rect { size: Value::constant(Vec2::new(10.0, 10.0)), radius: Value::constant(0.0) },
        )
        .with_fill(Color::rgb(1.0, 1.0, 1.0))
        .with_transform(Transform {
            opacity: Value::expr(Expr::reference(NodeId(1), PropPath::Opacity)),
            ..Transform::default()
        });
        let doc =
            Document::new(100.0, 100.0, Node::group(0, "root").with_child(driver).with_child(square));

        let scene = evaluate(&doc, 0.0);
        let item = scene.items.iter().find(|i| i.source == NodeId(2)).unwrap();
        assert!((item.opacity - 0.4).abs() < 1e-9, "opacity = {}", item.opacity);
        assert!(scene.warnings.is_empty());
    }

    #[test]
    fn a_broken_script_warns_against_the_node_that_owns_it() {
        use crate::expr::Expr;
        // A script that can't compile: the frame still renders (the property
        // falls back to a neutral value) but the scene says which node broke.
        let square = Node::shape(
            7,
            "square",
            Shape::Rect {
                size: Value::constant(Vec2::new(10.0, 10.0)),
                radius: Value::constant(0.0),
            },
        )
        .with_transform(Transform {
            opacity: Value::expr(Expr::Script("this is not rhai".into())),
            ..Transform::default()
        });
        let doc = Document::new(100.0, 100.0, Node::group(0, "root").with_child(square));
        let scene = evaluate(&doc, 0.0);
        assert_eq!(scene.items.len(), 1, "the frame still renders");
        let (node, msg) = scene.warnings.first().expect("a warning reached the scene");
        assert_eq!(*node, NodeId(7), "attributed to the node with the script");
        assert!(msg.contains("script"), "{msg}");
    }

    #[test]
    fn an_ambiguous_name_warns_rather_than_silently_picking() {
        use crate::expr::Expr;
        // Two nodes named "dup": a script referencing that name has to pick
        // one, but the choice depends on tree order, so it says so.
        let dup = |id: u64| {
            Node::group(id, "dup")
                .with_transform(Transform { opacity: Value::constant(0.5), ..Transform::default() })
        };
        let reader = Node::shape(
            9,
            "reader",
            Shape::Rect {
                size: Value::constant(Vec2::new(10.0, 10.0)),
                radius: Value::constant(0.0),
            },
        )
        .with_transform(Transform {
            opacity: Value::expr(Expr::Script("value(\"dup\", \"opacity\")".into())),
            ..Transform::default()
        });
        let doc = Document::new(
            100.0,
            100.0,
            Node::group(0, "root").with_child(dup(1)).with_child(dup(2)).with_child(reader),
        );
        let scene = evaluate(&doc, 0.0);
        let msg = scene
            .warnings
            .iter()
            .find(|(_, m)| m.contains("named 'dup'"))
            .map(|(_, m)| m.clone())
            .expect("the ambiguity should reach the scene");
        assert!(msg.contains('2'), "says how many: {msg}");
    }

    #[test]
    fn an_expression_cycle_surfaces_as_a_scene_warning() {
        use crate::expr::{Expr, PropPath};
        // A visible square whose opacity references itself: evaluate must return
        // (not hang) and report the cycle in the scene's warnings.
        let square = Node::shape(
            2,
            "square",
            Shape::Rect { size: Value::constant(Vec2::new(10.0, 10.0)), radius: Value::constant(0.0) },
        )
        .with_transform(Transform {
            opacity: Value::expr(Expr::reference(NodeId(2), PropPath::Opacity)),
            ..Transform::default()
        });
        let doc = Document::new(100.0, 100.0, Node::group(0, "root").with_child(square));

        let scene = evaluate(&doc, 0.0);
        assert!(!scene.warnings.is_empty(), "the cycle should reach the scene");
    }

    /// A layer with a time range draws only inside `[in, out)`, and `out` is
    /// exclusive so two abutting clips never both draw on the seam frame.
    #[test]
    fn a_trimmed_layer_only_draws_inside_its_window() {
        use crate::node::LayerTiming;
        let square = Node::shape(
            1,
            "square",
            Shape::Rect { size: Value::constant(Vec2::new(10.0, 10.0)), radius: Value::constant(0.0) },
        )
        .with_timing(LayerTiming::new(10, 20));
        let doc = Document::new(100.0, 100.0, Node::group(0, "root").with_child(square));

        let drawn = |f: f64| !evaluate(&doc, f).items.is_empty();
        assert!(!drawn(9.0), "before in");
        assert!(drawn(10.0), "on in");
        assert!(drawn(19.5), "inside, between frames");
        assert!(!drawn(20.0), "out is exclusive");
    }

    /// The payoff of local time: two layers share the *same* keyframes and play
    /// them at different comp times, purely from their `start`.
    #[test]
    fn keyframes_are_sampled_in_layer_local_time() {
        use crate::node::LayerTiming;
        let clip = |id: u64, at: i64| {
            Node::shape(
                id,
                "clip",
                Shape::Rect {
                    size: Value::constant(Vec2::new(10.0, 10.0)),
                    radius: Value::constant(0.0),
                },
            )
            .with_timing(LayerTiming::new(at, at + 24))
            .with_transform(Transform {
                position: Value::Keyframed(Track::new(vec![
                    Keyframe::linear(0, Vec2::new(0.0, 0.0)),
                    Keyframe::linear(24, Vec2::new(240.0, 0.0)),
                ])),
                ..Transform::default()
            })
        };
        let doc = Document::new(
            100.0,
            100.0,
            Node::group(0, "root").with_child(clip(1, 0)).with_child(clip(2, 100)),
        );

        // 12 frames into each clip, both sit at the same *local* midpoint.
        let x_of = |scene: &Scene, id: u64| {
            scene.items.iter().find(|i| i.source == NodeId(id)).unwrap().transform.as_coeffs()[4]
        };
        let early = evaluate(&doc, 12.0);
        let late = evaluate(&doc, 112.0);
        assert_eq!(early.items.len(), 1, "only the first clip is live at 12");
        assert!((x_of(&early, 1) - 120.0).abs() < 1e-6);
        assert!((x_of(&late, 2) - 120.0).abs() < 1e-6, "same animation, retimed");
    }

    /// Slipping moves the content under a fixed window: same `[in, out)`, later
    /// `start`, so the layer shows an earlier part of its animation.
    #[test]
    fn slipping_shifts_content_without_moving_the_window() {
        use crate::node::LayerTiming;
        let square = |start: i64| {
            Node::shape(
                1,
                "square",
                Shape::Rect {
                    size: Value::constant(Vec2::new(10.0, 10.0)),
                    radius: Value::constant(0.0),
                },
            )
            .with_timing(LayerTiming { start, in_: 0, out: 24 })
            .with_transform(Transform {
                position: Value::Keyframed(Track::new(vec![
                    Keyframe::linear(0, Vec2::new(0.0, 0.0)),
                    Keyframe::linear(24, Vec2::new(240.0, 0.0)),
                ])),
                ..Transform::default()
            })
        };
        let x_at = |start: i64| {
            let doc = Document::new(100.0, 100.0, Node::group(0, "root").with_child(square(start)));
            evaluate(&doc, 12.0).items[0].transform.as_coeffs()[4]
        };
        assert!((x_at(0) - 120.0).abs() < 1e-6, "unslipped: local 12");
        assert!((x_at(6) - 60.0).abs() < 1e-6, "slipped later: local 6");
    }

    /// A timed layer must not leak its local frame onto the sibling that
    /// follows it — the shift is scoped to its own subtree.
    #[test]
    fn a_layers_local_time_does_not_leak_to_its_sibling() {
        use crate::node::LayerTiming;
        let animated = |id: u64| {
            Node::shape(
                id,
                "s",
                Shape::Rect {
                    size: Value::constant(Vec2::new(10.0, 10.0)),
                    radius: Value::constant(0.0),
                },
            )
            .with_transform(Transform {
                position: Value::Keyframed(Track::new(vec![
                    Keyframe::linear(0, Vec2::new(0.0, 0.0)),
                    Keyframe::linear(24, Vec2::new(240.0, 0.0)),
                ])),
                ..Transform::default()
            })
        };
        let shifted = animated(1).with_timing(LayerTiming { start: 6, in_: 0, out: 100 });
        let plain = animated(2);
        let doc =
            Document::new(100.0, 100.0, Node::group(0, "root").with_child(shifted).with_child(plain));

        let scene = evaluate(&doc, 12.0);
        let x = |id: u64| {
            scene.items.iter().find(|i| i.source == NodeId(id)).unwrap().transform.as_coeffs()[4]
        };
        assert!((x(1) - 60.0).abs() < 1e-6, "shifted layer at local 6");
        assert!((x(2) - 120.0).abs() < 1e-6, "sibling still at comp frame 12");
    }

    /// **The Stage 2 payoff.** Two clips of *different lengths* share one
    /// expression — opacity = `t01` — and each fades across its own duration.
    /// No keyframes, and nothing about the expression mentions either clip.
    #[test]
    fn one_expression_fits_itself_to_each_clips_length() {
        use crate::expr::{Expr, TimeSource};
        use crate::node::LayerTiming;
        let clip = |id: u64, in_: i64, out: i64| {
            Node::shape(
                id,
                "clip",
                Shape::Rect {
                    size: Value::constant(Vec2::new(10.0, 10.0)),
                    radius: Value::constant(0.0),
                },
            )
            .with_timing(LayerTiming::new(in_, out))
            .with_transform(Transform {
                opacity: Value::expr(Expr::Time(TimeSource::T01)),
                ..Transform::default()
            })
        };
        // A short clip and a long one, starting at different comp frames.
        let doc = Document::new(
            100.0,
            100.0,
            Node::group(0, "root").with_child(clip(1, 0, 10)).with_child(clip(2, 100, 200)),
        );

        let opacity_of = |frame: f64, id: u64| {
            evaluate(&doc, frame).items.iter().find(|i| i.source == NodeId(id)).unwrap().opacity
        };
        // Halfway through each clip — 5 frames into one, 50 into the other —
        // both are at 0.5, because each measures against its own length.
        assert!((opacity_of(5.0, 1) - 0.5).abs() < 1e-9, "short clip midpoint");
        assert!((opacity_of(150.0, 2) - 0.5).abs() < 1e-9, "long clip midpoint");
        // And each starts at 0.
        assert!(opacity_of(0.0, 1).abs() < 1e-9);
        assert!(opacity_of(100.0, 2).abs() < 1e-9);
    }

    /// The same thing through the Rhai scope rather than the IR — the two
    /// spellings have to agree, since they're one vocabulary.
    #[test]
    fn a_script_reads_the_same_layer_clock() {
        use crate::expr::Expr;
        use crate::node::LayerTiming;
        let square = Node::shape(
            1,
            "s",
            Shape::Rect { size: Value::constant(Vec2::new(10.0, 10.0)), radius: Value::constant(0.0) },
        )
        .with_timing(LayerTiming::new(20, 60))
        .with_transform(Transform {
            // Local in/out are 0/40 here, so this reads (localTime + in + out).
            opacity: Value::expr(Expr::Script("(localTime + inPoint + outPoint) / 100.0".into())),
            ..Transform::default()
        });
        let doc = Document::new(100.0, 100.0, Node::group(0, "root").with_child(square));

        // Comp frame 30 → local 10, inPoint 0, outPoint 40 → 50/100.
        let scene = evaluate(&doc, 30.0);
        assert!(scene.warnings.is_empty(), "{:?}", scene.warnings);
        assert!((scene.items[0].opacity - 0.5).abs() < 1e-9, "{}", scene.items[0].opacity);
    }

    /// An untimed layer still has a meaningful clock: it reads as one clip
    /// spanning the composition, so `t01` works before any trimming exists.
    #[test]
    fn an_untimed_layer_reads_the_comp_as_its_window() {
        use crate::expr::{Expr, TimeSource};
        let square = Node::shape(
            1,
            "s",
            Shape::Rect { size: Value::constant(Vec2::new(10.0, 10.0)), radius: Value::constant(0.0) },
        )
        .with_transform(Transform {
            opacity: Value::expr(Expr::Time(TimeSource::T01)),
            ..Transform::default()
        });
        let mut doc = Document::new(100.0, 100.0, Node::group(0, "root").with_child(square));
        doc.fps = 10.0;
        doc.duration = 10.0; // 100 frames
        assert_eq!(doc.duration_frames(), 100);

        assert!((evaluate(&doc, 50.0).items[0].opacity - 0.5).abs() < 1e-9);
        assert!((evaluate(&doc, 100.0).items[0].opacity - 1.0).abs() < 1e-9);
    }

    /// `replace` must keep the node's place among its siblings — that ordering
    /// is draw order, so pre-composing a layer must not restack it.
    #[test]
    fn replace_keeps_the_layers_place_in_draw_order() {
        let mut root = Node::group(0, "root")
            .with_child(dot(1))
            .with_child(dot(2))
            .with_child(dot(3));
        let old = root.replace(NodeId(2), Node::group(9, "instance")).expect("found");
        assert_eq!(old.id, NodeId(2), "the old node comes back");
        let ids: Vec<u64> = root.children.iter().map(|c| c.id.0).collect();
        assert_eq!(ids, vec![1, 9, 3], "swapped in place, not appended");
    }

    /// A comp always shows *something* in the switcher, including one loaded
    /// from a file written before comps had names.
    #[test]
    fn a_nameless_comp_falls_back_to_a_generated_label() {
        use crate::node::CompId;
        let mut comp = Document::new(10.0, 10.0, Node::group(0, "r"));
        assert_eq!(comp.label(CompId(0)), "Comp 1", "1-based for humans");
        comp.name = "  ".into();
        assert_eq!(comp.label(CompId(3)), "Comp 4", "blank is still nameless");
        comp.name = "Subtitles".into();
        assert_eq!(comp.label(CompId(3)), "Subtitles");
    }

    // --- Multi-comp / pre-comps (stage 3) ---

    /// A 10x10 square at the origin, for building comps to nest.
    fn dot(id: u64) -> Node {
        Node::shape(
            id,
            "dot",
            Shape::Rect {
                size: Value::constant(Vec2::new(10.0, 10.0)),
                radius: Value::constant(0.0),
            },
        )
    }

    /// The reason for the registry: **one comp, placed twice**. Both instances
    /// render, each folded through its own layer's transform — which inline
    /// nesting could never express.
    #[test]
    fn one_comp_instanced_twice_renders_twice() {
        use crate::node::{CompId, Project};
        let inner = Document::new(100.0, 100.0, Node::group(0, "inner-root").with_child(dot(1)));
        let mut project = Project::single(inner);
        let inner_id = project.root;

        let place = |id: u64, x: f64| {
            Node::group(id, "instance")
                .with_precomp(inner_id)
                .with_transform(Transform {
                    position: Value::constant(Vec2::new(x, 0.0)),
                    ..Transform::default()
                })
        };
        let outer = Document::new(
            100.0,
            100.0,
            Node::group(10, "outer-root").with_child(place(11, 50.0)).with_child(place(12, 200.0)),
        );
        let outer_id = project.insert(outer);
        project.root = outer_id;

        let scene = evaluate_project(&project, 0.0);
        assert!(scene.warnings.is_empty(), "{:?}", scene.warnings);
        assert_eq!(scene.items.len(), 2, "both placements render");
        let mut xs: Vec<f64> =
            scene.items.iter().map(|i| i.transform.as_coeffs()[4]).collect();
        xs.sort_by(|a, b| a.partial_cmp(b).unwrap());
        assert!((xs[0] - 50.0).abs() < 1e-9, "first instance at its layer's x");
        assert!((xs[1] - 200.0).abs() < 1e-9, "second at its own");
        // Provenance points at the *inner* node — both items came from it.
        assert!(scene.items.iter().all(|i| i.source == NodeId(1)));
    }

    /// A precomp layer's opacity folds into everything the nested comp emits —
    /// the "vector paste-through" compositing of v1.
    #[test]
    fn a_precomp_layer_folds_its_opacity_into_the_nested_comp() {
        use crate::node::Project;
        let inner_child = dot(1).with_transform(Transform {
            opacity: Value::constant(0.5),
            ..Transform::default()
        });
        let mut project =
            Project::single(Document::new(100.0, 100.0, Node::group(0, "i").with_child(inner_child)));
        let inner_id = project.root;
        let outer = Document::new(
            100.0,
            100.0,
            Node::group(10, "o").with_child(Node::group(11, "inst").with_precomp(inner_id).with_transform(
                Transform { opacity: Value::constant(0.5), ..Transform::default() },
            )),
        );
        project.root = project.insert(outer);

        let scene = evaluate_project(&project, 0.0);
        assert!((scene.items[0].opacity - 0.25).abs() < 1e-9, "0.5 × 0.5");
    }

    /// Trimming/slipping a precomp retimes **everything inside it**: the nested
    /// comp's own time is the layer's local frame. This is also where nested
    /// timing becomes properly relative, which stage 1 deliberately left open.
    #[test]
    fn a_precomp_is_retimed_by_its_layers_local_time() {
        use crate::node::{LayerTiming, Project};
        // Inner comp: a dot sliding 0 -> 240 over 24 frames of *its* time.
        let inner_child = dot(1).with_transform(Transform {
            position: Value::Keyframed(Track::new(vec![
                Keyframe::linear(0, Vec2::new(0.0, 0.0)),
                Keyframe::linear(24, Vec2::new(240.0, 0.0)),
            ])),
            ..Transform::default()
        });
        let mut project =
            Project::single(Document::new(100.0, 100.0, Node::group(0, "i").with_child(inner_child)));
        let inner_id = project.root;
        // Placed starting at comp frame 100.
        let outer = Document::new(
            100.0,
            100.0,
            Node::group(10, "o").with_child(
                Node::group(11, "inst").with_precomp(inner_id).with_timing(LayerTiming::new(100, 200)),
            ),
        );
        project.root = project.insert(outer);

        // Comp frame 112 = local 12 = the inner animation's midpoint.
        let scene = evaluate_project(&project, 112.0);
        assert_eq!(scene.items.len(), 1);
        assert!((scene.items[0].transform.as_coeffs()[4] - 120.0).abs() < 1e-9);
        // And outside the instance's window it doesn't draw at all.
        assert!(evaluate_project(&project, 99.0).items.is_empty());
    }

    /// Comp-level cycle guard, mirroring the expression one: a comp that
    /// contains itself must warn and stop, not recurse until the stack dies.
    #[test]
    fn a_self_containing_comp_warns_instead_of_hanging() {
        use crate::node::Project;
        let mut project = Project::single(Document::new(100.0, 100.0, Node::group(0, "a")));
        let id = project.root;
        project
            .comp_mut(id)
            .unwrap()
            .root
            .children
            .push(Node::group(1, "self").with_precomp(id));

        let scene = evaluate_project(&project, 0.0);
        let (node, msg) = scene.warnings.first().expect("the cycle should warn");
        assert_eq!(*node, NodeId(1), "attributed to the instancing layer");
        assert!(msg.contains("itself"), "{msg}");
    }

    /// The indirect case: A instances B, B instances A. The guard is a stack of
    /// comps being evaluated, so it catches a cycle of any length.
    #[test]
    fn a_mutual_comp_cycle_warns() {
        use crate::node::{CompId, Project};
        let mut project = Project::single(Document::new(100.0, 100.0, Node::group(0, "a")));
        let a = project.root;
        let b = project.insert(Document::new(100.0, 100.0, Node::group(10, "b")));
        // a contains b …
        project.comp_mut(a).unwrap().root.children.push(Node::group(1, "->b").with_precomp(b));
        // … and b contains a.
        project.comp_mut(b).unwrap().root.children.push(Node::group(11, "->a").with_precomp(a));

        let scene = evaluate_project(&project, 0.0);
        assert!(
            scene.warnings.iter().any(|(_, m)| m.contains("itself")),
            "expected a cycle warning, got {:?}",
            scene.warnings
        );
        assert_eq!(project.root, a, "sanity: root untouched");
        let _ = CompId(0);
    }

    /// An instance pointing at a deleted comp warns rather than drawing nothing
    /// silently — a blank frame is indistinguishable from a broken one.
    #[test]
    fn a_dangling_precomp_reference_warns() {
        use crate::node::{CompId, Project};
        let mut project = Project::single(Document::new(100.0, 100.0, Node::group(0, "a")));
        let id = project.root;
        project
            .comp_mut(id)
            .unwrap()
            .root
            .children
            .push(Node::group(1, "ghost").with_precomp(CompId(999)));

        let scene = evaluate_project(&project, 0.0);
        assert!(
            scene.warnings.iter().any(|(_, m)| m.contains("no longer exists")),
            "{:?}",
            scene.warnings
        );
    }

    /// **The `.pbc` migration.** A pre-project document is exactly one comp, so
    /// an old file deserializes into `Comp` and `Project::single` wraps it —
    /// and a project round-trips like anything else.
    #[test]
    fn a_single_comp_document_becomes_a_one_comp_project() {
        use crate::node::Project;
        let json = r#"{"width":640.0,"height":480.0,"fps":24.0,"duration":5.0,
            "root":{"id":0,"name":"root","transform":{"anchor":{"Const":[0.0,0.0]},
            "position":{"Const":[0.0,0.0]},"rotation_deg":{"Const":0.0},
            "scale":{"Const":[1.0,1.0]},"opacity":{"Const":1.0}},
            "shape":null,"fill":null,"stroke":null,"children":[]}}"#;
        let legacy: Document = serde_json::from_str(json).unwrap();
        let mut project = Project::single(legacy);
        project.migrate();

        assert_eq!(project.comps.len(), 1);
        assert_eq!(project.root_comp().width, 640.0);

        let back: Project =
            serde_json::from_str(&serde_json::to_string(&project).unwrap()).unwrap();
        assert_eq!(back.root, project.root);
        assert_eq!(back.root_comp().fps, 24.0);
    }

    /// A precomp layer evaluated through the single-comp entry point can't
    /// resolve — there's no registry — so it says so rather than rendering an
    /// empty frame that looks correct.
    #[test]
    fn a_precomp_without_a_project_warns() {
        use crate::node::CompId;
        let doc = Document::new(
            100.0,
            100.0,
            Node::group(0, "root").with_child(Node::group(1, "inst").with_precomp(CompId(0))),
        );
        let scene = evaluate(&doc, 0.0);
        assert!(
            scene.warnings.iter().any(|(_, m)| m.contains("needs a project")),
            "{:?}",
            scene.warnings
        );
    }


    // --- Shared animation modules (the document-wide property graph) ---

    /// **The story this feature exists for.** Three "subtitles" of different
    /// lengths and in-points all link *one* module for their fade. Each plays it
    /// fitted to its own clip, because the module's body reads `t01` and `t01`
    /// is whichever layer is resolving.
    #[test]
    fn one_module_drives_many_layers_each_fitted_to_its_own_clip() {
        use crate::expr::{Expr, TimeSource};
        use crate::node::{LayerTiming, Module, Project};

        let mut project = Project::single(Document::new(100.0, 100.0, Node::group(0, "root")));
        let comp_id = project.root;
        // The module: opacity = t01. One definition, mentioning no layer.
        let fade = project.add_module(Module::new("fade", Expr::Time(TimeSource::T01)));

        let subtitle = |id: u64, in_: i64, out: i64| {
            dot(id).with_timing(LayerTiming::new(in_, out)).with_transform(Transform {
                opacity: Value::expr(Expr::Use { module: fade, overrides: Vec::new() }),
                ..Transform::default()
            })
        };
        let root = &mut project.comp_mut(comp_id).unwrap().root;
        root.children.push(subtitle(1, 0, 10));
        root.children.push(subtitle(2, 100, 200));
        root.children.push(subtitle(3, 300, 340));

        let opacity_at = |frame: f64, id: u64| {
            evaluate_comp(&project, comp_id, frame)
                .items
                .iter()
                .find(|i| i.source == NodeId(id))
                .map(|i| i.opacity)
        };
        // Halfway through each clip - 5, 50 and 20 frames in - all read 0.5.
        assert!((opacity_at(5.0, 1).unwrap() - 0.5).abs() < 1e-9);
        assert!((opacity_at(150.0, 2).unwrap() - 0.5).abs() < 1e-9);
        assert!((opacity_at(320.0, 3).unwrap() - 0.5).abs() < 1e-9);
    }

    /// Editing the module in one place changes every link - the point of a
    /// module over copied expressions.
    #[test]
    fn editing_a_module_changes_every_link() {
        use crate::expr::{Expr, ExprValue};
        use crate::node::{Module, Project};
        let mut project = Project::single(Document::new(100.0, 100.0, Node::group(0, "root")));
        let comp_id = project.root;
        let m = project.add_module(Module::new("dim", Expr::Lit(ExprValue::Num(0.2))));
        for id in 1..=3 {
            project.comp_mut(comp_id).unwrap().root.children.push(
                dot(id).with_transform(Transform {
                    opacity: Value::expr(Expr::Use { module: m, overrides: Vec::new() }),
                    ..Transform::default()
                }),
            );
        }
        let opacities = |p: &Project| {
            evaluate_comp(p, comp_id, 0.0).items.iter().map(|i| i.opacity).collect::<Vec<_>>()
        };
        assert!(opacities(&project).iter().all(|o| (o - 0.2).abs() < 1e-9));

        // One edit, at the definition site.
        project.module_mut(m).unwrap().body = Expr::Lit(ExprValue::Num(0.9));
        assert!(opacities(&project).iter().all(|o| (o - 0.9).abs() < 1e-9), "all three followed");
    }

    /// **Override is a layering, not a fork**: an overridden knob wins, and every
    /// knob left alone still inherits the module's default, so one diverging
    /// instance does not detach from the shared definition.
    #[test]
    fn an_override_replaces_one_knob_and_inherits_the_rest() {
        use crate::expr::{Expr, ExprValue};
        use crate::node::{Module, ParamValue, Project};
        let mut project = Project::single(Document::new(100.0, 100.0, Node::group(0, "root")));
        let comp_id = project.root;
        // opacity = level * scale, both knobs.
        let m = project.add_module(
            Module::new(
                "two-knobs",
                Expr::Mul(
                    Box::new(Expr::Param { node: None, name: "level".into() }),
                    Box::new(Expr::Param { node: None, name: "scale".into() }),
                ),
            )
            .with_param("level", ParamValue::Num(Value::constant(0.5)))
            .with_param("scale", ParamValue::Num(Value::constant(1.0))),
        );
        let link = |id: u64, overrides: Vec<(String, Expr)>| {
            dot(id).with_transform(Transform {
                opacity: Value::expr(Expr::Use { module: m, overrides }),
                ..Transform::default()
            })
        };
        let root = &mut project.comp_mut(comp_id).unwrap().root;
        root.children.push(link(1, Vec::new()));
        root.children.push(link(2, vec![("scale".into(), Expr::Lit(ExprValue::Num(0.5)))]));

        let scene = evaluate_comp(&project, comp_id, 0.0);
        assert!(scene.warnings.is_empty(), "{:?}", scene.warnings);
        let op = |id: u64| scene.items.iter().find(|i| i.source == NodeId(id)).unwrap().opacity;
        assert!((op(1) - 0.5).abs() < 1e-9, "inherits both defaults");
        // Overrode `scale` only; `level` still comes from the module.
        assert!((op(2) - 0.25).abs() < 1e-9, "0.5 inherited x 0.5 override");

        // And the definition still reaches the overridden instance.
        project.module_mut(m).unwrap().params[0].value = ParamValue::Num(Value::constant(1.0));
        let scene = evaluate_comp(&project, comp_id, 0.0);
        let op = |id: u64| scene.items.iter().find(|i| i.source == NodeId(id)).unwrap().opacity;
        assert!((op(2) - 0.5).abs() < 1e-9, "override layered over the new default");
    }

    /// A module that links itself warns and falls back, exactly as a property
    /// cycle and a comp cycle do.
    #[test]
    fn a_module_that_links_itself_warns() {
        use crate::expr::{Expr, ExprValue};
        use crate::node::{Module, Project};
        let mut project = Project::single(Document::new(100.0, 100.0, Node::group(0, "root")));
        let comp_id = project.root;
        let m = project.add_module(Module::new("loop", Expr::Lit(ExprValue::Num(1.0))));
        project.module_mut(m).unwrap().body = Expr::Use { module: m, overrides: Vec::new() };
        project.comp_mut(comp_id).unwrap().root.children.push(dot(1).with_transform(Transform {
            opacity: Value::expr(Expr::Use { module: m, overrides: Vec::new() }),
            ..Transform::default()
        }));

        let scene = evaluate_comp(&project, comp_id, 0.0);
        assert!(
            scene.warnings.iter().any(|(_, msg)| msg.contains("links itself")),
            "{:?}",
            scene.warnings
        );
    }

    /// Typos are worth surfacing: an override naming a knob the module does not
    /// have would otherwise silently do nothing.
    #[test]
    fn overriding_an_unknown_knob_warns() {
        use crate::expr::{Expr, ExprValue};
        use crate::node::{Module, Project};
        let mut project = Project::single(Document::new(100.0, 100.0, Node::group(0, "root")));
        let comp_id = project.root;
        let m = project.add_module(Module::new("plain", Expr::Lit(ExprValue::Num(1.0))));
        project.comp_mut(comp_id).unwrap().root.children.push(dot(1).with_transform(Transform {
            opacity: Value::expr(Expr::Use {
                module: m,
                overrides: vec![("typo".into(), Expr::Lit(ExprValue::Num(0.0)))],
            }),
            ..Transform::default()
        }));
        let scene = evaluate_comp(&project, comp_id, 0.0);
        assert!(
            scene.warnings.iter().any(|(_, msg)| msg.contains("no parameter")),
            "{:?}",
            scene.warnings
        );
    }

    /// A module link round-trips through `.pbc`, overrides included.
    #[test]
    fn a_project_with_modules_round_trips() {
        use crate::expr::{Expr, ExprValue, TimeSource};
        use crate::node::{Module, ParamValue, Project};
        let mut project = Project::single(Document::new(100.0, 100.0, Node::group(0, "root")));
        let m = project.add_module(
            Module::new("fade", Expr::Time(TimeSource::T01))
                .with_param("amount", ParamValue::Num(Value::constant(0.5))),
        );
        let root_id = project.root;
        project.comp_mut(root_id).unwrap().root.children.push(dot(1).with_transform(Transform {
            opacity: Value::expr(Expr::Use {
                module: m,
                overrides: vec![("amount".into(), Expr::Lit(ExprValue::Num(0.25)))],
            }),
            ..Transform::default()
        }));
        let back: Project =
            serde_json::from_str(&serde_json::to_string(&project).unwrap()).unwrap();
        assert_eq!(back.modules.len(), 1);
        assert_eq!(back.module(m).unwrap().name, "fade");
        assert_eq!(back.module(m).unwrap().params.len(), 1);
    }

    /// Serde `default` *is* the migration: a `.pbc` written before layer timing
    /// existed loads with `timing: None` and behaves exactly as it did.
    #[test]
    fn a_document_without_timing_still_loads() {
        use crate::node::LayerTiming;
        let json = r#"{"width":100.0,"height":100.0,"fps":24.0,"duration":5.0,
            "root":{"id":0,"name":"root","transform":{"anchor":{"Const":[0.0,0.0]},
            "position":{"Const":[0.0,0.0]},"rotation_deg":{"Const":0.0},
            "scale":{"Const":[1.0,1.0]},"opacity":{"Const":1.0}},
            "shape":null,"fill":null,"stroke":null,"children":[]}}"#;
        let mut doc: Document = serde_json::from_str(json).unwrap();
        doc.migrate();
        assert_eq!(doc.root.timing, None);

        // …and a timed layer round-trips.
        doc.root.timing = Some(LayerTiming { start: 3, in_: 5, out: 9 });
        let back: Document = serde_json::from_str(&serde_json::to_string(&doc).unwrap()).unwrap();
        assert_eq!(back.root.timing, Some(LayerTiming { start: 3, in_: 5, out: 9 }));
    }

    /// A text layer draws through the ordinary shape pipeline: `evaluate` must
    /// hand back real outline geometry, with the fill and transform every other
    /// shape gets. This is the whole payoff of resolving text to a `BezPath` —
    /// no renderer had to learn about text.
    #[test]
    fn a_text_layer_evaluates_to_outline_geometry() {
        let doc = Document::new(
            640.0,
            480.0,
            Node::group(0, "root").with_child(
                Node::shape(
                    1,
                    "caption",
                    Shape::Text {
                        content: "Hi".into(),
                        family: String::new(),
                        size: Value::constant(48.0),
                        align: crate::text::TextAlign::Left,
                        max_width: None,
                    },
                )
                .with_fill(Color::rgb(1.0, 0.0, 0.0)),
            ),
        );
        let scene = evaluate(&doc, 0.0);
        assert!(scene.warnings.is_empty(), "{:?}", scene.warnings);
        assert_eq!(scene.items.len(), 1);
        assert!(!scene.items[0].path.is_empty(), "glyph outlines reached the draw list");
        assert!(scene.items[0].fill.is_some(), "text fills like any other shape");
    }

    /// The font size is a `Value`, so it animates — the reason it's a `Value`
    /// at all. Bigger size at a later frame ⇒ a bigger outline.
    #[test]
    fn a_text_layers_font_size_animates() {
        use crate::value::{Keyframe, Track};
        let doc = Document::new(
            640.0,
            480.0,
            Node::group(0, "root").with_child(Node::shape(
                1,
                "caption",
                Shape::Text {
                    content: "Hi".into(),
                    family: String::new(),
                    size: Value::Keyframed(Track::new(vec![
                        Keyframe::linear(0, 20.0),
                        Keyframe::linear(10, 80.0),
                    ])),
                    align: crate::text::TextAlign::Left,
                    max_width: None,
                },
            )),
        );
        let width_at = |f: f64| {
            kurbo::Shape::bounding_box(&evaluate(&doc, f).items[0].path).width()
        };
        assert!(width_at(10.0) > width_at(0.0), "the keyframed size drives the glyphs");
    }

    /// A missing font still draws (parley substitutes), so the *only* signal
    /// that the wrong typeface is on screen is this warning. It rides the same
    /// `scene.warnings` channel as a broken script, which is what puts it behind
    /// the comp bar's yellow indicator for free.
    #[test]
    fn a_missing_font_warns_but_still_draws() {
        let text = |family: &str| Shape::Text {
            content: "Hi".into(),
            family: family.into(),
            size: Value::constant(32.0),
            align: crate::text::TextAlign::Left,
            max_width: None,
        };
        let doc_with = |family: &str| {
            Document::new(
                640.0,
                480.0,
                Node::group(0, "root").with_child(Node::shape(1, "caption", text(family))),
            )
        };

        let scene = evaluate(&doc_with("NoSuchFontFamily-XYZZY"), 0.0);
        assert_eq!(scene.warnings.len(), 1, "the substitution is reported");
        assert_eq!(scene.warnings[0].0, NodeId(1), "blamed on the text layer");
        assert!(scene.warnings[0].1.contains("isn't installed"), "{}", scene.warnings[0].1);
        assert!(!scene.items[0].path.is_empty(), "and it still drew something");

        // The deliberate default must stay silent, or every new text layer
        // would ship with a warning on it.
        assert!(evaluate(&doc_with(""), 0.0).warnings.is_empty(), "blank family is silent");
    }

    /// Text must survive a save/load like any other shape — including the fields
    /// that aren't `Value`s.
    #[test]
    fn a_text_layer_round_trips_through_json() {
        let doc = Document::new(
            640.0,
            480.0,
            Node::group(0, "root").with_child(Node::shape(
                1,
                "caption",
                Shape::Text {
                    content: "two\nlines".into(),
                    family: "Georgia".into(),
                    size: Value::constant(31.0),
                    align: crate::text::TextAlign::Center,
                    max_width: Some(250.0),
                },
            )),
        );
        let back: Document =
            serde_json::from_str(&serde_json::to_string(&doc).unwrap()).unwrap();
        match back.root.children[0].shape.as_ref().unwrap() {
            Shape::Text { content, family, align, max_width, .. } => {
                assert_eq!(content, "two\nlines");
                assert_eq!(family, "Georgia");
                assert_eq!(*align, crate::text::TextAlign::Center);
                assert_eq!(*max_width, Some(250.0));
            }
            other => panic!("expected text, got {other:?}"),
        }
    }

    #[test]
    fn document_round_trips_through_json() {
        let doc = Document::new(
            640.0,
            480.0,
            Node::group(0, "root").with_child(Node::shape(
                1,
                "dot",
                Shape::Ellipse { size: Value::constant(Vec2::new(20.0, 20.0)) },
            )),
        );
        let json = serde_json::to_string(&doc).unwrap();
        let back: Document = serde_json::from_str(&json).unwrap();
        assert_eq!(back.root.children.len(), 1);
    }
}
