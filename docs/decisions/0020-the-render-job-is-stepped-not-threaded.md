# 0020. The GUI render job is stepped from the redraw loop, not threaded

- **Status:** Accepted — implemented
- **Decided:** 2026-09-08

## Context

[0018](0018-two-render-buttons.md) settled *what* the export buttons are. This
settles *how* the work runs once one is pressed.

The requirement from the production plan is explicit: a user must be able to get
a file out "without the editor freezing". A render is three hundred to several
thousand frames; at 1080p that is seconds to minutes. Blocking the event loop
for that long is not a slow UI, it is an application Windows offers to close for
you.

The reflex answer is a worker thread. It does not survive contact with what a
render actually needs:

- the **`wgpu` device and queue**, which the preview renders through every frame,
- the **vello `Renderer`**, which owns the shader pipelines and the glyph atlas,
- the **footage cache**, which decodes and holds video frames.

All three live in `App`, on the main thread, and all three are used by the
preview. Moving them to a worker costs one of two things:

1. **A second device.** Two `wgpu` devices means two copies of the pipelines and
   the atlas, and on a laptop with integrated *and* discrete graphics it can mean
   rendering the export on a **different adapter than the preview**. That is
   precisely the machine where "the render matches the preview" most needs to
   hold, and the fix would be to defeat the thing we just paid for.
2. **A lock around the shared state**, held for the duration of each frame's
   render. The preview then blocks on the export's lock — the editor freezes
   again, by a longer route, with a data race added to the design.

There is a third pressure: vello's `render_to_texture` plus a texture readback is
a synchronous stall by construction (`read_texture_rgba` waits on the map). A
thread does not make that asynchronous; it only moves where the wait happens.

## Decision

**A render job is a state machine stepped from the redraw loop.**

Each redraw renders a fixed slice of frames (`FRAME_BUDGET`, currently 4), pushes
them to the encoder, and requests another redraw. The job holds its own
`OffscreenTarget` and its encoder; the GPU handles are passed in per step rather
than owned.

Three properties follow, and they are the reason this is the right shape rather
than merely the easy one:

1. **Nothing is shared across threads, so nothing needs a lock.** The export and
   the preview use the same device on the same thread, in turn. Preview-equals-
   export is then a statement about one renderer, not two.
2. **Cancellation is immediate and honest.** There is no thread to signal and
   join; the next step simply does not happen. See the note on `Encoder::abort`
   below, which is the part that is *not* free.
3. **The project is snapshotted at start.** A job clones the `Project` it will
   render, so edits made while it runs cannot change the output halfway through.
   A file that is frames 0–149 of one document and 150–299 of another is a bug
   with no symptom until someone watches the result.

The admitted cost: **the editor's frame rate drops while a render runs.** The UI
stays live and cancellable, but it is sharing a GPU with the export. This is
honest rather than hidden, and it is the correct trade — the alternative designs
buy smoothness with either a wrong-adapter render or a lock that freezes the
editor outright.

## Consequences

- **`Encoder::abort` had to exist.** Cancelling by dropping the encoder is
  actively wrong for a process-backed one: closing ffmpeg's stdin is exactly the
  signal that means *finalize the container*, so a dropped ffmpeg encoder
  produces a complete, playable, **wrong-length** video with nothing to mark it
  as partial. `abort` kills the process before the pipe closes and removes the
  fragment. `PngSequence` keeps its frames — a stills sequence is visibly
  partial, and the frames that rendered are often why someone cancelled.
- **Frame-parallel rendering composes with this rather than replacing it.** The
  parallel win is in `evaluate`, which is pure and is the CPU half of a step; the
  GPU half stays serial because there is one device. A step can evaluate its
  slice in parallel and render the results in order.
- **One job at a time.** They would contend for the single GPU anyway, so a
  queue of several would only interleave the contention. `RenderQueue` holds at
  most one active job plus a log of what finished.
- **`FRAME_BUDGET` is a latency knob, not a throughput one.** Raising it reduces
  per-redraw overhead and makes Cancel less responsive. If it ever needs to be
  adaptive, the signal to drive it from is the measured step time, not the frame
  count.
