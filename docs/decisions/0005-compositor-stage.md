# 0005. One compositor stage, shared by effects, keying, masking and 2.5D

- **Status:** Accepted — not yet built
- **Recorded:** 2026-09-08, extracted verbatim from the README design journal.

## Context

Effects, keying, mattes, blend modes and 2.5D card placement each look like a
separate feature and are all facets of the same missing subsystem.

## Decision

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
