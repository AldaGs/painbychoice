# 0029. Preview proxies

- **Status:** Accepted — implemented (`render/src/proxy.rs`, `live/src/proxies.rs`, `FootageCache::proxy_for`, `motion proxy`)
- **Decided:** 2026-11-15

## Context

Brief section 3: generate low-res, all-intra proxies on import in the
background; preview from the proxy, export from the original.

## Decision

1. **A proxy is found by where it would be, not by a document field.** Its
   path is `<cache>/pbc/proxies/<stem>-<fnv64(abs path | size | mtime)>.mov`
   (`%LOCALAPPDATA%` on Windows, `PBC_PROXY_DIR` overrides). Nothing in the
   `.pbc` changes, so proxies never enter undo history, never trip the live
   link, survive a project being copied, and go stale on their own when the
   source is replaced. The brief's `asset.proxy` field is not needed.
2. **540p, every frame a keyframe, the source's own frames.** ffmpeg sidecar,
   `-g 1` x264 where the local ffmpeg has it, MJPEG otherwise;
   `-fps_mode passthrough`, so source frame N is proxy frame N (pinned by a
   test). Only footage over 810 px tall or 1440 px wide gets one. A 20 s 4K
   H.264 clip proxies in ~4 s.
3. **The swap is one place: `FootageCache::image`.** With proxies on, a video
   is decoded from an `Asset` with the proxy's path and size, through either
   decode path (sidecar or realtime libav). Footage is drawn onto the layer's
   own rectangle, so a smaller frame lands in the same place.
4. **Proxy frames are cached under their own id** (`id | 1<<63`). An export
   shares the preview's cache and turns proxies off for its step, so it can
   never be handed a proxy frame (pinned by a test).
5. **Background, one ffmpeg at a time**, queued for every heavy video on open
   and after an import, polled from `about_to_wait` so an idle editor still
   picks them up. "Preview proxies" in the Assets panel switches them;
   `motion proxy x.pbc` makes the same files headlessly.

## Consequences

- Proxies are never cleaned up; delete the cache folder to reclaim space.
- A proxy failure is shown in the Assets panel; the original keeps playing.
