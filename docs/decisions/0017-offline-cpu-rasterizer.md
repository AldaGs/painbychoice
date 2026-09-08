# 0017. The offline renderer rasterizes on the CPU, and parity with the preview is structural

- **Status:** Accepted — implemented
- **Decided:** 2026-09-08

## Context

Phase 1 needs frames as **pixels**, headlessly: in a test, in CI, on a build
machine, in `motion render`. The editor's preview is rasterized by vello on
wgpu, which needs a GPU and, in practice, a display session.

The production plan states the goal as "the exported frame equals the previewed
frame". Taken literally — per-pixel equality — that forces the offline renderer
to *be* vello, which means no rendering without a GPU, and no test that can
assert on a pixel in CI.

There is also a subtlety worth naming: **two rasterizers never agree
bit-for-bit.** They antialias differently. That is not a defect in either, and
no amount of care makes tiny-skia and vello produce identical edges.

## Decision

Ship a **CPU rasterizer** (`render/src/raster.rs`, tiny-skia) as the offline
path, and split the parity claim in two:

- **Structural parity — asserted.** The same layers, in the same order, with the
  same blend modes, mattes, masks and opacities. This is what silently goes
  wrong, and it is now pinned in *pixels* by tests that need no GPU, on top of
  the same properties the SVG backend already pins in markup.
- **Per-pixel parity — deferred**, and belongs to the offscreen vello target,
  which should live where a GPU device already exists.

`docs/decisions/0004` named tiny-skia for exactly this role ("pure-Rust CPU
fallback, offline only"), so this is that decision being cashed in rather than a
new one.

Footage is **not drawn** by this backend and is **reported** instead, through
the same seam [0016](0016-svg-mattes-are-luminance-masks.md) introduced.
Decoding belongs to the shell's frame cache; wiring a decoder in here would put
file IO inside the one place that is supposed to be free of it.

## Consequences

- `cargo test` can assert on rendered pixels. That is new, and it is what makes
  the compositor work in Phase 3 something we can verify rather than eyeball.
- `motion render` works on any machine, with no display and no GPU. That is the
  CI and batch story.
- A video exported from the CLI and one exported from the editor's future render
  queue will differ in antialiasing. **This must be stated, not discovered.**
  When the GUI render queue lands it should use the GPU path, and the CLI should
  say which rasterizer it used.
- The CPU path is single-threaded today and cleanly pixel-bound. Frames are
  independent and `evaluate` is pure, so frame-parallel rendering is available
  whenever it is worth taking — see `docs/performance.md`.
