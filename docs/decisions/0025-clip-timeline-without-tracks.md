# 0025. A clip timeline without tracks

- **Status:** Accepted — implemented (`crates/live/src/clips.rs`, strips view)
- **Decided:** 2026-11-14

## Context

PBC Edit needs NLE editing (razor, ripple, roll, slip) over a document that
is a layer stack, not a set of tracks. The brief (section 2.1) keeps the
document trackless and asks the view to do the three jobs tracks quietly do:
packing many clips into few rows, targeting and locking, and keeping related
clips in sync.

## Decision

1. **Rows are computed, never stored.** A comp's top-level layers are packed,
   back to front, into the lowest row where they overlap nothing (half-open
   windows, so touching clips share a row). The strips view shows either one
   row per layer (as before) or the packed rows ("Packed rows" toggle).
2. **The tools are Sonant's, ported with their tests, onto `LayerTiming`.**
   `at = in_`, `len = out - in_`. Sonant's media `offset` is implicit: local
   time is `comp - start`, so moving `in_` alone already trims a head without
   moving the content. Ripple and roll act **within a packed row**, the way an
   NLE's act within a track.
   - Razor: both halves keep `start`, so no keyframe is moved or copied; the
     right half is a re-id'd copy placed just above the left.
   - Slip stays alt+drag in every tool, unclamped as before.
3. **Sync-lock is a group id on the layer** (`Node::sync`). Layers sharing one
   take the same edge deltas from any timing edit, are cut together by the
   razor (the right halves get a fresh shared id, so halves pair up), and are
   deleted together. `Node::link` was already AE parenting, so this is a new
   field, not a reuse. A footage layer carries its own sound, so the
   "video ↔ its audio" link Sonant needed is already one layer here; sync-lock
   is for captions and SFX (Phase 4).
4. **`Node::tags`** holds free labels for targeting and locking by tag. Stored
   and round-tripped now; no tool reads them yet.

Both new fields are `serde(default)` and skipped when empty, so old files load
unchanged and new files carry nothing extra until used. Evaluation ignores
both; invariants 1–3 are untouched.

## Consequences

- Edits act on **top-level** layers only. A nested layer keeps the plain
  trim/slide/slip it had.
- No source-length clamp: PBC never had one (an animation has no end), and
  footage's is in source frames at another rate. Add when it bites.
- Ripple-deleting two sync partners that share a row closes each gap from the
  original layout, not cumulatively. Correct for the A/V-pair case it exists for.
- No keyboard shortcuts for tools yet: Sonant's keys (`C`, `B`, `R`…) collide
  with PBC's (`R` restarts playback).
