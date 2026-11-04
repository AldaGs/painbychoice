//! The Subtitles panel: transcribe the comp's audio onto a text layer, set the
//! layout rules, and read and edit the result as a document — one block per
//! subtitle, the spoken word lit while playing.
//!
//! Like every panel it only *reports* what the user did ([`SubsEdit`]); the
//! app applies it after the UI pass, so undo covers it for free. All the
//! editing logic is in [`motion_core::subs`]; this file is layout.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::Receiver;

use motion_core::timebase::Timebase;
use motion_core::subs::{self, Rules, Subtitle, Subtitles, Transcript, Word};

use crate::*;

/// What the panel asks for this frame.
#[derive(Default)]
pub(crate) struct SubsEdits {
    pub(crate) transcribe: bool,
    pub(crate) cancel: bool,
    pub(crate) rules: Option<Rules>,
    pub(crate) words: Option<Vec<Word>>,
    /// Highlight color and cumulative, for the layer's Karaoke animator.
    pub(crate) karaoke: Option<([f32; 3], bool)>,
    /// Comp frame.
    pub(crate) seek: Option<i64>,
}

/// A transcription running on its own thread.
pub(crate) struct SubsJob {
    pub(crate) node: NodeId,
    pub(crate) rx: Receiver<Result<Transcript, String>>,
    pub(crate) progress: std::sync::Arc<AtomicU32>,
    pub(crate) cancel: std::sync::Arc<AtomicBool>,
}

/// Panel-local state that outlives a frame.
#[derive(Default)]
pub(crate) struct SubsUi {
    /// The subtitle being edited (its first word) and its text so far.
    editing: Option<(usize, String)>,
    /// The subtitle lit last frame, to scroll only when it changes.
    shown: Option<usize>,
    pub(crate) status: String,
}

/// What the panel reads.
pub(crate) struct SubsView<'a> {
    /// The selected layer, if it is a text layer: its subtitles (if any), its
    /// local-time start, and its Karaoke animator's color and cumulative.
    pub(crate) layer: Option<(Option<&'a Subtitles>, i64, Option<([f32; 3], bool)>)>,
    pub(crate) frame: i64,
    pub(crate) timebase: Timebase,
    pub(crate) progress: Option<u32>,
    /// Follow the lit subtitle only while playing: a click that seeks must not
    /// scroll the list out from under a double-click.
    pub(crate) playing: bool,
}

const ACCENT: egui::Color32 = egui::Color32::from_rgb(70, 140, 255);
const KARAOKE: egui::Color32 = egui::Color32::from_rgb(255, 210, 60);
const SHAKY: egui::Color32 = egui::Color32::from_rgb(240, 150, 60);

pub(crate) fn subs_ui(ui: &mut egui::Ui, v: &SubsView, st: &mut SubsUi, out: &mut SubsEdits) {
    match v.progress {
        Some(p) => {
            ui.horizontal(|ui| {
                ui.add(egui::ProgressBar::new(p as f32 / 100.0).text(format!("Transcribing {p}%")).desired_width(160.0));
                out.cancel |= ui.button("Cancel").clicked();
            });
        }
        None => {
            let label = match v.layer {
                Some((Some(_), ..)) => "Transcribe again",
                Some((None, ..)) => "Transcribe into layer",
                None => "Transcribe comp",
            };
            out.transcribe |= ui
                .button(label)
                .on_hover_text("Runs whisper on the comp's mixed audio. With a text layer selected, it fills that layer; otherwise it makes a new one.")
                .clicked();
        }
    }
    if !st.status.is_empty() {
        ui.label(egui::RichText::new(&st.status).small().weak());
    }
    let Some((Some(s), start, karaoke)) = v.layer else { return };
    ui.separator();

    let mut r = s.rules.clone();
    ui.horizontal_wrapped(|ui| {
        for (name, wpl, lines) in [("16:9", 7, 2), ("9:16", 3, 2), ("1:1", 4, 2)] {
            if ui.selectable_label(r.words_per_line == wpl && r.max_lines == lines, name).clicked() {
                (r.words_per_line, r.max_lines) = (wpl, lines);
            }
        }
    });
    egui::Grid::new("subs_rules").num_columns(2).show(ui, |ui| {
        let row = |ui: &mut egui::Ui, name: &str, w: egui::DragValue| {
            ui.label(name);
            ui.add(w);
            ui.end_row();
        };
        row(ui, "Words/line", egui::DragValue::new(&mut r.words_per_line).range(1..=20));
        row(ui, "Lines", egui::DragValue::new(&mut r.max_lines).range(1..=3));
        row(ui, "Max chars", egui::DragValue::new(&mut r.max_chars).range(0..=200));
        let mut ms = (r.pause_break * 1000.0).round();
        row(ui, "Pause break (ms)", egui::DragValue::new(&mut ms).range(0.0..=5000.0).speed(10.0));
        r.pause_break = ms / 1000.0;
        row(ui, "Min frames", egui::DragValue::new(&mut r.min_frames).range(0..=240));
        row(ui, "Close gap frames", egui::DragValue::new(&mut r.close_gap_frames).range(0..=240));
    });
    if r != s.rules {
        out.rules = Some(r);
    }
    if let Some((mut rgb, mut cumulative)) = karaoke {
        ui.horizontal(|ui| {
            ui.label("Highlight");
            let c = crate::props::color_button(ui, &mut rgb).changed();
            if c | ui.checkbox(&mut cumulative, "Cumulative").changed() {
                out.karaoke = Some((rgb, cumulative));
            }
        });
    }
    ui.separator();

    let words = &s.transcript.words;
    let tb = &v.timebase;
    let list = subs::layout(words, &s.rules, tb);
    let local = v.frame - start;
    let lit = subs::at(&list, local).map(|x| x.words.start);
    let scroll = v.playing && lit.is_some() && lit != st.shown;
    st.shown = lit;
    for (k, sub) in list.iter().enumerate() {
        ui.push_id(sub.words.start, |ui| block(ui, v, st, out, words, &list, k, start, lit == Some(sub.words.start) && scroll));
    }
}

/// One subtitle: its timecode and lock, then its lines (or the editor).
#[allow(clippy::too_many_arguments)]
fn block(ui: &mut egui::Ui, v: &SubsView, st: &mut SubsUi, out: &mut SubsEdits, words: &[Word], list: &[Subtitle], k: usize, start: i64, scroll: bool) {
    let sub = &list[k];
    let local = v.frame - start;
    let lit = sub.start <= local && local < sub.end;
    let top = ui.cursor().min;
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(v.timebase.timecode(sub.start as f64)).monospace().weak());
        if sub.locked
            && ui.small_button("🔒").on_hover_text("Edited by hand. Click to return it to automatic layout.").clicked()
        {
            let mut w = words.to_vec();
            subs::unlock(&mut w, sub.words.clone());
            out.words = Some(w);
        }
        if st.editing.is_none() && ui.small_button("✏").on_hover_text("Edit (or double-click the text)").clicked() {
            st.editing = Some((sub.words.start, subs::edit_text(words, sub.words.clone())));
        }
    });
    if st.editing.as_ref().is_some_and(|e| e.0 == sub.words.start) {
        editor(ui, st, out, words, list, k);
    } else {
        let active = lit.then(|| sub.active_word(words, local, &v.timebase)).flatten();
        let mut open = false;
        for line in &sub.lines {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                for i in line.clone() {
                    let w = &words[i];
                    let mut t = egui::RichText::new(&w.text);
                    if active == Some(i) {
                        t = t.color(egui::Color32::BLACK).background_color(KARAOKE);
                    } else if w.p.is_some_and(|p| p < 0.5) {
                        t = t.color(SHAKY).underline();
                    }
                    let r = ui.add(egui::Label::new(t).sense(egui::Sense::click()));
                    if r.clicked() {
                        out.seek = Some(start + v.timebase.seconds_to_frames(w.start));
                    }
                    open |= r.double_clicked();
                }
            });
        }
        if open {
            st.editing = Some((sub.words.start, subs::edit_text(words, sub.words.clone())));
        }
    }
    let rect = egui::Rect::from_min_max(top, egui::pos2(top.x + 3.0, ui.cursor().min.y));
    if lit {
        ui.painter().rect_filled(rect.translate(egui::vec2(-6.0, 0.0)), 0.0, ACCENT);
        if scroll {
            ui.scroll_to_rect(rect, Some(egui::Align::Center));
        }
    }
    ui.add_space(6.0);
}

/// The in-place editor. Enter splits at the cursor, Shift+Enter breaks the
/// line, Backspace at the start merges into the previous subtitle, Esc
/// cancels, Ctrl+Enter or clicking away commits.
fn editor(ui: &mut egui::Ui, st: &mut SubsUi, out: &mut SubsEdits, words: &[Word], list: &[Subtitle], k: usize) {
    let sub = &list[k];
    let Some((_, text)) = st.editing.as_mut() else { return };
    let id = ui.id().with("edit");
    let cursor = egui::text_edit::TextEditState::load(ui.ctx(), id)
        .and_then(|s| s.cursor.char_range())
        .map(|r| (usize::from(r.primary.index), r.is_empty()));
    let focused = ui.memory(|m| m.has_focus(id));
    let (enter, ctrl_enter, esc, back) = ui.input_mut(|i| {
        if !focused {
            return (false, false, false, false);
        }
        let ctrl_enter = i.consume_key(egui::Modifiers::COMMAND, egui::Key::Enter);
        let enter = i.consume_key(egui::Modifiers::NONE, egui::Key::Enter);
        let esc = i.consume_key(egui::Modifiers::NONE, egui::Key::Escape);
        let back = k > 0 && cursor == Some((0, true)) && i.consume_key(egui::Modifiers::NONE, egui::Key::Backspace);
        (enter, ctrl_enter, esc, back)
    });
    let r = ui.add(egui::TextEdit::multiline(text).id(id).desired_rows(2).desired_width(f32::INFINITY));
    if !focused && !r.lost_focus() {
        r.request_focus();
    }
    let mut w = words.to_vec();
    if esc {
        st.editing = None;
    } else if back {
        subs::merge(&mut w, &list[k - 1], sub);
        out.words = Some(w);
        st.editing = None;
    } else if enter {
        let at = cursor.map_or(text.chars().count(), |c| c.0);
        let split = text.char_indices().nth(at).map_or(text.len(), |(b, _)| b);
        subs::split(&mut w, sub.words.clone(), &text[..split], &text[split..]);
        out.words = Some(w);
        st.editing = None;
    } else if ctrl_enter || r.lost_focus() {
        if *text != subs::edit_text(words, sub.words.clone()) {
            subs::edit(&mut w, sub.words.clone(), text);
            out.words = Some(w);
        }
        st.editing = None;
    }
}

/// The color and cumulative of a layer's first Karaoke animator.
pub(crate) fn karaoke_of(node: &MNode) -> Option<([f32; 3], bool)> {
    node.text_animators.iter().find_map(|a| match (&a.selector, &a.fill) {
        (motion_core::node::TextSelector::Karaoke { cumulative }, Some(f)) => {
            let c = f.resolve(&mut motion_core::expr::EvalCtx::at(0.0));
            Some(([c.r as f32, c.g as f32, c.b as f32], *cumulative))
        }
        _ => None,
    })
}

/// Set (or add) a layer's Karaoke animator.
pub(crate) fn set_karaoke(node: &mut MNode, rgb: [f32; 3], cumulative: bool) {
    use motion_core::node::{TextAnimator, TextSelector, TextUnit};
    let fill = Some(Value::constant(motion_core::Color::rgb(rgb[0] as f64, rgb[1] as f64, rgb[2] as f64)));
    let selector = TextSelector::Karaoke { cumulative };
    match node.text_animators.iter_mut().find(|a| matches!(a.selector, TextSelector::Karaoke { .. })) {
        Some(a) => (a.selector, a.fill) = (selector, fill),
        None => node.text_animators.push(TextAnimator { based_on: TextUnit::Words, selector, position: Vec2::ZERO, opacity: None, fill }),
    }
}

/// Start transcribing the comp's whole mix on a thread.
pub(crate) fn start_job(
    project: MProject,
    comp: motion_core::CompId,
    sounds: std::collections::HashMap<motion_core::AssetId, std::sync::Arc<motion_render::Sound>>,
    node: NodeId,
) -> Result<SubsJob, String> {
    use motion_render::transcribe;
    let whisper = transcribe::Whisper::find().ok_or("No whisper install found (expects the autoSUBS one in %APPDATA%, or PBC_WHISPER)")?;
    let (tx, rx) = std::sync::mpsc::channel();
    let progress = std::sync::Arc::new(AtomicU32::new(0));
    let cancel = std::sync::Arc::new(AtomicBool::new(false));
    let (p, c) = (progress.clone(), cancel.clone());
    std::thread::spawn(move || {
        let end = project.comp(comp).map_or(0, |c| c.duration_frames);
        let mix = motion_render::mix_comp(&project, comp, 0, end, transcribe::RATE, &sounds);
        let _ = tx.send(transcribe::transcribe(&whisper, &mix, None, |x| p.store(x, Ordering::Relaxed), &c));
    });
    Ok(SubsJob { node, rx, progress, cancel })
}

/// A fresh subtitle layer: bottom-centred white text, yellow karaoke.
pub(crate) fn new_layer(id: NodeId, w: f64, h: f64) -> MNode {
    let mut n = MNode::shape(
        id.0,
        "Subtitles",
        MShape::Text {
            content: Value::constant(String::new()),
            family: String::new(),
            weight: 700,
            italic: false,
            tracking: Value::constant(0.0),
            leading: Value::constant(0.0),
            size: Value::constant((h / 14.0).round()),
            align: TextAlign::Center,
            // No wrap: the layout rules already break lines, and centred text
            // with a wrap width lands off centre.
            max_width: None,
        },
    )
    .with_fill(motion_core::Color::rgb(1.0, 1.0, 1.0));
    n.transform.position = Value::constant(motion_core::vec3::Vec3::flat(w / 2.0, h * 0.85));
    set_karaoke(&mut n, [1.0, 0.82, 0.2], false);
    n
}

impl App {
    /// Apply the panel's edits and collect a finished transcription. Whether
    /// the document changed.
    pub(crate) fn apply_subs(&mut self, e: SubsEdits) -> bool {
        let mut dirty = false;
        if let Some(f) = e.seek {
            self.playing = false;
            self.seek_frame(f);
        }
        if e.cancel {
            if let Some(j) = &self.subs_job {
                j.cancel.store(true, Ordering::Relaxed);
            }
        }
        if e.transcribe && self.subs_job.is_none() {
            // Into the selected text layer, else a new one at the top.
            let text = self.selected.filter(|id| matches!(self.doc().root.find(*id).and_then(|n| n.shape.as_ref()), Some(MShape::Text { .. })));
            let node = match text {
                Some(id) => id,
                None => {
                    let id = NodeId(self.next_id);
                    self.next_id += 1;
                    let (w, h) = (self.doc().width, self.doc().height);
                    self.doc_mut().root.children.push(new_layer(id, w, h));
                    self.selected = Some(id);
                    dirty = true;
                    id
                }
            };
            match start_job(self.project.clone(), self.current, self.sounds.clone(), node) {
                Ok(j) => {
                    self.subs_job = Some(j);
                    self.subs_ui.status = String::new();
                }
                Err(m) => self.subs_ui.status = m,
            }
        }
        let done = self.subs_job.as_ref().and_then(|j| j.rx.try_recv().ok());
        if let Some(result) = done {
            let id = self.subs_job.take().unwrap().node;
            match result {
                Ok(t) => {
                    self.subs_ui.status = format!("{} words{}", t.words.len(), t.language.as_ref().map_or(String::new(), |l| format!(" · {l}")));
                    if let Some(n) = self.doc_mut().root.find_mut(id) {
                        let rules = n.subtitles.take().map(|s| s.rules).unwrap_or_default();
                        n.subtitles = Some(Subtitles { transcript: t, rules });
                        dirty = true;
                    }
                }
                Err(m) => self.subs_ui.status = m,
            }
        }
        let Some(id) = self.selected else { return dirty };
        let Some(n) = self.doc_mut().root.find_mut(id) else { return dirty };
        if let Some((rgb, cumulative)) = e.karaoke {
            set_karaoke(n, rgb, cumulative);
            dirty = true;
        }
        if let Some(s) = n.subtitles.as_mut() {
            if let Some(r) = e.rules {
                s.rules = r;
                dirty = true;
            }
            if let Some(w) = e.words {
                s.transcript.words = w;
                dirty = true;
            }
        }
        dirty
    }
}
