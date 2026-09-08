# 0011. Pre-comps and the per-layer time model

- **Status:** Accepted — implemented
- **Recorded:** 2026-09-08, extracted verbatim from the README design journal.

## Context

A pre-comp is a layer with a time range, so nested comps and per-layer timing
are the same change seen from two sides.

## Decision

The next step past #5. Two intertwined-but-separable deliverables: a **per-layer
time model** (cheap, foundational) and **nested comps** (the data-model change it
unblocks). Grounding: today `Document = { width, height, fps, duration, root:
Node }` is *one* composition; `Node` has **no time range** (every layer is live
the whole comp); `evaluate(doc, frame)` walks the tree with a single **absolute**
`EvalCtx.frame`. Note `resolve_target` already saves/restores `ctx.frame` to
sample at a shifted time — the exact mechanism local time needs.

**Three decisions (recommendations, not yet locked):**
1. **Multi-comp model — registry + instances** (recommended) over inline nesting.
   `Document` becomes a project of comps keyed by `CompId`; a layer can be a
   `Precomp(CompId)` *instance*. Only this gives **reuse** (one comp placed
   twice) and sets up the shared-module/override story; inline nesting is less
   code but can't instance. ("A comp *is* a graph node," as the agreed order
   says.)
2. **Keyframes stored in local frames** (recommended) — a layer's own keys and
   expressions are authored relative to its `start`, so two subtitles with
   different in-points play the *same* local keyframes at different comp times.
   This is what makes "one animation, retimed per clip" fall out for free.
3. **v1 precomp compositing = "vector paste-through"** — geometry composes
   correctly (fold the precomp layer's xf/opacity into its items), but **no**
   isolated rasterization / blend modes / 2D-vs-3D collapse (those need the
   compositor stage, which is later). Correct for subtitles; a known limit.

**Staged build:**
- ~~**Stage 1 — layer time model**~~ ✅ Done (2026-07-19). `Node.timing:
  Option<LayerTiming>` (`{ start, in_, out }` in comp frames) lands exactly as
  planned below, plus a **clip bar** at the top of the timeline for the selected
  layer: drag an edge to trim, the body to slide, `Trim…` gives a layer a range
  covering the whole comp (so enabling it never moves anything), `Clip ×` takes
  it back to `None`. A tick inside the bar marks where local frame 0 sits.
  - The trim window is **half-open** `[in, out)` so two clips meeting at frame N
    don't both draw on N, and the liveness check happens *before* anything
    resolves — a hidden layer and its subtree cost nothing.
  - Drags latch their **grab mode and the timing they started from** at press,
    then apply the total delta to that original. Incremental deltas would make a
    drag that clamped at frame 0 refuse to spring back.
  - **egui gotcha, cost two rounds of debugging:** inside `drag_started()`,
    `interact_pointer_pos()` is *not* where the press landed. egui only fires
    `drag_started` once the pointer crosses its drag threshold, and by then that
    call reports where the pointer is *now* — already off the handle — so every
    trim silently read as a slide. Use `i.pointer.press_origin()` for anything
    that hit-tests the press itself; the marquee already did.
  - Hit-test the **painted** edges, not the raw ones: a clip can extend past the
    visible window (the default range ends one frame past it), and an edge you
    can't see is an edge you can't grab. Nearest edge wins where the two
    handles overlap, or a clip a few pixels wide can never be trimmed shorter.
  - `start` is deliberately separate from `in_`: trim moves an edge only, slide
    moves all three, and **slip** (alt+drag, as in AE) moves `start` alone so
    the content shifts under a fixed window. Slip is unclamped in both
    directions — AE clamps it to the source footage's bounds, but there is no
    footage here, and a negative local frame simply holds the track's first key.
  - Slip's only feedback is the local-0 marker inside the bar (the bar itself
    doesn't move), so when `start` slips out of the visible clip the marker
    becomes a `<`/`>` pinned to the edge it went past rather than vanishing.
  - Not to be confused with **AE's work area** (now built — see *The work area*
    below): that is a comp-level *preview* range (view state), while these are
    per-layer in/out points that change evaluation (document state).
  - **Out-of-bounds is allowed, by decision (2026-07-20):** `drag_clip` has no
    upper clamp against the comp's duration, so a layer's `out` can extend past
    the comp end — a layer **may outlive the comp**, as in AE. It's harmless
    (eval is half-open `[in, out)` and the comp only renders `[0, duration)`, so
    the overhang never draws) and keeps `drag_clip` a pure function of the clip.
    Pinned by a test so a future clamp can't slip in unnoticed.
  - **Known limit, deliberate:** local time is `comp_frame − start`, so a timed
    layer nested under another timed layer reads *comp* time, not its parent's
    local time. Nested time is a comp-level concern — Stage 3's business.
- The plan as written: `Node` gains `#[serde(default)] timing: Option<LayerTiming>`
  (`{ start, in_, out }` in frames; `None` = today's behaviour). Add
  `EvalCtx.comp_frame` (global) beside `frame` (now the current layer's *local*
  frame); `walk` computes `local = comp_frame − start`, skips drawing outside
  `[in, out)`, and sets `ctx.frame` for the subtree via the existing
  save/restore. Serde `default` covers migration (no `migrate()` change);
  trim/slip eval + round-trip tests. UI: in/out clip bars in the timeline (lean
  on the existing `ClipTrack`/`tracks` scaffold), drag to trim/slide.
- ~~**Stage 2 — local-time expression sources**~~ ✅ Done (2026-07-19).
  `Expr::Time(TimeSource)` — `Local` / `In` / `Out` / `T01` — plus `localTime`,
  `inPoint`, `outPoint` and `t01` in the Rhai scope. One vocabulary, two
  spellings: `TimeSource::label()` is the identifier a script uses, so a graph
  node and a script name the same reading the same way.
  - **Everything is in layer-*local* frames**, matching the domain keyframes are
    authored in — so `inPoint` is the in-point relative to the layer's own frame
    0, and an expression reads identically on two clips with different
    in-points. (AE's `inPoint` is comp-time; local is the coherent choice here
    because Stage 1 made keyframes local.)
  - An **untimed layer reads the comp as its window** (`in = 0`,
    `out = duration_frames`), so `t01` is meaningful before anything is trimmed
    rather than degenerating to 0.
  - `EvalCtx.timing` carries the current layer's window, saved/restored by
    `walk` beside `frame` — so a nested layer reads its own clock, not an
    ancestor's.
  - Proven by the test that motivated the feature: two clips of *different
    lengths* share one expression (`opacity = t01`) and each fades across its
    own duration, with no keyframes and nothing clip-specific in the expression.
- The plan as written: **local-time expression sources** (small, rides on Stage 1).
  `Expr::LocalTime / InPoint / OutPoint` + a `t01` convenience
  (`clamp((frame−in)/(out−in), 0, 1)`), and `inPoint`/`outPoint`/`localTime` in
  the Rhai scope. Now "ease in over the first N frames, hold, ease out over the
  last N" is **one** expression that auto-fits any layer — the subtitle payoff,
  before pre-comps even exist. Fully unit-testable.
- ~~**Stage 3 — multi-comp data model**~~ ✅ Done (2026-07-19), minimal UI.
  `Comp` is what `Document` always was — the rename *is* the feature — with
  `pub type Document = Comp;` kept so existing call sites still read.
  `Project { comps: BTreeMap<CompId, Comp>, root: CompId }` is the registry, and
  `Node.precomp: Option<CompId>` makes a layer an **instance**.
  - **Registry + instances, not inline nesting**: the same comp placed twice
    renders twice and is edited once. Proven by test.
  - A precomp is evaluated at the **layer's local frame**, so trimming or
    slipping an instance retimes everything inside it — and this is where nested
    timing finally becomes properly relative, which stage 1 left open. Each comp
    gets its own `EvalCtx`, so expressions and name lookups are scoped to their
    own tree (cross-comp references stay out of scope for v1).
  - **Comp-level cycle guard** is a stack of the comps currently being
    evaluated, so it catches `A→A` *and* `A→B→A` rather than only self-reference.
    A dangling `CompId` warns too — a silently blank frame is indistinguishable
    from a broken one.
  - **Three save formats load**, newest first: a project; the pre-comps wrapper
    holding one `document`; and a bare `Document` from before the wrapper. Note
    the trap: every `SaveFile` field defaults, so a *bare document parses as an
    empty `SaveFile`* — the fallback keys on "parsed but carries nothing", not
    on a parse failure. Only the project form is ever written.
  - `App` holds `project` + `current` behind `doc()`/`doc_mut()`, so opening a
    different comp (stage 4) is a one-field change. Two sites deliberately reach
    through the field instead: an accessor borrows all of `self`, which loses the
    field-level disjointness `selected_keys` needs.
- The plan as written: **multi-comp data model** (the big/risky one; minimal UI).
  `Project { comps: Map<CompId, Comp>, root: CompId }`, `Comp = { size, fps,
  duration, root: Node }`; a `Precomp(CompId)` layer kind;
  `evaluate(project, comp_id, frame)` recurses into a precomp at the layer's
  local frame and folds in xf/opacity. **Comp-level cycle guard** (A→B→A → warn
  + skip, mirroring the expr guard). **`.pbc` migration**: wrap today's single
  `root` as the one comp and reconcile with the existing `Project { document,
  layout }` wrapper; old files still load. Cross-comp references stay out of
  scope for v1.
- ~~**Stage 4 — pre-comp UI.**~~ ✅ Done (2026-07-19), click-tested.
  A comp switcher + rename field in the comp bar (the switcher hides itself
  while there's only one comp, so a single-comp project looks exactly as it
  did), `[c]` marking a precomp layer with an **open** button beside it, and
  **Pre-compose selection** in the layers panel.
  - Pre-composing is **visually a no-op**, which is the whole point: it
    reorganizes without changing the frame. The layer's transform travels *into*
    the new comp with it and the instance is left neutral — applying it at both
    levels would double it, which is the classic way this goes wrong. Tested by
    evaluating before and after and comparing.
  - The instance keeps the layer's **place among its siblings** (`Node::replace`
    swaps in position), since sibling order is draw order.
  - The new comp inherits the open comp's size/fps/duration, so nested content
    keeps its coordinate space and timing.
  - Opening a comp rebuilds everything comp-scoped — selection, the id counter,
    the timeline window. **Node ids are per-comp**, so a stale `next_id` would
    hand out ids colliding with the newly opened tree.
  - The operation itself lives in `precompose_into`, outside `App`, so it's unit
    tested rather than only reachable by clicking.
- The plan as written: **pre-comp UI.** Comp switcher in the comp bar; "pre-compose
  selection" (move selected layers into a new comp, replace with an instance —
  the core AE workflow); open/close a comp; precomp layer in the layers panel.

**Blast radius to record before saved multi-comp docs exist** (same discipline
as the 2.5D note): the `Document`→`Project` shape, the `evaluate` signature,
hit-testing/selection, the `.pbc` loader, and every live-app assumption of a
single `doc`. All cheap now, expensive once multi-comp docs are in the wild.
