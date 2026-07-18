//! motion — runnable shell (pre-GUI).
//!
//! Builds a demo document in code, evaluates it at a series of times, and
//! writes each frame to an SVG in ./out/. This proves the full pipeline end to
//! end — document model → evaluation → render — before any GPU or window code
//! exists. Replace this binary with a winit + vello + egui shell later; the
//! engine underneath does not change.

use std::fs;
use std::path::Path;

use kurbo::Vec2;
use motion_core::{
    evaluate, Color, Document, Keyframe, Node, Shape, Track, Transform, Value,
};
use motion_render::scene_to_svg;

fn demo_document() -> Document {
    // A red square that slides across and eases, with a spinning child dot.
    let dot = Node::shape(
        2,
        "dot",
        Shape::Ellipse { size: Value::constant(Vec2::new(40.0, 40.0)) },
    )
    .with_fill(Color::rgb(1.0, 1.0, 1.0))
    .with_transform(Transform {
        position: Value::constant(Vec2::new(0.0, -120.0)),
        rotation_deg: Value::Keyframed(Track::new(vec![
            Keyframe::linear(0.0, 0.0),
            Keyframe::linear(2.0, 360.0),
        ])),
        ..Transform::default()
    });

    let square = Node::shape(
        1,
        "square",
        Shape::Rect {
            size: Value::constant(Vec2::new(200.0, 200.0)),
            radius: Value::Keyframed(Track::new(vec![
                Keyframe::smooth(0.0, 0.0),
                Keyframe::smooth(2.0, 100.0),
            ])),
        },
    )
    .with_fill(Color::rgb(0.90, 0.25, 0.25))
    .with_transform(Transform {
        position: Value::Keyframed(Track::new(vec![
            Keyframe::smooth(0.0, Vec2::new(300.0, 540.0)),
            Keyframe::smooth(2.0, Vec2::new(1620.0, 540.0)),
        ])),
        rotation_deg: Value::Keyframed(Track::new(vec![
            Keyframe::smooth(0.0, 0.0),
            Keyframe::smooth(2.0, 90.0),
        ])),
        ..Transform::default()
    })
    .with_child(dot);

    Document::new(1920.0, 1080.0, Node::group(0, "root").with_child(square))
}

fn main() {
    let doc = demo_document();
    let out_dir = Path::new("out");
    fs::create_dir_all(out_dir).expect("create out/");

    let bg = Color::rgb(0.08, 0.09, 0.11);
    let frames = 9;
    for i in 0..frames {
        let t = i as f64 / (frames - 1) as f64 * 2.0; // 0..2s
        let scene = evaluate(&doc, t);
        for (id, msg) in &scene.warnings {
            eprintln!("warning [node {}]: {msg}", id.0);
        }
        let svg = scene_to_svg(&scene, doc.width, doc.height, bg);
        let path = out_dir.join(format!("frame_{i:02}.svg"));
        fs::write(&path, svg).expect("write svg");
        println!(
            "t={t:.2}s  ->  {}  ({} items)",
            path.display(),
            scene.items.len()
        );
    }
    println!("\nDone. Open out/frame_*.svg to scrub the animation by hand.");
}
