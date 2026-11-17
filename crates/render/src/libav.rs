//! In-process video decode through linked libav (`ffmpeg-next`), for the
//! realtime preview. See ADR 0023.
//!
//! The sidecar in [`crate::decode`] stays the reference path: it is what export
//! and the CPU rasterizer read, and the test at the bottom of this file pins
//! this decoder to it frame for frame. What this one adds is what a process
//! boundary cannot give: a seek that costs milliseconds rather than a process
//! start, hardware decode, and frames handed over as the YUV planes the codec
//! produced — never converted to RGBA on the CPU, which at 4K is 33MB a frame
//! and the single largest cost of the old preview path.
//!
//! Still not a codec (invariant 23): every byte of decoding is FFmpeg's.

use std::path::Path;

use ffmpeg_next as ff;
use ff::ffi;
use ff::format::Pixel;
use ff::util::color;
use motion_core::asset::DecodeError;

/// Which YCbCr→RGB matrix a frame was encoded with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Matrix {
    Bt601,
    Bt709,
}

/// One decoded frame as 8-bit NV12: a full-resolution luma plane and one
/// half-resolution plane of interleaved Cb,Cr. Rows are tightly packed (no
/// stride padding), so a plane uploads to a texture as-is.
#[derive(Clone, Debug)]
pub struct YuvFrame {
    pub width: u32,
    pub height: u32,
    /// `width * height` bytes.
    pub y: Vec<u8>,
    /// `chroma_width() * chroma_height() * 2` bytes.
    pub uv: Vec<u8>,
    pub matrix: Matrix,
    /// Full ("JPEG") range rather than the usual 16–235 video range.
    pub full_range: bool,
}

impl YuvFrame {
    pub fn chroma_width(&self) -> u32 {
        self.width.div_ceil(2)
    }
    pub fn chroma_height(&self) -> u32 {
        self.height.div_ceil(2)
    }
    pub fn bytes(&self) -> usize {
        self.y.len() + self.uv.len()
    }

    /// The offsets and 3x3 matrix that take `(y, cb, cr)` in 0..1 to RGB:
    /// `rgb = m * (yuv - offset)`. Shared by the CPU reference below and the
    /// preview's shader, so the two cannot drift.
    pub fn coefficients(&self) -> ([f32; 3], [[f32; 3]; 3]) {
        let (kr, kb) = match self.matrix {
            Matrix::Bt601 => (0.299f32, 0.114f32),
            Matrix::Bt709 => (0.2126, 0.0722),
        };
        let kg = 1.0 - kr - kb;
        let (y_off, y_scale, c_scale) = if self.full_range {
            (0.0, 1.0, 1.0)
        } else {
            (16.0 / 255.0, 255.0 / 219.0, 255.0 / 224.0)
        };
        let rv = 2.0 * (1.0 - kr) * c_scale;
        let bu = 2.0 * (1.0 - kb) * c_scale;
        let gu = -2.0 * (1.0 - kb) * kb / kg * c_scale;
        let gv = -2.0 * (1.0 - kr) * kr / kg * c_scale;
        // Rows are R, G, B; columns are Y, Cb, Cr.
        ([y_off, 0.5, 0.5], [[y_scale, 0.0, rv], [y_scale, gu, gv], [y_scale, bu, 0.0]])
    }

    /// CPU conversion to straight RGBA. The reference the shader is tested
    /// against, and what the parity test compares with the sidecar — never
    /// called in the preview path.
    pub fn to_rgba(&self) -> Vec<u8> {
        let (off, m) = self.coefficients();
        let (w, h, cw) = (self.width as usize, self.height as usize, self.chroma_width() as usize);
        let mut out = vec![255u8; w * h * 4];
        for row in 0..h {
            for col in 0..w {
                let c = ((row / 2) * cw + col / 2) * 2;
                let yuv = [
                    self.y[row * w + col] as f32 / 255.0 - off[0],
                    self.uv[c] as f32 / 255.0 - off[1],
                    self.uv[c + 1] as f32 / 255.0 - off[2],
                ];
                let px = &mut out[(row * w + col) * 4..][..3];
                for (ch, r) in px.iter_mut().zip(m) {
                    *ch = ((r[0] * yuv[0] + r[1] * yuv[1] + r[2] * yuv[2]) * 255.0).round().clamp(0.0, 255.0) as u8;
                }
            }
        }
        out
    }
}

fn err(path: &Path, what: impl std::fmt::Display) -> DecodeError {
    DecodeError::Malformed(format!("{}: {what}", path.display()))
}

/// An open file, decoding forward one frame at a time.
pub struct LibavStream {
    input: ff::format::context::Input,
    decoder: ff::decoder::Video,
    stream: usize,
    /// Seconds per pts tick.
    tick: f64,
    /// pts of source frame 0.
    start: i64,
    /// How far the index's timestamps (decode order) run ahead of display
    /// time, in pts ticks: the reorder delay B-frames add. Measured off the
    /// first entry, which is the first keyframe, shown at `start`.
    index_lead: i64,
    fps: f64,
    next: i64,
    /// Frames before this are decoded and thrown away: a seek lands on the
    /// keyframe before the target.
    skip_until: i64,
    /// pts below which a packet belongs to a frame being skipped over.
    skip_pts: i64,
    /// Hand back the first frame decoded after a seek even though it is before
    /// the frame asked for — the keyframe the seek landed on. It is on screen
    /// a GOP's worth of decoding sooner than the exact frame, which is what a
    /// scrub wants to see while it waits. See [`Self::next_frame`].
    pub keyframe_first: bool,
    keyframe_owed: bool,
    eof_sent: bool,
    hardware: bool,
    scaler: Option<ff::software::scaling::Context>,
    path: std::path::PathBuf,
}

// The contexts hold raw pointers, but nothing here is shared: a stream is owned
// by exactly one decode thread at a time.
unsafe impl Send for LibavStream {}

impl LibavStream {
    /// Open `path` positioned so the first [`Self::next_frame`] is source frame
    /// `from`. `fps` is the asset's probed rate — the same number the sidecar
    /// path indexes frames by, so the two agree on what "frame N" means.
    ///
    /// `hardware` asks for the platform's hardware decoder; when the machine or
    /// the codec can't, FFmpeg falls back to software by itself.
    pub fn open(path: &Path, from: i64, fps: f64, hardware: bool) -> Result<Self, DecodeError> {
        static INIT: std::sync::Once = std::sync::Once::new();
        INIT.call_once(|| {
            let _ = ff::init();
            ff::util::log::set_level(ff::util::log::Level::Error);
        });

        let input = ff::format::input(&path).map_err(|e| err(path, e))?;
        let st = input
            .streams()
            .best(ff::media::Type::Video)
            .ok_or_else(|| err(path, "no video stream"))?;
        let stream = st.index();
        let tb = st.time_base();
        let tick = tb.numerator() as f64 / tb.denominator().max(1) as f64;
        let start = if st.start_time() == ffi::AV_NOPTS_VALUE { 0 } else { st.start_time() };

        let mut ctx = ff::codec::context::Context::from_parameters(st.parameters()).map_err(|e| err(path, e))?;
        let hardware = hardware && attach_hw_device(&mut ctx);
        if !hardware {
            // Frame threads: the only way software 4K keeps up. Count 0 = one
            // per core, FFmpeg's choice.
            ctx.set_threading(ff::threading::Config { kind: ff::threading::Type::Frame, count: 0 });
        }
        let decoder = ctx.decoder().video().map_err(|e| err(path, e))?;

        let mut s = LibavStream {
            input,
            decoder,
            stream,
            tick,
            start,
            index_lead: 0,
            fps: if fps > 0.0 { fps } else { 25.0 },
            next: 0,
            skip_until: 0,
            skip_pts: i64::MIN,
            keyframe_first: false,
            keyframe_owed: false,
            eof_sent: false,
            hardware,
            scaler: None,
            path: path.to_path_buf(),
        };
        s.index_lead = s.index_entry(0).map_or(0, |first| (s.start - first).max(0));
        if from > 0 {
            s.seek(from)?;
        }
        Ok(s)
    }

    /// Whether the last frame came off a hardware decoder. Before the first
    /// frame, whether one was offered.
    pub fn is_hardware(&self) -> bool {
        self.hardware
    }

    /// The source frame the next [`Self::next_frame`] will return.
    pub fn next_index(&self) -> i64 {
        self.next.max(self.skip_until)
    }

    /// Reposition so the next frame returned is `frame`. Costs a container seek
    /// plus decoding forward from the keyframe before it — milliseconds, where
    /// the sidecar pays a process start.
    pub fn seek(&mut self, frame: i64) -> Result<(), DecodeError> {
        let frame = frame.max(0);
        // In AV_TIME_BASE units, as `avformat_seek_file` wants with no stream
        // selected. Aim at the frame's own start; the range lets the demuxer
        // land on any keyframe at or before it.
        let secs = frame as f64 / self.fps + self.start as f64 * self.tick;
        let ts = (secs * ffi::AV_TIME_BASE as f64) as i64;
        self.input.seek(ts, ..ts).map_err(|e| err(&self.path, e))?;
        self.decoder.flush();
        self.eof_sent = false;
        self.skip_until = frame;
        // Half a frame early, so rounding can't classify the target itself as
        // skippable.
        self.skip_pts = self.start + ((frame as f64 - 0.5) / self.fps / self.tick) as i64;
        self.keyframe_owed = true;
        self.next = frame;
        Ok(())
    }

    /// Move forward so the next frame returned is `frame`, **without seeking**:
    /// the frames in between are decoded and dropped, never returned, and the
    /// ones nothing refers to are not decoded at all (the same B-frame skip a
    /// seek uses). No-op unless `frame` is ahead of [`Self::next_index`].
    ///
    /// The forward half of a scrub. Handing the in-between frames back, which
    /// is what walking with [`Self::next_frame`] did, put them on screen one
    /// after another: a jump a little way ahead *played* the footage in
    /// between instead of going to the frame.
    pub fn skip_to(&mut self, frame: i64) {
        if frame <= self.next_index() {
            return;
        }
        self.skip_until = frame;
        self.skip_pts = self.start + ((frame as f64 - 0.5) / self.fps / self.tick) as i64;
        // Not a seek: there is no keyframe to offer.
        self.keyframe_owed = false;
    }

    /// The keyframe at or before `frame`, from the container's index, or
    /// `None` when the container has none (raw streams, some live captures).
    ///
    /// What decides between [`Self::skip_to`] and [`Self::seek`]: a seek to
    /// `frame` lands on this keyframe and decodes forward from it, so when it
    /// is at or before where the stream already is, seeking only re-decodes
    /// frames already passed and skipping forward is never slower.
    ///
    /// MP4 indexes by decode time, which with B-frames runs a frame or two
    /// ahead of display time; `index_lead` takes that out. Variable reorder
    /// can still leave it a frame early, which errs towards skipping where a
    /// seek would have been a frame cheaper.
    pub fn keyframe_at_or_before(&self, frame: i64) -> Option<i64> {
        // Half a frame late, so rounding can't put `frame`'s own timestamp
        // after the point asked about. Asked and answered in index time.
        let ts = self.start + ((frame.max(0) as f64 + 0.5) / self.fps / self.tick) as i64 - self.index_lead;
        // SAFETY: `stream` indexes this input's own stream array, which lives
        // as long as `input`.
        let i = unsafe {
            let st = *(*self.input.as_ptr()).streams.add(self.stream);
            ffi::av_index_search_timestamp(st, ts, ffi::AVSEEK_FLAG_BACKWARD as i32)
        };
        let at = self.index_entry(i)? + self.index_lead;
        Some((((at - self.start) as f64) * self.tick * self.fps).round() as i64)
    }

    /// Timestamp of entry `i` of the stream's index, in pts ticks.
    fn index_entry(&self, i: i32) -> Option<i64> {
        if i < 0 {
            return None;
        }
        // SAFETY: as above; the entry is read straight away, before anything
        // can add to the index and move it.
        unsafe {
            let st = *(*self.input.as_ptr()).streams.add(self.stream);
            let e = ffi::avformat_index_get_entry(st, i);
            (!e.is_null()).then(|| (*e).timestamp)
        }
    }

    /// Decode the next frame, or `None` at the end of the footage.
    ///
    /// With [`Self::keyframe_first`] set, the first call after a seek may
    /// return a frame whose index is *below* the one sought: the keyframe.
    /// Every later call is in order from the frame asked for.
    pub fn next_frame(&mut self) -> Result<Option<(i64, YuvFrame)>, DecodeError> {
        let mut raw = ff::frame::Video::empty();
        loop {
            match self.decoder.receive_frame(&mut raw) {
                Ok(()) => {
                    let index = match raw.pts().or(raw.timestamp()) {
                        Some(pts) => ((pts - self.start) as f64 * self.tick * self.fps).round() as i64,
                        // No timestamps at all (raw streams): count.
                        None => self.next,
                    };
                    if index < self.skip_until {
                        if std::mem::take(&mut self.keyframe_owed) && self.keyframe_first {
                            return self.convert(&raw).map(|f| Some((index, f)));
                        }
                        continue;
                    }
                    self.keyframe_owed = false;
                    self.skip_until = 0;
                    self.next = index + 1;
                    return self.convert(&raw).map(|f| Some((index, f)));
                }
                Err(ff::Error::Other { errno: ff::error::EAGAIN }) => {}
                Err(ff::Error::Eof) => return Ok(None),
                Err(e) => return Err(err(&self.path, e)),
            }
            // The decoder wants input.
            if self.eof_sent {
                return Ok(None);
            }
            let mut fed = false;
            let mut packet = ff::Packet::empty();
            while packet.read(&mut self.input).is_ok() {
                if packet.stream() == self.stream {
                    // On the way to a sought frame, frames nothing refers to
                    // (B-frames, mostly) needn't be decoded at all: the target
                    // can't depend on them. Roughly halves the cost of a seek
                    // into footage that has them.
                    let skippable = self.skip_until > 0 && packet.pts().is_some_and(|p| p < self.skip_pts);
                    self.decoder.skip_frame(if skippable { ff::Discard::NonReference } else { ff::Discard::Default });
                    self.decoder.send_packet(&packet).map_err(|e| err(&self.path, e))?;
                    fed = true;
                    break;
                }
            }
            if !fed {
                let _ = self.decoder.send_eof();
                self.eof_sent = true;
            }
        }
    }

    fn convert(&mut self, raw: &ff::frame::Video) -> Result<YuvFrame, DecodeError> {
        // A hardware frame lives on the GPU; bring it to system memory. It
        // arrives as NV12 (or P010), which is the layout we want anyway.
        let mut sw = ff::frame::Video::empty();
        // Attaching a device is only an offer; this is whether it was taken.
        self.hardware = unsafe { !(*raw.as_ptr()).hw_frames_ctx.is_null() };
        let frame = if self.hardware {
            let r = unsafe { ffi::av_hwframe_transfer_data(sw.as_mut_ptr(), raw.as_ptr(), 0) };
            if r < 0 {
                return Err(err(&self.path, ff::Error::from(r)));
            }
            unsafe { ffi::av_frame_copy_props(sw.as_mut_ptr(), raw.as_ptr()) };
            &sw
        } else {
            raw
        };

        let (w, h) = (frame.width(), frame.height());
        let (cw, ch) = (w.div_ceil(2) as usize, h.div_ceil(2) as usize);
        let fmt = frame.format();
        let mut out = YuvFrame {
            width: w,
            height: h,
            y: Vec::with_capacity(w as usize * h as usize),
            uv: Vec::with_capacity(cw * ch * 2),
            // Untagged means BT.601, because that is what swscale assumes and
            // therefore what the sidecar and every export already draw. Preview
            // must equal export before it equals convention.
            matrix: match frame.color_space() {
                color::Space::BT709 | color::Space::BT2020NCL | color::Space::BT2020CL => Matrix::Bt709,
                _ => Matrix::Bt601,
            },
            full_range: frame.color_range() == color::Range::JPEG || fmt == Pixel::YUVJ420P,
        };

        let mut converted = ff::frame::Video::empty();
        let frame = match fmt {
            Pixel::NV12 | Pixel::YUV420P | Pixel::YUVJ420P => frame,
            // ponytail: anything else (10-bit, 4:2:2, 4:4:4) goes through
            // swscale down to 8-bit NV12 on this thread. Correct but slow and
            // lossy; a P010 upload path is the upgrade when HDR footage matters.
            _ => {
                let scaler = match &mut self.scaler {
                    Some(s) => s,
                    None => self.scaler.insert(
                        ff::software::scaling::Context::get(fmt, w, h, Pixel::NV12, w, h, ff::software::scaling::Flags::BILINEAR)
                            .map_err(|e| err(&self.path, e))?,
                    ),
                };
                scaler.run(frame, &mut converted).map_err(|e| err(&self.path, e))?;
                &converted
            }
        };

        let (w, h) = (w as usize, h as usize);
        let stride = frame.stride(0);
        for row in frame.data(0).chunks(stride).take(h) {
            out.y.extend_from_slice(&row[..w]);
        }
        if frame.format() == Pixel::NV12 {
            let stride = frame.stride(1);
            for row in frame.data(1).chunks(stride).take(ch) {
                out.uv.extend_from_slice(&row[..cw * 2]);
            }
        } else {
            let (su, sv) = (frame.stride(1), frame.stride(2));
            for (u, v) in frame.data(1).chunks(su).zip(frame.data(2).chunks(sv)).take(ch) {
                for (u, v) in u[..cw].iter().zip(&v[..cw]) {
                    out.uv.push(*u);
                    out.uv.push(*v);
                }
            }
        }
        Ok(out)
    }
}

/// Give the codec context the platform's hardware decode device. Returns
/// whether one was attached; FFmpeg's default `get_format` then picks the
/// hardware pixel format when the codec supports it and software when not.
fn attach_hw_device(ctx: &mut ff::codec::context::Context) -> bool {
    let kind = if cfg!(target_os = "windows") {
        ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_D3D11VA
    } else if cfg!(target_os = "macos") {
        ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_VIDEOTOOLBOX
    } else {
        ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_VAAPI
    };
    unsafe {
        let mut device: *mut ffi::AVBufferRef = std::ptr::null_mut();
        if ffi::av_hwdevice_ctx_create(&mut device, kind, std::ptr::null(), std::ptr::null_mut(), 0) < 0 {
            return false;
        }
        // The context owns the reference from here and frees it with itself.
        (*ctx.as_mut_ptr()).hw_device_ctx = device;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FfmpegDecoder;
    use motion_core::asset::Decoder;
    use std::process::Command;

    /// A clip where every frame is visibly different from its neighbours
    /// (`testsrc2` animates everywhere), generated by ffmpeg so no binary
    /// lives in the repo. `None` when ffmpeg isn't installed.
    fn fixture(name: &str, extra: &[&str]) -> Option<std::path::PathBuf> {
        let path = std::env::temp_dir().join(name);
        let made = Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-f", "lavfi", "-i", "testsrc2=size=320x180:rate=30:duration=3"])
            // A long GOP with B-frames: seeking has to decode forward from a
            // keyframe and reorder, which is where an index goes wrong.
            .args(["-c:v", "libx264", "-pix_fmt", "yuv420p", "-g", "45", "-bf", "2"])
            .args(extra)
            .arg(&path)
            .status();
        if !matches!(made, Ok(s) if s.success()) {
            // Said out loud: a fixture that silently fails to generate turns
            // every test below into a pass that checked nothing.
            eprintln!("skipping: ffmpeg (with libx264) couldn't generate {name}");
            return None;
        }
        Some(path)
    }

    fn mean_abs_diff(a: &[u8], b: &[u8]) -> f64 {
        assert_eq!(a.len(), b.len());
        a.iter().zip(b).map(|(x, y)| (*x as f64 - *y as f64).abs()).sum::<f64>() / a.len() as f64
    }

    /// **The Phase 0 parity gate.** Frame N from the in-process decoder is the
    /// same picture as frame N from the sidecar, after a seek into the middle
    /// of a GOP — and is *not* the picture of N-1 or N+1, which is what an
    /// off-by-one in the pts→index mapping would look like.
    fn libav_matches_the_sidecar(name: &str, extra: &[&str], hardware: bool) {
        let Some(path) = fixture(name, extra) else { return };
        let sidecar = FfmpegDecoder::new();
        let meta = sidecar.open(&path).expect("probe the fixture");

        for want in [0i64, 1, 37, 52, 89] {
            let mut stream = LibavStream::open(&path, want, meta.fps, hardware).expect("open");
            let (index, frame) = stream.next_frame().expect("decode").expect("a frame");
            assert_eq!(index, want, "the first frame after a seek is the one asked for");
            let ours = frame.to_rgba();

            let same = mean_abs_diff(&ours, &sidecar.frame(&path, want, &meta).unwrap().rgba);
            // Not bit-exact: swscale interpolates chroma and we replicate it.
            assert!(same < 2.5, "frame {want} differs from the sidecar's by {same:.2}");
            for neighbour in [want - 1, want + 1] {
                if neighbour < 0 || neighbour >= meta.frames {
                    continue;
                }
                let other = mean_abs_diff(&ours, &sidecar.frame(&path, neighbour, &meta).unwrap().rgba);
                assert!(other > same * 2.0, "frame {want} looks as much like {neighbour} ({other:.2}) as itself ({same:.2})");
            }

            // And walking forward from there stays in step.
            if want + 1 < meta.frames {
                let (next, _) = stream.next_frame().unwrap().unwrap();
                assert_eq!(next, want + 1);
            }
        }
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn frame_n_matches_the_sidecar_after_a_seek() {
        libav_matches_the_sidecar("pbc_libav_parity.mp4", &[], false);
    }

    /// Tagged BT.709 footage takes the other matrix; both paths must honour it.
    #[test]
    fn bt709_footage_matches_the_sidecar() {
        libav_matches_the_sidecar(
            "pbc_libav_parity_709.mp4",
            &["-colorspace", "bt709", "-color_primaries", "bt709", "-color_trc", "bt709"],
            false,
        );
    }

    /// The hardware decoder, where there is one, is the same decoder as far as
    /// the picture goes. On a machine without it this exercises the fallback.
    #[test]
    fn hardware_decode_matches_the_sidecar() {
        libav_matches_the_sidecar("pbc_libav_parity_hw.mp4", &[], true);
    }

    /// The keyframe a seek lands on is offered once, early, and does not
    /// disturb the order of what follows.
    #[test]
    fn a_seek_can_show_its_keyframe_first() {
        let Some(path) = fixture("pbc_libav_kf.mp4", &[]) else { return };
        let mut stream = LibavStream::open(&path, 0, 30.0, false).unwrap();
        stream.keyframe_first = true;
        stream.seek(70).unwrap();
        // GOP is 45, so the keyframe before 70 is 45.
        let seen: Vec<i64> = (0..3).map(|_| stream.next_frame().unwrap().unwrap().0).collect();
        assert_eq!(seen, vec![45, 70, 71]);
        let _ = std::fs::remove_file(&path);
    }

    /// Skipping forward returns the frame asked for and nothing before it, and
    /// it is the same picture a seek to that frame gives.
    #[test]
    fn skipping_forward_returns_only_the_frame_asked_for() {
        let Some(path) = fixture("pbc_libav_skip.mp4", &[]) else { return };
        let sidecar = FfmpegDecoder::new();
        let meta = sidecar.open(&path).expect("probe the fixture");
        let mut stream = LibavStream::open(&path, 10, meta.fps, false).unwrap();
        stream.keyframe_first = true;
        assert_eq!(stream.next_frame().unwrap().unwrap().0, 0, "the seek's keyframe");
        assert_eq!(stream.next_frame().unwrap().unwrap().0, 10);
        // Inside the GOP, then across into the next one.
        for want in [30i64, 62] {
            stream.skip_to(want);
            assert_eq!(stream.next_index(), want);
            let (index, frame) = stream.next_frame().unwrap().unwrap();
            assert_eq!(index, want, "skipping hands back no frame before the target");
            let diff = mean_abs_diff(&frame.to_rgba(), &sidecar.frame(&path, want, &meta).unwrap().rgba);
            assert!(diff < 2.5, "frame {want} differs from the sidecar's by {diff:.2}");
        }
        // Backwards is not a skip.
        stream.skip_to(5);
        assert_eq!(stream.next_frame().unwrap().unwrap().0, 63);
        let _ = std::fs::remove_file(&path);
    }

    /// The index names the GOP boundaries (45 frames here), with the B-frame
    /// reorder delay in its decode-order timestamps taken out — without that,
    /// the keyframe at 45 reads as 43 and "the keyframe before 44" is wrong.
    #[test]
    fn the_index_knows_where_the_keyframes_are() {
        let Some(path) = fixture("pbc_libav_index.mp4", &[]) else { return };
        let stream = LibavStream::open(&path, 0, 30.0, false).unwrap();
        let near = |got: Option<i64>, kf: i64| got.is_some_and(|g| (kf - 1..=kf).contains(&g));
        assert!(near(stream.keyframe_at_or_before(0), 0), "{:?}", stream.keyframe_at_or_before(0));
        assert!(near(stream.keyframe_at_or_before(44), 0), "{:?}", stream.keyframe_at_or_before(44));
        assert!(near(stream.keyframe_at_or_before(70), 45), "{:?}", stream.keyframe_at_or_before(70));
        assert!(near(stream.keyframe_at_or_before(89), 45), "{:?}", stream.keyframe_at_or_before(89));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_stream_ends_cleanly_at_the_last_frame() {
        let Some(path) = fixture("pbc_libav_end.mp4", &[]) else { return };
        let mut stream = LibavStream::open(&path, 85, 30.0, false).unwrap();
        let mut seen = Vec::new();
        while let Some((i, _)) = stream.next_frame().unwrap() {
            seen.push(i);
        }
        assert_eq!(seen, vec![85, 86, 87, 88, 89]);
        let _ = std::fs::remove_file(&path);
    }
}
