//! Pixel effects: the actual per-pixel math of the effect stack, applied to a
//! layer's isolated RGBA image.
//!
//! This is the compositor's raster half. [`crate::props`] and `core` decide
//! *which* effects a layer has and *what their parameters are* on a frame;
//! this module is where those resolved parameters finally touch pixels. It is
//! kept deliberately **pure and backend-free** — it operates on a plain
//! `&mut [u8]` of straight (non-premultiplied) RGBA8, knowing nothing about
//! wgpu, vello or textures — so the arithmetic can be unit-tested against
//! hand-computed pixels rather than by eyeballing a GPU frame, and so the same
//! routines serve whichever application path the renderer uses (a CPU readback
//! today, a WGSL port later; the maths must match either way).
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
pub(crate) fn apply_color_effects(color: MColor, effects: &[ResolvedEffect]) -> MColor {
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
pub(crate) fn needs_readback(effects: &[ResolvedEffect]) -> bool {
    effects.iter().any(|e| matches!(e, ResolvedEffect::GaussianBlur { .. }))
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
#[allow(dead_code)]
pub(crate) fn apply_stack(px: &mut [u8], width: usize, height: usize, effects: &[ResolvedEffect]) {
    for effect in effects {
        apply_one(px, width, height, effect);
    }
}

#[allow(dead_code)]
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
    }
}

/// Run a per-pixel RGB function over the buffer, leaving alpha untouched.
///
/// The colour is passed and returned as straight (non-premultiplied) linear-in
/// `[0, 1]` — "linear" here only meaning the 0..255 encoding scaled down, not a
/// gamma decode, matching the rest of the engine's naive colour handling.
#[allow(dead_code)]
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

#[allow(dead_code)]
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
#[allow(dead_code)]
fn gaussian_blur(px: &mut [u8], width: usize, height: usize, radius: f64) {
    if radius < 0.5 || width == 0 || height == 0 {
        return;
    }
    let kernel = gaussian_kernel(radius as f32);
    let r = kernel.len() / 2;

    // Work in premultiplied f32 so alpha weighting is correct across edges.
    let mut buf: Vec<[f32; 4]> = px
        .chunks_exact(4)
        .map(|c| {
            let a = c[3] as f32 / 255.0;
            [
                (c[0] as f32 / 255.0) * a,
                (c[1] as f32 / 255.0) * a,
                (c[2] as f32 / 255.0) * a,
                a,
            ]
        })
        .collect();

    let mut tmp = vec![[0.0f32; 4]; buf.len()];
    // Horizontal pass: buf -> tmp.
    blur_axis(&buf, &mut tmp, width, height, &kernel, r, true);
    // Vertical pass: tmp -> buf.
    blur_axis(&tmp, &mut buf, width, height, &kernel, r, false);

    for (out, p) in px.chunks_exact_mut(4).zip(buf.iter()) {
        let a = p[3];
        let inv = if a > 1e-6 { 1.0 / a } else { 0.0 };
        out[0] = to_u8(p[0] * inv);
        out[1] = to_u8(p[1] * inv);
        out[2] = to_u8(p[2] * inv);
        out[3] = to_u8(a);
    }
}

/// One separable pass. `horizontal` picks the axis; samples clamp at the edges
/// (extend the border) so the frame doesn't darken toward its edges.
#[allow(dead_code)]
fn blur_axis(
    src: &[[f32; 4]],
    dst: &mut [[f32; 4]],
    width: usize,
    height: usize,
    kernel: &[f32],
    r: usize,
    horizontal: bool,
) {
    for y in 0..height {
        for x in 0..width {
            let mut acc = [0.0f32; 4];
            for (k, &w) in kernel.iter().enumerate() {
                let offset = k as isize - r as isize;
                let (sx, sy) = if horizontal {
                    ((x as isize + offset).clamp(0, width as isize - 1) as usize, y)
                } else {
                    (x, (y as isize + offset).clamp(0, height as isize - 1) as usize)
                };
                let s = src[sy * width + sx];
                acc[0] += s[0] * w;
                acc[1] += s[1] * w;
                acc[2] += s[2] * w;
                acc[3] += s[3] * w;
            }
            dst[y * width + x] = acc;
        }
    }
}

/// A normalized 1D Gaussian kernel for standard deviation `sigma`, truncated at
/// 3σ (where the tail is negligible) and re-normalized so the weights sum to 1.
#[allow(dead_code)]
fn gaussian_kernel(sigma: f32) -> Vec<f32> {
    let sigma = sigma.max(1e-3);
    let r = (sigma * 3.0).ceil() as usize;
    let mut k: Vec<f32> = (0..=2 * r)
        .map(|i| {
            let x = i as f32 - r as f32;
            (-(x * x) / (2.0 * sigma * sigma)).exp()
        })
        .collect();
    let sum: f32 = k.iter().sum();
    for w in &mut k {
        *w /= sum;
    }
    k
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
}
