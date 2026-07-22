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
