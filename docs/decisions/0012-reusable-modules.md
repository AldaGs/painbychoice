# 0012. Reusable animation modules — shared, auto-retimed, overridable

- **Status:** Accepted — implemented
- **Recorded:** 2026-09-08, extracted verbatim from the README design journal.

## Context

Copy-pasted keyframes are the thing every animator maintains by hand and
nobody wants to.

## Decision

*The target user story* (recorded so the pieces below have a home): drop a video
layer, lay subtitles over it, define **one** entrance/exit animation, and have
every subtitle play it **fitted to its own clip** — the animation starts at each
layer's in-point and finishes at its out-point. Edit the one definition and all
of them update; but any single subtitle can **override** it and diverge.

This is not one feature — it's the intersection of five, most already on the
roadmap. What it needs, and where each piece lands:

1. ~~**Text layers**~~ ✅ **Done (2026-07-20).** `Shape::Text { content, family,
   size, align, max_width }`, shaped with **parley** (bidi, script segmentation,
   font fallback, line breaking, alignment) against **system fonts**, with
   **skrifa** pulling the outlines for the shaped glyph ids. See *Text* below for
   the two decisions that shaped it. Subtitles are just text layers whose in/out
   match each caption's timing.

2. **A layer time model — in/out points** (`core` change) — today a node has no
   time range: the comp has a single `duration` and every layer is live for all
   of it. Add per-layer `in`/`out` (and a `start`, for slipping) in frames, with
   trims honoured by `evaluate` (a layer outside its range doesn't draw). *When:*
   **with pre-comps** (the next agreed step past #5) — a pre-comp *is* a layer
   with an in/out, so the two share the model; do it once. This is the
   prerequisite that makes "retime to each clip" mean anything.

3. **Layer-local time in expressions** (small expression-surface addition) — the
   reason one module fits every clip: the animation reads **normalized progress**
   `t01 = clamp((frame − in) / (out − in), 0, 1)` (and raw `localTime = frame −
   in`) instead of the absolute `frame`. New leaf sources — an `Expr::InPoint` /
   `OutPoint` / `LocalTime` family and the `t01` convenience — plus
   `inPoint`/`outPoint` in the script scope, alongside today's `frame`/`time`.
   *When:* rides directly on (2). With it, "ease in over the first N frames,
   hold, ease out over the last N" is **one** expression that auto-fits any
   layer's duration — no per-layer retiming wiring.

4. **The shared, linked animation module + override** (the heart of it) — this is
   the **document-wide property graph** step (agreed order past #5) made concrete.
   Today the graph is *per-node, per-property*: a property's `Expr` can `Ref`
   another node or read a `param`, but the recipe lives on that one property. A
   *module* is a **named driver stored once at the document level** that many
   properties **link** to; editing it edits every link. Mechanism, in the grain
   of what already exists:
   - A module is a **parameterized, time-relative graph fragment** — an `Expr`
     tree that reads `t01` / `localTime` (so it retimes per layer, item 3) and
     whose tunables are exposed `Param`s (amplitude, ease, direction — the knobs
     you tweak once). The procedural generators (`osc`/`ramp`/…) are the
     ready-made bodies for these.
   - A property **links** it with a new `Expr` arm — `Expr::Use { module,
     overrides }` — resolved through the **same memoized, cycle-guarded
     `EvalCtx`** as every other reference: a module referenced by 50 layers
     collapses shared sub-results through the frame memo, and a module that reads
     a layer it drives warns and falls back exactly as a cycle does today. Build
     the sandbox once (see *the two unifying insights* below) and this is nearly
     free.
   - **Override is a layering, not a fork:** the instance stores *only* the knob
     values (or, at the extreme, a whole sub-expression) it wants different;
     unset knobs inherit the module. Same shape as `Value`'s
     const→keyframe→expr layering, and the same as a pre-comp instance overriding
     an exposed parameter — so design the override model **once** for both.
   *When:* **after pre-comps** — it needs the layer-time model (2) and reuses the
   pre-comp instance/override machinery, so it belongs *in* the document-wide
   graph step, not before it.

5. **Video footage** (parallel subsystem) — only the *background* needs it, and
   it's the already-decided **asset registry + compositor** work ([0008](0008-asset-registry.md)). It does **not** block the subtitle-animation story; the two are
   independent tracks and can be built in either order.

**Already possible today, as a manual prototype** (the honest through-line): the
seed of item 4 exists. Put the animation on a "controller" node's exposed
parameters and drive each text layer's property with an `Expr` that reads the
controller (`param_of` / `Ref`); change the controller once and all linked layers
update. What's missing is (a) auto-retiming to each layer's clip (items 2–3),
(b) a first-class *module* edited in one place rather than a convention around
one node, and (c) per-instance override. So this feature is less "new engine"
than **promoting a pattern the expression graph already supports into a named,
retimed, overridable first-class object** — which is why it maps onto the
document-wide-graph step rather than inventing a sixth subsystem.

**Net build order for this story:** text layers (anytime) → layer time model +
local-time sources (with pre-comps) → the linked module + override (with the
document-wide property graph); footage rides its own track and gates only the
video background.
