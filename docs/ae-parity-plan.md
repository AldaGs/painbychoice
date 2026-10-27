# AE parity: making PBC rebuild the `ae-ref` projects

Written 2026-10-27. Goal: the 9 After Effects projects in `ae-ref/` (exported to
JSON) open in PBC and render close enough to AE to ship. The feature counts below
are measured over all 9 files: ~3,300 layers, 2,678 expressions, 60 effect types.

## Step 1 — Get the data across (exporter fix + importer)

Nothing else can be checked against the references until they load.

**1a. Exporter (the AE-side script — not in this repo).** The JSON drops data PBC
needs and cannot guess:

| Missing | Evidence | Needed for |
| --- | --- | --- |
| Path vertices | all 7,175 `ADBE Vector Shape` are `"valueType": "NoValue"` | every pen-drawn shape, masks |
| Parent | no `parent` key on any layer | rigs, Delay Child |
| Blend mode, track matte | no key | compositing |
| Layer kind (solid/null/camera/light/text/shape/footage/precomp) | only `sourceId` | knowing what to build |
| Footage file path + solid colour/size | `Footage` items carry only `id`/`name` | footage and solids |
| Default-valued props | e.g. strokes with no `Stroke Color` | correct colours/widths |
| Mask path + mode + feather | mask atoms have no shape | masks |
| Text document (font, size, fill, tracking, justification) | only `text` string | text layers |

Proposed additions, per layer: `parent` (layer index), `blendMode`, `trackMatte`
(`alpha`/`alphaInverted`/`luma`/`lumaInverted`), `kind`; per path property:
`value: {vertices, inTangents, outTangents, closed}` (keyframed the same way);
per footage item: `file`, or `solid: {color, width, height}`; export every
property's value even when it equals the default.

**1b. Importer — `motion-core::ae`.** `ae::import(&str) -> Result<Project, _>`
plus a `pbc` CLI subcommand. Maps:

- Compositions → `Comp` (size, fps, duration, background); precomp layers →
  `Node::precomp` by `sourceId`.
- Layer order: AE index 1 is on top; PBC draws children in order, last on top,
  so layers are reversed.
- `startTime`/`inPoint`/`duration` → `LayerTiming`; `hidden`/`locked` flags.
- Transform (anchor, position — joined or separated —, scale, X/Y/Z rotation,
  opacity) with keyframes.
- **Keyframe easing:** AE's speed/influence → PBC's normalized cubic handles
  (`x = influence`, `y = speed·influence·Δt/Δv`), Hold → `Interp::Hold`.
- Shape groups → nested group nodes; Rect/Ellipse/Path → `Shape`; Fill/Stroke
  → `fill`/`stroke`; Merge → `compound`.
- Anything unmapped (expressions, effects, text animators) → a per-import
  **report** listing what was dropped and where, so progress is measurable.

Done when: all 9 files import without error and the report counts what is left.

**Status 2026-10-27:** 1b landed as `crates/core/src/ae.rs` + `motion import-ae
<export.json> <out.pbc>`. All 9 files import and render. Biggest drops in the
report: bezier paths (6,300+, blocked on 1a), footage (~940, blocked on 1a),
expressions (~1,900). 
**Update:** the exporter is the Go `.aep` parser in `Desktop/aep/aftereffects-aep-parser`
(byte layouts cross-checked against py-aep, MIT). It now writes `layerId`, `kind`,
`parent`, `blendMode`, `trackMatte`, `matteLayer`. PBC gained `Node::link`
(AE-style transform-only parenting, no nesting), and the importer maps parents,
the 15 shared blend modes, alpha mattes (including AE 23's matte-below, by
swapping the pair). Bezier path vertices are decoded too (`path.go`: shph
bounding box + normalized ldat triples) and import as `Shape::Vector`, keyframed
per point; every shape path in the 8 projects now comes across. Footage file
paths, solid colours and mask info (`mkif`: mode, invert) are decoded too:
footage imports as assets + `Shape::Image` (all 593 referenced files resolve on
this machine), solids as exact vector rects, and the first Add/Subtract mask
as `Node::mask` (mode-None masks are skipped, as AE ignores them for alpha).
A shape group's paths share
one fill as a `Merge` compound (nonzero, like AE), so letter counters are holes.
Missing transform props fall back to AE's defaults (comp-centre position,
source-centre anchor). Still to decode in 1a: other default-valued props (e.g.
stroke colour), text documents.

## Step 2 — Shape strokes

Extend `Stroke` with `cap`, `join`, `miter`, `dashes` (+ offset), and paint
with a `Gradient` (linear/radial, stops) for both fill and stroke. Taper and
wave are rare; later. Done when: the shape layers in the references match AE
side-by-side.

## Step 3 — Expressions and controls

PBC already has `Param`s (= AE expression controls) and a Rhai bridge with
`value()`/`wiggle()`. Approach: **recognize the common patterns at import and
lower them to native IR**, rather than embedding a JS engine.

| Pattern | Count | Native form |
| --- | --- | --- |
| Stroke-width scale fix (`value / length(toComp…)`) | 1,229 | drop (PBC strokes don't scale with the layer) or a non-scaling stroke flag |
| Delay Child | 498 | parent-offset in time — a native "follow parent with delay" |
| Inertial bounce (`amp/freq/decay`) | 173 | a built-in `bounce(amp, freq, decay)` |
| Type Array | 116 | text animators (step 4) |
| `posterizeTime(n); wiggle(f,a)` | ~170 | `wiggle` + a `posterize` wrapper |
| `loopOut()` | 18 | track loop mode on `Value` |
| `sourceRectAtTime` box | 29 | a `layer_bounds()` function |
| `thisComp.layer("x").effect("y")("z")` links | ~200 | `Ref` to that layer's `Param` |

Expressions that don't match go to Rhai unchanged when they parse, otherwise
into the report. Revisit a JS engine (boa/quickjs) only if the report's long
tail blocks a real project.

## Step 4 — Text animators

`Shape::Text` gains `animators: Vec<TextAnimator>`: properties (position,
scale, rotation, opacity, fill, tracking) + range selectors (start/end/offset,
by character/word/line, shape: square/ramp/smooth) + the expression selector.
Per-glyph transforms are applied after shaping. Done when the Type Array
layers animate like AE.

## Step 5 — Effects

Port in this order (count in the references): Fill (71), Offset (57),
Transform (56), CC Composite (53), Unmult (17), Corner Pin (13), Set Matte
(12), CC RepeTile (19), Turbulent Displace (11), then Change to Color,
Fractal Noise, Colorama, Bulge, Stroke, Simple Choker, Linear Color Key,
Motion Tile, Spill Suppressor and the single-use tail. Third-party (Deep Glow,
Shadow Studio) map to PBC's `Glow`/`DropShadow` approximations. Mocha, Bendio
and loopFlow are out of scope.
