//! `motion` — the headless renderer.
//!
//! The offline half of PBC: it opens a `.pbc`, evaluates it frame by frame, and
//! writes the result somewhere. No window, no GPU, no editor.
//!
//! It exists for three reasons, in order of importance:
//!
//! 1. **It is the batch and CI story.** A render that needs a logged-in desktop
//!    session is a render that cannot run on a build machine or a farm.
//! 2. **It keeps the engine honest as a library.** Everything this binary does,
//!    it does through the same public API a third party would use. A seam that
//!    only the editor can reach is a seam that will grow editor assumptions.
//! 3. **It is the fastest way to see a change in pixels** without opening the
//!    app.
//!
//! # What it renders with
//!
//! The CPU rasterizer ([`motion_render::raster`]), which draws vectors and
//! reports footage rather than drawing it — see that module for why, and for
//! the difference between structural and per-pixel parity with the editor's
//! GPU preview. Anything it cannot draw exactly is **printed**, never silently
//! omitted.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use motion_core::{demo::demo_document, evaluate_comp, Color, CompId, Project};
use motion_render::{
    output_size, rasterize, scene_to_svg_reporting, Encoder, FfmpegEncoder, OutputSpec,
    PngSequence,
};

const USAGE: &str = "\
motion — the headless PBC renderer

USAGE:
  motion render <project.pbc> --out <file|dir> [options]
  motion render --demo --out <file|dir> [options]
  motion demo [--out <dir>]

RENDER OPTIONS:
  --out <path>       Output. An extension ffmpeg knows (.mp4, .mov, .webm)
                     encodes a video; anything else is a directory that
                     receives a PNG sequence. Required.
  --comp <n>         Composition id to render. Default: the project's root.
  --start <frame>    First frame. Default: 0.
  --end <frame>      Last frame, inclusive. Default: the comp's last.
  --scale <factor>   Output scale, e.g. 0.5 for a half-size preview. Default: 1.
  --fps <rate>       Override the output rate. The comp is still evaluated on
                     its own frames; this only changes playback speed.
  --arg <ffmpeg arg> Passed through to ffmpeg, before the output path. Repeatable.
  --demo             Render the built-in demo document instead of a file.
                     The way to check that rendering works on a machine with
                     no project to hand.
  -q, --quiet        Only report errors.

  motion demo        Writes the built-in demo document as SVG frames to ./out,
                     which is what this binary did before it could render.
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("render") => match render(&args[1..]) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("error: {e}");
                ExitCode::FAILURE
            }
        },
        Some("demo") => {
            demo(args.get(2).map(Path::new).unwrap_or(Path::new("out")));
            ExitCode::SUCCESS
        }
        Some("-h") | Some("--help") | None => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        Some(other) => {
            eprintln!("error: unknown command '{other}'\n\n{USAGE}");
            ExitCode::FAILURE
        }
    }
}

/// The parsed `render` invocation.
#[derive(Debug)]
struct Opts {
    /// Absent when `--demo` supplies the project instead.
    project: Option<PathBuf>,
    out: PathBuf,
    comp: Option<u64>,
    start: Option<i64>,
    end: Option<i64>,
    scale: f64,
    fps: Option<f64>,
    ffmpeg_args: Vec<String>,
    quiet: bool,
    demo: bool,
}

/// Hand-rolled rather than a `clap` dependency: the flag set is small and
/// stable, and the binary's whole job is to be the thin edge of the library.
fn parse(args: &[String]) -> Result<Opts, String> {
    let mut o = Opts {
        project: None,
        out: PathBuf::new(),
        comp: None,
        start: None,
        end: None,
        scale: 1.0,
        fps: None,
        ffmpeg_args: Vec::new(),
        quiet: false,
        demo: false,
    };
    let mut positional = None;
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        let mut value = |name: &str| -> Result<String, String> {
            i += 1;
            args.get(i).cloned().ok_or_else(|| format!("{name} needs a value"))
        };
        match a {
            "--out" => o.out = PathBuf::from(value("--out")?),
            "--comp" => {
                o.comp = Some(value("--comp")?.parse().map_err(|_| "--comp wants a number")?)
            }
            "--start" => {
                o.start = Some(value("--start")?.parse().map_err(|_| "--start wants a frame")?)
            }
            "--end" => o.end = Some(value("--end")?.parse().map_err(|_| "--end wants a frame")?),
            "--scale" => {
                o.scale = value("--scale")?.parse().map_err(|_| "--scale wants a number")?
            }
            "--fps" => o.fps = Some(value("--fps")?.parse().map_err(|_| "--fps wants a rate")?),
            "--arg" => o.ffmpeg_args.push(value("--arg")?),
            "--demo" => o.demo = true,
            "-q" | "--quiet" => o.quiet = true,
            other if other.starts_with('-') => return Err(format!("unknown option '{other}'")),
            other => positional = Some(PathBuf::from(other)),
        }
        i += 1;
    }
    o.project = positional;
    if o.project.is_none() && !o.demo {
        return Err("a .pbc to render is required (or --demo)".into());
    }
    if o.out.as_os_str().is_empty() {
        return Err("--out is required".into());
    }
    if !(o.scale > 0.0 && o.scale.is_finite()) {
        return Err("--scale must be a positive number".into());
    }
    Ok(o)
}

/// Whether the output path names a video container. Extension-driven, because
/// deciding for the user is how you end up owning a codec table.
fn is_video(path: &Path) -> bool {
    const VIDEO: [&str; 8] = ["mp4", "mov", "mkv", "webm", "avi", "m4v", "mxf", "gif"];
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| VIDEO.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

fn render(args: &[String]) -> Result<(), String> {
    let o = parse(args)?;
    let project = match &o.project {
        Some(path) => {
            let text = std::fs::read_to_string(path)
                .map_err(|e| format!("reading {}: {e}", path.display()))?;
            Project::from_pbc(&text)?
        }
        None => Project::single(demo_document()),
    };

    let comp_id = o.comp.map(CompId).unwrap_or(project.root);
    let comp = project
        .comp(comp_id)
        .ok_or_else(|| format!("no composition {} in this project", comp_id.0))?;

    // The frame range, clamped to the comp. `end` is inclusive because a user
    // asking for frames 0..100 means 101 frames — the timeline's own reading.
    let last = comp.duration_frames - 1;
    let start = o.start.unwrap_or(0).max(0);
    let end = o.end.unwrap_or(last).min(last);
    if end < start {
        return Err(format!("empty range: frames {start} to {end}"));
    }

    let (w, h) = output_size(comp.width, comp.height, o.scale);
    let spec = OutputSpec { width: w, height: h, fps: o.fps.unwrap_or(comp.fps) };

    let mut encoder: Box<dyn Encoder> = if is_video(&o.out) {
        Box::new(FfmpegEncoder::new(&o.out, spec, &o.ffmpeg_args).map_err(|e| e.to_string())?)
    } else {
        let stem = o
            .project
            .as_deref()
            .and_then(Path::file_stem)
            .and_then(|s| s.to_str())
            .unwrap_or("frame");
        Box::new(PngSequence::new(&o.out, stem, spec).map_err(|e| e.to_string())?)
    };

    if !o.quiet {
        eprintln!(
            "rendering {} frames of \"{}\" at {}x{} @ {} → {}",
            end - start + 1,
            comp.label(comp_id),
            w,
            h,
            spec.fps,
            encoder.name()
        );
    }

    // Notes are deduplicated: a footage layer is unrenderable on every one of
    // three hundred frames, and saying so three hundred times buries anything
    // else. Reported once, with the count.
    let mut notes: std::collections::BTreeMap<String, usize> = Default::default();
    let started = std::time::Instant::now();

    for frame in start..=end {
        let scene = evaluate_comp(&project, comp_id, frame as f64);
        for (id, msg) in &scene.warnings {
            *notes.entry(format!("node {}: {msg}", id.0)).or_default() += 1;
        }
        let (pixels, report) =
            rasterize(&scene, comp.width as u32, comp.height as u32, comp.bg, o.scale)
                .map_err(|e| e.to_string())?;
        for note in report {
            *notes.entry(note).or_default() += 1;
        }
        encoder.push(&pixels).map_err(|e| e.to_string())?;
        if !o.quiet && (frame - start) % 25 == 0 {
            eprint!("\r  frame {frame}/{end}");
        }
    }
    encoder.finish().map_err(|e| e.to_string())?;

    if !o.quiet {
        let secs = started.elapsed().as_secs_f64();
        let frames = (end - start + 1) as f64;
        eprintln!(
            "\r  {} frames in {secs:.2}s ({:.1} fps) → {}",
            frames as i64,
            frames / secs.max(f64::MIN_POSITIVE),
            o.out.display()
        );
    }
    for (note, count) in &notes {
        eprintln!("note: {note}{}", if *count > 1 { format!(" (×{count})") } else { String::new() });
    }
    Ok(())
}

/// The built-in demo, as SVG frames. What this binary used to be, kept because
/// it is the one command that needs no input file and exercises the whole
/// pipeline — document model → evaluation → render — in one line.
fn demo(out_dir: &Path) {
    let doc = demo_document();
    std::fs::create_dir_all(out_dir).expect("create out dir");
    let bg = Color::rgb(0.08, 0.09, 0.11);
    let tb = doc.timebase();
    let samples = 9;
    let last_frame = tb.seconds_to_frames(2.0);
    for i in 0..samples {
        let frame = (i as f64 / (samples - 1) as f64 * last_frame as f64).round();
        let scene = motion_core::evaluate(&doc, frame);
        for (id, msg) in &scene.warnings {
            eprintln!("warning [node {}]: {msg}", id.0);
        }
        let (svg, report) = scene_to_svg_reporting(&scene, doc.width, doc.height, bg, &[]);
        for note in &report {
            eprintln!("note [frame {frame}]: {note}");
        }
        let path = out_dir.join(format!("frame_{i:02}.svg"));
        std::fs::write(&path, svg).expect("write svg");
        println!(
            "{}  (frame {frame})  ->  {}  ({} items)",
            tb.timecode(frame),
            path.display(),
            scene.items.len()
        );
    }
    println!("\nDone. Open {}/frame_*.svg to scrub the animation by hand.", out_dir.display());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &str) -> Vec<String> {
        s.split_whitespace().map(String::from).collect()
    }

    /// The output's extension picks the encoder. Nothing else does — no flag,
    /// no guess — so a `.mp4` is a video and a bare directory is a sequence.
    #[test]
    fn the_extension_chooses_the_encoder() {
        assert!(is_video(Path::new("film.mp4")));
        assert!(is_video(Path::new("MASTER.MOV")), "case-insensitive");
        assert!(!is_video(Path::new("frames")));
        assert!(!is_video(Path::new("frames/shot_01")));
    }

    #[test]
    fn a_render_needs_a_project_and_an_output() {
        assert!(parse(&args("--out frames")).is_err(), "no project");
        assert!(parse(&args("film.pbc")).is_err(), "no output");
        assert!(parse(&args("film.pbc --out frames")).is_ok());
        assert!(parse(&args("--demo --out frames")).is_ok(), "--demo needs no file");
    }

    /// A zero or negative scale would ask for a zero-pixel frame, which fails
    /// far away from the flag that caused it. Refused where it is readable.
    #[test]
    fn a_nonsense_scale_is_refused_at_the_flag() {
        assert!(parse(&args("f.pbc --out o --scale 0")).is_err());
        assert!(parse(&args("f.pbc --out o --scale -1")).is_err());
        assert!(parse(&args("f.pbc --out o --scale 0.5")).is_ok());
    }

    /// A misspelled flag must not be swallowed as the project path — that would
    /// render the wrong thing and report success.
    #[test]
    fn an_unknown_flag_is_an_error_not_a_filename() {
        let err = parse(&args("f.pbc --out o --qality high")).unwrap_err();
        assert!(err.contains("--qality"), "{err}");
    }

    /// Options carry their values, and `--arg` accumulates so several ffmpeg
    /// settings can be passed through.
    #[test]
    fn options_parse_into_the_render_request() {
        let o = parse(&args(
            "film.pbc --out out.mp4 --comp 2 --start 10 --end 40 --scale 2 --fps 24 \
             --arg -crf --arg 18 -q",
        ))
        .unwrap();
        assert_eq!(o.project.unwrap().to_str().unwrap(), "film.pbc");
        assert_eq!(o.comp, Some(2));
        assert_eq!((o.start, o.end), (Some(10), Some(40)));
        assert_eq!(o.scale, 2.0);
        assert_eq!(o.fps, Some(24.0));
        assert_eq!(o.ffmpeg_args, vec!["-crf", "18"]);
        assert!(o.quiet);
    }

    /// End to end, on the built-in demo: frames are evaluated, rasterized and
    /// written. The `--demo` path exists so this is answerable with no fixture
    /// on any machine, and a small scale keeps it quick.
    #[test]
    fn the_demo_renders_a_png_sequence() {
        let dir = std::env::temp_dir().join(format!("pbc_cli_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let out = dir.to_str().unwrap().to_string();
        render(&args(&format!("--demo --out {out} --end 2 --scale 0.05 -q"))).unwrap();
        let written: Vec<_> = std::fs::read_dir(&dir).unwrap().filter_map(|e| e.ok()).collect();
        assert_eq!(written.len(), 3, "frames 0, 1 and 2");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The range is inclusive and clamped to the comp: asking past the end
    /// renders to the end rather than erroring or writing blank frames.
    #[test]
    fn the_frame_range_is_inclusive_and_clamped() {
        let dir = std::env::temp_dir().join(format!("pbc_cli_range_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let out = dir.to_str().unwrap().to_string();
        // The demo comp is 300 frames; ask for far beyond it.
        render(&args(&format!("--demo --out {out} --start 297 --end 9999 --scale 0.05 -q")))
            .unwrap();
        let n = std::fs::read_dir(&dir).unwrap().count();
        assert_eq!(n, 3, "frames 297, 298, 299 — clamped, not extended");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
