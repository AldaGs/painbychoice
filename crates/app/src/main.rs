//! motion — runnable shell (pre-GUI).
//!
//! Builds a demo document in code, evaluates it at a series of times, and
//! writes each frame to an SVG in ./out/. This proves the full pipeline end to
//! end — document model → evaluation → render — before any GPU or window code
//! exists. Replace this binary with a winit + vello + egui shell later; the
//! engine underneath does not change.

use std::fs;
use std::path::Path;

use motion_core::{demo::demo_document, evaluate, Color};
use motion_render::scene_to_svg;

fn main() {
    let doc = demo_document();
    let out_dir = Path::new("out");
    fs::create_dir_all(out_dir).expect("create out/");

    let bg = Color::rgb(0.08, 0.09, 0.11);
    let tb = doc.timebase();
    // Sample 9 evenly-spaced instants across the first 2 seconds. `evaluate`
    // works in frames, so convert once here — this binary is an "edge" and
    // seconds are a presentation unit.
    let samples = 9;
    let last_frame = tb.seconds_to_frames(2.0);
    for i in 0..samples {
        let frame = (i as f64 / (samples - 1) as f64 * last_frame as f64).round();
        let scene = evaluate(&doc, frame);
        for (id, msg) in &scene.warnings {
            eprintln!("warning [node {}]: {msg}", id.0);
        }
        // The demo document is a bare comp with no project behind it, so it has
        // no footage library to hand over.
        let svg = scene_to_svg(&scene, doc.width, doc.height, bg, &[]);
        let path = out_dir.join(format!("frame_{i:02}.svg"));
        fs::write(&path, svg).expect("write svg");
        println!(
            "{}  (frame {frame})  ->  {}  ({} items)",
            tb.timecode(frame),
            path.display(),
            scene.items.len()
        );
    }
    println!("\nDone. Open out/frame_*.svg to scrub the animation by hand.");
}
