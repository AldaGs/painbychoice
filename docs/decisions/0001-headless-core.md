# 0001. A headless `core` the engine can be tested without a window

- **Status:** Accepted — implemented
- **Recorded:** 2026-09-08, extracted verbatim from the README design journal.

## Context

The engine has to be provable without a GPU or a window, or every correctness
question becomes a question about rendering.

## Decision

Four crates, deliberately layered; `core` knows nothing about GPUs or windowing,
and a frame can be evaluated in a unit test.

```
crates/
  core/    document model + evaluation engine. No GPU, no windowing. (unit-tested)
  render/  evaluated Scene -> pixels. SVG backend (offline). vello lives in live/.
  app/     offline binary `motion`
  live/    the real editor `pbc`: winit + vello (wgpu) + egui over the engine
```

This separation is the whole design; keep it.

## Consequences

Every feature has to answer "which side of the line is this on?", which is the
question that keeps `evaluate` pure. It is also why the render backend boundary
(`Scene → pixels`, two implementations) stays honest enough to be an escape
hatch — see [0004](0004-vector-first-raster-compositor.md).
