# 0004. Vector-first substrate, raster compositor on top

- **Status:** Accepted — not yet built
- **Recorded:** 2026-09-08, extracted verbatim from the README design journal.

## Context

"Vector vs raster renderer" reads like the decision to make, and it is a false
choice: vello *is* a rasterizer. The real axis is the scene model.

## Decision

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
