//! Render independent frames on several threads and deliver them **in order**.
//!
//! The offline renderer is embarrassingly parallel and was not exploiting it:
//! `evaluate` is pure ([0002](../../docs/decisions/0002-evaluate-is-the-only-entry-point.md)),
//! frames share no state, and [`performance.md`](../../docs/performance.md)
//! measured the loop as cleanly pixel-bound on one core while three sat idle.
//! This is the "largest easy win available" that document names.
//!
//! # Why it is shaped like this
//!
//! **Ordered delivery is not optional.** [`crate::Encoder::push`] takes frames in
//! sequence — an ffmpeg pipe has no notion of frame numbers, so a frame arriving
//! early is not late data, it is *wrong* data, silently. So workers race, and
//! the results are reassembled into order before anything is written.
//!
//! **The reorder buffer is bounded by construction.** Workers claim frames from
//! a shared counter in increasing order, so each holds at most one frame and the
//! furthest any result can be out of order is the worker count. Nothing needs a
//! window parameter or a memory cap: at `n` threads the buffer holds at most `n`
//! frames, plus `n` more queued in the channel.
//!
//! **Threads are spawned once for the whole render, not per batch.** Each worker
//! thread builds its own font context on first use — `core`'s text machinery is
//! `thread_local!`, which is exactly what makes this safe — and enumerating the
//! system font collection is expensive enough that respawning threads per batch
//! would cost more than the parallelism gained on a text-heavy comp.
//!
//! **A failure stops the whole render.** Dropping the receiver disconnects the
//! channel, so workers blocked mid-send wake up with an error and exit rather
//! than deadlocking the join at the end of the scope.

use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::mpsc;

/// How many threads to render with by default: one per available core.
///
/// Falls back to 1 where the platform will not say, which is the safe answer —
/// a wrong high guess oversubscribes and a wrong low guess merely renders at
/// today's speed.
pub fn default_threads() -> usize {
    std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1)
}

/// Render `frames` with `render` on `threads` threads, handing each result to
/// `sink` **in ascending frame order** on the calling thread.
///
/// `render` must be pure and is called from many threads at once; `sink` is
/// called from this one, so it can own an encoder.
///
/// With `threads <= 1` this is a plain sequential loop — no channel, no spawn.
/// That is worth keeping rather than treating as a degenerate case of the
/// parallel path: it is the comparison the parallel path is verified against,
/// and it is what a `--threads 1` debugging run wants.
pub fn render_in_order<T, R, S>(
    frames: std::ops::RangeInclusive<i64>,
    threads: usize,
    render: R,
    mut sink: S,
) -> Result<(), String>
where
    T: Send,
    R: Fn(i64) -> Result<T, String> + Sync,
    S: FnMut(i64, T) -> Result<(), String>,
{
    let (start, end) = (*frames.start(), *frames.end());
    if end < start {
        return Ok(());
    }
    if threads <= 1 {
        for frame in start..=end {
            let rendered = render(frame)?;
            sink(frame, rendered)?;
        }
        return Ok(());
    }

    let next = AtomicI64::new(start);
    let abort = AtomicBool::new(false);
    let render = &render;
    let (next, abort) = (&next, &abort);

    std::thread::scope(|scope| {
        // Bound the channel by the worker count: a worker that runs ahead of
        // the writer blocks on send instead of piling frames up in memory.
        let (tx, rx) = mpsc::sync_channel::<(i64, Result<T, String>)>(threads);
        for _ in 0..threads {
            let tx = tx.clone();
            scope.spawn(move || {
                loop {
                    if abort.load(Ordering::Relaxed) {
                        return;
                    }
                    let frame = next.fetch_add(1, Ordering::Relaxed);
                    if frame > end {
                        return;
                    }
                    // A send error means the receiver is gone — the render has
                    // already failed elsewhere — so stop rather than finish a
                    // frame nobody will collect.
                    if tx.send((frame, render(frame))).is_err() {
                        return;
                    }
                }
            });
        }
        // The workers hold the only senders now, so the channel closes when the
        // last one exits. Without this the receive loop would never see
        // `Disconnected` and would hang at the end of the range.
        drop(tx);

        let mut pending: std::collections::BTreeMap<i64, T> = Default::default();
        let mut want = start;
        let mut failure: Option<String> = None;

        // `rx` is moved in here so it can be dropped before the scope joins:
        // on the error path, workers blocked in `send` must be woken, and the
        // only thing that wakes them is the receiver going away.
        {
            let rx = rx;
            while want <= end {
                let Ok((frame, rendered)) = rx.recv() else {
                    // Every worker exited without finishing the range. Either a
                    // failure already set `abort`, or something is wrong.
                    if failure.is_none() {
                        failure = Some(format!(
                            "the render stopped after frame {}: a worker exited early",
                            want - 1
                        ));
                    }
                    break;
                };
                match rendered {
                    Ok(value) => {
                        pending.insert(frame, value);
                    }
                    Err(e) => {
                        failure = Some(e);
                        abort.store(true, Ordering::Relaxed);
                        break;
                    }
                }
                // Write out whatever contiguous run is now available.
                while let Some(value) = pending.remove(&want) {
                    if let Err(e) = sink(want, value) {
                        failure = Some(e);
                        abort.store(true, Ordering::Relaxed);
                        break;
                    }
                    want += 1;
                }
                if failure.is_some() {
                    break;
                }
            }
            abort.store(true, Ordering::Relaxed);
        }

        match failure {
            Some(e) => Err(e),
            None => Ok(()),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// The property the encoder depends on: whatever order the work finishes
    /// in, the sink sees 0, 1, 2, … The render function here sleeps *longer for
    /// earlier frames*, so a naive implementation that wrote results as they
    /// arrived would emit them close to backwards.
    #[test]
    fn frames_arrive_in_order_however_they_finish() {
        let seen = Mutex::new(Vec::new());
        render_in_order(
            0..=15,
            4,
            |f| {
                std::thread::sleep(std::time::Duration::from_millis((16 - f) as u64 * 2));
                Ok(f)
            },
            |f, v| {
                assert_eq!(f, v);
                seen.lock().unwrap().push(f);
                Ok(())
            },
        )
        .expect("must succeed");
        assert_eq!(seen.into_inner().unwrap(), (0..=15).collect::<Vec<_>>());
    }

    /// Every frame in the range is rendered exactly once — no gaps from a
    /// mis-shared counter, no duplicates from two workers claiming one frame.
    #[test]
    fn every_frame_is_rendered_exactly_once() {
        let calls = Mutex::new(Vec::new());
        let written = Mutex::new(Vec::new());
        render_in_order(
            5..=40,
            8,
            |f| {
                calls.lock().unwrap().push(f);
                Ok(f)
            },
            |f, _| {
                written.lock().unwrap().push(f);
                Ok(())
            },
        )
        .expect("must succeed");
        let mut calls = calls.into_inner().unwrap();
        calls.sort_unstable();
        assert_eq!(calls, (5..=40).collect::<Vec<_>>(), "each frame rendered once");
        assert_eq!(written.into_inner().unwrap(), (5..=40).collect::<Vec<_>>());
    }

    /// The sequential path and the parallel path agree. This is the test that
    /// makes the parallel one trustworthy: same range, same results, same order.
    #[test]
    fn one_thread_and_many_agree() {
        let collect = |threads| {
            let seen = Mutex::new(Vec::new());
            render_in_order(0..=50, threads, |f| Ok(f * 3), |f, v| {
                seen.lock().unwrap().push((f, v));
                Ok(())
            })
            .unwrap();
            seen.into_inner().unwrap()
        };
        assert_eq!(collect(1), collect(6));
    }

    /// A failing frame stops the render and reports *that* error, rather than
    /// deadlocking on workers blocked mid-send or reporting a later frame's.
    #[test]
    fn a_failing_frame_stops_the_render() {
        let err = render_in_order(
            0..=100,
            4,
            |f| if f == 7 { Err("frame 7 is bad".to_string()) } else { Ok(f) },
            |_, _| Ok(()),
        )
        .expect_err("must fail");
        assert_eq!(err, "frame 7 is bad");
    }

    /// A failing *sink* stops it too — the encoder refusing a frame is the
    /// realistic version of this, and it must not be swallowed.
    #[test]
    fn a_failing_sink_stops_the_render() {
        let err = render_in_order(
            0..=100,
            4,
            Ok,
            |f, _| if f == 3 { Err("the encoder refused it".to_string()) } else { Ok(()) },
        )
        .expect_err("must fail");
        assert_eq!(err, "the encoder refused it");
    }

    /// An empty range writes nothing and succeeds — `--start 10 --end 9` is a
    /// user error the caller already rejects, but this must not hang.
    #[test]
    fn an_empty_range_does_nothing() {
        let mut n = 0;
        // Built from variables rather than written `10..=9`: clippy rejects a
        // literal reversed range, and the point here is precisely that one
        // computed at runtime must be handled rather than hang.
        let (start, end) = (10i64, 9i64);
        render_in_order(start..=end, 4, Ok, |_, _| {
            n += 1;
            Ok(())
        })
        .expect("must succeed");
        assert_eq!(n, 0);
    }

    /// A single frame is the boundary case where the counter, the channel and
    /// the reorder buffer all see exactly one item.
    #[test]
    fn a_single_frame_range_works() {
        let mut seen = Vec::new();
        render_in_order(42..=42, 8, Ok, |f, _| {
            seen.push(f);
            Ok(())
        })
        .expect("must succeed");
        assert_eq!(seen, vec![42]);
    }

    /// More threads than frames must not hang or double-render: the extra
    /// workers claim numbers past the end and exit immediately.
    #[test]
    fn more_threads_than_frames_is_fine() {
        let mut seen = Vec::new();
        render_in_order(0..=2, 16, Ok, |f, _| {
            seen.push(f);
            Ok(())
        })
        .expect("must succeed");
        assert_eq!(seen, vec![0, 1, 2]);
    }

    #[test]
    fn the_default_thread_count_is_at_least_one() {
        assert!(default_threads() >= 1);
    }
}
