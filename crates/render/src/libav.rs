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

pub use crate::yuv::{Matrix, Transfer, YuvFrame};

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
    /// Clockwise quarter turns that make the picture upright (phone footage
    /// stored sideways). The sidecar's ffmpeg applies the same turn itself.
    turns: u8,
    path: std::path::PathBuf,
}

/// Clockwise quarter turns (0..4) the stream's display matrix asks for.
fn display_turns(st: &ff::format::stream::Stream) -> u8 {
    unsafe {
        let par = (*st.as_ptr()).codecpar;
        let sd = ffi::av_packet_side_data_get(
            (*par).coded_side_data,
            (*par).nb_coded_side_data,
            ffi::AVPacketSideDataType::AV_PKT_DATA_DISPLAYMATRIX,
        );
        if sd.is_null() || (*sd).size < 36 {
            return 0;
        }
        // Counter-clockwise degrees, so `-90` means turn clockwise once —
        // the same reading as ffmpeg's own autorotate.
        let ccw = ffi::av_display_rotation_get((*sd).data as *const i32);
        if !ccw.is_finite() {
            return 0;
        }
        ((-ccw / 90.0).round() as i64).rem_euclid(4) as u8
    }
}

/// `plane` (`w`×`h` pixels of `px` bytes) turned clockwise `turns` times.
// ponytail: a CPU transpose, a few ms per 4K frame; rotating in the YUV shader
// is the upgrade if portrait footage has to keep up with several clips.
fn rotate_plane(plane: &[u8], w: usize, h: usize, px: usize, turns: u8) -> Vec<u8> {
    if turns == 0 {
        return plane.to_vec();
    }
    let mut out = vec![0u8; plane.len()];
    let ow = if turns == 2 { w } else { h };
    for y in 0..h {
        for x in 0..w {
            let (ox, oy) = match turns {
                1 => (h - 1 - y, x),
                2 => (w - 1 - x, h - 1 - y),
                _ => (y, w - 1 - x),
            };
            let (s, d) = ((y * w + x) * px, (oy * ow + ox) * px);
            out[d..d + px].copy_from_slice(&plane[s..s + px]);
        }
    }
    out
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
        let turns = display_turns(&st);

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
            turns,
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
                color::Space::BT709 => Matrix::Bt709,
                color::Space::BT2020NCL | color::Space::BT2020CL => Matrix::Bt2020,
                _ => Matrix::Bt601,
            },
            full_range: frame.color_range() == color::Range::JPEG || fmt == Pixel::YUVJ420P,
            transfer: match frame.color_transfer_characteristic() {
                color::TransferCharacteristic::ARIB_STD_B67 => Transfer::Hlg,
                color::TransferCharacteristic::SMPTE2084 => Transfer::Pq,
                _ => Transfer::Sdr,
            },
        };

        // 10-bit 4:2:0 (phone HDR, and what a hardware decoder hands back for
        // it as P010): keep the top 8 bits of each sample. A shift per sample
        // where swscale cost a 4K frame's worth of filtering.
        // ponytail: 8 bits of an HDR signal can band in smooth skies; a P010
        // texture (R16Unorm planes) is the upgrade.
        let shift = match fmt {
            Pixel::YUV420P10LE => Some(2),
            Pixel::P010LE => Some(8),
            _ => None,
        };
        if let Some(shift) = shift {
            let top = |row: &[u8], n: usize, out: &mut Vec<u8>| {
                out.extend(row[..n * 2].chunks_exact(2).map(|p| ((u16::from_le_bytes([p[0], p[1]]) as u32 + (1 << shift >> 1)) >> shift).min(255) as u8));
            };
            let (wu, hu) = (w as usize, h as usize);
            for row in frame.data(0).chunks(frame.stride(0)).take(hu) {
                top(row, wu, &mut out.y);
            }
            if fmt == Pixel::P010LE {
                for row in frame.data(1).chunks(frame.stride(1)).take(ch) {
                    top(row, cw * 2, &mut out.uv);
                }
            } else {
                let (mut u, mut v) = (Vec::with_capacity(cw), Vec::with_capacity(cw));
                for (ur, vr) in frame.data(1).chunks(frame.stride(1)).zip(frame.data(2).chunks(frame.stride(2))).take(ch) {
                    u.clear();
                    v.clear();
                    top(ur, cw, &mut u);
                    top(vr, cw, &mut v);
                    out.uv.extend(u.iter().zip(&v).flat_map(|(a, b)| [*a, *b]));
                }
            }
            return Ok(self.turn(out));
        }

        let mut converted = ff::frame::Video::empty();
        let frame = match fmt {
            Pixel::NV12 | Pixel::YUV420P | Pixel::YUVJ420P => frame,
            // ponytail: anything else (4:2:2, 4:4:4, 12-bit) goes through
            // swscale down to 8-bit NV12 on this thread. Correct but slow.
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
        Ok(self.turn(out))
    }

    /// `frame` turned upright by the stream's display rotation.
    fn turn(&self, mut out: YuvFrame) -> YuvFrame {
        if self.turns != 0 {
            let (w, h) = (out.width as usize, out.height as usize);
            let (cw, ch) = (out.chroma_width() as usize, out.chroma_height() as usize);
            out.y = rotate_plane(&out.y, w, h, 1, self.turns);
            out.uv = rotate_plane(&out.uv, cw, ch, 2, self.turns);
            if self.turns % 2 == 1 {
                (out.width, out.height) = (out.height, out.width);
            }
        }
        out
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
        parity(&path, hardware);
    }

    fn parity(path: &std::path::Path, hardware: bool) {
        let path = path.to_path_buf();
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

    /// HLG phone footage: tone mapped the same way by both paths. 8-bit
    /// takes the NV12 route, 10-bit the shift that replaced swscale.
    #[test]
    fn hlg_footage_matches_the_sidecar() {
        let hlg = ["-colorspace", "bt2020nc", "-color_primaries", "bt2020", "-color_trc", "arib-std-b67"];
        libav_matches_the_sidecar("pbc_libav_parity_hlg.mp4", &hlg, false);
        let ten: Vec<&str> = hlg.iter().copied().chain(["-pix_fmt", "yuv420p10le"]).collect();
        libav_matches_the_sidecar("pbc_libav_parity_hlg10.mp4", &ten, false);
    }

    /// The hardware decoder, where there is one, is the same decoder as far as
    /// the picture goes. On a machine without it this exercises the fallback.
    #[test]
    fn hardware_decode_matches_the_sidecar() {
        libav_matches_the_sidecar("pbc_libav_parity_hw.mp4", &[], true);
    }

    /// Portrait phone footage: stored sideways with a display rotation. Both
    /// paths show it upright, at the size the import reports.
    #[test]
    fn rotated_footage_matches_the_sidecar() {
        let Some(flat) = fixture("pbc_libav_flat.mp4", &[]) else { return };
        let path = std::env::temp_dir().join("pbc_libav_rotated.mp4");
        let made = Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-display_rotation", "-90", "-i"])
            .arg(&flat)
            .args(["-c", "copy"])
            .arg(&path)
            .status();
        if !matches!(made, Ok(s) if s.success()) {
            eprintln!("skipping: this ffmpeg can't tag a display rotation");
            return;
        }
        let meta = FfmpegDecoder::new().open(&path).unwrap();
        assert_eq!((meta.width, meta.height), (180.0, 320.0), "imported upright");
        let (_, f) = LibavStream::open(&path, 0, meta.fps, false).unwrap().next_frame().unwrap().unwrap();
        assert_eq!((f.width, f.height), (180, 320));
        parity(&path, false);
        let _ = std::fs::remove_file(&flat);
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
