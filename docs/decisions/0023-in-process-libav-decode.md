# 0023. In-process libav decode for the realtime preview

- **Status:** Accepted — spike implemented behind the `libav` feature
- **Decided:** 2026-11-14
- **Supersedes, for the preview only:** the sidecar choice recorded in the
  consequences of [0008](0008-asset-registry.md). Export, the CPU rasterizer and
  stills keep the sidecar.

## Context

[0008](0008-asset-registry.md) named `ffmpeg-next` for video and then shipped an
`ffmpeg` *sidecar* instead, "so a Windows build has no C library to compile".
That was right for a motion tool that occasionally places one clip. It does not
hold for an editor that plays several:

- A sidecar hands over RGBA, converted on the CPU. A 4K frame is 33MB, copied
  through a pipe and uploaded again every redraw.
- A seek is a process start (~229ms, measured in `live/src/footage.rs`), so
  scrubbing restarts a decoder per jump.
- There is no hardware decode, and no way to share one.

PBC Edit (the clip-timeline front-end) needs three or four 4K streams playing
together. Phase 0 of that work was a go / no-go on exactly this.

## Decision

**The preview decodes video in-process through linked libav** (`ffmpeg-next`,
dynamically linked, LGPL build), **and keeps frames in YUV until a shader.**

- `render/src/libav.rs` — `LibavStream`: container seek + decode forward,
  hardware decode where the platform offers it (D3D11VA / VideoToolbox / VAAPI,
  falling back to frame-threaded software by itself), frames out as 8-bit NV12.
- `live/src/video.rs` — one decode-ahead thread and one small frame cache per
  clip, keyed by frame; planes uploaded as two textures and converted to RGB by
  a WGSL pass into a texture vello draws through `override_image`.
- **Nothing in this path tells the time.** The playhead still comes from the
  master clock ([0021](0021-audio-is-the-master-clock.md)). A redraw shows the
  newest decoded frame at or before the playhead and never waits for a decoder;
  the audio callback is untouched. Video drops frames, audio does not hear of it.
- **The sidecar remains the reference.** A render sets `FootageCache::realtime`
  off and reads the sidecar. `libav::tests` pin frame N of this decoder to frame
  N of the sidecar after a mid-GOP seek, and `video::tests` pin the shader to
  the CPU conversion those tests use — so preview-equals-export is still a
  tested claim, not a hope.

This keeps [0007](0007-never-implement-codecs.md) and invariant 23: every byte
of decoding is still FFmpeg's. What changes is which side of a process boundary
it runs on.

## Consequences

- **A C dependency, optional for now.** Building with `--features libav` needs
  an FFmpeg SDK (`FFMPEG_DIR`) and libclang for bindgen; running needs FFmpeg's
  shared libraries on `PATH`. The default build is unchanged and still has no C
  library in it. Shipping PBC Edit means bundling those DLLs — LGPL, dynamically
  linked, which is the arrangement the licence is written for.
- **Untagged footage is BT.601**, because that is what swscale assumes and so
  what the sidecar and every export draw. Convention says BT.709 for HD. Fixing
  it means fixing both paths together; fixing one would break the parity above.
- **8-bit only.** Anything that isn't NV12 / yuv420p goes through swscale to
  8-bit NV12 on the decode thread: correct, slow, and it discards HDR.
- **Hardware frames take a round trip.** A D3D11VA frame is copied to system
  memory and uploaded again through wgpu. It is fast enough (see
  `docs/performance.md`) and it is not zero-copy; sharing the decoder's texture
  with wgpu is the upgrade if 8K or many more streams ever matter.
- **Exact-frame scrubbing is bounded by GOP length**, not by this design: the
  frames between the keyframe and the target have to be decoded. All-intra
  proxies are what make scrubbing instant, and they were already on the roadmap.
- **A scrub goes to the frame; it never plays the way there** (amended
  2026-11-17). The preview used to show the seek's keyframe first, always, and a
  short jump ahead walked the stream handing every frame in between to the
  screen, so stepping a little way forward *played* the footage. Now a jump
  forward skips (`LibavStream::skip_to`) whenever the container's index puts the
  target's keyframe no further on than the decoder already is (the same work as
  a seek, minus the frames already behind it), and nothing in between is
  published. The last picture holds until the exact frame arrives; the keyframe
  stands in only once a seek has taken 150ms, and any stand-in must be nearer
  the playhead than what is already up. Pinned by
  `video::tests::a_jump_forward_publishes_no_frame_in_between`.
- **Variable frame rate is approximated** on a constant grid at the probed rate,
  the same as the sidecar. A gap holds the previous frame.
- **One wgpu device.** A second window on a different adapter would not see the
  converted textures.

## Found on the way

Pinning the two decoders together exposed an off-by-one in the sidecar:
`seek_seconds` aimed at the middle of frame N's interval, ffmpeg keeps frames at
or after the seek point, so every request returned frame N+1 and frame 0 of a
clip was unreachable — in the preview and in exports alike. Fixed in
`render/src/decode.rs`; the stream test now expects ten frames of a ten-frame
clip rather than "about nine".
