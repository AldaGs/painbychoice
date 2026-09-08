//! An offscreen vello render target: a composition frame, at full resolution,
//! as RGBA8 bytes — rendered by the **same rasterizer that draws the preview**.
//!
//! This is the piece that makes preview-equals-export provable rather than
//! argued. The CPU rasterizer in `render/src/raster.rs` exists so a frame can be
//! rendered and asserted on with no GPU at all, and
//! [`decisions/0017`](../../../docs/decisions/0017-offline-cpu-rasterizer.md)
//! is explicit that its parity with the preview is *structural* — same layers,
//! same order, same blend modes and mattes — and not per-pixel, because two
//! independent rasterizers do not agree on antialiasing. That is the right
//! trade for the headless CLI. It is the wrong one for the editor's own export
//! button: a user who renders from the GUI is entitled to the frame they were
//! just looking at, down to the pixel.
//!
//! So the editor renders through vello, into a texture instead of a surface,
//! and reads the pixels back. There is no second rasterizer in this path and
//! therefore no parity question — the export *is* the preview, minus the
//! editor's furniture (see [`Chrome::none`]).
//!
//! Three things this deliberately does not do:
//!
//! - **No letterbox.** The preview fits the comp into a window and paints a
//!   backdrop around it; a render is exactly the comp, so `fit` is a plain
//!   scale and the target is the comp's own size.
//! - **No chrome.** No frame border, no passepartout, no selection outline, no
//!   onion skins. [`Chrome::none`] is the whole of it.
//! - **No device of its own.** It borrows the editor's `wgpu` device and vello
//!   `Renderer`. Standing up a second device to render a frame would double the
//!   GPU memory for the atlas and the shaders, and would render with a
//!   different adapter on a laptop that has two.
//!
//! What it *does* do, and must: run the preview's own effect-stack readback, so
//! a layer with a blur exports blurred. See [`FrameRenderer::frame`].

use crate::scene::{read_texture_rgba, Chrome};
use kurbo::Affine;
use motion_core::node::CompId;
use motion_core::{evaluate_comp, Project as MProject};
use vello::wgpu;
use vello::Scene as VScene;

use crate::footage::FootageCache;
use crate::scene::to_vello;

/// A reusable offscreen colour target sized to one composition.
///
/// Held for the life of a render job rather than rebuilt per frame: allocating
/// a 1920x1080 texture three hundred times is three hundred GPU allocations for
/// a buffer whose size never changes.
pub(crate) struct OffscreenTarget {
    width: u32,
    height: u32,
    texture: wgpu::Texture,
    view: wgpu::TextureView,
}

impl OffscreenTarget {
    /// Allocate a target `width` x `height`.
    ///
    /// `STORAGE_BINDING` because vello renders by compute shader and writes the
    /// image as a storage texture — without it `render_to_texture` fails at
    /// bind-group creation, not at draw. `COPY_SRC` so the pixels can be read
    /// back. The pair is the same one `rasterize_effect_layers` uses for the
    /// blur readback, for the same two reasons.
    pub(crate) fn new(device: &wgpu::Device, width: u32, height: u32) -> Option<Self> {
        if width == 0 || height == 0 {
            return None;
        }
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("export-target"),
            size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        Some(OffscreenTarget { width, height, texture, view })
    }

    pub(crate) fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Render an assembled vello scene into this target and read it back as
    /// tight RGBA8, `width * height * 4` bytes, top row first.
    ///
    /// The base colour is **transparent**, not the preview's backdrop: the comp
    /// background is drawn by [`to_vello`] as the comp's own `bg`, and anything
    /// painted underneath it would show through wherever that background is
    /// itself transparent — which is exactly the case a PNG sequence exists to
    /// preserve.
    pub(crate) fn render(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        renderer: &mut vello::Renderer,
        scene: &VScene,
    ) -> Result<Vec<u8>, String> {
        renderer
            .render_to_texture(
                device,
                queue,
                scene,
                &self.view,
                &vello::RenderParams {
                    base_color: vello::peniko::Color::TRANSPARENT,
                    width: self.width,
                    height: self.height,
                    antialiasing_method: vello::AaConfig::Area,
                },
            )
            .map_err(|e| format!("vello render failed: {e:?}"))?;
        read_texture_rgba(device, queue, &self.texture, self.width, self.height)
            .ok_or_else(|| "reading the rendered frame back from the GPU failed".to_string())
    }
}

/// The scale and pixel size an export uses for one comp.
///
/// Separated from the render so the arithmetic is testable without a GPU — the
/// rounding here decides the encoder's frame size, and an encoder is entitled to
/// refuse a frame that is one pixel off (`encode.rs` does exactly that).
///
/// Dimensions are rounded rather than truncated, and floored at 1: a comp scaled
/// to 0.25 must not become a zero-width texture, which is an allocation failure
/// rather than a small picture.
pub(crate) fn export_size(comp: (f64, f64), scale: f64) -> (u32, u32) {
    let s = if scale.is_finite() && scale > 0.0 { scale } else { 1.0 };
    let w = ((comp.0 * s).round() as i64).clamp(1, u32::MAX as i64) as u32;
    let h = ((comp.1 * s).round() as i64).clamp(1, u32::MAX as i64) as u32;
    (w, h)
}

/// Everything an offscreen render needs that is not the frame number.
///
/// A struct because the render loop calls this once per frame and every field is
/// constant across the job; threading eight arguments through a hot loop is how
/// one of them ends up recomputed per frame by accident.
pub(crate) struct FrameRenderer<'a> {
    pub(crate) device: &'a wgpu::Device,
    pub(crate) queue: &'a wgpu::Queue,
    pub(crate) renderer: &'a mut vello::Renderer,
    pub(crate) target: &'a OffscreenTarget,
    pub(crate) footage: &'a mut FootageCache,
}

impl FrameRenderer<'_> {
    /// Render one frame of `comp` from `project` and return it as RGBA8.
    ///
    /// The frame number is a `f64` because that is what `evaluate_comp` takes —
    /// the timebase is frames-native ([`decisions/0003`]) and sub-frame values
    /// are meaningful to motion blur later.
    ///
    /// Note what is *absent*: no orbit matrix. The preview's 3D navigation is a
    /// view *of* the comp, not part of it — a render is always through the
    /// comp's own camera, so an orbited preview and its export differ, and
    /// correctly so.
    ///
    /// The effect-stack readback **is** here, and has to be: a layer with a blur
    /// needs its pixels rendered, read back, filtered and drawn in place of its
    /// items, because vello has no layer-filter primitive. Running the same
    /// `rasterize_effect_layers` the preview runs is the whole of what makes the
    /// export match — skipping it would silently drop every blur from a
    /// delivered file, which is the class of bug nobody notices until a client
    /// does. It costs one extra render + readback per blurred layer per frame.
    pub(crate) fn frame(
        &mut self,
        project: &MProject,
        comp: CompId,
        frame: f64,
    ) -> Result<Vec<u8>, String> {
        let doc = project
            .comp(comp)
            .ok_or_else(|| format!("no composition {} in this project", comp.0))?;
        let dims = (doc.width, doc.height);
        let bg = doc.bg;
        let (w, h) = self.target.size();
        // The same scale the target was sized with, recovered from it, so the
        // picture cannot land at a different scale than the texture it is
        // rendered into.
        let fit = Affine::scale(w as f64 / dims.0.max(1e-6));

        let scene = evaluate_comp(project, comp, frame);
        // Blurred layers rasterized and filtered first, so `to_vello` can drop
        // each one's processed image in place of its raw items — the same order
        // the preview uses. A layer whose readback fails is simply absent from
        // the map and draws unfiltered, so the worst case is a missing blur
        // rather than a failed render.
        let effect_images = crate::scene::rasterize_effect_layers(
            &scene,
            fit,
            w,
            h,
            self.device,
            self.queue,
            self.renderer,
            self.footage,
            &project.assets,
        );
        let vs = to_vello(
            &scene,
            fit,
            dims,
            bg,
            &Chrome::none(),
            self.footage,
            &project.assets,
            &effect_images,
        );
        self.target.render(self.device, self.queue, self.renderer, &vs)
    }
}

/// A `wgpu` device and a vello `Renderer` with no window attached.
///
/// Only used by the tests below, and it is the reason they are worth having:
/// every runtime assumption in this module — that vello will render into a
/// `STORAGE_BINDING | COPY_SRC` texture, that the readback lands right way up,
/// that the channels come back RGBA — is invisible to a compile and untestable
/// against a surface that needs a window. A headless device makes the whole
/// path assertable in `cargo test`.
#[cfg(test)]
pub(crate) struct Headless {
    pub(crate) device: wgpu::Device,
    pub(crate) queue: wgpu::Queue,
    pub(crate) renderer: vello::Renderer,
}

#[cfg(test)]
impl Headless {
    /// Returns `None` when the machine has no usable adapter — CI without a
    /// GPU, a software stack vello refuses. The tests skip rather than fail:
    /// a missing GPU is not a broken render path, and a test that fails on
    /// every headless CI box is a test people learn to ignore.
    pub(crate) fn new() -> Option<Self> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            force_fallback_adapter: false,
            compatible_surface: None,
        }))
        .ok()?;
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("headless-test"),
            required_features: wgpu::Features::empty(),
            required_limits: adapter.limits(),
            memory_hints: wgpu::MemoryHints::default(),
            experimental_features: wgpu::ExperimentalFeatures::disabled(),
            trace: wgpu::Trace::Off,
        }))
        .ok()?;
        let renderer = vello::Renderer::new(
            &device,
            vello::RendererOptions {
                use_cpu: false,
                antialiasing_support: vello::AaSupport::area_only(),
                num_init_threads: std::num::NonZeroUsize::new(1),
                pipeline_cache: None,
            },
        )
        .ok()?;
        Some(Headless { device, queue, renderer })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The size arithmetic is what the encoder checks its frames against, so a
    /// rounding disagreement here is a hard error at encode time rather than a
    /// slightly wrong picture.
    #[test]
    fn a_full_scale_export_is_the_comp_size() {
        assert_eq!(export_size((1920.0, 1080.0), 1.0), (1920, 1080));
    }

    #[test]
    fn a_half_scale_export_halves_both_axes() {
        assert_eq!(export_size((1920.0, 1080.0), 0.5), (960, 540));
    }

    /// 1080 * 0.33 is 356.4 — truncation would give 356 and rounding gives 356,
    /// but 1920 * 0.33 is 633.6, where the two disagree. Rounding is the rule.
    #[test]
    fn a_fractional_scale_rounds_rather_than_truncates() {
        assert_eq!(export_size((1920.0, 1080.0), 0.33), (634, 356));
    }

    /// A scale small enough to round a dimension to zero must still produce a
    /// texture: zero is an allocation failure, not a very small picture.
    #[test]
    fn a_tiny_scale_still_leaves_one_pixel() {
        assert_eq!(export_size((16.0, 9.0), 0.001), (1, 1));
    }

    /// Garbage in the scale field falls back to full size rather than
    /// propagating a NaN into a texture descriptor.
    #[test]
    fn a_nonsense_scale_falls_back_to_full_size() {
        assert_eq!(export_size((100.0, 50.0), f64::NAN), (100, 50));
        assert_eq!(export_size((100.0, 50.0), 0.0), (100, 50));
        assert_eq!(export_size((100.0, 50.0), -2.0), (100, 50));
    }

    use motion_core::{Color as MColor, Comp, Node as MNode, Project as MProject, Shape, Value};

    /// A comp with one red square, off-centre and *not* vertically symmetric,
    /// on a white ground.
    ///
    /// The asymmetry is the point: a vertically flipped readback of a centred
    /// shape is indistinguishable from a correct one, which is exactly the trap
    /// a naive orientation check falls into.
    fn one_square_project() -> MProject {
        let mut square = MNode::group(1, "square");
        square.shape = Some(Shape::Rect {
            size: Value::constant(kurbo::Vec2::new(20.0, 20.0)),
            radius: Value::constant(0.0),
        });
        square.fill = Some(Value::constant(MColor::rgb(1.0, 0.0, 0.0)));
        // Near the top-left of a 100x100 comp.
        square.transform.position = Value::constant(motion_core::Vec3::new(25.0, 15.0, 0.0));
        let mut comp = Comp::new(100.0, 100.0, MNode::group(0, "root").with_child(square));
        comp.bg = MColor::rgb(1.0, 1.0, 1.0);
        comp.duration_frames = 1;
        MProject::single(comp)
    }

    /// Read one pixel as `(r, g, b, a)`.
    fn px(rgba: &[u8], w: u32, x: u32, y: u32) -> (u8, u8, u8, u8) {
        let i = ((y * w + x) * 4) as usize;
        (rgba[i], rgba[i + 1], rgba[i + 2], rgba[i + 3])
    }

    /// The whole offscreen path on a real GPU: allocate, render, read back.
    ///
    /// Asserts the three things that are invisible until this actually runs —
    /// the shape lands where the document puts it (**not** flipped), the
    /// channels come back **RGBA and not BGRA**, and the buffer is exactly the
    /// size the encoder will be handed.
    #[test]
    fn an_offscreen_render_comes_back_upright_and_in_rgba() {
        let Some(mut gpu) = Headless::new() else {
            eprintln!("no usable GPU adapter; skipping the offscreen render test");
            return;
        };
        let project = one_square_project();
        let comp = project.root;
        let target = OffscreenTarget::new(&gpu.device, 100, 100).expect("allocate");
        let mut cache = crate::footage::FootageCache::new(motion_render::default_registry());
        let rgba = FrameRenderer {
            device: &gpu.device,
            queue: &gpu.queue,
            renderer: &mut gpu.renderer,
            target: &target,
            footage: &mut cache,
        }
        .frame(&project, comp, 0.0)
        .expect("render one frame");

        assert_eq!(rgba.len(), 100 * 100 * 4, "exactly what the encoder expects");

        // The square is centred on (25, 15) and is 20 wide, so this is inside
        // it — and the mirrored point (25, 85) is background. A vertical flip
        // swaps which of the two is red, so the pair pins orientation.
        let inside = px(&rgba, 100, 25, 15);
        let mirrored = px(&rgba, 100, 25, 85);
        assert!(
            inside.0 > 200 && inside.1 < 60 && inside.2 < 60,
            "the square must be red at its own position, not {inside:?} \
             (a blue reading here means BGRA, not RGBA)"
        );
        assert!(
            mirrored.0 > 200 && mirrored.1 > 200 && mirrored.2 > 200,
            "the mirrored point must be the white background, not {mirrored:?} \
             (a red reading here means the readback is upside down)"
        );
    }

    /// A 30x30 red square centred in a 100x100 white comp — spans 35..65 on
    /// both axes, so a window of 30..70 straddles every edge.
    ///
    /// Separate from [`one_square_project`], whose square is deliberately
    /// off-centre for the orientation test and therefore sits outside any
    /// window a blur test would sample.
    fn centred_square_project() -> MProject {
        let mut square = MNode::group(1, "square");
        square.shape = Some(Shape::Rect {
            size: Value::constant(kurbo::Vec2::new(30.0, 30.0)),
            radius: Value::constant(0.0),
        });
        square.fill = Some(Value::constant(MColor::rgb(1.0, 0.0, 0.0)));
        square.transform.position =
            Value::constant(motion_core::Vec3::new(50.0, 50.0, 0.0));
        let mut comp = Comp::new(100.0, 100.0, MNode::group(0, "root").with_child(square));
        comp.bg = MColor::rgb(1.0, 1.0, 1.0);
        comp.duration_frames = 1;
        MProject::single(comp)
    }

    /// **A blur reaches an exported frame.**
    ///
    /// The regression this exists for is silent: without the effect-stack
    /// readback the frame still renders, still has the right size, and still
    /// looks broadly correct — the blur is simply gone. Nothing errors, so only
    /// a pixel assertion catches it.
    ///
    /// The test is a hard edge with and without a blur. Unblurred, the boundary
    /// column is either fully inside or fully outside the square; blurred, it
    /// carries intermediate values. So it counts pixels in a vertical strip
    /// across the edge that are neither the square nor the background, and that
    /// count must go from ~zero to clearly positive.
    #[test]
    fn a_blurred_layer_exports_blurred() {
        let Some(mut gpu) = Headless::new() else {
            eprintln!("no usable GPU adapter; skipping the blur export test");
            return;
        };

        // Count pixels along the square's right edge that are partway between
        // the red fill and the white ground — the signature of a filtered edge.
        let intermediate = |rgba: &[u8]| -> usize {
            let mut n = 0;
            for y in 30..70 {
                for x in 30..70 {
                    let (r, g, b, _) = px(rgba, 100, x, y);
                    let solid_red = r > 200 && g < 60 && b < 60;
                    let solid_white = r > 200 && g > 200 && b > 200;
                    if !solid_red && !solid_white {
                        n += 1;
                    }
                }
            }
            n
        };

        let render = |gpu: &mut Headless, project: &MProject| {
            let target = OffscreenTarget::new(&gpu.device, 100, 100).expect("allocate");
            let mut cache =
                crate::footage::FootageCache::new(motion_render::default_registry());
            FrameRenderer {
                device: &gpu.device,
                queue: &gpu.queue,
                renderer: &mut gpu.renderer,
                target: &target,
                footage: &mut cache,
            }
            .frame(project, project.root, 0.0)
            .expect("render one frame")
        };

        let sharp = render(&mut gpu, &centred_square_project());

        let mut blurred_project = centred_square_project();
        {
            let comp = blurred_project.comps.get_mut(&blurred_project.root).unwrap();
            let square = comp.root.find_mut(motion_core::NodeId(1)).expect("the square");
            square.effects.push(motion_core::effect::Effect {
                enabled: true,
                kind: motion_core::effect::EffectKind::GaussianBlur {
                    radius: motion_core::Value::constant(6.0),
                },
            });
        }
        let blurred = render(&mut gpu, &blurred_project);

        let (a, b) = (intermediate(&sharp), intermediate(&blurred));
        assert!(
            b > a + 40,
            "a blurred layer must export blurred: {a} soft pixels sharp vs {b} blurred —              an unchanged count means the effect readback never ran"
        );
    }

    /// Chrome is editor furniture and must not reach a rendered frame.
    ///
    /// The frame border is the one that would actually ship: it is drawn
    /// unconditionally in the preview, it hugs the comp bounds, and a 1.5px grey
    /// rectangle around every delivered frame is the kind of bug that survives
    /// to a client. So the corner pixel is asserted to be the comp background.
    #[test]
    fn a_rendered_frame_carries_no_frame_border() {
        let Some(mut gpu) = Headless::new() else {
            eprintln!("no usable GPU adapter; skipping the chrome test");
            return;
        };
        let project = one_square_project();
        let comp = project.root;
        let target = OffscreenTarget::new(&gpu.device, 100, 100).expect("allocate");
        let mut cache = crate::footage::FootageCache::new(motion_render::default_registry());
        let rgba = FrameRenderer {
            device: &gpu.device,
            queue: &gpu.queue,
            renderer: &mut gpu.renderer,
            target: &target,
            footage: &mut cache,
        }
        .frame(&project, comp, 0.0)
        .expect("render one frame");

        for (x, y) in [(0, 0), (99, 0), (0, 99), (99, 99), (50, 0), (0, 50)] {
            let p = px(&rgba, 100, x, y);
            assert!(
                p.0 > 200 && p.1 > 200 && p.2 > 200,
                "({x},{y}) should be the white comp background, not {p:?} — \
                 editor chrome has leaked into the render"
            );
        }
    }
}
