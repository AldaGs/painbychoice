//! Pixel effects: the actual per-pixel math of the effect stack, applied to a
//! layer's isolated RGBA image.
//!
//! This is the compositor's raster half. The editor's properties panel and `core` decide
//! *which* effects a layer has and *what their parameters are* on a frame;
//! this module is where those resolved parameters finally touch pixels. It is
//! kept deliberately **pure and backend-free** — it operates on a plain
//! `&mut [u8]` of straight (non-premultiplied) RGBA8, knowing nothing about
//! wgpu, vello or textures — so the arithmetic can be unit-tested against
//! hand-computed pixels rather than by eyeballing a GPU frame, and so the same
//! routines serve whichever application path the renderer uses (a CPU readback
//! today, a WGSL port later; the maths must match either way).
//!
//! It lives in `render`, not in the editor, because **both** rasterizers need
//! it: the editor's readback compositor and the CPU backend that `motion
//! render` and every headless test go through. Two copies of this arithmetic
//! would be two answers to "what does a 40% tint look like", and the one place
//! that must never disagree is the preview and the file.
//!
//! **Straight, not premultiplied**, for the colour adjustments: a tint or a
//! brightness change is defined on a pixel's own colour, independent of its
//! coverage. The blur is the one operation that *must* respect coverage — a
//! blurred edge bleeds colour across the alpha boundary — so it premultiplies
//! internally and converts back, keeping transparent pixels from darkening the
//! result.

use motion_core::{Color as MColor, ResolvedEffect};

/// Apply the **colour-adjustment** effects of a stack to a single straight
/// colour, in order, skipping any blur.
///
/// This is the in-scene fast path. A blur reworks a pixel from its *neighbours*,
/// so it needs the whole rasterized image and belongs to the readback pipeline
/// ([`apply_stack`]); the colour adjustments are per-pixel, so for a layer that
/// is a single vector item — a shape or a text outline, the common case — an
/// effect applied to that item's fill/stroke colour is exactly the effect
/// applied to the layer's pixels, and needs no offscreen target at all. A layer
/// with overlapping items composites them first, so per-item colour is then an
/// approximation; a raster (footage) layer isn't reached this way at all. Both
/// of those, and blur, are what the full-image path is for.
pub fn apply_color_effects(color: MColor, effects: &[ResolvedEffect]) -> MColor {
    let mut rgb = [color.r as f32, color.g as f32, color.b as f32];
    for e in effects {
        rgb = match *e {
            // Needs the image; handled by the readback path, a no-op here.
            ResolvedEffect::GaussianBlur { .. } => rgb,
            ResolvedEffect::BrightnessContrast { brightness, contrast } => {
                brightness_contrast(rgb, brightness, contrast)
            }
            ResolvedEffect::HueSaturation { hue, saturation, lightness } => {
                hue_saturation(rgb, hue, saturation, lightness)
            }
            ResolvedEffect::Tint { color: t, amount } => {
                tint_rgb(rgb, [t.r as f32, t.g as f32, t.b as f32], amount as f32)
            }
            ResolvedEffect::Levels { in_black, in_white, gamma, out_black, out_white } => {
                levels(rgb, in_black, in_white, gamma, out_black, out_white)
            }
            // A shadow is made of the layer's *alpha*, offset — there is no
            // such thing as one pixel's shadow. The readback path draws it.
            ResolvedEffect::DropShadow { .. } => rgb,
            ResolvedEffect::ColorBalance { red, green, blue } => color_balance(rgb, red, green, blue),
            // A glow spreads to neighbours and a key writes alpha; neither is
            // one pixel's colour. Both are the readback path's.
            ResolvedEffect::Glow { .. }
            | ResolvedEffect::ChromaKey { .. }
            | ResolvedEffect::ColorKey { .. }
            | ResolvedEffect::FastBoxBlur { .. }
            | ResolvedEffect::DirectionalBlur { .. }
            | ResolvedEffect::Sharpen { .. }
            | ResolvedEffect::UnsharpMask { .. }
            | ResolvedEffect::CrossBlur { .. }
            | ResolvedEffect::ChannelBlur { .. }
            | ResolvedEffect::BilateralBlur { .. }
            | ResolvedEffect::SmartBlur { .. }
            | ResolvedEffect::FindEdges { .. }
            | ResolvedEffect::Emboss { .. }
            | ResolvedEffect::ColorEmboss { .. }
            | ResolvedEffect::Mosaic { .. }
            | ResolvedEffect::RadialBlur { .. } => rgb,
            ResolvedEffect::TonalBalance { sh, mid, hi } => tonal_balance(rgb, sh, mid, hi),
            ResolvedEffect::Curves { ys } => curves(rgb, ys),
            ResolvedEffect::Exposure { .. } | ResolvedEffect::BlackWhite { .. } | ResolvedEffect::Vibrance { .. } | ResolvedEffect::PhotoFilter { .. } | ResolvedEffect::ChannelMixer { .. } | ResolvedEffect::GammaPedestalGain { .. } | ResolvedEffect::LeaveColor { .. } | ResolvedEffect::Posterize { .. } | ResolvedEffect::Threshold { .. } | ResolvedEffect::ThresholdRgb { .. } => ported_rgb(rgb, e),
        };
    }
    MColor::rgba(
        rgb[0].clamp(0.0, 1.0) as f64,
        rgb[1].clamp(0.0, 1.0) as f64,
        rgb[2].clamp(0.0, 1.0) as f64,
        color.a,
    )
}

/// Whether a stack has any effect that the in-scene colour path can't do on its
/// own — a blur. Such a layer needs the full-image readback pipeline; until that
/// lands, the panel can warn that the effect won't show in the preview.
pub fn needs_readback(effects: &[ResolvedEffect]) -> bool {
    effects.iter().any(|e| {
        matches!(
            e,
            ResolvedEffect::GaussianBlur { .. }
                | ResolvedEffect::DropShadow { .. }
                | ResolvedEffect::Glow { .. }
                | ResolvedEffect::ChromaKey { .. }
                | ResolvedEffect::ColorKey { .. }
                | ResolvedEffect::FastBoxBlur { .. }
                | ResolvedEffect::DirectionalBlur { .. }
                | ResolvedEffect::Sharpen { .. }
                | ResolvedEffect::UnsharpMask { .. }
                | ResolvedEffect::CrossBlur { .. }
                | ResolvedEffect::ChannelBlur { .. }
                | ResolvedEffect::BilateralBlur { .. }
                | ResolvedEffect::SmartBlur { .. }
                | ResolvedEffect::FindEdges { .. }
                | ResolvedEffect::Emboss { .. }
                | ResolvedEffect::ColorEmboss { .. }
                | ResolvedEffect::Mosaic { .. }
                | ResolvedEffect::RadialBlur { .. }
        )
    })
}

/// Apply an ordered effect stack to an RGBA8 image in place.
///
/// `px` is `width * height * 4` bytes of straight RGBA8, row-major. Effects run
/// in order, each seeing the previous one's output — the stack semantics the
/// panel presents top-to-bottom.
///
/// The full-image path, for the readback compositor (blur, footage layers,
/// overlapping content). Not yet wired into the renderer — kept tested so the
/// arithmetic is trustworthy before the GPU plumbing that will feed it lands.
pub fn apply_stack(px: &mut [u8], width: usize, height: usize, effects: &[ResolvedEffect]) {
    for effect in effects {
        apply_one(px, width, height, effect);
    }
}

fn apply_one(px: &mut [u8], width: usize, height: usize, effect: &ResolvedEffect) {
    match *effect {
        ResolvedEffect::GaussianBlur { radius } => gaussian_blur(px, width, height, radius),
        // The colour adjustments are all per-pixel, so they share one loop over
        // the buffer and differ only in the function applied to each pixel's
        // straight RGB.
        ResolvedEffect::BrightnessContrast { brightness, contrast } => {
            map_rgb(px, |c| brightness_contrast(c, brightness, contrast))
        }
        ResolvedEffect::HueSaturation { hue, saturation, lightness } => {
            map_rgb(px, |c| hue_saturation(c, hue, saturation, lightness))
        }
        ResolvedEffect::Tint { color, amount } => {
            let tint = [color.r as f32, color.g as f32, color.b as f32];
            map_rgb(px, |c| tint_rgb(c, tint, amount as f32))
        }
        ResolvedEffect::Levels { in_black, in_white, gamma, out_black, out_white } => {
            map_rgb(px, |c| levels(c, in_black, in_white, gamma, out_black, out_white))
        }
        ResolvedEffect::DropShadow { color, offset_x, offset_y, radius, opacity } => {
            drop_shadow(px, width, height, color, offset_x, offset_y, radius, opacity)
        }
        ResolvedEffect::Glow { threshold, radius, intensity } => {
            glow(px, width, height, threshold, radius, intensity)
        }
        ResolvedEffect::ColorBalance { red, green, blue } => {
            map_rgb(px, |c| color_balance(c, red, green, blue))
        }
        ResolvedEffect::ChromaKey { color, tolerance, softness } => {
            chroma_key(px, color, tolerance, softness)
        }
        ResolvedEffect::TonalBalance { sh, mid, hi } => map_rgb(px, |c| tonal_balance(c, sh, mid, hi)),
        ResolvedEffect::Curves { ys } => map_rgb(px, |c| curves(c, ys)),
        ResolvedEffect::Exposure { .. } | ResolvedEffect::BlackWhite { .. } | ResolvedEffect::Vibrance { .. } | ResolvedEffect::PhotoFilter { .. } | ResolvedEffect::ChannelMixer { .. } | ResolvedEffect::GammaPedestalGain { .. } | ResolvedEffect::LeaveColor { .. } | ResolvedEffect::Posterize { .. } | ResolvedEffect::Threshold { .. } | ResolvedEffect::ThresholdRgb { .. } => map_rgb(px, |c| ported_rgb(c, effect)),
        ResolvedEffect::ColorKey { color, tolerance, softness } => {
            color_key(px, color, tolerance, softness)
        }
        ResolvedEffect::FastBoxBlur { radius, iterations } => fast_box_blur(px, width, height, radius, iterations),
        ResolvedEffect::DirectionalBlur { angle, length } => directional_blur(px, width, height, angle, length),
        ResolvedEffect::Sharpen { amount } => unsharp(px, width, height, amount, 1.0, 0.0),
        ResolvedEffect::UnsharpMask { amount, radius, threshold } => unsharp(px, width, height, amount, radius, threshold),
        ResolvedEffect::CrossBlur { radius_x, radius_y, mode } => cross_blur(px, width, height, radius_x, radius_y, mode),
        ResolvedEffect::ChannelBlur { red, green, blue, alpha } => channel_blur(px, width, height, [red, green, blue, alpha]),
        ResolvedEffect::BilateralBlur { radius, threshold } => edge_preserving(px, width, height, radius, threshold as f32, false),
        ResolvedEffect::SmartBlur { radius, threshold } => edge_preserving(px, width, height, radius, threshold as f32, true),
        ResolvedEffect::FindEdges { blend, invert } => find_edges(px, width, height, blend as f32, invert),
        ResolvedEffect::Emboss { direction, relief, contrast, blend } => emboss(px, width, height, direction, relief, contrast as f32, blend as f32, false),
        ResolvedEffect::ColorEmboss { direction, relief, contrast, blend } => emboss(px, width, height, direction, relief, contrast as f32, blend as f32, true),
        ResolvedEffect::Mosaic { horizontal, vertical } => mosaic(px, width, height, horizontal as usize, vertical as usize),
        ResolvedEffect::RadialBlur { amount, center, zoom } => radial_blur(px, width, height, amount, center, zoom),
    }
}

// The three effects below are ported from EffectCraft (MIT OR Apache-2.0,
// Copyright (c) 2026 ArtCraft Team and the EffectCraft contributors); see NOTICE.

/// Luma-weighted shadow / midtone / highlight RGB shifts (EffectCraft's
/// `color_balance`, minus its Preserve Luminosity option).
fn tonal_balance(c: [f32; 3], sh: [f64; 3], mid: [f64; 3], hi: [f64; 3]) -> [f32; 3] {
    let l = (0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]).clamp(0.0, 1.0);
    let ws = (1.0 - l * 2.0).clamp(0.0, 1.0);
    let wh = (l * 2.0 - 1.0).clamp(0.0, 1.0);
    let wm = 1.0 - ws - wh;
    std::array::from_fn(|i| {
        (c[i] + (sh[i] as f32 * ws + mid[i] as f32 * wm + hi[i] as f32 * wh) * 0.5).clamp(0.0, 1.0)
    })
}

/// Master tone curve through `(0,0) (.25,y0) (.5,y1) (.75,y2) (1,1)`, monotone
/// cubic Hermite (Fritsch-Carlson) as in EffectCraft's `Curve`.
fn curves(c: [f32; 3], ys: [f64; 3]) -> [f32; 3] {
    let xs = [0.0f32, 0.25, 0.5, 0.75, 1.0];
    let y = [0.0f32, ys[0] as f32, ys[1] as f32, ys[2] as f32, 1.0];
    let d: [f32; 4] = std::array::from_fn(|k| (y[k + 1] - y[k]) / (xs[k + 1] - xs[k]));
    let mut m = [d[0], 0.0, 0.0, 0.0, d[3]];
    for k in 1..4 {
        m[k] = if d[k - 1] * d[k] <= 0.0 { 0.0 } else { (d[k - 1] + d[k]) * 0.5 };
    }
    for k in 0..4 {
        if d[k].abs() < 1e-12 {
            m[k] = 0.0;
            m[k + 1] = 0.0;
        } else {
            let (a, b) = (m[k] / d[k], m[k + 1] / d[k]);
            let s = a * a + b * b;
            if s > 9.0 {
                let t = 3.0 / s.sqrt();
                m[k] = t * a * d[k];
                m[k + 1] = t * b * d[k];
            }
        }
    }
    c.map(|v| {
        let v = v.clamp(0.0, 1.0);
        let k = ((v * 4.0) as usize).min(3);
        let t = (v - xs[k]) / 0.25;
        let (t2, t3) = (t * t, t * t * t);
        (2.0 * t3 - 3.0 * t2 + 1.0) * y[k]
            + (t3 - 2.0 * t2 + t) * 0.25 * m[k]
            + (-2.0 * t3 + 3.0 * t2) * y[k + 1]
            + (t3 - t2) * 0.25 * m[k + 1]
    })
}

/// Zero alpha where every channel is within `tolerance` of the key colour
/// (Chebyshev, EffectCraft's Color Key); `softness` ramps it back. Edge
/// thin/feather (spatial matte ops) are not ported.
fn color_key(px: &mut [u8], key: MColor, tolerance: f64, softness: f64) {
    let k = [key.r as f32, key.g as f32, key.b as f32];
    let (tol, soft) = (tolerance as f32, softness as f32);
    for p in px.chunks_exact_mut(4) {
        let d = (0..3).map(|i| (p[i] as f32 / 255.0 - k[i]).abs()).fold(0.0, f32::max);
        let keep = if d <= tol {
            0.0
        } else if soft > 0.0 {
            ((d - tol) / soft).min(1.0)
        } else {
            1.0
        };
        p[3] = (p[3] as f32 * keep).round() as u8;
    }
}

fn luma(c: [f32; 3]) -> f32 {
    0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]
}

fn srgb_to_linear(v: f32) -> f32 {
    if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
}

fn linear_to_srgb(v: f32) -> f32 {
    if v <= 0.0031308 { v * 12.92 } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 }
}

/// `|v|^p` keeping `v`'s sign, as EffectCraft's Exposure does.
fn mirror_pow(v: f32, p: f32) -> f32 {
    v.abs().powf(p).copysign(v)
}

/// The per-pixel effects ported from EffectCraft's `ec.color.*` set, in one
/// match so the in-scene colour path and the image path share every formula.
/// Straight RGB in, straight RGB out; other variants pass through.
fn ported_rgb(c: [f32; 3], e: &ResolvedEffect) -> [f32; 3] {
    match *e {
        ResolvedEffect::Exposure { exposure, offset, gamma } => c.map(|v| {
            let o = mirror_pow(srgb_to_linear(v.max(0.0)) * 2f32.powf(exposure as f32) + offset as f32, 1.0 / gamma as f32);
            linear_to_srgb(o.abs()).copysign(o)
        }),
        ResolvedEffect::BlackWhite { reds, yellows, greens, cyans, blues, magentas } => {
            let w = [reds, yellows, greens, cyans, blues, magentas].map(|v| v as f32);
            let (h, s, _) = rgb_to_hsl(c);
            // Interpolate the six hue weights around the colour wheel.
            let hp = h * 6.0;
            let i = hp.floor() as usize % 6;
            let wt = w[i] + (w[(i + 1) % 6] - w[i]) * (hp - hp.floor());
            [(luma(c) + (wt - 0.5) * s * 0.6).max(0.0); 3]
        }
        ResolvedEffect::Vibrance { vibrance, saturation } => {
            let (h, s, l) = rgb_to_hsl(c);
            let s = (s * (1.0 + saturation as f32) + vibrance as f32 * (1.0 - s) * s.min(0.5)).clamp(0.0, 1.0);
            hsl_to_rgb(h, s, l)
        }
        ResolvedEffect::PhotoFilter { color, density } => {
            let k = [color.r as f32, color.g as f32, color.b as f32];
            let o: [f32; 3] = std::array::from_fn(|i| c[i] + (c[i] * k[i] - c[i]) * density as f32);
            let r = luma(c) / luma(o).max(1e-6);
            o.map(|v| v * r)
        }
        ResolvedEffect::ChannelMixer { rr, rg, rb, rc, gr, gg, gb, gc, br, bg, bb, bc } => {
            let row = |a: f64, b: f64, d: f64, k: f64| (c[0] as f64 * a + c[1] as f64 * b + c[2] as f64 * d + k).max(0.0) as f32;
            [row(rr, rg, rb, rc), row(gr, gg, gb, gc), row(br, bg, bb, bc)]
        }
        ResolvedEffect::GammaPedestalGain { gamma, pedestal, gain, black_stretch } => c.map(|v| {
            // Black stretch lifts the low values (a toe that fades out by mid grey).
            let v = if black_stretch > 0.0 && v > 0.0 && v < 1.0 {
                v + black_stretch as f32 * 0.25 * v * (1.0 - v).powi(4)
            } else {
                v
            };
            (pedestal as f32 + (gain - pedestal) as f32 * v.max(0.0).powf(gamma as f32)).max(0.0)
        }),
        ResolvedEffect::LeaveColor { color, amount, tolerance, softness } => {
            let k = [color.r as f32, color.g as f32, color.b as f32];
            let d = (0..3).map(|i| (c[i] - k[i]).powi(2)).sum::<f32>().sqrt() / 3f32.sqrt();
            let (tol, soft) = (tolerance as f32, softness as f32);
            let keep = if tol >= 1.0 || d <= tol {
                1.0
            } else if soft <= 1e-6 {
                0.0
            } else {
                (1.0 - (d - tol) / soft).clamp(0.0, 1.0)
            };
            let g = luma(c);
            c.map(|v| v + (g - v) * amount as f32 * (1.0 - keep))
        }
        ResolvedEffect::Posterize { level } => {
            let lv = level as f32 - 1.0;
            c.map(|v| (v.clamp(0.0, 1.0) * lv).round() / lv)
        }
        ResolvedEffect::Threshold { level } => [if luma(c) >= level as f32 { 1.0 } else { 0.0 }; 3],
        ResolvedEffect::ThresholdRgb { red, green, blue, blend } => {
            let t = [red, green, blue];
            std::array::from_fn(|i| {
                let o = if c[i] >= t[i] as f32 { 1.0 } else { 0.0 };
                o + (c[i] - o) * blend as f32
            })
        }
        _ => c,
    }
}

fn color_balance(c: [f32; 3], red: f64, green: f64, blue: f64) -> [f32; 3] {
    [c[0] + red as f32, c[1] + green as f32, c[2] + blue as f32].map(|v| v.clamp(0.0, 1.0))
}

/// Blur the pixels brighter than `threshold` and add them back over the image.
///
/// The add happens in premultiplied terms, so the halo spilling onto
/// transparent canvas carries its own alpha rather than tinting nothing.
fn glow(px: &mut [u8], width: usize, height: usize, threshold: f64, radius: f64, intensity: f64) {
    let t = threshold as f32;
    let mut bright = px.to_vec();
    for p in bright.chunks_exact_mut(4) {
        let luma = (0.2126 * p[0] as f32 + 0.7152 * p[1] as f32 + 0.0722 * p[2] as f32) / 255.0;
        if luma < t {
            p[3] = 0;
        }
    }
    gaussian_blur(&mut bright, width, height, radius);
    let k = intensity as f32;
    for (out, g) in px.chunks_exact_mut(4).zip(bright.chunks_exact(4)) {
        let ga = g[3] as f32 / 255.0 * k;
        if ga <= 0.0 {
            continue;
        }
        let oa = out[3] as f32 / 255.0;
        let a = (oa + ga).min(1.0);
        for ch in 0..3 {
            let sum = out[ch] as f32 / 255.0 * oa + g[ch] as f32 / 255.0 * ga;
            out[ch] = to_u8((sum / a).min(1.0));
        }
        out[3] = to_u8(a);
    }
}

/// Zero the alpha of pixels within `tolerance` (RGB distance) of `key`, ramping
/// back to untouched over `softness`. Colour is left alone — spill suppression
/// is a separate job.
fn chroma_key(px: &mut [u8], key: MColor, tolerance: f64, softness: f64) {
    let k = [key.r as f32, key.g as f32, key.b as f32];
    let (tol, soft) = (tolerance as f32, softness as f32);
    for p in px.chunks_exact_mut(4) {
        let d = (0..3).map(|i| (p[i] as f32 / 255.0 - k[i]).powi(2)).sum::<f32>().sqrt();
        let keep = if d <= tol {
            0.0
        } else if soft > 0.0 {
            ((d - tol) / soft).min(1.0)
        } else {
            1.0
        };
        p[3] = (p[3] as f32 * keep).round() as u8;
    }
}

/// Run a per-pixel RGB function over the buffer, leaving alpha untouched.
///
/// The colour is passed and returned as straight (non-premultiplied) linear-in
/// `[0, 1]` — "linear" here only meaning the 0..255 encoding scaled down, not a
/// gamma decode, matching the rest of the engine's naive colour handling.
fn map_rgb(px: &mut [u8], mut f: impl FnMut([f32; 3]) -> [f32; 3]) {
    for chunk in px.chunks_exact_mut(4) {
        let out = f([
            chunk[0] as f32 / 255.0,
            chunk[1] as f32 / 255.0,
            chunk[2] as f32 / 255.0,
        ]);
        chunk[0] = to_u8(out[0]);
        chunk[1] = to_u8(out[1]);
        chunk[2] = to_u8(out[2]);
    }
}

fn to_u8(v: f32) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0).round() as u8
}

/// Brightness is a signed offset; contrast pivots about mid-grey. Both are
/// centred on `0` (no change), so an effect added and left at its defaults is an
/// identity pass.
fn brightness_contrast(c: [f32; 3], brightness: f64, contrast: f64) -> [f32; 3] {
    let b = brightness as f32;
    let k = 1.0 + contrast as f32;
    c.map(|v| (v - 0.5) * k + 0.5 + b)
}

/// Mix each channel toward the tint colour by `amount` — a straight linear
/// interpolation, so `0` is the original and `1` is the flat tint.
fn tint_rgb(c: [f32; 3], tint: [f32; 3], amount: f32) -> [f32; 3] {
    let a = amount.clamp(0.0, 1.0);
    [
        c[0] + (tint[0] - c[0]) * a,
        c[1] + (tint[1] - c[1]) * a,
        c[2] + (tint[2] - c[2]) * a,
    ]
}

/// Remap one channel's tonal range: `in_black`..`in_white` in, `gamma` bending
/// the middle, `out_black`..`out_white` out.
///
/// A degenerate input range (white at or below black) collapses to a hard
/// threshold rather than dividing by zero — which is what dragging the two
/// handles past each other visibly means, so the edge case and the intent
/// agree.
fn levels(
    c: [f32; 3],
    in_black: f64,
    in_white: f64,
    gamma: f64,
    out_black: f64,
    out_white: f64,
) -> [f32; 3] {
    let (ib, iw) = (in_black as f32, in_white as f32);
    let (ob, ow) = (out_black as f32, out_white as f32);
    let inv_gamma = 1.0 / gamma.max(1e-6) as f32;
    let span = iw - ib;
    c.map(|v| {
        let t = if span.abs() < 1e-6 {
            if v >= iw { 1.0 } else { 0.0 }
        } else {
            ((v - ib) / span).clamp(0.0, 1.0)
        };
        ob + t.powf(inv_gamma) * (ow - ob)
    })
}

/// Paint a blurred, offset copy of the image's own alpha underneath it.
///
/// The shadow is built as a full RGBA image — the shadow colour everywhere,
/// carrying the *shifted* alpha — and then blurred with the same kernel a
/// Gaussian Blur effect uses, so a shadow's softness and a blur's mean the same
/// thing. The composite is `source over shadow`, done in premultiplied f32 and
/// converted back at the end, which is the only way the semi-transparent edge
/// of the artwork ends up over the shadow rather than mixed into it.
///
/// A positive offset moves the shadow **right and down**, matching the y-down
/// pixel grid every backend here writes into.
#[allow(clippy::too_many_arguments)]
fn drop_shadow(
    px: &mut [u8],
    width: usize,
    height: usize,
    color: MColor,
    offset_x: f64,
    offset_y: f64,
    radius: f64,
    opacity: f64,
) {
    if width == 0 || height == 0 {
        return;
    }
    let (dx, dy) = (offset_x.round() as isize, offset_y.round() as isize);
    let rgb = [to_u8(color.r as f32), to_u8(color.g as f32), to_u8(color.b as f32)];

    // The shadow's own image: sampled from where the artwork *was*, so the
    // copy lands where the offset puts it.
    let mut shadow = vec![0u8; px.len()];
    for y in 0..height as isize {
        for x in 0..width as isize {
            let (sx, sy) = (x - dx, y - dy);
            if sx < 0 || sy < 0 || sx >= width as isize || sy >= height as isize {
                continue;
            }
            let src = ((sy as usize) * width + sx as usize) * 4;
            let dst = ((y as usize) * width + x as usize) * 4;
            shadow[dst] = rgb[0];
            shadow[dst + 1] = rgb[1];
            shadow[dst + 2] = rgb[2];
            shadow[dst + 3] = px[src + 3];
        }
    }
    gaussian_blur(&mut shadow, width, height, radius);

    let strength = opacity.clamp(0.0, 1.0) as f32;
    for (out, sh) in px.chunks_exact_mut(4).zip(shadow.chunks_exact(4)) {
        let sa = (sh[3] as f32 / 255.0) * strength;
        let oa = out[3] as f32 / 255.0;
        // src over shadow, in premultiplied terms.
        let a = oa + sa * (1.0 - oa);
        if a <= 0.0 {
            out.fill(0);
            continue;
        }
        for ch in 0..3 {
            let s = out[ch] as f32 / 255.0 * oa;
            let d = sh[ch] as f32 / 255.0 * sa * (1.0 - oa);
            out[ch] = to_u8((s + d) / a);
        }
        out[3] = to_u8(a);
    }
}

/// Rotate hue (degrees) and shift saturation / lightness (signed amounts) via
/// an HSL round-trip.
fn hue_saturation(c: [f32; 3], hue: f64, sat: f64, light: f64) -> [f32; 3] {
    let (mut h, mut s, mut l) = rgb_to_hsl(c);
    h = (h + (hue as f32) / 360.0).rem_euclid(1.0);
    s = (s + sat as f32).clamp(0.0, 1.0);
    l = (l + light as f32).clamp(0.0, 1.0);
    hsl_to_rgb(h, s, l)
}

fn rgb_to_hsl(c: [f32; 3]) -> (f32, f32, f32) {
    let (r, g, b) = (c[0], c[1], c[2]);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) / 2.0;
    let d = max - min;
    if d.abs() < 1e-6 {
        return (0.0, 0.0, l); // grey: hue undefined, saturation zero
    }
    let s = d / (1.0 - (2.0 * l - 1.0).abs());
    let h = if max == r {
        ((g - b) / d).rem_euclid(6.0)
    } else if max == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    } / 6.0;
    (h, s, l)
}

fn hsl_to_rgb(h: f32, s: f32, l: f32) -> [f32; 3] {
    if s.abs() < 1e-6 {
        return [l, l, l];
    }
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let x = c * (1.0 - ((h * 6.0).rem_euclid(2.0) - 1.0).abs());
    let m = l - c / 2.0;
    let (r, g, b) = match (h * 6.0) as i32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    [r + m, g + m, b + m]
}

/// Separable Gaussian blur. `radius` is the standard deviation in pixels; a
/// radius below half a pixel is a no-op (the kernel would be a single tap).
///
/// Colour is **premultiplied** for the duration of the blur so that a
/// transparent pixel contributes no colour to its neighbours — otherwise a
/// blurred edge picks up whatever stale RGB sat under the transparency. Two 1D
/// passes (horizontal then vertical) give the 2D Gaussian at O(n·r) instead of
/// O(n·r²).
fn gaussian_blur(px: &mut [u8], width: usize, height: usize, radius: f64) {
    if radius < 0.5 {
        return;
    }
    let m = (radius * 3.0).ceil() as usize + 1;
    with_premul(px, width, height, m, |buf, w, h| gauss_buf(buf, w, h, radius as f32));
}

/// Run `f` on a premultiplied f32 copy of the layer's visible pixels grown by
/// `margin`, and write the result back as straight RGBA8. Only that box can
/// change: a small shape on a big canvas blurs a small box, not the whole
/// frame. The one place the spatial effects share their premultiply round-trip.
fn with_premul(
    px: &mut [u8],
    width: usize,
    height: usize,
    margin: usize,
    f: impl FnOnce(Vec<[f32; 4]>, usize, usize) -> Vec<[f32; 4]>,
) {
    if width == 0 || height == 0 {
        return;
    }
    let Some((x0, y0, x1, y1)) = alpha_bounds(px, width, height) else { return };
    let (x0, y0) = (x0.saturating_sub(margin), y0.saturating_sub(margin));
    let (x1, y1) = ((x1 + margin).min(width), (y1 + margin).min(height));
    let (w, h) = (x1 - x0, y1 - y0);

    // Work in premultiplied f32 so alpha weighting is correct across edges.
    let mut buf: Vec<[f32; 4]> = Vec::with_capacity(w * h);
    for y in y0..y1 {
        for c in px[(y * width + x0) * 4..(y * width + x1) * 4].chunks_exact(4) {
            let a = c[3] as f32 / 255.0;
            buf.push([
                (c[0] as f32 / 255.0) * a,
                (c[1] as f32 / 255.0) * a,
                (c[2] as f32 / 255.0) * a,
                a,
            ]);
        }
    }
    let buf = f(buf, w, h);

    for (y, row) in (y0..y1).zip(buf.chunks_exact(w)) {
        let dst = &mut px[(y * width + x0) * 4..(y * width + x1) * 4];
        for (out, p) in dst.chunks_exact_mut(4).zip(row) {
            let a = p[3];
            let inv = if a > 1e-6 { 1.0 / a } else { 0.0 };
            out[0] = to_u8(p[0] * inv);
            out[1] = to_u8(p[1] * inv);
            out[2] = to_u8(p[2] * inv);
            out[3] = to_u8(a);
        }
    }
}

/// One box blur pass of radius `rx` along x and `ry` along y (0 skips that axis).
fn box_pass(buf: Vec<[f32; 4]>, w: usize, h: usize, rx: usize, ry: usize) -> Vec<[f32; 4]> {
    let mut a = buf;
    let mut b = vec![[0.0f32; 4]; a.len()];
    for (r, horizontal) in [(rx, true), (ry, false)] {
        if r > 0 {
            box_axis(&a, &mut b, w, h, r, horizontal);
            std::mem::swap(&mut a, &mut b);
        }
    }
    a
}

/// Gaussian of standard deviation `sigma` as three box passes (running sums).
fn gauss_buf(mut buf: Vec<[f32; 4]>, w: usize, h: usize, sigma: f32) -> Vec<[f32; 4]> {
    for r in box_radii(sigma) {
        buf = box_pass(buf, w, h, r, r);
    }
    buf
}

/// Build a `w` x `h` buffer from `f(x, y)`, bands of rows on separate threads.
fn par_pixels(w: usize, h: usize, f: impl Fn(usize, usize) -> [f32; 4] + Sync) -> Vec<[f32; 4]> {
    let mut out = vec![[0.0f32; 4]; w * h];
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get()).min(h.max(1));
    let band = h.div_ceil(threads).max(1);
    std::thread::scope(|scope| {
        for (b, chunk) in out.chunks_mut(band * w).enumerate() {
            let f = &f;
            scope.spawn(move || {
                for (i, o) in chunk.iter_mut().enumerate() {
                    *o = f(i % w, b * band + i / w);
                }
            });
        }
    });
    out
}

// The blur-category effects below are ported from EffectCraft (MIT OR
// Apache-2.0, Copyright (c) 2026 ArtCraft Team and the EffectCraft
// contributors); see NOTICE. Each keeps the source's separable / running-sum
// approach, so cost does not grow with the radius squared.

fn lerp4(fx: [f32; 4], orig: [f32; 4], k: f32) -> [f32; 4] {
    std::array::from_fn(|c| fx[c] + (orig[c] - fx[c]) * k)
}

fn find_edges(px: &mut [u8], width: usize, height: usize, blend: f32, invert: bool) {
    with_premul(px, width, height, 1, |buf, w, h| {
        let l = |x: usize, y: usize, dx: i64, dy: i64| {
            let p = buf[(y as i64 + dy).clamp(0, h as i64 - 1) as usize * w + (x as i64 + dx).clamp(0, w as i64 - 1) as usize];
            luma([p[0], p[1], p[2]])
        };
        par_pixels(w, h, |x, y| {
            let gx = l(x, y, 1, -1) + 2.0 * l(x, y, 1, 0) + l(x, y, 1, 1) - l(x, y, -1, -1) - 2.0 * l(x, y, -1, 0) - l(x, y, -1, 1);
            let gy = l(x, y, -1, 1) + 2.0 * l(x, y, 0, 1) + l(x, y, 1, 1) - l(x, y, -1, -1) - 2.0 * l(x, y, 0, -1) - l(x, y, 1, -1);
            let m = (gx * gx + gy * gy).sqrt().min(1.0);
            let v = if invert { m } else { 1.0 - m };
            let a = buf[y * w + x][3];
            lerp4([v * a, v * a, v * a, a], buf[y * w + x], blend)
        })
    });
}

/// Relief from the difference of two bilinear samples `relief` px either side
/// along `direction`; `colour` adds it to the pixel's own colour instead of grey.
fn emboss(px: &mut [u8], width: usize, height: usize, direction: f64, relief: f64, contrast: f32, blend: f32, colour: bool) {
    let (dx, dy) = (direction.to_radians().cos() * relief, -direction.to_radians().sin() * relief);
    with_premul(px, width, height, relief.ceil() as usize + 1, |buf, w, h| {
        let at = |x: i64, y: i64| buf[y.clamp(0, h as i64 - 1) as usize * w + x.clamp(0, w as i64 - 1) as usize];
        // Bilinear sample at pixel-centre coordinates (sx, sy).
        let sample = |sx: f64, sy: f64| {
            let (fx, fy) = (sx - 0.5, sy - 0.5);
            let (x0, y0) = (fx.floor(), fy.floor());
            let (tx, ty) = ((fx - x0) as f32, (fy - y0) as f32);
            let (x0, y0) = (x0 as i64, y0 as i64);
            let top = lerp4(at(x0, y0), at(x0 + 1, y0), tx);
            lerp4(top, lerp4(at(x0, y0 + 1), at(x0 + 1, y0 + 1), tx), ty)
        };
        let unpre = |p: [f32; 4]| if p[3] > 1e-6 { [p[0] / p[3], p[1] / p[3], p[2] / p[3]] } else { [0.0; 3] };
        par_pixels(w, h, |x, y| {
            let o = buf[y * w + x];
            let (cx, cy) = (x as f64 + 0.5, y as f64 + 0.5);
            let (a, b) = (sample(cx + dx, cy + dy), sample(cx - dx, cy - dy));
            let fx = if colour {
                if o[3] <= 0.0 {
                    return o;
                }
                let (c, pa, pb) = (unpre(o), unpre(a), unpre(b));
                let e: [f32; 3] = std::array::from_fn(|i| (c[i] + (pa[i] - pb[i]) * contrast).max(0.0));
                [e[0] * o[3], e[1] * o[3], e[2] * o[3], o[3]]
            } else {
                let v = (0.5 + (luma([a[0], a[1], a[2]]) - luma([b[0], b[1], b[2]])) * contrast).clamp(0.0, 1.0);
                [v * o[3], v * o[3], v * o[3], o[3]]
            };
            lerp4(fx, o, blend)
        })
    });
}

/// `hb` x `vb` blocks across the layer's visible bounds, each the mean of its pixels.
fn mosaic(px: &mut [u8], width: usize, height: usize, hb: usize, vb: usize) {
    // Margin 0: tiles follow the visible bounds, not the whole canvas.
    with_premul(px, width, height, 0, |mut buf, w, h| {
        let (cw, ch) = ((w as f64 / hb as f64).max(1.0), (h as f64 / vb as f64).max(1.0));
        let (nx, ny) = ((w as f64 / cw).ceil() as usize, (h as f64 / ch).ceil() as usize);
        let tile = |x: usize, y: usize| ((y as f64 / ch) as usize).min(ny - 1) * nx + ((x as f64 / cw) as usize).min(nx - 1);
        let mut sums = vec![([0.0f64; 4], 0.0f64); nx * ny];
        for (i, p) in buf.iter().enumerate() {
            let s = &mut sums[tile(i % w, i / w)];
            for c in 0..4 {
                s.0[c] += p[c] as f64;
            }
            s.1 += 1.0;
        }
        for (i, p) in buf.iter_mut().enumerate() {
            let (s, n) = sums[tile(i % w, i / w)];
            *p = std::array::from_fn(|c| (s[c] / n.max(1.0)) as f32);
        }
        buf
    });
}

/// Spin (`amount` degrees) or zoom (`amount` percent) blur: 32 bilinear samples along an arc /
/// ray through each pixel about `center`. `center` is in buffer pixels, which are canvas pixels
/// (the layer's isolated target is the whole canvas), origin top-left. Margin = whole canvas so
/// the crop `with_premul` makes starts at (0,0) and `center` needs no offset.
fn radial_blur(px: &mut [u8], width: usize, height: usize, amount: f64, center: (f64, f64), zoom: bool) {
    let amount = if zoom { amount / 100.0 } else { amount };
    if amount.abs() < 1e-6 {
        return;
    }
    with_premul(px, width, height, width.max(height), |buf, w, h| {
        let at = |x: i64, y: i64| {
            if x < 0 || y < 0 || x >= w as i64 || y >= h as i64 { [0.0; 4] } else { buf[y as usize * w + x as usize] }
        };
        par_pixels(w, h, |x, y| {
            let (vx, vy) = (x as f64 + 0.5 - center.0, y as f64 + 0.5 - center.1);
            let mut acc = [0.0f32; 4];
            for i in 0..32 {
                let t = i as f64 / 31.0 - 0.5;
                let (sx, sy) = if zoom {
                    let k = 1.0 + t * amount;
                    (center.0 + vx * k, center.1 + vy * k)
                } else {
                    let (s, c) = (t * amount).to_radians().sin_cos();
                    (center.0 + vx * c - vy * s, center.1 + vx * s + vy * c)
                };
                let (fx, fy) = (sx - 0.5, sy - 0.5);
                let (x0, y0) = (fx.floor(), fy.floor());
                let (tx, ty) = ((fx - x0) as f32, (fy - y0) as f32);
                let (x0, y0) = (x0 as i64, y0 as i64);
                let top = lerp4(at(x0, y0), at(x0 + 1, y0), tx);
                let p = lerp4(top, lerp4(at(x0, y0 + 1), at(x0 + 1, y0 + 1), tx), ty);
                for c in 0..4 {
                    acc[c] += p[c];
                }
            }
            acc.map(|v| v / 32.0)
        })
    });
}

fn fast_box_blur(px: &mut [u8], width: usize, height: usize, radius: f64, iterations: f64) {
    let (r, it) = (radius.round() as usize, iterations as usize);
    if r == 0 {
        return;
    }
    with_premul(px, width, height, r * it + 1, |mut buf, w, h| {
        for _ in 0..it {
            buf = box_pass(buf, w, h, r, r);
        }
        buf
    });
}

fn cross_blur(px: &mut [u8], width: usize, height: usize, radius_x: f64, radius_y: f64, mode: usize) {
    let (rx, ry) = (radius_x.round() as usize, radius_y.round() as usize);
    if rx == 0 && ry == 0 {
        return;
    }
    with_premul(px, width, height, rx.max(ry) * 3 + 1, |buf, w, h| {
        let (mut hz, mut vt) = (buf.clone(), buf);
        for _ in 0..3 {
            hz = box_pass(hz, w, h, rx, 0);
            vt = box_pass(vt, w, h, 0, ry);
        }
        // Transfer mode: 0 Blend (mean), 1 Add, 2 Screen, 3 Lighten; alpha clamped like colour.
        hz.iter()
            .zip(&vt)
            .map(|(a, b)| {
                std::array::from_fn(|c| {
                    let v = match mode {
                        1 => a[c] + b[c],
                        2 => a[c] + b[c] - a[c] * b[c],
                        3 => a[c].max(b[c]),
                        _ => (a[c] + b[c]) * 0.5,
                    };
                    v.clamp(0.0, 1.0)
                })
            })
            .collect()
    });
}

/// Mean of bilinear samples along a line `length` pixels long through each
/// pixel, `angle` degrees clockwise from up.
fn directional_blur(px: &mut [u8], width: usize, height: usize, angle: f64, length: f64) {
    if length < 0.5 {
        return;
    }
    let (dx, dy) = (angle.to_radians().sin(), -angle.to_radians().cos());
    let n = (length.ceil() as usize * 2 + 1).clamp(3, 257);
    with_premul(px, width, height, length.ceil() as usize + 1, |buf, w, h| {
        let at = |x: isize, y: isize| buf[y.clamp(0, h as isize - 1) as usize * w + x.clamp(0, w as isize - 1) as usize];
        par_pixels(w, h, |x, y| {
            let mut acc = [0.0f32; 4];
            for i in 0..n {
                let t = (i as f64 / (n - 1) as f64 - 0.5) * length;
                let (sx, sy) = (x as f64 + dx * t, y as f64 + dy * t);
                let (x0, y0) = (sx.floor(), sy.floor());
                let (fx, fy) = ((sx - x0) as f32, (sy - y0) as f32);
                let (x0, y0) = (x0 as isize, y0 as isize);
                let (a, b, c, d) = (at(x0, y0), at(x0 + 1, y0), at(x0, y0 + 1), at(x0 + 1, y0 + 1));
                for ch in 0..4 {
                    acc[ch] += (a[ch] * (1.0 - fx) + b[ch] * fx) * (1.0 - fy) + (c[ch] * (1.0 - fx) + d[ch] * fx) * fy;
                }
            }
            acc.map(|v| v / n as f32)
        })
    });
}

/// Add back `amount` of the difference between the image and a `sigma` blur of
/// it, wherever that difference is at least `threshold` (fraction of full scale).
fn unsharp(px: &mut [u8], width: usize, height: usize, amount: f64, sigma: f64, threshold: f64) {
    with_premul(px, width, height, (sigma * 3.0).ceil() as usize + 1, |mut buf, w, h| {
        let blurred = gauss_buf(buf.clone(), w, h, sigma as f32);
        for (o, bl) in buf.iter_mut().zip(&blurred) {
            for c in 0..3 {
                let d = o[c] - bl[c];
                if d.abs() >= threshold as f32 {
                    o[c] = (o[c] + d * amount as f32).max(0.0);
                }
            }
        }
        buf
    });
}

/// Per-channel Gaussian (sigma = half the blurriness). Each colour channel is
/// blurred together with alpha at its own sigma and divided back to straight,
/// then re-premultiplied by the alpha blurred at the alpha sigma.
fn channel_blur(px: &mut [u8], width: usize, height: usize, blurriness: [f64; 4]) {
    let sig = blurriness.map(|b| (b * 0.5) as f32);
    let max = sig.iter().cloned().fold(0.0, f32::max);
    if max < 0.05 {
        return;
    }
    with_premul(px, width, height, (max * 3.0).ceil() as usize + 1, |buf, w, h| {
        let planes: Vec<Vec<[f32; 4]>> =
            sig.iter().map(|&s| if s < 0.05 { buf.clone() } else { gauss_buf(buf.clone(), w, h, s) }).collect();
        let mut out = buf;
        for (i, o) in out.iter_mut().enumerate() {
            let a = planes[3][i][3].clamp(0.0, 1.0);
            for c in 0..3 {
                let q = planes[c][i];
                o[c] = if q[3] > 1e-6 { q[c] / q[3] * a } else { 0.0 };
            }
            o[3] = a;
        }
        out
    });
}

/// Edge-preserving neighbourhood average. `hard`: box kernel with a hard
/// luminance range test (Smart Blur); otherwise Gaussian spatial x Gaussian
/// colour-range weights (bilateral filter). Big radii sample on a stride.
fn edge_preserving(px: &mut [u8], width: usize, height: usize, r: f64, range: f32, hard: bool) {
    if r < 0.5 {
        return;
    }
    let ri = r.ceil() as i64;
    let stride = ((r / 6.0).ceil() as i64).max(1);
    let sig_s2 = 2.0 * (r / 2.0).max(0.5).powi(2);
    let sig_r2 = 2.0 * range.max(1e-3).powi(2);
    with_premul(px, width, height, ri as usize, |buf, w, h| {
        let at = |x: i64, y: i64| buf[y.clamp(0, h as i64 - 1) as usize * w + x.clamp(0, w as i64 - 1) as usize];
        par_pixels(w, h, |x, y| {
            let c = buf[y * w + x];
            let cl = luma([c[0], c[1], c[2]]);
            let mut acc = [0.0f32; 4];
            let mut wsum = 0.0f32;
            let mut dy = -ri;
            while dy <= ri {
                let mut dx = -ri;
                while dx <= ri {
                    let d2 = (dx * dx + dy * dy) as f64;
                    if d2 <= r * r + 0.5 {
                        let q = at(x as i64 + dx, y as i64 + dy);
                        let wt = if hard {
                            if (luma([q[0], q[1], q[2]]) - cl).abs() <= range { 1.0 } else { 0.0 }
                        } else {
                            let dc: f32 = (0..4).map(|k| (q[k] - c[k]).powi(2)).sum();
                            ((-d2 / sig_s2) as f32 - dc / sig_r2).exp()
                        };
                        if wt > 0.0 {
                            for k in 0..4 {
                                acc[k] += q[k] * wt;
                            }
                            wsum += wt;
                        }
                    }
                    dx += stride;
                }
                dy += stride;
            }
            if wsum > 0.0 { acc.map(|v| v / wsum) } else { c }
        })
    });
}

/// Radii of the three box blurs whose sequence matches a Gaussian of `sigma`
/// (the standard "boxes for Gauss" sizing: widths `wl`/`wl + 2` chosen so
/// the variances sum to `sigma²`).
fn box_radii(sigma: f32) -> [usize; 3] {
    let n = 3.0;
    let ideal = (12.0 * sigma * sigma / n + 1.0).sqrt();
    let mut wl = ideal.floor() as i32;
    if wl % 2 == 0 {
        wl -= 1;
    }
    let wl = wl.max(1);
    let wu = wl + 2;
    let m = ((12.0 * sigma * sigma - n * (wl * wl) as f32 - 4.0 * n * wl as f32 - 3.0 * n)
        / (-4.0 * wl as f32 - 4.0))
        .round() as i32;
    let r = |i: i32| (((if i < m { wl } else { wu }) - 1) / 2) as usize;
    [r(0), r(1), r(2)]
}

/// The half-open box `(x0, y0, x1, y1)` holding every pixel with any alpha,
/// or `None` for a fully transparent image.
fn alpha_bounds(px: &[u8], width: usize, height: usize) -> Option<(usize, usize, usize, usize)> {
    let (mut x0, mut y0, mut x1, mut y1) = (width, height, 0, 0);
    for y in 0..height {
        let row = &px[y * width * 4..(y + 1) * width * 4];
        let Some(first) = row.chunks_exact(4).position(|p| p[3] != 0) else { continue };
        let last = row.chunks_exact(4).rposition(|p| p[3] != 0).unwrap_or(first);
        (x0, x1) = (x0.min(first), x1.max(last + 1));
        (y0, y1) = (y0.min(y), y + 1);
    }
    (x1 > x0).then_some((x0, y0, x1, y1))
}

/// One box-blur pass of radius `r` along an axis, as a running sum. Samples
/// clamp at the edges (extend the border) so the frame doesn't darken toward
/// its edges.
///
/// Split into bands of output rows across threads. A vertical pass works per
/// band too: each band primes its column sums at its first row and slides.
fn box_axis(
    src: &[[f32; 4]],
    dst: &mut [[f32; 4]],
    width: usize,
    height: usize,
    r: usize,
    horizontal: bool,
) {
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get()).min(height.max(1));
    let band = height.div_ceil(threads).max(1);
    let inv = 1.0 / (2 * r + 1) as f32;
    let ri = r as isize;
    std::thread::scope(|scope| {
        for (b, out) in dst.chunks_mut(band * width).enumerate() {
            let y0 = b * band;
            let rows = out.len() / width;
            scope.spawn(move || {
                if horizontal {
                    for row in 0..rows {
                        let line = &src[(y0 + row) * width..(y0 + row + 1) * width];
                        let px = |i: isize| line[i.clamp(0, width as isize - 1) as usize];
                        let mut acc = [0.0f32; 4];
                        for i in -ri..=ri {
                            let s = px(i);
                            for c in 0..4 {
                                acc[c] += s[c];
                            }
                        }
                        for x in 0..width {
                            out[row * width + x] = acc.map(|v| v * inv);
                            let (add, sub) = (px(x as isize + ri + 1), px(x as isize - ri));
                            for c in 0..4 {
                                acc[c] += add[c] - sub[c];
                            }
                        }
                    }
                } else {
                    let row_of = |y: isize| {
                        let y = y.clamp(0, height as isize - 1) as usize;
                        &src[y * width..(y + 1) * width]
                    };
                    let mut acc = vec![[0.0f32; 4]; width];
                    for y in (y0 as isize - ri)..=(y0 as isize + ri) {
                        for (a, s) in acc.iter_mut().zip(row_of(y)) {
                            for c in 0..4 {
                                a[c] += s[c];
                            }
                        }
                    }
                    for row in 0..rows {
                        let y = (y0 + row) as isize;
                        for (o, a) in out[row * width..(row + 1) * width].iter_mut().zip(&acc) {
                            *o = a.map(|v| v * inv);
                        }
                        let (add, sub) = (row_of(y + ri + 1), row_of(y - ri));
                        for ((a, p), m) in acc.iter_mut().zip(add).zip(sub) {
                            for c in 0..4 {
                                a[c] += p[c] - m[c];
                            }
                        }
                    }
                }
            });
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use motion_core::value::Color;

    fn solid(rgba: [u8; 4], n: usize) -> Vec<u8> {
        rgba.iter().cycle().take(n * 4).copied().collect()
    }

    /// A full tint replaces the colour outright and leaves alpha alone.
    #[test]
    fn a_full_tint_replaces_the_colour() {
        let mut px = solid([10, 20, 30, 200], 4);
        apply_stack(&mut px, 2, 2, &[ResolvedEffect::Tint { color: Color::rgb(1.0, 0.0, 0.0), amount: 1.0 }]);
        for chunk in px.chunks_exact(4) {
            assert_eq!(chunk, &[255, 0, 0, 200]);
        }
    }

    /// The identity curve — full range in, full range out, straight gamma —
    /// changes nothing. Adding Levels must not alter the frame until it is
    /// dialled in.
    #[test]
    fn identity_levels_change_nothing() {
        let mut px = solid([10, 128, 240, 200], 4);
        let before = px.clone();
        apply_stack(
            &mut px,
            2,
            2,
            &[ResolvedEffect::Levels {
                in_black: 0.0,
                in_white: 1.0,
                gamma: 1.0,
                out_black: 0.0,
                out_white: 1.0,
            }],
        );
        assert_eq!(px, before);
    }

    /// Levels stretches the input range onto the output range: half-grey with
    /// the input white pulled down to 0.5 lands on white.
    #[test]
    fn levels_stretch_the_range() {
        let mut px = solid([128, 128, 128, 255], 1);
        apply_stack(
            &mut px,
            1,
            1,
            &[ResolvedEffect::Levels {
                in_black: 0.0,
                in_white: 0.5,
                gamma: 1.0,
                out_black: 0.0,
                out_white: 1.0,
            }],
        );
        assert!(px[0] > 250, "128 with white at 0.5 should clip to white, got {}", px[0]);
        assert_eq!(px[3], 255, "and alpha is untouched");
    }

    /// Gamma bends the middle without moving the ends — the property that
    /// makes it a *curve* control rather than another brightness.
    #[test]
    fn gamma_moves_the_midtones_and_leaves_the_ends() {
        let mut px = vec![0, 0, 0, 255, 128, 128, 128, 255, 255, 255, 255, 255];
        apply_stack(
            &mut px,
            3,
            1,
            &[ResolvedEffect::Levels {
                in_black: 0.0,
                in_white: 1.0,
                gamma: 2.0,
                out_black: 0.0,
                out_white: 1.0,
            }],
        );
        assert_eq!(px[0], 0, "black stays black");
        assert_eq!(px[8], 255, "white stays white");
        assert!(px[4] > 140, "the midtone lifts, got {}", px[4]);
    }

    #[test]
    fn ported_effects_pixel_math() {
        // Curves: default seed is the identity; lifting the midpoint brightens.
        let mut px = solid([10, 128, 240, 200], 1);
        let before = px.clone();
        apply_stack(&mut px, 1, 1, &[ResolvedEffect::Curves { ys: [0.25, 0.5, 0.75] }]);
        assert_eq!(px, before);
        apply_stack(&mut px, 1, 1, &[ResolvedEffect::Curves { ys: [0.25, 0.75, 0.75] }]);
        assert!(px[1] > 150);
        // Balance: a blue shadow shift moves a dark pixel, not a white one.
        let mut px = vec![20, 20, 20, 255, 255, 255, 255, 255];
        apply_stack(&mut px, 2, 1, &[ResolvedEffect::TonalBalance { sh: [0.0, 0.0, 1.0], mid: [0.0; 3], hi: [0.0; 3] }]);
        assert!(px[2] > px[0] + 40 && px[4..7] == [255, 255, 255]);
        // Key: removes the key colour (within tolerance), keeps the rest.
        let mut px = vec![0, 0, 255, 255, 255, 0, 0, 255];
        let key = Color::rgb(0.0, 0.0, 1.0);
        apply_stack(&mut px, 2, 1, &[ResolvedEffect::ColorKey { color: key, tolerance: 0.1, softness: 0.0 }]);
        assert_eq!((px[3], px[7]), (0, 255));
        // Exposure: +1 stop doubles linear light (128 -> ~176); 0 stops is identity.
        let mut px = solid([128, 128, 128, 255], 1);
        apply_stack(&mut px, 1, 1, &[ResolvedEffect::Exposure { exposure: 0.0, offset: 0.0, gamma: 1.0 }]);
        assert_eq!(px[0], 128);
        apply_stack(&mut px, 1, 1, &[ResolvedEffect::Exposure { exposure: 1.0, offset: 0.0, gamma: 1.0 }]);
        assert!((174..=178).contains(&px[0]), "got {}", px[0]);
        // Black & White: grey out, and the red weight steers how bright red lands.
        let bw = |reds| {
            let mut px = solid([255, 0, 0, 255], 1);
            apply_stack(&mut px, 1, 1, &[ResolvedEffect::BlackWhite { reds, yellows: 0.6, greens: 0.4, cyans: 0.6, blues: 0.2, magentas: 0.8 }]);
            px
        };
        assert!(bw(0.4)[0] == bw(0.4)[1] && bw(1.0)[0] > bw(0.4)[0]);
        // Vibrance: boosts a muted colour, leaves grey and zero alone.
        let mut px = solid([150, 100, 100, 255], 1);
        apply_stack(&mut px, 1, 1, &[ResolvedEffect::Vibrance { vibrance: 1.0, saturation: 0.0 }]);
        assert!(px[0] - px[1] > 50);
        // Photo filter: a blue filter keeps luminance but pulls red down relative to blue.
        let mut px = solid([128, 128, 128, 255], 1);
        apply_stack(&mut px, 1, 1, &[ResolvedEffect::PhotoFilter { color: Color::rgb(0.0, 0.0, 1.0), density: 1.0 }]);
        assert!(px[2] > 128 && px[0] == 0);
        // Channel mixer: red <- blue swap; Gamma/Pedestal/Gain: gain halves, pedestal lifts.
        let mut px = solid([200, 0, 50, 255], 1);
        apply_stack(&mut px, 1, 1, &[ResolvedEffect::ChannelMixer { rr: 0.0, rg: 0.0, rb: 1.0, rc: 0.0, gr: 0.0, gg: 1.0, gb: 0.0, gc: 0.0, br: 1.0, bg: 0.0, bb: 0.0, bc: 0.0 }]);
        assert_eq!(px[..3], [50, 0, 200]);
        let mut px = solid([128, 128, 128, 255], 1);
        apply_stack(&mut px, 1, 1, &[ResolvedEffect::GammaPedestalGain { gamma: 1.0, pedestal: 0.0, gain: 0.5, black_stretch: 0.0 }]);
        assert_eq!(px[0], 64);
        apply_stack(&mut px, 1, 1, &[ResolvedEffect::GammaPedestalGain { gamma: 1.0, pedestal: 0.5, gain: 1.0, black_stretch: 0.0 }]);
        assert!(px[0] > 150);
        // Leave colour: keeps the key colour, greys the rest.
        let mut px = vec![255, 0, 0, 255, 0, 0, 255, 255];
        apply_stack(&mut px, 2, 1, &[ResolvedEffect::LeaveColor { color: Color::rgb(1.0, 0.0, 0.0), amount: 1.0, tolerance: 0.15, softness: 0.0 }]);
        assert_eq!(px[..3], [255, 0, 0]);
        assert!(px[4] == px[5] && px[5] == px[6]);
    }

    #[test]
    fn ported_blur_pixel_math() {
        // A lone opaque white pixel at (4,4) of a 9x9 field.
        let dot = || {
            let mut px = vec![0u8; 9 * 9 * 4];
            px[(4 * 9 + 4) * 4..][..4].copy_from_slice(&[255; 4]);
            px
        };
        let alpha = |px: &[u8], x: usize, y: usize| px[(y * 9 + x) * 4 + 3] as i32;
        // Box blur, radius 1, one pass: the 3x3 neighbourhood gets 1/9 each, outside stays clear.
        let mut px = dot();
        apply_stack(&mut px, 9, 9, &[ResolvedEffect::FastBoxBlur { radius: 1.0, iterations: 1.0 }]);
        assert!((alpha(&px, 3, 3) - 28).abs() <= 1 && alpha(&px, 2, 4) == 0);
        // Cross blur: spreads along both axes but not the diagonal.
        let mut px = dot();
        apply_stack(&mut px, 9, 9, &[ResolvedEffect::CrossBlur { radius_x: 1.0, radius_y: 1.0, mode: 0 }]);
        assert!(alpha(&px, 3, 4) > 0 && alpha(&px, 4, 3) > 0 && alpha(&px, 3, 3) == 0);
        // Directional blur at 90 degrees (rightwards) smears horizontally only.
        let mut px = dot();
        apply_stack(&mut px, 9, 9, &[ResolvedEffect::DirectionalBlur { angle: 90.0, length: 4.0 }]);
        assert!(alpha(&px, 6, 4) > 0 && alpha(&px, 4, 6) == 0);
        // Channel blur on alpha only: the alpha spreads, the dot keeps its own colour.
        let mut px = dot();
        apply_stack(&mut px, 9, 9, &[ResolvedEffect::ChannelBlur { red: 0.0, green: 0.0, blue: 0.0, alpha: 4.0 }]);
        assert!(alpha(&px, 3, 4) > 0 && alpha(&px, 4, 4) < 255 && px[(4 * 9 + 4) * 4] == 255);
        // Sharpen: a step edge overshoots on the bright side and undershoots on the dark side.
        let step = || -> Vec<u8> { (0..16).flat_map(|x| if x < 8 { [100, 100, 100, 255] } else { [200, 200, 200, 255] }).collect() };
        let mut px = step();
        apply_stack(&mut px, 16, 1, &[ResolvedEffect::Sharpen { amount: 1.0 }]);
        assert!(px[8 * 4] > 200 && px[7 * 4] < 100);
        // Unsharp threshold above the edge contrast leaves it untouched.
        let mut px = step();
        apply_stack(&mut px, 16, 1, &[ResolvedEffect::UnsharpMask { amount: 1.0, radius: 2.0, threshold: 0.9 }]);
        assert_eq!(px, step());
        // Bilateral / smart blur keep a hard edge (the range test rejects the other side).
        for e in [
            ResolvedEffect::BilateralBlur { radius: 3.0, threshold: 0.05 },
            ResolvedEffect::SmartBlur { radius: 3.0, threshold: 0.1 },
        ] {
            let mut px = step();
            apply_stack(&mut px, 16, 1, &[e]);
            assert!(px[7 * 4] < 110 && px[8 * 4] > 190, "{:?}", &px[28..36]);
        }
    }

    #[test]
    fn ported_stylize_pixel_math() {
        let one = |e: ResolvedEffect, px: [u8; 4]| {
            let mut v = px.to_vec();
            apply_stack(&mut v, 1, 1, &[e]);
            v
        };
        // Posterize to 2 levels snaps to black / white; threshold splits on luminance.
        assert_eq!(one(ResolvedEffect::Posterize { level: 2.0 }, [200, 50, 130, 255])[..3], [255, 0, 255]);
        assert_eq!(one(ResolvedEffect::Threshold { level: 0.5 }, [200, 200, 200, 255])[..3], [255; 3]);
        assert_eq!(one(ResolvedEffect::ThresholdRgb { red: 0.5, green: 0.5, blue: 0.5, blend: 0.0 }, [200, 50, 130, 255])[..3], [255, 0, 255]);
        // Find Edges: flat area is white, a step edge goes dark.
        let step: Vec<u8> = (0..8).flat_map(|x| if x < 4 { [0, 0, 0, 255] } else { [255, 255, 255, 255] }).collect();
        let mut px = step.clone();
        apply_stack(&mut px, 8, 1, &[ResolvedEffect::FindEdges { blend: 0.0, invert: false }]);
        assert!(px[0] == 255 && px[3 * 4] < 30);
        // Emboss: flat is mid grey; a step lights one side.
        let mut px = step.clone();
        apply_stack(&mut px, 8, 1, &[ResolvedEffect::Emboss { direction: 0.0, relief: 1.0, contrast: 1.0, blend: 0.0 }]);
        assert!((px[0] as i32 - 128).abs() <= 1 && px[3 * 4] != 128);
        // Mosaic with 2 blocks across averages each half to one colour.
        let mut px = step;
        apply_stack(&mut px, 8, 1, &[ResolvedEffect::Mosaic { horizontal: 2.0, vertical: 1.0 }]);
        assert!(px[..16].iter().step_by(4).all(|&v| v == 0) && px[16..].iter().step_by(4).all(|&v| v == 255));
    }

    #[test]
    fn mosaic_tiles_follow_the_visible_bounds() {
        // 8x8 canvas, transparent except a 4x4 block at (2,2) of four distinct quadrants.
        let mut px = vec![0u8; 8 * 8 * 4];
        for y in 2..6 {
            for x in 2..6 {
                let v = [40u8, 120, 200, 250][(y - 2) / 2 * 2 + (x - 2) / 2];
                px[(y * 8 + x) * 4..][..4].copy_from_slice(&[v, v, v, 255]);
            }
        }
        apply_stack(&mut px, 8, 8, &[ResolvedEffect::Mosaic { horizontal: 2.0, vertical: 2.0 }]);
        let at = |x: usize, y: usize| px[(y * 8 + x) * 4];
        assert_eq!([at(2, 2), at(5, 2), at(2, 5), at(5, 5)], [40, 120, 200, 250]);
        assert_eq!(px[3], 0);
        assert_eq!(px[(7 * 8 + 7) * 4 + 3], 0);
    }

    /// A drop shadow paints where the artwork is *not*, offset from it, and
    /// leaves the artwork itself opaque and its own colour.
    #[test]
    fn a_drop_shadow_lands_beside_the_artwork() {
        // One opaque white pixel at (2,2) of a 9x9 transparent field.
        let (w, h) = (9usize, 9usize);
        let mut px = vec![0u8; w * h * 4];
        let at = |x: usize, y: usize| (y * w + x) * 4;
        px[at(2, 2)..at(2, 2) + 4].copy_from_slice(&[255, 255, 255, 255]);

        apply_stack(
            &mut px,
            w,
            h,
            &[ResolvedEffect::DropShadow {
                color: Color::rgb(0.0, 0.0, 0.0),
                offset_x: 3.0,
                offset_y: 3.0,
                // Hard-edged, so the assertions are about placement rather
                // than about the blur kernel.
                radius: 0.0,
                opacity: 1.0,
            }],
        );

        let alpha = |x: usize, y: usize| px[at(x, y) + 3];
        assert_eq!(alpha(5, 5), 255, "the shadow lands down-right of the artwork");
        assert_eq!(&px[at(5, 5)..at(5, 5) + 3], &[0, 0, 0], "and is the shadow colour");
        assert_eq!(&px[at(2, 2)..at(2, 2) + 4], &[255, 255, 255, 255], "the artwork is untouched");
        assert_eq!(alpha(8, 8), 0, "and nothing lands where neither is");
    }

    /// A transparent layer casts no shadow: the shadow is made of the layer's
    /// own alpha, so nothing in means nothing out.
    #[test]
    fn nothing_casts_no_shadow() {
        let mut px = vec![0u8; 4 * 16];
        let before = px.clone();
        apply_stack(
            &mut px,
            4,
            4,
            &[ResolvedEffect::DropShadow {
                color: Color::rgb(0.0, 0.0, 0.0),
                offset_x: 1.0,
                offset_y: 1.0,
                radius: 2.0,
                opacity: 1.0,
            }],
        );
        assert_eq!(px, before);
    }

    /// Shadow opacity scales the shadow and not the artwork — the two are
    /// composited, not blended together.
    #[test]
    fn shadow_opacity_dims_only_the_shadow() {
        let (w, h) = (9usize, 9usize);
        let at = |x: usize, y: usize| (y * w + x) * 4;
        let render = |opacity: f64| {
            let mut px = vec![0u8; w * h * 4];
            px[at(2, 2)..at(2, 2) + 4].copy_from_slice(&[255, 255, 255, 255]);
            apply_stack(
                &mut px,
                w,
                h,
                &[ResolvedEffect::DropShadow {
                    color: Color::rgb(0.0, 0.0, 0.0),
                    offset_x: 3.0,
                    offset_y: 3.0,
                    radius: 0.0,
                    opacity,
                }],
            );
            px
        };
        let half = render(0.5);
        assert!((120..=136).contains(&half[at(5, 5) + 3]), "half-strength shadow");
        assert_eq!(half[at(2, 2) + 3], 255, "the artwork keeps its own alpha");
    }

    /// A zero-amount tint is an identity pass.
    #[test]
    fn a_zero_tint_changes_nothing() {
        let mut px = solid([10, 20, 30, 200], 4);
        let before = px.clone();
        apply_stack(&mut px, 2, 2, &[ResolvedEffect::Tint { color: Color::rgb(1.0, 0.0, 0.0), amount: 0.0 }]);
        assert_eq!(px, before);
    }

    /// Positive brightness raises every channel; the default (0,0) is identity.
    #[test]
    fn brightness_raises_and_defaults_are_identity() {
        let mut px = solid([100, 100, 100, 255], 1);
        apply_stack(&mut px, 1, 1, &[ResolvedEffect::BrightnessContrast { brightness: 0.2, contrast: 0.0 }]);
        assert!(px[0] > 100, "brightness should raise the channel");

        let mut id = solid([100, 128, 200, 255], 1);
        let before = id.clone();
        apply_stack(&mut id, 1, 1, &[ResolvedEffect::BrightnessContrast { brightness: 0.0, contrast: 0.0 }]);
        assert_eq!(id, before, "zero brightness/contrast is identity");
    }

    /// A 180° hue rotation on pure red lands on cyan (red's complement),
    /// confirming the HSL round-trip.
    #[test]
    fn a_180_hue_rotation_takes_red_to_cyan() {
        let mut px = solid([255, 0, 0, 255], 1);
        apply_stack(&mut px, 1, 1, &[ResolvedEffect::HueSaturation { hue: 180.0, saturation: 0.0, lightness: 0.0 }]);
        assert!(px[0] < 10 && px[1] > 245 && px[2] > 245, "red + 180° = cyan, got {:?}", &px[..3]);
    }

    /// A grey image survives an HSL round-trip unchanged (hue of a grey is
    /// undefined, and the code must not introduce a tint).
    #[test]
    fn grey_is_stable_through_hue_saturation() {
        let mut px = solid([128, 128, 128, 255], 1);
        apply_stack(&mut px, 1, 1, &[ResolvedEffect::HueSaturation { hue: 90.0, saturation: 0.0, lightness: 0.0 }]);
        for &v in &px[..3] {
            assert!((v as i32 - 128).abs() <= 1, "grey shifted: {:?}", &px[..3]);
        }
    }

    /// Blurring a single bright pixel spreads its colour into its neighbours and
    /// conserves premultiplied energy (nothing is created or destroyed).
    #[test]
    fn blur_spreads_a_hot_pixel_to_its_neighbours() {
        let (w, h) = (5, 5);
        let mut px = vec![0u8; w * h * 4];
        let centre = (2 * w + 2) * 4;
        px[centre..centre + 4].copy_from_slice(&[255, 255, 255, 255]);
        apply_stack(&mut px, w, h, &[ResolvedEffect::GaussianBlur { radius: 1.0 }]);

        // The centre dimmed and a neighbour lit up.
        assert!(px[centre + 3] < 255, "centre alpha should have spread out");
        let neighbour = (2 * w + 1) * 4;
        assert!(px[neighbour + 3] > 0, "neighbour should have picked up alpha");
    }

    /// A sub-pixel radius is a no-op, so an effect dialled to nothing costs
    /// nothing and changes nothing.
    #[test]
    fn a_tiny_blur_is_a_no_op() {
        let (w, h) = (4, 4);
        let mut px = solid([200, 50, 50, 255], w * h);
        let before = px.clone();
        apply_stack(&mut px, w, h, &[ResolvedEffect::GaussianBlur { radius: 0.1 }]);
        assert_eq!(px, before);
    }

    /// The in-scene colour path (what the renderer calls per item): a full tint
    /// replaces the colour and keeps alpha.
    #[test]
    fn the_colour_path_applies_a_tint() {
        let out = apply_color_effects(
            Color::rgba(0.1, 0.2, 0.3, 0.5),
            &[ResolvedEffect::Tint { color: Color::rgb(1.0, 0.0, 0.0), amount: 1.0 }],
        );
        assert_eq!(out, Color::rgba(1.0, 0.0, 0.0, 0.5));
    }

    /// A blur in the stack is skipped by the colour path (it needs the whole
    /// image), so a colour surrounded by blurs still comes through.
    #[test]
    fn the_colour_path_skips_blur() {
        let c = Color::rgb(0.4, 0.6, 0.8);
        let only_blur = apply_color_effects(c, &[ResolvedEffect::GaussianBlur { radius: 5.0 }]);
        // Approximate: the colour path round-trips through f32, so equality is to
        // within that precision, not bit-exact.
        for (a, b) in [(only_blur.r, c.r), (only_blur.g, c.g), (only_blur.b, c.b), (only_blur.a, c.a)] {
            assert!((a - b).abs() < 1e-6, "blur alone changed the colour: {only_blur:?} vs {c:?}");
        }
        assert!(needs_readback(&[ResolvedEffect::GaussianBlur { radius: 5.0 }]));
        assert!(!needs_readback(&[ResolvedEffect::Tint { color: c, amount: 1.0 }]));
    }

    /// The stack is ordered: tint-then-brighten differs from brighten-then-tint.
    #[test]
    fn effects_apply_in_order() {
        let stack_a = [
            ResolvedEffect::Tint { color: Color::rgb(0.0, 0.0, 1.0), amount: 0.5 },
            ResolvedEffect::BrightnessContrast { brightness: 0.3, contrast: 0.0 },
        ];
        let stack_b = [stack_a[1], stack_a[0]];
        let mut a = solid([100, 100, 100, 255], 1);
        let mut b = a.clone();
        apply_stack(&mut a, 1, 1, &stack_a);
        apply_stack(&mut b, 1, 1, &stack_b);
        assert_ne!(a, b, "effect order should matter");
    }

    /// Chroma key clears the key colour, keeps what is far from it, and
    /// partially keeps what lands in the softness ramp.
    #[test]
    fn chroma_key_clears_the_key_and_keeps_the_rest() {
        // green, red, and a green 0.35 away (inside tolerance+softness).
        let mut px = vec![0, 255, 0, 255, 255, 0, 0, 255, 0, 166, 0, 255];
        apply_stack(
            &mut px,
            3,
            1,
            &[ResolvedEffect::ChromaKey {
                color: Color::rgb(0.0, 1.0, 0.0),
                tolerance: 0.3,
                softness: 0.1,
            }],
        );
        assert_eq!(px[3], 0, "the key goes transparent");
        assert_eq!(&px[4..8], &[255, 0, 0, 255], "far colours are untouched");
        assert!(px[11] > 0 && px[11] < 255, "the ramp is partial, got {}", px[11]);
    }

    /// Glow only brightens, spills onto empty canvas, and ignores anything
    /// below its threshold.
    #[test]
    fn glow_spills_bright_pixels_only() {
        let (w, h) = (9usize, 1usize);
        let mut px = vec![0u8; w * h * 4];
        px[16..20].copy_from_slice(&[255, 255, 255, 255]);
        let mut dim = px.clone();
        dim[16..20].copy_from_slice(&[40, 40, 40, 255]);
        let glow = [ResolvedEffect::Glow { threshold: 0.5, radius: 2.0, intensity: 1.0 }];
        apply_stack(&mut px, w, h, &glow);
        assert!(px[5 * 4 + 3] > 0, "the halo spills beside a bright pixel");
        assert_eq!(&px[16..20], &[255, 255, 255, 255], "and the source stays");
        let before = dim.clone();
        apply_stack(&mut dim, w, h, &glow);
        assert_eq!(dim, before, "a dim pixel doesn't glow");
    }

    /// Zero balance is the identity; a red push moves only red.
    #[test]
    fn color_balance_shifts_one_channel() {
        let mut px = solid([100, 100, 100, 255], 1);
        let before = px.clone();
        apply_stack(&mut px, 1, 1, &[ResolvedEffect::ColorBalance { red: 0.0, green: 0.0, blue: 0.0 }]);
        assert_eq!(px, before);
        apply_stack(&mut px, 1, 1, &[ResolvedEffect::ColorBalance { red: 0.2, green: 0.0, blue: 0.0 }]);
        assert!(px[0] > 140 && px[1] == 100 && px[2] == 100, "got {:?}", &px[..3]);
    }

    /// Spin blur about a chosen centre: a bright pixel to the right of the centre smears along
    /// the arc (up/down), and with the centre moved onto the pixel nothing smears.
    #[test]
    fn radial_blur_follows_its_centre() {
        let (w, h) = (21usize, 21usize);
        let mk = || {
            let mut px = vec![0u8; w * h * 4];
            px[(10 * w + 15) * 4..][..4].copy_from_slice(&[255, 255, 255, 255]);
            px
        };
        let a = |px: &[u8], x: usize, y: usize| px[(y * w + x) * 4 + 3];
        let mut spun = mk();
        apply_stack(&mut spun, w, h, &[ResolvedEffect::RadialBlur { amount: 60.0, center: (10.5, 10.5), zoom: false }]);
        assert!(a(&spun, 15, 8) > 0 || a(&spun, 15, 12) > 0, "smears along the arc");
        let mut still = mk();
        apply_stack(&mut still, w, h, &[ResolvedEffect::RadialBlur { amount: 60.0, center: (15.5, 10.5), zoom: false }]);
        assert_eq!(a(&still, 15, 8), 0);
        assert_eq!(a(&still, 15, 10), 255);
    }

    #[test]
    fn find_edges_invert_flips_flat_areas() {
        let mut px = vec![200u8; 8 * 8 * 4];
        px.chunks_exact_mut(4).for_each(|p| p[3] = 255);
        apply_stack(&mut px, 8, 8, &[ResolvedEffect::FindEdges { blend: 0.0, invert: true }]);
        assert_eq!(px[(4 * 8 + 4) * 4], 0);
    }
}

#[cfg(test)]
/// `cargo test -p motion-render time_glow -- --ignored --nocapture`
mod bench {
    #[test]
    #[ignore]
    fn time_glow() {
        let (w, h) = (1920usize, 1080usize);
        let mut px = vec![0u8; w * h * 4];
        for y in 500..600 {
            for x in 900..1000 {
                px[(y * w + x) * 4..(y * w + x) * 4 + 4].copy_from_slice(&[255, 200, 200, 255]);
            }
        }
        let t = std::time::Instant::now();
        super::apply_stack(&mut px, w, h, &[motion_core::ResolvedEffect::Glow { threshold: 0.6, radius: 12.0, intensity: 1.0 }]);
        eprintln!("glow 1080p: {:?}", t.elapsed());
        let t = std::time::Instant::now();
        super::gaussian_blur(&mut px, w, h, 12.0);
        eprintln!("blur 1080p: {:?}", t.elapsed());
    }
}
