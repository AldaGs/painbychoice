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

pub mod eval;
pub mod node;
pub mod value;

pub use eval::{evaluate, RenderItem, Scene};
pub use node::{Document, Node, NodeId, Shape, Stroke, Transform};
pub use value::{Animatable, Color, Handle, Keyframe, Track, Value};
