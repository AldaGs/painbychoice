//! motion-render — turns an evaluated `Scene` into pixels.
//!
//! Today there is one backend: SVG. It has zero GPU dependencies, so the whole
//! core → render → output pipeline runs and is verifiable immediately. The
//! real-time GPU backend (vello on wgpu) slots in behind the same `Scene`
//! input later without touching `motion-core`.

pub mod decode;

pub use decode::{default_registry, FfmpegDecoder, ImageDecoder};

use kurbo::Shape as _;
use motion_core::{Asset, BlendMode, Color, ComposeMode, Scene};

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

    // Isolated layers become nested `<g>`s carrying `mix-blend-mode`, which is
    // SVG's own name for the same operation — so the offline render agrees
    // with the GPU one instead of quietly dropping every blend mode. The
    // nesting order comes from `Scene` itself, so both backends open groups
    // the same way by construction.
    let groups = scene.nesting_order();
    let mut next_group = 0usize;
    let mut open: Vec<usize> = Vec::new();

    // Track mattes are **not** implemented here yet: SVG expresses them with
    // `<mask>` rather than a coverage rule, which is a different construction
    // from the `<g>` nesting above. Until that lands, the matte layer's items
    // are skipped rather than drawn — an unmatted layer is wrong, but a matte
    // painted as a solid shape over the content is wrong *and* unrecognisable.
    // The GPU backend does mattes properly; see `to_vello`.
    let matte_ranges: Vec<(usize, usize)> = scene
        .groups
        .iter()
        .filter(|g| g.compose != ComposeMode::SrcOver)
        .map(|g| (g.start, g.end))
        .collect();
    let is_matte = |i: usize| matte_ranges.iter().any(|(s, e)| i >= *s && i < *e);

    for (i, item) in scene.items.iter().enumerate() {
        while let Some(g) = groups.get(next_group).filter(|g| g.start == i) {
            // Advance first: a matte group is consumed rather than opened, and
            // leaving the cursor on it would stall every group after it.
            next_group += 1;
            if g.compose != ComposeMode::SrcOver {
                continue;
            }
            // A mask becomes a `<clipPath>` defined inline and referenced by
            // the group it clips. `clipRule` carries the even-odd rule an
            // inverted mask needs — core has already built the donut, so this
            // backend doesn't re-derive the inversion and can't disagree with
            // the GPU one about it.
            let clip_ref = match &g.clip {
                Some(mask) => {
                    let id = format!("mask{}_{}", g.source.0, i);
                    let c = mask.transform.as_coeffs();
                    out.push_str(&format!(
                        "  <clipPath id=\"{id}\" clipPathUnits=\"userSpaceOnUse\">\
                         <path transform=\"matrix({} {} {} {} {} {})\" d=\"{}\" \
                         clip-rule=\"{}\"/></clipPath>\n",
                        c[0],
                        c[1],
                        c[2],
                        c[3],
                        c[4],
                        c[5],
                        mask.path.to_svg(),
                        if mask.even_odd { "evenodd" } else { "nonzero" },
                    ));
                    format!(" clip-path=\"url(#{id})\"")
                }
                None => String::new(),
            };
            out.push_str(&format!(
                "  <g style=\"mix-blend-mode:{}\" opacity=\"{:.3}\"{clip_ref}>\n",
                css_blend(g.blend),
                g.alpha.clamp(0.0, 1.0)
            ));
            open.push(g.end);
        }

        if is_matte(i) {
            // Consumed by a matte this backend can't yet express; see above.
            while open.last() == Some(&(i + 1)) {
                out.push_str("  </g>\n");
                open.pop();
            }
            continue;
        }

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
        //
        // A missing `href` is footage the project no longer has. `evaluate`
        // already warned; fall through and draw the plain rectangle so the
        // layer's place on screen is still visible.
        let footage_href = item.image.and_then(|paint| {
            assets
                .iter()
                .find(|a| a.id == paint.asset)
                .map(|a| a.path.to_string_lossy().replace('&', "&amp;"))
        });
        match footage_href {
            Some(href) => {
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
            }
            None => out.push_str(&format!(
                "  <path transform=\"{matrix}\" d=\"{d}\" fill=\"{fill}\"{stroke}/>\n"
            )),
        }

        // Close every layer that ended with this item, innermost first. This
        // has to run for *every* item, which is why drawing footage is an arm
        // above rather than an early `continue`.
        while open.last() == Some(&(i + 1)) {
            out.push_str("  </g>\n");
            open.pop();
        }
    }
    for _ in 0..open.len() {
        out.push_str("  </g>\n");
    }

    out.push_str("</svg>\n");
    out
}

/// The CSS keyword for a blend mode.
///
/// A straight rename: `BlendMode` is deliberately the standard sixteen that CSS
/// and SVG already define, so nothing is approximated here.
fn css_blend(mode: BlendMode) -> &'static str {
    match mode {
        BlendMode::Normal => "normal",
        BlendMode::Multiply => "multiply",
        BlendMode::Screen => "screen",
        BlendMode::Overlay => "overlay",
        BlendMode::Darken => "darken",
        BlendMode::Lighten => "lighten",
        BlendMode::ColorDodge => "color-dodge",
        BlendMode::ColorBurn => "color-burn",
        BlendMode::HardLight => "hard-light",
        BlendMode::SoftLight => "soft-light",
        BlendMode::Difference => "difference",
        BlendMode::Exclusion => "exclusion",
        BlendMode::Hue => "hue",
        BlendMode::Saturation => "saturation",
        BlendMode::Color => "color",
        BlendMode::Luminosity => "luminosity",
    }
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

#[cfg(test)]
mod svg_tests {
    use super::*;
    use motion_core::{Comp, Node, Shape, Value};

    fn blended(id: u64, blend: BlendMode) -> Node {
        let mut n = Node::group(id, format!("n{id}"));
        n.shape = Some(Shape::Rect {
            size: Value::constant(kurbo::Vec2::new(10.0, 10.0)),
            radius: Value::constant(0.0),
        });
        n.fill = Some(Value::constant(Color::rgb(1.0, 1.0, 1.0)));
        n.blend = blend;
        n
    }

    fn render(root: Node) -> String {
        let comp = Comp::new(100.0, 100.0, root);
        scene_to_svg(&motion_core::evaluate(&comp, 0.0), 100.0, 100.0, Color::rgb(0.0, 0.0, 0.0), &[])
    }

    /// A blend mode reaches the offline render too. Dropping it here would make
    /// the exported frame quietly disagree with the preview — the worst kind of
    /// rendering bug, because nothing reports it.
    #[test]
    fn a_blend_mode_survives_into_the_svg() {
        let svg = render(Node::group(0, "root").with_child(blended(1, BlendMode::Multiply)));
        assert!(svg.contains("mix-blend-mode:multiply"), "{svg}");
    }

    /// Every `<g>` opened is closed. Unbalanced tags are not a rendering
    /// artefact but a broken file, and nothing in the pipeline would catch it.
    #[test]
    fn nested_groups_are_balanced() {
        let mut outer = blended(1, BlendMode::Multiply);
        outer.children.push(blended(2, BlendMode::Screen));
        let svg = render(Node::group(0, "root").with_child(outer));

        assert_eq!(svg.matches("<g ").count(), 2);
        assert_eq!(svg.matches("</g>").count(), 2);
        // And the inner one opens after the outer one.
        let outer_at = svg.find("mix-blend-mode:multiply").unwrap();
        let inner_at = svg.find("mix-blend-mode:screen").unwrap();
        assert!(outer_at < inner_at);
    }

    /// The ordinary document gains no wrapper at all.
    #[test]
    fn an_unblended_document_emits_no_groups() {
        let svg = render(Node::group(0, "root").with_child(blended(1, BlendMode::Normal)));
        assert!(!svg.contains("<g "), "{svg}");
    }
}

#[cfg(test)]
mod mask_tests {
    use super::*;
    use motion_core::{Comp, Mask, Node, Shape, Value};

    fn masked(inverted: bool) -> String {
        let mut n = Node::group(1, "n");
        n.shape = Some(Shape::Rect {
            size: Value::constant(kurbo::Vec2::new(10.0, 10.0)),
            radius: Value::constant(0.0),
        });
        n.fill = Some(Value::constant(Color::rgb(1.0, 1.0, 1.0)));
        n.mask = Some(Mask {
            shape: Shape::Ellipse { size: Value::constant(kurbo::Vec2::new(4.0, 4.0)) },
            inverted,
        });
        let comp = Comp::new(100.0, 100.0, Node::group(0, "root").with_child(n));
        scene_to_svg(
            &motion_core::evaluate(&comp, 0.0),
            100.0,
            100.0,
            Color::rgb(0.0, 0.0, 0.0),
            &[],
        )
    }

    /// A mask reaches the offline render as a `<clipPath>`, so an exported
    /// frame agrees with the preview instead of silently drawing the unmasked
    /// layer.
    #[test]
    fn a_mask_becomes_a_clip_path() {
        let svg = masked(false);
        assert!(svg.contains("<clipPath"), "{svg}");
        assert!(svg.contains("clip-path=\"url(#"), "{svg}");
        assert!(svg.contains("clip-rule=\"nonzero\""), "{svg}");
    }

    /// An inverted mask carries the even-odd rule, which is what makes the
    /// punched-out shape a hole rather than a second solid region. Core builds
    /// the geometry, so both backends invert identically.
    #[test]
    fn an_inverted_mask_clips_even_odd() {
        assert!(masked(true).contains("clip-rule=\"evenodd\""));
    }
}
