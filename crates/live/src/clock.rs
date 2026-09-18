//! The master clock: what time it is, and who gets to say.
//!
//! # The inversion
//!
//! Playback used to read the wall clock and derive a frame from it. That is
//! correct for a silent piece and wrong the moment there is sound, because a
//! sound card does not run at wall-clock speed. Its crystal is close to
//! nominal and not equal to it — a "48000Hz" device is typically 48000 ± a few
//! parts per million — so two clocks that start together drift apart at a rate
//! of roughly a frame every few minutes. Over a thirty-second cut nobody sees
//! it; over a three-minute piece the picture visibly slides off the music, and
//! it slides *differently on every machine*, which is the worst kind of bug to
//! be told about.
//!
//! So when a comp has sound, **audio becomes the time source**: the device
//! reports how many sample frames it has actually consumed, and the frame shown
//! is derived from that. The picture then cannot drift from the sound, because
//! it is no longer being told the time by anything else. If the device runs
//! slightly fast, the whole piece runs slightly fast, together.
//!
//! When a comp has no sound there is nothing to sync to and the wall clock is
//! the right answer, so [`MasterClock`] falls back to it. Both paths produce
//! the same kind of value — a position in seconds — so nothing downstream knows
//! which clock it is on.
//!
//! # Why the position is an atomic
//!
//! The audio callback runs on a realtime thread owned by the driver. It must
//! not lock, allocate, or block: overrunning its deadline is an audible click,
//! not a slow frame. So it publishes its progress with a single relaxed atomic
//! store and the UI thread reads it with a single relaxed load. No mutex, no
//! channel, and no ordering requirement beyond "eventually visible" — a UI that
//! reads a sample position a few microseconds stale draws the same frame.

use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::Arc;
use std::time::Instant;

/// Where the current time comes from.
///
/// Shared with the audio callback through an `Arc`, which is why the position
/// lives behind an atomic rather than in `App`.
#[derive(Clone, Debug, Default)]
pub(crate) struct AudioPosition {
    /// Sample frames the device has consumed since playback started.
    ///
    /// Written only by the audio callback, read only by the UI. `i64` rather
    /// than `u64` because the mixer's positions are signed — a layer can start
    /// before zero — and one signed type across the audio path is worth more
    /// than the extra range.
    position: Arc<AtomicI64>,
    /// Whether the stream is actually running. A device can fail to start, or
    /// vanish when headphones are unplugged, and a clock that kept reporting the
    /// last sample position forever would freeze the playhead.
    live: Arc<AtomicBool>,
}

impl AudioPosition {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Called from the **audio callback**. Must stay allocation-free and
    /// lock-free; see the module docs.
    pub(crate) fn advance(&self, sample_frames: i64) {
        self.position.fetch_add(sample_frames, Ordering::Relaxed);
    }

    /// Called from the UI thread when the playhead moves, so the clock restarts
    /// from the new place instead of jumping back to where the device had got
    /// to.
    pub(crate) fn set(&self, sample_frames: i64) {
        self.position.store(sample_frames, Ordering::Relaxed);
    }

    pub(crate) fn get(&self) -> i64 {
        self.position.load(Ordering::Relaxed)
    }

    pub(crate) fn set_live(&self, live: bool) {
        self.live.store(live, Ordering::Relaxed);
    }

    pub(crate) fn is_live(&self) -> bool {
        self.live.load(Ordering::Relaxed)
    }
}

/// Which clock a reading came from — surfaced so the UI can say so, and so a
/// test can assert the fallback actually happened rather than inferring it from
/// a number.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ClockSource {
    /// Audio is running and is the time source.
    Audio,
    /// No sound, or no working device: the wall clock, as it always was.
    Wall,
}

/// The playback clock.
///
/// Deliberately holds no document and no `App`: it is handed the facts it needs
/// per call, so the whole of it is testable without a device, a window, or a
/// project.
#[derive(Clone, Debug)]
pub(crate) struct MasterClock {
    pub(crate) audio: AudioPosition,
    /// The device's rate. `0` when no stream has started.
    pub(crate) sample_rate: u32,
}

impl MasterClock {
    pub(crate) fn new() -> Self {
        MasterClock { audio: AudioPosition::new(), sample_rate: 0 }
    }

    /// Whether audio is currently driving time.
    ///
    /// Requires a running stream *and* a rate to divide by. Both, because a
    /// stream that started and then reported no rate would otherwise produce a
    /// division by zero at the exact moment playback begins.
    pub(crate) fn source(&self, comp_has_audio: bool) -> ClockSource {
        if comp_has_audio && self.audio.is_live() && self.sample_rate > 0 {
            ClockSource::Audio
        } else {
            ClockSource::Wall
        }
    }

    /// The current playback position in seconds, before looping is applied.
    ///
    /// `wall` is what the wall clock would have said — passed in rather than
    /// read here so the fallback path stays a pure function of its inputs.
    pub(crate) fn seconds(&self, comp_has_audio: bool, wall: f64) -> f64 {
        match self.source(comp_has_audio) {
            ClockSource::Audio => self.audio.get() as f64 / self.sample_rate as f64,
            ClockSource::Wall => wall,
        }
    }
}

impl Default for MasterClock {
    fn default() -> Self {
        Self::new()
    }
}

/// Fold a position in seconds into a loop span, and report how many times it
/// wrapped.
///
/// The wrap count is what the audio side needs that the picture side never did:
/// when the playhead loops, the device's sample counter has to be pulled back to
/// the loop start, or the next block would be mixed from a position past the end
/// of the loop and play silence. Returning the count rather than only the folded
/// time is what lets the caller notice a loop happened at all.
pub(crate) fn fold_into_loop(t: f64, lo: f64, hi: f64) -> (f64, i64) {
    let span = hi - lo;
    if !(span > 0.0) || !t.is_finite() {
        return (lo, 0);
    }
    let rel = t - lo;
    let laps = (rel / span).floor();
    (lo + (rel - laps * span), laps as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn with_no_audio_the_wall_clock_wins() {
        let clock = MasterClock::new();
        assert_eq!(clock.source(false), ClockSource::Wall);
        assert_eq!(clock.seconds(false, 1.25), 1.25);
    }

    /// A comp with sound still falls back while the stream is not running —
    /// during startup, and after a device disappears. A clock that waited for a
    /// dead device would freeze the playhead.
    #[test]
    fn a_comp_with_sound_falls_back_until_the_stream_is_live() {
        let mut clock = MasterClock::new();
        clock.sample_rate = 48_000;
        assert_eq!(clock.source(true), ClockSource::Wall, "not started yet");
        clock.audio.set_live(true);
        assert_eq!(clock.source(true), ClockSource::Audio);
        clock.audio.set_live(false);
        assert_eq!(clock.source(true), ClockSource::Wall, "the device went away");
    }

    /// A live stream that reports no rate must not become the time source: the
    /// division would be by zero at the moment playback starts.
    #[test]
    fn a_live_stream_with_no_rate_is_not_the_time_source() {
        let clock = MasterClock::new();
        clock.audio.set_live(true);
        assert_eq!(clock.source(true), ClockSource::Wall);
    }

    /// **The inversion.** With audio live, time comes from the samples the
    /// device consumed, and the wall clock is ignored entirely — including when
    /// the two disagree, which is the whole point.
    #[test]
    fn time_comes_from_the_samples_played_not_the_wall() {
        let mut clock = MasterClock::new();
        clock.sample_rate = 48_000;
        clock.audio.set_live(true);
        clock.audio.set(24_000);
        assert!((clock.seconds(true, 999.0) - 0.5).abs() < 1e-9, "half a second of samples");
    }

    /// The callback accumulates; the UI reads the total. This is the whole
    /// contract between the two threads.
    #[test]
    fn the_callback_accumulates_and_the_ui_reads_it() {
        let mut clock = MasterClock::new();
        clock.sample_rate = 48_000;
        clock.audio.set_live(true);
        for _ in 0..10 {
            clock.audio.advance(4_800);
        }
        assert_eq!(clock.audio.get(), 48_000);
        assert!((clock.seconds(true, 0.0) - 1.0).abs() < 1e-9);
    }

    /// Seeking pulls the device's counter to the new place rather than letting
    /// the clock jump back to wherever playback had reached.
    #[test]
    fn seeking_resets_the_sample_position() {
        let clock = MasterClock::new();
        clock.audio.advance(100_000);
        clock.audio.set(0);
        assert_eq!(clock.audio.get(), 0);
    }

    #[test]
    fn folding_leaves_a_position_inside_its_span_alone() {
        let (t, laps) = fold_into_loop(1.5, 1.0, 3.0);
        assert!((t - 1.5).abs() < 1e-12);
        assert_eq!(laps, 0);
    }

    /// The wrap count is the part the audio side needs: without it the caller
    /// cannot know to pull the device's counter back to the loop start.
    #[test]
    fn folding_reports_how_many_times_it_wrapped() {
        let (t, laps) = fold_into_loop(7.5, 1.0, 3.0);
        assert!((t - 1.5).abs() < 1e-12, "7.5 folds into [1,3) at 1.5, got {t}");
        assert_eq!(laps, 3);
    }

    /// A position before the span wraps backwards rather than clamping — the
    /// same behaviour as the picture side's `wrap_into`.
    #[test]
    fn folding_wraps_backwards_too() {
        let (t, laps) = fold_into_loop(0.5, 1.0, 3.0);
        assert!((t - 2.5).abs() < 1e-12, "got {t}");
        assert_eq!(laps, -1);
    }

    /// A degenerate span is a work area dragged to nothing; it must not divide
    /// by zero or hang.
    #[test]
    fn a_degenerate_span_folds_to_its_start() {
        assert_eq!(fold_into_loop(5.0, 2.0, 2.0), (2.0, 0));
        assert_eq!(fold_into_loop(5.0, 3.0, 1.0), (3.0, 0));
        assert_eq!(fold_into_loop(f64::NAN, 0.0, 2.0), (0.0, 0));
    }
}
