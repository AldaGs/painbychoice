//! Decoding sound: files in, interleaved stereo samples out.
//!
//! The audio half of [`crate::decode`], and it makes the opposite call about
//! *how* to decode, for a reason worth stating.
//!
//! Video decodes through an **ffmpeg sidecar** streaming forward, because a
//! frame is large, a render walks frames in order, and the process boundary buys
//! format coverage for almost nothing. Audio decodes **in-process with
//! symphonia** because playback is the opposite shape of problem: the device
//! asks for a few milliseconds at a time, on a realtime callback, and needs to
//! seek the moment somebody drags the playhead. Spawning a process per seek
//! would be audible.
//!
//! # Whole files, in memory, at their native rate
//!
//! A decoded sound is held **complete** rather than streamed. Sound is small
//! next to picture — three minutes of 48kHz stereo `f32` is about 69MB, which is
//! eight 4K frames — and having all of it addressable is what makes scrubbing
//! and looping instant. The cost is import latency and memory on a very long
//! file, which is written down here rather than discovered later.
//!
//! Rate conversion happens on **read**, not on import, so the stored samples are
//! always what the file actually contained. See [`Sound::read`] for the
//! interpolation and its honest limits.

use std::path::Path;

use motion_core::asset::{AssetMeta, DecodeError};
use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::codecs::CodecParameters;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;

/// A fully decoded sound: interleaved stereo `f32` at its own native rate.
///
/// Always **stereo**, whatever the file was. Downmixing at the edge means the
/// mixer, the clock and the exporter each deal with one channel layout instead
/// of every layout a file might have — the same reasoning that makes every
/// frame in this crate 8-bit RGBA.
#[derive(Clone, Debug, PartialEq)]
pub struct Sound {
    /// The file's own rate, not the project's.
    pub sample_rate: u32,
    /// Interleaved `[l, r, l, r, …]`, so `samples.len() == frames * 2`.
    pub samples: Vec<f32>,
}

impl Sound {
    /// Length in sample frames.
    pub fn frames(&self) -> usize {
        self.samples.len() / 2
    }

    pub fn seconds(&self) -> f64 {
        if self.sample_rate == 0 {
            return 0.0;
        }
        self.frames() as f64 / self.sample_rate as f64
    }

    /// Read `count` sample frames starting at `from`, resampled to `rate`.
    ///
    /// `from` and `count` are in the **caller's** rate, not this sound's, which
    /// is what lets a 44.1kHz file sit in a 48kHz project without anything above
    /// this line knowing. Out-of-range reads return silence rather than a short
    /// buffer or an error: running off the end of a sound is ordinary, not
    /// exceptional.
    ///
    /// The resampling is **linear interpolation**, and that is a deliberate
    /// floor rather than a considered choice. It is transparent when the rates
    /// match (the common case, and an exact copy — no arithmetic touches the
    /// samples), and audibly imperfect on a large ratio, where it will alias in
    /// the top octave. Good enough to edit and preview against; a windowed-sinc
    /// resampler is the upgrade, and the seam for it is this function alone.
    pub fn read(&self, from: i64, count: usize, rate: u32) -> Vec<f32> {
        let mut out = vec![0.0; count * 2];
        self.read_into(from, &mut out, rate);
        out
    }

    /// [`Sound::read`] into a caller-provided buffer.
    ///
    /// The form the audio callback uses: `read` allocates, and an allocation on
    /// the device's realtime thread is an audible click rather than a slow
    /// frame. `out` is interleaved stereo and its length decides how many sample
    /// frames are read.
    pub fn read_into(&self, from: i64, out: &mut [f32], rate: u32) {
        out.fill(0.0);
        let count = out.len() / 2;
        if self.sample_rate == 0 || rate == 0 || count == 0 {
            return;
        }
        let frames = self.frames() as i64;

        // Matched rates: a straight copy, so the overwhelmingly common case
        // costs no interpolation and is bit-exact.
        if rate == self.sample_rate {
            for i in 0..count as i64 {
                let src = from + i;
                if src < 0 || src >= frames {
                    continue;
                }
                out[i as usize * 2] = self.samples[src as usize * 2];
                out[i as usize * 2 + 1] = self.samples[src as usize * 2 + 1];
            }
            return;
        }

        let ratio = self.sample_rate as f64 / rate as f64;
        for i in 0..count {
            let pos = (from + i as i64) as f64 * ratio;
            let base = pos.floor();
            let frac = (pos - base) as f32;
            let a = base as i64;
            let b = a + 1;
            if a < 0 || a >= frames {
                continue;
            }
            // The last sample has no successor to interpolate towards; holding
            // it is correct and avoids reading past the end.
            let bi = if b >= frames { a } else { b };
            for ch in 0..2 {
                let s0 = self.samples[a as usize * 2 + ch];
                let s1 = self.samples[bi as usize * 2 + ch];
                out[i * 2 + ch] = s0 + (s1 - s0) * frac;
            }
        }
    }
}

/// Decode a whole sound file.
///
/// Handles any container and codec symphonia was built with; a file it cannot
/// read is a [`DecodeError`], never a panic. Channels are folded to stereo:
/// mono is duplicated to both sides, and anything wider keeps its first two
/// channels — a real downmix of 5.1 is a matrixing decision this has no business
/// making silently, and taking the front pair is the predictable answer.
pub fn decode_sound(path: &Path) -> Result<Sound, DecodeError> {
    // A missing file is its own variant: "the sound moved" is a relink, not a
    // corrupt file, and the two lead to different offers.
    let file = std::fs::File::open(path).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => DecodeError::Missing(path.to_path_buf()),
        _ => DecodeError::Malformed(format!("{}: {e}", path.display())),
    })?;
    let stream = MediaSourceStream::new(Box::new(file), Default::default());

    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }

    let mut format = symphonia::default::get_probe()
        .probe(&hint, stream, FormatOptions::default(), MetadataOptions::default())
        .map_err(|e| DecodeError::Malformed(format!("{}: {e}", path.display())))?;

    // The first track that actually carries audio parameters. A container can
    // hold video and subtitles too, and picking by index would sometimes hand
    // the decoder a subtitle stream.
    let (track_id, params) = format
        .tracks()
        .iter()
        .find_map(|t| match t.codec_params.as_ref() {
            Some(CodecParameters::Audio(a)) => Some((t.id, a.clone())),
            _ => None,
        })
        .ok_or_else(|| DecodeError::Malformed(format!("{}: no audio track", path.display())))?;

    let sample_rate = params.sample_rate.unwrap_or(0);
    if sample_rate == 0 {
        return Err(DecodeError::Malformed(format!(
            "{}: the audio track reports no sample rate",
            path.display()
        )));
    }

    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(&params, &AudioDecoderOptions::default())
        .map_err(|e| DecodeError::Malformed(format!("{}: {e}", path.display())))?;

    let mut samples: Vec<f32> = Vec::new();
    let mut scratch: Vec<f32> = Vec::new();
    loop {
        let packet = match format.next_packet() {
            Ok(Some(p)) => p,
            Ok(None) => break,
            // The end of the stream, however this container spells it.
            Err(symphonia::core::errors::Error::IoError(e))
                if e.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break
            }
            Err(symphonia::core::errors::Error::ResetRequired) => break,
            Err(e) => return Err(DecodeError::Malformed(format!("{}: {e}", path.display()))),
        };
        if packet.track_id != track_id {
            continue;
        }
        match decoder.decode(&packet) {
            Ok(decoded) => {
                scratch.clear();
                // symphonia converts whatever sample format the codec produced
                // into `f32` here, so this module never has to enumerate them.
                decoded.copy_to_vec_interleaved(&mut scratch);
                let channels = decoded.spec().channels().count().max(1);
                fold_to_stereo(&scratch, channels, &mut samples);
            }
            // A damaged packet mid-file is skipped rather than failing the
            // import: a sound with a glitch is more useful than no sound.
            Err(symphonia::core::errors::Error::DecodeError(_)) => continue,
            Err(e) => return Err(DecodeError::Malformed(format!("{}: {e}", path.display()))),
        }
    }

    Ok(Sound { sample_rate, samples })
}

/// Fold interleaved samples of any channel count down to stereo.
///
/// Mono is duplicated to both sides; anything wider keeps its **first two**
/// channels. A real 5.1 downmix is a matrixing decision with no obviously right
/// answer, and inventing one silently would be worse than taking the front pair
/// — which is at least predictable and is what the layer's own pan then acts on.
fn fold_to_stereo(interleaved: &[f32], channels: usize, out: &mut Vec<f32>) {
    if channels == 0 {
        return;
    }
    let frames = interleaved.len() / channels;
    out.reserve(frames * 2);
    for i in 0..frames {
        let l = interleaved[i * channels];
        let r = if channels > 1 { interleaved[i * channels + 1] } else { l };
        out.push(l);
        out.push(r);
    }
}

/// Read a sound file's metadata without decoding all of it.
///
/// The import path: an [`AssetMeta`] the document can cache, so nothing has to
/// reopen the file to know how long the sound is.
///
/// It decodes the file to count sample frames, because a container's declared
/// duration is frequently absent and occasionally a lie, and a wrong length here
/// becomes a layer of the wrong duration. Correct beats fast at import; the
/// result is cached in the `.pbc` and never recomputed.
pub fn probe_sound(path: &Path) -> Result<AssetMeta, DecodeError> {
    let sound = decode_sound(path)?;
    Ok(AssetMeta::sound(sound.sample_rate, 2, sound.frames() as u64))
}

/// Whether this path looks like something [`decode_sound`] should be handed.
///
/// Extension-driven, like the container table in `encode.rs`, and for the same
/// reason: deciding by sniffing means opening every file a user drops.
pub fn is_audio_file(path: &Path) -> bool {
    const AUDIO: [&str; 9] =
        ["wav", "mp3", "flac", "ogg", "oga", "m4a", "aac", "aiff", "aif"];
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| AUDIO.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(rate: u32, frames: usize) -> Sound {
        let mut samples = Vec::with_capacity(frames * 2);
        for i in 0..frames {
            let v = (i as f32 / frames as f32) * 2.0 - 1.0;
            samples.push(v);
            samples.push(-v);
        }
        Sound { sample_rate: rate, samples }
    }

    #[test]
    fn the_audio_extensions_are_recognised_case_insensitively() {
        assert!(is_audio_file(Path::new("music.mp3")));
        assert!(is_audio_file(Path::new("VOICE.WAV")));
        assert!(!is_audio_file(Path::new("clip.mp4")));
        assert!(!is_audio_file(Path::new("noext")));
    }

    /// Matched rates must be an exact copy: the common case has to be
    /// bit-transparent, or every project would be quietly resampled.
    #[test]
    fn a_matched_rate_read_is_exact() {
        let s = tone(48_000, 100);
        let got = s.read(10, 5, 48_000);
        assert_eq!(&got[..], &s.samples[20..30]);
    }

    /// Reading before the start or past the end is silence, not a short buffer
    /// — the caller asked for `count` frames and gets `count` frames.
    #[test]
    fn out_of_range_reads_are_silence_of_the_right_length() {
        let s = tone(48_000, 10);
        let before = s.read(-5, 4, 48_000);
        assert_eq!(before.len(), 8);
        assert!(before.iter().all(|v| *v == 0.0));
        let after = s.read(100, 4, 48_000);
        assert!(after.iter().all(|v| *v == 0.0));
    }

    /// A read straddling the end fills what exists and pads the rest.
    #[test]
    fn a_read_over_the_end_is_padded() {
        let s = tone(48_000, 10);
        let got = s.read(8, 4, 48_000);
        assert_eq!(got.len(), 8);
        assert_ne!(got[0], 0.0, "frame 8 exists");
        assert_eq!(&got[4..8], &[0.0, 0.0, 0.0, 0.0], "frames 10 and 11 do not");
    }

    /// Resampling changes the rate the caller reads in: a second of the caller's
    /// time must cover a second of the sound, whatever the file's own rate.
    #[test]
    fn resampling_maps_a_second_to_a_second() {
        // 100 frames at 100Hz is one second.
        let s = tone(100, 100);
        // Read one second at 50Hz: 50 frames spanning the whole sound.
        let got = s.read(0, 50, 50);
        assert_eq!(got.len(), 100);
        // The last frame read should be near the end of the sound, not halfway.
        let last_left = got[98];
        let expected = s.samples[98 * 2]; // frame 98 of 100
        assert!(
            (last_left - expected).abs() < 0.05,
            "reading 50 frames at half rate must span the whole sound: {last_left} vs {expected}"
        );
    }

    /// Interpolation lands between its neighbours rather than snapping — the
    /// property that distinguishes it from nearest-neighbour.
    #[test]
    fn interpolation_lands_between_neighbours() {
        let s = Sound { sample_rate: 2, samples: vec![0.0, 0.0, 1.0, 1.0] };
        // At the caller's 4Hz, frame 1 sits halfway between the two.
        let got = s.read(1, 1, 4);
        assert!((got[0] - 0.5).abs() < 1e-6, "expected the midpoint, got {}", got[0]);
    }

    /// A zero rate on either side is a device or a file that has not reported
    /// itself; silence beats dividing by zero.
    #[test]
    fn a_zero_rate_reads_silence() {
        let s = tone(48_000, 10);
        assert!(s.read(0, 4, 0).iter().all(|v| *v == 0.0));
        let broken = Sound { sample_rate: 0, samples: vec![1.0; 20] };
        assert!(broken.read(0, 4, 48_000).iter().all(|v| *v == 0.0));
    }

    #[test]
    fn a_sounds_length_is_in_sample_frames() {
        let s = tone(48_000, 24_000);
        assert_eq!(s.frames(), 24_000);
        assert!((s.seconds() - 0.5).abs() < 1e-9);
    }

    /// A file that is not audio fails rather than returning an empty sound —
    /// an empty sound would import as a silent layer and look like a bug in the
    /// mixer.
    #[test]
    fn a_non_audio_file_is_refused() {
        let dir = std::env::temp_dir().join(format!("pbc_snd_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("not.wav");
        std::fs::write(&path, b"this is not a wav file at all").unwrap();
        assert!(decode_sound(&path).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **A real file, decoded.** Written with ffmpeg when it is available, so
    /// this exercises the actual container and codec path rather than a
    /// hand-built buffer: a one-second 440Hz tone must come back as one second
    /// of non-silent stereo at the rate it was written.
    #[test]
    fn a_real_wav_decodes_to_the_right_length() {
        if !crate::encode::FfmpegEncoder::available() {
            return;
        }
        let dir = std::env::temp_dir().join(format!("pbc_wav_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tone.wav");
        // `volume=10` because ffmpeg's `sine` generator is quiet — about -21dB,
        // a peak of 0.088. Amplified to near full scale the peak below becomes a
        // real check on the integer-to-float scaling: a missing divide, or
        // 32767 where 32768 belongs, moves it.
        let ok = std::process::Command::new(crate::encode::ffmpeg_bin_for_tests())
            .args(["-v", "error", "-y", "-f", "lavfi", "-i", "sine=frequency=440:duration=1"])
            .args(["-af", "volume=10", "-ar", "48000", "-ac", "2"])
            .arg(&path)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !ok {
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }

        let sound = decode_sound(&path).expect("a real wav must decode");
        assert_eq!(sound.sample_rate, 48_000);
        assert!(
            (sound.frames() as i64 - 48_000).abs() < 1_000,
            "one second at 48kHz, got {} frames",
            sound.frames()
        );
        let peak = sound.samples.iter().fold(0.0f32, |a, b| a.max(b.abs()));
        assert!(
            (0.5..=1.0).contains(&peak),
            "a near-full-scale tone must decode near full scale, got a peak of {peak}"
        );
        // A 440Hz tone crosses zero 880 times a second. Checking that as well as
        // the peak is what separates "decoded the samples" from "decoded a
        // constant of about the right size".
        let crossings = sound
            .samples
            .chunks_exact(2)
            .map(|f| f[0])
            .collect::<Vec<_>>()
            .windows(2)
            .filter(|w| (w[0] < 0.0) != (w[1] < 0.0))
            .count();
        assert!(
            (700..=1100).contains(&crossings),
            "a 440Hz tone crosses zero ~880 times per second, counted {crossings}"
        );

        // And the metadata path agrees with the decode.
        let meta = probe_sound(&path).expect("probe");
        assert_eq!(meta.sample_rate, 48_000);
        assert_eq!(meta.samples, sound.frames() as u64);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// Write interleaved stereo `f32` as a 16-bit PCM WAV.
///
/// Hand-written rather than pulling in a WAV crate: the header is 44 bytes of
/// entirely specified layout, this is the only place that needs it, and
/// [0007](../../docs/decisions/0007-never-implement-codecs.md)'s "never
/// implement a codec" is about *compression*, not about a container that is a
/// length and some samples.
///
/// 16-bit because the destination is an ffmpeg mux that will re-encode to AAC
/// anyway, so the file exists for seconds and 32-bit float would double it for
/// no audible gain. Samples are clamped before scaling — a value outside
/// `[-1, 1]` would otherwise wrap and turn a loud passage into noise.
pub fn write_wav(path: &Path, samples: &[f32], sample_rate: u32) -> std::io::Result<()> {
    use std::io::Write;

    let channels: u16 = 2;
    let bits: u16 = 16;
    let byte_rate = sample_rate * channels as u32 * (bits / 8) as u32;
    let block_align = channels * (bits / 8);
    let data_len = (samples.len() * 2) as u32;

    let file = std::fs::File::create(path)?;
    let mut w = std::io::BufWriter::new(file);

    w.write_all(b"RIFF")?;
    w.write_all(&(36 + data_len).to_le_bytes())?;
    w.write_all(b"WAVE")?;
    w.write_all(b"fmt ")?;
    w.write_all(&16u32.to_le_bytes())?; // PCM chunk size
    w.write_all(&1u16.to_le_bytes())?; // format: PCM
    w.write_all(&channels.to_le_bytes())?;
    w.write_all(&sample_rate.to_le_bytes())?;
    w.write_all(&byte_rate.to_le_bytes())?;
    w.write_all(&block_align.to_le_bytes())?;
    w.write_all(&bits.to_le_bytes())?;
    w.write_all(b"data")?;
    w.write_all(&data_len.to_le_bytes())?;

    for s in samples {
        // Rounded, not truncated: `as i16` rounds toward zero, which biases
        // every sample inward by up to a full step and is a quantisation error
        // twice as large as it needs to be.
        //
        // 32767 rather than 32768 because the positive range of i16 stops one
        // short, and scaling by 32768 makes a full-scale sample wrap negative.
        let v = (s.clamp(-1.0, 1.0) * 32767.0).round() as i16;
        w.write_all(&v.to_le_bytes())?;
    }
    w.flush()
}

#[cfg(test)]
mod wav_tests {
    use super::*;

    #[test]
    fn a_written_wav_reads_back_as_what_went_in() {
        let dir = std::env::temp_dir().join(format!("pbc_wavw_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("out.wav");

        // A ramp, so a channel swap or an off-by-one in the header shows up.
        let mut samples = Vec::new();
        for i in 0..1000 {
            samples.push(i as f32 / 1000.0);
            samples.push(-(i as f32) / 1000.0);
        }
        write_wav(&path, &samples, 48_000).expect("write");

        let back = decode_sound(&path).expect("the wav we just wrote must decode");
        assert_eq!(back.sample_rate, 48_000);
        assert_eq!(back.frames(), 1000);
        // Two quantisations sit between what went in and what came back — the
        // f32→i16 write and symphonia's i16→f32 read, which do not agree on
        // whether the divisor is 32767 or 32768 — so the tolerance is two
        // steps, not one.
        for i in (0..2000).step_by(97) {
            assert!(
                (back.samples[i] - samples[i]).abs() < 2.0 / 32767.0,
                "sample {i}: wrote {} read {}",
                samples[i],
                back.samples[i]
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Full scale must stay full scale rather than wrapping to negative, which
    /// is what scaling by 32768 instead of 32767 would do.
    #[test]
    fn full_scale_does_not_wrap() {
        let dir = std::env::temp_dir().join(format!("pbc_wavf_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("loud.wav");
        write_wav(&path, &[1.0, -1.0, 2.0, -2.0], 48_000).expect("write");
        let back = decode_sound(&path).expect("decode");
        assert!(back.samples[0] > 0.99, "+1.0 stayed positive: {}", back.samples[0]);
        assert!(back.samples[1] < -0.99, "-1.0 stayed negative: {}", back.samples[1]);
        assert!(back.samples[2] > 0.99, "out-of-range clamped, not wrapped");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// Mix a comp's sound for a frame range into interleaved stereo.
///
/// The export counterpart of the playback callback, and deliberately *not*
/// realtime: this allocates the whole mix at once, because a render has no
/// deadline and a file is easier to reason about than a stream.
///
/// The range is inclusive, matching the renderer and `--start/--end`, and the
/// result is exactly as long as the picture — `end - start + 1` frames' worth of
/// samples — so the two streams cannot disagree about the length of the piece.
///
/// Automation is resolved **per block** rather than per frame, at
/// [`AUTOMATION_BLOCK`] sample frames, for the same reason the callback does it:
/// level and pan are control-rate parameters, and at ~21ms a block the
/// difference is inaudible while the saving is a whole tree walk per block
/// instead of per sample.
pub fn mix_comp(
    project: &motion_core::Project,
    comp: motion_core::CompId,
    start: i64,
    end: i64,
    sample_rate: u32,
    sounds: &std::collections::HashMap<motion_core::AssetId, Sound>,
) -> Vec<f32> {
    use motion_core::audio::frames_to_sample_frames as to_sf;

    let Some(c) = project.comp(comp) else { return Vec::new() };
    let fps = c.timebase().fps();
    if sample_rate == 0 || fps <= 0.0 || end < start {
        return Vec::new();
    }

    let from = to_sf(start as f64, fps, sample_rate);
    let to = to_sf((end + 1) as f64, fps, sample_rate);
    let total = (to - from).max(0) as usize;
    let mut out = vec![0.0f32; total * 2];
    let mut scratch = vec![0.0f32; AUTOMATION_BLOCK * 2];

    let mut done = 0usize;
    while done < total {
        let block = AUTOMATION_BLOCK.min(total - done);
        let origin = from + done as i64;
        // The frame this block starts on, for resolving level and pan.
        let at_frame = motion_core::audio::sample_frames_to_frames(origin, fps, sample_rate);
        let sources = motion_core::evaluate_audio(project, comp, at_frame, sample_rate);
        motion_core::mix_into(
            &mut out[done * 2..(done + block) * 2],
            &mut scratch,
            origin,
            &sources,
            |asset, at, buf| match sounds.get(&asset) {
                Some(sound) => {
                    sound.read_into(at, buf, sample_rate);
                    true
                }
                None => false,
            },
        );
        done += block;
    }
    out
}

/// How many sample frames share one resolution of level and pan.
///
/// 1024 at 48kHz is about 21ms. Small enough that a fade sounds continuous,
/// large enough that resolving the document per block is not the dominant cost
/// of a mixdown.
pub const AUTOMATION_BLOCK: usize = 1024;

#[cfg(test)]
mod mix_tests {
    use super::*;
    use motion_core::asset::AssetId;
    use motion_core::node::{Comp, LayerTiming, Node, Project};
    use motion_core::AudioClip;

    fn project_with_sound(frames: i64) -> (Project, std::collections::HashMap<AssetId, Sound>) {
        let mut layer = Node::group(1, "sound");
        layer.timing = Some(LayerTiming::new(0, frames));
        layer.audio = Some(AudioClip::new(AssetId(1)));
        let mut comp = Comp::new(64.0, 64.0, Node::group(0, "root").with_child(layer));
        comp.fps = 24.0;
        comp.duration_frames = frames;

        let mut sounds = std::collections::HashMap::new();
        sounds.insert(
            AssetId(1),
            Sound { sample_rate: 48_000, samples: vec![0.5; 48_000 * 2 * 4] },
        );
        (Project::single(comp), sounds)
    }

    /// **The mix is exactly as long as the picture.** A soundtrack that is even
    /// slightly the wrong length drifts against the video, and `-shortest`
    /// would silently truncate whichever lost.
    #[test]
    fn the_mix_is_exactly_as_long_as_the_frame_range() {
        let (project, sounds) = project_with_sound(48);
        // Frames 0..=23 at 24fps is one second.
        let mix = mix_comp(&project, project.root, 0, 23, 48_000, &sounds);
        assert_eq!(mix.len(), 48_000 * 2, "one second of stereo at 48kHz");
    }

    /// A partial range mixes only its own span, and starts from the right place
    /// in the sound rather than from zero.
    #[test]
    fn a_partial_range_mixes_only_itself() {
        let (project, sounds) = project_with_sound(96);
        let mix = mix_comp(&project, project.root, 24, 47, 48_000, &sounds);
        assert_eq!(mix.len(), 48_000 * 2, "one second");
        assert!(mix.iter().any(|s| *s != 0.0), "the second second is not silent");
    }

    /// A comp with no sound mixes to silence of the right length rather than to
    /// nothing — the video still needs a track of the right duration if one is
    /// requested at all.
    #[test]
    fn a_silent_comp_still_mixes_the_right_length() {
        let mut comp = Comp::new(64.0, 64.0, Node::group(0, "root"));
        comp.fps = 24.0;
        comp.duration_frames = 24;
        let project = Project::single(comp);
        let mix = mix_comp(&project, project.root, 0, 23, 48_000, &Default::default());
        assert_eq!(mix.len(), 48_000 * 2);
        assert!(mix.iter().all(|s| *s == 0.0));
    }

    /// The layer's gain reaches the mixdown, so a level of 0.5 on a 0.5 sound
    /// arrives at 0.25 times the centre-pan gain.
    #[test]
    fn the_layers_level_reaches_the_mix() {
        let (mut project, sounds) = project_with_sound(48);
        {
            let comp = project.comps.get_mut(&project.root).unwrap();
            let layer = comp.root.find_mut(motion_core::NodeId(1)).unwrap();
            layer.audio.as_mut().unwrap().level = motion_core::Value::constant(0.5);
        }
        let mix = mix_comp(&project, project.root, 0, 23, 48_000, &sounds);
        // 0.5 sample * 0.5 level * √½ centre pan.
        let expected = 0.5 * 0.5 * std::f32::consts::FRAC_1_SQRT_2;
        assert!(
            (mix[0] - expected).abs() < 1e-5,
            "expected {expected}, got {}",
            mix[0]
        );
    }

    /// A degenerate or reversed range mixes nothing rather than allocating a
    /// nonsense length.
    #[test]
    fn a_reversed_range_mixes_nothing() {
        let (project, sounds) = project_with_sound(48);
        assert!(mix_comp(&project, project.root, 20, 10, 48_000, &sounds).is_empty());
        assert!(mix_comp(&project, project.root, 0, 23, 0, &sounds).is_empty());
    }
}
