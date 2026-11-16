//! Decoded footage for the CPU rasterizer: the headless counterpart of the
//! editor's frame cache.
//!
//! Simpler than the editor's, because an offline render has no playhead to keep
//! up with: frames are decoded on demand through the same
//! [`crate::default_registry`] the editor uses, converted once to a premultiplied
//! [`Pixmap`], and shared between render threads.
//!
//! # Why each video keeps a stream open
//!
//! This used to decode every frame with [`Decoder::frame`](motion_core::asset::Decoder::frame):
//! one ffmpeg process per frame per clip, each seeking to the keyframe before
//! its target and decoding forward from there. On a 1080x1920 comp with two
//! 1080p clips that was ~95% of the render (4.8 fps with the footage, 84.6 fps
//! with solids in its place), the same start-up cost the editor's cache measured
//! at ~229ms against ~1.5ms for reading the next frame off an open stream.
//!
//! So each video gets one [`FrameStream`], walked forward. That fits the
//! renderer exactly: [`crate::render_in_order`] hands out frames in increasing
//! order, never more than the thread count apart. A thread that wants frame
//! `n` takes the clip's stream, walks it to `n` and caches every frame it steps
//! over, so the threads asking for the frames just behind `n` find them waiting
//! instead of seeking back. A stream is reopened only for a jump backwards past
//! the cache or too far forwards to walk (a time remap, a contact sheet).
//!
//! One stream per clip means one clip decodes on one thread at a time. That is
//! fine: different clips still decode in parallel, ffmpeg threads its own
//! decoder, and rasterizing stays parallel.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use motion_core::asset::{Asset, AssetId, AssetMeta, DecoderRegistry, Frame, FrameStream, ImagePaint};
use motion_core::Project;
use tiny_skia::Pixmap;

/// Decoded frames held at once, in bytes. A still is one entry for the whole
/// render; a video render touches each frame about once, so this only has to
/// cover the frames in flight across threads, the ones a walk stepped over on
/// their way to the threads that want them, and motion-blur sub-samples landing
/// on the same source frame. 1080p is ~8MB a frame, so this is ~60 of those.
const BUDGET_BYTES: usize = 512 * 1024 * 1024;

/// How far a stream is walked forward to reach a frame before reopening it at
/// that frame instead. Reading a frame off an open stream is ~1.5ms and
/// reopening is ~229ms plus decoding from the keyframe, so walking a couple of
/// seconds is still the cheaper of the two.
const MAX_WALK: i64 = 120;

type Key = (AssetId, i64);

/// One open stream on one clip.
struct Session {
    stream: Box<dyn FrameStream>,
}

#[derive(Default)]
struct Cache {
    frames: HashMap<Key, Arc<Pixmap>>,
    /// Insertion order, so the oldest go first. A render moves forward, so the
    /// oldest frame is the one furthest behind every thread.
    order: VecDeque<Key>,
    bytes: usize,
}

impl Cache {
    fn get(&self, key: &Key) -> Option<Arc<Pixmap>> {
        self.frames.get(key).cloned()
    }

    fn insert(&mut self, key: Key, px: Arc<Pixmap>) {
        let size = px.data().len();
        if self.frames.insert(key, px).is_some() {
            return;
        }
        self.order.push_back(key);
        self.bytes += size;
        // Never evict the frame just inserted: a frame bigger than the budget
        // still has to reach the thread that asked for it.
        while self.bytes > BUDGET_BYTES && self.order.len() > 1 {
            let Some(old) = self.order.pop_front() else { break };
            if let Some(px) = self.frames.remove(&old) {
                self.bytes -= px.data().len();
            }
        }
    }
}

pub struct Footage {
    assets: BTreeMap<AssetId, Asset>,
    registry: DecoderRegistry,
    frames: Mutex<Cache>,
    /// One stream slot per video asset. The outer map is fixed at construction,
    /// so the only locks a decode takes are its own clip's and, briefly, the
    /// cache's.
    streams: HashMap<AssetId, Mutex<Option<Session>>>,
}

impl Footage {
    pub fn new(project: &Project) -> Self {
        let streams = project.assets.iter().filter(|(_, a)| a.frames > 1).map(|(id, _)| (*id, Mutex::new(None))).collect();
        Footage { assets: project.assets.clone(), registry: crate::default_registry(), frames: Default::default(), streams }
    }

    /// Whether any footage exists to draw, so callers can skip building one.
    pub fn is_empty(&self) -> bool {
        self.assets.is_empty()
    }

    /// One frame, decoding it on a miss. The cache lock is never held while
    /// decoding, so different clips decode in parallel.
    pub fn frame(&self, paint: ImagePaint) -> Result<Arc<Pixmap>, String> {
        // A still has one frame whatever the layer asks for.
        let asset = self.assets.get(&paint.asset).ok_or_else(|| format!("asset {} is not in the project", paint.asset.0))?;
        let key = (paint.asset, if asset.frames <= 1 { 0 } else { paint.source_frame });
        if let Some(px) = self.frames.lock().unwrap().get(&key) {
            return Ok(px);
        }
        let decoder = self
            .registry
            .decoder_for(&asset.path)
            .ok_or_else(|| format!("{}: no decoder for this file type", asset.path.display()))?;
        if !asset.path.exists() {
            return Err(format!("{}: file not found", asset.path.display()));
        }
        let meta = asset.meta();
        if let Some(slot) = self.streams.get(&key.0) {
            if let Some(px) = self.from_stream(slot, asset, &meta, key)? {
                return Ok(px);
            }
        }
        // A still, a decoder with no stream, or a stream that ran out early. A
        // probed frame count can overstate the file by one (container vs stream
        // duration), so the last frame may not seek. Hold the one before it, as
        // the editor does, rather than drop the layer.
        let frame = decoder
            .frame(&asset.path, key.1, &meta)
            .or_else(|e| if key.1 > 0 { decoder.frame(&asset.path, key.1 - 1, &meta) } else { Err(e) })
            .map_err(|e| format!("{}: {e}", asset.path.display()))?;
        let px = Arc::new(to_pixmap(&frame)?);
        self.frames.lock().unwrap().insert(key, px.clone());
        Ok(px)
    }

    /// Serve `key` off the clip's open stream, walking or reopening it.
    /// `Ok(None)` when no stream can produce the frame (no sequential mode, or
    /// the stream ended before reaching it) and the caller should fall back.
    fn from_stream(&self, slot: &Mutex<Option<Session>>, asset: &Asset, meta: &AssetMeta, key: Key) -> Result<Option<Arc<Pixmap>>, String> {
        let mut slot = slot.lock().unwrap();
        // Another thread may have walked past this frame while we waited.
        if let Some(px) = self.frames.lock().unwrap().get(&key) {
            return Ok(Some(px));
        }
        let want = key.1;
        let reachable = slot.as_ref().is_some_and(|s| {
            let at = s.stream.next_index();
            want >= at && want - at <= MAX_WALK
        });
        if !reachable {
            // Dropping the old stream kills its ffmpeg.
            *slot = None;
            match self.registry.stream(&asset.path, want, meta) {
                Some(stream) => *slot = Some(Session { stream }),
                None => return Ok(None),
            }
        }
        let session = slot.as_mut().expect("opened above");
        loop {
            let at = session.stream.next_index();
            match session.stream.next_frame() {
                Ok(Some(frame)) => {
                    let px = Arc::new(to_pixmap(&frame)?);
                    // Every frame stepped over is cached: the threads just
                    // behind this one are about to ask for them.
                    self.frames.lock().unwrap().insert((key.0, at), px.clone());
                    if at >= want {
                        return Ok(Some(px));
                    }
                }
                Ok(None) => {
                    *slot = None;
                    return Ok(None);
                }
                Err(e) => {
                    *slot = None;
                    return Err(format!("{}: {e}", asset.path.display()));
                }
            }
        }
    }
}

/// Decoded RGBA to a premultiplied pixmap. Video is opaque, and premultiplying
/// an opaque pixel changes nothing, so the common case is a straight copy.
fn to_pixmap(frame: &Frame) -> Result<Pixmap, String> {
    let mut px = Pixmap::new(frame.width, frame.height).ok_or("empty frame")?;
    if frame.rgba.chunks_exact(4).all(|p| p[3] == 255) {
        px.data_mut().copy_from_slice(&frame.rgba);
    } else {
        for (src, dst) in frame.rgba.chunks_exact(4).zip(px.pixels_mut()) {
            *dst = tiny_skia::ColorU8::from_rgba(src[0], src[1], src[2], src[3]).premultiply();
        }
    }
    Ok(px)
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

    /// The open-stream path returns exactly the frame the per-frame seek does,
    /// however the frames are asked for: in order, out of order across threads
    /// the way `render_in_order` asks, backwards past the cache, and forwards
    /// past `MAX_WALK`. The per-frame decode is the reference because it is what
    /// this path replaced, and it is pinned to the in-process decoder by
    /// `libav::tests`.
    ///
    /// Skips when ffmpeg isn't installed, like the decoder's own tests.
    #[test]
    fn streamed_frames_match_the_per_frame_decode() {
        let made_ok = |s: std::io::Result<std::process::ExitStatus>| matches!(s, Ok(s) if s.success());
        let dir = std::env::temp_dir().join(format!("pbc_footage_stream_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let clip = dir.join("clip.mp4");
        // 200 frames, a keyframe every 30: long enough to walk, reopen past
        // MAX_WALK, and seek mid-GOP. Every frame differs (testsrc's counter).
        let made = std::process::Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-f", "lavfi", "-i", "testsrc=size=64x36:rate=25:duration=8"])
            .args(["-c:v", "libx264", "-g", "30", "-pix_fmt", "yuv420p"])
            .arg(&clip)
            .status();
        if !made_ok(made) {
            eprintln!("skipping: ffmpeg not on PATH or couldn't make a fixture");
            return;
        }
        let registry = crate::default_registry();
        let meta = registry.open(&clip).expect("probe the fixture");
        let aid = AssetId(3);
        let mut project = Project::single(motion_core::Document::new(64.0, 36.0, Node::group(0, "root")));
        project.assets.insert(aid, Asset::video(aid, &clip, meta.width, meta.height, meta.frames, meta.fps));
        let asset = project.assets[&aid].clone();
        let decoder = registry.decoder_for(&clip).unwrap();
        let reference = |n: i64| decoder.frame(&clip, n, &asset.meta()).unwrap().rgba;
        let paint = |n: i64| ImagePaint { asset: aid, source_frame: n };

        let footage = Footage::new(&project);
        let check = |n: i64| {
            let got = footage.frame(paint(n)).unwrap();
            assert_eq!(got.data(), &reference(n)[..], "frame {n}");
        };
        // Out of order across threads, the way the parallel renderer asks.
        std::thread::scope(|s| {
            for t in 0..4 {
                let check = &check;
                s.spawn(move || (t..60).step_by(4).for_each(|n| check(n)));
            }
        });
        // Backwards, forwards past MAX_WALK, mid-GOP, and the last frame.
        for n in [10, 5, 190, 100, 101, 45, meta.frames - 1] {
            check(n);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
