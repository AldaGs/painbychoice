//! Footage: images and video, referenced by the document, decoded elsewhere.
//!
//! **The registry stores references, never pixels.** An [`Asset`] is a path
//! plus the metadata needed to lay the footage out in time and space (its
//! native size, how many frames it has, how fast they were shot). Decoding is
//! somebody else's job — see [`Decoder`] — and the decoded pixels live in the
//! renderer's cache, outside the document entirely.
//!
//! That split is what keeps [`crate::evaluate`] pure. A frame can be evaluated
//! in a `cargo test` with no files on disk and no GPU: the walk emits an
//! [`ImagePaint`] naming an asset and a source frame, and only the backend that
//! actually makes pixels ever opens the file. It also keeps a `.pbc` small and
//! portable, and makes relinking moved footage a one-field edit rather than a
//! re-import.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Stable identity for a piece of footage within a project.
///
/// Stable for the same reason [`crate::CompId`] is: layers hold one, so
/// removing an asset from the middle of the library must not silently repoint
/// every layer after it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct AssetId(pub u64);

/// What kind of footage this is.
///
/// Only the *time* behaviour differs — a still is footage with exactly one
/// frame — but the distinction is worth naming because it decides which
/// [`Decoder`] handles the file and whether time remapping means anything.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AssetKind {
    Image,
    Video,
    /// Sound with no picture. A video *with* sound is still [`AssetKind::Video`]
    /// — the kind says what the file is for, and the audio metadata below is
    /// populated for either, because a layer showing a clip should hear it too.
    Audio,
}

impl AssetKind {
    /// Whether this kind puts pixels on the screen. Audio does not, which is
    /// the one thing every drawing path needs to know about it.
    pub fn is_visual(self) -> bool {
        matches!(self, AssetKind::Image | AssetKind::Video)
    }
}

/// One piece of imported footage.
///
/// The metadata fields are **cached from the file at import**, not read on
/// demand: `evaluate` needs an image's native size to know how big to draw it
/// and its frame count to know how long it is, and neither may cost a disk
/// read in the middle of a pure function. The cost is that they can go stale if
/// the file changes underneath — [`Asset::relink`] re-imports rather than
/// patching, so a replaced file brings its real metadata with it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Asset {
    pub id: AssetId,
    /// What the project panel shows. Defaults to the file name, but it's a
    /// user-facing label — renaming it must not move the file.
    pub name: String,
    /// Where the footage lives. Stored as given (absolute today); a missing
    /// file is a *warning* at render time, never a load failure, so a project
    /// opened on another machine still shows its structure.
    pub path: PathBuf,
    pub kind: AssetKind,
    /// Native pixel size. Seeds a layer's `size` at import so footage lands at
    /// 100%, and is what "Fit to comp" measures against.
    pub width: f64,
    pub height: f64,
    /// How many frames the source has. `1` for a still.
    ///
    /// This is the **intrinsic content length** the layer time model never had:
    /// until now a `LayerTiming` could trim a layer but nothing knew how long
    /// its content actually was, so "the clip's own duration" was unanswerable.
    pub frames: i64,
    /// The rate the source frames were shot at. `0.0` for a still, which has no
    /// rate — checked rather than assumed, because dividing by it is how a
    /// source frame gets picked.
    pub fps: f64,
    /// Audio: samples per second. `0` when the asset carries no sound.
    ///
    /// Separate fields rather than reusing `frames`/`fps` for a sample count and
    /// a sample rate. The arithmetic would work and the confusion would not be
    /// worth it: a video with sound needs *both* pairs at once, and a field
    /// whose meaning depends on the kind is a field someone reads wrongly.
    /// `#[serde(default)]` on all three, so a pre-audio `.pbc` still loads.
    #[serde(default)]
    pub sample_rate: u32,
    /// Audio: channel count. `0` when the asset carries no sound.
    #[serde(default)]
    pub channels: u16,
    /// Audio: length in **sample frames** (one per channel-set, not per sample),
    /// which is the unit every position in the audio path is measured in.
    #[serde(default)]
    pub samples: u64,
}

impl Asset {
    /// A still image asset.
    pub fn image(id: AssetId, path: impl Into<PathBuf>, width: f64, height: f64) -> Self {
        let path = path.into();
        Self {
            id,
            name: default_name(&path),
            path,
            kind: AssetKind::Image,
            width,
            height,
            frames: 1,
            fps: 0.0,
            sample_rate: 0,
            channels: 0,
            samples: 0,
        }
    }

    /// A sound asset.
    ///
    /// `width`/`height`/`frames`/`fps` stay at their empty values: sound has no
    /// picture, and a zero size is what [`AssetKind::is_visual`] exists to keep
    /// anything from trying to draw.
    pub fn audio(
        id: AssetId,
        path: impl Into<PathBuf>,
        sample_rate: u32,
        channels: u16,
        samples: u64,
    ) -> Self {
        let path = path.into();
        Self {
            id,
            name: default_name(&path),
            path,
            kind: AssetKind::Audio,
            width: 0.0,
            height: 0.0,
            frames: 0,
            fps: 0.0,
            sample_rate,
            channels,
            samples,
        }
    }

    /// How long this asset's sound runs, in seconds. `0.0` when it has none.
    ///
    /// Guards the rate rather than assuming it, for the same reason [`Self::fps`]
    /// is checked before dividing: a malformed file reporting a zero rate would
    /// otherwise produce an infinite duration and a comp that never ends.
    pub fn audio_seconds(&self) -> f64 {
        if self.sample_rate == 0 {
            return 0.0;
        }
        self.samples as f64 / self.sample_rate as f64
    }

    /// Whether this asset carries sound — true for an audio file, and for a
    /// video whose import found an audio stream.
    pub fn has_audio(&self) -> bool {
        self.sample_rate > 0 && self.channels > 0 && self.samples > 0
    }

    /// A video asset.
    pub fn video(
        id: AssetId,
        path: impl Into<PathBuf>,
        width: f64,
        height: f64,
        frames: i64,
        fps: f64,
    ) -> Self {
        let path = path.into();
        Self {
            id,
            name: default_name(&path),
            path,
            kind: AssetKind::Video,
            width,
            height,
            frames: frames.max(1),
            fps,
            sample_rate: 0,
            channels: 0,
            samples: 0,
        }
    }

    /// Point this asset at a different file, taking the replacement's metadata.
    ///
    /// Deliberately a whole-metadata swap rather than a path edit: a relink
    /// that kept the old size and frame count would draw the new footage
    /// stretched and trimmed to the shape of the file it replaced, which looks
    /// like a rendering bug rather than a stale field. The user-facing `name`
    /// and the `id` survive, because those are what the document refers to.
    pub fn relink(&mut self, replacement: Asset) {
        let Asset { path, kind, width, height, frames, fps, .. } = replacement;
        self.path = path;
        self.kind = kind;
        self.width = width;
        self.height = height;
        self.frames = frames;
        self.fps = fps;
    }

    /// This asset's metadata, for handing to a decoder.
    ///
    /// The document caches these facts at import precisely so nothing has to
    /// re-read the file to learn them. A decoder that probed for its own
    /// metadata on every frame would make that cache pointless — and did, at
    /// ~37ms per frame, before this existed.
    pub fn meta(&self) -> AssetMeta {
        AssetMeta {
            kind: self.kind,
            width: self.width,
            height: self.height,
            frames: self.frames,
            fps: self.fps,
            sample_rate: self.sample_rate,
            channels: self.channels,
            samples: self.samples,
        }
    }

    /// How long this footage runs in a comp at `comp_fps`, in comp frames.
    ///
    /// A still has no duration of its own — it holds forever — so this is
    /// `None` for one, rather than `1`: "as long as you like" and "one frame"
    /// are different answers, and the caller that seeds a layer's out-point
    /// needs to tell them apart.
    pub fn duration_in_comp(&self, comp_fps: f64) -> Option<i64> {
        if self.kind == AssetKind::Image || self.fps <= 0.0 || comp_fps <= 0.0 {
            return None;
        }
        Some(((self.frames as f64) * comp_fps / self.fps).round().max(1.0) as i64)
    }

    /// Which frame of the source to show at `local_frame` of a comp running at
    /// `comp_fps`.
    ///
    /// Two things happen here. First the comp's clock is converted to the
    /// source's, so 30fps footage in a 60fps comp holds each frame for two —
    /// the same wall-clock mapping [`crate::Timebase`] does for keys, applied
    /// to pixels. Then the result is **clamped** into the source's range, so
    /// footage that runs out holds its last frame instead of vanishing or
    /// wrapping; that matches the layer time model, where a value outside a
    /// track clamps rather than disappearing.
    pub fn source_frame(&self, local_frame: f64, comp_fps: f64) -> i64 {
        if self.kind == AssetKind::Image {
            return 0;
        }
        let ratio = if self.fps > 0.0 && comp_fps > 0.0 { self.fps / comp_fps } else { 1.0 };
        self.clamp_frame(local_frame * ratio)
    }

    /// Clamp a source-frame number into the range this footage actually has.
    ///
    /// Public because time remapping produces a source frame directly — the
    /// user's curve is already in source frames — and it must be held to the
    /// same bounds as natural playback.
    pub fn clamp_frame(&self, source_frame: f64) -> i64 {
        if !source_frame.is_finite() {
            return 0;
        }
        (source_frame.floor() as i64).clamp(0, self.frames - 1)
    }
}

fn default_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "footage".to_string())
}

/// What the walk hands a renderer for one raster layer: which footage, and
/// which frame of it.
///
/// Note what is *not* here: no pixels, no texture handle, no file path. The
/// evaluated scene stays a pure description, and resolving this pair into an
/// actual image is the backend's job — which is what lets the SVG backend
/// embed a file reference while the vello backend uploads a texture, from the
/// same scene.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ImagePaint {
    pub asset: AssetId,
    /// Zero-based frame of the source. Always in range for the asset it names.
    pub source_frame: i64,
}

/// Decoded pixels for one frame of footage: tightly packed 8-bit RGBA.
///
/// One shape for stills and video alike, because everything above the decoder
/// treats a still as one-frame footage. Premultiplication is deliberately *not*
/// applied here — the compositor stage owns alpha handling, and a decoder that
/// premultiplied would quietly make its output unusable for keying.
#[derive(Clone, Debug, PartialEq)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    /// `width * height * 4` bytes, row-major, straight (non-premultiplied) RGBA.
    pub rgba: Vec<u8>,
}

impl Frame {
    pub fn new(width: u32, height: u32, rgba: Vec<u8>) -> Result<Self, DecodeError> {
        let want = width as usize * height as usize * 4;
        if rgba.len() != want {
            return Err(DecodeError::Malformed(format!(
                "expected {want} bytes for {width}x{height} RGBA, got {}",
                rgba.len()
            )));
        }
        Ok(Self { width, height, rgba })
    }
}

/// Why a decoder couldn't produce something.
#[derive(Clone, Debug, PartialEq)]
pub enum DecodeError {
    /// No registered decoder claimed the file.
    Unsupported(String),
    /// The file isn't where the asset says it is.
    Missing(PathBuf),
    /// It's there and claimed, but it didn't decode.
    Malformed(String),
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DecodeError::Unsupported(ext) => write!(f, "no decoder handles '{ext}' files"),
            DecodeError::Missing(p) => write!(f, "footage not found: {}", p.display()),
            DecodeError::Malformed(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for DecodeError {}

/// Reads footage off disk.
///
/// A trait with a registry behind it (rather than a `match` on file extension)
/// because this is one of the seams the plugin plan names: importers are
/// registered impls, and our built-ins go through the same door a third-party
/// one would. It is also the concrete escape hatch for the video backend — an
/// `ffmpeg` sidecar and in-process `ffmpeg-next` bindings are two impls of this
/// trait, and swapping them changes nothing above it.
pub trait Decoder: Send + Sync {
    /// A short name, for error messages and for telling two impls apart.
    fn name(&self) -> &str;

    /// Whether this decoder handles the file. Called in registration order;
    /// the first claimant wins.
    fn probe(&self, path: &Path) -> bool;

    /// Read the footage's metadata without decoding pixels. This is what
    /// import calls to build an [`Asset`].
    fn open(&self, path: &Path) -> Result<AssetMeta, DecodeError>;

    /// Decode one frame, out of order. `source_frame` is always in range for
    /// `meta`.
    ///
    /// `meta` is passed in rather than re-read because the caller already has
    /// it — see [`Asset::meta`]. This is random access, so it is the
    /// *expensive* path for anything that has to seek; prefer
    /// [`Decoder::stream`] whenever frames are wanted in order.
    fn frame(&self, path: &Path, source_frame: i64, meta: &AssetMeta)
        -> Result<Frame, DecodeError>;

    /// Open a sequential reader positioned at `from`.
    ///
    /// **This is the difference between a preview that plays and one that
    /// doesn't.** Playback asks for frames in order, and a decoder re-opened
    /// per frame pays its whole start-up cost every time: for the ffmpeg
    /// sidecar that measured ~229ms/frame, against ~1.5ms/frame read from a
    /// stream left open. Decoding was never the expensive part.
    ///
    /// `None` (the default) means "no sequential mode" and the caller falls
    /// back to [`Decoder::frame`] — which is right for a still, where one
    /// frame is already in order.
    fn stream(&self, _path: &Path, _from: i64, _meta: &AssetMeta) -> Option<Box<dyn FrameStream>> {
        None
    }
}

/// A decoder positioned in a piece of footage, handing out frames in order.
///
/// Deliberately forward-only: that is the whole shape of the thing that makes
/// it cheap. Offering a `seek` here would hide the fact that seeking means
/// discarding the stream and opening another one, which is exactly the cost
/// this trait exists to let callers avoid.
pub trait FrameStream: Send {
    /// The next frame. `Ok(None)` at the end of the footage.
    fn next_frame(&mut self) -> Result<Option<Frame>, DecodeError>;

    /// Which source frame [`FrameStream::next_frame`] will return next.
    fn next_index(&self) -> i64;
}

/// What [`Decoder::open`] reports: everything [`Asset`] caches, minus identity.
///
/// The audio fields default to zero via [`AssetMeta::visual`], so a decoder that
/// only knows about pixels does not have to mention them — but a decoder that
/// finds an audio stream in a video *can* report it, which is what lets one
/// layer carry both picture and sound.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct AssetMeta {
    pub kind: AssetKind,
    pub width: f64,
    pub height: f64,
    pub frames: i64,
    pub fps: f64,
    pub sample_rate: u32,
    pub channels: u16,
    pub samples: u64,
}

impl Default for AssetKind {
    fn default() -> Self {
        AssetKind::Image
    }
}

impl AssetMeta {
    /// Picture-only metadata: the common case, and the one every existing
    /// decoder reports.
    pub fn visual(kind: AssetKind, width: f64, height: f64, frames: i64, fps: f64) -> Self {
        AssetMeta { kind, width, height, frames, fps, ..Default::default() }
    }

    /// Sound-only metadata.
    pub fn sound(sample_rate: u32, channels: u16, samples: u64) -> Self {
        AssetMeta {
            kind: AssetKind::Audio,
            sample_rate,
            channels,
            samples,
            ..Default::default()
        }
    }

    /// The same metadata with an audio stream attached — how a video decoder
    /// reports a clip that has sound.
    pub fn with_audio(mut self, sample_rate: u32, channels: u16, samples: u64) -> Self {
        self.sample_rate = sample_rate;
        self.channels = channels;
        self.samples = samples;
        self
    }
}

impl AssetMeta {
    /// Build an asset for `path` from this metadata.
    pub fn into_asset(self, id: AssetId, path: impl Into<PathBuf>) -> Asset {
        let path = path.into();
        Asset {
            id,
            name: default_name(&path),
            path,
            kind: self.kind,
            width: self.width,
            height: self.height,
            // A sound has no frames, so the `max(1)` that keeps a still from
            // claiming zero must not invent one for audio.
            frames: if self.kind.is_visual() { self.frames.max(1) } else { self.frames },
            fps: self.fps,
            sample_rate: self.sample_rate,
            channels: self.channels,
            samples: self.samples,
        }
    }
}

/// The registered decoders, tried in order.
#[derive(Default)]
pub struct DecoderRegistry {
    decoders: Vec<Box<dyn Decoder>>,
}

impl DecoderRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a decoder. Later registrations are tried later, so a built-in
    /// registered first keeps its files — a plugin extends the set rather than
    /// silently capturing formats that already worked.
    pub fn register(&mut self, decoder: Box<dyn Decoder>) {
        self.decoders.push(decoder);
    }

    pub fn decoder_for(&self, path: &Path) -> Option<&dyn Decoder> {
        self.decoders.iter().map(|d| d.as_ref()).find(|d| d.probe(path))
    }

    /// Read `path`'s metadata with whichever decoder claims it.
    pub fn open(&self, path: &Path) -> Result<AssetMeta, DecodeError> {
        if !path.exists() {
            return Err(DecodeError::Missing(path.to_path_buf()));
        }
        match self.decoder_for(path) {
            Some(d) => d.open(path),
            None => Err(DecodeError::Unsupported(
                path.extension().map(|e| e.to_string_lossy().into_owned()).unwrap_or_default(),
            )),
        }
    }

    /// Decode one frame of `path`, out of order.
    pub fn frame(
        &self,
        path: &Path,
        source_frame: i64,
        meta: &AssetMeta,
    ) -> Result<Frame, DecodeError> {
        if !path.exists() {
            return Err(DecodeError::Missing(path.to_path_buf()));
        }
        match self.decoder_for(path) {
            Some(d) => d.frame(path, source_frame, meta),
            None => Err(DecodeError::Unsupported(
                path.extension().map(|e| e.to_string_lossy().into_owned()).unwrap_or_default(),
            )),
        }
    }

    /// Open a sequential reader on `path` at `from`, if its decoder has one.
    pub fn stream(
        &self,
        path: &Path,
        from: i64,
        meta: &AssetMeta,
    ) -> Option<Box<dyn FrameStream>> {
        if !path.exists() {
            return None;
        }
        self.decoder_for(path)?.stream(path, from, meta)
    }

    pub fn is_empty(&self) -> bool {
        self.decoders.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clip() -> Asset {
        // 48 frames at 24fps: two seconds of footage.
        Asset::video(AssetId(1), "/tmp/clip.mp4", 1920.0, 1080.0, 48, 24.0)
    }

    /// Footage slower than the comp holds each source frame for several comp
    /// frames — the same wall-clock mapping the timebase does for keys.
    #[test]
    fn source_frames_follow_wall_clock_not_frame_numbers() {
        let a = clip();
        // 24fps source in a 48fps comp: comp frame 10 is source frame 5.
        assert_eq!(a.source_frame(10.0, 48.0), 5);
        assert_eq!(a.source_frame(11.0, 48.0), 5);
        // Matching rates are the identity.
        assert_eq!(a.source_frame(10.0, 24.0), 10);
        // A faster source runs ahead.
        assert_eq!(a.source_frame(10.0, 12.0), 20);
    }

    /// Running past the end holds the last frame. Wrapping would silently
    /// restart the clip and vanishing would look like a dropped layer; a track
    /// clamps outside its keys, and footage clamps outside its frames.
    #[test]
    fn footage_that_runs_out_holds_its_last_frame() {
        let a = clip();
        assert_eq!(a.source_frame(1000.0, 24.0), 47);
        assert_eq!(a.source_frame(-5.0, 24.0), 0);
        assert_eq!(a.clamp_frame(f64::NAN), 0);
    }

    /// A still is footage with one frame, so every moment of it is frame 0 —
    /// but it has no *duration*, because it holds for as long as you leave it
    /// on screen.
    #[test]
    fn a_still_has_one_frame_and_no_duration() {
        let a = Asset::image(AssetId(2), "/tmp/logo.png", 512.0, 512.0);
        assert_eq!(a.frames, 1);
        assert_eq!(a.source_frame(900.0, 24.0), 0);
        assert_eq!(a.duration_in_comp(24.0), None);
    }

    /// A clip's length in the comp is wall-clock, so it stretches when the comp
    /// runs at a different rate. This is the intrinsic content length the layer
    /// time model never had.
    #[test]
    fn a_clips_duration_converts_into_comp_frames() {
        let a = clip();
        assert_eq!(a.duration_in_comp(24.0), Some(48));
        assert_eq!(a.duration_in_comp(48.0), Some(96));
        assert_eq!(a.duration_in_comp(12.0), Some(24));
    }

    /// Relinking takes the replacement's metadata wholesale: keeping the old
    /// size or frame count would draw the new footage stretched and trimmed to
    /// the shape of the file it replaced.
    #[test]
    fn relinking_takes_the_replacements_metadata_but_keeps_identity() {
        let mut a = clip();
        a.name = "background".into();
        a.relink(Asset::video(AssetId(99), "/tmp/other.mov", 1280.0, 720.0, 100, 30.0));
        assert_eq!(a.id, AssetId(1), "the document refers to this id");
        assert_eq!(a.name, "background", "a user's label survives a relink");
        assert_eq!((a.width, a.height), (1280.0, 720.0));
        assert_eq!(a.frames, 100);
        assert_eq!(a.path, PathBuf::from("/tmp/other.mov"));
    }

    /// A frame whose buffer doesn't match its dimensions is refused at
    /// construction rather than read out of bounds later.
    #[test]
    fn a_frames_buffer_must_match_its_dimensions() {
        assert!(Frame::new(2, 2, vec![0; 16]).is_ok());
        assert!(matches!(Frame::new(2, 2, vec![0; 15]), Err(DecodeError::Malformed(_))));
    }
}
