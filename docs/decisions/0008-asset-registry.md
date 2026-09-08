# 0008. Footage import: an asset registry, references never pixels

- **Status:** Accepted — implemented
- **Recorded:** 2026-09-08, extracted verbatim from the README design journal.

## Context

Footage forces the first change to a model that has been purely procedural.

## Decision

Forces the first change to the currently-procedural model: an **asset registry**
— nodes reference an asset by id/path; the registry owns the decoded source +
frame cache. Store *references* in `.pbc`, never pixels.
- Images: `image` (raster) + `resvg` (SVG) → texture.
- Video: `ffmpeg-next` decode; the hard part is **frame-accurate seeking** for
  non-linear scrubbing (seek-to-keyframe + decode-forward + frame cache).
- Audio: `symphonia` decode + `cpal`/`rodio` output. Needs a real **master
  clock** (today's playback is a wall-clock loop) and a waveform for sync.

## Consequences

Implemented as `core/src/asset.rs` plus `render/src/decode.rs` (an `ffmpeg`
*sidecar* rather than linked libav, so a Windows build has no C library to
compile) and `live/src/footage.rs` (the threaded decode cache). The split holds:
the document stores references, `evaluate` stays pure, and everything that
touches a file lives in the shell.
