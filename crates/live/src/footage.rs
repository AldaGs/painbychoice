//! The decoded-frame cache: the one place in the editor that turns an
//! [`ImagePaint`] into actual pixels.
//!
//! This is the other half of the split `core` makes. The document holds
//! references and `evaluate` stays pure; everything that touches a file, costs
//! milliseconds, or has to be thrown away when memory runs short lives here, in
//! the shell — which is why the engine can still be tested by rendering a frame
//! in `cargo test`.
//!
//! # Why there is a thread in here
//!
//! Decoding runs on a worker, and the UI never waits for it. Two measurements
//! decided the shape of this module:
//!
//! - Re-opening the decoder per frame cost **~229ms**; reading frames from a
//!   stream left open costs **~1.5ms**. Decoding was never the expensive part —
//!   *starting* was. So the worker keeps one [`FrameStream`] alive and walks it
//!   forward, and only falls back to random access when the playhead jumps
//!   somewhere the stream can't reach.
//! - Even so, a seek costs a process restart. On the UI thread that is a
//!   visible freeze, so a miss returns the nearest frame already decoded and
//!   the real one arrives a moment later. The preview holds instead of
//!   stalling — which is what every editor does, and the reason scrubbing feels
//!   continuous rather than punishing.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender, TryRecvError};

use motion_core::asset::{
    Asset, AssetId, AssetMeta, DecodeError, DecoderRegistry, Frame, FrameStream, ImagePaint,
};
use vello::peniko::{Blob, ImageAlphaType, ImageData, ImageFormat};

/// How much decoded footage to keep resident, in bytes.
///
/// A budget rather than a frame count because frames differ in size by two
/// orders of magnitude — a hundred 4K frames is 3GB, a hundred icons is
/// nothing, and only one of those numbers is a sensible limit.
const BUDGET_BYTES: usize = 512 * 1024 * 1024;

/// How far ahead of the last request the worker will decode unprompted.
///
/// This is what makes playback smooth: by the time the playhead arrives, the
/// frame is already here. Bounded because a runaway prefetch would evict the
/// frames around the playhead to make room for frames nobody asked for.
const PREFETCH_AHEAD: i64 = 48;

/// How far the worker will walk a stream forward to reach a wanted frame
/// before giving up and re-opening it.
///
/// Walking is ~1.5ms/frame and re-opening is ~229ms, so stepping over a couple
/// of seconds of footage is still much cheaper than a seek. Past that, seeking
/// wins.
const MAX_WALK: i64 = 120;

/// One cached frame, with the clock reading that makes eviction possible.
struct Cached {
    image: ImageData,
    bytes: usize,
    /// When it was last drawn. A plain counter, not a timestamp: the only
    /// question ever asked of it is which entry is oldest.
    used: u64,
}

type Key = (AssetId, i64);

/// Which generation of the project a message belongs to.
///
/// Asset ids are only meaningful within one project, so a reply crossing a
/// project change describes footage its key no longer names. The epoch is what
/// lets those be recognised and dropped instead of being written in as this
/// project's pixels.
type Epoch = u64;

/// What the UI thread asks the worker for.
enum Request {
    Frame { epoch: Epoch, key: Key, path: PathBuf, meta: AssetMeta },
    /// Drop the open stream — the project changed under us.
    Reset,
    Stop,
}

/// What comes back. Unsolicited frames are normal: they are prefetch.
enum Response {
    Frame { epoch: Epoch, key: Key, result: Result<Frame, String> },
    /// The worker has stopped running ahead — it reached the end of the
    /// footage, hit an error, or caught up with the window it was given.
    ///
    /// Without this the UI could wait forever for a frame that is never
    /// coming: it deliberately does *not* re-request frames the prefetch has
    /// promised, so it has to be told when that promise lapses.
    PrefetchEnded { epoch: Epoch },
}

/// Frames the worker has already promised to deliver.
///
/// The prefetcher emits **every** frame from the last request up to `until`, so
/// anything in that range is already on its way. Asking for it again would be
/// worse than redundant: the stream has likely passed it, so the request reads
/// as a backwards seek and restarts the decoder — the exact ~229ms cost this
/// module exists to avoid.
struct Promised {
    asset: AssetId,
    /// The request that started this run. The prefetch only ever moves
    /// *forward*, so nothing before this is promised — a backwards seek has to
    /// be asked for, and pays for a fresh stream.
    from: i64,
    until: i64,
}

/// Decoded footage frames, keyed by source and frame number.
pub(crate) struct FootageCache {
    to_worker: Sender<Request>,
    from_worker: Receiver<Response>,
    worker: Option<std::thread::JoinHandle<()>>,
    frames: HashMap<Key, Cached>,
    /// Frames that failed to decode, so a broken file is attempted **once**
    /// rather than re-spawning a decoder sixty times a second. Cleared
    /// wholesale by [`FootageCache::clear`]; a per-asset retry arrives with
    /// relinking, which is the user saying "try again".
    failed: HashMap<Key, String>,
    /// Requested and not yet answered, so the same frame isn't queued once per
    /// redraw while the worker is busy with it.
    inflight: HashSet<Key>,
    /// What the running prefetch has already promised. See [`Promised`].
    promised: Option<Promised>,
    bytes: usize,
    clock: u64,
    epoch: Epoch,
}

impl FootageCache {
    pub(crate) fn new(decoders: DecoderRegistry) -> Self {
        let (to_worker, rx) = channel::<Request>();
        let (tx, from_worker) = channel::<Response>();
        let worker = std::thread::Builder::new()
            .name("footage-decode".into())
            .spawn(move || decode_loop(decoders, rx, tx))
            .ok();
        Self {
            to_worker,
            from_worker,
            worker,
            frames: HashMap::new(),
            failed: HashMap::new(),
            inflight: HashSet::new(),
            promised: None,
            bytes: 0,
            clock: 0,
            epoch: 0,
        }
    }

    /// Read a file's metadata, for import.
    ///
    /// The one decode that *is* synchronous, and rightly so: it happens once
    /// per import, behind a file dialog the user has just dismissed, and
    /// nothing can be shown until it answers.
    pub(crate) fn probe(&self, path: &Path) -> Result<AssetMeta, DecodeError> {
        // Built fresh rather than borrowed from the worker: the registry lives
        // over there now, and probing is rare enough that constructing a few
        // decoders costs nothing next to reading the file.
        motion_render::default_registry().open(path)
    }

    /// Take delivery of everything the worker has finished.
    ///
    /// Called once per redraw, before drawing. Frames arriving here include
    /// ones nobody asked for — that is prefetch working.
    pub(crate) fn collect(&mut self) {
        while let Ok(r) = self.from_worker.try_recv() {
            self.take(r);
        }
    }

    fn take(&mut self, r: Response) {
        match r {
            // From a project that is no longer open: this key names different
            // footage now, so keeping the pixels would draw the old project's
            // content under the new one's layers.
            Response::Frame { epoch, .. } | Response::PrefetchEnded { epoch }
                if epoch != self.epoch => {}
            Response::PrefetchEnded { .. } => self.promised = None,
            Response::Frame { key, result, .. } => {
                self.inflight.remove(&key);
                match result {
                    Ok(frame) => self.insert(key, frame),
                    Err(msg) => {
                        self.failed.insert(key, msg);
                    }
                }
            }
        }
    }

    /// Whether the running prefetch has already promised this frame.
    fn promised(&self, paint: ImagePaint) -> bool {
        self.promised.as_ref().is_some_and(|p| {
            p.asset == paint.asset
                && paint.source_frame >= p.from
                && paint.source_frame <= p.until
        })
    }

    /// Whether the worker still owes us anything — the UI keeps redrawing
    /// while this is true, so a frame that arrives late still gets shown.
    ///
    /// Includes the running prefetch, and must: frames it has promised are not
    /// re-requested, so if the UI stopped redrawing it would sit on a stale
    /// picture with nothing scheduled to replace it.
    pub(crate) fn is_busy(&self) -> bool {
        !self.inflight.is_empty() || self.promised.is_some()
    }

    /// Pixels to draw for one paint, and whether they are the frame actually
    /// asked for.
    ///
    /// A miss queues the decode and hands back the nearest frame already
    /// decoded for that footage, flagged `false`. Returning nothing would make
    /// the layer flash its fill colour every time the playhead moved somewhere
    /// new, which reads as flickering rather than as loading.
    ///
    /// `None` means there is genuinely nothing to show: no frame of this
    /// footage has ever decoded.
    pub(crate) fn image(&mut self, asset: &Asset, paint: ImagePaint) -> Option<(&ImageData, bool)> {
        let key = (paint.asset, paint.source_frame);
        self.clock += 1;

        let exact = self.frames.contains_key(&key);
        // Not asking for a frame the prefetch already promised is the whole
        // trick: the running stream has probably passed it, so a request would
        // read as a backwards seek and restart the decoder.
        if !exact
            && !self.failed.contains_key(&key)
            && !self.promised(paint)
            && self.inflight.insert(key)
        {
            // A dropped send is fine: the worker is gone only when the editor
            // is shutting down, and then nothing will be drawn again anyway.
            let _ = self.to_worker.send(Request::Frame {
                epoch: self.epoch,
                key,
                path: asset.path.clone(),
                meta: asset.meta(),
            });
            self.promised = Some(Promised {
                asset: paint.asset,
                from: paint.source_frame,
                until: paint.source_frame + PREFETCH_AHEAD,
            });
        }

        let hit = if exact { Some(key) } else { self.nearest(paint.asset, paint.source_frame) };
        let entry = self.frames.get_mut(&hit?)?;
        entry.used = self.clock;
        Some((&entry.image, exact))
    }

    /// The decoded frame of this footage closest to `want`.
    fn nearest(&self, asset: AssetId, want: i64) -> Option<Key> {
        self.frames
            .keys()
            .filter(|(a, _)| *a == asset)
            .min_by_key(|(_, f)| (f - want).abs())
            .copied()
    }

    /// Why this frame didn't decode, if it didn't.
    pub(crate) fn error(&self, paint: ImagePaint) -> Option<&str> {
        self.failed.get(&(paint.asset, paint.source_frame)).map(|s| s.as_str())
    }

    /// Drop everything, including the recorded failures and the worker's open
    /// stream.
    ///
    /// **Required when the open project changes.** Asset ids are per-project,
    /// so `AssetId(3)` in the file just loaded is a different piece of footage
    /// from `AssetId(3)` in the one being closed — keeping the cache would draw
    /// the old project's pixels under the new project's layers.
    pub(crate) fn clear(&mut self) {
        self.frames.clear();
        self.failed.clear();
        // Answers to requests already in flight describe footage that no longer
        // means what it did, so they must not be written in as this project's
        // frames. Bumping the epoch makes `collect` drop them.
        self.inflight.clear();
        self.promised = None;
        self.epoch += 1;
        self.bytes = 0;
        let _ = self.to_worker.send(Request::Reset);
    }

    fn insert(&mut self, key: Key, frame: Frame) {
        let bytes = frame.rgba.len();
        let image = ImageData {
            data: Blob::new(std::sync::Arc::new(frame.rgba)),
            format: ImageFormat::Rgba8,
            // Straight, not premultiplied: the decoders deliberately don't
            // premultiply, because a keyer needs the original colour behind a
            // transparent pixel.
            alpha_type: ImageAlphaType::Alpha,
            width: frame.width,
            height: frame.height,
        };
        if let Some(old) = self.frames.insert(key, Cached { image, bytes, used: self.clock }) {
            self.bytes -= old.bytes;
        }
        self.bytes += bytes;
        self.evict_to_budget();
    }

    /// Drop least-recently-drawn frames until the budget is met.
    ///
    /// Scrubbing a timeline touches frames in a sweep, so least-recently-used
    /// is the right policy almost by definition: the frames furthest behind the
    /// playhead are the ones least likely to be wanted next.
    fn evict_to_budget(&mut self) {
        while self.bytes > BUDGET_BYTES && self.frames.len() > 1 {
            let Some(oldest) = self.frames.iter().min_by_key(|(_, c)| c.used).map(|(k, _)| *k)
            else {
                break;
            };
            if let Some(c) = self.frames.remove(&oldest) {
                self.bytes -= c.bytes;
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn resident_bytes(&self) -> usize {
        self.bytes
    }

    /// Block until this frame is in hand (or known to have failed).
    ///
    /// Tests only — the editor must never wait for the worker, which is the
    /// entire point of it being a worker. Bounded so a design mistake shows up
    /// as a failing assertion rather than a test run that hangs forever.
    #[cfg(test)]
    pub(crate) fn wait_for(&mut self, paint: ImagePaint) {
        let key = (paint.asset, paint.source_frame);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !self.frames.contains_key(&key) && !self.failed.contains_key(&key) {
            let Some(left) = deadline.checked_duration_since(std::time::Instant::now()) else {
                return;
            };
            match self.from_worker.recv_timeout(left) {
                Ok(r) => self.take(r),
                Err(_) => return,
            }
        }
    }
}

impl Drop for FootageCache {
    fn drop(&mut self) {
        let _ = self.to_worker.send(Request::Stop);
        if let Some(w) = self.worker.take() {
            let _ = w.join();
        }
    }
}

/// The open stream, and which footage it is reading.
struct Session {
    asset: AssetId,
    meta: AssetMeta,
    stream: Box<dyn FrameStream>,
}

/// The worker: serve requested frames, and run ahead when there is nothing to
/// serve.
fn decode_loop(decoders: DecoderRegistry, rx: Receiver<Request>, tx: Sender<Response>) {
    let mut session: Option<Session> = None;
    // The furthest frame worth running ahead to, and which project generation
    // asked for it — both set by the last real request, so prefetched frames
    // are attributed exactly like the request that motivated them.
    let mut prefetch_until = 0i64;
    let mut epoch: Epoch = 0;
    // Whether there is an outstanding promise to retract. Mirrors the cache's
    // `promised`, and exists so the worker cannot announce the end of a
    // prefetch it never began — an unpaired `PrefetchEnded` at start-up would
    // cancel the very first promise before its frames arrived, and the
    // requester would then ask again for frames the stream had already passed.
    let mut promise_open = false;

    loop {
        // Real work first; only run ahead when the queue is empty.
        let req = match rx.try_recv() {
            Ok(req) => Some(req),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => return,
        };

        let req = match req {
            Some(req) => Some(req),
            // Nothing pending: decode one frame ahead, then look again. One
            // frame at a time so a request arriving mid-prefetch waits
            // microseconds rather than for a whole batch.
            None => {
                if prefetch_one(&mut session, prefetch_until, epoch, &tx) {
                    continue;
                }
                // Fully caught up. Say so before blocking — the UI holds off
                // re-requesting frames the prefetch promised, so it has to
                // learn when there is nothing more coming.
                if std::mem::take(&mut promise_open)
                    && tx.send(Response::PrefetchEnded { epoch }).is_err()
                {
                    return;
                }
                // Block, so the thread costs nothing while the user is reading
                // the screen rather than scrubbing it.
                match rx.recv() {
                    Ok(req) => Some(req),
                    Err(_) => return,
                }
            }
        };

        match req {
            Some(Request::Stop) | None => return,
            Some(Request::Reset) => {
                session = None;
                promise_open = false;
            }
            Some(Request::Frame { epoch: e, key, path, meta }) => {
                let result = serve(&decoders, &mut session, key, &path, &meta);
                prefetch_until = key.1 + PREFETCH_AHEAD;
                epoch = e;
                promise_open = true;
                if tx.send(Response::Frame { epoch: e, key, result }).is_err() {
                    return;
                }
            }
        }
    }
}

/// Produce one requested frame, reusing the open stream when it can reach it.
fn serve(
    decoders: &DecoderRegistry,
    session: &mut Option<Session>,
    key: Key,
    path: &Path,
    meta: &AssetMeta,
) -> Result<Frame, String> {
    let (asset, want) = key;

    // The fast path, and the whole point of the module: the stream is already
    // sitting on this frame, or close enough in front of it that walking there
    // is cheaper than re-opening.
    if let Some(s) = session.as_mut() {
        let at = s.stream.next_index();
        if s.asset == asset && want >= at && want - at <= MAX_WALK {
            loop {
                let at = s.stream.next_index();
                match s.stream.next_frame() {
                    Ok(Some(frame)) if at == want => return Ok(frame),
                    // Walking over the frames in between.
                    Ok(Some(_)) => continue,
                    // Ran off the end; fall through and re-open, which reports
                    // honestly rather than guessing.
                    Ok(None) => break,
                    Err(e) => return Err(e.to_string()),
                }
            }
        }
    }

    // Either different footage, a jump backwards, or too far ahead to walk.
    // Open a fresh stream *at* the wanted frame, which both answers this
    // request and leaves the worker positioned for the ones that follow it.
    *session = decoders
        .stream(path, want, meta)
        .map(|stream| Session { asset, meta: *meta, stream });
    match session.as_mut() {
        Some(s) => match s.stream.next_frame() {
            Ok(Some(frame)) => Ok(frame),
            Ok(None) => Err(format!("frame {want} is past the end of {}", path.display())),
            Err(e) => Err(e.to_string()),
        },
        // No sequential mode — a still, where one frame is already in order.
        None => decoders.frame(path, want, meta).map_err(|e| e.to_string()),
    }
}

/// Decode the next frame of the open stream, unasked. Returns whether it did.
fn prefetch_one(
    session: &mut Option<Session>,
    until: i64,
    epoch: Epoch,
    tx: &Sender<Response>,
) -> bool {
    let Some(s) = session.as_mut() else { return false };
    let at = s.stream.next_index();
    if at > until || at > s.meta.frames - 1 {
        return false;
    }
    match s.stream.next_frame() {
        Ok(Some(frame)) => {
            tx.send(Response::Frame { epoch, key: (s.asset, at), result: Ok(frame) }).is_ok()
        }
        // A prefetch failure is silent on purpose: nobody asked for this frame,
        // and if they ever do, the real request will surface the error then.
        Ok(None) | Err(_) => {
            *session = None;
            false
        }
    }
}
