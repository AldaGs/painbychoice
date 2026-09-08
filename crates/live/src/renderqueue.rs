//! The GUI render queue: the two buttons from
//! [`decisions/0018`](../../../docs/decisions/0018-two-render-buttons.md), and
//! the incremental job that drives them.
//!
//! Not in `app.rs`, deliberately — the production plan names the render queue
//! and the effect stack as the two things that must *not* land in a file that is
//! already four thousand lines.
//!
//! # Why the job is incremental rather than threaded
//!
//! Rendering three hundred frames takes seconds to minutes, and the requirement
//! is that the editor does not freeze while it happens. The obvious answer is a
//! worker thread, and it is the wrong one here: the render needs the `wgpu`
//! device, the vello `Renderer` and the footage cache, all of which live in
//! `App` and are used by the preview on the main thread. Moving them costs
//! either a second GPU device — a different adapter on a two-GPU laptop, which
//! is the one machine where preview-equals-export must hold — or a lock held for
//! the whole job, which freezes the editor by another route.
//!
//! So a job is a **state machine stepped from the redraw loop**: each redraw
//! renders a slice of frames ([`FRAME_BUDGET`]) and asks for another redraw. The
//! UI stays live, the work is visible, and cancelling is immediate because
//! cancellation is just dropping the job. The cost is that the editor's frame
//! rate drops while a render runs, which is honest — the GPU is busy.
//!
//! Frame-parallel rendering (still ahead in the plan) parallelises `evaluate`,
//! which is pure and the CPU half of this loop. It composes with this design
//! rather than replacing it.

use std::path::{Path, PathBuf};
use std::time::Instant;

use motion_core::node::CompId;
use motion_core::{Project as MProject, RenderPreset};
use motion_render::{
    is_video_container, Encoder, FfmpegEncoder, OutputSpec, PngSequence, Quality,
};

use crate::offscreen::{export_size, FrameRenderer, OffscreenTarget};

/// How many frames one redraw renders before yielding to the UI.
///
/// A compromise with no clever answer: too low and the per-redraw overhead
/// dominates a fast comp, too high and the window stops responding to a cancel.
/// Four keeps a 1080p job responsive on a slow GPU while costing little on a
/// fast one.
const FRAME_BUDGET: usize = 4;

/// What a step of the job did, so the caller knows whether to ask for another
/// redraw.
#[derive(Debug, PartialEq)]
pub(crate) enum Step {
    /// More frames remain; call again.
    Working,
    /// The job finished and closed its output cleanly.
    Done,
    /// The job stopped. The string is shown to the user.
    Failed(String),
}

/// One export in progress.
pub(crate) struct RenderJob {
    /// A **snapshot** of the project taken when the job started.
    ///
    /// Copied rather than borrowed so editing during a render cannot change
    /// what is being rendered halfway through: a job that picked up an edit at
    /// frame 150 would produce a file that never existed as a document, and
    /// the bug would only be visible in the output.
    project: MProject,
    comp: CompId,
    /// Inclusive frame range, as the timeline reads it.
    start: i64,
    end: i64,
    /// The next frame to render.
    next: i64,
    out: PathBuf,
    quality: Quality,
    encoder: Option<Box<dyn Encoder>>,
    target: OffscreenTarget,
    started: Instant,
}

impl RenderJob {
    /// Total frames this job will write.
    pub(crate) fn total(&self) -> i64 {
        (self.end - self.start + 1).max(0)
    }

    /// Frames written so far.
    pub(crate) fn done(&self) -> i64 {
        (self.next - self.start).clamp(0, self.total())
    }

    pub(crate) fn quality(&self) -> Quality {
        self.quality
    }

    pub(crate) fn elapsed(&self) -> f64 {
        self.started.elapsed().as_secs_f64()
    }

    /// Render up to [`FRAME_BUDGET`] frames.
    ///
    /// The encoder is taken out of the `Option` to finish it, because
    /// [`Encoder::finish`] consumes a `Box<Self>` — an encoder that is merely
    /// dropped cannot report a muxer failure, and a truncated video that
    /// reported success is worse than an error.
    /// The GPU handles are passed in rather than held, because they belong to
    /// `App` and are shared with the preview. The target *is* held, and is
    /// borrowed from a disjoint field of `self` alongside the mutable `next` —
    /// which is why this takes the parts instead of a ready-made
    /// [`FrameRenderer`] that would have to borrow the job twice.
    pub(crate) fn step(
        &mut self,
        device: &vello::wgpu::Device,
        queue: &vello::wgpu::Queue,
        vello_renderer: &mut vello::Renderer,
        footage: &mut crate::footage::FootageCache,
    ) -> Step {
        let mut renderer = FrameRenderer {
            device,
            queue,
            renderer: vello_renderer,
            target: &self.target,
            footage,
        };
        for _ in 0..FRAME_BUDGET {
            if self.next > self.end {
                let Some(encoder) = self.encoder.take() else {
                    return Step::Failed("the encoder was already closed".to_string());
                };
                return match encoder.finish() {
                    Ok(()) => Step::Done,
                    Err(e) => Step::Failed(format!("closing {}: {e}", self.out.display())),
                };
            }
            let rgba = match renderer.frame(&self.project, self.comp, self.next as f64) {
                Ok(px) => px,
                Err(e) => return Step::Failed(format!("frame {}: {e}", self.next)),
            };
            let Some(encoder) = self.encoder.as_mut() else {
                return Step::Failed("the encoder was already closed".to_string());
            };
            if let Err(e) = encoder.push(&rgba) {
                return Step::Failed(format!("writing frame {}: {e}", self.next));
            }
            self.next += 1;
        }
        Step::Working
    }
}

/// A finished job, kept so the user can see what happened after the progress bar
/// is gone. This is the render queue's memory.
pub(crate) struct RenderRecord {
    pub(crate) out: PathBuf,
    pub(crate) quality: Quality,
    pub(crate) frames: i64,
    pub(crate) seconds: f64,
    /// `None` on success, the message on failure.
    pub(crate) error: Option<String>,
}

/// The queue: at most one job running, plus what has finished.
///
/// One at a time because they contend for a single GPU; queueing several would
/// only interleave the contention. The `Vec` is the log, newest last.
#[derive(Default)]
pub(crate) struct RenderQueue {
    pub(crate) active: Option<RenderJob>,
    pub(crate) log: Vec<RenderRecord>,
}

impl RenderJob {
    /// Abandon this job's output. See [`Encoder::abort`] — dropping a
    /// container-writing encoder finalizes it, which is the opposite of what
    /// cancelling means.
    fn abort(&mut self) {
        if let Some(encoder) = self.encoder.take() {
            encoder.abort();
        }
    }
}

impl RenderQueue {
    /// Retire the active job into the log.
    ///
    /// An `error` means the job did not produce a usable file, so its encoder is
    /// aborted rather than dropped. On success the encoder is already gone —
    /// [`RenderJob::step`] consumed it in `finish` — so there is nothing to
    /// abort and this is a no-op.
    pub(crate) fn finish(&mut self, error: Option<String>) {
        let Some(mut job) = self.active.take() else { return };
        if error.is_some() {
            job.abort();
        }
        self.log.push(RenderRecord {
            out: job.out.clone(),
            quality: job.quality,
            frames: job.done(),
            seconds: job.elapsed(),
            error,
        });
    }

    /// The most recent outcome, for a one-line status.
    pub(crate) fn last(&self) -> Option<&RenderRecord> {
        self.log.last()
    }
}

/// Where a Draft render writes, given the project's own path.
///
/// Predictable is the entire requirement — Draft asks no questions, so a user
/// has to be able to find the file without being told. Next to the project,
/// with a `_draft` suffix, is the answer they can guess.
///
/// An unsaved project has no directory to be next to, so it goes to the
/// system temp directory. That is deliberate over the current working
/// directory, which for a windowed app is wherever the launcher happened to be.
pub(crate) fn draft_path(project: Option<&Path>) -> PathBuf {
    match project.and_then(|p| p.file_stem().map(|s| (p, s.to_string_lossy().into_owned()))) {
        Some((path, stem)) => {
            let dir = path.parent().unwrap_or(Path::new("."));
            dir.join(format!("{stem}_draft.mp4"))
        }
        None => std::env::temp_dir().join("pbc_draft.mp4"),
    }
}

/// Resolve a preset's output path against the project's location.
///
/// A relative path in a preset is relative to **the project file**, not to the
/// process's working directory: the preset travels in the `.pbc`, so the only
/// anchor that means the same thing on another machine is the project itself.
pub(crate) fn resolve_out(project: Option<&Path>, out: &str) -> PathBuf {
    let candidate = Path::new(out);
    if candidate.is_absolute() {
        return candidate.to_path_buf();
    }
    match project.and_then(|p| p.parent()) {
        Some(dir) => dir.join(candidate),
        None => candidate.to_path_buf(),
    }
}

/// Whether a draft would land on a path some master preset claims.
///
/// Rule 2 of [`decisions/0018`]: overwriting a deliverable with a preview is
/// unrecoverable and entirely avoidable. The `_draft` suffix makes this
/// near-impossible by accident, so a hit means a preset deliberately names that
/// file — and the right response is to refuse loudly, not to quietly pick a
/// different name the user will then go looking for.
pub(crate) fn collides_with_a_preset(
    draft: &Path,
    presets: &[RenderPreset],
    project: Option<&Path>,
) -> bool {
    presets.iter().any(|p| resolve_out(project, &p.out) == draft)
}

/// Progress as a fraction and a human line: `"148/300 · 12.4s · ~13s left"`.
///
/// The estimate is a flat extrapolation from the frames done so far, which is
/// honest for this workload: frames of one comp cost about the same, and the
/// alternative — a windowed average — jitters more than it informs.
pub(crate) fn progress(done: i64, total: i64, elapsed: f64) -> (f32, String) {
    if total <= 0 {
        return (0.0, "nothing to render".to_string());
    }
    let frac = (done as f64 / total as f64).clamp(0.0, 1.0);
    let mut line = format!("{done}/{total} · {elapsed:.1}s");
    if done > 0 && done < total && elapsed > 0.0 {
        let remaining = elapsed / done as f64 * (total - done) as f64;
        line.push_str(&format!(" · ~{remaining:.0}s left"));
    }
    (frac as f32, line)
}

/// Build the encoder an output path implies.
///
/// The extension picks the container, exactly as it does on the command line —
/// one rule, so a preset and a `--out` cannot mean different things.
fn encoder_for(
    out: &Path,
    spec: OutputSpec,
    quality: Quality,
    extra: &[String],
) -> Result<Box<dyn Encoder>, String> {
    if is_video_container(out) {
        // The preset first, the user's own arguments after it, so a saved
        // argument can override the preset rather than fight it.
        let mut args = quality.ffmpeg_args(out);
        args.extend(extra.iter().cloned());
        FfmpegEncoder::new(out, spec, &args)
            .map(|e| Box::new(e) as Box<dyn Encoder>)
            .map_err(|e| e.to_string())
    } else {
        let stem = out.file_name().and_then(|s| s.to_str()).unwrap_or("frame");
        PngSequence::new(out, stem, spec)
            .map(|e| Box::new(e) as Box<dyn Encoder>)
            .map_err(|e| e.to_string())
    }
}

/// Everything needed to start a job that is not the GPU.
pub(crate) struct JobSpec<'a> {
    pub(crate) project: &'a MProject,
    pub(crate) comp: CompId,
    pub(crate) out: PathBuf,
    pub(crate) quality: Quality,
    pub(crate) scale: f64,
    pub(crate) ffmpeg_args: Vec<String>,
}

/// Start a render, allocating its target and opening its encoder.
///
/// Fails before writing anything if the comp is missing or the encoder cannot
/// open — an export that fails should fail at the button, not at frame 200.
pub(crate) fn start(
    spec: JobSpec<'_>,
    device: &vello::wgpu::Device,
) -> Result<RenderJob, String> {
    let comp = spec
        .project
        .comp(spec.comp)
        .ok_or_else(|| format!("no composition {} in this project", spec.comp.0))?;

    let (w, h) = export_size((comp.width, comp.height), spec.scale);
    let target = OffscreenTarget::new(device, w, h)
        .ok_or_else(|| format!("could not allocate a {w}x{h} render target"))?;

    let out_spec = OutputSpec { width: w, height: h, fps: comp.fps };
    let encoder = encoder_for(&spec.out, out_spec, spec.quality, &spec.ffmpeg_args)?;

    Ok(RenderJob {
        project: spec.project.clone(),
        comp: spec.comp,
        start: 0,
        end: comp.duration_frames - 1,
        next: 0,
        out: spec.out,
        quality: spec.quality,
        encoder: Some(encoder),
        target,
        started: Instant::now(),
    })
}

/// What the render bar asked for this frame. At most one of these is true —
/// they are buttons, and the pass is one frame.
///
/// Reported rather than acted on, like every other panel intent in the editor:
/// the UI closure cannot borrow `App`, and starting a render mid-pass would
/// mutate the project snapshot the pass is drawing from.
#[derive(Default)]
pub(crate) struct RenderEdits {
    pub(crate) draft: bool,
    pub(crate) master: bool,
    pub(crate) cancel: bool,
}

/// A running job as the bar needs to show it — an owned snapshot, so the UI
/// closure holds no borrow of `App`.
pub(crate) struct RenderProgress {
    pub(crate) frac: f32,
    pub(crate) line: String,
    pub(crate) quality: Quality,
}

/// The last finished job: a chip for the bar and the detail for its tooltip.
///
/// Two strings rather than one because the comp bar cannot grow — `short` is
/// bounded (a filename), `text` is not (a full path, or an ffmpeg error), and
/// only the bounded one is allowed on the row. See [`render_ui`].
pub(crate) struct RenderSummary {
    /// The bar chip. A filename, or nothing much.
    pub(crate) short: String,
    /// The hover detail: the full path and timing, or the error.
    pub(crate) text: String,
    pub(crate) failed: bool,
}

impl RenderSummary {
    /// Build both strings from a record.
    ///
    /// The full path goes in the tooltip because a user who just exported needs
    /// to find the file and a basename does not tell them where it went — but
    /// the bar shows the basename, because the path is unbounded and the row is
    /// not.
    pub(crate) fn of(record: &RenderRecord) -> Self {
        let name = record
            .out
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| record.out.display().to_string());
        match &record.error {
            Some(e) => RenderSummary {
                short: "Render failed".to_string(),
                text: format!("{} → {}: {e}", record.quality.label(), record.out.display()),
                failed: true,
            },
            None => RenderSummary {
                short: format!("✓ {name}"),
                text: format!(
                    "{} · {} frames in {:.1}s → {}",
                    record.quality.label(),
                    record.frames,
                    record.seconds,
                    record.out.display()
                ),
                failed: false,
            },
        }
    }
}

/// The two buttons, and whatever the queue is doing.
///
/// Draws **inline on the composition bar's existing row**, and that is a
/// constraint rather than a style choice: the `Comp` leaf is a fixed
/// [`COMP_H`]-tall strip and is not scroll-wrapped, so anything that allocates
/// past it pushes the whole layout down and shoves the bottom panel off-screen
/// (invariant 16). Hence no `ui.horizontal` of its own, no second row, and
/// nothing here that can grow without bound — the last render's path lives in a
/// tooltip, not in the bar, because a long path would overflow the row
/// sideways for the same reason.
///
/// Draft and Master are **different verbs**, not one button with a mode, and
/// the bar says so by never putting a settings affordance on Draft. While a job
/// runs both are replaced by a progress bar and a Cancel — one GPU, one job.
pub(crate) fn render_ui(
    ui: &mut egui::Ui,
    active: Option<&RenderProgress>,
    last: Option<&RenderSummary>,
    out: &mut RenderEdits,
) {
    match active {
        Some(p) => {
            ui.add(
                egui::ProgressBar::new(p.frac)
                    .desired_width(120.0)
                    .text(format!("{} {}", p.quality.label(), p.line)),
            );
            if ui.button("Cancel").clicked() {
                out.cancel = true;
            }
        }
        None => {
            if ui
                .button("Draft")
                .on_hover_text(
                    "Render the whole comp at full resolution with fast encoder                      settings, beside the project. No questions asked.",
                )
                .clicked()
            {
                out.draft = true;
            }
            if ui
                .button("Master")
                .on_hover_text(
                    "Render the deliverable using the project's saved render                      preset, so everyone on this project produces the same file.",
                )
                .clicked()
            {
                out.master = true;
            }
            // The outcome as a short chip with the detail on hover. A rendered
            // path is easily eighty characters and this row has no room for it.
            if let Some(s) = last {
                if s.failed {
                    ui.colored_label(egui::Color32::from_rgb(220, 90, 80), "Render failed")
                        .on_hover_text(&s.text);
                } else {
                    ui.weak(&s.short).on_hover_text(&s.text);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn preset(out: &str) -> RenderPreset {
        RenderPreset { out: out.to_string(), ..RenderPreset::default_master() }
    }

    /// Draft lands beside the project under a guessable name — the property
    /// that lets Draft ask no questions.
    #[test]
    fn a_draft_goes_next_to_the_project() {
        let p = draft_path(Some(Path::new("/films/titles.pbc")));
        assert_eq!(p.file_name().unwrap(), "titles_draft.mp4");
        assert_eq!(p.parent().unwrap(), Path::new("/films"));
    }

    /// An unsaved project has no directory of its own, and the working
    /// directory of a windowed app is not a place a user can find.
    #[test]
    fn an_unsaved_project_drafts_to_the_temp_directory() {
        let p = draft_path(None);
        assert_eq!(p.parent().unwrap(), std::env::temp_dir());
        assert_eq!(p.file_name().unwrap(), "pbc_draft.mp4");
    }

    /// A preset's relative path is anchored to the project file, so the same
    /// `.pbc` writes to the same place on someone else's machine.
    #[test]
    fn a_relative_preset_resolves_against_the_project() {
        let p = resolve_out(Some(Path::new("/films/titles.pbc")), "deliver/final.mp4");
        assert_eq!(p, Path::new("/films/deliver/final.mp4"));
    }

    /// An absolute preset path is used as written — it already means one place.
    #[test]
    fn an_absolute_preset_path_is_left_alone() {
        let p = resolve_out(Some(Path::new("/films/titles.pbc")), "/mnt/deliver/final.mp4");
        assert_eq!(p, Path::new("/mnt/deliver/final.mp4"));
    }

    /// Rule 2: a draft must never write a path a master preset claims.
    #[test]
    fn a_draft_that_would_overwrite_a_master_is_detected() {
        let project = Some(Path::new("/films/titles.pbc"));
        let draft = draft_path(project);
        assert!(
            collides_with_a_preset(&draft, &[preset("titles_draft.mp4")], project),
            "a preset naming the draft path must be caught"
        );
        assert!(
            !collides_with_a_preset(&draft, &[preset("titles.mp4")], project),
            "an ordinary master path is not a collision"
        );
    }

    /// The usual case, stated as a test so the `_draft` suffix cannot be
    /// removed without something failing: a default master preset and a draft
    /// of the same project do not collide.
    #[test]
    fn the_default_master_never_collides_with_a_draft() {
        let project = Some(Path::new("/films/titles.pbc"));
        let draft = draft_path(project);
        assert!(!collides_with_a_preset(&draft, &[RenderPreset::default_master()], project));
    }

    #[test]
    fn progress_is_a_fraction_and_a_line() {
        let (frac, line) = progress(150, 300, 12.0);
        assert!((frac - 0.5).abs() < 1e-6);
        assert!(line.starts_with("150/300 · 12.0s"), "{line}");
        assert!(line.contains("left"), "a half-done job estimates: {line}");
    }

    /// No estimate before the first frame (there is nothing to extrapolate
    /// from) and none after the last (there is nothing left).
    #[test]
    fn progress_estimates_only_while_it_can() {
        assert!(!progress(0, 300, 0.5).1.contains("left"));
        assert!(!progress(300, 300, 12.0).1.contains("left"));
    }

    /// An empty range is a state the UI can reach (a one-frame comp, a range
    /// dragged to nothing), and it must not divide by zero.
    #[test]
    fn an_empty_range_has_no_progress() {
        let (frac, line) = progress(0, 0, 1.0);
        assert_eq!(frac, 0.0);
        assert_eq!(line, "nothing to render");
    }

    /// **The whole export path, end to end, on a real GPU** — what pressing
    /// Draft does, minus the mouse.
    ///
    /// Starts a job, steps it exactly as the redraw loop does, and checks the
    /// files landed. Worth the GPU dependency because every interesting failure
    /// in this module lives between `start` and the last `step`: an off-by-one
    /// in the range writes the wrong number of frames, a mis-sized target makes
    /// the encoder reject frame 0, and a `Step` that never reports `Done` hangs
    /// the loop forever. None of that is visible to a compile.
    #[test]
    fn a_job_renders_every_frame_and_finishes() {
        let Some(mut gpu) = crate::offscreen::Headless::new() else {
            eprintln!("no usable GPU adapter; skipping the end-to-end render job test");
            return;
        };
        let mut comp = motion_core::Comp::new(
            32.0,
            32.0,
            motion_core::Node::group(0, "root"),
        );
        comp.duration_frames = 10;
        let project = MProject::single(comp);

        let dir = std::env::temp_dir().join(format!("pbc_job_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);

        let mut job = start(
            JobSpec {
                project: &project,
                comp: project.root,
                out: dir.clone(),
                quality: Quality::Draft,
                scale: 1.0,
                ffmpeg_args: Vec::new(),
            },
            &gpu.device,
        )
        .expect("the job must open");
        assert_eq!(job.total(), 10, "an inclusive range of 0..=9");

        let mut cache = crate::footage::FootageCache::new(motion_render::default_registry());
        // A generous bound rather than `loop`: a `step` that never reports Done
        // is a real failure mode, and hanging the suite is a bad way to find it.
        let mut guard = 0;
        let outcome = loop {
            match job.step(&gpu.device, &gpu.queue, &mut gpu.renderer, &mut cache) {
                Step::Working => {}
                other => break other,
            }
            guard += 1;
            assert!(guard < 100, "the job never finished");
        };
        assert_eq!(outcome, Step::Done, "the job must finish cleanly");
        assert_eq!(job.done(), 10, "every frame accounted for");

        let written = std::fs::read_dir(&dir).expect("the output directory").count();
        assert_eq!(written, 10, "one PNG per frame");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A job aimed at a comp that isn't there fails at the button rather than
    /// at frame 200 — and writes nothing on the way.
    #[test]
    fn a_job_for_a_missing_comp_refuses_to_start() {
        let Some(gpu) = crate::offscreen::Headless::new() else {
            eprintln!("no usable GPU adapter; skipping the missing-comp test");
            return;
        };
        let project =
            MProject::single(motion_core::Comp::new(8.0, 8.0, motion_core::Node::group(0, "root")));
        let dir = std::env::temp_dir().join(format!("pbc_missing_{}", std::process::id()));
        let err = start(
            JobSpec {
                project: &project,
                comp: CompId(999),
                out: dir.clone(),
                quality: Quality::Draft,
                scale: 1.0,
                ffmpeg_args: Vec::new(),
            },
            &gpu.device,
        );
        let err = match err {
            Ok(_) => panic!("a missing comp must not start a job"),
            Err(e) => e,
        };
        assert!(err.contains("999"), "the message names the comp: {err}");
        assert!(!dir.exists(), "nothing is written for a job that never started");
    }
}
