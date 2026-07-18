//! motion-render — turns an evaluated `Scene` into pixels.
//!
//! Today there is one backend: SVG. It has zero GPU dependencies, so the whole
//! core → render → output pipeline runs and is verifiable immediately. The
//! real-time GPU backend (vello on wgpu) slots in behind the same `Scene`
//! input later without touching `motion-core`.

use motion_core::{Color, Scene};

/// Serialize an evaluated scene to an SVG string sized to the composition.
pub fn scene_to_svg(scene: &Scene, width: f64, height: f64, background: Color) -> String {
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
