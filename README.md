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

## Reference: the EBN project

`../_extendBlueNode` (Extend Blue Node) is a prior project — a node-graph →
ExtendScript compiler for After Effects. Ideas worth borrowing here: the IR +
dumb-printer split (for the future expression/parametric IR), line→nodeId
provenance (already applied as `RenderItem.source`), and the recursive
splittable `layoutTree` (for roadmap #4). Skip its ExtendScript/CEP machinery.
