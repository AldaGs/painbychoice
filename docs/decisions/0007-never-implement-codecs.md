# 0007. Export: never implement codecs

- **Status:** Accepted — not yet built
- **Recorded:** 2026-09-08, extracted verbatim from the README design journal.

## Context

Encoding is a solved problem with decades of edge cases, and it is not what
this project is for.

## Decision

- Render deterministic frames → pipe raw RGBA to **ffmpeg** (binary via stdin
  `rawvideo`, or `ffmpeg-next` bindings). `evaluate(doc, t)` is pure and
  non-realtime — a perfect offline render queue (full quality at any fps,
  ignoring playback speed). PNG-sequence export is nearly free (`image` crate).
- Pure-Rust encoders (`rav1e`, `gif`) are supplementary, not the H.264 path.
- Put encoders behind an `Encoder` trait: ffmpeg one impl, PNG-sequence another.
