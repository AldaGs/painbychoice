# 0016. Track mattes in SVG are luminance masks, and divergences are reported

- **Status:** Accepted — implemented
- **Decided:** 2026-09-08

## Context

The engine models a track matte as a **coverage rule**: an inner group composed
`DestIn` (keep the backdrop where the matte is opaque) or `DestOut` (keep it
where the matte is transparent) over the pair that encloses it. The GPU backend
implements that directly.

SVG has no coverage rule, so the offline backend **skipped matte layers
entirely** — it drew the content unmatted and the matte not at all. That is a
silent disagreement between two backends, and Phase 1 of
[`../production-plan.md`](../production-plan.md) rests on the claim that an
exported frame equals the previewed one. A backend that quietly draws something
plausible and wrong is the least traceable failure a renderer can have.

Two ways out: implement it, or formally demote the SVG backend to a debug path
that is not a render target.

## Decision

**Implement it, as a luminance mask, and report what is still inexact.**

`<mask>` in SVG is luminance-based by default:

- `DestIn` → an implicit black ground with the matte's shapes painted **white**
  at their own coverage. White at alpha *a* over black has luminance *a*, so the
  mask's luminance **is** the matte's alpha. Exact.
- `DestOut` → the same construction inverted: an explicit **white** ground the
  matte subtracts from in black, giving luminance *1−a*. Also exact.

Deliberately **not** `mask-type="alpha"`. It would express `DestIn` directly,
but CSS masking gives no inverse for it, so `DestOut` would remain an
approximation. Luminance gets both exactly, with one construction and no
dependence on an SVG2 feature a consumer might not support.

Where the mapping is genuinely not exact — **footage used as a matte**, whose
luminance is not its alpha, so a dark opaque pixel masks as if transparent — the
mask is still emitted and the divergence is **named**. `scene_to_svg_reporting`
returns those notes and the offline binary prints them; `scene_to_svg` remains
for callers that do not want them.

## Consequences

- The offline render agrees with the GPU one for vector mattes, which is what
  the parity tests in Phase 1 will assert against.
- The backend now has a vocabulary for "I could not draw this exactly", which is
  reusable: every future construct SVG cannot express should add a note rather
  than a silent omission.
- The remaining raster-matte gap is a known, reported limitation rather than an
  invisible one. Closing it would need alpha extracted from the source, which
  means decoding in a backend that deliberately has no decoders — so it stays
  reported until the compositor stage ([0005](0005-compositor-stage.md)) makes
  the question moot.
- The SVG backend is **not** demoted: it remains the headless way to verify
  compositing semantics without a GPU, which is more valuable now than it was
  before export existed, not less.
