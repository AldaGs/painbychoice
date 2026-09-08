# 0006. 2.5D: target Level A (flat layers in 3D), not Level B

- **Status:** Accepted — data model implemented
- **Recorded:** 2026-09-08, extracted verbatim from the README design journal.

## Context

"3D" spans everything from AE-style postcards in space to meshes, materials,
lights and shadows. The second is a different product tier — AE only got it by
bundling Cinema4D.

## Decision

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
