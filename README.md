# Pain By Choice (PBC)

A hybrid vector motion / animation tool — non-destructive, non-linear, parametric.
A blend of After Effects, Figma, Animate, and Cavalry. Rust engine.

> **Status:** working single-window editor. You can build a composition from
> scratch, animate it with keyframes + editable easing, scrub/play, and save/load.
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
  playhead. Looped playback in wall-clock time.
- **Layers** (left) — scene tree; select, reorder (▲/▼), add Rect/Ellipse/Group,
  delete (✕), Save…/Load… (`.pbc` JSON via serde).
- **Properties** (right) — resolved values for the selection; drag or click-type
  to edit. A painted **stopwatch** per property (filled = animated, hollow =
  constant) inserts a keyframe at the playhead — first click on a constant
  promotes it to a track (this is how a property *starts* animating).
- **Timeline / dopesheet** (bottom) — one row per animated property, keyframes as
  diamonds on a shared time axis with a red playhead. Click track to seek, click
  a diamond to select (Del removes), drag to retime (clamped between neighbours).
- **Easing editor** — selecting a keyframe reveals a CSS-style cubic-bezier
  editor for its outgoing segment: draggable control points + Linear/Smooth/
  Ease In/Ease Out presets.

## Key code locations

- `core/src/value.rs` — `Value<T>`, `Track<T>`, `Keyframe`, `Handle`, easing
  solver. Keyframe ops: `set_at`, `insert_key` (const→track), `move_key`
  (neighbour-clamped), `remove_key`, `segment_handles` / `set_segment_handles`.
- `core/src/node.rs` — `Node`, `Transform`, `Shape` (parametric Rect/Ellipse/
  Path), `Document`. Tree ops: `find`, `find_mut`, `reorder_child`, `remove`.
- `core/src/eval.rs` — `evaluate(doc, t) -> Scene`, `RenderItem` (+provenance).
- `core/src/demo.rs` — the demo document loaded on launch.
- `live/src/main.rs` — everything UI. `App::render` is the per-frame heart:
  evaluate → hit-test → gather snapshots → run egui → apply `*Edits` → GPU. Panel
  fns: `comp_ui`, `tree_ui`, `transport_ui`, `dopesheet_ui`, `properties_ui`,
  `ease_editor`, `key_button`. Layout: `fit_transform` + the `*_H`/`*_W` consts.

## Known issues / gotchas

- **egui default font lacks many glyphs** (◆ ◇ ● ○ ❚ ⟲ ▸) — they render as tofu
  boxes. `▶` and `•` are safe; otherwise *paint* the indicator (see `key_button`)
  or use plain words. Learned the hard way.
- **Time is continuous float seconds** — no frame grid yet. fps exists but only
  drives ←/→ step. This is the next big thing (see roadmap #1).
- Panel sizes are in egui *points*; the canvas fit is in *physical pixels* —
  multiply reserved sizes by `window.scale_factor()` (already done in `render`).
- LF/CRLF warnings on commit are harmless (no `.gitattributes` yet).

## Roadmap (agreed order)

Decided sequence: **composition settings ✅ → frame-based timeline → keyframe UX
→ …**. Composition settings are done. Next up:

1. **Frame-based timeline (next).** Make the whole app frame-aware (the "borrow
   from After Effects" work). Add: a time ruler with **frame ticks**, **snap**
   keyframes + playhead to frames (using `doc.fps`), a **frame/timecode readout**,
   and a **zoomable** timeline. Deliberately *not* borrowing AE's heavier
   machinery (separate graph editor, nested comps) — the inline bezier editor
   already covers easing. Touch points: `dopesheet_ui` time↔x mapping, the
   transport readout, `seek`/`current_time`, and per-frame stepping.
2. **Keyframe UX polish.** Multi-select, box-select, copy/paste, drag multiple
   keys, better selection visuals. Benefits from frame-snapping existing first.
3. **More shape params + stroke editing** in the properties panel.
4. **Blender-style splittable/dockable panels** (see EBN's `layoutTree` idea).
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
