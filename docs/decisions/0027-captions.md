# 0027. Captions: word-time selection, presets as data, cutting by text

- **Status:** Accepted — implemented (`core/src/captions.rs`, `caption_presets.json`, Karaoke `ramp`, `clips::remove_range`, Subtitles panel)
- **Decided:** 2026-11-15

## Context

Phase 4 asks for a caption layer, a selector driven by word times, Sonant's
presets as data, text-based cutting and a model downloader. PBC already had
most of the layer: a text layer with `subtitles` lays out words each frame,
and a Karaoke selector lights the spoken word.

## Decision

1. **The caption layer is the subtitled text layer.** No new node type. The
   brief's `{transcript, style, animation, overrides}` maps onto it: the
   transcript lives on the layer, a preset is *baked* into ordinary
   properties, and anything changed afterwards is the override.
2. **The word-time selector is Karaoke with a `ramp`.** Each word's
   selection rises 0→1 over `ramp` seconds from its own start. With
   `cumulative` and Subtract mode it selects the words not yet in, which is
   every Sonant entry animation (pop, rise, drop, spin…) as an animator's
   property offsets. `ramp: 0` is the old step, so files are unchanged.
3. **Presets are Sonant's `Style` JSON**, the fields PBC honours, shipped as
   `caption_presets.json` (22 built-ins). `Project::captionPresets` holds more
   in the same shape, so an agent authors one by writing JSON. `Subtitles`
   gains `uppercase`, a display transform that leaves the transcript's case.
4. **Cutting by text removes time from the whole comp** (`Op::RemoveRange`):
   layers crossing the range are razored, pieces inside are deleted, later
   content moves left, the comp gets shorter. A run of picked words takes the
   pause after it (to the next word's start). The subtitled layer is not
   split: it loses the words spoken in the range and later words move up.
   One undo step. Each cut is also **recorded on the comp** (`Comp::cuts`:
   the layers taken out and their stack places, the razor pairs at its
   edges, the words and caption timings) so it can be restored later, after
   other edits, from the Subtitles panel or `apply restore`: later content
   moves right, removed layers return, razor halves rejoin, words come back.
   Removed ids stay reserved (the id floor rises) so a restore can't collide.
5. **Model downloader** fetches `ggml-base.bin` from the whisper.cpp model
   repo into the autoSUBS models folder with the system `curl`. No HTTP crate.
   `Whisper::find` also accepts a CPU `whisper-cli.exe` in that folder.

## Consequences

- Linear ramps only; Sonant's spring/back overshoot, box backgrounds, glow,
  shadow, glitch and wave effects are not modelled (those presets fall back
  to a fade or the highlight alone).
- Graph links to a layer removed by a cut are forgotten and don't return
  with it. Restoring several cuts at the same spot out of order is approximate
  (newest first is exact).
- Cutting cuts music too. Keep music on its own comp/precomp if it must run
  through a cut.
- The whisper exe is not downloaded (a binary from a release page); only models.
