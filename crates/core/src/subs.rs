//! Subtitles from a word-timed transcript.
//!
//! A [`Transcript`] is what speech recognition hands back: words with times in
//! **seconds** (the audio's own clock, so a comp changing frame rate never
//! invalidates it) and how sure the recogniser was of each. [`layout`] turns it
//! into [`Subtitle`]s on the comp's frame grid. Nothing is stored about the
//! layout itself — change a rule and the subtitles are simply laid out again.
//!
//! Manual edits win: a [`Word::locked`] word is placed only by its own
//! [`Break`], never by the automatic rules, and the subtitle it starts keeps
//! its timing exactly.
//!
//! The JSON shape is the one the AE autoSUBS panel writes (`<name>.subs.json`),
//! so a transcript made there loads here unchanged.

use std::ops::Range;

use serde::{Deserialize, Serialize};

use crate::timebase::Timebase;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Transcript {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    pub words: Vec<Word>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Word {
    pub text: String,
    /// Seconds.
    pub start: f64,
    /// Seconds.
    pub end: f64,
    /// Recogniser confidence, 0..1. Absent for typed-in words.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub p: Option<f64>,
    #[serde(default, rename = "br", skip_serializing_if = "Break::is_auto")]
    pub brk: Break,
    #[serde(default, rename = "lk", skip_serializing_if = "std::ops::Not::not")]
    pub locked: bool,
}

/// What follows a word.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "BreakJson", into = "BreakJson")]
pub enum Break {
    /// The rules decide.
    #[default]
    Auto,
    /// Nothing, whatever the rules say (max chars still wraps).
    None,
    /// A new line in the same subtitle.
    Line,
    /// A new subtitle.
    Subtitle,
}

impl Break {
    fn is_auto(&self) -> bool {
        *self == Break::Auto
    }
}

/// `br` on disk: `true`, `false` or `"line"`, as autoSUBS writes it.
#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum BreakJson {
    Bool(bool),
    Str(String),
}

impl TryFrom<BreakJson> for Break {
    type Error = String;
    fn try_from(j: BreakJson) -> Result<Self, String> {
        match j {
            BreakJson::Bool(true) => Ok(Break::Subtitle),
            BreakJson::Bool(false) => Ok(Break::None),
            BreakJson::Str(s) if s == "line" => Ok(Break::Line),
            BreakJson::Str(s) => Err(format!("unknown break {s:?}")),
        }
    }
}

impl From<Break> for BreakJson {
    fn from(b: Break) -> Self {
        match b {
            // Never written: `Auto` is skipped. Kept total for `into`.
            Break::Auto | Break::None => BreakJson::Bool(false),
            Break::Subtitle => BreakJson::Bool(true),
            Break::Line => BreakJson::Str("line".into()),
        }
    }
}

/// The automatic layout rules. Defaults match the autoSUBS panel's.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Rules {
    pub words_per_line: usize,
    pub max_lines: usize,
    /// 0 = no limit.
    pub max_chars: usize,
    /// A silence longer than this (seconds) ends the subtitle.
    pub pause_break: f64,
    pub min_frames: i64,
    /// A gap shorter than this is closed by stretching the earlier subtitle.
    pub close_gap_frames: i64,
}

impl Default for Rules {
    fn default() -> Self {
        Self { words_per_line: 5, max_lines: 1, max_chars: 0, pause_break: 0.6, min_frames: 12, close_gap_frames: 6 }
    }
}

/// One on-screen subtitle: word index ranges into the transcript, and frames.
#[derive(Clone, Debug, PartialEq)]
pub struct Subtitle {
    pub words: Range<usize>,
    pub lines: Vec<Range<usize>>,
    /// First frame shown.
    pub start: i64,
    /// First frame no longer shown.
    pub end: i64,
    pub locked: bool,
}

impl Subtitle {
    /// The lines' words joined by spaces, lines by `\n`.
    pub fn text(&self, words: &[Word]) -> String {
        let line = |r: &Range<usize>| words[r.clone()].iter().map(|w| w.text.as_str()).collect::<Vec<_>>().join(" ");
        self.lines.iter().map(line).collect::<Vec<_>>().join("\n")
    }

    /// Index of the last word started by `frame` (the karaoke word), holding
    /// through gaps between words; `None` before the first one starts.
    pub fn active_word(&self, words: &[Word], frame: i64, tb: &Timebase) -> Option<usize> {
        let t = tb.frames_to_seconds(frame as f64) + 1e-6;
        self.words.clone().take_while(|&i| words[i].start <= t).last()
    }
}

pub fn layout(words: &[Word], rules: &Rules, tb: &Timebase) -> Vec<Subtitle> {
    let max_lines = rules.max_lines.max(1);
    let wpl = rules.words_per_line.max(1);
    let mut subs: Vec<Vec<Range<usize>>> = Vec::new();
    let mut lines: Vec<Range<usize>> = Vec::new();
    let mut line_start = 0;
    let mut len = 0;

    // Close the line ending before word `end`; then, if `full`, the subtitle.
    let end_line = |lines: &mut Vec<Range<usize>>, line_start: &mut usize, len: &mut usize, end: usize| {
        if end > *line_start {
            lines.push(*line_start..end);
        }
        *line_start = end;
        *len = 0;
    };
    let end_sub = |subs: &mut Vec<Vec<Range<usize>>>, lines: &mut Vec<Range<usize>>| {
        if !lines.is_empty() {
            subs.push(std::mem::take(lines));
        }
    };

    for (i, w) in words.iter().enumerate() {
        let chars = w.text.chars().count();
        let in_line = i - line_start;
        if in_line > 0 && !w.locked && rules.max_chars > 0 && len + 1 + chars > rules.max_chars {
            end_line(&mut lines, &mut line_start, &mut len, i);
            if lines.len() >= max_lines {
                end_sub(&mut subs, &mut lines);
            }
        }
        len += if i > line_start { 1 } else { 0 } + chars;
        let next = words.get(i + 1);
        let after = i + 1;
        let new_line = |lines: &mut Vec<Range<usize>>, subs: &mut Vec<Vec<Range<usize>>>, ls: &mut usize, len: &mut usize| {
            end_line(lines, ls, len, after);
            if lines.len() >= max_lines {
                end_sub(subs, lines);
            }
        };
        match w.brk {
            Break::Subtitle => {
                end_line(&mut lines, &mut line_start, &mut len, after);
                end_sub(&mut subs, &mut lines);
            }
            Break::Line if w.locked => end_line(&mut lines, &mut line_start, &mut len, after),
            Break::Line => new_line(&mut lines, &mut subs, &mut line_start, &mut len),
            Break::Auto if !w.locked => {
                let sentence = w.text.ends_with(['.', '?', '!']);
                let pause = next.is_some_and(|n| n.start - w.end > rules.pause_break);
                if sentence || pause {
                    end_line(&mut lines, &mut line_start, &mut len, after);
                    end_sub(&mut subs, &mut lines);
                } else if after - line_start >= wpl {
                    new_line(&mut lines, &mut subs, &mut line_start, &mut len);
                }
            }
            _ => {}
        }
    }
    end_line(&mut lines, &mut line_start, &mut len, words.len());
    end_sub(&mut subs, &mut lines);

    // Onto the frame grid: start floors and end ceils so every word is fully
    // covered; the epsilon keeps a time already on a frame from slipping one.
    let fps = tb.fps();
    let mut out: Vec<Subtitle> = subs
        .into_iter()
        .map(|lines| {
            let words_r = lines[0].start..lines[lines.len() - 1].end;
            let locked = words[words_r.start].locked;
            let start = (words[words_r.start].start * fps + 1e-6).floor() as i64;
            let end = (words[words_r.end - 1].end * fps - 1e-6).ceil() as i64;
            let end = end.max(start + if locked { 1 } else { rules.min_frames });
            Subtitle { words: words_r, lines, start, end, locked }
        })
        .collect();
    for i in 1..out.len() {
        let next = out[i].start;
        let prev = &mut out[i - 1];
        if (!prev.locked && next - prev.end < rules.close_gap_frames) || prev.end > next {
            prev.end = next;
        }
    }
    out
}

/// A transcript on a text layer: what [`crate::node::Node::subtitles`] holds.
/// Word times count from the layer's own start.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Subtitles {
    pub transcript: Transcript,
    #[serde(default)]
    pub rules: Rules,
}

impl Subtitles {
    /// The text on screen at local `frame` (empty between subtitles), and the
    /// karaoke word's index within it.
    pub fn at(&self, frame: f64, fps: f64) -> (String, Option<usize>) {
        let tb = Timebase::new(fps);
        let words = &self.transcript.words;
        // ponytail: lays out the whole transcript every frame, O(words); cache on (words, rules, fps) if long ones get slow
        let subs = layout(words, &self.rules, &tb);
        let frame = frame.floor() as i64;
        match at(&subs, frame) {
            Some(s) => (s.text(words), s.active_word(words, frame, &tb).map(|i| i - s.words.start)),
            None => (String::new(), None),
        }
    }
}

// Edits. Each works on the words of one subtitle (a word index range) and
// leaves the result locked: manual edits win over the automatic layout.

/// Edited text → words, each flagged when a line break (`\n`) follows it.
fn tokens(text: &str) -> Vec<(&str, bool)> {
    let lines: Vec<Vec<&str>> = text.lines().map(|l| l.split_whitespace().collect::<Vec<_>>()).filter(|l| !l.is_empty()).collect();
    let n = lines.len();
    lines.iter().enumerate().flat_map(|(i, l)| l.iter().enumerate().map(move |(j, t)| (*t, i + 1 < n && j + 1 == l.len()))).collect()
}

/// The text to edit for `r`: only *manual* line breaks become `\n`, so the
/// automatic wrapping isn't pinned by committing it unchanged.
pub fn edit_text(words: &[Word], r: Range<usize>) -> String {
    let mut s = String::new();
    for i in r.clone() {
        s += &words[i].text;
        if i + 1 < r.end {
            s.push(if words[i].brk == Break::Line { '\n' } else { ' ' });
        }
    }
    s
}

/// Replace words `r` with `text`. Words that survive (a longest common
/// subsequence) keep their timing; new ones share the time between their kept
/// neighbours by length; deleted ones go. Returns the new words' range.
fn retime(words: &mut Vec<Word>, r: Range<usize>, text: &str) -> Range<usize> {
    let toks = tokens(text);
    let old: Vec<Word> = words[r.clone()].to_vec();
    if toks.is_empty() || old.is_empty() {
        words.splice(r.clone(), []);
        return r.start..r.start;
    }
    // LCS table over the texts.
    let (n, m) = (old.len(), toks.len());
    let mut d = vec![vec![0usize; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            d[i][j] = if old[i].text == toks[j].0 { d[i + 1][j + 1] + 1 } else { d[i + 1][j].max(d[i][j + 1]) };
        }
    }
    let mut out: Vec<Option<Word>> = vec![None; m];
    let (mut i, mut j) = (0, 0);
    while i < n && j < m {
        if old[i].text == toks[j].0 {
            out[j] = Some(old[i].clone());
            (i, j) = (i + 1, j + 1);
        } else if d[i + 1][j] >= d[i][j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    let mut j = 0;
    while j < m {
        if out[j].is_some() {
            j += 1;
            continue;
        }
        let e = (j..m).find(|&k| out[k].is_some()).unwrap_or(m);
        let mut from = if j > 0 { out[j - 1].as_ref().unwrap().end } else { old[0].start };
        let to = if e < m { out[e].as_ref().unwrap().start } else { old[n - 1].end };
        let mut span = (to - from).max(0.0);
        // No room (inserted between touching words): take the back half of
        // the previous word, or the front half of the next.
        if span < 0.05 * (e - j) as f64 {
            if j > 0 {
                let p = out[j - 1].as_mut().unwrap();
                from = (p.start + p.end) / 2.0;
                span += p.end - from;
                p.end = from;
            } else if e < m {
                let q = out[e].as_mut().unwrap();
                let mid = (q.start + q.end) / 2.0;
                span = mid - from;
                q.start = mid;
            }
        }
        let total: usize = toks[j..e].iter().map(|t| t.0.chars().count()).sum();
        for k in j..e {
            let len = span * toks[k].0.chars().count() as f64 / total.max(1) as f64;
            out[k] = Some(Word { text: toks[k].0.to_string(), start: from, end: from + len, p: None, brk: Break::Auto, locked: false });
            from += len;
        }
        j = e;
    }
    let last_brk = old[n - 1].brk;
    let mut out: Vec<Word> = out.into_iter().map(Option::unwrap).collect();
    for (k, w) in out.iter_mut().enumerate() {
        w.brk = if k + 1 == m {
            if last_brk == Break::Line { Break::Auto } else { last_brk }
        } else if toks[k].1 {
            Break::Line
        } else if w.brk == Break::Line {
            Break::Auto
        } else {
            w.brk
        };
    }
    words.splice(r.clone(), out);
    r.start..r.start + m
}

/// Lock words `r` as exactly one subtitle, breaking lines where a word's
/// break is already [`Break::Line`] or it is listed in `line_ends`.
pub fn lock(words: &mut [Word], r: Range<usize>, line_ends: &[usize]) {
    if r.is_empty() {
        return;
    }
    for i in r.clone() {
        let w = &mut words[i];
        w.locked = true;
        w.brk = if i + 1 == r.end {
            Break::Subtitle
        } else if line_ends.contains(&i) || w.brk == Break::Line {
            Break::Line
        } else {
            Break::Auto
        };
    }
    if r.start > 0 {
        words[r.start - 1].brk = Break::Subtitle;
    }
}

/// Back to the automatic layout for words `r`.
pub fn unlock(words: &mut [Word], r: Range<usize>) {
    for w in &mut words[r] {
        w.locked = false;
        w.brk = Break::Auto;
    }
}

/// Commit edited `text` for words `r` as one locked subtitle.
pub fn edit(words: &mut Vec<Word>, r: Range<usize>, text: &str) {
    let r = retime(words, r, text);
    lock(words, r, &[]);
}

/// Split words `r` at the cursor: `before` and `after` become two locked
/// subtitles.
pub fn split(words: &mut Vec<Word>, r: Range<usize>, before: &str, after: &str) {
    let (before, after) = (before.trim_end(), after.trim_start());
    let nb = tokens(before).len();
    let r = retime(words, r, &format!("{before}\n{after}"));
    if nb == 0 || nb >= r.len() {
        return lock(words, r, &[]);
    }
    lock(words, r.start + nb..r.end, &[]);
    lock(words, r.start..r.start + nb, &[]);
}

/// Join `this` subtitle onto the one before it, keeping both's lines.
pub fn merge(words: &mut [Word], prev: &Subtitle, this: &Subtitle) {
    let ends: Vec<usize> = prev.lines.iter().map(|l| l.end - 1).collect();
    lock(words, prev.words.start..this.words.end, &ends);
}

/// The subtitle on screen at `frame`.
pub fn at(subs: &[Subtitle], frame: i64) -> Option<&Subtitle> {
    subs.iter().find(|s| s.start <= frame && frame < s.end)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(text: &str, start: f64, end: f64) -> Word {
        Word { text: text.into(), start, end, p: None, brk: Break::Auto, locked: false }
    }

    fn texts(words: &[Word], rules: &Rules) -> Vec<String> {
        layout(words, rules, &Timebase::new(24.0)).iter().map(|s| s.text(words)).collect()
    }

    /// A run of words: one per half second, touching, so no pause breaks.
    fn run(text: &str) -> Vec<Word> {
        text.split(' ').enumerate().map(|(i, t)| w(t, i as f64 * 0.5, i as f64 * 0.5 + 0.5)).collect()
    }

    #[test]
    fn words_per_line_and_lines_fill_subtitles() {
        let words = run("a b c d e f g");
        let r = Rules { words_per_line: 2, max_lines: 2, ..Rules::default() };
        assert_eq!(texts(&words, &r), ["a b\nc d", "e f\ng"]);
    }

    #[test]
    fn punctuation_and_pauses_end_a_subtitle() {
        let mut words = run("hi. you there");
        words[2].start += 1.0;
        words[2].end += 1.0;
        assert_eq!(texts(&words, &Rules::default()), ["hi.", "you", "there"]);
    }

    #[test]
    fn max_chars_wraps_before_the_word_that_overflows() {
        let words = run("aaaa bbbb cc");
        let r = Rules { max_chars: 9, max_lines: 2, ..Rules::default() };
        assert_eq!(texts(&words, &r), ["aaaa bbbb\ncc"]);
    }

    #[test]
    fn a_locked_subtitle_ignores_the_rules() {
        let mut words = run("one two three. four");
        for w in &mut words[..3] {
            w.locked = true;
        }
        words[0].brk = Break::Line;
        words[2].brk = Break::Subtitle;
        let r = Rules { words_per_line: 1, ..Rules::default() };
        assert_eq!(texts(&words, &r), ["one\ntwo three.", "four"]);
    }

    #[test]
    fn timing_snaps_to_frames_with_min_duration_and_gap_closing() {
        let tb = Timebase::new(24.0);
        // 0.5s = frame 12 exactly; must not floor to 11.
        let words = vec![w("a.", 0.5, 0.52), w("b.", 1.2, 1.3), w("c", 3.0, 3.1)];
        let s = layout(&words, &Rules::default(), &tb);
        // a: min duration takes it to 24, 4 frames short of b (28): closed.
        assert_eq!((s[0].start, s[0].end, s[1].start), (12, 28, 28));
        // b: min pushes its end to 40; c starts at 72, a 32-frame gap kept.
        assert_eq!((s[1].end, s[2].start), (40, 72));
    }

    #[test]
    fn karaoke_word_holds_through_gaps() {
        let tb = Timebase::new(10.0);
        let words = vec![w("a", 0.0, 0.2), w("b", 0.5, 0.7)];
        let s = &layout(&words, &Rules::default(), &tb)[0];
        assert_eq!(s.active_word(&words, 3, &tb), Some(0));
        assert_eq!(s.active_word(&words, 5, &tb), Some(1));
    }

    #[test]
    fn reads_autosubs_json() {
        let t: Transcript = serde_json::from_str(
            r#"{"version":1,"language":"es","words":[
                {"text":"¿Dónde","start":1.35,"end":2.85,"p":0.84,"br":"line","lk":true},
                {"text":"andan?","start":2.97,"end":3.54,"br":true}]}"#,
        )
        .unwrap();
        assert_eq!((t.words[0].brk, t.words[0].locked, t.words[1].brk), (Break::Line, true, Break::Subtitle));
        let back = serde_json::to_string(&t.words[1]).unwrap();
        assert!(back.contains(r#""br":true"#) && !back.contains("lk"), "{back}");
    }

    #[test]
    fn editing_keeps_unchanged_words_timing_and_locks() {
        let mut words = run("the quick fox jumps");
        edit(&mut words, 0..4, "the quick brown\nfox");
        let got: Vec<_> = words.iter().map(|w| (w.text.as_str(), w.start, w.end, w.brk, w.locked)).collect();
        // "brown" is new: it takes the back half of "quick", which touches "fox".
        // "jumps" is deleted. Ends locked as one subtitle.
        assert_eq!(
            got,
            [
                ("the", 0.0, 0.5, Break::Auto, true),
                ("quick", 0.5, 0.75, Break::Auto, true),
                ("brown", 0.75, 1.0, Break::Line, true),
                ("fox", 1.0, 1.5, Break::Subtitle, true),
            ]
        );
        assert_eq!(texts(&words, &Rules::default()), ["the quick brown\nfox"]);
        assert_eq!(edit_text(&words, 0..4), "the quick brown\nfox");
    }

    #[test]
    fn split_then_merge_round_trips() {
        let mut words = run("a b c d");
        split(&mut words, 0..4, "a b ", "c d");
        assert_eq!(texts(&words, &Rules::default()), ["a b", "c d"]);
        let subs = layout(&words, &Rules::default(), &Timebase::new(24.0));
        merge(&mut words, &subs[0], &subs[1]);
        assert_eq!(texts(&words, &Rules::default()), ["a b\nc d"]);
        unlock(&mut words, 0..4);
        assert_eq!(texts(&words, &Rules::default()), ["a b c d"]);
    }

    /// Parity with autoSUBS: `SUBS_JSON=<file> cargo test -p motion-core subs::tests::dump -- --ignored --nocapture`
    /// prints `start end text` at 24 fps, default rules, for diffing against `buildLines`.
    #[test]
    #[ignore]
    fn dump() {
        let t: Transcript = serde_json::from_str(&std::fs::read_to_string(std::env::var("SUBS_JSON").unwrap()).unwrap()).unwrap();
        for s in layout(&t.words, &Rules::default(), &Timebase::new(24.0)) {
            println!("{} {} {}", s.start, s.end, s.text(&t.words).replace('\n', "|"));
        }
    }
}
