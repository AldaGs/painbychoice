//! `pbc render`: the editor's GPU export with no window (ADR 0030).
//!
//! The same job the Draft/Master buttons start (`renderqueue`), on a headless
//! wgpu device, so a script or an agent gets the preview's own pixels —
//! vello, effects readback and all — rather than the CPU reference renderer
//! `motion render` uses. Prints one JSON report, like the `motion` commands.

use std::path::PathBuf;

use crate::offscreen::Headless;
use crate::renderqueue::{start, JobSpec, Step};

const USAGE: &str = "usage: pbc render <project.pbc> --out <file|dir> [--comp n] [--start f] [--end f] [--scale x] [--quality draft|master]";

/// Run `pbc render` with the arguments after `render`. Returns the exit code.
pub(crate) fn render(args: &[String]) -> i32 {
    let report = match run(args) {
        Ok(r) => r,
        Err(e) => serde_json::json!({ "ok": false, "errors": [e] }),
    };
    println!("{report}");
    if report["ok"] == true { 0 } else { 1 }
}

fn run(args: &[String]) -> Result<serde_json::Value, String> {
    let (mut input, mut out, mut comp, mut start_f, mut end_f, mut scale, mut quality) =
        (None, None, None, None, None, 1.0f64, motion_render::Quality::Draft);
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        let mut value = |name: &str| {
            i += 1;
            args.get(i).cloned().ok_or_else(|| format!("{name} needs a value"))
        };
        let num = |v: String, what: &str| v.parse::<i64>().map_err(|_| format!("{what} wants a number"));
        match a {
            "--out" => out = Some(PathBuf::from(value("--out")?)),
            "--comp" => comp = Some(motion_core::CompId(num(value("--comp")?, "--comp")? as u64)),
            "--start" => start_f = Some(num(value("--start")?, "--start")?),
            "--end" => end_f = Some(num(value("--end")?, "--end")?),
            "--scale" => scale = value("--scale")?.parse().map_err(|_| "--scale wants a number")?,
            "--quality" => {
                let v = value("--quality")?;
                quality = motion_render::Quality::parse(&v).ok_or_else(|| format!("--quality wants draft or master, not '{v}'"))?;
            }
            o if o.starts_with('-') => return Err(format!("unknown option '{o}'\n{USAGE}")),
            p => input = Some(PathBuf::from(p)),
        }
        i += 1;
    }
    let (input, out) = (input.ok_or(USAGE)?, out.ok_or(USAGE)?);
    let text = std::fs::read_to_string(&input).map_err(|e| format!("{}: {e}", input.display()))?;
    let (mut project, _) = crate::app::parse_save(&text)?;
    project.migrate();
    project.resolve_asset_paths(input.parent().unwrap_or(std::path::Path::new(".")));
    motion_render::attach_footage_sound(&mut project);
    let comp = comp.unwrap_or(project.root);
    let total = project.comp(comp).ok_or_else(|| format!("no composition {}", comp.0))?.duration_frames;

    let sounds: std::collections::HashMap<_, _> = project
        .assets
        .values()
        .filter(|a| a.has_audio())
        .filter_map(|a| motion_render::decode_sound(&a.path).ok().map(|s| (a.id, std::sync::Arc::new(s))))
        .collect();

    let mut gpu = Headless::new().ok_or("no usable GPU adapter")?;
    let range = (start_f.is_some() || end_f.is_some()).then(|| (start_f.unwrap_or(0), end_f.unwrap_or(total - 1)));
    let started = std::time::Instant::now();
    let mut job = start(
        JobSpec { project: &project, comp, out: out.clone(), quality, scale, ffmpeg_args: Vec::new(), range, sounds: &sounds },
        &gpu.device,
    )?;
    // The export's own cache: originals, through the sidecar, never proxies.
    let mut cache = crate::footage::FootageCache::new(motion_render::default_registry());
    loop {
        match job.step(&gpu.device, &gpu.queue, &mut gpu.renderer, &mut cache) {
            Step::Working => eprint!("\r  frame {}/{}", job.done(), job.total()),
            Step::Done => break,
            Step::Failed(e) => return Err(e),
        }
    }
    eprintln!();
    Ok(serde_json::json!({
        "ok": true,
        "output": out.display().to_string(),
        "frames": job.done(),
        "seconds": started.elapsed().as_secs_f64(),
        "renderer": "gpu",
    }))
}
