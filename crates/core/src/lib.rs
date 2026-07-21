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

pub mod demo;
pub mod eval;
pub mod expr;
pub mod node;
pub mod text;
pub mod timebase;
pub mod value;

pub use eval::{evaluate, evaluate_comp, evaluate_project, RenderItem, Scene};
pub use expr::{
    eval_script, eval_script_ctx, EvalCtx, Expr, ExprKind, ExprValue, FromExpr, Generator,
    PropPath, ToExpr, Waveform,
};
pub use node::{Comp, CompId, Document, Node, NodeId, Project, Shape, Stroke, Transform};
pub use text::{text_bounds, text_to_path, TextAlign};
pub use timebase::Timebase;
pub use value::{Animatable, Color, Handle, Keyframe, Track, Value};
