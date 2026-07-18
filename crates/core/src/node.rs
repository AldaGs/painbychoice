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
    /// Resolve to (matrix, opacity) at time `t`. The matrix maps local space to
    /// parent space: translate(position) · rotate · scale · translate(-anchor).
    pub fn resolve(&self, t: f64) -> (kurbo::Affine, f64) {
        let anchor = self.anchor.resolve(t);
        let position = self.position.resolve(t);
        let rot = self.rotation_deg.resolve(t).to_radians();
        let scale = self.scale.resolve(t);
        let m = kurbo::Affine::translate(position)
            * kurbo::Affine::rotate(rot)
            * kurbo::Affine::scale_non_uniform(scale.x, scale.y)
            * kurbo::Affine::translate(-anchor);
        (m, self.opacity.resolve(t))
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
    pub fn to_path(&self, t: f64) -> BezPath {
        match self {
            Shape::Path(p) => p.clone(),
            Shape::Rect { size, radius } => {
                let s = size.resolve(t);
                let r = radius.resolve(t);
                let rect = Rect::new(-s.x / 2.0, -s.y / 2.0, s.x / 2.0, s.y / 2.0);
                RoundedRect::from_rect(rect, r).to_path(0.1)
            }
            Shape::Ellipse { size } => {
                let s = size.resolve(t);
                kurbo::Ellipse::new((0.0, 0.0), (s.x / 2.0, s.y / 2.0), 0.0).to_path(0.1)
            }
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
}
