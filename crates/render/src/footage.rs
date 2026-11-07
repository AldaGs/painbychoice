//! Decoded footage for the CPU rasterizer: the headless counterpart of the
//! editor's frame cache.
//!
//! Much simpler than the editor's, because an offline render has no playhead to
//! keep up with: frames are decoded on demand through the same
//! [`crate::default_registry`] the editor uses, converted once to a premultiplied
//! [`Pixmap`], and shared between render threads.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

use motion_core::asset::{Asset, AssetId, DecoderRegistry, ImagePaint};
use motion_core::Project;
use tiny_skia::Pixmap;

/// Frames held at once. A still is one entry for the whole render; a video
/// render touches each frame about once, so this only has to cover the frames
/// in flight across threads, plus motion-blur sub-samples landing on the same
/// source frame.
// ponytail: wholesale clear when full; an LRU only matters if many clips
// interleave out of order.
const CAP: usize = 256;

pub struct Footage {
    assets: BTreeMap<AssetId, Asset>,
    registry: DecoderRegistry,
    frames: Mutex<HashMap<(AssetId, i64), Arc<Pixmap>>>,
}

impl Footage {
    pub fn new(project: &Project) -> Self {
        Footage { assets: project.assets.clone(), registry: crate::default_registry(), frames: Default::default() }
    }

    /// Whether any footage exists to draw, so callers can skip building one.
    pub fn is_empty(&self) -> bool {
        self.assets.is_empty()
    }

    /// One frame, decoding it on a miss. The lock is not held while decoding,
    /// so threads decode different frames in parallel; two threads racing for
    /// the same frame just decode it twice.
    pub fn frame(&self, paint: ImagePaint) -> Result<Arc<Pixmap>, String> {
        // A still has one frame whatever the layer asks for.
        let asset = self.assets.get(&paint.asset).ok_or_else(|| format!("asset {} is not in the project", paint.asset.0))?;
        let key = (paint.asset, if asset.frames <= 1 { 0 } else { paint.source_frame });
        if let Some(px) = self.frames.lock().unwrap().get(&key) {
            return Ok(px.clone());
        }
        let decoder = self
            .registry
            .decoder_for(&asset.path)
            .ok_or_else(|| format!("{}: no decoder for this file type", asset.path.display()))?;
        if !asset.path.exists() {
            return Err(format!("{}: file not found", asset.path.display()));
        }
        let meta = asset.meta();
        // A probed frame count can overstate the file by one (container vs
        // stream duration), so the last frame may not seek. Hold the one
        // before it, as the editor does, rather than drop the layer.
        let frame = decoder
            .frame(&asset.path, key.1, &meta)
            .or_else(|e| if key.1 > 0 { decoder.frame(&asset.path, key.1 - 1, &meta) } else { Err(e) })
            .map_err(|e| format!("{}: {e}", asset.path.display()))?;
        let mut px = Pixmap::new(frame.width, frame.height).ok_or("empty frame")?;
        for (src, dst) in frame.rgba.chunks_exact(4).zip(px.pixels_mut()) {
            *dst = tiny_skia::ColorU8::from_rgba(src[0], src[1], src[2], src[3]).premultiply();
        }
        let px = Arc::new(px);
        let mut frames = self.frames.lock().unwrap();
        if frames.len() >= CAP {
            frames.clear();
        }
        frames.insert(key, px.clone());
        Ok(px)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use motion_core::{evaluate_comp_sample, mat4::Mat4, node::Shape, Color, Node, Value};

    /// A PNG on disk reaches the rasterized pixels, where it used to be a note.
    #[test]
    fn a_still_is_drawn_into_the_frame() {
        let dir = std::env::temp_dir().join(format!("pbc_footage_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let png = dir.join("red.png");
        image::save_buffer(&png, &[255, 0, 0, 255].repeat(16), 4, 4, image::ColorType::Rgba8).unwrap();

        let aid = AssetId(0);
        let mut layer = Node::group(9, "still");
        layer.shape = Some(Shape::Image { asset: aid, size: Value::constant(kurbo::Vec2::new(10.0, 10.0)), time_remap: None });
        layer.transform.position = Value::constant(motion_core::vec3::Vec3::new(10.0, 10.0, 0.0));
        let mut project = Project::single(motion_core::Document::new(20.0, 20.0, Node::group(0, "root").with_child(layer)));
        project.assets.insert(aid, Asset::image(aid, &png, 4.0, 4.0));
        let root = project.root;

        let scene = evaluate_comp_sample(&project, root, 0.0, 0.0, Mat4::IDENTITY);
        let footage = Footage::new(&project);
        let (px, report) = crate::rasterize_with(&scene, 20, 20, Color::rgb(0.0, 0.0, 0.0), 1.0, Some(&footage)).unwrap();
        assert!(report.is_empty(), "{report:?}");
        let at = |x: usize, y: usize| &px[(y * 20 + x) * 4..][..3];
        assert_eq!(at(10, 10), [255, 0, 0], "centre of the 10x10 placed at (10,10)");
        assert_eq!(at(1, 1), [0, 0, 0], "outside it");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
