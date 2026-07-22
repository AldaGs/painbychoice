//! motion-core — the headless engine.
//!
//! A `Document` is a scene graph where every animatable property is a
//! `Value<T>` (a constant or a keyframe track today; an expression / parametric
//! IR later). `evaluate(&doc, t)` is a pure function that resolves the whole
//! tree at time `t` into a flat `Scene` of draw items. That single idea gives
//! us non-destructive editing and non-linear scrubbing for free.
//!
//! This crate has no GPU and no windowing dependency on purpose: the engine
//! must be testable by rendering a frame in a `cargo test`, not a window.

pub mod asset;
pub mod composite;
pub mod demo;
pub mod eval;
pub mod expr;
pub mod graph;
pub mod lower;
pub mod node;
pub mod raise;
pub mod registry;
pub mod socket;
pub mod text;
pub mod timebase;
pub mod value;

pub use asset::{
    Asset, AssetId, AssetKind, AssetMeta, DecodeError, Decoder, DecoderRegistry, Frame, FrameStream,
    ImagePaint,
};
pub use composite::{BlendMode, Mask};
pub use eval::{evaluate, evaluate_comp, evaluate_project, LayerGroup, MaskPath, RenderItem, Scene};
pub use expr::{
    eval_script, eval_script_ctx, BinOp, EvalCtx, Expr, ExprKind, ExprValue, FromExpr, Generator,
    MathOp, UnOp,
    PropPath, ToExpr, Waveform,
};
pub use node::{
    Comp, CompId, Document, Grid, Guide, GuideAxis, Guides, Node, NodeId, Onion, Project, Shape,
    Stroke, Transform, ViewAids,
};
pub use graph::{
    Binding, ConnectError, Edge, Endpoint, GraphCtx, GraphError, GraphNode, GraphNodeId, NodeGraph,
    ShapeBinding, TextConfig,
};
pub use lower::{compile_modules, lower_geometry, lower_output};
pub use raise::{raise, raise_geometry, RaiseShapeError};
pub use registry::{
    builtin_descriptors, NodeCategory, NodeDescriptor, NodeRegistry, RegisterError,
};
pub use socket::{Socket, SocketType};
pub use text::{text_bounds, text_to_path, TextAlign};
pub use timebase::Timebase;
pub use value::{mirror_handle, Animatable, Color, EasePreset, Handle, Interp, Keyframe, Track, Value};
