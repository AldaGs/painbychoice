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
//! **Every effect here is an image-in, image-out filter at the same size**, and
//! that uniformity is what keeps the backend seam to one operation. A drop
//! shadow appears to break it — a shadow falls *outside* the artwork — but it
//! does not, because a layer's isolated target is the **whole canvas** in both
//! rasterizers, not a box around its artwork. The shadow has somewhere to fall
//! for the same reason a blur's halo does. What would genuinely need a
//! margin-sized target is a shadow that must survive being drawn *past the
//! frame edge*, which nothing downstream can see anyway.

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
    /// Remap the tonal range: everything at or below `in_black` becomes
    /// `out_black`, everything at or above `in_white` becomes `out_white`, and
    /// `gamma` bends the curve between them (`1` is straight, above `1`
    /// brightens the midtones).
    ///
    /// The grading control people reach for first, and per-channel identical —
    /// a colour cast is corrected by a hue shift or a tint, not by pretending
    /// this is three effects.
    Levels {
        in_black: Value<f64>,
        in_white: Value<f64>,
        gamma: Value<f64>,
        out_black: Value<f64>,
        out_white: Value<f64>,
    },
    /// A blurred, offset copy of the layer's own alpha, painted in `color`
    /// underneath it.
    ///
    /// The offset is in composition pixels and is **not** rotated with the
    /// layer: a shadow is cast by a light in the scene, not by the artwork, so
    /// spinning a layer must not spin its shadow around with it.
    DropShadow {
        color: Value<Color>,
        offset_x: Value<f64>,
        offset_y: Value<f64>,
        radius: Value<f64>,
        opacity: Value<f64>,
    },
    /// The layer's bright parts, blurred and added back on top: everything
    /// above `threshold` (luma, `0..1`) bleeds out by `radius` pixels at
    /// `intensity` strength. Additive, so a glow can only brighten.
    Glow { threshold: Value<f64>, radius: Value<f64>, intensity: Value<f64> },
    /// Signed per-channel shifts in `[-1, 1]`, added to red, green and blue —
    /// pushing a cast in or out without the all-channels-at-once of Levels.
    ColorBalance { red: Value<f64>, green: Value<f64>, blue: Value<f64> },
    /// Knock out pixels near `color`: within `tolerance` (RGB distance) they go
    /// fully transparent, then alpha ramps back over `softness`.
    ///
    /// The first effect that writes *alpha* rather than colour, and so the
    /// first the in-scene colour fast path cannot approximate at all.
    ChromaKey { color: Value<Color>, tolerance: Value<f64>, softness: Value<f64> },
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
            // The identity curve: full range in, full range out, straight
            // gamma. Adding Levels changes nothing until it is dialled in.
            EffectType::Levels => EffectKind::Levels {
                in_black: Value::constant(0.0),
                in_white: Value::constant(1.0),
                gamma: Value::constant(1.0),
                out_black: Value::constant(0.0),
                out_white: Value::constant(1.0),
            },
            // A shadow *is* the effect, so unlike the others its seed is not a
            // no-op: adding it with everything at zero would look broken.
            // Down-right, soft, black, half strength — the default every
            // compositor ships.
            EffectType::DropShadow => EffectKind::DropShadow {
                color: Value::constant(Color::rgb(0.0, 0.0, 0.0)),
                offset_x: Value::constant(8.0),
                offset_y: Value::constant(8.0),
                radius: Value::constant(6.0),
                opacity: Value::constant(0.5),
            },
            // Like the shadow, a glow is the thing it adds: seeded visible.
            EffectType::Glow => EffectKind::Glow {
                threshold: Value::constant(0.6),
                radius: Value::constant(12.0),
                intensity: Value::constant(1.0),
            },
            EffectType::ColorBalance => EffectKind::ColorBalance {
                red: Value::constant(0.0),
                green: Value::constant(0.0),
                blue: Value::constant(0.0),
            },
            // Green screen: the key everyone adds this for first.
            EffectType::ChromaKey => EffectKind::ChromaKey {
                color: Value::constant(Color::rgb(0.0, 1.0, 0.0)),
                tolerance: Value::constant(0.3),
                softness: Value::constant(0.1),
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
            EffectKind::Levels { .. } => EffectType::Levels,
            EffectKind::DropShadow { .. } => EffectType::DropShadow,
            EffectKind::Glow { .. } => EffectType::Glow,
            EffectKind::ColorBalance { .. } => EffectType::ColorBalance,
            EffectKind::ChromaKey { .. } => EffectType::ChromaKey,
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
            EffectKind::Levels { in_black, in_white, gamma, out_black, out_white } => {
                ResolvedEffect::Levels {
                    in_black: in_black.resolve(ctx),
                    in_white: in_white.resolve(ctx),
                    // A gamma of zero is a division by zero downstream and a
                    // negative one is meaningless; clamped here so no backend
                    // has to guess.
                    gamma: gamma.resolve(ctx).clamp(0.01, 100.0),
                    out_black: out_black.resolve(ctx),
                    out_white: out_white.resolve(ctx),
                }
            }
            EffectKind::DropShadow { color, offset_x, offset_y, radius, opacity } => {
                ResolvedEffect::DropShadow {
                    color: color.resolve(ctx),
                    offset_x: offset_x.resolve(ctx),
                    offset_y: offset_y.resolve(ctx),
                    radius: radius.resolve(ctx).max(0.0),
                    opacity: opacity.resolve(ctx).clamp(0.0, 1.0),
                }
            }
            EffectKind::Glow { threshold, radius, intensity } => ResolvedEffect::Glow {
                threshold: threshold.resolve(ctx).clamp(0.0, 1.0),
                radius: radius.resolve(ctx).max(0.0),
                intensity: intensity.resolve(ctx).max(0.0),
            },
            EffectKind::ColorBalance { red, green, blue } => ResolvedEffect::ColorBalance {
                red: red.resolve(ctx).clamp(-1.0, 1.0),
                green: green.resolve(ctx).clamp(-1.0, 1.0),
                blue: blue.resolve(ctx).clamp(-1.0, 1.0),
            },
            EffectKind::ChromaKey { color, tolerance, softness } => ResolvedEffect::ChromaKey {
                color: color.resolve(ctx),
                tolerance: tolerance.resolve(ctx).max(0.0),
                softness: softness.resolve(ctx).max(0.0),
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
            EffectKind::Levels { in_black, in_white, gamma, out_black, out_white } => {
                num(in_black);
                num(in_white);
                num(gamma);
                num(out_black);
                num(out_white);
            }
            EffectKind::DropShadow { color, offset_x, offset_y, radius, opacity } => {
                col(color);
                num(offset_x);
                num(offset_y);
                num(radius);
                num(opacity);
            }
            EffectKind::Glow { threshold, radius, intensity } => {
                num(threshold);
                num(radius);
                num(intensity);
            }
            EffectKind::ColorBalance { red, green, blue } => {
                num(red);
                num(green);
                num(blue);
            }
            EffectKind::ChromaKey { color, tolerance, softness } => {
                col(color);
                num(tolerance);
                num(softness);
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
    Levels,
    DropShadow,
    Glow,
    ColorBalance,
    ChromaKey,
}

impl EffectType {
    /// Every kind, in menu order.
    pub const ALL: [EffectType; 9] = [
        EffectType::GaussianBlur,
        EffectType::BrightnessContrast,
        EffectType::HueSaturation,
        EffectType::Tint,
        EffectType::Levels,
        EffectType::DropShadow,
        EffectType::Glow,
        EffectType::ColorBalance,
        EffectType::ChromaKey,
    ];

    pub fn label(self) -> &'static str {
        match self {
            EffectType::GaussianBlur => "Gaussian Blur",
            EffectType::BrightnessContrast => "Brightness & Contrast",
            EffectType::HueSaturation => "Hue / Saturation",
            EffectType::Tint => "Tint",
            EffectType::Levels => "Levels",
            EffectType::DropShadow => "Drop Shadow",
            EffectType::Glow => "Glow",
            EffectType::ColorBalance => "Color Balance",
            EffectType::ChromaKey => "Chroma Key",
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
    Levels { in_black: f64, in_white: f64, gamma: f64, out_black: f64, out_white: f64 },
    DropShadow { color: Color, offset_x: f64, offset_y: f64, radius: f64, opacity: f64 },
    Glow { threshold: f64, radius: f64, intensity: f64 },
    ColorBalance { red: f64, green: f64, blue: f64 },
    ChromaKey { color: Color, tolerance: f64, softness: f64 },
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

    /// A gamma of zero would divide by zero in every backend; it is clamped
    /// here so none of them has to decide what to do about it.
    #[test]
    fn levels_gamma_cannot_be_zero() {
        let e = Effect::new(EffectKind::Levels {
            in_black: Value::constant(0.0),
            in_white: Value::constant(1.0),
            gamma: Value::constant(0.0),
            out_black: Value::constant(0.0),
            out_white: Value::constant(1.0),
        });
        let mut ctx = EvalCtx::at(0.0);
        match e.resolve(&mut ctx) {
            Some(ResolvedEffect::Levels { gamma, .. }) => assert!(gamma > 0.0),
            other => panic!("expected levels, got {other:?}"),
        }
    }

    /// A drop shadow's seed is deliberately *not* neutral — it is the one
    /// effect whose whole content is the thing it adds, so adding it must show
    /// something rather than look broken.
    #[test]
    fn a_drop_shadow_seeds_visible() {
        let mut ctx = EvalCtx::at(0.0);
        match Effect::seed(EffectType::DropShadow).resolve(&mut ctx) {
            Some(ResolvedEffect::DropShadow { offset_x, offset_y, opacity, .. }) => {
                assert!(offset_x != 0.0 || offset_y != 0.0, "an unoffset shadow hides behind");
                assert!(opacity > 0.0);
            }
            other => panic!("expected a shadow, got {other:?}"),
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
