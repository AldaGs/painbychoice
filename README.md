# Pain By Choice (PBC)

A hybrid vector motion / animation tool — non-destructive, non-linear, parametric.
A blend of After Effects, Figma, Animate, and Cavalry. Rust engine.

> **Status:** working single-window editor. You can build a composition from
> scratch, animate it with frame-accurate keyframes + editable easing,
> scrub/play, and save/load — with a **splittable dockable-panel** workspace and
> an **expression / node-graph** editor (including Rhai script nodes) driving any
> property.
> Repo: https://github.com/AldaGs/painbychoice

---

## Architecture

Four crates, deliberately layered. `core` is headless and knows nothing about
GPUs or windows — the engine must be testable by rendering a frame in a unit
test, not a window. This separation is the whole design; keep it.

```
crates/
  core/    document model + evaluation engine. No GPU, no windowing. (unit-tested)
  render/  evaluated Scene -> pixels. SVG backend (offline). vello lives in live/.
  app/     offline binary `motion`: evaluate the demo doc -> out/frame_*.svg
  live/    the real editor `pbc`: winit + vello (wgpu) + egui over the engine
```

### The core idea

Every animatable property is a `Value<T>` — a *recipe*, never a baked result:
a constant or a keyframe track today; an expression / parametric-node IR later.
`evaluate(&doc, t)` is a pure function that resolves the whole scene graph at
time `t` into a flat `Scene` of draw items. Non-destructive editing and
non-linear scrubbing both fall out of that single design choice.

A resolve takes an **`EvalCtx`** rather than a bare frame:
`Value::resolve(&self, ctx: &mut EvalCtx)`. `EvalCtx` carries the frame, the
document, a resolve cache, and a warnings sink — one struct threaded through the
whole walk so nothing needs re-plumbing as the engine grows. `evaluate` builds
one context and shares it down the walk (`&mut`, because resolving an expression
mutates the cache).

Every evaluated item carries a `source: NodeId` (provenance) so a frame traces
back to the node that produced it — used for click-to-select and debugging.

### Expressions (`core/src/expr.rs`)

A third `Value` arm beside `Const`/`Keyframed`: `Value::Expr(Expr)` computes a
property from *other* values. This is the shared substrate roadmap #5 is built
on — expressions and (later) a node graph are two front-ends that lower to the
same `Expr` IR (the EBN "IR + dumb-printer" split: the IR is data, evaluation is
a pure tree-walk).

- **Dynamic↔typed edge.** An expression works in `ExprValue { Num, Vec2, Color }`
  and pins the type down only at the property, via `FromExpr`/`ToExpr` (impl'd
  for exactly the scriptable types — never `BezPath`). A kind mismatch resolves
  to `T::fallback()` (a neutral zero), never a failed frame.
- **The IR** is deliberately tiny: `Lit`, `Ref { node, prop, time_offset }`, and
  `Add`/`Mul`/`Neg` (`a - b` lowers to `Add(a, Neg(b))`). `Ref`'s `time_offset`
  is the `valueAtTime(t')` case — sampling another property at a *shifted* frame.
- **Dependency resolution is pull-based DFS** — a dependency resolves because you
  recurse into it, so there's no separate topo sort. `EvalCtx`'s `ResolveCache`
  adds a `visiting` set (a back-edge is a **cycle** → a `scene.warnings` entry +
  a neutral fallback, so a self-referential doc warns instead of hanging) and a
  `(node, prop, frame)` **memo** (the frame is in the key, so an off-time sample
  can't poison the primary value's slot).
- **Determinism** is by construction: every node is a pure function of the frame
  and the values it reads — no IO, no clock. That's the same sandbox a script
  engine (Rhai, next) and WASM plugins will reuse.

No authoring UI yet — expressions are built in code or a hand-edited `.pbc`
(`Value::Expr` serializes like any other value). The node-graph panel that lets
you *build* them is the next #5 step.

### Shared animation modules (`Module` + `Expr::Use`)

The document-wide property graph, made concrete (2026-07-19), with UI.

A `Module { name, params, body }` lives on the `Project`, not a node: one
definition, addressable from every comp. A property links it with
`Expr::Use { module, overrides }`. Editing the definition edits every link.

This is less "new engine" than promoting a pattern the expression graph already
supported by convention (park the animation on a controller node and `Ref` it)
into a first-class object with a real definition site, per-link overrides, and
automatic retiming.

- **Overrides are call-by-value, evaluated in the caller's scope**, before the
  module's scope is pushed. That is what lets a link feed the module its own
  `t01` or a node param, and it keeps the body a pure function of its knobs.
  A module body has no state to read back, so laziness would buy nothing and
  cost a borrow-checker fight over storing `&Expr` in the context.
- **Override is a layering, not a fork.** A link stores *only* the knobs it wants
  different; the rest inherit, so a later edit to the module still reaches an
  overridden instance. Same shape as `Value`'s const→keyframe→expr layering.
- **`param("x")` inside a body means the module's knob**, not the owning node's —
  a module is closed over its own scope, which is what makes it reusable. An
  explicit `Param { node: Some(id) }` still reaches that node deliberately.
- **Retiming is free**: a body reading `t01`/`localTime` reads *whichever layer
  is resolving*, so one module fits itself to every clip. That is the subtitle
  story, and it is a unit test.
- **Third cycle guard, same discipline** as the property and comp ones: a module
  that links itself warns and falls back rather than recursing. An override
  naming a knob the module lacks warns too — a silent no-op would be a typo trap.

**The UI**, in the graph panel: a **Modules** list (rename / delete / **edit**),
a **`-> module`** button on any expression-driven property, and a **`link`**
picker on any property that isn't one yet. A link's box shows a module picker
and one row per knob reading either `inherit` or the overridden value.

The **edit** button is the graph-UI step (below): it opens the module's *body*
on the same node canvas a property uses, plus the module's own knobs — see
*Editing a module body* below. Before it, a module could be *made* (by
extracting a property) but its body edited nowhere; you could only relink or
tweak knobs at a call site.

- **Extract is a no-op on the frame.** The recipe moves to the module and the
  property links it, so pressing `-> module` on work you care about is safe.
  Unit-tested by evaluating either side and comparing.
- **`inherit` is spelled out, not implied by a blank field.** The point of a
  module is that unset knobs keep following the definition; a UI that can't show
  the difference between "inheriting 0.4" and "overridden to 0.4" hides it.
- Repointing a link keeps overrides whose knob names still exist, and deleting a
  module leaves its links warning rather than silently reverting.
- `apply_graph_op` takes the whole `Project` now, since modules are project-wide.
  The tests that predate projects go through an `apply_op` shim.

- `ExprKind::Use` is deliberately **not** in `ExprKind::ALL`: that list is the
  in-box kind picker, and seeding a link needs a *module* picker, not a bare
  kind. A property links a module through `-> module` / `link`; a module body's
  own boxes can't spawn a nested link from the kind menu (which keeps a module
  from accidentally linking itself while you edit it). Repointing an existing
  link still uses the module picker inside `use_editor`.

### Editing a module body (the Blender-standard graph UI step)

The next roadmap step past pre-comps, made concrete (2026-07-20). A module's
**body** is now edited on the same node canvas a property is, so a module is
authored in one place rather than only inherited from the property it was
extracted with.

The seam is a `GraphTarget`: every tree-editing op (`SetKind` / `SetLit` /
`SetRef` / `SetScript` / `SetParam` / `SetWaveform` / `SetOverride` /
`SetModule`) now carries `GraphTarget::Prop(kind)` **or**
`GraphTarget::Module(id)` instead of a bare `PropKind`, and `edit_expr`
resolves the tree root from it — the node's property, or `module.body`. The
canvas, the box layout, the kind picker, and every in-box editor are byte-for-
byte the same for both; only the address differs. That is what makes this cheap:
the module body reuses the whole property-canvas machinery.

- **No selection required.** A module body isn't any node's property, so
  `apply_graph_op` takes `selected: Option<NodeId>`; the node-scoped ops
  (promote/bake/extract/link and any `Prop`-targeted edit) no-op without one,
  while module edits go through regardless. Which module is open is **view
  state** (`App::editing_module`), reported as a `GraphEdits::edit_module`
  intent and applied beside the document op, never in it — the same discipline
  as the canvas' box positions.
- **A module's knobs are editable too**, through the same parameters surface a
  node has: `ParamOwner::{Node(id), Module(id)}` says whose knobs an add/remove
  touches. A body full of `param("…")` nodes is useless without knobs to point
  them at, so the two ship together; removing a knob leaves the body's
  `param()` warning and falling back, like any dangling reference.
- **Deleting the open module closes it** (the app clears `editing_module`), so
  the panel can't keep editing a body that no longer exists.
- **Left for later, deliberately:** a link's *override* can be a whole
  sub-expression, but that's still shown-not-edited — it would want its own
  nested canvas at the call site, which the body canvas doesn't provide. And a
  module-body script's `param("x")` previews as a fallback (the module scope
  isn't pushed for the preview) though it resolves correctly at render time
  through the link.

### Icons (`live/src/icon.rs`)

Tabler Icons as a font (MIT, subsetted to 8 KB — see `live/assets/NOTICE.md`).
Registered in its **own** family rather than as a fallback on the proportional
one: a fallback would let any missing character silently resolve to an icon
glyph, turning a text bug into a baffling picture. `icon::text` / `icon::button`
ask for the family explicitly, so an icon is always deliberate.

Every glyph is a **named const** — a raw `"\u{ea62}"` at a call site is
unsearchable, and you can't tell a chevron from a trash can by reading the diff.
Adding an icon means adding a const *and* re-running the subset with its
codepoint; miss the second step and it renders as tofu, which is a visible
failure rather than a blank button.

### Frames are the native time domain

`core` thinks in **frames, not seconds**. `Keyframe.frame` is an `i64`, and
`evaluate(&doc, frame)` takes a *fractional* frame: keys sit on the grid, the
playhead need not (which leaves room for sub-frame sampling — motion blur —
later). Seconds are a **presentation unit**, converted only at the edges by
`core/src/timebase.rs`.

The payoff is that `fps` never has to be threaded into the value engine: a
track is a function of frames, and only the composition knows what a frame is
worth in wall-clock time. Changing fps therefore re-times the document without
drifting keyframes off their frames — the same thing After Effects does.
Integer frames also killed two float-epsilon fudges (key matching, and
neighbour clamping when dragging).

### The work area (2026-07-20)

AE's work area: a comp-level **preview** range that bounds the playback loop.
The deliberate distinction it turns on — recorded back when the layer time model
landed — is **view state vs document state**: the work area bounds *playback*,
so it changes nothing the renderer sees and is never saved with the `.pbc`;
per-layer in/out points change *evaluation*, so they are. `App::work_area:
Option<WorkArea>` sits beside `view` and resets the same way, when a comp opens.

- **The loop is the only thing confined.** `raw_time()` folds the wall clock
  into the work-area span *while playing* (`wrap_into` over `loop_bounds_secs`);
  **while paused** the playhead sits exactly where it was placed. So scrubbing
  and `←/→` still reach the whole comp — you can park on a frame outside the
  band to inspect it — and only *looping* stays inside. Restart (`R` / the
  button) returns to the work-area start, not always frame 0.
- **Set with `B`/`N` at the playhead** (AE's keys): `B` the start, `N` the end.
  The end is **exclusive** (like a layer clip's `[in, out)`), so `N` at frame F
  keeps F as the last previewed frame. The first press seeds the other edge from
  the comp extent, so one keystroke makes a valid range.
- **The math is pure and tested.** `loop_bounds` (work area clamped into the
  comp, `hi > lo` always — the span can't invert or empty), `wrap_into` (the
  cyclic fold, holding at `lo` on a collapsed span), and `with_work_start` /
  `with_work_end` (the edge-seeding) are free functions in `timeline.rs`, unit-
  tested without a window; `App`'s methods are thin wrappers. The ruler band is
  drawn from `loop_bounds` on the same `Axis` the playhead uses, so they can't
  drift.
- **Ephemeral, by choice.** It could persist with the layout later, but AE-style
  per-comp preview state is genuinely session-scoped; keeping it out of the
  `.pbc` avoids a format change for something you re-set constantly.

### The preview camera (2026-07-20)

Zoom + pan for the canvas, layered onto the fit that was already there. The
state is `App::nav: CanvasNav { zoom: Option<f64>, pan: (f64, f64) }` — **view
state**, like `view` and `work_area`, reset to Fit when a comp opens.

- **Fit is `zoom == None`**, recomputed each frame from the (possibly resized)
  canvas rect so the comp stays framed as panels move. It insets the canvas rect
  by `FIT_MARGIN` (20 logical points) on every side — the deliberate gap from the
  surrounding panels. `Some(z)` pins a fixed zoom at `z` *logical points per comp
  pixel* (100% = 1.0), positioned by `pan`, an offset in physical pixels from the
  centred placement.
- **All the math is pure and lives in `scene.rs`**: `canvas_transform` (the one
  the render path calls, replacing the bare `fit_transform`), `canvas_scale`,
  `nav_zoom_about` (zoom about a point — keeps the comp point under the cursor
  fixed, the invariant that makes the wheel feel right, and a unit test), and
  `fit_area`. Scale is clamped to `[MIN_SCALE, MAX_SCALE]`.
- **Input in the winit handler**: scroll = zoom about the cursor, middle-drag =
  pan (`pan_drag` holds the press cursor + pan; pan tracks the cursor 1:1 in
  physical pixels). Starting a pan from Fit first pins the current framing as an
  explicit zoom so the pan has a fixed scale to move against.
- **The toolbar is a stacked strip, not a floating card.** The canvas leaf gives
  up a `CANVAS_BAR_H` strip at its bottom edge; `canvas_toolbar` fills it with
  the panel fill and reports picks through `CanvasEdits`, applied after the UI
  pass (the same defer-then-apply discipline as every other panel). Built to hold
  more preview tools later.

### The transform gizmo (2026-07-20)

`live/src/gizmo.rs`. On-canvas move / rotate / scale handles for the selected
layer. Two decisions carry the design:

- **Painted with egui, not vello.** egui's pass runs *after* the vello render in
  `App::redraw`, so a plain `ui.painter()` lands on top of the frame — no
  compositing work, and `ui.interact` supplies hover and drag for free.
- **It emits ordinary `PropEdits`.** A handle drag fills in `pos_x`/`rot`/
  `scale_x`/… — the same struct the properties panel's DragValues fill in — so
  it goes through `apply_edits` and **auto-keys exactly like typing the number
  does**. There is no second write path into the document that could disagree
  with the first.

The arithmetic lives in `resolve_drag`, which is pure (no egui, no `App`) and
therefore unit-tested without a window, the same way `apply_fps_edit` is. It
resolves every delta against a **pre-drag snapshot** (`GizmoDrag`) rather than
stacking on the previous frame — re-deriving from a moving base accumulates
error, and with an auto-keying property that error gets baked into a keyframe.

Three gotchas worth keeping:

- **The pivot is `position`, not `anchor`.** `Transform::resolve` builds
  `translate(position) · rotate · scale · translate(-anchor)`, so the anchor
  point *maps to* `position` in parent space. Rotation and scale hang off there.
- **The parent matrix is recovered, not looked up.** `GizmoTarget::new` computes
  `parent = world · local⁻¹` from the node's world matrix (`Scene::places`,
  so a bare group gets handles too). That is why
  `anchor` had to join `NodeInfo`: leave it out of `local` and the recovered
  parent is wrong, so the gizmo tracks the cursor at an offset. A zero scale
  makes `local` singular, so the recovery substitutes a scale of 1 (an
  approximation, but it beats handles at NaN).
- **It must never claim the whole canvas.** `is_pointer_over_egui` is what tells
  the winit handler to skip click-picking, so `gizmo_ui` calls `ui.interact` on a
  small rect **around the pointer**, and only while a handle is under it or a
  drag is live. A canvas-wide interactive rect would make the preview unclickable
  everywhere the gizmo is shown.

Handle sizes are constants in **logical points**, not comp units, so the gizmo
stays the same size on screen at every zoom — a gizmo that scaled with the comp
would vanish exactly when you zoomed out to grab it.

### Grid, rulers and guides (2026-07-20)

`live/src/aids.rs`, driven by `Comp::aids` (`ViewAids { grid, rulers, guides }`).
Toggles live in the preview's tool strip; right-click **Grid** for spacing and
subdivisions, right-click **Guides** to clear them all.

All three are **saved in the `.pbc`**, unlike the zoom/pan camera which is
session state. A guide you dropped to line up a title is part of how the
composition was built; losing it on reopen would defeat the only job guides
have. Grid spacing and guide positions are in **composition pixels**, never
screen pixels, so they stay put under zoom and mean the same thing at any
magnification.

**Rulers take space; the grid and guides float.** The ruler band is subtracted
from the canvas leaf's rect exactly as the zoom strip already subtracts
`CANVAS_BAR_H`. That is not cosmetic: the remaining rect *is* the canvas rect,
so it feeds `canvas_transform` and therefore `pick`. Paint rulers over the
canvas instead and every click under one selects geometry the user can't see.
`ruler_inset()` is the single authority on the size, used by both `App` and this
module so they cannot disagree about where the canvas is. Toggling rulers
therefore resizes the drawing area and re-fits the comp.

Guides are dragged out of a ruler, moved by dragging, and deleted by dragging
back onto a ruler (the standard gesture, and the only delete besides *Clear*).
The in-flight drag lives on `App`, not in the document, so a half-finished drag
can't be saved or bump `doc_rev`. Hiding guides is a view toggle that keeps them
— a hidden guide is also un-grabbable, so it can't intercept a click meant for
the artwork beneath it.

Two loops here are driven by a step value, and **a non-finite or zero step never
terminates**, hanging the editor. Both are guarded, and both guards are tested:
`Grid::step()` special-cases non-finite *before* clamping, because `f64::clamp`
propagates NaN rather than rejecting it — a hand-edited `.pbc` is all it takes.
`ruler_step()` does the same for a degenerate zoom. Grid levels are also skipped
once their lines fall within 5px on screen, or a zoomed-out comp would draw
thousands of overlapping lines into a solid wash.

Like the gizmo, `aids_ui` returns whether it owns the pointer, and `App` gates
click-picking on it (`App::aids_hot`) — `is_pointer_over_egui` is area-based and
stays false inside the canvas hole, so an interactive overlay there cannot
suppress picking on its own. See the gotcha of that name below.

### The anchor handle and the selection box (2026-07-21)

The gizmo grew a **ring just outside its centre**, drawn as a circle crossed by
four ticks: drag it to move the layer's anchor. It sits between the free-move
square and everything else, and is hit-tested before the arrows — which pass
straight through that radius — because it is the smaller, more specific target.

**Dragging it moves the pivot without moving the layer**, which is After
Effects' Pan Behind tool. That takes two coordinated edits, not one. The layer
draws at `pos + R·S·(q − anchor)` for each local point `q`, so holding that
fixed while the pivot follows the pointer by `delta` needs:

    pos'    = pos + delta
    anchor' = anchor + (R·S)⁻¹ · delta

Emitting one without the other is the classic version of this bug: move only
the anchor and the artwork jumps; move only the position and the pivot doesn't
end up where you dropped it. A test checks a *rotated, non-uniformly scaled*
layer, since that is where a naive `anchor += delta` looks right on a plain
layer and is visibly wrong on a real one. A collapsed scale makes `R·S`
singular, so that case holds rather than writing infinities into the document.

Editing **Anchor in the properties panel deliberately does not compensate** —
there you are asking to re-origin the layer, and it should move. Which behaviour
you get is which control you reached for, matching AE.

The anchor is now a full animatable property: `PropKind::Anchor` per the recipe
above, so it has a panel row, a stopwatch, a dopesheet row, retiming, easing and
copy/paste for free. It was already addressable from expressions
(`PropPath::Anchor`) but had no UI — an odd asymmetry, now closed.

**The selection box is one box per drawable item** in the selection's subtree,
not a single rect around the lot. A union tells you only the group's extent,
which is the least informative thing about it; per-item shows what the group
contains and where each piece sits. For a plain single-shape layer — the common
case — the two are identical, so nothing is lost.

Bounds come from each item's path through its world transform, giving the
axis-aligned box of the rotated shape rather than a rotated rectangle — "how
much room does this take up". They are drawn with corner ticks and are
deliberately **not grabbable**: resizing by a bbox corner would fight the scale
handles, which already own that gesture. A layer that draws nothing gets no box
at all, rather than a zero-size rect at the origin that would look like a bug.

Note this is computed in `live` from the evaluated scene. Bounding-box *snapping*
would want it in `core` beside `Scene::places` instead, so sibling bounds are
available without a second pass.

### Snapping (2026-07-21)

A gizmo **move** snaps to the composition edges and centre, to visible guides,
and to visible grid lines. `Comp::aids.snap` toggles it (tool strip: **Snap**);
holding **Ctrl** bypasses it for one drag, as in Blender and Figma, so precise
placement is a key away rather than a trip to a toggle and back. The line you
snapped to is drawn in pink, because otherwise a snap is indistinguishable from
a stuck cursor.

**You snap to what you can see.** One flag, not one per target: showing the grid
arms grid snapping and hiding it disarms it. That is one less thing to keep
consistent, and the screen already tells you what is armed. Composition edges
and centre are the exception — they exist whether or not anything is shown, and
are what you align to most.

Four decisions worth keeping:

- **The tolerance is in screen points** (`SNAP_PX`), converted to comp units per
  frame by `snap_tolerance`. In comp units it would grow as you zoom out until
  everything snapped and shrink as you zoom in until nothing did. In screen
  terms the pull feels constant, and *zooming in is how you escape a snap* —
  which is what people already expect. Same lesson as the guide grab band.
- **Snapping happens in composition space**, then converts back to the layer's
  parent space. Guides and the grid are comp-space objects; snapping in parent
  space would silently mean something different at every nesting depth.
- **An axis drag keeps its constraint.** The correction is projected onto the
  arrow's axis (in comp space, since the parent may rotate it), so a drag slides
  *along* the arrow onto a guide and can never be pulled sideways off it.
  Applying the raw 2D offset would break the one promise the arrow makes.
- **Grid lines are never enumerated.** The nearest multiple is computed
  directly, so a 1px grid on a 100,000px comp costs the same as a 500px grid
  instead of building a million candidates.

Moves only: rotating or scaling *to* a guide is a different question with a
different answer (an angle, not a point), and pretending a position snap covers
it would just make those handles stick for no visible reason.

Not yet: snapping a layer's **bounding-box edges** rather than its pivot. That
is what you want for laying out a title against a margin, and it needs per-frame
bounds for the dragged layer plus a candidate set from its siblings.

### The motion path (2026-07-20)

`live/src/motionpath.rs`. An animated layer's pivot trajectory, drawn on the
preview as a curve with a dot per frame — dot *spacing* is the reading, since
bunched dots are slow and spread dots are fast, which a bare curve can't show.
Keyframed samples draw as larger squares, the current frame gets a ring.

**Only position gets one, and that is a design decision, not a gap.** Position
*is* a spatial curve: it lives in the same space as the canvas, so drawing it
shows the data rather than a visualisation invented for it. Nothing else on a
layer has that property — a "rotation path" would be a made-up mapping, and
value-over-time for every other property is already better served by the
dopesheet and the graph editor. The whole-layer equivalent for everything else
is **onion skinning** (ghost the rendered layer at ±N frames), which covers
rotation, scale, opacity, fill, shape params and text in one feature instead of
one visualisation per property. Not built yet.

Each sample is a full `evaluate_comp`, and the point is read out of the scene's
new pivot table. That is not the cheap way — the cheap way walks the ancestor
transforms directly — but it is the only way the path cannot disagree with the
canvas: parent chains, expressions, pre-comp instancing and `LayerTiming`'s
local-frame shift all bend where a layer actually is, and re-deriving that
outside `eval.rs` would be a second implementation of its walk, silently
drifting from the first. Same reasoning as the gizmo emitting ordinary
`PropEdits` rather than writing the document itself.

**The cost is real and the cache is what makes it affordable.** A ±60 window is
121 scene evaluations, so the path rebuilds only when its key changes —
selection, frame window, or `App::doc_rev`, a counter bumped on every document
change. It *does* rebuild on every frame of a gizmo drag, since each delta bumps
the revision. That is the case to watch on a heavy comp and the first thing to
optimise if it ever feels slow. `Comp::motion_path_range` (default 60, capped at
`MAX_RANGE`) is therefore a cost dial as much as a clutter dial.

Display-only for now: dragging a keyframe along the path needs per-key
hit-testing and a spatial-tangent story, and wants the path proven correct first.

### `Scene::places` — why a node's place is separate from its drawing

`eval.rs`'s walk records a `Placement { world, pivot }` for every live node,
alongside the draw list. A node need not draw anything — a group or a null has
no shape and so no `RenderItem` — but it still has a place, and it is exactly
the sort of layer you parent things to and animate. **Both** editor overlays
read it: the motion path takes `pivot`, the transform gizmo takes `world`, and
so both work on a bare group.

Three properties are load-bearing:

- `pivot` is the **anchor**, not the local origin. `local` maps the anchor point
  to `position` by construction, so this is the point the layer rotates and
  scales about, and the point the gizmo centres on. An overlay drawn anywhere
  else sits away from where the layer visibly turns.
- `world` is the whole **parent chain** multiplied out, which is what an overlay
  needs to know which way the layer's axes point — not merely where it is. A
  pivot alone would place the gizmo correctly but aim its arrows wrong.
- A node outside its time window is **absent, not zeroed**. The walk returns
  before reaching it, so "the layer isn't here on this frame" is expressible —
  which is what lets the motion path break its polyline instead of drawing a
  line to the origin, and what stops the gizmo lingering over an off-screen
  layer.

### Colours

Three surfaces, deliberately distinct, and they live in three different places
for a reason:

| Surface | Value | Where it lives |
| --- | --- | --- |
| UI chrome (panels, headers, widgets) | `#2d2d2d` | `theme::UI_BASE` → `egui::Visuals` |
| Preview backdrop (the letterbox) | `#23262d` | `theme::PREVIEW_BACKDROP`, vello's `base_color` |
| Composition area (inside the frame) | `#5d677e` **default** | `Comp::bg` — **document data** |

The backdrop is vello's, not egui's, because the canvas is a GPU hole with no
egui behind it. The composition colour is a **per-comp setting** rather than a
theme constant: it is what the frame renders against, so it belongs to the
document and is saved with it (`#[serde(default)]`, so pre-`bg` `.pbc` files load
on the default rather than rendering transparent). Edit it in the composition bar
under **BG**. Widget states are `shade()` offsets from `UI_BASE`, so re-tinting
the editor is one constant.

### `live/` module layout

`main.rs` grew to ~5,100 lines and was split by concern (2026-07-19). It was a
**pure move** — the only edit was widening visibility to `pub(crate)`; no
renames, no reordering, no logic changes, and the test count was identical
either side of the split.

```
live/src/
  main.rs      the `use` block, module decls, and `fn main` (~66 lines)
  app.rs       App: window/GPU state, winit handler, the per-frame update
  timeline.rs  transport bar, ruler, clip bar, dopesheet
  graph.rs     expression / node-graph panel and its GraphOps
  props.rs     properties panel, easing, and the PropKind enumeration
  dock.rs      panel layout tree, its editors, the composition bar
  layers.rs    scene-tree panel
  scene.rs     evaluated Scene -> vello, canvas fit/zoom/pan + pick, zoom toolbar
  tests.rs     the unit tests
```

Every module opens with `use crate::*;`, and `main.rs` re-exports each module
with `use <module>::*;`. So the crate root is one shared namespace and no module
keeps its own import bookkeeping — moving an item between modules needs no
import edits at either end. (Modules are children of the crate root, so this
glob reaches root's private items too; that's why it works.)

What the split deliberately did **not** do: `App::update` is still ~850 lines.
Moving it made it findable, not simpler. Decomposing it is a separate job — the
UI pass has real ordering constraints (measure the canvas → run egui → apply
edits after) that a naive split would break.

**UI discipline in `live/`:** the egui closure never borrows `App`. Each panel
reads a plain snapshot gathered before the closure and reports intent into a
small `*Edits` struct; `App` applies those after the closure. This keeps the
render-path field borrows (state/context/renderers/doc) from colliding. Follow
this pattern for any new panel.

## Run it

```bash
cargo test --workspace     # engine + hit-test unit tests (all green)
cargo run -p motion-live   # THE EDITOR (opens a window) — this is the app
cargo run --bin motion     # offline: writes out/frame_00.svg .. frame_08.svg
```

Windows note: the running `pbc.exe` locks the binary, so `cargo build` can't
replace it while open. Kill it first: `taskkill //F //IM pbc.exe`.

## What works today (live editor)

- **Composition bar** (top) — editable Size (W×H), FPS, Duration, **BG**. Drives
  canvas fit, playback, frame step, timeline. Comp bounds drawn with a fill +
  border, the fill being `Comp::bg` — a per-comp **setting** saved in the `.pbc`
  (default `#5d677e`), not a theme constant. See *Colours* below.
- **Canvas** — vello rasterizes `evaluate(doc, t)` each frame; click a shape to
  select (front-most, via `NodeId` provenance). Selection gets a yellow outline.
  **Zoomable + pannable**: scroll zooms about the cursor, middle-drag pans, and a
  stacked tool strip at the bottom offers **Fit** plus fixed zoom stops
  (25/50/100/200/400/800%). **Fit** (the default) frames the comp in the canvas
  area with a 20px gap from the surrounding panels, and re-fits as they resize.
  See *The preview camera* below.
- **Transform gizmo** — the selected layer gets on-canvas handles: a centre
  square (free move), an X/Y arrow each (move along the *layer's* own axis), a
  box at the end of each axis (scale that axis), a corner box (uniform scale),
  and a ring (rotate). Everything pivots on the **anchor point**. The handles are
  a fixed size in screen points, so they stay grabbable at any zoom. See
  *The transform gizmo* below.
- **Transport** — Play/Pause (Space), Restart (R), ←/→ frame step, scrubbable
  playhead (an integer slider, so it can only land on frames). Readout is
  `hh:mm:ss.ff` plus `[frame/last]`. Playback runs off the wall clock but
  *quantizes* to the frame grid, so changing FPS visibly changes the playback
  cadence.
- **Work area** (AE's) — a comp-level **preview range** shown as a translucent
  band on the ruler. `B` sets its start at the playhead, `N` its end; **playback
  loops within it** and Restart returns to its start. It's **view state** — it
  bounds the loop, never evaluation — so it isn't saved with the document and
  resets when a comp opens. Scrubbing and frame-stepping still reach the whole
  comp (only *looping* is confined), so you can inspect a frame outside the band
  while paused. See *The work area* below.
- **Layers** (left) — scene tree; select, reorder (▲/▼), add Rect/Ellipse/Group,
  delete (✕), Save…/Load… (`.pbc` JSON via serde — document *and* panel layout).
- **Properties** (right) — resolved values for the selection; drag or click-type
  to edit. A painted **stopwatch** per property (filled = animated, hollow =
  constant) inserts a keyframe at the playhead — first click on a constant
  promotes it to a track (this is how a property *starts* animating).
  Transform (Position/Rotation/Scale/Opacity), **Fill**, **Stroke** (color +
  width, with add/remove — a node without one shows `+ add`), and **parametric
  geometry**: Size for a Rect or Ellipse, Radius for a Rect. Rows appear only
  where the property exists — a group has no fill, an ellipse no radius, an
  imported `Path` no size. **All of them are animatable**, on equal footing:
  every one gets a stopwatch, a dopesheet row, and the full selection /
  retime / copy-paste / easing treatment.
- **Timeline / dopesheet** (bottom) — a **frame ruler** with adaptive ticks
  (1/2/5/10-frame steps plus whole-second multiples, so labels land on round
  timecodes when zoomed out; per-frame minor ticks once frames are ≥6px apart),
  then one row per animated property with keyframes as diamonds and a red
  playhead. Click track to seek, click a diamond to select (Del removes), drag
  to retime (clamped between neighbours). Everything **snaps to frames** at any
  zoom.
  - **Multi-select**: ctrl/shift-click a diamond toggles it in or out of the
    selection; dragging a box on empty track marquee-selects everything inside
    it (replacing the selection, so shrinking the box deselects). Selected keys
    are drawn larger with a red border.
  - **Group retime**: dragging any selected diamond moves the *whole* selection
    as a rigid block, so internal spacing is preserved. The block clamps against
    its own outer neighbours rather than each key's — and across every affected
    property at once, so a multi-property selection translates instead of
    deforming. Dragging an unselected key selects it first.
  - **Copy/paste** (ctrl+C / ctrl+V): copies whole keyframes — values *and*
    easing handles — and pastes them with the block's first key on the playhead,
    spacing intact. Pasting selects what landed, so the next drag moves it.
    Suppressed while a text field has focus.
  - **Scroll** to zoom (the frame under the cursor stays pinned), **shift+scroll**
    to pan.
  - **Edge auto-pan**: while dragging the ruler or a keyframe, hold near either
    end of the track and the view scrolls that way — so a key can be dragged
    past the visible range. Drag-only on purpose; hover-panning would scroll the
    timeline out from under the pointer.
- **Easing editor** — selecting **exactly one** keyframe reveals a CSS-style
  cubic-bezier editor for its outgoing segment: draggable control points +
  Linear/Smooth/Ease In/Ease Out presets. Deliberately hidden for a multi-key
  selection: a segment belongs to one key, so there is no "the" curve for a set.
- **Dockable panels** — every area carries a header: an editor picker to change
  what it shows, plus split (`|`/`-`) and close (`x`). Drag the splitters to
  resize. A **Layout** menu (comp bar) switches between built-in presets
  (`Default`/`Animation`/`Design`) and saves the current arrangement as a
  session preset; the active layout and user presets are written into the
  `.pbc`. The canvas and the comp/transport toolbars are fixed chrome (no
  header), which keeps the single-canvas invariants safe.
- **Graph / expressions** — a summonable **Graph** panel (pick it in any area's
  header) drives the selected node's properties with expressions. `= fx`
  promotes a property (seeded from its current value); `bake` freezes it back to
  a constant. The expression is a **node canvas** — boxes wired parent↔child,
  each a `value` / `ref` (another node's property, at an optional frame offset) /
  `add` / `mul` / `neg` / **`script`** (a Rhai one-liner over `frame`/`time`,
  with its live result or error shown). Drag boxes to arrange them. A cycle or a
  bad script falls back to a neutral value instead of breaking the frame.

## Key code locations

- `core/src/timebase.rs` — `Timebase`: the **only** place that converts between
  seconds and frames, plus `timecode()` → `hh:mm:ss.ff` (non-drop-frame).
  Reach for `doc.timebase()`; never divide by `fps` by hand.
- `core/src/value.rs` — `Value<T>`, `Track<T>`, `Keyframe` (`.frame: i64`),
  `Handle`, easing solver. Keyframe ops: `set_at`, `insert_key` (const→track),
  `move_key` (neighbour-clamped, ±1 frame), `remove_key`, `key_frames`,
  `segment_handles` / `set_segment_handles`.
  Multi-key ops: `move_keys_limits` / `move_keys` (rigid block, clamped against
  the *block's* outer neighbours — see the doc comment for why per-key clamping
  collapses a selection), and `keys_at` / `insert_keys` for copy/paste.
- `core/src/node.rs` — `Node`, `Transform`, `Shape` (parametric Rect/Ellipse/
  Path), `Document`. Tree ops: `find`, `find_mut`, `reorder_child`, `remove`.
  Also `Document::timebase()`, `duration_frames()`, and **`migrate()`**.
- `core/src/eval.rs` — `evaluate(doc, frame) -> Scene`, `RenderItem`
  (+provenance).
- `core/src/expr.rs` — expressions: `EvalCtx` (the resolve context: frame, doc,
  cache, warnings), `ExprValue` + `From`/`ToExpr`, the `Expr` IR, `PropPath`,
  `eval_expr` with the memo + cycle-detecting `ResolveCache`, and `eval_script`
  (Rhai, on a thread-local engine) for `Expr::Script` nodes.
- `core/src/demo.rs` — the demo document loaded on launch.
- `live/src/main.rs` — everything UI. `App::render` is the per-frame heart:
  evaluate → hit-test → gather snapshots → run egui → apply `*Edits` → GPU. Panel
  fns: `comp_ui`, `tree_ui`, `transport_ui`, `dopesheet_ui`, `properties_ui`,
  `graph_ui` (the expression editor; `apply_graph_op` applies its deferred
  edits), `ease_editor`, `key_button`. Each panel fn renders into a `&mut Ui` it is
  handed — it does **not** create its own `egui::Panel`; placement is the
  layout tree's job (see below).
  Timeline mapping: `TimelineView` (the visible frame window) + `Axis`
  (frame↔pixel), built once by the ruler and reused by every row so they cannot
  drift out of alignment.
  Keyframe selection: `KeyRef` = `(PropKind, index)`, `KeySelection` =
  `BTreeSet<KeyRef>` (ordered so `group_selection_by_prop` can bucket it in one
  pass), `KeyClipboard`/`ClipTrack` for copy/paste — `ClipTrack` is the
  type-erasure boundary that keeps `Vec2` keys off a scalar property.
  **`prop_of` / `prop_of_mut`** are the single place `PropKind` is matched:
  they hand back a `PropRef`/`PropRefMut` (Vec2 | Num | Color) and every
  keyframe op goes through that. See below.
  Graph canvas: `layout_expr` (tidy-tree placement, `box_height` per kind) +
  `expr_canvas`/`expr_box` draw it; every edit is one deferred `GraphOp` keyed by
  `(property, tree-path)` applied by `apply_graph_op` (a free fn over
  `&mut Document`, so it's unit-tested). Node positions are ephemeral egui-memory
  view state, not saved with the doc.

### The panel layout tree

`Dock` is a binary tree — `Split { side, size, resizable, first, second }` with
`Editor` leaves — borrowed from EBN's `layoutTree`. A split pins `first` to one
edge at `size` points and gives `second` the remainder, so the tree's nesting
*is* egui's outermost-to-innermost panel order, and `show_dock` renders the whole
thing by recursing into a plain `Ui`. `Dock::default_layout()` builds the stock
arrangement; adding named presets means writing more constructors.

Two things are load-bearing:

- **The canvas is a leaf.** vello paints it, not egui, but it must occupy a leaf
  so the tree knows where the leftover hole is. It has to be the *innermost*
  one (there's a test): every other panel claims an edge, the canvas is what
  remains.
- **The fit takes that leaf's measured rect**, not the window minus hardcoded
  panel sizes. The constants version could not survive a draggable splitter:
  `pick` inverts this transform, so stale geometry doesn't just misdraw the
  canvas, it sends every click to the wrong shape. The leaf measures itself with
  **`available_rect_before_wrap()`, not `max_rect()`** — egui shrinks a `Ui`'s
  *available* region for the sibling panels shown before the canvas leaf but
  leaves `max_rect` at the full window, so `max_rect` would fit the comp to the
  whole window and float the tool strip in the window corner (see *Known issues*).

`size` is stored in the tree (and written back from the real panel rect each
frame) rather than living only in egui's panel memory — that's what keeps the
tree the source of truth, so saving layouts is a `serde` derive rather than a
scrape of egui internals.

**Split / join / retype.** Every *content* area wears a thin header
(`area_header`): an editor picker plus `|`/`-`/`x` buttons. Splitting a leaf
rewrites it to a `Split` of two clones; closing an area collapses its parent
`Split` to the surviving sibling; the picker swaps a leaf's `Editor`. These are
pure tree edits (`Dock::apply`), and — like every other panel — they don't
mutate mid-render: an area header only *records* a `DockCmd` against the leaf's
`path` (a `Vec<Branch>` naming it from the root), which `render` applies once
the egui pass is done. Restructuring the tree while its panels are still laid
out would desync egui's per-panel ids.

- **Only the three content editors are `SWAPPABLE`** (Layers, Properties,
  Dopesheet). The canvas and the comp/transport toolbars are *structural* leaves
  with **no header** — so a user can't duplicate, retype, or close them. That's
  not a UI nicety, it's what keeps the two canvas invariants safe: there is
  always exactly one canvas leaf to measure (`canvas_rect`), and it stays the
  tree's innermost leaf. A closed area is always content, so the canvas — its
  sibling, or in an untouched ancestor branch — always survives.

**Known wrinkle:** the canvas rect is measured during the UI pass, but the fit is
needed *before* it (to pick, and to build the vello scene), so the fit uses the
previous frame's rect. Stale only while a splitter or the window is actively
dragged, and self-correcting on the repaint a drag guarantees.

### Adding an animatable property

`PropKind` names every animatable property; `prop_of`/`prop_of_mut` borrow one
off a `Node` as a type-erased `PropRef`/`PropRefMut`. Everything else — dopesheet
rows, retiming, delete, copy/paste, easing, the stopwatch — is written against
that pair, so **adding a property is a `PropKind` variant, an entry in
`PropKind::ALL`, a `label()` arm, and one arm in each of the two `prop_of`
functions.** No other match statement should have to grow.

`PropRef` returns `Option` because not every node has every property (a group
has no fill; an `Ellipse` no radius; a `Path` no parametric size) — callers skip
`None` rather than branching on shape kind, which is what keeps the "does this
node have it" question in exactly one place. The two functions must agree on
which properties exist, or reads and writes silently target different things;
there's a test pinning that.

### Loading a `.pbc`: always call `migrate()`

Pre-frame-grid documents stored keyframe times as float **seconds**. A
`Keyframe` can't convert itself (it has no timebase), so deserializing parks the
old value in a serde-only `legacy_seconds` field and `Document::migrate()`
converts it using the document's own `fps`. **Any new load path must call it**;
it's a no-op on an already-migrated doc. The legacy field is never
re-serialized, so a file is permanently migrated on its first save. Keys that
round onto the same frame collapse to one.

### Saving the layout: the `Project` wrapper

The `.pbc` is a `Project { document, layout }` (both in `live/`), **not** a bare
`Document` — the UI layout can't live in `core::Document` without breaking the
headless-engine split, so the app wraps the two on the way to disk. `layout`
holds the active `Dock` and the user presets (built-ins are code, so they're
never stored; `Preset::builtin` is `#[serde(skip)]` and reconstructs as `false`).

Two rules keep this safe:

- **Reading is backward-compatible.** `load` tries `Project` first; a pre-layout
  file is a bare `Document` with no `document` field, so that parse fails and the
  loader falls back to deserializing a plain `Document` (with the default
  layout). Distinguishing the two is exactly the absent/present `document` key —
  don't give `Project::document` a serde default or the fallback stops firing.
- **A loaded layout is validated.** `Dock::is_valid` requires the invariants the
  render path assumes (one canvas, innermost; comp + transport present). A file
  that fails is dropped for `default_layout()` rather than wedging the editor
  with, say, a layout that has no way back to the comp bar. User presets are
  filtered the same way.

## Known issues / gotchas

- ~~**egui default font lacks many glyphs**~~ — fixed by bundling an icon font
  (see *Icons* above). It used to be that `◆ ◇ ● ○ ❚ ⟲ ▸` rendered as tofu, only
  `▶`/`•` were safe, and anything else had to be *painted* (see `key_button`) or
  spelled out in words. Icons now come from `icon::*`; the painted indicators
  that remain are kept because they encode state (a filled vs hollow stopwatch),
  not because a glyph was unavailable.
- **The box-select flag round-trips through egui memory.** Only a row's
  `Response` can tell us a drag began on empty track (a diamond grabs the press
  first), but the marquee rect is needed *before* the rows loop — so "a box is
  live" is stashed with `data_mut` and read on the next frame. The one-frame lag
  is invisible (the box has no area worth hit-testing until the pointer moves);
  don't try to "fix" it by hoisting the hit-test out of the loop.
- **egui eats the shift modifier on shift+wheel**, rewriting it into a
  *horizontal* scroll. So the pan signal is a nonzero `smooth_scroll_delta.x`,
  not `modifiers.shift` — checking `shift` silently does nothing.
- **`ui.max_rect()` is the whole window, even for the canvas leaf.** egui shrinks
  a `Ui`'s *available* region for sibling panels shown before it, but not
  `max_rect`. Measuring the canvas leaf with `max_rect` fit the comp to the whole
  window and floated the zoom strip in the window corner; use
  `available_rect_before_wrap()` for the leftover central region.
- **An egui panel's returned rect is content-driven, and it persists.**
  `Panel::show` hands back the *inner response* rect, which grew (or shrank) to
  whatever the content allocated — clamped only at `max_size` — and egui stores
  that same rect as the panel's `PanelState`, so the next frame starts from it.
  A leaf whose content changes height therefore resizes its own panel and shoves
  every other leaf around, canvas included. This is why selecting a layer used to
  resize the whole window: the dopesheet grows a row per animatable property, so
  every select/deselect moved the preview. The invariant that fixes it is
  `Editor::scroll_wrapped` — **every leaf must either fill its area exactly or
  scroll inside it, never allocate past it** — enforced by a test. Note the
  scroll wrapper is `ScrollArea::vertical` with `auto_shrink([false; 2])`:
  horizontal scrolling would desync the dopesheet tracks from the ruler (frames
  map across the panel's *width*), and letting it auto-shrink reintroduces the
  same bug from the other side.
- **Hit-test a drag at `press_origin`, never at the live pointer.** egui reports
  `drag_started()` only once the pointer has moved past its drag threshold, so
  by the time you handle it the pointer has usually left the small target that
  was under the press. Resolving "what did they grab?" against
  `pointer_latest_pos()` therefore finds nothing and the grab silently fails —
  the smaller the target, the more often. This is what made guides need a *held*
  click to drag: holding still kept the pointer inside the 5pt band long enough
  to be found. Use `ui.ctx().input(|i| i.pointer.press_origin())`.
  **`Response::interact_pointer_pos()` is not a substitute** — despite the name
  it tracks the ongoing interaction and moves with the drag. Both `aids.rs` and
  `gizmo.rs` resolve their grabs this way; the gizmo's handles are big enough
  that it rarely misfired, which is exactly why it went unnoticed there.
- **`is_pointer_over_egui` is area-based, not widget-based.** It asks which
  *layer* is under the pointer, and for the background layer whether the point
  falls outside the root `Ui`'s available rect. So it is `false` everywhere in
  the canvas hole **no matter what you draw or `ui.interact` there** — an
  interactive rect inside the hole does not make egui "want" the pointer. Any
  canvas-space widget that must not double as a canvas click therefore needs its
  own flag; the gizmo uses `App::gizmo_hot`. Getting this wrong is silent: the
  widget works, and the click *also* falls through to the picker.
- **Redraw is event-driven** (`ControlFlow::Wait`). Anything that must keep
  animating while the pointer is held still (edge auto-pan) needs an explicit
  `ctx.request_repaint()`, or it stops the moment input stops.
- Panel sizes are in egui *points*; the canvas fit is in *physical pixels* —
  multiply reserved sizes by `window.scale_factor()` (already done in `render`).
- LF/CRLF warnings on commit are harmless (no `.gitattributes` yet).
- **Don't round-trip source files through PowerShell** `Get-Content -Raw` /
  `Set-Content`: PS 5.1 decodes as ANSI and mojibakes every non-ASCII character
  (`—`, `×`, `▲`). Edit files directly.

## Roadmap (agreed order)

Decided sequence: **composition settings ✅ → frame-based timeline ✅ → keyframe
UX ✅ → shape/stroke params ✅ → dockable panels ✅ → node graph + expression
IR ✅ → …**. Items 1–5 are complete; the build order now continues under *Agreed
order past #5* below (pre-comps first). The stages of each item:

1. ~~**Frame-based timeline.**~~ ✅ Done. Frames are `core`'s native time domain,
   with a ruler, timecode readout, snapping at any zoom, zoom/pan, and edge
   auto-pan. Deliberately *not* borrowed from AE: a separate graph editor and
   nested comps — the inline bezier editor already covers easing.
   - Left open: `duration` is still stored in **seconds** with
     `duration_frames()` derived. Storing frames outright is arguably more
     correct (the comp end would always land on a frame boundary) but it's a
     `.pbc` format change, so it wants to ride along with the next migration.
2. ~~**Keyframe UX polish.**~~ ✅ Done. `selected_key` became a `KeySelection`
   set; ctrl/shift-click toggles, dragging a box on empty track marquee-selects,
   a drag moves the whole selection as a rigid block, and ctrl+C/V copies keys
   (values *and* easing handles) to land on the playhead with their spacing
   intact. The group move clamps against the *block's* outer neighbours
   (`Track::move_keys_limits`), intersected across every affected track so a
   multi-property selection translates instead of deforming.
   - The marquee's "a box is live" flag round-trips through egui memory, so it
     lags the press by one frame — invisible, since the box has no area worth
     hit-testing until the pointer moves. Only a row response can tell us the
     drag began on empty track rather than on a diamond.
   - Paste replaces any key already sitting on a landing frame (the one-key-per-
     frame invariant `sample` needs) and drops keys that would land before frame
     0 rather than piling them up there.
3. ~~**More shape params + stroke editing.**~~ ✅ Done. Rect/Ellipse Size, Rect
   Radius, and Stroke colour + width are editable and fully animatable, plus
   add/remove stroke.
   - This pass also closed a gap: **fill was editable and had a stopwatch but no
     `PropKind` variant**, so fill keyframes existed with no dopesheet row —
     uncreatable-to-manage, invisible to select/retime/delete. Rather than add a
     fifth special case, `PropKind` became the single enumeration of animatable
     properties behind `prop_of`/`prop_of_mut`.
4. ~~**Blender-style splittable/dockable panels**~~ — ✅ Done. The layout
   tree + draggable splitters are done (see *The panel layout tree* above), and
   the canvas fit now derives from the tree instead of hardcoded panel sizes.
   Delivered in order:
   - ~~**Split / join areas** and a per-area dropdown to change which `Editor`
     an area shows.~~ ✅ Done. Each content area carries a header with an editor
     picker and split (`|` left/right, `-` top/bottom) + close (`x`) buttons;
     the ops are pure `Dock` tree rewrites applied after the UI pass (see *Split
     / join / retype* above). Deliberately **not** the literal drag-a-corner
     gesture: layered on egui's own panel splitters that's far more fragile than
     header controls, for the same capability. The canvas and the two toolbars
     stay header-less on purpose, which is what protects the single-canvas and
     innermost-canvas invariants.
   - ~~**Layout presets**: several named defaults plus user-made ones.~~ ✅ Done.
     A **Layout** menu in the comp bar switches between built-ins (`Default`,
     `Animation` — a tall dopesheet; `Design` — no dopesheet, wide canvas) and
     lets you name + save the current arrangement as a preset. Each built-in is
     just a `Dock` constructor listed in `builtin_presets()`; a user preset is a
     cloned tree (`Dock` is now `Clone`). A test pins that *every* preset keeps
     the structural guarantees (one innermost canvas; comp + transport present,
     since those headerless toolbars can't be re-added if a preset drops them).
   - ~~**Save the layout into the project** so a `.pbc` reopens the way it was
     left.~~ ✅ Done. The `.pbc` is now a `Project { document, layout }` wrapper
     (see *Saving the layout* below); the active dock and user presets ride
     alongside the document. Built-ins stay code, not data. A loaded layout is
     validated (`Dock::is_valid`) and discarded for the default if it's broken,
     so a hand-edited file can't wedge the editor. Old bare-`Document` `.pbc`
     files still open — the loader falls back to a plain document parse.

   With that, **item #4 is complete.** Next is the node/expression IR (#5).
5. ~~**Node graph + expression IR**~~ (`Value::Expr` / `Value::Parametric`) — the
   big differentiator; the IR/printer discipline borrowed from the EBN project.
   ✅ Done — built in stages:
   - ~~**The `EvalCtx` seam.**~~ ✅ Done. `resolve` takes an `EvalCtx` instead of
     a bare frame (see *The core idea* above).
   - ~~**`Value::Expr` + the IR.**~~ ✅ Done — the headless engine now evaluates
     expressions. See *Expressions* below. In short: a `crate::expr` module with
     the dynamic `ExprValue { Num, Vec2, Color }` and its `From`/`ToExpr` edge, a
     tiny IR (`Lit`, `Ref { node, prop, time_offset }`, `Add`/`Mul`/`Neg`), and a
     `ResolveCache` on `EvalCtx` doing per-frame memoization + cycle detection (a
     cycle → a `scene.warnings` entry + a neutral fallback, never a hang).
   - ~~**Node-graph panel.**~~ ✅ Done. A new `Editor::Graph` (summonable into any
     content area via the split/join picker — no default-layout change) lets you
     drive the selected node's properties with expressions: **`= fx`** promotes a
     property (seeded from its current value), **bake** freezes it back to a
     constant, and the expression is edited on a **node canvas** — boxes wired
     parent↔child, each with a kind picker (`value`/`ref`/`add`/`mul`/`neg`/
     `script`) and a compact editor; changing one node's kind grows the tree
     (operators seed neutral inputs). Layout is a tidy-tree auto-placement
     (`layout_expr`, a tested pure function) where each box's **height varies by
     kind** (`box_height`) so a `ref`'s three pickers or a `script`'s field +
     result line get the room they need and the stack stays clear; edits are
     deferred `GraphOp`s addressed by `(property, tree-path)` and applied after
     the UI pass by `apply_graph_op` (a free function, so the whole flow is
     unit-tested) — the same discipline as the dock. Boxes start on the tidy-tree
     layout and can be **dragged** to rearrange; positions are remembered per
     (node, property) in egui memory (ephemeral view state, not saved with the
     document).
   - ~~**Rhai scripting** (first cut).~~ ✅ Done. A `script` node kind holds Rhai
     source (`Expr::Script`), evaluated each frame with `frame`/`time` in scope;
     the result is a number (→ `Num`) or a 2/3/4-element array (→ `Vec2`/`Color`).
     `eval_script` runs on a thread-local engine; a bad script resolves to a
     neutral fallback (never breaks the frame) and the editor shows the error
     live.
   - ~~**The scripting bridge** — `value()` / `wiggle()`.~~ ✅ Done. Rhai's
     registered functions must be `'static`, so a script had no way to reach the
     `&mut EvalCtx` of the evaluation that called it. `mod bridge` in
     `core/src/expr.rs` parks that borrow in a **thread-local raw pointer** for
     exactly the span of one `eval_with_scope` — **the crate's only `unsafe`**,
     kept tiny so its three soundness rules can be checked by reading it: the
     guard clears the pointer on drop (lifetime), `with_ctx` *takes* it out for
     the callback so a second `&mut` can't coexist (aliasing), and it's
     thread-local so it can't escape (threads). A nested script re-parks through
     `enter` from the inner borrow, which is the correct nesting order.
     On top of it: **`value("A", "opacity")`** and **`value_at("A", "opacity",
     frame - 10)`** — by node *name*, first match in tree order — routed through
     the same memoized, cycle-guarded `resolve_prop` as `Expr::Ref`, so a
     self-reference warns and falls back instead of recursing until the stack
     goes; and **`wiggle(freq, amp[, seed])`**, smoothstep value noise that is
     deterministic per frame (scrubbing is stable, a render matches the preview)
     with a seed so x and y are independent streams. Confirmed working in a live
     session.

   - ~~**Exposed parameters** (the first half of `Value::Parametric`).~~ ✅ Done.
     A node carries named, animatable knobs (`Param` / `ParamValue`) that
     expressions read via an `Expr::Param` node and scripts via `param("x")` /
     `param_of("node", "x")` — one control driving many properties, and (once
     comps can nest) what a pre-comp will expose to its parent. Resolved
     through the same memoized, cycle-guarded path as a property reference, so
     a self-driving parameter warns instead of hanging. **A node-relative
     `param()` needs to know whose property is resolving**: `EvalCtx::in_node`
     marks that, and anything resolving a node's properties outside `evaluate`
     (the properties readout, the script preview) must go through it or it will
     show a fallback where the canvas shows the real value.
   - ~~**Procedural generators** (the other half).~~ ✅ Done. Typed-knob motion
     primitives instead of free-text Rhai for the common cases: **`osc`**
     (`offset + amp·wave(freq·frame + phase)`, with a sine/triangle/square/saw
     waveform), **`noise`** (the same value noise behind `wiggle()`, as a knobbed
     node), **`ramp`** (a linear `from→to` across a frame window, clamped flat
     outside), and **`bounce`** (`amp·e^(−decay·frame)·cos(2π·freq·frame)`, the
     classic overshoot-and-settle). Each is a new `Expr::Gen(Generator)` arm
     resolving to a `Num` (feed it through `mul` to broadcast onto a vec/colour),
     and — the reason this waited for parameters — **every knob is itself an
     `Expr`**: it defaults to a literal you drag in the canvas but can be rewired
     to a `param`/`ref`/expression like any other node. Frame-native and
     deterministic (the same contract as `wiggle`: scrubbing is stable, a render
     matches the preview). In the graph canvas a generator's knobs are wired-in
     child boxes labelled by name (`freq`/`amp`/…); picking a generator from any
     box's kind menu seeds it, and edits route through the same `GraphOp` /
     `apply_graph_op` path as every other node, so the whole flow is unit-tested.

## Text

`Shape::Text { content, family, size, align, max_width }` — `core/src/text.rs`.
Two decisions, both load-bearing:

**Text resolves to glyph *outlines*, not glyph runs.** `Shape::to_path(ctx)` is
the seam every renderer consumes, so shaping into a `BezPath` means a text layer
fills, strokes, transforms, keyframes, and animates through the *existing*
pipeline — the SVG backend and the offline `motion` binary render text without
knowing it exists, and **no renderer changed at all** to add this. Handing vello
glyph runs (the obvious route, and what an earlier note here assumed) would have
been live-only. parley does the real work — bidi, script segmentation, font
fallback, line breaking, alignment — and skrifa pulls outlines for the shaped
glyph ids; the only coordinate work here is the y-flip from font space (y-up,
from the baseline) into layout space (y-down). Text is centred on the origin, the
convention `Rect`/`Ellipse` already follow, so anchor/rotation/scale behave.

**Families resolve against the system font set** — so `family` stores a *name*,
never font bytes. **This is the one place the engine is not deterministic:**
everywhere else `evaluate(doc, t)` is pure, so a render matches the preview and
tests pin output; a `.pbc` naming "Futura" instead draws Futura where it's
installed and a fallback where it isn't. An unknown or blank family falls back
through the generic sans-serif stack rather than failing, so a project from
another machine still draws. The tests in `text.rs` therefore assert *structure*
(non-empty path, bigger size ⇒ bigger box, wrapping ⇒ taller and narrower,
unknown family still draws) and never exact coordinates.

Only `size` is a `Value`, so it's the only keyframable/expression-drivable text
channel (`PropKind::TextSize`, `PropPath::TextSize` → `text_size` in scripts).
`content` is a plain `String` because [`Value`] carries only interpolatable,
expression-typed values (`f64`/`Vec2`/`Color`) — there's no string in
`ExprValue`, so **a keyframed or scripted string (a typewriter effect) needs a
wider value model** and is the obvious next step for this primitive.

Shaping contexts (parley's `FontContext`/`LayoutContext`) are `thread_local` and
reused, like `expr.rs`'s script engine — enumerating system fonts is far too
expensive to redo per frame.

### Picking a font, and when one is missing

A missing font is **invisible by construction**: parley substitutes and the text
draws perfectly well in the wrong face, so nothing about the frame reveals it.
Hence `text::font_exists` and two places that report it:

- **`scene.warnings`** — `Shape::to_path` warns through `EvalCtx::warn_here`
  (widened to `pub(crate)` so a *shape* can warn, not just an expression), which
  puts it behind the comp bar's existing amber indicator for free. Same channel
  as a broken script, same "fall back to something drawable but say so" rule as a
  dangling `Ref`.
- **The Font row** — an amber warning glyph beside the picker, naming the font
  and explaining that the project still stores the name, so it will look right on
  a machine that has it.

A **blank family is never "missing"**: it means "use the default" deliberately, so
it reports as existing and never warns — otherwise every new text layer would
ship with a warning on it.

The picker itself follows the modern-editor shape: a searchable list of every
installed family with recently-applied fonts pinned on top, and **hover to
preview, click to apply**. Hovering only reports
`PropEdits::text_family_preview`; `App::preview_project` then renders *that*
frame from a throwaway clone with the family swapped in, so browsing hundreds of
fonts never touches the document and needs no undo. The clone only happens while
a row is hovered, so the common path pays nothing. Recents are session-only
(`remember_font`, a free function so its most-recently-used ordering is
unit-tested) — they're app state, not project state, so they stay out of the
`.pbc`.

**Agreed order past #5** (decided 2026-07-19): multi-composition / pre-comps ✅
→ document-wide property graph ✅ → Blender-standard graph UI (in progress) →
the Nuke-style *image* graph. Pre-comps come before the big graph because a comp
*is* a graph node, so building the graph first means rebuilding it; the image
graph is last because it needs the raster compositor stage below, which isn't
built. Note the distinction: today's graph is a **property** graph (values into
properties); Nuke's is an **image** graph (operations on pixels). They're
different machines.

**Canvas gizmos + grids (added 2026-07-20).** Not part of the agreed sequence
above — it came in sideways as a preview-panel need. The **transform gizmo is
done** (see *The transform gizmo*). Still open, in this order:

1. ~~**Grid + rulers + guides**~~ ✅ Done — see *Grid, rulers and guides*.
   Per-comp state saved in the `.pbc`, alongside `Comp::bg`.
2. ~~**Snapping**~~ ✅ Done for the pivot — see *Snapping*. Still open: snapping
   a layer's **bounding-box edges** and to *other layers'* bounds, which needs
   per-frame bounds for the dragged layer and a candidate set from its siblings.
3. ~~**Anchor-point handle + selection bbox**~~ ✅ Done — see *The anchor handle
   and the selection box*.
4. **Onion skinning** — ghosts of the rendered layer at ±N frames, tinted by
   direction (past warm, future cool). This is the answer for every property a
   motion path *can't* show; see *The motion path*. Will want the same caching
   discipline, and is strictly more expensive (each ghost is a whole scene).

The **motion path** from this track is done — see *The motion path* below.

> **Graph-UI progress (2026-07-20):** module bodies now have a real editing
> surface — you open a module from the graph panel and edit its body + knobs on
> the same node canvas a property uses (see *Editing a module body* above).
> **Seeding a fresh `Use` link from the kind picker is now done:** since a bare
> `ExprKind` can't name a module, the box's kind combo lists the project's
> modules below the primitives (`ExprKind::ALL`), and choosing one emits a
> `SetModule` op — which repoints an existing link *or* replaces any other kind
> with a fresh `use <module>` (no overrides). No core change: `Use` stays out of
> `ExprKind::ALL`, and `Expr::seed(Use)`'s placeholder module is never reached by
> the picker.
>
> **Override sub-expressions are now editable on the canvas** (the last open
> graph-UI item). A link's overrides became **first-class children** of the
> `Expr::Use` node — `arity`/`child`/`at`/`at_mut` treat `overrides[i]` as slot
> `i` — so the existing canvas lays each override out as a wired box with its own
> kind picker, and edits route through the ordinary `GraphOp`s (path = the link's
> path + the child slot). An override can therefore be a literal, a `ref`, a
> `param`, a script — anything, not just a literal. The `use_editor` row shrank to
> the two-state toggle: **override** seeds a literal `0` child to build from,
> **inherit** (the `x`) drops it so the knob follows the module again; the value
> itself is edited in the child box, labelled with the knob name (derived in the
> canvas, since override names are dynamic and core's `slot_label` is `&'static`).
> `eval_use` is unchanged — it still reads overrides by name — so this is a walk
> change, not a semantics change. With that, the Blender-standard graph UI step is
> complete.

> **Timeline UX + fps retiming (2026-07-20, user-verified):** changing a comp's
> fps now **re-grids** the animation instead of leaving keys on stale frame
> numbers — a key at frame 120 @ 60fps (two seconds in) lands on frame 48 @ 24fps.
> `Comp::set_fps` is the only supported way to write the rate on a comp with
> content (a plain `fps =` shifts every key in seconds); it walks the tree
> rescaling every frame position by `new_fps/old_fps`, `LayerTiming` included.
> The conversion rounds to whole frames, so it's lossy — keys under a frame apart
> merge, first one wins. The fps **drag** applies as a *single* retime off a
> pre-drag snapshot (`App::fps_drag`), not one rounding per delta, or a slow drag
> would shred dense keys passing through every intermediate rate. The keyframe
> **selection rides along**: a `KeyRef` is a track index, and a merge shifts
> indices, so `remap_selection` re-resolves the selection through frames across
> the retime (snapshotted in the drag too, so a long drag can't walk it off its
> keys). Per-delta cost is a tree clone + retime — measured ~1.3 ms at 100k keys
> in release, linear, only a concern near 500k.
>
> Dopesheet + transport got a Blender-style pass: the label/track split is now
> **two resizable columns** sharing one full-height splitter, with every row's
> label cell allocated at the identical width via `allocate_exact_size` (the old
> `allocate_ui_with_layout` grew a row whose button/name didn't fit, desyncing
> its track from the ruler axis). The timeline header gained **zoom in / out /
> fit** buttons (anchored at the playhead; the wheel and the buttons share
> `zoomed()`). The transport is `|◀ ◀◀ ▶ ▶▶ ▶|` — jump to range start, prev/next
> keyframe (disabled when there's none), play/pause, jump to range end — plus
> numeric **Start/End** fields over the existing work area. The old blue playhead
> slider is **gone**: it mapped `0..=last_frame` while the ruler maps the visible
> window, so once zoomed it landed the playhead somewhere other than where you
> dropped it.
>
> Icons are a real font now (`live/src/icon.rs`, Tabler subsetted to the named
> codepoints — regenerate the subset when adding a glyph, see `assets/NOTICE.md`),
> so the old "egui renders ◆◇ as tofu, paint your own" gotcha is retired for
> anything drawn through `icon::`.

> Two riders on this order, both feeding the *reusable animation modules* feature
> spec'd in the design section below: the **pre-comps** step also introduces the
> **per-layer in/out time model** (a pre-comp is a layer with a time range) and
> the **layer-local time sources** expressions need to retime one animation to
> each clip; the **document-wide property graph** step is where a shared,
> overridable animation *module* becomes first-class. **Text layers** are a
> separate near-term primitive, independent of the graph work.

> The bigger, further-out features (renderer/compositor model, 2.5D, footage
> import, export, plugins, expressions) have their architecture decided in the
> **Design decisions** section below — read it before starting any of them.

## Design decisions for the big features (agreed — not yet built)

Architecture calls made while planning past the current editor, recorded so the
reasoning survives. **Nothing here is implemented yet**; the Roadmap above is
still the build order. Read this before starting any of it.

### Renderer: vector-first substrate, raster compositor on top

"Vector vs raster renderer" is a false choice — vello *is* a rasterizer (vector
paths → pixels on the GPU). The real axis is the *scene model*, and the decision
is **vector-first substrate with a raster compositing stage layered on top.**

- Authoring primitives (shapes, text, masks, paths) are vector — resolution
  independence + editable geometry are non-negotiable. This is the substrate.
- Compositing + effects (footage, keying, blur, glow, colour) are per-pixel
  raster ops. These live in a stage *above* vector rasterization.

Flow: vello rasterizes each layer's vector content to a texture → a compositor
stage (our own wgpu passes) runs effects, blend modes, masks, and (later) 3D
placement → composite to frame. **vello is the vector→pixels stage, not the
compositor. We build the compositor.**

- vello was the right pick for the vector stage (best GPU vector rasterizer in
  Rust, wgpu-based so it shares a device with our compute passes). Risks: young,
  API churn (pinned 0.9), thin text/image-filter features — but we build the
  effect layer ourselves regardless of renderer.
- **Escape hatch:** if vello's maturity bites, `rust-skia` drops into the *same
  stage* (heavier, C++ FFI, far more complete). `tiny-skia` = pure-Rust CPU
  fallback, offline only.
- The swap stays contained because `render/` already abstracts `Scene → pixels`
  with two backends (offline SVG + live vello). Keep that boundary honest.

### The compositor stage (the one subsystem several features share)

Effects, keying, masking, blend modes, **and** 2.5D layer placement are all
facets of one new subsystem: a compositor that combines rasterized layer
textures. Model it as: each layer can render to its own offscreen target; an
ordered effect stack (GPU passes) processes that target; then it composites
(blend mode + opacity + mask) into its parent.
- Masks / mattes: vello handles these natively (`push_layer` with clip + blend +
  alpha; track mattes via intermediate layers).
- Keying (chroma/luma): a per-pixel shader op vello won't do — render footage to
  a texture, run a wgpu keyer pass (alpha from colour distance), feed the result
  back in as an image.
- Effect params are `Value<T>` like everything else, so they animate for free.

### 2.5D (AE-style 3D layers) — AGREED to target

Target **Level A: flat layers positioned/rotated in 3D space, viewed through an
animatable camera, with depth ordering.** NOT Level B (real meshes / materials /
lights / shadows — a different product tier; AE only got it by bundling
Cinema4D). Level B is explicitly out of scope.

This is a **`core` decision before a rendering one.** Do the cheap data-model
part early; defer the expensive render part.

- **Widen `Transform` to 3D early** (cheap insurance): `Value<Vec3>` anchor/
  position/scale, 3-axis rotation (Euler XYZ or quaternion), resolve to
  `glam::Mat4` composed down the tree instead of `kurbo::Affine`. 2D becomes the
  z=0 case; existing behaviour preserved. **`glam` is already a `core`
  dependency, currently unused — put there for exactly this.**
- **Blast radius** of the widening: `Transform` (node.rs), `eval::walk` (compose
  Mat4), `RenderItem.transform` (eval.rs), hit-testing + click-select, the
  properties panel (a Z field), and a **serialized-format migration** for
  existing `.pbc` docs. All cheap now, expensive once 3D docs exist in the wild —
  so do the model change before shipping saved 3D docs.
- **Camera**: a real document node (position, target, FOV, near/far), itself
  `Value`-driven and keyframable. Today's "camera" is `fit_transform`; in 3D it
  becomes a first-class animatable object.
- **Render side (defer until building the compositor):** vello still rasterizes
  each flat card's vector content to a texture; a wgpu pass places that texture
  as a projected quad using the camera matrices + a **depth buffer** (or
  painter's sort by z — AE-style, fine because flat cards rarely intersect). No
  new renderer needed for Level A; it's another job of the compositor stage.
- **Downstream:** click-to-select becomes ray-picking through the camera (or
  hit-testing projected quads). Not a blocker, just carried along.

### Render output / export — never implement codecs

- Render deterministic frames → pipe raw RGBA to **ffmpeg** (binary via stdin
  `rawvideo`, or `ffmpeg-next` bindings). `evaluate(doc, t)` is pure and
  non-realtime — a perfect offline render queue (full quality at any fps,
  ignoring playback speed). PNG-sequence export is nearly free (`image` crate).
- Pure-Rust encoders (`rav1e`, `gif`) are supplementary, not the H.264 path.
- Put encoders behind an `Encoder` trait: ffmpeg one impl, PNG-sequence another.

### Footage import (images / video / audio)

Forces the first change to the currently-procedural model: an **asset registry**
— nodes reference an asset by id/path; the registry owns the decoded source +
frame cache. Store *references* in `.pbc`, never pixels.
- Images: `image` (raster) + `resvg` (SVG) → texture.
- Video: `ffmpeg-next` decode; the hard part is **frame-accurate seeking** for
  non-linear scrubbing (seek-to-keyframe + decode-forward + frame cache).
- Audio: `symphonia` decode + `cpal`/`rodio` output. Needs a real **master
  clock** (today's playback is a wall-clock loop) and a waveform for sync.

### Plugin system — design plugin-shaped now, stabilise the ABI later

"Expose from the beginning" ≠ "publish a frozen ABI on an unstable core."
- **Now (cheap, good regardless):** make effects, generator nodes, importers,
  exporters trait objects behind registries; dogfood our built-ins through the
  same seams. A third-party plugin is then "another registered impl."
- **Later:** the third-party boundary is **WASM via `wasmtime`** (sandboxed,
  hot-reloadable, language-agnostic) for logic/generator/expression plugins;
  C-ABI (`abi_stable`) only if a per-pixel effect needs native speed.
- Ship the stable SDK when the node/expression IR settles (Roadmap #5), not
  before. Promise "plugin-ready architecture" now, "stable plugin SDK" later.

### Code-driven animation (expressions) — extends Roadmap #5

`Value::Expr` is another `Value<T>` recipe; `evaluate` runs it instead of
sampling keyframes. Expressions and the node graph are two front-ends that lower
to the **same IR** (the EBN IR + dumb-printer discipline).
> **Status:** the core of this is now built — see *Expressions* above and
> `core/src/expr.rs`. The bullets below record the reasoning; ✅ marks what's
> implemented, and what's still ahead (Rhai, `wiggle`, stroke/shape refs).

- **Signature ripple:** ✅ `resolve(&self, t)` → `resolve(&self, ctx: &mut
  EvalCtx)` carrying `{ frame, doc, cache, warnings }`. A single-`t` "bake first"
  pre-pass can't work because `valueAtTime(t')` samples at *other* times, which
  is the whole reason the context — not a bare frame — is threaded.
- **Dynamic↔typed boundary:** ✅ `ExprValue { Num, Vec2, Color }` + `FromExpr` /
  `ToExpr`, implemented only for scriptable `T` (not `BezPath` — enforced by the
  trait bound). Mismatch → `fallback()`.
- **Dependency graph is implicit** in pull-based DFS (a dependency resolves
  before its dependent because you recurse into it first — no separate topo
  sort). ✅ `ResolveCache`: a `visiting` set for cycle detection (a cycle →
  `fallback` + a `scene.warnings` entry, reusing the provenance channel) and a
  `(node, prop, frame)` memo (the frame in the key matters — off-time samples
  must not poison the primary value).
- **Determinism:** ✅ expressions are pure functions of (frame, inputs) — no IO,
  no wall clock. `wiggle()` (seeded from node, prop, frame) is still to come with
  the script engine. This is the *same sandbox* WASM plugins need — build it once.
- Engine: start with **Rhai** (pure-Rust, easy, safe) — *not yet wired*; the IR
  and evaluator are in place for it to lower into. Swap behind the IR later if
  AE-JS compatibility (`boa`/v8) or Lua (`mlua`) is wanted.

### Pre-comps + the layer time model — implementation plan (proposed)

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

### Reusable animation modules — shared, auto-retimed, overridable

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
   it's the already-decided **asset registry + compositor** work (see *Footage
   import* above). It does **not** block the subtitle-animation story; the two are
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

### The two unifying insights (why this isn't N separate projects)

1. **One deterministic, sandboxed eval-with-dependency-graph** serves
   expressions, WASM plugins, effect params, and media sampling. Build it once.
2. **One compositor stage** serves effects, keying, masking, blend modes, and
   2.5D card placement. Build it once.

Everything else (ffmpeg export, asset registry) hangs off `evaluate` staying the
single pure entry point. Protect that.

## Reference: the EBN project

`../_extendBlueNode` (Extend Blue Node) is a prior project — a node-graph →
ExtendScript compiler for After Effects. Ideas worth borrowing here: the IR +
dumb-printer split (for the future expression/parametric IR), line→nodeId
provenance (already applied as `RenderItem.source`), and the recursive
splittable `layoutTree` (for roadmap #4). Skip its ExtendScript/CEP machinery.
