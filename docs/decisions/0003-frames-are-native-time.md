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

The last piece of this landed on 2026-09-08: `Comp::duration_frames` is now
**stored**, with `duration_seconds()` derived. It used to be the other way
round, which made "how many frames does this comp contain?" the output of a
`.round()` — 5.0s at 23.976fps is 119.88 frames, so a renderer could write 119
or 120. Harmless for a playhead, a ±1-frame difference in an exported file.

A pre-frames `.pbc` still opens: the old seconds field is read into a private
`legacy_duration` and folded by `migrate()`, which is where the conversion has
to happen because serde gives no guarantee that `fps` is deserialized first. The
legacy field is never written back out.
