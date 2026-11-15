# 0026. Buses and outputs

- **Status:** Accepted — implemented (`core/src/audio.rs`, `render` `mix_stem`/`lufs`, `live/src/mixer.rs`, `motion render --output`)
- **Decided:** 2026-11-15

## Context

The brief (section 2.2) routes sound clip → bus → mix instead of picking an
output per clip, and makes exports (full mix, stems) a choice of buses. Sonant
ducked music per item; here ducking moves to the bus.

## Decision

1. **`Project::buses` and `Project::outputs`**, plus **`AudioClip::bus`** (an
   id). All `serde(default)` and skipped when empty: old files load unchanged
   and write nothing new. An unknown or missing bus id means "straight to the
   mix", so deleting a bus never silences a layer.
2. **A bus is folded into its sources, not mixed separately.** Gain, mute and
   solo multiply each source's gain in `evaluate_audio` (`audio::route`). No
   per-bus buffers, and `mix_into`'s signature and realtime rules are unchanged.
   Solo silences every non-soloed bus *and* unrouted sound.
3. **Ducking is a function of the timeline, not a level detector.** A source on
   a ducked bus is multiplied per sample by Sonant's `duckGain`, over the live
   spans of the key bus's sources, with 0.25s linear fades. Stateless and
   deterministic: preview, export and every stem agree, and nothing can pump.
   Muted or stem-excluded key sources are kept at zero gain, so they still duck.
4. **An output is a list of bus ids**; empty is the full mix. Stems are the
   mix with every other source at zero gain (`mix_stem`), so a music stem stays
   ducked exactly as it is in the mix. Mute and solo apply to exports too.
5. **Loudness is ported, not a dependency.** BS.1770 integrated loudness is
   ~50 lines (Sonant's `lufs` with its −3.01 LUFS reference test) instead of the
   `ebur128` crate the brief's stack table named. `motion preview` reports LUFS,
   sample peak, clipping and silence for the mix and each bus.

## Consequences

- Ducking keys on whole clips. A dialogue clip with pauses keeps music down
  through them; when transcripts land (Phase 4) the key spans can be word
  ranges instead, which is what Sonant did.
- Peak is sample peak, not true peak (no oversampling).
- The Mixer has live meters: the callback raises lock-free per-bus and mix
  peaks (`mix_into_metered`), the UI reads and resets them each frame. A bus
  meter is the loudest of its sources, not their sum. Stems export from the
  Render menu as well as the CLI.
- Bus gain is a `Value<f64>` (keyframes, like a layer's level), resolved once
  per `evaluate_audio` at the top-level comp's frame; a plain number from an
  older file loads as a constant. During playback the editor republishes the
  mix every frame, so layer and bus automation is heard live in ~16 ms steps
  (exports resolve it per 21 ms block).
