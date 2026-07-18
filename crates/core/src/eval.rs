//! Evaluation: the pure function `(Document, t) -> Scene`. Scrubbing to any
//! time is just calling this with a different `t`; nothing is cached or baked,
//! which is what makes the whole thing non-linear and non-destructive.

use kurbo::{Affine, BezPath};

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

/// Evaluate a document at time `t` (in seconds) into a flat `Scene`.
pub fn evaluate(doc: &Document, t: f64) -> Scene {
    let mut scene = Scene::default();
    walk(&doc.root, Affine::IDENTITY, 1.0, t, &mut scene);
    scene
}

fn walk(node: &Node, parent_xf: Affine, parent_opacity: f64, t: f64, scene: &mut Scene) {
    let (local_xf, local_opacity) = node.transform.resolve(t);
    let xf = parent_xf * local_xf;
    let opacity = parent_opacity * local_opacity.clamp(0.0, 1.0);

    if let Some(shape) = &node.shape {
        let path = shape.to_path(t);
        let fill = node.fill.as_ref().map(|f| f.resolve(t));
        let stroke = node
            .stroke
            .as_ref()
            .map(|s| (s.color.resolve(t), s.width.resolve(t)));

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

    for child in &node.children {
        walk(child, xf, opacity, t, scene);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::{Node, Shape, Transform};
    use crate::value::{Keyframe, Track, Value};
    use kurbo::Vec2;

    /// The canonical smoke test: a keyframed square whose position animates.
    /// At t=0.5 it should sit exactly halfway between its two keys, and the
    /// evaluated render item's transform must reflect that.
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
                Keyframe::linear(0.0, Vec2::new(0.0, 0.0)),
                Keyframe::linear(1.0, Vec2::new(200.0, 100.0)),
            ])),
            ..Transform::default()
        });

        let doc = Document::new(1920.0, 1080.0, Node::group(0, "root").with_child(square));

        let scene = evaluate(&doc, 0.5);
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
