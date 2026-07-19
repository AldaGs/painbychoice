//! Evaluation: the pure function `(Document, t) -> Scene`. Scrubbing to any
//! time is just calling this with a different `t`; nothing is cached or baked,
//! which is what makes the whole thing non-linear and non-destructive.

use kurbo::{Affine, BezPath};

use crate::expr::EvalCtx;
use crate::node::{Document, Node, NodeId};
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
    walk(&doc.root, Affine::IDENTITY, 1.0, &mut ctx, &mut scene);
    scene.warnings.append(&mut ctx.take_warnings());
    scene
}

fn walk(node: &Node, parent_xf: Affine, parent_opacity: f64, ctx: &mut EvalCtx, scene: &mut Scene) {
    // Everything resolved below belongs to this node, so a warning raised deep
    // in an expression (a bad script, an ambiguous name) is tagged with it.
    let prev_node = ctx.enter_node(node.id);
    let (local_xf, local_opacity) = node.transform.resolve(ctx);
    let xf = parent_xf * local_xf;
    let opacity = parent_opacity * local_opacity.clamp(0.0, 1.0);

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

    // Children are walked *inside* this node's mark only in the sense that each
    // re-marks itself; restore ours first so a sibling can't inherit it.
    ctx.exit_node(prev_node);

    for child in &node.children {
        walk(child, xf, opacity, ctx, scene);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::{Node, Shape, Transform};
    use crate::value::{Keyframe, Track, Value};
    use kurbo::Vec2;

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
