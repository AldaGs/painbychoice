//! Text animators in the editor: After Effects' per-character / word / line
//! animation system.
//!
//! A text layer carries a stack of [`TextAnimator`]s. Each has a selector
//! (Range, Wiggly, Stagger or Karaoke) deciding *how much* of every unit is
//! selected, and a set of properties (position, scale, rotation, opacity,
//! tracking, fill…) describing what happens to a fully selected unit. Every
//! numeric property and range control is a `Value`, and each one is exposed as
//! a [`PropKind::TextAnim`], so it gets a stopwatch, a dopesheet row, curves,
//! retiming and copy/paste through the same machinery every other property
//! uses.
//!
//! This module holds the parameter addressing ([`TextAnimParam`]), the panel's
//! snapshot ([`TextAnimInfo`]), the edit ops ([`TextAnimOp`]), the presets and
//! the panel UI.

use crate::*;
use motion_core::node::{AnchorGrouping, RangeShape, TextAnimator, TextSelector, TextUnit};

/// One animatable value inside one text animator. Ordered because it rides in
/// [`PropKind::TextAnim`], the key of a `BTreeSet`: declaration order is
/// dopesheet row order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum TextAnimParam {
    // Selector controls.
    RangeStart,
    RangeEnd,
    RangeOffset,
    RangeAmount,
    WiggleMax,
    WiggleMin,
    WiggleRate,
    StaggerAmount,
    // Properties.
    Position,
    Anchor,
    Scale,
    Rotation,
    Skew,
    Opacity,
    Tracking,
    Fill,
    Stroke,
    StrokeWidth,
}

impl TextAnimParam {
    /// The properties an animator can hold, in the "Add" menu's order.
    pub(crate) const PROPERTIES: [TextAnimParam; 10] = [
        TextAnimParam::Position,
        TextAnimParam::Anchor,
        TextAnimParam::Scale,
        TextAnimParam::Rotation,
        TextAnimParam::Skew,
        TextAnimParam::Opacity,
        TextAnimParam::Tracking,
        TextAnimParam::Fill,
        TextAnimParam::Stroke,
        TextAnimParam::StrokeWidth,
    ];

    pub(crate) fn label(self) -> &'static str {
        match self {
            TextAnimParam::RangeStart => "Start",
            TextAnimParam::RangeEnd => "End",
            TextAnimParam::RangeOffset => "Offset",
            TextAnimParam::RangeAmount => "Amount",
            TextAnimParam::WiggleMax => "Max Amount",
            TextAnimParam::WiggleMin => "Min Amount",
            TextAnimParam::WiggleRate => "Wiggles/Sec",
            TextAnimParam::StaggerAmount => "Influence",
            TextAnimParam::Position => "Position",
            TextAnimParam::Anchor => "Anchor Point",
            TextAnimParam::Scale => "Scale",
            TextAnimParam::Rotation => "Rotation",
            TextAnimParam::Skew => "Skew",
            TextAnimParam::Opacity => "Opacity",
            TextAnimParam::Tracking => "Tracking",
            TextAnimParam::Fill => "Fill Color",
            TextAnimParam::Stroke => "Stroke Color",
            TextAnimParam::StrokeWidth => "Stroke Width",
        }
    }

    fn is_property(self) -> bool {
        Self::PROPERTIES.contains(&self)
    }

    /// What a freshly added property starts at: a visible change, so adding
    /// one shows what it does.
    fn seed(self, a: &mut TextAnimator) {
        let c = |v: f64| Some(Value::constant(v));
        let v2 = |x: f64, y: f64| Some(Value::constant(Vec2::new(x, y)));
        match self {
            TextAnimParam::Position => a.position = v2(0.0, -40.0),
            TextAnimParam::Anchor => a.anchor = v2(0.0, 0.0),
            TextAnimParam::Scale => a.scale = v2(200.0, 200.0),
            TextAnimParam::Rotation => a.rotation = c(45.0),
            TextAnimParam::Skew => a.skew = c(20.0),
            TextAnimParam::Opacity => a.opacity = c(0.0),
            TextAnimParam::Tracking => a.tracking = c(200.0),
            TextAnimParam::Fill => a.fill = Some(Value::constant(MColor::rgb(1.0, 0.25, 0.25))),
            TextAnimParam::Stroke => a.stroke = Some(Value::constant(MColor::rgb(1.0, 1.0, 1.0))),
            TextAnimParam::StrokeWidth => a.stroke_width = c(2.0),
            _ => {}
        }
    }

    fn clear(self, a: &mut TextAnimator) {
        match self {
            TextAnimParam::Position => a.position = None,
            TextAnimParam::Anchor => a.anchor = None,
            TextAnimParam::Scale => a.scale = None,
            TextAnimParam::Rotation => a.rotation = None,
            TextAnimParam::Skew => a.skew = None,
            TextAnimParam::Opacity => a.opacity = None,
            TextAnimParam::Tracking => a.tracking = None,
            TextAnimParam::Fill => a.fill = None,
            TextAnimParam::Stroke => a.stroke = None,
            TextAnimParam::StrokeWidth => a.stroke_width = None,
            _ => {}
        }
    }

    /// Display unit and drag speed for the panel.
    fn speed(self) -> f64 {
        match self {
            TextAnimParam::Opacity => 0.01,
            TextAnimParam::WiggleRate => 0.05,
            _ => 1.0,
        }
    }
}

/// Borrow one animatable value of an animator. `None` when it doesn't have it:
/// a property not added, or a control of another selector kind.
pub(crate) fn anim_value(a: &TextAnimator, p: TextAnimParam) -> Option<PropRef<'_>> {
    use TextAnimParam as P;
    Some(match (p, &a.selector) {
        (P::RangeStart, TextSelector::Range { start, .. }) => PropRef::Num(start),
        (P::RangeEnd, TextSelector::Range { end, .. }) => PropRef::Num(end),
        (P::RangeOffset, TextSelector::Range { offset, .. }) => PropRef::Num(offset),
        (P::RangeAmount, TextSelector::Range { amount, .. }) => PropRef::Num(amount),
        (P::WiggleMax, TextSelector::Wiggly { max, .. }) => PropRef::Num(max),
        (P::WiggleMin, TextSelector::Wiggly { min, .. }) => PropRef::Num(min),
        (P::WiggleRate, TextSelector::Wiggly { wiggles, .. }) => PropRef::Num(wiggles),
        (P::StaggerAmount, TextSelector::Stagger { amount, .. }) => PropRef::Num(amount),
        (P::Position, _) => PropRef::Vec2(a.position.as_ref()?),
        (P::Anchor, _) => PropRef::Vec2(a.anchor.as_ref()?),
        (P::Scale, _) => PropRef::Vec2(a.scale.as_ref()?),
        (P::Rotation, _) => PropRef::Num(a.rotation.as_ref()?),
        (P::Skew, _) => PropRef::Num(a.skew.as_ref()?),
        (P::Opacity, _) => PropRef::Num(a.opacity.as_ref()?),
        (P::Tracking, _) => PropRef::Num(a.tracking.as_ref()?),
        (P::Fill, _) => PropRef::Color(a.fill.as_ref()?),
        (P::Stroke, _) => PropRef::Color(a.stroke.as_ref()?),
        (P::StrokeWidth, _) => PropRef::Num(a.stroke_width.as_ref()?),
        _ => return None,
    })
}

/// Mutable twin of [`anim_value`]; the two must agree.
pub(crate) fn anim_value_mut(a: &mut TextAnimator, p: TextAnimParam) -> Option<PropRefMut<'_>> {
    use TextAnimParam as P;
    Some(match (p, &mut a.selector) {
        (P::RangeStart, TextSelector::Range { start, .. }) => PropRefMut::Num(start),
        (P::RangeEnd, TextSelector::Range { end, .. }) => PropRefMut::Num(end),
        (P::RangeOffset, TextSelector::Range { offset, .. }) => PropRefMut::Num(offset),
        (P::RangeAmount, TextSelector::Range { amount, .. }) => PropRefMut::Num(amount),
        (P::WiggleMax, TextSelector::Wiggly { max, .. }) => PropRefMut::Num(max),
        (P::WiggleMin, TextSelector::Wiggly { min, .. }) => PropRefMut::Num(min),
        (P::WiggleRate, TextSelector::Wiggly { wiggles, .. }) => PropRefMut::Num(wiggles),
        (P::StaggerAmount, TextSelector::Stagger { amount, .. }) => PropRefMut::Num(amount),
        (P::Position, _) => PropRefMut::Vec2(a.position.as_mut()?),
        (P::Anchor, _) => PropRefMut::Vec2(a.anchor.as_mut()?),
        (P::Scale, _) => PropRefMut::Vec2(a.scale.as_mut()?),
        (P::Rotation, _) => PropRefMut::Num(a.rotation.as_mut()?),
        (P::Skew, _) => PropRefMut::Num(a.skew.as_mut()?),
        (P::Opacity, _) => PropRefMut::Num(a.opacity.as_mut()?),
        (P::Tracking, _) => PropRefMut::Num(a.tracking.as_mut()?),
        (P::Fill, _) => PropRefMut::Color(a.fill.as_mut()?),
        (P::Stroke, _) => PropRefMut::Color(a.stroke.as_mut()?),
        (P::StrokeWidth, _) => PropRefMut::Num(a.stroke_width.as_mut()?),
        _ => return None,
    })
}

/// Every param an animator currently has, selector controls first.
pub(crate) fn anim_params(a: &TextAnimator) -> Vec<TextAnimParam> {
    use TextAnimParam as P;
    [
        P::RangeStart, P::RangeEnd, P::RangeOffset, P::RangeAmount, P::WiggleMax, P::WiggleMin, P::WiggleRate, P::StaggerAmount,
    ]
    .into_iter()
    .chain(TextAnimParam::PROPERTIES)
    .filter(|&p| anim_value(a, p).is_some())
    .collect()
}

/// A param's value on this frame, as the panel edits it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum AnimVal {
    Num(f64),
    Vec2(Vec2),
    Color([f32; 3]),
}

/// One animator as the panel shows it: its discrete settings (cloned) and its
/// values resolved on this frame.
pub(crate) struct TextAnimInfo {
    pub(crate) anim: TextAnimator,
    pub(crate) rows: Vec<(TextAnimParam, AnimVal, bool)>,
}

pub(crate) fn snapshot(node: &MNode, ctx: &mut EvalCtx) -> Vec<TextAnimInfo> {
    node.text_animators
        .iter()
        .map(|a| TextAnimInfo {
            anim: a.clone(),
            rows: anim_params(a)
                .into_iter()
                .filter_map(|p| {
                    let r = anim_value(a, p)?;
                    let animated = r.is_animated();
                    let v = match r {
                        PropRef::Num(v) => AnimVal::Num(v.resolve(ctx)),
                        PropRef::Vec2(v) => AnimVal::Vec2(v.resolve(ctx)),
                        PropRef::Color(v) => {
                            let c = v.resolve(ctx);
                            AnimVal::Color([c.r as f32, c.g as f32, c.b as f32])
                        }
                        _ => return None,
                    };
                    Some((p, v, animated))
                })
                .collect(),
        })
        .collect()
}

/// Which selector an animator uses, for the panel's picker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SelectorKind {
    Range,
    Wiggly,
    Stagger,
    Karaoke,
}

impl SelectorKind {
    const ALL: [SelectorKind; 4] = [SelectorKind::Range, SelectorKind::Wiggly, SelectorKind::Stagger, SelectorKind::Karaoke];

    fn of(s: &TextSelector) -> Self {
        match s {
            TextSelector::Range { .. } => SelectorKind::Range,
            TextSelector::Wiggly { .. } => SelectorKind::Wiggly,
            TextSelector::Stagger { .. } => SelectorKind::Stagger,
            TextSelector::Karaoke { .. } => SelectorKind::Karaoke,
        }
    }

    fn label(self) -> &'static str {
        match self {
            SelectorKind::Range => "Range",
            SelectorKind::Wiggly => "Wiggly",
            SelectorKind::Stagger => "Stagger",
            SelectorKind::Karaoke => "Karaoke",
        }
    }

    fn make(self) -> TextSelector {
        match self {
            SelectorKind::Range => TextSelector::range(),
            SelectorKind::Wiggly => TextSelector::wiggly(),
            SelectorKind::Stagger => TextSelector::Stagger {
                amount: Value::constant(100.0),
                delays: Vec::new(),
                step: 3.0,
                shift: 0.0,
                reverse: false,
            },
            SelectorKind::Karaoke => TextSelector::Karaoke { cumulative: false },
        }
    }
}

/// Ready-made animators, the way AE's text animation presets start you off.
/// Reveal presets key the range's Start from 0 to 100 over one second from the
/// playhead, so the effect plays out unit by unit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TextAnimPreset {
    Blank,
    FadeInChars,
    SlideUpWords,
    ScalePopChars,
    RotateInChars,
    DropInLines,
    TrackingIn,
    WiggleChars,
    ColorSweep,
}

impl TextAnimPreset {
    pub(crate) const ALL: [TextAnimPreset; 9] = [
        TextAnimPreset::Blank,
        TextAnimPreset::FadeInChars,
        TextAnimPreset::SlideUpWords,
        TextAnimPreset::ScalePopChars,
        TextAnimPreset::RotateInChars,
        TextAnimPreset::DropInLines,
        TextAnimPreset::TrackingIn,
        TextAnimPreset::WiggleChars,
        TextAnimPreset::ColorSweep,
    ];

    pub(crate) fn label(self) -> &'static str {
        match self {
            TextAnimPreset::Blank => "Blank animator",
            TextAnimPreset::FadeInChars => "Fade in · characters",
            TextAnimPreset::SlideUpWords => "Slide up · words",
            TextAnimPreset::ScalePopChars => "Scale pop · characters",
            TextAnimPreset::RotateInChars => "Rotate in · characters",
            TextAnimPreset::DropInLines => "Drop in · lines",
            TextAnimPreset::TrackingIn => "Tracking in · whole text",
            TextAnimPreset::WiggleChars => "Wiggle · characters",
            TextAnimPreset::ColorSweep => "Color sweep · characters",
        }
    }

    /// The animator, with its reveal keyed from `frame` over `len` frames.
    pub(crate) fn build(self, frame: i64, len: i64) -> TextAnimator {
        let ramp = |from: f64, to: f64| {
            let mut a = Keyframe::linear(frame, from);
            a.out_handle = Handle::SMOOTH_OUT;
            let mut b = Keyframe::linear(frame + len.max(1), to);
            b.in_handle = Handle::SMOOTH_IN;
            Value::Keyframed(Track::new(vec![a, b]))
        };
        let reveal = |unit: TextUnit, shape: RangeShape| {
            let mut a = TextAnimator::new(unit);
            // Units still inside the range are the ones not yet revealed: the
            // range's start sweeps across and leaves them at rest.
            a.selector = TextSelector::Range {
                start: ramp(0.0, 100.0),
                end: Value::constant(100.0),
                offset: Value::constant(0.0),
                amount: Value::constant(100.0),
                shape,
                ease_high: 0.0,
                ease_low: 0.0,
                randomize: false,
                seed: 0,
            };
            a
        };
        let c = |v: f64| Some(Value::constant(v));
        let v2 = |x: f64, y: f64| Some(Value::constant(Vec2::new(x, y)));
        let mut a = match self {
            TextAnimPreset::Blank => TextAnimator::new(TextUnit::Chars),
            TextAnimPreset::FadeInChars => {
                let mut a = reveal(TextUnit::Chars, RangeShape::Square);
                a.opacity = c(0.0);
                a
            }
            TextAnimPreset::SlideUpWords => {
                let mut a = reveal(TextUnit::Words, RangeShape::Square);
                a.grouping = AnchorGrouping::Word;
                a.position = v2(0.0, 60.0);
                a.opacity = c(0.0);
                a
            }
            TextAnimPreset::ScalePopChars => {
                let mut a = reveal(TextUnit::CharsExcludingSpaces, RangeShape::Square);
                a.scale = v2(0.0, 0.0);
                a.opacity = c(0.0);
                a
            }
            TextAnimPreset::RotateInChars => {
                let mut a = reveal(TextUnit::CharsExcludingSpaces, RangeShape::Square);
                a.rotation = c(-90.0);
                a.position = v2(0.0, 30.0);
                a.opacity = c(0.0);
                a
            }
            TextAnimPreset::DropInLines => {
                let mut a = reveal(TextUnit::Lines, RangeShape::Square);
                a.grouping = AnchorGrouping::Line;
                a.position = v2(0.0, -80.0);
                a.opacity = c(0.0);
                a
            }
            TextAnimPreset::TrackingIn => {
                let mut a = TextAnimator::new(TextUnit::Chars);
                if let TextSelector::Range { amount, .. } = &mut a.selector {
                    *amount = ramp(100.0, 0.0);
                }
                a.tracking = c(600.0);
                a.opacity = c(0.0);
                a
            }
            TextAnimPreset::WiggleChars => {
                let mut a = TextAnimator::new(TextUnit::Chars);
                a.selector = TextSelector::wiggly();
                a.position = v2(0.0, 12.0);
                a.rotation = c(12.0);
                a
            }
            TextAnimPreset::ColorSweep => {
                let mut a = TextAnimator::new(TextUnit::Chars);
                a.selector = TextSelector::Range {
                    start: Value::constant(0.0),
                    end: Value::constant(25.0),
                    offset: ramp(-25.0, 100.0),
                    amount: Value::constant(100.0),
                    shape: RangeShape::Smooth,
                    ease_high: 0.0,
                    ease_low: 0.0,
                    randomize: false,
                    seed: 0,
                };
                a.fill = Some(Value::constant(MColor::rgb(1.0, 0.85, 0.2)));
                a.scale = v2(120.0, 120.0);
                a
            }
        };
        if self != TextAnimPreset::Blank {
            a.name = self.label().split(" · ").next().unwrap_or("").to_string();
        }
        a
    }
}

/// One edit to a layer's animator stack this frame.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum TextAnimOp {
    Add(TextAnimPreset),
    Remove(usize),
    Duplicate(usize),
    Move { index: usize, delta: isize },
    Rename(usize, String),
    SetUnit(usize, TextUnit),
    SetGrouping(usize, AnchorGrouping),
    SetSelector(usize, SelectorKind),
    SetShape(usize, RangeShape),
    SetEase { index: usize, high: bool, value: f64 },
    SetRandomize(usize, bool),
    SetSeed(usize, u32),
    SetCorrelation(usize, f64),
    SetCumulative(usize, bool),
    SetStagger { index: usize, step: f64, shift: f64, reverse: bool },
    AddProp(usize, TextAnimParam),
    RemoveProp(usize, TextAnimParam),
    /// Set a value (auto-keying it if it's animated).
    Set { index: usize, param: TextAnimParam, value: AnimVal },
}

/// Apply one animator edit, returning whether anything changed. Indices are
/// checked: a stale op from a stack that shrank in between no-ops.
pub(crate) fn apply_text_anim_op(node: &mut MNode, frame: i64, fps: f64, op: &TextAnimOp) -> bool {
    let anims = &mut node.text_animators;
    let n = anims.len();
    match op {
        TextAnimOp::Add(p) => {
            anims.push(p.build(frame, fps.round() as i64));
            true
        }
        &TextAnimOp::Remove(i) => i < n && {
            anims.remove(i);
            true
        },
        &TextAnimOp::Duplicate(i) => i < n && {
            let a = anims[i].clone();
            anims.insert(i + 1, a);
            true
        },
        &TextAnimOp::Move { index, delta } => match index.checked_add_signed(delta) {
            Some(j) if index < n && j < n => {
                anims.swap(index, j);
                true
            }
            _ => false,
        },
        op => {
            let i = match op {
                TextAnimOp::Rename(i, _)
                | TextAnimOp::SetUnit(i, _)
                | TextAnimOp::SetGrouping(i, _)
                | TextAnimOp::SetSelector(i, _)
                | TextAnimOp::SetShape(i, _)
                | TextAnimOp::SetRandomize(i, _)
                | TextAnimOp::SetSeed(i, _)
                | TextAnimOp::SetCorrelation(i, _)
                | TextAnimOp::SetCumulative(i, _)
                | TextAnimOp::AddProp(i, _)
                | TextAnimOp::RemoveProp(i, _)
                | TextAnimOp::SetEase { index: i, .. }
                | TextAnimOp::SetStagger { index: i, .. }
                | TextAnimOp::Set { index: i, .. } => *i,
                _ => return false,
            };
            let Some(a) = anims.get_mut(i) else { return false };
            edit_animator(a, frame, op)
        }
    }
}

fn edit_animator(a: &mut TextAnimator, frame: i64, op: &TextAnimOp) -> bool {
    match op {
        TextAnimOp::Rename(_, s) => a.name = s.clone(),
        &TextAnimOp::SetUnit(_, u) => a.based_on = u,
        &TextAnimOp::SetGrouping(_, g) => a.grouping = g,
        &TextAnimOp::SetSelector(_, k) => {
            if SelectorKind::of(&a.selector) == k {
                return false;
            }
            a.selector = k.make();
        }
        &TextAnimOp::SetShape(_, s) => match &mut a.selector {
            TextSelector::Range { shape, .. } => *shape = s,
            _ => return false,
        },
        &TextAnimOp::SetEase { high, value, .. } => match &mut a.selector {
            TextSelector::Range { ease_high, ease_low, .. } => *(if high { ease_high } else { ease_low }) = value.clamp(-100.0, 100.0),
            _ => return false,
        },
        &TextAnimOp::SetRandomize(_, r) => match &mut a.selector {
            TextSelector::Range { randomize, .. } => *randomize = r,
            _ => return false,
        },
        &TextAnimOp::SetSeed(_, s) => match &mut a.selector {
            TextSelector::Range { seed, .. } | TextSelector::Wiggly { seed, .. } => *seed = s,
            _ => return false,
        },
        &TextAnimOp::SetCorrelation(_, c) => match &mut a.selector {
            TextSelector::Wiggly { correlation, .. } => *correlation = c.clamp(0.0, 100.0),
            _ => return false,
        },
        &TextAnimOp::SetCumulative(_, c) => match &mut a.selector {
            TextSelector::Karaoke { cumulative } => *cumulative = c,
            _ => return false,
        },
        &TextAnimOp::SetStagger { step: s, shift: sh, reverse: r, .. } => match &mut a.selector {
            TextSelector::Stagger { step, shift, reverse, .. } => {
                *step = s;
                *shift = sh;
                *reverse = r;
            }
            _ => return false,
        },
        &TextAnimOp::AddProp(_, p) => {
            if !p.is_property() || anim_value(a, p).is_some() {
                return false;
            }
            p.seed(a);
        }
        &TextAnimOp::RemoveProp(_, p) => {
            if anim_value(a, p).is_none() {
                return false;
            }
            p.clear(a);
        }
        &TextAnimOp::Set { param, value, .. } => match (anim_value_mut(a, param), value) {
            (Some(PropRefMut::Num(v)), AnimVal::Num(x)) => v.set_at(frame, x),
            (Some(PropRefMut::Vec2(v)), AnimVal::Vec2(x)) => v.set_at(frame, x),
            (Some(PropRefMut::Color(v)), AnimVal::Color(c)) => v.set_at(frame, rgb_color(c)),
            _ => return false,
        },
        TextAnimOp::Add(_) | TextAnimOp::Remove(_) | TextAnimOp::Duplicate(_) | TextAnimOp::Move { .. } => return false,
    }
    true
}

fn unit_label(u: TextUnit) -> &'static str {
    match u {
        TextUnit::Chars => "Characters",
        TextUnit::CharsExcludingSpaces => "Characters (no spaces)",
        TextUnit::Words => "Words",
        TextUnit::Lines => "Lines",
    }
}

fn grouping_label(g: AnchorGrouping) -> &'static str {
    match g {
        AnchorGrouping::Character => "Character",
        AnchorGrouping::Word => "Word",
        AnchorGrouping::Line => "Line",
        AnchorGrouping::All => "All",
    }
}

fn shape_label(s: RangeShape) -> &'static str {
    match s {
        RangeShape::Square => "Square",
        RangeShape::RampUp => "Ramp Up",
        RangeShape::RampDown => "Ramp Down",
        RangeShape::Triangle => "Triangle",
        RangeShape::Round => "Round",
        RangeShape::Smooth => "Smooth",
    }
}

const SHAPES: [RangeShape; 6] =
    [RangeShape::Square, RangeShape::RampUp, RangeShape::RampDown, RangeShape::Triangle, RangeShape::Round, RangeShape::Smooth];
const UNITS: [TextUnit; 4] = [TextUnit::Chars, TextUnit::CharsExcludingSpaces, TextUnit::Words, TextUnit::Lines];
const GROUPINGS: [AnchorGrouping; 4] = [AnchorGrouping::Character, AnchorGrouping::Word, AnchorGrouping::Line, AnchorGrouping::All];

fn combo<T: Copy + PartialEq>(
    ui: &mut egui::Ui,
    id: impl std::hash::Hash + std::fmt::Debug,
    cur: T,
    all: &[T],
    label: impl Fn(T) -> &'static str,
) -> Option<T> {
    let mut out = None;
    egui::ComboBox::from_id_salt(id).selected_text(label(cur)).show_ui(ui, |ui| {
        for &v in all {
            if ui.selectable_label(v == cur, label(v)).clicked() && v != cur {
                out = Some(v);
            }
        }
    });
    out
}

/// The "Animate" section of a text layer's properties: the animator stack.
pub(crate) fn text_animators_ui(ui: &mut egui::Ui, anims: &[TextAnimInfo], edits: &mut PropEdits) {
    ui.separator();
    ui.horizontal(|ui| {
        ui.strong("Text Animators");
        ui.menu_button("Animate ▾", |ui| {
            for p in TextAnimPreset::ALL {
                if ui.button(p.label()).clicked() {
                    edits.text_anim = Some(TextAnimOp::Add(p));
                    ui.close();
                }
            }
        })
        .response
        .on_hover_text("Add an animator: per-character, word or line animation");
    });
    if anims.is_empty() {
        ui.weak("Animate characters, words or lines: add an animator, pick what it does, and key its selector.");
        return;
    }
    let last = anims.len() - 1;
    for (i, info) in anims.iter().enumerate() {
        let a = &info.anim;
        let title = if a.name.is_empty() { format!("Animator {}", i + 1) } else { a.name.clone() };
        egui::CollapsingHeader::new(egui::RichText::new(title).strong())
            .id_salt(("text_anim", i))
            .default_open(true)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let mut name = a.name.clone();
                    if ui.add(egui::TextEdit::singleline(&mut name).hint_text("Name").desired_width(110.0)).changed() {
                        edits.text_anim = Some(TextAnimOp::Rename(i, name));
                    }
                    if ui.add_enabled(i > 0, egui::Button::new("Up").small()).clicked() {
                        edits.text_anim = Some(TextAnimOp::Move { index: i, delta: -1 });
                    }
                    if ui.add_enabled(i < last, egui::Button::new("Dn").small()).clicked() {
                        edits.text_anim = Some(TextAnimOp::Move { index: i, delta: 1 });
                    }
                    if ui.small_button("Dup").clicked() {
                        edits.text_anim = Some(TextAnimOp::Duplicate(i));
                    }
                    if ui.small_button("Remove").clicked() {
                        edits.text_anim = Some(TextAnimOp::Remove(i));
                    }
                });
                egui::Grid::new(("text_anim_grid", i)).num_columns(3).striped(true).show(ui, |ui| {
                    ui.label("Based On");
                    if let Some(u) = combo(ui, ("ta_unit", i), a.based_on, &UNITS, unit_label) {
                        edits.text_anim = Some(TextAnimOp::SetUnit(i, u));
                    }
                    ui.label("");
                    ui.end_row();

                    ui.label("Anchor Group");
                    if let Some(g) = combo(ui, ("ta_group", i), a.grouping, &GROUPINGS, grouping_label) {
                        edits.text_anim = Some(TextAnimOp::SetGrouping(i, g));
                    }
                    ui.label("");
                    ui.end_row();

                    ui.label("Selector");
                    if let Some(k) = combo(ui, ("ta_sel", i), SelectorKind::of(&a.selector), &SelectorKind::ALL, SelectorKind::label) {
                        edits.text_anim = Some(TextAnimOp::SetSelector(i, k));
                    }
                    ui.label("");
                    ui.end_row();

                    selector_settings(ui, i, &a.selector, edits);

                    for &(param, val, animated) in &info.rows {
                        value_row(ui, i, param, val, animated, edits);
                    }
                });
                ui.menu_button("+ Property", |ui| {
                    for p in TextAnimParam::PROPERTIES {
                        let has = info.rows.iter().any(|r| r.0 == p);
                        if ui.add_enabled(!has, egui::Button::new(p.label())).clicked() {
                            edits.text_anim = Some(TextAnimOp::AddProp(i, p));
                            ui.close();
                        }
                    }
                });
            });
    }
}

/// The discrete (non-animatable) settings of a selector.
fn selector_settings(ui: &mut egui::Ui, i: usize, sel: &TextSelector, edits: &mut PropEdits) {
    match sel {
        TextSelector::Range { shape, ease_high, ease_low, randomize, seed, .. } => {
            ui.label("  Shape");
            if let Some(s) = combo(ui, ("ta_shape", i), *shape, &SHAPES, shape_label) {
                edits.text_anim = Some(TextAnimOp::SetShape(i, s));
            }
            ui.label("");
            ui.end_row();

            ui.label("  Ease High / Low");
            ui.horizontal(|ui| {
                let (mut h, mut l) = (*ease_high, *ease_low);
                if ui.add(crate::calc::num(&mut h).speed(1.0).range(-100.0..=100.0).suffix("%")).changed() {
                    edits.text_anim = Some(TextAnimOp::SetEase { index: i, high: true, value: h });
                }
                if ui.add(crate::calc::num(&mut l).speed(1.0).range(-100.0..=100.0).suffix("%")).changed() {
                    edits.text_anim = Some(TextAnimOp::SetEase { index: i, high: false, value: l });
                }
            });
            ui.label("");
            ui.end_row();

            ui.label("  Randomize");
            ui.horizontal(|ui| {
                let mut r = *randomize;
                if ui.checkbox(&mut r, "").changed() {
                    edits.text_anim = Some(TextAnimOp::SetRandomize(i, r));
                }
                if *randomize {
                    let mut s = *seed;
                    if ui.add(egui::DragValue::new(&mut s).prefix("seed ")).changed() {
                        edits.text_anim = Some(TextAnimOp::SetSeed(i, s));
                    }
                }
            });
            ui.label("");
            ui.end_row();
        }
        TextSelector::Wiggly { correlation, seed, .. } => {
            ui.label("  Correlation");
            ui.horizontal(|ui| {
                let mut c = *correlation;
                if ui.add(crate::calc::num(&mut c).speed(1.0).range(0.0..=100.0).suffix("%")).changed() {
                    edits.text_anim = Some(TextAnimOp::SetCorrelation(i, c));
                }
                let mut s = *seed;
                if ui.add(egui::DragValue::new(&mut s).prefix("seed ")).changed() {
                    edits.text_anim = Some(TextAnimOp::SetSeed(i, s));
                }
            });
            ui.label("");
            ui.end_row();
        }
        TextSelector::Stagger { step, shift, reverse, .. } => {
            ui.label("  Step / Shift");
            ui.horizontal(|ui| {
                let (mut st, mut sh, mut r) = (*step, *shift, *reverse);
                let mut changed = ui.add(crate::calc::num(&mut st).speed(0.25).suffix(" f")).on_hover_text("Frames of delay per unit").changed();
                changed |= ui.add(crate::calc::num(&mut sh).speed(0.25).suffix(" f")).on_hover_text("Frames of delay for every unit").changed();
                changed |= ui.checkbox(&mut r, "Reverse").changed();
                if changed {
                    edits.text_anim = Some(TextAnimOp::SetStagger { index: i, step: st, shift: sh, reverse: r });
                }
            });
            ui.label("");
            ui.end_row();
        }
        TextSelector::Karaoke { cumulative } => {
            ui.label("  Cumulative");
            let mut c = *cumulative;
            if ui.checkbox(&mut c, "").on_hover_text("Keep every word spoken so far selected").changed() {
                edits.text_anim = Some(TextAnimOp::SetCumulative(i, c));
            }
            ui.label("");
            ui.end_row();
        }
    }
}

/// One animatable row: its value editor, stopwatch, and (for a property) a
/// remove button.
fn value_row(ui: &mut egui::Ui, i: usize, param: TextAnimParam, val: AnimVal, animated: bool, edits: &mut PropEdits) {
    let indent = if param.is_property() { "" } else { "  " };
    ui.label(format!("{indent}{}", param.label()));
    ui.horizontal(|ui| {
        let set = |edits: &mut PropEdits, value| edits.text_anim = Some(TextAnimOp::Set { index: i, param, value });
        match val {
            AnimVal::Num(mut v) => {
                let suffix = match param {
                    TextAnimParam::Rotation | TextAnimParam::Skew => "°",
                    TextAnimParam::RangeStart
                    | TextAnimParam::RangeEnd
                    | TextAnimParam::RangeOffset
                    | TextAnimParam::RangeAmount
                    | TextAnimParam::WiggleMax
                    | TextAnimParam::WiggleMin
                    | TextAnimParam::StaggerAmount => "%",
                    _ => "",
                };
                let mut drag = crate::calc::num(&mut v).speed(param.speed()).suffix(suffix);
                if param == TextAnimParam::Opacity {
                    drag = drag.range(0.0..=1.0);
                }
                if ui.add(drag).changed() {
                    set(edits, AnimVal::Num(v));
                }
            }
            AnimVal::Vec2(v) => {
                let (mut x, mut y) = (v.x, v.y);
                let suffix = if param == TextAnimParam::Scale { "%" } else { "" };
                let cx = ui.add(crate::calc::num(&mut x).speed(param.speed()).prefix("x ").suffix(suffix)).changed();
                let cy = ui.add(crate::calc::num(&mut y).speed(param.speed()).prefix("y ").suffix(suffix)).changed();
                if cx || cy {
                    set(edits, AnimVal::Vec2(Vec2::new(x, y)));
                }
            }
            AnimVal::Color(mut c) => {
                if crate::props::color_button(ui, &mut c).changed() {
                    set(edits, AnimVal::Color(c));
                }
            }
        }
        if param.is_property() && ui.small_button("✕").on_hover_text("Remove this property").clicked() {
            edits.text_anim = Some(TextAnimOp::RemoveProp(i, param));
        }
    });
    if key_button(ui, animated) {
        edits.key.insert(PropKind::TextAnim { index: i, param });
    }
    ui.end_row();
}
