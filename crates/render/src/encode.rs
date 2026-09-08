//! Frames out. **We never implement a codec** — see
//! `docs/decisions/0007-never-implement-codecs.md`.
//!
//! An [`Encoder`] takes RGBA frames in composition order and writes them
//! somewhere. Two implementations ship: a PNG sequence (pure Rust, always
//! available, and the format a compositing pipeline actually wants) and an
//! `ffmpeg` sidecar fed raw frames over stdin.
//!
//! The sidecar is deliberately the *same* shape as [`crate::decode`]'s: a child
//! process rather than linked libav, so a Windows build has no C library to
//! compile, and the tool's absence is reported as "ffmpeg isn't installed"
//! rather than as a mysterious failure. It is also why encoding a format we
//! have never heard of costs nothing: it is ffmpeg's problem, named by the
//! output extension.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

/// Why a frame could not be written.
#[derive(Debug)]
pub enum EncodeError {
    /// The encoder needs a tool that isn't on PATH.
    MissingTool(String),
    /// The output could not be opened or written.
    Io(std::io::Error),
    /// The encoder rejected the frame or the settings.
    Failed(String),
}

impl std::fmt::Display for EncodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EncodeError::MissingTool(m) => write!(f, "{m}"),
            EncodeError::Io(e) => write!(f, "{e}"),
            EncodeError::Failed(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for EncodeError {}

impl From<std::io::Error> for EncodeError {
    fn from(e: std::io::Error) -> Self {
        EncodeError::Io(e)
    }
}

/// What the encoder is being asked to produce.
///
/// Frames and rate travel together because an encoder that gets one without the
/// other writes a file that plays at the wrong speed — the failure that looks
/// like a render bug and isn't.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OutputSpec {
    pub width: u32,
    pub height: u32,
    /// Frames per second, as the comp defines it. Fractional rates (23.976 =
    /// 24000/1001) are passed through exactly rather than rounded — the whole
    /// reason the comp stores frames.
    pub fps: f64,
}

impl OutputSpec {
    /// Bytes in one RGBA frame. What [`Encoder::push`] must be handed.
    pub fn frame_bytes(&self) -> usize {
        self.width as usize * self.height as usize * 4
    }
}

/// Somewhere rendered frames go.
///
/// Deliberately tiny, and deliberately *not* generic over pixel format: every
/// frame is 8-bit RGBA, non-premultiplied, in composition order. One format at
/// this seam means an encoder can never disagree with the rasterizer about what
/// it was handed.
pub trait Encoder {
    /// A human name for the encoder, for progress and errors.
    fn name(&self) -> &str;

    /// Write one frame. Frames arrive in order, exactly
    /// [`OutputSpec::frame_bytes`] long.
    fn push(&mut self, rgba: &[u8]) -> Result<(), EncodeError>;

    /// Close the output. Takes `Box<Self>` so it can consume the encoder
    /// through a trait object — an encoder that is merely dropped has no way to
    /// report that the muxer failed, and a truncated video that reported
    /// success is worse than an error.
    fn finish(self: Box<Self>) -> Result<(), EncodeError>;

    /// Abandon the output: the render was **cancelled**, and whatever has been
    /// written is not a deliverable.
    ///
    /// Distinct from `finish` because dropping an encoder is not neutral. A
    /// process-backed encoder holds a pipe, and closing that pipe is exactly the
    /// signal that means *"the stream ended, finalize the file"* — so simply
    /// dropping a cancelled ffmpeg encoder produces a **complete, playable, and
    /// wrong** video: the piece, truncated at the frame the user cancelled on,
    /// with nothing about it to say so. That is the same failure `finish`'s
    /// signature exists to prevent, reached from the other direction.
    ///
    /// The default is to drop, which is right for an encoder whose partial
    /// output is self-evidently partial (numbered stills). Anything that muxes a
    /// container should override it.
    fn abort(self: Box<Self>) {}
}

/// Frames as numbered PNGs in a directory.
///
/// Lossless, needs no external tool, and is what a compositing hand-off usually
/// wants anyway. Also the encoder the tests use, because it is the one that can
/// be verified anywhere.
pub struct PngSequence {
    dir: PathBuf,
    stem: String,
    spec: OutputSpec,
    index: usize,
    written: Vec<PathBuf>,
}

impl PngSequence {
    /// `dir/stem_00000.png`, numbered from zero.
    pub fn new(dir: impl AsRef<Path>, stem: &str, spec: OutputSpec) -> Result<Self, EncodeError> {
        std::fs::create_dir_all(dir.as_ref())?;
        Ok(Self {
            dir: dir.as_ref().to_path_buf(),
            stem: stem.to_string(),
            spec,
            index: 0,
            written: Vec::new(),
        })
    }

    /// The files written so far, in order.
    pub fn frames(&self) -> &[PathBuf] {
        &self.written
    }
}

impl Encoder for PngSequence {
    fn name(&self) -> &str {
        "png-sequence"
    }

    fn push(&mut self, rgba: &[u8]) -> Result<(), EncodeError> {
        if rgba.len() != self.spec.frame_bytes() {
            return Err(EncodeError::Failed(format!(
                "frame is {} bytes, expected {} for {}x{} RGBA",
                rgba.len(),
                self.spec.frame_bytes(),
                self.spec.width,
                self.spec.height
            )));
        }
        // Five digits: a hundred thousand frames is over an hour at 24fps, and
        // fixed width is what makes the sequence sort correctly everywhere.
        let path = self.dir.join(format!("{}_{:05}.png", self.stem, self.index));
        image::save_buffer(
            &path,
            rgba,
            self.spec.width,
            self.spec.height,
            image::ColorType::Rgba8,
        )
        .map_err(|e| EncodeError::Failed(format!("writing {}: {e}", path.display())))?;
        self.written.push(path);
        self.index += 1;
        Ok(())
    }

    fn finish(self: Box<Self>) -> Result<(), EncodeError> {
        Ok(())
    }

    /// The written frames are kept. A half-finished PNG sequence is *visibly*
    /// half-finished — the numbers stop — and the frames that did render are
    /// often the reason someone cancelled, so deleting them would throw away
    /// the only product of the work.
    fn abort(self: Box<Self>) {}
}

/// How hard the encoder should work — the **two-button model**.
///
/// Borrowed from build tooling, where `run` and `release` are different verbs
/// rather than one verb with a settings dialog. The two answer different
/// questions, and conflating them is what makes an export dialog something
/// people dread:
///
/// - [`Quality::Draft`] — "let me see it move." Fast to encode, big on disk,
///   never asked a question. The equivalent of `cargo run`.
/// - [`Quality::Master`] — "this is the deliverable." Slow, small, visually
///   lossless, and *reproducible*: the settings belong to the project, not to
///   whatever was last typed into a dialog.
///
/// Both render **every pixel** at full resolution. Draft is cheaper to *encode*,
/// not cheaper to render — a preview that silently halved resolution would be
/// the fastest way to ship the wrong file, and a draft you cannot trust is a
/// draft nobody uses.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Quality {
    /// Fast, disposable, no questions asked.
    #[default]
    Draft,
    /// The deliverable.
    Master,
}

impl Quality {
    pub const ALL: [Quality; 2] = [Quality::Draft, Quality::Master];

    pub fn label(self) -> &'static str {
        match self {
            Quality::Draft => "Draft",
            Quality::Master => "Master",
        }
    }

    /// Parse the name a CLI flag or a saved preset uses.
    pub fn parse(s: &str) -> Option<Quality> {
        match s.trim().to_ascii_lowercase().as_str() {
            "draft" | "quick" | "preview" => Some(Quality::Draft),
            "master" | "release" | "final" => Some(Quality::Master),
            _ => None,
        }
    }

    /// The ffmpeg arguments this quality implies for `path`'s container.
    ///
    /// Container-aware because the right answer differs: `.mov` at master
    /// quality means **ProRes**, which is what an editorial hand-off expects and
    /// what H.264 is wrong for; everything else means H.264 at a CRF chosen for
    /// the job. A user's own `--arg` is appended after these, so anything here
    /// can be overridden without editing this table.
    ///
    /// Deliberately a *small* table. The moment it grows a codec matrix we are
    /// maintaining the thing [0007](../../docs/decisions/0007-never-implement-codecs.md)
    /// says not to maintain — one sensible default per quality, and ffmpeg's own
    /// flags for everything else.
    pub fn ffmpeg_args(self, path: &Path) -> Vec<String> {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .unwrap_or_default();
        let a = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        match (self, ext.as_str()) {
            // ProRes 422 HQ: the editorial interchange format, and the reason
            // anyone asks for a .mov master in the first place.
            (Quality::Master, "mov") => a(&["-c:v", "prores_ks", "-profile:v", "3"]),
            (Quality::Master, _) => {
                a(&["-c:v", "libx264", "-preset", "slow", "-crf", "16"])
            }
            // `veryfast` rather than `ultrafast`: the latter's files are large
            // enough that writing them costs back the encode time it saved.
            (Quality::Draft, _) => a(&["-c:v", "libx264", "-preset", "veryfast", "-crf", "23"]),
        }
    }
}

/// Whether an output path names a **video container** rather than a directory
/// of stills. Extension-driven, because deciding for the user is how you end up
/// owning a codec table ([0007](../../docs/decisions/0007-never-implement-codecs.md)).
///
/// Lives here rather than in a caller so the CLI and the editor's render queue
/// cannot disagree about what `.mkv` means — one container table, one answer.
pub fn is_video_container(path: &Path) -> bool {
    const VIDEO: [&str; 8] = ["mp4", "mov", "mkv", "webm", "avi", "m4v", "mxf", "gif"];
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| VIDEO.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

/// The `ffmpeg` binary to invoke. Shared with [`crate::decode`], so a bundled
/// build points both halves at one binary.
fn ffmpeg_bin() -> String {
    std::env::var("PBC_FFMPEG").unwrap_or_else(|_| "ffmpeg".into())
}

/// Raw frames piped to `ffmpeg`, which does the actual encoding.
///
/// The container and codec come from the output path's extension — `.mp4`,
/// `.mov`, `.webm`, whatever ffmpeg knows — because guessing on the user's
/// behalf is how you end up maintaining a codec table you did not want.
pub struct FfmpegEncoder {
    child: Child,
    path: PathBuf,
    spec: OutputSpec,
    /// Extra arguments placed before the output path — the quality knobs.
    name: String,
}

impl FfmpegEncoder {
    /// Whether `ffmpeg` is actually callable. Ask before offering a video
    /// format in the UI, so the failure is explained up front rather than after
    /// a render.
    pub fn available() -> bool {
        Command::new(ffmpeg_bin())
            .arg("-version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok()
    }

    /// Start ffmpeg, reading raw RGBA from stdin.
    ///
    /// `extra` is placed before the output path: quality settings, a pixel
    /// format, a codec override. The defaults below are a reasonable H.264
    /// master when the output is `.mp4` and ffmpeg picks the rest.
    pub fn new(
        path: impl AsRef<Path>,
        spec: OutputSpec,
        extra: &[String],
    ) -> Result<Self, EncodeError> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        let bin = ffmpeg_bin();
        let mut cmd = Command::new(&bin);
        cmd.args(["-hide_banner", "-loglevel", "error", "-y"])
            // The input description. `-framerate` before `-i` applies to the
            // raw stream being read; after it, it would be a *filter* on an
            // already-timed one, which is the classic way to get a file that
            // plays at the wrong speed.
            .args(["-f", "rawvideo", "-pix_fmt", "rgba"])
            .args(["-s", &format!("{}x{}", spec.width, spec.height)])
            .args(["-framerate", &format_rate(spec.fps)])
            .args(["-i", "-"])
            // yuv420p rather than ffmpeg's pick: it is the one chroma format
            // every player and browser handles, and the default for H.264 from
            // RGBA input is not it.
            .args(["-pix_fmt", "yuv420p"]);
        for a in extra {
            cmd.arg(a);
        }
        let child = cmd
            .arg(&path)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|_| {
                EncodeError::MissingTool(format!(
                    "encoding video needs '{bin}' on PATH (or set PBC_FFMPEG)"
                ))
            })?;
        let name = format!("ffmpeg → {}", path_label(&path));
        Ok(Self { child, path, spec, name })
    }
}

impl Encoder for FfmpegEncoder {
    fn name(&self) -> &str {
        &self.name
    }

    fn push(&mut self, rgba: &[u8]) -> Result<(), EncodeError> {
        if rgba.len() != self.spec.frame_bytes() {
            return Err(EncodeError::Failed(format!(
                "frame is {} bytes, expected {}",
                rgba.len(),
                self.spec.frame_bytes()
            )));
        }
        let stdin = self
            .child
            .stdin
            .as_mut()
            .ok_or_else(|| EncodeError::Failed("ffmpeg stdin closed".into()))?;
        // A broken pipe means ffmpeg died — usually a rejected setting. Its
        // stderr has the reason, and `finish` will surface it, so don't paper
        // over the write error with a generic one.
        stdin.write_all(rgba).map_err(|e| {
            EncodeError::Failed(format!("ffmpeg stopped accepting frames ({e})"))
        })
    }

    fn finish(mut self: Box<Self>) -> Result<(), EncodeError> {
        // Close stdin first: ffmpeg finalizes the container on EOF, and waiting
        // on a process that is still expecting input deadlocks.
        drop(self.child.stdin.take());
        let out = self.child.wait_with_output()?;
        if out.status.success() {
            return Ok(());
        }
        Err(EncodeError::Failed(format!(
            "ffmpeg failed writing {}: {}",
            path_label(&self.path),
            String::from_utf8_lossy(&out.stderr).trim()
        )))
    }

    /// Kill ffmpeg and remove the partial file.
    ///
    /// The kill has to come *before* stdin is dropped. Dropping the pipe first
    /// is how you finalize a container — ffmpeg reads EOF, writes the trailer,
    /// and exits successfully, leaving a file that plays perfectly and is the
    /// wrong length. Killing first means the process dies mid-stream and the
    /// container is never finalized.
    ///
    /// Removing the file is then not optional: ffmpeg has already truncated
    /// whatever was at that path on open, so leaving the fragment behind trades
    /// a missing file for a corrupt one wearing a deliverable's name. A failure
    /// to remove it is ignored — this path is already the error path, and there
    /// is nothing useful to say to a user who has just pressed Cancel.
    fn abort(mut self: Box<Self>) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_file(&self.path);
    }
}

/// A frame rate as ffmpeg wants it.
///
/// Fractional broadcast rates are exact ratios, not decimals: 23.976 is
/// 24000/1001, and handing ffmpeg the rounded decimal makes a file whose
/// timestamps drift against the audio over a long piece.
fn format_rate(fps: f64) -> String {
    for (num, den) in [(24000, 1001), (30000, 1001), (60000, 1001), (120000, 1001)] {
        if (fps - num as f64 / den as f64).abs() < 1e-6 {
            return format!("{num}/{den}");
        }
    }
    format!("{fps}")
}

fn path_label(p: &Path) -> String {
    p.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_else(|| p.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> OutputSpec {
        OutputSpec { width: 4, height: 2, fps: 24.0 }
    }

    fn frame(v: u8) -> Vec<u8> {
        vec![v; spec().frame_bytes()]
    }

    /// The encoder every test can rely on, end to end: frames land as numbered
    /// files, in order, readable back at the size we claimed.
    #[test]
    fn a_png_sequence_writes_numbered_frames() {
        let dir = std::env::temp_dir().join(format!("pbc_png_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut enc = PngSequence::new(&dir, "frame", spec()).unwrap();
        enc.push(&frame(10)).unwrap();
        enc.push(&frame(20)).unwrap();
        assert_eq!(enc.frames().len(), 2);
        assert!(enc.frames()[0].ends_with("frame_00000.png"));
        assert!(enc.frames()[1].ends_with("frame_00001.png"));

        let img = image::open(&enc.frames()[1]).unwrap().to_rgba8();
        assert_eq!(img.dimensions(), (4, 2));
        assert_eq!(img.as_raw()[0], 20);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A frame of the wrong size is a bug upstream, and it must be *reported*
    /// rather than written: a short frame silently padded produces a file that
    /// looks fine until someone scrubs it.
    #[test]
    fn a_wrongly_sized_frame_is_refused() {
        let dir = std::env::temp_dir().join(format!("pbc_png_bad_{}", std::process::id()));
        let mut enc = PngSequence::new(&dir, "frame", spec()).unwrap();
        let err = enc.push(&[0u8; 3]).unwrap_err();
        assert!(matches!(err, EncodeError::Failed(_)), "{err:?}");
        assert!(enc.frames().is_empty(), "nothing written");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The master `.mov` is ProRes, not H.264 — a `.mov` master exists for
    /// editorial hand-off, and handing over H.264 in a `.mov` wrapper is the
    /// wrong answer wearing the right extension.
    #[test]
    fn a_mov_master_is_prores_and_an_mp4_master_is_x264() {
        let mov = Quality::Master.ffmpeg_args(Path::new("cut.mov"));
        assert!(mov.contains(&"prores_ks".to_string()), "{mov:?}");
        let mp4 = Quality::Master.ffmpeg_args(Path::new("cut.mp4"));
        assert!(mp4.contains(&"libx264".to_string()), "{mp4:?}");
        assert!(mp4.contains(&"16".to_string()), "a master CRF, {mp4:?}");
    }

    /// Draft is cheaper to *encode*, and identical in what it renders. If this
    /// ever starts changing resolution, a draft becomes something you can ship
    /// by accident.
    #[test]
    fn draft_differs_only_in_encoder_effort() {
        let draft = Quality::Draft.ffmpeg_args(Path::new("cut.mp4"));
        assert!(draft.contains(&"veryfast".to_string()), "{draft:?}");
        assert!(
            !draft.iter().any(|a| a == "-s" || a == "-vf"),
            "draft must not resize or filter: {draft:?}"
        );
    }

    /// The names a user might type, including the ones borrowed from build
    /// tools, all land somewhere sensible.
    #[test]
    fn quality_parses_the_words_people_use() {
        assert_eq!(Quality::parse("draft"), Some(Quality::Draft));
        assert_eq!(Quality::parse("Quick"), Some(Quality::Draft));
        assert_eq!(Quality::parse("release"), Some(Quality::Master));
        assert_eq!(Quality::parse(" MASTER "), Some(Quality::Master));
        assert_eq!(Quality::parse("best"), None);
    }

    /// Broadcast rates are ratios. Handing ffmpeg `23.976` writes timestamps
    /// that drift; `24000/1001` does not.
    #[test]
    fn fractional_rates_stay_exact_ratios() {
        assert_eq!(format_rate(24000.0 / 1001.0), "24000/1001");
        assert_eq!(format_rate(30000.0 / 1001.0), "30000/1001");
        assert_eq!(format_rate(24.0), "24");
        assert_eq!(format_rate(60.0), "60");
    }

    /// Without ffmpeg the failure has to name the tool and the override, not
    /// surface as a generic IO error. Skipped when ffmpeg *is* installed —
    /// there the real path is covered by the round-trip test below.
    #[test]
    fn a_missing_ffmpeg_is_named() {
        if FfmpegEncoder::available() {
            return;
        }
        let Err(err) = FfmpegEncoder::new("out.mp4", spec(), &[]) else {
            panic!("ffmpeg is absent, so constructing an encoder must fail");
        };
        match err {
            EncodeError::MissingTool(m) => assert!(m.contains("PBC_FFMPEG"), "{m}"),
            other => panic!("expected a named tool, got {other:?}"),
        }
    }

    /// The real encoder against real ffmpeg, when it is installed: frames in,
    /// a playable file out, with the frame count we pushed.
    ///
    /// Skips rather than fails without ffmpeg, the same contract as the decoder
    /// test — the tool is a runtime dependency, not a build one.
    /// Cancelling must not leave a file that looks like a deliverable.
    ///
    /// The interesting half is that the naive implementation *passes* a
    /// "did it stop?" test while failing this one: dropping the encoder makes
    /// ffmpeg finalize a perfectly playable, wrong-length video. So the
    /// assertion is about the file being **gone**, not about the process.
    #[test]
    fn an_aborted_ffmpeg_encode_leaves_no_file() {
        if !FfmpegEncoder::available() {
            return;
        }
        let dir = std::env::temp_dir().join(format!("pbc_abort_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("cancelled.mp4");
        let spec = OutputSpec { width: 32, height: 32, fps: 24.0 };
        let mut enc =
            FfmpegEncoder::new(&path, spec, &Quality::Draft.ffmpeg_args(&path)).unwrap();
        for _ in 0..4 {
            enc.push(&vec![255u8; spec.frame_bytes()]).unwrap();
        }
        Box::new(enc).abort();
        assert!(!path.exists(), "a cancelled render must not leave {}", path.display());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A cancelled still sequence keeps what it wrote: the frames are visibly
    /// partial, and they are often the reason someone cancelled.
    #[test]
    fn an_aborted_png_sequence_keeps_its_frames() {
        let dir = std::env::temp_dir().join(format!("pbc_abort_png_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let spec = OutputSpec { width: 4, height: 4, fps: 24.0 };
        let mut enc = PngSequence::new(&dir, "shot", spec).unwrap();
        enc.push(&vec![0u8; spec.frame_bytes()]).unwrap();
        enc.push(&vec![0u8; spec.frame_bytes()]).unwrap();
        Box::new(enc).abort();
        let n = std::fs::read_dir(&dir).unwrap().count();
        assert_eq!(n, 2, "the frames that rendered are kept");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_real_ffmpeg_encode_writes_a_playable_file() {
        if !FfmpegEncoder::available() {
            return;
        }
        let dir = std::env::temp_dir().join(format!("pbc_mp4_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join("clip.mp4");
        // Even dimensions: yuv420p halves chroma, so an odd size is rejected.
        let spec = OutputSpec { width: 16, height: 16, fps: 24.0 };
        let mut enc = Box::new(FfmpegEncoder::new(&out, spec, &[]).unwrap());
        for i in 0..10u8 {
            enc.push(&vec![i * 20; spec.frame_bytes()]).unwrap();
        }
        enc.finish().expect("ffmpeg should finish cleanly");
        assert!(out.exists() && std::fs::metadata(&out).unwrap().len() > 0);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
