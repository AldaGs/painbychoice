//! The built-in [`Decoder`] impls: stills through the `image` crate, video
//! through an `ffmpeg` sidecar.
//!
//! They live in `render` rather than `core` for the same reason the SVG backend
//! does — this is the crate that turns descriptions into pixels, and `core`
//! stays a pure value engine with no file formats in it. They are registered
//! through the same [`DecoderRegistry`] a third-party importer would use; the
//! built-ins get no special door.

use std::path::Path;
use std::process::Command;

use motion_core::asset::{
    AssetKind, AssetMeta, DecodeError, Decoder, DecoderRegistry, Frame, FrameStream,
};

/// The decoders PBC ships with, in probe order.
///
/// Stills first: they're cheap to probe and the common case, and an image
/// decoder that claims a file must not have to out-argue a video one over a
/// `.png`.
pub fn default_registry() -> DecoderRegistry {
    let mut reg = DecoderRegistry::new();
    reg.register(Box::new(ImageDecoder));
    reg.register(Box::new(PsdDecoder::default()));
    reg.register(Box::new(AiDecoder::default()));
    reg.register(Box::new(RawDecoder));
    reg.register(Box::new(HeicDecoder::new()));
    reg.register(Box::new(FfmpegDecoder::new()));
    reg
}

fn extension(path: &Path) -> String {
    path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default()
}

/// The `ffmpeg` binary to invoke, overridable so a bundled build can point at
/// its own instead of whatever is on PATH. Shared by every decoder that shells
/// out to it — video *and* HEIC stills.
fn ffmpeg_bin() -> String {
    std::env::var("PBC_FFMPEG").unwrap_or_else(|_| "ffmpeg".into())
}

/// The `ffprobe` binary to invoke. See [`ffmpeg_bin`].
fn ffprobe_bin() -> String {
    std::env::var("PBC_FFPROBE").unwrap_or_else(|_| "ffprobe".into())
}

/// Still images, via the `image` crate.
pub struct ImageDecoder;

const IMAGE_EXTS: &[&str] =
    &["png", "jpg", "jpeg", "gif", "bmp", "tif", "tiff", "webp", "tga", "ico", "qoi"];

impl Decoder for ImageDecoder {
    fn name(&self) -> &str {
        "image"
    }

    fn probe(&self, path: &Path) -> bool {
        IMAGE_EXTS.contains(&extension(path).as_str())
    }

    fn open(&self, path: &Path) -> Result<AssetMeta, DecodeError> {
        // Dimensions only — this runs at import to build the asset, and
        // decoding the whole bitmap to learn how wide it is would make
        // importing a folder of 4K stills needlessly slow.
        let (w, h) = image::image_dimensions(path)
            .map_err(|e| DecodeError::Malformed(format!("{}: {e}", path.display())))?;
        Ok(AssetMeta::visual(AssetKind::Image, w as f64, h as f64, 1, 0.0))
    }

    fn frame(
        &self,
        path: &Path,
        _source_frame: i64,
        _meta: &AssetMeta,
    ) -> Result<Frame, DecodeError> {
        // `source_frame` is ignored rather than checked: a still is one-frame
        // footage, and `Asset::source_frame` already clamps every request on it
        // to 0. An animated GIF is read as its first frame today.
        let img = image::open(path)
            .map_err(|e| DecodeError::Malformed(format!("{}: {e}", path.display())))?
            .to_rgba8();
        let (w, h) = img.dimensions();
        Frame::new(w, h, img.into_raw())
    }
}

/// Photoshop documents: the flattened image, or one layer of it when the
/// asset names one (After Effects' "import as composition" footage).
///
/// Parsed files are kept: a layered PSD backs one asset per layer, and each
/// would otherwise parse the whole document again.
#[derive(Default)]
pub struct PsdDecoder {
    cache: std::sync::Mutex<std::collections::HashMap<std::path::PathBuf, std::sync::Arc<psd::Psd>>>,
}

impl PsdDecoder {
    fn load(&self, path: &Path) -> Result<std::sync::Arc<psd::Psd>, DecodeError> {
        if let Some(p) = self.cache.lock().unwrap().get(path) {
            return Ok(p.clone());
        }
        let bytes = std::fs::read(path).map_err(|e| DecodeError::Malformed(format!("{}: {e}", path.display())))?;
        // The crate panics on some documents it does not support (e.g. 16-bit
        // channels); that is a bad file, not a crashed render.
        let parsed = std::panic::catch_unwind(|| psd::Psd::from_bytes(&bytes))
            .map_err(|_| DecodeError::Malformed(format!("{}: unsupported PSD", path.display())))?
            .map_err(|e| DecodeError::Malformed(format!("{}: {e}", path.display())))?;
        let p = std::sync::Arc::new(parsed);
        self.cache.lock().unwrap().insert(path.to_path_buf(), p.clone());
        Ok(p)
    }
}

impl Decoder for PsdDecoder {
    fn name(&self) -> &str {
        "psd"
    }
    fn probe(&self, path: &Path) -> bool {
        matches!(extension(path).as_str(), "psd" | "psb")
    }
    fn open(&self, path: &Path) -> Result<AssetMeta, DecodeError> {
        let p = self.load(path)?;
        Ok(AssetMeta::visual(AssetKind::Image, p.width() as f64, p.height() as f64, 1, 0.0))
    }
    fn frame(&self, path: &Path, _source_frame: i64, meta: &AssetMeta) -> Result<Frame, DecodeError> {
        let p = self.load(path)?;
        let (w, h) = (p.width(), p.height());
        let Some(want) = &meta.layer else {
            return Frame::new(w, h, std::panic::catch_unwind(|| p.rgba()).map_err(|_| bad(path))?);
        };
        // AE counts group dividers in its layer index and the crate does not,
        // so the layer is found by name, and by its box when names repeat.
        let [l, t, r, b] = want.bounds.map(|v| v.round() as i32);
        let named: Vec<&psd::PsdLayer> = p.layers().iter().filter(|x| x.name().trim() == want.name.trim()).collect();
        let layer = named
            .iter()
            .find(|x| (x.layer_left(), x.layer_top(), x.layer_right(), x.layer_bottom()) == (l, t, r, b))
            .or(named.first())
            .ok_or_else(|| DecodeError::Malformed(format!("{}: no layer {:?}", path.display(), want.name)))?;
        let canvas = std::panic::catch_unwind(|| layer.rgba()).map_err(|_| bad(path))?;
        if want.full_frame || canvas.len() != (w * h * 4) as usize {
            return Frame::new(w, h, canvas);
        }
        // Layer Size: just the layer's box, clipped to the canvas.
        let (x0, y0) = (l.clamp(0, w as i32) as u32, t.clamp(0, h as i32) as u32);
        let (x1, y1) = (r.clamp(0, w as i32) as u32, b.clamp(0, h as i32) as u32);
        let (cw, ch) = (x1.saturating_sub(x0).max(1), y1.saturating_sub(y0).max(1));
        let mut out = vec![0u8; (cw * ch * 4) as usize];
        for y in 0..ch.min(y1 - y0) {
            let src = (((y0 + y) * w + x0) * 4) as usize;
            let dst = (y * cw * 4) as usize;
            out[dst..dst + ((x1 - x0) * 4) as usize].copy_from_slice(&canvas[src..src + ((x1 - x0) * 4) as usize]);
        }
        Frame::new(cw, ch, out)
    }
}

/// A PDF text string: UTF-16BE behind a byte-order mark, otherwise
/// PDFDocEncoding, which matches Latin-1 for the accents layer names use.
fn pdf_text(b: &[u8]) -> String {
    match b {
        [0xFE, 0xFF, rest @ ..] => {
            String::from_utf16_lossy(&rest.chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect::<Vec<_>>())
        }
        _ => b.iter().map(|&c| c as char).collect(),
    }
}

/// What [`AiDecoder::only_layer`] says about a file with no PDF layers.
const NO_LAYERS: &str = "no PDF layers";

/// Illustrator files (and PDFs), rasterized at one pixel per point — what
/// After Effects shows at 100% — through `hayro`. An `.ai` saved with PDF
/// compatibility (Illustrator's default) is a PDF; one layer of it is shown by
/// switching every other optional-content group off with `lopdf` first.
#[derive(Default)]
pub struct AiDecoder {
    cache: std::sync::Mutex<std::collections::HashMap<std::path::PathBuf, std::sync::Arc<Vec<u8>>>>,
}

impl AiDecoder {
    fn bytes(&self, path: &Path) -> Result<std::sync::Arc<Vec<u8>>, DecodeError> {
        if let Some(b) = self.cache.lock().unwrap().get(path) {
            return Ok(b.clone());
        }
        let b = std::sync::Arc::new(std::fs::read(path).map_err(|e| DecodeError::Malformed(format!("{}: {e}", path.display())))?);
        self.cache.lock().unwrap().insert(path.to_path_buf(), b.clone());
        Ok(b)
    }

    /// `pdf` with only the optional-content group called `name` on.
    fn only_layer(pdf: &[u8], name: &str) -> Result<Vec<u8>, String> {
        use lopdf::{Dictionary, Object};
        let mut doc = lopdf::Document::load_mem(pdf).map_err(|e| e.to_string())?;
        let props = match doc.catalog().map_err(|e| e.to_string())?.get(b"OCProperties").map_err(|_| NO_LAYERS.to_string())? {
            Object::Reference(r) => doc.get_object(*r).and_then(Object::as_dict).map_err(|e| e.to_string())?.clone(),
            o => o.as_dict().map_err(|e| e.to_string())?.clone(),
        };
        let ocgs = props.get(b"OCGs").and_then(Object::as_array).map_err(|e| e.to_string())?;
        let want = ocgs
            .iter()
            .filter_map(|o| o.as_reference().ok())
            .find(|r| {
                let n = doc.get_object(*r).and_then(Object::as_dict).and_then(|d| d.get(b"Name")).ok();
                n.and_then(|n| n.as_str().ok()).is_some_and(|n| pdf_text(n).trim() == name.trim())
            })
            .ok_or_else(|| format!("no layer {name:?}"))?;
        let mut config = Dictionary::new();
        config.set("BaseState", Object::Name(b"OFF".to_vec()));
        config.set("ON", Object::Array(vec![Object::Reference(want)]));
        config.set("OFF", Object::Array(vec![]));
        let mut props = props;
        props.set("D", Object::Dictionary(config));
        doc.catalog_mut().map_err(|e| e.to_string())?.set("OCProperties", Object::Dictionary(props));
        let mut out = Vec::new();
        doc.save_to(&mut out).map_err(|e| e.to_string())?;
        Ok(out)
    }

    fn render(path: &Path, pdf: Vec<u8>) -> Result<(u32, u32, Vec<u8>), DecodeError> {
        let err = |m: String| DecodeError::Malformed(format!("{}: {m}", path.display()));
        let pdf = hayro::hayro_syntax::Pdf::new(pdf).map_err(|e| err(format!("{e:?}")))?;
        let page = pdf.pages().first().ok_or_else(|| err("no pages".into()))?;
        let pix = hayro::render(
            page,
            &hayro::RenderCache::new(),
            &hayro::hayro_interpret::InterpreterSettings::default(),
            &hayro::RenderSettings::default(),
            &hayro::PixmapSettings::default(),
        );
        let mut rgba = Vec::with_capacity(pix.data().len() * 4);
        for p in pix.data() {
            let un = |c: u8| if p.a == 0 { 0 } else { ((c as u32 * 255) / p.a as u32).min(255) as u8 };
            rgba.extend_from_slice(&[un(p.r), un(p.g), un(p.b), p.a]);
        }
        Ok((pix.width() as u32, pix.height() as u32, rgba))
    }
}

impl Decoder for AiDecoder {
    fn name(&self) -> &str {
        "ai"
    }
    fn probe(&self, path: &Path) -> bool {
        matches!(extension(path).as_str(), "ai" | "pdf")
    }
    fn open(&self, path: &Path) -> Result<AssetMeta, DecodeError> {
        let pdf = hayro::hayro_syntax::Pdf::new(self.bytes(path)?.to_vec())
            .map_err(|e| DecodeError::Malformed(format!("{}: {e:?}", path.display())))?;
        let (w, h) = pdf.pages().first().map(|p| p.render_dimensions()).unwrap_or((0.0, 0.0));
        Ok(AssetMeta::visual(AssetKind::Image, w as f64, h as f64, 1, 0.0))
    }
    fn frame(&self, path: &Path, _source_frame: i64, meta: &AssetMeta) -> Result<Frame, DecodeError> {
        let bytes = self.bytes(path)?;
        let pdf = match &meta.layer {
            // A file without PDF layers (Illustrator's "Create Acrobat
            // Layers" off) can only be drawn whole.
            Some(l) if !l.name.is_empty() => match Self::only_layer(&bytes, &l.name) {
                Ok(pdf) => pdf,
                Err(m) if m == NO_LAYERS => bytes.to_vec(),
                Err(m) => return Err(DecodeError::Malformed(format!("{}: {m}", path.display()))),
            },
            _ => bytes.to_vec(),
        };
        let (w, h, rgba) = Self::render(path, pdf)?;
        let Some(l) = meta.layer.as_ref().filter(|l| !l.full_frame) else {
            return Frame::new(w, h, rgba);
        };
        // Layer Size: the artwork box at the footage's own size. The box is
        // in PDF page points, y up from the bottom edge, and may reach past
        // the page; hayro clips to the page, so that part stays empty.
        // ponytail: art outside the artboard is lost; render with an offset
        // transform via `render_into` if a project needs it.
        let (bx, by) = (l.bounds[0].floor() as i64, (h as f64 - l.bounds[3]).floor() as i64);
        let (cw, ch) = (meta.width.round().max(1.0) as u32, meta.height.round().max(1.0) as u32);
        let mut out = vec![0u8; (cw * ch * 4) as usize];
        for y in 0..ch as i64 {
            let sy = by + y;
            if sy < 0 || sy >= h as i64 {
                continue;
            }
            let (x0, x1) = ((-bx).clamp(0, cw as i64), (w as i64 - bx).clamp(0, cw as i64));
            if x0 >= x1 {
                continue;
            }
            let src = ((sy * w as i64 + bx + x0) * 4) as usize;
            let dst = ((y * cw as i64 + x0) * 4) as usize;
            let n = ((x1 - x0) * 4) as usize;
            out[dst..dst + n].copy_from_slice(&rgba[src..src + n]);
        }
        Frame::new(cw, ch, out)
    }
}

fn bad(path: &Path) -> DecodeError {
    DecodeError::Malformed(format!("{}: unsupported PSD", path.display()))
}

/// Camera RAW, decoded and developed to sRGB in-process via `imagepipe`.
///
/// A separate decoder from [`ImageDecoder`] because the `image` crate can't read
/// a mosaiced sensor file at all: a `.CR2` or `.NEF` isn't a bitmap, it's raw
/// photosite values that have to be demosaiced and colour-managed before there
/// are pixels. `imagepipe` owns that pipeline (and pulls `rawloader` for the
/// per-camera sensor read), and it is pure Rust — so this stays consistent with
/// the deliberate no-heavy-C-deps stance the video sidecar exists to hold.
pub struct RawDecoder;

/// Canon and Nikon, as asked for. CR3 is intentionally absent: `rawloader`'s
/// support for Canon's newer container is partial, and claiming a file we can't
/// actually develop would turn "unsupported format" into a decode error deep in
/// the render. Extending this list is how the set grows once a format is proven.
const RAW_EXTS: &[&str] = &["cr2", "crw", "nef", "nrw"];

impl RawDecoder {
    /// Decode and develop `path` to a full-size 8-bit sRGB image.
    ///
    /// The one expensive operation for a RAW: there is no cheap header read that
    /// yields the *developed* dimensions (crop and orientation only settle once
    /// the pipeline runs), so both [`Decoder::open`] and [`Decoder::frame`] go
    /// through here. `open` throws the pixels away — the architecture forbids it
    /// from caching them — which means a RAW is developed twice across import
    /// and first render. Acceptable: import is once, and the frame cache above
    /// keeps every later request off this path.
    fn develop(path: &Path) -> Result<(u32, u32, Vec<u8>), DecodeError> {
        // `0, 0` = no size cap: develop at native resolution.
        let img = imagepipe::simple_decode_8bit(path, 0, 0)
            .map_err(|e| DecodeError::Malformed(format!("{}: {e}", path.display())))?;
        // `imagepipe` hands back tightly-packed RGB; footage is RGBA everywhere
        // above the decoder, so widen it with an opaque alpha. A RAW has no alpha
        // channel of its own, so fully opaque is the correct, not a placeholder,
        // value.
        let mut rgba = Vec::with_capacity(img.width * img.height * 4);
        for px in img.data.chunks_exact(3) {
            rgba.extend_from_slice(px);
            rgba.push(255);
        }
        Ok((img.width as u32, img.height as u32, rgba))
    }
}

impl Decoder for RawDecoder {
    fn name(&self) -> &str {
        "raw"
    }

    fn probe(&self, path: &Path) -> bool {
        RAW_EXTS.contains(&extension(path).as_str())
    }

    fn open(&self, path: &Path) -> Result<AssetMeta, DecodeError> {
        let (w, h, _) = Self::develop(path)?;
        Ok(AssetMeta::visual(AssetKind::Image, w as f64, h as f64, 1, 0.0))
    }

    fn frame(
        &self,
        path: &Path,
        _source_frame: i64,
        _meta: &AssetMeta,
    ) -> Result<Frame, DecodeError> {
        // A RAW is a still: one frame, and `Asset::source_frame` already clamps
        // every request to 0. See [`ImageDecoder::frame`].
        let (w, h, rgba) = Self::develop(path)?;
        Frame::new(w, h, rgba)
    }
}

/// HEIC/HEIF stills, decoded through the `ffmpeg` sidecar.
///
/// Reuses the video path's binary rather than linking `libheif`: the same
/// reasoning that made video a sidecar (a heavy C dependency to build on
/// Windows, and no reason to link it when a process boundary does) applies
/// here, and there is no mature pure-Rust HEIC decoder to reach for instead. A
/// HEIC is treated as a one-frame still — HEIC image *sequences* read as their
/// first frame, the same way an animated GIF does.
pub struct HeicDecoder {
    ffmpeg: String,
    ffprobe: String,
}

const HEIC_EXTS: &[&str] = &["heic", "heif", "hif"];

impl Default for HeicDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl HeicDecoder {
    pub fn new() -> Self {
        Self { ffmpeg: ffmpeg_bin(), ffprobe: ffprobe_bin() }
    }

    fn missing_tool(&self, tool: &str) -> DecodeError {
        DecodeError::Unsupported(format!(
            "HEIC needs '{tool}' on PATH (or set PBC_FFMPEG / PBC_FFPROBE)"
        ))
    }
}

impl Decoder for HeicDecoder {
    fn name(&self) -> &str {
        "heic"
    }

    fn probe(&self, path: &Path) -> bool {
        HEIC_EXTS.contains(&extension(path).as_str())
    }

    fn open(&self, path: &Path) -> Result<AssetMeta, DecodeError> {
        // Dimensions only, like [`ImageDecoder::open`]: cheap, and no bitmap is
        // decoded just to size the layer.
        let out = Command::new(&self.ffprobe)
            .args([
                "-v",
                "error",
                "-select_streams",
                "v:0",
                "-show_entries",
                "stream=width,height",
                "-of",
                "default=noprint_wrappers=1",
            ])
            .arg(path)
            .output()
            .map_err(|_| self.missing_tool(&self.ffprobe))?;
        if !out.status.success() {
            return Err(DecodeError::Malformed(format!(
                "ffprobe couldn't read {}: {}",
                path.display(),
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        let text = String::from_utf8_lossy(&out.stdout);
        let mut width = None;
        let mut height = None;
        for line in text.lines() {
            let Some((k, v)) = line.split_once('=') else { continue };
            match k.trim() {
                "width" => width = v.trim().parse::<f64>().ok(),
                "height" => height = v.trim().parse::<f64>().ok(),
                _ => {}
            }
        }
        let (Some(width), Some(height)) = (width, height) else {
            return Err(DecodeError::Malformed(format!(
                "ffprobe reported no image stream in {}",
                path.display()
            )));
        };
        Ok(AssetMeta::visual(AssetKind::Image, width, height, 1, 0.0))
    }

    fn frame(
        &self,
        path: &Path,
        _source_frame: i64,
        meta: &AssetMeta,
    ) -> Result<Frame, DecodeError> {
        let (w, h) = (meta.width as u32, meta.height as u32);
        let out = Command::new(&self.ffmpeg)
            .args(["-v", "error"])
            .arg("-i")
            .arg(path)
            .args(["-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "rgba", "-"])
            .output()
            .map_err(|_| self.missing_tool(&self.ffmpeg))?;
        if !out.status.success() {
            return Err(DecodeError::Malformed(format!(
                "ffmpeg couldn't decode {}: {}",
                path.display(),
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        Frame::new(w, h, out.stdout)
    }
}

/// Video, by shelling out to `ffmpeg`/`ffprobe`.
///
/// **A sidecar, deliberately.** Linking libav into the process is faster and
/// seeks better, but it is a heavy C dependency to build on Windows and it is
/// not a decision that needs making yet: this decoder and an `ffmpeg-next` one
/// are two impls of the same trait, so swapping is a registration change and
/// nothing above the trait notices. It also matches the export plan — pipe raw
/// frames to ffmpeg, never implement a codec.
pub struct FfmpegDecoder {
    ffmpeg: String,
    ffprobe: String,
}

// Containers, not codecs: probing is by extension and the codec inside is
// ffmpeg's problem, so ProRes and DNxHD ride in on `mov`/`mxf` and need no entry
// of their own. `mxf` is the broadcast delivery wrapper for exactly those two;
// `mts`/`m2ts`/`ts` are the transport-stream containers camera AVCHD ships in.
const VIDEO_EXTS: &[&str] = &[
    "mp4", "mov", "m4v", "avi", "mkv", "webm", "mpg", "mpeg", "wmv", "flv", "ogv", "mxf", "mts",
    "m2ts", "ts", "m2v", "3gp",
];

impl Default for FfmpegDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl FfmpegDecoder {
    pub fn new() -> Self {
        Self { ffmpeg: ffmpeg_bin(), ffprobe: ffprobe_bin() }
    }

    /// Whether the tools are actually callable. Used by the UI to explain a
    /// failed video import as "ffmpeg isn't installed" rather than as a
    /// mysterious decode error.
    pub fn available(&self) -> bool {
        Command::new(&self.ffprobe)
            .arg("-version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok()
    }

    fn missing_tool(&self, tool: &str) -> DecodeError {
        DecodeError::Unsupported(format!(
            "video needs '{tool}' on PATH (or set PBC_FFMPEG / PBC_FFPROBE)"
        ))
    }
}

impl Decoder for FfmpegDecoder {
    fn name(&self) -> &str {
        "ffmpeg"
    }

    fn probe(&self, path: &Path) -> bool {
        VIDEO_EXTS.contains(&extension(path).as_str())
    }

    fn open(&self, path: &Path) -> Result<AssetMeta, DecodeError> {
        let out = Command::new(&self.ffprobe)
            .args([
                "-v",
                "error",
                "-select_streams",
                "v:0",
                "-show_entries",
                "stream=width,height,r_frame_rate,nb_frames,duration",
                "-of",
                "default=noprint_wrappers=1",
            ])
            .arg(path)
            .output()
            .map_err(|_| self.missing_tool(&self.ffprobe))?;
        if !out.status.success() {
            return Err(DecodeError::Malformed(format!(
                "ffprobe couldn't read {}: {}",
                path.display(),
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        parse_probe(&String::from_utf8_lossy(&out.stdout))
    }

    fn frame(
        &self,
        path: &Path,
        source_frame: i64,
        meta: &AssetMeta,
    ) -> Result<Frame, DecodeError> {
        let (w, h) = (meta.width as u32, meta.height as u32);
        let out = Command::new(&self.ffmpeg)
            .args(["-v", "error", "-ss", &format!("{:.6}", seek_seconds(source_frame, meta))])
            .arg("-i")
            .arg(path)
            .args(["-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "rgba", "-"])
            .output()
            .map_err(|_| self.missing_tool(&self.ffmpeg))?;
        if !out.status.success() {
            return Err(DecodeError::Malformed(format!(
                "ffmpeg couldn't decode frame {source_frame} of {}: {}",
                path.display(),
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        if out.stdout.is_empty() {
            // Seeking past the end yields nothing rather than an error. The
            // caller clamps, so reaching here means the probed frame count
            // overstated the file — say so instead of returning a blank frame
            // that looks like a hole in the footage.
            return Err(DecodeError::Malformed(format!(
                "frame {source_frame} is past the end of {}",
                path.display()
            )));
        }
        Frame::new(w, h, out.stdout)
    }

    fn stream(&self, path: &Path, from: i64, meta: &AssetMeta) -> Option<Box<dyn FrameStream>> {
        // One process, left running, with frames read off its stdout as they
        // come. Everything expensive about the sidecar — process creation,
        // container parsing, decoder setup — is paid once here instead of once
        // per frame.
        let child = Command::new(&self.ffmpeg)
            .args(["-v", "error", "-ss", &format!("{:.6}", seek_seconds(from, meta))])
            .arg("-i")
            .arg(path)
            .args(["-f", "rawvideo", "-pix_fmt", "rgba", "-"])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .stdin(std::process::Stdio::null())
            .spawn()
            .ok()?;
        let mut child = child;
        Some(Box::new(FfmpegStream {
            stdout: std::io::BufReader::new(child.stdout.take()?),
            child,
            width: meta.width as u32,
            height: meta.height as u32,
            next: from,
            last: meta.frames - 1,
        }))
    }
}

/// Where in the file frame `n` sits, in seconds.
///
/// Lands in the *middle* of the frame's own interval: asking for its exact
/// start invites a rounding error to pick up the frame before it.
fn seek_seconds(n: i64, meta: &AssetMeta) -> f64 {
    if meta.fps > 0.0 {
        (n as f64 + 0.5) / meta.fps
    } else {
        0.0
    }
}

/// A running `ffmpeg` piping raw frames, read one at a time.
struct FfmpegStream {
    child: std::process::Child,
    stdout: std::io::BufReader<std::process::ChildStdout>,
    width: u32,
    height: u32,
    next: i64,
    /// Last valid source frame. The probed frame count can overstate a
    /// variable-rate file, so the stream can also just end; both are handled.
    last: i64,
}

impl FrameStream for FfmpegStream {
    fn next_frame(&mut self) -> Result<Option<Frame>, DecodeError> {
        use std::io::Read;
        if self.next > self.last {
            return Ok(None);
        }
        let mut buf = vec![0u8; self.width as usize * self.height as usize * 4];
        // `read_exact` rather than `read`: a pipe hands over whatever happens to
        // be buffered, so a single read routinely returns a partial frame.
        // Treating that as a whole one would tear the picture.
        match self.stdout.read_exact(&mut buf) {
            Ok(()) => {
                self.next += 1;
                Frame::new(self.width, self.height, buf).map(Some)
            }
            // A clean EOF is the end of the footage, not a failure.
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => Ok(None),
            Err(e) => Err(DecodeError::Malformed(format!("reading frame {}: {e}", self.next))),
        }
    }

    fn next_index(&self) -> i64 {
        self.next
    }
}

impl Drop for FfmpegStream {
    /// Kill the process rather than waiting it out.
    ///
    /// A stream is dropped when the playhead jumps somewhere this one can't
    /// reach, and the child is usually mid-decode with a full pipe. Left alone
    /// it would sit blocked on a write nobody will ever read, so seeking around
    /// a timeline would leak an ffmpeg per jump.
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Parse `ffprobe`'s `key=value` lines into metadata.
///
/// A free function so the parsing — which is where the fiddly parts are (a
/// rational frame rate, a frame count that is often absent) — is testable
/// without ffmpeg installed.
fn parse_probe(text: &str) -> Result<AssetMeta, DecodeError> {
    let mut width = None;
    let mut height = None;
    let mut fps = None;
    let mut nb_frames = None;
    let mut duration = None;
    for line in text.lines() {
        let Some((k, v)) = line.split_once('=') else { continue };
        let v = v.trim();
        match k.trim() {
            "width" => width = v.parse::<f64>().ok(),
            "height" => height = v.parse::<f64>().ok(),
            // Reported as a rational ("30000/1001"), because that is what
            // broadcast rates actually are.
            "r_frame_rate" => {
                fps = match v.split_once('/') {
                    Some((n, d)) => match (n.parse::<f64>(), d.parse::<f64>()) {
                        (Ok(n), Ok(d)) if d != 0.0 => Some(n / d),
                        _ => None,
                    },
                    None => v.parse().ok(),
                }
            }
            "nb_frames" => nb_frames = v.parse::<i64>().ok(),
            "duration" => duration = v.parse::<f64>().ok(),
            _ => {}
        }
    }
    let (Some(width), Some(height)) = (width, height) else {
        return Err(DecodeError::Malformed("ffprobe reported no video stream".into()));
    };
    let fps = fps.filter(|f| *f > 0.0).unwrap_or(25.0);
    // `nb_frames` is exact but frequently absent (it isn't in the container for
    // most streaming formats), so fall back to duration × rate. Both can be
    // wrong for a variable-rate file; a length that is slightly off is much
    // better than refusing the import, and `clamp_frame` keeps a bad count from
    // ever asking for a frame that isn't there.
    let frames = nb_frames
        .filter(|n| *n > 0)
        .or_else(|| duration.map(|d| (d * fps).round() as i64))
        .unwrap_or(1)
        .max(1);
    Ok(AssetMeta::visual(AssetKind::Video, width, height, frames, fps))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Broadcast rates are rationals: 29.97 is 30000/1001, and rounding it to
    /// 30 would drift a frame every thirty-three seconds.
    #[test]
    fn a_rational_frame_rate_is_kept_as_a_ratio() {
        let m = parse_probe("width=1920\nheight=1080\nr_frame_rate=30000/1001\nnb_frames=300\n")
            .unwrap();
        assert!((m.fps - 29.97).abs() < 0.01, "got {}", m.fps);
        assert_eq!(m.frames, 300);
        assert_eq!(m.kind, AssetKind::Video);
    }

    /// Most containers don't carry a frame count, so length comes from the
    /// duration instead. Refusing the import over a missing field would reject
    /// most real files.
    #[test]
    fn a_missing_frame_count_falls_back_to_duration() {
        let m = parse_probe("width=640\nheight=480\nr_frame_rate=25/1\nduration=4.000000\n")
            .unwrap();
        assert_eq!(m.frames, 100);
    }

    /// A file with no video stream is refused rather than imported as a
    /// zero-sized layer that draws nothing and explains nothing.
    #[test]
    fn a_file_without_a_video_stream_is_refused() {
        assert!(matches!(parse_probe("duration=10.0\n"), Err(DecodeError::Malformed(_))));
    }

    /// The streaming path against **real ffmpeg**, because the fake decoder in
    /// the editor's tests can't catch a mistake in how frames are framed on the
    /// pipe — a wrong buffer size or a partial read would tear the picture, and
    /// only the real thing shows that.
    ///
    /// Skips when ffmpeg isn't installed rather than failing: the tool is
    /// optional, and stills work without it.
    #[test]
    fn a_real_ffmpeg_stream_yields_whole_frames_in_order() {
        let d = FfmpegDecoder::new();
        if !d.available() {
            eprintln!("skipping: ffmpeg not on PATH");
            return;
        }
        // A tiny clip, generated by the tool under test so the fixture can't
        // rot and nothing binary lives in the repo.
        let path = std::env::temp_dir().join("pbc_stream_test.mp4");
        let made = Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-f", "lavfi", "-i"])
            .arg("testsrc=size=32x16:rate=10:duration=1")
            .args(["-c:v", "libx264", "-pix_fmt", "yuv420p"])
            .arg(&path)
            .status();
        if !matches!(made, Ok(s) if s.success()) {
            eprintln!("skipping: couldn't generate a fixture");
            return;
        }

        let meta = d.open(&path).expect("probe the fixture");
        assert_eq!((meta.width, meta.height), (32.0, 16.0));

        let mut stream = d.stream(&path, 0, &meta).expect("video streams");
        let mut count = 0;
        while let Some(frame) = stream.next_frame().expect("a frame or the end") {
            // The load-bearing assertion: a frame is *whole*. Reading whatever
            // happened to be buffered on the pipe would pass a shorter buffer
            // through and tear the image.
            assert_eq!(frame.rgba.len(), 32 * 16 * 4, "frame {count} is a full frame");
            assert_eq!((frame.width, frame.height), (32, 16));
            count += 1;
            assert_eq!(stream.next_index(), count, "the position advances by one");
        }
        assert!(count >= 9, "a one-second 10fps clip has ~10 frames, got {count}");

        let _ = std::fs::remove_file(&path);
    }

    /// Probing is by extension, and the two built-ins must not fight over a
    /// file: whichever claims it, only one does.
    #[test]
    fn the_built_in_decoders_claim_disjoint_files() {
        let reg = default_registry();
        assert_eq!(reg.decoder_for(Path::new("a/b/logo.PNG")).map(|d| d.name()), Some("image"));
        assert_eq!(reg.decoder_for(Path::new("a/b/clip.MOV")).map(|d| d.name()), Some("ffmpeg"));
        // The still-image formats each land on their own decoder, and ahead of
        // the video one so a HEIC never falls through to it.
        assert_eq!(reg.decoder_for(Path::new("a/b/shot.CR2")).map(|d| d.name()), Some("raw"));
        assert_eq!(reg.decoder_for(Path::new("a/b/shot.NEF")).map(|d| d.name()), Some("raw"));
        assert_eq!(reg.decoder_for(Path::new("a/b/pic.HEIC")).map(|d| d.name()), Some("heic"));
        assert!(reg.decoder_for(Path::new("notes.txt")).is_none());
    }
}
#[cfg(test)]
mod ai_layers {
    use lopdf::{dictionary, Document, Object, Stream};

    /// A 20x10 pt page with two layers: "Left" fills the left half red,
    /// "Right" the right half blue.
    fn two_layer_pdf() -> Vec<u8> {
        let mut doc = Document::with_version("1.5");
        let left = doc.add_object(dictionary! { "Type" => "OCG", "Name" => Object::string_literal("Left") });
        let right = doc.add_object(dictionary! { "Type" => "OCG", "Name" => Object::string_literal("Right") });
        let content = b"/OC /L BDC 1 0 0 rg 0 0 10 10 re f EMC /OC /R BDC 0 0 1 rg 10 0 10 10 re f EMC".to_vec();
        let content = doc.add_object(Stream::new(dictionary! {}, content));
        let pages = doc.new_object_id();
        let page = doc.add_object(dictionary! {
            "Type" => "Page", "Parent" => pages, "MediaBox" => vec![0.into(), 0.into(), 20.into(), 10.into()],
            "Contents" => content,
            "Resources" => dictionary! { "Properties" => dictionary! { "L" => left, "R" => right } },
        });
        doc.objects.insert(pages, Object::Dictionary(dictionary! { "Type" => "Pages", "Kids" => vec![page.into()], "Count" => 1 }));
        let catalog = doc.add_object(dictionary! {
            "Type" => "Catalog", "Pages" => pages,
            "OCProperties" => dictionary! { "OCGs" => vec![left.into(), right.into()], "D" => dictionary! {} },
        });
        doc.trailer.set("Root", catalog);
        let mut out = Vec::new();
        doc.save_to(&mut out).unwrap();
        out
    }

    #[test]
    fn one_layer_of_a_layered_pdf_draws_alone() {
        let path = std::path::Path::new("layers.pdf");
        let pdf = two_layer_pdf();
        let (w, _, all) = super::AiDecoder::render(path, pdf.clone()).unwrap();
        let px = |rgba: &[u8], x: u32| rgba[(5 * w + x) as usize * 4..][..4].to_vec();
        assert_eq!((px(&all, 5)[0], px(&all, 15)[2]), (255, 255), "both layers by default");
        let (_, _, left) = super::AiDecoder::render(path, super::AiDecoder::only_layer(&pdf, "Left").unwrap()).unwrap();
        assert_eq!(px(&left, 5), vec![255, 0, 0, 255]);
        assert_eq!(px(&left, 15)[3], 0, "the other layer is off");
        assert!(super::AiDecoder::only_layer(&pdf, "Nope").is_err());
    }
}

#[cfg(test)]
mod psd_smoke {
    #[test]
    #[ignore]
    fn decodes_a_real_layer() {
        use motion_core::asset::{AssetMeta, Decoder, SourceLayer};
        let path = std::path::PathBuf::from(std::env::var("PBC_PSD").unwrap());
        let d = super::PsdDecoder::default();
        let mut meta = d.open(&path).unwrap();
        println!("canvas {}x{}", meta.width, meta.height);
        meta.layer = Some(SourceLayer { index: None, name: std::env::var("PBC_PSD_LAYER").unwrap(), bounds: [201.0, 838.0, 884.0, 1045.0], full_frame: false });
        let f = d.frame(&path, 0, &meta).unwrap();
        let opaque = f.rgba.chunks(4).filter(|p| p[3] > 0).count();
        println!("layer {}x{} opaque px {}", f.width, f.height, opaque);
        let _ = AssetMeta::default();
    }
}
#[cfg(test)]
mod ai_smoke {
    #[test]
    #[ignore]
    fn renders_a_real_layer() {
        use motion_core::asset::{Decoder, SourceLayer};
        let path = std::path::PathBuf::from(std::env::var("PBC_AI").unwrap());
        let d = super::AiDecoder::default();
        let mut meta = d.open(&path).unwrap();
        println!("page {}x{}", meta.width, meta.height);
        let b: Vec<f64> = std::env::var("PBC_AI_BOUNDS").unwrap().split(',').map(|v| v.parse().unwrap()).collect();
        meta.width = std::env::var("PBC_AI_W").unwrap().parse().unwrap();
        meta.height = std::env::var("PBC_AI_H").unwrap().parse().unwrap();
        meta.layer = Some(SourceLayer { index: None, name: std::env::var("PBC_AI_LAYER").unwrap(), bounds: [b[0], b[1], b[2], b[3]], full_frame: false });
        let t = std::time::Instant::now();
        let f = d.frame(&path, 0, &meta).unwrap();
        let opaque = f.rgba.chunks(4).filter(|p| p[3] > 0).count();
        println!("layer {}x{} opaque {} in {:?}", f.width, f.height, opaque, t.elapsed());
        image::save_buffer(std::env::var("PBC_OUT").unwrap(), &f.rgba, f.width, f.height, image::ColorType::Rgba8).unwrap();
    }
}
