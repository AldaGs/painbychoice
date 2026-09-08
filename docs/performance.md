# Performance: what to measure, and what a comparison with After Effects is worth

> Part of the PBC documentation set — see [`README.md`](README.md) for the map.

## Can we measure whether we are faster than After Effects?

Yes, but only for a claim much narrower than "our renderer is faster than
AE's", and the narrow claim is the one worth having anyway.

**The honest framing.** "Faster than AE" is not a measurable statement, because
the two programs are not doing the same work. AE's render time on a real project
is dominated by things PBC does not have yet — effects, footage decode, colour
management, 3D — and by things it never will (a plugin ecosystem executing
arbitrary code per frame). A benchmark that compares a PBC project against an AE
project measures **which features each tool has**, not which renderer is faster,
and it will always flatter whichever tool is missing more.

**The measurable claim.** Build the *same* composition in both — one that uses
only what both support — and measure wall-clock render time to the same output.
That is a real number. Keep it narrow and it stays true:

> "For a 1080p vector composition of N animated shapes with no effects, PBC
> renders M frames per second and AE renders K."

That is defensible, reproducible, and enough. Anything broader is marketing.

### If you do it, control these

Otherwise the number measures the wrong thing:

- **Encoding.** Both must write the same format, or you are timing x264 twice
  with different settings. Prefer a PNG sequence, or the same ffmpeg flags.
- **The build.** A debug build is not a data point — see below.
- **Cold versus warm.** AE caches aggressively (RAM preview, disk cache). Time a
  clean render, and say which you timed.
- **Threads.** Say how many cores each tool used. This is the biggest single
  difference right now (below).
- **The composition.** Publish it. A benchmark without its project file is an
  anecdote.

### What is actually worth measuring, in order

1. **Frames per second on a fixed reference composition, tracked over time.**
   Our own number, against our own past. This catches regressions, which is what
   performance work is mostly for, and needs no second program.
2. **Scaling with resolution and layer count.** Where the curve bends tells you
   what to fix.
3. **The split between evaluation and rasterization.** Two very different
   optimisation problems; a single total hides which one you have.
4. **Interactive latency** — time from an edit to a redrawn frame. For an
   animation tool this matters *more* than batch render speed, and it is where
   AE is genuinely weak. It is also the number a user feels every second they
   work, rather than once at the end.

Comparing against AE, if it happens at all, belongs after these.

## What we know today

Measured 2026-09-08 on the CI container (4 cores), release build, the built-in
demo composition at 1920×1080, rendering to a PNG sequence:

| Scale | Output | Frames/sec |
| --- | --- | --- |
| 1.0 | 1920×1080 | **24.7** |
| 0.5 | 960×540 | 108.3 |
| 0.25 | 480×270 | 439.0 |

Re-measured 2026-09-08 on a **Windows 11 developer machine** (the same demo,
same release profile, CPU rasterizer), which is roughly 3× the container:

| Output | Encoder | Frames/sec | 300 frames in |
| --- | --- | --- | --- |
| 1920×1080 | PNG sequence | **76.4** | 3.9s |
| 1920×1080 | ffmpeg, H.264 draft | 70.9 | 4.2s |
| 1920×1080 | ffmpeg, H.264 master | 68.2 | 4.4s |
| 1920×1080 | ffmpeg, ProRes HQ | 57.2 | 5.3s |

The encoder spread is the useful part: **master costs ~4% over draft**, and
ProRes ~25% over H.264, at this resolution. Both are small next to the
rasterizer, which is the other way round from what the two-button model assumes
— it assumes encoding is what you save by choosing Draft. At 1080p on this
content the honest saving is a few percent. Draft's value is that it asks no
questions, not that it is dramatically faster, and the table above is why
[0018](decisions/0018-two-render-buttons.md)'s rule that *draft renders every
pixel* costs so little.

Three things fall out of that, and all three are actionable:

- **It is cleanly pixel-bound.** Four times faster per halving of each
  dimension, almost exactly. Evaluation is not the bottleneck at this
  complexity; filling pixels is.
- **It is single-threaded.** Four cores were available and one was used. Frames
  are independent and `evaluate` is pure — the render loop is embarrassingly
  parallel, and this is the largest easy win available. It has not been taken
  yet because correctness came first and the seam is trivial to add later.
- **PNG encoding is inside that number.** A meaningful share of it, at 1080p.
  Splitting raster time from encode time is worth doing before optimising
  either.

**A debug build is ~100× slower** (the same 300-frame render: 244s debug versus
~12s release on the container, 3.9s on the developer machine above). Never quote, compare, or investigate a timing from `cargo run`
without `--release`. This is the single most common way to arrive at a wrong
conclusion about this codebase's speed.

## The structural advantages, and the one structural cost

Worth knowing which of these is real before optimising anything.

**Advantages we actually have:**

- **`evaluate` is pure.** No wall clock, no IO, no hidden state — so frames can
  be rendered in any order, on any thread, or cached and reused, with no
  invalidation logic to get wrong. AE's expression engine cannot make this
  promise, which is why its multi-frame rendering took so long to arrive and
  still carries caveats.
- **Frames are the native time domain**, so there is no re-derivation of "where
  are we?" per frame.
- **No plugin ABI to defend.** Every frame runs code we compiled.

**The cost, stated plainly:** we are a young renderer without an effect
pipeline. When effects land ([0005](decisions/0005-compositor-stage.md)) they
will dominate render time exactly as they do in AE, and today's numbers will
stop being representative. Measure again then, and do not carry a pre-effects
number forward as if it still meant something.

## Rules for a performance claim in this project

1. Release build, or it is not a measurement.
2. Say the machine, the core count, the resolution and the output format.
3. Publish the composition.
4. Compare against our own past number first.
5. Never state a cross-tool comparison broader than the composition you actually
   ran.
