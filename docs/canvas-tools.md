# Canvas tools

Everything that draws in, or manipulates, the composition viewport: the preview
camera, the transform gizmo, and the authoring aids around them. All of it is
`live/` code — the engine knows none of it exists.

> Part of the PBC documentation set — see [`docs/README.md`](README.md) for the map.

## The preview camera (2026-07-20)

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

## The work area (2026-07-20)

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

## The transform gizmo (2026-07-20)

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

## Grid, rulers and guides (2026-07-20)

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
suppress picking on its own. See the gotcha of that name in [`gotchas.md`](gotchas.md).

## The anchor handle and the selection box (2026-07-21)

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

## Snapping (2026-07-21)

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

**Edges and centres snap, not only the pivot** (2026-07-21). Pivot-only is the
common half-measure and it cannot lay a title flush against a margin: the pivot
usually sits in the middle of the artwork, so "on the guide" puts the *centre*
there rather than the edge. Each axis now considers the pivot, both edges and
the centre as sources, and takes the smallest correction over every
source/target pair.

Sibling layers are targets too, so "line this up with that" works without
dropping a guide first. Their extents come from `Placement::bounds`, filled by
the same walk that produces everything else — which is why bounds live in `core`
rather than being re-derived per candidate in the editor.

Two subtleties:

- **A layer is never offered its own ancestors.** A group's extent is the union
  of its children's, so any ancestor's box *contains* the dragged layer and
  moves with it; snapping to one pins the drag against a target that runs away
  from it. The root is an ancestor of everything, so excluding ancestors
  excludes it for free. `snap_excluded` does subtree + ancestors in one walk.
- **The dragged layer's bounds are translated, not re-measured.** The cached
  extent describes where the *scene* last put the layer, a frame behind during a
  drag. A move is a pure translation, so shifting the box by how far the pivot
  travelled is exact — and far cheaper than re-evaluating the comp every drag
  frame just to re-measure a rectangle.

## Onion skins (2026-07-21)

`live/src/onion.rs`, `Comp::aids.onion`. Ghosts of the frame either side of the
playhead, so you can see where the animation came from and where it is going
without scrubbing. Tool strip: **Onion**; right-click for counts, spacing and
opacity.

**This is the whole-layer answer to every property a motion path can't draw.** A
path works for position because position *is* a spatial curve in the same space
as the canvas. Nothing else has that property — there is no natural geometry for
"rotation over time", and inventing one per property would mean a new
visualisation for every property ever added. A ghost of the *rendered* layer
sidesteps that: rotation, scale, opacity, fill, shape parameters and text all
show at once, because it is the actual picture rather than a plot of one channel.

**Drawn by vello, unlike every other overlay.** The gizmo, motion path and aids
are egui overlays; ghosts are geometry — filled and stroked paths — and vello
already draws exactly that. Same rasteriser means a ghost looks like a faded
version of the frame rather than an approximation of it, and it sits *under* the
live frame in one scene rather than on a layer above it. The frame you are
editing must never be the faint one.

Four decisions worth keeping:

- **Cached exactly like the motion path**, keyed on selection, playhead,
  settings and `App::doc_rev` — each ghost is a full `evaluate_comp`. Six ghosts
  is six evaluations per rebuild, cheaper than the path's 121, but each retains
  its *geometry*, so ghosts cost memory where the path costs only points. The
  cache must be filled **before** `to_vello`, or ghosts lag the playhead by a
  frame and visibly drift out of step on a fast scrub.
- **Ghosts outside the comp are skipped, not clamped.** Clamping would pile
  duplicates of frame 0 on each other and read as the animation stalling there
  rather than as running out of frames.
- **Fade follows the ghost's index, not its frame distance**, so the nearest is
  always the most solid whatever the spacing is. A lone ghost is fully solid
  rather than divided by zero into invisibility.
- **The tint keeps some of the layer's own colour** (past warm, future cool, the
  Maya/Blender convention). Fully tinting would flatten a multi-coloured scene
  into two silhouettes and lose which layer is which.

Ghosts the selection, or the whole comp when nothing is selected — the "review
the animation" case. Same cost either way: the evaluation is whole-comp
regardless and only the filter differs. `step` defaults to 2 because at 60fps
neighbouring frames are nearly identical, so a step of 1 draws six copies of the
same picture.

## The motion path (2026-07-20)

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
