# Phase 0 — playback spike: go / no-go

2026-11-14 · branch `phase-0-playback-spike` · decision recorded in
[ADR 0023](decisions/0023-in-process-libav-decode.md) · numbers in
[`performance.md`](performance.md#realtime-video-playback-phase-0-spike)

## Verdict: go, with one caveat

Realtime multi-stream 4K playback works on this engine. Exact-frame scrubbing
of long-GOP 4K does not meet 100ms and will not without proxies.

| Gate | Result | |
| --- | --- | --- |
| 3 overlapping 4K H.264 at 30fps, <1% dropped | 0 of 1,347 dropped | pass |
| …plus one 4K HEVC (4 streams) | 0 of 1,796 dropped | pass |
| Scrub shows *a* frame within 100ms (3 clips) | median 88ms, max 108ms | marginal |
| Scrub shows *the exact* frame within 100ms (3 clips) | median 221ms, max 360ms | **fail** |
| Decoded frame N matches the sidecar | `libav::tests`, after mid-GOP seeks | pass |
| Audio stays the master clock | by construction; see below | not measured |

## What was measured, and what was not

The benchmark (`video::tests::playback_bench`) drives the editor's own
`FootageCache`, conversion pass and vello renderer from a wall clock, headless.
It does **not** cover:

- **The real window.** No egui, no swapchain present, no vsync. Redraws took a
  median 2.8ms of a 16.7ms budget, so there is room, but it is not the same
  measurement. The editor was only smoke-launched with the feature on; nobody
  has yet played 4K footage in it by hand.
- **Audio.** The audio callback and the master clock were not changed, and no
  decode thread touches them, so audio cannot be made to wait by video. That is
  an argument, not a measurement: the bench plays no sound.
- **Real footage.** The clips are synthetic (`testsrc2` + noise, NVENC, 45Mbps
  H.264 / 30Mbps HEVC, 8-bit, GOP 30, two B-frames). Phone footage has similar
  bitrates and GOPs; camera footage (10-bit, 4:2:2, long GOP) will be slower
  and currently goes through an 8-bit software conversion.
- **Other machines.** One laptop with an RTX 4060. Software decode on the same
  machine also played four streams without drops, but scrubbed at ~450ms.

## Recommendations for Phase 1 onward

1. **Pull proxies forward** from Phase 6 if scrub feel matters early. All-intra
   proxies make every frame a keyframe, which is the only thing that gets
   exact-frame scrubbing under 100ms.
2. **Keep `libav` a feature until the FFmpeg DLLs are bundled**, then make it
   the default for `pbc-edit`.
3. **Play real footage in the window once** before building on this: import a
   few phone clips, run with `PBC_VIDEO_STATS=1`, and read the drop count.

## Also in this branch

- A pre-existing off-by-one in the sidecar decoder is fixed (every footage
  frame was one late; see ADR 0023). This shifts footage in existing projects
  one frame earlier, to where it should have been.
- `tests::two_footage_layers_each_keep_their_own_stream` is flaky on `main`
  (it failed every time when run alone on untouched `main`, and intermittently
  in full runs). Not touched here.
