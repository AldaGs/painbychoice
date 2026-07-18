# Pain By Choice (PBC)

A hybrid vector motion / animation tool — non-destructive, non-linear, parametric.
A blend of After Effects, Figma, Animate, and Cavalry. Rust engine. Early scaffold.

## Architecture

Three crates, deliberately layered. `core` is headless and knows nothing about
GPUs or windows — the engine must be testable by rendering a frame in a unit
test, not a window.

```
crates/
  core/    document model + evaluation engine. No GPU, no windowing.
  render/  evaluated Scene -> pixels. SVG backend today; vello/wgpu later.
  app/     runnable shell. For now: evaluate a demo doc and write SVG frames.
```

### The core idea

Every animatable property is a `Value<T>` — a *recipe*, never a baked result:
a constant or a keyframe track today; an expression / parametric-node IR later.
`evaluate(&doc, t)` is a pure function that resolves the whole scene graph at
time `t` into a flat `Scene` of draw items. Non-destructive editing and
non-linear scrubbing both fall out of that single design choice.

Every evaluated item carries a `source: NodeId` (provenance) so a frame traces
back to the node that produced it — for selection and debugging.

## Run it

```bash
cargo test --workspace     # engine tests: easing, tracks, keyframe ops, hit-testing
cargo run -p motion-live   # the live GPU editor (window)
cargo run --bin motion     # offline: writes out/frame_00.svg .. frame_08.svg
```

## Live editor (`pbc`)

`crates/live` is a winit + vello + egui shell over the engine:

- **Canvas** — vello rasterizes `evaluate(doc, t)` every frame; click a shape to select it.
- **Transport** — play/pause, restart, scrubbable playhead.
- **Layers** (left) — the scene tree; select, reorder (▲/▼), add (Rect/Ellipse/Group), delete, Save…/Load….
- **Properties** (right) — resolved values for the selection; drag or type to edit. Editing an animated value keys it at the playhead.
- **Timeline / dopesheet** (bottom) — keyframes as diamonds per property; click to seek, click a ◆ to select (Del removes), drag to retime.

Save/Load round-trips the whole document to `.pbc` (JSON via serde).

## Roadmap (next)

1. Keyframe easing handles editable in the timeline (currently linear/smooth presets).
2. More shape params + stroke editing in the properties panel.
3. Blender-style splittable/dockable panels (see EBN's `layoutTree`).
4. Node graph + expression IR (`Value::Expr` / `Value::Parametric`).
