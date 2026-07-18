//! pbc — the live GPU shell.
//!
//! Opens a window and drives the engine in real time: every frame it reads the
//! wall clock, computes a loop time `t`, calls `motion_core::evaluate(doc, t)`,
//! converts the resulting `Scene` into a `vello::Scene`, and rasterizes it on
//! the GPU. This is the real-time proof of the pull-based model — scrubbing is
//! just "evaluate at a different `t`", now at 60fps.
//!
//! Controls:  Space = play/pause   Left/Right = step   R = restart   Esc = quit
//!
//! The engine (`motion-core`) has no idea this exists. Swapping the offline SVG
//! backend for this GPU one required zero changes to the document model.

use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::Instant;

use kurbo::{Affine, Stroke as KurboStroke};
use motion_core::{demo::demo_document, evaluate, Color as MColor, Document, Scene as MScene};
use vello::peniko::{Color, Fill};
use vello::util::{RenderContext, RenderSurface};
use vello::{AaConfig, AaSupport, Renderer, RendererOptions, Scene as VScene};
use winit::application::ApplicationHandler;
use winit::event::{ElementState, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, NamedKey};
use winit::window::{Window, WindowId};

/// Convert one core `Color` into a vello/peniko color, folding in an opacity.
fn to_peniko(c: MColor, opacity: f64) -> Color {
    Color::new([
        c.r as f32,
        c.g as f32,
        c.b as f32,
        (c.a * opacity) as f32,
    ])
}

/// Convert an evaluated engine `Scene` into a `vello::Scene`, prepending a
/// global transform that fits the composition into the window.
fn to_vello(scene: &MScene, fit: Affine) -> VScene {
    let mut vs = VScene::new();
    for item in &scene.items {
        let xf = fit * item.transform;
        if let Some(fill) = item.fill {
            vs.fill(
                Fill::NonZero,
                xf,
                to_peniko(fill, item.opacity),
                None,
                &item.path,
            );
        }
        if let Some((color, width)) = item.stroke {
            vs.stroke(
                &KurboStroke::new(width),
                xf,
                to_peniko(color, item.opacity),
                None,
                &item.path,
            );
        }
    }
    vs
}

/// "Contain" fit: scale the doc uniformly to fit the window and center it.
fn fit_transform(doc: &Document, win_w: f64, win_h: f64) -> Affine {
    let scale = (win_w / doc.width).min(win_h / doc.height);
    let dx = (win_w - doc.width * scale) * 0.5;
    let dy = (win_h - doc.height * scale) * 0.5;
    Affine::translate((dx, dy)) * Affine::scale(scale)
}

enum RenderState {
    Active {
        surface: RenderSurface<'static>,
        window: Arc<Window>,
    },
    Suspended(Option<Arc<Window>>),
}

struct App {
    context: RenderContext,
    /// One renderer per wgpu device, indexed by `RenderSurface::dev_id`.
    renderers: Vec<Option<Renderer>>,
    state: RenderState,
    vscene: VScene,
    doc: Document,

    // Playback clock.
    playing: bool,
    /// Wall-clock anchor: `now - anchor` maps to document time when playing.
    anchor: Instant,
    /// Frozen document time when paused / stepping.
    paused_t: f64,
}

impl App {
    fn new(doc: Document) -> Self {
        Self {
            context: RenderContext::new(),
            renderers: Vec::new(),
            state: RenderState::Suspended(None),
            vscene: VScene::new(),
            doc,
            playing: true,
            anchor: Instant::now(),
            paused_t: 0.0,
        }
    }

    /// Current looped document time in seconds.
    fn current_time(&self) -> f64 {
        let raw = if self.playing {
            self.anchor.elapsed().as_secs_f64()
        } else {
            self.paused_t
        };
        if self.doc.duration > 0.0 {
            raw.rem_euclid(self.doc.duration)
        } else {
            raw
        }
    }

    fn seek(&mut self, t: f64) {
        let t = t.rem_euclid(self.doc.duration.max(f64::MIN_POSITIVE));
        self.paused_t = t;
        // Re-anchor so that resuming continues from `t`.
        self.anchor = Instant::now() - std::time::Duration::from_secs_f64(t);
    }

    fn toggle_play(&mut self) {
        if self.playing {
            self.paused_t = self.current_time();
            self.playing = false;
        } else {
            self.anchor = Instant::now()
                - std::time::Duration::from_secs_f64(self.paused_t);
            self.playing = true;
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        let RenderState::Suspended(cached) = &mut self.state else {
            return;
        };
        let window = cached.take().unwrap_or_else(|| {
            let attrs = Window::default_attributes()
                .with_title("Pain By Choice")
                .with_inner_size(winit::dpi::LogicalSize::new(1280.0, 720.0));
            Arc::new(event_loop.create_window(attrs).unwrap())
        });

        let size = window.inner_size();
        let surface = pollster::block_on(self.context.create_surface(
            window.clone(),
            size.width.max(1),
            size.height.max(1),
            vello::wgpu::PresentMode::AutoVsync,
        ))
        .expect("create surface");

        // Ensure a renderer exists for this surface's device.
        while self.renderers.len() <= surface.dev_id {
            self.renderers.push(None);
        }
        if self.renderers[surface.dev_id].is_none() {
            let device = &self.context.devices[surface.dev_id].device;
            self.renderers[surface.dev_id] = Some(
                Renderer::new(
                    device,
                    RendererOptions {
                        use_cpu: false,
                        antialiasing_support: AaSupport::area_only(),
                        num_init_threads: NonZeroUsize::new(1),
                        pipeline_cache: None,
                    },
                )
                .expect("create renderer"),
            );
        }

        self.state = RenderState::Active { surface, window };
    }

    fn suspended(&mut self, _event_loop: &ActiveEventLoop) {
        if let RenderState::Active { window, .. } = &self.state {
            self.state = RenderState::Suspended(Some(window.clone()));
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _id: WindowId,
        event: WindowEvent,
    ) {
        // Grab a cheap handle to the window without holding a borrow of
        // `self.state`, so event handlers are free to call `self` methods.
        let window = match &self.state {
            RenderState::Active { window, .. } => window.clone(),
            RenderState::Suspended(_) => return,
        };

        match event {
            WindowEvent::CloseRequested => event_loop.exit(),

            WindowEvent::Resized(size) => {
                if let RenderState::Active { surface, .. } = &mut self.state {
                    self.context
                        .resize_surface(surface, size.width.max(1), size.height.max(1));
                }
                window.request_redraw();
            }

            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                let step = 1.0 / self.doc.fps.max(1.0);
                match event.logical_key {
                    Key::Named(NamedKey::Space) => self.toggle_play(),
                    Key::Named(NamedKey::Escape) => event_loop.exit(),
                    Key::Named(NamedKey::ArrowRight) => {
                        self.playing = false;
                        let t = self.current_time() + step;
                        self.seek(t);
                    }
                    Key::Named(NamedKey::ArrowLeft) => {
                        self.playing = false;
                        let t = self.current_time() - step;
                        self.seek(t);
                    }
                    Key::Character(ref s) if s == "r" || s == "R" => self.seek(0.0),
                    _ => {}
                }
                window.request_redraw();
            }

            WindowEvent::RedrawRequested => {
                self.render(&window);
                if self.playing {
                    window.request_redraw();
                }
            }
            _ => {}
        }
    }
}

impl App {
    /// Evaluate the document at the current time and rasterize it. Kept as its
    /// own method so the disjoint field borrows (`state` / `context` /
    /// `renderers` / `vscene`) don't collide with the `self`-method calls in
    /// the event handlers.
    fn render(&mut self, window: &Window) {
        let t = self.current_time();
        let scene = evaluate(&self.doc, t);
        for (id, msg) in &scene.warnings {
            eprintln!("warning [node {}]: {msg}", id.0);
        }

        let size = window.inner_size();
        let fit = fit_transform(&self.doc, size.width as f64, size.height as f64);
        self.vscene = to_vello(&scene, fit);

        let RenderState::Active { surface, .. } = &mut self.state else {
            return;
        };

        use vello::wgpu::CurrentSurfaceTexture as Cst;
        let surface_texture = match surface.surface.get_current_texture() {
            Cst::Success(t) | Cst::Suboptimal(t) => t,
            _ => {
                // Timeout / occluded / outdated / lost — skip and retry.
                window.request_redraw();
                return;
            }
        };

        let device_handle = &self.context.devices[surface.dev_id];
        let renderer = self.renderers[surface.dev_id].as_mut().unwrap();
        renderer
            .render_to_texture(
                &device_handle.device,
                &device_handle.queue,
                &self.vscene,
                &surface.target_view,
                &vello::RenderParams {
                    base_color: Color::new([0.08, 0.09, 0.11, 1.0]),
                    width: surface.config.width,
                    height: surface.config.height,
                    antialiasing_method: AaConfig::Area,
                },
            )
            .expect("render");

        // Blit vello's target texture onto the swapchain surface.
        let mut encoder = device_handle.device.create_command_encoder(
            &vello::wgpu::CommandEncoderDescriptor { label: Some("blit") },
        );
        surface.blitter.copy(
            &device_handle.device,
            &mut encoder,
            &surface.target_view,
            &surface_texture
                .texture
                .create_view(&vello::wgpu::TextureViewDescriptor::default()),
        );
        device_handle.queue.submit([encoder.finish()]);
        surface_texture.present();
    }
}

fn main() {
    let event_loop = EventLoop::new().unwrap();
    event_loop.set_control_flow(ControlFlow::Wait);
    let mut app = App::new(demo_document());
    println!("Pain By Choice — live. Space=play/pause  ←/→=step  R=restart  Esc=quit");
    event_loop.run_app(&mut app).unwrap();
}
