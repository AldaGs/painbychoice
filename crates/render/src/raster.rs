//! `Scene` → RGBA pixels, on the CPU.
//!
//! The offline rasterizer. It exists so a frame can be rendered **without a
//! GPU** — in a test, in CI, on a build machine, in `motion render` — which is
//! what makes compositing behaviour something we can assert on rather than
//! something we look at.
//!
//! # Its relationship to the GPU backend
//!
//! This is not the renderer the editor previews with, and it is not (yet) the
//! one an exported video should come from. vello rasterizes the preview, and
//! two rasterizers will never agree bit-for-bit: they antialias differently,
//! and that is not a defect in either.
//!
//! So the parity claim this backend supports is **structural, not per-pixel**:
//! the same layers, in the same order, with the same blend modes, mattes,
//! masks and opacities. That is the part that goes wrong in ways nobody
//! notices, and it is exactly what the SVG backend's tests already pin — this
//! one pins it in pixels as well. Per-pixel parity with the preview is a
//! question for the offscreen vello target, which belongs where a GPU device
//! already exists.
//!
//! # What it does not draw
//!
//! Footage. Decoding belongs to the shell's frame cache, not to a pure
//! function over a `Scene`, and wiring a decoder in here would put file IO
//! inside the one place that is supposed to be free of it. A raster item is
//! **reported**, never silently skipped — the same contract
//! [`crate::scene_to_svg_reporting`] follows, and for the same reason: a
//! backend that quietly omits something produces a frame that looks plausible
//! and is wrong.

use kurbo::{Affine, BezPath, PathEl};
use motion_core::{BlendMode, Color, ComposeMode, LayerGroup, Scene};
use tiny_skia::{
    BlendMode as SkBlend, FillRule, Paint, PathBuilder, Pixmap, PixmapPaint, Stroke, Transform,
};

/// Why a frame could not be rasterized.
#[derive(Debug)]
pub enum RasterError {
    /// A composition size the rasterizer cannot allocate.
    BadSize(u32, u32),
}

impl std::fmt::Display for RasterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RasterError::BadSize(w, h) => write!(f, "cannot rasterize a {w}x{h} frame"),
        }
    }
}

impl std::error::Error for RasterError {}

/// Rasterize one evaluated frame to non-premultiplied 8-bit RGBA.
///
/// Returns the pixels and the list of things this backend could not draw
/// exactly — see the module docs for why that list exists rather than a silent
/// omission.
pub fn rasterize(
    scene: &Scene,
    width: u32,
    height: u32,
    background: Color,
    scale: f64,
) -> Result<(Vec<u8>, Vec<String>), RasterError> {
    let (w, h) = (
        ((width as f64 * scale).round() as u32).max(1),
        ((height as f64 * scale).round() as u32).max(1),
    );
    let mut pixmap = Pixmap::new(w, h).ok_or(RasterError::BadSize(w, h))?;
    pixmap.fill(sk_color(background));

    let mut report = Vec::new();
    draw_range(scene, 0, scene.items.len(), None, &mut pixmap, scale, &mut report);

    // tiny-skia works premultiplied; every encoder here wants straight alpha.
    let mut out = Vec::with_capacity((w as usize) * (h as usize) * 4);
    for px in pixmap.pixels() {
        let a = px.alpha();
        let un = |c: u8| if a == 0 { 0 } else { ((c as u32 * 255) / a as u32).min(255) as u8 };
        out.extend_from_slice(&[un(px.red()), un(px.green()), un(px.blue()), a]);
    }
    Ok((out, report))
}

/// Draw items `[start, end)`, opening any isolation group that begins inside
/// the range.
///
/// Recursive rather than a stack-and-loop because an isolated group *is* a
/// nested render: it gets its own transparent pixmap, and the recursion is what
/// gives it one. `Scene`'s ranges nest but never partially overlap, so this
/// terminates on the same guarantee the SVG backend relies on.
fn draw_range(
    scene: &Scene,
    start: usize,
    end: usize,
    open: Option<&LayerGroup>,
    target: &mut Pixmap,
    scale: f64,
    report: &mut Vec<String>,
) {
    let mut i = start;
    while i < end {
        // The outermost group starting exactly here, if any. `nesting_order`
        // sorts enclosing ranges first, so the first match is the one to open.
        //
        // `open` is the group whose contents we are *already* drawing, skipped
        // by identity rather than by range. Comparing ranges instead looks
        // equivalent and is not: a matte's outer pair spans exactly the range
        // it is rendered into, so a range test skips it, and the matte inside
        // then applies its coverage rule straight to the backdrop — erasing
        // everything already drawn outside the matte, which is precisely what
        // the isolation exists to prevent.
        let group = scene
            .nesting_order()
            .into_iter()
            .find(|g| {
                g.start == i && g.end <= end && !open.is_some_and(|o| std::ptr::eq(*g, o))
            });
        match group {
            Some(g) => {
                draw_group(scene, g, target, scale, report);
                i = g.end;
            }
            None => {
                draw_item(scene, i, target, scale, report);
                i += 1;
            }
        }
    }
}

/// Render one isolated layer into its own pixmap, then composite it.
///
/// The isolation is the point: a group's opacity applies **once to the
/// composited result**, which is why two overlapping shapes in a 50% group do
/// not show through each other. Applying it per item would be the other
/// behaviour, and it is the one users report as a bug.
fn draw_group(
    scene: &Scene,
    g: &LayerGroup,
    target: &mut Pixmap,
    scale: f64,
    report: &mut Vec<String>,
) {
    let Some(mut layer) = Pixmap::new(target.width(), target.height()) else { return };
    draw_range(scene, g.start, g.end, Some(g), &mut layer, scale, report);

    // A mask clips the layer before it meets the backdrop. Core has already
    // resolved the geometry — including inverting it into a donut — so this
    // backend cannot disagree with the others about what "inverted" means.
    if let Some(mask) = &g.clip {
        if let Some(path) = to_sk_path(&mask.path, mask.transform, scale) {
            let mut cut = Pixmap::new(layer.width(), layer.height()).unwrap();
            let mut paint = Paint { anti_alias: true, ..Default::default() };
            paint.set_color(tiny_skia::Color::WHITE);
            let rule = if mask.even_odd { FillRule::EvenOdd } else { FillRule::Winding };
            cut.fill_path(&path, &paint, rule, Transform::identity(), None);
            // Keep the layer only where the mask painted.
            layer.draw_pixmap(
                0,
                0,
                cut.as_ref(),
                &PixmapPaint { blend_mode: SkBlend::DestinationIn, ..Default::default() },
                Transform::identity(),
                None,
            );
        }
    }

    // The effect stack runs on the isolated layer, before it meets the
    // backdrop — after the mask, because an effect works on what the layer
    // actually shows, and before blend and opacity, because those describe how
    // the finished layer combines rather than what it is. The same order the
    // editor's readback compositor uses, which is what lets the two agree.
    if !g.effects.is_empty() {
        apply_effects(&mut layer, &g.effects);
    }

    let mut paint = PixmapPaint {
        opacity: g.alpha.clamp(0.0, 1.0) as f32,
        blend_mode: match g.compose {
            ComposeMode::SrcOver => sk_blend(g.blend),
            ComposeMode::DestIn => SkBlend::DestinationIn,
            ComposeMode::DestOut => SkBlend::DestinationOut,
        },
        ..Default::default()
    };
    // A matte contributes coverage, not colour, and never its own opacity.
    if g.compose != ComposeMode::SrcOver {
        paint.opacity = 1.0;
    }
    target.draw_pixmap(0, 0, layer.as_ref(), &paint, Transform::identity(), None);
}

/// Run a layer's effect stack over its pixels.
///
/// tiny-skia works **premultiplied** and [`crate::fx`] works **straight**, so
/// this converts both ways around the call. That is not a detail to optimise
/// away: a tint or a brightness change is defined on a pixel's own colour, and
/// applying it to premultiplied values would make the result depend on the
/// layer's coverage — a 50%-transparent red would tint differently from an
/// opaque one.
///
/// The layer pixmap is the full canvas, so an effect that spreads beyond the
/// artwork (a blur's halo) has somewhere to spread to.
fn apply_effects(layer: &mut Pixmap, effects: &[motion_core::ResolvedEffect]) {
    let (w, h) = (layer.width() as usize, layer.height() as usize);
    let mut rgba = Vec::with_capacity(w * h * 4);
    for px in layer.pixels() {
        let a = px.alpha();
        let un = |c: u8| if a == 0 { 0 } else { ((c as u32 * 255) / a as u32).min(255) as u8 };
        rgba.extend_from_slice(&[un(px.red()), un(px.green()), un(px.blue()), a]);
    }

    crate::fx::apply_stack(&mut rgba, w, h, effects);

    for (px, out) in rgba.chunks_exact(4).zip(layer.pixels_mut()) {
        // `from_rgba8` premultiplies, which is exactly the trip back.
        *out = tiny_skia::ColorU8::from_rgba(px[0], px[1], px[2], px[3]).premultiply();
    }
}

fn draw_item(scene: &Scene, i: usize, target: &mut Pixmap, scale: f64, report: &mut Vec<String>) {
    let item = &scene.items[i];
    if item.image.is_some() {
        report.push(format!(
            "layer {} is footage; the CPU rasterizer draws vectors only, so it is \
             left out of this frame (the GPU backend draws it)",
            item.source.0
        ));
        return;
    }
    let Some(path) = to_sk_path(&item.path, item.transform, scale) else { return };
    let alpha = item.opacity.clamp(0.0, 1.0);

    if let Some(fill) = item.fill {
        let mut paint = Paint { anti_alias: true, ..Default::default() };
        paint.set_color(sk_color(with_alpha(fill, alpha)));
        target.fill_path(&path, &paint, FillRule::Winding, Transform::identity(), None);
    }
    if let Some((color, width)) = item.stroke {
        let mut paint = Paint { anti_alias: true, ..Default::default() };
        paint.set_color(sk_color(with_alpha(color, alpha)));
        // The stroke is scaled with the geometry: a 2px outline at 2× output
        // is 4px, or the render would not be the same picture larger.
        let stroke = Stroke { width: (width * scale) as f32, ..Default::default() };
        target.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
    }
}

/// kurbo path + its transform → a tiny-skia path in device space.
///
/// The transform is applied to the *points* rather than handed to tiny-skia as
/// a matrix, so that the output scale multiplies cleanly on top of it and a
/// stroke width can be scaled to match.
fn to_sk_path(path: &BezPath, transform: Affine, scale: f64) -> Option<tiny_skia::Path> {
    let xf = Affine::scale(scale) * transform;
    let mut b = PathBuilder::new();
    for el in path.elements() {
        match *el {
            PathEl::MoveTo(p) => {
                let p = xf * p;
                b.move_to(p.x as f32, p.y as f32);
            }
            PathEl::LineTo(p) => {
                let p = xf * p;
                b.line_to(p.x as f32, p.y as f32);
            }
            PathEl::QuadTo(a, p) => {
                let (a, p) = (xf * a, xf * p);
                b.quad_to(a.x as f32, a.y as f32, p.x as f32, p.y as f32);
            }
            PathEl::CurveTo(c1, c2, p) => {
                let (c1, c2, p) = (xf * c1, xf * c2, xf * p);
                b.cubic_to(
                    c1.x as f32,
                    c1.y as f32,
                    c2.x as f32,
                    c2.y as f32,
                    p.x as f32,
                    p.y as f32,
                );
            }
            PathEl::ClosePath => b.close(),
        }
    }
    b.finish()
}

fn with_alpha(mut c: Color, mul: f64) -> Color {
    c.a *= mul;
    c
}

fn sk_color(c: Color) -> tiny_skia::Color {
    tiny_skia::Color::from_rgba(
        c.r.clamp(0.0, 1.0) as f32,
        c.g.clamp(0.0, 1.0) as f32,
        c.b.clamp(0.0, 1.0) as f32,
        c.a.clamp(0.0, 1.0) as f32,
    )
    .unwrap_or(tiny_skia::Color::TRANSPARENT)
}

/// `BlendMode` is the standard sixteen, so this is a rename rather than an
/// approximation — the same relationship [`crate::css_blend`] has with CSS.
fn sk_blend(mode: BlendMode) -> SkBlend {
    match mode {
        BlendMode::Normal => SkBlend::SourceOver,
        BlendMode::Multiply => SkBlend::Multiply,
        BlendMode::Screen => SkBlend::Screen,
        BlendMode::Overlay => SkBlend::Overlay,
        BlendMode::Darken => SkBlend::Darken,
        BlendMode::Lighten => SkBlend::Lighten,
        BlendMode::ColorDodge => SkBlend::ColorDodge,
        BlendMode::ColorBurn => SkBlend::ColorBurn,
        BlendMode::HardLight => SkBlend::HardLight,
        BlendMode::SoftLight => SkBlend::SoftLight,
        BlendMode::Difference => SkBlend::Difference,
        BlendMode::Exclusion => SkBlend::Exclusion,
        BlendMode::Hue => SkBlend::Hue,
        BlendMode::Saturation => SkBlend::Saturation,
        BlendMode::Color => SkBlend::Color,
        BlendMode::Luminosity => SkBlend::Luminosity,
    }
}

/// The composition's bounding box in device pixels, for a `scale` render.
/// Exposed because the render queue needs to size its output before it has a
/// frame in hand.
pub fn output_size(width: f64, height: f64, scale: f64) -> (u32, u32) {
    (
        ((width * scale).round() as u32).max(1),
        ((height * scale).round() as u32).max(1),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use motion_core::{Comp, MatteMode, Node, Shape, Value};

    const BLACK: Color = Color { r: 0.0, g: 0.0, b: 0.0, a: 1.0 };

    /// The pixel at (x, y) as RGBA.
    fn px(buf: &[u8], w: u32, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * w + x) as usize) * 4;
        [buf[i], buf[i + 1], buf[i + 2], buf[i + 3]]
    }

    /// A square centred at `at` in composition space. Positioned explicitly
    /// because a node sits at the comp *origin* — its top-left — until it is
    /// moved, which is the thing a test asserting on a pixel must not assume.
    fn square_at(id: u64, size: f64, fill: Color, at: f64) -> Node {
        let mut n = Node::group(id, format!("n{id}"));
        n.shape = Some(Shape::Rect {
            size: Value::constant(kurbo::Vec2::new(size, size)),
            radius: Value::constant(0.0),
        });
        n.fill = Some(Value::constant(fill));
        n.transform.position = Value::constant(motion_core::Vec3::new(at, at, 0.0));
        n
    }

    fn square(id: u64, size: f64, fill: Color) -> Node {
        square_at(id, size, fill, 0.0)
    }

    fn render(comp: &Comp, scale: f64) -> (Vec<u8>, Vec<String>, u32) {
        let scene = motion_core::evaluate(comp, 0.0);
        let (w, _) = output_size(comp.width, comp.height, scale);
        let (buf, report) =
            rasterize(&scene, comp.width as u32, comp.height as u32, BLACK, scale).unwrap();
        (buf, report, w)
    }

    /// The floor: a shape reaches actual pixels, in the right place, over a
    /// background that is painted rather than left transparent.
    #[test]
    fn a_filled_shape_lands_on_the_canvas() {
        let comp = Comp::new(
            40.0,
            40.0,
            Node::group(0, "root").with_child(square_at(1, 20.0, Color::rgb(1.0, 0.0, 0.0), 20.0)),
        );
        let (buf, report, w) = render(&comp, 1.0);
        assert!(report.is_empty(), "{report:?}");
        assert_eq!(px(&buf, w, 20, 20), [255, 0, 0, 255], "centre is the shape");
        assert_eq!(px(&buf, w, 1, 1), [0, 0, 0, 255], "corner is the background");
    }

    /// An effect on a layer reaches the exported pixels.
    ///
    /// The claim Phase 3 is measured by: the offline backend is what `motion
    /// render` and every headless test go through, and until now it drew the
    /// layer and dropped its effect stack on the floor — so a comp with a tint
    /// previewed one way and exported another.
    #[test]
    fn an_effect_reaches_the_rasterized_pixels() {
        let mut layer = square_at(1, 20.0, Color::rgb(1.0, 1.0, 1.0), 20.0);
        layer.effects.push(motion_core::Effect::new(motion_core::EffectKind::Tint {
            color: Value::constant(Color::rgb(1.0, 0.0, 0.0)),
            amount: Value::constant(1.0),
        }));
        let comp = Comp::new(40.0, 40.0, Node::group(0, "root").with_child(layer));
        let (buf, report, w) = render(&comp, 1.0);
        assert!(report.is_empty(), "{report:?}");
        let [r, g, b, _] = px(&buf, w, 20, 20);
        assert!(r > 200 && g < 60 && b < 60, "a full tint should paint it red, got {r},{g},{b}");
    }

    /// A muted effect contributes nothing — it is dropped from the resolved
    /// stack rather than run as an identity pass, so this also pins that
    /// `resolve` really drops it.
    #[test]
    fn a_disabled_effect_changes_nothing() {
        let plain = square_at(1, 20.0, Color::rgb(1.0, 1.0, 1.0), 20.0);
        let mut muted = plain.clone();
        let mut effect = motion_core::Effect::new(motion_core::EffectKind::Tint {
            color: Value::constant(Color::rgb(1.0, 0.0, 0.0)),
            amount: Value::constant(1.0),
        });
        effect.enabled = false;
        muted.effects.push(effect);

        let a = render(&Comp::new(40.0, 40.0, Node::group(0, "root").with_child(plain)), 1.0).0;
        let b = render(&Comp::new(40.0, 40.0, Node::group(0, "root").with_child(muted)), 1.0).0;
        assert_eq!(a, b, "a muted effect must leave the frame byte-identical");
    }

    /// A blur spreads the layer beyond its own edges, and the isolated layer is
    /// the whole canvas so it has somewhere to spread to. A halo shaved off at
    /// the shape's bounding box is the classic symptom of compositing an effect
    /// into a target sized to the artwork.
    #[test]
    fn a_blur_spreads_past_the_shapes_edge() {
        let mut layer = square_at(1, 10.0, Color::rgb(1.0, 1.0, 1.0), 20.0);
        layer.effects.push(motion_core::Effect::new(motion_core::EffectKind::GaussianBlur {
            radius: Value::constant(4.0),
        }));
        let comp = Comp::new(40.0, 40.0, Node::group(0, "root").with_child(layer));
        let (buf, _, w) = render(&comp, 1.0);

        // The square spans 15..25; a few pixels outside it should have picked
        // up some light from the blur, and the far corner none.
        let [r, ..] = px(&buf, w, 27, 20);
        assert!(r > 0, "the blur should reach past the edge");
        assert_eq!(px(&buf, w, 1, 1), [0, 0, 0, 255], "but not across the whole frame");
    }

    /// Effects run **before** the layer's opacity, not after: opacity says how
    /// the finished layer combines with the backdrop, and a stack that ran
    /// after it would be adjusting the composite instead of the layer.
    #[test]
    fn an_effect_runs_inside_the_layer_not_on_the_composite() {
        let mut layer = square_at(1, 20.0, Color::rgb(1.0, 1.0, 1.0), 20.0);
        layer.transform.opacity = Value::constant(0.5);
        layer.effects.push(motion_core::Effect::new(motion_core::EffectKind::Tint {
            color: Value::constant(Color::rgb(1.0, 0.0, 0.0)),
            amount: Value::constant(1.0),
        }));
        let comp = Comp::new(40.0, 40.0, Node::group(0, "root").with_child(layer));
        let (buf, _, w) = render(&comp, 1.0);
        // Red at half strength over black: the tint is full, the *layer* is
        // half. Tinting the composite instead would drag the black backdrop
        // into the tint and come out a different colour entirely.
        let [r, g, b, _] = px(&buf, w, 20, 20);
        assert!((100..=160).contains(&r), "half-strength red, got {r}");
        assert!(g < 40 && b < 40, "and no green or blue, got {g},{b}");
    }

    /// Scale multiplies the picture, it does not crop or letterbox it: the same
    /// frame, larger. This is what a "render at 2×" setting means, and getting
    /// it wrong is invisible until someone compares two exports.
    #[test]
    fn scale_renders_the_same_picture_larger() {
        let comp = Comp::new(
            40.0,
            40.0,
            Node::group(0, "root").with_child(square_at(1, 20.0, Color::rgb(1.0, 0.0, 0.0), 20.0)),
        );
        let (buf, _, w) = render(&comp, 2.0);
        assert_eq!(w, 80);
        assert_eq!(px(&buf, w, 40, 40), [255, 0, 0, 255], "still the shape at the centre");
        assert_eq!(px(&buf, w, 2, 2), [0, 0, 0, 255], "still background at the corner");
    }

    /// Determinism, in pixels. `evaluate` is pure and this backend has no state
    /// of its own, so the same frame must render byte-identically every time —
    /// the property every render-farm and every caching decision rests on.
    #[test]
    fn the_same_frame_renders_byte_identically_twice() {
        let comp = Comp::new(
            32.0,
            32.0,
            Node::group(0, "root")
                .with_child(square(1, 20.0, Color::rgb(0.2, 0.4, 0.9)))
                .with_child(square(2, 10.0, Color::rgb(1.0, 1.0, 0.0))),
        );
        let (a, _, _) = render(&comp, 1.0);
        let (b, _, _) = render(&comp, 1.0);
        assert_eq!(a, b);
    }

    /// A group's opacity applies once to the composited result. Two identical
    /// overlapping half-opaque shapes inside one 50% layer must not stack up to
    /// something more opaque than the layer itself.
    /// A blend mode reaches actual pixels through the isolation path.
    ///
    /// This is the group path end to end: the layer is rendered into its own
    /// pixmap and composited with its mode, rather than painted straight on.
    /// `Screen` over red lifts it toward white; `Multiply` leaves it red.
    ///
    /// Note what is *not* tested here: a group fading as a unit. A blend mode
    /// covers a layer's **own artwork**, not its children — a child composites
    /// on its own terms (`Node::blend`) — so a plain group with two children
    /// isolates nothing, and the per-item-versus-per-group distinction only
    /// becomes visible through a precomp. `core`'s own tests pin that at the
    /// model level.
    fn blended_over_red(mode: BlendMode) -> [u8; 4] {
        let mut top = square_at(2, 20.0, Color::rgb(0.0, 0.0, 1.0), 16.0);
        top.blend = mode;
        let comp = Comp::new(
            32.0,
            32.0,
            Node::group(0, "root")
                .with_child(square_at(1, 30.0, Color::rgb(1.0, 0.0, 0.0), 16.0))
                .with_child(top),
        );
        let (buf, _, w) = render(&comp, 1.0);
        px(&buf, w, 16, 16)
    }

    #[test]
    fn a_blend_mode_reaches_the_pixels() {
        let [_, _, mb, _] = blended_over_red(BlendMode::Multiply);
        let [sr, _, sb, _] = blended_over_red(BlendMode::Screen);
        assert_eq!(mb, 0, "blue multiplied by red keeps no blue");
        assert_eq!(sb, 255, "screen keeps the blue");
        assert_eq!(sr, 255, "and the red underneath");
    }

    /// A track matte cuts the content, in pixels. The matte layer itself must
    /// not appear: it contributes shape, not colour.
    #[test]
    fn a_track_matte_cuts_the_content_it_covers() {
        let mut content = square_at(1, 30.0, Color::rgb(1.0, 0.0, 0.0), 20.0);
        content.matte = Some(MatteMode::Alpha);
        // A small matte in the middle, so the content survives only there.
        let matte = square_at(2, 10.0, Color::rgb(0.0, 1.0, 0.0), 20.0);
        let comp = Comp::new(
            40.0,
            40.0,
            Node::group(0, "root").with_child(content).with_child(matte),
        );
        let (buf, _, w) = render(&comp, 1.0);
        assert_eq!(px(&buf, w, 20, 20), [255, 0, 0, 255], "content kept under the matte");
        assert_eq!(px(&buf, w, 20, 32), [0, 0, 0, 255], "content cut away outside it");
        // The matte's own green never reaches the frame.
        assert!(
            !buf.chunks(4).any(|p| p[1] > 40 && p[0] < 40),
            "the matte layer was painted instead of consumed"
        );
    }

    /// Footage is not drawn here, and that must be *said*. A silently missing
    /// layer is the failure this whole reporting seam exists to prevent.
    #[test]
    fn footage_is_reported_rather_than_silently_dropped() {
        use motion_core::asset::AssetId;
        let mut n = square(1, 20.0, Color::rgb(1.0, 0.0, 0.0));
        n.shape = Some(Shape::Image {
            size: Value::constant(kurbo::Vec2::new(20.0, 20.0)),
            asset: AssetId(0),
            time_remap: None,
        });
        let comp = Comp::new(40.0, 40.0, Node::group(0, "root").with_child(n));
        let scene = motion_core::evaluate(&comp, 0.0);
        let has_image = scene.items.iter().any(|i| i.image.is_some());
        let (_, report) = rasterize(&scene, 40, 40, BLACK, 1.0).unwrap();
        if has_image {
            assert!(!report.is_empty(), "footage must be reported");
            assert!(report[0].contains("footage"), "{report:?}");
        }
    }
}


/// Average equally sized straight-RGBA8 frames — motion blur's accumulate.
///
/// Weighted by alpha, so a transparent sample adds coverage but no colour:
/// a shape sweeping over an empty background blurs to its own colour, not
/// toward black.
pub fn average_frames(frames: &[Vec<u8>]) -> Vec<u8> {
    let Some(first) = frames.first() else { return Vec::new() };
    if frames.len() == 1 {
        return first.clone();
    }
    let n = frames.len() as f32;
    let mut out = vec![0u8; first.len()];
    // Row-sized bands across threads: a preview frame is ~15 MB per sample,
    // and this runs once per played frame when motion blur is on.
    let threads = std::thread::available_parallelism().map_or(1, |t| t.get());
    let band = (out.len() / 4).div_ceil(threads).max(1) * 4;
    std::thread::scope(|scope| {
        for (b, chunk) in out.chunks_mut(band).enumerate() {
            scope.spawn(move || {
                let base = b * band;
                for (i, px) in chunk.chunks_exact_mut(4).enumerate() {
                    let at = base + i * 4;
                    let (mut rgb, mut a) = ([0f32; 3], 0f32);
                    for f in frames {
                        let s = &f[at..at + 4];
                        let sa = s[3] as f32;
                        for c in 0..3 {
                            rgb[c] += s[c] as f32 * sa;
                        }
                        a += sa;
                    }
                    if a > 0.0 {
                        for c in 0..3 {
                            px[c] = (rgb[c] / a).round() as u8;
                        }
                    }
                    px[3] = (a / n).round() as u8;
                }
            });
        }
    });
    out
}

#[cfg(test)]
mod average_tests {
    use super::average_frames;

    /// End to end: a box sliding 40px per frame, rendered with motion blur,
    /// leaves partial coverage behind it that a sharp render does not.
    #[test]
    fn a_moving_box_smears_with_motion_blur() {
        use motion_core::{Comp, Keyframe, Node, Project, Shape, Track, Value, Vec3};
        let mut n = Node::group(1, "box");
        n.shape = Some(Shape::Rect {
            size: Value::constant(kurbo::Vec2::new(20.0, 20.0)),
            radius: Value::constant(0.0),
        });
        n.fill = Some(Value::constant(motion_core::Color::rgb(1.0, 1.0, 1.0)));
        n.transform.position = Value::Keyframed(Track::new(vec![
            Keyframe::linear(0, Vec3::flat(0.0, 50.0)),
            Keyframe::linear(10, Vec3::flat(400.0, 50.0)),
        ]));
        let mut root = Node::group(0, "root");
        root.children = vec![n];
        let mut comp = Comp::new(500.0, 100.0, root);
        comp.motion_blur.enabled = true;
        let project = Project::single(comp.clone());
        let render = |offsets: Vec<f64>| {
            let frames: Vec<Vec<u8>> = offsets
                .into_iter()
                .map(|s| {
                    let scene = motion_core::evaluate_comp_sample(
                        &project, project.root, 5.0, s, motion_core::mat4::Mat4::IDENTITY,
                    );
                    super::rasterize(&scene, 500, 100, motion_core::Color::rgb(0.0, 0.0, 0.0), 1.0).unwrap().0
                })
                .collect();
            average_frames(&frames)
        };
        let partial = |px: &[u8]| px.chunks_exact(4).filter(|p| p[0] > 10 && p[0] < 245).count();
        let sharp = render(vec![0.0]);
        let blurred = render(comp.motion_blur.offsets());
        assert!(partial(&blurred) > partial(&sharp) + 100, "{} vs {}", partial(&blurred), partial(&sharp));
    }

    #[test]
    fn averaging_blends_coverage_and_keeps_colour() {
        let red = vec![255, 0, 0, 255];
        let clear = vec![0, 0, 0, 0];
        assert_eq!(average_frames(&[red.clone(), clear]), vec![255, 0, 0, 128]);
        assert_eq!(average_frames(&[red.clone()]), red);
    }
}
