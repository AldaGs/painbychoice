//! The audio output stream: what the device asks for, and who fills it.
//!
//! This is the only part of the audio path that runs on somebody else's thread.
//! The driver calls back every few milliseconds wanting a block of samples, and
//! that callback has a hard deadline — miss it and the user hears a click, not
//! a dropped frame. So the rules inside [`MixEngine::fill`] are stricter than
//! anywhere else in the codebase:
//!
//! - **No allocation.** Buffers are sized once when the stream opens.
//! - **No blocking.** The shared state is read through a `try_lock` that holds
//!   the guard just long enough to clone an `Arc`. If the UI thread happens to
//!   be mid-swap, the block is silence — one block, at most a few milliseconds
//!   — which is strictly better than blocking the driver and glitching.
//! - **No decoding.** Sounds are decoded on import and read from memory here.
//!
//! The engine publishes its progress into [`crate::clock::AudioPosition`], and
//! that is the whole of the inversion: the device says what time it is, and the
//! picture follows. See [`crate::clock`] for why.

use std::sync::{Arc, Mutex};

use motion_core::asset::AssetId;
use motion_core::AudioSource;
use motion_render::Sound;

use crate::clock::AudioPosition;

/// Everything the callback needs, published as one immutable snapshot.
///
/// Immutable and swapped wholesale rather than mutated in place: the callback
/// then never sees a half-updated mix — sources from one edit and gains from
/// the next — and the lock it takes is only ever held for a pointer clone.
#[derive(Default)]
pub(crate) struct MixState {
    /// The comp's sounds, already resolved to positions and gains.
    pub(crate) sources: Vec<AudioSource>,
    /// Decoded sounds, by asset. Shared with the importer, never mutated here.
    pub(crate) sounds: std::collections::HashMap<AssetId, Arc<Sound>>,
    /// Where the loop starts and ends, in comp sample frames. The callback
    /// wraps within this rather than running off the end of the piece.
    pub(crate) loop_span: (i64, i64),
    /// Whether the transport is rolling. A stopped transport still keeps the
    /// stream open — tearing a device down and back up per play is slow and, on
    /// some drivers, audible — and simply outputs silence.
    pub(crate) playing: bool,
}

/// The shared handle between the UI thread and the audio callback.
#[derive(Clone, Default)]
pub(crate) struct SharedMix(Arc<Mutex<Arc<MixState>>>);

impl SharedMix {
    /// Publish a new mix. Called from the UI thread whenever the document, the
    /// transport or the loop bounds change.
    pub(crate) fn publish(&self, state: MixState) {
        if let Ok(mut slot) = self.0.lock() {
            *slot = Arc::new(state);
        }
    }

    /// Take a snapshot **without ever blocking**. `None` means the UI is
    /// mid-swap and this block should be silence.
    fn snapshot(&self) -> Option<Arc<MixState>> {
        self.0.try_lock().ok().map(|slot| Arc::clone(&slot))
    }
}

/// Fills device buffers from the published mix.
///
/// Owns its scratch space, so the callback allocates nothing.
pub(crate) struct MixEngine {
    shared: SharedMix,
    position: AudioPosition,
    sample_rate: u32,
    /// Preallocated working buffer for one source's contribution.
    scratch: Vec<f32>,
}

impl MixEngine {
    pub(crate) fn new(
        shared: SharedMix,
        position: AudioPosition,
        sample_rate: u32,
        max_block_frames: usize,
    ) -> Self {
        MixEngine {
            shared,
            position,
            sample_rate,
            // Generous: a driver may ask for a larger block than it advertised,
            // and `mix_into` clamps to this rather than panicking, so being
            // short would quietly quieten the mix.
            scratch: vec![0.0; max_block_frames.max(1) * 2 * 4],
        }
    }

    /// Fill one device block. **This is the realtime callback** — see the module
    /// docs for what it may not do.
    ///
    /// `out` is interleaved stereo. Returns the sample-frame position playback
    /// reached, which the caller publishes to the clock.
    pub(crate) fn fill(&mut self, out: &mut [f32]) {
        let Some(state) = self.shared.snapshot() else {
            // The UI is mid-swap. One silent block beats blocking the driver.
            out.fill(0.0);
            return;
        };
        if !state.playing {
            out.fill(0.0);
            return;
        }

        let frames = out.len() / 2;
        let origin = self.position.get();
        let (lo, hi) = state.loop_span;

        // Looping is applied here rather than by the caller because the device
        // asks for a block that may straddle the loop point, and half a block
        // of silence at every lap is exactly the artefact a user would describe
        // as "it stutters when it repeats".
        let span = hi - lo;
        let pos = if span > 0 { lo + (origin - lo).rem_euclid(span) } else { origin };

        let sounds = &state.sounds;
        let rate = self.sample_rate;
        let scratch = &mut self.scratch;
        motion_core::mix_into(out, scratch, pos, &state.sources, |asset, from, buf| {
            match sounds.get(&asset) {
                Some(sound) => {
                    sound.read_into(from, buf, rate);
                    true
                }
                // Not imported yet, or failed to decode: silence, never a stall.
                None => false,
            }
        });

        self.position.set(pos + frames as i64);
    }
}

/// The output device, held so the stream stays alive.
///
/// `cpal`'s stream stops when dropped, so this exists to be kept in `App` — the
/// value is the point, not its methods.
pub(crate) struct AudioOut {
    #[allow(dead_code)]
    stream: cpal::Stream,
    pub(crate) sample_rate: u32,
}

/// Open the default output device and start a stream feeding it from `shared`.
///
/// Returns `None` when there is no usable device, which is an ordinary state
/// rather than an error: a machine with no sound card, a remote session, an
/// output that has been unplugged. The editor keeps working on the wall clock —
/// see [`crate::clock::MasterClock::source`] — and this returning `None` is
/// exactly what selects that fallback.
pub(crate) fn start(shared: SharedMix, position: AudioPosition) -> Option<AudioOut> {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

    let host = cpal::default_host();
    let device = host.default_output_device()?;
    let config = device.default_output_config().ok()?;
    let sample_rate = config.sample_rate();
    let channels = config.channels() as usize;

    let mut engine = MixEngine::new(shared, position.clone(), sample_rate, 4096);
    // The device may not be stereo. Mixing is always stereo, so a wider device
    // gets the pair in its first two channels and silence elsewhere rather than
    // the mix smeared across a layout we did not ask about.
    let mut stereo = vec![0.0f32; 4096 * 2];

    let err_position = position.clone();
    let stream = device
        .build_output_stream(
            config.config(),
            move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                let frames = data.len() / channels.max(1);
                let needed = frames * 2;
                if stereo.len() < needed {
                    // Only reachable if a driver asks for more than the buffer
                    // was sized for. Growing here allocates on the audio thread,
                    // which is worse than one quiet block, so this fills silence
                    // and lets the next block be correct.
                    data.fill(0.0);
                    return;
                }
                engine.fill(&mut stereo[..needed]);
                for (i, out_frame) in data.chunks_mut(channels.max(1)).enumerate() {
                    for (ch, slot) in out_frame.iter_mut().enumerate() {
                        *slot = if ch < 2 { stereo[i * 2 + ch] } else { 0.0 };
                    }
                }
            },
            move |err| {
                // The device went away mid-stream — headphones unplugged, a
                // sample-rate change. Dropping out of "live" is what hands time
                // back to the wall clock instead of freezing the playhead on the
                // last sample position.
                eprintln!("audio stream error: {err}");
                err_position.set_live(false);
            },
            None,
        )
        .ok()?;

    stream.play().ok()?;
    position.set_live(true);
    Some(AudioOut { stream, sample_rate })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone_sound(rate: u32, frames: usize, value: f32) -> Arc<Sound> {
        Arc::new(Sound { sample_rate: rate, samples: vec![value; frames * 2] })
    }

    fn engine_with(state: MixState, rate: u32) -> (MixEngine, AudioPosition) {
        let shared = SharedMix::default();
        shared.publish(state);
        let pos = AudioPosition::new();
        (MixEngine::new(shared, pos.clone(), rate, 512), pos)
    }

    /// A stopped transport outputs silence and does not advance time — the
    /// stream stays open, because tearing a device down per play is slow and on
    /// some drivers audible.
    #[test]
    fn a_stopped_transport_is_silent_and_does_not_advance() {
        let (mut engine, pos) =
            engine_with(MixState { playing: false, ..Default::default() }, 48_000);
        let mut out = vec![1.0; 32];
        engine.fill(&mut out);
        assert!(out.iter().all(|s| *s == 0.0));
        assert_eq!(pos.get(), 0, "a stopped transport does not move the clock");
    }

    /// Playing advances the clock by exactly the block it filled. This is the
    /// contract the whole inversion rests on: if this drifts, the picture does.
    #[test]
    fn playing_advances_the_clock_by_the_block_size() {
        let mut sounds = std::collections::HashMap::new();
        sounds.insert(AssetId(1), tone_sound(48_000, 1000, 0.5));
        let state = MixState {
            sources: vec![AudioSource {
                asset: AssetId(1),
                start: 0,
                end: 1000,
                offset: 0,
                gain: (1.0, 1.0),
            }],
            sounds,
            loop_span: (0, 1000),
            playing: true,
        };
        let (mut engine, pos) = engine_with(state, 48_000);
        let mut out = vec![0.0; 64]; // 32 sample frames
        engine.fill(&mut out);
        assert_eq!(pos.get(), 32);
        engine.fill(&mut out);
        assert_eq!(pos.get(), 64, "each block advances by its own length");
    }

    /// The samples actually reach the device buffer, at their gain.
    #[test]
    fn the_mix_reaches_the_output_buffer() {
        let mut sounds = std::collections::HashMap::new();
        sounds.insert(AssetId(1), tone_sound(48_000, 1000, 0.5));
        let state = MixState {
            sources: vec![AudioSource {
                asset: AssetId(1),
                start: 0,
                end: 1000,
                offset: 0,
                gain: (1.0, 0.5),
            }],
            sounds,
            loop_span: (0, 1000),
            playing: true,
        };
        let (mut engine, _) = engine_with(state, 48_000);
        let mut out = vec![0.0; 8];
        engine.fill(&mut out);
        assert!((out[0] - 0.5).abs() < 1e-6, "left at full gain");
        assert!((out[1] - 0.25).abs() < 1e-6, "right at half");
    }

    /// A source whose sound has not been decoded yet is silence, not a stall —
    /// the audio thread can never wait for a file.
    #[test]
    fn a_missing_sound_is_silence_not_a_stall() {
        let state = MixState {
            sources: vec![AudioSource {
                asset: AssetId(99),
                start: 0,
                end: 1000,
                offset: 0,
                gain: (1.0, 1.0),
            }],
            sounds: Default::default(),
            loop_span: (0, 1000),
            playing: true,
        };
        let (mut engine, pos) = engine_with(state, 48_000);
        let mut out = vec![1.0; 8];
        engine.fill(&mut out);
        assert!(out.iter().all(|s| *s == 0.0));
        assert_eq!(pos.get(), 4, "time still advances — the piece is playing, silently");
    }

    /// **Looping happens inside a block.** A device block that straddles the
    /// loop point must come back to the start rather than reading past the end,
    /// which is the artefact a user would describe as a stutter on repeat.
    #[test]
    fn playback_wraps_within_the_loop_span() {
        let mut sounds = std::collections::HashMap::new();
        sounds.insert(AssetId(1), tone_sound(48_000, 1000, 0.5));
        let state = MixState {
            sources: vec![AudioSource {
                asset: AssetId(1),
                start: 0,
                end: 100,
                offset: 0,
                gain: (1.0, 1.0),
            }],
            sounds,
            loop_span: (0, 100),
            playing: true,
        };
        let (mut engine, pos) = engine_with(state, 48_000);
        // Jump the clock past the end of the loop; the next block must fold back
        // into it and still produce sound.
        pos.set(250);
        let mut out = vec![0.0; 16];
        engine.fill(&mut out);
        assert!(
            out.iter().any(|s| *s != 0.0),
            "a position past the loop end must fold back and still play"
        );
    }

    /// The engine is only ever handed a complete mix. A half-updated one — new
    /// sources with old gains — is what publishing an immutable snapshot
    /// prevents, and a swap mid-callback yields at worst one silent block.
    #[test]
    fn a_published_mix_replaces_the_previous_one_wholesale() {
        let shared = SharedMix::default();
        shared.publish(MixState { playing: true, loop_span: (0, 100), ..Default::default() });
        let first = shared.snapshot().expect("a snapshot");
        shared.publish(MixState { playing: false, loop_span: (0, 200), ..Default::default() });
        let second = shared.snapshot().expect("a snapshot");
        assert!(first.playing && !second.playing);
        assert_eq!(second.loop_span, (0, 200));
        assert!(!Arc::ptr_eq(&first, &second), "a publish swaps, never mutates");
    }
}
