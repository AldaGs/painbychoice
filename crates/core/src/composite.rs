//! Compositing: how a layer's pixels combine with what is already behind it.
//!
//! This is the front door to the **compositor stage** — the subsystem that
//! effects, mattes, masks and (later) 2.5D placement are all facets of. It
//! starts with blend modes because they are the part that needs *isolation*
//! and nothing else: to multiply a layer over its backdrop you must first have
//! the layer as its own image, which is precisely the offscreen target every
//! later feature also needs.
//!
//! Nothing here decides *how* the isolation happens. `core` says a layer is
//! isolated and how it combines; a backend that can do it does, and one that
//! can't draws the layers plainly and is no worse off than before.

use serde::{Deserialize, Serialize};

/// How a layer's result combines with the backdrop beneath it.
///
/// The sixteen separable + non-separable modes shared by CSS, PDF, SVG and
/// every compositing tool — deliberately that set rather than a bespoke one,
/// because they are what users already know the behaviour of, and what the GPU
/// backend implements natively.
///
/// After Effects has more (Add, Vivid Light, Stencil…), but several of those
/// are the same maths under a different name and the rest are better reached
/// once the effect stack exists. Starting from the standard set means every
/// mode here is exactly right rather than approximately so.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum BlendMode {
    /// Ordinary painting: the layer covers what is behind it, in proportion to
    /// its alpha. The default, and the only mode that needs **no** isolation —
    /// which is why an untouched document renders exactly as it always did.
    #[default]
    Normal,
    Multiply,
    Screen,
    Overlay,
    Darken,
    Lighten,
    ColorDodge,
    ColorBurn,
    HardLight,
    SoftLight,
    Difference,
    Exclusion,
    Hue,
    Saturation,
    Color,
    Luminosity,
}

impl BlendMode {
    /// Every mode, in menu order: Normal, then the separable modes grouped the
    /// way every compositing app groups them (darkening, lightening, contrast,
    /// difference), then the non-separable colour modes.
    pub const ALL: [BlendMode; 16] = [
        BlendMode::Normal,
        BlendMode::Multiply,
        BlendMode::Darken,
        BlendMode::ColorBurn,
        BlendMode::Screen,
        BlendMode::Lighten,
        BlendMode::ColorDodge,
        BlendMode::Overlay,
        BlendMode::HardLight,
        BlendMode::SoftLight,
        BlendMode::Difference,
        BlendMode::Exclusion,
        BlendMode::Hue,
        BlendMode::Saturation,
        BlendMode::Color,
        BlendMode::Luminosity,
    ];

    /// What the menu shows.
    pub fn label(self) -> &'static str {
        match self {
            BlendMode::Normal => "Normal",
            BlendMode::Multiply => "Multiply",
            BlendMode::Screen => "Screen",
            BlendMode::Overlay => "Overlay",
            BlendMode::Darken => "Darken",
            BlendMode::Lighten => "Lighten",
            BlendMode::ColorDodge => "Color Dodge",
            BlendMode::ColorBurn => "Color Burn",
            BlendMode::HardLight => "Hard Light",
            BlendMode::SoftLight => "Soft Light",
            BlendMode::Difference => "Difference",
            BlendMode::Exclusion => "Exclusion",
            BlendMode::Hue => "Hue",
            BlendMode::Saturation => "Saturation",
            BlendMode::Color => "Color",
            BlendMode::Luminosity => "Luminosity",
        }
    }

    /// Whether a layer using this mode has to be rendered **in isolation** —
    /// composited as a finished image rather than painted straight onto the
    /// frame.
    ///
    /// Only `Normal` escapes, and that is what keeps isolation something you
    /// opt into. An offscreen target per layer would otherwise cost a GPU pass
    /// for every group in the document, most of which just paint normally.
    pub fn needs_isolation(self) -> bool {
        self != BlendMode::Normal
    }
}

/// How a layer borrows the shape of the layer above it.
///
/// A **track matte**: the layer above supplies coverage, and is consumed rather
/// than drawn. It is the standard way to cut one layer to the shape of another —
/// text filled with footage, a logo revealed by a moving gradient — and unlike
/// [`Mask`] the cutting shape can be anything a layer can be, including a whole
/// animated composition.
///
/// "The layer above" is the After Effects convention, and here it means the next
/// sibling in document order, which the layers panel draws directly above (it
/// lists front-first). Adjacency is also what makes this cheap: the pair is
/// already contiguous in the draw list, so a matte costs the same range
/// machinery every other isolated layer uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MatteMode {
    /// Show this layer where the layer above is opaque.
    Alpha,
    /// Show this layer where the layer above is *transparent*.
    AlphaInverted,
}

impl MatteMode {
    pub const ALL: [MatteMode; 2] = [MatteMode::Alpha, MatteMode::AlphaInverted];

    pub fn label(self) -> &'static str {
        match self {
            MatteMode::Alpha => "Alpha",
            MatteMode::AlphaInverted => "Alpha Inverted",
        }
    }

    /// How the matte layer composites onto the content beneath it.
    pub fn compose(self) -> ComposeMode {
        match self {
            MatteMode::Alpha => ComposeMode::DestIn,
            MatteMode::AlphaInverted => ComposeMode::DestOut,
        }
    }
}

/// Porter-Duff coverage rules — how a layer's *alpha* meets the backdrop's,
/// as distinct from how their colours mix ([`BlendMode`]).
///
/// Only the three the compositor currently produces. A matte works by
/// compositing the matte layer onto the content with `DestIn`, which keeps the
/// content where the matte is opaque and discards the matte's own colour: the
/// matte contributes shape, not pixels, which is exactly what "consumed rather
/// than drawn" means.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ComposeMode {
    /// Ordinary painting.
    #[default]
    SrcOver,
    /// Keep the backdrop only where this layer is opaque.
    DestIn,
    /// Keep the backdrop only where this layer is transparent.
    DestOut,
}

/// A shape that limits where a layer draws.
///
/// The mask is an ordinary [`Shape`](crate::node::Shape), which is the whole
/// design: it is parametric, so its size and corner radius are `Value`s that
/// keyframe and can be driven by the node graph exactly like a rectangle
/// layer's. A mask needed no new geometry model, no new animation path, and no
/// new editor concepts — it is a shape that clips instead of filling.
///
/// It lives in the **layer's own space**, sharing the layer's transform, so
/// moving or rotating a layer moves its mask with it. That is what makes a mask
/// feel attached rather than merely overlapping.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Mask {
    pub shape: crate::node::Shape,
    /// Hide what is *inside* the shape instead of outside it.
    ///
    /// A flag rather than a second mask kind because it is the same geometry
    /// either way — only the fill rule changes.
    #[serde(default)]
    pub inverted: bool,
}

impl Mask {
    /// A mask from a shape, masking everything outside it.
    pub fn new(shape: crate::node::Shape) -> Self {
        Self { shape, inverted: false }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The default has to be `Normal`, and `Normal` has to be free: every
    /// document written before this existed deserializes into it, and none of
    /// them may start paying for offscreen targets.
    #[test]
    fn the_default_mode_costs_nothing() {
        assert_eq!(BlendMode::default(), BlendMode::Normal);
        assert!(!BlendMode::default().needs_isolation());
    }

    /// Every other mode needs isolating — that is what distinguishes them.
    #[test]
    fn every_other_mode_needs_isolation() {
        for m in BlendMode::ALL {
            assert_eq!(m.needs_isolation(), m != BlendMode::Normal, "{}", m.label());
        }
    }

    /// The menu lists each mode exactly once. A duplicate would be a picker
    /// with two entries that do the same thing.
    #[test]
    fn the_menu_lists_each_mode_once() {
        let mut seen = std::collections::BTreeSet::new();
        for m in BlendMode::ALL {
            assert!(seen.insert(m.label()), "{} listed twice", m.label());
        }
        assert_eq!(seen.len(), BlendMode::ALL.len());
    }
}
