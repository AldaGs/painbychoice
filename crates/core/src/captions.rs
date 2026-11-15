//! Caption presets as data (ADR 0027).
//!
//! A preset is Sonant's caption `Style` JSON, verbatim in the fields PBC can
//! honour, so the built-ins port as a data file and an agent writes a new one
//! the same way. [`apply`] bakes a preset into an ordinary subtitled text
//! layer — font, fill, stroke, position and text animators — so everything it
//! sets stays editable as plain properties afterwards.

use kurbo::Vec2;
use serde::{Deserialize, Serialize};

use crate::node::{Node, Shape as NodeShape, SelectorMode, Stroke, TextAnimator, TextSelector, TextUnit};
use crate::value::{Color, Value};
use crate::vec3::Vec3;

/// One caption look. Sizes are fractions of the comp height, `outline` of the
/// font size, `duration` the per-word entry in milliseconds.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptionStyle {
    pub name: String,
    #[serde(default)]
    pub family: String,
    #[serde(default = "w700")]
    pub weight: f64,
    #[serde(default)]
    pub italic: bool,
    #[serde(default = "size")]
    pub size: f64,
    #[serde(default = "white")]
    pub color: String,
    /// The spoken word's color.
    #[serde(default = "white")]
    pub highlight: String,
    #[serde(default = "black")]
    pub stroke: String,
    #[serde(default = "outline")]
    pub outline: f64,
    #[serde(default)]
    pub uppercase: bool,
    /// Sonant's animation name: how each word enters as it is spoken.
    #[serde(default)]
    pub animation: String,
    #[serde(default = "duration")]
    pub duration: f64,
    #[serde(default = "half")]
    pub x: f64,
    #[serde(default = "y")]
    pub y: f64,
}

fn w700() -> f64 {
    700.0
}
fn size() -> f64 {
    0.065
}
fn white() -> String {
    "#ffffff".into()
}
fn black() -> String {
    "#000000".into()
}
fn outline() -> f64 {
    0.16
}
fn duration() -> f64 {
    320.0
}
fn half() -> f64 {
    0.5
}
fn y() -> f64 {
    0.75
}

/// The presets that ship with PBC (Sonant's).
pub fn builtin() -> &'static [CaptionStyle] {
    static ALL: std::sync::OnceLock<Vec<CaptionStyle>> = std::sync::OnceLock::new();
    ALL.get_or_init(|| serde_json::from_str(include_str!("caption_presets.json")).expect("caption_presets.json parses"))
}

fn hex(s: &str) -> Color {
    let s = s.trim_start_matches('#');
    let c = |i| s.get(i..i + 2).and_then(|h| u8::from_str_radix(h, 16).ok()).map_or(1.0, |v| v as f64 / 255.0);
    Color::rgb(c(0), c(2), c(4))
}

/// Bake `style` into a text layer in a `w`×`h` comp. A non-text layer is left alone.
pub fn apply(node: &mut Node, style: &CaptionStyle, w: f64, h: f64) {
    let px = style.size * h;
    let Some(NodeShape::Text { family, weight, italic, size, .. }) = node.shape.as_mut() else { return };
    *family = style.family.clone();
    *weight = Value::constant(style.weight);
    *italic = style.italic;
    *size = Value::constant(px.round());
    node.fill = Some(Value::constant(hex(&style.color)));
    node.stroke = (style.outline > 0.0).then(|| Stroke {
        color: Value::constant(hex(&style.stroke)),
        width: Value::constant(style.outline * px),
        style: Default::default(),
    });
    node.transform.position = Value::constant(Vec3::flat(style.x * w, style.y * h));
    if let Some(s) = node.subtitles.as_mut() {
        s.uppercase = style.uppercase;
    }
    node.text_animators = animators(style, px);
}

/// The spoken-word highlight, plus an entry each unspoken word sits at until
/// it is said. The entry is a word-time selector in Subtract mode: it selects
/// words not yet in, fading to nothing over `duration` once spoken.
/// ponytail: linear ramps; Sonant's spring/back easing overshoot is not modelled.
fn animators(style: &CaptionStyle, px: f64) -> Vec<TextAnimator> {
    let mut out = Vec::new();
    let mut entry = TextAnimator {
        name: "Entry".into(),
        selector: TextSelector::Karaoke { cumulative: true, ramp: (style.duration / 1000.0).max(0.0) },
        mode: SelectorMode::Subtract,
        ..TextAnimator::new(TextUnit::Words)
    };
    let gone = Some(Value::constant(0.0));
    let v2 = |x, y| Some(Value::constant(Vec2::new(x, y)));
    match style.animation.as_str() {
        "pop" | "elastic" | "bounce" => (entry.scale, entry.opacity) = (v2(40.0, 40.0), gone),
        "zoom-in" => (entry.scale, entry.opacity) = (v2(200.0, 200.0), gone),
        "slide-up" | "rise" | "swipe-up" => (entry.position, entry.opacity) = (v2(0.0, 0.5 * px), gone),
        "drop" => (entry.position, entry.opacity) = (v2(0.0, -0.8 * px), gone),
        "spin" => (entry.rotation, entry.opacity) = (Some(Value::constant(-90.0)), gone),
        "flip" => (entry.rotation_x, entry.opacity) = (Some(Value::constant(90.0)), gone),
        "fade" | "blur-in" | "glitch" | "neon" | "shake" => entry.opacity = gone,
        _ => {}
    }
    if entry.opacity.is_some() {
        out.push(entry);
    }
    if !matches!(style.animation.as_str(), "none" | "fade") {
        out.push(TextAnimator {
            name: "Highlight".into(),
            selector: TextSelector::Karaoke { cumulative: false, ramp: 0.0 },
            fill: Some(Value::constant(hex(&style.highlight))),
            ..TextAnimator::new(TextUnit::Words)
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_builtins_parse_and_bake_into_a_text_layer() {
        let all = builtin();
        assert_eq!(all.len(), 22);
        let pop = all.iter().find(|s| s.name == "Viral pop").unwrap();
        let mut n = Node::shape(
            1,
            "cap",
            NodeShape::Text {
                content: Value::constant(String::new()),
                family: String::new(),
                weight: Value::constant(400.0),
                italic: false,
                tracking: Value::constant(0.0),
                leading: Value::constant(0.0),
                size: Value::constant(10.0),
                align: crate::text::TextAlign::Center,
                max_width: None,
            },
        );
        apply(&mut n, pop, 1080.0, 1920.0);
        let Some(NodeShape::Text { family, .. }) = &n.shape else { panic!() };
        assert_eq!(family, "Montserrat");
        assert_eq!(n.text_animators.len(), 2, "entry and highlight");
        assert_eq!(n.text_animators[0].mode, SelectorMode::Subtract);
        assert!(n.stroke.is_some());
    }
}
