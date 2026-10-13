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
use crate::registry::{NodeCategory, NodeDescriptor};
use crate::socket::SocketType;

/// `(colour, numbers)` of an `EffectKind`, numbers in [`EffectType::params`]
/// order. A macro so one arm list serves `&` and `&mut` alike — match
/// ergonomics picks the borrow from the scrutinee.
macro_rules! effect_fields {
    ($kind:expr) => {
        match $kind {
            EffectKind::GaussianBlur { radius } => (None, vec![radius]),
            EffectKind::BrightnessContrast { brightness, contrast } => {
                (None, vec![brightness, contrast])
            }
            EffectKind::HueSaturation { hue, saturation, lightness } => {
                (None, vec![hue, saturation, lightness])
            }
            EffectKind::Tint { color, amount } => (Some(color), vec![amount]),
            EffectKind::Levels { in_black, in_white, gamma, out_black, out_white } => {
                (None, vec![in_black, in_white, gamma, out_black, out_white])
            }
            EffectKind::DropShadow { color, offset_x, offset_y, radius, opacity } => {
                (Some(color), vec![offset_x, offset_y, radius, opacity])
            }
            EffectKind::Glow { threshold, radius, intensity } => {
                (None, vec![threshold, radius, intensity])
            }
            EffectKind::ColorBalance { red, green, blue } => (None, vec![red, green, blue]),
            EffectKind::ChromaKey { color, tolerance, softness } => {
                (Some(color), vec![tolerance, softness])
            }
            EffectKind::TonalBalance { sh_r, sh_g, sh_b, mid_r, mid_g, mid_b, hi_r, hi_g, hi_b } => {
                (None, vec![sh_r, sh_g, sh_b, mid_r, mid_g, mid_b, hi_r, hi_g, hi_b])
            }
            EffectKind::Curves { shadows, midtones, highlights } => {
                (None, vec![shadows, midtones, highlights])
            }
            EffectKind::ColorKey { color, tolerance, softness } => {
                (Some(color), vec![tolerance, softness])
            }
            EffectKind::LeaveColor { color, amount, tolerance, softness } => {
                (Some(color), vec![amount, tolerance, softness])
            }
            EffectKind::GammaPedestalGain { gamma, pedestal, gain, black_stretch } => {
                (None, vec![gamma, pedestal, gain, black_stretch])
            }
            EffectKind::ChannelMixer { rr, rg, rb, rc, gr, gg, gb, gc, br, bg, bb, bc } => {
                (None, vec![rr, rg, rb, rc, gr, gg, gb, gc, br, bg, bb, bc])
            }
            EffectKind::PhotoFilter { color, density } => {
                (Some(color), vec![density])
            }
            EffectKind::Vibrance { vibrance, saturation } => {
                (None, vec![vibrance, saturation])
            }
            EffectKind::BlackWhite { reds, yellows, greens, cyans, blues, magentas } => {
                (None, vec![reds, yellows, greens, cyans, blues, magentas])
            }
            EffectKind::Exposure { exposure, offset, gamma } => {
                (None, vec![exposure, offset, gamma])
            }
        }
    };
}


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
    /// Shadows / midtones / highlights colour balance, each a signed RGB shift
    /// in `[-1, 1]` weighted by the pixel's luma. (Port of EffectCraft's Color
    /// Balance; distinct from the flat [`EffectKind::ColorBalance`].)
    TonalBalance {
        sh_r: Value<f64>,
        sh_g: Value<f64>,
        sh_b: Value<f64>,
        mid_r: Value<f64>,
        mid_g: Value<f64>,
        mid_b: Value<f64>,
        hi_r: Value<f64>,
        hi_g: Value<f64>,
        hi_b: Value<f64>,
    },
    /// A master tone curve through `(0,0) (.25,shadows) (.5,midtones)
    /// (.75,highlights) (1,1)` - the output at three fixed inputs, so each
    /// point keyframes like any number. Identity is `0.25 / 0.5 / 0.75`.
    Curves { shadows: Value<f64>, midtones: Value<f64>, highlights: Value<f64> },
    /// Knock out pixels whose every channel is within `tolerance` of `color`
    /// (Chebyshev distance, EffectCraft's Color Key); alpha ramps back over
    /// `softness`. Unlike [`EffectKind::ChromaKey`] (Euclidean) it keys a box.
    ColorKey { color: Value<Color>, tolerance: Value<f64>, softness: Value<f64> },
    /// Exposure in stops (gain `2^exposure`), a linear-light `offset`, and `gamma` (EffectCraft Exposure, master group).
    Exposure {
        exposure: Value<f64>,
        offset: Value<f64>,
        gamma: Value<f64>,
    },
    /// Greyscale with per-hue brightness weights as fractions (0.4 = AE's 40%) (EffectCraft Black & White; the optional tint is not ported).
    BlackWhite {
        reds: Value<f64>,
        yellows: Value<f64>,
        greens: Value<f64>,
        cyans: Value<f64>,
        blues: Value<f64>,
        magentas: Value<f64>,
    },
    /// Boost the less-saturated colours more than the already-saturated ones; `saturation` is a flat signed amount (EffectCraft Vibrance).
    Vibrance {
        vibrance: Value<f64>,
        saturation: Value<f64>,
    },
    /// Multiply by `color`, mixed in by `density`, keeping the pixel luminance (EffectCraft Photo Filter with Preserve Luminosity on; no preset menu).
    PhotoFilter {
        color: Value<Color>,
        density: Value<f64>,
    },
    /// Each output channel as a weighted sum of R, G, B plus a constant (EffectCraft Channel Mixer, monochrome off). Params are row-major: rr rg rb rc / gr gg gb gc / br bg bb bc.
    ChannelMixer {
        rr: Value<f64>,
        rg: Value<f64>,
        rb: Value<f64>,
        rc: Value<f64>,
        gr: Value<f64>,
        gg: Value<f64>,
        gb: Value<f64>,
        gc: Value<f64>,
        br: Value<f64>,
        bg: Value<f64>,
        bb: Value<f64>,
        bc: Value<f64>,
    },
    /// `pedestal + (gain - pedestal) * v^gamma` on every channel, with a black-stretch toe (EffectCraft Gamma/Pedestal/Gain, one master set instead of per channel).
    GammaPedestalGain {
        gamma: Value<f64>,
        pedestal: Value<f64>,
        gain: Value<f64>,
        black_stretch: Value<f64>,
    },
    /// Desaturate everything farther than `tolerance` (RGB distance / sqrt 3) from `color`, by `amount`; `softness` feathers the edge (EffectCraft Leave Color, RGB matching).
    LeaveColor {
        color: Value<Color>,
        amount: Value<f64>,
        tolerance: Value<f64>,
        softness: Value<f64>,
    },
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
            EffectType::TonalBalance => {
                let z = || Value::constant(0.0);
                EffectKind::TonalBalance {
                    sh_r: z(),
                    sh_g: z(),
                    sh_b: z(),
                    mid_r: z(),
                    mid_g: z(),
                    mid_b: z(),
                    hi_r: z(),
                    hi_g: z(),
                    hi_b: z(),
                }
            }
            EffectType::Curves => EffectKind::Curves {
                shadows: Value::constant(0.25),
                midtones: Value::constant(0.5),
                highlights: Value::constant(0.75),
            },
            EffectType::ColorKey => EffectKind::ColorKey {
                color: Value::constant(Color::rgb(0.0, 0.0, 1.0)),
                tolerance: Value::constant(0.1),
                softness: Value::constant(0.0),
            },
            EffectType::Exposure => EffectKind::Exposure {
                exposure: Value::constant(0.0),
                offset: Value::constant(0.0),
                gamma: Value::constant(1.0),
            },
            EffectType::BlackWhite => EffectKind::BlackWhite {
                reds: Value::constant(0.4),
                yellows: Value::constant(0.6),
                greens: Value::constant(0.4),
                cyans: Value::constant(0.6),
                blues: Value::constant(0.2),
                magentas: Value::constant(0.8),
            },
            EffectType::Vibrance => EffectKind::Vibrance {
                vibrance: Value::constant(0.0),
                saturation: Value::constant(0.0),
            },
            EffectType::PhotoFilter => EffectKind::PhotoFilter {
                color: Value::constant(Color::rgb(0.93, 0.54, 0.09)),
                density: Value::constant(0.25),
            },
            EffectType::ChannelMixer => EffectKind::ChannelMixer {
                rr: Value::constant(1.0),
                rg: Value::constant(0.0),
                rb: Value::constant(0.0),
                rc: Value::constant(0.0),
                gr: Value::constant(0.0),
                gg: Value::constant(1.0),
                gb: Value::constant(0.0),
                gc: Value::constant(0.0),
                br: Value::constant(0.0),
                bg: Value::constant(0.0),
                bb: Value::constant(1.0),
                bc: Value::constant(0.0),
            },
            EffectType::GammaPedestalGain => EffectKind::GammaPedestalGain {
                gamma: Value::constant(1.0),
                pedestal: Value::constant(0.0),
                gain: Value::constant(1.0),
                black_stretch: Value::constant(0.0),
            },
            EffectType::LeaveColor => EffectKind::LeaveColor {
                color: Value::constant(Color::rgb(1.0, 0.0, 0.0)),
                amount: Value::constant(1.0),
                tolerance: Value::constant(0.15),
                softness: Value::constant(0.0),
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
            EffectKind::TonalBalance { .. } => EffectType::TonalBalance,
            EffectKind::Curves { .. } => EffectType::Curves,
            EffectKind::ColorKey { .. } => EffectType::ColorKey,
            EffectKind::Exposure { .. } => EffectType::Exposure,
            EffectKind::BlackWhite { .. } => EffectType::BlackWhite,
            EffectKind::Vibrance { .. } => EffectType::Vibrance,
            EffectKind::PhotoFilter { .. } => EffectType::PhotoFilter,
            EffectKind::ChannelMixer { .. } => EffectType::ChannelMixer,
            EffectKind::GammaPedestalGain { .. } => EffectType::GammaPedestalGain,
            EffectKind::LeaveColor { .. } => EffectType::LeaveColor,
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
            EffectKind::TonalBalance { sh_r, sh_g, sh_b, mid_r, mid_g, mid_b, hi_r, hi_g, hi_b } => {
                let mut c = |v: &Value<f64>| v.resolve(ctx).clamp(-1.0, 1.0);
                ResolvedEffect::TonalBalance {
                    sh: [c(sh_r), c(sh_g), c(sh_b)],
                    mid: [c(mid_r), c(mid_g), c(mid_b)],
                    hi: [c(hi_r), c(hi_g), c(hi_b)],
                }
            }
            EffectKind::Curves { shadows, midtones, highlights } => ResolvedEffect::Curves {
                ys: [shadows.resolve(ctx), midtones.resolve(ctx), highlights.resolve(ctx)],
            },
            EffectKind::ColorKey { color, tolerance, softness } => ResolvedEffect::ColorKey {
                color: color.resolve(ctx),
                tolerance: tolerance.resolve(ctx).max(0.0),
                softness: softness.resolve(ctx).max(0.0),
            },
            EffectKind::Exposure { exposure, offset, gamma } => ResolvedEffect::Exposure {
                exposure: exposure.resolve(ctx),
                offset: offset.resolve(ctx),
                gamma: gamma.resolve(ctx).max(0.01),
            },
            EffectKind::BlackWhite { reds, yellows, greens, cyans, blues, magentas } => ResolvedEffect::BlackWhite {
                reds: reds.resolve(ctx),
                yellows: yellows.resolve(ctx),
                greens: greens.resolve(ctx),
                cyans: cyans.resolve(ctx),
                blues: blues.resolve(ctx),
                magentas: magentas.resolve(ctx),
            },
            EffectKind::Vibrance { vibrance, saturation } => ResolvedEffect::Vibrance {
                vibrance: vibrance.resolve(ctx).clamp(-1.0, 1.0),
                saturation: saturation.resolve(ctx).clamp(-1.0, 1.0),
            },
            EffectKind::PhotoFilter { color, density } => ResolvedEffect::PhotoFilter {
                color: color.resolve(ctx),
                density: density.resolve(ctx).clamp(0.0, 1.0),
            },
            EffectKind::ChannelMixer { rr, rg, rb, rc, gr, gg, gb, gc, br, bg, bb, bc } => ResolvedEffect::ChannelMixer {
                rr: rr.resolve(ctx),
                rg: rg.resolve(ctx),
                rb: rb.resolve(ctx),
                rc: rc.resolve(ctx),
                gr: gr.resolve(ctx),
                gg: gg.resolve(ctx),
                gb: gb.resolve(ctx),
                gc: gc.resolve(ctx),
                br: br.resolve(ctx),
                bg: bg.resolve(ctx),
                bb: bb.resolve(ctx),
                bc: bc.resolve(ctx),
            },
            EffectKind::GammaPedestalGain { gamma, pedestal, gain, black_stretch } => ResolvedEffect::GammaPedestalGain {
                gamma: gamma.resolve(ctx).max(0.01),
                pedestal: pedestal.resolve(ctx),
                gain: gain.resolve(ctx),
                black_stretch: black_stretch.resolve(ctx).max(0.0),
            },
            EffectKind::LeaveColor { color, amount, tolerance, softness } => ResolvedEffect::LeaveColor {
                color: color.resolve(ctx),
                amount: amount.resolve(ctx).clamp(0.0, 1.0),
                tolerance: tolerance.resolve(ctx).clamp(0.0, 1.0),
                softness: softness.resolve(ctx).clamp(0.0, 1.0),
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
    /// callback. Built on [`effect_fields!`], so migrate and retime see exactly
    /// the parameters the panel and the registry do.
    fn for_each_value(
        &mut self,
        mut num: impl FnMut(&mut Value<f64>),
        mut col: impl FnMut(&mut Value<Color>),
    ) {
        let (color, nums) = effect_fields!(&mut self.kind);
        color.map(&mut col);
        nums.into_iter().for_each(&mut num);
    }

    /// The effect's numeric parameter in `slot` — the slot's meaning is
    /// [`EffectType::params`]`[slot]`.
    pub fn num(&self, slot: usize) -> Option<&Value<f64>> {
        effect_fields!(&self.kind).1.into_iter().nth(slot)
    }

    pub fn num_mut(&mut self, slot: usize) -> Option<&mut Value<f64>> {
        effect_fields!(&mut self.kind).1.into_iter().nth(slot)
    }

    /// The effect's colour parameter, for the kinds that have one.
    pub fn color(&self) -> Option<&Value<Color>> {
        effect_fields!(&self.kind).0
    }

    pub fn color_mut(&mut self) -> Option<&mut Value<Color>> {
        effect_fields!(&mut self.kind).0
    }
}


/// One numeric parameter of an effect type, as the registry and the panel see
/// it. Defaults are not here: they live in [`Effect::seed`], the one place an
/// effect's starting values are written.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EffectParamSpec {
    /// Socket id on the effect's descriptor.
    pub id: &'static str,
    pub label: &'static str,
    /// Drag speed per pixel: coarse for pixel distances, fine for `0..1` amounts.
    pub step: f64,
}

/// A macro rather than a `const fn` so the tables below are struct literals,
/// which `&[...]` promotes to `'static`; a call would not be.
macro_rules! p {
    ($id:expr, $label:expr, $step:expr) => {
        EffectParamSpec { id: $id, label: $label, step: $step }
    };
}

/// The flat discriminant of [`EffectKind`] — one value per effect kind, with no
/// parameters. This is what an "add effect" menu is built from and what a label
/// comes off of.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
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
    TonalBalance,
    Curves,
    ColorKey,
    Exposure,
    BlackWhite,
    Vibrance,
    PhotoFilter,
    ChannelMixer,
    GammaPedestalGain,
    LeaveColor,
}

impl EffectType {
    /// Every kind, in menu order.
    pub const ALL: [EffectType; 19] = [
        EffectType::GaussianBlur,
        EffectType::BrightnessContrast,
        EffectType::HueSaturation,
        EffectType::Tint,
        EffectType::Levels,
        EffectType::DropShadow,
        EffectType::Glow,
        EffectType::ColorBalance,
        EffectType::ChromaKey,
        EffectType::TonalBalance,
        EffectType::Curves,
        EffectType::ColorKey,
        EffectType::Exposure,
        EffectType::BlackWhite,
        EffectType::Vibrance,
        EffectType::PhotoFilter,
        EffectType::ChannelMixer,
        EffectType::GammaPedestalGain,
        EffectType::LeaveColor,
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
            EffectType::TonalBalance => "Color Balance (S/M/H)",
            EffectType::Curves => "Curves",
            EffectType::ColorKey => "Color Key",
            EffectType::Exposure => "Exposure",
            EffectType::BlackWhite => "Black & White",
            EffectType::Vibrance => "Vibrance",
            EffectType::PhotoFilter => "Photo Filter",
            EffectType::ChannelMixer => "Channel Mixer",
            EffectType::GammaPedestalGain => "Gamma / Pedestal / Gain",
            EffectType::LeaveColor => "Leave Color",
        }
    }

    /// The registry id: namespaced like a plugin's, so a built-in and an
    /// `acme.blur` sit side by side without colliding.
    pub fn id(self) -> &'static str {
        match self {
            EffectType::GaussianBlur => "fx.gaussian_blur",
            EffectType::BrightnessContrast => "fx.brightness_contrast",
            EffectType::HueSaturation => "fx.hue_saturation",
            EffectType::Tint => "fx.tint",
            EffectType::Levels => "fx.levels",
            EffectType::DropShadow => "fx.drop_shadow",
            EffectType::Glow => "fx.glow",
            EffectType::ColorBalance => "fx.color_balance",
            EffectType::ChromaKey => "fx.chroma_key",
            EffectType::TonalBalance => "fx.tonal_balance",
            EffectType::Curves => "fx.curves",
            EffectType::ColorKey => "fx.color_key",
            EffectType::Exposure => "fx.exposure",
            EffectType::BlackWhite => "fx.black_white",
            EffectType::Vibrance => "fx.vibrance",
            EffectType::PhotoFilter => "fx.photo_filter",
            EffectType::ChannelMixer => "fx.channel_mixer",
            EffectType::GammaPedestalGain => "fx.gamma_pedestal_gain",
            EffectType::LeaveColor => "fx.leave_color",
        }
    }

    /// The built-in effect a registry id names, or `None` for anything else
    /// (a plugin's, which has no pixel routine yet).
    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|t| t.id() == id)
    }

    /// The numeric parameters, in slot order — what [`Effect::num`] indexes.
    pub fn params(self) -> &'static [EffectParamSpec] {
        match self {
            EffectType::GaussianBlur => &[p!("radius", "Radius", 0.5)],
            EffectType::BrightnessContrast => {
                &[p!("brightness", "Brightness", 0.01), p!("contrast", "Contrast", 0.01)]
            }
            EffectType::HueSaturation => &[
                p!("hue", "Hue", 0.01),
                p!("saturation", "Saturation", 0.01),
                p!("lightness", "Lightness", 0.01),
            ],
            EffectType::Tint => &[p!("amount", "Amount", 0.01)],
            EffectType::Levels => &[
                p!("in_black", "In Black", 0.01),
                p!("in_white", "In White", 0.01),
                p!("gamma", "Gamma", 0.01),
                p!("out_black", "Out Black", 0.01),
                p!("out_white", "Out White", 0.01),
            ],
            EffectType::DropShadow => &[
                p!("offset_x", "Offset X", 0.5),
                p!("offset_y", "Offset Y", 0.5),
                p!("radius", "Softness", 0.5),
                p!("opacity", "Opacity", 0.01),
            ],
            EffectType::Glow => &[
                p!("threshold", "Threshold", 0.01),
                p!("radius", "Radius", 0.5),
                p!("intensity", "Intensity", 0.01),
            ],
            EffectType::ColorBalance => {
                &[p!("red", "Red", 0.01), p!("green", "Green", 0.01), p!("blue", "Blue", 0.01)]
            }
            EffectType::ChromaKey | EffectType::ColorKey => {
                &[p!("tolerance", "Tolerance", 0.01), p!("softness", "Softness", 0.01)]
            }
            EffectType::TonalBalance => &[
                p!("sh_r", "Shadow Red", 0.01),
                p!("sh_g", "Shadow Green", 0.01),
                p!("sh_b", "Shadow Blue", 0.01),
                p!("mid_r", "Midtone Red", 0.01),
                p!("mid_g", "Midtone Green", 0.01),
                p!("mid_b", "Midtone Blue", 0.01),
                p!("hi_r", "Highlight Red", 0.01),
                p!("hi_g", "Highlight Green", 0.01),
                p!("hi_b", "Highlight Blue", 0.01),
            ],
            EffectType::Exposure => &[
                p!("exposure", "Exposure", 0.05),
                p!("offset", "Offset", 0.005),
                p!("gamma", "Gamma Correction", 0.01),
            ],
            EffectType::BlackWhite => &[
                p!("reds", "Reds", 0.01),
                p!("yellows", "Yellows", 0.01),
                p!("greens", "Greens", 0.01),
                p!("cyans", "Cyans", 0.01),
                p!("blues", "Blues", 0.01),
                p!("magentas", "Magentas", 0.01),
            ],
            EffectType::Vibrance => &[
                p!("vibrance", "Vibrance", 0.01),
                p!("saturation", "Saturation", 0.01),
            ],
            EffectType::PhotoFilter => &[
                p!("density", "Density", 0.01),
            ],
            EffectType::ChannelMixer => &[
                p!("rr", "Red-Red", 0.01),
                p!("rg", "Red-Green", 0.01),
                p!("rb", "Red-Blue", 0.01),
                p!("rc", "Red-Const", 0.01),
                p!("gr", "Green-Red", 0.01),
                p!("gg", "Green-Green", 0.01),
                p!("gb", "Green-Blue", 0.01),
                p!("gc", "Green-Const", 0.01),
                p!("br", "Blue-Red", 0.01),
                p!("bg", "Blue-Green", 0.01),
                p!("bb", "Blue-Blue", 0.01),
                p!("bc", "Blue-Const", 0.01),
            ],
            EffectType::GammaPedestalGain => &[
                p!("gamma", "Gamma", 0.01),
                p!("pedestal", "Pedestal", 0.01),
                p!("gain", "Gain", 0.01),
                p!("black_stretch", "Black Stretch", 0.01),
            ],
            EffectType::LeaveColor => &[
                p!("amount", "Amount to Decolor", 0.01),
                p!("tolerance", "Tolerance", 0.01),
                p!("softness", "Edge Softness", 0.01),
            ],
            EffectType::Curves => &[
                p!("shadows", "Shadows (at 0.25)", 0.01),
                p!("midtones", "Midtones (at 0.5)", 0.01),
                p!("highlights", "Highlights (at 0.75)", 0.01),
            ],
        }
    }

    /// This effect as a registry descriptor: a layer in, its parameters as
    /// defaulted inputs (seed values), a layer out — the shape a plugin effect
    /// declares too.
    pub fn descriptor(self) -> NodeDescriptor {
        use crate::expr::ExprValue;
        let seed = Effect::seed(self);
        let mut d = NodeDescriptor::new(self.id(), NodeCategory::Effect, self.label())
            .input("layer", "Layer", SocketType::Layer);
        if let Some(c) = seed.color() {
            d = d.input_def("color", "Color", SocketType::Color, ExprValue::Color(c.resolve(&mut EvalCtx::at(0.0))));
        }
        for (slot, spec) in self.params().iter().enumerate() {
            let v = seed.num(slot).expect("params() and effect_fields! agree").resolve(&mut EvalCtx::at(0.0));
            d = d.input_def(spec.id, spec.label, SocketType::Number, ExprValue::Num(v));
        }
        d.output("layer", "Layer", SocketType::Layer)
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
    TonalBalance { sh: [f64; 3], mid: [f64; 3], hi: [f64; 3] },
    Curves { ys: [f64; 3] },
    ColorKey { color: Color, tolerance: f64, softness: f64 },
    Exposure { exposure: f64, offset: f64, gamma: f64 },
    BlackWhite { reds: f64, yellows: f64, greens: f64, cyans: f64, blues: f64, magentas: f64 },
    Vibrance { vibrance: f64, saturation: f64 },
    PhotoFilter { color: Color, density: f64 },
    ChannelMixer { rr: f64, rg: f64, rb: f64, rc: f64, gr: f64, gg: f64, gb: f64, gc: f64, br: f64, bg: f64, bb: f64, bc: f64 },
    GammaPedestalGain { gamma: f64, pedestal: f64, gain: f64, black_stretch: f64 },
    LeaveColor { color: Color, amount: f64, tolerance: f64, softness: f64 },
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

    /// The parameter table and the stored fields agree for every kind: same
    /// count, and a descriptor socket per parameter. This is what lets the
    /// panel index by slot without a per-kind match.
    #[test]
    fn param_tables_match_the_fields() {
        for ty in EffectType::ALL {
            let e = Effect::seed(ty);
            let n = ty.params().len();
            assert!(e.num(n - 1).is_some() && e.num(n).is_none(), "{ty:?}");
            let d = ty.descriptor();
            for spec in ty.params() {
                assert!(d.find_input(spec.id).is_some(), "{ty:?}.{}", spec.id);
            }
            assert_eq!(EffectType::from_id(ty.id()), Some(ty));
        }
        let reg = crate::registry::NodeRegistry::with_effects();
        assert_eq!(reg.len(), EffectType::ALL.len());
        assert!(reg.contains("fx.exposure") && reg.contains("fx.leave_color"));
    }
}
