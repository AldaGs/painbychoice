# 0030. `pbc render`: the GPU export, headless

- **Status:** Accepted — implemented (`live/src/headless.rs`, `offscreen::Headless`)
- **Decided:** 2026-11-15

## Context

The brief (section 3) wants the vello offscreen target as the main export
path, with the CPU rasterizer kept as the reference. The editor's render queue
already exports through vello (`offscreen.rs`); scripts and agents only had
`motion render`, which is the CPU reference and draws footage differently.

## Decision

1. **`pbc render <file.pbc> --out <path> [--comp] [--start] [--end] [--scale]
   [--quality]`** runs the render queue's own job on a headless wgpu device,
   before any window or event loop exists, and prints one JSON report. Same
   pixels as the preview and the Draft/Master buttons, effects readback included.
2. It lives in the editor binary, not in `motion`, because the GPU renderer,
   the scene conversion and the footage cache are all `live` code. Moving them
   into a shared crate is the larger refactor this avoids for now.
3. `motion render` stays the CPU reference: no GPU needed, multi-threaded, and
   what the tests compare against.

## The bug this found

Exports stalled 60 s every ~32 frames of 1080p video and wrote the previous
frame in place of the stalled one. The prefetch runs as far ahead as the
memory budget allows (~32 frames at 1080p) while the cache treats 48 as
promised; the frame after the run-ahead was never requested, and the export's
wait cleared the lapsed promise without asking again. `FootageCache::decode_now`
re-requests whenever a promise lapses (pinned by a test that times out on the
old code). A 48-frame 1080p export went from 61 s to 0.9 s, every frame
matching its source.
