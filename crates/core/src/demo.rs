//! A hand-built demo document, shared by the offline SVG app and the live
//! GPU shell so both render the same scene.

use kurbo::Vec2;

use crate::vec3::Vec3;

use crate::node::{Document, Node, Shape, Transform};
use crate::value::{Color, Keyframe, Track, Value};

/// A red rounded square that slides across and eases, carrying a spinning
/// white dot. Exercises keyframes, temporal easing, parametric geometry
/// (the corner radius animates), transform composition, and nesting.
///
/// Keyframes are in frames: the animation spans `0..=END`, which at the
/// document's default 60fps is the first two seconds.
pub fn demo_document() -> Document {
    /// Last frame of the demo animation — 2s at the default 60fps.
    const END: i64 = 120;

    let dot = Node::shape(
        2,
        "dot",
        Shape::Ellipse { size: Value::constant(Vec2::new(40.0, 40.0)) },
    )
    .with_fill(Color::rgb(1.0, 1.0, 1.0))
    .with_transform(Transform {
        position: Value::constant(Vec3::flat(0.0, -120.0)),
        rotation: Value::Keyframed(Track::new(vec![
            Keyframe::linear(0, Vec3::ZERO),
            Keyframe::linear(END, Vec3::new(0.0, 0.0, 360.0)),
        ])),
        ..Transform::default()
    });

    let square = Node::shape(
        1,
        "square",
        Shape::Rect {
            size: Value::constant(Vec2::new(200.0, 200.0)),
            radius: Value::Keyframed(Track::new(vec![
                Keyframe::smooth(0, 0.0),
                Keyframe::smooth(END, 100.0),
            ])),
        },
    )
    .with_fill(Color::rgb(0.90, 0.25, 0.25))
    .with_transform(Transform {
        position: Value::Keyframed(Track::new(vec![
            Keyframe::smooth(0, Vec3::flat(300.0, 540.0)),
            Keyframe::smooth(END, Vec3::flat(1620.0, 540.0)),
        ])),
        rotation: Value::Keyframed(Track::new(vec![
            Keyframe::smooth(0, Vec3::ZERO),
            Keyframe::smooth(END, Vec3::new(0.0, 0.0, 90.0)),
        ])),
        ..Transform::default()
    })
    .with_child(dot);

    Document::new(1920.0, 1080.0, Node::group(0, "root").with_child(square))
}
