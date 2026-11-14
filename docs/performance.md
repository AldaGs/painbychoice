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

**Frame-parallel rendering, measured 2026-09-08 on the same machine** (16
logical cores, demo at 1920x1080, PNG sequence, `--threads n`):

| Threads | Compression on the writer | Compression on the workers |
| --- | --- | --- |
| 1 | 68.5 | 72.8 |
| 2 | 169.9 | 143.3 |
| 4 | 267.5 | 225.8 |
| 8 | 312.8 | 345.4 |
| 16 (default) | 307.3 | **434.1** |

The left column stopped scaling at about eight threads, and the reason was the
writer: frames rendered in parallel but were *compressed* by the one thread that
wrote them. Measured directly, `PngSequence::push` was **1.78 ms/frame** at
1080p, against an Amdahl-implied serial fraction of ~2.9 ms/frame — so
compression was most of the ceiling.

Moving it to the workers ([`Preparer`], `encode.rs`) is the right column: **6.0x
over sequential**, and it keeps scaling past eight where it used to flatten. The
low-thread rows are slightly *worse*, which is the honest cost — `push` now
copies the frame before compressing it, and with one worker there is nothing to
win back. The crossover is around four threads.

**The ffmpeg path is unaffected by that change and is capped lower** (~215 fps at
8 threads): its `Preparer` is a passthrough because ffmpeg compresses in its own
process, so the ceiling there is the sidecar and the pipe, not our writer.

Note the 2-thread row in the left column is superlinear, which is measurement
noise rather than magic: single-run timings on a loaded desktop carry a few
percent, and 68.5 there against 76.4 in the table above is that spread.

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
- ~~**It is single-threaded.**~~ Taken, 2026-09-08: the offline renderer runs
  frames on all cores and writes them in order (`render/src/parallel.rs`). See
  the scaling table above, and note where it stops.
- **PNG encoding is inside that number.** A meaningful share of it, at 1080p.
  Splitting raster time from encode time is worth doing before optimising
  either.

**A debug build is ~100× slower** (the same 300-frame render: 244s debug versus
~12s release on the container, 3.9s on the developer machine above). Never quote, compare, or investigate a timing from `cargo run`
without `--release`. This is the single most common way to arrive at a wrong
conclusion about this codebase's speed.

## Realtime video playback (Phase 0 spike)

Measured 2026-11-14, release build, `--features libav`
([ADR 0023](decisions/0023-in-process-libav-decode.md)). Windows 11, Intel
i7-13620H (10 cores / 16 threads), 32GB, NVIDIA RTX 4060 Laptop (8GB, driver
581.57), FFmpeg 9.0 shared LGPL build.

**The composition.** Synthetic 4K clips, generated rather than committed:

```bash
ffmpeg -f lavfi -i "testsrc2=size=3840x2160:rate=30:duration=20,noise=alls=12:allf=t,hue=h=90" -c:v h264_nvenc -preset p5 -b:v 45M -g 30 -bf 2 -pix_fmt yuv420p h264_1.mp4
```

Three of those with different hues, plus one `hevc_nvenc -b:v 30M`. Each is
drawn at 42% scale and 85% opacity, offset, into a 1920x1080 target — all of
them overlapping. Played for 15s on a wall clock at 30fps with a redraw every
1/60s, then scrubbed to twenty scattered frames:

```bash
PBC_BENCH_DIR=<clips> cargo test --release -p motion-live --features libav -- --ignored playback_bench --nocapture
```

| Clips | Decode | Dropped | Redraw median / p99 | Scrub to a frame, median / max | Scrub to the exact frame, median / max |
| --- | --- | --- | --- | --- | --- |
| 1 H.264 | D3D11VA | 0 / 449 | — | 48 / 141 ms | 98 / 343 ms |
| 3 H.264 | D3D11VA | 0 / 1347 | 2.8 / 9.5 ms | **88 / 108 ms** | 221 / 360 ms |
| 3 H.264 + 1 HEVC | D3D11VA | **0 / 1796** | 3.6 / 12.6 ms | 101 / 124 ms | 269 / 439 ms |
| 3 H.264 + 1 HEVC | software | 0 / 1788 | 3.9 / 16.8 ms | 445 / 501 ms | 470 / 712 ms |

"Dropped" counts frames the playhead asked a clip for and moved past before
they were on screen. "A frame" is the keyframe the seek landed on; "the exact
frame" is the one under the playhead. Scrub times are until *every* clip got
there, so they are the slowest clip's.

What this says:

- **Playback has headroom.** Four 4K streams, no drops, and a redraw that uses
  under a quarter of a 60Hz frame.
- **Scrubbing is GOP-bound.** Reaching an exact frame means decoding from the
  keyframe before it — fifteen frames on average here, at about 140 frames a
  second per stream. No decoder design removes that; all-intra proxies do.
- **Hardware decode pays on a seek** (4x sooner to a first frame, 1.7x to the
  exact one, with four streams) and little during playback, where software
  also keeps up on this CPU.

What it does not say: this is a headless loop with no egui and no swapchain
present, on one machine, with synthetic 8-bit footage and no sound. See
[`phase-0-playback-spike.md`](phase-0-playback-spike.md).

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
