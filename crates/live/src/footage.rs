//! The decoded-frame cache: the one place in the editor that turns an
//! [`ImagePaint`] into actual pixels.
//!
//! This is the other half of the split `core` makes. The document holds
//! references and `evaluate` stays pure; everything that touches a file, costs
//! milliseconds, or has to be thrown away when memory runs short lives here, in
//! the shell — which is why the engine can still be tested by rendering a frame
//! in `cargo test`.

use std::collections::HashMap;
use std::path::PathBuf;

use motion_core::asset::{Asset, AssetId, DecodeError, DecoderRegistry, ImagePaint};
use vello::peniko::{Blob, ImageAlphaType, ImageData, ImageFormat};

/// How much decoded footage to keep resident, in bytes.
///
/// A budget rather than a frame count because frames differ in size by two
/// orders of magnitude — a hundred 4K frames is 3GB, a hundred icons is
/// nothing, and only one of those numbers is a sensible limit.
const BUDGET_BYTES: usize = 512 * 1024 * 1024;

/// One cached frame, with the clock reading that makes eviction possible.
struct Cached {
    image: ImageData,
    bytes: usize,
    /// When it was last drawn. A plain counter, not a timestamp: the only
    /// question ever asked of it is which entry is oldest.
    used: u64,
}

/// Decoded footage frames, keyed by source and frame number.
pub(crate) struct FootageCache {
    decoders: DecoderRegistry,
    frames: HashMap<(AssetId, i64), Cached>,
    /// Frames that failed to decode, so a broken file is attempted **once**
    /// rather than re-shelling out to ffmpeg sixty times a second. Cleared
    /// wholesale by [`FootageCache::clear`]; a per-asset retry arrives with
    /// relinking, which is the user saying "try again".
    failed: HashMap<(AssetId, i64), String>,
    bytes: usize,
    clock: u64,
}

impl FootageCache {
    pub(crate) fn new(decoders: DecoderRegistry) -> Self {
        Self {
            decoders,
            frames: HashMap::new(),
            failed: HashMap::new(),
            bytes: 0,
            clock: 0,
        }
    }

    /// Read a file's metadata, for import. Returns the error verbatim so the
    /// UI can say *why* (missing ffmpeg reads very differently from a corrupt
    /// file).
    pub(crate) fn probe(&self, path: &PathBuf) -> Result<motion_core::AssetMeta, DecodeError> {
        self.decoders.open(path)
    }

    /// The pixels for one paint, decoding on demand.
    ///
    /// `None` means "nothing to draw here" — missing file, no decoder, a
    /// corrupt frame. The caller draws the layer's rectangle instead, so a
    /// broken import still shows *where* it is rather than vanishing.
    pub(crate) fn image(&mut self, asset: &Asset, paint: ImagePaint) -> Option<&ImageData> {
        let key = (paint.asset, paint.source_frame);
        self.clock += 1;
        if self.failed.contains_key(&key) {
            return None;
        }
        if !self.frames.contains_key(&key) {
            match self.decoders.frame(&asset.path, paint.source_frame) {
                Ok(frame) => {
                    let bytes = frame.rgba.len();
                    let image = ImageData {
                        data: Blob::new(std::sync::Arc::new(frame.rgba)),
                        format: ImageFormat::Rgba8,
                        // Straight, not premultiplied: the decoders deliberately
                        // don't premultiply, because a keyer needs the original
                        // colour behind a transparent pixel.
                        alpha_type: ImageAlphaType::Alpha,
                        width: frame.width,
                        height: frame.height,
                    };
                    self.frames.insert(key, Cached { image, bytes, used: self.clock });
                    self.bytes += bytes;
                    self.evict_to_budget();
                }
                Err(e) => {
                    self.failed.insert(key, e.to_string());
                    return None;
                }
            }
        }
        let entry = self.frames.get_mut(&key)?;
        entry.used = self.clock;
        Some(&entry.image)
    }

    /// Why this frame didn't decode, if it didn't.
    pub(crate) fn error(&self, paint: ImagePaint) -> Option<&str> {
        self.failed.get(&(paint.asset, paint.source_frame)).map(|s| s.as_str())
    }

    /// Drop everything, including the recorded failures.
    ///
    /// **Required when the open project changes.** Asset ids are per-project,
    /// so `AssetId(3)` in the file just loaded is a different piece of footage
    /// from `AssetId(3)` in the one being closed — keeping the cache would draw
    /// the old project's pixels under the new project's layers.
    pub(crate) fn clear(&mut self) {
        self.frames.clear();
        self.failed.clear();
        self.bytes = 0;
    }

    /// Drop least-recently-drawn frames until the budget is met.
    ///
    /// Scrubbing a timeline touches frames in a sweep, so least-recently-used
    /// is the right policy almost by definition: the frames furthest behind the
    /// playhead are the ones least likely to be wanted next.
    fn evict_to_budget(&mut self) {
        while self.bytes > BUDGET_BYTES && self.frames.len() > 1 {
            let Some(oldest) = self.frames.iter().min_by_key(|(_, c)| c.used).map(|(k, _)| *k)
            else {
                break;
            };
            if let Some(c) = self.frames.remove(&oldest) {
                self.bytes -= c.bytes;
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn resident_bytes(&self) -> usize {
        self.bytes
    }
}
