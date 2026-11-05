# Text animation — remaining AE parity

Follows the text-animator work (animatable weight; position/anchor/scale/
rotation/skew/opacity/tracking/fill/stroke animator properties; anchor
grouping; Range shapes, ease, randomize; Wiggly selector; editor panel and
presets). What is left, in build order. Each phase lands on its own with
tests, and keeps old `.pbc` files loading.

## Phase 1 — Multiple selectors per animator, with modes

AE stacks selectors inside one animator and folds them with a mode.

- `TextAnimator` keeps its first `selector` plus `mode`, and gains
  `more_selectors: Vec<ExtraSelector { mode, selector }>` (both serde-default,
  so existing files are unchanged).
- `SelectorMode`: Add, Subtract, Intersect, Min, Max, Difference. Folding
  starts at 0; a *first* selector in Subtract mode selects the inverse
  (starts from fully selected), as AE does. Result clamped to -1..1.
- Every selector's animatable controls are addressed by
  `PropKind::TextAnim { index, sel, param }`, so a second selector's range
  keys get their own dopesheet rows.
- Editor: each selector is a sub-block with mode, kind, and remove;
  "+ Selector" appends a Range in Add mode.
- AE import: additional selectors and their modes are mapped instead of
  reported as dropped.

**Status:** implemented (core, editor, AE import, tests).

## Phase 2 — Expression selector

- `TextSelector::Expression { script, amount }`: a Rhai script evaluated
  per unit with AE's names in scope — `textIndex` (1-based), `textTotal`,
  `selectorValue` (the selection so far, 0..100) — plus everything a normal
  script sees (`time`, `wiggle()`, `value()`…). It returns 0..100; `amount`
  scales it. A failing script selects nothing and warns, never breaks the
  frame.
- Editor: a multiline script field under the selector.

**Status:** implemented (core, editor, AE import, tests).

## Phase 3 — Per-character 3D

- `TextAnimator` gains `position_z`, `rotation_x`, `rotation_y`, and the
  layer a `per_char_3d` switch (AE's "Enable Per-character 3D").
- Glyph transforms become 3D (`mat4`) about the grouping anchor. With a
  camera / 3D layer, each glyph is projected through the existing
  `warp_path` route that tipped layers already use; without one, the
  projection is orthographic (rotation X/Y foreshortens, Z does nothing).
- Glyph depth sorting inside the layer so turned characters overlap right.
- Editor rows + stopwatches for the new properties; AE import of
  `ADBE Text Position 3D` z, `ADBE Text Rotation X/Y`.

## Phase 4 — Per-character blur

- Animator property `blur: Value<Vec2>` (px, x/y).
- `RenderItem` gains an optional per-item blur; the vello backend renders
  blurred items into an isolated offscreen and runs the existing
  Gaussian (`render::fx`), batching glyph runs that share a blur radius so
  a fully selected word costs one pass, not one per glyph.
- Export / render queue paths use the same route.

## Phase 5 — Text on a path

- `Node::text_path: Option<TextPath>`: the path source (the layer's mask, or
  another vector layer by id), `first_margin`, `last_margin` (animatable),
  `perpendicular`, `reverse`, `force_alignment`.
- After shaping, each glyph's advance centre is mapped to an arc-length
  position on the flattened path (kurbo `ParamCurveArclen`); the glyph is
  translated there and, when perpendicular, rotated to the tangent. Animator
  transforms apply after, in the glyph's path-aligned frame, as in AE.
- Animating First Margin is the classic "text runs along a line" move and
  needs no extra machinery — it is a `Value`.
- Editor: a "Path Options" block on text layers; canvas draws the path.

## Out of scope for now

Character offset / character value (re-shaping per unit), line anchor,
grouping alignment, the "Index" range units mode (percent only), and
per-character 3D bevels.
