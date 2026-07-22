//! motion-render — turns an evaluated `Scene` into pixels.
//!
//! Today there is one backend: SVG. It has zero GPU dependencies, so the whole
//! core → render → output pipeline runs and is verifiable immediately. The
//! real-time GPU backend (vello on wgpu) slots in behind the same `Scene`
//! input later without touching `motion-core`.

pub mod decode;

pub use decode::{default_registry, FfmpegDecoder, ImageDecoder};

use kurbo::Shape as _;
use motion_core::{Asset, Color, Scene};

/// Serialize an evaluated scene to an SVG string sized to the composition.
///
/// Footage-free scenes can pass an empty `assets` slice; a raster item whose
/// asset isn't listed degrades to its plain rectangle rather than failing.
pub fn scene_to_svg(
    scene: &Scene,
    width: f64,
    height: f64,
    background: Color,
    assets: &[Asset],
) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{width}\" height=\"{height}\" \
         viewBox=\"0 0 {width} {height}\">\n"
    ));
    out.push_str(&format!(
        "  <rect x=\"0\" y=\"0\" width=\"{width}\" height=\"{height}\" fill=\"{}\"/>\n",
        css_rgba(background)
    ));

    for item in &scene.items {
        // kurbo emits SVG path data directly; we push the node transform as an
        // SVG matrix so the geometry stays in local coordinates.
        let d = item.path.to_svg();
        let c = item.transform.as_coeffs();
        let matrix = format!(
            "matrix({} {} {} {} {} {})",
            c[0], c[1], c[2], c[3], c[4], c[5]
        );

        let fill = match item.fill {
            Some(color) => css_rgba(with_alpha(color, item.opacity)),
            None => "none".to_string(),
        };
        let stroke = match item.stroke {
            Some((color, w)) => format!(
                " stroke=\"{}\" stroke-width=\"{w}\"",
                css_rgba(with_alpha(color, item.opacity))
            ),
            None => String::new(),
        };

        // Footage: the frame rectangle becomes a clip path and the source is
        // referenced by *path*, not embedded. Consistent with how the document
        // stores footage — references, never pixels — and it keeps this
        // backend free of decoders. `preserveAspectRatio="none"` because the
        // item's rect is already the size the layer says to draw at; letting
        // SVG letterbox would disagree with every other renderer's geometry.
        //
        // Only the *first* frame of a clip is addressable this way, so this is
        // honest for stills and approximate for video — an SVG has no notion of
        // a source frame. The GPU backend is where video actually plays.
        if let Some(paint) = item.image {
            let href = assets
                .iter()
                .find(|a| a.id == paint.asset)
                .map(|a| a.path.to_string_lossy().replace('&', "&amp;"));
            // A `None` here is footage the project no longer has. `evaluate`
            // already warned; fall through and draw the plain rectangle so the
            // layer's place on screen is still visible.
            if let Some(href) = href {
                let b = item.path.bounding_box();
                out.push_str(&format!(
                    "  <image transform=\"{matrix}\" x=\"{}\" y=\"{}\" width=\"{}\" \
                     height=\"{}\" preserveAspectRatio=\"none\" opacity=\"{:.3}\" \
                     href=\"{href}\"/>\n",
                    b.x0,
                    b.y0,
                    b.width(),
                    b.height(),
                    item.opacity.clamp(0.0, 1.0),
                ));
                continue;
            }
        }

        out.push_str(&format!(
            "  <path transform=\"{matrix}\" d=\"{d}\" fill=\"{fill}\"{stroke}/>\n"
        ));
    }

    out.push_str("</svg>\n");
    out
}

fn with_alpha(mut c: Color, mul: f64) -> Color {
    c.a *= mul;
    c
}

fn css_rgba(c: Color) -> String {
    let to255 = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!(
        "rgba({},{},{},{:.3})",
        to255(c.r),
        to255(c.g),
        to255(c.b),
        c.a.clamp(0.0, 1.0)
    )
}
