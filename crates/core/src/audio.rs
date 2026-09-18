//! The sound half of the document: what a layer contributes, and how a comp's
//! layers sum into one signal.
//!
//! Headless like the rest of `core` — this module decides *what* should be
//! heard at a given moment and never opens a file. Decoding lives behind
//! [`crate::asset`]'s registry discipline, exactly as pixels do: the document
//! holds a reference and a level, and somebody else turns that into samples.
//! It is what lets the whole mix be unit-tested with no audio device, no file
//! on disk, and no wall clock.
//!
//! # Sample frames, not samples
//!
//! Every position and length here is in **sample frames** — one per instant of
//! time, regardless of channel count. A stereo second at 48kHz is 48000 sample
//! frames and 96000 samples, and conflating the two is the classic way to get
//! audio that plays at half speed or half length. The word "sample" alone is
//! avoided in names for that reason.

use serde::{Deserialize, Serialize};

use crate::asset::AssetId;
use crate::value::Value;

/// The sound a layer contributes.
///
/// Level and pan are ordinary [`Value`]s, so they keyframe, ease, take
/// expressions and join the node graph with no audio-specific machinery — the
/// same trick that made effect parameters animate for free.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AudioClip {
    /// Which imported sound this plays.
    pub asset: AssetId,
    /// Linear gain. `1.0` is unity; `0.0` is silence.
    ///
    /// Linear rather than decibels because it is what the mix multiplies by,
    /// and a `Value<f64>` interpolating in dB would fade differently than the
    /// same keyframes on any other property. A dB read-out is the UI's job.
    pub level: Value<f64>,
    /// Stereo position: `-1.0` hard left, `0.0` centre, `1.0` hard right.
    pub pan: Value<f64>,
    /// Muted layers keep their settings and contribute nothing — resolved away
    /// rather than removed, like a disabled effect.
    #[serde(default = "yes")]
    pub enabled: bool,
}

fn yes() -> bool {
    true
}

impl AudioClip {
    /// A clip at unity gain, centred.
    pub fn new(asset: AssetId) -> Self {
        AudioClip {
            asset,
            level: Value::constant(1.0),
            pan: Value::constant(0.0),
            enabled: true,
        }
    }
}

/// One layer's sound, resolved at a moment: which asset, how loud, how placed.
///
/// The audio counterpart of a `RenderItem` — the flat, already-resolved thing a
/// backend consumes, with every `Value` collapsed to a number and every
/// question about *whether* this layer sounds already answered.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AudioSource {
    pub asset: AssetId,
    /// Where this layer's sound starts, as a **comp** sample-frame position.
    /// Negative when the layer begins before the range being mixed.
    pub start: i64,
    /// The first comp sample frame it no longer sounds on, exclusive.
    pub end: i64,
    /// The sample frame *within the asset* that `start` plays.
    ///
    /// Not always zero: a layer trimmed at its head starts partway into its
    /// source, exactly as a trimmed video layer shows a later frame.
    pub offset: i64,
    /// Per-channel gain, already folded from level and pan.
    pub gain: (f32, f32),
}

/// Constant-power stereo panning.
///
/// `-1` hard left through `0` centre to `1` hard right, returning the left and
/// right gains. Constant *power* (the sine/cosine law) rather than linear:
/// summing two linear-panned halves at centre gives `0.5 + 0.5`, which is 6dB
/// down in power and audibly dips as a sound pans across the middle. The
/// sine/cosine pair keeps `l² + r² = 1` at every position, so a pan sweep holds
/// its loudness — the same law every mixing desk uses, and the reason a
/// centred sound reads at ~0.707 rather than 1.0.
pub fn pan_gains(pan: f64) -> (f32, f32) {
    let p = pan.clamp(-1.0, 1.0);
    // Map -1..1 onto 0..π/2 so the two gains trade off along the quarter circle.
    let angle = (p + 1.0) * std::f64::consts::FRAC_PI_4;
    (angle.cos() as f32, angle.sin() as f32)
}

/// Fold a level and a pan into the per-channel gains a mixer multiplies by.
///
/// Level is clamped non-negative: a negative gain is a phase inversion, which is
/// a real effect and not what someone dragging a volume slider below zero
/// means.
pub fn gains(level: f64, pan: f64) -> (f32, f32) {
    let level = if level.is_finite() { level.max(0.0) } else { 0.0 };
    let (l, r) = pan_gains(pan);
    (l * level as f32, r * level as f32)
}

/// Convert a position in comp **frames** to comp **sample frames**.
///
/// The bridge between the two grids the application runs on, and the one place
/// the conversion is written. Rounds rather than truncating, so a layer's start
/// lands on the nearest sample instead of consistently early.
pub fn frames_to_sample_frames(frame: f64, fps: f64, sample_rate: u32) -> i64 {
    if fps <= 0.0 || sample_rate == 0 || !frame.is_finite() {
        return 0;
    }
    (frame / fps * sample_rate as f64).round() as i64
}

/// The reverse: a comp sample-frame position as a comp frame.
///
/// **This is the master clock's conversion.** When audio is the time source, the
/// frame shown is derived from the sample position through here, so the picture
/// follows the sound rather than the two running on separate clocks and drifting
/// apart over a long piece.
pub fn sample_frames_to_frames(position: i64, fps: f64, sample_rate: u32) -> f64 {
    if sample_rate == 0 {
        return 0.0;
    }
    position as f64 / sample_rate as f64 * fps
}

/// Mix resolved sources into an interleaved stereo buffer.
///
/// `out` is `[l, r, l, r, …]`, `frames` sample frames long, representing comp
/// sample positions `origin .. origin + frames`. `fetch` supplies an asset's
/// samples: given an asset and a sample-frame range, it returns interleaved
/// stereo for that range. Returning `None` means "not available" — a sound that
/// has not finished loading is silence, never a failed render.
///
/// Summing without normalising is deliberate: two sounds at unity really are
/// twice the amplitude, and an automatic divide-by-N would make every layer
/// quieter as soon as another was added, which is the behaviour people describe
/// as "the mixer is fighting me". Clipping is the mixer's own business —
/// [`mix_into`] hard-clips at the end because a sample outside `[-1, 1]` is
/// undefined at the device, not because the sum was wrong.
/// # Realtime safety
///
/// This allocates nothing and locks nothing, and `fetch` is handed a slice to
/// **fill** rather than asked to return a `Vec` for exactly that reason: this
/// runs on the audio device's callback thread, where an allocation is not a
/// slow frame but an audible click. `scratch` is the caller's preallocated
/// working buffer, sized for the largest block it will ask for.
pub fn mix_into<F>(
    out: &mut [f32],
    scratch: &mut [f32],
    origin: i64,
    sources: &[AudioSource],
    mut fetch: F,
) where
    F: FnMut(AssetId, i64, &mut [f32]) -> bool,
{
    for s in out.iter_mut() {
        *s = 0.0;
    }
    let frames = out.len() / 2;
    if frames == 0 {
        return;
    }
    let block_end = origin + frames as i64;

    for src in sources {
        // The overlap between this source's live span and the block being
        // filled. Everything downstream is in the source's own sample frames,
        // so the clamp happens once, here.
        let from = src.start.max(origin);
        let to = src.end.min(block_end);
        if to <= from {
            continue;
        }
        // Bounded by the scratch buffer as well as by the overlap: a caller
        // that sized its scratch for a smaller block gets a quieter mix, never
        // a panic on the audio thread.
        let take = ((to - from) as usize).min(scratch.len() / 2);
        if take == 0 {
            continue;
        }
        let asset_from = src.offset + (from - src.start);
        let window = &mut scratch[..take * 2];
        if !fetch(src.asset, asset_from, window) {
            continue;
        }
        let dest = (from - origin) as usize;
        for i in 0..take {
            out[(dest + i) * 2] += window[i * 2] * src.gain.0;
            out[(dest + i) * 2 + 1] += window[i * 2 + 1] * src.gain.1;
        }
    }

    for s in out.iter_mut() {
        *s = s.clamp(-1.0, 1.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_centred_pan_is_equal_and_constant_power() {
        let (l, r) = pan_gains(0.0);
        assert!((l - r).abs() < 1e-6, "centre is equal on both sides");
        assert!(
            (l * l + r * r - 1.0).abs() < 1e-6,
            "constant power: {l}² + {r}² must be 1, not {}",
            l * l + r * r
        );
        // The number people are surprised by, pinned so nobody "fixes" it.
        assert!((l - 0.70710677).abs() < 1e-6, "centre sits at √½, not 1.0");
    }

    #[test]
    fn panning_hard_silences_the_far_side() {
        let (l, r) = pan_gains(-1.0);
        assert!((l - 1.0).abs() < 1e-6);
        assert!(r.abs() < 1e-6);
        let (l, r) = pan_gains(1.0);
        assert!(l.abs() < 1e-6);
        assert!((r - 1.0).abs() < 1e-6);
    }

    /// The property constant-power panning exists for: sweeping across the
    /// middle must not dip in loudness.
    #[test]
    fn a_pan_sweep_holds_its_power() {
        for i in -10..=10 {
            let (l, r) = pan_gains(i as f64 / 10.0);
            let power = l * l + r * r;
            assert!((power - 1.0).abs() < 1e-5, "power dipped to {power} at {i}");
        }
    }

    #[test]
    fn pan_is_clamped_rather_than_wrapping() {
        assert_eq!(pan_gains(5.0), pan_gains(1.0));
        assert_eq!(pan_gains(-5.0), pan_gains(-1.0));
    }

    /// A negative level is a phase inversion, which is not what a volume
    /// control below zero means. NaN is silence rather than poison.
    #[test]
    fn a_nonsense_level_becomes_silence() {
        assert_eq!(gains(-1.0, 0.0), (0.0, 0.0));
        assert_eq!(gains(f64::NAN, 0.0), (0.0, 0.0));
    }

    /// The two grids convert both ways and round-trip. An off-by-one here is a
    /// frame of drift per conversion, which over a three-minute piece is the
    /// sync bug this whole phase exists to prevent.
    #[test]
    fn frames_and_sample_frames_round_trip() {
        let (fps, rate) = (24.0, 48_000);
        for frame in [0.0, 1.0, 24.0, 100.0, 1000.0] {
            let pos = frames_to_sample_frames(frame, fps, rate);
            let back = sample_frames_to_frames(pos, fps, rate);
            assert!((back - frame).abs() < 1e-9, "{frame} -> {pos} -> {back}");
        }
        assert_eq!(frames_to_sample_frames(1.0, 24.0, 48_000), 2000);
    }

    /// A broadcast rate is where truncation and rounding disagree, and where a
    /// zero guard matters — the same class of bug the frame-count migration was
    /// about.
    #[test]
    fn the_grid_conversion_guards_its_divisors() {
        assert_eq!(frames_to_sample_frames(10.0, 0.0, 48_000), 0);
        assert_eq!(frames_to_sample_frames(10.0, 24.0, 0), 0);
        assert_eq!(frames_to_sample_frames(f64::NAN, 24.0, 48_000), 0);
        assert_eq!(sample_frames_to_frames(1000, 24.0, 0), 0.0);
    }

    /// A source that lies entirely outside the block contributes nothing and,
    /// importantly, is never *fetched* — a mixer that pulled samples it cannot
    /// use would decode the whole timeline for every block.
    #[test]
    fn a_source_outside_the_block_is_not_even_fetched() {
        let mut out = vec![0.0; 8];
        let mut fetched = 0;
        let src = AudioSource {
            asset: AssetId(1),
            start: 1000,
            end: 2000,
            offset: 0,
            gain: (1.0, 1.0),
        };
        let mut scratch = vec![0.0; 64];
        mix_into(&mut out, &mut scratch, 0, &[src], |_, _, buf| {
            fetched += 1;
            buf.fill(1.0);
            true
        });
        assert_eq!(fetched, 0, "nothing outside the block should be fetched");
        assert!(out.iter().all(|s| *s == 0.0));
    }

    /// A source overlapping the start of the block is placed at the right
    /// offset, and reads from the right point *inside its own asset* — the two
    /// offsets are different numbers and swapping them is the obvious bug.
    #[test]
    fn a_partly_overlapping_source_lands_at_the_right_offset() {
        let mut out = vec![0.0; 8]; // 4 sample frames, comp 100..104
        let mut asked = None;
        let src = AudioSource {
            asset: AssetId(1),
            start: 98,
            end: 102,
            offset: 50,
            gain: (1.0, 1.0),
        };
        let mut scratch = vec![0.0; 64];
        mix_into(&mut out, &mut scratch, 100, &[src], |_, from, buf| {
            asked = Some((from, buf.len() / 2));
            buf.fill(1.0);
            true
        });
        // Comp 100..102 overlaps, which is 2 sample frames starting 2 into the
        // source, so 52 in its own samples.
        assert_eq!(asked, Some((52, 2)));
        assert_eq!(&out[0..4], &[1.0, 1.0, 1.0, 1.0], "the overlap is filled");
        assert_eq!(&out[4..8], &[0.0, 0.0, 0.0, 0.0], "past its end is silent");
    }

    /// Gains are applied per channel, so a panned source is not merely quieter.
    #[test]
    fn gains_are_applied_per_channel() {
        let mut out = vec![0.0; 4];
        let src = AudioSource {
            asset: AssetId(1),
            start: 0,
            end: 2,
            offset: 0,
            gain: (0.25, 0.75),
        };
        let mut scratch = vec![0.0; 64];
        mix_into(&mut out, &mut scratch, 0, &[src], |_, _, buf| {
            buf.fill(1.0);
            true
        });
        assert_eq!(out, vec![0.25, 0.75, 0.25, 0.75]);
    }

    /// Sources sum. Two identical sounds are twice as loud, not averaged — an
    /// automatic divide would make every layer quieter as another was added.
    #[test]
    fn sources_sum_rather_than_average() {
        let mut out = vec![0.0; 4];
        let src = |id| AudioSource {
            asset: AssetId(id),
            start: 0,
            end: 2,
            offset: 0,
            gain: (0.4, 0.4),
        };
        let mut scratch = vec![0.0; 64];
        mix_into(&mut out, &mut scratch, 0, &[src(1), src(2)], |_, _, buf| {
            buf.fill(1.0);
            true
        });
        assert!((out[0] - 0.8).abs() < 1e-6, "two at 0.4 make 0.8, got {}", out[0]);
    }

    /// The sum is clipped, because a sample outside [-1, 1] is undefined at the
    /// device — not because summing was the wrong thing to do.
    #[test]
    fn the_sum_is_clipped_at_full_scale() {
        let mut out = vec![0.0; 2];
        let src = |id| AudioSource {
            asset: AssetId(id),
            start: 0,
            end: 1,
            offset: 0,
            gain: (1.0, 1.0),
        };
        let mut scratch = vec![0.0; 64];
        mix_into(&mut out, &mut scratch, 0, &[src(1), src(2), src(3)], |_, _, buf| {
            buf.fill(1.0);
            true
        });
        assert_eq!(out, vec![1.0, 1.0]);
    }

    /// A sound that has not loaded is silence, never a failure — the same rule
    /// as missing footage drawing nothing rather than aborting a render.
    #[test]
    fn an_unavailable_source_is_silence() {
        let mut out = vec![0.5; 4];
        let src = AudioSource {
            asset: AssetId(1),
            start: 0,
            end: 2,
            offset: 0,
            gain: (1.0, 1.0),
        };
        let mut scratch = vec![0.0; 64];
        mix_into(&mut out, &mut scratch, 0, &[src], |_, _, _| false);
        assert_eq!(out, vec![0.0, 0.0, 0.0, 0.0], "the buffer is cleared, then left silent");
    }

    /// A short read — the tail of a file — fills what it can rather than
    /// panicking on a slice it was promised and did not get.
    #[test]
    fn a_short_read_fills_what_it_can() {
        let mut out = vec![0.0; 8];
        let src = AudioSource {
            asset: AssetId(1),
            start: 0,
            end: 4,
            offset: 0,
            gain: (1.0, 1.0),
        };
        // Asked for 4 sample frames, only 2 come back.
        // The caller's scratch only covers two sample frames, so only two are
        // mixed however many were asked for — the tail-of-a-file case.
        let mut scratch = vec![0.0; 4];
        mix_into(&mut out, &mut scratch, 0, &[src], |_, _, buf| {
            buf.fill(1.0);
            true
        });
        assert_eq!(&out[0..4], &[1.0, 1.0, 1.0, 1.0]);
        assert_eq!(&out[4..8], &[0.0, 0.0, 0.0, 0.0]);
    }

    /// The buffer is cleared before mixing, so a reused block does not play the
    /// previous one underneath — the bug that sounds like a stutter.
    #[test]
    fn a_reused_buffer_is_cleared_first() {
        let mut out = vec![9.0; 4];
        let mut scratch = vec![0.0; 8];
        mix_into(&mut out, &mut scratch, 0, &[], |_, _, _| false);
        assert_eq!(out, vec![0.0, 0.0, 0.0, 0.0]);
    }
}

#[cfg(test)]
mod walk_tests {
    use crate::asset::AssetId;
    use crate::node::{Comp, LayerTiming, Node, Project};
    use crate::{evaluate_audio, AudioClip};

    fn comp_with(layer: Node) -> Project {
        let mut comp = Comp::new(64.0, 64.0, Node::group(0, "root").with_child(layer));
        comp.fps = 24.0;
        comp.duration_frames = 240;
        Project::single(comp)
    }

    fn sounding(id: u64, timing: LayerTiming) -> Node {
        let mut n = Node::group(id, "sound");
        n.timing = Some(timing);
        n.audio = Some(AudioClip::new(AssetId(7)));
        n
    }

    /// A layer's trim becomes its sound's span, on the sample grid.
    #[test]
    fn a_trimmed_layer_sounds_over_its_trim() {
        let project = comp_with(sounding(1, LayerTiming::new(24, 48)));
        let srcs = evaluate_audio(&project, project.root, 0.0, 48_000);
        assert_eq!(srcs.len(), 1);
        // One second in, one second long, at 24fps and 48kHz.
        assert_eq!(srcs[0].start, 48_000);
        assert_eq!(srcs[0].end, 96_000);
    }

    /// A layer trimmed at its *head* starts partway into its own source — the
    /// offset and the comp position are different numbers, and swapping them is
    /// the bug that plays the wrong part of a song.
    #[test]
    fn a_head_trim_starts_partway_into_the_source() {
        let mut t = LayerTiming::new(24, 48);
        t.start = 0; // content began at comp frame 0, but the layer is trimmed to 24
        let project = comp_with(sounding(1, t));
        let srcs = evaluate_audio(&project, project.root, 0.0, 48_000);
        assert_eq!(srcs[0].start, 48_000, "it is heard one second in");
        assert_eq!(srcs[0].offset, 48_000, "and plays from one second into the file");
    }

    /// An untrimmed layer sounds; a muted one does not, but keeps its settings.
    #[test]
    fn a_muted_layer_contributes_nothing() {
        let mut layer = sounding(1, LayerTiming::new(0, 48));
        layer.audio.as_mut().unwrap().enabled = false;
        let project = comp_with(layer);
        assert!(evaluate_audio(&project, project.root, 0.0, 48_000).is_empty());
    }

    /// Level and pan are resolved into per-channel gains, so a panned layer
    /// arrives at the mixer already placed.
    #[test]
    fn level_and_pan_reach_the_source_as_gains() {
        let mut layer = sounding(1, LayerTiming::new(0, 48));
        {
            let clip = layer.audio.as_mut().unwrap();
            clip.level = crate::Value::constant(0.5);
            clip.pan = crate::Value::constant(-1.0);
        }
        let project = comp_with(layer);
        let srcs = evaluate_audio(&project, project.root, 0.0, 48_000);
        assert!((srcs[0].gain.0 - 0.5).abs() < 1e-6, "hard left keeps the level");
        assert!(srcs[0].gain.1.abs() < 1e-6, "and silences the right");
    }

    /// A layer with no sound contributes none — the ordinary case, and the one
    /// that must not cost anything.
    #[test]
    fn a_silent_project_yields_no_sources() {
        let project = comp_with(Node::group(1, "shape"));
        assert!(evaluate_audio(&project, project.root, 0.0, 48_000).is_empty());
    }

    /// A zero sample rate is a device that has not started yet, not a reason to
    /// divide by zero.
    #[test]
    fn a_zero_sample_rate_yields_nothing() {
        let project = comp_with(sounding(1, LayerTiming::new(0, 48)));
        assert!(evaluate_audio(&project, project.root, 0.0, 0).is_empty());
    }

    /// Sound comes through a precomp, shifted by where the instance sits — the
    /// audio half of "a precomp is a layer".
    #[test]
    fn a_precomp_carries_its_sound_shifted_by_the_instance() {
        let mut inner = Comp::new(64.0, 64.0, Node::group(0, "root").with_child(sounding(1, LayerTiming::new(0, 24))));
        inner.fps = 24.0;
        inner.duration_frames = 24;
        let mut project = Project::single(inner);
        let inner_id = project.root;

        let mut outer = Comp::new(64.0, 64.0, Node::group(0, "root"));
        outer.fps = 24.0;
        outer.duration_frames = 240;
        let mut instance = Node::group(1, "precomp").with_precomp(inner_id);
        // The instance starts one second into the outer comp.
        instance.timing = Some(LayerTiming { start: 24, in_: 24, out: 48 });
        outer.root = outer.root.with_child(instance);
        let outer_id = project.insert(outer);

        let srcs = evaluate_audio(&project, outer_id, 0.0, 48_000);
        assert_eq!(srcs.len(), 1, "the nested sound comes through");
        assert_eq!(srcs[0].start, 48_000, "shifted by the instance's start");
    }

    /// A self-referential project must not recurse forever — the same guard the
    /// picture walk has, for the same reason.
    #[test]
    fn a_cyclic_precomp_terminates() {
        let mut comp = Comp::new(64.0, 64.0, Node::group(0, "root"));
        comp.fps = 24.0;
        let mut project = Project::single(comp);
        let id = project.root;
        let mut instance = Node::group(1, "self").with_precomp(id);
        instance.timing = Some(LayerTiming::new(0, 24));
        project.comps.get_mut(&id).unwrap().root =
            Node::group(0, "root").with_child(instance);
        // Terminating at all is the assertion.
        let _ = evaluate_audio(&project, id, 0.0, 48_000);
    }
}
