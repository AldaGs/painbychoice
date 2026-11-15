//! Proxies: low-res, all-intra copies of heavy video for the preview (ADR 0029).
//!
//! The preview decodes the proxy, export decodes the original. A proxy is
//! found by **where it would be**, not by a field in the document: its path is
//! a hash of the source's absolute path, size and modification time, in the
//! user's cache folder. That keeps proxies out of the undo history and the
//! live link, survives a project being copied, and goes stale on its own when
//! the source changes (its hash no longer matches).
//!
//! Every frame is a keyframe (`-g 1`, or MJPEG), so scrubbing never decodes
//! from a distant keyframe. The frame count and timing are copied from the
//! source (`-fps_mode passthrough`), so source frame N is proxy frame N.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Proxies are made at this height (aspect kept). Footage no taller doesn't get one.
pub const HEIGHT: u32 = 540;

/// Where proxies live: `%LOCALAPPDATA%\pbc\proxies` (or `$XDG_CACHE_HOME`/
/// `~/.cache` elsewhere). `PBC_PROXY_DIR` overrides.
pub fn dir() -> Option<PathBuf> {
    if let Some(d) = std::env::var_os("PBC_PROXY_DIR") {
        return Some(d.into());
    }
    let base = std::env::var_os("LOCALAPPDATA")
        .or_else(|| std::env::var_os("XDG_CACHE_HOME"))
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| Path::new(&h).join(".cache")))?;
    Some(base.join("pbc").join("proxies"))
}

/// FNV-1a: stable across Rust versions, unlike `DefaultHasher`.
fn fnv(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, b| (h ^ *b as u64).wrapping_mul(0x100_0000_01b3))
}

/// The proxy `src` would have. `None` if the source can't be read.
pub fn path_for(src: &Path) -> Option<PathBuf> {
    let m = std::fs::metadata(src).ok()?;
    let mtime = m.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs();
    let abs = std::path::absolute(src).unwrap_or_else(|_| src.to_path_buf());
    let key = format!("{}|{}|{}", abs.display(), m.len(), mtime);
    let stem = src.file_stem().and_then(|s| s.to_str()).unwrap_or("clip");
    let stem: String = stem.chars().map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).take(40).collect();
    Some(dir()?.join(format!("{stem}-{:016x}.mov", fnv(key.as_bytes()))))
}

/// The proxy for `src`, if one has been made.
pub fn existing(src: &Path) -> Option<PathBuf> {
    let p = path_for(src).filter(|p| p.exists())?;
    // Marked used, so pruning drops the proxies nobody has opened longest.
    let _ = std::fs::File::options().write(true).open(&p).and_then(|f| f.set_modified(std::time::SystemTime::now()));
    Some(p)
}

/// How much the proxy folder may hold: `PBC_PROXY_MAX_GB`, default 20 GB.
pub fn budget() -> u64 {
    let gb = std::env::var("PBC_PROXY_MAX_GB").ok().and_then(|v| v.parse::<f64>().ok()).unwrap_or(20.0);
    (gb.max(0.0) * 1e9) as u64
}

/// Delete the least recently used proxies until the folder fits in
/// `max_bytes`, plus any half-made `.part` files. Returns (files, bytes) freed.
/// Nothing else lives in the folder, so nothing else is touched.
pub fn prune(max_bytes: u64) -> (usize, u64) {
    let Some(entries) = dir().and_then(|d| std::fs::read_dir(d).ok()) else { return (0, 0) };
    let mut files: Vec<(std::time::SystemTime, u64, PathBuf)> = entries
        .filter_map(|e| {
            let e = e.ok()?;
            let m = e.metadata().ok()?;
            let ok = m.is_file() && e.path().extension().is_some_and(|x| x == "mov");
            ok.then_some((m.modified().ok()?, m.len(), e.path()))
        })
        .collect();
    let (mut n, mut freed) = (0, 0);
    // Half-made files never become proxies; they are only ever litter.
    files.retain(|(_, len, p)| {
        let part = p.to_string_lossy().ends_with(".part.mov");
        if part && std::fs::remove_file(p).is_ok() {
            n += 1;
            freed += len;
        }
        !part
    });
    files.sort();
    let mut total: u64 = files.iter().map(|f| f.1).sum();
    for (_, len, p) in files {
        if total <= max_bytes {
            break;
        }
        if std::fs::remove_file(&p).is_ok() {
            total -= len;
            n += 1;
            freed += len;
        }
    }
    (n, freed)
}

/// Whether footage this size is worth a proxy.
pub fn wanted(width: f64, height: f64) -> bool {
    height > HEIGHT as f64 * 1.5 || width > HEIGHT as f64 * 1.5 * 16.0 / 9.0
}

/// Make `src`'s proxy (blocking; run it on a thread) and return its path. An
/// existing one is returned as is. Written beside and renamed, so a half-made
/// proxy is never picked up.
pub fn make(src: &Path) -> Result<PathBuf, String> {
    let out = path_for(src).ok_or_else(|| format!("{}: can't read the source", src.display()))?;
    if out.exists() {
        return Ok(out);
    }
    std::fs::create_dir_all(out.parent().unwrap()).map_err(|e| e.to_string())?;
    let part = out.with_extension("part.mov");
    let scale = format!("scale=-2:'min({HEIGHT},ih)'");
    // All-intra H.264 where this ffmpeg has x264; MJPEG (always built in) otherwise.
    let codecs: [&[&str]; 2] = [
        &["-c:v", "libx264", "-preset", "veryfast", "-crf", "23", "-g", "1", "-pix_fmt", "yuv420p"],
        &["-c:v", "mjpeg", "-q:v", "4", "-pix_fmt", "yuvj420p"],
    ];
    let mut last = String::new();
    for codec in codecs {
        let mut cmd = Command::new("ffmpeg");
        cmd.args(["-v", "error", "-y", "-i"]).arg(src);
        cmd.args(["-map", "0:v:0", "-an", "-sn", "-fps_mode", "passthrough", "-vf", &scale]).args(codec).arg(&part);
        #[cfg(windows)]
        std::os::windows::process::CommandExt::creation_flags(&mut cmd, 0x0800_0000); // CREATE_NO_WINDOW
        match cmd.output() {
            Ok(o) if o.status.success() => {
                std::fs::rename(&part, &out).map_err(|e| e.to_string())?;
                prune(budget());
                return Ok(out);
            }
            Ok(o) => last = String::from_utf8_lossy(&o.stderr).trim().to_string(),
            Err(e) => return Err(format!("starting ffmpeg: {e}")),
        }
    }
    let _ = std::fs::remove_file(&part);
    Err(format!("proxy for {} failed: {last}", src.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use motion_core::Decoder;

    /// A proxy is smaller, keeps every frame, and is found again by its source;
    /// touching the source makes it stale.
    #[test]
    fn a_proxy_keeps_every_frame_at_a_lower_size() {
        let work = std::env::temp_dir().join(format!("pbc_proxy_{}", std::process::id()));
        std::fs::create_dir_all(&work).unwrap();
        std::env::set_var("PBC_PROXY_DIR", work.join("cache"));
        let src = work.join("src.mp4");
        let made = Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-f", "lavfi", "-i", "testsrc=size=1920x1080:rate=24:duration=1"])
            .args(["-c:v", "mpeg4", "-q:v", "5"])
            .arg(&src)
            .status();
        if !matches!(made, Ok(s) if s.success()) {
            eprintln!("skipping: couldn't generate a fixture");
            return;
        }
        assert!(existing(&src).is_none());
        let proxy = make(&src).expect("proxy");
        assert_eq!(existing(&src).as_deref(), Some(proxy.as_path()));
        let d = crate::decode::FfmpegDecoder::new();
        let (a, b) = (d.open(&src).unwrap(), d.open(&proxy).unwrap());
        assert_eq!(b.height, HEIGHT as f64);
        assert_eq!(b.width, 960.0);
        assert_eq!(a.frames, b.frames, "frame N of the source is frame N of the proxy");
        assert!(wanted(1920.0, 1080.0) && !wanted(960.0, 540.0));
        // A budget of nothing prunes it, and a stray half-made file with it.
        std::fs::write(proxy.with_extension("part.mov"), b"x").unwrap();
        let (n, freed) = prune(0);
        assert_eq!(n, 2);
        assert!(freed > 1 && existing(&src).is_none());
        let _ = std::fs::remove_dir_all(&work);
    }
}
