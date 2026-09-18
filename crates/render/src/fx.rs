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
            ResolvedEffect::Glow { .. } | ResolvedEffect::ChromaKey { .. } => rgb,
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
    if radius < 0.5 || width == 0 || height == 0 {
        return;
    }

    // Only the layer's visible pixels plus the blur's reach can change: a
    // small shape on a big canvas blurs a small box, not the whole frame.
    let Some((x0, y0, x1, y1)) = alpha_bounds(px, width, height) else { return };
    let m = (radius * 3.0).ceil() as usize + 1;
    let (x0, y0) = (x0.saturating_sub(m), y0.saturating_sub(m));
    let (x1, y1) = ((x1 + m).min(width), (y1 + m).min(height));
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

    // Three box blurs approximate the Gaussian (central limit), and a box is
    // a running sum: cost per pixel no longer grows with the radius, which is
    // what made a large glow or soft shadow stall the preview.
    let mut tmp = vec![[0.0f32; 4]; buf.len()];
    for r in box_radii(radius as f32) {
        box_axis(&buf, &mut tmp, w, h, r, true);
        box_axis(&tmp, &mut buf, w, h, r, false);
    }

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
