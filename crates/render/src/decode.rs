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
    reg.register(Box::new(FfmpegDecoder::new()));
    reg
}

fn extension(path: &Path) -> String {
    path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default()
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
        Ok(AssetMeta {
            kind: AssetKind::Image,
            width: w as f64,
            height: h as f64,
            frames: 1,
            fps: 0.0,
        })
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

const VIDEO_EXTS: &[&str] =
    &["mp4", "mov", "m4v", "avi", "mkv", "webm", "mpg", "mpeg", "wmv", "flv", "ogv"];

impl Default for FfmpegDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl FfmpegDecoder {
    pub fn new() -> Self {
        // Overridable so a bundled build can point at its own binaries without
        // depending on what happens to be on PATH.
        Self {
            ffmpeg: std::env::var("PBC_FFMPEG").unwrap_or_else(|_| "ffmpeg".into()),
            ffprobe: std::env::var("PBC_FFPROBE").unwrap_or_else(|_| "ffprobe".into()),
        }
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
    Ok(AssetMeta { kind: AssetKind::Video, width, height, frames, fps })
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
        assert!(reg.decoder_for(Path::new("notes.txt")).is_none());
    }
}
