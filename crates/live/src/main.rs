//! pbc — the live GPU shell with an egui overlay.
//!
//! Every frame: read the wall clock, compute a looped time `t`, call
//! `motion_core::evaluate(doc, t)`, rasterize the resulting `Scene` with vello,
//! then draw an egui transport bar (play/pause, restart, and a scrubbable
//! playhead) on top. Dragging the playhead just seeks — i.e. evaluates at a
//! different `t` — which is the whole non-linear model made interactive.
//!
//! Rendering order per frame:
//!   1. vello renders the scene into its offscreen target texture,
//!   2. we blit that target onto the swapchain surface,
//!   3. egui renders the UI on top (LoadOp::Load, so it composites over).
//!
//! The engine (`motion-core`) has no idea any of this exists.

use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::Instant;

use kurbo::{Affine, Point, Shape as _, Stroke as KurboStroke, Vec2};
use motion_core::{
    demo::demo_document, evaluate_comp, node::ParamValue, node::CompId, node::ModuleId, node::Module as MModule, Color as MColor,
    Comp, Document, EvalCtx, Expr, Project as MProject,
    ExprKind, ExprValue, Generator, Handle, Keyframe, Node as MNode, NodeId, PropPath,
    node::LayerTiming,
    Scene as MScene, Shape as MShape, Transform, Value, Waveform,
};
use serde::{Deserialize, Serialize};
use vello::peniko::{Color, Fill};
use vello::util::{RenderContext, RenderSurface};
use vello::wgpu;
use vello::{AaConfig, AaSupport, Renderer, RendererOptions, Scene as VScene};
use winit::application::ApplicationHandler;
use winit::event::{ElementState, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, NamedKey};
use winit::window::{Window, WindowId};

// The editor split by concern. Each module is a plain move out of this file;
// they glob-import `crate::*` (which re-exports these `use`s and every sibling
// below) so no module needs its own import bookkeeping.
mod app;
mod dock;
mod gizmo;
mod graph;
mod icon;
mod layers;
mod props;
mod scene;
mod theme;
mod timeline;
#[cfg(test)]
mod tests;

use app::*;
use dock::*;
use gizmo::*;
use graph::*;
use layers::*;
use props::*;
use scene::*;
use timeline::*;

fn main() {
    let event_loop = EventLoop::new().unwrap();
    event_loop.set_control_flow(ControlFlow::Wait);
    let mut app = App::new(demo_document());
    println!("Pain By Choice — live. Space=play/pause  ←/→=step  R=restart  Esc=quit");
    event_loop.run_app(&mut app).unwrap();
}
