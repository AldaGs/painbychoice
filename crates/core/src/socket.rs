//! The node-graph **port type system**: what a socket carries, and the colour
//! the UI paints its dot.
//!
//! This is step 1 of the composition node graph (see *The composition node
//! graph* in the README's design section). It is deliberately **pure metadata**
//! — it says nothing about *evaluation*. The closed IR enums (`Expr`, `Shape`,
//! `Generator`) stay the evaluation substrate; a socket type only describes the
//! shape of a wire so the descriptor-driven canvas can draw it and refuse an
//! illegal connection. Lowering a graph to the IR is a later step, kept out of
//! here on purpose so this layer can't grow into a rival evaluator.

use serde::{Deserialize, Serialize};

use crate::expr::ExprValue;
use crate::value::Color;

/// The type a node socket (port) carries. Determines what may connect to what,
/// and — via [`SocketType::color`] — the Blender-style coloured dot the graph
/// draws for it. **Colour is a property of the type, defined once here**, rather
/// than chosen per node in the UI: a `Vector` output and a `Vector` input read
/// as the same wire everywhere because they share the one colour.
///
/// The first three mirror [`crate::expr::ExprValue`]'s kinds exactly — they are
/// the value types the *property* graph already flows — so an object-scope node
/// lowers to an `Expr` without a type translation. The rest are the
/// *scene*-graph additions: geometry, whole layers, and mattes have no
/// `ExprValue`, which is precisely why they need the bigger graph.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SocketType {
    /// A scalar. Mirrors `ExprValue::Num`.
    Number,
    /// A 2D vector — a position, a size, an offset. Mirrors `ExprValue::Vec2`.
    Vector,
    /// An RGBA colour. Mirrors `ExprValue::Color`.
    Color,
    /// Text. Mirrors `ExprValue::Str`. The one value type with no arithmetic,
    /// so it deliberately does **not** interchange with `Number` the way `Time`
    /// does — a number feeding a text input would silently stringify, which is
    /// fine as an explicit `Add` (concatenation) but not as a wire.
    Text,
    /// Vector geometry — a shape's outline (a rectangle node's `geometry`
    /// output). Has no `ExprValue`: geometry isn't an interpolatable scalar,
    /// which is the same reason a `Shape::Path` isn't a `Value`.
    Geometry,
    /// A rasterized layer/render — the output of a layer or composition, and the
    /// input an effect or compositing node consumes. Behind the **compositor
    /// stage**, which isn't built: a `Layer` socket can exist on a descriptor
    /// today, but nothing evaluates one until that stage lands.
    Layer,
    /// An alpha / coverage channel — a matte or a mask. Also gated on the
    /// compositor stage.
    Matte,
    /// A time / frame value — a clock a node reads instead of the global frame,
    /// so an animation can be retimed by rewiring where its time comes from.
    Time,
}

impl SocketType {
    /// Every socket type, in a stable order — for a legend, and so a test can
    /// assert each has a distinct colour and a label.
    pub const ALL: [SocketType; 8] = [
        SocketType::Number,
        SocketType::Vector,
        SocketType::Color,
        SocketType::Text,
        SocketType::Geometry,
        SocketType::Layer,
        SocketType::Matte,
        SocketType::Time,
    ];

    /// The short name shown on hover / in a legend. Spelled out rather than
    /// derived from `Debug` so renaming a variant can't silently change the
    /// user-facing vocabulary — the same rule [`crate::expr::PropPath::name`]
    /// follows.
    pub fn label(self) -> &'static str {
        match self {
            SocketType::Number => "Number",
            SocketType::Vector => "Vector",
            SocketType::Color => "Color",
            SocketType::Text => "Text",
            SocketType::Geometry => "Geometry",
            SocketType::Layer => "Layer",
            SocketType::Matte => "Matte",
            SocketType::Time => "Time",
        }
    }

    /// Whether an output of `self` may feed an input of `other`. Equal types
    /// always connect; beyond that, two pairs interchange:
    ///
    /// - **`Time` and `Number`**, because a layer-clock reading *is* a number
    ///   (`localTime` into a `mul`, a literal frame into a ramp's `start`).
    /// - **`Text` and `Number`**, because the math nodes' sockets are declared
    ///   `Number` but are really "any scalar value" — a `value` node's output is
    ///   `Number` whatever literal it holds, which is how a `Vec2` already flows
    ///   through an `add` today. Refusing text there would make `Add`'s
    ///   concatenation unbuildable on the canvas even between two strings.
    ///
    /// Everything else must match exactly. Note the asymmetry with
    /// [`crate::expr::FromExpr`]: a wire being *legal* is not a promise the
    /// value converts at the far end — a number reaching a text **property**
    /// still falls back to empty, and `Add` is how you ask for it in words.
    pub fn feeds(self, other: SocketType) -> bool {
        use SocketType::{Number, Text, Time};
        self == other
            || matches!(
                (self, other),
                (Time, Number) | (Number, Time) | (Text, Number) | (Number, Text)
            )
    }

    /// The colour of this type's socket dot — the whole point of a *typed* port
    /// being that you can read a graph's dataflow at a glance. Hues follow
    /// Blender's convention where there's an equivalent (grey number, purple
    /// vector, yellow colour, green geometry) and are chosen to stay distinct
    /// for the scene-graph additions.
    pub fn color(self) -> Color {
        match self {
            // Blender's float grey.
            SocketType::Number => Color::rgb(0.63, 0.63, 0.63),
            // Blender's vector purple.
            SocketType::Vector => Color::rgb(0.39, 0.35, 0.78),
            // Blender's colour yellow.
            SocketType::Color => Color::rgb(0.86, 0.74, 0.20),
            // A warm red, well clear of the colour yellow beside it.
            SocketType::Text => Color::rgb(0.78, 0.31, 0.31),
            // Blender's geometry green.
            SocketType::Geometry => Color::rgb(0.0, 0.62, 0.35),
            // A render/image blue — distinct from the vector purple.
            SocketType::Layer => Color::rgb(0.25, 0.55, 0.85),
            // A desaturated slate for alpha/coverage.
            SocketType::Matte => Color::rgb(0.60, 0.62, 0.66),
            // A clock orange.
            SocketType::Time => Color::rgb(0.85, 0.47, 0.20),
        }
    }
}

/// One port on a node type: a stable id (how a wire names its endpoint), a human
/// label, and the [`SocketType`] it carries.
///
/// Fields are **owned** (`String`), not `&'static str`: a descriptor is built at
/// runtime by a plugin exactly as it is by a built-in, so its socket names can't
/// be baked into the binary. The registry is the seam a plugin registers
/// through, so everything reachable from a descriptor has to be constructible
/// without compile-time knowledge.
///
/// `Eq` is deliberately *not* derived: the `default` value carries an
/// `ExprValue`, which holds `f64`s and so is only `PartialEq`. A socket is
/// descriptor metadata, never a map key, so `PartialEq` is all it needs.
#[derive(Clone, Debug, PartialEq)]
pub struct Socket {
    /// Addresses this port in a wire. Unique among a descriptor's inputs (and,
    /// separately, its outputs) — [`crate::registry::NodeDescriptor`] enforces
    /// it, since a duplicate would make an endpoint ambiguous.
    pub id: String,
    /// What the canvas prints beside the dot.
    pub label: String,
    pub ty: SocketType,
    /// The literal an **input** feeds when nothing is wired to it — a node's
    /// resting value, and where lowering reads an unwired knob's default (so a
    /// fresh `osc` already oscillates rather than sitting at zero). `None` on
    /// outputs, and on inputs that must be wired (geometry / layer / matte,
    /// which have no scalar literal).
    pub default: Option<ExprValue>,
}

impl Socket {
    /// A socket with no input default (an output, or an input that must be
    /// wired).
    pub fn new(id: impl Into<String>, label: impl Into<String>, ty: SocketType) -> Self {
        Self { id: id.into(), label: label.into(), ty, default: None }
    }

    /// An input socket whose unwired value is `default`.
    pub fn with_default(
        id: impl Into<String>,
        label: impl Into<String>,
        ty: SocketType,
        default: ExprValue,
    ) -> Self {
        Self { id: id.into(), label: label.into(), ty, default: Some(default) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every type must paint a *distinct* dot — a typed port whose colour
    /// collides with another's defeats the one thing colour-coding is for.
    #[test]
    fn every_socket_type_has_a_distinct_colour() {
        for (i, a) in SocketType::ALL.iter().enumerate() {
            for b in &SocketType::ALL[i + 1..] {
                assert_ne!(a.color(), b.color(), "{} and {} share a colour", a.label(), b.label());
            }
        }
    }

    /// Colours are RGB in [0,1] and opaque — a socket dot with an out-of-range
    /// or transparent colour would draw wrong (or invisibly).
    #[test]
    fn socket_colours_are_in_gamut_and_opaque() {
        for t in SocketType::ALL {
            let c = t.color();
            for ch in [c.r, c.g, c.b] {
                assert!((0.0..=1.0).contains(&ch), "{} channel out of range: {ch}", t.label());
            }
            assert_eq!(c.a, 1.0, "{} dot must be opaque", t.label());
        }
    }

    /// `ALL` is the list the UI and tests iterate; a variant left out of it is
    /// invisible to both. Guard the count so adding a variant without listing it
    /// fails here rather than silently.
    #[test]
    fn all_lists_every_variant() {
        assert_eq!(SocketType::ALL.len(), 8);
        // Labels are unique too, so the legend can't show two "Number"s.
        let mut labels: Vec<_> = SocketType::ALL.iter().map(|t| t.label()).collect();
        labels.sort_unstable();
        labels.dedup();
        assert_eq!(labels.len(), SocketType::ALL.len());
    }
}
