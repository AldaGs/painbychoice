# 0003. Frames are the native time domain

- **Status:** Accepted — implemented
- **Recorded:** 2026-09-08, extracted from the README design journal.

## Context

Storing time in seconds makes every keyframe position a rounding question and
lets the composition end land between two frames.

## Decision

Frames are what `core` stores and reasons in; seconds are a presentation unit at
the edges. `Timebase` is the **only** place the two convert, and
`timecode()` → `hh:mm:ss.ff` lives there too. Never divide by `fps` by hand.

See [`../architecture.md`](../architecture.md) for how this threads through the
timeline, snapping and playback.

## Consequences

Keyframes snap to frames at any zoom by construction, and changing a comp's fps
*re-grids* the animation (`Comp::set_fps`) instead of leaving keys on stale
frame numbers.

One piece is still outstanding and is now blocking: `Comp::duration` is stored
in **seconds** with the frame count derived through a rounding conversion, so
"how many frames does this comp contain?" is answered by a `.round()`. Harmless
for a playhead, a ±1-frame ambiguity for a renderer. Phase 0 of
[`../production-plan.md`](../production-plan.md).
