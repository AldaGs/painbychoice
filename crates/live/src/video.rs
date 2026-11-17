//! Realtime video for the preview: in-process decode, YUV on the GPU.
//!
//! The sidecar path in [`crate::footage`] hands vello a 33MB RGBA buffer per 4K
//! frame, converted on the CPU and re-uploaded every redraw. That is fine for
//! one clip and a slideshow for three. This module is the other path
//! (ADR 0023), and it differs in exactly the ways that matter:
//!
//! - **One decode thread per clip**, each running ahead of the playhead into
//!   its own small cache keyed by frame. A clip never waits on another clip.
//! - **Frames stay YUV.** The planes the codec produced are uploaded as two
//!   textures and converted by a fragment shader into a texture vello draws
//!   through `override_image`. No RGBA ever exists on the CPU.
//! - **Nothing here tells the time.** The playhead comes from the master clock
//!   (ADR 0021) exactly as before; a redraw shows the newest decoded frame at
//!   or before it and never waits. Video drops frames, audio never hears of it.
//!
//! Preview only. Export keeps the sidecar, which the decoder's tests pin this
//! path to frame for frame.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Instant;

use motion_core::asset::{Asset, AssetId, ImagePaint};
use motion_render::libav::{LibavStream, YuvFrame};
use vello::peniko::{Blob, ImageAlphaType, ImageData, ImageFormat};
use vello::wgpu;

/// Decoded frames one clip may hold. A 4K NV12 frame is 12.4MB, so this is
/// about thirteen of them — under half a second of run-ahead, which is all a
/// decoder that keeps up needs.
const CLIP_BUDGET_BYTES: usize = 160 * 1024 * 1024;
/// The run-ahead ceiling for small footage, where the byte budget would allow
/// hundreds of frames nobody is waiting for.
const MAX_AHEAD: i64 = 24;
/// Frames kept behind the playhead, so a late redraw still has the frame it
/// was about to show and a one-frame step back is free.
const KEEP_BEHIND: i64 = 2;
/// How far ahead of the decoder a wanted frame may be before a seek beats
/// decoding forward to it — for footage whose container has no index. With an
/// index the keyframes decide (see `decode_loop`).
const WALK: i64 = 12;
/// How long a seek may take before its keyframe is put up as a stand-in.
///
/// The keyframe can be a whole GOP (seconds) before the frame asked for. When
/// the exact frame follows within a few frames' time — a hardware decoder on
/// 1080p, any all-intra proxy — flashing it first only shows footage nobody
/// asked to see, so the screen holds the last picture and goes straight to the
/// exact frame. Past this, something near the target beats a frozen picture.
const KEYFRAME_AFTER: std::time::Duration = std::time::Duration::from_millis(150);

/// A picture for a position whose exact frame isn't decoded yet.
struct Approx {
    index: i64,
    frame: Arc<YuvFrame>,
    /// The frame this was decoded on the way to.
    for_want: i64,
    /// For the keyframe a seek landed on: when the seek began. It is shown
    /// only once [`KEYFRAME_AFTER`] has passed. `None` for an exact frame the
    /// playhead moved past while it decoded, which is shown straight away: it
    /// is where the playhead just was, not footage it skipped over.
    seeked: Option<Instant>,
}

/// What the decode thread and the UI share.
struct Queue {
    frames: BTreeMap<i64, Arc<YuvFrame>>,
    /// The frame the UI is drawing. The thread fills `want..=want + ahead`.
    want: i64,
    /// Something to show while the exact frame is still a GOP's decoding away.
    approx: Option<Approx>,
    /// First frame past the end of the file, once the decoder has found it.
    end: Option<i64>,
    /// The file would not open or decode. The clip falls back to the sidecar.
    error: Option<String>,
    /// Whether the decoder is actually running on the GPU.
    hardware: bool,
    stop: bool,
}

struct Shared {
    q: Mutex<Queue>,
    wake: Condvar,
}

fn decode_loop(shared: Arc<Shared>, path: PathBuf, fps: f64, frame_bytes: usize, hardware: bool) {
    let ahead = ((CLIP_BUDGET_BYTES / frame_bytes.max(1)) as i64 - KEEP_BEHIND).clamp(2, MAX_AHEAD);
    let mut stream: Option<LibavStream> = None;
    let fail = |e: String| shared.q.lock().unwrap().error = Some(e);
    loop {
        // The first frame of the window that isn't decoded yet — or sleep
        // until the playhead moves and there is one.
        let need = {
            let mut q = shared.q.lock().unwrap();
            loop {
                if q.stop {
                    return;
                }
                let (want, end) = (q.want, q.end.unwrap_or(i64::MAX));
                q.frames.retain(|f, _| *f >= want - KEEP_BEHIND && *f <= want + ahead);
                match (want..=want + ahead).find(|f| *f < end && !q.frames.contains_key(f)) {
                    Some(n) => break n,
                    None => q = shared.wake.wait(q).unwrap(),
                }
            }
        };

        let s = match &mut stream {
            Some(s) => s,
            None => match LibavStream::open(&path, need, fps, hardware) {
                Ok(mut s) => {
                    s.keyframe_first = true;
                    stream.insert(s)
                }
                Err(e) => return fail(e.to_string()),
            },
        };
        // Forward or seek. A seek lands on the keyframe at or before `need` and
        // decodes forward from there, so when that keyframe is no further on
        // than the stream already is, skipping forward does the same work
        // minus the frames already behind us. Either way nothing between here
        // and `need` is handed back: walking with `next_frame` used to, and
        // every one of those frames went on screen in turn, so a jump a little
        // way ahead played the footage in between.
        let at = s.next_index();
        let forward = need >= at
            && match s.keyframe_at_or_before(need) {
                Some(kf) => kf <= at,
                None => need - at <= WALK,
            };
        let mut seeked = None;
        if forward {
            s.skip_to(need);
        } else {
            seeked = Some(Instant::now());
            if let Err(e) = s.seek(need) {
                return fail(e.to_string());
            }
        }
        match s.next_frame() {
            Ok(Some((index, frame))) => {
                let frame = Arc::new(frame);
                let mut q = shared.q.lock().unwrap();
                q.hardware = s.is_hardware();
                if index < need {
                    // The keyframe on the way to `need`: something to look at,
                    // if the exact frame turns out to be slow.
                    q.approx = Some(Approx { index, frame, for_want: need, seeked: seeked.or(Some(Instant::now())) });
                    continue;
                }
                if index < q.want - KEEP_BEHIND {
                    // The playhead moved on while this decoded (a fast drag).
                    // Too far behind for the cache, but it is the latest real
                    // position, which beats a keyframe or a frozen picture.
                    q.approx = Some(Approx { index, frame: frame.clone(), for_want: need, seeked: None });
                }
                // ponytail: a gap in the timestamps (variable frame rate) is
                // filled by holding this frame over it, or `need` would stay
                // missing and the loop would seek to it forever. Proper VFR
                // wants pts-based lookup instead of a frame grid.
                for i in need.min(index)..=index {
                    if i >= q.want - KEEP_BEHIND {
                        q.frames.insert(i, frame.clone());
                    }
                }
            }
            Ok(None) => shared.q.lock().unwrap().end = Some(s.next_index().min(need)),
            Err(e) => return fail(e.to_string()),
        }
    }
}

/// The GPU side of one clip.
struct ClipTex {
    y: wgpu::Texture,
    uv: wgpu::Texture,
    rgba_view: wgpu::TextureView,
    uniforms: wgpu::Buffer,
    bind: wgpu::BindGroup,
}

struct Clip {
    shared: Arc<Shared>,
    thread: Option<std::thread::JoinHandle<()>>,
    /// What scenes draw. A stand-in with no pixels of its own: vello swaps in
    /// the converted texture by this image's blob id.
    handle: ImageData,
    tex: Option<ClipTex>,
    want: i64,
    since: Instant,
    /// Whether `want` has been on screen exactly, for the drop count.
    hit: bool,
    shown: Option<i64>,
    /// Whether what is on screen belongs to the current `want` — the exact
    /// frame, a neighbour, or the keyframe of the seek towards it.
    fresh: bool,
    /// Drawn in the redraw being built.
    touched: bool,
    pending: bool,
}

impl Drop for Clip {
    fn drop(&mut self) {
        self.shared.q.lock().unwrap().stop = true;
        self.shared.wake.notify_all();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// What the preview has managed, for `docs/performance.md` and for
/// `PBC_VIDEO_STATS=1`.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Stats {
    /// Distinct frames the playhead asked a clip for.
    pub(crate) wanted: u64,
    /// Of those, how many the playhead left before they were shown.
    pub(crate) dropped: u64,
    /// The slowest request-to-screen time, in milliseconds.
    pub(crate) worst_ms: f64,
    /// Frames put on screen that were not the frame the playhead was on: the
    /// stand-ins shown while the exact frame decodes.
    pub(crate) standins: u64,
}

pub(crate) enum Lookup<'a> {
    /// Draw this; the flag says whether it is the exact frame.
    Ready(&'a ImageData, bool),
    /// Nothing decoded yet. Draw the layer's placeholder for a moment.
    Pending,
    /// This clip can't be decoded in-process; use the sidecar.
    Unavailable,
}

pub(crate) struct VideoPlayback {
    clips: HashMap<AssetId, Clip>,
    gpu: Option<Gpu>,
    pub(crate) stats: Stats,
    /// Ask FFmpeg for the platform's hardware decoder. `PBC_HWDEC=0` turns it
    /// off, for measuring and for drivers that misbehave.
    pub(crate) hardware: bool,
}

impl Default for VideoPlayback {
    fn default() -> Self {
        Self {
            clips: HashMap::new(),
            gpu: None,
            stats: Stats::default(),
            hardware: std::env::var("PBC_HWDEC").map_or(true, |v| v != "0"),
        }
    }
}

impl VideoPlayback {
    /// The image to draw for `paint`, noting that the playhead wants it.
    pub(crate) fn image(&mut self, asset: &Asset, paint: ImagePaint) -> Lookup<'_> {
        let want = paint.source_frame;
        let hardware = self.hardware;
        let clip = self.clips.entry(paint.asset).or_insert_with(|| {
            let (w, h) = (asset.width.round().max(1.0) as u32, asset.height.round().max(1.0) as u32);
            let shared = Arc::new(Shared {
                q: Mutex::new(Queue { frames: BTreeMap::new(), want, approx: None, end: None, error: None, hardware: false, stop: false }),
                wake: Condvar::new(),
            });
            let (s, path, fps) = (shared.clone(), asset.path.clone(), asset.fps);
            let frame_bytes = (w as usize * h as usize * 3) / 2;
            let thread = std::thread::Builder::new()
                .name("video-decode".into())
                .spawn(move || decode_loop(s, path, fps, frame_bytes, hardware))
                .ok();
            Clip {
                shared,
                thread,
                handle: ImageData {
                    data: Blob::new(Arc::new([])),
                    format: ImageFormat::Rgba8,
                    alpha_type: ImageAlphaType::Alpha,
                    width: w,
                    height: h,
                },
                tex: None,
                // One off, so the first request counts as a change below.
                want: want - 1,
                since: Instant::now(),
                hit: true,
                shown: None,
                fresh: false,
                touched: false,
                pending: false,
            }
        });
        if clip.thread.is_none() {
            return Lookup::Unavailable;
        }
        if clip.want != want {
            self.stats.wanted += 1;
            if !clip.hit {
                self.stats.dropped += 1;
            }
            clip.want = want;
            clip.since = Instant::now();
            clip.hit = clip.shown == Some(want);
            clip.fresh = clip.hit;
            let mut q = clip.shared.q.lock().unwrap();
            if q.error.is_some() {
                return Lookup::Unavailable;
            }
            q.want = want;
            clip.shared.wake.notify_all();
        }
        clip.touched = true;
        match clip.shown {
            Some(shown) => Lookup::Ready(&clip.handle, shown == want),
            None if clip.shared.q.lock().unwrap().error.is_some() => Lookup::Unavailable,
            None => {
                clip.pending = true;
                Lookup::Pending
            }
        }
    }

    /// Upload and convert the newest frame of every clip drawn this redraw.
    /// Called between building the scene and rendering it. Never waits on a
    /// decoder: a clip whose frame isn't ready keeps showing the one it has.
    pub(crate) fn prepare(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, renderer: &mut vello::Renderer) {
        // ponytail: one device. A second window on a different adapter would
        // draw the stand-in image; give each device its own textures if that
        // configuration ever shows up.
        let gpu = self.gpu.get_or_insert_with(|| Gpu::new(device));
        for clip in self.clips.values_mut() {
            if !std::mem::take(&mut clip.touched) {
                // Not on screen: nothing to wait for.
                clip.pending = false;
                continue;
            }
            let (pick, ended) = {
                let q = clip.shared.q.lock().unwrap();
                // Only frames near the playhead: straight after a jump the
                // cache still holds the old position until the decoder prunes
                // it, and that is not a picture of where the playhead is.
                let pick = q
                    .frames
                    .range(clip.want - KEEP_BEHIND..=clip.want)
                    .next_back()
                    .or_else(|| q.frames.range(clip.want..=clip.want + MAX_AHEAD).next())
                    .map(|(i, f)| (*i, f.clone(), true))
                    .or_else(|| {
                        // A stand-in has to earn its place: nearer the playhead
                        // than the picture already up. Otherwise it is left
                        // over from an earlier position — the keyframe of some
                        // seek long ago — and putting it up shows footage from
                        // nowhere near where the playhead is.
                        let nearer = |a: &Approx| clip.shown.is_none_or(|s| (a.index - clip.want).abs() < (s - clip.want).abs());
                        q.approx
                            .as_ref()
                            .filter(|a| nearer(a) && a.seeked.is_none_or(|t| t.elapsed() >= KEYFRAME_AFTER))
                            .map(|a| (a.index, a.frame.clone(), a.for_want == clip.want))
                    });
                (pick, q.error.is_some() || q.end.is_some_and(|e| clip.want >= e))
            };
            if let Some((index, frame, fresh)) = pick {
                clip.fresh |= fresh;
                if clip.shown != Some(index) {
                    let tex = clip.tex.get_or_insert_with(|| {
                        let (tex, rgba) = gpu.clip_textures(device, clip.handle.width, clip.handle.height);
                        renderer.override_image(
                            &clip.handle,
                            Some(wgpu::TexelCopyTextureInfoBase {
                                texture: rgba,
                                mip_level: 0,
                                origin: wgpu::Origin3d::ZERO,
                                aspect: wgpu::TextureAspect::All,
                            }),
                        );
                        tex
                    });
                    // A file whose frames aren't the size the probe reported
                    // can't go into these textures; leave the last picture up.
                    if (frame.width, frame.height) == (clip.handle.width, clip.handle.height) {
                        gpu.convert(device, queue, tex, &frame);
                        renderer.mark_override_image_dirty(&clip.handle);
                        clip.shown = Some(index);
                        if index != clip.want {
                            self.stats.standins += 1;
                        }
                    }
                }
                if index == clip.want && !clip.hit {
                    clip.hit = true;
                    self.stats.worst_ms = self.stats.worst_ms.max(clip.since.elapsed().as_secs_f64() * 1e3);
                }
            }
            clip.pending = clip.shown != Some(clip.want) && !ended;
        }
    }

    /// Whether a redraw is owed: some clip on screen is not yet showing the
    /// frame the playhead asked for.
    pub(crate) fn is_busy(&self) -> bool {
        self.clips.values().any(|c| c.pending)
    }

    /// Whether every clip drawn last redraw is showing exactly its frame.
    #[cfg(test)]
    pub(crate) fn settled(&self) -> bool {
        self.clips.values().all(|c| c.shown == Some(c.want))
    }

    /// Whether every clip is showing *a* frame for where the playhead now is.
    #[cfg(test)]
    pub(crate) fn fresh(&self) -> bool {
        self.clips.values().all(|c| c.fresh)
    }

    /// How many clips are decoding on the GPU, of how many are open.
    #[cfg(test)]
    pub(crate) fn hardware_clips(&self) -> (usize, usize) {
        (self.clips.values().filter(|c| c.shared.q.lock().unwrap().hardware).count(), self.clips.len())
    }

    /// Stop every decoder and forget every clip. Asset ids are per-project.
    pub(crate) fn clear(&mut self, renderer: Option<&mut vello::Renderer>) {
        if let Some(r) = renderer {
            for clip in self.clips.values() {
                r.override_image(&clip.handle, None);
            }
        }
        self.clips.clear();
    }
}

/// The YUV→RGB conversion pass, shared by every clip.
struct Gpu {
    pipeline: wgpu::RenderPipeline,
    sampler: wgpu::Sampler,
}

const SHADER: &str = r#"
struct U { off: vec4<f32>, r: vec4<f32>, g: vec4<f32>, b: vec4<f32> };
@group(0) @binding(0) var y_tex: texture_2d<f32>;
@group(0) @binding(1) var uv_tex: texture_2d<f32>;
@group(0) @binding(2) var samp: sampler;
@group(0) @binding(3) var<uniform> u: U;

struct V { @builtin(position) pos: vec4<f32>, @location(0) uv: vec2<f32> };

// One triangle covering the target.
@vertex fn vs(@builtin(vertex_index) i: u32) -> V {
    let p = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    var o: V;
    o.pos = vec4<f32>(p * 2.0 - 1.0, 0.0, 1.0);
    o.uv = vec2<f32>(p.x, 1.0 - p.y);
    return o;
}

// `motion_render::yuv::hdr_to_sdr`, line for line; `off.w` is the transfer
// (0 SDR, 1 HLG, 2 PQ). The test below holds the two together.
fn hdr_to_sdr(rgb: vec3<f32>, transfer: f32) -> vec3<f32> {
    var nits: vec3<f32>;
    if (transfer < 0.5) {
        return rgb;
    } else if (transfer < 1.5) {
        let a = 0.17883277; let b = 0.28466892; let c = 0.5599107;
        let v = clamp(rgb, vec3<f32>(0.0), vec3<f32>(1.0));
        let e = select((exp((v - c) / a) + b) / 12.0, v * v / 3.0, v <= vec3<f32>(0.5));
        let ys = dot(vec3<f32>(0.2627, 0.6780, 0.0593), e);
        nits = e * (1000.0 * pow(max(ys, 1e-6), 0.2));
    } else {
        let p = pow(clamp(rgb, vec3<f32>(0.0), vec3<f32>(1.0)), vec3<f32>(1.0 / 78.84375));
        nits = 10000.0 * pow(max(p - 0.8359375, vec3<f32>(0.0)) / (18.851563 - 18.6875 * p), vec3<f32>(1.0 / 0.15930176));
    }
    let x = nits / 406.0;
    let lin = max(vec3<f32>(
        dot(vec3<f32>(1.6605, -0.5876, -0.0728), x),
        dot(vec3<f32>(-0.1246, 1.1329, -0.0083), x),
        dot(vec3<f32>(-0.0182, -0.1006, 1.1187), x),
    ), vec3<f32>(0.0));
    let peak = max(lin.r, max(lin.g, lin.b));
    let k = 0.8;
    let top = (1000.0 / 406.0 - k) / (1.0 - k);
    var scale = 1.0;
    if (peak > k) {
        let t = (peak - k) / (1.0 - k);
        scale = (k + (1.0 - k) * min(t * (1.0 + t / (top * top)) / (1.0 + t), 1.0)) / peak;
    }
    let s = lin * scale;
    let grey = dot(vec3<f32>(0.2126, 0.7152, 0.0722), s);
    let desat = 0.5 * clamp((peak - k) / (1.0 - k), 0.0, 1.0);
    return pow(min(mix(s, vec3<f32>(grey), desat), vec3<f32>(1.0)), vec3<f32>(1.0 / 2.4));
}

@fragment fn fs(v: V) -> @location(0) vec4<f32> {
    let yuv = vec3<f32>(textureSample(y_tex, samp, v.uv).r, textureSample(uv_tex, samp, v.uv).rg) - u.off.xyz;
    let rgb = vec3<f32>(dot(u.r.xyz, yuv), dot(u.g.xyz, yuv), dot(u.b.xyz, yuv));
    return vec4<f32>(hdr_to_sdr(rgb, u.off.w), 1.0);
}
"#;

impl Gpu {
    fn new(device: &wgpu::Device) -> Self {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("yuv-to-rgb"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("yuv-to-rgb"),
            layout: None,
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                // Written as-is: these are the same non-linear bytes the
                // sidecar hands vello, so the two paths draw alike.
                targets: &[Some(wgpu::TextureFormat::Rgba8Unorm.into())],
            }),
            multiview_mask: None,
            cache: None,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        Gpu { pipeline, sampler }
    }

    /// The plane textures for one clip, and the RGBA texture vello reads.
    fn clip_textures(&self, device: &wgpu::Device, w: u32, h: u32) -> (ClipTex, wgpu::Texture) {
        let tex = |label, width: u32, height: u32, format, usage| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage,
                view_formats: &[],
            })
        };
        let planes = wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST;
        let y = tex("video-y", w, h, wgpu::TextureFormat::R8Unorm, planes);
        let uv = tex("video-uv", w.div_ceil(2), h.div_ceil(2), wgpu::TextureFormat::Rg8Unorm, planes);
        let rgba = tex(
            "video-rgba",
            w,
            h,
            wgpu::TextureFormat::Rgba8Unorm,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        );
        let uniforms = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("video-matrix"),
            size: 64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let (yv, uvv) = (y.create_view(&Default::default()), uv.create_view(&Default::default()));
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("video"),
            layout: &self.pipeline.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&yv) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&uvv) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&self.sampler) },
                wgpu::BindGroupEntry { binding: 3, resource: uniforms.as_entire_binding() },
            ],
        });
        let rgba_view = rgba.create_view(&Default::default());
        (ClipTex { y, uv, rgba_view, uniforms, bind }, rgba)
    }

    /// Upload one frame's planes and convert them into the clip's RGBA texture.
    fn convert(&self, device: &wgpu::Device, queue: &wgpu::Queue, tex: &ClipTex, frame: &YuvFrame) {
        let write = |texture: &wgpu::Texture, data: &[u8], width: u32, height: u32, bpp: u32| {
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                data,
                wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(width * bpp), rows_per_image: Some(height) },
                wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
            );
        };
        write(&tex.y, &frame.y, frame.width, frame.height, 1);
        write(&tex.uv, &frame.uv, frame.chroma_width(), frame.chroma_height(), 2);

        let (off, m) = frame.coefficients();
        let rows = [[off[0], off[1], off[2], frame.transfer.code()], [m[0][0], m[0][1], m[0][2], 0.0], [m[1][0], m[1][1], m[1][2], 0.0], [m[2][0], m[2][1], m[2][2], 0.0]];
        let bytes: Vec<u8> = rows.iter().flatten().flat_map(|f| f.to_ne_bytes()).collect();
        queue.write_buffer(&tex.uniforms, 0, &bytes);

        let mut encoder = device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("yuv-to-rgb"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &tex.rgba_view,
                    resolve_target: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::BLACK), store: wgpu::StoreOp::Store },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &tex.bind, &[]);
            pass.draw(0..3, 0..1);
        }
        queue.submit([encoder.finish()]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::offscreen::Headless;
    use motion_render::libav::{Matrix, Transfer};

    /// **The shader is the CPU reference.** The same planes through
    /// `YuvFrame::to_rgba` and through the GPU pass give the same picture, so
    /// the decoder's parity test against the sidecar covers the preview too.
    #[test]
    fn the_shader_agrees_with_the_cpu_conversion() {
        let Some(mut gpu) = Headless::new() else {
            eprintln!("skipping: no GPU adapter");
            return;
        };
        let (w, h) = (64u32, 36u32);
        let cases = [
            (Matrix::Bt601, false, Transfer::Sdr),
            (Matrix::Bt709, false, Transfer::Sdr),
            (Matrix::Bt709, true, Transfer::Sdr),
            (Matrix::Bt2020, false, Transfer::Hlg),
            (Matrix::Bt2020, false, Transfer::Pq),
        ];
        // Smooth chroma ramps: chroma is interpolated on the GPU and replicated
        // on the CPU, and a ramp keeps that difference under a level — or a
        // little over where an HDR curve steepens it. Flat chroma has nothing
        // to interpolate, so there the maths alone is compared, tightly.
        let ramp: Vec<u8> = (0..(w / 2) * (h / 2)).flat_map(|i| [(64 + (i % (w / 2)) * 4) as u8, (64 + (i / (w / 2)) * 7) as u8]).collect();
        let flat: Vec<u8> = [148u8, 108].repeat(((w / 2) * (h / 2)) as usize);
        for ((matrix, full_range, transfer), (uv, budget)) in cases.into_iter().flat_map(|c| {
            let ramp_budget = if c.2 == Transfer::Sdr { 1.5 } else { 2.5 };
            [(c, (ramp.clone(), ramp_budget)), (c, (flat.clone(), 0.75))]
        }) {
            let frame = YuvFrame {
                width: w,
                height: h,
                y: (0..w * h).map(|i| (16 + (i % w) * 219 / w) as u8).collect(),
                uv,
                matrix,
                full_range,
                transfer,
            };
            let pass = Gpu::new(&gpu.device);
            let (tex, rgba) = pass.clip_textures(&gpu.device, w, h);
            pass.convert(&gpu.device, &gpu.queue, &tex, &frame);
            let got = crate::scene::read_texture_rgba(&gpu.device, &gpu.queue, &rgba, w, h).expect("readback");
            let want = frame.to_rgba();
            let diff = got.iter().zip(&want).map(|(a, b)| (*a as f64 - *b as f64).abs()).sum::<f64>() / want.len() as f64;
            assert!(diff < budget, "{matrix:?} {transfer:?} full={full_range} budget {budget}: shader differs from the CPU reference by {diff:.2}");
            // Orientation: the top-left pixel is the top-left pixel.
            let d0 = (got[0] as i32 - want[0] as i32).abs();
            assert!(d0 <= 3, "top-left pixel is off by {d0}: flipped?");
        }
        let _ = &mut gpu.renderer;
    }

    /// **A jump forward goes to the frame; it does not play the way there.**
    /// Drives the decode thread directly (no GPU) and records every frame it
    /// publishes, cached or as a stand-in. A short jump past the run-ahead used
    /// to walk the stream with every frame in between handed out as the
    /// stand-in, and the preview drew each of them: scrubbing played footage.
    #[test]
    fn a_jump_forward_publishes_no_frame_in_between() {
        let path = std::env::temp_dir().join(format!("pbc_video_jump_{}.mp4", std::process::id()));
        let made = std::process::Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-f", "lavfi", "-i", "testsrc2=size=320x180:rate=30:duration=3"])
            .args(["-c:v", "libx264", "-pix_fmt", "yuv420p", "-g", "45", "-bf", "2"])
            .arg(&path)
            .status();
        if !matches!(made, Ok(s) if s.success()) {
            eprintln!("skipping: ffmpeg (with libx264) couldn't generate a fixture");
            return;
        }
        let shared = Arc::new(Shared {
            q: Mutex::new(Queue { frames: BTreeMap::new(), want: 0, approx: None, end: None, error: None, hardware: false, stop: false }),
            wake: Condvar::new(),
        });
        let thread = {
            let (s, p) = (shared.clone(), path.clone());
            std::thread::spawn(move || decode_loop(s, p, 30.0, 320 * 180 * 3 / 2, false))
        };
        // Everything the thread has made visible, polled until `until` holds.
        let watch = |until: &dyn Fn(&Queue) -> bool| {
            let mut seen = std::collections::BTreeSet::new();
            let t = Instant::now();
            loop {
                {
                    let q = shared.q.lock().unwrap();
                    seen.extend(q.frames.keys().copied());
                    seen.extend(q.approx.as_ref().map(|a| a.index));
                    assert!(q.error.is_none(), "{:?}", q.error);
                    if until(&q) {
                        return seen;
                    }
                }
                assert!(t.elapsed() < std::time::Duration::from_secs(10), "the decoder never got there");
                std::thread::sleep(std::time::Duration::from_micros(200));
            }
        };
        let jump = |to: i64| {
            shared.q.lock().unwrap().want = to;
            shared.wake.notify_all();
        };

        // Settle at 0 with the run-ahead full (24 frames, then the decoder sleeps).
        watch(&|q| q.frames.contains_key(&MAX_AHEAD));
        std::thread::sleep(std::time::Duration::from_millis(50));
        // A short way past the run-ahead, inside the GOP: within the old walk.
        jump(35);
        let seen = watch(&|q| q.frames.contains_key(&35));
        let between: Vec<_> = seen.range(MAX_AHEAD + 1..35).collect();
        assert!(between.is_empty(), "frames between the old run-ahead and the target were published: {between:?}");

        // Across a GOP: seeks, and the only stand-in is the keyframe, held
        // back until the seek has been slow.
        jump(80);
        let seen = watch(&|q| q.frames.contains_key(&80));
        let between: Vec<_> = seen.range(35 + MAX_AHEAD + 1..80).filter(|f| **f != 45).collect();
        assert!(between.is_empty(), "only the keyframe may stand in: {between:?}");

        shared.q.lock().unwrap().stop = true;
        shared.wake.notify_all();
        thread.join().unwrap();
        let _ = std::fs::remove_file(&path);
    }

    /// **The Phase 0 gate, as a measurement** — not an assertion, because the
    /// answer belongs to a machine and goes in `docs/performance.md`.
    ///
    /// Plays the clips in `PBC_BENCH_DIR` overlapped for fifteen seconds
    /// through the editor's own cache, conversion pass and vello renderer, on
    /// a wall clock at 30fps with a redraw every 1/60s, then scrubs. Release
    /// build or it is not a measurement:
    ///
    /// `cargo test --release -p motion-live --features libav -- --ignored playback_bench --nocapture`
    #[test]
    #[ignore]
    fn playback_bench() {
        use crate::footage::FootageCache;
        use kurbo::Affine;
        use std::time::Duration;
        let dir = std::path::PathBuf::from(std::env::var("PBC_BENCH_DIR").expect("set PBC_BENCH_DIR"));
        let names = std::env::var("PBC_BENCH_CLIPS").unwrap_or("h264_1.mp4,h264_2.mp4,h264_3.mp4,hevc_1.mp4".into());
        let mut gpu = Headless::new().expect("the bench needs a GPU");
        let registry = motion_render::default_registry();
        let assets: Vec<Asset> = names
            .split(',')
            .enumerate()
            .map(|(i, n)| {
                let p = dir.join(n);
                registry.open(&p).expect("probe").into_asset(AssetId(i as u64 + 1), p)
            })
            .collect();
        let mut cache = FootageCache::new(motion_render::default_registry());
        cache.realtime = true;

        let (w, h) = (1920u32, 1080u32);
        let target = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("bench"),
            size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = target.create_view(&Default::default());

        // One redraw, the way `App::render` does it: look every clip up, build
        // the scene, convert, render. Returns how long it took with the GPU
        // drained, so queued work can't hide.
        let redraw = |cache: &mut FootageCache, gpu: &mut Headless, frame: i64| -> f64 {
            let t = Instant::now();
            let mut vs = vello::Scene::new();
            for (i, a) in assets.iter().enumerate() {
                let paint = ImagePaint { asset: a.id, source_frame: a.clamp_frame(frame as f64) };
                if let Some((image, _)) = cache.image(a, paint) {
                    // Each 4K clip scaled to most of a 1080p comp, offset and
                    // part transparent so all of them are really composited.
                    let place = Affine::translate((i as f64 * 110.0, i as f64 * 60.0)) * Affine::scale(0.42);
                    vs.draw_image(&vello::peniko::ImageBrush::new(image.clone()).with_alpha(0.85), place);
                }
            }
            cache.video.prepare(&gpu.device, &gpu.queue, &mut gpu.renderer);
            gpu.renderer
                .render_to_texture(
                    &gpu.device,
                    &gpu.queue,
                    &vs,
                    &view,
                    &vello::RenderParams {
                        base_color: vello::peniko::Color::BLACK,
                        width: w,
                        height: h,
                        antialiasing_method: vello::AaConfig::Area,
                    },
                )
                .expect("render");
            let _ = gpu.device.poll(wgpu::PollType::wait_indefinitely());
            t.elapsed().as_secs_f64() * 1e3
        };
        // Redraw until every clip shows exactly `frame`. Returns how long until
        // every clip showed *something* for the new position, and how long
        // until every clip showed the exact frame.
        let settle = |cache: &mut FootageCache, gpu: &mut Headless, frame: i64| -> (f64, f64) {
            let t = Instant::now();
            let mut first = None;
            loop {
                redraw(cache, gpu, frame);
                if first.is_none() && cache.video.fresh() {
                    first = Some(t.elapsed().as_secs_f64() * 1e3);
                }
                if cache.video.settled() {
                    let exact = t.elapsed().as_secs_f64() * 1e3;
                    return (first.unwrap_or(exact), exact);
                }
                assert!(t.elapsed() < Duration::from_secs(20), "frame {frame} never arrived");
                std::thread::sleep(Duration::from_millis(1));
            }
        };

        let (_, cold) = settle(&mut cache, &mut gpu, 0);
        std::thread::sleep(Duration::from_millis(500));
        cache.video.stats = Default::default();

        // Play.
        let (secs, fps, tick) = (15.0, 30.0, 1.0 / 60.0);
        let mut times = Vec::new();
        let start = Instant::now();
        let mut n = 0u32;
        loop {
            let t = start.elapsed().as_secs_f64();
            if t >= secs {
                break;
            }
            times.push(redraw(&mut cache, &mut gpu, (t * fps) as i64));
            n += 1;
            if let Ok(wait) = Duration::try_from_secs_f64(n as f64 * tick - start.elapsed().as_secs_f64()) {
                std::thread::sleep(wait);
            }
        }
        let play = cache.video.stats;
        let (hw, open) = cache.video.hardware_clips();
        times.sort_by(f64::total_cmp);
        let pct = |p: f64| times[((times.len() - 1) as f64 * p) as usize];

        // Scrub: twenty jumps to frames nowhere near each other.
        let (mut firsts, mut jumps) = (Vec::new(), Vec::new());
        let mut x = 12345u64;
        for _ in 0..20 {
            x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let (first, exact) = settle(&mut cache, &mut gpu, ((x >> 33) % 580) as i64);
            firsts.push(first);
            jumps.push(exact);
        }
        firsts.sort_by(f64::total_cmp);
        jumps.sort_by(f64::total_cmp);

        // Step scrubs: a jump of 36-40 frames at a time with a pause between,
        // like pressing a key or clicking a little way along the ruler. Past
        // the run-ahead, inside or across a GOP. Counts the frames put on
        // screen that were not the one asked for.
        let mut steps = |from: i64, by: i64| {
            settle(&mut cache, &mut gpu, from);
            std::thread::sleep(Duration::from_millis(300));
            cache.video.stats = Default::default();
            let mut exact = Vec::new();
            for k in 1..=15 {
                exact.push(settle(&mut cache, &mut gpu, from + by * k).1);
                std::thread::sleep(Duration::from_millis(300));
            }
            exact.sort_by(f64::total_cmp);
            (exact[7], exact[14], cache.video.stats.standins)
        };
        let fwd = steps(0, 40);
        let back = steps(560, -36);

        println!("clips: {names}  ({hw}/{open} on the hardware decoder)");
        println!("cold start to first frame of all clips: {cold:.0} ms");
        println!(
            "play {secs}s @ {fps}fps: {} clip-frames wanted, {} dropped ({:.2}%), worst request-to-screen {:.0} ms",
            play.wanted,
            play.dropped,
            100.0 * play.dropped as f64 / play.wanted.max(1) as f64,
            play.worst_ms
        );
        println!("redraw: {} redraws, median {:.1} ms, p99 {:.1} ms, max {:.1} ms", times.len(), pct(0.5), pct(0.99), pct(1.0));
        println!("scrub (all clips showing a frame): median {:.0} ms, p90 {:.0} ms, max {:.0} ms", firsts[10], firsts[18], firsts[19]);
        println!("scrub (all clips exact): median {:.0} ms, p90 {:.0} ms, max {:.0} ms", jumps[10], jumps[18], jumps[19]);
        for (name, (median, max, standins)) in [("forward 40", fwd), ("back 36", back)] {
            println!("step {name} x15 (all clips exact): median {median:.0} ms, max {max:.0} ms, {standins} stand-in frames shown");
        }
    }
}
