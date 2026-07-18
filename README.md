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
cargo test --workspace     # 8 tests: easing, tracks, transform composition, json round-trip
cargo run --bin motion     # writes out/frame_00.svg .. frame_08.svg — scrub by opening them
```

## Roadmap (next)

1. GPU backend: `render` gains a vello-on-wgpu path behind the same `Scene`.
2. App shell: `winit` window + `egui` panels, Blender-style splittable layout.
3. Canvas view + playback (calls `evaluate` in a loop).
4. Editors: selection, properties panel, timeline, interactive keyframing.
5. Node graph + expression IR (`Value::Expr` / `Value::Parametric`).
