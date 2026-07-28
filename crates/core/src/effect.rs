//! The effect stack: ordered pixel operations applied to a layer's finished,
//! isolated image.
//!
//! This is the second tenant of the compositor stage (after blend modes,
//! mattes and masks in [`crate::composite`]). An effect is fundamentally a
//! *raster* operation — it reworks the pixels a layer produced rather than the
//! geometry that produced them — so it can only act on a layer that has been
//! rendered into an offscreen target of its own. That is precisely the
//! isolation the compositor already builds for a blend mode: an effect is one
//! more reason a layer isolates, and one more thing that happens to the
//! isolated image before it composites with the backdrop.
//!
//! **Where the pipeline is split.** As everywhere in this crate, the authored,
//! animatable form lives here as `Value<T>` params ([`Effect`]) and the pure
//! `evaluate` walk resolves them for one frame into a backend-facing
//! [`ResolvedEffect`] carrying plain numbers. `core` decides *which* effects a
//! layer has and *what their parameters are on this frame*; it never touches a
//! pixel. A backend that can rasterize a layer applies the resolved stack; one
//! that cannot (the SVG backend, the offline `motion` binary) ignores it and
//! draws the layer plainly — exactly the arrangement track mattes already have.
//!
//! **The first set is the same-bounds filters**: blur and colour adjustments,
//! every one of which reads the isolated RGBA image and writes an image of the
//! same size. That uniformity is deliberate — one backend seam (filter an image
//! in place) covers all of them. Effects that *grow* the layer's bounds (drop
//! shadow, glow) are a later addition: they need the offscreen target sized with
//! margin, which is a real change to how isolation is measured, not just another
//! filter.

use serde::{Deserialize, Serialize};

use crate::expr::EvalCtx;
use crate::value::{Color, Value};

/// One entry in a layer's effect stack: an animatable pixel operation plus
/// whether it is currently switched on.
///
/// `enabled` is a plain flag, not the absence of the effect, so an effect can
/// be muted to compare with and without it and switched back on with its
/// parameters (and their keyframes) intact — the standard behaviour of every
/// effect panel.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Effect {
    /// Off effects stay in the stack but contribute nothing — resolved away
    /// rather than removed, so their parameters survive the toggle.
    #[serde(default = "yes")]
    pub enabled: bool,
    pub kind: EffectKind,
}

fn yes() -> bool {
    true
}

/// The kinds of pixel effect, each carrying its own animatable parameters.
///
/// An enum of variants-with-`Value`-fields, the same shape as [`crate::node::Shape`]:
/// a new effect is a new arm, and its parameters keyframe, retime and take
/// expressions through the ordinary `Value` machinery for free. The variants
/// here are the same-bounds filters; see the module docs for why that set comes
/// first.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum EffectKind {
    /// Isotropic Gaussian blur. `radius` is the standard deviation in
    /// composition pixels; `0` is a no-op.
    GaussianBlur { radius: Value<f64> },
    /// Linear brightness and contrast. Both are signed amounts centred on `0`
    /// (no change); positive brightens / increases contrast.
    BrightnessContrast { brightness: Value<f64>, contrast: Value<f64> },
    /// HSL adjustment. `hue` is a rotation in degrees; `saturation` and
    /// `lightness` are signed amounts centred on `0`.
    HueSaturation { hue: Value<f64>, saturation: Value<f64>, lightness: Value<f64> },
    /// Push every pixel toward `color` by `amount` in `[0, 1]` (a linear mix),
    /// preserving alpha. The classic single-colour wash.
    Tint { color: Value<Color>, amount: Value<f64> },
}

impl Effect {
    /// A new, enabled effect of the given kind, seeded with the neutral /
    /// conventional defaults an "add effect" menu should drop in.
    pub fn new(kind: EffectKind) -> Self {
        Self { enabled: true, kind }
    }

    /// The default effect for a menu choice — every parameter at its no-op or
    /// conventional starting value, so adding one never changes the frame until
    /// the user dials it in.
    pub fn seed(ty: EffectType) -> Self {
        let kind = match ty {
            EffectType::GaussianBlur => EffectKind::GaussianBlur { radius: Value::constant(8.0) },
            EffectType::BrightnessContrast => EffectKind::BrightnessContrast {
                brightness: Value::constant(0.0),
                contrast: Value::constant(0.0),
            },
            EffectType::HueSaturation => EffectKind::HueSaturation {
                hue: Value::constant(0.0),
                saturation: Value::constant(0.0),
                lightness: Value::constant(0.0),
            },
            EffectType::Tint => EffectKind::Tint {
                color: Value::constant(Color::rgb(1.0, 1.0, 1.0)),
                amount: Value::constant(1.0),
            },
        };
        Self::new(kind)
    }

    /// Which kind this is, as the flat [`EffectType`] discriminant used by menus
    /// and labels.
    pub fn effect_type(&self) -> EffectType {
        match self.kind {
            EffectKind::GaussianBlur { .. } => EffectType::GaussianBlur,
            EffectKind::BrightnessContrast { .. } => EffectType::BrightnessContrast,
            EffectKind::HueSaturation { .. } => EffectType::HueSaturation,
            EffectKind::Tint { .. } => EffectType::Tint,
        }
    }

    /// Resolve this effect's parameters at the current frame, or `None` if it is
    /// disabled (so a muted effect drops out of the stack the backend sees).
    pub fn resolve(&self, ctx: &mut EvalCtx) -> Option<ResolvedEffect> {
        if !self.enabled {
            return None;
        }
        Some(match &self.kind {
            EffectKind::GaussianBlur { radius } => {
                ResolvedEffect::GaussianBlur { radius: radius.resolve(ctx).max(0.0) }
            }
            EffectKind::BrightnessContrast { brightness, contrast } => {
                ResolvedEffect::BrightnessContrast {
                    brightness: brightness.resolve(ctx),
                    contrast: contrast.resolve(ctx),
                }
            }
            EffectKind::HueSaturation { hue, saturation, lightness } => {
                ResolvedEffect::HueSaturation {
                    hue: hue.resolve(ctx),
                    saturation: saturation.resolve(ctx),
                    lightness: lightness.resolve(ctx),
                }
            }
            EffectKind::Tint { color, amount } => ResolvedEffect::Tint {
                color: color.resolve(ctx),
                amount: amount.resolve(ctx).clamp(0.0, 1.0),
            },
        })
    }

    pub(crate) fn migrate_frames(&mut self, fps: f64) {
        self.for_each_value(|v| v.migrate_frames(fps), |c| c.migrate_frames(fps));
    }

    pub(crate) fn retime(&mut self, ratio: f64) {
        self.for_each_value(|v| v.retime(ratio), |c| c.retime(ratio));
    }

    /// Walk every scalar / colour `Value` in this effect, applying the matching
    /// callback. The one place the parameter set of each variant is enumerated
    /// for the grid operations, so migrate and retime can't drift apart.
    fn for_each_value(
        &mut self,
        mut num: impl FnMut(&mut Value<f64>),
        mut col: impl FnMut(&mut Value<Color>),
    ) {
        match &mut self.kind {
            EffectKind::GaussianBlur { radius } => num(radius),
            EffectKind::BrightnessContrast { brightness, contrast } => {
                num(brightness);
                num(contrast);
            }
            EffectKind::HueSaturation { hue, saturation, lightness } => {
                num(hue);
                num(saturation);
                num(lightness);
            }
            EffectKind::Tint { color, amount } => {
                col(color);
                num(amount);
            }
        }
    }
}

/// The flat discriminant of [`EffectKind`] — one value per effect kind, with no
/// parameters. This is what an "add effect" menu is built from and what a label
/// comes off of.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum EffectType {
    GaussianBlur,
    BrightnessContrast,
    HueSaturation,
    Tint,
}

impl EffectType {
    /// Every kind, in menu order.
    pub const ALL: [EffectType; 4] = [
        EffectType::GaussianBlur,
        EffectType::BrightnessContrast,
        EffectType::HueSaturation,
        EffectType::Tint,
    ];

    pub fn label(self) -> &'static str {
        match self {
            EffectType::GaussianBlur => "Gaussian Blur",
            EffectType::BrightnessContrast => "Brightness & Contrast",
            EffectType::HueSaturation => "Hue / Saturation",
            EffectType::Tint => "Tint",
        }
    }
}

/// A layer's effect with its parameters fixed to their values on one frame —
/// the backend-facing form.
///
/// Plain numbers and colours, no `Value`, no `EvalCtx`: a renderer applies these
/// to an isolated RGBA image knowing nothing about keyframes or expressions,
/// the same way [`crate::eval::RenderItem`] hands it a `BezPath` rather than a
/// `Shape`. Every same-bounds filter maps to a routine of the form
/// `image -> image` at the same size.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ResolvedEffect {
    GaussianBlur { radius: f64 },
    BrightnessContrast { brightness: f64, contrast: f64 },
    HueSaturation { hue: f64, saturation: f64, lightness: f64 },
    Tint { color: Color, amount: f64 },
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A disabled effect resolves to nothing, so the backend never sees it —
    /// muting is "drop from the resolved stack", not "apply an identity pass".
    #[test]
    fn a_disabled_effect_resolves_away() {
        let mut e = Effect::seed(EffectType::GaussianBlur);
        e.enabled = false;
        let mut ctx = EvalCtx::at(0.0);
        assert_eq!(e.resolve(&mut ctx), None);
    }

    /// An enabled seed resolves to its default parameters.
    #[test]
    fn an_enabled_effect_resolves_its_params() {
        let e = Effect::seed(EffectType::GaussianBlur);
        let mut ctx = EvalCtx::at(0.0);
        assert_eq!(e.resolve(&mut ctx), Some(ResolvedEffect::GaussianBlur { radius: 8.0 }));
    }

    /// A negative blur radius is a no-op, not a panic or a mirrored kernel:
    /// clamped to zero on the way out.
    #[test]
    fn blur_radius_never_goes_negative() {
        let e = Effect::new(EffectKind::GaussianBlur { radius: Value::constant(-5.0) });
        let mut ctx = EvalCtx::at(0.0);
        assert_eq!(e.resolve(&mut ctx), Some(ResolvedEffect::GaussianBlur { radius: 0.0 }));
    }

    /// Tint amount is a mix factor, so it is clamped to `[0, 1]` — a value the
    /// user dragged past 1 can't over-mix.
    #[test]
    fn tint_amount_is_clamped_to_a_mix() {
        let e = Effect::new(EffectKind::Tint {
            color: Value::constant(Color::rgb(1.0, 0.0, 0.0)),
            amount: Value::constant(3.0),
        });
        let mut ctx = EvalCtx::at(0.0);
        match e.resolve(&mut ctx) {
            Some(ResolvedEffect::Tint { amount, .. }) => assert_eq!(amount, 1.0),
            other => panic!("expected a tint, got {other:?}"),
        }
    }

    /// The discriminant round-trips: every type seeds an effect that reports the
    /// same type back, and each label is distinct.
    #[test]
    fn every_type_seeds_and_labels_itself() {
        let mut seen = std::collections::BTreeSet::new();
        for ty in EffectType::ALL {
            assert_eq!(Effect::seed(ty).effect_type(), ty);
            assert!(seen.insert(ty.label()), "{} listed twice", ty.label());
        }
        assert_eq!(seen.len(), EffectType::ALL.len());
    }
}
