//! The scene graph: a tree of `Node`s where every animatable field is a
//! `Value<_>`. Nothing here is baked — `eval` turns a document + time into a
//! flat `Scene`.

use kurbo::{BezPath, Rect, RoundedRect, Shape as _, Vec2};
use serde::{Deserialize, Serialize};

use crate::value::{Color, Value};

/// Stable identity for a node, used for selection and for tracing an evaluated
/// render item back to its source (EBN's line→nodeId map idea, applied to a
/// pull-based dataflow graph).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct NodeId(pub u64);

/// An affine transform, every channel animatable. Resolves to a
/// `kurbo::Affine` plus a scalar opacity at a given time.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Transform {
    pub anchor: Value<Vec2>,
    pub position: Value<Vec2>,
    pub rotation_deg: Value<f64>,
    pub scale: Value<Vec2>,
    pub opacity: Value<f64>,
}

impl Default for Transform {
    fn default() -> Self {
        Self {
            anchor: Value::constant(Vec2::ZERO),
            position: Value::constant(Vec2::ZERO),
            rotation_deg: Value::constant(0.0),
            scale: Value::constant(Vec2::new(1.0, 1.0)),
            opacity: Value::constant(1.0),
        }
    }
}

impl Transform {
    /// Resolve to (matrix, opacity) at `frame`. The matrix maps local space to
    /// parent space: translate(position) · rotate · scale · translate(-anchor).
    pub fn resolve(&self, frame: f64) -> (kurbo::Affine, f64) {
        let anchor = self.anchor.resolve(frame);
        let position = self.position.resolve(frame);
        let rot = self.rotation_deg.resolve(frame).to_radians();
        let scale = self.scale.resolve(frame);
        let m = kurbo::Affine::translate(position)
            * kurbo::Affine::rotate(rot)
            * kurbo::Affine::scale_non_uniform(scale.x, scale.y)
            * kurbo::Affine::translate(-anchor);
        (m, self.opacity.resolve(frame))
    }

    pub(crate) fn migrate_frames(&mut self, fps: f64) {
        self.anchor.migrate_frames(fps);
        self.position.migrate_frames(fps);
        self.rotation_deg.migrate_frames(fps);
        self.scale.migrate_frames(fps);
        self.opacity.migrate_frames(fps);
    }
}

/// A drawable shape. Parametric variants resolve their geometry at time `t`,
/// so a rectangle's size can itself be keyframed.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Shape {
    /// A pre-built path (imported / drawn by hand).
    Path(BezPath),
    /// Rounded rectangle centered on the origin, animatable size and corner.
    Rect {
        size: Value<Vec2>,
        radius: Value<f64>,
    },
    /// Ellipse centered on the origin, animatable size (width/height).
    Ellipse { size: Value<Vec2> },
}

impl Shape {
    pub fn to_path(&self, frame: f64) -> BezPath {
        match self {
            Shape::Path(p) => p.clone(),
            Shape::Rect { size, radius } => {
                let s = size.resolve(frame);
                let r = radius.resolve(frame);
                let rect = Rect::new(-s.x / 2.0, -s.y / 2.0, s.x / 2.0, s.y / 2.0);
                RoundedRect::from_rect(rect, r).to_path(0.1)
            }
            Shape::Ellipse { size } => {
                let s = size.resolve(frame);
                kurbo::Ellipse::new((0.0, 0.0), (s.x / 2.0, s.y / 2.0), 0.0).to_path(0.1)
            }
        }
    }

    pub(crate) fn migrate_frames(&mut self, fps: f64) {
        match self {
            Shape::Path(_) => {}
            Shape::Rect { size, radius } => {
                size.migrate_frames(fps);
                radius.migrate_frames(fps);
            }
            Shape::Ellipse { size } => size.migrate_frames(fps),
        }
    }
}

/// A stroke: animatable color and width.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Stroke {
    pub color: Value<Color>,
    pub width: Value<f64>,
}

/// One node in the scene graph. A group (no shape) just composes its children;
/// a leaf carries a shape + paint.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Node {
    pub id: NodeId,
    pub name: String,
    pub transform: Transform,
    pub shape: Option<Shape>,
    pub fill: Option<Value<Color>>,
    pub stroke: Option<Stroke>,
    pub children: Vec<Node>,
}

impl Node {
    pub fn group(id: u64, name: impl Into<String>) -> Self {
        Self {
            id: NodeId(id),
            name: name.into(),
            transform: Transform::default(),
            shape: None,
            fill: None,
            stroke: None,
            children: Vec::new(),
        }
    }

    pub fn shape(id: u64, name: impl Into<String>, shape: Shape) -> Self {
        Self {
            id: NodeId(id),
            name: name.into(),
            transform: Transform::default(),
            shape: Some(shape),
            fill: None,
            stroke: None,
            children: Vec::new(),
        }
    }

    pub fn with_fill(mut self, color: Color) -> Self {
        self.fill = Some(Value::constant(color));
        self
    }

    pub fn with_transform(mut self, transform: Transform) -> Self {
        self.transform = transform;
        self
    }

    pub fn with_child(mut self, child: Node) -> Self {
        self.children.push(child);
        self
    }

    /// Depth-first search for a node by id, self included.
    pub fn find(&self, id: NodeId) -> Option<&Node> {
        if self.id == id {
            return Some(self);
        }
        self.children.iter().find_map(|c| c.find(id))
    }

    /// Mutable depth-first search for a node by id, self included.
    pub fn find_mut(&mut self, id: NodeId) -> Option<&mut Node> {
        if self.id == id {
            return Some(self);
        }
        self.children.iter_mut().find_map(|c| c.find_mut(id))
    }

    /// Move the child with `id` among its siblings by `delta` (e.g. -1 up, +1
    /// down), clamped to the ends. Searches the whole subtree for the parent.
    /// Returns whether a move happened. Child order is also draw order, so this
    /// restacks the node visually.
    pub fn reorder_child(&mut self, id: NodeId, delta: i32) -> bool {
        if let Some(i) = self.children.iter().position(|c| c.id == id) {
            let j = (i as i32 + delta).clamp(0, self.children.len() as i32 - 1) as usize;
            if i != j {
                self.children.swap(i, j);
                return true;
            }
            return false;
        }
        self.children.iter_mut().any(|c| c.reorder_child(id, delta))
    }

    /// Remove the node with `id` from this subtree (cannot remove `self`).
    /// Returns the removed node, or `None` if not found.
    pub fn remove(&mut self, id: NodeId) -> Option<Node> {
        if let Some(i) = self.children.iter().position(|c| c.id == id) {
            return Some(self.children.remove(i));
        }
        self.children.iter_mut().find_map(|c| c.remove(id))
    }

    /// Recursively convert legacy float-seconds keyframes to frames at `fps`.
    pub(crate) fn migrate_frames(&mut self, fps: f64) {
        self.transform.migrate_frames(fps);
        if let Some(shape) = &mut self.shape {
            shape.migrate_frames(fps);
        }
        if let Some(fill) = &mut self.fill {
            fill.migrate_frames(fps);
        }
        if let Some(stroke) = &mut self.stroke {
            stroke.color.migrate_frames(fps);
            stroke.width.migrate_frames(fps);
        }
        for child in &mut self.children {
            child.migrate_frames(fps);
        }
    }
}

/// The whole animated document: a root node plus composition settings.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Document {
    pub width: f64,
    pub height: f64,
    pub fps: f64,
    pub duration: f64,
    pub root: Node,
}

impl Document {
    pub fn new(width: f64, height: f64, root: Node) -> Self {
        Self {
            width,
            height,
            fps: 60.0,
            duration: 5.0,
            root,
        }
    }

    /// The composition's frame grid. Every seconds↔frames conversion and every
    /// timecode string goes through this — never divide by `fps` by hand.
    pub fn timebase(&self) -> crate::timebase::Timebase {
        crate::timebase::Timebase::new(self.fps)
    }

    /// Bring a freshly-deserialized document up to the current format.
    ///
    /// Today that means converting legacy float-seconds keyframes to frames
    /// using this document's `fps`. Must be called after *every* load — it is
    /// a no-op on an already-migrated doc, so calling it twice is safe.
    pub fn migrate(&mut self) {
        let fps = self.timebase().fps();
        self.root.migrate_frames(fps);
    }

    /// Total length of the composition in whole frames: 5s @ 24fps = 120.
    pub fn duration_frames(&self) -> i64 {
        self.timebase().seconds_to_frames(self.duration)
    }
}
