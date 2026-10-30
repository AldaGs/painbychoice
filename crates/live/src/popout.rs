//! Floating windows: content panels popped out of the main window, so they can
//! sit on another monitor.
//!
//! A floating window is deliberately thin. It owns its own OS window, wgpu
//! surface, egui context and egui renderer — egui keeps one context per
//! window — and a [`Dock`] of content editors. It owns **no editor logic**: the
//! main window's frame runs every window's UI pass with the same leaf-drawing
//! closure, so a panel in a floating window reads the same snapshot and reports
//! into the same edit structs as one docked in the main window. There is still
//! exactly one update and one apply phase per frame.
//!
//! The canvas, comp bar and transport never float (see [`Dock::is_floatable`]):
//! the canvas is the single vello target, and the bars are the main window's
//! fixed chrome. Phase 1 of multi-monitor support stops there; a second canvas
//! would need a second vello target.

use crate::*;

/// A floating window as saved in the `.pbc` layout and as queued for opening.
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct FloatSpec {
    pub(crate) dock: Dock,
    /// Outer position in physical pixels on the virtual desktop — which is
    /// what places the window on the right monitor. `None` lets the OS pick
    /// (and is all Wayland ever allows).
    #[serde(default)]
    pub(crate) pos: Option<(i32, i32)>,
    /// Inner size in logical points.
    pub(crate) size: (f32, f32),
}

pub(crate) struct Popout {
    pub(crate) window: Arc<Window>,
    pub(crate) surface: RenderSurface<'static>,
    pub(crate) ctx: egui::Context,
    pub(crate) state: egui_winit::State,
    pub(crate) renderer: egui_wgpu::Renderer,
    pub(crate) dock: Dock,
}

impl Popout {
    /// Open the window `spec` describes. If the OS or GPU refuses, the spec
    /// comes back so the caller can dock its panels back rather than lose them.
    pub(crate) fn open(
        event_loop: &ActiveEventLoop,
        context: &mut RenderContext,
        spec: FloatSpec,
    ) -> Result<Popout, FloatSpec> {
        let title = format!("Pain By Choice — {}", spec.dock.title());
        let mut attrs = Window::default_attributes()
            .with_title(title)
            .with_inner_size(winit::dpi::LogicalSize::new(
                spec.size.0.max(160.0) as f64,
                spec.size.1.max(120.0) as f64,
            ))
            .with_window_icon(window_icon());
        if let Some((x, y)) = spec.pos {
            attrs = attrs.with_position(winit::dpi::PhysicalPosition::new(x, y));
        }
        let Ok(window) = event_loop.create_window(attrs) else {
            return Err(spec);
        };
        let window = Arc::new(window);
        let size = window.inner_size();
        let Ok(surface) = pollster::block_on(context.create_surface(
            window.clone(),
            size.width.max(1),
            size.height.max(1),
            wgpu::PresentMode::AutoVsync,
        )) else {
            return Err(spec);
        };
        // Same fonts and theme as the main window; egui keeps them per context.
        let ctx = egui::Context::default();
        icon::install(&ctx);
        theme::install(&ctx);
        let state = egui_winit::State::new(
            ctx.clone(),
            egui::ViewportId::ROOT,
            &window,
            Some(window.scale_factor() as f32),
            Some(winit::window::Theme::Dark),
            None,
        );
        let renderer = egui_wgpu::Renderer::new(
            &context.devices[surface.dev_id].device,
            surface.format,
            egui_wgpu::RendererOptions::default(),
        );
        Ok(Popout { window, surface, ctx, state, renderer, dock: spec.dock })
    }

    /// Where the window is now, for saving.
    pub(crate) fn spec(&self) -> FloatSpec {
        let size = self.window.inner_size().to_logical::<f32>(self.window.scale_factor());
        FloatSpec {
            dock: self.dock.clone(),
            pos: self.window.outer_position().ok().map(|p| (p.x, p.y)),
            size: (size.width, size.height),
        }
    }

    /// Present one finished egui pass. No vello here: a floating window holds
    /// only egui panels, which paint their own backgrounds over the clear.
    pub(crate) fn paint(&mut self, context: &RenderContext, out: egui::FullOutput) {
        self.state.handle_platform_output(&self.window, out.platform_output);
        let ppp = self.ctx.pixels_per_point();
        let jobs = self.ctx.tessellate(out.shapes, ppp);

        use wgpu::CurrentSurfaceTexture as Cst;
        let texture = match self.surface.surface.get_current_texture() {
            Cst::Success(tx) | Cst::Suboptimal(tx) => tx,
            _ => {
                self.window.request_redraw();
                return;
            }
        };
        let dev = &context.devices[self.surface.dev_id];
        for (id, delta) in &out.textures_delta.set {
            self.renderer.update_texture(&dev.device, &dev.queue, *id, delta);
        }
        let screen = egui_wgpu::ScreenDescriptor {
            size_in_pixels: [self.surface.config.width, self.surface.config.height],
            pixels_per_point: ppp,
        };
        let mut encoder = dev
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("popout") });
        let user_buffers =
            self.renderer.update_buffers(&dev.device, &dev.queue, &mut encoder, &jobs, &screen);
        let view = texture.texture.create_view(&wgpu::TextureViewDescriptor::default());
        {
            let mut rpass = encoder
                .begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("popout egui"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &view,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                            store: wgpu::StoreOp::Store,
                        },
                        depth_slice: None,
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                })
                .forget_lifetime();
            self.renderer.render(&mut rpass, &jobs, &screen);
        }
        for id in &out.textures_delta.free {
            self.renderer.free_texture(id);
        }
        dev.queue.submit(user_buffers.into_iter().chain([encoder.finish()]));
        texture.present();
    }
}

impl Dock {
    /// A window title for a floating tree: its editors, in walk order.
    pub(crate) fn title(&self) -> String {
        fn walk(d: &Dock, out: &mut Vec<&'static str>) {
            match d {
                Dock::Leaf(e) => {
                    if !out.contains(&e.label()) {
                        out.push(e.label());
                    }
                }
                Dock::Split { first, second, .. } => {
                    walk(first, out);
                    walk(second, out);
                }
            }
        }
        let mut names = Vec::new();
        walk(self, &mut names);
        names.join(" · ")
    }
}
