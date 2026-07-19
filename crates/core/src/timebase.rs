//! The frame grid: the one place that knows what a frame is worth in seconds.
//!
//! `core` is moving to frames as its native time domain — a `Track` is a
//! function of frames, and only the composition knows the wall-clock meaning of
//! one. Seconds are a *presentation* unit, converted at the edges through a
//! `Timebase`. Keeping every conversion here means fps never has to be threaded
//! into the value engine, and changing fps moves the wall-clock timing of a
//! document without silently drifting its keyframes off their frames.

use serde::{Deserialize, Serialize};

/// A frame grid defined by a frame rate.
///
/// Fractional rates (29.97, 23.976) are allowed for wall-clock conversion, but
/// the *timecode* frame field counts in whole frames per second — see
/// [`Timebase::nominal_fps`].
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Timebase {
    fps: f64,
}

impl Default for Timebase {
    fn default() -> Self {
        Self::new(60.0)
    }
}

impl Timebase {
    /// A timebase at `fps`. Non-finite or non-positive rates fall back to 1.0
    /// rather than poisoning every later conversion with NaN/∞.
    pub fn new(fps: f64) -> Self {
        let fps = if fps.is_finite() && fps > 0.0 { fps } else { 1.0 };
        Self { fps }
    }

    pub fn fps(&self) -> f64 {
        self.fps
    }

    /// The frame rate rounded to whole frames, for timecode's `ff` field and
    /// for frame-count ceilings. 29.97 counts as 30 frames per timecode second,
    /// which is what non-drop-frame timecode does.
    pub fn nominal_fps(&self) -> u32 {
        self.fps.round().max(1.0) as u32
    }

    /// Seconds → exact (possibly fractional) frame position. Fractional frames
    /// are meaningful: playback runs off a wall clock and lands between frames.
    pub fn seconds_to_frames_exact(&self, seconds: f64) -> f64 {
        seconds * self.fps
    }

    /// Seconds → the nearest whole frame. This is the snap: 5s @ 24fps = 120.
    pub fn seconds_to_frames(&self, seconds: f64) -> i64 {
        self.seconds_to_frames_exact(seconds).round() as i64
    }

    /// Frame position → seconds. Takes an `f64` so it accepts both a whole
    /// frame and a fractional playhead.
    pub fn frames_to_seconds(&self, frames: f64) -> f64 {
        frames / self.fps
    }

    /// Snap a fractional frame position to the grid.
    pub fn snap(&self, frames: f64) -> i64 {
        frames.round() as i64
    }

    /// The duration of one frame, in seconds. The ←/→ step.
    pub fn frame_duration(&self) -> f64 {
        1.0 / self.fps
    }

    /// Format a frame position as `hh:mm:ss.ff` (non-drop-frame).
    ///
    /// The `ff` field counts whole frames within the second and so is always in
    /// `0..nominal_fps`. Negative positions are formatted with a leading `-`.
    pub fn timecode(&self, frames: f64) -> String {
        let total = frames.round() as i64;
        let sign = if total < 0 { "-" } else { "" };
        let total = total.unsigned_abs();

        let per_second = self.nominal_fps() as u64;
        let ff = total % per_second;
        let total_seconds = total / per_second;
        let ss = total_seconds % 60;
        let mm = (total_seconds / 60) % 60;
        let hh = total_seconds / 3600;

        format!("{sign}{hh:02}:{mm:02}:{ss:02}.{ff:02}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seconds_round_trip_through_frames() {
        let tb = Timebase::new(24.0);
        assert_eq!(tb.seconds_to_frames(5.0), 120, "5s @ 24fps is 120 frames");
        assert!((tb.frames_to_seconds(120.0) - 5.0).abs() < 1e-12);
    }

    #[test]
    fn seconds_to_frames_rounds_to_nearest() {
        let tb = Timebase::new(24.0);
        // Just under and just over a frame boundary both snap to it.
        assert_eq!(tb.seconds_to_frames(1.0 / 24.0 * 0.6), 1);
        assert_eq!(tb.seconds_to_frames(1.0 / 24.0 * 1.4), 1);
        // The exact form keeps the fraction the snap throws away.
        assert!((tb.seconds_to_frames_exact(0.5) - 12.0).abs() < 1e-12);
    }

    #[test]
    fn timecode_fields_roll_over() {
        let tb = Timebase::new(24.0);
        assert_eq!(tb.timecode(0.0), "00:00:00.00");
        assert_eq!(tb.timecode(23.0), "00:00:00.23");
        assert_eq!(tb.timecode(24.0), "00:00:01.00", "24 frames @ 24fps is one second");
        assert_eq!(tb.timecode(120.0), "00:00:05.00");
        assert_eq!(tb.timecode(24.0 * 60.0), "00:01:00.00");
        assert_eq!(tb.timecode(24.0 * 3600.0), "01:00:00.00");
    }

    #[test]
    fn timecode_frame_field_never_reaches_fps() {
        // The ff field must stay in 0..fps for every frame across a few seconds.
        let tb = Timebase::new(30.0);
        for f in 0..300 {
            let tc = tb.timecode(f as f64);
            let ff: u32 = tc.rsplit('.').next().unwrap().parse().unwrap();
            assert!(ff < 30, "frame {f} formatted {tc} with ff >= fps");
        }
    }

    #[test]
    fn fractional_fps_uses_nominal_for_timecode() {
        let tb = Timebase::new(29.97);
        assert_eq!(tb.nominal_fps(), 30);
        assert_eq!(tb.timecode(30.0), "00:00:01.00");
        // ...but wall-clock conversion keeps the true rate.
        assert!((tb.frames_to_seconds(30.0) - 30.0 / 29.97).abs() < 1e-12);
    }

    #[test]
    fn negative_positions_format_with_a_sign() {
        let tb = Timebase::new(24.0);
        assert_eq!(tb.timecode(-1.0), "-00:00:00.01");
        assert_eq!(tb.timecode(-24.0), "-00:00:01.00");
    }

    #[test]
    fn degenerate_fps_does_not_poison_conversions() {
        for bad in [0.0, -5.0, f64::NAN, f64::INFINITY] {
            let tb = Timebase::new(bad);
            assert!(tb.fps() > 0.0);
            assert!(tb.frames_to_seconds(10.0).is_finite(), "fps {bad} produced non-finite seconds");
        }
    }
}
