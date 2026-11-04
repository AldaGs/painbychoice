//! Speech to a word-timed [`Transcript`], through a **whisper.cpp sidecar**.
//!
//! The same call the video path makes: `whisper-cli` is run as a process
//! rather than linked, so the CUDA build (and its 1 GB of runtime) never has
//! to be built here, and a GPU or CPU build is just a different exe. The audio
//! goes in as a 16 kHz WAV written by [`write_wav`] — whatever the caller
//! mixed, so a comp mix comes back timed in comp time.
//!
//! Installs are found where the AE autoSUBS panel puts them; there is no
//! downloader yet.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};

use motion_core::subs::{Break, Transcript, Word};

use crate::audio::write_wav;

/// The rate whisper wants. Mix or read at this before calling [`transcribe`].
pub const RATE: u32 = 16_000;

/// Where whisper lives.
#[derive(Clone, Debug)]
pub struct Whisper {
    pub exe: PathBuf,
    pub model: PathBuf,
    /// Silero VAD model. Without it whisper glues leading silence onto the
    /// first word, so word starts come out early.
    pub vad: Option<PathBuf>,
    /// Run on the CPU even if the exe is a GPU build.
    pub cpu: bool,
}

impl Whisper {
    /// The autoSUBS install, or `PBC_WHISPER` (an exe) to override the exe.
    /// Prefers the CUDA build when an NVIDIA driver is present, and the largest
    /// model the install has.
    pub fn find() -> Option<Self> {
        let root = PathBuf::from(std::env::var_os("APPDATA")?).join("ae-autosubs-whisper");
        let models = root.join("models");
        let exe = std::env::var_os("PBC_WHISPER").map(PathBuf::from).or_else(|| {
            let cuda = root.join("cuda").join("whisper-cli.exe");
            let nvidia = Path::new(&std::env::var_os("SystemRoot").unwrap_or("C:\\Windows".into())).join("System32\\nvcuda.dll");
            (cuda.exists() && nvidia.exists()).then_some(cuda)
        })?;
        let model = ["large-v3-turbo", "small", "base"].iter().map(|m| models.join(format!("ggml-{m}.bin"))).find(|p| p.exists())?;
        let vad = Some(models.join("ggml-silero-v5.1.2.bin")).filter(|p| p.exists());
        Some(Self { exe, model, vad, cpu: false })
    }
}

/// Transcribe interleaved stereo `samples` at [`RATE`]. `language` is a
/// whisper code (`"es"`) or `None` to detect it. `progress` gets 0..100;
/// setting `cancel` kills whisper and returns `Err("cancelled")`.
pub fn transcribe(
    whisper: &Whisper,
    samples: &[f32],
    language: Option<&str>,
    mut progress: impl FnMut(u32),
    cancel: &AtomicBool,
) -> Result<Transcript, String> {
    let base = std::env::temp_dir().join(format!("pbc-whisper-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos())));
    let (wav, json) = (base.with_extension("wav"), base.with_extension("json"));
    let result = (|| {
        write_wav(&wav, samples, RATE).map_err(|e| format!("writing audio: {e}"))?;
        let mut cmd = Command::new(&whisper.exe);
        cmd.arg("-m").arg(&whisper.model).arg("-f").arg(&wav).arg("-l").arg(language.unwrap_or("auto"));
        // One word per segment, full JSON (token probabilities), progress on stderr.
        cmd.args(["-ojf", "-ml", "1", "-sow", "-pp", "-of"]).arg(&base);
        if let Some(v) = &whisper.vad {
            cmd.arg("--vad").arg("-vm").arg(v);
        }
        if whisper.cpu {
            cmd.arg("-ng");
        }
        #[cfg(windows)]
        std::os::windows::process::CommandExt::creation_flags(&mut cmd, 0x0800_0000); // CREATE_NO_WINDOW
        let mut child = cmd.stdout(Stdio::null()).stderr(Stdio::piped()).spawn().map_err(|e| format!("starting {}: {e}", whisper.exe.display()))?;

        // stderr on its own thread so the wait below can watch `cancel`.
        let (tx, rx) = std::sync::mpsc::channel();
        let stderr = child.stderr.take().unwrap();
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                let _ = tx.send(line);
            }
        });
        let mut log = String::new();
        let status = loop {
            for line in rx.try_iter() {
                match line.split_once("progress =").and_then(|(_, p)| p.trim().trim_end_matches('%').parse().ok()) {
                    Some(p) => progress(p),
                    None => log.push_str(&line),
                }
            }
            if cancel.load(Ordering::Relaxed) {
                let _ = child.kill();
                return Err("cancelled".to_string());
            }
            if let Some(s) = child.try_wait().map_err(|e| e.to_string())? {
                break s;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        };
        let out = std::fs::read_to_string(&json).ok().filter(|_| status.success());
        let tail = &log[log.len().saturating_sub(500)..];
        let out = out.ok_or_else(|| format!("whisper failed ({status}): {tail}"))?;
        normalize(&out)
    })();
    let _ = std::fs::remove_file(&wav);
    let _ = std::fs::remove_file(&json);
    result
}

/// whisper's `-ojf` JSON → a transcript. Bracketed segments (`[MUSIC]`) are
/// dropped; `p` is the mean probability of the word's non-special tokens.
pub fn normalize(json: &str) -> Result<Transcript, String> {
    let v: serde_json::Value = serde_json::from_str(json).map_err(|e| format!("whisper output: {e}"))?;
    let mut words = Vec::new();
    for s in v["transcription"].as_array().into_iter().flatten() {
        let text = s["text"].as_str().unwrap_or("").trim();
        if text.is_empty() || (text.starts_with('[') && text.ends_with(']')) {
            continue;
        }
        let start = s["offsets"]["from"].as_f64().unwrap_or(0.0) / 1000.0;
        // ponytail: a word before a pause can swallow the silence; capped at 1.5 s like autoSUBS, a VAD-aware end would be better
        let end = (s["offsets"]["to"].as_f64().unwrap_or(0.0) / 1000.0).min(start + 1.5);
        let ps: Vec<f64> = s["tokens"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|t| !t["text"].as_str().is_some_and(|t| t.starts_with("[_") && t.ends_with(']')))
            .filter_map(|t| t["p"].as_f64())
            .collect();
        let p = (!ps.is_empty()).then(|| (ps.iter().sum::<f64>() / ps.len() as f64 * 100.0).round() / 100.0);
        words.push(Word { text: text.to_string(), start, end, p, brk: Break::Auto, locked: false });
    }
    let language = v["result"]["language"].as_str().filter(|l| !l.is_empty()).map(str::to_string);
    Ok(Transcript { language, words })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_whisper_json() {
        let t = normalize(
            r#"{"result":{"language":"es"},"transcription":[
                {"text":" Hola","offsets":{"from":1000,"to":5000},"tokens":[{"text":"[_BEG_]","p":0.1},{"text":" Hola","p":0.8},{"text":"","p":0.6}]},
                {"text":" [MUSIC]","offsets":{"from":5000,"to":6000},"tokens":[]},
                {"text":" mundo.","offsets":{"from":5200,"to":5600},"tokens":[{"text":" mundo.","p":0.9}]}]}"#,
        )
        .unwrap();
        assert_eq!(t.language.as_deref(), Some("es"));
        let w: Vec<_> = t.words.iter().map(|w| (w.text.as_str(), w.start, w.end, w.p)).collect();
        assert_eq!(w, [("Hola", 1.0, 2.5, Some(0.7)), ("mundo.", 5.2, 5.6, Some(0.9))]);
    }

    /// The real thing, on this machine's autoSUBS install:
    /// `PBC_AUDIO=<file> cargo test -p motion-render transcribe::tests::real -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn real() {
        let sound = crate::audio::decode_sound(Path::new(&std::env::var("PBC_AUDIO").unwrap())).unwrap();
        let n = (sound.seconds() * RATE as f64) as usize;
        let whisper = Whisper::find().expect("no whisper install found");
        let t = transcribe(&whisper, &sound.read(0, n, RATE), None, |p| eprint!("{p}% "), &AtomicBool::new(false)).unwrap();
        println!("\n{:?} {} words", t.language, t.words.len());
        for w in t.words.iter().take(10) {
            println!("{:.2}-{:.2} {:?} {}", w.start, w.end, w.p, w.text);
        }
    }
}
