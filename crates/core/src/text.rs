//! Text shaping: a string plus a font spec, shaped and laid out into **glyph
//! outlines** as one [`BezPath`].
//!
//! ## Why outlines rather than glyph runs
//!
//! [`crate::node::Shape`] resolves to a `BezPath` and everything downstream —
//! the SVG backend, the live vello canvas, fill, stroke, the transform stack —
//! consumes only that. Turning shaped glyphs into outlines here means a text
//! layer animates through the *existing* pipeline with no renderer changes, and
//! the offline `motion` binary can render text as well as the GPU shell can.
//! The alternative (handing vello glyph runs) would have been live-only.
//!
//! Shaping is real, not a hand-rolled advance walk: parley does bidi, script
//! segmentation, font fallback, line breaking, and alignment, and skrifa pulls
//! the outlines for the shaped glyph ids.
//!
//! ## The determinism caveat
//!
//! Every other part of the engine is deterministic: `evaluate(doc, t)` is a pure
//! function, so a render matches the preview and tests can pin output. **Text is
//! the one exception**, because families are resolved against the *system* font
//! set: a `.pbc` naming "Futura" draws Futura on a machine that has it and a
//! fallback on one that doesn't. The document stores the family *name* (never
//! font bytes), and a name that resolves to nothing falls back through the
//! generic stack rather than failing. Tests here therefore assert structural
//! facts (a non-empty path, alignment/size relationships) rather than exact
//! coordinates, which would be machine-dependent.

use std::cell::RefCell;

use kurbo::{BezPath, Point, Rect, Shape as _};
use serde::{Deserialize, Serialize};

use parley::{
    Alignment, AlignmentOptions, FontContext, FontFamily, FontFamilyName, LayoutContext,
    PositionedLayoutItem, StyleProperty,
};
use skrifa::{
    instance::{LocationRef, NormalizedCoord, Size},
    outline::{DrawSettings, OutlinePen},
    FontRef, GlyphId, MetadataProvider,
};

thread_local! {
    /// One font context per thread, reused across shapes. It owns the system
    /// font collection — enumerating that is expensive, so it must not be
    /// rebuilt per frame. Same discipline as `expr.rs`'s script engine.
    static FONT_CTX: RefCell<FontContext> = RefCell::new(FontContext::new());
    /// Layout scratch, likewise reused (it caches shaping work internally).
    static LAYOUT_CTX: RefCell<LayoutContext<()>> = RefCell::new(LayoutContext::new());
}

/// How lines sit relative to the text block's own box.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum TextAlign {
    #[default]
    Left,
    Center,
    Right,
}

impl TextAlign {
    pub fn label(self) -> &'static str {
        match self {
            TextAlign::Left => "left",
            TextAlign::Center => "center",
            TextAlign::Right => "right",
        }
    }

    /// Every alignment, in picker order.
    pub const ALL: [TextAlign; 3] = [TextAlign::Left, TextAlign::Center, TextAlign::Right];

    fn to_parley(self) -> Alignment {
        match self {
            TextAlign::Left => Alignment::Left,
            TextAlign::Center => Alignment::Center,
            TextAlign::Right => Alignment::Right,
        }
    }
}

/// Collects skrifa's outline callbacks into a [`BezPath`], moving each glyph to
/// its shaped position and flipping the y axis.
///
/// Fonts are y-**up** (from the baseline); the layout — and the rest of this
/// engine — is y-**down**. So a glyph point `(x, y)` lands at
/// `(origin.x + x, origin.y − y)`, which is the only coordinate conversion in
/// the text path.
struct PathPen {
    path: BezPath,
    /// Where this glyph's baseline origin sits, already including the
    /// block-centering offset.
    origin: (f64, f64),
}

impl PathPen {
    fn at(&self, x: f32, y: f32) -> Point {
        Point::new(self.origin.0 + x as f64, self.origin.1 - y as f64)
    }
}

impl OutlinePen for PathPen {
    fn move_to(&mut self, x: f32, y: f32) {
        self.path.move_to(self.at(x, y));
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.path.line_to(self.at(x, y));
    }
    fn quad_to(&mut self, cx: f32, cy: f32, x: f32, y: f32) {
        self.path.quad_to(self.at(cx, cy), self.at(x, y));
    }
    fn curve_to(&mut self, cx0: f32, cy0: f32, cx1: f32, cy1: f32, x: f32, y: f32) {
        self.path.curve_to(self.at(cx0, cy0), self.at(cx1, cy1), self.at(x, y));
    }
    fn close(&mut self) {
        self.path.close_path();
    }
}

/// Shape `content` and return its glyph outlines as one path, **centred on the
/// origin** — the same convention `Rect`/`Ellipse` follow, so a text layer's
/// anchor, rotation, and scale behave like every other shape's.
///
/// `family` is a system font family name; empty (or unresolvable) falls back to
/// the generic sans-serif stack. `size` is the font size in pixels. `max_width`
/// wraps the text when `Some` and leaves it on one line when `None` — which is
/// also what alignment is measured against.
pub fn text_to_path(
    content: &str,
    family: &str,
    size: f64,
    align: TextAlign,
    max_width: Option<f64>,
) -> BezPath {
    // A guard, not an optimization: parley is happy to lay out nothing, but the
    // callers below would divide a zero-size block when centring it.
    if content.is_empty() || !size.is_finite() || size <= 0.0 {
        return BezPath::new();
    }

    FONT_CTX.with(|fcx| {
        LAYOUT_CTX.with(|lcx| {
            let mut fcx = fcx.borrow_mut();
            let mut lcx = lcx.borrow_mut();
            // scale 1.0: this is document space, not physical pixels — the
            // canvas transform handles device scaling later. No quantization for
            // the same reason (rounding here would fight the zoom).
            let mut builder = lcx.ranged_builder(&mut fcx, content, 1.0, false);
            builder.push_default(StyleProperty::FontSize(size as f32));
            builder.push_default(StyleProperty::FontFamily(font_family(family)));
            let mut layout: parley::Layout<()> = builder.build(content);

            let advance = max_width.map(|w| w as f32);
            layout.break_all_lines(advance);
            layout.align(align.to_parley(), AlignmentOptions::default());

            // Centre the whole block on the origin. Alignment has already placed
            // each line inside the block, so this only moves the block itself.
            let (w, h) = (layout.width() as f64, layout.height() as f64);
            let (ox, oy) = (-w / 2.0, -h / 2.0);

            let mut path = BezPath::new();
            for line in layout.lines() {
                for item in line.items() {
                    let PositionedLayoutItem::GlyphRun(run) = item else {
                        // Inline boxes are a rich-text feature we don't author.
                        continue;
                    };
                    let font = run.run().font();
                    let Ok(font_ref) = FontRef::from_index(font.data.data(), font.index) else {
                        continue;
                    };
                    let outlines = font_ref.outline_glyphs();
                    // Variable-font axis positions. parley hands these back as
                    // raw `i16` bits and skrifa wants the `F2Dot14` newtype over
                    // the same bits — converted rather than transmuted, since a
                    // per-run allocation is nothing next to outlining glyphs.
                    let coords: Vec<NormalizedCoord> = run
                        .run()
                        .normalized_coords()
                        .iter()
                        .map(|c| NormalizedCoord::from_bits(*c))
                        .collect();
                    let size = Size::new(run.run().font_size());
                    for glyph in run.positioned_glyphs() {
                        let Some(outline) = outlines.get(GlyphId::from(glyph.id)) else {
                            continue;
                        };
                        let mut pen = PathPen {
                            path: std::mem::take(&mut path),
                            origin: (ox + glyph.x as f64, oy + glyph.y as f64),
                        };
                        // `DrawSettings` isn't `Copy`, so it's rebuilt per glyph
                        // (it's a couple of words over the borrowed coords).
                        let settings = DrawSettings::unhinted(size, LocationRef::new(&coords));
                        // A glyph that fails to draw is skipped, not fatal — the
                        // same "a bad part falls back, the frame survives" rule
                        // the expression engine follows.
                        let _ = outline.draw(settings, &mut pen);
                        path = pen.path;
                    }
                }
            }
            path
        })
    })
}

/// Whether `family` names a font this machine actually has.
///
/// A blank name is *not* missing — it means "use the default" deliberately, so
/// it reports `true` and never warns. A name that isn't installed still draws
/// (parley falls back), which is exactly why this has to be asked separately:
/// the drawing succeeds, so nothing else would reveal the substitution.
pub fn font_exists(family: &str) -> bool {
    let name = family.trim();
    if name.is_empty() {
        return true;
    }
    FONT_CTX.with(|f| f.borrow_mut().collection.family_id(name).is_some())
}

/// Every font family installed on this machine, sorted case-insensitively —
/// the picker's list. Enumerating is cheap after the first call because the
/// collection is built once per thread and reused.
pub fn system_families() -> Vec<String> {
    FONT_CTX.with(|f| {
        let mut names: Vec<String> =
            f.borrow_mut().collection.family_names().map(str::to_string).collect();
        names.sort_by_key(|n| n.to_lowercase());
        names.dedup();
        names
    })
}

/// The family to shape with. An empty name means "whatever the system calls
/// sans-serif", which is also the fallback a named family gets when it isn't
/// installed — so a `.pbc` from another machine still draws something.
fn font_family(family: &str) -> FontFamily<'_> {
    let name = family.trim();
    if name.is_empty() {
        FontFamily::Source(std::borrow::Cow::Borrowed("sans-serif"))
    } else {
        FontFamily::Single(FontFamilyName::Named(std::borrow::Cow::Borrowed(name)))
    }
}

/// The tight bounds of shaped text, for the editor's picking and fitting. Empty
/// text has no box, so this is `None` rather than a zero rect at the origin.
pub fn text_bounds(
    content: &str,
    family: &str,
    size: f64,
    align: TextAlign,
    max_width: Option<f64>,
) -> Option<Rect> {
    let path = text_to_path(content, family, size, align, max_width);
    (!path.is_empty()).then(|| path.bounding_box())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Shaping runs against whatever fonts the machine has, so these assert
    /// *structure* — never exact coordinates, which differ per font set.
    #[test]
    fn empty_or_degenerate_text_has_no_path() {
        assert!(text_to_path("", "", 32.0, TextAlign::Left, None).is_empty());
        assert!(text_to_path("hi", "", 0.0, TextAlign::Left, None).is_empty());
        assert!(text_to_path("hi", "", f64::NAN, TextAlign::Left, None).is_empty());
    }

    #[test]
    fn text_shapes_to_a_non_empty_centred_path() {
        let path = text_to_path("Hello", "", 32.0, TextAlign::Left, None);
        assert!(!path.is_empty(), "shaping produced outlines");
        let b = path.bounding_box();
        // Centred on the origin: the box straddles it in both axes rather than
        // starting there (the convention Rect/Ellipse already follow).
        assert!(b.min_x() < 0.0 && b.max_x() > 0.0, "straddles x=0: {b:?}");
        assert!(b.min_y() < 0.0 && b.max_y() > 0.0, "straddles y=0: {b:?}");
    }

    #[test]
    fn a_bigger_size_makes_a_bigger_box() {
        let small = text_bounds("Hello", "", 16.0, TextAlign::Left, None).unwrap();
        let big = text_bounds("Hello", "", 64.0, TextAlign::Left, None).unwrap();
        assert!(big.width() > small.width(), "{} vs {}", big.width(), small.width());
        assert!(big.height() > small.height());
    }

    #[test]
    fn wrapping_makes_the_block_taller_and_narrower() {
        let one_line = text_bounds("the quick brown fox", "", 24.0, TextAlign::Left, None).unwrap();
        let wrapped =
            text_bounds("the quick brown fox", "", 24.0, TextAlign::Left, Some(60.0)).unwrap();
        assert!(wrapped.height() > one_line.height(), "wrapping added lines");
        assert!(wrapped.width() < one_line.width(), "wrapping narrowed the block");
    }

    /// An unknown family must fall back rather than producing nothing — the
    /// cross-machine case the system-font choice makes real.
    #[test]
    fn an_unknown_family_falls_back_to_something_drawable() {
        let path = text_to_path("Hello", "NoSuchFontFamily-XYZZY", 32.0, TextAlign::Left, None);
        assert!(!path.is_empty(), "fell back to an installed font");
    }

    #[test]
    fn a_missing_family_is_detectable_but_a_blank_one_is_not_missing() {
        assert!(!font_exists("NoSuchFontFamily-XYZZY"), "an absent family is reported missing");
        // Blank means "use the default" on purpose — never a warning.
        assert!(font_exists(""), "blank is deliberate, not missing");
        assert!(font_exists("   "), "whitespace-only is blank too");
    }

    #[test]
    fn the_system_family_list_is_usable_and_self_consistent() {
        let families = system_families();
        assert!(!families.is_empty(), "the machine has fonts to offer");
        // Sorted case-insensitively, so the picker reads alphabetically.
        let mut sorted = families.clone();
        sorted.sort_by_key(|n| n.to_lowercase());
        assert_eq!(families, sorted, "already in picker order");
        // Anything the list offers must pass the existence check the warning
        // keys off, or picking a font from the picker would warn about itself.
        for name in families.iter().take(25) {
            assert!(font_exists(name), "listed family '{name}' should exist");
        }
    }
}
