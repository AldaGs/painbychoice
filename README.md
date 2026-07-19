# Pain By Choice (PBC)

A hybrid vector motion / animation tool — non-destructive, non-linear, parametric.
A blend of After Effects, Figma, Animate, and Cavalry. Rust engine.

> **Status:** working single-window editor. You can build a composition from
> scratch, animate it with frame-accurate keyframes + editable easing,
> scrub/play, and save/load.
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

Every evaluated item carries a `source: NodeId` (provenance) so a frame traces
back to the node that produced it — used for click-to-select and debugging.

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

- **Composition bar** (top) — editable Size (W×H), FPS, Duration. Drives canvas
  fit, playback, frame step, timeline. Comp bounds drawn with a fill + border.
- **Canvas** — vello rasterizes `evaluate(doc, t)` each frame; click a shape to
  select (front-most, via `NodeId` provenance). Selection gets a yellow outline.
- **Transport** — Play/Pause (Space), Restart (R), ←/→ frame step, scrubbable
  playhead (an integer slider, so it can only land on frames). Readout is
  `hh:mm:ss.ff` plus `[frame/last]`. Playback runs off the wall clock but
  *quantizes* to the frame grid, so changing FPS visibly changes the playback
  cadence.
- **Layers** (left) — scene tree; select, reorder (▲/▼), add Rect/Ellipse/Group,
  delete (✕), Save…/Load… (`.pbc` JSON via serde).
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
- `core/src/demo.rs` — the demo document loaded on launch.
- `live/src/main.rs` — everything UI. `App::render` is the per-frame heart:
  evaluate → hit-test → gather snapshots → run egui → apply `*Edits` → GPU. Panel
  fns: `comp_ui`, `tree_ui`, `transport_ui`, `dopesheet_ui`, `properties_ui`,
  `ease_editor`, `key_button`. Each panel fn renders into a `&mut Ui` it is
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
- **`fit_transform` takes that leaf's measured rect**, not the window minus
  hardcoded panel sizes. The constants version could not survive a draggable
  splitter: `pick` inverts this transform, so stale geometry doesn't just
  misdraw the canvas, it sends every click to the wrong shape.

`size` is stored in the tree (and written back from the real panel rect each
frame) rather than living only in egui's panel memory — that's what keeps the
tree the source of truth, so saving layouts is a `serde` derive rather than a
scrape of egui internals.

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

## Known issues / gotchas

- **egui default font lacks many glyphs** (◆ ◇ ● ○ ❚ ⟲ ▸) — they render as tofu
  boxes. `▶` and `•` are safe; otherwise *paint* the indicator (see `key_button`)
  or use plain words. Learned the hard way.
- **The box-select flag round-trips through egui memory.** Only a row's
  `Response` can tell us a drag began on empty track (a diamond grabs the press
  first), but the marquee rect is needed *before* the rows loop — so "a box is
  live" is stashed with `data_mut` and read on the next frame. The one-frame lag
  is invisible (the box has no area worth hit-testing until the pointer moves);
  don't try to "fix" it by hoisting the hit-test out of the loop.
- **egui eats the shift modifier on shift+wheel**, rewriting it into a
  *horizontal* scroll. So the pan signal is a nonzero `smooth_scroll_delta.x`,
  not `modifiers.shift` — checking `shift` silently does nothing.
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
UX ✅ → shape/stroke params ✅ → dockable panels 🚧 → …**. Next up:

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
4. **Blender-style splittable/dockable panels** — 🚧 in progress. The layout
   tree + draggable splitters are done (see *The panel layout tree* above), and
   the canvas fit now derives from the tree instead of hardcoded panel sizes.
   Still to come, in order:
   - **Split / join areas** (Blender's drag-a-border-corner) and a per-area
     dropdown to change which `Editor` an area shows.
   - **Layout presets**: several named defaults plus user-made ones. The tree is
     already serialization-ready (`size` lives in the tree, and
     `default_layout()` is just one constructor among future many).
   - **Save the layout into the project** once it's been changed, so a `.pbc`
     reopens the way it was left.
5. **Node graph + expression IR** (`Value::Expr` / `Value::Parametric`) — the big
   differentiator; the IR/printer discipline borrowed from the EBN project.

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
- **Signature ripple:** `resolve(&self, t)` → `resolve(&self, ctx: &mut
  EvalCtx)` carrying `{ t, doc, engine, cache }`. A single-`t` "bake first"
  pre-pass can't work because `valueAtTime(t')` samples at *other* times.
- **Dynamic↔typed boundary:** `ExprValue { Num, Vec2, Color }` + `FromExpr` /
  `ToExpr`, implemented only for scriptable `T` (not `BezPath` — enforced by the
  trait bound).
- **Dependency graph is implicit** in pull-based DFS (a dependency resolves
  before its dependent because you recurse into it first — no separate topo
  sort). Add to `ResolveCache`: a `visiting` set for cycle detection (a cycle →
  `fallback` + a `scene.warnings` entry, reusing the provenance channel) and a
  `(node, prop, t)` memo (the `t` in the key matters — off-time samples must not
  poison the primary value).
- **Determinism:** expressions are pure functions of (t, inputs) — no IO, no
  wall clock; `wiggle()` seeds from (node, prop, t). This is the *same sandbox*
  WASM plugins need — build it once.
- Engine: start with **Rhai** (pure-Rust, easy, safe); swap behind the IR later
  if AE-JS compatibility (`boa`/v8) or Lua (`mlua`) is wanted.

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
