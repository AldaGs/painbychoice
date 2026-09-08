# 0021. Audio is the master clock

- **Status:** Accepted — implemented
- **Decided:** 2026-09-08

## Context

Playback read the wall clock: an `Instant` taken when the transport started,
elapsed seconds folded into the loop span, and the frame derived from that. For
a silent piece this is exactly right, and it was right for as long as PBC had
nothing to hear.

Sound breaks it, and not for the reason people expect. The problem is not
latency — a fixed offset is measurable and correctable. The problem is **rate**.
A sound card's clock is its own crystal, and a device that advertises 48000Hz
runs at 48000 plus or minus a few parts per million. The wall clock is a
different crystal with a different error. Two clocks started together therefore
diverge linearly, at something like a frame every few minutes.

Over a thirty-second cut nobody notices. Over a three-minute piece the picture
visibly slides off the music. Worst of all it slides *by a different amount on
every machine*, which makes it the kind of bug that arrives as "the video is out
of sync on my laptop but fine on yours" and cannot be reproduced by the person
who has to fix it.

Correcting for it is not possible from the wall-clock side. You cannot measure
the device's true rate without asking the device, and once you are asking the
device you have already conceded the argument.

## Decision

**When a composition has sound, the audio device is the time source.**

The device reports how many sample frames it has actually consumed; the frame
shown is derived from that count. The picture cannot drift from the sound,
because nothing else is telling it the time. If the device runs a little fast,
the entire piece runs a little fast — together, which is the only property that
matters.

**When a composition has no sound, the wall clock is the time source**, exactly
as before. There is nothing to stay in sync with, and a silent piece should not
depend on an audio device existing.

The fallback is not a special case bolted on; it is one of two branches in
`MasterClock::seconds`, and both return a position in seconds so that nothing
downstream — looping, the playhead, evaluation — knows or cares which clock it
is on.

Three conditions must all hold for audio to take over, and each is a way the
naive version fails:

1. **The comp actually has sound.** Otherwise there is nothing to follow.
2. **The stream is live.** A device that failed to open, or vanished when
   headphones were unplugged, would otherwise freeze the playhead at the last
   sample position it reported.
3. **The rate is non-zero.** A live stream that reports no rate would divide by
   zero at the exact moment playback begins.

## Consequences

- **Seeking moves both clocks.** The wall-clock anchor and the device's sample
  counter are set together, because a device that starts or stops *after* a seek
  must resume from where the playhead is rather than from where it had itself
  reached.
- **The mix is republished on every transport or document change.** Publishing
  too often costs a tree walk and some `Arc` clones; missing a publish means the
  sound and the picture disagree. The bias is deliberate.
- **The audio callback owns hard constraints the rest of the codebase does not.**
  It runs on the driver's realtime thread, where overrunning the deadline is an
  audible click rather than a slow frame. So it must not allocate, block, or
  decode. This is why the shared mix is an immutable snapshot read through a
  `try_lock` that is held only long enough to clone an `Arc` — and why
  `mix_into` takes a caller-provided scratch slice rather than returning a
  `Vec`. That signature exists for this constraint alone.
- **Looping is applied inside the callback**, because a device block can straddle
  the loop point, and half a block of silence at every lap is what a user would
  report as "it stutters when it repeats".
- **The clock holds no document and no `App`.** It is handed the facts it needs
  per call, which is what makes the inversion, the fallback and the loop folding
  testable with no device, no window and no project — the same discipline as
  `evaluate` being pure.
- **Not yet inverted: export.** A render has no device and no realtime deadline;
  it walks frames and mixes the matching sample range. The two paths share the
  mixer and the document walk, and differ only in who says what time it is,
  which is the correct place for them to differ.
